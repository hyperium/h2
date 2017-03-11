use {frame, proto, ConnectionError};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use futures::*;

pub struct Connection<T> {
    inner: Inner<T>,
}

type Inner<T> =
    proto::Settings<
        proto::PingPong<
            proto::FramedWrite<
                proto::FramedRead<
                    length_delimited::FramedRead<T>>>>>;

impl<T: AsyncRead + AsyncWrite> Connection<T> {
    pub fn new(io: T) -> Connection<T> {
        // Delimit the frames
        let framed_read = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(6)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        // Map to `Frame` types
        let framed_read = proto::FramedRead::new(framed_read);

        // Frame encoder
        let mut framed = proto::FramedWrite::new(framed_read);

        // Ok, so this is a **little** hacky, but it works for now.
        //
        // The ping/pong behavior SHOULD be given highest priority (6.7).
        // However, the connection handshake requires the settings frame to be
        // sent as the very first one. This needs special handling because
        // otherwise there is a race condition where the peer could send its
        // settings frame followed immediately by a Ping, in which case, we
        // don't want to accidentally send the pong before finishing the
        // connection hand shake.
        //
        // So, to ensure correct ordering, we write the settings frame here
        // before fully constructing the connection struct. Technically, `Async`
        // operations should not be performed in `new` because this might not
        // happen on a task, however we have full control of the I/O and we know
        // that the settings frame will get buffered and not actually perform an
        // I/O op.
        let initial_settings = frame::SettingSet::default();
        let frame = frame::Settings::new(initial_settings.clone());
        assert!(framed.start_send(frame.into()).unwrap().is_ready());

        // Add ping/pong handler
        let ping_pong = proto::PingPong::new(framed);

        // Add settings handler
        let connection = proto::Settings::new(ping_pong, initial_settings);

        Connection {
            inner: connection,
        }
    }
}

impl<T: AsyncRead + AsyncWrite> Stream for Connection<T> {
    type Item = frame::Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<frame::Frame>, ConnectionError> {
        self.inner.poll()
    }
}

impl<T: AsyncRead + AsyncWrite> Sink for Connection<T> {
    type SinkItem = frame::Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: frame::Frame) -> StartSend<frame::Frame, ConnectionError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}
