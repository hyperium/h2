use frame::{Error, Head};
use bytes::Bytes;

#[derive(Debug, Clone, Default)]
pub struct Settings {
    flag: SettingsFlag,
    // Fields
    header_table_size: Option<u32>,
    enable_push: Option<bool>,
    max_concurrent_streams: Option<u32>,
    initial_window_size: Option<u32>,
    max_frame_size: Option<u32>,
    max_header_list_size: Option<u32>,
}

/// An enum that lists all valid settings that can be sent in a SETTINGS
/// frame.
///
/// Each setting has a value that is a 32 bit unsigned integer (6.5.1.).
pub enum Setting {
    HeaderTableSize(u32),
    EnablePush(u32),
    MaxConcurrentStreams(u32),
    InitialWindowSize(u32),
    MaxFrameSize(u32),
    MaxHeaderListSize(u32),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct SettingsFlag(u8);

const ACK: u8 = 0x1;
const ALL: u8 = ACK;

// ===== impl Settings =====

impl Settings {
    pub fn load(head: Head, payload: Bytes) -> Result<Settings, Error> {
        use self::Setting::*;

        debug_assert_eq!(head.kind(), ::frame::Kind::Settings);

        if head.stream_id() != 0 {
            // TODO: raise ProtocolError
            unimplemented!();
        }

        // Load the flag
        let flag = SettingsFlag::load(head.flag());

        if flag.is_ack() {
            // Ensure that the payload is empty
            if payload.len() > 0 {
                // TODO: raise a FRAME_SIZE_ERROR
                unimplemented!();
            }

            // Return the ACK frame
            return Ok(Settings {
                flag: flag,
                .. Settings::default()
            });
        }

        // Ensure the payload length is correct, each setting is 6 bytes long.
        if payload.len() % 6 != 0 {
            return Err(Error::PartialSettingLength);
        }

        let mut settings = Settings::default();
        debug_assert!(!settings.flag.is_ack());

        for raw in payload.chunks(6) {
            match Setting::load(raw) {
                Some(HeaderTableSize(val)) => {
                    settings.header_table_size = Some(val);
                }
                Some(EnablePush(val)) => {
                    settings.enable_push = Some(val == 1);
                }
                Some(MaxConcurrentStreams(val)) => {
                    settings.max_concurrent_streams = Some(val);
                }
                Some(InitialWindowSize(val)) => {
                    settings.initial_window_size = Some(val);
                }
                Some(MaxFrameSize(val)) => {
                    settings.max_frame_size = Some(val);
                }
                Some(MaxHeaderListSize(val)) => {
                    settings.max_header_list_size = Some(val);
                }
                None => {}
            }
        }

        Ok(settings)
    }
}

// ===== impl Setting =====

impl Setting {
    /// Creates a new `Setting` with the correct variant corresponding to the
    /// given setting id, based on the settings IDs defined in section
    /// 6.5.2.
    pub fn from_id(id: u16, val: u32) -> Option<Setting> {
        use self::Setting::*;

        match id {
            1 => Some(HeaderTableSize(val)),
            2 => Some(EnablePush(val)),
            3 => Some(MaxConcurrentStreams(val)),
            4 => Some(InitialWindowSize(val)),
            5 => Some(MaxFrameSize(val)),
            6 => Some(MaxHeaderListSize(val)),
            _ => None,
        }
    }

    /// Creates a new `Setting` by parsing the given buffer of 6 bytes, which
    /// contains the raw byte representation of the setting, according to the
    /// "SETTINGS format" defined in section 6.5.1.
    ///
    /// The `raw` parameter should have length at least 6 bytes, since the
    /// length of the raw setting is exactly 6 bytes.
    ///
    /// # Panics
    ///
    /// If given a buffer shorter than 6 bytes, the function will panic.
    fn load(raw: &[u8]) -> Option<Setting> {
        let id: u16 = ((raw[0] as u16) << 8) | (raw[1] as u16);
        let val: u32 = unpack_octets_4!(raw, 2, u32);

        Setting::from_id(id, val)
    }
}

// ===== impl SettingsFlag =====

impl SettingsFlag {
    pub fn load(bits: u8) -> SettingsFlag {
        SettingsFlag(bits & ALL)
    }

    pub fn ack() -> SettingsFlag {
        SettingsFlag(ACK)
    }

    pub fn is_ack(&self) -> bool {
        self.0 & ACK == ACK
    }
}
