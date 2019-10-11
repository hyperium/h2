use h2::server;

use bytes::*;
use http::uri;
use http::{Request, Response, StatusCode};

use std::error::Error;
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
    let _ = env_logger::try_init();

    let mut listener = TcpListener::bind("127.0.0.1:5928").await?;

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
        println!("request uri: {}", request.uri());
        println!("GOT request: {:?}", request);
        let response = Response::builder().status(StatusCode::OK).body(()).unwrap();

        let mut pushed_uri_parts: uri::Parts  = request.into_parts().0.uri.into();
        pushed_uri_parts.path_and_query = uri::PathAndQuery::from_static("/pushed").into();
        let uri1 = uri::Uri::from_parts(pushed_uri_parts).unwrap();
        println!("uri1 {}", uri1);

        //let uri2 = uri::Uri::from_static("http://127.0.0.1:5928/pushed2");
        let uri2 = uri::Uri::from_static("https://http2.akamai.com/pushed2");

        println!("uri2 {}", uri2);

        let pushed_req = Request::builder()
            .uri(uri1)
            .body(())
            .unwrap();

        let pushed_req2 = Request::builder()
            .uri(uri2)
            .body(())
            .unwrap();

        let pushed_rsp = http::Response::builder().status(200).body(()).unwrap();
        let pushed_rsp2 = http::Response::builder().status(200).body(()).unwrap();
        let mut send_pushed = respond
            .push_request(pushed_req)
            .unwrap()
            .send_response(pushed_rsp, false)
            .unwrap();

        let mut send_pushed2 = respond
            .push_request(pushed_req2)
            .unwrap()
            .send_response(pushed_rsp2, false)
            .unwrap();

        let mut send = respond.send_response(response, false)?;

        println!(">>>> pushing data");
        send_pushed.send_data(Bytes::from_static(b"Pushed data!\n"), true)?;
        send_pushed2.send_data(Bytes::from_static(b"Another Pushed data!\n"), true)?;
        println!(">>>> sending data");
        send.send_data(Bytes::from_static(b"hello world"), false)?;
    }

    println!("~~~~~~~~~~~~~~~~~~~~~~~~~~~ H2 connection CLOSE !!!!!! ~~~~~~~~~~~");

    Ok(())
}
