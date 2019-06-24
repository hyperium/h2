use std::fmt;

use bytes::{Bytes, IntoBuf};
use http::{self, HeaderMap, HttpTryFrom};

use super::SendFrame;
use h2::frame::{self, Frame, StreamId};

pub const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
pub const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

// ==== helper functions to easily construct h2 Frames ====

pub fn headers<T>(id: T) -> Mock<frame::Headers>
where
    T: Into<StreamId>,
{
    Mock(frame::Headers::new(
        id.into(),
        frame::Pseudo::default(),
        HeaderMap::default(),
    ))
}

pub fn data<T, B>(id: T, buf: B) -> Mock<frame::Data>
where
    T: Into<StreamId>,
    B: Into<Bytes>,
{
    Mock(frame::Data::new(id.into(), buf.into()))
}

pub fn push_promise<T1, T2>(id: T1, promised: T2) -> Mock<frame::PushPromise>
where
    T1: Into<StreamId>,
    T2: Into<StreamId>,
{
    Mock(frame::PushPromise::new(
        id.into(),
        promised.into(),
        frame::Pseudo::default(),
        HeaderMap::default(),
    ))
}

pub fn window_update<T>(id: T, sz: u32) -> frame::WindowUpdate
where
    T: Into<StreamId>,
{
    frame::WindowUpdate::new(id.into(), sz)
}

pub fn go_away<T>(id: T) -> Mock<frame::GoAway>
where
    T: Into<StreamId>,
{
    Mock(frame::GoAway::new(id.into(), frame::Reason::NO_ERROR))
}

pub fn reset<T>(id: T) -> Mock<frame::Reset>
where
    T: Into<StreamId>,
{
    Mock(frame::Reset::new(id.into(), frame::Reason::NO_ERROR))
}

pub fn settings() -> Mock<frame::Settings> {
    Mock(frame::Settings::default())
}

pub fn settings_ack() -> Mock<frame::Settings> {
    Mock(frame::Settings::ack())
}

pub fn ping(payload: [u8; 8]) -> Mock<frame::Ping> {
    Mock(frame::Ping::new(payload))
}

// === Generic helpers of all frame types

pub struct Mock<T>(T);

impl<T: fmt::Debug> fmt::Debug for Mock<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<T> From<Mock<T>> for Frame
where
    T: Into<Frame>,
{
    fn from(src: Mock<T>) -> Self {
        src.0.into()
    }
}

// Headers helpers

impl Mock<frame::Headers> {
    pub fn request<M, U>(self, method: M, uri: U) -> Self
    where
        M: HttpTryInto<http::Method>,
        U: HttpTryInto<http::Uri>,
    {
        let method = method.try_into().unwrap();
        let uri = uri.try_into().unwrap();
        let (id, _, fields) = self.into_parts();
        let frame = frame::Headers::new(id, frame::Pseudo::request(method, uri), fields);
        Mock(frame)
    }

    pub fn response<S>(self, status: S) -> Self
    where
        S: HttpTryInto<http::StatusCode>,
    {
        let status = status.try_into().unwrap();
        let (id, _, fields) = self.into_parts();
        let frame = frame::Headers::new(id, frame::Pseudo::response(status), fields);
        Mock(frame)
    }

    pub fn fields(self, fields: HeaderMap) -> Self {
        let (id, pseudo, _) = self.into_parts();
        let frame = frame::Headers::new(id, pseudo, fields);
        Mock(frame)
    }

    pub fn field<K, V>(self, key: K, value: V) -> Self
    where
        K: HttpTryInto<http::header::HeaderName>,
        V: HttpTryInto<http::header::HeaderValue>,
    {
        let (id, pseudo, mut fields) = self.into_parts();
        fields.insert(key.try_into().unwrap(), value.try_into().unwrap());
        let frame = frame::Headers::new(id, pseudo, fields);
        Mock(frame)
    }

    pub fn scheme(self, value: &str) -> Self
    {
        let (id, mut pseudo, fields) = self.into_parts();
        let value = value.parse().unwrap();

        pseudo.set_scheme(value);

        Mock(frame::Headers::new(id, pseudo, fields))
    }

    pub fn eos(mut self) -> Self {
        self.0.set_end_stream();
        self
    }

    pub fn into_fields(self) -> HeaderMap {
        self.0.into_parts().1
    }

    fn into_parts(self) -> (StreamId, frame::Pseudo, HeaderMap) {
        assert!(!self.0.is_end_stream(), "eos flag will be lost");
        assert!(self.0.is_end_headers(), "unset eoh will be lost");
        let id = self.0.stream_id();
        let parts = self.0.into_parts();
        (id, parts.0, parts.1)
    }
}

impl From<Mock<frame::Headers>> for frame::Headers {
    fn from(src: Mock<frame::Headers>) -> Self {
        src.0
    }
}

impl From<Mock<frame::Headers>> for SendFrame {
    fn from(src: Mock<frame::Headers>) -> Self {
        Frame::Headers(src.0)
    }
}

// Data helpers

impl Mock<frame::Data> {
    pub fn padded(mut self) -> Self {
        self.0.set_padded();
        self
    }

    pub fn eos(mut self) -> Self {
        self.0.set_end_stream(true);
        self
    }
}

impl From<Mock<frame::Data>> for SendFrame {
    fn from(src: Mock<frame::Data>) -> Self {
        let id = src.0.stream_id();
        let eos = src.0.is_end_stream();
        let is_padded = src.0.is_padded();
        let payload = src.0.into_payload();
        let mut frame = frame::Data::new(id, payload.into_buf());
        frame.set_end_stream(eos);
        if is_padded {
            frame.set_padded();
        }
        Frame::Data(frame)
    }
}


// PushPromise helpers

impl Mock<frame::PushPromise> {
    pub fn request<M, U>(self, method: M, uri: U) -> Self
    where
        M: HttpTryInto<http::Method>,
        U: HttpTryInto<http::Uri>,
    {
        let method = method.try_into().unwrap();
        let uri = uri.try_into().unwrap();
        let (id, promised, _, fields) = self.into_parts();
        let frame =
            frame::PushPromise::new(id, promised, frame::Pseudo::request(method, uri), fields);
        Mock(frame)
    }

    pub fn fields(self, fields: HeaderMap) -> Self {
        let (id, promised, pseudo, _) = self.into_parts();
        let frame = frame::PushPromise::new(id, promised, pseudo, fields);
        Mock(frame)
    }

    pub fn field<K, V>(self, key: K, value: V) -> Self
    where
        K: HttpTryInto<http::header::HeaderName>,
        V: HttpTryInto<http::header::HeaderValue>,
    {
        let (id, promised, pseudo, mut fields) = self.into_parts();
        fields.insert(key.try_into().unwrap(), value.try_into().unwrap());
        let frame = frame::PushPromise::new(id, promised, pseudo, fields);
        Mock(frame)
    }


    fn into_parts(self) -> (StreamId, StreamId, frame::Pseudo, HeaderMap) {
        assert!(self.0.is_end_headers(), "unset eoh will be lost");
        let id = self.0.stream_id();
        let promised = self.0.promised_id();
        let parts = self.0.into_parts();
        (id, promised, parts.0, parts.1)
    }
}

impl From<Mock<frame::PushPromise>> for SendFrame {
    fn from(src: Mock<frame::PushPromise>) -> Self {
        Frame::PushPromise(src.0)
    }
}

// GoAway helpers

impl Mock<frame::GoAway> {
    pub fn protocol_error(self) -> Self {
        self.reason(frame::Reason::PROTOCOL_ERROR)
    }

    pub fn internal_error(self) -> Self {
        self.reason(frame::Reason::INTERNAL_ERROR)
    }

    pub fn flow_control(self) -> Self {
        self.reason(frame::Reason::FLOW_CONTROL_ERROR)
    }

    pub fn frame_size(self) -> Self {
        self.reason(frame::Reason::FRAME_SIZE_ERROR)
    }

    pub fn no_error(self) -> Self {
        self.reason(frame::Reason::NO_ERROR)
    }

    pub fn reason(self, reason: frame::Reason) -> Self {
        Mock(frame::GoAway::new(
            self.0.last_stream_id(),
            reason,
        ))
    }
}

impl From<Mock<frame::GoAway>> for SendFrame {
    fn from(src: Mock<frame::GoAway>) -> Self {
        Frame::GoAway(src.0)
    }
}

// ==== Reset helpers

impl Mock<frame::Reset> {
    pub fn protocol_error(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::PROTOCOL_ERROR))
    }

    pub fn flow_control(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::FLOW_CONTROL_ERROR))
    }

    pub fn refused(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::REFUSED_STREAM))
    }

    pub fn cancel(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::CANCEL))
    }

    pub fn stream_closed(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::STREAM_CLOSED))
    }

    pub fn internal_error(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::INTERNAL_ERROR))
    }

    pub fn reason(self, reason: frame::Reason) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, reason))
    }
}

impl From<Mock<frame::Reset>> for SendFrame {
    fn from(src: Mock<frame::Reset>) -> Self {
        Frame::Reset(src.0)
    }
}

// ==== Settings helpers

impl Mock<frame::Settings> {
    pub fn max_concurrent_streams(mut self, max: u32) -> Self {
        self.0.set_max_concurrent_streams(Some(max));
        self
    }

    pub fn initial_window_size(mut self, val: u32) -> Self {
        self.0.set_initial_window_size(Some(val));
        self
    }

    pub fn max_header_list_size(mut self, val: u32) -> Self {
        self.0.set_max_header_list_size(Some(val));
        self
    }
}

impl From<Mock<frame::Settings>> for frame::Settings {
    fn from(src: Mock<frame::Settings>) -> Self {
        src.0
    }
}

impl From<Mock<frame::Settings>> for SendFrame {
    fn from(src: Mock<frame::Settings>) -> Self {
        Frame::Settings(src.0)
    }
}

// ==== Ping helpers

impl Mock<frame::Ping> {
    pub fn pong(self) -> Self {
        let payload = self.0.into_payload();
        Mock(frame::Ping::pong(payload))
    }
}

impl From<Mock<frame::Ping>> for SendFrame {
    fn from(src: Mock<frame::Ping>) -> Self {
        Frame::Ping(src.0)
    }
}

// ==== "trait alias" for types that are HttpTryFrom and have Debug Errors ====

pub trait HttpTryInto<T> {
    type Error: fmt::Debug;

    fn try_into(self) -> Result<T, Self::Error>;
}

impl<T, U> HttpTryInto<T> for U
where
    T: HttpTryFrom<U>,
    T::Error: fmt::Debug,
{
    type Error = T::Error;

    fn try_into(self) -> Result<T, Self::Error> {
        T::try_from(self)
    }
}
