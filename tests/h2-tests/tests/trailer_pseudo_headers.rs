use futures::StreamExt;
use h2_support::prelude::*;
use tokio::sync::oneshot;

fn trailers(kind: &str) -> frame::Headers {
    let mut pseudo = frame::Pseudo::default();
    match kind {
        "method" => pseudo.method = Some(Method::GET),
        "scheme" => pseudo.scheme = Some(util::byte_str("https")),
        "authority" => pseudo.authority = Some(util::byte_str("example.com")),
        "path" => pseudo.path = Some(util::byte_str("/other")),
        "protocol" => pseudo.protocol = Some(Protocol::from_static("websocket")),
        "status" => pseudo.status = Some(StatusCode::OK),
        "empty" => {}
        _ => unreachable!(),
    }
    frames::headers(1).pseudo(pseudo).eos().into()
}

async fn expect_body_error(mut body: RecvStream) {
    loop {
        match body
            .data()
            .await
            .expect("malformed trailers must not complete the body")
        {
            Ok(bytes) => body.flow_control().release_capacity(bytes.len()).unwrap(),
            Err(error) => {
                assert_eq!(error.reason(), Some(Reason::PROTOCOL_ERROR));
                break;
            }
        }
    }
}

#[tokio::test]
async fn request_trailer_pseudo_headers_reset_before_closing_receive_side() {
    for local_closed in [false, true] {
        for kind in [
            "method",
            "scheme",
            "authority",
            "path",
            "protocol",
            "status",
        ] {
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
                peer.recv_frame(frames::reset(1).protocol_error()).await;
                peer.send_frame(
                    frames::headers(3)
                        .request("GET", "https://example.com/")
                        .eos(),
                )
                .await;
                peer.recv_frame(frames::headers(3).response(200).eos())
                    .await;
            };
            let server = async move {
                let mut connection = server::handshake(io).await.unwrap();
                let (request, mut response) = connection.next().await.unwrap().unwrap();
                let send = response
                    .send_response(Response::new(()), local_closed)
                    .unwrap();
                let (_, sibling) =
                    join(expect_body_error(request.into_body()), connection.next()).await;
                drop(send);
                let (_, mut response) = sibling.unwrap().unwrap();
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
async fn response_trailer_pseudo_headers_reset_before_closing_receive_side() {
    for local_closed in [false, true] {
        for kind in [
            "method",
            "scheme",
            "authority",
            "path",
            "protocol",
            "status",
        ] {
            let (io, mut peer) = mock::new();
            let (head_tx, head_rx) = oneshot::channel();
            let peer = async move {
                peer.assert_client_handshake().await;
                let head = frames::headers(1).request("GET", "https://example.com/");
                peer.recv_frame(if local_closed { head.eos() } else { head })
                    .await;
                peer.send_frame(frames::headers(1).response(200)).await;
                head_rx.await.unwrap();
                peer.send_frame(frames::data(1, "body")).await;
                peer.send_frame(trailers(kind)).await;
                peer.recv_frame(frames::reset(1).protocol_error()).await;
                peer.recv_frame(
                    frames::headers(3)
                        .request("GET", "https://example.com/")
                        .eos(),
                )
                .await;
                peer.send_frame(frames::headers(3).response(200).eos())
                    .await;
            };
            let client = async move {
                let (mut client, mut connection) = client::handshake(io).await.unwrap();
                let request = Request::builder()
                    .uri("https://example.com/")
                    .body(())
                    .unwrap();
                let (response, send) = client.send_request(request, local_closed).unwrap();
                let response = connection.drive(response).await.unwrap();
                head_tx.send(()).unwrap();
                connection
                    .drive(expect_body_error(response.into_body()))
                    .await;
                drop(send);
                let response = connection
                    .drive(client.get("https://example.com/"))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert!(response.body().is_end_stream());
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
async fn trailer_pseudo_headers_after_end_stream_preserve_connection_error() {
    for kind in ["empty", "path", "status"] {
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
            peer.send_frame(trailers(kind)).await;
            peer.recv_frame(frames::go_away(1).protocol_error()).await;
        };
        let server = async move {
            let mut connection = server::handshake(io).await.unwrap();
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
}
