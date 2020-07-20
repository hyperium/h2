use futures::future::join;
use futures::{FutureExt, StreamExt};
use h2_support::prelude::*;
use h2_support::DEFAULT_WINDOW_SIZE;
use std::task::Context;

#[tokio::test]
async fn single_stream_send_large_body() {
    h2_support::trace_init!();

    let payload = vec![0; 1024];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25, 151,
            33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 4, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[..])
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        if !h2.poll_unpin(&mut cx).is_ready() {
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
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.await.unwrap();
}

#[tokio::test]
async fn multiple_streams_with_payload_greater_than_default_window() {
    h2_support::trace_init!();

    let payload = vec![0; 16384 * 5 - 1];
    let payload_clone = payload.clone();

    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::headers(1).request("POST", "https://http2.akamai.com/"))
            .await;
        srv.recv_frame(frames::headers(3).request("POST", "https://http2.akamai.com/"))
            .await;
        srv.recv_frame(frames::headers(5).request("POST", "https://http2.akamai.com/"))
            .await;
        srv.recv_frame(frames::data(1, &payload[0..16_384])).await;
        srv.recv_frame(frames::data(1, &payload[16_384..(16_384 * 2)]))
            .await;
        srv.recv_frame(frames::data(1, &payload[(16_384 * 2)..(16_384 * 3)]))
            .await;
        srv.recv_frame(frames::data(1, &payload[(16_384 * 3)..(16_384 * 4 - 1)]))
            .await;
        srv.send_frame(frames::settings()).await;
        srv.recv_frame(frames::settings_ack()).await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        srv.send_frame(frames::headers(3).response(200).eos()).await;
        srv.send_frame(frames::headers(5).response(200).eos()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.unwrap();
        let request1 = Request::post("https://http2.akamai.com/").body(()).unwrap();
        let request2 = Request::post("https://http2.akamai.com/").body(()).unwrap();
        let request3 = Request::post("https://http2.akamai.com/").body(()).unwrap();
        let (response1, mut stream1) = client.send_request(request1, false).unwrap();
        let (_response2, mut stream2) = client.send_request(request2, false).unwrap();
        let (_response3, mut stream3) = client.send_request(request3, false).unwrap();

        // The capacity should be immediately
        // allocated to default window size (smaller than payload)
        stream1.reserve_capacity(payload_clone.len());
        assert_eq!(stream1.capacity(), DEFAULT_WINDOW_SIZE);

        stream2.reserve_capacity(payload_clone.len());
        assert_eq!(stream2.capacity(), 0);

        stream3.reserve_capacity(payload_clone.len());
        assert_eq!(stream3.capacity(), 0);

        stream1.send_data(payload_clone.into(), true).unwrap();

        // hold onto streams so they don't close
        // stream1 doesn't close because response1 is used
        let _res = conn.drive(response1).await.expect("response");
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn single_stream_send_extra_large_body_multi_frames_one_buffer() {
    h2_support::trace_init!();

    let payload = vec![0; 32_768];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25, 151,
            33, 233, 132,
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

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        if !h2.poll_unpin(&mut cx).is_ready() {
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

    // Get the response
    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.await.unwrap();
}

#[tokio::test]
async fn single_stream_send_body_greater_than_default_window() {
    h2_support::trace_init!();

    let payload = vec![0; 16384 * 5 - 1];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(frames::SETTINGS_ACK)
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25, 151,
            33, 233, 132,
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
        .write(&payload[16_384..(16_384 * 2)])
        .write(&[
            // DATA
            0, 64, 0, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384 * 2)..(16_384 * 3)])
        .write(&[
            // DATA
            0, 63, 255, 0, 0, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384 * 3)..(16_384 * 4 - 1)])
        // Read window update
        .read(&[0, 0, 4, 8, 0, 0, 0, 0, 0, 0, 0, 64, 0])
        .read(&[0, 0, 4, 8, 0, 0, 0, 0, 1, 0, 0, 64, 0])
        .write(&[
            // DATA
            0, 64, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[(16_384 * 4 - 1)..(16_384 * 5 - 1)])
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    // Poll h2 once to get notifications
    loop {
        // Run the connection until all work is done, this handles processing
        // the handshake.
        if !h2.poll_unpin(&mut cx).is_ready() {
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
        if !h2.poll_unpin(&mut cx).is_ready() {
            break;
        }
    }

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(response).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.await.unwrap();
}

#[tokio::test]
async fn single_stream_send_extra_large_body_multi_frames_multi_buffer() {
    h2_support::trace_init!();

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
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41, 172, 75, 143, 168, 233, 25, 151,
            33, 233, 132,
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

    let (mut client, mut h2) = client::handshake(mock).await.unwrap();

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
    let resp = h2.run(response).await.unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.await.unwrap();
}

#[tokio::test]
async fn send_data_receive_window_update() {
    h2_support::trace_init!();
    let (m, mut mock) = mock::new();

    let h2 = async move {
        let (mut client, mut h2) = client::handshake(m).await.unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        // Send request
        let (_response, mut stream) = client.send_request(request, false).unwrap();

        // Send data frame
        stream.send_data("hello".into(), false).unwrap();

        stream.reserve_capacity(frame::DEFAULT_INITIAL_WINDOW_SIZE as usize);

        // Wait for capacity
        let mut stream = h2
            .drive(util::wait_for_capacity(
                stream,
                frame::DEFAULT_INITIAL_WINDOW_SIZE as usize,
            ))
            .await;
        let payload = vec![0; frame::DEFAULT_INITIAL_WINDOW_SIZE as usize];
        stream.send_data(payload.into(), true).unwrap();

        // keep `stream` from being dropped in order to prevent
        // it from sending an RST_STREAM frame.
        std::mem::forget(stream);
        h2.await.unwrap();
    };

    let mock = async move {
        let _ = mock.assert_client_handshake().await;

        let frame = mock.next().await.unwrap();
        let request = assert_headers!(frame.unwrap());
        assert!(!request.is_end_stream());
        let frame = mock.next().await.unwrap();
        let data = assert_data!(frame.unwrap());

        // Update the windows
        let len = data.payload().len();
        let f = frame::WindowUpdate::new(StreamId::zero(), len as u32);
        mock.send(f.into()).await.unwrap();

        let f = frame::WindowUpdate::new(data.stream_id(), len as u32);
        mock.send(f.into()).await.unwrap();

        for _ in 0..3usize {
            let frame = mock.next().await.unwrap();
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload().len(), frame::DEFAULT_MAX_FRAME_SIZE as usize);
        }
        let frame = mock.next().await.unwrap();
        let data = assert_data!(frame.unwrap());
        assert_eq!(
            data.payload().len(),
            (frame::DEFAULT_MAX_FRAME_SIZE - 1) as usize
        );
    };

    join(mock, h2).await;
}
