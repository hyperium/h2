use futures::StreamExt;
use h2_support::prelude::*;
use tokio::sync::oneshot;

fn trailers(kind: &str) -> frame::Headers {
    let mut head = frames::headers(1);
    match kind {
        "small" => head = head.field("x-trailer", "ok"),
        "exact" => head = head.field("x", "x".repeat(223)),
        "one_over" => head = head.field("x", "x".repeat(224)),
        "single" => head = head.field("x-large", "x".repeat(512)),
        "aggregate" => {
            for i in 0..8 {
                head = head.field(format!("x-field-{}", i), "x".repeat(16));
            }
        }
        _ => unreachable!(),
    }
    head.eos().into()
}

async fn check_body(mut body: RecvStream, kind: &str) {
    let valid = matches!(kind, "small" | "exact");
    while let Some(data) = body.data().await {
        match data {
            Ok(bytes) => body.flow_control().release_capacity(bytes.len()).unwrap(),
            Err(error) => {
                assert!(!valid);
                assert_eq!(error.reason(), Some(Reason::PROTOCOL_ERROR));
                return;
            }
        }
    }
    assert!(valid, "oversized trailers must not complete the body");
    let trailers = body.trailers().await.unwrap().unwrap();
    assert_eq!(trailers.len(), 1);
    if kind == "exact" {
        assert_eq!(trailers["x"], "x".repeat(223));
    } else {
        assert_eq!(trailers["x-trailer"], "ok");
    }
}

#[tokio::test]
async fn request_trailers_enforce_configured_header_list_size() {
    for local_closed in [false, true] {
        for kind in ["small", "exact", "one_over", "single", "aggregate"] {
            let valid = matches!(kind, "small" | "exact");
            let (io, mut peer) = mock::new();
            let peer = async move {
                peer.assert_server_handshake().await;
                peer.send_frame(frames::headers(1).request("POST", "https://example.com/"))
                    .await;
                let head = frames::headers(1).response(200);
                peer.recv_frame(if local_closed { head.eos() } else { head })
                    .await;
                peer.send_frame(frames::data(1, "body")).await;
                peer.send_frame(trailers(kind)).await;
                if !valid {
                    peer.recv_frame(frames::reset(1).protocol_error()).await;
                } else if !local_closed {
                    peer.recv_frame(frames::data(1, "").eos()).await;
                }
                let mut sibling = frames::headers(3).request("GET", "https://example.com/");
                if kind == "aggregate" {
                    sibling = sibling.field("x-field-7", "x".repeat(16));
                }
                peer.send_frame(sibling.eos()).await;
                peer.recv_frame(frames::headers(3).response(200).eos())
                    .await;
            };
            let server = async move {
                let mut connection = server::Builder::new()
                    .max_header_list_size(256)
                    .handshake::<_, Bytes>(io)
                    .await
                    .unwrap();
                let (request, mut response) = connection.next().await.unwrap().unwrap();
                let mut send = response
                    .send_response(Response::new(()), local_closed)
                    .unwrap();
                let receive = async move {
                    check_body(request.into_body(), kind).await;
                    if valid && !local_closed {
                        send.send_data(Bytes::new(), true).unwrap();
                    }
                };
                let (_, sibling) = join(receive, connection.next()).await;
                let (request, mut response) = sibling.unwrap().unwrap();
                if kind == "aggregate" {
                    assert_eq!(request.headers()["x-field-7"], "x".repeat(16));
                }
                response.send_response(Response::new(()), true).unwrap();
                assert!(connection.next().await.is_none());
            };
            tokio::time::timeout(Duration::from_secs(5), join(peer, server))
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn response_trailers_enforce_configured_header_list_size() {
    for local_closed in [false, true] {
        for kind in ["small", "exact", "one_over", "single", "aggregate"] {
            let valid = matches!(kind, "small" | "exact");
            let (io, mut peer) = mock::new();
            let (head_tx, head_rx) = oneshot::channel();
            let (finished_tx, finished_rx) = oneshot::channel();
            let peer = async move {
                peer.assert_client_handshake().await;
                let head = frames::headers(1).request("GET", "https://example.com/");
                peer.recv_frame(if local_closed { head.eos() } else { head })
                    .await;
                peer.send_frame(frames::headers(1).response(200)).await;
                head_rx.await.unwrap();
                peer.send_frame(frames::data(1, "body")).await;
                peer.send_frame(trailers(kind)).await;
                if !valid {
                    peer.recv_frame(frames::reset(1).protocol_error()).await;
                } else if !local_closed {
                    peer.recv_frame(frames::data(1, "").eos()).await;
                }
                finished_tx.send(()).unwrap();
                peer.recv_frame(
                    frames::headers(3)
                        .request("GET", "https://example.com/")
                        .eos(),
                )
                .await;
                let mut sibling = frames::headers(3).response(200);
                if kind == "aggregate" {
                    sibling = sibling.field("x-field-7", "x".repeat(16));
                }
                peer.send_frame(sibling.eos()).await;
            };
            let client = async move {
                let (mut client, mut connection) = client::Builder::new()
                    .max_header_list_size(256)
                    .handshake::<_, Bytes>(io)
                    .await
                    .unwrap();
                let request = Request::builder()
                    .uri("https://example.com/")
                    .body(())
                    .unwrap();
                let (response, mut send) = client.send_request(request, local_closed).unwrap();
                let response = connection.drive(response).await.unwrap();
                head_tx.send(()).unwrap();
                connection
                    .drive(check_body(response.into_body(), kind))
                    .await;
                if valid && !local_closed {
                    send.send_data(Bytes::new(), true).unwrap();
                }
                connection.drive(finished_rx).await.unwrap();
                drop(send);
                let response = connection
                    .drive(client.get("https://example.com/"))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert!(response.body().is_end_stream());
                if kind == "aggregate" {
                    assert_eq!(response.headers()["x-field-7"], "x".repeat(16));
                }
                drop(client);
                connection.await.unwrap();
            };
            tokio::time::timeout(Duration::from_secs(5), join(peer, client))
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn oversized_trailers_after_end_stream_preserve_connection_error() {
    let (io, mut peer) = mock::new();
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let peer = async move {
        peer.assert_server_handshake().await;
        peer.send_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        accepted_rx.await.unwrap();
        peer.send_frame(trailers("single")).await;
        peer.recv_frame(frames::go_away(1).protocol_error()).await;
    };
    let server = async move {
        let mut connection = server::Builder::new()
            .max_header_list_size(256)
            .handshake::<_, Bytes>(io)
            .await
            .unwrap();
        let (mut request, response) = connection.next().await.unwrap().unwrap();
        accepted_tx.send(()).unwrap();
        let error = connection.next().await.unwrap().err().unwrap();
        assert!(error.is_go_away());
        assert_eq!(error.reason(), Some(Reason::PROTOCOL_ERROR));
        assert!(request.body_mut().data().await.is_none());
        drop(response);
    };
    tokio::time::timeout(Duration::from_secs(5), join(peer, server))
        .await
        .unwrap();
}
