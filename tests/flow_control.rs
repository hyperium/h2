extern crate h2_test_support;
use h2_test_support::prelude::*;

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

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    let mut stream = h2.request(request, false).unwrap();

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), 0);

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn release_capacity_sends_window_update() {
    let _ = ::env_logger::init();

    let payload = vec![0u8; 65_535];

    let (io, srv) = mock::new();

    fn make_request() -> Request<()> {
        Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(()).unwrap()
    }


    /* // End Goal:
    let mock = srv.assert_client_handshake().unwrap()
        // .assert_settings ?
        .and_then(|(_settings, srv)| {
            srv.into_future().unwrap()
        })
        .recv(
            frames::headers(1)
                .request(head.method, head.uri)
                .eos()
        )
        .send(&[
            frames::headers(1)
                .response(200),
            frames::data(1, &payload[0..16_384]),
            frames::data(1, &payload[0..16_384]),
            frames::data(1, &payload[0..16_384]),
        ])
        .recv(
            frames::window_update(0, 32_768)
        )
        .recv(
            frames::window_update(1, 32_768)
        )
        */

    let mock = srv.assert_client_handshake().unwrap()
        .and_then(|(_settings, srv)| {
            srv.into_future().unwrap()
        })
        .and_then(|(frame, mut srv)| {
            let head = make_request().into_parts().0;
            let expected = frames::headers(1)
                .request(head.method, head.uri)
                .fields(head.headers)
                .eos()
                .into();
            assert_eq!(frame.unwrap(), expected);

            let res = frames::headers(1)
                .response(200)
                .into();
            srv.send(res).unwrap();
            let data = frames::data(1, &payload[0..16_384]);
            srv.send(data.into()).unwrap();
            let data = frames::data(1, &payload[16_384..16_384 * 2]);
            srv.send(data.into()).unwrap();
            let data = frames::data(1, &payload[16_384 * 2..16_384 * 3]);
            srv.send(data.into()).unwrap();
            srv.into_future().unwrap()
        })
        .and_then(|(frame, srv)| {
            let expected = frames::window_update(0, 32_768);
            assert_eq!(frame.unwrap(), expected.into());
            srv.into_future().unwrap()
        })
        .and_then(|(frame, mut srv)| {
            let expected = frames::window_update(1, 32_768);
            assert_eq!(frame.unwrap(), expected.into());

            let data = frames::data(1, &payload[16_384 * 3..])
                .eos();
            srv.send(data.into()).unwrap();
            Ok(())
        });

    let h2 = Client::handshake(io).unwrap()
        .and_then(|mut h2| {
            eprintln!("h2.request");
            let request = make_request();

            let req = h2.request(request, true).unwrap()
                .unwrap()
                // Get the response
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.into_future().unwrap()
                })

                // read some body to use up window size to below half
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), 16_384);
                    body.into_future().unwrap()
                })
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), 16_384);
                    body.into_future().unwrap()
                })
                .and_then(|(buf, mut body)| {
                    let buf = buf.unwrap();
                    assert_eq!(buf.len(), 16_384);
                    body.release_capacity(buf.len() * 2).unwrap();
                    body.into_future().unwrap()
                })
                .and_then(|(buf, _)| {
                    assert_eq!(buf.unwrap().len(), 16_383);
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

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // GET /
            0, 0, 16, 1, 5, 0, 0, 0, 1, 130, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 4, 0, 0, 0, 1, 0x88])
        .read(&[
            // DATA
            0, 0, 16, 0, 1, 0, 0, 0, 1,
        ])
        .read(&payload[..])
        // write() or WINDOW_UPDATE purposefully excluded

        // we send a 2nd stream in order to test the window update is
        // is actually written to the socket
        .write(&[
            // GET /
            0, 0, 4, 1, 5, 0, 0, 0, 3, 130, 135, 190, 132,
        ])
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 3, 0x88])
        .build();

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    let request = Request::builder()
        .method(Method::GET)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    let mut stream = h2.request(request, true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut body = resp.into_parts().1;
    let buf = h2.run(poll_fn(|| body.poll())).unwrap().unwrap();

    // release some capacity to send a window_update
    body.release_capacity(buf.len()).unwrap();

    // send a 2nd stream to force flushing of window updates
    let request = Request::builder()
        .method(Method::GET)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();
    let mut stream = h2.request(request, true).unwrap();
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::OK);


    h2.wait().unwrap();
}

#[test]
#[ignore]
fn expand_window_sends_window_update() {
}

#[test]
#[ignore]
fn expand_window_calls_are_coalesced() {
}

#[test]
#[ignore]
fn recv_data_overflows_window() {
}

#[test]
#[ignore]
fn recv_window_update_causes_overflow() {
    // A received window update causes the window to overflow.
}
