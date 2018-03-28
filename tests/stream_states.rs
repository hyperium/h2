#[macro_use]
extern crate log;

pub mod support;
use support::prelude::*;

#[test]
fn send_recv_headers_only() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29,
                0xAC, 0x4B, 0x8F, 0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn send_recv_data() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 0, 5, 0, 1, 0, 0, 0, 1, 104, 101, 108, 108, 111,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            // HEADERS
            0, 0, 1, 1, 4, 0, 0, 0, 1, 136,
            // DATA
            0, 0, 5, 0, 1, 0, 0, 0, 1, 119, 111, 114, 108, 100
        ])
        .build();

    let (mut client, mut h2) = client::Builder::new().handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let (response, mut stream) = client.send_request(request, false).unwrap();

    // Reserve send capacity
    stream.reserve_capacity(5);

    assert_eq!(stream.capacity(), 5);

    // Send the data
    stream.send_data("hello", true).unwrap();

    // Get the response
    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Take the body
    let (_, body) = resp.into_parts();

    // Wait for all the data frames to be received
    let bytes = h2.run(body.collect()).unwrap();

    // One byte chunk
    assert_eq!(1, bytes.len());

    assert_eq!(bytes[0], &b"world"[..]);

    // The H2 connection is closed
    h2.wait().unwrap();
}

#[test]
fn send_headers_recv_data_single_frame() {
    let _ = env_logger::try_init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 16, 1, 5, 0, 0, 0, 1, 130, 135, 65, 139, 157, 41, 172, 75,
            143, 168, 233, 25, 151, 33, 233, 132
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[
            0, 0, 1, 1, 4, 0, 0, 0, 1, 136, 0, 0, 5, 0, 0, 0, 0, 0, 1, 104, 101,
            108, 108, 111, 0, 0, 5, 0, 1, 0, 0, 0, 1, 119, 111, 114, 108, 100,
        ])
        .build();

    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Take the body
    let (_, body) = resp.into_parts();

    // Wait for all the data frames to be received
    let bytes = h2.run(body.collect()).unwrap();

    // Two data frames
    assert_eq!(2, bytes.len());

    assert_eq!(bytes[0], &b"hello"[..]);
    assert_eq!(bytes[1], &b"world"[..]);

    // The H2 connection is closed
    h2.wait().unwrap();
}

#[test]
fn closed_streams_are_released() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::get("https://example.com/").body(()).unwrap();

        // Send request
        let (response, _) = client.send_request(request, true).unwrap();
        h2.drive(response).and_then(move |(_, response)| {
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

            Ok(())
        })
    });

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(204).eos())
        .close();

    let _ = h2.join(srv).wait().unwrap();
}

#[test]
fn errors_if_recv_frame_exceeds_max_frame_size() {
    let _ = ::env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(())
            .unwrap();

        let req = client
            .send_request(request, true)
            .unwrap()
            .0.unwrap()
            .and_then(|resp| {
                assert_eq!(resp.status(), StatusCode::OK);
                let body = resp.into_parts().1;
                body.concat2().then(|res| {
                    let err = res.unwrap_err();
                    assert_eq!(err.to_string(), "protocol error: frame with invalid size");
                    Ok::<(), ()>(())
                })
            });

        // client should see a conn error
        let conn = h2.then(|res| {
            let err = res.unwrap_err();
            assert_eq!(err.to_string(), "protocol error: frame with invalid size");
            Ok::<(), ()>(())
        });
        conn.unwrap().join(req)
    });

    // a bad peer
    srv.codec_mut().set_max_send_frame_size(16_384 * 4);

    let srv = srv.assert_client_handshake()
        .unwrap()
        .ignore_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_385]).eos())
        .recv_frame(frames::go_away(0).frame_size())
        .close();

    let _ = h2.join(srv).wait().unwrap();
}


#[test]
fn configure_max_frame_size() {
    let _ = ::env_logger::try_init();
    let (io, mut srv) = mock::new();

    let h2 = client::Builder::new()
        .max_frame_size(16_384 * 2)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req = client
                .send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.concat2().expect("body")
                })
                .and_then(|buf| {
                    assert_eq!(buf.len(), 16_385);
                    Ok(())
                });

            h2.expect("client").join(req)
        });

    // a good peer
    srv.codec_mut().set_max_send_frame_size(16_384 * 2);

    let srv = srv.assert_client_handshake()
        .unwrap()
        .ignore_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_385]).eos())
        .close();

    let _ = h2.join(srv).wait().expect("wait");
}

#[test]
fn recv_goaway_finishes_processed_streams() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .recv_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::go_away(1))
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_384]).eos())
        // expecting a goaway of 0, since server never initiated a stream
        .recv_frame(frames::go_away(0));
        //.close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req1 = client.send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.concat2().expect("body")
                })
                .and_then(|buf| {
                    assert_eq!(buf.len(), 16_384);
                    Ok(())
                });


            // this request will trigger a goaway
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();
            let req2 = client.send_request(request, true)
                .unwrap()
                .0.then(|res| {
                    let err = res.unwrap_err();
                    assert_eq!(err.to_string(), "protocol error: not a result of an error");
                    Ok::<(), ()>(())
                });

            h2.expect("client").join3(req1, req2)
        });


    h2.join(srv).wait().expect("wait");
}

#[test]
fn recv_next_stream_id_updated_by_malformed_headers() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();


    let bad_auth = util::byte_str("not:a/good authority");
    let mut bad_headers: frame::Headers = frames::headers(1)
        .request("GET", "https://example.com/")
        .eos()
        .into();
    bad_headers.pseudo_mut().authority = Some(bad_auth);

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        // bad headers -- should error.
        .send_frame(bad_headers)
        .recv_frame(frames::reset(1).protocol_error())
        // this frame is good, but the stream id should already have been incr'd
        .send_frame(frames::headers(1)
            .request("GET", "https://example.com/")
            .eos())
        .recv_frame(frames::go_away(1).protocol_error())
        .close();

        let srv = server::handshake(io)
            .expect("handshake")
            .and_then(|srv| srv.into_future().then(|res| {
                let (err, _) = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "protocol error: unspecific protocol error detected"
                );

                Ok::<(), ()>(())
            })
        );

        srv.join(client).wait().expect("wait");
}

#[test]
fn skipped_stream_ids_are_implicitly_closed() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv
        .assert_client_handshake()
        .expect("handshake")
        .recv_settings()
        .recv_frame(frames::headers(5)
            .request("GET", "https://example.com/")
            .eos(),
        )
        // send the response on a lower-numbered stream, which should be
        // implicitly closed.
        .send_frame(frames::headers(3).response(200));

        let h2 = client::Builder::new()
            .initial_stream_id(5)
            .handshake::<_, Bytes>(io)
            .expect("handshake")
            .and_then(|(mut client, h2)| {
                let request = Request::builder()
                    .method(Method::GET)
                    .uri("https://example.com/")
                    .body(())
                    .unwrap();

                let req = client.send_request(request, true)
                    .unwrap()
                    .0.then(|res| {
                        let err = res.unwrap_err();
                        assert_eq!(
                            err.to_string(),
                            "protocol error: unspecific protocol error detected");
                        Ok::<(), ()>(())
                    });
                    // client should see a conn error
                    let conn = h2.then(|res| {
                        let err = res.unwrap_err();
                        assert_eq!(
                            err.to_string(),
                            "protocol error: unspecific protocol error detected"
                        );
                        Ok::<(), ()>(())
                    });
                    conn.unwrap().join(req)
            });


        h2.join(srv).wait().expect("wait");

}

#[test]
fn send_rst_stream_allows_recv_data() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .recv_frame(frames::reset(1).cancel())
        // sending frames after canceled!
        //   note: sending 2 to cosume 50% of connection window
        .send_frame(frames::data(1, vec![0; 16_384]))
        .send_frame(frames::data(1, vec![0; 16_384]).eos())
        // make sure we automatically free the connection window
        .recv_frame(frames::window_update(0, 16_384 * 2))
        // do a pingpong to ensure no other frames were sent
        .ping_pong([1; 8])
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req = client.send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // drop resp will send a reset
                    Ok(())
                });

            conn.expect("client")
                .drive(req)
                .and_then(|(conn, _)| conn)
        });


    client.join(srv).wait().expect("wait");
}

#[test]
fn send_rst_stream_allows_recv_trailers() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_384]))
        .recv_frame(frames::reset(1).cancel())
        // sending frames after canceled!
        .send_frame(frames::headers(1).field("foo", "bar").eos())
        // do a pingpong to ensure no other frames were sent
        .ping_pong([1; 8])
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req = client.send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // drop resp will send a reset
                    Ok(())
                });

            conn.expect("client")
                .drive(req)
                .and_then(|(conn, _)| conn)
        });


    client.join(srv).wait().expect("wait");
}

#[test]
fn rst_stream_expires() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_384]))
        .recv_frame(frames::reset(1).cancel())
        // wait till after the configured duration
        .idle_ms(15)
        .ping_pong([1; 8])
        // sending frame after canceled!
        .send_frame(frames::data(1, vec![0; 16_384]).eos())
        .recv_frame(frames::go_away(0).protocol_error())
        .close();

    let client = client::Builder::new()
        .reset_stream_duration(Duration::from_millis(10))
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req = client.send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // drop resp will send a reset
                    Ok(())
                })
                .map_err(|()| -> Error {
                    unreachable!()
                });

            conn.drive(req)
                .and_then(|(conn, _)| conn.expect_err("client"))
                .map(|err| {
                    assert_eq!(
                        err.to_string(),
                        "protocol error: unspecific protocol error detected"
                    );
                    drop(client);
                })
        });


    client.join(srv).wait().expect("wait");
}

#[test]
fn rst_stream_max() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .recv_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16]))
        .send_frame(frames::headers(3).response(200))
        .send_frame(frames::data(3, vec![0; 16]))
        .recv_frame(frames::reset(1).cancel())
        .recv_frame(frames::reset(3).cancel())
        // sending frame after canceled!
        // newer streams trump older streams
        // 3 is still being ignored
        .send_frame(frames::data(3, vec![0; 16]).eos())
        // ping pong to be sure of no goaway
        .ping_pong([1; 8])
        // 1 has been evicted, will get a goaway
        .send_frame(frames::data(1, vec![0; 16]).eos())
        .recv_frame(frames::go_away(0).protocol_error())
        .close();

    let client = client::Builder::new()
        .max_concurrent_reset_streams(1)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req1 = client.send_request(request, true)
                .unwrap()
                .0.expect("response1")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // drop resp will send a reset
                    Ok(())
                })
                .map_err(|()| -> Error {
                    unreachable!()
                });

            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req2 = client.send_request(request, true)
                .unwrap()
                .0.expect("response2")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // drop resp will send a reset
                    Ok(())
                })
                .map_err(|()| -> Error {
                    unreachable!()
                });

            conn.drive(req1.join(req2))
                .and_then(|(conn, _)| conn.expect_err("client"))
                .map(|err| {
                    assert_eq!(
                        err.to_string(),
                        "protocol error: unspecific protocol error detected"
                    );
                })
        });


    client.join(srv).wait().expect("wait");
}

#[test]
fn reserved_state_recv_window_update() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(
            frames::push_promise(1, 2)
                .request("GET", "https://example.com/push")
        )
        // it'd be weird to send a window update on a push promise,
        // since the client can't send us data, but whatever. The
        // point is that it's allowed, so we're testing it.
        .send_frame(frames::window_update(2, 128))
        .send_frame(frames::headers(1).response(200).eos())
        // ping pong to ensure no goaway
        .ping_pong([1; 8])
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let req = client.send_request(request, true)
                .unwrap()
                .0.expect("response")
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    Ok(())
                });


            conn.drive(req)
                .and_then(|(conn, _)| conn.expect("client"))
        });


    client.join(srv).wait().expect("wait");
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
        .wait().expect("handshake");

    // Send the request
    let mut request = request::Head::default();
    request.method = Method::POST;
    request.uri = "https://http2.akamai.com/".parse().unwrap();

    let id = 1.into();
    let h2 = h2.send_request(id, request, true).wait().expect("send request");

    let body = "hello";

    // Send the data
    let err = h2.send_data(id, body.into(), true).wait().unwrap_err();
    assert_user_err!(err, UnexpectedFrameType);
}

#[test]
#[ignore]
fn exceed_max_streams() {
}
*/


#[test]
fn rst_while_closing() {
    // Test to reproduce panic in issue #246 --- receipt of a RST_STREAM frame
    // on a stream in the Half Closed (remote) state with a queued EOS causes
    // a panic.
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::headers(1).eos())
        // Idling for a moment here is necessary to ensure that the client
        // enqueues its TRAILERS frame *before* we send the RST_STREAM frame
        // which causes the panic.
        .idle_ms(1)
        // Send the RST_STREAM frame which causes the client to panic.
        .send_frame(frames::reset(1).cancel())
        .ping_pong([1; 8])
        .close();
        ;

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // The request should be left streaming.
            let (resp, mut stream) = client.send_request(request, false)
                .expect("send_request");
            let req = resp
                // on receipt of an EOS response from the server, transition
                // the stream Open => Half Closed (remote).
                .expect("response")
                .and_then(move |resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    // Enqueue trailers frame.
                    let _ = stream.send_trailers(HeaderMap::new());
                    Ok(())
                })
                .map_err(|()| -> Error {
                    unreachable!()
                });

            conn.drive(req)
                .and_then(|(conn, _)| conn.expect("client"))
        });


    client.join(srv).wait().expect("wait");
}
