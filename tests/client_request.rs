extern crate h2;
extern crate http;
extern crate futures;
extern crate mock_io;
extern crate env_logger;

use h2::client;
use http::request;

use futures::*;

#[test]
fn handshake() {
    let _ = ::env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(SETTINGS_ACK)
        .build();

    let mut h2 = client::handshake(mock)
        .wait().unwrap();

    // At this point, the connection should be closed
    assert!(Stream::wait(h2).next().is_none());
}

#[test]
fn get_with_204_response() {
    let _ = ::env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        // Write GET /
        .write(&[
               0, 0, 0x10, 1, 5, 0, 0, 0, 1, 0x82, 0x87, 0x41, 0x8B, 0x9D, 0x29,
                0xAC, 0x4B, 0x8F, 0xA8, 0xE9, 0x19, 0x97, 0x21, 0xE9, 0x84,
        ])
        .write(SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let mut h2 = client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let mut request = request::Head::default();
    request.uri = "https://http2.akamai.com/".parse().unwrap();
    let h2 = h2.send_request(1.into(), request, true).wait().unwrap();

    // Get the response
    let (resp, h2) = h2.into_future().wait().unwrap();

    println!("RESP: {:?}", resp);

    assert!(Stream::wait(h2).next().is_none());
}

#[test]
#[ignore]
fn request_without_scheme() {
}

#[test]
#[ignore]
fn request_with_h1_version() {
}

#[test]
#[ignore]
fn invalid_client_stream_id() {
}

#[test]
#[ignore]
fn invalid_server_stream_id() {
}

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

trait MockH2 {
    fn handshake(&mut self) -> &mut Self;
}

impl MockH2 for mock_io::Builder {
    fn handshake(&mut self) -> &mut Self {
        self.write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            // Settings frame
            .write(SETTINGS)
            .read(SETTINGS)
            .read(SETTINGS_ACK)
    }
}
