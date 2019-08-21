use h2::server;

use bytes::*;
use http::{Response, StatusCode};

use std::error::Error;
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
    let _ = env_logger::try_init();

    let mut listener = TcpListener::bind(&"127.0.0.1:5928".parse()?)?;

    println!("listening on {:?}", listener.local_addr());

    loop {
        if let Ok((socket, _peer_addr)) = listener.accept().await {
            tokio::spawn(async move {
                if let Err(e) = handle(socket).await {
                    println!("  -> err={:?}", e);
                }
            });
        }
    }
}

async fn handle(socket: TcpStream) -> Result<(), Box<dyn Error>> {
    let mut connection = server::handshake(socket).await?;
    println!("H2 connection bound");

    while let Some(result) = connection.accept().await {
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
