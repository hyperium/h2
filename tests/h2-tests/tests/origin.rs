use futures::channel::oneshot;
use futures::StreamExt;
use h2_support::prelude::*;

#[tokio::test]
async fn server_sends_origins() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();
    let (tx, rx) = oneshot::channel();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        tx.send(()).unwrap();
        client
            .recv_frame(frames::origin([
                "https://a.example",
                "https://b.example:8443",
            ]))
            .await;
        client.ping_pong([1; 8]).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        // The origins are queued once the handshake is done.
        tokio::select! {
            _ = rx => {}
            _ = srv.next() => panic!("connection closed"),
        }
        srv.send_origins(["https://a.example", "https://b.example:8443"]);
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn server_origins_added_before_poll_share_a_frame() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();
    let (tx, rx) = oneshot::channel();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        tx.send(()).unwrap();
        client
            .recv_frame(frames::origin(["https://a.example", "https://b.example"]))
            .await;
        client.ping_pong([1; 8]).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        // The origins are queued once the handshake is done.
        tokio::select! {
            _ = rx => {}
            _ = srv.next() => panic!("connection closed"),
        }
        srv.send_origins(["https://a.example"]);
        srv.send_origins(["https://b.example"]);
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn server_skips_origins_that_cannot_be_encoded() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        // No ORIGIN frame is written for an origin which is skipped.
        client.ping_pong([1; 8]).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        srv.send_origins(["https://ä.example"]);
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn server_splits_origins_to_fit_max_frame_size() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();
    let (tx, rx) = oneshot::channel();

    let origins: Vec<String> = (0..1000)
        .map(|i| format!("https://{:04}.example", i))
        .collect();

    let client = {
        let origins = origins.clone();
        async move {
            let settings = client.assert_server_handshake().await;
            assert_default_settings!(settings);
            tx.send(()).unwrap();

            let mut frames = 0;
            let mut received = Vec::new();
            while received.len() < origins.len() {
                match client.next().await.unwrap().unwrap() {
                    frame::Frame::Origin(origin) => received.extend(origin.into_origins()),
                    frame => panic!("unexpected frame {:?}", frame),
                }
                frames += 1;
            }
            assert!(frames > 1);
            assert_eq!(received, origins);
            client.ping_pong([1; 8]).await;
        }
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        // The origins are queued once the handshake is done.
        tokio::select! {
            _ = rx => {}
            _ = srv.next() => panic!("connection closed"),
        }
        srv.send_origins(origins);
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn server_ignores_origin_frame() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);
        client
            .send_frame(frames::origin(["https://a.example"]))
            .await;
        client.ping_pong([1; 8]).await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn client_reads_origins() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let _ = srv.assert_client_handshake().await;
        srv.send_frame(frames::origin(["https://a.example", "https://b.example"]))
            .await;
        srv.send_frame(frames::origin(["https://b.example", "https://c.example"]))
            .await;
    };

    let h2 = async move {
        let (_client, mut conn) = client::handshake(io).await.unwrap();
        assert!(conn.received_origins().is_empty());
        assert!(!conn.has_received_origin("https://a.example"));
        (&mut conn).await.unwrap();

        let mut origins = conn.received_origins();
        origins.sort();
        assert_eq!(
            origins,
            [
                "https://a.example",
                "https://b.example",
                "https://c.example"
            ]
        );
        assert!(conn.has_received_origin("https://a.example"));
        assert!(!conn.has_received_origin("https://d.example"));
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn client_caps_origins() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let origins: Vec<String> = (0..300)
        .map(|i| format!("https://{:03}.example", i))
        .collect();

    let srv = {
        let origins = origins.clone();
        async move {
            let _ = srv.assert_client_handshake().await;
            // Duplicates are only counted once.
            srv.send_frame(frames::origin(origins.clone())).await;
            srv.send_frame(frames::origin(origins)).await;
        }
    };

    let h2 = async move {
        let (_client, mut conn) = client::handshake(io).await.unwrap();
        (&mut conn).await.unwrap();

        assert_eq!(conn.received_origins().len(), 256);
        assert!(conn.has_received_origin(&origins[0]));
        assert!(!conn.has_received_origin(&origins[299]));
    };

    join(srv, h2).await;
}

#[tokio::test]
async fn client_ignores_origin_frame_on_a_stream() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let _ = srv.assert_client_handshake().await;
        // An ORIGIN frame with stream id 1, which has to be ignored.
        let mut frame = vec![0, 0, 19, 0x0c, 0, 0, 0, 0, 1, 0, 17];
        frame.extend_from_slice(b"https://a.example");
        srv.send_bytes(&frame).await;
        srv.send_frame(frames::origin(["https://b.example"])).await;
    };

    let h2 = async move {
        let (_client, mut conn) = client::handshake(io).await.unwrap();
        (&mut conn).await.unwrap();

        assert_eq!(conn.received_origins(), ["https://b.example"]);
    };

    join(srv, h2).await;
}
