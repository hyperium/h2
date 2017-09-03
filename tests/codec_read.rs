#[macro_use]
pub mod support;
use support::*;

#[test]
fn read_none() {
    let mut codec = Codec::from(
        mock_io::Builder::new()
        .build());

    assert_closed!(codec);
}

#[test]
fn read_data_no_pad() {
    let mut codec = codec(&[
        0, 0, 5, 0, 0, 0, 0, 0, 1,
        b'h', b'e', b'l', b'l', b'o',
    ]);

    let data = poll_data!(codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);

    assert_closed!(codec);
}

fn codec(src: &[u8]) -> Codec<mock_io::Mock> {
    Codec::new(
        mock_io::Builder::new()
        .read(src)
        .build())
}
