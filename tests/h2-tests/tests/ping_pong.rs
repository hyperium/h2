#[macro_use]
extern crate h2_support;

use h2_support::prelude::*;

#[test]
fn recv_single_ping() {
    let _ = ::env_logger::try_init();
    let (m, mock) = mock::new();

    // Create the handshake
    let h2 = client::handshake(m)
        .unwrap()
        .and_then(|(client, conn)| {
            conn.unwrap()
                .map(|c| (client, c))
        });

    let mock = mock.assert_client_handshake()
        .unwrap()
        .and_then(|(_, mut mock)| {
            let frame = frame::Ping::new(Default::default());
            mock.send(frame.into()).unwrap();

            mock.into_future().unwrap()
        })
        .and_then(|(frame, _)| {
            let pong = assert_ping!(frame.unwrap());

            // Payload is correct
            assert_eq!(*pong.payload(), <[u8; 8]>::default());

            // Is ACK
            assert!(pong.is_ack());

            Ok(())
        });

    let _ = h2.join(mock).wait().unwrap();
}

#[test]
fn recv_multiple_pings() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let client = client.assert_server_handshake()
        .expect("client handshake")
        .recv_settings()
        .send_frame(frames::ping([1; 8]))
        .send_frame(frames::ping([2; 8]))
        .recv_frame(frames::ping([1; 8]).pong())
        .recv_frame(frames::ping([2; 8]).pong())
        .close();

    let srv = server::handshake(io)
        .expect("handshake")
        .and_then(|srv| {
            // future of first request, which never comes
            srv.into_future().unwrap()
        });

    srv.join(client).wait().expect("wait");
}

#[test]
fn pong_has_highest_priority() {
    let _ = ::env_logger::try_init();
    let (io, client) = mock::new();

    let data = Bytes::from(vec![0; 16_384]);

    let client = client.assert_server_handshake()
        .expect("client handshake")
        .recv_settings()
        .send_frame(
            frames::headers(1)
                .request("POST", "https://http2.akamai.com/")
        )
        .send_frame(frames::data(1, data.clone()).eos())
        .send_frame(frames::ping([1; 8]))
        .recv_frame(frames::ping([1; 8]).pong())
        .recv_frame(frames::headers(1).response(200).eos())
        .close();

    let srv = server::handshake(io)
        .expect("handshake")
        .and_then(|srv| {
            // future of first request
            srv.into_future().unwrap()
        }).and_then(move |(reqstream, srv)| {
            let (req, mut stream) = reqstream.expect("request");
            assert_eq!(req.method(), "POST");
            let body = req.into_parts().1;

            body.concat2()
                .expect("body")
                .and_then(move |body| {
                    assert_eq!(body.len(), data.len());
                    let res = Response::builder()
                        .status(200)
                        .body(())
                        .unwrap();
                    stream.send_response(res, true).expect("response");
                    srv.into_future().unwrap()
                })
        });

    srv.join(client).wait().expect("wait");
}

#[test]
fn user_ping_pong() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .expect("srv handshake")
        .recv_settings()
        .recv_frame(frames::ping(frame::Ping::USER))
        .send_frame(frames::ping(frame::Ping::USER).pong())
        .recv_frame(frames::go_away(0))
        .recv_eof();

    let client = client::handshake(io)
        .expect("client handshake")
        .and_then(|(client, conn)| {
            // yield once so we can ack server settings
            conn
                .drive(util::yield_once())
                .map(move |(conn, ())| (client, conn))
        })
        .and_then(|(client, mut conn)| {
            // `ping_pong()` method conflict with mock future ext trait.
            let mut ping_pong = client::Connection::ping_pong(&mut conn)
                .expect("taking ping_pong");
            ping_pong
                .send_ping(Ping::opaque())
                .expect("send ping");

            // multiple pings results in a user error...
            assert_eq!(
                ping_pong.send_ping(Ping::opaque()).expect_err("ping 2").to_string(),
                "user error: send_ping before received previous pong",
                "send_ping while ping pending is a user error",
            );

            conn
                .drive(futures::future::poll_fn(move || {
                    ping_pong.poll_pong()
                }))
                .and_then(move |(conn, _pong)| {
                    drop(client);
                    conn.expect("client")
                })
        });

    client.join(srv).wait().expect("wait");
}

#[test]
fn user_notifies_when_connection_closes() {
    let _ = ::env_logger::try_init();
    let (io, srv) = mock::new();

    let srv = srv.assert_client_handshake()
        .expect("srv handshake")
        .recv_settings();

    let client = client::handshake(io)
        .expect("client handshake")
        .and_then(|(client, conn)| {
            // yield once so we can ack server settings
            conn
                .drive(util::yield_once())
                .map(move |(conn, ())| (client, conn))
        })
        .map(|(_client, conn)| conn);

    let (mut client, srv) = client.join(srv).wait().expect("wait");

    // `ping_pong()` method conflict with mock future ext trait.
    let mut ping_pong = client::Connection::ping_pong(&mut client)
        .expect("taking ping_pong");

    // Spawn a thread so we can park a task waiting on `poll_pong`, and then
    // drop the client and be sure the parked task is notified...
    let t = thread::spawn(move || {
        poll_fn(|| { ping_pong.poll_pong() })
            .wait()
            .expect_err("poll_pong should error");
        ping_pong
    });

    // Sleep to let the ping thread park its task...
    thread::sleep(Duration::from_millis(50));
    drop(client);
    drop(srv);

    let mut ping_pong = t.join().expect("ping pong thread join");

    // Now that the connection is closed, also test `send_ping` errors...
    assert_eq!(
        ping_pong.send_ping(Ping::opaque()).expect_err("send_ping").to_string(),
        "broken pipe",
    );
}
