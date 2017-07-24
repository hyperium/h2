use ConnectionError;
use frame::SettingSet;

/// Allows settings updates to be pushed "down" the transport (i.e. from Settings down to
/// FramedWrite).
pub trait ApplySettings {
    fn apply_local_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError>;
    fn apply_remote_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError>;
}

macro_rules! proxy_apply_settings {
    ($struct:ident $(, $targs:ident)*) => (
        impl<T: ApplySettings$(, $targs)*> ApplySettings for $struct<T$(, $targs)*> {
            fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
                self.inner.apply_local_settings(set)
            }

            fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
                self.inner.apply_remote_settings(set)
            }
        }
    )
}
