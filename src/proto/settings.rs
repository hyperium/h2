use proto::*;
use frame::{self, SettingSet};

#[derive(Debug)]
pub struct Settings {
    remote_push_enabled: Option<bool>,
    remote_max_concurrent_streams: Option<u32>,
    remote_initial_window_size: WindowSize,

    // Number of acks remaining to send to the peer
    remaining_acks: usize,

    // Holds a new set of local values to be applied.
    pending_local: Option<SettingSet>,

    // True when we have received a settings frame from the remote.
    received_remote: bool,
}

impl Settings {
    pub fn recv_settings(&mut self, frame: frame::Settings) {
        if frame.is_ack() {
            debug!("received remote settings ack");
            // TODO: handle acks
        } else {
            unimplemented!();
            // self.remaining_acks += 1;
        }
    }
}
