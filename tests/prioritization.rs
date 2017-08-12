pub mod support;
use support::*;

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

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    let request = Request::builder()
        .method(method::POST)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    let mut stream = h2.request(request, false).unwrap();

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), status::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn single_stream_send_extra_large_body_multi_frames() {
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
        .write(frames::SETTINGS_ACK)
        .write(&[
            // DATA
            0, 64, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[16_384..])
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    let request = Request::builder()
        .method(method::POST)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    let mut stream = h2.request(request, false).unwrap();

    // Send the data
    stream.send_data(payload.into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), status::NO_CONTENT);

    h2.wait().unwrap();
}
