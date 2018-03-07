#[macro_use]
pub mod support;
use support::{DEFAULT_WINDOW_SIZE};
use support::prelude::*;

#[test]
fn single_stream_send_large_body() {
    let _ = ::env_logger::try_init();

    let payload = [0; 1024];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
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
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let notify = MockNotify::new();
    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        notify.with(|| h2.poll()).unwrap();

        if !notify.is_notified() {
            break;
        }
    }

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let (response, mut stream) = client.send_request(request, false).unwrap();

    // Reserve capacity to send the payload
    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    assert!(notify.is_notified());

    // Get the response
    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn multiple_streams_with_payload_greater_than_default_window() {
    let _ = ::env_logger::try_init();

    let payload = vec![0; 16384*5-1];

    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1).request("POST", "https://http2.akamai.com/")
        )
        .recv_frame(
            frames::headers(3).request("POST", "https://http2.akamai.com/")
        )
        .recv_frame(
            frames::headers(5).request("POST", "https://http2.akamai.com/")
        )
        .recv_frame(frames::data(1, &payload[0..16_384]))
        .recv_frame(frames::data(1, &payload[16_384..(16_384*2)]))
        .recv_frame(frames::data(1, &payload[(16_384*2)..(16_384*3)]))
        .recv_frame(frames::data(1, &payload[(16_384*3)..(16_384*4-1)]))
        .send_frame(frames::settings())
        .recv_frame(frames::settings_ack())
        .send_frame(frames::headers(1).response(200).eos())
        .send_frame(frames::headers(3).response(200).eos())
        .send_frame(frames::headers(5).response(200).eos())
        .close();

    let client = client::handshake(io).unwrap()
        .and_then(|(mut client, conn)| {
            let request1 = Request::post("https://http2.akamai.com/").body(()).unwrap();
            let request2 = Request::post("https://http2.akamai.com/").body(()).unwrap();
            let request3 = Request::post("https://http2.akamai.com/").body(()).unwrap();
            let (response1, mut stream1) = client.send_request(request1, false).unwrap();
            let (_response2, mut stream2) = client.send_request(request2, false).unwrap();
            let (_response3, mut stream3) = client.send_request(request3, false).unwrap();

            // The capacity should be immediately
            // allocated to default window size (smaller than payload)
            stream1.reserve_capacity(payload.len());
            assert_eq!(stream1.capacity(), DEFAULT_WINDOW_SIZE);

            stream2.reserve_capacity(payload.len());
            assert_eq!(stream2.capacity(), 0);

            stream3.reserve_capacity(payload.len());
            assert_eq!(stream3.capacity(), 0);

            stream1.send_data(payload[..].into(), true).unwrap();

            // hold onto streams so they don't close
            // stream1 doesn't close because response1 is used
            conn.drive(response1.expect("response")).map(|c| (c, client, stream2, stream3))
        })
        .and_then(|((conn, _res), client, stream2, stream3)| {
            conn.expect("client").map(|c| (c, client, stream2, stream3))
        });

    srv.join(client).wait().unwrap();
}

#[test]
fn single_stream_send_extra_large_body_multi_frames_one_buffer() {
    let _ = ::env_logger::try_init();

    let payload = vec![0; 32_768];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
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
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let notify = MockNotify::new();
    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        notify.with(|| h2.poll()).unwrap();

        if !notify.is_notified() {
            break;
        }
    }

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let (response, mut stream) = client.send_request(request, false).unwrap();

    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    assert!(notify.is_notified());

    // Get the response
    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn single_stream_send_body_greater_than_default_window() {
    let _ = ::env_logger::try_init();

    let payload = vec![0; 16384*5-1];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
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
            0, 64, 0, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[16_384..(16_384*2)])
        .write(&[
            // DATA
            0, 64, 0, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384*2)..(16_384*3)])
        .write(&[
            // DATA
            0, 63, 255, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384*3)..(16_384*4-1)])

        // Read window update
        .read(&[0, 0, 4, 8, 0, 0, 0, 0, 0, 0, 0, 64, 0])
        .read(&[0, 0, 4, 8, 0, 0, 0, 0, 1, 0, 0, 64, 0])

        .write(&[
            // DATA
            0, 64, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384*4-1)..(16_384*5-1)])
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let notify = MockNotify::new();
    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        notify.with(|| h2.poll()).unwrap();

        if !notify.is_notified() {
            break;
        }
    }

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let (response, mut stream) = client.send_request(request, false).unwrap();

    // Flush request head
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        notify.with(|| h2.poll()).unwrap();

        if !notify.is_notified() {
            break;
        }
    }

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    assert!(notify.is_notified());

    // Get the response
    let resp = h2.run(response).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn single_stream_send_extra_large_body_multi_frames_multi_buffer() {
    let _ = ::env_logger::try_init();

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

    let (mut client, mut h2) = client::handshake(mock).wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let (response, mut stream) = client.send_request(request, false).unwrap();

    stream.reserve_capacity(payload.len());

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), payload.len());

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(response).unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn send_data_receive_window_update() {
    let _ = ::env_logger::try_init();
    let (m, mock) = mock::new();

    let h2 = client::handshake(m)
        .unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            // Send request
            let (response, mut stream) = client.send_request(request, false).unwrap();

            // Send data frame
            stream.send_data("hello".into(), false).unwrap();

            stream.reserve_capacity(frame::DEFAULT_INITIAL_WINDOW_SIZE as usize);

            // Wait for capacity
            h2.drive(util::wait_for_capacity(
                stream,
                frame::DEFAULT_INITIAL_WINDOW_SIZE as usize,
            ).map(|s| (response, s)))
        })
        .and_then(|(h2, (_r, mut stream))| {
            let payload = vec![0; frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];
            stream.send_data(payload.into(), true).unwrap();

            // keep `stream` from being dropped in order to prevent
            // it from sending an RST_STREAM frame.
            std::mem::forget(stream);
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
