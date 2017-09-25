use h2::client;

use bytes::Bytes;
use futures::{Async, Future, Poll};

pub fn wait_for_capacity(stream: client::Stream<Bytes>, target: usize) -> WaitForCapacity {
    WaitForCapacity {
        stream: Some(stream),
        target: target,
    }
}

pub struct WaitForCapacity {
    stream: Option<client::Stream<Bytes>>,
    target: usize,
}

impl WaitForCapacity {
    fn stream(&mut self) -> &mut client::Stream<Bytes> {
        self.stream.as_mut().unwrap()
    }
}

impl Future for WaitForCapacity {
    type Item = client::Stream<Bytes>;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, ()> {
        let _ = try_ready!(self.stream().poll_capacity().map_err(|_| panic!()));

        let act = self.stream().capacity();

        println!("CAP={:?}", act);

        if act >= self.target {
            return Ok(self.stream.take().unwrap().into());
        }

        Ok(Async::NotReady)
    }
}
