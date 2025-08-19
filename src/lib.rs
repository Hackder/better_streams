use futures::Stream;
use std::cell::Cell;
use std::fmt::Debug;
use std::future::Future;
use std::rc::Rc;
use std::task::Poll;

pin_project_lite::pin_project! {
    pub struct FnStream<T, Fut>
    where
        Fut: Future<Output = ()>,
    {
        value: Rc<Cell<Option<T>>>,
        #[pin]
        future: Fut,
    }
}

impl<T, Fut> Stream for FnStream<T, Fut>
where
    Fut: Future<Output = ()>,
{
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let future = this.future;

        match Future::poll(future, cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => {
                match this.value.take() {
                    Some(value) => Poll::Ready(Some(value)),
                    None => {
                        // If no value is available, we return Pending
                        // and the stream will be polled again later.
                        Poll::Pending
                    }
                }
            }
        }
    }
}

pub struct FnStreamSend {
    sent: bool,
}

impl Future for FnStreamSend {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if self.sent {
            Poll::Ready(())
        } else {
            self.get_mut().sent = true;
            Poll::Pending
        }
    }
}

pub struct FnStreamSender<'a, T> {
    _lifetime: std::marker::PhantomData<&'a ()>,
    value: Rc<Cell<Option<T>>>,
}

impl<'a, T> FnStreamSender<'a, T>
where
    T: Debug,
{
    pub fn new(value: Rc<Cell<Option<T>>>) -> Self {
        Self {
            _lifetime: std::marker::PhantomData,
            value,
        }
    }

    #[must_use]
    pub fn send(&mut self, value: T) -> FnStreamSend {
        if let Some(_) = self.value.take() {
            panic!(
                "FnStreamSender can only send one value at a time. Previous send was not awaited"
            );
        }

        self.value.set(Some(value));

        FnStreamSend { sent: false }
    }
}

pub fn fn_stream<T, F>(f: F) -> FnStream<T, impl Future<Output = ()>>
where
    F: for<'a> AsyncFnOnce(FnStreamSender<'a, T>) -> (),
    T: Debug,
{
    let value = Rc::new(Cell::new(None));

    let sender = FnStreamSender::new(value.clone());

    let fut = f(sender);

    FnStream { value, future: fut }
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt as _, pin_mut};

    use super::*;

    #[tokio::test]
    async fn single_send() {
        let stream = fn_stream(async |mut sender| {
            sender.send(1).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn multiple_sends() {
        let stream = fn_stream(async |mut sender| {
            sender.send(1).await;
            sender.send(2).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn future_between_sends() {
        let stream = fn_stream(async |mut sender| {
            sender.send(1).await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            sender.send(2).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn nested_fn_send() {
        async fn nested(sender: &mut FnStreamSender<'_, i32>) {
            sender.send(2).await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            sender.send(3).await;
        }

        let stream = fn_stream(async |mut sender| {
            sender.send(1).await;
            nested(&mut sender).await;
            sender.send(4).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, Some(3));
        assert_eq!(stream.next().await, Some(4));
        assert_eq!(stream.next().await, None);
    }

    #[test]
    fn infallible_lifetime() {
        let a = 1;
        futures_executor::block_on(async {
            let b = 2;
            let a = &a;
            let b = &b;
            let stream = fn_stream(async |mut sender| {
                sender.send(a).await;
                sender.send(b).await;
            });
            pin_mut!(stream);
            assert_eq!(Some(a), stream.next().await);
            assert_eq!(Some(b), stream.next().await);
            assert_eq!(None, stream.next().await);
        });
    }
}
