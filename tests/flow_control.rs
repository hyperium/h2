#[macro_use]
extern crate h2_test_support;
use h2_test_support::prelude::*;

// In this case, the stream & connection both have capacity, but capacity is not
// explicitly requested.
#[test]
fn send_data_without_requesting_capacity() {
    let _ = ::env_logger::init();

    let payload = [0; 1024];

    let mock = mock_io::Builder::new()
        .handshake()
        .write(&[
            // POST /
            0, 0, 16, 1, 4, 0, 0, 0, 1, 131, 135, 65, 139, 157, 41,
            172, 75, 143, 168, 233, 25, 151, 33, 233, 132,
        ])
        .write(&[
            // DATA
            0, 4, 0, 0, 1, 0, 0, 0, 1,
        ])
        .write(&payload[..])
        .write(frames::SETTINGS_ACK)
        // Read response
        .read(&[0, 0, 1, 1, 5, 0, 0, 0, 1, 0x89])
        .build();

    let mut h2 = Client::handshake(mock)
        .wait().unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("https://http2.akamai.com/")
        .body(()).unwrap();

    let mut stream = h2.request(request, false).unwrap();

    // The capacity should be immediately allocated
    assert_eq!(stream.capacity(), 0);

    // Send the data
    stream.send_data(payload[..].into(), true).unwrap();

    // Get the response
    let resp = h2.run(poll_fn(|| stream.poll_response())).unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    h2.wait().unwrap();
}

#[test]
fn release_capacity_sends_window_update() {
    let _ = ::env_logger::init();

    let payload = vec![0u8; 16_384];

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        .send_frame(frames::data(1, &payload[..]))
        .send_frame(frames::data(1, &payload[..]))
        .send_frame(frames::data(1, &payload[..]))
        .recv_frame(
            frames::window_update(0, 32_768)
        )
        .recv_frame(
            frames::window_update(1, 32_768)
        )
        .send_frame(frames::data(1, &payload[..]).eos())
        // gotta end the connection
        .map(drop);

    let h2 = Client::handshake(io).unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let req = h2.request(request, true).unwrap()
                .unwrap()
                // Get the response
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.into_future().unwrap()
                })

                // read some body to use up window size to below half
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    body.into_future().unwrap()
                })
                .and_then(|(buf, body)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    body.into_future().unwrap()
                })
                .and_then(|(buf, mut body)| {
                    let buf = buf.unwrap();
                    assert_eq!(buf.len(), payload.len());
                    body.release_capacity(buf.len() * 2).unwrap();
                    body.into_future().unwrap()
                })
                .and_then(|(buf, _)| {
                    assert_eq!(buf.unwrap().len(), payload.len());
                    Ok(())
                });
            h2.unwrap().join(req)
        });
    h2.join(mock).wait().unwrap();
}

#[test]
fn release_capacity_of_small_amount_does_not_send_window_update() {
    let _ = ::env_logger::init();

    let payload = [0; 16];

    let (io, srv) = mock::new();

    let mock = srv.assert_client_handshake().unwrap()
        .recv_settings()
        .recv_frame(
            frames::headers(1)
                .request("GET", "https://http2.akamai.com/")
                .eos()
        )
        .send_frame(
            frames::headers(1)
                .response(200)
        )
        .send_frame(frames::data(1, &payload[..]).eos())
        // gotta end the connection
        .map(drop);

    let h2 = Client::handshake(io).unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let req = h2.request(request, true).unwrap()
                .unwrap()
                // Get the response
                .and_then(|resp| {
                    assert_eq!(resp.status(), StatusCode::OK);
                    let body = resp.into_parts().1;
                    body.into_future().unwrap()
                })
                // read the small body and then release it
                .and_then(|(buf, mut body)| {
                    let buf = buf.unwrap();
                    assert_eq!(buf.len(), 16);
                    body.release_capacity(buf.len()).unwrap();
                    body.into_future().unwrap()
                })
                .and_then(|(buf, _)| {
                    assert!(buf.is_none());
                    Ok(())
                });
            h2.unwrap().join(req)
        });
    h2.join(mock).wait().unwrap();
}

#[test]
#[ignore]
fn expand_window_sends_window_update() {
}

#[test]
#[ignore]
fn expand_window_calls_are_coalesced() {
}

#[test]
#[ignore]
fn recv_data_overflows_window() {
}

#[test]
#[ignore]
fn recv_window_update_causes_overflow() {
    // A received window update causes the window to overflow.
}

#[test]
fn stream_close_by_data_frame_releases_capacity() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    let window_size = frame::DEFAULT_INITIAL_WINDOW_SIZE as usize;

    let h2 = Client::handshake(m).unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            // Send request
            let mut s1 = h2.request(request, false).unwrap();

            // This effectively reserves the entire connection window
            s1.reserve_capacity(window_size);

            // The capacity should be immediately available as nothing else is
            // happening on the stream.
            assert_eq!(s1.capacity(), window_size);

            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            // Create a second stream
            let mut s2 = h2.request(request, false).unwrap();

            // Request capacity
            s2.reserve_capacity(5);

            // There should be no available capacity (as it is being held up by
            // the previous stream
            assert_eq!(s2.capacity(), 0);

            // Closing the previous stream by sending an empty data frame will
            // release the capacity to s2
            s1.send_data("".into(), true).unwrap();

            // The capacity should be available
            assert_eq!(s2.capacity(), 5);

            // Send the frame
            s2.send_data("hello".into(), true).unwrap();

            // Wait for the connection to close
            h2.unwrap()
        })
        ;

    let mock = mock.assert_client_handshake().unwrap()
        // Get the first frame
        .and_then(|(_, mock)| mock.into_future().unwrap())
        .and_then(|(frame, mock)| {
            let request = assert_headers!(frame.unwrap());

            assert_eq!(request.stream_id(), 1);
            assert!(!request.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let request = assert_headers!(frame.unwrap());

            assert_eq!(request.stream_id(), 3);
            assert!(!request.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let data = assert_data!(frame.unwrap());

            assert_eq!(data.stream_id(), 1);
            assert_eq!(data.payload().len(), 0);
            assert!(data.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, _)| {
            let data = assert_data!(frame.unwrap());

            assert_eq!(data.stream_id(), 3);
            assert_eq!(data.payload(), "hello");
            assert!(data.is_end_stream());

            Ok(())
        })
        ;

    let _ = h2.join(mock)
        .wait().unwrap();
}

#[test]
fn stream_close_by_trailers_frame_releases_capacity() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    let window_size = frame::DEFAULT_INITIAL_WINDOW_SIZE as usize;

    let h2 = Client::handshake(m).unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            // Send request
            let mut s1 = h2.request(request, false).unwrap();

            // This effectively reserves the entire connection window
            s1.reserve_capacity(window_size);

            // The capacity should be immediately available as nothing else is
            // happening on the stream.
            assert_eq!(s1.capacity(), window_size);

            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            // Create a second stream
            let mut s2 = h2.request(request, false).unwrap();

            // Request capacity
            s2.reserve_capacity(5);

            // There should be no available capacity (as it is being held up by
            // the previous stream
            assert_eq!(s2.capacity(), 0);

            // Closing the previous stream by sending a trailers frame will
            // release the capacity to s2
            s1.send_trailers(Default::default()).unwrap();

            // The capacity should be available
            assert_eq!(s2.capacity(), 5);

            // Send the frame
            s2.send_data("hello".into(), true).unwrap();

            // Wait for the connection to close
            h2.unwrap()
        })
        ;

    let mock = mock.assert_client_handshake().unwrap()
        // Get the first frame
        .and_then(|(_, mock)| mock.into_future().unwrap())
        .and_then(|(frame, mock)| {
            let request = assert_headers!(frame.unwrap());

            assert_eq!(request.stream_id(), 1);
            assert!(!request.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let request = assert_headers!(frame.unwrap());

            assert_eq!(request.stream_id(), 3);
            assert!(!request.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, mock)| {
            let trailers = assert_headers!(frame.unwrap());

            assert_eq!(trailers.stream_id(), 1);
            assert!(trailers.is_end_stream());

            mock.into_future().unwrap()
        })
        .and_then(|(frame, _)| {
            let data = assert_data!(frame.unwrap());

            assert_eq!(data.stream_id(), 3);
            assert_eq!(data.payload(), "hello");
            assert!(data.is_end_stream());

            Ok(())
        })
        ;

    let _ = h2.join(mock)
        .wait().unwrap();
}

#[test]
#[ignore]
fn stream_close_by_send_reset_frame_releases_capacity() {
}

#[test]
#[ignore]
fn stream_close_by_recv_reset_frame_releases_capacity() {
}

use futures::{Async, Poll};

struct GetResponse {
    stream: Option<client::Stream<Bytes>>,
}

impl Future for GetResponse {
    type Item = (Response<client::Body<Bytes>>, client::Stream<Bytes>);
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let response = match self.stream.as_mut().unwrap().poll_response() {
            Ok(Async::Ready(v)) => v,
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(e) => panic!("unexpected error; {:?}", e),
        };

        Ok(Async::Ready((response, self.stream.take().unwrap())))
    }
}

#[test]
fn recv_window_update_on_stream_closed_by_data_frame() {
    let _ = ::env_logger::init();
    let (m, mock) = mock::new();

    let h2 = Client::handshake(m).unwrap()
        .and_then(|mut h2| {
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://http2.akamai.com/")
                .body(()).unwrap();

            let stream = h2.request(request, false).unwrap();

            // Wait for the response
            h2.drive(GetResponse {
                stream: Some(stream),
            })
        })
        .and_then(|(h2, (response, mut stream))| {
            assert_eq!(response.status(), StatusCode::OK);

            // Send a data frame, this will also close the connection
            stream.send_data("hello".into(), true).unwrap();

            // Wait for the connection to close
            h2.unwrap()
        })
        ;

    let mock = mock.assert_client_handshake().unwrap()
        // Get the first frame
        .and_then(|(_, mock)| mock.into_future().unwrap())
        .and_then(|(frame, mut mock)| {
            let request = assert_headers!(frame.unwrap());

            assert_eq!(request.stream_id(), 1);
            assert!(!request.is_end_stream());

            // Send the response which also closes the stream
            let mut f = frame::Headers::new(
                request.stream_id(),
                frame::Pseudo::response(StatusCode::OK),
                HeaderMap::new());
            f.set_end_stream();

            mock.send(f.into()).unwrap();

            mock.into_future().unwrap()
        })
        .and_then(|(frame, mut mock)| {
            let data = assert_data!(frame.unwrap());
            assert_eq!(data.payload(), "hello");

            // Send a window update just for fun
            let f = frame::WindowUpdate::new(
                data.stream_id(), data.payload().len() as u32);

            mock.send(f.into()).unwrap();

            Ok(())
        })
        ;

    let _ = h2.join(mock)
        .wait().unwrap();
}
