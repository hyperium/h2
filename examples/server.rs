#![feature(async_await)]

use h2::server;

use bytes::*;
use futures::*;
use http::{Response, StatusCode};

use tokio::net::{TcpListener, TcpStream};
use std::error::Error;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
    let _ = env_logger::try_init();

    let listener = TcpListener::bind(&"127.0.0.1:5928".parse().unwrap()).unwrap();

    println!("listening on {:?}", listener.local_addr());
    let mut incoming = listener.incoming();

    while let Some(socket) = incoming.next().await {
        tokio::spawn(async move {
            if let Err(e) = handle(socket).await {
                println!("  -> err={:?}", e); 
            }
        });
    }

    Ok(())
}

async fn handle(socket: io::Result<TcpStream>) -> Result<(), Box<dyn Error>> {
    let mut connection = server::handshake(socket?).await?;
    println!("H2 connection bound");

    while let Some(result) = connection.next().await {
        let (request, mut respond) = result?;
        println!("GOT request: {:?}", request);
        let response = Response::builder().status(StatusCode::OK).body(()).unwrap();

        let mut send = respond.send_response(response, false)?;

        println!(">>>> sending data");
        send.send_data(Bytes::from_static(b"hello world"), true)?;
    }

    println!("~~~~~~~~~~~~~~~~~~~~~~~~~~~ H2 connection CLOSE !!!!!! ~~~~~~~~~~~");
    
    Ok(())
}