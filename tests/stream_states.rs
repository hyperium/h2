#[macro_use]
extern crate log;

pub mod support;
use support::*;

#[test]
fn headers_only_bidirectional() {
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
        h2::Frame::Headers { headers, .. } => {
            assert_eq!(headers.status, status::NO_CONTENT);
        }
        _ => panic!("unexpected frame"),
    }

    // No more frames
    info!("ensure no more responses");
    assert!(Stream::wait(h2).next().is_none());;
}
