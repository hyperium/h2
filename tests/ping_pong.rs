#[macro_use]
pub mod support;
use support::prelude::*;

#[test]
fn recv_single_ping() {
    let _ = ::env_logger::init();
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
    let _ = ::env_logger::init();
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
    let _ = ::env_logger::init();
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
