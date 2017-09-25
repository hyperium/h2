#[macro_use]
extern crate log;

extern crate h2_test_support;
use h2_test_support::prelude::*;

#[test]
fn handshake() {
    let _ = ::env_logger::init();

    let mock = mock_io::Builder::new()
        .handshake()
        .write(SETTINGS_ACK)
        .build();

    let h2 = Client::handshake(mock).wait().unwrap();

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
        // Write GO_AWAY
        .write(&[0, 0, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
        .build();

    let mut h2 = Client::handshake(mock).wait().unwrap();

    // Send the request
    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    info!("sending request");
    let stream = h2.send_request(request, true).unwrap();

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
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let req = h2.send_request(request, true).unwrap().unwrap();

            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();


            // second cannot use the next stream id, it's over

            let poll_err = h2.poll_ready().unwrap_err();
            assert_eq!(poll_err.to_string(), "user error: stream ID overflowed");

            let err = h2.send_request(request, true).unwrap_err();
            assert_eq!(err.to_string(), "user error: stream ID overflowed");

            h2.expect("h2").join(req)
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
        .recv_frame(frames::headers(1).request("POST", "https://example.com/"))
        .send_frame(frames::headers(1).response(200))
        .close();

    let h2 = Client::handshake(io)
        .expect("handshake")
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // first request is allowed
            let req = h2.send_request(request, false).unwrap().unwrap();

            // drive the connection some so we can receive the server settings
            h2.drive(req)
        })
        .and_then(|(mut h2, _)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://example.com/")
                .body(())
                .unwrap();

            // second stream is over max concurrent
            assert!(h2.poll_ready().expect("poll_ready").is_not_ready());

            let err = h2.send_request(request, true).unwrap_err();
            assert_eq!(err.to_string(), "user error: rejected");

            h2.expect("h2")
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
#[ignore]
fn sending_request_on_closed_soket() {}

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
