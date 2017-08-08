use super::*;

#[derive(Debug)]
pub(super) struct Stream<B> {
    /// The h2 stream identifier
    pub id: StreamId,

    /// Current state of the stream
    pub state: State,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<Bytes>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<B>,

    /// Next node in the `Stream` linked list.
    ///
    /// This field is used in different linked lists depending on the stream
    /// state.
    pub next: Option<store::Key>,

    /// The stream's pending push promises
    pub pending_push_promises: store::List<B>,

    /// True if the stream is currently pending send
    pub is_pending_send: bool,
}

impl<B> Stream<B> {
    pub fn new(id: StreamId) -> Stream<B> {
        Stream {
            id,
            state: State::default(),
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            pending_send: buffer::Deque::new(),
            next: None,
            pending_push_promises: store::List::new(),
            is_pending_send: false,
        }
    }

    pub fn send_flow_control(&mut self) -> Option<&mut FlowControl> {
        self.state.send_flow_control()
    }

    pub fn recv_flow_control(&mut self) -> Option<&mut FlowControl> {
        self.state.recv_flow_control()
    }

    pub fn notify_recv(&mut self) {
        if let Some(ref mut task) = self.recv_task {
            task.notify();
        }
    }
}
