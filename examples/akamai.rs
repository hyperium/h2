fn main() {
    // Enable the below code once tokio_rustls moves to std::future
}

/*
#![feature(async_await)]

use h2::client;

use futures::*;
use http::{Method, Request};

use tokio::net::TcpStream;

use rustls::Session;
use tokio_rustls::ClientConfigExt;
use webpki::DNSNameRef;

use std::net::ToSocketAddrs;
use std::error::Error;

const ALPN_H2: &str = "h2";

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
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

    let tcp = TcpStream::connect(&addr).await?;
    let dns_name = DNSNameRef::try_from_ascii_str("http2.akamai.com").unwrap();
    let res = tls_client_config.connect_async(dns_name, tcp).await;
    let tls = res.unwrap();
    {
        let (_, session) = tls.get_ref();
        let negotiated_protocol = session.get_alpn_protocol();
        assert_eq!(Some(ALPN_H2), negotiated_protocol.as_ref().map(|x| &**x));
    }

    println!("Starting client handshake");
    let (mut client, h2) = client::handshake(tls).await?;

    let request = Request::builder()
                    .method(Method::GET)
                    .uri("https://http2.akamai.com/")
                    .body(())
                    .unwrap();

    let (response, _) = client.send_request(request, true).unwrap();
    let (_, mut body) = response.await?.into_parts();
    while let Some(chunk) = body.next().await {
        println!("RX: {:?}", chunk?);
    }
    Ok(())
}
*/
