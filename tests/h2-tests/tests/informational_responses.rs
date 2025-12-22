#![deny(warnings)]

use futures::{future::poll_fn, StreamExt};
use h2_support::prelude::*;
use http::{Response, StatusCode};

#[tokio::test]
async fn send_100_continue() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        // Send a POST request
        client
            .send_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;

        // Expect 100 Continue response first
        client
            .recv_frame(frames::headers(1).response(StatusCode::CONTINUE))
            .await;

        // Send request body after receiving 100 Continue
        client
            .send_frame(frames::data(1, &b"request body"[..]).eos())
            .await;

        // Expect final response
        client
            .recv_frame(frames::headers(1).response(StatusCode::OK).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::POST);

        // Send 100 Continue informational response
        let continue_response = Response::builder()
            .status(StatusCode::CONTINUE)
            .body(())
            .unwrap();
        stream.send_informational(continue_response).unwrap();

        // Send final response
        let rsp = Response::builder().status(StatusCode::OK).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn send_103_early_hints() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        // Send a GET request
        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;

        // Expect 103 Early Hints response first
        client
            .recv_frame(frames::headers(1).response(StatusCode::EARLY_HINTS).field(
                "link",
                "</style.css>; rel=preload; as=style, </script.js>; rel=preload; as=script",
            ))
            .await;

        // Expect final response
        client
            .recv_frame(frames::headers(1).response(StatusCode::OK).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::GET);

        // Send 103 Early Hints informational response
        let early_hints_response = Response::builder()
            .status(StatusCode::EARLY_HINTS)
            .header(
                "link",
                "</style.css>; rel=preload; as=style, </script.js>; rel=preload; as=script",
            )
            .body(())
            .unwrap();
        stream.send_informational(early_hints_response).unwrap();

        // Send final response
        let rsp = Response::builder().status(StatusCode::OK).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn send_multiple_informational_responses() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        client
            .send_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;

        // Expect 100 Continue
        client
            .recv_frame(frames::headers(1).response(StatusCode::CONTINUE))
            .await;

        client
            .send_frame(frames::data(1, &b"request body"[..]).eos())
            .await;

        // Expect 103 Early Hints
        client
            .recv_frame(
                frames::headers(1)
                    .response(StatusCode::EARLY_HINTS)
                    .field("link", "</style.css>; rel=preload; as=style"),
            )
            .await;

        // Expect final response
        client
            .recv_frame(frames::headers(1).response(StatusCode::OK).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::POST);

        // Send 100 Continue
        let continue_response = Response::builder()
            .status(StatusCode::CONTINUE)
            .body(())
            .unwrap();
        stream.send_informational(continue_response).unwrap();

        // Send 103 Early Hints
        let early_hints_response = Response::builder()
            .status(StatusCode::EARLY_HINTS)
            .header("link", "</style.css>; rel=preload; as=style")
            .body(())
            .unwrap();
        stream.send_informational(early_hints_response).unwrap();

        // Send final response
        let rsp = Response::builder().status(StatusCode::OK).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn invalid_informational_status_returns_error() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        client
            .send_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;

        // Should only receive the final response since invalid informational response errors out
        client
            .recv_frame(frames::headers(1).response(StatusCode::OK).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::GET);

        // Try to send invalid informational response (200 is not 1xx)
        // This should return an error
        let invalid_response = Response::builder().status(StatusCode::OK).body(()).unwrap();
        let result = stream.send_informational(invalid_response);

        // Expect error for invalid status code
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("invalid informational status code"));

        // Send actual final response after error
        let rsp = Response::builder().status(StatusCode::OK).body(()).unwrap();
        stream.send_response(rsp, true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}

#[tokio::test]
async fn client_poll_informational_responses() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let srv = async move {
        let recv_settings = srv.assert_client_handshake().await;
        assert_default_settings!(recv_settings);

        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;

        // Send 103 Early Hints
        srv.send_frame(
            frames::headers(1)
                .response(StatusCode::EARLY_HINTS)
                .field("link", "</style.css>; rel=preload"),
        )
        .await;

        // Send final response
        srv.send_frame(frames::headers(1).response(StatusCode::OK).eos())
            .await;
    };

    let client = async move {
        let (client, connection) = client::handshake(io).await.expect("handshake");

        let request = Request::builder()
            .method("GET")
            .uri("https://example.com/")
            .body(())
            .unwrap();

        let (mut response_future, _) = client
            .ready()
            .await
            .unwrap()
            .send_request(request, true)
            .unwrap();

        let conn_fut = async move {
            connection.await.expect("connection error");
        };

        let response_fut = async move {
            // Poll for informational responses
            loop {
                match poll_fn(|cx| response_future.poll_informational(cx)).await {
                    Some(Ok(info_response)) => {
                        assert_eq!(info_response.status(), StatusCode::EARLY_HINTS);
                        assert_eq!(
                            info_response.headers().get("link").unwrap(),
                            "</style.css>; rel=preload"
                        );
                        break;
                    }
                    Some(Err(e)) => panic!("Error polling informational: {:?}", e),
                    None => break,
                }
            }

            // Get the final response
            let response = response_future.await.expect("response error");
            assert_eq!(response.status(), StatusCode::OK);
        };

        join(conn_fut, response_fut).await;
    };

    join(srv, client).await;
}

#[tokio::test]
async fn informational_responses_with_body_streaming() {
    h2_support::trace_init!();
    let (io, mut client) = mock::new();

    let client = async move {
        let settings = client.assert_server_handshake().await;
        assert_default_settings!(settings);

        client
            .send_frame(frames::headers(1).request("POST", "https://example.com/"))
            .await;

        // Expect 100 Continue
        client
            .recv_frame(frames::headers(1).response(StatusCode::CONTINUE))
            .await;

        client.send_frame(frames::data(1, &b"chunk1"[..])).await;

        // Expect 103 Early Hints while still receiving body
        client
            .recv_frame(
                frames::headers(1)
                    .response(StatusCode::EARLY_HINTS)
                    .field("link", "</resource.js>; rel=preload"),
            )
            .await;

        client
            .send_frame(frames::data(1, &b"chunk2"[..]).eos())
            .await;

        // Expect final response with streaming body
        client
            .recv_frame(frames::headers(1).response(StatusCode::OK))
            .await;

        client
            .recv_frame(frames::data(1, &b"response data"[..]).eos())
            .await;
    };

    let srv = async move {
        let mut srv = server::handshake(io).await.expect("handshake");
        let (req, mut stream) = srv.next().await.unwrap().unwrap();

        assert_eq!(req.method(), &http::Method::POST);

        // Send 100 Continue
        let continue_response = Response::builder()
            .status(StatusCode::CONTINUE)
            .body(())
            .unwrap();
        stream.send_informational(continue_response).unwrap();

        // Send 103 Early Hints while processing
        let early_hints_response = Response::builder()
            .status(StatusCode::EARLY_HINTS)
            .header("link", "</resource.js>; rel=preload")
            .body(())
            .unwrap();
        stream.send_informational(early_hints_response).unwrap();

        // Send final response with body
        let rsp = Response::builder().status(StatusCode::OK).body(()).unwrap();
        let mut send_stream = stream.send_response(rsp, false).unwrap();

        send_stream.send_data("response data".into(), true).unwrap();

        assert!(srv.next().await.is_none());
    };

    join(client, srv).await;
}
