use h2;
use h2::frame::{self, Frame};

use futures::{Async, Stream, Poll};
use futures::task::{self, Task};

use tokio_io::{AsyncRead, AsyncWrite};

use std::{cmp, io};
use std::io::ErrorKind::WouldBlock;
use std::sync::{Arc, Mutex, Condvar};

/// A mock I/O
pub struct Mock {
    pipe: Pipe,
}

pub struct Handle {
    codec: ::Codec<Pipe>,
}

struct Pipe {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    condvar: Condvar,
}

struct State {
    rx: Vec<u8>,
    rx_task: Option<Task>,
    tx: Vec<u8>,
}

/// Create a new mock and handle
pub fn new() -> (Mock, Handle) {
    let inner = Arc::new(Inner {
        state: Mutex::new(State {
            rx: vec![],
            rx_task: None,
            tx: vec![],
        }),
        condvar: Condvar::new(),
    });

    let mock = Mock {
        pipe: Pipe {
            inner: inner.clone(),
        },
    };

    let handle = Handle {
        codec: h2::Codec::new(Pipe { inner }),
    };

    (mock, handle)
}

// ===== impl Handle =====

impl Handle {
    /// Receive the next frame
    pub fn recv(&mut self) -> Result<Option<h2::frame::Frame>, h2::RecvError> {
        match self.codec.poll()? {
            Async::Ready(frame) => Ok(frame),
            Async::NotReady => panic!(),
        }
    }

    /// Send a frame
    pub fn send(&mut self, item: ::Frame) -> Result<(), h2::SendError> {
        // Queue the frame
        self.codec.buffer(item).unwrap();

        // Flush the frame
        assert!(self.codec.flush()?.is_ready());

        Ok(())
    }

    pub fn write_preface(&mut self) {
        use std::io::Write;

        // Write the connnection preface
        self.codec.get_mut().write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").unwrap();
    }

    /// Perform the H2 handshake
    pub fn handshake_client(&mut self) -> frame::Settings {
        self.write_preface();

        // Send a settings frame
        let frame = frame::Settings::default();
        self.send(frame.into()).unwrap();

        // Read a settings frame
        let settings = match self.recv().unwrap() {
            Some(Frame::Settings(frame)) => frame,
            Some(frame) => {
                panic!("unexpected frame; frame={:?}", frame);
            }
            None => {
                panic!("unexpected EOF");
            }
        };

        let ack = frame::Settings::ack();
        self.send(ack.into()).unwrap();

        // TODO... recv ACK

        settings
    }
}

// ===== impl Mock =====

impl io::Read for Mock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(buf.len() > 0, "attempted read with zero length buffer... wut?");

        let mut me = self.pipe.inner.state.lock().unwrap();

        if me.rx.is_empty() {
            me.rx_task = Some(task::current());
            return Err(WouldBlock.into());
        }

        let n = cmp::min(buf.len(), me.rx.len());
        buf[..n].copy_from_slice(&me.rx[..n]);

        Ok(n)
    }
}

impl io::Write for Mock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut me = self.pipe.inner.state.lock().unwrap();

        me.tx.extend(buf);
        self.pipe.inner.condvar.notify_one();

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ===== impl Pipe =====

impl io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(buf.len() > 0, "attempted read with zero length buffer... wut?");

        let mut me = self.inner.state.lock().unwrap();

        while me.tx.is_empty() {
            me = self.inner.condvar.wait(me).unwrap();
        }

        let n = cmp::min(buf.len(), me.tx.len());
        buf[..n].copy_from_slice(&me.tx[..n]);

        Ok(n)
    }
}

impl AsyncRead for Pipe {
}

impl io::Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut me = self.inner.state.lock().unwrap();
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
