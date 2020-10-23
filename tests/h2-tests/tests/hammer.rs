use futures::{FutureExt, StreamExt, TryFutureExt};
use h2_support::prelude::*;

use std::io;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
};
use tokio::net::{TcpListener, TcpStream};

struct Server {
    addr: SocketAddr,
    reqs: Arc<AtomicUsize>,
    _join: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn serve<F>(mk_data: F) -> Self
    where
        F: Fn() -> Bytes,
        F: Send + Sync + 'static,
    {
        let mk_data = Arc::new(mk_data);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt
            .block_on(TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let reqs = Arc::new(AtomicUsize::new(0));
        let reqs2 = reqs.clone();
        let join = thread::spawn(move || {
            let server = async move {
                loop {
                    let socket = listener.accept().await.map(|(s, _)| s);
                    let reqs = reqs2.clone();
                    let mk_data = mk_data.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_request(socket, reqs, mk_data).await {
                            eprintln!("serve conn error: {:?}", e)
                        }
                    });
                }
            };

            rt.block_on(server);
        });

        Self {
            addr,
            _join: Some(join),
            reqs,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr.clone()
    }

    fn request_count(&self) -> usize {
        self.reqs.load(Ordering::Acquire)
    }
}

async fn handle_request<F>(
    socket: io::Result<TcpStream>,
    reqs: Arc<AtomicUsize>,
    mk_data: Arc<F>,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Bytes,
    F: Send + Sync + 'static,
{
    let mut conn = server::handshake(socket?).await?;
    while let Some(result) = conn.next().await {
        let (_, mut respond) = result?;
        reqs.fetch_add(1, Ordering::Release);
        let response = Response::builder().status(StatusCode::OK).body(()).unwrap();
        let mut send = respond.send_response(response, false)?;
        send.send_data(mk_data(), true)?;
    }
    Ok(())
}

#[test]
fn hammer_client_concurrency() {
    // This reproduces issue #326.
    const N: usize = 5000;

    let server = Server::serve(|| Bytes::from_static(b"hello world!"));

    let addr = server.addr();
    let rsps = Arc::new(AtomicUsize::new(0));

    for i in 0..N {
        print!("sending {}", i);
        let rsps = rsps.clone();
        let tcp = TcpStream::connect(&addr);
        let tcp = tcp
            .then(|res| {
                let tcp = res.unwrap();
                client::handshake(tcp)
            })
            .then(move |res| {
                let rsps = rsps;
                let (mut client, h2) = res.unwrap();
                let request = Request::builder()
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();

                let (response, mut stream) = client.send_request(request, false).unwrap();
                stream.send_trailers(HeaderMap::new()).unwrap();

                tokio::spawn(async move {
                    h2.await.unwrap();
                });

                response
                    .and_then(|response| {
                        let mut body = response.into_body();

                        async move {
                            while let Some(res) = body.data().await {
                                res?;
                            }
                            body.trailers().await?;
                            Ok(())
                        }
                    })
                    .map_err(|e| {
                        panic!("client error: {:?}", e);
                    })
                    .map(move |_| {
                        rsps.fetch_add(1, Ordering::Release);
                    })
            });

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(tcp);
        println!("...done");
    }

    println!("all done");

    assert_eq!(N, rsps.load(Ordering::Acquire));
    assert_eq!(N, server.request_count());
}
