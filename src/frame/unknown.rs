use frame::Head;
use bytes::Bytes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    head: Head,
    payload: Bytes,
}

impl Unknown {
    pub fn new(head: Head, payload: Bytes) -> Unknown {
        Unknown {
            head: head,
            payload: payload,
        }
    }
}
