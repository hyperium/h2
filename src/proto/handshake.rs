use {ConnectionError, Peer};
use frame::{self, Frame};
use proto::{self, Connection};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use futures::{Future, Sink, Stream, Poll, Async, AsyncSink};

use std::marker::PhantomData;

/// Implements the settings component of the initial H2 handshake
pub struct Handshake<T, P> {
    // Upstream transport
    inner: Option<Inner<T>>,

    // True when the local settings have been sent
    settings_sent: bool,

    // Peer
    peer: PhantomData<P>,
}

struct Inner<T> {
    // Upstream transport
    framed: proto::Framed<T>,

    // Our settings
    local: frame::SettingSet,
}

impl<T, P> Handshake<T, P>
    where T: AsyncRead + AsyncWrite,
{
    /// Initiate an HTTP/2.0 handshake.
    pub fn new(io: T, local: frame::SettingSet) -> Self {
        // Delimit the frames
        let framed_read = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        // Map to `Frame` types
        let framed_read = proto::FramedRead::new(framed_read);

        // Frame encoder
        let mut framed = proto::FramedWrite::new(framed_read);

        Handshake {
            inner: Some(Inner {
                framed: framed,
                local: local,
            }),
            settings_sent: false,
            peer: PhantomData,
        }
    }

    /// Returns a reference to the local settings.
    ///
    /// # Panics
    ///
    /// Panics if `HandshakeInner` has already been consumed.
    fn local(&self) -> &frame::SettingSet {
        &self.inner.as_ref().unwrap().local
    }

    /// Returns a mutable reference to `HandshakeInner`.
    ///
    /// # Panics
    ///
    /// Panics if `HandshakeInner` has already been consumed.
    fn inner_mut(&mut self) -> &mut proto::Framed<T> {
        &mut self.inner.as_mut().unwrap().framed
    }
}

// Either a client or server. satisfied when we have sent a SETTINGS frame and
// have sent an ACK for the remote's settings.
impl<T, P> Future for Handshake<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    type Item = Connection<T, P>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if !self.settings_sent {
            let frame = frame::Settings::new(self.local().clone()).into();

            if let AsyncSink::NotReady(_) = try!(self.inner_mut().start_send(frame)) {
                // This shouldn't really happen, but if it does, try again
                // later.
                return Ok(Async::NotReady);
            }

            // Try flushing...
            try!(self.inner_mut().poll_complete());

            self.settings_sent = true;
        }

        match try_ready!(self.inner_mut().poll()) {
            Some(Frame::Settings(v)) => {
                if v.is_ack() {
                    // TODO: unexpected ACK, protocol error
                    unimplemented!();
                } else {
                    let remote = v.into_set();
                    let inner = self.inner.take().unwrap();

                    // Add ping/pong handler
                    let ping_pong = proto::PingPong::new(inner.framed);

                    // Add settings handler
                    let settings = proto::Settings::new(
                        ping_pong, inner.local, remote);

                    // Finally, convert to the `Connection`
                    let connection = settings.into();

                    return Ok(Async::Ready(connection));
                }
            }
            // TODO: handle handshake failure
            _ => unimplemented!(),
        }
    }
}
