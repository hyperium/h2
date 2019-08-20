#![feature(async_await)]
#![deny(warnings)]

use futures::future::{join, poll_fn};
use futures::{StreamExt, TryStreamExt};
use h2_support::prelude::*;
use tokio::io::AsyncWriteExt;

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

#[tokio::test]
async fn read_preface_in_multiple_frames() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .read(b"PRI * HTTP/2.0")
        .read(b"\r\n\r\nSM\r\n\r\n")
        .write(SETTINGS)
        .read(SETTINGS)
        .write(SETTINGS_ACK)
        .read(SETTINGS_ACK)
        .build();

    let mut h2 = server::handshake(mock).await.unwrap();

    assert!(h2.next().await.is_none());
}

#[tokio::test]
async fn server_builder_set_max_concurrent_streams() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(1));

    let client = async move {
        let recv_settings = client.assert_server_handshake().await;
        assert_frame_eq(recv_settings, settings);
        client
            .send_frame(frames::headers(1).request("GET", "https://example.com/"))
            .await;
        client
            .send_frame(frames::headers(3).request("GET", "https://example.com/"))
            .await;
        client
            .send_frame(frames::data(1, &b"hello"[..]).eos())
            .await;
        client.recv_frame(frames::reset(3).refused()).await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
    };

    let mut builder = server::Builder::new();
    builder.max_concurrent_streams(1);

    let h2 = async move {
        let mut srv = builder.handshake::<_, Bytes>(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::GET);

        let rsp = http::Response::builder().status(200).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, h2).await;
}

#[tokio::test]
async fn serve_request() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::GET);

        let rsp = http::Response::builder().status(200).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[test]
#[ignore]
fn accept_with_pending_connections_after_socket_close() {}

#[tokio::test]
async fn recv_invalid_authority() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let bad_auth = util::byte_str("not:a/good authority");
    let mut bad_headers: frame::Headers = frames::headers(1)
        .request("GET", "https://example.com/")
        .eos()
        .into();
    bad_headers.pseudo_mut().authority = Some(bad_auth);

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client.send_frame(bad_headers).await;
        client.recv_frame(frames::reset(1).protocol_error()).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn recv_connection_header() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let req = |id, name, val| {
        frames::headers(id)
            .request("GET", "https://example.com/")
            .field(name, val)
            .eos()
    };

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client.send_frame(req(1, "connection", "foo")).await;
        client.send_frame(req(3, "keep-alive", "5")).await;
        client.send_frame(req(5, "proxy-connection", "bar")).await;
        client
            .send_frame(req(7, "transfer-encoding", "chunked"))
            .await;
        client.send_frame(req(9, "upgrade", "HTTP/2.0")).await;
        client.recv_frame(frames::reset(1).protocol_error()).await;
        client.recv_frame(frames::reset(3).protocol_error()).await;
        client.recv_frame(frames::reset(5).protocol_error()).await;
        client.recv_frame(frames::reset(7).protocol_error()).await;
        client.recv_frame(frames::reset(9).protocol_error()).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn sends_reset_cancel_when_req_body_is_dropped() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
        client.recv_frame(frames::reset(1).cancel()).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        {
            let (req, mut stream) = srv.next().await.unwrap().unwrap();
            assert_eq!(req.method(), &http::Method::POST);

            let rsp = http::Response::builder().status(200).body(()).unwrap();
            stream.send_response(rsp, true).unwrap();
        }
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn abrupt_shutdown() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        client.recv_frame(frames::go_away(1).internal_error()).await;
        client.recv_eof().await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, tx) = srv.next().await.unwrap().expect("server receives request");

        let req_fut = async move {
            let body = req.into_body().try_concat().await;
            drop(tx);
            let err = body.expect_err("request body should error");
            assert_eq!(
                err.reason(),
                Some(Reason::INTERNAL_ERROR),
                "streams should be also error with user's reason",
            );
        };

        srv.abrupt_shutdown(Reason::INTERNAL_ERROR);

        let srv_fut = async move {
            poll_fn(move |cx| srv.poll_closed(cx))
                .await
                .expect("server");
        };

        join(req_fut, srv_fut).await;
    };

    join(client, srv).await;
}

#[tokio::test]
async fn graceful_shutdown() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        // 2^31 - 1 = 2147483647
        // Note: not using a constant in the library because library devs
        // can be unsmart.
        client.recv_frame(frames::go_away(2147483647)).await;
        client.recv_frame(frames::ping(frame::Ping::SHUTDOWN)).await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
        // Pretend this stream was sent while the GOAWAY was in flight
        client
            .send_frame(frames::headers(3).request("POST", "https://example.com/"))
            .await;
        client
            .send_frame(frames::ping(frame::Ping::SHUTDOWN).pong())
            .await;
        client.recv_frame(frames::go_away(3)).await;
        // streams sent after GOAWAY receive no response
        client
            .send_frame(frames::headers(7).request("GET", "https://example.com/"))
            .await;
        client.send_frame(frames::data(7, "").eos()).await;
        client.send_frame(frames::data(3, "").eos()).await;
        client
            .recv_frame(frames::headers(3).response(200).eos())
            .await;
        client.recv_eof().await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();
        assert_eq!(req.method(), &http::Method::GET);

        srv.graceful_shutdown();

        let rsp = http::Response::builder().status(200).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        let (req, mut stream) = srv.next().await.unwrap().unwrap();
        assert_eq!(req.method(), &http::Method::POST);
        let body = req.into_parts().1;

        let body = async move {
            let buf = body.try_concat().await.unwrap();
            assert!(buf.is_empty());

            let rsp = http::Response::builder().status(200).body(()).unwrap();
            stream.send_response(rsp, true).unwrap();
        };

        let mut srv = Box::pin(async move {
            assert!(srv.next().await.is_none(), "unexpected request");
        });
        srv.drive(body).await;
        srv.await;
    };

    join(client, srv).await;
}

#[tokio::test]
async fn sends_reset_cancel_when_res_body_is_dropped() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        client.recv_frame(frames::headers(1).response(200)).await;
        client.recv_frame(frames::reset(1).cancel()).await;
        client
            .send_frame(
                frames::headers(3)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        client.recv_frame(frames::headers(3).response(200)).await;
        client.recv_frame(frames::data(3, vec![0; 10])).await;
        client.recv_frame(frames::reset(3).cancel()).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        {
            let (req, mut stream) = srv.next().await.unwrap().unwrap();

            assert_eq!(req.method(), &http::Method::GET);

            let rsp = http::Response::builder().status(200).body(()).unwrap();
            stream.send_response(rsp, false).unwrap();
            // SendStream dropped
        }
        {
            let (_req, mut stream) = srv.next().await.unwrap().unwrap();
            let rsp = http::Response::builder().status(200).body(()).unwrap();
            let mut tx = stream.send_response(rsp, false).unwrap();
            tx.send_data(vec![0; 10].into(), false).unwrap();
            // no send_data with eos
        }

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn too_big_headers_sends_431() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_frame_eq(settings, frames::settings().max_header_list_size(10));
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .field("some-header", "some-value")
                    .eos(),
            )
            .await;
        client
            .recv_frame(frames::headers(1).response(431).eos())
            .await;
        idle_ms(10).await;
    };

    let srv = async move {
        let mut srv = server::Builder::new()
            .max_header_list_size(10)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let req = srv.next().await;
        assert!(req.is_none(), "req is {:?}", req);
    };

    join(client, srv).await;
}

#[tokio::test]
async fn too_big_headers_sends_reset_after_431_if_not_eos() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_frame_eq(settings, frames::settings().max_header_list_size(10));
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .field("some-header", "some-value"),
            )
            .await;
        client
            .recv_frame(frames::headers(1).response(431).eos())
            .await;
        client.recv_frame(frames::reset(1).refused()).await;
    };

    let srv = async move {
        let mut srv = server::Builder::new()
            .max_header_list_size(10)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let req = srv.next().await;
        assert!(req.is_none(), "req is {:?}", req);
    };

    join(client, srv).await;
}

#[tokio::test]
async fn poll_reset() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        idle_ms(10).await;
        client.send_frame(frames::reset(1).cancel()).await;
    };

    let srv = async move {
        let mut srv = server::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let (_req, mut tx) = srv.next().await.expect("server").unwrap();
        let conn = async move {
            let req = srv.next().await;
            assert!(req.is_none(), "no second request");
        };
        join(conn, async move {
            let reason = poll_fn(move |cx| tx.poll_reset(cx))
                .await
                .expect("poll_reset");
            assert_eq!(reason, Reason::CANCEL);
        })
        .await;
    };
    join(client, srv).await;
}

#[tokio::test]
async fn poll_reset_io_error() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        idle_ms(10).await;
    };

    let srv = async move {
        let mut srv = server::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let (_req, mut tx) = srv.next().await.expect("server").unwrap();
        let conn = async move {
            let req = srv.next().await;
            assert!(req.is_none(), "no second request");
        };
        join(conn, async move {
            poll_fn(move |cx| tx.poll_reset(cx))
                .await
                .expect_err("poll_reset should error")
        })
        .await;
    };

    join(client, srv).await;
}

#[tokio::test]
async fn poll_reset_after_send_response_is_user_error() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        client.recv_frame(frames::headers(1).response(200)).await;
        client
            .recv_frame(
                // After the error, our server will drop the handles,
                // meaning we receive a RST_STREAM here.
                frames::reset(1).cancel(),
            )
            .await;
        idle_ms(10).await;
    };

    let srv = async move {
        let mut srv = server::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let (_req, mut tx) = srv.next().await.expect("server").expect("request");
        let conn = async move {
            let req = srv.next().await;
            assert!(req.is_none(), "no second request");
        };
        tx.send_response(Response::new(()), false)
            .expect("response");
        drop(_req);
        join(
            async {
                poll_fn(move |cx| tx.poll_reset(cx))
                    .await
                    .expect_err("poll_reset should error")
            },
            conn,
        )
        .await;
    };

    join(client, srv).await;
}

#[tokio::test]
async fn server_error_on_unclean_shutdown() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let srv = server::Builder::new().handshake::<_, Bytes>(io);

    client.write_all(b"PRI *").await.expect("write");
    drop(client);

    srv.await.expect_err("should error");
}

#[tokio::test]
async fn request_without_authority() {
    let _ = env_logger::try_init();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "/just-a-path")
                    .scheme("http")
                    .eos(),
            )
            .await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();
        assert_eq!(req.uri().path(), "/just-a-path");

        let rsp = Response::new(());
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}
