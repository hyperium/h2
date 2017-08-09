extern crate h2;
extern crate http;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate io_dump;
extern crate env_logger;

use h2::client::Client;

use http::*;
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
        Client::handshake(tcp)
    })
    .then(|res| {
        let mut client = res.unwrap();

        println!("sending request");

        let request = Request::builder()
            .uri("https://http2.akamai.com/")
            .body(()).unwrap();

        let stream = client.request(request, true).unwrap();
        client.join(stream.and_then(|response| {
            println!("GOT RESPONSE: {:?}", response);
            Ok(())
        }))
    })
    ;

    core.run(tcp).unwrap();
}
