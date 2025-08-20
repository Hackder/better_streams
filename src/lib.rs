use futures::Stream;
use std::cell::Cell;
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;

type Value<T> = Arc<thread_local::ThreadLocal<Cell<Option<T>>>>;

pin_project_lite::pin_project! {
    pub struct FnStream<T, Fut>
    where
        Fut: Future<Output = ()>,
        T: Send,
    {
        value: Value<T>,
        #[pin]
        future: Fut,
    }
}

impl<T, Fut> Stream for FnStream<T, Fut>
where
    Fut: Future<Output = ()>,
    T: Send,
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
                let value_cell = this.value.get_or(|| Cell::new(None));
                match value_cell.take() {
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

pub struct FnStreamEmit {
    sent: bool,
}

impl Future for FnStreamEmit {
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

pub struct FnStreamEmitter<T: Send> {
    value: Value<T>,
}

impl<T> FnStreamEmitter<T>
where
    T: Send,
{
    pub fn new(value: Value<T>) -> Self {
        Self { value }
    }

    #[must_use]
    pub fn emit(&mut self, value: T) -> FnStreamEmit {
        let value_cell = self.value.get_or(|| Cell::new(None));
        if value_cell.take().is_some() {
            panic!(
                "FnStreamEmitter can only send one value at a time. Previous send was not awaited"
            );
        }

        value_cell.set(Some(value));

        FnStreamEmit { sent: false }
    }
}

pub fn fn_stream<T, F>(f: F) -> FnStream<T, impl Future<Output = ()>>
where
    F: for<'a> AsyncFnOnce(&'a mut FnStreamEmitter<T>) -> (),
    T: Send,
{
    let value = Arc::new(thread_local::ThreadLocal::new());
    let value_clone = Arc::clone(&value);

    FnStream {
        value,
        future: async move {
            let mut sender = FnStreamEmitter::new(value_clone);
            f(&mut sender).await;
        },
    }
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt as _, pin_mut};

    use super::*;

    fn assert_send<T: Send>(_: T) {}

    #[test]
    fn fn_stream_send_is_send() {
        let stream = fn_stream(async |sender| {
            sender.emit(1).await;
        });

        assert_send(stream);
    }

    #[tokio::test]
    async fn single_send() {
        let stream = fn_stream(async |sender| {
            sender.emit(1).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn multiple_sends() {
        let stream = fn_stream(async |sender| {
            sender.emit(1).await;
            sender.emit(2).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn future_between_sends() {
        let stream = fn_stream(async |sender| {
            sender.emit(1).await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            sender.emit(2).await;
        });

        pin_mut!(stream);

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn nested_fn_send() {
        async fn nested(sender: &mut FnStreamEmitter<i32>) {
            sender.emit(2).await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            sender.emit(3).await;
        }

        let stream = fn_stream(async |mut sender| {
            sender.emit(1).await;
            nested(&mut sender).await;
            sender.emit(4).await;
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
            let stream = fn_stream(async |sender| {
                sender.emit(a).await;
                sender.emit(b).await;
            });
            pin_mut!(stream);
            assert_eq!(Some(a), stream.next().await);
            assert_eq!(Some(b), stream.next().await);
            assert_eq!(None, stream.next().await);
        });
    }

    // #[tokio::test]
    // async fn sender_cannot_leak_to_new_future() {
    //     let stream = fn_stream(async |sender| {
    //         sender.send(1).await;
    //         let local_set = tokio::task::LocalSet::new();
    //         local_set.spawn_local(async move {
    //             sender.send(2).await;
    //         });
    //         tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    //     });
    //
    //     pin_mut!(stream);
    //
    //     assert_eq!(stream.next().await, Some(1));
    //     assert_eq!(stream.next().await, None);
    // }
}
