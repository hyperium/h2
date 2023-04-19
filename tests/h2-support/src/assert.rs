#[macro_export]
macro_rules! assert_closed {
    ($transport:expr) => {{
        use futures::StreamExt;

        assert!($transport.next().await.is_none());
    }};
}

#[macro_export]
macro_rules! assert_headers {
    ($frame:expr) => {{
        match $frame {
            h2::frame::Frame::Headers(v) => v,
            f => panic!("expected HEADERS; actual={:?}", f),
        }
    }};
}

#[macro_export]
macro_rules! assert_data {
    ($frame:expr) => {{
        match $frame {
            h2::frame::Frame::Data(v) => v,
            f => panic!("expected DATA; actual={:?}", f),
        }
    }};
}

#[macro_export]
macro_rules! assert_ping {
    ($frame:expr) => {{
        match $frame {
            h2::frame::Frame::Ping(v) => v,
            f => panic!("expected PING; actual={:?}", f),
        }
    }};
}

#[macro_export]
macro_rules! assert_settings {
    ($frame:expr) => {{
        match $frame {
            h2::frame::Frame::Settings(v) => v,
            f => panic!("expected SETTINGS; actual={:?}", f),
        }
    }};
}

#[macro_export]
macro_rules! assert_go_away {
    ($frame:expr) => {{
        match $frame {
            h2::frame::Frame::GoAway(v) => v,
            f => panic!("expected GO_AWAY; actual={:?}", f),
        }
    }};
}

#[macro_export]
macro_rules! poll_err {
    ($transport:expr) => {{
        use futures::StreamExt;
        match $transport.next().await {
            Some(Err(e)) => e,
            frame => panic!("expected error; actual={:?}", frame),
        }
    }};
}

#[macro_export]
macro_rules! poll_frame {
    ($type: ident, $transport:expr) => {{
        use futures::StreamExt;
        use h2::frame::Frame;

        match $transport.next().await {
            Some(Ok(Frame::$type(frame))) => frame,
            frame => panic!("unexpected frame; actual={:?}", frame),
        }
    }};
}

#[macro_export]
macro_rules! assert_default_settings {
    ($settings: expr) => {{
        assert_frame_eq($settings, frame::Settings::default());
    }};
}

use h2::frame::Frame;

#[track_caller]
pub fn assert_frame_eq<T: Into<Frame>, U: Into<Frame>>(t: T, u: U) {
    let actual: Frame = t.into();
    let expected: Frame = u.into();
    match (actual, expected) {
        (Frame::Data(a), Frame::Data(b)) => {
            assert_eq!(
                a.payload().len(),
                b.payload().len(),
                "assert_frame_eq data payload len"
            );
            assert_eq!(a, b, "assert_frame_eq");
        }
        (a, b) => {
            assert_eq!(a, b, "assert_frame_eq");
        }
    }
}
