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

    let (_, h2) = Client::handshake(mock).wait().unwrap();

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

    let h2 = Client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, h2)| {
            ::std::thread::spawn(move || {
                let request = Request::builder()
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();
                let res = client
                    .send_request(request, true)
                    .unwrap()
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

    let (mut client, h2) = Client::handshake(mock).wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let stream = client.send_request(request, true).unwrap();

    // The connection errors
    assert!(h2.wait().is_err());

    // The stream errors
    assert!(stream.wait().is_err());
}

#[test]
fn request_stream_id_overflows() {
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();


    let h2 = Client::builder()
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
            let req = client.send_request(request, true).unwrap().unwrap();

            h2.drive(req).and_then(move |(h2, _)| {
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

                h2.expect("h2")
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
        .send_frame(frames::headers(::std::u32::MAX >> 1).response(200))
        .close();

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

    let h2 = Client::handshake(io)
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
            let req = client.send_request(request, true).unwrap().unwrap();
            h2.drive(req).map(move |(h2, _)| (client, h2))
        })
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let mut req = client.send_request(request, false).unwrap();

            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // second request is put into pending_open
            let mut req2 = client.send_request(request, false).unwrap();

            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // third stream is over max concurrent
            assert!(client.poll_ready().expect("poll_ready").is_not_ready());

            let err = client.send_request(request, true).unwrap_err();
            assert_eq!(err.to_string(), "user error: rejected");

            req.send_data("hello".into(), true).expect("req send_data");
            h2.drive(req.expect("req")).and_then(move |(h2, _)| {
                req2.send_data("hello".into(), true)
                    .expect("req2 send_data");
                h2.expect("h2").join(req2.expect("req2"))
            })
        });

    h2.join(srv).wait().expect("wait");
}

#[test]
#[ignore]
fn request_without_scheme() {}

#[test]
#[ignore]
fn request_with_h1_version() {}


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

    let h2 = Client::handshake(io)
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
