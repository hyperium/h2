//! Black-box compatibility cases: this exact file is run against both
//! unmodified upstream and the concurrency candidate. No new API is used.
use futures::channel::oneshot;
use futures::StreamExt;
use h2_support::prelude::*;
use std::pin::Pin;
use std::task::Context;

const PENDING_ERROR: &str = "user error: send_ping before received previous pong";

fn prime<'a>(
    pings: &'a mut PingPong,
) -> Pin<Box<dyn Future<Output = Result<Pong, h2::Error>> + Send + 'a>> {
    // Deliberately retain the lifetime accepted by the original async method.
    // No 'static, owned-future, or new concurrency contract is assumed here.
    let mut future = Box::pin(pings.ping(Ping::opaque()));
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    future
}

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(5), test)
        .await
        .expect("compatibility test stalled");
}

#[tokio::test]
async fn compat_sequential_original_ping_result_and_send_trait() {
    fn assert_shape<F: Future<Output = Result<Pong, h2::Error>> + Send>(_: &F) {}
    bounded(async {
        let (io, mut peer) = mock::new();
        let server = async move {
            peer.assert_client_handshake().await;
            for _ in 0..2 {
                let ping = assert_ping!(peer.next().await.unwrap().unwrap());
                assert!(!ping.is_ack());
                peer.send_frame(frames::ping(*ping.payload()).pong()).await;
            }
            peer.recv_frame(frames::go_away(0)).await;
            peer.recv_eof().await;
        };
        let client = async move {
            let (client, mut conn) = client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
            for _ in 0..2 {
                let future = pings.ping(Ping::opaque());
                assert_shape(&future);
                let result: Result<Pong, h2::Error> = conn.drive(future).await;
                let _: Pong = result.unwrap();
            }
            drop(client);
            conn.await.unwrap();
        };
        join(server, client).await;
    })
    .await;
}

#[tokio::test]
async fn compat_unpolled_ping_does_not_reserve_or_send() {
    bounded(async {
        let (io, mut peer) = mock::new();
        let server = async move {
            peer.assert_client_handshake().await;
            peer.recv_frame(frames::ping(frame::Ping::USER)).await;
            peer.send_frame(frames::ping(frame::Ping::USER).pong())
                .await;
            peer.recv_frame(frames::go_away(0)).await;
            peer.recv_eof().await;
        };
        let client = async move {
            let (client, mut conn) = client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
            for _ in 0..128 {
                drop(pings.ping(Ping::opaque()));
            }
            pings.send_ping(Ping::opaque()).unwrap();
            conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
            drop(client);
            conn.await.unwrap();
        };
        join(server, client).await;
    })
    .await;
}

#[tokio::test]
async fn compat_low_level_pending_makes_high_level_first_poll_error() {
    bounded(async {
        let (io, mut peer) = mock::new();
        let server = async move {
            peer.assert_client_handshake().await;
            peer.recv_frame(frames::ping(frame::Ping::USER)).await;
            peer.send_frame(frames::ping(frame::Ping::USER).pong())
                .await;
            peer.recv_frame(frames::go_away(0)).await;
            peer.recv_eof().await;
        };
        let client = async move {
            let (client, mut conn) = client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
            pings.send_ping(Ping::opaque()).unwrap();
            assert_eq!(
                pings.ping(Ping::opaque()).await.unwrap_err().to_string(),
                PENDING_ERROR
            );
            conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
            drop(client);
            conn.await.unwrap();
        };
        join(server, client).await;
    })
    .await;
}

#[derive(Clone, Copy, Debug)]
enum CancelAt {
    Queued,
    Sent,
    Acked,
}

async fn cancellation_compatibility(stage: CancelAt) {
    bounded(async move {
        let (io, mut peer) = mock::new();
        let (sent_tx, sent_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let server = async move {
            peer.assert_client_handshake().await;
            let first = assert_ping!(peer.next().await.unwrap().unwrap());
            assert!(!first.is_ack());
            sent_tx.send(()).unwrap();
            ack_rx.await.unwrap();
            peer.send_frame(frames::ping(*first.payload()).pong()).await;
            // Barrier ensures the ACK is actually processed, not just written.
            peer.send_frame(frames::ping([0xc1; 8])).await;
            peer.recv_frame(frames::ping([0xc1; 8]).pong()).await;
            done_tx.send(()).unwrap();
            peer.recv_frame(frames::ping(frame::Ping::USER)).await;
            peer.send_frame(frames::ping(frame::Ping::USER).pong()).await;
            peer.recv_frame(frames::go_away(0)).await;
            peer.recv_eof().await;
        };
        let client = async move {
            let (client, mut conn) = client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
            let mut sent_rx = Some(sent_rx);
            let mut ack_tx = Some(ack_tx);
            let mut done_rx = Some(done_rx);
            {
                let future = prime(&mut pings);
                match stage {
                    CancelAt::Queued => {}
                    CancelAt::Sent => { conn.drive(sent_rx.take().unwrap()).await.unwrap(); }
                    CancelAt::Acked => {
                        conn.drive(sent_rx.take().unwrap()).await.unwrap();
                        ack_tx.take().unwrap().send(()).unwrap();
                        conn.drive(done_rx.take().unwrap()).await.unwrap();
                    }
                }
                drop(future);
            }
            // The old async ping leaves its single slot occupied when canceled.
            assert_eq!(pings.send_ping(Ping::opaque()).unwrap_err().to_string(), PENDING_ERROR);
            assert_eq!(pings.ping(Ping::opaque()).await.unwrap_err().to_string(), PENDING_ERROR);
            if let Some(sent_rx) = sent_rx { conn.drive(sent_rx).await.unwrap(); }
            if let Some(ack_tx) = ack_tx { ack_tx.send(()).unwrap(); }
            if let Some(done_rx) = done_rx { conn.drive(done_rx).await.unwrap(); }
            // Low-level polling can drain the orphaned high-level response.
            conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
            pings.send_ping(Ping::opaque()).unwrap();
            conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
            println!("cancel {:?}: both sends rejected until orphan drained by poll_pong; low-level send then succeeds", stage);
            drop(client);
            conn.await.unwrap();
        };
        join(server, client).await;
    }).await;
}

#[tokio::test]
async fn compat_cancel_started_before_wire_retains_drainable_slot() {
    cancellation_compatibility(CancelAt::Queued).await;
}

#[tokio::test]
async fn compat_cancel_sent_before_ack_retains_drainable_slot() {
    cancellation_compatibility(CancelAt::Sent).await;
}

#[tokio::test]
async fn compat_cancel_after_ack_before_result_poll_retains_drainable_slot() {
    cancellation_compatibility(CancelAt::Acked).await;
}

#[tokio::test]
async fn compat_connection_drop_discards_acked_but_unpolled_high_level_result() {
    bounded(async {
        let (io, mut peer) = mock::new();
        let (done_tx, done_rx) = oneshot::channel();
        let server = async move {
            peer.assert_client_handshake().await;
            let ping = assert_ping!(peer.next().await.unwrap().unwrap());
            peer.send_frame(frames::ping(*ping.payload()).pong()).await;
            peer.send_frame(frames::ping([0xc2; 8])).await;
            peer.recv_frame(frames::ping([0xc2; 8]).pong()).await;
            done_tx.send(()).unwrap();
            peer.recv_eof().await;
        };
        let client = async move {
            let (_client, mut conn) = client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
            let future = prime(&mut pings);
            conn.drive(done_rx).await.unwrap();
            drop(conn);
            assert_eq!(future.await.unwrap_err().to_string(), "broken pipe");
            assert_eq!(pings.send_ping(Ping::opaque()).unwrap_err().to_string(), "broken pipe");
            println!("ACK processed, result unpolled, then connection dropped: original high-level future reports broken pipe");
        };
        join(server, client).await;
    }).await;
}

#[tokio::test]
async fn compat_terminal_connection_retained_leaves_started_ping_pending_until_drop() {
    use futures::FutureExt;
    bounded(async {
        let (io,mut peer)=mock::new();
        let server=async move {
            peer.assert_client_handshake().await;
            let ping=assert_ping!(peer.next().await.unwrap().unwrap());
            assert!(!ping.is_ack());
            drop(peer);
        };
        let client=async move {
            let(_client,mut conn)=client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings=client::Connection::ping_pong(&mut conn).unwrap();
            let mut future=prime(&mut pings);
            let _terminal=(&mut conn).await;
            assert!(future.as_mut().now_or_never().is_none());
            drop(conn);
            assert_eq!(future.await.unwrap_err().to_string(),"broken pipe");
            println!("terminal connection retained: started ping stays Pending; actual Drop changes it to broken pipe");
        };
        join(server,client).await;
    }).await;
}

#[tokio::test]
async fn compat_first_handle_after_terminal_poll_stays_usable_until_drop() {
    use futures::FutureExt;
    bounded(async {
        let(io,mut peer)=mock::new();
        let server=async move {peer.assert_client_handshake().await;drop(peer);};
        let client=async move {
            let(_client,mut conn)=client::handshake(io).await.unwrap();
            let _terminal=(&mut conn).await;
            let mut pings=client::Connection::ping_pong(&mut conn).unwrap();
            pings.send_ping(Ping::opaque()).unwrap();
            assert!(poll_fn(|cx|pings.poll_pong(cx)).now_or_never().is_none());
            drop(conn);
            assert_eq!(poll_fn(|cx|pings.poll_pong(cx)).await.unwrap_err().to_string(),"broken pipe");
            println!("first handle after terminal poll: send succeeds and poll is Pending until actual connection Drop");
        };
        join(server,client).await;
    }).await;
}

#[tokio::test]
async fn compat_terminal_connection_retained_preserves_already_received_ack() {
    bounded(async {
        let(io,mut peer)=mock::new();
        let server=async move {
            peer.assert_client_handshake().await;
            let ping=assert_ping!(peer.next().await.unwrap().unwrap());
            peer.send_frame(frames::ping(*ping.payload()).pong()).await;
            peer.send_frame(frames::go_away(0)).await;
            peer.recv_frame(frames::go_away(0)).await;
            peer.recv_eof().await;
        };
        let client=async move {
            let(_client,mut conn)=client::handshake(io).await.unwrap();
            conn.drive(util::yield_once()).await;
            let mut pings=client::Connection::ping_pong(&mut conn).unwrap();
            let future=prime(&mut pings);
            (&mut conn).await.unwrap();
            let _:Pong=future.await.unwrap();
            println!("terminal connection retained: previously received ACK still returns Pong before actual Drop");
            drop(conn);
        };
        join(server,client).await;
    }).await;
}
