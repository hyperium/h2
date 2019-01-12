extern crate tokio;
#[macro_use]
extern crate h2_support;

use h2_support::prelude::*;
use h2_support::futures::{Async, Poll};

use tokio::net::{TcpListener, TcpStream};
use std::{net::SocketAddr, thread, sync::{atomic::{AtomicUsize, Ordering}, Arc}};

struct Server {
    addr: SocketAddr,
    reqs: Arc<AtomicUsize>,
    join: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn serve<F>(mk_data: F) -> Self
    where
        F: Fn() -> Bytes,
        F: Send + Sync + 'static,
    {
        let mk_data = Arc::new(mk_data);

        let listener = TcpListener::bind(&SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        let reqs = Arc::new(AtomicUsize::new(0));
        let reqs2 = reqs.clone();
        let join = thread::spawn(move || {
            let server = listener.incoming().for_each(move |socket| {
                let reqs = reqs2.clone();
                let mk_data = mk_data.clone();
                let connection = server::handshake(socket)
                    .and_then(move |conn| {
                        conn.for_each(move |(_, mut respond)| {
                            reqs.fetch_add(1, Ordering::Release);
                            let response = Response::builder().status(StatusCode::OK).body(()).unwrap();
                            let mut send = respond.send_response(response, false)?;
                            send.send_data(mk_data(), true).map(|_|())
                        })
                    })
                    .map_err(|e| eprintln!("serve conn error: {:?}", e));

                tokio::spawn(Box::new(connection));
                Ok(())
            })
            .map_err(|e| eprintln!("serve error: {:?}", e));

            tokio::run(server);
        });

        Self {
            addr,
            join: Some(join),
            reqs
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr.clone()
    }

    fn request_count(&self) -> usize {
        self.reqs.load(Ordering::Acquire)
    }
}


struct Process {
    body: RecvStream,
    trailers: bool,
}

impl Future for Process {
    type Item = ();
    type Error = h2::Error;

    fn poll(&mut self) -> Poll<(), h2::Error> {
        loop {
            if self.trailers {
                return match self.body.poll_trailers()? {
                    Async::NotReady => Ok(Async::NotReady),
                    Async::Ready(_) => Ok(().into()),
                };
            } else {
                match self.body.poll()? {
                    Async::NotReady => return Ok(Async::NotReady),
                    Async::Ready(None) => {
                        self.trailers = true;
                    },
                    _ => {},
                }
            }
        }
    }
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
        let tcp = tcp.then(|res| {
            let tcp = res.unwrap();
            client::handshake(tcp)
        }).then(move |res| {
                let rsps = rsps;
                let (mut client, h2) = res.unwrap();
                let request = Request::builder()
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();

                let (response, mut stream) = client.send_request(request, false).unwrap();
                stream.send_trailers(HeaderMap::new()).unwrap();

                tokio::spawn(h2.map_err(|e| panic!("client conn error: {:?}", e)));

                response
                    .and_then(|response| {
                        let (_, body) = response.into_parts();

                        Process {
                            body,
                            trailers: false,
                        }
                    })
                    .map_err(|e| {
                        panic!("client error: {:?}", e);
                    })
                    .map(move |_| {
                        rsps.fetch_add(1, Ordering::Release);
                    })
            });

        tokio::run(tcp);
        println!("...done");
    }

    println!("all done");

    assert_eq!(N, rsps.load(Ordering::Acquire));
    assert_eq!(N, server.request_count());
}
