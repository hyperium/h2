extern crate h2;
extern crate http;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
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
        &"127.0.0.1:5928".parse().unwrap(),
        &core.handle());

    let tcp = tcp.then(|res| {
        let tcp = io_dump::Dump::to_stdout(res.unwrap());
        client::handshake(tcp)
    })
    .then(|res| {
        let conn = res.unwrap();

        println!("sending request");

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
    ;

    core.run(tcp).unwrap();
}
