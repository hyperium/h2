use {client, frame, proto, server};
use codec::RecvError;
use frame::{Reason, StreamId};

use frame::DEFAULT_INITIAL_WINDOW_SIZE;
use proto::*;

use bytes::{Bytes, IntoBuf};
use futures::Stream;
use tokio_io::{AsyncRead, AsyncWrite};

use std::marker::PhantomData;
use std::time::Duration;

/// An H2 connection
#[derive(Debug)]
pub(crate) struct Connection<T, P, B: IntoBuf = Bytes>
where
    P: Peer,
{
    /// Tracks the connection level state transitions.
    state: State,

    /// An error to report back once complete.
    ///
    /// This exists separately from State in order to support
    /// graceful shutdown.
    error: Option<Reason>,

    /// Read / write frame values
    codec: Codec<T, Prioritized<B::Buf>>,

    /// Ping/pong handler
    ping_pong: PingPong,

    /// Connection settings
    settings: Settings,

    /// Stream state handler
    streams: Streams<B::Buf, P>,

    /// Client or server
    _phantom: PhantomData<P>,
}

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub next_stream_id: StreamId,
    pub reset_stream_duration: Duration,
    pub reset_stream_max: usize,
    pub settings: frame::Settings,
}

#[derive(Debug)]
enum State {
    /// Currently open in a sane state
    Open,

    /// Waiting to send a GOAWAY frame
    GoAway(frame::GoAway),

    /// The codec must be flushed
    Flush(Reason),

    /// In a closed state
    Closed(Reason),
}

impl<T, P, B> Connection<T, P, B>
where
    T: AsyncRead + AsyncWrite,
    P: Peer,
    B: IntoBuf,
{
    pub fn new(
        codec: Codec<T, Prioritized<B::Buf>>,
        config: Config,
    ) -> Connection<T, P, B> {
        let streams = Streams::new(streams::Config {
            local_init_window_sz: config.settings
                .initial_window_size()
                .unwrap_or(DEFAULT_INITIAL_WINDOW_SIZE),
            local_max_initiated: None,
            local_next_stream_id: config.next_stream_id,
            local_push_enabled: config.settings.is_push_enabled(),
            local_reset_duration: config.reset_stream_duration,
            local_reset_max: config.reset_stream_max,
            remote_init_window_sz: DEFAULT_INITIAL_WINDOW_SIZE,
            remote_max_initiated: config.settings
                .max_concurrent_streams()
                .map(|max| max as usize),
        });
        Connection {
            state: State::Open,
            error: None,
            codec: codec,
            ping_pong: PingPong::new(),
            settings: Settings::new(),
            streams: streams,
            _phantom: PhantomData,
        }
    }

    pub fn set_target_window_size(&mut self, size: WindowSize) {
        self.streams.set_target_connection_window_size(size);
    }

    /// Returns `Ready` when the connection is ready to receive a frame.
    ///
    /// Returns `RecvError` as this may raise errors that are caused by delayed
    /// processing of received frames.
    fn poll_ready(&mut self) -> Poll<(), RecvError> {
        // The order of these calls don't really matter too much as only one
        // should have pending work.
        try_ready!(self.ping_pong.send_pending_pong(&mut self.codec));
        try_ready!(
            self.settings
                .send_pending_ack(&mut self.codec, &mut self.streams)
        );
        try_ready!(self.streams.send_pending_refusal(&mut self.codec));

        Ok(().into())
    }

    fn transition_to_go_away(&mut self, id: StreamId, e: Reason) {
        let goaway = frame::GoAway::new(id, e);
        self.state = State::GoAway(goaway);
    }

    /// Closes the connection by transitioning to a GOAWAY state
    /// iff there are no streams or references
    pub fn maybe_close_connection_if_no_streams(&mut self) {
        // If we poll() and realize that there are no streams or references
        // then we can close the connection by transitioning to GOAWAY
        if self.streams.num_active_streams() == 0 && !self.streams.has_streams_or_other_references() {
            self.close_connection();
        }
    }

    /// Closes the connection by transitioning to a GOAWAY state
    pub fn close_connection(&mut self) {
        let last_processed_id = self.streams.last_processed_id();
        self.transition_to_go_away(last_processed_id, Reason::NO_ERROR);
    }

    /// Advances the internal state of the connection.
    pub fn poll(&mut self) -> Poll<(), proto::Error> {
        use codec::RecvError::*;

        loop {
            // TODO: probably clean up this glob of code
            match self.state {
                // When open, continue to poll a frame
                State::Open => {
                    match self.poll2() {
                        // The connection has shutdown normally
                        Ok(Async::Ready(())) => return Ok(().into()),
                        // The connection is not ready to make progress
                        Ok(Async::NotReady) => {
                            // Ensure all window updates have been sent.
                            //
                            // This will also handle flushing `self.codec`
                            try_ready!(self.streams.poll_complete(&mut self.codec));

                            if self.error.is_some() {
                                if self.streams.num_active_streams() == 0 {
                                    let last_processed_id = self.streams.last_processed_id();
                                    self.transition_to_go_away(last_processed_id, Reason::NO_ERROR);
                                    continue;
                                }
                            }

                            return Ok(Async::NotReady);
                        },
                        // Attempting to read a frame resulted in a connection level
                        // error. This is handled by setting a GOAWAY frame followed by
                        // terminating the connection.
                        Err(Connection(e)) => {
                            debug!("Connection::poll; err={:?}", e);

                            // Reset all active streams
                            let last_processed_id = self.streams.recv_err(&e.into());
                            self.transition_to_go_away(last_processed_id, e);
                        },
                        // Attempting to read a frame resulted in a stream level error.
                        // This is handled by resetting the frame then trying to read
                        // another frame.
                        Err(Stream {
                            id,
                            reason,
                        }) => {
                            trace!("stream level error; id={:?}; reason={:?}", id, reason);
                            self.streams.send_reset(id, reason);
                        },
                        // Attempting to read a frame resulted in an I/O error. All
                        // active streams must be reset.
                        //
                        // TODO: Are I/O errors recoverable?
                        Err(Io(e)) => {
                            let e = e.into();

                            // Reset all active streams
                            self.streams.recv_err(&e);

                            // Return the error
                            return Err(e);
                        },
                    }
                },
                State::GoAway(frame) => {
                    // Ensure the codec is ready to accept the frame
                    try_ready!(self.codec.poll_ready());

                    // Buffer the GOAWAY frame
                    self.codec
                        .buffer(frame.into())
                        .ok()
                        .expect("invalid GO_AWAY frame");

                    // GOAWAY sent, transition the connection to a closed state
                    // Determine what error code should be returned to user.
                    let reason = if let Some(theirs) = self.error.take() {
                        let ours = frame.reason();
                        match (ours, theirs) {
                            // If either side reported an error, return that
                            // to the user.
                            (Reason::NO_ERROR, err) | (err, Reason::NO_ERROR) => err,
                            // If both sides reported an error, give their
                            // error back to th user. We assume our error
                            // was a consequence of their error, and less
                            // important.
                            (_, theirs) => theirs,
                        }
                    } else {
                        frame.reason()
                    };
                    self.state = State::Flush(reason);
                },
                State::Flush(reason) => {
                    // Flush the codec
                    try_ready!(self.codec.flush());

                    // Transition the state to error
                    self.state = State::Closed(reason);
                },
                State::Closed(reason) => if let Reason::NO_ERROR = reason {
                    return Ok(Async::Ready(()));
                } else {
                    return Err(reason.into());
                },
            }
        }
    }

    fn poll2(&mut self) -> Poll<(), RecvError> {
        use frame::Frame::*;

        // This happens outside of the loop to prevent needing to do a clock
        // check and then comparison of the queue possibly multiple times a
        // second (and thus, the clock wouldn't have changed enough to matter).
        self.clear_expired_reset_streams();

        loop {
            // First, ensure that the `Connection` is able to receive a frame
            try_ready!(self.poll_ready());

            match try_ready!(self.codec.poll()) {
                Some(Headers(frame)) => {
                    trace!("recv HEADERS; frame={:?}", frame);
                    self.streams.recv_headers(frame)?;
                },
                Some(Data(frame)) => {
                    trace!("recv DATA; frame={:?}", frame);
                    self.streams.recv_data(frame)?;
                },
                Some(Reset(frame)) => {
                    trace!("recv RST_STREAM; frame={:?}", frame);
                    self.streams.recv_reset(frame)?;
                },
                Some(PushPromise(frame)) => {
                    trace!("recv PUSH_PROMISE; frame={:?}", frame);
                    self.streams.recv_push_promise(frame)?;
                },
                Some(Settings(frame)) => {
                    trace!("recv SETTINGS; frame={:?}", frame);
                    self.settings.recv_settings(frame);
                },
                Some(GoAway(frame)) => {
                    trace!("recv GOAWAY; frame={:?}", frame);
                    // This should prevent starting new streams,
                    // but should allow continuing to process current streams
                    // until they are all EOS. Once they are, State should
                    // transition to GoAway.
                    self.streams.recv_goaway(&frame);
                    self.error = Some(frame.reason());
                },
                Some(Ping(frame)) => {
                    trace!("recv PING; frame={:?}", frame);
                    self.ping_pong.recv_ping(frame);
                },
                Some(WindowUpdate(frame)) => {
                    trace!("recv WINDOW_UPDATE; frame={:?}", frame);
                    self.streams.recv_window_update(frame)?;
                },
                Some(Priority(frame)) => {
                    trace!("recv PRIORITY; frame={:?}", frame);
                    // TODO: handle
                },
                None => {
                    trace!("codec closed");
                    self.streams.recv_eof();
                    return Ok(Async::Ready(()));
                },
            }
        }
    }

    fn clear_expired_reset_streams(&mut self) {
        self.streams.clear_expired_reset_streams();
    }
}

impl<T, B> Connection<T, client::Peer, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    pub(crate) fn streams(&self) -> &Streams<B::Buf, client::Peer> {
        &self.streams
    }
}

impl<T, B> Connection<T, server::Peer, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    pub fn next_incoming(&mut self) -> Option<StreamRef<B::Buf>> {
        self.streams.next_incoming()
    }
}
