use ConnectionError;
use proto::*;

pub trait ControlPing {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError>;
    fn take_pong(&mut self) -> Option<PingPayload>;
}

macro_rules! proxy_control_ping {
    ($outer:ident) => (
        impl<T: ControlPing> ControlPing for $outer<T> {
            fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
                self.inner.start_ping(body)
            }

            fn take_pong(&mut self) -> Option<PingPayload> {
                self.inner.take_pong()
            }
        }
    )
}
