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

    /// A stream's capacity is never advertised past the connection's capacity.
    /// This value represents the amount of the stream window that has been
    /// temporarily withheld.
    pub unadvertised_send_window: WindowSize,
}

#[derive(Debug)]
pub(super) struct Next;

#[derive(Debug)]
pub(super) struct NextCapacity;

impl<B> Stream<B> {
    pub fn new(id: StreamId) -> Stream<B> {
        Stream {
            id,
            state: State::default(),
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            send_task: None,
            pending_send: buffer::Deque::new(),
            next: None,
            next_capacity: None,
            is_pending_send_capacity: false,
            pending_push_promises: store::List::new(),
            is_pending_send: false,
            unadvertised_send_window: 0,
        }
    }

    // TODO: remove?
    pub fn send_flow_control(&mut self) -> Option<&mut FlowControl> {
        self.state.send_flow_control()
    }

    // TODO: remove?
    pub fn recv_flow_control(&mut self) -> Option<&mut FlowControl> {
        self.state.recv_flow_control()
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

    fn set_next<B>(stream: &mut Stream<B>, key: store::Key) {
        debug_assert!(stream.next.is_none());
        stream.next = Some(key);
    }

    fn take_next<B>(stream: &mut Stream<B>) -> store::Key {
        stream.next.take().unwrap()
    }
}

impl store::Next for NextCapacity {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_capacity
    }

    fn set_next<B>(stream: &mut Stream<B>, key: store::Key) {
        debug_assert!(stream.next_capacity.is_none());
        stream.next_capacity = Some(key);
    }

    fn take_next<B>(stream: &mut Stream<B>) -> store::Key {
        stream.next_capacity.take().unwrap()
    }
}
