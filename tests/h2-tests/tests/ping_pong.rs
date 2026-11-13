use futures::channel::oneshot;
use futures::StreamExt;
use h2_support::assert_ping;
use h2_support::prelude::*;

#[tokio::test]
async fn recv_single_ping() {
    h2_support::trace_init!();
    let (m, mut mock) = mock::new();

    // Create the handshake
    let h2 = async move {
        let (_client, conn) = client::handshake(m).await.unwrap();
        let _: () = conn.await.unwrap();
    };

    let mock = async move {
        let _ = mock.assert_client_handshake().await;
        let frame = frame::Ping::new(Default::default());
        mock.send(frame.into()).await.unwrap();
        let frame = mock.next().await.unwrap();

        let pong = assert_ping!(frame.unwrap());

        // Payload is correct
        assert_eq!(*pong.payload(), <[u8; 8]>::default());

        // Is ACK
        assert!(pong.is_ack());
    };

    join(mock, h2).await;
}

#[tokio::test]
async fn recv_multiple_pings() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client.send_frame(frames::ping([1; 8])).await;
        client.send_frame(frames::ping([2; 8])).await;
        client.recv_frame(frames::ping([1; 8]).pong()).await;
        client.recv_frame(frames::ping([2; 8]).pong()).await;
    };

    let srv = async move {
        let mut s = server::handshake(io).await.expect("handshake");
        assert!(s.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn pong_has_highest_priority() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let data = Bytes::from(vec![0; 16_384]);
    let data_clone = data.clone();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(frames::headers(1).request("POST", "https://http2.akamai.com/"))
            .await;
        client.send_frame(frames::data(1, data_clone).eos()).await;
        client.send_frame(frames::ping([1; 8])).await;
        client.recv_frame(frames::ping([1; 8]).pong()).await;
        client
            .recv_frame(frames::headers(1).response(200).eos())
            .await;
    };

    let srv = async move {
        let mut s = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = s.next().await.unwrap().unwrap();
        assert_eq!(req.method(), "POST");
        let body = req.into_parts().1;

        let body = util::concat(body).await.expect("body");
        assert_eq!(body.len(), data.len());
        let res = Response::builder().status(200).body(()).unwrap();
        stream.send_response(res, true).expect("response");
        assert!(s.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn user_ping_pong() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(frames::ping(frame::Ping::USER)).await;
        srv.send_frame(frames::ping(frame::Ping::USER).pong()).await;
        srv.recv_frame(frames::go_away(0)).await;
        srv.recv_eof().await;
    };

    let client = async move {
        let (client, mut conn) = client::handshake(io).await.expect("client handshake");
        // yield once so we can ack server settings
        conn.drive(util::yield_once()).await;
        // `ping_pong()` method conflict with mock future ext trait.
        let mut ping_pong = client::Connection::ping_pong(&mut conn).expect("taking ping_pong");
        ping_pong.send_ping(Ping::opaque()).expect("send ping");

        // multiple pings results in a user error...
        assert_eq!(
            ping_pong
                .send_ping(Ping::opaque())
                .expect_err("ping 2")
                .to_string(),
            "user error: send_ping before received previous pong",
            "send_ping while ping pending is a user error",
        );

        conn.drive(futures::future::poll_fn(move |cx| ping_pong.poll_pong(cx)))
            .await
            .unwrap();
        drop(client);
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn user_notifies_when_connection_closes() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();
    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv
    };

    #[allow(clippy::async_yields_async)]
    let client = async move {
        let (_client, mut conn) = client::handshake(io).await.expect("client handshake");
        // yield once so we can ack server settings
        conn.drive(util::yield_once()).await;
        conn
    };

    let (srv, mut client) = join(srv, client).await;

    // `ping_pong()` method conflict with mock future ext trait.
    let mut ping_pong = client::Connection::ping_pong(&mut client).expect("taking ping_pong");

    // Spawn a thread so we can park a task waiting on `poll_pong`, and then
    // drop the client and be sure the parked task is notified...
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        poll_fn(|cx| ping_pong.poll_pong(cx))
            .await
            .expect_err("poll_pong should error");
        tx.send(ping_pong).unwrap();
    });

    // Sleep to let the ping thread park its task...
    idle_ms(50).await;
    drop(client);
    drop(srv);

    let mut ping_pong = rx.await.expect("ping pong spawn join");

    // Now that the connection is closed, also test `send_ping` errors...
    assert_eq!(
        ping_pong
            .send_ping(Ping::opaque())
            .expect_err("send_ping")
            .to_string(),
        "broken pipe",
    );
}

// These tests deliberately control the peer instead of using an auto-acking h2
// server: the peer must observe multiple PINGs before sending any ACK.
#[tokio::test]
async fn concurrent_pings_match_out_of_order_and_ignore_unknown_and_duplicate_acks() {
    use futures::FutureExt;

    let (io, mut srv) = mock::new();
    let (unknown_tx, unknown_rx) = oneshot::channel();
    let (continue_tx, continue_rx) = oneshot::channel();
    let (duplicate_tx, duplicate_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let (first_tx, first_rx) = oneshot::channel();

    let peer = async move {
        srv.assert_client_handshake().await;
        let mut payloads = Vec::new();
        for _ in 0..3 {
            let ping = assert_ping!(srv.next().await.unwrap().unwrap());
            assert!(!ping.is_ack());
            assert_ne!(*ping.payload(), frame::Ping::USER);
            assert_ne!(*ping.payload(), frame::Ping::SHUTDOWN);
            assert!(!payloads.contains(ping.payload()));
            payloads.push(*ping.payload());
        }

        // The challenge/response is a processing barrier for the preceding ACK.
        let unknown = [0xf1; 8];
        assert!(!payloads.contains(&unknown));
        srv.send_frame(frames::ping(unknown).pong()).await;
        srv.send_frame(frames::ping([0xf2; 8])).await;
        srv.recv_frame(frames::ping([0xf2; 8]).pong()).await;
        unknown_tx.send(()).unwrap();
        continue_rx.await.unwrap();

        srv.send_frame(frames::ping(payloads[2]).pong()).await;
        srv.send_frame(frames::ping(payloads[2]).pong()).await;
        srv.send_frame(frames::ping([0xf3; 8])).await;
        srv.recv_frame(frames::ping([0xf3; 8]).pong()).await;
        duplicate_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        srv.send_frame(frames::ping(payloads[1]).pong()).await;
        srv.send_frame(frames::ping([0xf4; 8])).await;
        srv.recv_frame(frames::ping([0xf4; 8]).pong()).await;
        second_tx.send(()).unwrap();
        first_rx.await.unwrap();
        srv.send_frame(frames::ping(payloads[0]).pong()).await;
        srv.recv_frame(frames::go_away(0)).await;
        srv.recv_eof().await;
    };

    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let mut first = Box::pin(prime_ping(&mut pings));
        let mut second = Box::pin(prime_ping(&mut pings));
        let mut third = Box::pin(prime_ping(&mut pings));
        drop(pings);
        // The connection still permits only one handle extraction, but live
        // owned futures remain valid after that handle is dropped.
        assert!(client::Connection::ping_pong(&mut conn).is_none());
        conn.drive(unknown_rx).await.unwrap();
        assert!(first.as_mut().now_or_never().is_none());
        assert!(second.as_mut().now_or_never().is_none());
        assert!(third.as_mut().now_or_never().is_none());
        continue_tx.send(()).unwrap();
        conn.drive(third).await.unwrap();
        conn.drive(duplicate_rx).await.unwrap();
        assert!(first.as_mut().now_or_never().is_none());
        assert!(second.as_mut().now_or_never().is_none());
        finish_tx.send(()).unwrap();
        conn.drive(second).await.unwrap();
        conn.drive(second_rx).await.unwrap();
        assert!(first.as_mut().now_or_never().is_none());
        first_tx.send(()).unwrap();
        conn.drive(first).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };

    tokio::time::timeout(Duration::from_secs(5), join(peer, client))
        .await
        .expect("concurrent ping test stalled");
}

#[tokio::test]
async fn concurrent_ping_limit_includes_unconsumed_completions_and_releases_slots() {
    let (io, mut srv) = mock::new();
    let (acked_tx, acked_rx) = oneshot::channel();
    let peer = async move {
        srv.assert_client_handshake().await;
        let mut payloads = Vec::new();
        for _ in 0..64 {
            let ping = assert_ping!(srv.next().await.unwrap().unwrap());
            assert!(!ping.is_ack());
            assert!(!payloads.contains(ping.payload()));
            payloads.push(*ping.payload());
        }
        for payload in payloads {
            srv.send_frame(frames::ping(payload).pong()).await;
        }
        srv.send_frame(frames::ping([0xe1; 8])).await;
        srv.recv_frame(frames::ping([0xe1; 8]).pong()).await;
        acked_tx.send(()).unwrap();
        let next = assert_ping!(srv.next().await.unwrap().unwrap());
        srv.send_frame(frames::ping(*next.payload()).pong()).await;
        srv.recv_frame(frames::go_away(0)).await;
        srv.recv_eof().await;
    };
    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let mut responses: Vec<_> = (0..64).map(|_| prime_ping(&mut pings)).collect();
        assert!(pings.ping(Ping::opaque()).await.is_err());
        conn.drive(acked_rx).await.unwrap();
        assert!(pings.ping(Ping::opaque()).await.is_err());
        responses.pop().unwrap().await.unwrap();
        let replacement = prime_ping(&mut pings);
        assert!(pings.ping(Ping::opaque()).await.is_err());
        for response in responses {
            response.await.unwrap();
        }
        conn.drive(replacement).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(peer, client))
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_ping_cancellation_drains_before_new_send_and_never_reuses_payload() {
    use futures::FutureExt;
    let (io, mut srv) = mock::new();
    let (sent_tx, sent_rx) = oneshot::channel();
    let (late_tx, late_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let peer = async move {
        srv.assert_client_handshake().await;
        let old = assert_ping!(srv.next().await.unwrap().unwrap());
        sent_tx.send(()).unwrap();
        srv.send_frame(frames::ping(*old.payload()).pong()).await;
        let next = assert_ping!(srv.next().await.unwrap().unwrap());
        assert_ne!(old.payload(), next.payload());
        srv.send_frame(frames::ping(*old.payload()).pong()).await;
        srv.send_frame(frames::ping([0xe2; 8])).await;
        srv.recv_frame(frames::ping([0xe2; 8]).pong()).await;
        late_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        srv.send_frame(frames::ping(*next.payload()).pong()).await;
        srv.recv_frame(frames::go_away(0)).await;
        srv.recv_eof().await;
    };
    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let cancelled = prime_ping(&mut pings);
        conn.drive(sent_rx).await.unwrap();
        drop(cancelled);
        assert!(pings.ping(Ping::opaque()).await.is_err());
        assert!(pings.send_ping(Ping::opaque()).is_err());
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        let mut replacement = prime_ping(&mut pings);
        conn.drive(late_rx).await.unwrap();
        assert!(replacement.as_mut().now_or_never().is_none());
        finish_tx.send(()).unwrap();
        conn.drive(replacement).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(peer, client))
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_and_legacy_sends_reject_overlap_but_allow_sequential_transition() {
    let (io, mut peer) = mock::new();
    let server = async move {
        peer.assert_client_handshake().await;
        let a = assert_ping!(peer.next().await.unwrap().unwrap());
        let b = assert_ping!(peer.next().await.unwrap().unwrap());
        assert_ne!(a.payload(), b.payload());
        peer.send_frame(frames::ping(*b.payload()).pong()).await;
        peer.send_frame(frames::ping(*a.payload()).pong()).await;
        peer.recv_frame(frames::ping(frame::Ping::USER)).await;
        peer.send_frame(frames::ping(frame::Ping::USER).pong())
            .await;
        let last = assert_ping!(peer.next().await.unwrap().unwrap());
        peer.send_frame(frames::ping(*last.payload()).pong()).await;
        peer.recv_frame(frames::go_away(0)).await;
        peer.recv_eof().await;
    };
    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let first = prime_ping(&mut pings);
        let second = prime_ping(&mut pings);
        assert!(pings.send_ping(Ping::opaque()).is_err());
        conn.drive(first).await.unwrap();
        assert!(pings.send_ping(Ping::opaque()).is_err());
        conn.drive(second).await.unwrap();
        pings.send_ping(Ping::opaque()).unwrap();
        assert!(pings.ping(Ping::opaque()).await.is_err());
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        conn.drive(pings.ping(Ping::opaque())).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(server, client))
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_ping_acks_do_not_consume_server_shutdown_ping() {
    let (io, mut peer) = mock::new();
    let (sent_tx, sent_rx) = oneshot::channel();
    let client = async move {
        peer.assert_server_handshake().await;
        let first = assert_ping!(peer.next().await.unwrap().unwrap());
        let second = assert_ping!(peer.next().await.unwrap().unwrap());
        assert!(!first.is_ack());
        assert!(!second.is_ack());
        sent_tx.send(()).unwrap();
        peer.recv_frame(frames::go_away(2_147_483_647)).await;
        peer.recv_frame(frames::ping(frame::Ping::SHUTDOWN)).await;
        peer.send_frame(frames::ping(*second.payload()).pong())
            .await;
        peer.send_frame(frames::ping(*first.payload()).pong()).await;
        peer.send_frame(frames::ping(frame::Ping::SHUTDOWN).pong())
            .await;
        peer.recv_frame(frames::go_away(0)).await;
        peer.recv_eof().await;
    };
    let server = async move {
        let mut conn = server::handshake(io).await.unwrap();
        conn.next().drive(util::yield_once()).await;
        let mut pings = conn.ping_pong().unwrap();
        let first = prime_ping(&mut pings);
        let second = prime_ping(&mut pings);
        conn.next().drive(sent_rx).await.unwrap();
        conn.graceful_shutdown();
        assert!(conn.next().await.is_none());
        // Received ACKs remain available while the terminal object is retained.
        first.await.unwrap();
        second.await.unwrap();
        let late = prime_ping(&mut pings);
        drop(conn);
        assert!(late.await.is_err());
    };
    tokio::time::timeout(Duration::from_secs(5), join(client, server))
        .await
        .expect("shutdown ping matching test stalled");
}

// Prime the original lazy API once without driving the connection. This lets
// tests distinguish queued/in-flight PINGs from merely constructed futures.
fn prime_ping(
    pings: &mut PingPong,
) -> std::pin::Pin<Box<impl Future<Output = Result<Pong, h2::Error>> + Send + 'static>> {
    let mut future = Box::pin(pings.ping(Ping::opaque()));
    let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    future
}

#[tokio::test]
async fn original_ping_future_is_send_static_and_survives_handle_drop_before_first_poll() {
    fn assert_send_static<T: Send + 'static>(_: &T) {}

    let (io, mut peer) = mock::new();
    let server = async move {
        peer.assert_client_handshake().await;
        let ping = assert_ping!(peer.next().await.unwrap().unwrap());
        assert!(!ping.is_ack());
        peer.send_frame(frames::ping(*ping.payload()).pong()).await;
        peer.recv_frame(frames::go_away(0)).await;
        peer.recv_eof().await;
    };
    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let future = pings.ping(Ping::opaque());
        assert_send_static(&future);
        drop(pings);
        conn.drive(future).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(server, client))
        .await
        .expect("owned ping did not survive handle drop");
}

#[tokio::test]
async fn orphan_fifo_does_not_consume_live_response_or_skip_unacked_head() {
    use futures::FutureExt;
    let (io, mut peer) = mock::new();
    let (stage_tx, stage_rx) = oneshot::channel();
    let (next_tx, next_rx) = oneshot::channel();
    let server = async move {
        peer.assert_client_handshake().await;
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(*assert_ping!(peer.next().await.unwrap().unwrap()).payload());
        }
        // First live response and second orphan are ACKed, first orphan isn't.
        peer.send_frame(frames::ping(ids[0]).pong()).await;
        peer.send_frame(frames::ping(ids[2]).pong()).await;
        peer.send_frame(frames::ping([0xd1; 8])).await;
        peer.recv_frame(frames::ping([0xd1; 8]).pong()).await;
        stage_tx.send(()).unwrap();
        next_rx.await.unwrap();
        peer.send_frame(frames::ping(ids[1]).pong()).await;
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
        let mut live = prime_ping(&mut pings);
        let first_orphan = prime_ping(&mut pings);
        let second_orphan = prime_ping(&mut pings);
        drop(first_orphan);
        drop(second_orphan);
        conn.drive(stage_rx).await.unwrap();
        // poll_pong cannot steal the live ACK or skip the first orphan.
        assert!(poll_fn(|cx| pings.poll_pong(cx)).now_or_never().is_none());
        live.as_mut()
            .now_or_never()
            .expect("live ACK not retained")
            .unwrap();
        assert!(pings.ping(Ping::opaque()).await.is_err());
        next_tx.send(()).unwrap();
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        assert!(pings.send_ping(Ping::opaque()).is_err());
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        pings.send_ping(Ping::opaque()).unwrap();
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(server, client))
        .await
        .unwrap();
}

#[tokio::test]
async fn sixty_four_orphans_remain_bounded_and_all_must_drain_before_new_send() {
    let (io, mut peer) = mock::new();
    let (done_tx, done_rx) = oneshot::channel();
    let server = async move {
        peer.assert_client_handshake().await;
        let mut ids = Vec::new();
        for _ in 0..64 {
            ids.push(*assert_ping!(peer.next().await.unwrap().unwrap()).payload());
        }
        for id in ids.into_iter().rev() {
            peer.send_frame(frames::ping(id).pong()).await;
        }
        peer.send_frame(frames::ping([0xd3; 8])).await;
        peer.recv_frame(frames::ping([0xd3; 8]).pong()).await;
        done_tx.send(()).unwrap();
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
        let responses: Vec<_> = (0..64).map(|_| prime_ping(&mut pings)).collect();
        assert!(pings.ping(Ping::opaque()).await.is_err());
        drop(responses);
        assert!(pings.ping(Ping::opaque()).await.is_err());
        assert!(pings.send_ping(Ping::opaque()).is_err());
        conn.drive(done_rx).await.unwrap();
        for _ in 0..64 {
            assert!(pings.ping(Ping::opaque()).await.is_err());
            assert!(pings.send_ping(Ping::opaque()).is_err());
            conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        }
        pings.send_ping(Ping::opaque()).unwrap();
        conn.drive(poll_fn(|cx| pings.poll_pong(cx))).await.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(server, client))
        .await
        .unwrap();
}

#[tokio::test]
async fn connection_drop_wakes_live_future_and_orphan_drain_waiter() {
    let (io, mut peer) = mock::new();
    let server = async move {
        peer.assert_client_handshake().await;
        peer
    };
    #[allow(clippy::async_yields_async)]
    let client = async move {
        let (_client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        conn
    };
    let (peer, mut conn) = join(server, client).await;
    let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
    let live = prime_ping(&mut pings);
    drop(prime_ping(&mut pings));
    let (parked_tx, parked_rx) = oneshot::channel();
    let waiter = tokio::spawn(async move {
        let both = join(live, poll_fn(|cx| pings.poll_pong(cx)));
        let mut both = Box::pin(both);
        poll_fn(|cx| {
            assert!(both.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        parked_tx.send(()).unwrap();
        let (live, orphan) = both.await;
        assert_eq!(live.unwrap_err().to_string(), "broken pipe");
        assert_eq!(orphan.unwrap_err().to_string(), "broken pipe");
    });
    parked_rx.await.unwrap();
    drop(conn);
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn original_ping_calls_can_be_joined_directly() {
    let (io, mut peer) = mock::new();
    let server = async move {
        peer.assert_client_handshake().await;
        // Observe both requests before ACKing either one.
        let first = assert_ping!(peer.next().await.unwrap().unwrap());
        let second = assert_ping!(peer.next().await.unwrap().unwrap());
        assert_ne!(first.payload(), second.payload());
        peer.send_frame(frames::ping(*second.payload()).pong())
            .await;
        peer.send_frame(frames::ping(*first.payload()).pong()).await;
        peer.recv_frame(frames::go_away(0)).await;
        peer.recv_eof().await;
    };
    let client = async move {
        let (client, mut conn) = client::handshake(io).await.unwrap();
        conn.drive(util::yield_once()).await;
        let mut pings = client::Connection::ping_pong(&mut conn).unwrap();
        let (first, second) = conn
            .drive(async { tokio::join!(pings.ping(Ping::opaque()), pings.ping(Ping::opaque())) })
            .await;
        first.unwrap();
        second.unwrap();
        drop(client);
        conn.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), join(server, client))
        .await
        .expect("directly joined original ping calls stalled");
}
