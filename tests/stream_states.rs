#[macro_use]
extern crate log;

pub mod support;
use support::prelude::*;

#[test]
fn send_recv_headers_only() {
    let _ = env_logger::init();

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

    let (mut client, mut h2) = Client::handshake(mock).wait().unwrap();

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
    let _ = env_logger::init();

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

    let (mut client, mut h2) = Client::builder().handshake(mock).wait().unwrap();

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
    let _ = env_logger::init();

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

    let (mut client, mut h2) = Client::handshake(mock).wait().unwrap();

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
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
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

            drop(response);

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
    let _ = ::env_logger::init();
    let (io, mut srv) = mock::new();

    let h2 = Client::handshake(io).unwrap().and_then(|(mut client, h2)| {
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
    let _ = ::env_logger::init();
    let (io, mut srv) = mock::new();

    let h2 = Client::builder()
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
    let _ = ::env_logger::init();
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

    let h2 = Client::handshake(io)
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

/*
#[test]
fn send_data_after_headers_eos() {
    let _ = env_logger::init();

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
