use std::fmt;

use bytes::{Bytes, IntoBuf};
use http::{self, HeaderMap, HttpTryFrom};

use h2::frame::{self, Frame, StreamId};
use super::SendFrame;

pub const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
pub const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];

// ==== helper functions to easily construct h2 Frames ====

pub fn headers<T>(id: T) -> MockHeaders
    where T: Into<StreamId>,
{
    MockHeaders(frame::Headers::new(
        id.into(),
        frame::Pseudo::default(),
        HeaderMap::default(),
    ))
}

pub fn data<T, B>(id: T, buf: B) -> MockData
    where T: Into<StreamId>,
          B: Into<Bytes>,
{
    MockData(frame::Data::new(id.into(), buf.into()))
}

pub fn window_update<T>(id: T, sz: u32) -> frame::WindowUpdate
    where T: Into<StreamId>,
{
    frame::WindowUpdate::new(id.into(), sz)
}

// Headers helpers

pub struct MockHeaders(frame::Headers);

impl MockHeaders {
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
        MockHeaders(frame)
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
        MockHeaders(frame)
    }

    pub fn fields(self, fields: HeaderMap) -> Self {
        let (id, pseudo, _) = self.into_parts();
        let frame = frame::Headers::new(id, pseudo, fields);
        MockHeaders(frame)
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

impl fmt::Debug for MockHeaders {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl From<MockHeaders> for Frame {
    fn from(src: MockHeaders) -> Self {
        Frame::Headers(src.0)
    }
}

impl From<MockHeaders> for SendFrame {
    fn from(src: MockHeaders) -> Self {
        Frame::Headers(src.0)
    }
}

// Data helpers

pub struct MockData(frame::Data);

impl MockData {
    pub fn eos(mut self) -> Self {
        self.0.set_end_stream(true);
        self
    }
}

impl fmt::Debug for MockData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl From<MockData> for Frame {
    fn from(src: MockData) -> Self {
        Frame::Data(src.0)
    }
}

impl From<MockData> for SendFrame {
    fn from(src: MockData) -> Self {
        let id = src.0.stream_id();
        let eos = src.0.is_end_stream();
        let payload = src.0.into_payload();
        let mut frame = frame::Data::new(id, payload.into_buf());
        frame.set_end_stream(eos);
        Frame::Data(frame)
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
