use futures::StreamExt;
use h2_support::prelude::*;

#[tokio::test]
async fn raw_path_fragments_are_rejected_but_encoded_hashes_survive() {
    for path in [
        "/admin#fragment",
        "/admin?query#fragment",
        "/admin#\r\ninvalid",
        "/admin?query#\r\ninvalid",
    ] {
        let (io, mut peer) = mock::new();
        let mut headers: frame::Headers = frames::headers(1)
            .request("GET", "https://example.com/")
            .eos()
            .into();
        headers.pseudo_mut().path = Some(util::byte_str(path));
        let peer = async move {
            peer.assert_server_handshake().await;
            peer.send_frame(headers).await;
            peer.recv_frame(frames::reset(1).protocol_error()).await;
            for (id, path) in [(3, "/admin%23fragment"), (5, "/admin?query=%23fragment")] {
                peer.send_frame(
                    frames::headers(id)
                        .request("GET", format!("https://example.com{}", path))
                        .eos(),
                )
                .await;
                peer.recv_frame(frames::headers(id).response(200).eos())
                    .await;
            }
        };
        let server = async move {
            let mut connection = server::handshake(io).await.unwrap();
            for path in ["/admin%23fragment", "/admin?query=%23fragment"] {
                let (request, mut response) = connection.next().await.unwrap().unwrap();
                assert_eq!(request.uri().path_and_query().unwrap().as_str(), path);
                response.send_response(Response::new(()), true).unwrap();
            }
            assert!(connection.next().await.is_none());
        };
        tokio::time::timeout(Duration::from_secs(5), join(peer, server))
            .await
            .unwrap();
    }
}
