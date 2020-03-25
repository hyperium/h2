#![deny(warnings)]

use futures::future::{join, join3, lazy, try_join};
use futures::{FutureExt, StreamExt, TryStreamExt};
use h2_support::prelude::*;
use h2_support::util::yield_once;
use std::task::Poll;
use tokio::sync::oneshot;

#[tokio::test]
async fn send_recv_headers_only() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29, 0xAC, 0x4B, 0x8F,
            0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    log::info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.await.unwrap();
}

#[tokio::test]
async fn send_recv_data() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25, 151,
            33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 0, 5, 0, 1, 0, 0, 0, 1, 104, 101, 108, 108, 111,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            // HEADERS
            0, 0, 1, 1, 4, 0, 0, 0, 1, 136, // DATA
            0, 0, 5, 0, 1, 0, 0, 0, 1, 119, 111, 114, 108, 100,
        ])
        .build();

    let (mut client, mut h2) = client::Builder::new().handshake(mock).await.unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    log::info!("sending request");
    let (response, mut stream) = client.send_request(request, false).unwrap();

    // Reserve send capacity
    stream.reserve_capacity(5);

    assert_eq!(stream.capacity(), 5);

    // Send the data
    stream.send_data("hello".as_bytes(), true).unwrap();

    // Get the response
    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Take the body
    let (_, body) = resp.into_parts();

    // Wait for all the data frames to be received
    let bytes: Vec<_> = h2.run(body.try_collect()).await.unwrap();

    // One byte chunk
    assert_eq!(1, bytes.len());

    assert_eq!(bytes[0], &b"world"[..]);

    // The H2 connection is closed
    h2.await.unwrap();
}

#[tokio::test]
async fn send_headers_recv_data_single_frame() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 16, 1, 5, 0, 0, 0, 1, 130, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25,
            151, 33, 233, 132,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            0, 0, 1, 1, 4, 0, 0, 0, 1, 136, 0, 0, 5, 0, 0, 0, 0, 0, 1, 104, 101, 108, 108, 111, 0,
            0, 5, 0, 1, 0, 0, 0, 1, 119, 111, 114, 108, 100,
        ])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    log::info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Take the body
    let (_, body) = resp.into_parts();

    // Wait for all the data frames to be received
    let bytes: Vec<_> = h2.run(body.try_collect()).await.unwrap();

    // Two data frames
    assert_eq!(2, bytes.len());

    assert_eq!(bytes[0], &b"hello"[..]);
    assert_eq!(bytes[1], &b"world"[..]);

    // The H2 connection is closed
    h2.await.unwrap();
}

#[tokio::test]
async fn closed_streams_are_released() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = async move {
        let (mut client, mut h2) = client::handshake(io).await.unwrap();
        let request = Request::get("https://example.com/").body(()).unwrap();

        // Send request
        let (response, _) = client.send_request(request, true).unwrap();
        let response = h2.drive(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // There are no active streams
        assert_eq!(0, client.num_active_streams());

        // The response contains a handle for the body. This keeps the
        // stream wired.
        assert_eq!(1, client.num_wired_streams());

        let (_, body) = response.into_parts();
        assert!(body.is_end_stream());
        drop(body);

        // The stream state is now free
        assert_eq!(0, client.num_wired_streams());
    };

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(204).eos()).await;
    };
    join(srv, h2).await;
}

#[tokio::test]
async fn errors_if_recv_frame_exceeds_max_frame_size() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = async move {
        let (mut client, h2) = client::handshake(io).await.unwrap();
        let req = async move {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_parts().1;
            let res = util::concat(body).await;
            let err = res.unwrap_err();
            assert_eq!(err.to_string(), "protocol error: frame with invalid size");
        };

        // client should see a conn error
        let conn = async move {
            let err = h2.await.unwrap_err();
            assert_eq!(err.to_string(), "protocol error: frame with invalid size");
        };
        join(conn, req).await;
    };

    // a bad peer
    srv.codec_mut().set_max_send_frame_size(16_384 * 4);

    let srv = async move {
        let _ = srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_385]).eos()).await;
        srv.recv_frame(frames::go_away(0).frame_size()).await;
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn configure_max_frame_size() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = async move {
        let (mut client, h2) = client::Builder::new()
            .max_frame_size(16_384 * 2)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let req = async move {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_parts().1;
            let buf = util::concat(body).await.expect("body");
            assert_eq!(buf.len(), 16_385);
        };

        join(async move { h2.await.expect("client") }, req).await;
    };
    // a good peer
    srv.codec_mut().set_max_send_frame_size(16_384 * 2);

    let srv = async move {
        let _ = srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_385]).eos()).await;
    };
    join(srv, h2).await;
}

#[tokio::test]
async fn recv_goaway_finishes_processed_streams() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::go_away(1)).await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_384]).eos()).await;
        // expecting a goaway of 0, since server never initiated a stream
        srv.recv_frame(frames::go_away(0)).await;
        //.close();
    };

    let h2 = async move {
        let (mut client, h2) = client::handshake(io).await.expect("handshake");
        let mut client_clone = client.clone();
        let req1 = async move {
            let resp = client_clone
                .get("https://example.com")
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_parts().1;
            let buf = util::concat(body).await.expect("body");
            assert_eq!(buf.len(), 16_384);
        };

        // this request will trigger a goaway
        let req2 = async move {
            let err = client.get("https://example.com/").await.unwrap_err();
            assert_eq!(err.to_string(), "protocol error: not a result of an error");
        };

        join3(async move { h2.await.expect("client") }, req1, req2).await;
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn recv_goaway_with_higher_last_processed_id() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::go_away(1)).await;
        // a bigger goaway? kaboom
        srv.send_frame(frames::go_away(3)).await;
        // expecting a goaway of 0, since server never initiated a stream
        srv.recv_frame(frames::go_away(0).protocol_error()).await;
        //.close();
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let err = conn
            .drive(client.get("https://example.com"))
            .await
            .expect_err("client should error");
        assert_eq!(err.reason(), Some(Reason::PROTOCOL_ERROR));
    };

    join(srv, client).await;
}

#[tokio::test]
async fn recv_next_stream_id_updated_by_malformed_headers() {
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
        // bad headers -- should error.
        client.send_frame(bad_headers).await;
        client.recv_frame(frames::reset(1).protocol_error()).await;
        // this frame is good, but the stream id should already have been incr'd
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
        client.recv_frame(frames::go_away(1).protocol_error()).await;
    };
    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let res = srv.next().await.unwrap();
        let err = res.unwrap_err();
        assert_eq!(err.reason(), Some(h2::Reason::PROTOCOL_ERROR));
    };

    join(srv, client).await;
}

#[tokio::test]
async fn skipped_stream_ids_are_implicitly_closed() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(5)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        // send the response on a lower-numbered stream, which should be
        // implicitly closed.
        srv.send_frame(frames::headers(3).response(299)).await;
        // however, our client choose to send a RST_STREAM because it
        // can't tell if it had previously reset '3'.
        srv.recv_frame(frames::reset(3).stream_closed()).await;
        srv.send_frame(frames::headers(5).response(200).eos()).await;
    };

    let h2 = async move {
        let (mut client, mut h2) = client::Builder::new()
            .initial_stream_id(5)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let req = async move {
            let res = client.get("https://example.com/").await.expect("response");
            assert_eq!(res.status(), StatusCode::OK);
        };
        h2.drive(req).await;
        h2.await.expect("client");
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn send_rst_stream_allows_recv_data() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.recv_frame(frames::reset(1).cancel()).await;
        // sending frames after canceled!
        //   note: sending 2 to cosume 50% of connection window
        srv.send_frame(frames::data(1, vec![0; 16_384])).await;
        srv.send_frame(frames::data(1, vec![0; 16_384]).eos()).await;
        // make sure we automatically free the connection window
        srv.recv_frame(frames::window_update(0, 16_384 * 2)).await;
        // do a pingpong to ensure no other frames were sent
        srv.ping_pong([1; 8]).await;
    };

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");
        let req = async {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            // drop resp will send a reset
        };

        let mut conn = Box::pin(async move {
            conn.await.expect("client");
        });
        conn.drive(req).await;
        conn.await;
        drop(client);
    };

    join(srv, client).await;
}

#[tokio::test]
async fn send_rst_stream_allows_recv_trailers() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_384])).await;
        srv.recv_frame(frames::reset(1).cancel()).await;
        // sending frames after canceled!
        srv.send_frame(frames::headers(1).field("foo", "bar").eos())
            .await;
        // do a pingpong to ensure no other frames were sent
        srv.ping_pong([1; 8]).await;
    };

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");
        let req = async {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            // drop resp will send a reset
        };

        let mut conn = Box::pin(async move { conn.await.expect("client") });
        conn.drive(req).await;
        conn.await;
        drop(client);
    };

    join(srv, client).await;
}

#[tokio::test]
async fn rst_stream_expires() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_384])).await;
        srv.recv_frame(frames::reset(1).cancel()).await;
        // wait till after the configured duration
        idle_ms(15).await;
        srv.ping_pong([1; 8]).await;
        // sending frame after canceled!
        srv.send_frame(frames::data(1, vec![0; 16_384]).eos()).await;
        // window capacity is returned
        srv.recv_frame(frames::window_update(0, 16_384 * 2)).await;
        // and then stream error
        srv.recv_frame(frames::reset(1).stream_closed()).await;
    };

    let client = async move {
        let (mut client, conn) = client::Builder::new()
            .reset_stream_duration(Duration::from_millis(10))
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let req = async {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
            // drop resp will send a reset
        };

        // no connection error should happen
        let mut conn = Box::pin(async move { conn.await.expect("client") });
        conn.drive(req).await;
        conn.await;
        drop(client);
    };

    join(srv, client).await;
}

#[tokio::test]
async fn rst_stream_max() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16])).await;
        srv.send_frame(frames::headers(3).response(200)).await;
        srv.send_frame(frames::data(3, vec![0; 16])).await;
        srv.recv_frame(frames::reset(1).cancel()).await;
        srv.recv_frame(frames::reset(3).cancel()).await;
        // sending frame after canceled!
        // newer streams trump older streams
        // 3 is still being ignored
        srv.send_frame(frames::data(3, vec![0; 16]).eos()).await;
        // ping pong to be sure of no goaway
        srv.ping_pong([1; 8]).await;
        // 1 has been evicted, will get a reset
        srv.send_frame(frames::data(1, vec![0; 16]).eos()).await;
        srv.recv_frame(frames::reset(1).stream_closed()).await;
    };

    let client = async move {
        let (mut client, conn) = client::Builder::new()
            .max_concurrent_reset_streams(1)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let mut client_clone = client.clone();
        let req1 = async move {
            let resp = client_clone
                .get("https://example.com/")
                .await
                .expect("response1");
            assert_eq!(resp.status(), StatusCode::OK);
            // drop resp will send a reset
        };

        let req2 = async {
            let resp = client.get("https://example.com/").await.expect("response2");
            assert_eq!(resp.status(), StatusCode::OK);
            // drop resp will send a reset
        };

        // no connection error should happen
        let mut conn = Box::pin(async move {
            conn.await.expect("client");
        });
        conn.drive(join(req1, req2)).await;
        conn.await;
        drop(client);
    };

    join(srv, client).await;
}

#[tokio::test]
async fn reserved_state_recv_window_update() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::push_promise(1, 2).request("GET", "https://example.com/push"))
            .await;
        // it'd be weird to send a window update on a push promise,
        // since the client can't send us data, but whatever. The
        // point is that it's allowed, so we're testing it.
        srv.send_frame(frames::window_update(2, 128)).await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        // ping pong to ensure no goaway
        srv.ping_pong([1; 8]).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let req = async move {
            let resp = client.get("https://example.com/").await.expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
        };

        conn.drive(req).await;
        conn.await.expect("client");
    };

    join(srv, client).await;
}
/*
#[test]
fn send_data_after_headers_eos() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            // GET /, no EOS
            0, 0, 16, 1, 5, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172,
            75, 143, 168, 233, 25, 151, 33, 233, 132
        ])
        .build();

    let h2 = client::handshake(mock)
        .await.expect("handshake");

    // Send the request
    let mut request = request::Head::default();
    request.method = Method::POST;
    request.uri = "https://http2.akamai.com/".parse().unwrap();

    let id = 1.into();
    let h2 = h2.send_request(id, request, true).await.expect("send request");

    let body = "hello";

    // Send the data
    let err = h2.send_data(id, body.into(), true).await.unwrap_err();
    assert_user_err!(err, UnexpectedFrameType);
}

#[test]
#[ignore]
fn exceed_max_streams() {
}
*/

#[tokio::test]
async fn rst_while_closing() {
    // Test to reproduce panic in issue #246 --- receipt of a RST_STREAM frame
    // on a stream in the Half Closed (remote) state with a queued EOS causes
    // a panic.
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    // Rendevous when we've queued a trailers frame
    let (tx, rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::headers(1).eos()).await;
        // Idling for a moment here is necessary to ensure that the client
        // enqueues its TRAILERS frame *before* we send the RST_STREAM frame
        // which causes the panic.
        rx.await.unwrap();
        // Send the RST_STREAM frame which causes the client to panic.
        srv.send_frame(frames::reset(1).cancel()).await;
        srv.ping_pong([1; 8]).await;
        srv.recv_frame(frames::go_away(0).no_error()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // The request should be left streaming.
        let req = async move {
            let (resp, stream) = client.send_request(request, false).expect("send_request");
            // on receipt of an EOS response from the server, transition
            // the stream Open => Half Closed (remote).
            let resp = resp.await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            stream
        };
        let mut stream = conn.drive(req).await;
        // Enqueue trailers frame.
        let _ = stream.send_trailers(HeaderMap::new());
        // Signal the server mock to send RST_FRAME
        let _ = tx.send(()).unwrap();
        drop(stream);
        yield_once().await;
        // yield once to allow the server mock to be polled
        // before the conn flushes its buffer
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn rst_with_buffered_data() {
    // Data is buffered in `FramedWrite` and the stream is reset locally before
    // the data is fully flushed. Given that resetting a stream requires
    // clearing all associated state for that stream, this test ensures that the
    // buffered up frame is correctly handled.
    let _ = env_logger::try_init();

    // This allows the settings + headers frame through
    let (io, mut srv) = mock::new_with_write_capacity(73);

    // Synchronize the client / server on response
    let (tx, rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.buffer_bytes(128).await;
        srv.send_frame(frames::headers(1).response(204).eos()).await;
        srv.send_frame(frames::reset(1).cancel()).await;
        rx.await.unwrap();
        srv.unbounded_bytes().await;
        srv.recv_frame(frames::data(1, vec![0; 16_384])).await;
    };

    // A large body
    let body = vec![0u8; 2 * frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // Send the request
        let (resp, mut stream) = client.send_request(request, false).expect("send_request");

        // Send the data
        stream.send_data(body.into(), true).unwrap();

        conn.drive(resp).await.ok();
        tx.send(()).unwrap();
        conn.await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn err_with_buffered_data() {
    // Data is buffered in `FramedWrite` and the stream is reset locally before
    // the data is fully flushed. Given that resetting a stream requires
    // clearing all associated state for that stream, this test ensures that the
    // buffered up frame is correctly handled.
    let _ = env_logger::try_init();

    // This allows the settings + headers frame through
    let (io, mut srv) = mock::new_with_write_capacity(73);

    // Synchronize the client / server on response
    let (tx, rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.buffer_bytes(128).await;
        srv.send_frame(frames::headers(1).response(204).eos()).await;
        // Send invalid data
        srv.send_bytes(b"\x00\x00\x00\x00\x00\x00\x00\x00\x00")
            .await;
        rx.await.unwrap();
        srv.unbounded_bytes().await;
        srv.recv_frame(frames::data(1, vec![0; 16_384])).await;
    };

    // A large body
    let body = vec![0; 2 * frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // Send the request
        let (resp, mut stream) = client.send_request(request, false).expect("send_request");

        // Send the data
        stream.send_data(body.into(), true).unwrap();

        drop(client);
        let res = try_join(conn, resp).await;
        assert!(res.is_err());
        tx.send(()).unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn send_err_with_buffered_data() {
    // Data is buffered in `FramedWrite` and the stream is reset locally before
    // the data is fully flushed. Given that resetting a stream requires
    // clearing all associated state for that stream, this test ensures that the
    // buffered up frame is correctly handled.
    let _ = env_logger::try_init();

    // This allows the settings + headers frame through
    let (io, mut srv) = mock::new_with_write_capacity(73);

    // Synchronize the client / server on response
    let (tx, rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.buffer_bytes(128).await;
        srv.send_frame(frames::headers(1).response(204).eos()).await;
        rx.await.unwrap();
        srv.unbounded_bytes().await;
        srv.recv_frame(frames::data(1, vec![0; 16_384])).await;
        srv.recv_frame(frames::reset(1).cancel()).await;
        srv.recv_frame(frames::go_away(0).no_error()).await;
    };

    // A large body
    let body = vec![0; 2 * frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // Send the request
        let (resp, mut stream) = client.send_request(request, false).expect("send_request");

        // Send the data
        stream.send_data(body.into(), true).unwrap();

        // Hack to drive the connection, trying to flush data
        lazy(|cx| {
            if let Poll::Ready(v) = conn.poll_unpin(cx) {
                v.unwrap();
            }
        })
        .await;

        // Send a reset
        stream.send_reset(Reason::CANCEL);
        drop(stream);
        drop(client);
        conn.drive(resp).await.ok();
        tx.send(()).unwrap();
        conn.await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn srv_window_update_on_lower_stream_id() {
    // See https://github.com/hyperium/h2/issues/208
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(7)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(
            frames::push_promise(7, 2).request("GET", "https://http2.akamai.com/style.css"),
        )
        .await;
        srv.send_frame(frames::headers(7).eos()).await;
        srv.recv_frame(frames::reset(2).cancel()).await;
        srv.send_frame(frames::window_update(5, 66666)).await;
    };

    let client = async move {
        let (mut client, mut h2) = client::Builder::new()
            .initial_stream_id(7)
            .handshake::<_, Bytes>(io)
            .await
            .unwrap();
        let request = Request::builder()
            .method("GET")
            .uri("https://example.com/")
            .body(())
            .unwrap();

        let response = async {
            let resp = client
                .send_request(request, true)
                .unwrap()
                .0
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::OK);
        };

        h2.drive(response).await;
        println!("RESPONSE DONE");
        h2.await.expect("client");
        drop(client);
    };
    join(srv, client).await;
}
