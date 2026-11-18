use super::{Builder, Connection, ReceiveAction, ReceiveEvent, SendResponse};
use crate::{Reason, RecvStream};

use bytes::Bytes;
use http::{Request, Response};
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Debug, Default)]
struct WakeCounter(AtomicUsize);

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
struct IoState {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    reader: Option<Waker>,
}

#[derive(Clone, Debug, Default)]
struct BufferedIo(Arc<Mutex<IoState>>);

impl BufferedIo {
    fn feed(&self, bytes: &[u8]) {
        let reader = {
            let mut state = self.0.lock().unwrap();
            state.incoming.extend(bytes.iter().copied());
            state.reader.take()
        };
        if let Some(reader) = reader {
            reader.wake();
        }
    }

    fn wrote_frame(&self, kind: u8, stream_id: u32, payload: &[u8]) -> bool {
        let state = self.0.lock().unwrap();
        let mut bytes = state.outgoing.as_slice();
        while !bytes.is_empty() {
            assert!(bytes.len() >= 9, "incomplete output frame header");
            let len = (usize::from(bytes[0]) << 16)
                | (usize::from(bytes[1]) << 8)
                | usize::from(bytes[2]);
            assert!(bytes.len() >= 9 + len, "incomplete output frame payload");
            let id = u32::from_be_bytes(bytes[5..9].try_into().unwrap()) & 0x7fff_ffff;
            if bytes[3] == kind && id == stream_id && &bytes[9..9 + len] == payload {
                return true;
            }
            bytes = &bytes[9 + len..];
        }
        false
    }
}

impl AsyncRead for BufferedIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut state = self.0.lock().unwrap();
        if state.incoming.is_empty() {
            // Exhausting the currently available input must not simulate EOF.
            state.reader = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let len = state.incoming.len().min(buf.remaining());
        let bytes: Vec<_> = state.incoming.drain(..len).collect();
        buf.put_slice(&bytes);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for BufferedIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.lock().unwrap().outgoing.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Observed {
    RequestHeaders(u32),
    GoAway(u32, Reason),
}

type Observations = Arc<Mutex<Vec<Observed>>>;

struct Harness {
    connection: Connection<BufferedIo, Bytes>,
    io: BufferedIo,
    wakes: Arc<WakeCounter>,
    waker: Waker,
}

impl Harness {
    fn new(builder: Builder) -> Self {
        let io = BufferedIo::default();
        io.feed(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
        io.feed(&frame(0x4, 0, 0, &[]));
        io.feed(&frame(0x4, 0x1, 0, &[]));

        let wakes = Arc::new(WakeCounter::default());
        let waker = Waker::from(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut handshake = Box::pin(builder.handshake::<_, Bytes>(io.clone()));
        for _ in 0..16 {
            match handshake.as_mut().poll(&mut cx) {
                Poll::Ready(Ok(connection)) => {
                    return Harness {
                        connection,
                        io,
                        wakes,
                        waker,
                    };
                }
                Poll::Ready(Err(err)) => panic!("handshake failed: {err}"),
                Poll::Pending => (),
            }
        }
        panic!("handshake did not finish with an available client preface");
    }

    fn observe(&mut self, observations: &Observations, yield_headers: bool, yield_go_away: bool) {
        let observations = observations.clone();
        self.connection.set_receive_observer(move |event| {
            let (event, should_yield) = match event {
                ReceiveEvent::RequestHeaders { stream_id } => {
                    (Observed::RequestHeaders(stream_id.as_u32()), yield_headers)
                }
                ReceiveEvent::GoAway {
                    last_stream_id,
                    reason,
                } => (
                    Observed::GoAway(last_stream_id.as_u32(), reason),
                    yield_go_away,
                ),
            };
            observations.lock().unwrap().push(event);
            if should_yield {
                ReceiveAction::Yield
            } else {
                ReceiveAction::Continue
            }
        });
    }

    fn accept_ready(&mut self) -> (Request<RecvStream>, SendResponse<Bytes>) {
        let mut cx = Context::from_waker(&self.waker);
        match self.connection.poll_accept(&mut cx) {
            Poll::Ready(Some(Ok(request))) => request,
            other => panic!("expected an accepted request in this poll, got {other:?}"),
        }
    }

    fn poll_closed(&mut self) -> Poll<Result<(), crate::Error>> {
        let mut cx = Context::from_waker(&self.waker);
        self.connection.poll_closed(&mut cx)
    }

    fn close_error(&mut self) -> crate::Error {
        for _ in 0..16 {
            match self.poll_closed() {
                Poll::Ready(Err(err)) => return err,
                Poll::Ready(Ok(())) => panic!("connection closed without the expected error"),
                Poll::Pending => (),
            }
        }
        panic!("connection did not report its protocol error");
    }
}

fn frame(kind: u8, flags: u8, stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let len = payload.len();
    assert!(len <= 0x00ff_ffff);
    let mut bytes = vec![(len >> 16) as u8, (len >> 8) as u8, len as u8, kind, flags];
    bytes.extend_from_slice(&stream_id.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn request_headers(stream_id: u32) -> Vec<u8> {
    // Indexed :method GET, :scheme http, :path /, followed by a literal
    // :authority. No dynamic table entries are created.
    let mut block = vec![0x82, 0x86, 0x84, 0x01, 11];
    block.extend_from_slice(b"example.com");
    frame(0x1, 0x4, stream_id, &block)
}

fn go_away(last_stream_id: u32, reason: Reason) -> Vec<u8> {
    let mut payload = last_stream_id.to_be_bytes().to_vec();
    payload.extend_from_slice(&u32::from(reason).to_be_bytes());
    frame(0x7, 0, 0, &payload)
}

#[test]
fn no_error_go_away_is_observable_while_a_request_is_active() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, false, false);
    harness.io.feed(&request_headers(1));
    let (mut request, mut respond) = harness.accept_ready();
    let mut send = respond.send_response(Response::new(()), false).unwrap();

    let mut packet = go_away(0, Reason::NO_ERROR);
    packet.extend_from_slice(&frame(0x0, 0, 1, b"request body"));
    harness.io.feed(&packet);
    assert!(harness.poll_closed().is_pending());
    assert_eq!(
        *observations.lock().unwrap(),
        vec![
            Observed::RequestHeaders(1),
            Observed::GoAway(0, Reason::NO_ERROR),
        ]
    );

    let mut cx = Context::from_waker(&harness.waker);
    match request.body_mut().poll_data(&mut cx) {
        Poll::Ready(Some(Ok(bytes))) => assert_eq!(bytes.as_ref(), b"request body"),
        other => panic!("active request could not receive data after GOAWAY: {other:?}"),
    }
    send.send_data(Bytes::from_static(b"response body"), false)
        .unwrap();
    assert!(harness.poll_closed().is_pending());
    assert!(harness.io.wrote_frame(0x0, 1, b"response body"));
}

#[test]
fn yielding_headers_delivers_the_request_before_coalesced_go_away() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    let registered = Arc::new(Mutex::new(Vec::new()));
    let drain_snapshots = Arc::new(Mutex::new(Vec::new()));
    let seen = observations.clone();
    let known_requests = registered.clone();
    let snapshots = drain_snapshots.clone();
    harness
        .connection
        .set_receive_observer(move |event| match event {
            ReceiveEvent::RequestHeaders { stream_id } => {
                seen.lock()
                    .unwrap()
                    .push(Observed::RequestHeaders(stream_id.as_u32()));
                ReceiveAction::Yield
            }
            ReceiveEvent::GoAway {
                last_stream_id,
                reason,
            } => {
                seen.lock()
                    .unwrap()
                    .push(Observed::GoAway(last_stream_id.as_u32(), reason));
                snapshots
                    .lock()
                    .unwrap()
                    .push(known_requests.lock().unwrap().clone());
                ReceiveAction::Continue
            }
        });

    let mut packet = request_headers(1);
    packet.extend_from_slice(&go_away(0, Reason::NO_ERROR));
    harness.io.feed(&packet);
    let (request, _respond) = harness.accept_ready();
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(1)]
    );
    assert!(drain_snapshots.lock().unwrap().is_empty());
    registered
        .lock()
        .unwrap()
        .push(request.body().stream_id().as_u32());

    assert!(harness.poll_closed().is_pending());
    assert_eq!(*drain_snapshots.lock().unwrap(), vec![vec![1]]);
    assert_eq!(
        *observations.lock().unwrap(),
        vec![
            Observed::RequestHeaders(1),
            Observed::GoAway(0, Reason::NO_ERROR),
        ]
    );
}

#[test]
fn yielding_delivers_multiple_requests_in_wire_order_before_go_away() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, true, false);
    let mut packet = request_headers(1);
    packet.extend_from_slice(&request_headers(3));
    packet.extend_from_slice(&go_away(0, Reason::NO_ERROR));
    harness.io.feed(&packet);

    let (first, _first_response) = harness.accept_ready();
    assert_eq!(first.body().stream_id().as_u32(), 1);
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(1)]
    );
    let (second, _second_response) = harness.accept_ready();
    assert_eq!(second.body().stream_id().as_u32(), 3);
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(1), Observed::RequestHeaders(3)]
    );
    assert!(harness.poll_closed().is_pending());
    assert_eq!(
        *observations.lock().unwrap(),
        vec![
            Observed::RequestHeaders(1),
            Observed::RequestHeaders(3),
            Observed::GoAway(0, Reason::NO_ERROR),
        ]
    );
}

#[test]
fn trailers_for_a_queued_request_do_not_emit_another_headers_event() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, false, false);
    let mut packet = request_headers(1);
    // A literal x-test: done trailer, with END_HEADERS and END_STREAM.
    packet.extend_from_slice(&frame(0x1, 0x5, 1, b"\x00\x06x-test\x04done"));
    harness.io.feed(&packet);

    let (mut request, _respond) = harness.accept_ready();
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(1)]
    );
    let mut cx = Context::from_waker(&harness.waker);
    match request.body_mut().poll_trailers(&mut cx) {
        Poll::Ready(Ok(Some(trailers))) => assert_eq!(trailers["x-test"], "done"),
        other => panic!("expected the received request trailers, got {other:?}"),
    }
}

#[test]
fn refused_requests_do_not_emit_headers_events() {
    let mut builder = Builder::new();
    builder.max_concurrent_streams(0);
    let mut harness = Harness::new(builder);
    let observations = Observations::default();
    harness.observe(&observations, true, false);
    harness.io.feed(&request_headers(1));

    let mut cx = Context::from_waker(&harness.waker);
    assert!(harness.connection.poll_accept(&mut cx).is_pending());
    assert!(observations.lock().unwrap().is_empty());
    assert!(harness
        .io
        .wrote_frame(0x3, 1, &u32::from(Reason::REFUSED_STREAM).to_be_bytes()));
}

#[test]
fn malformed_requests_do_not_emit_headers_events() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, false, false);
    // A decodable HPACK block missing the required :method pseudo-header.
    let mut packet = frame(0x1, 0x4, 1, &[0x86, 0x84]);
    packet.extend_from_slice(&request_headers(3));
    harness.io.feed(&packet);

    let (request, _respond) = harness.accept_ready();
    assert_eq!(request.body().stream_id().as_u32(), 3);
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(3)]
    );
    assert!(harness
        .io
        .wrote_frame(0x3, 1, &u32::from(Reason::PROTOCOL_ERROR).to_be_bytes()));
}

#[test]
fn increasing_go_away_remains_a_protocol_error_without_an_observer() {
    let mut harness = Harness::new(Builder::new());
    harness.io.feed(&request_headers(1));
    let (_request, _respond) = harness.accept_ready();
    let mut packet = go_away(0, Reason::NO_ERROR);
    packet.extend_from_slice(&go_away(2, Reason::NO_ERROR));
    harness.io.feed(&packet);

    let error = harness.close_error();
    assert!(error.is_go_away());
    assert_eq!(error.reason(), Some(Reason::PROTOCOL_ERROR));
}

#[test]
fn observing_go_away_does_not_bypass_protocol_validation() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, true, true);
    harness.io.feed(&request_headers(1));
    let (_request, _respond) = harness.accept_ready();
    let mut packet = go_away(0, Reason::NO_ERROR);
    packet.extend_from_slice(&go_away(2, Reason::NO_ERROR));
    harness.io.feed(&packet);

    let error = harness.close_error();
    assert!(error.is_go_away());
    assert_eq!(error.reason(), Some(Reason::PROTOCOL_ERROR));
    assert_eq!(
        *observations.lock().unwrap(),
        vec![
            Observed::RequestHeaders(1),
            Observed::GoAway(0, Reason::NO_ERROR),
        ]
    );
}

#[test]
fn yielding_wakes_the_driver_and_resumes_buffered_input() {
    let mut harness = Harness::new(Builder::new());
    let observations = Observations::default();
    harness.observe(&observations, true, true);
    let mut packet = request_headers(1);
    packet.extend_from_slice(&go_away(0, Reason::NO_ERROR));
    harness.io.feed(&packet);
    let wakes_before_headers = harness.wakes.0.load(Ordering::SeqCst);

    assert!(harness.poll_closed().is_pending());
    assert_eq!(
        *observations.lock().unwrap(),
        vec![Observed::RequestHeaders(1)]
    );
    let wakes_before_go_away = harness.wakes.0.load(Ordering::SeqCst);
    assert!(wakes_before_go_away > wakes_before_headers);

    // No input is added: the next poll must resume already buffered frames.
    assert!(harness.poll_closed().is_pending());
    assert_eq!(
        *observations.lock().unwrap(),
        vec![
            Observed::RequestHeaders(1),
            Observed::GoAway(0, Reason::NO_ERROR),
        ]
    );
    assert!(harness.wakes.0.load(Ordering::SeqCst) > wakes_before_go_away);
    let (request, _respond) = harness.accept_ready();
    assert_eq!(request.body().stream_id().as_u32(), 1);
}
