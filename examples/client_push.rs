
use h2::client;
use http::{HeaderMap, Request};

use std::error::Error;

use tokio::net::TcpStream;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
    let _ = env_logger::try_init();

    let tcp = TcpStream::connect("127.0.0.1:5928").await?;
    let (mut client, h2) = client::handshake(tcp).await?;

    println!("sending request");

    let request = Request::builder()
        .uri("https://http2.akamai.com/")
        .body(())
        .unwrap();

    let request1 = Request::builder()
        .uri("https://http2.akamai.com/pushed")
        .body(())
        .unwrap();

    let request2 = Request::builder()
        .uri("https://http2.akamai.com/pushed2")
        .body(())
        .unwrap();

    let mut trailers = HeaderMap::new();
    trailers.insert("zomg", "hello".parse().unwrap());

    let (response, mut stream) = client.send_request(request, false).unwrap();
    let (response1, mut stream1) = client.send_request(request1, false).unwrap();
    let (response2, mut stream2) = client.send_request(request2, false).unwrap();

    // send trailers
    stream.send_trailers(trailers).unwrap();

    // Spawn a task to run the conn...
    tokio::spawn(async move {
        if let Err(e) = h2.await {
            println!("GOT ERR={:?}", e);
        }
    });

    let response = response.await?;
    println!("GOT RESPONSE: {:?}", response);

    let response1 = response1.await?;
    println!("GOT RESPONSE1: {:?}", response1);

    let response2 = response2.await?;
    println!("GOT RESPONSE2: {:?}", response1);

    // Get the body
    let mut body = response.into_body();
    let mut body1 = response1.into_body();
    let mut body2 = response2.into_body();

    while let Some(chunk) = body.data().await {
        println!("GOT CHUNK = {:?}", chunk?);
    }

    while let Some(chunk1) = body1.data().await {
        println!("GOT CHUNK1 = {:?}", chunk1?);
    }

    while let Some(chunk2) = body2.data().await {
        println!("GOT CHUNK2 = {:?}", chunk2?);
    }

    if let Some(trailers) = body.trailers().await? {
        println!("GOT TRAILERS: {:?}", trailers);
    }

    Ok(())
}
