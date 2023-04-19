use h2;

use bytes::{BufMut, Bytes};
use futures::ready;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub fn byte_str(s: &str) -> h2::frame::BytesStr {
    h2::frame::BytesStr::try_from(Bytes::copy_from_slice(s.as_bytes())).unwrap()
}

pub async fn concat(mut body: h2::RecvStream) -> Result<Bytes, h2::Error> {
    let mut vec = Vec::new();
    while let Some(chunk) = body.data().await {
        vec.put(chunk?);
    }
    Ok(vec.into())
}

pub async fn yield_once() {
    let mut yielded = false;
    futures::future::poll_fn(move |cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().clone().wake();
            Poll::Pending
        }
    })
    .await;
}

/// Should only be called after a non-0 capacity was requested for the stream.
pub fn wait_for_capacity(stream: h2::SendStream<Bytes>, target: usize) -> WaitForCapacity {
    WaitForCapacity {
        stream: Some(stream),
        target,
    }
}

pub struct WaitForCapacity {
    stream: Option<h2::SendStream<Bytes>>,
    target: usize,
}

impl WaitForCapacity {
    fn stream(&mut self) -> &mut h2::SendStream<Bytes> {
        self.stream.as_mut().unwrap()
    }
}

impl Future for WaitForCapacity {
    type Output = h2::SendStream<Bytes>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let _ = ready!(self.stream().poll_capacity(cx)).unwrap();

            let act = self.stream().capacity();

            // If a non-0 capacity was requested for the stream before calling
            // wait_for_capacity, then poll_capacity should return Pending
            // until there is a non-0 capacity.
            assert_ne!(act, 0);

            if act >= self.target {
                return Poll::Ready(self.stream.take().unwrap());
            }
        }
    }
}
