use frame::{Head, Error};
use super::{head, StreamId};

#[derive(Debug)]
pub struct Reset {
    stream_id: StreamId,
    error_code: u32,
}

impl Reset {
    pub fn load(head: Head, payload: &[u8]) -> Result<Reset, Error> {
        if payload.len() != 4 {
            // Invalid payload len
            // TODO: Handle error
            unimplemented!();
        }

        let error_code = unpack_octets_4!(payload, 0, u32);

        Ok(Reset {
            stream_id: head.stream_id(),
            error_code: error_code,
        })
    }
}
