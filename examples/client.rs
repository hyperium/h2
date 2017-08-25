extern crate h2;
extern crate http;
extern crate bytes;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate io_dump;
extern crate env_logger;

use h2::*;
use h2::client::Client;

use http::*;
use futures::*;
use bytes::*;

use tokio_core::reactor;
use tokio_core::net::TcpStream;

struct Process {
    body: Body<Bytes>,
    trailers: bool,
}

impl Future for Process {
    type Item = ();
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<(), ConnectionError> {
        loop {
            if self.trailers {
                let trailers = try_ready!(self.body.poll_trailers());

                println!("GOT TRAILERS: {:?}", trailers);

                return Ok(().into());
            } else {
                match try_ready!(self.body.poll()) {
                    Some(chunk) => {
                        println!("GOT CHUNK = {:?}", chunk);
                    }
                    None => {
                        self.trailers = true;
                    }
                }
            }
        }
    }
}

pub fn main() {
    let _ = env_logger::init();

    let mut core = reactor::Core::new().unwrap();;
    let handle = core.handle();

    let tcp = TcpStream::connect(
        &"127.0.0.1:5928".parse().unwrap(),
        &handle);

    let tcp = tcp.then(|res| {
        let tcp = io_dump::Dump::to_stdout(res.unwrap());
        Client::handshake(tcp)
    })
    .then(|res| {
        let mut client = res.unwrap();

        println!("sending request");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(()).unwrap();

        let mut trailers = h2::HeaderMap::new();
        trailers.insert("zomg", "hello".parse().unwrap());

        let mut stream = client.request(request, false).unwrap();

        // send trailers
        stream.send_trailers(trailers).unwrap();

        // Spawn a task to run the client...
        handle.spawn(client.map_err(|e| println!("GOT ERR={:?}", e)));

        stream.and_then(|response| {
            println!("GOT RESPONSE: {:?}", response);

            // Get the body
            let (_, body) = response.into_parts();

            Process {
                body,
                trailers: false,
            }
        }).map_err(|e| {
            println!("GOT ERR={:?}", e);
        })
    })
    ;

    core.run(tcp).unwrap();
}
