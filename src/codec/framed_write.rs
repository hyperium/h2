use crate::codec::UserError;
use crate::codec::UserError::*;
use crate::frame::{self, Frame, FrameSize};
use crate::hpack;

use bytes::{Buf, BufMut, BytesMut};
use std::num::NonZeroU32;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::io::poll_write_buf;

use std::io::{self, Cursor};

// A macro to get around a method needing to borrow &mut self
macro_rules! limited_write_buf {
    ($self:expr) => {{
        let limit = $self.max_frame_size() + frame::HEADER_LEN;
        $self.buf.get_mut().limit(limit)
    }};
}

#[derive(Debug)]
pub struct FramedWrite<T, B> {
    /// Upstream `AsyncWrite`
    inner: T,
    final_flush_done: bool,

    encoder: Encoder<B>,
}

#[derive(Debug)]
struct Encoder<B> {
    /// HPACK encoder
    hpack: hpack::EncoderState,

    /// Reusable storage for encoded headers or deferred control frames.
    /// `next` prevents header encoding while this buffer contains controls.
    scratch: BytesMut,

    /// Write buffer
    ///
    /// TODO: Should this be a ring buffer?
    buf: Cursor<BytesMut>,

    /// Next frame to encode
    next: Option<Next<B>>,

    /// Last data frame
    last_data_frame: Option<frame::Data<B>>,

    /// Max frame size, this is specified by the peer
    max_frame_size: FrameSize,

    /// Frame-size limit before the deferred control queue. Store size + 1 so
    /// zero remains a valid limit and `None` needs no separate discriminant.
    deferred_frame_size: Option<NonZeroU32>,

    /// Chain payloads bigger than this.
    chain_threshold: usize,

    /// Min buffer required to attempt to write a frame
    min_buffer_capacity: usize,
}

#[derive(Debug)]
enum Next<B> {
    Data(frame::Data<B>),
    Continuation(frame::Continuation),
    // Prevent ordinary frames from overtaking the separate control queue.
    Control,
}

/// Initialize the connection with this amount of write buffer.
///
/// The minimum MAX_FRAME_SIZE is 16kb, so always be able to send a HEADERS
/// frame that big.
const DEFAULT_BUFFER_CAPACITY: usize = 16 * 1_024;

const MAX_CONTROL_BUFFER_SIZE: usize = 16 * 1_024;

/// Chain payloads bigger than this when vectored I/O is enabled. The remote
/// will never advertise a max frame size less than this (well, the spec says
/// the max frame size can't be less than 16kb, so not even close).
const CHAIN_THRESHOLD: usize = 256;

/// Chain payloads bigger than this when vectored I/O is **not** enabled.
/// A larger value in this scenario will reduce the number of small and
/// fragmented data being sent, and hereby improve the throughput.
const CHAIN_THRESHOLD_WITHOUT_VECTORED_IO: usize = 1024;

// TODO: Make generic
impl<T, B> FramedWrite<T, B>
where
    T: AsyncWrite + Unpin,
    B: Buf,
{
    pub fn new(inner: T) -> FramedWrite<T, B> {
        let chain_threshold = if inner.is_write_vectored() {
            CHAIN_THRESHOLD
        } else {
            CHAIN_THRESHOLD_WITHOUT_VECTORED_IO
        };
        FramedWrite {
            inner,
            final_flush_done: false,
            encoder: Encoder {
                hpack: hpack::EncoderState::default(),
                scratch: BytesMut::new(),
                buf: Cursor::new(BytesMut::with_capacity(DEFAULT_BUFFER_CAPACITY)),
                next: None,
                last_data_frame: None,
                max_frame_size: frame::DEFAULT_MAX_FRAME_SIZE,
                deferred_frame_size: None,
                chain_threshold,
                min_buffer_capacity: chain_threshold + frame::HEADER_LEN,
            },
        }
    }

    /// Returns `Ready` when `send` is able to accept a frame
    ///
    /// Calling this function may result in the current contents of the buffer
    /// to be flushed to `T`.
    pub fn poll_ready(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        if !self.encoder.has_capacity() {
            // Try flushing
            ready!(self.flush(cx))?;

            if !self.encoder.has_capacity() {
                return Poll::Pending;
            }
        }

        Poll::Ready(Ok(()))
    }

    /// Returns whether a frame can be buffered without first flushing the
    /// underlying I/O object.
    pub(crate) fn has_capacity(&self) -> bool {
        self.encoder.has_capacity()
    }

    /// Buffer a frame.
    ///
    /// `poll_ready` must be called first to ensure that a frame may be
    /// accepted.
    pub fn buffer(&mut self, item: Frame<B>) -> Result<(), UserError> {
        self.encoder.buffer(item)
    }

    pub(crate) fn poll_ready_control(
        &mut self,
        cx: &mut Context,
        item: &Frame<B>,
    ) -> Poll<io::Result<()>> {
        if self.poll_ready(cx)?.is_ready() {
            return Poll::Ready(Ok(()));
        }

        let queued = if self.encoder.has_control() {
            self.encoder.scratch.len()
        } else {
            0
        };
        if control_frame_capacity(item) > MAX_CONTROL_BUFFER_SIZE - queued {
            return Poll::Pending;
        }

        Poll::Ready(Ok(()))
    }

    /// Buffer a control frame after `poll_ready_control` returns ready.
    pub(crate) fn buffer_control(&mut self, item: Frame<B>) -> Result<(), UserError> {
        if self.has_capacity() {
            return self.encoder.buffer(item);
        }

        // No new header block can be encoded until `next` is cleared. Reuse
        // the frame writer's scratch buffer for controls during that interval.
        if !self.encoder.has_control() {
            self.encoder.scratch.clear();
            self.encoder.deferred_frame_size = NonZeroU32::new(self.encoder.max_frame_size + 1);
            if self.encoder.next.is_none() {
                self.encoder.next = Some(Next::Control);
            }
        }
        let control = &mut self.encoder.scratch;
        assert!(control_frame_capacity(&item) <= MAX_CONTROL_BUFFER_SIZE - control.len());
        tracing::debug!(frame = ?item, "send");
        match item {
            Frame::Ping(frame) => frame.encode(control),
            Frame::Settings(frame) => frame.encode(control),
            Frame::Reset(frame) => frame.encode(control),
            Frame::GoAway(frame) => frame.encode(control),
            _ => unreachable!("expected control frame"),
        }

        Ok(())
    }

    /// Flush buffered data to the wire
    pub fn flush(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        let span = tracing::trace_span!("FramedWrite::flush");
        let _e = span.enter();

        loop {
            while !self.encoder.is_empty() {
                let n = match self.encoder.next {
                    Some(Next::Data(ref mut frame)) => {
                        tracing::trace!(queued_data_frame = true);
                        let mut buf = (&mut self.encoder.buf).chain(frame.payload_mut());
                        ready!(poll_write_buf(Pin::new(&mut self.inner), cx, &mut buf))?
                    }
                    _ => {
                        tracing::trace!(queued_data_frame = false);
                        ready!(poll_write_buf(
                            Pin::new(&mut self.inner),
                            cx,
                            &mut self.encoder.buf
                        ))?
                    }
                };
                if n == 0 {
                    // No progress is possible; retrying would busy-loop.
                    tracing::trace!("write returned zero, but non-zero bytes remaining");
                    return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
            }

            match self.encoder.unset_frame() {
                ControlFlow::Continue => (),
                ControlFlow::Break => break,
            }
        }

        if self.encoder.has_control() {
            self.encoder.next = Some(Next::Control);
            let control = &mut self.encoder.scratch;
            while control.has_remaining() {
                let n = ready!(poll_write_buf(Pin::new(&mut self.inner), cx, control))?;
                if n == 0 {
                    return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
            }
            control.clear();
            self.encoder.deferred_frame_size = None;
            self.encoder.next = None;
        }

        tracing::trace!("flushing buffer");
        // Flush the upstream
        ready!(Pin::new(&mut self.inner).poll_flush(cx))?;

        Poll::Ready(Ok(()))
    }

    /// Close the codec
    pub fn shutdown(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        if !self.final_flush_done {
            ready!(self.flush(cx))?;
            self.final_flush_done = true;
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn control_frame_capacity<B>(frame: &Frame<B>) -> usize {
    let payload = match frame {
        Frame::Ping(_) => 8,
        Frame::Settings(_) => 7 * 6,
        Frame::Reset(_) => 4,
        Frame::GoAway(frame) => frame.debug_data().len().saturating_add(8),
        _ => unreachable!("expected control frame"),
    };
    frame::HEADER_LEN.saturating_add(payload)
}

#[must_use]
enum ControlFlow {
    Continue,
    Break,
}

impl<B> Encoder<B>
where
    B: Buf,
{
    #[inline(always)]
    fn unset_frame(&mut self) -> ControlFlow {
        // Clear internal buffer
        self.buf.set_position(0);
        self.buf.get_mut().clear();

        // The data frame has been written, so unset it
        match self.next.take() {
            Some(Next::Data(frame)) => {
                self.last_data_frame = Some(frame);
                debug_assert!(self.is_empty());
                ControlFlow::Break
            }
            Some(Next::Continuation(frame)) => {
                let max_frame_size = self
                    .deferred_frame_size
                    .map_or(self.max_frame_size, |size| size.get() - 1);
                let limit = max_frame_size as usize + frame::HEADER_LEN;
                let mut buf = self.buf.get_mut().limit(limit);
                if let Some(continuation) = frame.encode(&mut buf) {
                    self.next = Some(Next::Continuation(continuation));
                } else if self.has_control() {
                    self.next = Some(Next::Control);
                }
                ControlFlow::Continue
            }
            Some(Next::Control) => {
                self.next = Some(Next::Control);
                ControlFlow::Break
            }
            None => ControlFlow::Break,
        }
    }

    fn buffer(&mut self, item: Frame<B>) -> Result<(), UserError> {
        // Ensure that we have enough capacity to accept the write.
        assert!(self.has_capacity());
        let span = tracing::trace_span!("FramedWrite::buffer", frame = ?item);
        let _e = span.enter();

        tracing::debug!(frame = ?item, "send");

        match item {
            Frame::Data(mut v) => {
                // Ensure that the payload is not greater than the max frame.
                let len = v.payload().remaining();

                if len > self.max_frame_size() {
                    return Err(PayloadTooBig);
                }

                if len >= self.chain_threshold {
                    let head = v.head();

                    // Encode the frame head to the buffer
                    head.encode(len, self.buf.get_mut());

                    if self.buf.get_ref().remaining() < self.chain_threshold {
                        let extra_bytes = self.chain_threshold - self.buf.remaining();
                        self.buf.get_mut().put(v.payload_mut().take(extra_bytes));
                    }

                    // Save the data frame
                    self.next = Some(Next::Data(v));
                } else {
                    v.encode_chunk(self.buf.get_mut());

                    // The chunk has been fully encoded, so there is no need to
                    // keep it around
                    assert_eq!(v.payload().remaining(), 0, "chunk not fully encoded");

                    // Save off the last frame...
                    self.last_data_frame = Some(v);
                }
            }
            Frame::Headers(v) => {
                let mut buf = limited_write_buf!(self);
                if let Some(continuation) =
                    v.encode_with_state(&mut self.hpack, &mut self.scratch, &mut buf)
                {
                    self.next = Some(Next::Continuation(continuation));
                }
            }
            Frame::PushPromise(v) => {
                let mut buf = limited_write_buf!(self);
                if let Some(continuation) =
                    v.encode_with_state(&mut self.hpack, &mut self.scratch, &mut buf)
                {
                    self.next = Some(Next::Continuation(continuation));
                }
            }
            Frame::Settings(v) => {
                v.encode(self.buf.get_mut());
                tracing::trace!(rem = self.buf.remaining(), "encoded settings");
            }
            Frame::GoAway(v) => {
                v.encode(self.buf.get_mut());
                tracing::trace!(rem = self.buf.remaining(), "encoded go_away");
            }
            Frame::Ping(v) => {
                v.encode(self.buf.get_mut());
                tracing::trace!(rem = self.buf.remaining(), "encoded ping");
            }
            Frame::WindowUpdate(v) => {
                v.encode(self.buf.get_mut());
                tracing::trace!(rem = self.buf.remaining(), "encoded window_update");
            }

            Frame::Priority(_) => {
                /*
                v.encode(self.buf.get_mut());
                tracing::trace!("encoded priority; rem={:?}", self.buf.remaining());
                */
                unimplemented!();
            }
            Frame::Reset(v) => {
                v.encode(self.buf.get_mut());
                tracing::trace!(rem = self.buf.remaining(), "encoded reset");
            }
        }

        Ok(())
    }

    fn has_capacity(&self) -> bool {
        self.next.is_none()
            && (self.buf.get_ref().capacity() - self.buf.get_ref().len()
                >= self.min_buffer_capacity)
    }

    fn is_empty(&self) -> bool {
        match self.next {
            Some(Next::Data(ref frame)) => !frame.payload().has_remaining(),
            _ => !self.buf.has_remaining(),
        }
    }
}

impl<B> Encoder<B> {
    fn has_control(&self) -> bool {
        self.deferred_frame_size.is_some()
    }

    fn max_frame_size(&self) -> usize {
        self.max_frame_size as usize
    }
}

impl<T, B> FramedWrite<T, B> {
    /// Returns the max frame size that can be sent
    pub fn max_frame_size(&self) -> usize {
        self.encoder.max_frame_size()
    }

    /// Set the peer's max frame size.
    pub fn set_max_frame_size(&mut self, val: usize) {
        assert!(val <= frame::MAX_MAX_FRAME_SIZE as usize);
        self.encoder.max_frame_size = val as FrameSize;
    }

    /// Set the peer's header table size.
    pub fn set_header_table_size(&mut self, val: usize) {
        self.encoder.hpack.update_max_size(val);
    }

    /// Retrieve the last data frame that has been sent
    pub fn take_last_data_frame(&mut self) -> Option<frame::Data<B>> {
        self.encoder.last_data_frame.take()
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: AsyncRead + Unpin, B> AsyncRead for FramedWrite<T, B> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

// We never project the Pin to `B`.
impl<T: Unpin, B> Unpin for FramedWrite<T, B> {}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    #[derive(Default)]
    struct TestIo {
        written: Vec<u8>,
        allowance: usize,
        vectored: bool,
    }

    impl AsyncWrite for TestIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.allowance == 0 {
                return Poll::Pending;
            }
            let len = buf.len().min(self.allowance).min(7);
            self.written.extend_from_slice(&buf[..len]);
            self.allowance -= len;
            Poll::Ready(Ok(len))
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bufs: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            self.poll_write(
                cx,
                bufs.iter()
                    .find(|buf| !buf.is_empty())
                    .map_or(&[], |buf| &buf[..]),
            )
        }

        fn is_write_vectored(&self) -> bool {
            self.vectored
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl<B: Buf> FramedWrite<TestIo, B> {
        fn poll_test(
            &mut self,
            f: impl FnOnce(&mut Self, &mut Context<'_>) -> Poll<io::Result<()>>,
        ) -> bool {
            let waker = Waker::from(Arc::new(NoopWake));
            f(self, &mut Context::from_waker(&waker))
                .map(Result::unwrap)
                .is_ready()
        }

        fn ready_control(&mut self, frame: &Frame<B>) -> bool {
            self.poll_test(|writer, cx| writer.poll_ready_control(cx, frame))
        }

        fn send_control(&mut self, frame: Frame<B>) {
            assert!(self.ready_control(&frame));
            self.buffer_control(frame).unwrap();
        }

        fn flush_with(&mut self, allowance: usize) -> bool {
            self.inner.allowance = allowance;
            self.poll_test(Self::flush)
        }

        fn ready(&mut self) -> bool {
            self.poll_test(Self::poll_ready)
        }
    }

    fn writer(vectored: bool) -> FramedWrite<TestIo, Bytes> {
        FramedWrite::new(TestIo {
            vectored,
            ..TestIo::default()
        })
    }

    fn data(payload: Bytes) -> Frame<Bytes> {
        frame::Data::new(1.into(), payload).into()
    }

    fn headers(stream: u32, size: usize) -> Frame<Bytes> {
        let mut fields = http::HeaderMap::new();
        fields.insert(
            "large",
            http::HeaderValue::from_bytes(&vec![b'x'; size]).unwrap(),
        );
        frame::Headers::new(stream.into(), frame::Pseudo::default(), fields).into()
    }

    fn wire_frames(mut wire: &[u8]) -> Vec<(u8, u8, &[u8])> {
        let mut frames = Vec::new();
        while !wire.is_empty() {
            assert!(wire.len() >= frame::HEADER_LEN);
            let len =
                (usize::from(wire[0]) << 16) | (usize::from(wire[1]) << 8) | usize::from(wire[2]);
            let end = frame::HEADER_LEN + len;
            assert!(wire.len() >= end);
            frames.push((wire[3], wire[4], &wire[frame::HEADER_LEN..end]));
            wire = &wire[end..];
        }
        frames
    }

    #[test]
    fn deferred_controls_preserve_order_at_every_write_boundary() {
        let payload = Bytes::from(vec![42; 2048]);
        let wire_len = payload.len() + frame::HEADER_LEN + 17 + 9 + 13;
        for vectored in [false, true] {
            // Also defer the first control after DATA has already begun writing.
            let stops = (0..=wire_len)
                .map(|stop| (0, stop))
                .chain([(5, frame::HEADER_LEN + payload.len() - 5 + 7)]);
            for (prefix, stop) in stops {
                let mut writer = writer(vectored);
                writer.buffer(data(payload.clone())).unwrap();
                writer.inner.allowance = prefix;
                writer.send_control(frame::Ping::pong([1; 8]).into());
                writer.send_control(frame::Settings::ack().into());
                writer.send_control(frame::Reset::new(3.into(), frame::Reason::CANCEL).into());
                assert!(!writer.ready());
                let complete = prefix + stop == wire_len;
                assert_eq!(writer.flush_with(stop), complete);
                assert_eq!(writer.has_capacity(), complete);
                assert_eq!(writer.encoder.has_control(), !complete);
                writer.send_control(frame::Ping::pong([2; 8]).into());
                assert!(writer.flush_with(usize::MAX));
                assert!(writer.has_capacity());
                assert!(!writer.encoder.has_control());
                assert_eq!(writer.take_last_data_frame().unwrap().payload().len(), 0);
                assert_eq!(
                    wire_frames(&writer.inner.written),
                    vec![
                        (0, 0, &payload[..]),
                        (6, 1, &[1; 8][..]),
                        (4, 1, &[][..]),
                        (3, 0, &[0, 0, 0, 8][..]),
                        (6, 1, &[2; 8][..]),
                    ]
                );
            }
        }
    }

    #[test]
    fn continuations_preserve_order_and_frame_size() {
        for (old_max, new_max, defer) in [
            (16_384, 16_384, true),
            (16_384, 65_536, true),
            (65_536, 16_384, true),
            (16_384, 65_536, false),
            (65_536, 16_384, false),
        ] {
            let mut writer = writer(false);
            writer.set_max_frame_size(old_max);
            writer.buffer(headers(1, 128 * 1024)).unwrap();
            assert!(matches!(writer.encoder.next, Some(Next::Continuation(..))));
            writer.inner.allowance = 5;
            if defer {
                writer.send_control(frame::Ping::pong([3; 8]).into());
                writer.send_control(frame::Settings::ack().into());
            } else {
                assert!(!writer.flush_with(5));
            }
            writer.set_max_frame_size(new_max);
            assert!(writer.flush_with(usize::MAX));
            let frames = wire_frames(&writer.inner.written);
            let end = frames.len() - if defer { 2 } else { 0 };
            assert!(end >= 2);
            assert_eq!(frames[0].0, 1);
            assert_eq!(frames[0].1 & 4, 0);
            assert!(frames[0].2.len() <= old_max);
            let limit = if defer { old_max } else { new_max };
            for (index, frame) in frames[1..end].iter().enumerate() {
                assert_eq!(frame.0, 9);
                assert!(frame.2.len() <= limit);
                assert_eq!(frame.1 & 4, if index + 2 == end { 4 } else { 0 });
            }
            if defer {
                assert_eq!(&frames[end..], &[(6, 1, &[3; 8][..]), (4, 1, &[][..])]);
            } else {
                assert!(frames[1..end].iter().any(|frame| frame.2.len() == new_max));
            }
            let next_block = writer.inner.written.len();
            writer.buffer(headers(3, 128 * 1024)).unwrap();
            assert!(writer.flush_with(usize::MAX));
            let frames = wire_frames(&writer.inner.written[next_block..]);
            assert_eq!(frames[0].0, 1);
            assert_eq!(frames[0].2.len(), new_max);
            assert!(frames.iter().all(|frame| frame.2.len() <= new_max));
        }
    }

    #[test]
    fn final_continuation_keeps_controls_before_new_frames() {
        let mut writer = writer(false);
        writer.buffer(headers(1, 32768)).unwrap();
        let first_frame_len = writer.encoder.buf.get_ref().len();
        writer.send_control(frame::Ping::pong([7; 8]).into());
        // The final CONTINUATION leaves room, but new frames must wait for PING.
        assert!(!writer.flush_with(first_frame_len + 5));
        assert_eq!(&writer.inner.written[first_frame_len + 3..], &[9, 4]);
        assert!(!writer.has_capacity());
        assert!(!writer.ready());
        writer.inner.allowance = usize::MAX;
        assert!(writer.ready());
        writer.buffer(data(Bytes::from_static(b"tail"))).unwrap();
        assert!(writer.flush_with(usize::MAX));
        let frames = wire_frames(&writer.inner.written);
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[0].0, 1);
        assert_eq!((frames[1].0, frames[1].1 & 4), (9, 4));
        assert_eq!(frames[2], (6, 1, &[7; 8][..]));
        assert_eq!(frames[3], (0, 0, &b"tail"[..]));
    }

    #[test]
    fn deferred_controls_follow_full_ordinary_buffer() {
        let mut writer = writer(false);
        let mut ordinary_count = 0;
        while writer.has_capacity() {
            writer.buffer(frame::Ping::pong([8; 8]).into()).unwrap();
            ordinary_count += 1;
        }
        assert!(writer.encoder.next.is_none());
        let ordinary_len = writer.encoder.buf.get_ref().len();
        writer.send_control(frame::Ping::pong([9; 8]).into());
        assert!(!writer.flush_with(ordinary_len + 5));
        assert!(!writer.has_capacity());
        assert!(!writer.ready());
        writer.inner.allowance = usize::MAX;
        assert!(writer.ready());
        writer.buffer(data(Bytes::from_static(b"tail"))).unwrap();
        assert!(writer.flush_with(usize::MAX));
        let frames = wire_frames(&writer.inner.written);
        assert_eq!(frames.len(), ordinary_count + 2);
        assert!(frames[..ordinary_count]
            .iter()
            .all(|frame| *frame == (6, 1, &[8; 8][..])));
        assert_eq!(frames[ordinary_count], (6, 1, &[9; 8][..]));
        assert_eq!(frames[ordinary_count + 1], (0, 0, &b"tail"[..]));
    }

    #[test]
    fn full_control_queue_preserves_pending_frame_until_writable() {
        let mut writer = writer(false);
        writer.buffer(data(Bytes::from(vec![42; 4096]))).unwrap();
        let ping_count = MAX_CONTROL_BUFFER_SIZE / (frame::HEADER_LEN + 8);
        for _ in 0..ping_count {
            writer.send_control(frame::Ping::pong([4; 8]).into());
        }
        let pending = Frame::from(frame::Ping::pong([5; 8]));
        let queued = writer.encoder.scratch.clone();
        for _ in 0..2 {
            assert!(!writer.ready_control(&pending));
            assert_eq!(&writer.encoder.scratch, &queued);
        }
        let goaway = frame::GoAway::new(frame::StreamId::zero(), frame::Reason::NO_ERROR).into();
        assert!(!writer.ready_control(&goaway));
        assert_eq!(&writer.encoder.scratch, &queued);
        assert!(writer.encoder.scratch.len() <= MAX_CONTROL_BUFFER_SIZE);
        writer.inner.allowance = usize::MAX;
        assert!(writer.ready_control(&pending));
        writer.buffer_control(pending).unwrap();
        assert!(writer.flush_with(usize::MAX));
        let frames = wire_frames(&writer.inner.written);
        assert_eq!(frames.len(), ping_count + 2);
        assert!(frames[1..=ping_count]
            .iter()
            .all(|frame| *frame == (6, 1, &[4; 8][..])));
        assert_eq!(frames.last().unwrap(), &(6, 1, &[5; 8][..]));
    }

    #[test]
    fn ready_control_does_not_allocate_deferred_buffer() {
        let mut writer = writer(false);
        writer.send_control(frame::Ping::pong([6; 8]).into());
        assert!(!writer.encoder.has_control());
        assert!(writer.flush_with(usize::MAX));
        assert_eq!(
            wire_frames(&writer.inner.written),
            vec![(6, 1, &[6; 8][..])]
        );
    }

    #[test]
    fn deferred_controls_reuse_scratch_between_episodes() {
        let mut writer = writer(false);
        for byte in 0..3 {
            let start = writer.inner.written.len();
            writer.buffer(headers(1, 5)).unwrap();
            assert!(writer.flush_with(usize::MAX));
            assert!(!writer.encoder.scratch.is_empty());
            writer.inner.allowance = 0;
            let payload = Bytes::from(vec![byte; 4096]);
            writer.buffer(data(payload.clone())).unwrap();
            writer.send_control(frame::Ping::pong([byte; 8]).into());
            assert!(writer.flush_with(usize::MAX));
            assert!(writer.has_capacity());
            assert!(!writer.encoder.has_control());
            assert_eq!(writer.take_last_data_frame().unwrap().payload().len(), 0);
            writer.buffer(headers(3, 5)).unwrap();
            assert!(writer.flush_with(usize::MAX));
            let frames = wire_frames(&writer.inner.written[start..]);
            assert_eq!(frames.len(), 4);
            assert_eq!(frames[0].0, 1);
            assert_eq!(frames[1], (0, 0, payload.as_ref()));
            assert_eq!(frames[2], (6, 1, &[byte; 8][..]));
            assert_eq!(frames[3].0, 1);
        }
    }

    #[test]
    fn borrowed_data_preserves_drop_checking() {
        let mut writer = FramedWrite::new(TestIo::default());
        // The payload is intentionally declared after the writer.
        let payload = vec![42; 4096];
        writer
            .buffer(frame::Data::new(1.into(), payload.as_slice()).into())
            .unwrap();
    }

    #[test]
    fn deferred_frame_size_preserves_zero_and_maximum_limits() {
        for limit in [
            0,
            1,
            frame::DEFAULT_MAX_FRAME_SIZE,
            frame::MAX_MAX_FRAME_SIZE,
        ] {
            let mut writer = writer(false);
            writer.buffer(data(Bytes::from(vec![42; 4096]))).unwrap();
            writer.set_max_frame_size(limit as usize);
            writer.send_control(frame::Ping::pong([1; 8]).into());
            assert_eq!(writer.encoder.deferred_frame_size.unwrap().get() - 1, limit);
            writer.set_max_frame_size(frame::DEFAULT_MAX_FRAME_SIZE as usize);
            assert!(writer.flush_with(usize::MAX));
            assert!(writer.encoder.deferred_frame_size.is_none());
            assert!(writer.has_capacity());
            assert_eq!(wire_frames(&writer.inner.written).len(), 2);
        }
    }

    #[test]
    fn dropping_pending_writer_releases_data_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct Tracked {
            data: Bytes,
            drops: Arc<AtomicUsize>,
        }
        impl Buf for Tracked {
            fn remaining(&self) -> usize {
                self.data.remaining()
            }
            fn chunk(&self) -> &[u8] {
                self.data.chunk()
            }
            fn advance(&mut self, cnt: usize) {
                self.data.advance(cnt);
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        for (defer, allowance) in [(false, 0), (true, 0), (true, 4105)] {
            let drops = Arc::new(AtomicUsize::new(0));
            let mut writer = FramedWrite::new(TestIo::default());
            writer
                .buffer(
                    frame::Data::new(
                        1.into(),
                        Tracked {
                            data: Bytes::from(vec![42; 4096]),
                            drops: drops.clone(),
                        },
                    )
                    .into(),
                )
                .unwrap();
            if defer {
                writer.send_control(frame::Ping::pong([1; 8]).into());
            }
            assert!(!writer.flush_with(allowance));
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            drop(writer);
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }
}

#[cfg(feature = "unstable")]
mod unstable {
    use super::*;

    impl<T, B> FramedWrite<T, B> {
        pub fn get_ref(&self) -> &T {
            &self.inner
        }
    }
}
