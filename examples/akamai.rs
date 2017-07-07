extern crate h2;
extern crate http;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate tokio_openssl;
extern crate openssl;
extern crate io_dump;
extern crate env_logger;

use h2::client;

use http::request;

use futures::*;

use tokio_core::reactor;
use tokio_core::net::TcpStream;

pub fn main() {
    let _ = env_logger::init();

    let mut core = reactor::Core::new().unwrap();;

    let tcp = TcpStream::connect(
        &"23.39.23.98:443".parse().unwrap(),
        &core.handle());

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

                client::handshake(tls)
            })
            .then(|res| {
                let conn = res.unwrap();

                let mut request = request::Head::default();
                request.uri = "https://http2.akamai.com/".parse().unwrap();
                // request.version = version::H2;

                conn.send_request(1.into(), request, true)
            })
            .then(|res| {
                let conn = res.unwrap();
                // Get the next message
                conn.for_each(|frame| {
                    println!("RX: {:?}", frame);
                    Ok(())
                })
            })
    });

    core.run(tcp).unwrap();
}
