#![no_main]
use h2_support::prelude::*;
use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};

#[derive(Debug, Arbitrary)]
struct HttpSpec {
    uri: Vec<u8>,
    header_name: Vec<u8>,
    header_value: Vec<u8>,
}

async fn fuzz_entry(inp: HttpSpec) {
    if let Ok(req) = Request::builder()
        .uri(&inp.uri[..])
        .header(&inp.header_name[..], &inp.header_value[..])
        .body(())
    {
        let (io, mut _srv) = mock::new();
        let (mut client, _h2) = client::Builder::new()
            .handshake::<_, Bytes>(io)
            .await
            .unwrap();

        let (_, _) = client.send_request(req, true).unwrap();
    }
}

fuzz_target!(|inp: HttpSpec| {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(fuzz_entry(inp));
});
