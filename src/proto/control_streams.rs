use proto::*;

/// Exposes stream states to "upper" layers of the transport (i.e. from StreamTracker up
/// to Connection).
pub trait ControlStreams {
    fn streams(&self) -> &Streams;

    fn streams_mut(&mut self) -> &mut Streams;
}

macro_rules! proxy_control_streams {
    ($outer:ident) => (
        impl<T: ControlStreams> ControlStreams for $outer<T> {
            fn streams(&self) -> &Streams {
                self.inner.streams()
            }

            fn streams_mut(&mut self) -> &mut Streams {
                self.inner.streams_mut()
            }
        }
    )
}
