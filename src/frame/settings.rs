use std::fmt;

use crate::frame::{util, Error, Frame, FrameSize, Head, Kind, StreamId};
use bytes::{BufMut, BytesMut};

/// How many settings one frame holds, the numerically lowest being kept.
///
/// Eight covers every identifier this crate defines; twenty leaves a dozen for
/// extensions. 6.5.2 permits ignoring what is not understood, and what is
/// dropped always sorts above the twenty lowest, never one this crate acts on.
pub(crate) const MAX_SETTINGS: usize = 20;

/// One slot per identifier this crate defines, so extensions a caller adds can
/// never crowd out a setting [`Settings::set`] must hold. What is left for
/// [`Settings::set`] to give is twelve.
const RESERVED: usize = SETTINGS_ENABLE_CONNECT_PROTOCOL as usize;

#[derive(Clone, Default, Eq, PartialEq)]
pub struct Settings {
    flags: SettingsFlags,
    len: u8,
    /// The identifiers held, ascending; `ids[..len]` is live.
    ///
    /// Ordered by identifier, not by the order a caller set them in, so a
    /// frame's bytes are a function of the values it carries.
    ids: [u16; MAX_SETTINGS],
    vals: [u32; MAX_SETTINGS],
}

/// The flags a SETTINGS frame can carry: 6.5 defines exactly one.
///
/// Two states rather than eight bits, which leaves 254 unused bit patterns for
/// `Option<Settings>` to put its `None` in, so the pending frame a
/// connection holds stays the size of a frame.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SettingsFlags {
    #[default]
    Empty,
    Ack,
}

const ACK: u8 = 0x1;

/// The default value of SETTINGS_HEADER_TABLE_SIZE
pub const DEFAULT_SETTINGS_HEADER_TABLE_SIZE: usize = 4_096;

/// The default value of SETTINGS_INITIAL_WINDOW_SIZE
pub const DEFAULT_INITIAL_WINDOW_SIZE: u32 = 65_535;

/// The default value of MAX_FRAME_SIZE
pub const DEFAULT_MAX_FRAME_SIZE: FrameSize = 16_384;

/// INITIAL_WINDOW_SIZE upper bound
pub const MAX_INITIAL_WINDOW_SIZE: usize = (1 << 31) - 1;

/// MAX_FRAME_SIZE upper bound
pub const MAX_MAX_FRAME_SIZE: FrameSize = (1 << 24) - 1;

// The identifiers this crate defines, from RFC 9113 6.5.2 and RFC 8441 3, so
// that each number is written once.
const SETTINGS_HEADER_TABLE_SIZE: u16 = 0x1;
const SETTINGS_ENABLE_PUSH: u16 = 0x2;
const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;
const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 0x6;
const SETTINGS_ENABLE_CONNECT_PROTOCOL: u16 = 0x8;

// ===== impl Settings =====

impl Settings {
    pub fn ack() -> Settings {
        Settings {
            flags: SettingsFlags::Ack,
            ..Settings::default()
        }
    }

    pub fn is_ack(&self) -> bool {
        self.flags.is_ack()
    }

    pub fn initial_window_size(&self) -> Option<u32> {
        self.get(SETTINGS_INITIAL_WINDOW_SIZE)
    }

    pub fn set_initial_window_size(&mut self, size: Option<u32>) {
        self.set(SETTINGS_INITIAL_WINDOW_SIZE, size)
    }

    pub fn max_concurrent_streams(&self) -> Option<u32> {
        self.get(SETTINGS_MAX_CONCURRENT_STREAMS)
    }

    pub fn set_max_concurrent_streams(&mut self, max: Option<u32>) {
        self.set(SETTINGS_MAX_CONCURRENT_STREAMS, max)
    }

    pub fn max_frame_size(&self) -> Option<u32> {
        self.get(SETTINGS_MAX_FRAME_SIZE)
    }

    pub fn set_max_frame_size(&mut self, size: Option<u32>) {
        self.set(SETTINGS_MAX_FRAME_SIZE, size)
    }

    pub fn max_header_list_size(&self) -> Option<u32> {
        self.get(SETTINGS_MAX_HEADER_LIST_SIZE)
    }

    pub fn set_max_header_list_size(&mut self, size: Option<u32>) {
        self.set(SETTINGS_MAX_HEADER_LIST_SIZE, size)
    }

    pub fn is_push_enabled(&self) -> Option<bool> {
        self.get(SETTINGS_ENABLE_PUSH).map(|val| val != 0)
    }

    pub fn set_enable_push(&mut self, enable: bool) {
        self.set(SETTINGS_ENABLE_PUSH, Some(enable as u32))
    }

    pub fn is_extended_connect_protocol_enabled(&self) -> Option<bool> {
        self.get(SETTINGS_ENABLE_CONNECT_PROTOCOL)
            .map(|val| val != 0)
    }

    pub fn set_enable_connect_protocol(&mut self, val: Option<u32>) {
        self.set(SETTINGS_ENABLE_CONNECT_PROTOCOL, val)
    }

    pub fn header_table_size(&self) -> Option<u32> {
        self.get(SETTINGS_HEADER_TABLE_SIZE)
    }

    pub fn set_header_table_size(&mut self, size: Option<u32>) {
        self.set(SETTINGS_HEADER_TABLE_SIZE, size)
    }

    pub fn get(&self, id: u16) -> Option<u32> {
        self.position(id).ok().map(|i| self.vals[i])
    }

    /// How many identifiers held are not ones this crate defines.
    fn extensions(&self) -> usize {
        self.ids[..self.len as usize]
            .iter()
            .filter(|id| **id > SETTINGS_ENABLE_CONNECT_PROTOCOL)
            .count()
    }

    ///
    /// # Panics
    ///
    /// On a `MAX_FRAME_SIZE` value outside the range specified by HTTP/2
    /// §6.5.2. With debug assertions on, also on an identifier this crate does
    /// not define when twelve such are already set and `id` is not among them;
    /// without them that setting is dropped and logged. Nothing already set is
    /// displaced either way.
    ///
    /// Peer SETTINGS go through [`Settings::load`], which validates fallibly
    /// and never panics.
    pub fn set(&mut self, id: u16, val: Option<u32>) {
        let Some(val) = val else {
            self.remove(id);
            return;
        };

        bounds(id, val);

        let full = id > SETTINGS_ENABLE_CONNECT_PROTOCOL
            && self.position(id).is_err()
            && self.extensions() >= MAX_SETTINGS - RESERVED;
        debug_assert!(
            !full,
            "a SETTINGS frame carries {} extension identifiers here, so \
             0x{id:04x} would not be sent",
            MAX_SETTINGS - RESERVED,
        );
        if full {
            tracing::debug!("settings full; not sending id=0x{:04x}", id);
            return;
        }

        self.insert(id, val);
    }

    /// Takes every value held by `src`, replacing any held for the same
    /// identifier here.
    pub fn merge(&mut self, src: &Settings) {
        for (id, val) in src.entries() {
            self.insert(id, val);
        }
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        let len = self.len as usize;
        self.ids[..len]
            .iter()
            .copied()
            .zip(self.vals[..len].iter().copied())
    }

    fn position(&self, id: u16) -> Result<usize, usize> {
        self.ids[..self.len as usize].binary_search(&id)
    }

    /// Holds `val` for `id`, keeping the numerically lowest identifiers when
    /// there is no room for them all.
    ///
    /// Best effort by design: this is the receive path, and 6.5.2 lets an
    /// endpoint ignore an identifier it does not understand. What it may never
    /// do is lose one it does, which keeping the lowest guarantees.
    fn insert(&mut self, id: u16, val: u32) {
        let len = self.len as usize;

        match self.position(id) {
            // 6.5.3: "Each parameter in a SETTINGS frame replaces any existing
            // value for that parameter."
            Ok(i) => self.vals[i] = val,
            Err(i) if len < MAX_SETTINGS => self.insert_at(i, id, val),
            Err(i) if i < MAX_SETTINGS => {
                self.len -= 1;
                self.insert_at(i, id, val);
            }
            Err(_) => {
                tracing::debug!("settings full; not retaining id=0x{:04x}", id);
            }
        }
    }

    /// Writes `(id, val)` at `i`, shifting the rest up. `self.len` must be
    /// below [`MAX_SETTINGS`].
    fn insert_at(&mut self, i: usize, id: u16, val: u32) {
        let len = self.len as usize;
        debug_assert!(len < MAX_SETTINGS);

        self.ids.copy_within(i..len, i + 1);
        self.vals.copy_within(i..len, i + 1);
        self.ids[i] = id;
        self.vals[i] = val;
        self.len += 1;
    }

    fn remove(&mut self, id: u16) {
        if let Ok(i) = self.position(id) {
            let len = self.len as usize;
            self.ids.copy_within(i + 1..len, i);
            self.vals.copy_within(i + 1..len, i);
            self.len -= 1;
        }
    }

    pub fn load(head: Head, payload: &[u8]) -> Result<Settings, Error> {
        debug_assert_eq!(head.kind(), crate::frame::Kind::Settings);

        if !head.stream_id().is_zero() {
            return Err(Error::InvalidStreamId);
        }

        // Load the flag
        let flag = SettingsFlags::load(head.flag());

        if flag.is_ack() {
            // Ensure that the payload is empty
            if !payload.is_empty() {
                return Err(Error::InvalidPayloadLength);
            }

            // Return the ACK frame
            return Ok(Settings::ack());
        }

        // Ensure the payload length is correct, each setting is 6 bytes long.
        if payload.len() % 6 != 0 {
            tracing::debug!("invalid settings payload length; len={:?}", payload.len());
            return Err(Error::InvalidPayloadAckSettings);
        }

        let mut settings = Settings::default();
        debug_assert!(!settings.flags.is_ack());

        for raw in payload.chunks(6) {
            let id: u16 = (u16::from(raw[0]) << 8) | u16::from(raw[1]);
            let val: u32 = unpack_octets_4!(raw, 2, u32);

            // 0x0 is Reserved in the registry, so it names nothing; 6.5.2 says
            // to ignore an identifier we do not understand, and this is one.
            if id == 0 {
                continue;
            }

            validate(id, val)?;
            settings.insert(id, val);
        }

        Ok(settings)
    }

    fn payload_len(&self) -> usize {
        self.len as usize * 6
    }

    pub fn encode(&self, dst: &mut BytesMut) {
        // Create & encode an appropriate frame head
        let head = Head::new(Kind::Settings, self.flags.into(), StreamId::zero());
        let payload_len = self.payload_len();

        tracing::trace!("encoding SETTINGS; len={}", payload_len);

        head.encode(payload_len, dst);

        for (id, val) in self.entries() {
            tracing::trace!("encoding setting; id=0x{:04x} val={}", id, val);
            dst.put_u16(id);
            dst.put_u32(val);
        }
    }
}

/// The bound a caller's own value must satisfy, which only one setting has.
fn bounds(id: u16, val: u32) {
    if id == SETTINGS_MAX_FRAME_SIZE {
        assert!(DEFAULT_MAX_FRAME_SIZE <= val && val <= MAX_MAX_FRAME_SIZE);
    }
}

/// The bounds RFC 9113 6.5.2 states for the settings this crate defines.
///
/// An identifier it does not define has no bound to apply here: the endpoint
/// implementing the extension that defines it is the one that can see whether
/// it asked for it.
fn validate(id: u16, val: u32) -> Result<(), Error> {
    match id {
        SETTINGS_ENABLE_PUSH | SETTINGS_ENABLE_CONNECT_PROTOCOL if val > 1 => {
            Err(Error::InvalidSettingValue)
        }
        SETTINGS_INITIAL_WINDOW_SIZE if val as usize > MAX_INITIAL_WINDOW_SIZE => {
            Err(Error::InvalidSettingValue)
        }
        SETTINGS_MAX_FRAME_SIZE
            if !(DEFAULT_MAX_FRAME_SIZE..=MAX_MAX_FRAME_SIZE).contains(&val) =>
        {
            Err(Error::InvalidSettingValue)
        }
        _ => Ok(()),
    }
}

/// The name RFC 9113 and RFC 8441 give an identifier, for `Debug`.
fn name(id: u16) -> Option<&'static str> {
    match id {
        SETTINGS_HEADER_TABLE_SIZE => Some("header_table_size"),
        SETTINGS_ENABLE_PUSH => Some("enable_push"),
        SETTINGS_MAX_CONCURRENT_STREAMS => Some("max_concurrent_streams"),
        SETTINGS_INITIAL_WINDOW_SIZE => Some("initial_window_size"),
        SETTINGS_MAX_FRAME_SIZE => Some("max_frame_size"),
        SETTINGS_MAX_HEADER_LIST_SIZE => Some("max_header_list_size"),
        SETTINGS_ENABLE_CONNECT_PROTOCOL => Some("enable_connect_protocol"),
        _ => None,
    }
}

impl<T> From<Settings> for Frame<T> {
    fn from(src: Settings) -> Frame<T> {
        Frame::Settings(src)
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut builder = f.debug_struct("Settings");
        builder.field("flags", &self.flags);

        for (id, val) in self.entries() {
            match name(id) {
                Some(name) => builder.field(name, &val),
                None => builder.field(&format!("0x{id:04x}"), &val),
            };
        }

        builder.finish()
    }
}

// ===== impl SettingsFlags =====

impl SettingsFlags {
    fn load(bits: u8) -> SettingsFlags {
        if bits & ACK == ACK {
            SettingsFlags::Ack
        } else {
            SettingsFlags::Empty
        }
    }

    fn is_ack(&self) -> bool {
        matches!(self, SettingsFlags::Ack)
    }
}

impl From<SettingsFlags> for u8 {
    fn from(src: SettingsFlags) -> u8 {
        match src {
            SettingsFlags::Empty => 0,
            SettingsFlags::Ack => ACK,
        }
    }
}

impl fmt::Debug for SettingsFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        util::debug_flags(f, u8::from(*self))
            .flag_if(self.is_ack(), "ACK")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload of `settings`, without the nine byte frame head.
    fn payload(settings: &Settings) -> Vec<u8> {
        let mut dst = BytesMut::new();
        settings.encode(&mut dst);
        dst[9..].to_vec()
    }

    /// A SETTINGS payload carrying each `(identifier, value)` pair in order.
    fn raw(pairs: &[(u16, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (id, val) in pairs {
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&val.to_be_bytes());
        }
        payload
    }

    fn load(pairs: &[(u16, u32)]) -> Settings {
        let head = Head::new(Kind::Settings, 0, StreamId::zero());
        Settings::load(head, &raw(pairs)).expect("a well formed SETTINGS payload")
    }

    #[test]
    fn an_undefined_identifier_encodes_as_it_was_set() {
        let mut settings = Settings::default();
        settings.set(0x9, Some(1));

        assert_eq!(payload(&settings), raw(&[(0x9, 1)]));
    }

    #[test]
    fn an_undefined_identifier_loads_as_it_arrived() {
        assert_eq!(load(&[(0x9, 7)]).get(0x9), Some(7));
    }

    /// The encoded order is the identifier order, not the order values were
    /// set in; otherwise the mock io tests, which compare frames byte for
    /// byte, would depend on the order a caller happened to use.
    #[test]
    fn undefined_identifiers_encode_in_identifier_order() {
        let mut descending = Settings::default();
        descending.set(0x4d45, Some(4));
        descending.set(0x4d44, Some(2));
        descending.set(0x9, Some(1));

        let mut ascending = Settings::default();
        ascending.set(0x9, Some(1));
        ascending.set(0x4d44, Some(2));
        ascending.set(0x4d45, Some(4));

        assert_eq!(
            payload(&descending),
            raw(&[(0x9, 1), (0x4d44, 2), (0x4d45, 4)]),
        );
        assert_eq!(payload(&descending), payload(&ascending));
        assert_eq!(descending, ascending, "and the two compare equal");
    }

    /// RFC 9113 6.5.3: "Each parameter in a SETTINGS frame replaces any
    /// existing value for that parameter."
    #[test]
    fn a_duplicate_undefined_identifier_takes_the_last_value() {
        assert_eq!(load(&[(0x9, 1), (0x9, 2)]).get(0x9), Some(2));

        let mut settings = Settings::default();
        settings.set(0x9, Some(1));
        settings.set(0x9, Some(2));
        assert_eq!(payload(&settings), raw(&[(0x9, 2)]));
    }

    /// One identifier occupies one slot however it is addressed: the named
    /// setter and `set` reach the same place, so the payload carries `0x4`
    /// once, with the value written last.
    #[test]
    fn one_identifier_occupies_one_slot() {
        let mut settings = Settings::default();
        settings.set_initial_window_size(Some(1_000));
        settings.set(4, Some(2_000));

        assert_eq!(settings.initial_window_size(), Some(2_000));
        assert_eq!(settings.get(4), Some(2_000));
        assert_eq!(payload(&settings), raw(&[(4, 2_000)]));

        settings.set_initial_window_size(Some(3_000));
        assert_eq!(settings.get(4), Some(3_000));
    }

    /// `set` is exactly as strict as the setter it routes to, which for
    /// `MAX_FRAME_SIZE` means refusing a value outside the legal range.
    #[test]
    #[should_panic]
    fn a_defined_identifier_set_generically_keeps_its_own_bounds() {
        Settings::default().set(5, Some(1));
    }

    #[test]
    fn merge_takes_the_later_value_for_every_identifier() {
        let mut settings = load(&[(0x9, 1), (0x10, 2), (4, 1_000)]);
        settings.merge(&load(&[(0x10, 3), (0x4d44, 4), (4, 2_000)]));

        assert_eq!(settings.get(0x9), Some(1), "untouched");
        assert_eq!(settings.get(0x10), Some(3), "replaced");
        assert_eq!(settings.get(0x4d44), Some(4), "added");
        assert_eq!(settings.get(4), Some(2_000), "replaced, in its own field");
    }

    /// Outbound, a full frame refuses a new identifier rather than displacing
    /// one already configured. Inbound prefers the lowest identifier; a sender
    /// has no such licence, every identifier it holds having been asked for.
    #[test]
    fn a_full_frame_refuses_rather_than_displace_what_was_configured() {
        let mut settings = Settings::default();
        for i in 0..(MAX_SETTINGS - RESERVED) as u16 {
            settings.set(0x200 - i, Some(1));
        }

        settings.set(0x200, Some(2));
        assert_eq!(
            settings.get(0x200),
            Some(2),
            "replacing one already held needs no room",
        );
    }

    /// The capacity bound. 6.5.2 obliges an endpoint to ignore an identifier
    /// it does not understand, and past the twentieth every identifier is one
    /// of those: seventeen exist, ten registered and seven drafted. So the
    /// excess is dropped rather than growing the frame or the connection.
    #[test]
    fn a_frame_beyond_capacity_ignores_the_excess() {
        let over: Vec<(u16, u32)> = (0..MAX_SETTINGS as u32 + 5)
            .map(|i| (0x1000 + i as u16, i))
            .collect();

        let settings = load(&over);

        assert_eq!(settings.entries().count(), MAX_SETTINGS);
        assert_eq!(settings.get(0x1000), Some(0), "the first is kept");
        assert_eq!(
            settings.get(0x1000 + MAX_SETTINGS as u16 - 1),
            Some(MAX_SETTINGS as u32 - 1),
            "and so is the last that fits",
        );
        assert_eq!(
            settings.get(0x1000 + MAX_SETTINGS as u16),
            None,
            "the next is not"
        );
        assert_eq!(
            payload(&settings).len(),
            MAX_SETTINGS * 6,
            "nor is it emitted"
        );
    }

    /// `None` means "send nothing for this", which the named setters took to
    /// mean clearing the field. With one store it has to mean removing the
    /// entry, including from the middle of the sorted run.
    #[test]
    fn clearing_a_setting_removes_it() {
        let mut settings = Settings::default();
        settings.set_header_table_size(Some(1));
        settings.set_initial_window_size(Some(2));
        settings.set_max_header_list_size(Some(3));

        settings.set_initial_window_size(None);

        assert_eq!(settings.initial_window_size(), None);
        assert_eq!(
            payload(&settings),
            raw(&[(1, 1), (6, 3)]),
            "and the rest keep order"
        );

        settings.set_header_table_size(None);
        settings.set_max_header_list_size(None);
        assert!(payload(&settings).is_empty());
    }

    /// `MAX_SETTINGS` high identifiers, then every identifier this crate defines.
    /// Each must still be applied: a peer cannot suppress a setting h2 acts on
    /// by padding the frame ahead of it.
    #[test]
    fn a_full_frame_still_admits_every_identifier_this_crate_defines() {
        let mut pairs: Vec<(u16, u32)> = (0..MAX_SETTINGS as u32)
            .map(|i| (0x1000 + i as u16, i))
            .collect();
        for id in [1u16, 2, 3, 4, 5, 6, 8] {
            pairs.push((
                id,
                if id == SETTINGS_MAX_FRAME_SIZE {
                    16_384
                } else {
                    1
                },
            ));
        }

        let settings = load(&pairs);

        assert_eq!(settings.header_table_size(), Some(1));
        assert_eq!(settings.is_push_enabled(), Some(true));
        assert_eq!(settings.max_concurrent_streams(), Some(1));
        assert_eq!(settings.initial_window_size(), Some(1));
        assert_eq!(settings.max_frame_size(), Some(16_384));
        assert_eq!(settings.max_header_list_size(), Some(1));
        assert_eq!(settings.is_extended_connect_protocol_enabled(), Some(true));
    }

    /// The same, with `0x7` -- unassigned, so this crate does not define it --
    /// taking the eighth of the low slots. All eight are below everything else,
    /// so all eight are kept.
    #[test]
    fn the_unassigned_low_identifier_crowds_nothing_out() {
        let mut pairs: Vec<(u16, u32)> = (0..MAX_SETTINGS as u32)
            .map(|i| (0x1000 + i as u16, i))
            .collect();
        pairs.push((0x7, 7));
        for id in [1u16, 2, 3, 4, 5, 6, 8] {
            pairs.push((
                id,
                if id == SETTINGS_MAX_FRAME_SIZE {
                    16_384
                } else {
                    1
                },
            ));
        }

        let settings = load(&pairs);

        assert_eq!(settings.get(0x7), Some(7));
        assert_eq!(settings.initial_window_size(), Some(1));
        assert_eq!(settings.is_extended_connect_protocol_enabled(), Some(true));
    }

    /// What is kept is the numerically lowest, not the earliest: a low
    /// identifier arriving last displaces the highest one held.
    #[test]
    fn the_lowest_identifiers_are_the_ones_retained() {
        let mut pairs: Vec<(u16, u32)> = (0..MAX_SETTINGS as u32)
            .map(|i| (0x2000 + i as u16, i))
            .collect();
        pairs.push((0x1000, 99));

        let settings = load(&pairs);

        assert_eq!(settings.get(0x1000), Some(99), "the lowest is kept");
        assert_eq!(
            settings.get(0x2000 + MAX_SETTINGS as u16 - 1),
            None,
            "and the highest it displaced is gone",
        );
        assert_eq!(settings.entries().count(), MAX_SETTINGS);
        let ids: Vec<u16> = settings.entries().map(|(id, _)| id).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "still ascending");
    }

    /// 6.5.3 holds across the overflow: a repeated identifier takes its last
    /// value even when the frame is full.
    #[test]
    fn a_duplicate_takes_its_last_value_in_a_full_frame() {
        let mut pairs: Vec<(u16, u32)> = (0..MAX_SETTINGS as u32)
            .map(|i| (0x1000 + i as u16, i))
            .collect();
        pairs.push((SETTINGS_INITIAL_WINDOW_SIZE, 1));
        pairs.push((SETTINGS_INITIAL_WINDOW_SIZE, 2));
        pairs.push((0x1000, 1));
        pairs.push((0x1000, 2));

        let settings = load(&pairs);

        assert_eq!(settings.initial_window_size(), Some(2));
        assert_eq!(settings.get(0x1000), Some(2));
    }

    /// Outbound, dropping is never right: a caller asked for the setting to be
    /// sent. `set` panics instead of quietly omitting it.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "would not be sent")]
    fn a_setting_that_would_not_be_sent_panics() {
        let mut settings = Settings::default();
        for i in 0..(MAX_SETTINGS - RESERVED) as u32 {
            settings.set(0x1000 + i as u16, Some(i));
        }
        settings.set(0x9000, Some(1));
    }

    /// The release half of the case above, which panics under the debug
    /// assertions `cargo test` builds with.
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_setting_that_would_not_be_sent_is_dropped() {
        let mut settings = Settings::default();
        for i in 0..(MAX_SETTINGS - RESERVED) as u32 {
            settings.set(0x1000 + i as u16, Some(i));
        }
        // Below everything held, the ordering that would displace one already
        // configured if a full frame were allowed to insert.
        settings.set(0x999, Some(1));

        assert_eq!(settings.get(0x999), None, "the one that would not be sent");
        let highest = 0x1000 + (MAX_SETTINGS - RESERVED) as u16 - 1;
        assert_eq!(
            settings.get(highest),
            Some((MAX_SETTINGS - RESERVED) as u32 - 1),
            "and nothing already configured displaced",
        );
    }

    /// And the reservation is what keeps that panic away from the identifiers
    /// this crate defines, however many extensions a caller added.
    #[test]
    fn a_defined_identifier_always_has_room() {
        let mut settings = Settings::default();
        for i in 0..(MAX_SETTINGS - RESERVED) as u32 {
            settings.set(0x1000 + i as u16, Some(i));
        }

        settings.set(SETTINGS_INITIAL_WINDOW_SIZE, Some(1_000));
        assert_eq!(settings.initial_window_size(), Some(1_000));
        assert_eq!(
            settings.get(0x1000 + (MAX_SETTINGS - RESERVED) as u16 - 1),
            Some(MAX_SETTINGS as u32 - RESERVED as u32 - 1),
            "and it displaced none of them",
        );
    }
}
