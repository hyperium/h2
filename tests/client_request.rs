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
    let _ = ::env_logger::init().unwrap();

    let mock = mock_io::Builder::new()
        .client_handshake()
        .build();

    let mut h2 = client::handshake(mock)
        .wait().unwrap();

    // At this point, the connection should be closed
    assert!(Stream::wait(h2).next().is_none());
}

#[test]
#[ignore] // Not working yet
fn hello_world() {
    let _ = ::env_logger::init().unwrap();

    let mock = mock_io::Builder::new()
        .client_handshake()
        // GET https://example.com/ HEADERS frame
        .write(&[0, 0, 13, 1, 5, 0, 0, 0, 1, 130, 135, 65, 136, 47, 145, 211, 93, 5, 92, 135, 167, 132])
        // .read(&[0, 0, 0, 1, 5, 0, 0, 0, 1])
        .build();

    let mut h2 = client::handshake(mock)
        .wait().unwrap();

    let mut request = request::Head::default();
    request.uri = "https://example.com/".parse().unwrap();
    // request.version = version::H2;

    println!("~~~ SEND REQUEST ~~~");
    // Send the request
    let mut h2 = h2.send_request(1, request, true).wait().unwrap();

    println!("~~~ WAIT FOR RESPONSE ~~~");
    // Iterate the response frames
    let mut h2 = Stream::wait(h2);

    let headers = h2.next().unwrap();

    // At this point, the connection should be closed
    assert!(h2.next().is_none());
}

#[test]
#[ignore]
fn request_without_scheme() {
}

#[test]
#[ignore]
fn request_with_h1_version() {
}

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

trait MockH2 {
    fn client_handshake(&mut self) -> &mut Self;
}

impl MockH2 for mock_io::Builder {
    fn client_handshake(&mut self) -> &mut Self {
        self.write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            // Settings frame
            .write(SETTINGS)
            /*
            .read(&[0, 0, 0, 4, 0, 0, 0, 0, 0])
            // Settings ACK
            .write(&[0, 0, 0, 4, 1, 0, 0, 0, 0])
            .read(&[0, 0, 0, 4, 1, 0, 0, 0, 0])
            */
    }
}
