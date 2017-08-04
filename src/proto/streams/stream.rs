use super::*;

#[derive(Debug)]
pub(super) struct Stream<B> {
    /// Current state of the stream
    pub state: State,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<Bytes>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<B>,

    /// Next stream pending send
    pub next_pending_send: Option<store::Key>,

    /// True if the stream is currently pending send
    pub is_pending_send: bool,
}

impl<B> Stream<B> {
    pub fn new() -> Stream<B> {
        Stream {
            state: State::default(),
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            pending_send: buffer::Deque::new(),
            next_pending_send: None,
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
