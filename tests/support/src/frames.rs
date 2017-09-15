use std::fmt;

use bytes::{Bytes, IntoBuf};
use http::{self, HeaderMap, HttpTryFrom};

use h2::frame::{self, Frame, StreamId};
use super::SendFrame;

pub const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
pub const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

// ==== helper functions to easily construct h2 Frames ====

pub fn headers<T>(id: T) -> Mock<frame::Headers>
    where T: Into<StreamId>,
{
    Mock(frame::Headers::new(
        id.into(),
        frame::Pseudo::default(),
        HeaderMap::default(),
    ))
}

pub fn data<T, B>(id: T, buf: B) -> Mock<frame::Data>
    where T: Into<StreamId>,
          B: Into<Bytes>,
{
    Mock(frame::Data::new(id.into(), buf.into()))
}

pub fn window_update<T>(id: T, sz: u32) -> frame::WindowUpdate
    where T: Into<StreamId>,
{
    frame::WindowUpdate::new(id.into(), sz)
}

pub fn go_away<T>(id: T) -> Mock<frame::GoAway>
    where T: Into<StreamId>,
{
    Mock(frame::GoAway::new(id.into(), frame::Reason::NoError))
}

pub fn reset<T>(id: T) -> Mock<frame::Reset>
    where T: Into<StreamId>,
{
    Mock(frame::Reset::new(id.into(), frame::Reason::NoError))
}

// === Generic helpers of all frame types

pub struct Mock<T>(T);

impl<T: fmt::Debug> fmt::Debug for Mock<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<T> From<Mock<T>> for Frame
where T: Into<Frame> {
    fn from(src: Mock<T>) -> Self {
        src.0.into()
    }
}

// Headers helpers

impl Mock<frame::Headers> {
    pub fn request<M, U>(self, method: M, uri: U) -> Self
        where M: HttpTryInto<http::Method>,
              U: HttpTryInto<http::Uri>,
    {
        let method = method.try_into().unwrap();
        let uri = uri.try_into().unwrap();
        let (id, _, fields) = self.into_parts();
        let frame = frame::Headers::new(
            id,
            frame::Pseudo::request(method, uri),
            fields
        );
        Mock(frame)
    }

    pub fn response<S>(self, status: S) -> Self
        where S: HttpTryInto<http::StatusCode>,
    {
        let status = status.try_into().unwrap();
        let (id, _, fields) = self.into_parts();
        let frame = frame::Headers::new(
            id,
            frame::Pseudo::response(status),
            fields
        );
        Mock(frame)
    }

    pub fn fields(self, fields: HeaderMap) -> Self {
        let (id, pseudo, _) = self.into_parts();
        let frame = frame::Headers::new(id, pseudo, fields);
        Mock(frame)
    }

    pub fn eos(mut self) -> Self {
        self.0.set_end_stream();
        self
    }

    fn into_parts(self) -> (StreamId, frame::Pseudo, HeaderMap) {
        assert!(!self.0.is_end_stream(), "eos flag will be lost");
        assert!(self.0.is_end_headers(), "unset eoh will be lost");
        let id = self.0.stream_id();
        let parts = self.0.into_parts();
        (id, parts.0, parts.1)
    }
}

impl From<Mock<frame::Headers>> for SendFrame {
    fn from(src: Mock<frame::Headers>) -> Self {
        Frame::Headers(src.0)
    }
}

// Data helpers

impl Mock<frame::Data> {
    pub fn eos(mut self) -> Self {
        self.0.set_end_stream(true);
        self
    }
}

impl From<Mock<frame::Data>> for SendFrame {
    fn from(src: Mock<frame::Data>) -> Self {
        let id = src.0.stream_id();
        let eos = src.0.is_end_stream();
        let payload = src.0.into_payload();
        let mut frame = frame::Data::new(id, payload.into_buf());
        frame.set_end_stream(eos);
        Frame::Data(frame)
    }
}

// GoAway helpers

impl Mock<frame::GoAway> {
    pub fn flow_control(self) -> Self {
        Mock(frame::GoAway::new(self.0.last_stream_id(), frame::Reason::FlowControlError))
    }

    pub fn frame_size(self) -> Self {
        Mock(frame::GoAway::new(self.0.last_stream_id(), frame::Reason::FrameSizeError))
    }
}

impl From<Mock<frame::GoAway>> for SendFrame {
    fn from(src: Mock<frame::GoAway>) -> Self {
        Frame::GoAway(src.0)
    }
}

// ==== Reset helpers

impl Mock<frame::Reset> {
    pub fn flow_control(self) -> Self {
        let id = self.0.stream_id();
        Mock(frame::Reset::new(id, frame::Reason::FlowControlError))
    }
}

impl From<Mock<frame::Reset>> for SendFrame {
    fn from(src: Mock<frame::Reset>) -> Self {
        Frame::Reset(src.0)
    }
}

// ==== "trait alias" for types that are HttpTryFrom and have Debug Errors ====

pub trait HttpTryInto<T> {
    type Error: fmt::Debug;

    fn try_into(self) -> Result<T, Self::Error>;
}

impl<T, U> HttpTryInto<T> for U
    where T: HttpTryFrom<U>,
          T::Error: fmt::Debug,
{
    type Error = T::Error;

    fn try_into(self) -> Result<T, Self::Error> {
        T::try_from(self)
    }
}
