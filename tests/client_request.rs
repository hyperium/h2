#[macro_use]
extern crate log;

pub mod support;
use support::prelude::*;

#[test]
fn handshake() {
    let _ = ::env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(SETTINGS_ACK)
        .build();

    let (_client, h2) = client::handshake(mock).wait().unwrap();

    trace!("hands have been shook");

    // At this point, the connection should be closed
    h2.wait().unwrap();
}

#[test]
fn client_other_thread() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200).eos())
        .close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            ::std::thread::spawn(move || {
                let request = Request::builder()
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();
                let res = client
                    .send_request(request, true)
                    .unwrap().0
                    .wait()
                    .expect("request");
                assert_eq!(res.status(), StatusCode::OK);
            });

            h2.expect("h2")
        });
    h2.join(srv).wait().expect("wait");
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
        // Write GO_AWAY
        .write(&[0, 0, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
        .build();

    let (mut client, h2) = client::handshake(mock).wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let (response, _) = client.send_request(request, true).unwrap();

    // The connection errors
    assert!(h2.wait().is_err());

    // The stream errors
    assert!(response.wait().is_err());
}

#[test]
fn request_stream_id_overflows() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();


    let h2 = client::Builder::new()
        .initial_stream_id(::std::u32::MAX >> 1)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let (response, _) = client.send_request(request, true).unwrap();

            h2.drive(response).and_then(move |(h2, _)| {
                let request = Request::builder()
                    .method(Method::GET)
                    .uri("https://example.com/")
                    .body(())
                    .unwrap();

                // second cannot use the next stream id, it's over

                let poll_err = client.poll_ready().unwrap_err();
                assert_eq!(poll_err.to_string(), "user error: stream ID overflowed");

                let err = client.send_request(request, true).unwrap_err();
                assert_eq!(err.to_string(), "user error: stream ID overflowed");

                h2.expect("h2").map(|ret| {
                    // Hold on to the `client` handle to avoid sending a GO_AWAY
                    // frame.
                    drop(client);
                    ret
                })
            })
        });

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(::std::u32::MAX >> 1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .send_frame(
            frames::headers(::std::u32::MAX >> 1)
                .response(200)
                .eos()
        )
        .idle_ms(10)
        .close();

    h2.join(srv).wait().expect("wait");
}

#[test]
fn client_builder_max_concurrent_streams() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let mut settings = frame::Settings::default();
    settings.set_max_concurrent_streams(Some(1));

    let srv = srv
        .assert_client_handshake()
        .unwrap()
        .recv_custom_settings(settings)
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos()
        )
        .send_frame(frames::headers(1).response(200).eos())
        .close();

    let mut builder = client::Builder::new();
    builder.max_concurrent_streams(1);

    let h2 = builder
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            let (response, _) = client.send_request(request, true).unwrap();
            h2.drive(response).map(move |(h2, _)| (client, h2))
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
fn request_over_max_concurrent_streams_errors() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();


    let srv = srv.assert_client_handshake_with_settings(frames::settings()
                // super tiny server
                .max_concurrent_streams(1))
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("POST", "https://example.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200).eos())
        .recv_frame(frames::headers(3).request("POST", "https://example.com/"))
        .send_frame(frames::headers(3).response(200))
        .recv_frame(frames::data(3, "hello").eos())
        .send_frame(frames::data(3, "").eos())
        .recv_frame(frames::headers(5).request("POST", "https://example.com/"))
        .send_frame(frames::headers(5).response(200))
        .recv_frame(frames::data(5, "hello").eos())
        .send_frame(frames::data(5, "").eos())
        .close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            // we send a simple req here just to drive the connection so we can
            // receive the server settings.
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let (response, _) = client.send_request(request, true).unwrap();
            h2.drive(response).map(move |(h2, _)| (client, h2))
        })
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let (resp1, mut stream1) = client.send_request(request, false).unwrap();

            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // second request is put into pending_open
            let (resp2, mut stream2) = client.send_request(request, false).unwrap();

            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // third stream is over max concurrent
            assert!(client.poll_ready().expect("poll_ready").is_not_ready());

            let err = client.send_request(request, true).unwrap_err();
            assert_eq!(err.to_string(), "user error: rejected");

            stream1.send_data("hello".into(), true).expect("req send_data");

            h2.drive(resp1.expect("req")).and_then(move |(h2, _)| {
                stream2.send_data("hello".into(), true)
                    .expect("req2 send_data");
                h2.expect("h2").join(resp2.expect("req2"))
            })
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
fn http_11_request_without_scheme_or_authority() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "/")
                .scheme("http")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200).eos())
        .close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            // we send a simple req here just to drive the connection so we can
            // receive the server settings.
            let request = Request::builder()
                .method(Method::GET)
                .uri("/")
                .body(())
                .unwrap();

            // first request is allowed
            let (response, _) = client.send_request(request, true).unwrap();
            h2.drive(response)
                .map(move |(h2, _)| (client, h2))
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
fn http_2_request_without_scheme_or_authority() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            // we send a simple req here just to drive the connection so we can
            // receive the server settings.
            let request = Request::builder()
                .version(Version::HTTP_2)
                .method(Method::GET)
                .uri("/")
                .body(())
                .unwrap();

            // first request is allowed
            assert!(client.send_request(request, true).is_err());
            h2.expect("h2").map(|ret| {
                // Hold on to the `client` handle to avoid sending a GO_AWAY frame.
                drop(client);
                ret
            })
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
#[ignore]
fn request_with_h1_version() {}

#[test]
fn request_with_connection_headers() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .close();

    let headers = vec![
        ("connection", "foo"),
        ("keep-alive", "5"),
        ("proxy-connection", "bar"),
        ("transfer-encoding", "chunked"),
        ("upgrade", "HTTP/2.0"),
        ("te", "boom"),
    ];

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(move |(mut client, conn)| {
            for (name, val) in headers {
                let req = Request::builder()
                    .uri("https://http2.akamai.com/")
                    .header(name, val)
                    .body(())
                    .unwrap();
                let err = client.send_request(req, true).expect_err(name);

                assert_eq!(err.to_string(), "user error: malformed headers");
            }
            conn.unwrap()
        });

    client.join(srv).wait().expect("wait");
}

#[test]
fn connection_close_notifies_response_future() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        // don't send any response, just close
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .then(|res| {
                    let err = res.expect_err("response");
                    assert_eq!(
                        err.to_string(),
                        "broken pipe"
                    );
                    Ok(())
                });

            conn.expect("conn").join(req)
        });

    client.join(srv).wait().expect("wait");
}

#[test]
fn connection_close_notifies_client_poll_ready() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .then(|res| {
                    let err = res.expect_err("response");
                    assert_eq!(
                        err.to_string(),
                        "broken pipe"
                    );
                    Ok::<_, ()>(())
                });

            conn.drive(req)
                .and_then(move |(_conn, _)| {
                    let err = client.poll_ready().expect_err("poll_ready");
                    assert_eq!(
                        err.to_string(),
                        "broken pipe"
                    );
                    Ok(())
                })
        });

    client.join(srv).wait().expect("wait");
}

#[test]
fn sending_request_on_closed_connection() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200).eos())
        // a bad frame!
        .send_frame(frames::headers(0).response(200).eos())
        .close();

    let h2 = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            // first request works
            let req = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .expect("response1")
                .map(|_| ());

            // after finish request1, there should be a conn error
            let h2 = h2.then(|res| {
                res.expect_err("h2 error");
                Ok::<(), ()>(())
            });

            h2.select(req)
                .then(|res| match res {
                    Ok((_, next)) => next,
                    Err(_) => unreachable!("both selected futures cannot error"),
                })
                .map(move |_| client)
        })
        .and_then(|mut client| {
            let poll_err = client.poll_ready().unwrap_err();
            let msg = "protocol error: unspecific protocol error detected";
            assert_eq!(poll_err.to_string(), msg);

            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();
            let send_err = client.send_request(request, true).unwrap_err();
            assert_eq!(send_err.to_string(), msg);

            Ok(())
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
fn recv_too_big_headers() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_custom_settings(
            frames::settings()
                .max_header_list_size(10)
        )
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .recv_frame(
            frames::headers(3)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(200).eos())
        .send_frame(frames::headers(3).response(200))
        // no reset for 1, since it's closed anyways
        // but reset for 3, since server hasn't closed stream
        .recv_frame(frames::reset(3).refused())
        .idle_ms(10)
        .close();

    let client = client::Builder::new()
        .max_header_list_size(10)
        .handshake::<_, Bytes>(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req1 = client
                .send_request(request, true)
                .expect("send_request")
                .0
                .expect_err("response1")
                .map(|err| {
                    assert_eq!(
                        err.reason(),
                        Some(Reason::REFUSED_STREAM)
                    );
                });

            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req2 = client
                .send_request(request, true)
                .expect("send_request")
                .0
                .expect_err("response2")
                .map(|err| {
                    assert_eq!(
                        err.reason(),
                        Some(Reason::REFUSED_STREAM)
                    );
                });

            conn.drive(req1.join(req2))
                .and_then(|(conn, _)| conn.expect("client"))
                .map(|c| (c, client))
        });

    client.join(srv).wait().expect("wait");

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
