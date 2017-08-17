use super::*;

#[derive(Debug)]
pub(super) struct Stream<B> {
    /// The h2 stream identifier
    pub id: StreamId,

    /// Current state of the stream
    pub state: State,

    /// Send data flow control
    pub send_flow: FlowControl,

    /// Receive data flow control
    pub recv_flow: FlowControl,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<Bytes>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// Task tracking additional send capacity (i.e. window updates).
    pub send_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<B>,

    /// Next node in the `Stream` linked list.
    ///
    /// This field is used in different linked lists depending on the stream
    /// state.
    pub next: Option<store::Key>,

    /// Next node in the linked list of streams waiting for additional
    /// connection level capacity.
    pub next_capacity: Option<store::Key>,

    /// True if the stream is waiting for outbound connection capacity
    pub is_pending_send_capacity: bool,

    /// The stream's pending push promises
    pub pending_push_promises: store::List<B>,

    /// True if the stream is currently pending send
    pub is_pending_send: bool,
}

#[derive(Debug)]
pub(super) struct Next;

#[derive(Debug)]
pub(super) struct NextCapacity;

impl<B> Stream<B> {
    pub fn new(id: StreamId) -> Stream<B>
    {
        Stream {
            id,
            state: State::default(),
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            send_task: None,
            recv_flow: FlowControl::new(),
            send_flow: FlowControl::new(),
            pending_send: buffer::Deque::new(),
            next: None,
            next_capacity: None,
            is_pending_send_capacity: false,
            pending_push_promises: store::List::new(),
            is_pending_send: false,
        }
    }

    pub fn notify_send(&mut self) {
        if let Some(task) = self.send_task.take() {
            task.notify();
        }
    }

    pub fn notify_recv(&mut self) {
        if let Some(task) = self.recv_task.take() {
            task.notify();
        }
    }
}

impl store::Next for Next {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next.take()
    }
}

impl store::Next for NextCapacity {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_capacity
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_capacity = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_capacity.take()
    }
}
