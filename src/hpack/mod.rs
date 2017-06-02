mod encoder;
mod decoder;
mod entry;
mod huffman;
mod table;

#[cfg(test)]
mod test;

pub use self::encoder::Encoder;
pub use self::entry::{Entry, Key};
pub use self::decoder::{Decoder, DecoderError};
