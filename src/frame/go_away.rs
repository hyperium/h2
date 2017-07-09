use frame::{Error, StreamId};

#[derive(Debug)]
pub struct GoAway {
    last_stream_id: StreamId,
    error_code: u32,
}

impl GoAway {
    pub fn load(payload: &[u8]) -> Result<GoAway, Error> {
        if payload.len() < 8 {
            // Invalid payload len
            // TODO: Handle error
            unimplemented!();
        }

        let last_stream_id = StreamId::parse(&payload[..4]);
        let error_code = unpack_octets_4!(payload, 4, u32);

        Ok(GoAway {
            last_stream_id: last_stream_id,
            error_code: error_code,
        })
    }
}
