#[macro_use]
extern crate log;

#[macro_use]
pub mod support;
use support::*;

use h2::Frame;

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

    let h2 = client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let mut request = request::Head::default();
    request.uri = "https://http2.akamai.com/".parse().unwrap();

    info!("sending request");
    let h2 = h2.send_request(1.into(), request, true).wait().unwrap();

    // Get the response

    info!("getting response");
    let (resp, h2) = h2.into_future().wait().unwrap();

    match resp.unwrap() {
        Frame::Headers { headers, .. } => {
            assert_eq!(headers.status, status::NO_CONTENT);
        }
        _ => panic!("unexpected frame"),
    }

    // No more frames
    info!("ensure no more responses");
    assert!(Stream::wait(h2).next().is_none());;
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

    let h2 = client::handshake(mock).wait().expect("handshake");

    // Send the request
    let mut request = request::Head::default();
    request.method = method::POST;
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let h2 = h2.send_request(1.into(), request, false).wait().expect("send request");

    let b = "hello";

    // Send the data
    let h2 = h2.send_data(1.into(), b.into(), true).wait().expect("send data");

    // Get the response headers
    let (resp, h2) = h2.into_future().wait().expect("into future");

    match resp.expect("response headers") {
        Frame::Headers { headers, .. } => {
            assert_eq!(headers.status, status::OK);
        }
        _ => panic!("unexpected frame"),
    }

    // Get the response body
    let (data, h2) = h2.into_future().wait().expect("into future");

    match data.expect("response data") {
        Frame::Data { id, data, end_of_stream, .. } => {
            assert_eq!(id, 1.into());
            assert_eq!(data, &b"world"[..]);
            assert!(end_of_stream);
        }
        _ => panic!("unexpected frame"),
    }

    assert!(Stream::wait(h2).next().is_none());;
}

#[test]
fn send_headers_recv_data() {
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

    let h2 = client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let mut request = request::Head::default();
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let h2 = h2.send_request(1.into(), request, true).wait().unwrap();

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
            assert_eq!(data, &b"hello"[..]);
            assert!(!end_of_stream);
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
}

#[test]
fn send_headers_twice_with_same_stream_id() {
    let _ = env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
            0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29,
                0xAC, 0x4B, 0x8F, 0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .build();

    let h2 = client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let mut request = request::Head::default();
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let h2 = h2.send_request(1.into(), request, true).wait().unwrap();

    // Send another request with the same stream ID
    let mut request = request::Head::default();
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let err = h2.send_request(1.into(), request, true).wait().unwrap_err();

    assert_user_err!(err, UnexpectedFrameType);
}

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
    request.method = method::POST;
    request.uri = "https://http2.akamai.com/".parse().unwrap();

    let id = 1.into();
    let h2 = h2.send_request(id, request, true).wait().expect("send request");

    let body = "hello";

    // Send the data
    let err = h2.send_data(id, body.into(), true).wait().unwrap_err();
    assert_user_err!(err, UnexpectedFrameType);
}

#[test]
fn send_data_without_headers() {
    let mock = mock_io::Builder::new()
        .handshake()
        .build();

    let h2 = client::handshake(mock)
        .wait().unwrap();

    let b = Bytes::from_static(b"hello world");
    let err = h2.send_data(1.into(), b, true).wait().unwrap_err();

    assert_user_err!(err, UnexpectedFrameType);
}

#[test]
#[ignore]
fn exceed_max_streams() {
}
