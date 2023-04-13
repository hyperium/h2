use bytes::Buf;
use h2::client::{ResponseFuture, SendRequest};
use http::Request;

/// Extend the `h2::client::SendRequest` type with convenience methods.
pub trait SendRequestExt {
    /// Convenience method to send a GET request and ignore the SendStream
    /// (since GETs don't need to send a body).
    fn get(&mut self, uri: &str) -> ResponseFuture;
}

impl<B> SendRequestExt for SendRequest<B>
where
    B: Buf,
{
    fn get(&mut self, uri: &str) -> ResponseFuture {
        let req = Request::builder()
            // method is GET by default
            .uri(uri)
            .body(())
            .expect("valid uri");

        let (fut, _tx) = self
            .send_request(req, /*eos =*/ true)
            .expect("send_request");

        fut
    }
}
