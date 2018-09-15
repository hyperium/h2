
#[macro_export]
macro_rules! assert_closed {
    ($transport:expr) => {{
        assert_eq!($transport.poll().unwrap(), None.into());
    }}
}

#[macro_export]
macro_rules! assert_headers {
    ($frame:expr) => {{
        match $frame {
            ::h2::frame::Frame::Headers(v) => v,
            f => panic!("expected HEADERS; actual={:?}", f),
        }
    }}
}

#[macro_export]
macro_rules! assert_data {
    ($frame:expr) => {{
        match $frame {
            ::h2::frame::Frame::Data(v) => v,
            f => panic!("expected DATA; actual={:?}", f),
        }
    }}
}

#[macro_export]
macro_rules! assert_ping {
    ($frame:expr) => {{
        match $frame {
            ::h2::frame::Frame::Ping(v) => v,
            f => panic!("expected PING; actual={:?}", f),
        }
    }}
}

#[macro_export]
macro_rules! assert_settings {
    ($frame:expr) => {{
        match $frame {
            ::h2::frame::Frame::Settings(v) => v,
            f => panic!("expected SETTINGS; actual={:?}", f),
        }
    }}
}

#[macro_export]
macro_rules! poll_err {
    ($transport:expr) => {{
        match $transport.poll() {
            Err(e) => e,
            frame => panic!("expected error; actual={:?}", frame),
        }
    }}
}

#[macro_export]
macro_rules! poll_frame {
    ($type: ident, $transport:expr) => {{
        use h2::frame::Frame;
        use futures::Async;

        match $transport.poll() {
            Ok(Async::Ready(Some(Frame::$type(frame)))) => frame,
            frame => panic!("unexpected frame; actual={:?}", frame),
        }
    }}
}
