use futures::StreamExt;
use h2_support::prelude::*;
use std::task::Poll;
use tokio::sync::oneshot;

#[tokio::test]
async fn recv_trailers_only() {
    h2_support::trace_init!();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29, 0xAC, 0x4B, 0x8F,
            0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            0, 0, 1, 1, 4, 0, 0, 0, 1, 0x88, 0, 0, 9, 1, 5, 0, 0, 0, 1, 0x40, 0x84, 0x42, 0x46,
            0x9B, 0x51, 0x82, 0x3F, 0x5F,
        ])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    tracing::info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    let response = h2.run(response).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (_, mut body) = response.into_parts();

    // Make sure there is no body
    let chunk = h2.run(Box::pin(body.next())).await;
    assert!(chunk.is_none());

    let trailers = h2
        .run(poll_fn(|cx| body.poll_trailers(cx)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(1, trailers.len());
    assert_eq!(trailers["status"], "ok");

    h2.await.unwrap();
}

#[tokio::test]
async fn send_trailers_immediately() {
    h2_support::trace_init!();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 4, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29, 0xAC, 0x4B, 0x8F,
            0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84, 0, 0, 0x0A, 1, 5, 0, 0, 0, 1, 0x40, 0x83,
            0xF6, 0x7A, 0x66, 0x84, 0x9C, 0xB4, 0x50, 0x7F,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            0, 0, 1, 1, 4, 0, 0, 0, 1, 0x88, 0, 0, 0x0B, 0, 1, 0, 0, 0, 1, 0x68, 0x65, 0x6C, 0x6C,
            0x6F, 0x20, 0x77, 0x6F, 0x72, 0x6C, 0x64,
        ])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    tracing::info!("sending request");
    let (response, mut stream) = client.send_request(request, false).unwrap();

    let mut trailers = HeaderMap::new();
    trailers.insert("zomg", "hello".parse().unwrap());

    stream.send_trailers(trailers).unwrap();

    let response = h2.run(response).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (_, mut body) = response.into_parts();

    // There is a data chunk
    let _ = h2.run(body.next()).await.unwrap().unwrap();

    let chunk = h2.run(body.next()).await;
    assert!(chunk.is_none());

    let trailers = h2.run(poll_fn(|cx| body.poll_trailers(cx))).await.unwrap();
    assert!(trailers.is_none());

    h2.await.unwrap();
}

#[test]
#[ignore]
fn recv_trailers_without_eos() {
    // This should be a protocol error?
}

#[tokio::test]
async fn poll_trailers_before_data_is_consumed() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();
    let (frames_ready_tx, frames_ready_rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);

        // 2. Receive the request.
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;

        // 3. Send response HEADERS followed by DATA and trailers.
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, "hello")).await;
        srv.send_frame(frames::headers(1).field("trailer-key", "trailer-val").eos())
            .await;

        // 4. Ensure all preceding frames have been processed by the client.
        srv.ping_pong([1; 8]).await;
        frames_ready_tx.send(()).unwrap();
    };

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");
        let conn = tokio::spawn(async move {
            conn.await.expect("client");
        });

        // 1. Send the request and wait for response HEADERS.
        let resp = client.get("https://example.com/").await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let mut body = resp.into_body();
        frames_ready_rx.await.unwrap();
        let mut first_poll = true;

        let trailers = tokio::time::timeout(
            Duration::from_secs(1),
            poll_fn(|cx| {
                if first_poll {
                    // 5. Poll trailers while DATA is at the front of pending_recv.
                    // This returns Pending and registers this future's waker.
                    first_poll = false;
                    assert!(
                        matches!(body.poll_trailers(cx), Poll::Pending),
                        "poll_trailers should be Pending when DATA is buffered"
                    );

                    // 6. Consume the DATA frame. The next poll reaches the
                    // queued trailers and wakes the waker registered in 5.
                    match body.poll_data(cx) {
                        Poll::Ready(Some(Ok(data))) => assert_eq!(data, "hello"),
                        other => panic!("expected DATA, got {:?}", other),
                    }
                    assert!(matches!(body.poll_data(cx), Poll::Ready(None)));

                    Poll::Pending
                } else {
                    // 7. This future must only be polled again after
                    // poll_data's notify_recv wakes it.
                    body.poll_trailers(cx)
                }
            })
            .wakened(),
        )
        .await
        .expect("poll_trailers was not woken")
        .expect("trailers result")
        .expect("should have trailers");

        assert_eq!(trailers["trailer-key"], "trailer-val");

        conn.await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn send_trailers_rejects_connection_specific_headers() {
    // RFC 9113 §8.2.2: endpoints MUST NOT *generate* an HTTP/2 message containing
    // connection-specific header fields. That obligation applies to trailer HEADERS
    // blocks just as it does to the main header block. `send_headers` already rejects
    // these; this test pins the same behavior for `send_trailers` (previously the only
    // send path that let them through onto the wire).
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        // The client opens the stream, then locally rejects every bad trailer (nothing
        // hits the wire for those), and finally sends a *valid* trailer to close cleanly.
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.recv_frame(frames::headers(1).field("x-trailer", "ok").eos())
            .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method("POST")
            .uri("https://example.com/")
            .body(())
            .unwrap();
        let (response, mut stream) = client.send_request(request, false).unwrap();

        // Every connection-specific header must be rejected in a trailer block.
        for name in [
            "connection",
            "keep-alive",
            "proxy-connection",
            "transfer-encoding",
            "upgrade",
        ] {
            let mut trailers = HeaderMap::new();
            trailers.insert(name, "x".parse().unwrap());
            let err = stream.send_trailers(trailers).expect_err(name);
            assert_eq!(err.to_string(), "user error: malformed headers");
        }
        // TE is connection-specific unless it is exactly `TE: trailers`.
        let mut te_bad = HeaderMap::new();
        te_bad.insert("te", "chunked".parse().unwrap());
        let err = stream.send_trailers(te_bad).expect_err("te: chunked");
        assert_eq!(err.to_string(), "user error: malformed headers");

        // The rejections above must not have corrupted stream state: a clean trailer
        // still sends and the exchange completes normally.
        let mut good = HeaderMap::new();
        good.insert("x-trailer", "ok".parse().unwrap());
        stream
            .send_trailers(good)
            .expect("valid trailer should send");

        let response = conn.drive(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        conn.await.unwrap();
    };

    join(srv, client).await;
}
