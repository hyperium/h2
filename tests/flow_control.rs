#[macro_use]
pub mod support;
use support::prelude::*;

// In this case, the stream & connection both have capacity, but capacity is not
// explicitly requested.
#[test]
fn send_data_without_requesting_capacity() {
    let _ = ::env_logger::init();

    let payload = [0; 1024];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 4, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[..])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let (mut client, mut h2) = Client::handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let (response, mut stream) = client.send_request(request, false).unwrap();

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), 0);

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    // Get the response
    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn release_capacity_sends_window_update() {
    let _ = ::env_logger::init();

    let payload = vec![0u8; 16_384];

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        .send_frame(frames::data(1, &payload[..]))
        .send_frame(frames::data(1, &payload[..]))
        .send_frame(frames::data(1, &payload[..]))
        .recv_frame(
            frames::window_update(0, 32_768)
        )
        .recv_frame(
            frames::window_update(1, 32_768)
        )
        .send_frame(frames::data(1, &payload[..]).eos())
        // gotta end the connection
        .map(drop);

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = client.send_request(request, true).unwrap()
                .0
                .unwrap()
                // Get the response
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.into_future().unwrap()
                })

                // read some body to use up window size to below half
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    body.into_future().unwrap()
                })
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    body.into_future().unwrap()
                })
                .and_then(|(buf, mut body)| {
                    let buf = buf.unwrap();
                    assert_eq!(buf.len(), payload.len());
                    body.release_capacity().release_capacity(buf.len() * 2).unwrap();
                    body.into_future().unwrap()
                })
                .and_then(|(buf, _)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    Ok(())
                });

        h2.unwrap().join(req)
    });
    h2.join(mock).wait().unwrap();
}

#[test]
fn release_capacity_of_small_amount_does_not_send_window_update() {
    let _ = ::env_logger::init();

    let payload = [0; 16];

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        .send_frame(frames::data(1, &payload[..]).eos())
        // gotta end the connection
        .map(drop);

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = client.send_request(request, true).unwrap()
                .0
                .unwrap()
                // Get the response
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.into_future().unwrap()
                })
                // read the small body and then release it
                .and_then(|(buf, mut body)| {
                    let buf = buf.unwrap();
                    assert_eq!(buf.len(), 16);
                    body.release_capacity().release_capacity(buf.len()).unwrap();
                    body.into_future().unwrap()
                })
                .and_then(|(buf, _)| {
                    assert!(buf.is_none());
                    Ok(())
                });
        h2.unwrap().join(req)
    });
    h2.join(mock).wait().unwrap();
}

#[test]
#[ignore]
fn expand_window_sends_window_update() {}

#[test]
#[ignore]
fn expand_window_calls_are_coalesced() {}

#[test]
fn recv_data_overflows_connection_window() {
    let _ = ::env_logger::init();

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        // fill the whole window
        .send_frame(frames::data(1, vec![0u8; 16_384]))
        .send_frame(frames::data(1, vec![0u8; 16_384]))
        .send_frame(frames::data(1, vec![0u8; 16_384]))
        .send_frame(frames::data(1, vec![0u8; 16_383]))
        // this frame overflows the window!
        .send_frame(frames::data(1, vec![0u8; 128]).eos())
        // expecting goaway for the conn, not stream
        .recv_frame(frames::go_away(0).flow_control());
    // connection is ended by client

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
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
                    assert_eq!(
                        err.to_string(),
                        "protocol error: flow-control protocol violated"
                    );
                    Ok::<(), ()>(())
                })
            });

        // client should see a flow control error
        let conn = h2.then(|res| {
            let err = res.unwrap_err();
            assert_eq!(
                err.to_string(),
                "protocol error: flow-control protocol violated"
            );
            Ok::<(), ()>(())
        });
        conn.unwrap().join(req)
    });
    h2.join(mock).wait().unwrap();
}

#[test]
fn recv_data_overflows_stream_window() {
    // this tests for when streams have smaller windows than their connection
    let _ = ::env_logger::init();

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .ignore_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        // fill the whole window
        .send_frame(frames::data(1, vec![0u8; 16_384]))
        // this frame overflows the window!
        .send_frame(frames::data(1, &[0; 16][..]).eos())
        // expecting goaway for the conn
        // TODO: change to a RST_STREAM eventually
        .recv_frame(frames::go_away(0).flow_control())
        // close the connection
        .map(drop);

    let h2 = Client::builder()
        .initial_window_size(16_384)
        .handshake::<_, Bytes>(io)
        .unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://http2.akamai.com/")
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
                        assert_eq!(
                            err.to_string(),
                            "protocol error: flow-control protocol violated"
                        );
                        Ok::<(), ()>(())
                    })
                });

            // client should see a flow control error
            let conn = h2.then(|res| {
                let err = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "protocol error: flow-control protocol violated"
                );
                Ok::<(), ()>(())
            });
            conn.unwrap().join(req)
        });
    h2.join(mock).wait().unwrap();
}



#[test]
#[ignore]
fn recv_window_update_causes_overflow() {
    // A received window update causes the window to overflow.
}

#[test]
fn stream_close_by_data_frame_releases_capacity() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let window_size = frame::DEFAULT_INITIAL_WINDOW_SIZE as usize;

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // Send request
        let (resp1, mut s1) = client.send_request(request, false).unwrap();

        // This effectively reserves the entire connection window
        s1.reserve_capacity(window_size);

        // The capacity should be immediately available as nothing else is
        // happening on the stream.
        assert_eq!(s1.capacity(), window_size);

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // Create a second stream
        let (resp2, mut s2) = client.send_request(request, false).unwrap();

        // Request capacity
        s2.reserve_capacity(5);

        // There should be no available capacity (as it is being held up by
        // the previous stream
        assert_eq!(s2.capacity(), 0);

        // Closing the previous stream by sending an empty data frame will
        // release the capacity to s2
        s1.send_data("".into(), true).unwrap();

        // The capacity should be available
        assert_eq!(s2.capacity(), 5);

        // Send the frame
        s2.send_data("hello".into(), true).unwrap();

        // Drive both streams to prevent the handles from being dropped
        // (which will send a RST_STREAM) before the connection is closed.
        h2.drive(resp1)
          .and_then(move |(h2, _)| h2.drive(resp2))
    })
    .unwrap();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(frames::headers(1).request("POST", "https://http2.akamai.com/"))
        .send_frame(frames::headers(1).response(200))
        .recv_frame(frames::headers(3).request("POST", "https://http2.akamai.com/"))
        .send_frame(frames::headers(3).response(200))
        .recv_frame(frames::data(1, &b""[..]).eos())
        .recv_frame(frames::data(3, &b"hello"[..]).eos())
        .close();

    let _ = h2.join(srv).wait().unwrap();
}

#[test]
fn stream_close_by_trailers_frame_releases_capacity() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let window_size = frame::DEFAULT_INITIAL_WINDOW_SIZE as usize;

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // Send request
        let (resp1, mut s1) = client.send_request(request, false).unwrap();

        // This effectively reserves the entire connection window
        s1.reserve_capacity(window_size);

        // The capacity should be immediately available as nothing else is
        // happening on the stream.
        assert_eq!(s1.capacity(), window_size);

        let request = Request::builder()
            .method(Method::POST)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // Create a second stream
        let (resp2, mut s2) = client.send_request(request, false).unwrap();

        // Request capacity
        s2.reserve_capacity(5);

        // There should be no available capacity (as it is being held up by
        // the previous stream
        assert_eq!(s2.capacity(), 0);

        // Closing the previous stream by sending a trailers frame will
        // release the capacity to s2
        s1.send_trailers(Default::default()).unwrap();

        // The capacity should be available
        assert_eq!(s2.capacity(), 5);

        // Send the frame
        s2.send_data("hello".into(), true).unwrap();

        // Drive both streams to prevent the handles from being dropped
        // (which will send a RST_STREAM) before the connection is closed.
        h2.drive(resp1)
          .and_then(move |(h2, _)| h2.drive(resp2))
    })
    .unwrap();

    let srv = srv.assert_client_handshake().unwrap()
        // Get the first frame
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("POST", "https://http2.akamai.com/")
        )
        .send_frame(frames::headers(1).response(200))
        .recv_frame(
            frames::headers(3)
                .request("POST", "https://http2.akamai.com/")
        )
        .send_frame(frames::headers(3).response(200))
        .recv_frame(frames::headers(1).eos())
        .recv_frame(frames::data(3, &b"hello"[..]).eos())
        .close();

    let _ = h2.join(srv).wait().unwrap();
}

#[test]
#[ignore]
fn stream_close_by_send_reset_frame_releases_capacity() {}

#[test]
#[ignore]
fn stream_close_by_recv_reset_frame_releases_capacity() {}

#[test]
fn recv_window_update_on_stream_closed_by_data_frame() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let h2 = Client::handshake(io)
        .unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let (response, stream) = client.send_request(request, false).unwrap();

            // Wait for the response
            h2.drive(response.map(|response| (response, stream)))
        })
        .and_then(|(h2, (response, mut stream))| {
            assert_eq!(response.status(), StatusCode::OK);

            // Send a data frame, this will also close the connection
            stream.send_data("hello".into(), true).unwrap();

            // keep `stream` from being dropped in order to prevent
            // it from sending an RST_STREAM frame.
            //
            // i know this is kind of evil, but it's necessary to
            // ensure that the stream is closed by the EOS frame,
            // and not by the RST_STREAM.
            std::mem::forget(stream);

            // Wait for the connection to close
            h2.unwrap()
        });

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(frames::headers(1).request("POST", "https://http2.akamai.com/"))
        .send_frame(frames::headers(1).response(200))
        .recv_frame(frames::data(1, "hello").eos())
        .send_frame(frames::window_update(1, 5))
        .map(drop);

    let _ = h2.join(srv).wait().unwrap();
}

#[test]
fn reserved_capacity_assigned_in_multi_window_updates() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let h2 = Client::handshake(io)
        .unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let (response, mut stream) = client.send_request(request, false).unwrap();

            // Consume the capacity
            let payload = vec![0; frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];
            stream.send_data(payload.into(), false).unwrap();

            // Reserve more data than we want
            stream.reserve_capacity(10);

            h2.drive(
                util::wait_for_capacity(stream, 5)
                    .map(|stream| (response, stream)))
        })
        .and_then(|(h2, (response, mut stream))| {
            stream.send_data("hello".into(), false).unwrap();
            stream.send_data("world".into(), true).unwrap();

            h2.drive(response)
        })
        .and_then(|(h2, response)| {
            assert_eq!(response.status(), StatusCode::NO_CONTENT);

            // Wait for the connection to close
            h2.unwrap()
        });

    let srv = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("POST", "https://http2.akamai.com/")
        )
        .recv_frame(frames::data(1, vec![0u8; 16_384]))
        .recv_frame(frames::data(1, vec![0u8; 16_384]))
        .recv_frame(frames::data(1, vec![0u8; 16_384]))
        .recv_frame(frames::data(1, vec![0u8; 16_383]))
        .idle_ms(100)
        // Increase the connection window
        .send_frame(
            frames::window_update(0, 10))
        // Incrementally increase the stream window
        .send_frame(
            frames::window_update(1, 4))
        .idle_ms(50)
        .send_frame(
            frames::window_update(1, 1))
        // Receive first chunk
        .recv_frame(frames::data(1, "hello"))
        .send_frame(
            frames::window_update(1, 5))
        // Receive second chunk
        .recv_frame(
            frames::data(1, "world").eos())
        .send_frame(
            frames::headers(1)
                .response(204)
                .eos()
        )
        /*
        .recv_frame(frames::data(1, "hello").eos())
        .send_frame(frames::window_update(1, 5))
        */
        .map(drop);

    let _ = h2.join(srv).wait().unwrap();
}

#[test]
fn connection_notified_on_released_capacity() {
    use futures::sync::oneshot;
    use std::sync::mpsc;
    use std::thread;

    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    // We're going to run the connection on a thread in order to isolate task
    // notifications. This test is here, in part, to ensure that the connection
    // receives the appropriate notifications to send out window updates.

    let (tx, rx) = mpsc::channel();

    // Because threading is fun
    let (settings_tx, settings_rx) = oneshot::channel();

    let th1 = thread::spawn(move || {
        srv.assert_client_handshake().unwrap()
            .recv_settings()
            .map(move |v| {
                settings_tx.send(()).unwrap();
                v
            })
            // Get the first request
            .recv_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/a")
                    .eos())
            // Get the second request
            .recv_frame(
                frames::headers(3)
                    .request("GET", "https://example.com/b")
                    .eos())
            // Send the first response
            .send_frame(frames::headers(1).response(200))
            // Send the second response
            .send_frame(frames::headers(3).response(200))

            // Fill the connection window
            .send_frame(frames::data(1, vec![0u8; 16_384]).eos())
            .idle_ms(100)
            .send_frame(frames::data(3, vec![0u8; 16_384]).eos())

            // The window update is sent
            .recv_frame(frames::window_update(0, 16_384))
            .map(drop)
            .wait().unwrap();
    });


    let th2 = thread::spawn(move || {
        let (mut client, h2) = Client::handshake(io).wait().unwrap();

        let (h2, _) = h2.drive(settings_rx).wait().unwrap();

        let request = Request::get("https://example.com/a").body(()).unwrap();

        tx.send(client.send_request(request, true).unwrap())
            .unwrap();

        let request = Request::get("https://example.com/b").body(()).unwrap();

        tx.send(client.send_request(request, true).unwrap())
            .unwrap();

        // Run the connection to completion
        h2.wait().unwrap();
    });

    // Get the two requests
    let (a, _) = rx.recv().unwrap();
    let (b, _) = rx.recv().unwrap();

    // Get the first response
    let response = a.wait().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (_, a) = response.into_parts();

    // Get the next chunk
    let (chunk, mut a) = a.into_future().wait().unwrap();
    assert_eq!(16_384, chunk.unwrap().len());

    // Get the second response
    let response = b.wait().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (_, b) = response.into_parts();

    // Get the next chunk
    let (chunk, b) = b.into_future().wait().unwrap();
    assert_eq!(16_384, chunk.unwrap().len());

    // Wait a bit
    thread::sleep(Duration::from_millis(100));

    // Release the capacity
    a.release_capacity().release_capacity(16_384).unwrap();

    th1.join().unwrap();
    th2.join().unwrap();

    // Explicitly drop this after the joins so that the capacity doesn't get
    // implicitly released before.
    drop(b);
}

#[test]
fn recv_settings_removes_available_capacity() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_initial_window_size(Some(0));

    let srv = srv.assert_client_handshake_with_settings(settings).unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("POST", "https://http2.akamai.com/")
        )
        .idle_ms(100)
        .send_frame(frames::window_update(0, 11))
        .send_frame(frames::window_update(1, 11))
        .recv_frame(frames::data(1, "hello world").eos())
        .send_frame(
            frames::headers(1)
                .response(204)
                .eos()
        )
        .close();


    let h2 = Client::handshake(io).unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let (response, mut stream) = client.send_request(request, false).unwrap();

            stream.reserve_capacity(11);

            h2.drive(util::wait_for_capacity(stream, 11).map(|s| (response, s)))
        })
        .and_then(|(h2, (response, mut stream))| {
            assert_eq!(stream.capacity(), 11);

            stream.send_data("hello world".into(), true).unwrap();

            h2.drive(response)
        })
        .and_then(|(h2, response)| {
            assert_eq!(response.status(), StatusCode::NO_CONTENT);

            // Wait for the connection to close
            h2.unwrap()
        });

    let _ = h2.join(srv)
        .wait().unwrap();
}

#[test]
fn recv_no_init_window_then_receive_some_init_window() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_initial_window_size(Some(0));

    let srv = srv.assert_client_handshake_with_settings(settings).unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("POST", "https://http2.akamai.com/")
        )
        .idle_ms(100)
        .send_frame(frames::settings().initial_window_size(10))
        .recv_frame(frames::settings_ack())
        .recv_frame(frames::data(1, "hello worl"))
        .idle_ms(100)
        .send_frame(frames::settings().initial_window_size(11))
        .recv_frame(frames::settings_ack())
        .recv_frame(frames::data(1, "d").eos())
        .send_frame(
            frames::headers(1)
                .response(204)
                .eos()
        )
        .close();


    let h2 = Client::handshake(io).unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let (response, mut stream) = client.send_request(request, false).unwrap();

            stream.reserve_capacity(11);

            h2.drive(util::wait_for_capacity(stream, 11).map(|s| (response, s)))
        })
        .and_then(|(h2, (response, mut stream))| {
            assert_eq!(stream.capacity(), 11);

            stream.send_data("hello world".into(), true).unwrap();

            h2.drive(response)
        })
        .and_then(|(h2, response)| {
            assert_eq!(response.status(), StatusCode::NO_CONTENT);

            // Wait for the connection to close
            h2.unwrap()
        });

    let _ = h2.join(srv)
        .wait().unwrap();
}

#[test]
fn settings_lowered_capacity_returns_capacity_to_connection() {
    use std::thread;

    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let window_size = frame::DEFAULT_INITIAL_WINDOW_SIZE as usize;

    // Spawn the server on a thread
    let th1 = thread::spawn(move || {
        srv.assert_client_handshake().unwrap()
            .recv_settings()
            .recv_frame(
                frames::headers(1)
                    .request("POST", "https://example.com/one")
            )
            .recv_frame(
                frames::headers(3)
                    .request("POST", "https://example.com/two")
            )
            .idle_ms(200)
            // Remove all capacity from streams
            .send_frame(frames::settings().initial_window_size(0))
            .recv_frame(frames::settings_ack())

            // Let stream 3 make progress
            .send_frame(frames::window_update(3, 11))
            .recv_frame(frames::data(3, "hello world").eos())

            // Wait a bit
            //
            // TODO: Receive signal from main thread
            .idle_ms(200)

            // Reset initial window size
            .send_frame(frames::settings().initial_window_size(window_size as u32))
            .recv_frame(frames::settings_ack())

            // Get data from first stream
            .recv_frame(frames::data(1, "hello world").eos())

            // Send responses
            .send_frame(
                frames::headers(1)
                    .response(204)
                    .eos()
            )
            .send_frame(
                frames::headers(3)
                    .response(204)
                    .eos()
            )
            .close()
            .wait().unwrap();
    });

    let (mut client, h2) = Client::handshake(io).unwrap()
        .wait().unwrap();

    // Drive client connection
    let th2 = thread::spawn(move || {
        h2.wait().unwrap();
    });

    let request = Request::post("https://example.com/one")
        .body(()).unwrap();

    let (resp1, mut stream1) = client.send_request(request, false).unwrap();

    let request = Request::post("https://example.com/two")
        .body(()).unwrap();

    let (resp2, mut stream2) = client.send_request(request, false).unwrap();

    // Reserve capacity for stream one, this will consume all connection level
    // capacity
    stream1.reserve_capacity(window_size);
    let stream1 = util::wait_for_capacity(stream1, window_size).wait().unwrap();

    // Now, wait for capacity on the other stream
    stream2.reserve_capacity(11);
    let mut stream2 = util::wait_for_capacity(stream2, 11).wait().unwrap();

    // Send data on stream 2
    stream2.send_data("hello world".into(), true).unwrap();

    // Wait for capacity on stream 1
    let mut stream1 = util::wait_for_capacity(stream1, 11).wait().unwrap();

    stream1.send_data("hello world".into(), true).unwrap();

    // Wait for responses..
    let resp = resp1.wait().unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = resp2.wait().unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    th1.join().unwrap();
    th2.join().unwrap();
}

#[test]
fn client_increase_target_window_size() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(frames::window_update(0, (2 << 20) - 65_535))
        .close();


    let client = Client::handshake(io).unwrap()
        .and_then(|(_client, mut conn)| {
            conn.set_target_window_size(2 << 20);

            conn.unwrap()
        });

    srv.join(client).wait().unwrap();
}

#[test]
fn increase_target_window_size_after_using_some() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_384]).eos())
        .recv_frame(frames::window_update(0, (2 << 20) - 65_535))
        .close();

    let client = Client::handshake(io).unwrap()
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let res = client.send_request(request, true).unwrap().0
                .and_then(|res| {
                    // "leak" the capacity for now
                    res.into_parts().1.concat2()
                });

            conn.drive(res)
                .and_then(|(mut conn, _bytes)| {
                    conn.set_target_window_size(2 << 20);
                    conn.unwrap()
                })
        });

    srv.join(client).wait().unwrap();
}

#[test]
fn decrease_target_window_size() {
     let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(frames::headers(1).response(200))
        .send_frame(frames::data(1, vec![0; 16_384]))
        .send_frame(frames::data(1, vec![0; 16_384]))
        .send_frame(frames::data(1, vec![0; 16_384]))
        .send_frame(frames::data(1, vec![0; 16_383]).eos())
        .recv_frame(frames::window_update(0, 16_384))
        .close();

    let client = Client::handshake(io).unwrap()
        .and_then(|(mut client, mut conn)| {
            conn.set_target_window_size(16_384 * 2);

            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();
            let (resp, _) = client.send_request(request, true).unwrap();
            conn.drive(resp.expect("response"))
        })
        .and_then(|(mut conn, res)| {
            conn.set_target_window_size(16_384);
            let mut body = res.into_parts().1;
            let mut cap = body.release_capacity().clone();

            conn.drive(body.concat2().expect("concat"))
                .and_then(move |(conn, bytes)| {
                    assert_eq!(bytes.len(), 65_535);
                    cap.release_capacity(bytes.len()).unwrap();
                    conn.expect("conn")
                })
        });

    srv.join(client).wait().unwrap();
}

#[test]
fn server_target_window_size() {
    let _ = ::env_logger::init();
    let (io, client) = mock::new();

    let client = client.assert_server_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(frames::window_update(0, (2 << 20) - 65_535))
        .close();

    let srv = Server::handshake(io).unwrap()
        .and_then(|mut conn| {
            conn.set_target_window_size(2 << 20);
            conn.into_future().unwrap()
        });

    srv.join(client).wait().unwrap();
}
