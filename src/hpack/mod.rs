mod encoder;
mod decoder;
pub(crate) mod header;
mod huffman;
mod table;

#[cfg(test)]
mod test;

pub use self::decoder::{Decoder, DecoderError, NeedMore};
pub use self::encoder::{Encode, EncodeState, Encoder, EncoderError};
pub use self::header::Header;
