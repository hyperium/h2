use std::net::SocketAddr;

#[test]
fn logical_deadlock() {

}

/// Opens a TCP socket and listens for HTTP2 connections.
/// 
/// Expects to read 10 KB of request body from each request,
/// to which it responds with a 200 OK with 10 KB of response body.
struct Server {
    local_addr: SocketAddr,
}

impl Server {
    /// Starts a new server. Drop it to signal the server to stop and to wait for it to stop.
    fn start() -> Self {
        
    }

    /// The address the server is listening on. Clients should send their traffic here.
    fn addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        todo!()
    }
}