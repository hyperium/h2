use futures::{Sink, Poll};

pub trait ReadySink: Sink {
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError>;
}
