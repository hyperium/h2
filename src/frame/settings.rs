use FrameSize;
use frame::{Frame, Error, Head, Kind, StreamId};
use bytes::{BytesMut, BufMut, BigEndian};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Settings {
    flags: SettingsFlags,
    // Fields
    values: SettingSet,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct SettingSet {
    header_table_size: Option<u32>,
    enable_push: Option<u32>,
    max_concurrent_streams: Option<u32>,
    initial_window_size: Option<u32>,
    max_frame_size: Option<u32>,
    max_header_list_size: Option<u32>,
}

impl SettingSet {
    pub fn enable_push(&self) -> Option<bool> {
        self.enable_push.map(|n| n != 0)
    }

    pub fn initial_window_size(&self) -> Option<u32> {
        self.initial_window_size
    }

    pub fn max_concurrent_streams(&self) -> Option<u32> {
        self.max_concurrent_streams
    }
}

/// An enum that lists all valid settings that can be sent in a SETTINGS
/// frame.
///
/// Each setting has a value that is a 32 bit unsigned integer (6.5.1.).
#[derive(Debug)]
pub enum Setting {
    HeaderTableSize(u32),
    EnablePush(u32),
    MaxConcurrentStreams(u32),
    InitialWindowSize(u32),
    MaxFrameSize(u32),
    MaxHeaderListSize(u32),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct SettingsFlags(u8);

const ACK: u8 = 0x1;
const ALL: u8 = ACK;

pub const DEFAULT_SETTINGS_HEADER_TABLE_SIZE: usize = 4_096;
pub const DEFAULT_MAX_FRAME_SIZE: FrameSize = 16_384;

// ===== impl Settings =====

impl Settings {
    pub fn ack() -> Settings {
        Settings {
            flags: SettingsFlags::ack(),
            .. Settings::default()
        }
    }

    pub fn new(values: SettingSet) -> Settings {
        Settings {
            flags: SettingsFlags::empty(),
            values: values,
        }
    }

    pub fn is_ack(&self) -> bool {
        self.flags.is_ack()
    }

    pub fn into_set(self) -> SettingSet {
        self.values
    }

    pub fn load(head: Head, payload: &[u8]) -> Result<Settings, Error> {
        use self::Setting::*;

        debug_assert_eq!(head.kind(), ::frame::Kind::Settings);

        if !head.stream_id().is_zero() {
            return Err(Error::InvalidStreamId);
        }

        // Load the flag
        let flag = SettingsFlags::load(head.flag());

        if flag.is_ack() {
            // Ensure that the payload is empty
            if payload.len() > 0 {
                return Err(Error::InvalidPayloadLength);
            }

            // Return the ACK frame
            return Ok(Settings::ack());
        }

        // Ensure the payload length is correct, each setting is 6 bytes long.
        if payload.len() % 6 != 0 {
            debug!("invalid settings payload length; len={:?}", payload.len());
            return Err(Error::InvalidPayloadAckSettings);
        }

        let mut settings = Settings::default();
        debug_assert!(!settings.flags.is_ack());

        for raw in payload.chunks(6) {
            match Setting::load(raw) {
                Some(HeaderTableSize(val)) => {
                    settings.values.header_table_size = Some(val);
                }
                Some(EnablePush(val)) => {
                    settings.values.enable_push = Some(val);
                }
                Some(MaxConcurrentStreams(val)) => {
                    settings.values.max_concurrent_streams = Some(val);
                }
                Some(InitialWindowSize(val)) => {
                    settings.values.initial_window_size = Some(val);
                }
                Some(MaxFrameSize(val)) => {
                    settings.values.max_frame_size = Some(val);
                }
                Some(MaxHeaderListSize(val)) => {
                    settings.values.max_header_list_size = Some(val);
                }
                None => {}
            }
        }

        Ok(settings)
    }

    fn payload_len(&self) -> usize {
        let mut len = 0;
        self.for_each(|_| len += 6);
        len
    }

    pub fn encode(&self, dst: &mut BytesMut) {
        // Create & encode an appropriate frame head
        let head = Head::new(Kind::Settings, self.flags.into(), StreamId::zero());
        let payload_len = self.payload_len();

        trace!("encoding SETTINGS; len={}", payload_len);

        head.encode(payload_len, dst);

        // Encode the settings
        self.for_each(|setting| {
            trace!("encoding setting; val={:?}", setting);
            setting.encode(dst)
        });
    }

    fn for_each<F: FnMut(Setting)>(&self, mut f: F) {
        use self::Setting::*;

        if let Some(v) = self.values.header_table_size {
            f(HeaderTableSize(v));
        }

        if let Some(v) = self.values.enable_push {
            f(EnablePush(v));
        }

        if let Some(v) = self.values.max_concurrent_streams {
            f(MaxConcurrentStreams(v));
        }

        if let Some(v) = self.values.initial_window_size {
            f(InitialWindowSize(v));
        }

        if let Some(v) = self.values.max_frame_size {
            f(MaxFrameSize(v));
        }

        if let Some(v) = self.values.max_header_list_size {
            f(MaxHeaderListSize(v));
        }
    }
}

impl<T> From<Settings> for Frame<T> {
    fn from(src: Settings) -> Frame<T> {
        Frame::Settings(src)
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

    fn encode(&self, dst: &mut BytesMut) {
        use self::Setting::*;

        let (kind, val) = match *self {
            HeaderTableSize(v) => (1, v),
            EnablePush(v) => (2, v),
            MaxConcurrentStreams(v) => (3, v),
            InitialWindowSize(v) => (4, v),
            MaxFrameSize(v) => (5, v),
            MaxHeaderListSize(v) => (6, v),
        };

        dst.put_u16::<BigEndian>(kind);
        dst.put_u32::<BigEndian>(val);
    }
}

// ===== impl SettingsFlags =====

impl SettingsFlags {
    pub fn empty() -> SettingsFlags {
        SettingsFlags(0)
    }

    pub fn load(bits: u8) -> SettingsFlags {
        SettingsFlags(bits & ALL)
    }

    pub fn ack() -> SettingsFlags {
        SettingsFlags(ACK)
    }

    pub fn is_ack(&self) -> bool {
        self.0 & ACK == ACK
    }
}

impl From<SettingsFlags> for u8 {
    fn from(src: SettingsFlags) -> u8 {
        src.0
    }
}
