#[macro_export]
macro_rules! assert_closed {
    ($transport:expr) => {{
        assert_eq!($transport.poll().unwrap(), None.into());
    }}
}

#[macro_export]
macro_rules! poll_data {
    ($transport:expr) => {{
        use h2::frame::Frame;
        use futures::Async;

        match $transport.poll() {
            Ok(Async::Ready(Some(Frame::Data(frame)))) => frame,
            frame => panic!("expected data frame; actual={:?}", frame),
        }
    }}
}

#[macro_export]
macro_rules! raw_codec {
    (
        $(
            $fn:ident => [$($chunk:expr,)+];
        )*
    ) => {{
        let mut b = $crate::mock_io::Builder::new();

        $({
            let mut chunk = vec![];

            $(
                $crate::codec::Chunk::push(&$chunk, &mut chunk);
            )+

            b.$fn(&chunk[..]);
        })*

        $crate::Codec::new(b.build())
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
