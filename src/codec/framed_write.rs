use crate::codec::UserError;
use crate::codec::UserError::*;
use crate::frame::{self, Frame, FrameSize};
use crate::hpack;

use bytes::{Buf, BufMut, BytesMut};
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use std::io::{self, Cursor, IoSlice};
use std::ops::ControlFlow;

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
    hpack: hpack::Encoder,

    /// Write buffer
    ///
    /// TODO: Should this be a ring buffer?
    buf: Cursor<BytesMut>,

    /// Vector of buffer data and data frames to send next
    next_vec: VecDeque<BufElement<B>>,

    /// Next continuation frame to encode
    next_continuation: Option<frame::Continuation>,

    /// Max frame size, this is specified by the peer
    max_frame_size: FrameSize,

    /// Chain payloads bigger than this.
    chain_threshold: usize,

    /// Min buffer required to attempt to write a frame
    min_buffer_capacity: usize,
}

#[derive(Debug)]
struct BufElement<B> {
    /// Number of bytes in the buffer that should be written before the next data frame payload.
    buf_len: usize,
    /// Data frame of the payload that should be written as part of this buffer element.
    data_frame: frame::Data<B>,
}

/// Initialize the connection with this amount of write buffer.
///
/// The minimum MAX_FRAME_SIZE is 16kb, so always be able to send a HEADERS
/// frame that big.
const DEFAULT_BUFFER_CAPACITY: usize = 16 * 1_024;

/// Chain payloads bigger than this when vectored I/O is enabled. The remote
/// will never advertise a max frame size less than this (well, the spec says
/// the max frame size can't be less than 16kb, so not even close).
const CHAIN_THRESHOLD: usize = 256;

/// Chain payloads bigger than this when vectored I/O is **not** enabled.
/// A larger value in this scenario will reduce the number of small and
/// fragmented data being sent, and hereby improve the throughput.
const CHAIN_THRESHOLD_WITHOUT_VECTORED_IO: usize = 1024;

const MAX_VECTORED_IO_COUNT: usize = 1024;

// TODO: Make generic
impl<T, B> FramedWrite<T, B>
where
    T: AsyncWrite + Unpin,
    B: Buf,
{
    pub fn new(inner: T) -> FramedWrite<T, B> {
        let (chain_threshold, next_vec_capacity) = if inner.is_write_vectored() {
            (CHAIN_THRESHOLD, MAX_VECTORED_IO_COUNT / 2)
        } else {
            (CHAIN_THRESHOLD_WITHOUT_VECTORED_IO, 1)
        };
        FramedWrite {
            inner,
            final_flush_done: false,
            encoder: Encoder {
                hpack: hpack::Encoder::default(),
                buf: Cursor::new(BytesMut::with_capacity(DEFAULT_BUFFER_CAPACITY)),
                next_vec: VecDeque::with_capacity(next_vec_capacity),
                next_continuation: None,
                max_frame_size: frame::DEFAULT_MAX_FRAME_SIZE,
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

    /// Buffer a frame.
    ///
    /// `poll_ready` must be called first to ensure that a frame may be
    /// accepted.
    pub fn buffer(&mut self, item: Frame<B>) -> Result<(), UserError> {
        self.encoder.buffer(item)
    }

    /// Flush buffered data to the wire
    pub fn flush(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        let span = tracing::trace_span!("FramedWrite::flush");
        let _e = span.enter();

        loop {
            while ready!(poll_write_buf(
                Pin::new(&mut self.inner),
                cx,
                &mut self.encoder
            ))?
            .is_continue()
            {}

            self.encoder.reclaim_empty_buffer();

            if let Some(frame) = self.encoder.next_continuation.take() {
                let mut buf = limited_write_buf!(self.encoder);
                self.encoder.next_continuation = frame.encode(&mut buf);
            } else {
                break;
            }
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

impl<B> Encoder<B>
where
    B: Buf,
{
    fn reclaim_empty_buffer(&mut self) {
        assert_eq!(self.buf.position(), 0);
        assert_eq!(self.buf.remaining(), 0);
        let buf = self.buf.get_mut();
        let _ = buf.try_reclaim(buf.capacity() + 1);
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
                let mut buf_len_to_push = 0;
                if len >= self.chain_threshold {
                    let head = v.head();

                    // Encode the frame head to the buffer
                    head.encode(len, self.buf.get_mut());

                    if self.buf.get_ref().remaining() < self.chain_threshold {
                        let extra_bytes = self.chain_threshold - self.buf.remaining();
                        self.buf.get_mut().put(v.payload_mut().take(extra_bytes));
                    }

                    buf_len_to_push = self.buf.remaining();
                    self.buf.advance(buf_len_to_push);
                } else {
                    v.encode_chunk(self.buf.get_mut());

                    // The chunk has been fully encoded, so there is no need to
                    // keep it around
                    assert_eq!(v.payload().remaining(), 0, "chunk not fully encoded");
                }

                // Push the most recent data frame...
                self.next_vec.push_back(BufElement {
                    buf_len: buf_len_to_push,
                    data_frame: v,
                });
            }
            Frame::Headers(v) => {
                let mut buf = limited_write_buf!(self);
                self.next_continuation = v.encode(&mut self.hpack, &mut buf);
            }
            Frame::PushPromise(v) => {
                let mut buf = limited_write_buf!(self);
                self.next_continuation = v.encode(&mut self.hpack, &mut buf);
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
        self.next_continuation.is_none()
            && self.next_vec.len() < self.next_vec.capacity()
            && (self.buf.get_ref().capacity() - self.buf.get_ref().len()
                >= self.min_buffer_capacity)
    }
}

impl<B> Encoder<B> {
    fn max_frame_size(&self) -> usize {
        self.max_frame_size as usize
    }
}

impl<B: Buf> Buf for Encoder<B> {
    fn remaining(&self) -> usize {
        let mut n = self.buf.get_ref().len();
        for next in self.next_vec.iter() {
            n = n.saturating_add(next.data_frame.payload().remaining());
        }
        n
    }

    fn chunk(&self) -> &[u8] {
        for next in self.next_vec.iter() {
            if next.buf_len > 0 {
                return &self.buf.get_ref()[..next.buf_len];
            }
            let slice = next.data_frame.payload().chunk();
            if slice.len() > 0 {
                return slice;
            }
        }
        return &*self.buf.get_ref();
    }

    fn advance(&mut self, mut n: usize) {
        for next in self.next_vec.iter_mut() {
            if next.buf_len > 0 {
                let i = n.min(next.buf_len);
                self.buf.get_mut().advance(i);
                self.buf
                    .set_position(self.buf.position().checked_sub(i as u64).unwrap());
                n -= i;
                next.buf_len -= i;
                if next.buf_len > 0 {
                    return;
                }
            }
            let rem = next.data_frame.payload().remaining();
            if rem > 0 {
                let i = n.min(rem);
                n -= i;
                next.data_frame.payload_mut().advance(i);
                if i < rem {
                    return;
                }
            }
        }
        if n > 0 {
            self.buf.get_mut().advance(n);
        }
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [IoSlice<'a>]) -> usize {
        let mut n = 0;
        let mut buf_index = 0;
        let mut vec_iter = self.next_vec.iter();
        while n < dst.len() {
            if let Some(next) = vec_iter.next() {
                if next.buf_len > 0 {
                    let buf_end = buf_index + next.buf_len;
                    dst[n] = IoSlice::new(&self.buf.get_ref()[buf_index..buf_end]);
                    buf_index = buf_end;
                    n += 1;
                    if n == dst.len() {
                        break;
                    }
                }
                let mut rem = next.data_frame.payload().remaining();
                if rem > 0 {
                    let n0 = n;
                    n = n.wrapping_add(next.data_frame.payload().chunks_vectored(&mut dst[n..]));
                    assert!(n0 <= n && n <= dst.len());
                    if rem < usize::MAX {
                        for s in &dst[n0..n] {
                            rem = rem.saturating_sub(s.len());
                        }
                    }
                    if rem > 0 {
                        break;
                    }
                }
                assert!(n <= dst.len());
            } else {
                if self.buf.get_ref().len() > buf_index {
                    dst[n] = IoSlice::new(&self.buf.get_ref()[buf_index..]);
                    n += 1;
                }
                break;
            }
        }
        n
    }

    fn has_remaining(&self) -> bool {
        if self.buf.get_ref().len() > 0 {
            return true;
        }
        for next in self.next_vec.iter() {
            if next.data_frame.payload().has_remaining() {
                return true;
            }
        }
        false
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

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T, B: Buf> FramedWrite<T, B> {
    /// Take back data frames that have been buffered and/or fully written.
    pub fn take_used_data_frames(&mut self) -> impl Iterator<Item = frame::Data<B>> + '_ {
        UsedDataFrameTaker {
            vec: &mut self.encoder.next_vec,
            index: 0,
        }
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

#[cfg(feature = "unstable")]
mod unstable {
    use super::*;

    impl<T, B> FramedWrite<T, B> {
        pub fn get_ref(&self) -> &T {
            &self.inner
        }
    }
}

fn poll_write_buf<T: AsyncWrite + ?Sized, B: Buf>(
    io: Pin<&mut T>,
    cx: &mut Context<'_>,
    buf: &mut B,
) -> Poll<io::Result<ControlFlow<usize, usize>>> {
    let n = if io.is_write_vectored() {
        let mut slices = [IoSlice::new(&[]); MAX_VECTORED_IO_COUNT];
        let cnt = buf.chunks_vectored(&mut slices);
        if cnt == 0 {
            return Poll::Ready(Ok(ControlFlow::Break(0)));
        }
        ready!(io.poll_write_vectored(cx, &slices[..cnt]))?
    } else {
        let slice = buf.chunk();
        if slice.len() == 0 {
            return Poll::Ready(Ok(ControlFlow::Break(0)));
        }
        ready!(io.poll_write(cx, slice))?
    };

    buf.advance(n);

    Poll::Ready(Ok(ControlFlow::Continue(n)))
}

struct UsedDataFrameTaker<'a, B> {
    vec: &'a mut VecDeque<BufElement<B>>,
    index: usize,
}

impl<'a, B: Buf> Iterator for UsedDataFrameTaker<'a, B> {
    type Item = frame::Data<B>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(item) = self.vec.get(self.index) {
            if item.buf_len == 0 && !item.data_frame.payload().has_remaining() {
                return self.vec.remove(self.index).map(|x| x.data_frame);
            }
            self.index += 1;
        }
        None
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.vec.len()))
    }
}
