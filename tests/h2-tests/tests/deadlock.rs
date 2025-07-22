use std::{
    mem,
    net::SocketAddr,
    sync::{
        atomic::{self, AtomicUsize},
        Arc,
    },
    time::Instant,
};

use futures::StreamExt;
use h2_support::prelude::*;
use tokio::{net::TcpStream, runtime::Runtime, sync::Barrier};

static REQUESTS_COMPLETED: AtomicUsize = AtomicUsize::new(0);

// The magic happens when concurrency is much greater than max_concurrent_streams.
const CONCURRENCY: usize = 100;
const MAX_CONCURRENT_STREAMS: u32 = 20;

// If we successfully complete this many requests per task, we can call the test a success.
// Typically, this test seems to fail in the 10-25 request range but no doubt there are many
// variables in play, as failures are somewhat random and timing-dependent.
const TARGET_REQUESTS_PER_TASK: usize = 100;

// The test is successful if all the expected requests have been completed.
const TARGET_REQUESTS_COMPLETED: usize = CONCURRENCY * TARGET_REQUESTS_PER_TASK;

// If no requests have been completed in this long, we consider the connection deadlocked.
const MUST_MAKE_PROGRESS_INTERVAL: Duration = Duration::from_secs(2);
const CHECK_FOR_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

// For easy repro, run as `cargo test -p h2-tests --test deadlock -- --no-capture`
#[test]
fn logical_deadlock() {
    let server_addr = serve();

    // We start all the tasks at the same time for increased water hammer.
    let start = Arc::new(Barrier::new(CONCURRENCY));

    let runtime = Runtime::new().unwrap();

    runtime.block_on(async move {
        let tcp_connection = TcpStream::connect(server_addr).await.unwrap();
        let (client, h2_connection) = client::handshake(tcp_connection).await.unwrap();

        tokio::spawn(async move {
            // This just does "connection stuff" independent of any requests.
            h2_connection.await.unwrap();
        });

        let mut join_handles = Vec::new();

        for _ in 0..CONCURRENCY {
            join_handles.push(tokio::spawn({
                let mut client = client.clone();
                let start = start.clone();

                async move {
                    start.wait().await;

                    for _ in 0..TARGET_REQUESTS_PER_TASK {
                        let request = Request::builder()
                            .method(Method::POST)
                            .uri("/")
                            .body(())
                            .unwrap();

                        let (response_future, mut request_body) =
                            client.send_request(request, false).unwrap();

                        request_body
                            .send_data(Bytes::from_static(REQUEST_BODY), true)
                            .unwrap();

                        // The anomaly we expect to see will result in this await never completing
                        // for all `CONCURRENCY` concurrent requests enqueued on this connection.
                        let mut response = match response_future.await {
                            Ok(response) => response,
                            Err(e)
                                if e.is_reset() && e.reason() == Some(Reason::REFUSED_STREAM) =>
                            {
                                // Not relevant - it means the server sent a max_concurrent_streams
                                // limit and the client has not yet seen it, so it is not enforced.
                                // Not an ideal outcome in general but not relevant to this test
                                // scenario - the anomaly we expect to see results in deadlocked
                                // requests, not failing requests.
                                REQUESTS_COMPLETED.fetch_add(1, atomic::Ordering::Relaxed);
                                continue;
                            }
                            Err(e) => {
                                // This failure is not expected as part of the test scenario, neither for fail nor pass results.
                                panic!("Request failed unexpectedly: {:?}", e);
                            }
                        };

                        let response_body_stream = response.body_mut();
                        let mut total_length: usize = 0;

                        while let Some(Ok(chunk)) = response_body_stream.next().await {
                            total_length += chunk.len();
                        }

                        assert_eq!(total_length, 10 * 1024);

                        REQUESTS_COMPLETED.fetch_add(1, atomic::Ordering::Relaxed);
                    }
                }
            }));
        }

        let mut last_progress = Instant::now();
        let mut last_requests_completed = 0;

        while REQUESTS_COMPLETED.load(atomic::Ordering::Relaxed) < TARGET_REQUESTS_COMPLETED {
            let requests_completed = REQUESTS_COMPLETED.load(atomic::Ordering::Relaxed);

            println!("Requests completed: {requests_completed} of {TARGET_REQUESTS_COMPLETED}",);

            if requests_completed > last_requests_completed {
                last_progress = Instant::now();
            }

            last_requests_completed = requests_completed;

            if Instant::now().duration_since(last_progress) > MUST_MAKE_PROGRESS_INTERVAL {
                panic!(
                    "No requests completed in the last {:?}, deadlock likely occurred",
                    MUST_MAKE_PROGRESS_INTERVAL
                );
            }

            thread::sleep(CHECK_FOR_PROGRESS_INTERVAL);
        }

        println!("All requests completed successfully.");

        for handle in join_handles {
            handle.await.unwrap();
        }
    });
}

static REQUEST_BODY: &[u8] = &[77; 10 * 1024];
static RESPONSE_BODY: &[u8] = &[88; 10 * 1024];

/// Starts a test server that will open a TCP socket and listen for HTTP/2 connections.
///
/// For simplify, it will listen forever (until the test binary terminates).
///
/// Expects to read 10 KB of request body from each request to `/`,
/// to which it responds with a 200 OK with 10 KB of response body.
fn serve() -> SocketAddr {
    let runtime = Runtime::new().unwrap();

    let local_addr = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();

        let local_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                println!("Waiting for connection on {}", local_addr);

                let (socket, _) = listener.accept().await.unwrap();

                println!("Accepted connection from {}", socket.peer_addr().unwrap());

                // Fork each connection.
                tokio::spawn(async move {
                    let mut connection = server::Builder::new()
                        .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
                        .handshake(socket)
                        .await
                        .expect("Failed to handshake");

                    while let Some(incoming) = connection.next().await {
                        let Ok((mut request, mut responder)) = incoming else {
                            // This generally means connection terminated.
                            break;
                        };

                        // Fork each request.
                        tokio::spawn(async move {
                            let request_body_stream = request.body_mut();
                            let mut total_length: usize = 0;

                            while let Some(Ok(chunk)) = request_body_stream.next().await {
                                total_length += chunk.len();
                            }

                            assert_eq!(total_length, 10 * 1024);

                            let response =
                                Response::builder().status(StatusCode::OK).body(()).unwrap();
                            let mut body_sender = responder.send_response(response, false).unwrap();
                            body_sender
                                .send_data(Bytes::from_static(RESPONSE_BODY), true)
                                .unwrap();
                        });
                    }
                });
            }
        });

        local_addr
    });

    // Keep it running until end of process.
    mem::forget(runtime);

    local_addr
}
