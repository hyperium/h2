pub mod support;
use support::prelude::*;

#[test]
fn recv_push_works() {
    // tests that by default, received push promises work
    // TODO: once API exists, read the pushed response
    let _ = ::env_logger::init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .send_frame(frames::headers(1).response(200).eos())
        .send_frame(frames::headers(2).response(200).eos());

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let req = client
            .send_request(request, true)
            .unwrap()
            .0.unwrap()
            .and_then(|resp| {
                assert_eq!(resp.status(), StatusCode::OK);
                Ok(())
            });

        h2.drive(req)
    });

    h2.join(mock).wait().unwrap();
}

#[test]
fn recv_push_when_push_disabled_is_conn_error() {
    let _ = ::env_logger::init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .ignore_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::push_promise(1, 3).request("GET", "https://http2.akamai.com/style.css"))
        .send_frame(frames::headers(1).response(200).eos())
        .recv_frame(frames::go_away(0).protocol_error());

    let h2 = client::Builder::new()
        .enable_push(false)
        .handshake::<_, Bytes>(io)
        .unwrap()
        .and_then(|(mut client, h2)| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req = client.send_request(request, true).unwrap().0.then(|res| {
                let err = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "protocol error: unspecific protocol error detected"
                );
                Ok::<(), ()>(())
            });

            // client should see a protocol error
            let conn = h2.then(|res| {
                let err = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "protocol error: unspecific protocol error detected"
                );
                Ok::<(), ()>(())
            });

            conn.unwrap().join(req)
        });

    h2.join(mock).wait().unwrap();
}

#[test]
fn pending_push_promises_reset_when_dropped() {
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
        .send_frame(
            frames::push_promise(1, 2)
                .request("GET", "https://http2.akamai.com/style.css")
        )
        .send_frame(frames::headers(1).response(200).eos())
        .recv_frame(frames::reset(2).cancel())
        .close();

    let client = client::handshake(io).unwrap().and_then(|(mut client, conn)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let req = client
            .send_request(request, true)
            .unwrap()
            .0.expect("response")
            .and_then(|resp| {
                assert_eq!(resp.status(), StatusCode::OK);
                Ok(())
            });

        conn.drive(req)
            .and_then(|(conn, _)| conn.expect("client"))
    });

    client.join(srv).wait().expect("wait");
}

#[test]
#[ignore]
fn recv_push_promise_with_unsafe_method_is_stream_error() {
    // for instance, when :method = POST
}

#[test]
#[ignore]
fn recv_push_promise_with_wrong_authority_is_stream_error() {
    // if server is foo.com, :authority = bar.com is stream error
}
