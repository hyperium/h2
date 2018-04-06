#![deny(warnings)]
pub mod support;
use support::prelude::*;

const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

#[test]
fn read_preface_in_multiple_frames() {
    let _ = ::env_logger::try_init();

    let mock = mock_io::Builder::new()
        .read(b"PRI * HTTP/2.0")
        .read(b"\r\n\r\nSM\r\n\r\n")
        .write(SETTINGS)
        .read(SETTINGS)
        .write(SETTINGS_ACK)
        .read(SETTINGS_ACK)
        .build();

    let h2 = server::handshake(mock).wait().unwrap();

    assert!(Stream::wait(h2).next().is_none());
}

#[test]
fn server_builder_set_max_concurrent_streams() {
    let _ = ::env_logger::try_init();
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

    let mut builder = server::Builder::new();
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
    let _ = ::env_logger::try_init();
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

    let srv = server::handshake(io).expect("handshake").and_then(|srv| {
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
fn recv_invalid_authority() {
    let _ = ::env_logger::try_init();
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

    let srv = server::handshake(io)
        .expect("handshake")
        .and_then(|srv| srv.into_future().unwrap());

    srv.join(client).wait().expect("wait");
}

#[test]
fn recv_connection_header() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let req = |id, name, val| {
        frames::headers(id)
            .request("GET", "https://example.com/")
            .field(name, val)
            .eos()
    };

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(req(1, "connection", "foo"))
        .send_frame(req(3, "keep-alive", "5"))
        .send_frame(req(5, "proxy-connection", "bar"))
        .send_frame(req(7, "transfer-encoding", "chunked"))
        .send_frame(req(9, "upgrade", "HTTP/2.0"))
        .recv_frame(frames::reset(1).protocol_error())
        .recv_frame(frames::reset(3).protocol_error())
        .recv_frame(frames::reset(5).protocol_error())
        .recv_frame(frames::reset(7).protocol_error())
        .recv_frame(frames::reset(9).protocol_error())
        .close();

    let srv = server::handshake(io)
        .expect("handshake")
        .and_then(|srv| srv.into_future().unwrap());

    srv.join(client).wait().expect("wait");
}

#[test]
fn sends_reset_cancel_when_req_body_is_dropped() {
    let _ = ::env_logger::try_init();
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

    let srv = server::handshake(io).expect("handshake").and_then(|srv| {
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

#[test]
fn abrupt_shutdown() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(
            frames::headers(1)
                .request("POST", "https://example.com/")
        )
        .recv_frame(frames::go_away(1))
        .close();

    let srv = server::handshake(io).expect("handshake").and_then(|srv| {
        srv.into_future().unwrap().and_then(|(_, mut srv)| {
            srv.abrupt_shutdown(Reason::NO_ERROR);
            srv.into_future().unwrap()
        })
    });

    srv.join(client).wait().expect("wait");
}

#[test]
fn graceful_shutdown() {
    let _ = ::env_logger::try_init();
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
        // 2^31 - 1 = 2147483647
        // Note: not using a constant in the library because library devs
        // can be unsmart.
        .recv_frame(frames::go_away(2147483647))
        .recv_frame(frames::ping(frame::Ping::SHUTDOWN))
        .recv_frame(frames::headers(1).response(200).eos())
        // Pretend this stream was sent while the GOAWAY was in flight
        .send_frame(
            frames::headers(3)
                .request("POST", "https://example.com/"),
        )
        .send_frame(frames::ping(frame::Ping::SHUTDOWN).pong())
        .recv_frame(frames::go_away(3))
        // streams sent after GOAWAY receive no response
        .send_frame(
            frames::headers(7)
                .request("GET", "https://example.com/"),
        )
        .send_frame(frames::data(7, "").eos())
        .send_frame(frames::data(3, "").eos())
        .recv_frame(frames::headers(3).response(200).eos())
        .close(); //TODO: closed()?

    let srv = server::handshake(io)
        .expect("handshake")
        .and_then(|srv| {
            srv.into_future().unwrap()
        })
        .and_then(|(reqstream, mut srv)| {
            let (req, mut stream) = reqstream.unwrap();

            assert_eq!(req.method(), &http::Method::GET);

            srv.graceful_shutdown();

            let rsp = http::Response::builder()
                .status(200)
                .body(())
                .unwrap();
            stream.send_response(rsp, true).unwrap();

            srv.into_future().unwrap()
        })
        .and_then(|(reqstream, srv)| {
            let (req, mut stream) = reqstream.unwrap();
            assert_eq!(req.method(), &http::Method::POST);
            let body = req.into_parts().1;

            let body = body.concat2().and_then(move |buf| {
                assert!(buf.is_empty());

                let rsp = http::Response::builder()
                    .status(200)
                    .body(())
                    .unwrap();
                stream.send_response(rsp, true).unwrap();
                Ok(())
            });

            srv.into_future()
                .map(|(req, _srv)| {
                    assert!(req.is_none(), "unexpected request");
                })
                .drive(body)
                .and_then(|(srv, ())| {
                    srv.expect("srv")
                })
        });

    srv.join(client).wait().expect("wait");
}

#[test]
fn sends_reset_cancel_when_res_body_is_dropped() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_settings()
        .send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos()
        )
        .recv_frame(frames::headers(1).response(200))
        .recv_frame(frames::reset(1).cancel())
        .send_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos()
        )
        .recv_frame(frames::headers(3).response(200))
        .recv_frame(frames::data(3, vec![0; 10]))
        .recv_frame(frames::reset(3).cancel())
        .close();

    let srv = server::handshake(io).expect("handshake").and_then(|srv| {
        srv.into_future().unwrap().and_then(|(reqstream, srv)| {
            let (req, mut stream) = reqstream.unwrap();

            assert_eq!(req.method(), &http::Method::GET);

            let rsp = http::Response::builder()
                .status(200)
                .body(())
                .unwrap();
            stream.send_response(rsp, false).unwrap();
            // SendStream dropped

            srv.into_future().unwrap()
        }).and_then(|(reqstream, srv)| {
            let (_req, mut stream) = reqstream.unwrap();

            let rsp = http::Response::builder()
                .status(200)
                .body(())
                .unwrap();
            let mut tx = stream.send_response(rsp, false).unwrap();
            tx.send_data(vec![0; 10].into(), false).unwrap();
            // no send_data with eos

            srv.into_future().unwrap()
        })
    });

    srv.join(client).wait().expect("wait");
}

#[test]
fn too_big_headers_sends_431() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_custom_settings(
            frames::settings()
                .max_header_list_size(10)
        )
        .send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .field("some-header", "some-value")
                .eos()
        )
        .recv_frame(frames::headers(1).response(431).eos())
        .idle_ms(10)
        .close();

    let srv = server::Builder::new()
        .max_header_list_size(10)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|srv| {
            srv.into_future()
                .expect("server")
                .map(|(req, _)| {
                    assert!(req.is_none(), "req is {:?}", req);
                })
        });

    srv.join(client).wait().expect("wait");
}

#[test]
fn too_big_headers_sends_reset_after_431_if_not_eos() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let client = client
        .assert_server_handshake()
        .unwrap()
        .recv_custom_settings(
            frames::settings()
                .max_header_list_size(10)
        )
        .send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .field("some-header", "some-value")
        )
        .recv_frame(frames::headers(1).response(431).eos())
        .recv_frame(frames::reset(1).refused())
        .close();

    let srv = server::Builder::new()
        .max_header_list_size(10)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|srv| {
            srv.into_future()
                .expect("server")
                .map(|(req, _)| {
                    assert!(req.is_none(), "req is {:?}", req);
                })
        });

    srv.join(client).wait().expect("wait");
}
