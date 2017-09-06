extern crate h2_test_support;
use h2_test_support::*;

#[test]
fn recv_single_ping() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    // Create the handshake
    let h2 = Client::handshake(m)
        // Wait until the connection finishes
        .then(|res| {
            res.unwrap()
            // Ok(res.unwrap())
        });

    let mock = mock.expect_client_handshake()
        .then(|res| {
            let (settings, mut mock) = res.unwrap();
            println!("GOT settings; {:?}", settings);

            let frame = frame::Ping::new();
            mock.send(frame.into()).unwrap();

            mock.into_future()
        })
        .then(|res| {
            let (frame, _) = res.unwrap();
            println!("~~~~~~~~ GOT FRAME: {:?}", frame);
            Ok(())
        });

    let _ = h2.join(mock)
        .wait().unwrap();
    /*

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

        */
    /*
    let h2 = client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let mut request = request::Head::default();
    request.method = method::POST;
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let h2 = h2.send_request(1.into(), request, false).wait().unwrap();

    // Send the data
    let b = [0; 300];
    let h2 = h2.send_data(1.into(), (&b[..]).into(), true).wait().unwrap();

    // Get the response headers
    let (resp, h2) = h2.into_future().wait().unwrap();

    match resp.unwrap() {
        Frame::Headers { headers, .. } => {
            assert_eq!(headers.status, status::OK);
        }
        _ => panic!("unexpected frame"),
    }

    // Get the response body
    let (data, h2) = h2.into_future().wait().unwrap();

    match data.unwrap() {
        Frame::Data { id, data, end_of_stream, .. } => {
            assert_eq!(id, 1.into());
            assert_eq!(data, &b"world"[..]);
            assert!(end_of_stream);
        }
        _ => panic!("unexpected frame"),
    }

    assert!(Stream::wait(h2).next().is_none());;
    */
}
