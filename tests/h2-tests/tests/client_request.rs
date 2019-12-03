use futures::future::{join, ready, select, Either};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use h2_support::prelude::*;
use std::pin::Pin;
use std::task::Context;

#[tokio::test]
async fn handshake() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(SETTINGS_ACK)
        .build();

    let (_client, h2) = client::handshake(mock).await.unwrap();

    log::trace!("hands have been shook");

    // At this point, the connection should be closed
    h2.await.unwrap();
}

#[tokio::test]
async fn client_other_thread() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let h2 = async move {
        let (mut client, h2) = client::handshake(io).await.unwrap();
        tokio::spawn(async move {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();
            let _res = client
                .send_request(request, true)
                .unwrap()
                .0
                .await
                .expect("request");
        });
        h2.await.expect("h2");
    };
    join(srv, h2).await;
}

#[tokio::test]
async fn recv_invalid_server_stream_id() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29, 0xAC, 0x4B, 0x8F,
            0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .write(SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 2, 137])
        // Write GO_AWAY
        .write(&[0, 0, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
        .build();

    let (mut client, h2) = client::handshake(mock).await.unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    log::info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    // The connection errors
    assert!(h2.await.is_err());

    // The stream errors
    assert!(response.await.is_err());
}

#[tokio::test]
async fn request_stream_id_overflows() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = async move {
        let (mut client, mut h2) = client::Builder::new()
            .initial_stream_id(::std::u32::MAX >> 1)
            .handshake::<_, Bytes>(io)
            .await
            .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // first request is allowed
        let (response, _) = client.send_request(request, true).unwrap();
        let _x = h2.drive(response).await.unwrap();

        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(())
            .unwrap();
        // second cannot use the next stream id, it's over
        let poll_err = poll_fn(|cx| client.poll_ready(cx)).await.unwrap_err();
        assert_eq!(poll_err.to_string(), "user error: stream ID overflowed");

        let err = client.send_request(request, true).unwrap_err();
        assert_eq!(err.to_string(), "user error: stream ID overflowed");

        h2.await.unwrap();
    };

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(::std::u32::MAX >> 1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(::std::u32::MAX >> 1).response(200).eos())
            .await;
        idle_ms(10).await;
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn client_builder_max_concurrent_streams() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(1));

    let srv = async move {
        let rcvd_settings = srv.assert_client_handshake().await;
        assert_frame_eq(settings, rcvd_settings);

        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let mut builder = client::Builder::new();
    builder.max_concurrent_streams(1);

    let h2 = async move {
        let (mut client, mut h2) = builder.handshake::<_, Bytes>(io).await.unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(())
            .unwrap();
        let (response, _) = client.send_request(request, true).unwrap();
        h2.drive(response).await.unwrap();
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn request_over_max_concurrent_streams_errors() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv
            .assert_client_handshake_with_settings(
                frames::settings()
                    // super tiny server
                    .max_concurrent_streams(1),
            )
            .await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("POST", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        srv.recv_frame(frames::headers(3).request("POST", "https://example.com/"))
            .await;
        srv.send_frame(frames::headers(3).response(200)).await;
        srv.recv_frame(frames::data(3, "hello").eos()).await;
        srv.send_frame(frames::data(3, "").eos()).await;
        srv.recv_frame(frames::headers(5).request("POST", "https://example.com/"))
            .await;
        srv.send_frame(frames::headers(5).response(200)).await;
        srv.recv_frame(frames::data(5, "hello").eos()).await;
        srv.send_frame(frames::data(5, "").eos()).await;
    };

    let h2 = async move {
        let (mut client, mut h2) = client::handshake(io).await.expect("handshake");
        // we send a simple req here just to drive the connection so we can
        // receive the server settings.
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();
        // first request is allowed
        let (response, _) = client.send_request(request, true).unwrap();
        h2.drive(response).await.unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // first request is allowed
        let (resp1, mut stream1) = client.send_request(request, false).unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // second request is put into pending_open
        let (resp2, mut stream2) = client.send_request(request, false).unwrap();

        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        // third stream is over max concurrent
        assert!(!client.poll_ready(&mut cx).is_ready());

        let err = client.send_request(request, true).unwrap_err();
        assert_eq!(err.to_string(), "user error: rejected");

        stream1
            .send_data("hello".into(), true)
            .expect("req send_data");

        h2.drive(async move {
            resp1.await.expect("req");
            stream2
                .send_data("hello".into(), true)
                .expect("req2 send_data");
        })
        .await;
        join(async move { h2.await.unwrap() }, async move {
            resp2.await.unwrap()
        })
        .await;
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn send_request_poll_ready_when_connection_error() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv
            .assert_client_handshake_with_settings(
                frames::settings()
                    // super tiny server
                    .max_concurrent_streams(1),
            )
            .await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("POST", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        srv.recv_frame(
            frames::headers(3)
                .request("POST", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(8).response(200).eos()).await;
    };

    let h2 = async move {
        let (mut client, mut h2) = client::handshake(io).await.expect("handshake");
        // we send a simple req here just to drive the connection so we can
        // receive the server settings.
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // first request is allowed
        let (response, _) = client.send_request(request, true).unwrap();
        h2.drive(response).await.unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // first request is allowed
        let (resp1, _) = client.send_request(request, true).unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // second request is put into pending_open
        let (resp2, _) = client.send_request(request, true).unwrap();

        // third stream is over max concurrent
        let until_ready = async move {
            poll_fn(move |cx| client.poll_ready(cx))
                .await
                .expect_err("client poll_ready");
        };

        // a FuturesUnordered is used on purpose!
        //
        // We don't want a join, since any of the other futures notifying
        // will make the until_ready future polled again, but we are
        // specifically testing that until_ready gets notified on its own.
        let mut unordered =
            futures::stream::FuturesUnordered::<Pin<Box<dyn Future<Output = ()>>>>::new();
        unordered.push(Box::pin(until_ready));
        unordered.push(Box::pin(async move {
            h2.await.expect_err("client conn");
        }));
        unordered.push(Box::pin(async move {
            resp1.await.expect_err("req1");
        }));
        unordered.push(Box::pin(async move {
            resp2.await.expect_err("req2");
        }));

        while let Some(_) = unordered.next().await {}
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn send_reset_notifies_recv_stream() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.recv_frame(frames::reset(1).refused()).await;
        srv.recv_frame(frames::go_away(0)).await;
        srv.recv_eof().await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        // first request is allowed
        let (resp1, mut tx) = client.send_request(request, false).unwrap();
        let res = conn.drive(resp1).await.unwrap();

        let tx = async move {
            tx.send_reset(h2::Reason::REFUSED_STREAM);
        };
        let rx = async {
            let mut body = res.into_body();
            body.next().await.unwrap().expect_err("RecvBody");
        };

        // a FuturesUnordered is used on purpose!
        //
        // We don't want a join, since any of the other futures notifying
        // will make the rx future polled again, but we are
        // specifically testing that rx gets notified on its own.
        let unordered = FuturesUnordered::<Pin<Box<dyn Future<Output = ()>>>>::new();
        unordered.push(Box::pin(rx));
        unordered.push(Box::pin(tx));

        conn.drive(unordered.for_each(ready)).await;
        drop(client); // now let client gracefully goaway
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn http_11_request_without_scheme_or_authority() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("GET", "/").scheme("http").eos())
            .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let h2 = async move {
        let (mut client, mut h2) = client::handshake(io).await.expect("handshake");

        // HTTP_11 request with just :path is allowed
        let request = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(())
            .unwrap();

        let (response, _) = client.send_request(request, true).unwrap();
        h2.drive(response).await.unwrap();
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn http_2_request_without_scheme_or_authority() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
    };

    let h2 = async move {
        let (mut client, h2) = client::handshake(io).await.expect("handshake");

        // HTTP_2 with only a :path is illegal, so this request should
        // be rejected as a user error.
        let request = Request::builder()
            .version(Version::HTTP_2)
            .method(Method::GET)
            .uri("/")
            .body(())
            .unwrap();

        client
            .send_request(request, true)
            .expect_err("should be UserError");
        let ret = h2.await.expect("h2");
        drop(client);
        ret
    };

    join(srv, h2).await;
}

#[test]
#[ignore]
fn request_with_h1_version() {}

#[tokio::test]
async fn request_with_connection_headers() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    // can't assert full handshake, since client never sends a request, and
    // thus never bothers to ack the settings...
    let srv = async move {
        srv.read_preface().await.unwrap();
        srv.recv_frame(frames::settings()).await;
        // goaway is required to make sure the connection closes because
        // of no active streams
        srv.recv_frame(frames::go_away(0)).await;
    };

    let headers = vec![
        ("connection", "foo"),
        ("keep-alive", "5"),
        ("proxy-connection", "bar"),
        ("transfer-encoding", "chunked"),
        ("upgrade", "HTTP/2.0"),
        ("te", "boom"),
    ];

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");

        for (name, val) in headers {
            let req = Request::builder()
                .uri("https://http2.akamai.com/")
                .header(name, val)
                .body(())
                .unwrap();
            let err = client.send_request(req, true).expect_err(name);
            assert_eq!(err.to_string(), "user error: malformed headers");
        }
        drop(client);
        conn.await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn connection_close_notifies_response_future() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();
    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        // don't send any response, just close
    };

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = async move {
            let res = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .await;
            let err = res.expect_err("response");
            assert_eq!(err.to_string(), "broken pipe");
        };
        join(async move { conn.await.expect("conn") }, req).await;
    };

    join(srv, client).await;
}

#[tokio::test]
async fn connection_close_notifies_client_poll_ready() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = async {
            let res = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .await;
            let err = res.expect_err("response");
            assert_eq!(err.to_string(), "broken pipe");
        };

        conn.drive(req).await;

        let err = poll_fn(move |cx| client.poll_ready(cx))
            .await
            .expect_err("poll_ready");
        assert_eq!(err.to_string(), "broken pipe");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn sending_request_on_closed_connection() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        // a bad frame!
        srv.send_frame(frames::headers(0).response(200).eos()).await;
    };

    let h2 = async move {
        let (mut client, h2) = client::handshake(io).await.expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // first request works
        let req = Box::pin(async {
            client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .await
                .expect("response1");
        });

        // after finish request1, there should be a conn error
        let h2 = Box::pin(async move {
            h2.await.expect_err("h2 error");
        });

        match select(h2, req).await {
            Either::Left((_, req)) => req.await,
            Either::Right((_, _h2)) => unreachable!("Shouldn't happen"), // TODO: Is this correct?
        };

        let poll_err = poll_fn(|cx| client.poll_ready(cx)).await.unwrap_err();
        let msg = "protocol error: unspecific protocol error detected";
        assert_eq!(poll_err.to_string(), msg);

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let send_err = client.send_request(request, true).unwrap_err();
        assert_eq!(send_err.to_string(), msg);
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn recv_too_big_headers() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_frame_eq(settings, frames::settings().max_header_list_size(10));
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        srv.send_frame(frames::headers(3).response(200)).await;
        // no reset for 1, since it's closed anyways
        // but reset for 3, since server hasn't closed stream
        srv.recv_frame(frames::reset(3).refused()).await;
        idle_ms(10).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::Builder::new()
            .max_header_list_size(10)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req1 = client.send_request(request, true);
        let req1 = async move {
            let err = req1.expect("send_request").0.await.expect_err("response1");
            assert_eq!(err.reason(), Some(Reason::REFUSED_STREAM));
        };

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req2 = client.send_request(request, true);
        let req2 = async move {
            let err = req2.expect("send_request").0.await.expect_err("response2");
            assert_eq!(err.reason(), Some(Reason::REFUSED_STREAM));
        };

        conn.drive(join(req1, req2)).await;
        conn.await.expect("client");
    };
    join(srv, client).await;
}

#[tokio::test]
async fn pending_send_request_gets_reset_by_peer_properly() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let payload = Bytes::from(vec![0; (frame::DEFAULT_INITIAL_WINDOW_SIZE * 2) as usize]);
    let max_frame_size = frame::DEFAULT_MAX_FRAME_SIZE as usize;

    let srv = async {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("GET", "https://http2.akamai.com/"))
            .await;
        // Note that we can only send up to ~4 frames of data by default
        srv.recv_frame(frames::data(1, &payload[0..max_frame_size]))
            .await;
        srv.recv_frame(frames::data(
            1,
            &payload[max_frame_size..(max_frame_size * 2)],
        ))
        .await;
        srv.recv_frame(frames::data(
            1,
            &payload[(max_frame_size * 2)..(max_frame_size * 3)],
        ))
        .await;
        srv.recv_frame(frames::data(
            1,
            &payload[(max_frame_size * 3)..(max_frame_size * 4 - 1)],
        ))
        .await;

        idle_ms(100).await;

        srv.send_frame(frames::reset(1).refused()).await;
        // Because all active requests are finished, connection should shutdown
        // and send a GO_AWAY frame. If the reset stream is bugged (and doesn't
        // count towards concurrency limit), then connection will not send
        // a GO_AWAY and this test will fail.
        srv.recv_frame(frames::go_away(0)).await;
        drop(srv);
    };

    let client = async {
        let (mut client, mut conn) = client::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let (response, mut stream) = client.send_request(request, false).expect("send_request");

        let response = async move {
            let err = response.await.expect_err("response");
            assert_eq!(err.reason(), Some(Reason::REFUSED_STREAM));
        };

        // Send the data
        stream.send_data(payload.clone(), true).unwrap();
        conn.drive(response).await;
        drop(client);
        drop(stream);
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn request_without_path() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);

        srv.recv_frame(
            frames::headers(1)
                .request("GET", "http://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        // Note the lack of trailing slash.
        let request = Request::get("http://example.com").body(()).unwrap();

        let (response, _) = client.send_request(request, true).unwrap();

        conn.drive(response).await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn request_options_with_star() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    // Note the lack of trailing slash.
    let uri = uri::Uri::from_parts({
        let mut parts = uri::Parts::default();
        parts.scheme = Some(uri::Scheme::HTTP);
        parts.authority = Some(uri::Authority::from_static("example.com"));
        parts.path_and_query = Some(uri::PathAndQuery::from_static("*"));
        parts
    })
    .unwrap();

    let uri_clone = uri.clone();
    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("OPTIONS", uri_clone).eos())
            .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri(uri)
            .body(())
            .unwrap();

        let (response, _) = client.send_request(request, true).unwrap();

        conn.drive(response).await.unwrap();
    };

    join(srv, client).await;
}

#[tokio::test]
async fn notify_on_send_capacity() {
    // This test ensures that the client gets notified when there is additional
    // send capacity. In other words, when the server is ready to accept a new
    // stream, the client is notified.
    use tokio::sync::oneshot;

    let _ = env_logger::try_init();

    let (io, mut srv) = mock::new();
    let (done_tx, done_rx) = oneshot::channel();
    let (tx, rx) = oneshot::channel();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(1));

    let srv = async move {
        let settings = srv.assert_client_handshake_with_settings(settings).await;
        // This is the ACK
        assert_default_settings!(settings);
        tx.send(()).unwrap();
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://www.example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://www.example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(3).response(200).eos()).await;
        srv.recv_frame(
            frames::headers(5)
                .request("GET", "https://www.example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(5).response(200).eos()).await;
        // Don't close the connection until the client is done doing its
        // checks.
        done_rx.await.unwrap();
    };

    let client = async move {
        let (mut client, conn) = client::handshake(io).await.expect("handshake");
        tokio::spawn(async move {
            rx.await.unwrap();

            let mut responses = vec![];

            for _ in 0..3usize {
                // Wait for capacity. If the client is **not** notified,
                // this hangs.
                poll_fn(|cx| client.poll_ready(cx)).await.unwrap();

                let request = Request::builder()
                    .uri("https://www.example.com/")
                    .body(())
                    .unwrap();

                let response = client.send_request(request, true).unwrap().0;

                responses.push(response);
            }

            for response in responses {
                let response = response.await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            }

            poll_fn(|cx| client.poll_ready(cx)).await.unwrap();

            done_tx.send(()).unwrap();
        });

        conn.await.expect("h2");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn send_stream_poll_reset() {
    let _ = env_logger::try_init();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;
        srv.send_frame(frames::reset(1).refused()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        let (_response, mut tx) = client.send_request(request, false).unwrap();
        let reason = conn
            .drive(poll_fn(move |cx| tx.poll_reset(cx)))
            .await
            .unwrap();
        assert_eq!(reason, Reason::REFUSED_STREAM);
    };

    join(srv, client).await;
}

#[tokio::test]
async fn drop_pending_open() {
    // This test checks that a stream queued for pending open behaves correctly when its
    // client drops.
    use tokio::sync::oneshot;
    let _ = env_logger::try_init();

    let (io, mut srv) = mock::new();
    let (init_tx, init_rx) = oneshot::channel();
    let (trigger_go_away_tx, trigger_go_away_rx) = oneshot::channel();
    let (sent_go_away_tx, sent_go_away_rx) = oneshot::channel();
    let (drop_tx, drop_rx) = oneshot::channel();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(2));

    let srv = async move {
        let settings = srv.assert_client_handshake_with_settings(settings).await;
        // This is the ACK
        assert_default_settings!(settings);
        init_tx.send(()).unwrap();
        srv.recv_frame(frames::headers(1).request("GET", "https://www.example.com/"))
            .await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://www.example.com/")
                .eos(),
        )
        .await;
        trigger_go_away_rx.await.unwrap();
        srv.send_frame(frames::go_away(3)).await;
        sent_go_away_tx.send(()).unwrap();
        drop_rx.await.unwrap();
        srv.send_frame(frames::headers(3).response(200).eos()).await;
        srv.recv_frame(frames::data(1, vec![]).eos()).await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
    };

    fn request() -> Request<()> {
        Request::builder()
            .uri("https://www.example.com/")
            .body(())
            .unwrap()
    }

    let client = async move {
        let (mut client, conn) = client::Builder::new()
            .max_concurrent_reset_streams(0)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let f = async move {
            init_rx.await.expect("init_rx");
            // Fill up the concurrent stream limit.
            poll_fn(|cx| client.poll_ready(cx)).await.unwrap();
            let mut response1 = client.send_request(request(), false).unwrap();
            poll_fn(|cx| client.poll_ready(cx)).await.unwrap();
            let response2 = client.send_request(request(), true).unwrap();
            poll_fn(|cx| client.poll_ready(cx)).await.unwrap();
            let response3 = client.send_request(request(), true).unwrap();

            // Trigger a GOAWAY frame to invalidate our third request.
            trigger_go_away_tx.send(()).unwrap();
            sent_go_away_rx.await.expect("sent_go_away_rx");
            // Now drop all the references to that stream.
            drop(response3);
            drop(client);
            drop_tx.send(()).unwrap();

            // Complete the second request, freeing up a stream.
            response2.0.await.expect("resp2");
            response1.1.send_data(Default::default(), true).unwrap();
            response1.0.await.expect("resp1")
        };

        join(
            async move {
                conn.await.expect("h2");
            },
            f,
        )
        .await;
    };

    join(srv, client).await;
}

#[tokio::test]
async fn malformed_response_headers_dont_unlink_stream() {
    // This test checks that receiving malformed headers frame on a stream with
    // no remaining references correctly resets the stream, without prematurely
    // unlinking it.
    use tokio::sync::oneshot;
    let _ = env_logger::try_init();

    let (io, mut srv) = mock::new();
    let (drop_tx, drop_rx) = oneshot::channel();
    let (queued_tx, queued_rx) = oneshot::channel();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);

        srv.recv_frame(frames::headers(1).request("GET", "http://example.com/"))
            .await;
        srv.recv_frame(frames::headers(3).request("GET", "http://example.com/"))
            .await;
        srv.recv_frame(frames::headers(5).request("GET", "http://example.com/"))
            .await;
        drop_tx.send(()).unwrap();
        queued_rx.await.unwrap();
        srv.send_bytes(&[
            // 2 byte frame
            0, 0, 2, // type: HEADERS
            1, // flags: END_STREAM | END_HEADERS
            5, // stream identifier: 3
            0, 0, 0, 3, // data - invalid (pseudo not at end of block)
            144,
            135, // Per the spec, this frame should cause a stream error of type
                 // PROTOCOL_ERROR.
        ])
        .await;
    };

    fn request() -> Request<()> {
        Request::builder()
            .uri("http://example.com/")
            .body(())
            .unwrap()
    }

    let client = async move {
        let (mut client, conn) = client::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");

        let (_req1, mut send1) = client.send_request(request(), false).unwrap();
        // Use up most of the connection window.
        send1.send_data(vec![0; 65534].into(), true).unwrap();
        let (req2, mut send2) = client.send_request(request(), false).unwrap();
        let (req3, mut send3) = client.send_request(request(), false).unwrap();

        let f = async move {
            drop_rx.await.unwrap();
            // Use up the remainder of the connection window.
            send2.send_data(vec![0; 2].into(), true).unwrap();
            // Queue up for more connection window.
            send3.send_data(vec![0; 1].into(), true).unwrap();
            queued_tx.send(()).unwrap();
            drop((req2, req3));
        };

        join(async move { conn.await.expect("h2") }, f).await;
    };

    join(srv, client).await;
}

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

trait MockH2 {
    fn handshake(&mut self) -> &mut Self;
}

impl MockH2 for mock_io::Builder {
    fn handshake(&mut self) -> &mut Self {
        self.write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            // Settings frame
            .write(SETTINGS)
            .read(SETTINGS)
            .read(SETTINGS_ACK)
    }
}
