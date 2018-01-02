#[macro_use]
pub mod support;
use support::prelude::*;

#[test]
fn write_continuation_frames() {
    // An invalid dependency ID results in a stream level error. The hpack
    // payload should still be decoded.
    let _ = ::env_logger::init();
    let (io, srv) = mock::new();

    let large = build_large_headers();

    // Build the large request frame
    let frame = large.iter().fold(
        frames::headers(1).request("GET", "https://http2.akamai.com/"),
        |frame, &(name, ref value)| frame.field(name, &value[..]));

    let srv = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(frame.eos())
        .send_frame(
            frames::headers(1)
                .response(204)
                .eos(),
        )
        .close();

    let client = client::handshake(io)
        .expect("handshake")
        .and_then(|(mut client, conn)| {
            let mut request = Request::builder();
            request.uri("https://http2.akamai.com/");

            for &(name, ref value) in &large {
                request.header(name, &value[..]);
            }

            let request = request
                .body(())
                .unwrap();

            let req = client
                .send_request(request, true)
                .expect("send_request1")
                .0
                .then(|res| {
                    let response = res.unwrap();
                    assert_eq!(response.status(), StatusCode::NO_CONTENT);
                    Ok::<_, ()>(())
                });

            conn.drive(req)
                .and_then(move |(h2, _)| {
                    h2.unwrap()
                })
        });

    client.join(srv).wait().expect("wait");
}

fn build_large_headers() -> Vec<(&'static str, String)> {
    vec![
        ("one", "hello".to_string()),
        ("two", build_large_string('2', 4 * 1024)),
        ("three", "three".to_string()),
        ("four", build_large_string('4', 4 * 1024)),
        ("five", "five".to_string()),
        ("six", build_large_string('6', 4 * 1024)),
        ("seven", "seven".to_string()),
        ("eight", build_large_string('8', 4 * 1024)),
        ("nine", "nine".to_string()),
        ("ten", build_large_string('0', 4 * 1024)),
    ]
}

fn build_large_string(ch: char, len: usize) -> String {
    let mut ret = String::new();

    for _ in 0..len {
        ret.push(ch);
    }

    ret
}
