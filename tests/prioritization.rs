#[macro_use]
extern crate h2_test_support;
use h2_test_support::prelude::*;

#[test]
fn single_stream_send_large_body() {
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

    let mut h2 = Client::handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let mut stream = h2.request(request, false).unwrap();

    // Reserve capacity to send the payload
    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn single_stream_send_extra_large_body_multi_frames_one_buffer() {
    let _ = ::env_logger::init();

    let payload = vec![0; 32_768];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 64, 0, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[0..16_384])
        .write(&[
            // DATA
            0, 64, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[16_384..])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let mut h2 = Client::handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let mut stream = h2.request(request, false).unwrap();

    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn single_stream_send_extra_large_body_multi_frames_multi_buffer() {
    let _ = ::env_logger::init();

    let payload = vec![0; 32_768];

    let mock = mock_io::Builder::new()
        // .handshake()
        .write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .write(frames::SETTINGS)
        .read(frames::SETTINGS)
        // Add wait to force the data writes to chill
        .wait(Duration::from_millis(10))

        // Rest

        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 64, 0, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[0..16_384])

        .write(frames::SETTINGS_ACK)
        .read(frames::SETTINGS_ACK)
        .wait(Duration::from_millis(10))

        .write(&[
            // DATA
            0, 64, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[16_384..])
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let mut h2 = Client::handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let mut stream = h2.request(request, false).unwrap();

    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn send_data_receive_window_update() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    let h2 = Client::handshake(m)
        .unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            // Send request
            let mut stream = h2.request(request, false).unwrap();

            // Send data frame
            stream.send_data("hello".into(), false).unwrap();

            stream.reserve_capacity(frame::DEFAULT_INITIAL_WINDOW_SIZE as usize);

            // Wait for capacity
            h2.drive(util::wait_for_capacity(
                stream,
                frame::DEFAULT_INITIAL_WINDOW_SIZE as usize,
            ))
        })
        .and_then(|(h2, mut stream)| {
            let payload = vec![0; frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];
            stream.send_data(payload.into(), true).unwrap();

            h2.unwrap()
        });

    let mock = mock.assert_client_handshake().unwrap()
        .and_then(|(_, mock)| mock.into_future().unwrap())
        .and_then(|(frame, mock)| {
            let request = assert_headers!(frame.unwrap());
            assert!(!request.is_end_stream());
            mock.into_future().unwrap()
        })
        .and_then(|(frame, mut mock)| {
            let data = assert_data!(frame.unwrap());

            // Update the windows
            let len = data.payload().len();
            let f = frame::WindowUpdate::new(StreamId::zero(), len as u32);
            mock.send(f.into()).unwrap();

            let f = frame::WindowUpdate::new(data.stream_id(), len as u32);
            mock.send(f.into()).unwrap();

            mock.into_future().unwrap()
        })
        // TODO: Dedup the following lines
        .and_then(|(frame, mock)| {
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload().len(), frame::DEFAULT_MAX_FRAME_SIZE as usize);
            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload().len(), frame::DEFAULT_MAX_FRAME_SIZE as usize);
            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload().len(), frame::DEFAULT_MAX_FRAME_SIZE as usize);
            mock.into_future().unwrap()
        })
        .and_then(|(frame, _)| {
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload().len(), (frame::DEFAULT_MAX_FRAME_SIZE-1) as usize);
            Ok(())
        });

    let _ = h2.join(mock).wait().unwrap();
}
