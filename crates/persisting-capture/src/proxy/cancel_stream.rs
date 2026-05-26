//! Stream wrapper that signals when the client stops reading (disconnect / cancel).

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::sync::oneshot;

/// Notifies `cancel_tx` when this stream is dropped (client disconnected or body consumed).
pub struct ClientCancelStream<S> {
    inner: S,
    cancel: Option<oneshot::Sender<()>>,
}

impl<S> ClientCancelStream<S> {
    pub fn new(inner: S, cancel: oneshot::Sender<()>) -> Self {
        Self {
            inner,
            cancel: Some(cancel),
        }
    }
}

impl<S> Drop for ClientCancelStream<S> {
    fn drop(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }
}

impl<S: Stream + Unpin> Stream for ClientCancelStream<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn cancel_fires_on_drop_before_eof() {
        let (tx, rx) = oneshot::channel();
        {
            let mut s = ClientCancelStream::new(stream::iter(vec![Ok::<u8, ()>(1u8), Ok(2)]), tx);
            assert_eq!(s.next().await, Some(Ok(1)));
            // drop without reading second item
        }
        rx.await.unwrap();
    }
}
