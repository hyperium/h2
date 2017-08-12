#[macro_use]
extern crate log;

#[macro_use]
pub mod support;
use support::*;

#[test]
fn handshake() {
    let _ = ::env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(SETTINGS_ACK)
        .build();

    let h2 = Client::handshake(mock)
        .wait().unwrap();

    trace!("hands have been shook");

    // At this point, the connection should be closed
    h2.wait().unwrap();
}

#[test]
fn recv_invalid_server_stream_id() {
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
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 2, 137])
        .build();

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    info!("sending request");
    let stream = h2.request(request, true).unwrap();

    // The connection errors
    assert_proto_err!(h2.wait().unwrap_err(), ProtocolError);

    // The stream errors
    assert_proto_err!(stream.wait().unwrap_err(), ProtocolError);
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
fn sending_request_on_closed_soket() {
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
