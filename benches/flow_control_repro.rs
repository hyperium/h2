use std::cmp;
use std::env;
use std::error::Error;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use std::{hint, pin::Pin};

use bytes::Bytes;
use h2::RecvStream;
use h2::client::{self, SendRequest};
use h2::server::{self, SendResponse};
use http::{Request, Response};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime;
use tokio::sync::Barrier;

type BoxError = Box<dyn Error + Send + Sync>;
type BoxSampleFuture = Pin<Box<dyn Future<Output = Result<RequestSample, BoxError>> + Send>>;

#[derive(Clone, Debug)]
struct Config {
    requests: usize,
    connections: usize,
    streams_per_connection: usize,
    response_size: usize,
    request_body_size: usize,
    stream_window_size: u32,
    connection_window_size: u32,
    worker_threads: usize,
    idle_ms: u64,
    spin_ms: u64,
    nodelay: bool,
}

#[derive(Debug)]
struct RequestSample {
    latency: Duration,
    bytes: usize,
}

#[derive(Debug)]
struct ConnectionStats {
    completed: usize,
    bytes: usize,
    latencies: Vec<Duration>,
}

#[derive(Debug)]
struct AggregateStats {
    completed: usize,
    bytes: usize,
    latencies: Vec<Duration>,
}

fn main() -> Result<(), BoxError> {
    let cfg = Config::from_env();

    let runtime = runtime::Builder::new_multi_thread()
        .worker_threads(cfg.worker_threads)
        .enable_all()
        .build()?;

    runtime.block_on(async_main(cfg))
}

impl Config {
    fn from_env() -> Self {
        Self {
            requests: env_usize("H2_BENCH_REQUESTS", 300),
            connections: env_usize("H2_BENCH_CONNECTIONS", 1),
            streams_per_connection: env_usize("H2_BENCH_STREAMS", 8),
            response_size: env_usize("H2_BENCH_RESPONSE_SIZE", 1024 * 1024),
            request_body_size: env_usize("H2_BENCH_REQUEST_BODY_SIZE", 0),
            stream_window_size: env_u32(
                "H2_BENCH_STREAM_WINDOW_SIZE",
                env_u32("H2_BENCH_WINDOW_SIZE", 65_535),
            ),
            connection_window_size: env_u32(
                "H2_BENCH_CONNECTION_WINDOW_SIZE",
                env_u32("H2_BENCH_WINDOW_SIZE", 65_535),
            ),
            worker_threads: env_usize("H2_BENCH_WORKER_THREADS", 8),
            idle_ms: env_u64("H2_BENCH_IDLE_MS", 0),
            spin_ms: env_u64("H2_BENCH_SPIN_MS", 0),
            nodelay: env_bool("H2_BENCH_NODELAY", false),
        }
    }
}

async fn async_main(cfg: Config) -> Result<(), BoxError> {
    if cfg.spin_ms > 0 {
        spin_cpu(Duration::from_millis(cfg.spin_ms), cfg.worker_threads);
    }

    if cfg.idle_ms > 0 {
        std::thread::sleep(Duration::from_millis(cfg.idle_ms));
    }

    let response_body = Bytes::from(vec![b'x'; cfg.response_size]);
    let request_body = Bytes::from(vec![b'y'; cfg.request_body_size]);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let accept_body = response_body.clone();
    let stream_window_size = cfg.stream_window_size;
    let connection_window_size = cfg.connection_window_size;
    let nodelay = cfg.nodelay;
    let accept_task = tokio::spawn(async move {
        accept_loop(
            listener,
            accept_body,
            stream_window_size,
            connection_window_size,
            nodelay,
        )
        .await;
    });

    let barrier = Arc::new(Barrier::new(cfg.connections + 1));
    let mut client_tasks = Vec::with_capacity(cfg.connections);
    let requests_per_connection = distribute(cfg.requests, cfg.connections);

    for request_count in requests_per_connection {
        let socket = TcpStream::connect(addr).await?;
        socket.set_nodelay(cfg.nodelay)?;
        let mut builder = client::Builder::new();
        builder
            .initial_window_size(cfg.stream_window_size)
            .initial_connection_window_size(cfg.connection_window_size)
            .max_concurrent_streams(cfg.streams_per_connection as u32);
        let (send_request, connection) = builder.handshake(socket).await?;

        tokio::spawn(async move {
            if let Err(err) = connection.await {
                eprintln!("client_connection_error={err:?}");
            }
        });

        let client_barrier = barrier.clone();
        let request_body = request_body.clone();
        let max_streams = cfg.streams_per_connection;

        client_tasks.push(tokio::spawn(async move {
            client_barrier.wait().await;
            run_client_connection(send_request, request_count, max_streams, request_body).await
        }));
    }

    barrier.wait().await;
    let started = Instant::now();

    let mut aggregate = AggregateStats {
        completed: 0,
        bytes: 0,
        latencies: Vec::with_capacity(cfg.requests),
    };

    for task in client_tasks {
        let stats = task.await??;
        aggregate.completed += stats.completed;
        aggregate.bytes += stats.bytes;
        aggregate.latencies.extend(stats.latencies);
    }

    let elapsed = started.elapsed();
    accept_task.abort();

    print_report(&cfg, elapsed, &mut aggregate);

    Ok(())
}

async fn accept_loop(
    listener: TcpListener,
    response_body: Bytes,
    stream_window_size: u32,
    connection_window_size: u32,
    nodelay: bool,
) {
    loop {
        match listener.accept().await {
            Ok((socket, _)) => {
                if let Err(err) = socket.set_nodelay(nodelay) {
                    eprintln!("set_nodelay_error={err:?}");
                    continue;
                }

                let response_body = response_body.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_connection(
                        socket,
                        response_body,
                        stream_window_size,
                        connection_window_size,
                    )
                    .await
                    {
                        eprintln!("server_connection_error={err:?}");
                    }
                });
            }
            Err(err) => {
                eprintln!("accept_error={err:?}");
                break;
            }
        }
    }
}

async fn serve_connection(
    socket: TcpStream,
    response_body: Bytes,
    stream_window_size: u32,
    connection_window_size: u32,
) -> Result<(), BoxError> {
    let mut builder = server::Builder::new();
    builder
        .initial_window_size(stream_window_size)
        .initial_connection_window_size(connection_window_size);
    let mut connection = builder.handshake(socket).await?;

    while let Some(result) = connection.accept().await {
        let (request, respond) = result?;
        let response_body = response_body.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_request(request, respond, response_body).await {
                eprintln!("request_error={err:?}");
            }
        });
    }

    Ok(())
}

async fn handle_request(
    mut request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    response_body: Bytes,
) -> Result<(), BoxError> {
    let body = request.body_mut();
    while let Some(data) = body.data().await {
        let data = data?;
        body.flow_control().release_capacity(data.len())?;
    }

    let response = Response::builder().status(200).body(())?;

    if response_body.is_empty() {
        respond.send_response(response, true)?;
    } else {
        let mut send = respond.send_response(response, false)?;
        send.send_data(response_body, true)?;
    }

    Ok(())
}

async fn run_client_connection(
    mut send_request: SendRequest<Bytes>,
    request_count: usize,
    max_streams: usize,
    request_body: Bytes,
) -> Result<ConnectionStats, BoxError> {
    let max_streams = cmp::max(1, max_streams);
    let mut sent = 0;
    let mut completed = 0;
    let mut bytes = 0;
    let mut latencies = Vec::with_capacity(request_count);
    let mut in_flight = Vec::with_capacity(max_streams);

    while completed < request_count {
        while sent < request_count && in_flight.len() < max_streams {
            send_request = send_request.ready().await?;
            let request = Request::builder()
                .method("GET")
                .uri("https://localhost/bench")
                .body(())?;
            let started = Instant::now();

            let response = if request_body.is_empty() {
                let (response, _) = send_request.send_request(request, true)?;
                response
            } else {
                let (response, mut body) = send_request.send_request(request, false)?;
                body.send_data(request_body.clone(), true)?;
                response
            };

            in_flight.push(Box::pin(receive_response(response, started)) as BoxSampleFuture);
            sent += 1;
        }

        let sample = next_response(&mut in_flight).await?;
        completed += 1;
        bytes += sample.bytes;
        latencies.push(sample.latency);
    }

    Ok(ConnectionStats {
        completed,
        bytes,
        latencies,
    })
}

async fn receive_response(
    response: h2::client::ResponseFuture,
    started: Instant,
) -> Result<RequestSample, BoxError> {
    let response = response.await?;
    if !response.status().is_success() {
        return Err(format!("unexpected status {}", response.status()).into());
    }

    let mut bytes = 0;
    let mut body = response.into_body();

    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        bytes += chunk.len();
        body.flow_control().release_capacity(chunk.len())?;
    }

    Ok(RequestSample {
        latency: started.elapsed(),
        bytes,
    })
}

async fn next_response(in_flight: &mut Vec<BoxSampleFuture>) -> Result<RequestSample, BoxError> {
    poll_fn(|cx| {
        for idx in 0..in_flight.len() {
            if let Poll::Ready(result) = in_flight[idx].as_mut().poll(cx) {
                drop(in_flight.swap_remove(idx));
                return Poll::Ready(result);
            }
        }

        Poll::Pending
    })
    .await
}

fn distribute(total: usize, buckets: usize) -> Vec<usize> {
    let buckets = cmp::max(1, buckets);
    let base = total / buckets;
    let remainder = total % buckets;

    (0..buckets)
        .map(|idx| base + usize::from(idx < remainder))
        .collect()
}

fn print_report(cfg: &Config, elapsed: Duration, aggregate: &mut AggregateStats) {
    aggregate.latencies.sort_unstable();

    let elapsed_secs = elapsed.as_secs_f64();
    let requests_per_second = if elapsed_secs == 0.0 {
        0.0
    } else {
        aggregate.completed as f64 / elapsed_secs
    };

    println!("requests={}", cfg.requests);
    println!("completed={}", aggregate.completed);
    println!("connections={}", cfg.connections);
    println!("streams_per_connection={}", cfg.streams_per_connection);
    println!("response_size={}", cfg.response_size);
    println!("request_body_size={}", cfg.request_body_size);
    println!("stream_window_size={}", cfg.stream_window_size);
    println!("connection_window_size={}", cfg.connection_window_size);
    println!("worker_threads={}", cfg.worker_threads);
    println!("idle_ms={}", cfg.idle_ms);
    println!("spin_ms={}", cfg.spin_ms);
    println!("nodelay={}", cfg.nodelay);
    println!("elapsed_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    println!("requests_per_second={:.3}", requests_per_second);
    println!("bytes={}", aggregate.bytes);
    println!(
        "latency_p50_us={:.3}",
        percentile_us(&aggregate.latencies, 0.50)
    );
    println!(
        "latency_p95_us={:.3}",
        percentile_us(&aggregate.latencies, 0.95)
    );
    println!(
        "latency_p99_us={:.3}",
        percentile_us(&aggregate.latencies, 0.99)
    );
}

fn percentile_us(values: &[Duration], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let idx = ((values.len() - 1) as f64 * percentile).ceil() as usize;
    values[idx].as_secs_f64() * 1_000_000.0
}

fn spin_cpu(duration: Duration, threads: usize) {
    let deadline = Instant::now() + duration;
    let mut handles = Vec::with_capacity(threads);

    for _ in 0..threads {
        handles.push(std::thread::spawn(move || {
            while Instant::now() < deadline {
                hint::spin_loop();
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
