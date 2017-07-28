use ConnectionError;
use proto::*;

/// An alias for types that implement Stream + Sink over H2 frames.
pub trait FrameStream<B>: Stream<Item = Frame, Error = ConnectionError> +
                          Sink<SinkItem = Frame<B>, SinkError = ConnectionError>
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError>;
}

pub trait Stage<B> {

    fn poll<T>(&mut self, upstream: &mut T) -> Poll<Option<Frame>, ConnectionError>
        where T: FrameStream<B>,
    {
        upstream.poll()
    }

    fn poll_ready<T>(&mut self, upstream: &mut T) -> Poll<(), ConnectionError>
        where T: FrameStream<B>,
    {
        upstream.poll_ready()
    }

    fn start_send<T>(&mut self, item: Frame<B>, upstream: &mut T) -> StartSend<Frame<B>, ConnectionError>
        where T: FrameStream<B>,
    {
        upstream.start_send(item)
    }

    fn poll_complete<T>(&mut self, upstream: &mut T) -> Poll<(), ConnectionError>
        where T: FrameStream<B>,
    {
        upstream.poll_complete()
    }
}

pub trait StreamStage {
}
