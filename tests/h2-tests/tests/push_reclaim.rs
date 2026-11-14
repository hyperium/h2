use futures::{FutureExt, StreamExt};
use h2_support::prelude::*;
use std::task::Poll;

#[tokio::test]
async fn dropped_push_reclaims_connection_capacity_for_reuse() {
    h2_support::trace_init!();

    tokio::time::timeout(Duration::from_secs(5), async {
        const CHUNK: &[u8] = &[b'x'; 16 * 1024];
        for (accepted, end_stream) in [(true, true), (false, true), (false, false)] {
            let (io, mut peer) = mock::new();
            let (mut client, mut connection) = client::Builder::new()
                .enable_push(true)
                .handshake::<_, Bytes>(io)
                .await
                .unwrap();
            let (mut response, send_stream) = client
                .send_request(Request::get("https://example.com/").body(()).unwrap(), true)
                .unwrap();

            connection
                .drive(async {
                    peer.assert_client_handshake().await;
                    peer.recv_frame(
                        frames::headers(1)
                            .request("GET", "https://example.com/")
                            .eos(),
                    )
                    .await;
                    peer.send_frame(
                        frames::push_promise(1, 2).request("GET", "https://example.com/push"),
                    )
                    .await;
                    peer.send_frame(frames::headers(2).response(200)).await;
                    peer.send_frame(frames::data(2, CHUNK)).await;
                    let mut last = frames::data(2, CHUNK);
                    if end_stream {
                        last = last.eos();
                    }
                    peer.send_frame(last).await;
                    peer.send_frame(frames::headers(1).response(204).eos())
                        .await;

                    // the ACK proves all preceding push DATA is buffered, without
                    // taking the promise or releasing any application capacity.
                    peer.ping_pong([1; 8]).await;
                })
                .await;

            if accepted {
                let mut promises = response.push_promises();
                let promise = promises.push_promise().await.unwrap().unwrap();
                let (_, pushed_response) = promise.into_parts();
                drop(pushed_response.await.unwrap().into_body());
            }
            drop(response.await.unwrap().into_body());
            drop(send_stream);

            // mock writes never block. One driver poll flushes the drop
            // updates, so missing credit fails immediately rather than hanging.
            poll_fn(|cx| {
                assert!(connection.poll_unpin(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let mut reclaimed = 0;
            let mut resets = 0;
            while let Some(frame) = peer.next().now_or_never() {
                match frame.unwrap().unwrap() {
                    frame::Frame::WindowUpdate(update) => {
                        assert_eq!(update.stream_id(), StreamId::zero());
                        reclaimed += update.size_increment();
                    }
                    frame::Frame::Reset(reset) => {
                        assert_eq!(reset.stream_id(), StreamId::from(2));
                        assert_eq!(reset.reason(), Reason::CANCEL);
                        resets += 1;
                    }
                    frame => panic!("unexpected frame after dropping push: {:?}", frame),
                }
            }
            assert_eq!(
                reclaimed,
                32 * 1024,
                "accepted={accepted}, end_stream={end_stream}"
            );
            assert_eq!(resets, usize::from(!end_stream));

            let (response, send_stream) = client
                .send_request(
                    Request::get("https://example.com/next").body(()).unwrap(),
                    true,
                )
                .unwrap();
            connection
                .drive(join(
                    async {
                        peer.recv_frame(
                            frames::headers(3)
                                .request("GET", "https://example.com/next")
                                .eos(),
                        )
                        .await;
                        peer.send_frame(frames::headers(3).response(200)).await;
                        peer.send_frame(frames::data(3, CHUNK)).await;
                        peer.send_frame(frames::data(3, CHUNK).eos()).await;
                    },
                    async {
                        let body = response.await.unwrap().into_body();
                        assert_eq!(util::concat(body).await.unwrap(), vec![b'x'; 32 * 1024]);
                    },
                ))
                .await;
            drop(send_stream);
        }
    })
    .await
    .expect("push reclamation stalled");
}
