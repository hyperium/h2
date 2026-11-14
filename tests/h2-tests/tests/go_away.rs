use h2_support::prelude::*;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::ReadBuf;

const REASON: u32 = 0x40;
const LOCAL_GO_AWAY: &[u8] = &[0, 0, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const REQUEST: &[u8] = &[
    0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29, 0xAC, 0x4B, 0x8F, 0xA8, 0xE9,
    0x19, 0x97, 0x21, 0xE9, 0x84,
];

fn go_away(last_stream_id: u32, reason: u32) -> Vec<u8> {
    let mut frame = vec![0, 0, 15, 7, 0, 0, 0, 0, 0];
    frame.extend_from_slice(&last_stream_id.to_be_bytes());
    frame.extend_from_slice(&reason.to_be_bytes());
    frame.extend_from_slice(b"backoff");
    frame
}

fn assert_remote_go_away(err: &h2::Error) {
    assert!(err.is_go_away(), "expected GOAWAY, got {:?}", err);
    assert!(err.is_remote());
    assert_eq!(err.reason(), Some(Reason::from(REASON)));
    assert!(err.to_string().contains("backoff"));
}

#[tokio::test]
async fn received_go_away_survives_read_error() {
    let io = mock_io::Builder::new()
        .handshake()
        .read(&go_away(0, REASON))
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (mut client, conn) = client::handshake(io).await.unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
    assert_remote_go_away(&poll_fn(|cx| client.poll_ready(cx)).await.unwrap_err());
}

#[tokio::test]
async fn received_go_away_survives_write_error() {
    let io = mock_io::Builder::new()
        .handshake()
        .read(&go_away(0, REASON))
        .write_error(io::ErrorKind::BrokenPipe.into())
        .build();
    let (_client, conn) = client::handshake(io).await.unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
}

#[tokio::test]
async fn received_go_away_survives_flush_error() {
    let io = mock_io::Builder::new()
        .handshake()
        .read(&go_away(0, REASON))
        .write(frames::SETTINGS_ACK)
        .build();
    let io = FailingClose {
        inner: io,
        bytes_read: 0,
        flush_at: Some(2 * frames::SETTINGS.len() + go_away(0, REASON).len()),
    };
    let (_client, conn) = client::handshake(io).await.unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
}

#[tokio::test]
async fn received_go_away_survives_shutdown_error() {
    let io = mock_io::Builder::new()
        .handshake()
        .read(&go_away(0, REASON))
        .write(frames::SETTINGS_ACK)
        .write(LOCAL_GO_AWAY)
        .build();
    let io = FailingClose {
        inner: io,
        bytes_read: 0,
        flush_at: None,
    };
    let (_client, conn) = client::handshake(io).await.unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
}

#[tokio::test]
async fn io_error_without_received_go_away_is_preserved() {
    let io = mock_io::Builder::new()
        .handshake()
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (_client, conn) = client::handshake(io).await.unwrap();

    let err = conn.await.unwrap_err();
    assert_eq!(err.get_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn no_error_go_away_does_not_hide_io_error() {
    let io = mock_io::Builder::new()
        .handshake()
        .read(&go_away(0, 0))
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (_client, conn) = client::handshake(io).await.unwrap();

    let err = conn.await.unwrap_err();
    assert_eq!(err.get_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn received_go_away_does_not_hide_io_error_on_active_stream() {
    let io = mock_io::Builder::new()
        .handshake()
        .write(REQUEST)
        .write(frames::SETTINGS_ACK)
        .read(&go_away(1, REASON))
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (mut client, conn) = client::handshake(io).await.unwrap();
    let request = Request::get("https://http2.akamai.com/").body(()).unwrap();
    let (response, _stream) = client.send_request(request, true).unwrap();

    let err = conn.await.unwrap_err();
    assert_eq!(err.get_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
    let err = response.await.unwrap_err();
    assert_eq!(err.get_io().unwrap().kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn received_go_away_survives_io_error_after_rejecting_stream() {
    let io = mock_io::Builder::new()
        .handshake()
        .write(REQUEST)
        .write(frames::SETTINGS_ACK)
        .read(&go_away(0, REASON))
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (mut client, conn) = client::handshake(io).await.unwrap();
    let request = Request::get("https://http2.akamai.com/").body(()).unwrap();
    let (response, _stream) = client.send_request(request, true).unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
    assert_remote_go_away(&response.await.unwrap_err());
}

#[tokio::test]
async fn received_go_away_survives_io_error_after_completing_stream() {
    let io = mock_io::Builder::new()
        .handshake()
        .write(REQUEST)
        .write(frames::SETTINGS_ACK)
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x88])
        .read(&go_away(1, REASON))
        .read_error(io::ErrorKind::ConnectionReset.into())
        .build();
    let (mut client, conn) = client::handshake(io).await.unwrap();
    let request = Request::get("https://http2.akamai.com/").body(()).unwrap();
    let (response, _stream) = client.send_request(request, true).unwrap();

    assert_remote_go_away(&conn.await.unwrap_err());
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.body().is_end_stream());
}

struct FailingClose<T> {
    inner: T,
    bytes_read: usize,
    flush_at: Option<usize>,
}

impl<T: AsyncRead + Unpin> AsyncRead for FailingClose<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        self.bytes_read += buf.filled().len() - before;
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for FailingClose<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self
            .flush_at
            .map_or(false, |offset| self.bytes_read >= offset)
        {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        } else {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
    }
}
