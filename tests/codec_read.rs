#[macro_use]
extern crate h2_test_support;
use h2_test_support::*;

#[test]
fn read_none() {
    let mut codec = Codec::from(
        mock_io::Builder::new()
        .build());

    assert_closed!(codec);
}

#[test]
fn read_data_no_padding() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 1,
            "hello",
        ];
    };

    let data = poll_data!(codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[test]
fn read_data_padding() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 11, 0, 0x8, 0, 0, 0, 1,
            5,       // Pad length
            "hello", // Data
            "world", // Padding
        ];
    };

    let data = poll_data!(codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[test]
fn read_data_stream_id_zero() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 0,
            "hello", // Data
        ];
    };

    poll_err!(codec);
}
