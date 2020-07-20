use futures::future::join;
use h2_support::prelude::*;

#[tokio::test]
async fn read_none() {
    let mut codec = Codec::from(mock_io::Builder::new().build());

    assert_closed!(codec);
}

#[test]
#[ignore]
fn read_frame_too_big() {}

// ===== DATA =====

#[tokio::test]
async fn read_data_no_padding() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 1,
            "hello",
        ];
    };

    let data = poll_frame!(Data, codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[tokio::test]
async fn read_data_empty_payload() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
    };

    let data = poll_frame!(Data, codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b""[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[tokio::test]
async fn read_data_end_stream() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 1, 0, 0, 0, 1,
            "hello",
        ];
    };

    let data = poll_frame!(Data, codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);
    assert!(data.is_end_stream());
    assert_closed!(codec);
}

#[tokio::test]
async fn read_data_padding() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 16, 0, 0x8, 0, 0, 0, 1,
            5,       // Pad length
            "helloworld", // Data
            "\0\0\0\0\0", // Padding
        ];
    };

    let data = poll_frame!(Data, codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"helloworld"[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[tokio::test]
async fn read_push_promise() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 0x5,
            0x5, 0x4,
            0, 0, 0, 0x1, // stream id
            0, 0, 0, 0x2, // promised id
            0x82, // HPACK :method="GET"
        ];
    };

    let pp = poll_frame!(PushPromise, codec);
    assert_eq!(pp.stream_id(), 1);
    assert_eq!(pp.promised_id(), 2);
    assert_eq!(pp.into_parts().0.method, Some(Method::GET));

    assert_closed!(codec);
}

#[tokio::test]
async fn read_data_stream_id_zero() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 0,
            "hello", // Data
        ];
    };

    poll_err!(codec);
}

// ===== HEADERS =====

#[test]
#[ignore]
fn read_headers_without_pseudo() {}

#[test]
#[ignore]
fn read_headers_with_pseudo() {}

#[test]
#[ignore]
fn read_headers_empty_payload() {}

#[tokio::test]
async fn read_continuation_frames() {
    h2_support::trace_init!();
    let (io, mut srv) = mock::new();

    let large = build_large_headers();
    let frame = large
        .iter()
        .fold(
            frames::headers(1).response(200),
            |frame, &(name, ref value)| frame.field(name, &value[..]),
        )
        .eos();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_default_settings!(settings);
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frame).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::handshake(io).await.expect("handshake");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let req = async {
            let res = client
                .send_request(request, true)
                .expect("send_request")
                .0
                .await
                .expect("response");
            assert_eq!(res.status(), StatusCode::OK);
            let (head, _body) = res.into_parts();
            let expected = large
                .iter()
                .fold(HeaderMap::new(), |mut map, &(name, ref value)| {
                    map.append(name, value.parse().unwrap());
                    map
                });
            assert_eq!(head.headers, expected);
        };

        conn.drive(req).await;
        conn.await.expect("client");
    };

    join(srv, client).await;
}

#[tokio::test]
async fn update_max_frame_len_at_rest() {
    use futures::StreamExt;

    h2_support::trace_init!();
    // TODO: add test for updating max frame length in flight as well?
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 1,
            "hello",
            0, 64, 1, 0, 0, 0, 0, 0, 1,
            vec![0; 16_385],
        ];
    };

    assert_eq!(poll_frame!(Data, codec).payload(), &b"hello"[..]);

    codec.set_max_recv_frame_size(16_384);

    assert_eq!(codec.max_recv_frame_size(), 16_384);
    assert_eq!(
        codec.next().await.unwrap().unwrap_err().to_string(),
        "frame with invalid size"
    );
}

#[tokio::test]
async fn read_goaway_with_debug_data() {
    let mut codec = raw_codec! {
        read => [
            // head
            0, 0, 22, 7, 0, 0, 0, 0, 0,
            // last_stream_id
            0, 0, 0, 1,
            // error_code
            0, 0, 0, 11,
            // debug_data
            "too_many_pings",
        ];
    };

    let data = poll_frame!(GoAway, codec);
    assert_eq!(data.reason(), Reason::ENHANCE_YOUR_CALM);
    assert_eq!(data.last_stream_id(), 1);
    assert_eq!(data.debug_data(), b"too_many_pings");

    assert_closed!(codec);
}
