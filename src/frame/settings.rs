use frame::{Error, Head, Kind};
use bytes::{Bytes, BytesMut, BufMut, BigEndian};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
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
            return Err(Error::InvalidStreamId);
        }

        // Load the flag
        let flag = SettingsFlag::load(head.flag());

        if flag.is_ack() {
            // Ensure that the payload is empty
            if payload.len() > 0 {
                return Err(Error::InvalidPayloadLength);
            }

            // Return the ACK frame
            return Ok(Settings {
                flag: flag,
                .. Settings::default()
            });
        }

        // Ensure the payload length is correct, each setting is 6 bytes long.
        if payload.len() % 6 != 0 {
            return Err(Error::InvalidPayloadAckSettings);
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

    pub fn encode_len(&self) -> usize {
        super::FRAME_HEADER_LEN + self.payload_len()
    }

    fn payload_len(&self) -> usize {
        let mut len = 0;
        self.for_each(|_| len += 6);
        len
    }

    pub fn encode(&self, dst: &mut BytesMut) -> Result<(), Error> {
        // Create & encode an appropriate frame head
        let head = Head::new(Kind::Settings, self.flag.into(), 0);
        let payload_len = self.payload_len();

        try!(head.encode(payload_len, dst));

        // Encode the settings
        self.for_each(|setting| setting.encode(dst));

        Ok(())
    }

    fn for_each<F: FnMut(Setting)>(&self, mut f: F) {
        use self::Setting::*;

        if let Some(v) = self.header_table_size {
            f(HeaderTableSize(v));
        }

        if let Some(v) = self.enable_push {
            f(EnablePush(if v { 1 } else { 0 }));
        }

        if let Some(v) = self.max_concurrent_streams {
            f(MaxConcurrentStreams(v));
        }

        if let Some(v) = self.initial_window_size {
            f(InitialWindowSize(v));
        }

        if let Some(v) = self.max_frame_size {
            f(MaxFrameSize(v));
        }

        if let Some(v) = self.max_header_list_size {
            f(MaxHeaderListSize(v));
        }
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

impl From<SettingsFlag> for u8 {
    fn from(src: SettingsFlag) -> u8 {
        src.0
    }
}
