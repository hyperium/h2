
#[macro_export]
macro_rules! assert_closed {
    ($transport:expr) => {{
        assert_eq!($transport.poll().unwrap(), None.into());
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
