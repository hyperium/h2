extern crate h2_support;

use h2_support::prelude::*;

#[test]
fn write_continuation_frames() {
    // An invalid dependency ID results in a stream level error. The hpack
    // payload should still be decoded.
    let _ = ::env_logger::try_init();
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
                }).map(|c| {
                    (c, client)
                })
        });

    client.join(srv).wait().expect("wait");
}
