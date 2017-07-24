use ConnectionError;
use frame::SettingSet;
use proto::*;

/// Exposes settings to "upper" layers of the transport (i.e. from Settings up to---and
/// above---Connection).
pub trait ControlSettings {
    fn update_local_settings(&mut self, set: SettingSet) -> Result<(), ConnectionError>;

    fn remote_push_enabled(&self) -> Option<bool>;
    fn remote_max_concurrent_streams(&self) -> Option<u32>;
    fn remote_initial_window_size(&self) -> WindowSize;
}
