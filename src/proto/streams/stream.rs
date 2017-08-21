use super::*;

#[derive(Debug)]
pub(super) struct Stream<B> {
    /// The h2 stream identifier
    pub id: StreamId,

    /// Current state of the stream
    pub state: State,

    /// Next node in the `Stream` linked list.
    ///
    /// This field is used in different linked lists depending on the stream
    /// state. First, it is used as part of the linked list of streams waiting
    /// to be accepted (either by a server or by a client as a push promise).
    /// Once the stream is accepted, this is used for the linked list of streams
    /// waiting to flush prioritized frames to the socket.
    pub next: Option<store::Key>,

    /// Set to true when the stream is queued
    pub is_queued: bool,

    // ===== Fields related to sending =====

    /// Send data flow control
    pub send_flow: FlowControl,

    /// Amount of send capacity that has been requested, but not yet allocated.
    pub requested_send_capacity: WindowSize,

    /// Amount of data buffered at the prioritization layer.
    pub buffered_send_data: WindowSize,

    /// Task tracking additional send capacity (i.e. window updates).
    pub send_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<B>,

    /// Next node in the linked list of streams waiting for additional
    /// connection level capacity.
    pub next_pending_send_capacity: Option<store::Key>,

    /// True if the stream is waiting for outbound connection capacity
    pub is_pending_send_capacity: bool,

    /// Set to true when the send capacity has been incremented
    pub send_capacity_inc: bool,

    // ===== Fields related to receiving =====

    /// Receive data flow control
    pub recv_flow: FlowControl,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<Bytes>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// The stream's pending push promises
    pub pending_push_promises: store::Queue<B, Next>,
}

#[derive(Debug)]
pub(super) struct Next;

#[derive(Debug)]
pub(super) struct NextSendCapacity;

impl<B> Stream<B> {
    pub fn new(id: StreamId) -> Stream<B>
    {
        Stream {
            id,
            state: State::default(),
            next: None,
            is_queued: false,

            // ===== Fields related to sending =====

            send_flow: FlowControl::new(),
            requested_send_capacity: 0,
            buffered_send_data: 0,
            send_task: None,
            pending_send: buffer::Deque::new(),
            is_pending_send_capacity: false,
            next_pending_send_capacity: None,
            send_capacity_inc: false,

            // ===== Fields related to receiving =====

            recv_flow: FlowControl::new(),
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            pending_push_promises: store::Queue::new(),
        }
    }

    pub fn assign_capacity(&mut self, capacity: WindowSize) {
        debug_assert!(capacity > 0);
        self.send_capacity_inc = true;
        self.send_flow.assign_capacity(capacity);

        // Only notify if the capacity exceeds the amount of buffered data
        if self.send_flow.available() > self.buffered_send_data {
            self.notify_send();
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

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_queued
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_queued = val;
    }
}

impl store::Next for NextSendCapacity {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_pending_send_capacity
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_pending_send_capacity = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_pending_send_capacity.take()
    }

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_pending_send_capacity
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_pending_send_capacity = val;
    }
}
