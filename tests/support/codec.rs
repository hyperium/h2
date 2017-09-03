macro_rules! raw_codec {
    (
        $(
            $fn:ident => [$($chunk:expr,)+];
        )*
    ) => {{
        let mut b = ::support::mock_io::Builder::new();

        $({
            let mut chunk = vec![];

            $(
                ::support::codec::Chunk::push(&$chunk, &mut chunk);
            )+

            b.$fn(&chunk[..]);
        })*

        ::support::Codec::new(b.build())
    }}
}

pub trait Chunk {
    fn push(&self, dst: &mut Vec<u8>);
}

impl Chunk for u8 {
    fn push(&self, dst: &mut Vec<u8>) {
        dst.push(*self);
    }
}

impl<'a> Chunk for &'a [u8] {
    fn push(&self, dst: &mut Vec<u8>) {
        dst.extend(*self)
    }
}

impl<'a> Chunk for &'a str {
    fn push(&self, dst: &mut Vec<u8>) {
        dst.extend(self.as_bytes())
    }
}
