use super::*;

#[derive(Debug)]
pub(super) struct Prioritize<B> {
    pending_send: Option<Indices>,

    /// Holds frames that are waiting to be written to the socket
    buffer: Buffer<B>,
}

#[derive(Debug, Clone, Copy)]
struct Indices {
    head: store::Key,
    tail: store::Key,
}

impl<B> Prioritize<B>
    where B: Buf,
{
    pub fn new() -> Prioritize<B> {
        Prioritize {
            pending_send: None,
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
            debug_assert!(self.pending_send.is_some());

            // Already queued to have frame processed.
            return;
        }

        // The next pointer shouldn't be set
        debug_assert!(stream.next_pending_send.is_none());

        // Queue the stream
        match self.pending_send {
            Some(ref mut idxs) => {
                // Update the current tail node to point to `stream`
                stream.resolve(idxs.tail).next_pending_send = Some(stream.key());

                // Update the tail pointer
                idxs.tail = stream.key();
            }
            None => {
                self.pending_send = Some(Indices {
                    head: stream.key(),
                    tail: stream.key(),
                });
            }
        }

        stream.is_pending_send = true;
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
        unimplemented!();
    }
}
