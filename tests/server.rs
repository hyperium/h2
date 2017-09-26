pub mod support;
use support::prelude::*;

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

    let h2 = Server::handshake(mock).wait().unwrap();

    assert!(Stream::wait(h2).next().is_none());
}

#[test]
fn serve_request() {
    let _ = ::env_logger::init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .recv_frame(frames::headers(1).response(200).eos())
        .close();

    let srv = Server::handshake(io).expect("handshake").and_then(|srv| {
        srv.into_future().unwrap().and_then(|(reqstream, srv)| {
            let (req, mut stream) = reqstream.unwrap();

            assert_eq!(req.method(), &http::Method::GET);

            let rsp = http::Response::builder().status(200).body(()).unwrap();
            stream.send_response(rsp, true).unwrap();

            srv.into_future().unwrap()
        })
    });

    srv.join(client).wait().expect("wait");
}

#[test]
#[ignore]
fn accept_with_pending_connections_after_socket_close() {}
