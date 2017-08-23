extern crate h2;
extern crate http;
extern crate bytes;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate io_dump;
extern crate env_logger;

use h2::server::Server;

use http::*;
use bytes::*;
use futures::*;

use tokio_core::reactor;
use tokio_core::net::TcpListener;

pub fn main() {
    let _ = env_logger::init();

    let mut core = reactor::Core::new().unwrap();;
    let handle = core.handle();

    let listener = TcpListener::bind(
        &"127.0.0.1:5928".parse().unwrap(),
        &handle).unwrap();

    println!("listening on {:?}", listener.local_addr());

    let server = listener.incoming().for_each(move |(socket, _)| {
        // let socket = io_dump::Dump::to_stdout(socket);


        let connection = Server::handshake(socket)
            .then(|res| {
                let conn = res.unwrap();

                println!("H2 connection bound");

                conn.for_each(|(request, mut stream)| {
                    println!("GOT request: {:?}", request);

                    let response = Response::builder()
                        .status(status::OK)
                        .body(()).unwrap();

                    if let Err(e) = stream.send_response(response, true) {
                        println!(" error responding; err={:?}", e);
                    }

                    println!(">>>> sending data");
                    stream.send_data(Bytes::from_static(b"hello world"), true).unwrap();

                    Ok(())
                }).and_then(|_| {
                    println!("~~~~~~~~~~~~~~~~~~~~~~~~~~~ H2 connection CLOSE !!!!!! ~~~~~~~~~~~");
                    Ok(())
                })
            })
            .then(|res| {
                let _ = res.unwrap();
                Ok(())
            })
            ;

        handle.spawn(connection);
        Ok(())
    });

    core.run(server).unwrap();
}
