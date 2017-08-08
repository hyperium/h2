extern crate h2;
extern crate http;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate tokio_openssl;
extern crate openssl;
extern crate io_dump;
extern crate env_logger;

use h2::client::Client;

use http::{method, Request};
use futures::*;

use tokio_core::reactor;
use tokio_core::net::TcpStream;

use std::net::ToSocketAddrs;

pub fn main() {
    let _ = env_logger::init();

    // Sync DNS resolution.
    let addr = "http2.akamai.com:443".to_socket_addrs()
        .unwrap().next().unwrap();

    println!("ADDR: {:?}", addr);

    let mut core = reactor::Core::new().unwrap();;
    let handle = core.handle();

    let tcp = TcpStream::connect(&addr, &handle);

    let tcp = tcp.then(|res| {
        use openssl::ssl::{SslMethod, SslConnectorBuilder};
        use tokio_openssl::SslConnectorExt;

        let tcp = res.unwrap();

        // Does anyone know how TLS even works?

        let mut b = SslConnectorBuilder::new(SslMethod::tls()).unwrap();
        b.builder_mut().set_alpn_protocols(&[b"h2"]).unwrap();

        let connector = b.build();
        connector.connect_async("http2.akamai.com", tcp)
            .then(|res| {
                let tls = res.unwrap();
                assert_eq!(Some(&b"h2"[..]), tls.get_ref().ssl().selected_alpn_protocol());

                // Dump output to stdout
                let tls = io_dump::Dump::to_stdout(tls);

                println!("Starting client handshake");
                Client::handshake(tls)
            })
            .then(|res| {
                let mut h2 = res.unwrap();

                let request = Request::builder()
                    .method(method::GET)
                    .uri("https://http2.akamai.com/")
                    .body(()).unwrap();

                let stream = h2.request(request, true).unwrap();

                let stream = stream.and_then(|response| {
                    let (_, body) = response.into_parts();

                    body.for_each(|chunk| {
                        println!("RX: {:?}", chunk);
                        Ok(())
                    })
                });

                h2.join(stream)
            })
    });

    core.run(tcp).unwrap();
}
