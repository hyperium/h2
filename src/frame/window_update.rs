use frame::{Head, Error};
use super::{StreamId};

#[derive(Debug)]
pub struct WindowUpdate {
    stream_id: StreamId,
    size_increment: u32,
}

const SIZE_INCREMENT_MASK: u32 = 1 << 31;

impl WindowUpdate {
    pub fn load(head: Head, payload: &[u8]) -> Result<WindowUpdate, Error> {
        if payload.len() != 4 {
            // Invalid payload len
            // TODO: Handle error
            unimplemented!();
        }

        let size_increment = unpack_octets_4!(payload, 0, u32)
            & !SIZE_INCREMENT_MASK;

        Ok(WindowUpdate {
            stream_id: head.stream_id(),
            size_increment: size_increment,
        })
    }
}
