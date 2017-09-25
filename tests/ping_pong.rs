#[macro_use]
pub mod support;
use support::prelude::*;

#[test]
fn recv_single_ping() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    // Create the handshake
    let h2 = Client::handshake(m).unwrap().and_then(|conn| conn.unwrap());

    let mock = mock.assert_client_handshake()
        .unwrap()
        .and_then(|(_, mut mock)| {
            let frame = frame::Ping::new();
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
