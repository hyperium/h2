pub mod support;
use support::prelude::*;

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

#[test]
fn read_preface_in_multiple_frames() {
    let _ = ::env_logger::init();

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
fn server_builder_set_max_concurrent_streams() {
    let _ = ::env_logger::init();
    let (io, client) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(1));

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_custom_settings(settings)
        .send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/"),
        )
        .send_frame(
            frames::headers(3)
                .request("GET", "https://example.com/"),
        )
        .send_frame(frames::data(1, &b"hello"[..]).eos(),)
        .recv_frame(frames::reset(3).refused())
        .recv_frame(frames::headers(1).response(200).eos())
        .close();

    let mut builder = Server::builder();
    builder.max_concurrent_streams(1);

    let h2 = builder
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|srv| {
            srv.into_future().unwrap().and_then(|(reqstream, srv)| {
                let (req, mut stream) = reqstream.unwrap();

                assert_eq!(req.method(), &http::Method::GET);

                let rsp =
                    http::Response::builder()
                        .status(200).body(())
                        .unwrap();
                stream.send_response(rsp, true).unwrap();

                srv.into_future().unwrap()
            })
        });

    h2.join(client).wait().expect("wait");
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

#[test]
fn sent_invalid_authority() {
    let _ = ::env_logger::init();
    let (io, client) = mock::new();

    let bad_auth = util::byte_str("not:a/good authority");
    let mut bad_headers: frame::Headers = frames::headers(1)
        .request("GET", "https://example.com/")
        .eos()
        .into();
    bad_headers.pseudo_mut().authority = Some(bad_auth);

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(bad_headers)
        .recv_frame(frames::reset(1).protocol_error())
        .close();

    let srv = Server::handshake(io)
        .expect("handshake")
        .and_then(|srv| srv.into_future().unwrap());

    srv.join(client).wait().expect("wait");
}

#[test]
fn sends_reset_cancel_when_body_is_dropped() {
    let _ = ::env_logger::init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(
            frames::headers(1)
                .request("POST", "https://example.com/")
        )
        .recv_frame(frames::headers(1).response(200).eos())
        .recv_frame(frames::reset(1).cancel())
        .close();

    let srv = Server::handshake(io).expect("handshake").and_then(|srv| {
        srv.into_future().unwrap().and_then(|(reqstream, srv)| {
            let (req, mut stream) = reqstream.unwrap();

            assert_eq!(req.method(), &http::Method::POST);

            let rsp = http::Response::builder().status(200).body(()).unwrap();
            stream.send_response(rsp, true).unwrap();

            srv.into_future().unwrap()
        })
    });

    srv.join(client).wait().expect("wait");
}
