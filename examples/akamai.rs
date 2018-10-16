extern crate env_logger;
extern crate futures;
extern crate h2;
extern crate http;
extern crate rustls;
extern crate tokio;
extern crate tokio_rustls;
extern crate webpki;
extern crate webpki_roots;

use h2::client;

use futures::*;
use http::{Method, Request};

use tokio::net::TcpStream;

use rustls::Session;
use tokio_rustls::ClientConfigExt;
use webpki::DNSNameRef;

use std::net::ToSocketAddrs;

const ALPN_H2: &str = "h2";

pub fn main() {
    let _ = env_logger::try_init();

    let tls_client_config = std::sync::Arc::new({
        let mut c = rustls::ClientConfig::new();
        c.root_store
            .add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
        c.alpn_protocols.push(ALPN_H2.to_owned());
        c
    });

    // Sync DNS resolution.
    let addr = "http2.akamai.com:443"
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap();

    println!("ADDR: {:?}", addr);

    let tcp = TcpStream::connect(&addr);
    let dns_name = DNSNameRef::try_from_ascii_str("http2.akamai.com").unwrap();

    let tcp = tcp.then(move |res| {
        let tcp = res.unwrap();
        tls_client_config
            .connect_async(dns_name, tcp)
            .then(|res| {
                let tls = res.unwrap();
                {
                    let (_, session) = tls.get_ref();
                    let negotiated_protocol = session.get_alpn_protocol();
                    assert_eq!(Some(ALPN_H2), negotiated_protocol.as_ref().map(|x| &**x));
                }

                println!("Starting client handshake");
                client::handshake(tls)
            })
            .then(|res| {
                let (mut client, h2) = res.unwrap();

                let request = Request::builder()
                    .method(Method::GET)
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();

                let (response, _) = client.send_request(request, true).unwrap();

                let stream = response.and_then(|response| {
                    let (_, body) = response.into_parts();

                    body.for_each(|chunk| {
                        println!("RX: {:?}", chunk);
                        Ok(())
                    })
                });

                h2.join(stream)
            })
    })
        .map_err(|e| eprintln!("ERROR: {:?}", e))
        .map(|((), ())| ());

    tokio::run(tcp);
}
