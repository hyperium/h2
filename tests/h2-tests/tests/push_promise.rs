use h2_support::prelude::*;

#[test]
fn recv_push_works() {
    let _ = env_logger::try_init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(404))
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .send_frame(frames::data(1, "").eos())
        .send_frame(frames::headers(2).response(200))
        .send_frame(frames::data(2, "promised_data").eos());

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let (mut resp, _) = client
            .send_request(request, true)
            .unwrap();
        let pushed = resp.push_promises();
        let check_resp_status = resp.unwrap().map(|resp| {
            assert_eq!(resp.status(), StatusCode::NOT_FOUND)
        });
        let check_pushed_request = pushed.and_then(|headers| {
            let (request, response) = headers.into_parts();
            assert_eq!(request.into_parts().0.method, Method::GET);
            response
        });
        let check_pushed_response = check_pushed_request.and_then(
            |resp| {
                assert_eq!(resp.status(), StatusCode::OK);
                resp.into_body().concat2().map(|b| assert_eq!(b, "promised_data"))
            }
        ).collect().unwrap().map(|ps| {
            assert_eq!(1, ps.len())
        });
        h2.drive(check_resp_status.join(check_pushed_response))
    });

    h2.join(mock).wait().unwrap();
}

#[test]
fn pushed_streams_arent_dropped_too_early() {
    // tests that by default, received push promises work
    let _ = env_logger::try_init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(404))
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .send_frame(frames::push_promise(1, 4).request("GET", "https://http2.akamai.com/style2.css"))
        .send_frame(frames::data(1, "").eos())
        .idle_ms(10)
        .send_frame(frames::headers(2).response(200))
        .send_frame(frames::headers(4).response(200).eos())
        .send_frame(frames::data(2, "").eos())
        .recv_frame(frames::go_away(4));

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let (mut resp, _) = client
            .send_request(request, true)
            .unwrap();
        let pushed = resp.push_promises();
        let check_status = resp.unwrap().and_then(|resp| {
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
        let check_pushed_headers = pushed.and_then(|headers| {
            let (request, response) = headers.into_parts();
            assert_eq!(request.into_parts().0.method, Method::GET);
            response
        });
        let check_pushed = check_pushed_headers.map(
            |resp| assert_eq!(resp.status(), StatusCode::OK)
        ).collect().unwrap().and_then(|ps| {
            assert_eq!(2, ps.len());
            Ok(())
        });
        h2.drive(check_status.join(check_pushed)).and_then(|(conn, _)| conn.expect("client"))
    });

    h2.join(mock).wait().unwrap();
}

#[test]
fn recv_push_when_push_disabled_is_conn_error() {
    let _ = env_logger::try_init();

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
    let _ = env_logger::try_init();

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
            .and_then(move |(conn, _)| conn.expect("client").map(move |()| drop(client)))
    });

    client.join(srv).wait().expect("wait");
}

#[test]
fn recv_push_promise_over_max_header_list_size() {
    let _ = env_logger::try_init();
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
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .recv_frame(frames::reset(2).refused())
        .send_frame(frames::headers(1).response(200).eos())
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

            let req = client
                .send_request(request, true)
                .expect("send_request")
                .0
                .expect_err("response")
                .map(|err| {
                    assert_eq!(
                        err.reason(),
                        Some(Reason::REFUSED_STREAM)
                    );
                });

            conn.drive(req)
                .and_then(|(conn, _)| conn.expect("client"))
        });
    client.join(srv).wait().expect("wait");
}

#[test]
fn recv_invalid_push_promise_headers_is_stream_protocol_error() {
    // Unsafe method or content length is stream protocol error
    let _ = env_logger::try_init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::headers(1).response(404))
        .send_frame(frames::push_promise(1, 2).request("POST", "https://http2.akamai.com/style.css"))
        .send_frame(
            frames::push_promise(1, 4)
                .request("GET", "https://http2.akamai.com/style.css")
                .field(http::header::CONTENT_LENGTH, 1)
        )
        .send_frame(
            frames::push_promise(1, 6)
                .request("GET", "https://http2.akamai.com/style.css")
                .field(http::header::CONTENT_LENGTH, 0)
        )
        .send_frame(frames::headers(1).response(404).eos())
        .recv_frame(frames::reset(2).protocol_error())
        .recv_frame(frames::reset(4).protocol_error())
        .send_frame(frames::headers(6).response(200).eos())
        .close();

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();
        let (mut resp, _) = client
            .send_request(request, true)
            .unwrap();
        let check_pushed_request = resp.push_promises().and_then(|headers| {
            headers.into_parts().1
        });
        let check_pushed_response = check_pushed_request
            .collect().unwrap().map(|ps| {
                // CONTENT_LENGTH = 0 is ok
                assert_eq!(1, ps.len())
            });
        h2.drive(check_pushed_response)
    });

    h2.join(mock).wait().unwrap();
}

#[test]
#[ignore]
fn recv_push_promise_with_wrong_authority_is_stream_error() {
    // if server is foo.com, :authority = bar.com is stream error
}

#[test]
fn recv_push_promise_skipped_stream_id() {
    let _ = env_logger::try_init();

    let (io, srv) = mock::new();
    let mock = srv.assert_client_handshake()
        .unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .send_frame(frames::push_promise(1, 4).request("GET", "https://http2.akamai.com/style.css"))
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .recv_frame(frames::go_away(0).protocol_error())
        .close();

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = client
            .send_request(request, true)
            .unwrap()
            .0
            .then(|res| {
                assert!(res.is_err());
                Ok::<_, ()>(())
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
fn recv_push_promise_dup_stream_id() {
    let _ = env_logger::try_init();

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
        .send_frame(frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"))
        .recv_frame(frames::go_away(0).protocol_error())
        .close();

    let h2 = client::handshake(io).unwrap().and_then(|(mut client, h2)| {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = client
            .send_request(request, true)
            .unwrap()
            .0
            .then(|res| {
                assert!(res.is_err());
                Ok::<_, ()>(())
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
