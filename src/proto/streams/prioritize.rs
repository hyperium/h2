use super::*;

#[derive(Debug)]
pub(super) struct Prioritize<B> {
    pending_send: store::List<B>,

    /// Holds frames that are waiting to be written to the socket
    buffer: Buffer<B>,
}

impl<B> Prioritize<B>
    where B: Buf,
{
    pub fn new() -> Prioritize<B> {
        Prioritize {
            pending_send: store::List::new(),
            buffer: Buffer::new(),
        }
    }

    pub fn queue_frame(&mut self,
                       frame: Frame<B>,
                       stream: &mut store::Ptr<B>)
    {
        // queue the frame in the buffer
        stream.pending_send.push_back(&mut self.buffer, frame);

        if stream.is_pending_send {
            debug_assert!(!self.pending_send.is_empty());

            // Already queued to have frame processed.
            return;
        }

        // Queue the stream
        self.push_sender(stream);
    }

    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        loop {
            // Ensure codec is ready
            try_ready!(dst.poll_ready());

            match self.pop_frame(store) {
                Some(frame) => {
                    // TODO: data frames should be handled specially...
                    let res = dst.start_send(frame)?;

                    // We already verified that `dst` is ready to accept the
                    // write
                    assert!(res.is_ready());
                }
                None => break,
            }
        }

        Ok(().into())
    }

    fn pop_frame(&mut self, store: &mut Store<B>) -> Option<Frame<B>> {
        match self.pop_sender(store) {
            Some(mut stream) => {
                let frame = stream.pending_send.pop_front(&mut self.buffer).unwrap();

                if !stream.pending_send.is_empty() {
                    self.push_sender(&mut stream);
                }

                Some(frame)
            }
            None => None,
        }
    }

    fn push_sender(&mut self, stream: &mut store::Ptr<B>) {
        debug_assert!(!stream.is_pending_send);

        self.pending_send.push(stream);

        stream.is_pending_send = true;
    }

    fn pop_sender<'a>(&mut self, store: &'a mut Store<B>) -> Option<store::Ptr<'a, B>> {
        match self.pending_send.pop(store) {
            Some(mut stream) => {
                stream.is_pending_send = false;
                Some(stream)
            }
            None => None,
        }
    }
}
