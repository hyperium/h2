mod encoder;
mod decoder;
mod header;
mod huffman;
mod table;

#[cfg(test)]
mod test;

pub use self::encoder::{Encoder, Encode, EncoderError};
pub use self::header::Header;
pub use self::decoder::{Decoder, DecoderError};
pub use self::table::Index;
