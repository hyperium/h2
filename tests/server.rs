extern crate h2;
extern crate http;
extern crate futures;
extern crate mock_io;
extern crate env_logger;

use h2::server::Server;

use futures::*;

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

#[test]
fn read_preface_in_multiple_frames() {
    let _ = ::env_logger::init().unwrap();

    let mock = mock_io::Builder::new()
        .read(b"PRI * HTTP/2.0")
        .read(b"\r\n\r\nSM\r\n\r\n")
        .write(SETTINGS)
        .read(SETTINGS)
        .write(SETTINGS_ACK)
        .read(SETTINGS_ACK)
        .build();

    let h2 = Server::handshake(mock)
        .wait().unwrap();

    assert!(Stream::wait(h2).next().is_none());
}

#[test]
#[ignore]
fn accept_with_pending_connections_after_socket_close() {
}
