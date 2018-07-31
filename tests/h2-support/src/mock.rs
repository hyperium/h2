use {frames, FutureExt, SendFrame};

use h2::{self, RecvError, SendError};
use h2::frame::{self, Frame};

use futures::{Async, Future, Poll, Stream};
use futures::sync::oneshot;
use futures::task::{self, Task};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::io::read_exact;

use std::{cmp, fmt, io, usize};
use std::io::ErrorKind::WouldBlock;
use std::sync::{Arc, Mutex};

/// A mock I/O
#[derive(Debug)]
pub struct Mock {
    pipe: Pipe,
}

#[derive(Debug)]
pub struct Handle {
    codec: ::Codec<Pipe>,
}

#[derive(Debug)]
pub struct Pipe {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    /// Data written by the test case to the h2 lib.
    rx: Vec<u8>,

    /// Notify when data is ready to be received.
    rx_task: Option<Task>,

    /// Data written by the `h2` library to be read by the test case.
    tx: Vec<u8>,

    /// Notify when data is written. This notifies the test case waiters.
    tx_task: Option<Task>,

    /// Number of bytes that can be written before `write` returns `NotReady`.
    tx_rem: usize,

    /// Task to notify when write capacity becomes available.
    tx_rem_task: Option<Task>,

    /// True when the pipe is closed.
    closed: bool,
}

const PREFACE: &'static [u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Create a new mock and handle
pub fn new() -> (Mock, Handle) {
    new_with_write_capacity(usize::MAX)
}

/// Create a new mock and handle allowing up to `cap` bytes to be written.
pub fn new_with_write_capacity(cap: usize) -> (Mock, Handle) {
    let inner = Arc::new(Mutex::new(Inner {
        rx: vec![],
        rx_task: None,
        tx: vec![],
        tx_task: None,
        tx_rem: cap,
        tx_rem_task: None,
        closed: false,
    }));

    let mock = Mock {
        pipe: Pipe {
            inner: inner.clone(),
        },
    };

    let handle = Handle {
        codec: h2::Codec::new(Pipe {
            inner,
        }),
    };

    (mock, handle)
}

// ===== impl Handle =====

impl Handle {
    /// Get a mutable reference to inner Codec.
    pub fn codec_mut(&mut self) -> &mut ::Codec<Pipe> {
        &mut self.codec
    }

    /// Send a frame
    pub fn send(&mut self, item: SendFrame) -> Result<(), SendError> {
        // Queue the frame
        self.codec.buffer(item).unwrap();

        // Flush the frame
        assert!(self.codec.flush()?.is_ready());

        Ok(())
    }

    /// Writes the client preface
    pub fn write_preface(&mut self) {
        use std::io::Write;

        // Write the connnection preface
        self.codec.get_mut().write(PREFACE).unwrap();
    }

    /// Read the client preface
    pub fn read_preface(self) -> Box<Future<Item = Self, Error = io::Error>> {
        let buf = vec![0; PREFACE.len()];
        let ret = read_exact(self, buf).and_then(|(me, buf)| {
            assert_eq!(buf, PREFACE);
            Ok(me)
        });

        Box::new(ret)
    }

    /// Perform the H2 handshake
    pub fn assert_client_handshake(
        self,
    ) -> Box<Future<Item = (frame::Settings, Self), Error = h2::Error>> {
        self.assert_client_handshake_with_settings(frame::Settings::default())
    }

    /// Perform the H2 handshake
    pub fn assert_client_handshake_with_settings<T>(
        mut self,
        settings: T,
    ) -> Box<Future<Item = (frame::Settings, Self), Error = h2::Error>>
    where
        T: Into<frame::Settings>,
    {
        let settings = settings.into();
        // Send a settings frame
        self.send(settings.into()).unwrap();

        let ret = self.read_preface()
            .unwrap()
            .and_then(|me| me.into_future().unwrap())
            .map(|(frame, mut me)| {
                match frame {
                    Some(Frame::Settings(settings)) => {
                        // Send the ACK
                        let ack = frame::Settings::ack();

                        // TODO: Don't unwrap?
                        me.send(ack.into()).unwrap();

                        (settings, me)
                    },
                    Some(frame) => {
                        panic!("unexpected frame; frame={:?}", frame);
                    },
                    None => {
                        panic!("unexpected EOF");
                    },
                }
            })
            .then(|res| {
                let (settings, me) = res.unwrap();

                me.into_future()
                    .map_err(|_| unreachable!("all previous futures unwrapped"))
                    .map(|(frame, me)| {
                        let f = assert_settings!(frame.unwrap());

                        // Is ACK
                        assert!(f.is_ack());

                        (settings, me)
                    })
            });

        Box::new(ret)
    }


    /// Perform the H2 handshake
    pub fn assert_server_handshake(
        self,
    ) -> Box<Future<Item = (frame::Settings, Self), Error = h2::Error>> {
        self.assert_server_handshake_with_settings(frame::Settings::default())
    }

    /// Perform the H2 handshake
    pub fn assert_server_handshake_with_settings<T>(
        mut self,
        settings: T,
    ) -> Box<Future<Item = (frame::Settings, Self), Error = h2::Error>>
    where
        T: Into<frame::Settings>,
    {
        self.write_preface();

        let settings = settings.into();
        self.send(settings.into()).unwrap();

        let ret = self.into_future()
            .unwrap()
            .map(|(frame, mut me)| {
                match frame {
                    Some(Frame::Settings(settings)) => {
                        // Send the ACK
                        let ack = frame::Settings::ack();

                        // TODO: Don't unwrap?
                        me.send(ack.into()).unwrap();

                        (settings, me)
                    },
                    Some(frame) => {
                        panic!("unexpected frame; frame={:?}", frame);
                    },
                    None => {
                        panic!("unexpected EOF");
                    },
                }
            })
            .then(|res| {
                let (settings, me) = res.unwrap();

                me.into_future()
                    .map_err(|e| panic!("error: {:?}", e))
                    .map(|(frame, me)| {
                        let f = assert_settings!(frame.unwrap());

                        // Is ACK
                        assert!(f.is_ack());

                        (settings, me)
                    })
            });

        Box::new(ret)
    }
}

impl Stream for Handle {
    type Item = Frame;
    type Error = RecvError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, RecvError> {
        self.codec.poll()
    }
}

impl io::Read for Handle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.codec.get_mut().read(buf)
    }
}

impl AsyncRead for Handle {}

impl io::Write for Handle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.codec.get_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncWrite for Handle {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        use std::io::Write;
        try_nb!(self.flush());
        Ok(().into())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        assert!(self.codec.shutdown().unwrap().is_ready());

        let mut me = self.codec.get_mut().inner.lock().unwrap();
        me.closed = true;

        if let Some(task) = me.rx_task.take() {
            task.notify();
        }
    }
}

// ===== impl Mock =====

impl io::Read for Mock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(
            buf.len() > 0,
            "attempted read with zero length buffer... wut?"
        );

        let mut me = self.pipe.inner.lock().unwrap();

        if me.rx.is_empty() {
            if me.closed {
                return Ok(0);
            }

            me.rx_task = Some(task::current());
            return Err(WouldBlock.into());
        }

        let n = cmp::min(buf.len(), me.rx.len());
        buf[..n].copy_from_slice(&me.rx[..n]);
        me.rx.drain(..n);

        Ok(n)
    }
}

impl AsyncRead for Mock {}

impl io::Write for Mock {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let mut me = self.pipe.inner.lock().unwrap();

        if me.closed {
            return Ok(buf.len());
        }

        if me.tx_rem == 0 {
            me.tx_rem_task = Some(task::current());
            return Err(io::ErrorKind::WouldBlock.into());
        }

        if buf.len() > me.tx_rem {
            buf = &buf[..me.tx_rem];
        }

        me.tx.extend(buf);
        me.tx_rem -= buf.len();

        if let Some(task) = me.tx_task.take() {
            task.notify();
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncWrite for Mock {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        use std::io::Write;
        try_nb!(self.flush());
        Ok(().into())
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let mut me = self.pipe.inner.lock().unwrap();
        me.closed = true;

        if let Some(task) = me.tx_task.take() {
            task.notify();
        }
    }
}

// ===== impl Pipe =====

impl io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(
            buf.len() > 0,
            "attempted read with zero length buffer... wut?"
        );

        let mut me = self.inner.lock().unwrap();


        if me.tx.is_empty() {
            if me.closed {
                return Ok(0);
            }

            me.tx_task = Some(task::current());
            return Err(WouldBlock.into());
        }

        let n = cmp::min(buf.len(), me.tx.len());
        buf[..n].copy_from_slice(&me.tx[..n]);
        me.tx.drain(..n);

        Ok(n)
    }
}

impl AsyncRead for Pipe {}

impl io::Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut me = self.inner.lock().unwrap();
        me.rx.extend(buf);

        if let Some(task) = me.rx_task.take() {
            task.notify();
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncWrite for Pipe {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        use std::io::Write;
        try_nb!(self.flush());
        Ok(().into())
    }
}

pub trait HandleFutureExt {
    fn recv_settings(self)
        -> RecvFrame<Box<Future<Item = (Option<Frame>, Handle), Error = ()>>>
    where
        Self: Sized + 'static,
        Self: Future<Item = (frame::Settings, Handle)>,
        Self::Error: fmt::Debug,
    {
        self.recv_custom_settings(frame::Settings::default())
    }

    fn recv_custom_settings<T>(self, settings: T)
        -> RecvFrame<Box<Future<Item = (Option<Frame>, Handle), Error = ()>>>
    where
        Self: Sized + 'static,
        Self: Future<Item = (frame::Settings, Handle)>,
        Self::Error: fmt::Debug,
        T: Into<frame::Settings>,
    {
        let map = self
            .map(|(settings, handle)| (Some(settings.into()), handle))
            .unwrap();

        let boxed: Box<Future<Item = (Option<Frame>, Handle), Error = ()>> =
            Box::new(map);
        RecvFrame {
            inner: boxed,
            frame: Some(settings.into().into()),
        }
    }

    fn ignore_settings(self) -> Box<Future<Item = Handle, Error = ()>>
    where
        Self: Sized + 'static,
        Self: Future<Item = (frame::Settings, Handle)>,
        Self::Error: fmt::Debug,
    {
        Box::new(self.map(|(_settings, handle)| handle).unwrap())
    }

    fn recv_frame<T>(self, frame: T) -> RecvFrame<<Self as IntoRecvFrame>::Future>
    where
        Self: IntoRecvFrame + Sized,
        T: Into<Frame>,
    {
        self.into_recv_frame(Some(frame.into()))
    }

    fn recv_eof(self) -> RecvFrame<<Self as IntoRecvFrame>::Future>
    where
        Self: IntoRecvFrame + Sized,
    {
        self.into_recv_frame(None)
    }

    fn send_frame<T>(self, frame: T) -> SendFrameFut<Self>
    where
        Self: Sized,
        T: Into<SendFrame>,
    {
        SendFrameFut {
            inner: self,
            frame: Some(frame.into()),
        }
    }

    fn send_bytes(self, data: &[u8]) -> Box<Future<Item = Handle, Error = Self::Error>>
    where
        Self: Future<Item = Handle> + Sized + 'static,
        Self::Error: fmt::Debug,
    {
        use bytes::Buf;
        use futures::future::poll_fn;
        use std::io::Cursor;

        let buf: Vec<_> = data.into();
        let mut buf = Cursor::new(buf);

        Box::new(self.and_then(move |handle| {
            let mut handle = Some(handle);

            poll_fn(move || {
                while buf.has_remaining() {
                    let res = handle.as_mut().unwrap()
                        .codec.get_mut()
                        .write_buf(&mut buf)
                        .map_err(|e| panic!("write err={:?}", e));

                    try_ready!(res);
                }

                Ok(handle.take().unwrap().into())
            })
        }))
    }

    fn ping_pong(self, payload: [u8; 8]) -> RecvFrame<<SendFrameFut<Self> as IntoRecvFrame>::Future>
    where
        Self: Future<Item=Handle> + Sized + 'static,
        Self::Error: fmt::Debug,
    {
        self.send_frame(frames::ping(payload))
            .recv_frame(frames::ping(payload).pong())
    }

    fn idle_ms(self, ms: usize) -> Box<Future<Item = Handle, Error = Self::Error>>
    where
        Self: Sized + 'static,
        Self: Future<Item = Handle>,
        Self::Error: fmt::Debug,
    {
        use std::thread;
        use std::time::Duration;


        Box::new(self.and_then(move |handle| {
            // This is terrible... but oh well
            let (tx, rx) = oneshot::channel();

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(ms as u64));
                tx.send(()).unwrap();
            });

            Idle {
                handle: Some(handle),
                timeout: rx,
            }.map_err(|_| unreachable!())
        }))
    }

    fn buffer_bytes(self, num: usize) -> Box<Future<Item = Handle, Error = Self::Error>>
    where Self: Sized + 'static,
          Self: Future<Item = Handle>,
          Self::Error: fmt::Debug,
    {
        use futures::future::poll_fn;

        Box::new(self.and_then(move |mut handle| {
            // Set tx_rem to num
            {
                let mut i = handle.codec.get_mut().inner.lock().unwrap();
                i.tx_rem = num;
            }

            let mut handle = Some(handle);

            poll_fn(move || {
                {
                    let mut inner = handle.as_mut().unwrap()
                        .codec.get_mut().inner.lock().unwrap();

                    if inner.tx_rem == 0 {
                        inner.tx_rem = usize::MAX;
                    } else {
                        inner.tx_task = Some(task::current());
                        return Ok(Async::NotReady);
                    }
                }

                Ok(handle.take().unwrap().into())
            })
        }))
    }

    fn unbounded_bytes(self) -> Box<Future<Item = Handle, Error = Self::Error>>
    where Self: Sized + 'static,
          Self: Future<Item = Handle>,
          Self::Error: fmt::Debug,
    {
        Box::new(self.and_then(|mut handle| {
            {
                let mut i = handle.codec.get_mut().inner.lock().unwrap();
                i.tx_rem = usize::MAX;

                if let Some(task) = i.tx_rem_task.take() {
                    task.notify();
                }
            }

            Ok(handle.into())
        }))
    }

    fn then_notify(self, tx: oneshot::Sender<()>) -> Box<Future<Item = Handle, Error = Self::Error>>
    where Self: Sized + 'static,
          Self: Future<Item = Handle>,
          Self::Error: fmt::Debug,
    {
        Box::new(self.map(move |handle| {
            tx.send(()).unwrap();
            handle
        }))
    }

    fn wait_for<F>(self, other: F) -> Box<Future<Item = Self::Item, Error = Self::Error>>
    where
        F: Future + 'static,
        Self: Future + Sized + 'static
    {
        Box::new(self.then(move |result| {
            other.then(move |_| result)
        }))
    }

    fn close(self) -> Box<Future<Item = (), Error = ()>>
    where
        Self: Future<Error = ()> + Sized + 'static,
    {
        Box::new(self.map(drop))
    }
}

pub struct RecvFrame<T> {
    inner: T,
    frame: Option<Frame>,
}

impl<T> Future for RecvFrame<T>
where
    T: Future<Item = (Option<Frame>, Handle)>,
    T::Error: fmt::Debug,
{
    type Item = Handle;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        use self::Frame::Data;

        let (frame, handle) = match self.inner.poll().unwrap() {
            Async::Ready((frame, handle)) => (frame, handle),
            Async::NotReady => return Ok(Async::NotReady),
        };

        match (frame, &self.frame) {
            (Some(Data(ref a)), &Some(Data(ref b))) => {
                assert_eq!(a.payload().len(), b.payload().len(), "recv_frame data payload len");
                assert_eq!(a, b, "recv_frame");
            }
            (ref a, b) => {
                assert_eq!(a, b, "recv_frame");
            }
        }

        Ok(Async::Ready(handle))
    }
}

pub struct SendFrameFut<T> {
    inner: T,
    frame: Option<SendFrame>,
}

impl<T> Future for SendFrameFut<T>
where
    T: Future<Item = Handle>,
    T::Error: fmt::Debug,
{
    type Item = Handle;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut handle = match self.inner.poll().unwrap() {
            Async::Ready(handle) => handle,
            Async::NotReady => return Ok(Async::NotReady),
        };
        handle.send(self.frame.take().unwrap()).unwrap();
        Ok(Async::Ready(handle))
    }
}

pub struct Idle {
    handle: Option<Handle>,
    timeout: oneshot::Receiver<()>,
}

impl Future for Idle {
    type Item = Handle;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.timeout.poll().unwrap().is_ready() {
            return Ok(self.handle.take().unwrap().into());
        }

        match self.handle.as_mut().unwrap().poll() {
            Ok(Async::NotReady) => Ok(Async::NotReady),
            res => {
                panic!("Idle received unexpected frame on handle; frame={:?}", res);
            },
        }
    }
}

impl<T> HandleFutureExt for T
where
    T: Future + 'static,
{
}

pub trait IntoRecvFrame {
    type Future: Future;
    fn into_recv_frame(self, frame: Option<Frame>) -> RecvFrame<Self::Future>;
}

impl IntoRecvFrame for Handle {
    type Future = ::futures::stream::StreamFuture<Self>;

    fn into_recv_frame(self, frame: Option<Frame>) -> RecvFrame<Self::Future> {
        RecvFrame {
            inner: self.into_future(),
            frame: frame,
        }
    }
}

impl<T> IntoRecvFrame for T
where
    T: Future<Item = Handle> + 'static,
    T::Error: fmt::Debug,
{
    type Future = Box<Future<Item = (Option<Frame>, Handle), Error = ()>>;

    fn into_recv_frame(self, frame: Option<Frame>) -> RecvFrame<Self::Future> {
        let into_fut = Box::new(
            self.unwrap()
                .and_then(|handle| handle.into_future().unwrap()),
        );
        RecvFrame {
            inner: into_fut,
            frame: frame,
        }
    }
}
