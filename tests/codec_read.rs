#[macro_use]
extern crate h2_test_support;
use h2_test_support::prelude::*;

use std::error::Error;

#[test]
fn read_none() {
    let mut codec = Codec::from(mock_io::Builder::new().build());

    assert_closed!(codec);
}

#[test]
fn read_frame_too_big() {}

// ===== DATA =====

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
fn read_data_empty_payload() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
    };

    let data = poll_data!(codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b""[..]);
    assert!(!data.is_end_stream());

    assert_closed!(codec);
}

#[test]
fn read_data_end_stream() {
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 1, 0, 0, 0, 1,
            "hello",
        ];
    };

    let data = poll_data!(codec);
    assert_eq!(data.stream_id(), 1);
    assert_eq!(data.payload(), &b"hello"[..]);
    assert!(data.is_end_stream());

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

// ===== HEADERS =====

#[test]
fn read_headers_without_pseudo() {}

#[test]
fn read_headers_with_pseudo() {}

#[test]
fn read_headers_empty_payload() {}

#[test]
fn update_max_frame_len_at_rest() {
    let _ = ::env_logger::init();
    // TODO: add test for updating max frame length in flight as well?
    let mut codec = raw_codec! {
        read => [
            0, 0, 5, 0, 0, 0, 0, 0, 1,
            "hello",
            0, 64, 1, 0, 0, 0, 0, 0, 1,
            vec![0; 16_385],
        ];
    };

    assert_eq!(poll_data!(codec).payload(), &b"hello"[..]);

    codec.set_max_recv_frame_size(16_384);

    assert_eq!(codec.max_recv_frame_size(), 16_384);
    assert_eq!(
        codec.poll().unwrap_err().description(),
        "frame with invalid size"
    );
}
