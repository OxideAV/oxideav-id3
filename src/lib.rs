// Per-frame parsing is a long dispatch; clippy prefers short fns but
// breaking this up only obfuscates the spec reference.
#![allow(clippy::needless_range_loop)]

//! ID3v1 and ID3v2 (2.2 / 2.3 / 2.4) tag parser + writer.
//!
//! The crate parses existing tags into a structured [`Id3Tag`] and can
//! serialise an [`Id3Tag`] back to bytes as either ID3v2.3 or ID3v2.4,
//! or an ID3v1/1.1 128-byte trailer. The typical consumer is a
//! container crate (oxideav-mp4, oxideav-flac, oxideav-mp3) that
//! extracts or strips the tag to hand the remaining bytes off to its
//! payload parser.
//!
//! The public surface:
//!
//! * [`parse_tag`] — take a `&[u8]` that starts with the 10-byte ID3v2
//!   header and return an [`Id3Tag`] plus the number of bytes consumed
//!   (so callers can seek past the tag and resume normal file reads).
//! * [`parse_id3v1`] — take the last 128 bytes of a file and, if they
//!   start with `TAG`, return the v1 tag.
//! * [`tag_size_at_head`] — peek at the first 10 bytes to work out the
//!   total on-disk tag size without parsing frames.
//! * [`to_key_value_pairs`] — normalise an [`Id3Tag`] into the
//!   Vorbis-comment-style `(key, value)` pairs the rest of the workspace
//!   uses (`title`, `artist`, `album`, `date`, ...).
//! * [`attached_pictures`] — pull the `APIC` / `PIC` frames out of a tag.
//! * [`write_tag`] — serialise an [`Id3Tag`] to the ID3v2.3 or 2.4 wire
//!   format. Unknown / v2.2 frames are pass-through (their raw payload
//!   is written verbatim under a promoted 4-char id).
//! * [`write_id3v1`] — serialise an [`Id3Tag`] as a 128-byte ID3v1/1.1
//!   trailer, pulling the standard text/comment/track/genre fields out
//!   of the tag's frames.
//!
//! Unsynchronisation (`0xFF 0x00` → `0xFF`) is reversed at the right
//! level for each version (whole-tag in 2.2/2.3, per-frame in 2.4), and
//! the v2.4 data-length indicator is honoured so tools that set it see
//! their real payload length.
//!
//! The extended header (spec §3.2 in both v2.3 and v2.4) is decoded
//! rather than skipped: a stored CRC-32 [ISO-3309] is verified against
//! the spec-defined region (frames-only for v2.3; frames + padding for
//! v2.4) and a mismatch is a hard parse error. `WriteOptions::with_crc`
//! emits a CRC-bearing extended header on the writer side; it composes
//! with the existing unsync modes via a round-trip-stable layering.
//!
//! Frames this parser knows about structurally:
//!
//! * `T***` text frames (v2.3/2.4) and their v2.2 equivalents (3-char ids).
//! * `TXXX` user-defined text.
//! * `W***` URL frames and `WXXX` user-defined URL.
//! * `COMM` comments and `USLT` lyrics.
//! * `APIC` attached pictures (v2.3/2.4) and `PIC` (v2.2).
//! * `POPM` popularimeter (email + rating + play counter).
//! * `PCNT` play counter.
//! * `PRIV` private frame (owner id + opaque bytes).
//! * `GEOB` general encapsulated object.
//! * `UFID` unique file identifier.
//! * `USER` terms-of-use frame.
//! * `OWNE` ownership / `COMR` commercial.
//! * `SYTC` synchronised tempo codes.
//! * `RVA2` / `EQU2` relative volume + equalisation (v2.4).
//! * `MCDI` music CD identifier (TOC).
//! * `ETCO` event timing codes / `POSS` position synchronisation.
//! * `SYLT` synchronised lyrics/text.
//! * `RBUF` recommended buffer size.
//! * `SEEK` seek frame / `SIGN` signature frame.
//! * `GRID` group identification registration.
//! * `ENCR` encryption method registration.
//! * `AENC` audio encryption / `LINK` linked information.
//! * `ASPI` audio seek point index (v2.4).
//! * `MLLT` MPEG location lookup table.
//!
//! Everything else lands in [`Id3Frame::Unknown`] with the raw payload
//! preserved so future code can extend recognition without reparsing.

use oxideav_core::{AttachedPicture, Error, PictureType, Result};

pub const ID3V2_HEADER_SIZE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id3Version {
    V2_2,
    V2_3,
    V2_4,
    V1,
}

#[derive(Clone, Debug)]
pub struct Id3Tag {
    pub version: Id3Version,
    pub frames: Vec<Id3Frame>,
}

#[derive(Clone, Debug)]
pub enum Id3Frame {
    /// Text-information frame (`T***` except `TXXX`), already decoded
    /// from whatever encoding was declared. Multi-value frames (v2.4
    /// uses NUL as the separator) split into several entries.
    Text { id: String, values: Vec<String> },
    /// `COMM` comment frame.
    Comment {
        lang: [u8; 3],
        description: String,
        text: String,
    },
    /// `USLT` unsynchronised lyrics frame — same on-the-wire shape as
    /// `COMM` so we use the same variant data.
    Lyrics {
        lang: [u8; 3],
        description: String,
        text: String,
    },
    /// `TXXX` user-defined text.
    UserText { description: String, value: String },
    /// `WXXX` user-defined URL.
    UserUrl { description: String, url: String },
    /// Non-user `W***` URL frame (`WCOM`, `WOAF`, ...). The URL is
    /// always ISO-8859-1 per spec.
    Url { id: String, url: String },
    /// `APIC` (v2.3/2.4) or `PIC` (v2.2) attached picture.
    Picture(AttachedPicture),
    /// `POPM` popularimeter. `email` is the (potentially empty) user
    /// id (NUL-terminated ISO-8859-1 string), `rating` is 1..=255
    /// (0 = unknown), `counter` is the play count. The spec allows
    /// the counter to be omitted *or* to grow past 32 bits by
    /// prefixing extra bytes; we widen into `u64` which covers any
    /// realistic count.
    Popularimeter {
        email: String,
        rating: u8,
        counter: u64,
    },
    /// `PCNT` play counter. The counter is always at least 32 bits and
    /// MAY grow byte-by-byte once it overflows; we widen into `u64`.
    PlayCounter { count: u64 },
    /// `PRIV` private frame. `owner` is a NUL-terminated ISO-8859-1
    /// owner identifier (typically a URL with an email), `data` is the
    /// opaque payload.
    Private { owner: String, data: Vec<u8> },
    /// `GEOB` general encapsulated object: arbitrary file embedded in
    /// the tag, identified by MIME type, original filename and
    /// content description.
    Geob {
        mime_type: String,
        filename: String,
        description: String,
        data: Vec<u8>,
    },
    /// `UFID` unique file identifier. `owner` is a NUL-terminated
    /// ISO-8859-1 owner identifier; `identifier` is up to 64 bytes of
    /// opaque database-specific id.
    Ufid { owner: String, identifier: Vec<u8> },
    /// `USER` terms-of-use frame (v2.3 §4.23 / v2.4 §4.22). A free-text
    /// description of the legal terms tied to a 3-byte ISO-639-2
    /// language code. Multiple `USER` frames may coexist in a tag as
    /// long as each one has a distinct `lang`.
    TermsOfUse { lang: [u8; 3], text: String },
    /// `OWNE` ownership frame (v2.3 §4.24 / v2.4 §4.23). Records the
    /// terms of a single purchase: ISO-4217 currency-prefixed price,
    /// 8-byte `YYYYMMDD` purchase date, and free-text seller name.
    /// Spec allows only one `OWNE` per tag.
    Ownership {
        price: String,
        date: String,
        seller: String,
    },
    /// `COMR` commercial frame (v2.3 §4.25 / v2.4 §4.24). Bundles a
    /// single competing offer (price, validity date, contact URL,
    /// delivery method, seller, description, optional company logo).
    /// Multiple `COMR` frames may coexist in a tag.
    Commercial {
        price: String,
        valid_until: String,
        contact_url: String,
        received_as: u8,
        seller: String,
        description: String,
        logo_mime: String,
        logo_data: Vec<u8>,
    },
    /// `SYTC` synchronised tempo codes (v2.4 §4.7). Carries a sequence
    /// of `(tempo_bpm, timestamp)` pairs against a single `time_format`
    /// byte (1 = MPEG frames, 2 = milliseconds). Tempos $00 and $01
    /// are reserved per spec (beat-free / single-stroke); larger
    /// values are the raw BPM, with a `$FF xx` two-byte extension
    /// covering 256..=510 BPM.
    SyncedTempo {
        time_format: u8,
        codes: Vec<(u16, u32)>,
    },
    /// `RVA2` relative volume adjustment 2 (v2.4 §4.11). Carries an
    /// identification string plus one entry per channel. The volume
    /// adjustment is a signed Q9.7 fixed-point dB value
    /// (`raw / 512.0`); the peak field is a zero-padded big-endian
    /// unsigned integer whose width is `ceil(bits_peak / 8)`.
    Rva2 {
        identification: String,
        channels: Vec<Rva2Channel>,
    },
    /// `EQU2` equalisation 2 (v2.4 §4.12). Carries a 1-byte
    /// interpolation method and an identification string, followed
    /// by `(frequency_hz_half, adjustment_q9_7)` pairs sorted by
    /// frequency. Frequencies are stored in units of 1/2 Hz; the
    /// adjustment uses the same fixed-point format as `RVA2`.
    Equ2 {
        interpolation: u8,
        identification: String,
        points: Vec<(u16, i16)>,
    },
    /// `MCDI` music CD identifier (v2.3 §4.5 / v2.4 §4.4). The frame
    /// body is the binary CD TOC table (1..=804 bytes per spec).
    /// We pass it through verbatim — interpretation is the consumer's
    /// job since the layout is the CD-DA TOC, not our format.
    MusicCdId { toc: Vec<u8> },
    /// `ETCO` event timing codes (v2.3 §4.6 / v2.4 §4.5). The
    /// `time_format` byte is 1 for MPEG frames, 2 for milliseconds.
    /// Each entry is `(event_type, timestamp)` per spec §4.5. The
    /// timestamps must be sorted ascending; we keep wire order so a
    /// caller can detect a non-conforming source.
    EventTimingCodes {
        time_format: u8,
        events: Vec<(u8, u32)>,
    },
    /// `SYLT` synchronised lyrics/text (v2.3 §4.10 / v2.4 §4.9). The
    /// `lang` is a 3-byte ISO-639-2 code, `time_format` matches ETCO,
    /// `content_type` is §4.9's 8-value enum (0=other, 1=lyrics,
    /// 2=transcription, 3=movement, 4=event, 5=chord, 6=trivia,
    /// 7=URLs-to-webpages, 8=URLs-to-images). Each sync entry is a
    /// terminated syllable plus a 32-bit timestamp.
    SyncedLyrics {
        lang: [u8; 3],
        time_format: u8,
        content_type: u8,
        description: String,
        syncs: Vec<(String, u32)>,
    },
    /// `POSS` position synchronisation (v2.3 §4.22 / v2.4 §4.21).
    /// Same `time_format` semantics as ETCO/SYLT; the single `position`
    /// is the offset from the first frame.
    PositionSync { time_format: u8, position: u32 },
    /// `RBUF` recommended buffer size (v2.3 §4.19 / v2.4 §4.18).
    /// `buffer_size` is the wire-encoded 24-bit BE value (0..=0xFFFFFF);
    /// `embedded_info` is the LSB of the flags byte; `offset_to_next`
    /// is a 32-bit BE byte offset to the next embedded tag.
    RecommendedBuffer {
        buffer_size: u32,
        embedded_info: bool,
        offset_to_next: u32,
    },
    /// `SEEK` seek frame (v2.4 §4.29). The minimum byte offset from
    /// the end of this tag to the start of the next embedded tag.
    Seek { min_offset_to_next_tag: u32 },
    /// `SIGN` signature frame (v2.4 §4.28). Binds a group of frames
    /// (per `GRID`) to a binary signature payload.
    Signature {
        group_symbol: u8,
        signature: Vec<u8>,
    },
    /// `GRID` group identification registration (v2.3 §4.27 / v2.4
    /// §4.26). Registers a group symbol so that the per-frame grouping
    /// flag (and `SIGN`) can refer to a set of frames as one unit.
    /// `owner` is a NUL-terminated ISO-8859-1 owner identifier (a URL
    /// with an email per spec), `group_symbol` is the $80-F0 value that
    /// associates frames with this group throughout the tag, and `data`
    /// is the optional group-dependent payload (e.g. a digital
    /// signature). Multiple `GRID` frames may coexist but each must
    /// carry a distinct symbol and a distinct owner.
    GroupId {
        owner: String,
        group_symbol: u8,
        data: Vec<u8>,
    },
    /// `ENCR` encryption method registration (v2.3 §4.25 / v2.4 §4.25).
    /// Registers an encryption method symbol so that the per-frame
    /// encryption flag can refer to it. `owner` is a NUL-terminated
    /// ISO-8859-1 owner identifier (a URL with an email per spec),
    /// `method_symbol` is the $80-F0 value associated with this method
    /// throughout the tag, and `data` is the optional encryption-specific
    /// payload. Multiple `ENCR` frames may coexist but each must carry a
    /// distinct symbol and a distinct owner. The wire layout is identical
    /// to `GRID` (owner + symbol byte + optional data) and is
    /// version-independent.
    EncryptionMethod {
        owner: String,
        method_symbol: u8,
        data: Vec<u8>,
    },
    /// `AENC` audio encryption (v2.3 §4.26 / v2.4 §4.19). Owner is a
    /// NUL-terminated URL/email; preview start + length are MPEG
    /// frame counts; encryption info is opaque scheme-specific data.
    AudioEncryption {
        owner: String,
        preview_start: u16,
        preview_length: u16,
        encryption_info: Vec<u8>,
    },
    /// `LINK` linked information (v2.3 §4.21 / v2.4 §4.20). The 4-byte
    /// `frame_id` names the frame to link in (3 bytes for v2.3 LINK,
    /// 4 bytes for v2.4); we always present it as a 4-byte array,
    /// zero-padding short v2.3 ids on the right. `url` is the
    /// ISO-8859-1 URL to the linked source. `additional` is any
    /// scheme-specific extra bytes (terminator-separated text strings
    /// per spec §4.20).
    LinkedInfo {
        frame_id: [u8; 4],
        url: String,
        additional: Vec<u8>,
    },
    /// `ASPI` audio seek point index (v2.4 §4.30). Provides a list of
    /// seek points within the audio data for variable-bit-rate streams.
    /// `indexed_data_start` is a byte offset from the beginning of the
    /// file; `indexed_data_length` is the byte length of the audio
    /// being indexed; each entry in `fractions` is an `Fi` value in the
    /// numerator of `Fi / 2^bits_per_index_point` (so an 8-bit point
    /// fits in `u16` trivially and a 16-bit point uses the full width).
    /// `bits_per_index_point` is 8 or 16 per spec; the writer rejects
    /// other widths. The presence of an ASPI frame implies a TLEN frame
    /// must also be present in the tag (this crate does not enforce
    /// that cross-frame invariant — that is a caller-level concern).
    /// ASPI is v2.4-only per spec but the wire layout is byte-aligned
    /// and version-independent, so the writer emits it under any
    /// version envelope.
    AudioSeekPointIndex {
        indexed_data_start: u32,
        indexed_data_length: u32,
        bits_per_index_point: u8,
        fractions: Vec<u16>,
    },
    /// `MLLT` MPEG location lookup table (v2.3 §4.7 / v2.4 §4.6).
    /// A jump-table for seeking inside an MPEG audio file: every Nth
    /// MPEG frame produces one reference whose three fixed fields
    /// describe what the encoder believes the cumulative byte offset
    /// and millisecond timestamp are, plus two per-reference deviation
    /// values (in bits) that correct the cumulative drift between
    /// belief and reality. There may be at most one `MLLT` frame per
    /// tag (spec §4.7 / §4.6).
    ///
    /// `mpeg_frames_between_reference` is the descriptor's u16
    /// "frame counter" increment (a value of 2 means the first
    /// reference is at the second MPEG frame, the second at the fourth,
    /// etc.). `bytes_between_reference` and `ms_between_reference` are
    /// 24-bit unsigned BE fields (the parser preserves the raw u32 in
    /// `0..=0xFF_FFFF`).
    ///
    /// Per-reference layout is `bits_for_bytes_deviation` bits of byte
    /// deviation followed by `bits_for_ms_deviation` bits of
    /// millisecond deviation, MSB-first, packed across byte boundaries
    /// in the spec's `%xxx....` form. The spec requires their sum to be
    /// a multiple of four — the parser tolerates a non-multiple-of-four
    /// sum by stopping once the remaining bits can no longer feed one
    /// complete reference, and the writer enforces the constraint by
    /// returning [`Error::invalid`] rather than emitting a stream the
    /// spec's reference reader could not align on.
    ///
    /// The two deviation widths are bounded at 32 bits each so a single
    /// reference fits in `(u32, u32)`. Anything wider is rejected on
    /// write; on read the parser refuses to interpret an out-of-range
    /// width and preserves the raw bytes via [`Id3Frame::Unknown`]
    /// rather than silently truncating.
    MpegLocationLookup {
        mpeg_frames_between_reference: u16,
        bytes_between_reference: u32,
        ms_between_reference: u32,
        bits_for_bytes_deviation: u8,
        bits_for_ms_deviation: u8,
        references: Vec<(u32, u32)>,
    },
    /// `RVRB` reverb frame (spec v2.3 §4.13 / v2.4 §4.13). A fixed
    /// 12-byte payload describing a subjective reverb pre-set:
    ///
    /// * `reverb_left_ms` / `reverb_right_ms` — u16 BE, delay between
    ///   bounces in milliseconds for each channel.
    /// * `bounces_left` / `bounces_right` — u8, number of bounces; a
    ///   value of `0xFF` means an infinite number of bounces per spec.
    /// * `feedback_ll` / `feedback_lr` / `feedback_rr` / `feedback_rl`
    ///   — u8, the four bounce-feedback amounts (left-to-left,
    ///   left-to-right, right-to-right, right-to-left) on the
    ///   spec's `$00 = 0% .. $FF = 100%` scale (a value of `0x7F`
    ///   yields the spec's worked example of "50% volume reduction
    ///   on the first bounce").
    /// * `premix_lr` / `premix_rl` — u8, pre-reverb cross-channel mix
    ///   on the same `$00..$FF` scale; both `0xFF` collapses to mono
    ///   when the reverb is applied symmetrically.
    ///
    /// The wire layout is byte-aligned and version-independent (v2.3
    /// and v2.4 are identical), so the writer accepts it under either
    /// envelope. Spec § says "There may only be one 'RVRB' frame in
    /// each tag" — uniqueness is a caller-level concern, not enforced
    /// here.
    Reverb {
        reverb_left_ms: u16,
        reverb_right_ms: u16,
        bounces_left: u8,
        bounces_right: u8,
        feedback_ll: u8,
        feedback_lr: u8,
        feedback_rr: u8,
        feedback_rl: u8,
        premix_lr: u8,
        premix_rl: u8,
    },
    /// Any frame whose id we don't parse structurally (RGAD, CHAP,
    /// ...). The payload is preserved verbatim so callers or later
    /// versions can recognise it without needing to reparse.
    Unknown { id: String, raw: Vec<u8> },
}

/// One `RVA2` channel entry (spec v2.4 §4.11). The raw 16-bit signed
/// `volume_adjustment` is in Q9.7 dB (`raw / 512.0` = dB). The peak
/// payload is zero-padded to whole bytes per spec; we keep the raw
/// byte width so a writer can round-trip the exact on-wire form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rva2Channel {
    /// Spec §4.11 channel-type enumeration ($00..=$08; values outside
    /// the table pass through as-is for forward compatibility).
    pub channel_type: u8,
    /// Signed Q9.7 dB. Divide by `512.0` to recover the dB value.
    pub volume_adjustment: i16,
    /// Spec: "Bits representing peak can be any number between 0 and
    /// 255. 0 means that there is no peak volume field."
    pub bits_peak: u8,
    /// Big-endian unsigned peak. Width = `ceil(bits_peak / 8)`.
    pub peak: Vec<u8>,
}

/// Decoded form of the `time_stamp_format` byte that appears in
/// `ETCO`, `SYTC`, `SYLT`, and `POSS` frames. The spec (v2.3 §4.6,
/// §4.8, §4.10, §4.22 and v2.4 §4.5, §4.7, §4.9, §4.21) defines two
/// values; anything else is reserved and surfaces as `None` from the
/// typed accessor.
///
/// Wire values:
///
/// * `$01` — MPEG frames as unit. Decoded as
///   [`TimestampUnit::MpegFrames`].
/// * `$02` — milliseconds as unit. Decoded as
///   [`TimestampUnit::Milliseconds`].
///
/// The numeric byte itself is unchanged across v2.3 → v2.4 (see spec
/// §4.10 vs §4.9), so the logical unit round-trips losslessly when a
/// tag is re-serialised under a different version envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimestampUnit {
    /// `$01` per spec — timestamps count MPEG audio frames from the
    /// start of the file.
    MpegFrames,
    /// `$02` per spec — timestamps are milliseconds from the start of
    /// the file.
    Milliseconds,
}

impl TimestampUnit {
    /// Decode a raw `time_stamp_format` byte. Returns `None` for
    /// reserved values (anything other than `$01` or `$02` per spec).
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(TimestampUnit::MpegFrames),
            2 => Some(TimestampUnit::Milliseconds),
            _ => None,
        }
    }

    /// Encode this unit back to the raw wire byte (`$01` or `$02`).
    pub fn to_wire(self) -> u8 {
        match self {
            TimestampUnit::MpegFrames => 1,
            TimestampUnit::Milliseconds => 2,
        }
    }
}

impl Id3Frame {
    /// Typed accessor for the `time_stamp_format` byte carried by the
    /// frames whose spec layout opens with one (`ETCO`, `SYTC`, `SYLT`,
    /// `POSS`). Returns `Some(unit)` when the wire byte is `$01` or
    /// `$02` per spec, and `None` for any other variant or any
    /// reserved wire byte. Lets callers handle the cross-version
    /// timestamp unit without matching on the raw `u8`.
    pub fn timestamp_unit(&self) -> Option<TimestampUnit> {
        let wire = match self {
            Id3Frame::EventTimingCodes { time_format, .. }
            | Id3Frame::SyncedTempo { time_format, .. }
            | Id3Frame::SyncedLyrics { time_format, .. }
            | Id3Frame::PositionSync { time_format, .. } => *time_format,
            _ => return None,
        };
        TimestampUnit::from_wire(wire)
    }
}

/// Parse an ID3v2 tag from a buffer that starts with the 10-byte
/// header. On success, returns the [`Id3Tag`] and the total number of
/// bytes consumed from `buf` (header + body + optional footer) —
/// callers can seek by that many bytes to reach the next byte after
/// the tag.
pub fn parse_tag(buf: &[u8]) -> Result<(Id3Tag, usize)> {
    if buf.len() < ID3V2_HEADER_SIZE {
        return Err(Error::NeedMore);
    }
    if &buf[0..3] != b"ID3" {
        return Err(Error::invalid("not an ID3v2 tag"));
    }
    let major = buf[3];
    let revision = buf[4];
    let flags = buf[5];
    let size = synchsafe_u32(buf[6], buf[7], buf[8], buf[9]) as usize;
    let footer_present = flags & 0x10 != 0;
    let total = ID3V2_HEADER_SIZE + size + if footer_present { 10 } else { 0 };
    if buf.len() < ID3V2_HEADER_SIZE + size {
        return Err(Error::NeedMore);
    }
    let body = &buf[ID3V2_HEADER_SIZE..ID3V2_HEADER_SIZE + size];
    let version = match major {
        2 => Id3Version::V2_2,
        3 => Id3Version::V2_3,
        4 => Id3Version::V2_4,
        other => {
            return Err(Error::unsupported(format!(
                "unknown ID3v2 major version {other}"
            )));
        }
    };

    // Footer (spec §3.4) is a v2.4-only construct. The footer flag bit
    // being set on a v2.3 (or v2.2) tag indicates a malformed or
    // version-confused producer; reject rather than silently advancing
    // past the spurious 10 bytes — which would corrupt the caller's
    // file-cursor accounting if those bytes were actually audio.
    if footer_present && !matches!(version, Id3Version::V2_4) {
        return Err(Error::invalid(
            "ID3v2 footer flag (0x10) is v2.4-only; rejected on v2.2/v2.3",
        ));
    }
    // Footer requires 10 more bytes after the body. Validate up front
    // so that a partial buffer is reported via NeedMore rather than
    // succeeding with an inconsistent `total`.
    if footer_present && buf.len() < total {
        return Err(Error::NeedMore);
    }
    if footer_present {
        let footer = &buf[ID3V2_HEADER_SIZE + size..total];
        if &footer[0..3] != b"3DI" {
            return Err(Error::invalid(
                "ID3v2 footer magic missing: expected b\"3DI\"",
            ));
        }
        // Spec §3.4: "The footer is a copy of the header, but with a
        // different identifier." Version, flags, and size MUST match.
        if footer[3] != major || footer[4] != revision {
            return Err(Error::invalid(
                "ID3v2 footer version/revision does not match header",
            ));
        }
        if footer[5] != flags {
            return Err(Error::invalid("ID3v2 footer flags do not match header"));
        }
        let footer_size = synchsafe_u32(footer[6], footer[7], footer[8], footer[9]) as usize;
        if footer_size != size {
            return Err(Error::invalid("ID3v2 footer size does not match header"));
        }
    }

    // Whole-tag unsync is a v2.2/v2.3 mechanism. v2.4 moves it to a
    // per-frame flag, but some taggers still set the header bit; we
    // honour whichever is present.
    let unsync_whole_tag =
        (flags & 0x80) != 0 && matches!(version, Id3Version::V2_2 | Id3Version::V2_3);
    // v2.4 also has a whole-tag unsync flag but it's strictly
    // "advisory" — the spec says the tag *may* be unsynchronised, and
    // per-frame flags are authoritative. We still decode the whole
    // body when the flag is set in v2.4 so older or strict taggers
    // work; per-frame unsync on an already-reversed buffer is a no-op.
    let unsync_v24_body = (flags & 0x80) != 0 && matches!(version, Id3Version::V2_4);

    let body_owned;
    let mut body: &[u8] = if unsync_whole_tag || unsync_v24_body {
        body_owned = reverse_unsync(body);
        &body_owned
    } else {
        body
    };

    // Extended header: 6 bytes in v2.3 (size is non-synchsafe, EXCLUDES
    // itself), 6+ bytes in v2.4 (first 4 bytes are synchsafe size
    // INCLUDING those 4 bytes). When the CRC flag is set we verify the
    // stored CRC-32 against the data the spec defines (v2.3: frames
    // only, excluding padding; v2.4: frames + padding). A mismatched
    // CRC is a hard parse error: a parser that accepted broken CRCs
    // would defeat the point of the field.
    if flags & 0x40 != 0 {
        body = parse_extended_header(version, body)?;
    }

    let frames = parse_frames(version, body);
    Ok((Id3Tag { version, frames }, total))
}

/// Peek at the first 10 bytes of a file. Returns `Some(total_size)` —
/// header + body + optional footer — when a valid ID3v2 tag starts
/// there, or `None` otherwise. Callers use this to size a read without
/// parsing frames yet.
pub fn tag_size_at_head(first10: &[u8]) -> Option<usize> {
    if first10.len() < 10 || &first10[0..3] != b"ID3" {
        return None;
    }
    let flags = first10[5];
    let size = synchsafe_u32(first10[6], first10[7], first10[8], first10[9]) as usize;
    let footer = if flags & 0x10 != 0 { 10 } else { 0 };
    Some(ID3V2_HEADER_SIZE + size + footer)
}

/// Parse an ID3v1 trailer. Returns `None` when the buffer doesn't
/// start with `TAG` or is shorter than 128 bytes.
pub fn parse_id3v1(trailer_128: &[u8]) -> Option<Id3Tag> {
    if trailer_128.len() < 128 || &trailer_128[0..3] != b"TAG" {
        return None;
    }
    let title = v1_string(&trailer_128[3..33]);
    let artist = v1_string(&trailer_128[33..63]);
    let album = v1_string(&trailer_128[63..93]);
    let year = v1_string(&trailer_128[93..97]);
    // ID3v1.1: if byte 125 is NUL and byte 126 is non-zero, the last
    // 2 bytes are a track number; otherwise the full 30 bytes are a
    // free-form comment.
    let (comment, track) = if trailer_128[125] == 0 && trailer_128[126] != 0 {
        (v1_string(&trailer_128[97..125]), Some(trailer_128[126]))
    } else {
        (v1_string(&trailer_128[97..127]), None)
    };
    let genre_byte = trailer_128[127];
    let genre = id3v1_genre(genre_byte).map(|s| s.to_string());

    let mut frames = Vec::new();
    if !title.is_empty() {
        frames.push(Id3Frame::Text {
            id: "TIT2".into(),
            values: vec![title],
        });
    }
    if !artist.is_empty() {
        frames.push(Id3Frame::Text {
            id: "TPE1".into(),
            values: vec![artist],
        });
    }
    if !album.is_empty() {
        frames.push(Id3Frame::Text {
            id: "TALB".into(),
            values: vec![album],
        });
    }
    if !year.is_empty() {
        frames.push(Id3Frame::Text {
            id: "TYER".into(),
            values: vec![year],
        });
    }
    if !comment.is_empty() {
        frames.push(Id3Frame::Comment {
            lang: *b"XXX",
            description: String::new(),
            text: comment,
        });
    }
    if let Some(t) = track {
        frames.push(Id3Frame::Text {
            id: "TRCK".into(),
            values: vec![t.to_string()],
        });
    }
    if let Some(g) = genre {
        frames.push(Id3Frame::Text {
            id: "TCON".into(),
            values: vec![g],
        });
    }

    Some(Id3Tag {
        version: Id3Version::V1,
        frames,
    })
}

/// Normalise an [`Id3Tag`] into flat `(key, value)` pairs using the
/// lowercase Vorbis-comment keys the rest of the workspace expects.
/// Known v2.3/v2.4 four-char ids map to their Vorbis equivalents;
/// v2.2 three-char ids map via the v2.2→v2.3 promotion table; unknown
/// ids pass through with their raw id lowercased.
pub fn to_key_value_pairs(tag: &Id3Tag) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in &tag.frames {
        match f {
            Id3Frame::Text { id, values } => {
                let key = text_frame_to_key(id);
                let value = values.join("/");
                if !value.is_empty() {
                    push_unique(&mut out, key, value);
                }
            }
            Id3Frame::Comment {
                description, text, ..
            } => {
                if !text.is_empty() {
                    let key = if description.is_empty() {
                        "comment".to_string()
                    } else {
                        format!("comment:{}", description.to_lowercase())
                    };
                    push_unique(&mut out, key, text.clone());
                }
            }
            Id3Frame::Lyrics {
                description, text, ..
            } => {
                if !text.is_empty() {
                    let key = if description.is_empty() {
                        "lyrics".to_string()
                    } else {
                        format!("lyrics:{}", description.to_lowercase())
                    };
                    push_unique(&mut out, key, text.clone());
                }
            }
            Id3Frame::UserText { description, value } => {
                if !value.is_empty() {
                    let key = if description.is_empty() {
                        "user_text".to_string()
                    } else {
                        description.to_lowercase()
                    };
                    push_unique(&mut out, key, value.clone());
                }
            }
            Id3Frame::UserUrl { description, url } => {
                if !url.is_empty() {
                    let key = if description.is_empty() {
                        "user_url".to_string()
                    } else {
                        format!("url:{}", description.to_lowercase())
                    };
                    push_unique(&mut out, key, url.clone());
                }
            }
            Id3Frame::Url { id, url } => {
                if !url.is_empty() {
                    push_unique(&mut out, format!("url:{}", id.to_lowercase()), url.clone());
                }
            }
            // Pictures are surfaced via attached_pictures(), not k/v.
            Id3Frame::Picture(_) => {}
            Id3Frame::Popularimeter {
                email,
                rating,
                counter,
            } => {
                // Surface as Vorbis-style "rating" / "rating_count"
                // keys, scoped by the (possibly empty) email so
                // multiple POPM frames don't collide. The rating byte
                // is the 1..=255 raw value; consumers that prefer
                // the 0..=5 "star" scale can rescale.
                let scope = if email.is_empty() {
                    String::new()
                } else {
                    format!(":{}", email.to_lowercase())
                };
                if *rating != 0 {
                    push_unique(&mut out, format!("rating{scope}"), rating.to_string());
                }
                if *counter != 0 {
                    push_unique(
                        &mut out,
                        format!("rating_count{scope}"),
                        counter.to_string(),
                    );
                }
            }
            Id3Frame::PlayCounter { count } => {
                push_unique(&mut out, "play_count".to_string(), count.to_string());
            }
            Id3Frame::TermsOfUse { lang, text } => {
                // Surface terms-of-use as a "termsofuse[:lang]" key
                // mirroring the COMM/lang style — multiple language
                // variants stay distinct.
                let lang_str = latin1_to_string(lang);
                let key = if lang_str.trim().is_empty() {
                    "termsofuse".to_string()
                } else {
                    format!("termsofuse:{}", lang_str.to_lowercase())
                };
                push_unique(&mut out, key, text.clone());
            }
            Id3Frame::Ownership {
                price,
                date,
                seller,
            } => {
                if !price.is_empty() {
                    push_unique(&mut out, "ownership_price".to_string(), price.clone());
                }
                let trimmed_date = date.trim().to_string();
                if !trimmed_date.is_empty() {
                    push_unique(&mut out, "ownership_date".to_string(), trimmed_date);
                }
                if !seller.is_empty() {
                    push_unique(&mut out, "ownership_seller".to_string(), seller.clone());
                }
            }
            // COMR / SYTC / RVA2 / EQU2 / PRIV / GEOB / UFID / MCDI /
            // ETCO / SYLT / POSS / RBUF / SEEK / SIGN / GRID / ENCR /
            // AENC / LINK carry structured or binary payloads that do not
            // project cleanly onto Vorbis-style text pairs. Callers that
            // need them should match on the enum.
            Id3Frame::Commercial { .. }
            | Id3Frame::SyncedTempo { .. }
            | Id3Frame::Rva2 { .. }
            | Id3Frame::Equ2 { .. }
            | Id3Frame::Private { .. }
            | Id3Frame::Geob { .. }
            | Id3Frame::Ufid { .. }
            | Id3Frame::MusicCdId { .. }
            | Id3Frame::EventTimingCodes { .. }
            | Id3Frame::SyncedLyrics { .. }
            | Id3Frame::PositionSync { .. }
            | Id3Frame::RecommendedBuffer { .. }
            | Id3Frame::Seek { .. }
            | Id3Frame::Signature { .. }
            | Id3Frame::GroupId { .. }
            | Id3Frame::EncryptionMethod { .. }
            | Id3Frame::AudioEncryption { .. }
            | Id3Frame::LinkedInfo { .. }
            | Id3Frame::AudioSeekPointIndex { .. }
            | Id3Frame::MpegLocationLookup { .. }
            | Id3Frame::Reverb { .. }
            | Id3Frame::Unknown { .. } => {}
        }
    }
    out
}

/// Extract only the attached pictures from a tag, cloned out as a
/// convenient Vec for callers that don't want to match on the enum.
pub fn attached_pictures(tag: &Id3Tag) -> Vec<AttachedPicture> {
    tag.frames
        .iter()
        .filter_map(|f| match f {
            Id3Frame::Picture(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn synchsafe_u32(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32 & 0x7F) << 21)
        | ((b as u32 & 0x7F) << 14)
        | ((c as u32 & 0x7F) << 7)
        | (d as u32 & 0x7F)
}

fn regular_u32(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32)
}

fn regular_u24(a: u8, b: u8, c: u8) -> u32 {
    ((a as u32) << 16) | ((b as u32) << 8) | (c as u32)
}

/// Reverse the ID3 unsynchronisation encoding: every `0xFF 0x00`
/// sequence collapses back to a bare `0xFF`. Other bytes pass through
/// verbatim. This is a byte-for-byte, stream-safe operation.
fn reverse_unsync(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        out.push(buf[i]);
        if buf[i] == 0xFF && i + 1 < buf.len() && buf[i + 1] == 0x00 {
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Apply the ID3 unsynchronisation encoding (the inverse of
/// [`reverse_unsync`]). For every `0xFF` byte that is followed by a
/// byte whose top three bits are set (`%111xxxxx`) OR by a literal
/// `0x00`, a `0x00` byte is inserted after the `0xFF`. A trailing
/// `0xFF` at the very end of the buffer is also escaped (spec
/// §6.1: "the special case when the last byte of the last frame is
/// $FF [...] can be solved by [...] unsynchronising the frame and
/// adding $00 to the end of the frame data"). The result, once
/// passed back through [`reverse_unsync`], reproduces the input
/// byte-for-byte.
fn apply_unsync(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        let b = buf[i];
        out.push(b);
        if b == 0xFF {
            let next = buf.get(i + 1).copied();
            let needs_escape = match next {
                Some(n) if (n & 0xE0) == 0xE0 => true, // false sync %111xxxxx
                Some(0x00) => true,                    // protect literal $FF $00
                None => true,                          // trailing $FF
                _ => false,
            };
            if needs_escape {
                out.push(0x00);
            }
        }
        i += 1;
    }
    out
}

/// Walk the extended header at the start of `body` and return the
/// remaining body (frames + padding). When the CRC flag is set, the
/// stored CRC-32 is verified against the spec-defined region:
///
/// * v2.3 (`%x0000000 00000000` extended-flags bit 15) — CRC covers the
///   frames only, excluding the padding whose size is announced in the
///   extended header itself.
/// * v2.4 (extended-flags bit `c` = 0x20) — CRC covers everything after
///   the extended header (frames + padding), per §3.2 "all the data
///   between the header and footer ... minus the extended header. Note
///   that this includes the padding".
///
/// The v2.4 restrictions flag (`d` = 0x10) is consumed but does not
/// influence parsing — it describes how the tag was *encoded*, not how
/// to decode it.
fn parse_extended_header(version: Id3Version, body: &[u8]) -> Result<&[u8]> {
    match version {
        Id3Version::V2_3 => {
            if body.len() < 4 {
                return Err(Error::invalid("ID3v2.3 extended header truncated"));
            }
            let ext_size = regular_u32(body[0], body[1], body[2], body[3]) as usize;
            // v2.3 §3.2: "Extended header size [...] excludes itself".
            // Currently 6 or 10 bytes (10 when CRC is present).
            let total = 4 + ext_size;
            if total > body.len() || ext_size < 6 {
                return Err(Error::invalid("ID3v2.3 extended header overflows tag"));
            }
            let ext = &body[4..total];
            // ext layout: %x0000000 00000000  (flags, 2 bytes)
            //             size of padding     (4 bytes regular)
            //             total frame CRC     (4 bytes regular, iff flag x set)
            let flag_hi = ext[0];
            let crc_present = flag_hi & 0x80 != 0;
            let padding_size = regular_u32(ext[2], ext[3], ext[4], ext[5]) as usize;
            let after = &body[total..];
            if padding_size > after.len() {
                return Err(Error::invalid(
                    "ID3v2.3 extended header padding size exceeds body",
                ));
            }
            if crc_present {
                if ext_size != 10 {
                    return Err(Error::invalid(
                        "ID3v2.3 extended header CRC flag set but size is not 10",
                    ));
                }
                let stored = regular_u32(ext[6], ext[7], ext[8], ext[9]);
                // v2.3 spec §3.2: CRC covers "the frames and only the
                // frames" — excludes the padding announced above.
                let frames_only = &after[..after.len() - padding_size];
                let computed = crc32_iso3309(frames_only);
                if computed != stored {
                    return Err(Error::invalid(format!(
                        "ID3v2.3 extended header CRC mismatch: stored={stored:#010x} computed={computed:#010x}"
                    )));
                }
            }
            Ok(after)
        }
        Id3Version::V2_4 => {
            if body.len() < 4 {
                return Err(Error::invalid("ID3v2.4 extended header truncated"));
            }
            let ext_size = synchsafe_u32(body[0], body[1], body[2], body[3]) as usize;
            // v2.4 §3.2: ext_size INCLUDES itself. "An extended header
            // can thus never have a size of fewer than six bytes."
            if ext_size < 6 || ext_size > body.len() {
                return Err(Error::invalid("ID3v2.4 extended header size invalid"));
            }
            let ext = &body[..ext_size];
            // ext layout (after the 4 size bytes):
            //   number-of-flag-bytes  $01
            //   extended flags        %0bcd0000
            //   per-flag attached data, in flag order b, c, d
            let num_flag_bytes = ext[4] as usize;
            if num_flag_bytes != 1 {
                return Err(Error::invalid(
                    "ID3v2.4 extended header: only single flag byte supported",
                ));
            }
            if ext.len() < 6 {
                return Err(Error::invalid("ID3v2.4 extended header truncated"));
            }
            let ext_flags = ext[5];
            let update = ext_flags & 0x40 != 0;
            let crc = ext_flags & 0x20 != 0;
            let restrictions = ext_flags & 0x10 != 0;
            // Reject unknown extended-flag bits per spec §3.2: "All
            // unknown flags MUST be unset and their corresponding data
            // removed when a tag is modified". A set unknown bit means
            // we cannot safely advance past the attached-data area.
            if ext_flags & !0x70 != 0 {
                return Err(Error::invalid(
                    "ID3v2.4 extended header: unknown extended-flag bits set",
                ));
            }
            let mut cursor = 6usize;
            let after = &body[ext_size..];
            let mut stored_crc: Option<u32> = None;
            for (flag_present, expected_len, name) in [
                (update, 0u8, "update"),
                (crc, 5u8, "crc"),
                (restrictions, 1u8, "restrictions"),
            ] {
                if !flag_present {
                    continue;
                }
                if cursor >= ext.len() {
                    return Err(Error::invalid(format!(
                        "ID3v2.4 extended header: missing data-length for {name} flag"
                    )));
                }
                let data_len = ext[cursor] as usize;
                if data_len != expected_len as usize {
                    return Err(Error::invalid(format!(
                        "ID3v2.4 extended header: {name} data-length is {data_len}, expected {expected_len}"
                    )));
                }
                cursor += 1;
                if cursor + data_len > ext.len() {
                    return Err(Error::invalid(format!(
                        "ID3v2.4 extended header: truncated data for {name} flag"
                    )));
                }
                if name == "crc" && data_len == 5 {
                    // Spec §3.2 "Total frame CRC    5 * %0xxxxxxx" — the
                    // 32-bit CRC is stored as 5 synchsafe bytes (35
                    // bits, upper 4 always zero).
                    stored_crc = Some(crc32_from_synchsafe5(
                        ext[cursor],
                        ext[cursor + 1],
                        ext[cursor + 2],
                        ext[cursor + 3],
                        ext[cursor + 4],
                    ));
                }
                cursor += data_len;
            }
            if let Some(stored) = stored_crc {
                // v2.4 §3.2: CRC is "calculated on all the data between
                // the header and footer as indicated by the header's
                // tag length field, minus the extended header. Note
                // that this includes the padding".
                let computed = crc32_iso3309(after);
                if computed != stored {
                    return Err(Error::invalid(format!(
                        "ID3v2.4 extended header CRC mismatch: stored={stored:#010x} computed={computed:#010x}"
                    )));
                }
            }
            Ok(after)
        }
        _ => Ok(body),
    }
}

/// CRC-32 [ISO-3309] — the IEEE 802.3 / PNG / zlib variant: polynomial
/// `0x04C11DB7` (reflected `0xEDB88320`), init `0xFFFF_FFFF`, xor-out
/// `0xFFFF_FFFF`. Used by the ID3v2.3 and v2.4 extended-header CRC
/// fields (spec §3.2 in both versions). The implementation is the
/// classic bit-by-bit table-free loop — the data spans we run it over
/// are tag-body-sized (KB at most), so the table is unnecessary.
fn crc32_iso3309(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// Decode the v2.4 extended-header CRC's 5-byte synchsafe encoding back
/// into a 32-bit value. Spec §3.2: "stored as a 35 bit synchsafe
/// integer, leaving the upper four bits always zeroed".
fn crc32_from_synchsafe5(a: u8, b: u8, c: u8, d: u8, e: u8) -> u32 {
    (((a as u64 & 0x7F) << 28)
        | ((b as u64 & 0x7F) << 21)
        | ((c as u64 & 0x7F) << 14)
        | ((d as u64 & 0x7F) << 7)
        | (e as u64 & 0x7F)) as u32
}

/// Encode a 32-bit CRC into 5 synchsafe bytes for v2.4 extended-header
/// emission. The upper 4 bits of the 35-bit value are always zero (the
/// CRC fits in 32 bits).
fn crc32_to_synchsafe5(crc: u32) -> [u8; 5] {
    let v = crc as u64;
    [
        ((v >> 28) & 0x07) as u8, // top 4 bits ride here; remaining 3 unused
        ((v >> 21) & 0x7F) as u8,
        ((v >> 14) & 0x7F) as u8,
        ((v >> 7) & 0x7F) as u8,
        (v & 0x7F) as u8,
    ]
}

fn parse_frames(version: Id3Version, body: &[u8]) -> Vec<Id3Frame> {
    let mut frames = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        // A byte of 0x00 at the start of a frame id is the padding
        // sentinel — everything from here to end of body is zeros.
        if body[i] == 0 {
            break;
        }
        match parse_one_frame(version, &body[i..]) {
            Ok((frame, consumed)) => {
                frames.push(frame);
                i += consumed;
            }
            Err(_) => {
                // Give up on further frames but keep what we parsed. A
                // single corrupted frame is common in real-world files
                // (truncated tags, buggy taggers) and shouldn't nuke
                // the whole tag.
                break;
            }
        }
    }
    frames
}

fn parse_one_frame(version: Id3Version, buf: &[u8]) -> Result<(Id3Frame, usize)> {
    match version {
        Id3Version::V2_2 => parse_v22_frame(buf),
        Id3Version::V2_3 => parse_v23_frame(buf),
        Id3Version::V2_4 => parse_v24_frame(buf),
        Id3Version::V1 => Err(Error::invalid("parse_one_frame called on v1")),
    }
}

fn parse_v22_frame(buf: &[u8]) -> Result<(Id3Frame, usize)> {
    if buf.len() < 6 {
        return Err(Error::invalid("v2.2 frame header truncated"));
    }
    let id = std::str::from_utf8(&buf[0..3])
        .map_err(|_| Error::invalid("v2.2 frame id not ASCII"))?
        .to_string();
    let size = regular_u24(buf[3], buf[4], buf[5]) as usize;
    if 6 + size > buf.len() {
        return Err(Error::invalid("v2.2 frame overflows tag body"));
    }
    let payload = &buf[6..6 + size];
    let frame = dispatch_v22(&id, payload);
    Ok((frame, 6 + size))
}

fn parse_v23_frame(buf: &[u8]) -> Result<(Id3Frame, usize)> {
    if buf.len() < 10 {
        return Err(Error::invalid("v2.3 frame header truncated"));
    }
    let id = std::str::from_utf8(&buf[0..4])
        .map_err(|_| Error::invalid("v2.3 frame id not ASCII"))?
        .to_string();
    let size = regular_u32(buf[4], buf[5], buf[6], buf[7]) as usize;
    let _flags = u16::from_be_bytes([buf[8], buf[9]]);
    if 10 + size > buf.len() {
        return Err(Error::invalid("v2.3 frame overflows tag body"));
    }
    let payload = &buf[10..10 + size];
    let frame = dispatch_v23_v24(&id, payload);
    Ok((frame, 10 + size))
}

fn parse_v24_frame(buf: &[u8]) -> Result<(Id3Frame, usize)> {
    if buf.len() < 10 {
        return Err(Error::invalid("v2.4 frame header truncated"));
    }
    let id = std::str::from_utf8(&buf[0..4])
        .map_err(|_| Error::invalid("v2.4 frame id not ASCII"))?
        .to_string();
    let size = synchsafe_u32(buf[4], buf[5], buf[6], buf[7]) as usize;
    let flags = u16::from_be_bytes([buf[8], buf[9]]);
    if 10 + size > buf.len() {
        return Err(Error::invalid("v2.4 frame overflows tag body"));
    }
    // Format flags (low byte): bit 0x01 = data-length indicator,
    // bit 0x02 = unsync, bit 0x04 = encryption, bit 0x08 =
    // compression, bit 0x40 = grouping identity.
    let fmt_flags = (flags & 0xFF) as u8;
    let data_length_indicator = fmt_flags & 0x01 != 0;
    let frame_unsync = fmt_flags & 0x02 != 0;
    let encrypted = fmt_flags & 0x04 != 0;
    let compressed = fmt_flags & 0x08 != 0;
    let grouping = fmt_flags & 0x40 != 0;

    let mut payload = &buf[10..10 + size];
    if grouping && !payload.is_empty() {
        payload = &payload[1..];
    }
    if encrypted || compressed {
        // We don't carry keys/zlib, so just emit an Unknown frame so
        // callers can see it was present.
        return Ok((
            Id3Frame::Unknown {
                id,
                raw: payload.to_vec(),
            },
            10 + size,
        ));
    }
    // The data-length indicator is 4 synchsafe bytes giving the real
    // (post-decompression, post-unsync) size. We don't decompress so
    // we just skip past the indicator.
    if data_length_indicator {
        if payload.len() < 4 {
            return Err(Error::invalid("v2.4 frame data-length indicator truncated"));
        }
        payload = &payload[4..];
    }
    let unsync_owned;
    if frame_unsync {
        unsync_owned = reverse_unsync(payload);
        payload = &unsync_owned;
        // Rust can't see the borrow across the `unsync_owned` binding
        // without an extra let, so give it one.
        let _ = &unsync_owned;
    }
    let frame = dispatch_v23_v24(&id, payload);
    Ok((frame, 10 + size))
}

/// Dispatch a v2.3/v2.4 frame payload to the right parser based on
/// its 4-char id.
fn dispatch_v23_v24(id: &str, payload: &[u8]) -> Id3Frame {
    if id == "TXXX" {
        return parse_txxx(id, payload);
    }
    if id.starts_with('T') && id != "TXXX" {
        return parse_text_frame(id, payload);
    }
    if id == "WXXX" {
        return parse_wxxx(id, payload);
    }
    if id.starts_with('W') && id != "WXXX" {
        return parse_url_frame(id, payload);
    }
    match id {
        "COMM" => parse_comm_like(payload, false),
        "USLT" => parse_comm_like(payload, true),
        "APIC" => parse_apic(payload),
        "POPM" => parse_popm(payload),
        "PCNT" => parse_pcnt(payload),
        "PRIV" => parse_priv(payload),
        "GEOB" => parse_geob(payload),
        "UFID" => parse_ufid(payload),
        "USER" => parse_user(payload),
        "OWNE" => parse_owne(payload),
        "COMR" => parse_comr(payload),
        "SYTC" => parse_sytc(payload),
        "RVA2" => parse_rva2(payload),
        "EQU2" => parse_equ2(payload),
        "MCDI" => parse_mcdi(payload),
        "ETCO" => parse_etco(payload),
        "SYLT" => parse_sylt(payload),
        "POSS" => parse_poss(payload),
        "RBUF" => parse_rbuf(payload),
        "SEEK" => parse_seek(payload),
        "SIGN" => parse_sign(payload),
        "GRID" => parse_grid(payload),
        "ENCR" => parse_encr(payload),
        "AENC" => parse_aenc(payload),
        "LINK" => parse_link(payload),
        "ASPI" => parse_aspi(payload),
        "MLLT" => parse_mllt(payload),
        "RVRB" => parse_rvrb(payload),
        _ => Id3Frame::Unknown {
            id: id.to_string(),
            raw: payload.to_vec(),
        },
    }
}

/// Dispatch a v2.2 (3-char id) frame payload. v2.2 ids are promoted
/// to their v2.3 four-char equivalents for caller-facing output so
/// `to_key_value_pairs` doesn't need to know about both.
fn dispatch_v22(id: &str, payload: &[u8]) -> Id3Frame {
    // Text frames — v2.2 uses 3-char ids that promote cleanly. We use
    // the dominant v2.3 equivalent.
    let promoted = v22_promote(id);
    if id == "TXX" {
        return parse_txxx(promoted, payload);
    }
    if id.starts_with('T') {
        return parse_text_frame(promoted, payload);
    }
    if id == "WXX" {
        return parse_wxxx(promoted, payload);
    }
    if id.starts_with('W') {
        return parse_url_frame(promoted, payload);
    }
    match id {
        "COM" => parse_comm_like(payload, false),
        "ULT" => parse_comm_like(payload, true),
        "PIC" => parse_pic(payload),
        "REV" => parse_rvrb(payload),
        _ => Id3Frame::Unknown {
            id: id.to_string(),
            raw: payload.to_vec(),
        },
    }
}

/// Promote v2.2 3-char ids to their v2.3 4-char equivalents. Entries
/// follow the ID3v2.2 → v2.3 conversion table. Unknown ids pass
/// through unchanged (they land in `Unknown` anyway).
fn v22_promote(id: &str) -> &str {
    match id {
        "TT1" => "TIT1",
        "TT2" => "TIT2",
        "TT3" => "TIT3",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TP3" => "TPE3",
        "TP4" => "TPE4",
        "TCM" => "TCOM",
        "TXT" => "TEXT",
        "TLA" => "TLAN",
        "TCO" => "TCON",
        "TAL" => "TALB",
        "TPA" => "TPOS",
        "TRK" => "TRCK",
        "TRC" => "TSRC",
        "TYE" => "TYER",
        "TDA" => "TDAT",
        "TIM" => "TIME",
        "TRD" => "TRDA",
        "TMT" => "TMED",
        "TFT" => "TFLT",
        "TBP" => "TBPM",
        "TCP" => "TCMP",
        "TCR" => "TCOP",
        "TPB" => "TPUB",
        "TEN" => "TENC",
        "TSS" => "TSSE",
        "TOF" => "TOFN",
        "TLE" => "TLEN",
        "TSI" => "TSIZ",
        "TDY" => "TDLY",
        "TKE" => "TKEY",
        "TOT" => "TOAL",
        "TOA" => "TOPE",
        "TOL" => "TOLY",
        "TOR" => "TORY",
        "TXX" => "TXXX",
        "REV" => "RVRB",
        "WAF" => "WOAF",
        "WAR" => "WOAR",
        "WAS" => "WOAS",
        "WCM" => "WCOM",
        "WCP" => "WCOP",
        "WPB" => "WPUB",
        "WXX" => "WXXX",
        other => other,
    }
}

fn parse_text_frame(id: &str, payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Text {
            id: id.to_string(),
            values: Vec::new(),
        };
    }
    let enc = payload[0];
    let text = decode_text(enc, &payload[1..]);
    // v2.4 splits multi-value text frames on NUL; v2.2/v2.3 use '/'.
    // We split on NUL unconditionally; v2.2/v2.3 frames almost never
    // have embedded NULs in practice so this is safe.
    let values: Vec<String> = text
        .split('\u{0}')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    Id3Frame::Text {
        id: id.to_string(),
        values,
    }
}

fn parse_txxx(id: &str, payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::UserText {
            description: String::new(),
            value: String::new(),
        };
    }
    let enc = payload[0];
    let rest = &payload[1..];
    let (description, after) = split_once_nul(enc, rest);
    let value = decode_text(enc, after);
    // TXXX frames with an empty description may occur; keep id for
    // Unknown fallback if caller wants it.
    let _ = id;
    Id3Frame::UserText { description, value }
}

fn parse_wxxx(id: &str, payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::UserUrl {
            description: String::new(),
            url: String::new(),
        };
    }
    let enc = payload[0];
    let rest = &payload[1..];
    let (description, after) = split_once_nul(enc, rest);
    // The URL itself is always ISO-8859-1 per spec.
    let url = latin1_trim(after);
    let _ = id;
    Id3Frame::UserUrl { description, url }
}

fn parse_url_frame(id: &str, payload: &[u8]) -> Id3Frame {
    // W*** URL frames: no encoding byte, payload is ISO-8859-1.
    let url = latin1_trim(payload);
    Id3Frame::Url {
        id: id.to_string(),
        url,
    }
}

fn parse_comm_like(payload: &[u8], lyrics: bool) -> Id3Frame {
    if payload.len() < 4 {
        let (d, t) = (String::new(), String::new());
        return if lyrics {
            Id3Frame::Lyrics {
                lang: [0; 3],
                description: d,
                text: t,
            }
        } else {
            Id3Frame::Comment {
                lang: [0; 3],
                description: d,
                text: t,
            }
        };
    }
    let enc = payload[0];
    let mut lang = [0u8; 3];
    lang.copy_from_slice(&payload[1..4]);
    let rest = &payload[4..];
    let (description, after) = split_once_nul(enc, rest);
    let text = decode_text(enc, after);
    if lyrics {
        Id3Frame::Lyrics {
            lang,
            description,
            text,
        }
    } else {
        Id3Frame::Comment {
            lang,
            description,
            text,
        }
    }
}

fn parse_apic(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Unknown {
            id: "APIC".into(),
            raw: Vec::new(),
        };
    }
    let enc = payload[0];
    let rest = &payload[1..];
    // MIME type is null-terminated ISO-8859-1.
    let (mime_bytes, after_mime) = split_once_nul_bytes(rest);
    let mime_type = latin1_trim(mime_bytes);
    if after_mime.is_empty() {
        return Id3Frame::Unknown {
            id: "APIC".into(),
            raw: payload.to_vec(),
        };
    }
    let picture_type = PictureType::from_u8(after_mime[0]);
    let after_type = &after_mime[1..];
    let (description, data) = split_once_nul(enc, after_type);
    Id3Frame::Picture(AttachedPicture {
        mime_type,
        picture_type,
        description,
        data: data.to_vec(),
    })
}

fn parse_pic(payload: &[u8]) -> Id3Frame {
    // v2.2 PIC layout: 1 encoding byte + 3 image-format bytes (JPG /
    // PNG / ...) + 1 picture-type byte + description (NUL-term'd in
    // declared encoding) + binary data.
    if payload.len() < 5 {
        return Id3Frame::Unknown {
            id: "PIC".into(),
            raw: payload.to_vec(),
        };
    }
    let enc = payload[0];
    let fmt = &payload[1..4];
    let picture_type = PictureType::from_u8(payload[4]);
    let after = &payload[5..];
    let (description, data) = split_once_nul(enc, after);
    // Turn the 3-char image format into a MIME type so the rest of
    // the workspace doesn't have to special-case v2.2.
    let mime_type = match fmt {
        b"JPG" | b"jpg" => "image/jpeg".to_string(),
        b"PNG" | b"png" => "image/png".to_string(),
        other => format!(
            "image/{}",
            std::str::from_utf8(other)
                .unwrap_or("")
                .to_ascii_lowercase()
        ),
    };
    Id3Frame::Picture(AttachedPicture {
        mime_type,
        picture_type,
        description,
        data: data.to_vec(),
    })
}

/// Parse a `POPM` popularimeter payload (spec §4.17). Layout is:
///
/// ```text
/// Email to user   <ISO-8859-1 string> $00
/// Rating          $xx
/// Counter         $xx xx xx xx (xx ...)    [optional, may grow > 4 bytes]
/// ```
///
/// The counter may be omitted entirely; if present it is at least
/// 32 bits and grows byte-by-byte once it overflows. We collect it
/// as a big-endian unsigned integer into `u64` which is enough for
/// counters up to 2^64 (the spec leaves the upper bound unbounded
/// but no real-world player will exceed `u64`).
fn parse_popm(payload: &[u8]) -> Id3Frame {
    // Email is always ISO-8859-1 (no encoding byte).
    let (email_bytes, after_email) = split_once_nul_bytes(payload);
    let email = latin1_to_string(email_bytes);
    if after_email.is_empty() {
        // Truncated: rating is missing.
        return Id3Frame::Popularimeter {
            email,
            rating: 0,
            counter: 0,
        };
    }
    let rating = after_email[0];
    let counter_bytes = &after_email[1..];
    let counter = be_unsigned(counter_bytes);
    Id3Frame::Popularimeter {
        email,
        rating,
        counter,
    }
}

/// Parse a `PCNT` play-counter payload (spec §4.16). The counter is
/// at least 32 bits and may grow byte-by-byte; we widen into `u64`.
fn parse_pcnt(payload: &[u8]) -> Id3Frame {
    Id3Frame::PlayCounter {
        count: be_unsigned(payload),
    }
}

/// Parse a `PRIV` private-frame payload (spec §4.27). Layout is:
///
/// ```text
/// Owner identifier      <ISO-8859-1 string> $00
/// The private data      <binary data>
/// ```
fn parse_priv(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, data) = split_once_nul_bytes(payload);
    Id3Frame::Private {
        owner: latin1_to_string(owner_bytes),
        data: data.to_vec(),
    }
}

/// Parse a `GEOB` general-encapsulated-object payload (spec §4.15).
/// Layout is:
///
/// ```text
/// Text encoding          $xx
/// MIME type              <ISO-8859-1 string> $00
/// Filename               <string in declared encoding> $00 (00)
/// Content description    <string in declared encoding> $00 (00)
/// Encapsulated object    <binary data>
/// ```
fn parse_geob(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Geob {
            mime_type: String::new(),
            filename: String::new(),
            description: String::new(),
            data: Vec::new(),
        };
    }
    let enc = payload[0];
    let rest = &payload[1..];
    // MIME type is always ISO-8859-1 regardless of the encoding byte.
    let (mime_bytes, after_mime) = split_once_nul_bytes(rest);
    let mime_type = latin1_to_string(mime_bytes);
    let (filename, after_fname) = split_once_nul(enc, after_mime);
    let (description, data) = split_once_nul(enc, after_fname);
    Id3Frame::Geob {
        mime_type,
        filename,
        description,
        data: data.to_vec(),
    }
}

/// Parse a `UFID` unique-file-identifier payload (spec §4.1). Layout
/// is:
///
/// ```text
/// Owner identifier        <ISO-8859-1 string> $00
/// Identifier              <up to 64 bytes binary data>
/// ```
fn parse_ufid(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, identifier) = split_once_nul_bytes(payload);
    Id3Frame::Ufid {
        owner: latin1_to_string(owner_bytes),
        identifier: identifier.to_vec(),
    }
}

/// Parse a `USER` terms-of-use payload (spec v2.3 §4.23 / v2.4 §4.22).
/// Layout is:
///
/// ```text
/// Text encoding   $xx
/// Language        $xx xx xx
/// The actual text <text string according to encoding>
/// ```
///
/// Truncated payloads (no language bytes, no text) fold to an empty
/// frame rather than erroring — the parser is structural, not
/// validating.
fn parse_user(payload: &[u8]) -> Id3Frame {
    if payload.len() < 4 {
        return Id3Frame::TermsOfUse {
            lang: *b"   ",
            text: String::new(),
        };
    }
    let enc = payload[0];
    let mut lang = [0u8; 3];
    lang.copy_from_slice(&payload[1..4]);
    let text = decode_text(enc, &payload[4..]);
    Id3Frame::TermsOfUse { lang, text }
}

/// Parse an `OWNE` ownership payload (spec v2.3 §4.24 / v2.4 §4.23).
/// Layout is:
///
/// ```text
/// Text encoding   $xx
/// Price paid      <ISO-8859-1 text> $00
/// Date of purch.  <8 chars, no terminator>
/// Seller          <text string according to encoding>
/// ```
///
/// "Price paid" is always ISO-8859-1 per the surrounding text
/// (currency code + decimal number); "Seller" follows the declared
/// encoding. If the buffer is shorter than the fixed-prefix length
/// the parser folds to an empty frame.
fn parse_owne(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Ownership {
            price: String::new(),
            date: String::new(),
            seller: String::new(),
        };
    }
    let enc = payload[0];
    let rest = &payload[1..];
    let (price_bytes, after_price) = split_once_nul_bytes(rest);
    let price = latin1_to_string(price_bytes);
    let date_len = 8usize.min(after_price.len());
    let date = latin1_to_string(&after_price[..date_len]);
    let seller_bytes = &after_price[date_len..];
    let seller = decode_text(enc, seller_bytes);
    Id3Frame::Ownership {
        price,
        date,
        seller,
    }
}

/// Parse a `COMR` commercial-frame payload (spec v2.3 §4.25 / v2.4
/// §4.24). Layout is:
///
/// ```text
/// Text encoding      $xx
/// Price string       <ISO-8859-1 text> $00
/// Valid until        <8 chars, no terminator>
/// Contact URL        <ISO-8859-1 text> $00
/// Received as        $xx
/// Name of seller     <text string according to encoding> $00 (00)
/// Description        <text string according to encoding> $00 (00)
/// Picture MIME type  <ISO-8859-1 text> $00          (optional)
/// Seller logo        <binary data>                  (optional)
/// ```
///
/// The MIME + logo block is optional and absent for most real-world
/// frames; we return empty strings + empty bytes when nothing follows
/// the description.
fn parse_comr(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return empty_comr();
    }
    let enc = payload[0];
    let rest = &payload[1..];
    let (price_bytes, after_price) = split_once_nul_bytes(rest);
    let price = latin1_to_string(price_bytes);
    if after_price.len() < 8 {
        return empty_comr();
    }
    let date = latin1_to_string(&after_price[..8]);
    let after_date = &after_price[8..];
    let (url_bytes, after_url) = split_once_nul_bytes(after_date);
    let contact_url = latin1_to_string(url_bytes);
    if after_url.is_empty() {
        return Id3Frame::Commercial {
            price,
            valid_until: date,
            contact_url,
            received_as: 0,
            seller: String::new(),
            description: String::new(),
            logo_mime: String::new(),
            logo_data: Vec::new(),
        };
    }
    let received_as = after_url[0];
    let after_recv = &after_url[1..];
    let (seller, after_seller) = split_once_nul(enc, after_recv);
    let (description, after_desc) = split_once_nul(enc, after_seller);
    // The optional logo: MIME (latin1, NUL-terminated) + binary bytes.
    // Absent when nothing follows the description.
    let (logo_mime, logo_data) = if after_desc.is_empty() {
        (String::new(), Vec::new())
    } else {
        let (mime_bytes, after_mime) = split_once_nul_bytes(after_desc);
        (latin1_to_string(mime_bytes), after_mime.to_vec())
    };
    Id3Frame::Commercial {
        price,
        valid_until: date,
        contact_url,
        received_as,
        seller,
        description,
        logo_mime,
        logo_data,
    }
}

fn empty_comr() -> Id3Frame {
    Id3Frame::Commercial {
        price: String::new(),
        valid_until: String::new(),
        contact_url: String::new(),
        received_as: 0,
        seller: String::new(),
        description: String::new(),
        logo_mime: String::new(),
        logo_data: Vec::new(),
    }
}

/// Parse a `SYTC` synchronised-tempo-codes payload (spec v2.4 §4.7).
/// Layout is:
///
/// ```text
/// Time stamp format   $xx
/// Tempo data          (<tempo> <32-bit BE timestamp>)*
/// ```
///
/// `<tempo>` is a single byte unless its value is `$FF`, in which case
/// one additional byte follows and the BPM is the sum of the two
/// (giving 2..=510 BPM, with $00 = beat-free / $01 = single stroke).
/// Truncated trailing pairs are skipped rather than rejected.
fn parse_sytc(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::SyncedTempo {
            time_format: 0,
            codes: Vec::new(),
        };
    }
    let time_format = payload[0];
    let mut codes: Vec<(u16, u32)> = Vec::new();
    let mut i = 1usize;
    while i < payload.len() {
        let (tempo, used) = if payload[i] == 0xFF {
            if i + 1 >= payload.len() {
                break;
            }
            (0xFFu16 + payload[i + 1] as u16, 2usize)
        } else {
            (payload[i] as u16, 1usize)
        };
        let ts_off = i + used;
        if ts_off + 4 > payload.len() {
            break;
        }
        let ts = regular_u32(
            payload[ts_off],
            payload[ts_off + 1],
            payload[ts_off + 2],
            payload[ts_off + 3],
        );
        codes.push((tempo, ts));
        i = ts_off + 4;
    }
    Id3Frame::SyncedTempo { time_format, codes }
}

/// Parse an `RVA2` relative-volume-adjustment-2 payload (spec v2.4
/// §4.11). Layout is:
///
/// ```text
/// Identification        <ISO-8859-1 text> $00
/// For each channel:
///   Type of channel       $xx
///   Volume adjustment     $xx xx                       (signed Q9.7 dB)
///   Bits representing peak $xx
///   Peak volume           ceil(bits / 8) bytes BE
/// ```
fn parse_rva2(payload: &[u8]) -> Id3Frame {
    let (ident_bytes, mut rest) = split_once_nul_bytes(payload);
    let identification = latin1_to_string(ident_bytes);
    let mut channels: Vec<Rva2Channel> = Vec::new();
    while rest.len() >= 4 {
        let channel_type = rest[0];
        let volume_adjustment = i16::from_be_bytes([rest[1], rest[2]]);
        let bits_peak = rest[3];
        let peak_bytes = (bits_peak as usize).div_ceil(8);
        if rest.len() < 4 + peak_bytes {
            break;
        }
        let peak = rest[4..4 + peak_bytes].to_vec();
        channels.push(Rva2Channel {
            channel_type,
            volume_adjustment,
            bits_peak,
            peak,
        });
        rest = &rest[4 + peak_bytes..];
    }
    Id3Frame::Rva2 {
        identification,
        channels,
    }
}

/// Parse an `EQU2` equalisation-2 payload (spec v2.4 §4.12). Layout is:
///
/// ```text
/// Interpolation method  $xx
/// Identification        <ISO-8859-1 text> $00
/// For each point:
///   Frequency           $xx xx     (units of 1/2 Hz, 0..32767 Hz)
///   Volume adjustment   $xx xx     (signed Q9.7 dB)
/// ```
fn parse_equ2(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Equ2 {
            interpolation: 0,
            identification: String::new(),
            points: Vec::new(),
        };
    }
    let interpolation = payload[0];
    let (ident_bytes, mut rest) = split_once_nul_bytes(&payload[1..]);
    let identification = latin1_to_string(ident_bytes);
    let mut points: Vec<(u16, i16)> = Vec::new();
    while rest.len() >= 4 {
        let freq = u16::from_be_bytes([rest[0], rest[1]]);
        let adj = i16::from_be_bytes([rest[2], rest[3]]);
        points.push((freq, adj));
        rest = &rest[4..];
    }
    Id3Frame::Equ2 {
        interpolation,
        identification,
        points,
    }
}

/// Parse an `MCDI` music CD identifier payload (spec v2.3 §4.5 /
/// v2.4 §4.4). The body is opaque binary CD-DA TOC bytes; we copy
/// it through verbatim so callers can do their own TOC analysis.
fn parse_mcdi(payload: &[u8]) -> Id3Frame {
    Id3Frame::MusicCdId {
        toc: payload.to_vec(),
    }
}

/// Parse an `ETCO` event timing codes payload (spec v2.3 §4.6 /
/// v2.4 §4.5). Layout is `time_format $xx` followed by pairs of
/// `event_type $xx + timestamp $xx xx xx xx`.
fn parse_etco(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::EventTimingCodes {
            time_format: 0,
            events: Vec::new(),
        };
    }
    let time_format = payload[0];
    let mut events: Vec<(u8, u32)> = Vec::new();
    let mut i = 1usize;
    while i + 5 <= payload.len() {
        let ev = payload[i];
        let ts = regular_u32(
            payload[i + 1],
            payload[i + 2],
            payload[i + 3],
            payload[i + 4],
        );
        events.push((ev, ts));
        i += 5;
    }
    Id3Frame::EventTimingCodes {
        time_format,
        events,
    }
}

/// Parse a `SYLT` synchronised lyrics payload (spec v2.3 §4.10 /
/// v2.4 §4.9). Layout is:
///
/// ```text
/// Text encoding        $xx
/// Language             $xx xx xx
/// Time stamp format    $xx
/// Content type         $xx
/// Content descriptor   <text> $00 (00)
/// For each sync:
///   Terminated text    <text> $00 (00)
///   Time stamp         $xx xx xx xx
/// ```
fn parse_sylt(payload: &[u8]) -> Id3Frame {
    if payload.len() < 6 {
        return Id3Frame::SyncedLyrics {
            lang: [0; 3],
            time_format: 0,
            content_type: 0,
            description: String::new(),
            syncs: Vec::new(),
        };
    }
    let enc = payload[0];
    let lang = [payload[1], payload[2], payload[3]];
    let time_format = payload[4];
    let content_type = payload[5];
    let rest = &payload[6..];
    let (description, mut after) = split_once_nul(enc, rest);
    let mut syncs: Vec<(String, u32)> = Vec::new();
    while !after.is_empty() {
        let (text, tail) = split_once_nul(enc, after);
        if tail.len() < 4 {
            // Truncated entry — keep what we got, stop.
            if !text.is_empty() {
                syncs.push((text, 0));
            }
            break;
        }
        let ts = regular_u32(tail[0], tail[1], tail[2], tail[3]);
        syncs.push((text, ts));
        after = &tail[4..];
    }
    Id3Frame::SyncedLyrics {
        lang,
        time_format,
        content_type,
        description,
        syncs,
    }
}

/// Parse a `POSS` position synchronisation payload (spec v2.3 §4.22 /
/// v2.4 §4.21). Layout: `time_format $xx` + 4-byte BE position.
fn parse_poss(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::PositionSync {
            time_format: 0,
            position: 0,
        };
    }
    let time_format = payload[0];
    let position = if payload.len() >= 5 {
        regular_u32(payload[1], payload[2], payload[3], payload[4])
    } else {
        // Spec says the position is 32 bits, but tolerate short forms
        // by zero-extending the available high bytes.
        let mut buf = [0u8; 4];
        let avail = payload.len() - 1;
        buf[4 - avail..].copy_from_slice(&payload[1..1 + avail]);
        u32::from_be_bytes(buf)
    };
    Id3Frame::PositionSync {
        time_format,
        position,
    }
}

/// Parse an `RBUF` recommended buffer size payload (spec v2.3 §4.19 /
/// v2.4 §4.18). Layout: 3-byte BE buffer size + 1-byte flags
/// (`%0000000x`) + optional 4-byte BE offset-to-next.
fn parse_rbuf(payload: &[u8]) -> Id3Frame {
    if payload.len() < 4 {
        return Id3Frame::RecommendedBuffer {
            buffer_size: 0,
            embedded_info: false,
            offset_to_next: 0,
        };
    }
    let buffer_size = regular_u24(payload[0], payload[1], payload[2]);
    let embedded_info = (payload[3] & 0x01) != 0;
    let offset_to_next = if payload.len() >= 8 {
        regular_u32(payload[4], payload[5], payload[6], payload[7])
    } else {
        0
    };
    Id3Frame::RecommendedBuffer {
        buffer_size,
        embedded_info,
        offset_to_next,
    }
}

/// Parse a `SEEK` seek-frame payload (spec v2.4 §4.29). Layout is a
/// single 32-bit BE byte offset.
fn parse_seek(payload: &[u8]) -> Id3Frame {
    let min = if payload.len() >= 4 {
        regular_u32(payload[0], payload[1], payload[2], payload[3])
    } else {
        0
    };
    Id3Frame::Seek {
        min_offset_to_next_tag: min,
    }
}

/// Parse a `SIGN` signature-frame payload (spec v2.4 §4.28). Layout:
/// 1-byte group symbol + remainder = binary signature.
fn parse_sign(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Signature {
            group_symbol: 0,
            signature: Vec::new(),
        };
    }
    Id3Frame::Signature {
        group_symbol: payload[0],
        signature: payload[1..].to_vec(),
    }
}

/// Parse a `GRID` group-identification-registration payload (spec v2.3
/// §4.27 / v2.4 §4.26). Layout: NUL-terminated owner identifier +
/// 1-byte group symbol + remainder = optional group-dependent data.
fn parse_grid(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, rest) = split_once_nul_bytes(payload);
    let owner = latin1_to_string(owner_bytes);
    if rest.is_empty() {
        return Id3Frame::GroupId {
            owner,
            group_symbol: 0,
            data: Vec::new(),
        };
    }
    Id3Frame::GroupId {
        owner,
        group_symbol: rest[0],
        data: rest[1..].to_vec(),
    }
}

/// Parse an `ENCR` encryption-method-registration payload (spec v2.3
/// §4.25 / v2.4 §4.25). Layout: NUL-terminated owner identifier +
/// 1-byte method symbol + remainder = optional encryption-specific
/// data. The on-wire shape matches `GRID`.
fn parse_encr(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, rest) = split_once_nul_bytes(payload);
    let owner = latin1_to_string(owner_bytes);
    if rest.is_empty() {
        return Id3Frame::EncryptionMethod {
            owner,
            method_symbol: 0,
            data: Vec::new(),
        };
    }
    Id3Frame::EncryptionMethod {
        owner,
        method_symbol: rest[0],
        data: rest[1..].to_vec(),
    }
}

/// Parse an `AENC` audio-encryption payload (spec v2.3 §4.26 / v2.4
/// §4.19). Layout: NUL-terminated owner identifier + 2-byte BE
/// preview-start + 2-byte BE preview-length + opaque encryption-info.
fn parse_aenc(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, rest) = split_once_nul_bytes(payload);
    let owner = latin1_to_string(owner_bytes);
    if rest.len() < 4 {
        return Id3Frame::AudioEncryption {
            owner,
            preview_start: 0,
            preview_length: 0,
            encryption_info: rest.to_vec(),
        };
    }
    let preview_start = u16::from_be_bytes([rest[0], rest[1]]);
    let preview_length = u16::from_be_bytes([rest[2], rest[3]]);
    let encryption_info = rest[4..].to_vec();
    Id3Frame::AudioEncryption {
        owner,
        preview_start,
        preview_length,
        encryption_info,
    }
}

/// Parse a `LINK` linked-information payload (spec v2.3 §4.21 /
/// v2.4 §4.20). v2.3 uses a 3-byte frame identifier while v2.4 uses
/// 4 bytes. We disambiguate by scanning the *next* byte after a
/// 3-character ASCII id triple: if it's a 4th ASCII upper/digit
/// character we treat the id as 4 bytes (v2.4); otherwise the 3-byte
/// v2.3 form, with the 4th array slot zero-padded for representation.
fn parse_link(payload: &[u8]) -> Id3Frame {
    if payload.len() < 3 {
        return Id3Frame::LinkedInfo {
            frame_id: [0; 4],
            url: String::new(),
            additional: Vec::new(),
        };
    }
    let is_v24_id = payload.len() >= 4 && is_id_char(payload[3]);
    let (frame_id, body): ([u8; 4], &[u8]) = if is_v24_id {
        (
            [payload[0], payload[1], payload[2], payload[3]],
            &payload[4..],
        )
    } else {
        ([payload[0], payload[1], payload[2], 0], &payload[3..])
    };
    let (url_bytes, additional_bytes) = split_once_nul_bytes(body);
    let url = latin1_to_string(url_bytes);
    Id3Frame::LinkedInfo {
        frame_id,
        url,
        additional: additional_bytes.to_vec(),
    }
}

/// Parse an `ASPI` audio-seek-point-index payload (spec v2.4 §4.30).
/// Layout:
///
/// ```text
///   Indexed data start (S)     $xx xx xx xx        (BE u32)
///   Indexed data length (L)    $xx xx xx xx        (BE u32)
///   Number of index points (N) $xx xx              (BE u16)
///   Bits per index point (b)   $xx                 (8 or 16)
///   Fraction at index (Fi)     $xx (xx)            (N entries, 1 or 2 bytes each)
/// ```
///
/// The fraction width depends on `bits_per_index_point`; values other
/// than 8 or 16 are accepted as a passthrough (fractions stay empty)
/// rather than rejected, so callers see the malformed-but-not-fatal
/// header and can decide how to react. Truncated trailing bytes in the
/// fraction list are dropped at parse time.
fn parse_aspi(payload: &[u8]) -> Id3Frame {
    if payload.len() < 11 {
        return Id3Frame::AudioSeekPointIndex {
            indexed_data_start: 0,
            indexed_data_length: 0,
            bits_per_index_point: 0,
            fractions: Vec::new(),
        };
    }
    let indexed_data_start = regular_u32(payload[0], payload[1], payload[2], payload[3]);
    let indexed_data_length = regular_u32(payload[4], payload[5], payload[6], payload[7]);
    let n = u16::from_be_bytes([payload[8], payload[9]]) as usize;
    let bits = payload[10];
    let body = &payload[11..];
    let mut fractions = Vec::with_capacity(n);
    match bits {
        8 => {
            let take = n.min(body.len());
            for &b in &body[..take] {
                fractions.push(b as u16);
            }
        }
        16 => {
            let take = n.min(body.len() / 2);
            for i in 0..take {
                fractions.push(u16::from_be_bytes([body[i * 2], body[i * 2 + 1]]));
            }
        }
        _ => {
            // Non-conforming width — pass through with the header
            // captured but no fractions decoded; a downstream consumer
            // can match on `bits_per_index_point` and inspect the raw
            // form if needed.
        }
    }
    Id3Frame::AudioSeekPointIndex {
        indexed_data_start,
        indexed_data_length,
        bits_per_index_point: bits,
        fractions,
    }
}

/// Parse an `MLLT` MPEG location lookup table payload (spec v2.3 §4.7 /
/// v2.4 §4.6). Layout:
///
/// ```text
/// MPEG frames between reference  $xx xx
/// Bytes between reference        $xx xx xx
/// Milliseconds between reference $xx xx xx
/// Bits for bytes deviation       $xx
/// Bits for milliseconds dev.     $xx
/// For each reference:
///   Deviation in bytes           %xxx....   (bits_for_bytes_deviation bits)
///   Deviation in milliseconds    %xxx....   (bits_for_ms_deviation bits)
/// ```
///
/// The two deviation widths together (per reference) must be a multiple
/// of four bits per spec. The parser tolerates a non-multiple-of-four
/// sum by stopping once the remaining bits in the payload can no longer
/// feed one complete reference. Per-reference widths above 32 bits are
/// rejected — the descriptor is preserved but the references are
/// dropped because we cannot fit them in `(u32, u32)`.
fn parse_mllt(payload: &[u8]) -> Id3Frame {
    if payload.len() < 10 {
        // Truncated descriptor — preserve the raw bytes since a partial
        // MLLT is not interpretable.
        return Id3Frame::Unknown {
            id: "MLLT".to_string(),
            raw: payload.to_vec(),
        };
    }
    let mpeg_frames_between_reference = u16::from_be_bytes([payload[0], payload[1]]);
    let bytes_between_reference = regular_u24(payload[2], payload[3], payload[4]);
    let ms_between_reference = regular_u24(payload[5], payload[6], payload[7]);
    let bits_for_bytes_deviation = payload[8];
    let bits_for_ms_deviation = payload[9];
    let body = &payload[10..];
    let mut references: Vec<(u32, u32)> = Vec::new();
    if bits_for_bytes_deviation <= 32 && bits_for_ms_deviation <= 32 {
        let total_bits = bits_for_bytes_deviation as usize + bits_for_ms_deviation as usize;
        if total_bits > 0 && body.len().saturating_mul(8) >= total_bits {
            // Pull references MSB-first across byte boundaries.
            let mut reader = BitReader::new(body);
            while reader.remaining() >= total_bits {
                let bytes_dev = reader.take(bits_for_bytes_deviation as usize);
                let ms_dev = reader.take(bits_for_ms_deviation as usize);
                references.push((bytes_dev, ms_dev));
            }
        }
    }
    Id3Frame::MpegLocationLookup {
        mpeg_frames_between_reference,
        bytes_between_reference,
        ms_between_reference,
        bits_for_bytes_deviation,
        bits_for_ms_deviation,
        references,
    }
}

/// Parse an `RVRB` reverb-frame payload (spec v2.3 §4.13 / v2.4 §4.13).
/// The layout is a fixed twelve bytes:
///
/// ```text
///   reverb_left_ms      $xx xx   (u16 BE)
///   reverb_right_ms     $xx xx   (u16 BE)
///   bounces_left        $xx
///   bounces_right       $xx
///   feedback_ll         $xx
///   feedback_lr         $xx
///   feedback_rr         $xx
///   feedback_rl         $xx
///   premix_lr           $xx
///   premix_rl           $xx
/// ```
///
/// A payload shorter than 12 bytes is preserved verbatim through
/// [`Id3Frame::Unknown`] — the layout is exact-size with no
/// terminators and a truncated frame cannot be interpreted
/// unambiguously. A payload longer than 12 bytes is treated as a
/// well-formed reverb followed by spurious trailing bytes per spec
/// "all the unknown bytes in a frame should be skipped"; the leading
/// 12 are decoded and the rest dropped.
fn parse_rvrb(payload: &[u8]) -> Id3Frame {
    if payload.len() < 12 {
        return Id3Frame::Unknown {
            id: "RVRB".to_string(),
            raw: payload.to_vec(),
        };
    }
    let reverb_left_ms = u16::from_be_bytes([payload[0], payload[1]]);
    let reverb_right_ms = u16::from_be_bytes([payload[2], payload[3]]);
    Id3Frame::Reverb {
        reverb_left_ms,
        reverb_right_ms,
        bounces_left: payload[4],
        bounces_right: payload[5],
        feedback_ll: payload[6],
        feedback_lr: payload[7],
        feedback_rr: payload[8],
        feedback_rl: payload[9],
        premix_lr: payload[10],
        premix_rl: payload[11],
    }
}

/// MSB-first bit reader for the `MLLT` per-reference deviation pair.
/// The spec packs `bits_for_bytes_deviation + bits_for_ms_deviation`
/// bits per reference across byte boundaries, with the requirement that
/// the sum is a multiple of four; the reader does not assume that
/// requirement so a truncated or non-conforming sum is handled by
/// stopping at the boundary rather than over-reading.
struct BitReader<'a> {
    buf: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, bit_pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() * 8 - self.bit_pos
    }

    /// Pull `n` bits (0 ≤ n ≤ 32) MSB-first into a `u32`. Out-of-buffer
    /// reads zero-fill, but callers gate on [`Self::remaining`] first.
    fn take(&mut self, n: usize) -> u32 {
        debug_assert!(n <= 32, "MLLT bit width clamped at 32");
        let mut value: u32 = 0;
        for _ in 0..n {
            let byte_idx = self.bit_pos >> 3;
            let bit_in_byte = 7 - (self.bit_pos & 7);
            let bit = if byte_idx < self.buf.len() {
                (self.buf[byte_idx] >> bit_in_byte) & 1
            } else {
                0
            };
            value = (value << 1) | bit as u32;
            self.bit_pos += 1;
        }
        value
    }
}

/// MSB-first bit writer dual to [`BitReader`]. Used by the `MLLT`
/// encoder to pack each `(bytes_dev, ms_dev)` pair across byte
/// boundaries per spec §4.7 / §4.6.
struct BitWriter {
    buf: Vec<u8>,
    bit_pos: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            bit_pos: 0,
        }
    }

    /// Push the low `n` bits of `value` MSB-first.
    fn push(&mut self, value: u32, n: usize) {
        debug_assert!(n <= 32, "MLLT bit width clamped at 32");
        for i in (0..n).rev() {
            let bit = ((value >> i) & 1) as u8;
            let byte_idx = self.bit_pos >> 3;
            let bit_in_byte = 7 - (self.bit_pos & 7);
            if byte_idx >= self.buf.len() {
                self.buf.push(0);
            }
            self.buf[byte_idx] |= bit << bit_in_byte;
            self.bit_pos += 1;
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// True for the upper-ASCII letters / digits used in ID3v2 frame
/// identifiers. The spec restricts frame ids to `A-Z 0-9` so the
/// LINK 3-vs-4 disambiguator can rely on this character class.
fn is_id_char(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

/// Decode a big-endian unsigned integer of arbitrary width into `u64`.
/// Used for `POPM` / `PCNT` counters which start at 32 bits and may
/// grow byte-by-byte beyond u32::MAX per spec §4.16. Buffers wider
/// than 8 bytes saturate to `u64::MAX` if any of the high bytes
/// dropped are non-zero, so an absurdly large counter still reads as
/// "very big" instead of silently wrapping.
fn be_unsigned(buf: &[u8]) -> u64 {
    if buf.len() > 8 {
        let high = &buf[..buf.len() - 8];
        if high.iter().any(|&b| b != 0) {
            return u64::MAX;
        }
    }
    let take = buf.len().min(8);
    let skip = buf.len() - take;
    let mut v: u64 = 0;
    for &b in &buf[skip..] {
        v = (v << 8) | b as u64;
    }
    v
}

/// Split `buf` on the first terminator for the given encoding,
/// returning (decoded_prefix, remainder_after_terminator). For 1-byte
/// encodings the terminator is `0x00`; for UTF-16 variants it is
/// `0x00 0x00` aligned on even offsets.
fn split_once_nul(enc: u8, buf: &[u8]) -> (String, &[u8]) {
    if enc == 1 || enc == 2 {
        // UTF-16: find a 2-byte NUL on an even boundary.
        let mut i = 0;
        while i + 1 < buf.len() {
            if buf[i] == 0 && buf[i + 1] == 0 {
                let prefix = decode_text(enc, &buf[..i]);
                return (prefix, &buf[i + 2..]);
            }
            i += 2;
        }
        (decode_text(enc, buf), &[])
    } else if let Some(pos) = buf.iter().position(|&b| b == 0) {
        let prefix = decode_text(enc, &buf[..pos]);
        (prefix, &buf[pos + 1..])
    } else {
        (decode_text(enc, buf), &[])
    }
}

/// Raw-bytes variant of [`split_once_nul`] that doesn't interpret the
/// declared encoding — used for MIME type fields which are always
/// ISO-8859-1 regardless of the frame-level encoding byte.
fn split_once_nul_bytes(buf: &[u8]) -> (&[u8], &[u8]) {
    if let Some(pos) = buf.iter().position(|&b| b == 0) {
        (&buf[..pos], &buf[pos + 1..])
    } else {
        (buf, &[])
    }
}

fn decode_text(enc: u8, buf: &[u8]) -> String {
    let s = match enc {
        0 => latin1_to_string(buf),
        1 => decode_utf16_bom(buf),
        2 => decode_utf16_be(buf),
        3 => String::from_utf8_lossy(buf).to_string(),
        _ => latin1_to_string(buf),
    };
    // Trim trailing NULs — many taggers pad strings with them.
    s.trim_end_matches('\u{0}').to_string()
}

fn latin1_to_string(buf: &[u8]) -> String {
    buf.iter().map(|&b| b as char).collect()
}

fn latin1_trim(buf: &[u8]) -> String {
    latin1_to_string(buf).trim_end_matches('\u{0}').to_string()
}

/// Decode a fixed-width ID3v1 text field. ID3v1 pads short strings
/// with NUL *or* spaces; we strip both from the trailing edge.
fn v1_string(buf: &[u8]) -> String {
    // Truncate at first NUL — anything after is padding.
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    latin1_to_string(&buf[..end])
        .trim_end_matches(' ')
        .to_string()
}

fn decode_utf16_bom(buf: &[u8]) -> String {
    if buf.len() < 2 {
        return String::new();
    }
    let (body, le) = match (buf[0], buf[1]) {
        (0xFF, 0xFE) => (&buf[2..], true),
        (0xFE, 0xFF) => (&buf[2..], false),
        _ => (buf, true), // Assume LE if missing a BOM.
    };
    decode_utf16_body(body, le)
}

fn decode_utf16_be(buf: &[u8]) -> String {
    decode_utf16_body(buf, false)
}

fn decode_utf16_body(buf: &[u8], little_endian: bool) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(buf.len() / 2);
    let mut i = 0;
    while i + 1 < buf.len() {
        let u = if little_endian {
            u16::from_le_bytes([buf[i], buf[i + 1]])
        } else {
            u16::from_be_bytes([buf[i], buf[i + 1]])
        };
        units.push(u);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

/// Map an ID3v2 text frame id to the Vorbis-style key the rest of
/// the workspace uses. Recognises the common frames and falls back
/// to a lowercased raw id for the rest.
fn text_frame_to_key(id: &str) -> String {
    match id {
        "TIT2" => "title",
        "TIT1" => "contentgroup",
        "TIT3" => "subtitle",
        "TPE1" => "artist",
        "TPE2" => "albumartist",
        "TPE3" => "conductor",
        "TPE4" => "remixer",
        "TALB" => "album",
        "TYER" => "date",
        "TDRC" => "date",
        "TDRL" => "releasedate",
        "TDOR" => "originaldate",
        "TCON" => "genre",
        "TRCK" => "track",
        "TPOS" => "disc",
        "TCOM" => "composer",
        "TEXT" => "lyricist",
        "TLAN" => "language",
        "TPUB" => "publisher",
        "TCOP" => "copyright",
        "TENC" => "encodedby",
        "TSSE" => "encoder",
        "TBPM" => "bpm",
        "TCMP" => "compilation",
        "TKEY" => "key",
        "TMED" => "media",
        "TOAL" => "originalalbum",
        "TOPE" => "originalartist",
        "TOLY" => "originallyricist",
        "TORY" => "originalyear",
        "TSRC" => "isrc",
        _ => {
            // Unknown T-frame: expose the raw id lowercased so callers
            // don't drop data silently.
            return id.to_ascii_lowercase();
        }
    }
    .to_string()
}

fn push_unique(out: &mut Vec<(String, String)>, key: String, value: String) {
    if !out.iter().any(|(k, v)| *k == key && *v == value) {
        out.push((key, value));
    }
}

/// Lookup table for ID3v1's genre byte. Covers Winamp's extended
/// ID3v1.1 set (0..191). Indexes beyond the table (or the sentinel
/// 0xFF = "no genre") return None.
fn id3v1_genre(b: u8) -> Option<&'static str> {
    const GENRES: &[&str] = &[
        "Blues",
        "Classic Rock",
        "Country",
        "Dance",
        "Disco",
        "Funk",
        "Grunge",
        "Hip-Hop",
        "Jazz",
        "Metal",
        "New Age",
        "Oldies",
        "Other",
        "Pop",
        "R&B",
        "Rap",
        "Reggae",
        "Rock",
        "Techno",
        "Industrial",
        "Alternative",
        "Ska",
        "Death Metal",
        "Pranks",
        "Soundtrack",
        "Euro-Techno",
        "Ambient",
        "Trip-Hop",
        "Vocal",
        "Jazz+Funk",
        "Fusion",
        "Trance",
        "Classical",
        "Instrumental",
        "Acid",
        "House",
        "Game",
        "Sound Clip",
        "Gospel",
        "Noise",
        "AlternRock",
        "Bass",
        "Soul",
        "Punk",
        "Space",
        "Meditative",
        "Instrumental Pop",
        "Instrumental Rock",
        "Ethnic",
        "Gothic",
        "Darkwave",
        "Techno-Industrial",
        "Electronic",
        "Pop-Folk",
        "Eurodance",
        "Dream",
        "Southern Rock",
        "Comedy",
        "Cult",
        "Gangsta",
        "Top 40",
        "Christian Rap",
        "Pop/Funk",
        "Jungle",
        "Native American",
        "Cabaret",
        "New Wave",
        "Psychadelic",
        "Rave",
        "Showtunes",
        "Trailer",
        "Lo-Fi",
        "Tribal",
        "Acid Punk",
        "Acid Jazz",
        "Polka",
        "Retro",
        "Musical",
        "Rock & Roll",
        "Hard Rock",
        "Folk",
        "Folk-Rock",
        "National Folk",
        "Swing",
        "Fast Fusion",
        "Bebob",
        "Latin",
        "Revival",
        "Celtic",
        "Bluegrass",
        "Avantgarde",
        "Gothic Rock",
        "Progressive Rock",
        "Psychedelic Rock",
        "Symphonic Rock",
        "Slow Rock",
        "Big Band",
        "Chorus",
        "Easy Listening",
        "Acoustic",
        "Humour",
        "Speech",
        "Chanson",
        "Opera",
        "Chamber Music",
        "Sonata",
        "Symphony",
        "Booty Bass",
        "Primus",
        "Porn Groove",
        "Satire",
        "Slow Jam",
        "Club",
        "Tango",
        "Samba",
        "Folklore",
        "Ballad",
        "Power Ballad",
        "Rhythmic Soul",
        "Freestyle",
        "Duet",
        "Punk Rock",
        "Drum Solo",
        "A capella",
        "Euro-House",
        "Dance Hall",
        "Goa",
        "Drum & Bass",
        "Club-House",
        "Hardcore",
        "Terror",
        "Indie",
        "BritPop",
        "Negerpunk",
        "Polsk Punk",
        "Beat",
        "Christian Gangsta Rap",
        "Heavy Metal",
        "Black Metal",
        "Crossover",
        "Contemporary Christian",
        "Christian Rock",
        "Merengue",
        "Salsa",
        "Thrash Metal",
        "Anime",
        "JPop",
        "Synthpop",
    ];
    GENRES.get(b as usize).copied()
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Serialise an [`Id3Tag`] to ID3v2 on-disk bytes.
///
/// `target_version` must be [`Id3Version::V2_3`] or [`Id3Version::V2_4`];
/// v2.2 is a read-only legacy format and [`Id3Version::V1`] is handled
/// by [`write_id3v1`] instead. Frames are written in the order they
/// appear in the tag.
///
/// * `Id3Frame::Text` — written as a standard `T***` frame with
///   encoding byte `3` (UTF-8) for v2.4 tags and encoding byte `1`
///   (UTF-16 with BOM) for v2.3 tags so non-ASCII content survives both
///   versions. Multi-value frames join with NUL for v2.4 and with `/`
///   for v2.3, matching what the parser splits on.
/// * `Id3Frame::Comment` / `Id3Frame::Lyrics` — encoded with the same
///   rules, preserving the language tag and description.
/// * `Id3Frame::UserText` / `UserUrl` — encoded as `TXXX` / `WXXX`.
/// * `Id3Frame::Url` — encoded as the given 4-char `W***` id, ISO-8859-1
///   payload.
/// * `Id3Frame::Picture` — encoded as `APIC` with the picture type,
///   MIME, description and raw bytes round-tripped verbatim.
/// * `Id3Frame::Unknown` — the raw payload is written verbatim under
///   the frame id (after promotion from v2.2 to v2.3 where applicable).
///
/// The resulting buffer starts with the 10-byte ID3v2 header and can be
/// prepended directly to an MP3 or other audio file.
pub fn write_tag(tag: &Id3Tag, target_version: Id3Version) -> Result<Vec<u8>> {
    write_tag_with_options(tag, target_version, &WriteOptions::default())
}

/// Strategy for inserting unsynchronisation `$00` bytes into a tag's
/// serialised body (spec §6.1). Unsync is the mechanism that hides
/// the MPEG sync pattern `%11111111 111xxxxx` (plus literal `$FF $00`
/// runs) from naive MPEG decoders that might otherwise mistake an ID3
/// tag for the start of an audio frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnsyncMode {
    /// No unsynchronisation. The tag header flag bit 0x80 is clear,
    /// no per-frame unsync format-flag bit is set, and the body is
    /// written verbatim. This is the historical default for the
    /// `write_tag` shorthand and remains the default of
    /// [`WriteOptions`].
    #[default]
    None,
    /// Whole-tag unsynchronisation. The entire serialised body
    /// (every frame header plus payload) is passed through
    /// [`apply_unsync`] and the header flag bit 0x80 is set. The
    /// 28-bit synchsafe size in the header reflects the
    /// post-unsync byte count, matching what the spec says callers
    /// should compute. Suitable for both v2.3 and v2.4 (v2.4
    /// permits whole-tag unsync as an "all frames" shortcut per
    /// §3.1).
    WholeTag,
    /// Per-frame unsynchronisation. v2.4-only: each individual
    /// frame body is unsynchronised independently, the frame's
    /// format-flag bit 0x02 is set, and the frame size in its
    /// header reflects the post-unsync length. The tag-header
    /// flag bit 0x80 is deliberately left *clear* — spec §6.1
    /// recommends ("SHOULD") setting it when every frame is
    /// unsynchronised, but this crate's parser treats the v2.4
    /// header bit as a whole-tag-body unsync signal and would
    /// double-reverse the bytes if it were also set here. The
    /// per-frame format-flag bit (spec §4.1.2) is the
    /// v2.4-authoritative location and is unambiguous on its own.
    /// Selecting `PerFrame` under a v2.3 target falls back to
    /// [`UnsyncMode::WholeTag`] (v2.3 has no per-frame format-flag
    /// byte for the unsync bit).
    PerFrame,
}

/// Options bag for [`write_tag_with_options`]. Constructed via
/// [`WriteOptions::default`] or [`WriteOptions::new`] and tweaked
/// with the builder-style setters.
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteOptions {
    pub unsync: UnsyncMode,
    /// Emit an extended header carrying a CRC-32 over the tag's frame
    /// data (spec §3.2 in both v2.3 and v2.4). Default `false`: no
    /// extended header is written.
    ///
    /// * v2.3 — the 10-byte extended header (4-byte size = 10, 2-byte
    ///   flags = `%10000000 00000000`, 4-byte size-of-padding = 0,
    ///   4-byte CRC-32) is inserted between the tag header and the
    ///   frames. The CRC covers the frame area only (no padding is
    ///   emitted alongside) per spec §3.2.
    /// * v2.4 — a 12-byte extended header (4-byte synchsafe size = 12,
    ///   1-byte flag-count = 1, 1-byte flags = 0x20, 1-byte CRC
    ///   data-length = 5, 5-byte synchsafe CRC-32) is inserted. The
    ///   CRC covers everything after the extended header (i.e. the
    ///   frames; this writer emits no padding, so frames-only equals
    ///   frames-plus-padding here) per spec §3.2.
    ///
    /// Note that the v2.3 spec says the CRC "should be calculated
    /// before unsynchronisation"; we always compute the CRC on the
    /// pre-unsync frame bytes, then run [`UnsyncMode::WholeTag`] over
    /// the concatenation of (extended header + frames). The parser
    /// reverses unsync first and then verifies the CRC against the
    /// reversed bytes, so the round-trip is exact for any combination
    /// of `crc` and `with_unsync`.
    pub crc: bool,
    /// Emit an ID3v2.4 footer (spec §3.4). Default `false`.
    ///
    /// When set, the tag-header bit 0x10 is set and a 10-byte trailer
    /// is appended after the body. The trailer's layout is a copy of
    /// the header bytes (same flags, same synchsafe size) but with
    /// identifier `b"3DI"` instead of `b"ID3"`. Per spec §3.4 this
    /// is REQUIRED for tags appended after the audio data so a reader
    /// can locate them by scanning backwards.
    ///
    /// Footer is a v2.4-only construct — the spec only defines it for
    /// v2.4. Requesting `footer = true` against an
    /// [`Id3Version::V2_3`] target returns
    /// [`Error::unsupported`]; we deliberately do NOT silently drop
    /// the flag because the caller asking for "append a footer" almost
    /// certainly wants a v2.4 file.
    pub footer: bool,
}

impl WriteOptions {
    /// Equivalent to [`WriteOptions::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style setter for the unsync strategy.
    pub fn with_unsync(mut self, mode: UnsyncMode) -> Self {
        self.unsync = mode;
        self
    }

    /// Builder-style setter for the extended-header CRC emission flag.
    /// See [`WriteOptions::crc`] for the per-version on-wire layout
    /// the writer produces.
    pub fn with_crc(mut self, enabled: bool) -> Self {
        self.crc = enabled;
        self
    }

    /// Builder-style setter for footer emission (spec §3.4, v2.4 only).
    /// See [`WriteOptions::footer`] for the on-wire layout.
    pub fn with_footer(mut self, enabled: bool) -> Self {
        self.footer = enabled;
        self
    }
}

/// Serialise an [`Id3Tag`] to the ID3v2 wire format with caller-supplied
/// options (currently: which unsynchronisation strategy to apply).
///
/// The output is always a self-contained tag with a 10-byte header
/// followed by the body; the synchsafe size field reflects the
/// post-unsync byte count. The output round-trips through [`parse_tag`]
/// for any of the three [`UnsyncMode`] settings — the parser detects
/// and reverses unsync from the header / per-frame flags identically
/// regardless of which mode produced the bytes.
pub fn write_tag_with_options(
    tag: &Id3Tag,
    target_version: Id3Version,
    options: &WriteOptions,
) -> Result<Vec<u8>> {
    let major: u8 = match target_version {
        Id3Version::V2_3 => 3,
        Id3Version::V2_4 => 4,
        Id3Version::V2_2 => {
            return Err(Error::unsupported(
                "writing ID3v2.2 is not supported; retag as v2.3 or v2.4",
            ));
        }
        Id3Version::V1 => {
            return Err(Error::unsupported(
                "use write_id3v1 to serialise an ID3v1 trailer",
            ));
        }
    };

    // Footer is defined only in ID3v2.4 (spec §3.4). Reject loudly
    // rather than silently dropping the flag — a caller asking for an
    // appended tag almost certainly wants v2.4 and would otherwise get
    // a tag the reader can't locate on a backwards scan.
    if options.footer && !matches!(target_version, Id3Version::V2_4) {
        return Err(Error::unsupported(
            "ID3v2 footer is v2.4-only; set target_version = V2_4 or clear footer",
        ));
    }

    // PerFrame is v2.4-only (v2.3 lacks the per-frame format-flag
    // byte the spec defines the bit in). Downgrade silently to
    // WholeTag rather than erroring — callers asked for "unsync"
    // and we delivered the closest available form.
    let effective_unsync = match (options.unsync, target_version) {
        (UnsyncMode::PerFrame, Id3Version::V2_3) => UnsyncMode::WholeTag,
        (mode, _) => mode,
    };

    let mut frame_bytes = Vec::new();
    for frame in &tag.frames {
        let frame_unsync = matches!(effective_unsync, UnsyncMode::PerFrame);
        write_frame_with_options(target_version, frame, frame_unsync, &mut frame_bytes)?;
    }

    // Optional extended header. We always emit the minimal CRC form
    // (no update / restrictions data), with size-of-padding = 0 in
    // v2.3 since this writer emits no padding. The CRC is computed on
    // the pre-unsync frame bytes — the v2.3 spec mandates this
    // ("calculated before unsynchronisation"), and for v2.4 it is the
    // natural interpretation since the parser always reverses unsync
    // before walking the extended header.
    let ext_header = if options.crc {
        Some(build_extended_header_crc(target_version, &frame_bytes)?)
    } else {
        None
    };

    let mut body = Vec::new();
    if let Some(ref ext) = ext_header {
        body.extend_from_slice(ext);
    }
    body.extend_from_slice(&frame_bytes);

    if matches!(effective_unsync, UnsyncMode::WholeTag) {
        body = apply_unsync(&body);
    }

    let size = body.len();
    if size >= 1 << 28 {
        return Err(Error::invalid(
            "ID3v2 tag body exceeds the 28-bit synchsafe size limit",
        ));
    }

    // Spec §6.1 says "If all frames in the tag are unsynchronised the
    // unsynchronisation flag in the tag header SHOULD be set." In v2.4
    // however, the bit's meaning is overloaded by this crate's parser:
    // when the v2.4 header bit is set, `parse_tag` runs `reverse_unsync`
    // over the *entire body* before walking frame headers. Setting it
    // alongside per-frame unsync would therefore double-decode and
    // corrupt the recovered payload. We deliberately leave the header
    // bit clear under PerFrame mode and let the per-frame format-flag
    // bit (0x02) carry the signal, which is the v2.4-authoritative
    // location per spec §4.1.2.
    let mut flags: u8 = match (effective_unsync, target_version) {
        (UnsyncMode::None, _) => 0,
        (UnsyncMode::WholeTag, _) => 0x80,
        // PerFrame is v2.4-only at this point (v2.3 was downgraded
        // above). Header bit stays clear so the parser does not
        // double-reverse the body.
        (UnsyncMode::PerFrame, _) => 0,
    };
    // Extended-header bit (bit 6) signals to the parser that the body
    // opens with an extended header.
    if ext_header.is_some() {
        flags |= 0x40;
    }
    // Footer-present bit (bit 4) signals a 10-byte "3DI..." trailer
    // after the body. The footer's own flags byte mirrors the header's
    // flags byte (spec §3.4 "the footer is a copy of the header"),
    // so we set this *before* serialising either copy of the flags
    // byte to keep header and footer byte-identical except for the
    // identifier.
    if options.footer {
        flags |= 0x10;
    }

    let footer_len = if options.footer { 10 } else { 0 };
    let mut out = Vec::with_capacity(ID3V2_HEADER_SIZE + size + footer_len);
    out.extend_from_slice(b"ID3");
    out.push(major);
    out.push(0); // revision
    out.push(flags);
    let s = size as u32;
    let s0 = ((s >> 21) & 0x7F) as u8;
    let s1 = ((s >> 14) & 0x7F) as u8;
    let s2 = ((s >> 7) & 0x7F) as u8;
    let s3 = (s & 0x7F) as u8;
    out.push(s0);
    out.push(s1);
    out.push(s2);
    out.push(s3);
    out.extend_from_slice(&body);
    if options.footer {
        // Spec §3.4: footer identifier is "3DI"; the rest of the
        // 10-byte block reproduces the header's version, flags, and
        // synchsafe size verbatim.
        out.extend_from_slice(b"3DI");
        out.push(major);
        out.push(0); // revision
        out.push(flags);
        out.push(s0);
        out.push(s1);
        out.push(s2);
        out.push(s3);
    }
    Ok(out)
}

/// Serialise the contents of an [`Id3Tag`] as a 128-byte ID3v1.1
/// trailer. The standard text fields (title, artist, album, date,
/// comment, track, genre) are pulled from the tag's frames; everything
/// else is dropped since ID3v1 has no room for it.
///
/// Field lengths and padding follow the ID3v1 spec: strings are ASCII
/// / ISO-8859-1, truncated to the field width and NUL-padded. If a
/// `TRCK` frame is present and parses as a `1..=255` integer, the
/// trailer is written in ID3v1.1 form (28-byte comment + NUL + track
/// byte). Unknown genre names fall back to byte `255` ("no genre").
pub fn write_id3v1(tag: &Id3Tag) -> Vec<u8> {
    let kv = to_key_value_pairs(tag);
    let get = |key: &str| -> String {
        kv.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };

    let mut out = vec![0u8; 128];
    out[0..3].copy_from_slice(b"TAG");
    write_v1_field(&mut out[3..33], &get("title"));
    write_v1_field(&mut out[33..63], &get("artist"));
    write_v1_field(&mut out[63..93], &get("album"));
    // Year is 4 chars; take the first 4 digits of whatever `date` holds.
    let year: String = get("date").chars().take(4).collect();
    write_v1_field(&mut out[93..97], &year);

    let comment = get("comment");
    let track_byte: Option<u8> = get("track")
        .split('/')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| (1..=255).contains(&n))
        .map(|n| n as u8);

    if let Some(t) = track_byte {
        write_v1_field(&mut out[97..125], &comment);
        out[125] = 0;
        out[126] = t;
    } else {
        write_v1_field(&mut out[97..127], &comment);
    }
    out[127] = id3v1_genre_index(&get("genre")).unwrap_or(0xFF);
    out
}

fn write_v1_field(dst: &mut [u8], s: &str) {
    for b in dst.iter_mut() {
        *b = 0;
    }
    // ISO-8859-1: code points < 256 map to the same byte value; drop
    // anything higher so we never emit multi-byte UTF-8 into a v1 field.
    let mut i = 0;
    for ch in s.chars() {
        if i >= dst.len() {
            break;
        }
        let c = ch as u32;
        if c < 256 {
            dst[i] = c as u8;
            i += 1;
        }
    }
}

/// Build the bytes of a CRC-bearing extended header for `target_version`,
/// computed against the given pre-unsync frame data. The output is
/// inserted between the 10-byte tag header and the frame bytes.
///
/// v2.3 layout (14 bytes total on the wire):
///
/// ```text
/// 00 00 00 0A      — extended-header size (excludes itself; with CRC
///                    present we emit 10 since the spec announces
///                    "currently 6 or 10 bytes, excludes itself")
/// 80 00            — extended flags, bit 15 (CRC present)
/// 00 00 00 00      — size of padding (this writer emits none)
/// xx xx xx xx      — total frame CRC-32 (regular u32)
/// ```
///
/// v2.4 layout (12 bytes total):
///
/// ```text
/// 00 00 00 0C      — synchsafe ext-header size, INCLUDES itself (=12)
/// 01               — number of flag bytes
/// 20               — flags %00100000 (bit c = CRC present)
/// 05               — CRC attached-data length
/// xx xx xx xx xx   — CRC-32 as 5 * %0xxxxxxx (35-bit synchsafe)
/// ```
fn build_extended_header_crc(target_version: Id3Version, frame_bytes: &[u8]) -> Result<Vec<u8>> {
    let crc = crc32_iso3309(frame_bytes);
    match target_version {
        Id3Version::V2_3 => {
            let mut out = Vec::with_capacity(14);
            // size = 10 (excludes itself), so total ext-area = 4 + 10 = 14
            out.extend_from_slice(&10u32.to_be_bytes());
            // flags: bit 15 (CRC present), all others clear
            out.push(0x80);
            out.push(0x00);
            // size of padding = 0 (no padding emitted)
            out.extend_from_slice(&0u32.to_be_bytes());
            // total frame CRC
            out.extend_from_slice(&crc.to_be_bytes());
            Ok(out)
        }
        Id3Version::V2_4 => {
            let mut out = Vec::with_capacity(12);
            // size = 12 (INCLUDES itself), synchsafe
            let s: u32 = 12;
            out.push(((s >> 21) & 0x7F) as u8);
            out.push(((s >> 14) & 0x7F) as u8);
            out.push(((s >> 7) & 0x7F) as u8);
            out.push((s & 0x7F) as u8);
            // number of flag bytes
            out.push(0x01);
            // extended flags: %00100000 (c = CRC present)
            out.push(0x20);
            // CRC attached data: length byte $05, then 5 synchsafe bytes
            out.push(0x05);
            out.extend_from_slice(&crc32_to_synchsafe5(crc));
            Ok(out)
        }
        _ => Err(Error::invalid(
            "extended-header CRC emission requires v2.3 or v2.4",
        )),
    }
}

/// Serialise a single frame into the caller's buffer, optionally
/// applying per-frame unsynchronisation (v2.4 only; the v2.3
/// format-flags byte has no unsync bit so the option is ignored under
/// v2.3). When requested, the encoded payload is passed through
/// [`apply_unsync`] before its length is computed for the frame
/// header, and the v2.4 format-flags byte gets bit 0x02 set per spec
/// §4.1.2.
fn write_frame_with_options(
    version: Id3Version,
    frame: &Id3Frame,
    per_frame_unsync: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let (id, mut payload) = encode_frame(version, frame)?;
    let mut id4 = [0u8; 4];
    let id_bytes = id.as_bytes();
    if id_bytes.len() != 4 || !id_bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::invalid(format!("invalid frame id for writer: {id}")));
    }
    id4.copy_from_slice(id_bytes);

    let apply_per_frame = per_frame_unsync && matches!(version, Id3Version::V2_4);
    if apply_per_frame {
        payload = apply_unsync(&payload);
    }

    out.extend_from_slice(&id4);
    let size = payload.len();
    match version {
        Id3Version::V2_4 => {
            if size >= 1 << 28 {
                return Err(Error::invalid("v2.4 frame size exceeds synchsafe limit"));
            }
            let s = size as u32;
            out.push(((s >> 21) & 0x7F) as u8);
            out.push(((s >> 14) & 0x7F) as u8);
            out.push(((s >> 7) & 0x7F) as u8);
            out.push((s & 0x7F) as u8);
        }
        Id3Version::V2_3 => {
            let s = size as u32;
            out.extend_from_slice(&s.to_be_bytes());
        }
        _ => unreachable!("validated in write_tag"),
    }
    // status flags = 0, format flags = 0x02 iff per-frame unsync was applied.
    let format_flags: u8 = if apply_per_frame { 0x02 } else { 0 };
    out.extend_from_slice(&[0, format_flags]);
    out.extend_from_slice(&payload);
    Ok(())
}

/// Produce the (id, payload) tuple for a frame. Callers wrap this with
/// the appropriate 10-byte frame header.
fn encode_frame(version: Id3Version, frame: &Id3Frame) -> Result<(String, Vec<u8>)> {
    // In v2.4 we default to UTF-8 (encoding byte 3); in v2.3 we use
    // UTF-16 with BOM (encoding byte 1) because the spec doesn't allow
    // encoding 3. Pure-ASCII values could use 0, but 1 / 3 are safe for
    // the whole Unicode range.
    let text_enc: u8 = match version {
        Id3Version::V2_4 => 3,
        _ => 1,
    };
    match frame {
        Id3Frame::Text { id, values } => {
            if id.len() != 4 {
                return Err(Error::invalid(format!(
                    "text frame id must be 4 chars: {id}"
                )));
            }
            let joined = match version {
                Id3Version::V2_4 => values.join("\u{0}"),
                _ => values.join("/"),
            };
            let mut payload = Vec::new();
            payload.push(text_enc);
            encode_string(&mut payload, text_enc, &joined);
            Ok((id.clone(), payload))
        }
        Id3Frame::UserText { description, value } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            encode_string(&mut payload, text_enc, description);
            encode_terminator(&mut payload, text_enc);
            encode_string(&mut payload, text_enc, value);
            Ok(("TXXX".to_string(), payload))
        }
        Id3Frame::UserUrl { description, url } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            encode_string(&mut payload, text_enc, description);
            encode_terminator(&mut payload, text_enc);
            // The URL itself is always ISO-8859-1.
            encode_latin1(&mut payload, url);
            Ok(("WXXX".to_string(), payload))
        }
        Id3Frame::Url { id, url } => {
            if id.len() != 4 {
                return Err(Error::invalid(format!(
                    "url frame id must be 4 chars: {id}"
                )));
            }
            let mut payload = Vec::new();
            encode_latin1(&mut payload, url);
            Ok((id.clone(), payload))
        }
        Id3Frame::Comment {
            lang,
            description,
            text,
        } => Ok((
            "COMM".to_string(),
            encode_comm_like(text_enc, lang, description, text),
        )),
        Id3Frame::Lyrics {
            lang,
            description,
            text,
        } => Ok((
            "USLT".to_string(),
            encode_comm_like(text_enc, lang, description, text),
        )),
        Id3Frame::Picture(pic) => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            // MIME is ISO-8859-1, NUL-terminated.
            encode_latin1(&mut payload, &pic.mime_type);
            payload.push(0);
            payload.push(pic.picture_type as u8);
            encode_string(&mut payload, text_enc, &pic.description);
            encode_terminator(&mut payload, text_enc);
            payload.extend_from_slice(&pic.data);
            Ok(("APIC".to_string(), payload))
        }
        Id3Frame::Popularimeter {
            email,
            rating,
            counter,
        } => {
            let mut payload = Vec::new();
            // Email is always ISO-8859-1 (no encoding byte).
            encode_latin1(&mut payload, email);
            payload.push(0);
            payload.push(*rating);
            // Spec: counter is at least 32 bits, and MAY be omitted
            // if no personal counter is wanted. We always emit at
            // least the 4-byte form; if the value exceeds u32::MAX
            // we widen to the smallest BE form that fits.
            encode_counter(&mut payload, *counter);
            Ok(("POPM".to_string(), payload))
        }
        Id3Frame::PlayCounter { count } => {
            let mut payload = Vec::new();
            encode_counter(&mut payload, *count);
            Ok(("PCNT".to_string(), payload))
        }
        Id3Frame::Private { owner, data } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            payload.extend_from_slice(data);
            Ok(("PRIV".to_string(), payload))
        }
        Id3Frame::Geob {
            mime_type,
            filename,
            description,
            data,
        } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            encode_latin1(&mut payload, mime_type);
            payload.push(0);
            encode_string(&mut payload, text_enc, filename);
            encode_terminator(&mut payload, text_enc);
            encode_string(&mut payload, text_enc, description);
            encode_terminator(&mut payload, text_enc);
            payload.extend_from_slice(data);
            Ok(("GEOB".to_string(), payload))
        }
        Id3Frame::Ufid { owner, identifier } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            // Spec caps identifier at 64 bytes; we still write what
            // the caller gave us — clamping is a caller policy
            // decision and silent truncation here would lose data.
            payload.extend_from_slice(identifier);
            Ok(("UFID".to_string(), payload))
        }
        Id3Frame::TermsOfUse { lang, text } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            payload.extend_from_slice(lang);
            encode_string(&mut payload, text_enc, text);
            Ok(("USER".to_string(), payload))
        }
        Id3Frame::Ownership {
            price,
            date,
            seller,
        } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            // Price is always ISO-8859-1 (currency-code + numeric);
            // seller follows the declared encoding.
            encode_latin1(&mut payload, price);
            payload.push(0);
            // Date is a fixed 8 chars (YYYYMMDD) — pad with spaces
            // when shorter, truncate when longer, so the written
            // frame matches the spec layout regardless of caller
            // hygiene.
            write_fixed_ascii8(&mut payload, date);
            encode_string(&mut payload, text_enc, seller);
            Ok(("OWNE".to_string(), payload))
        }
        Id3Frame::Commercial {
            price,
            valid_until,
            contact_url,
            received_as,
            seller,
            description,
            logo_mime,
            logo_data,
        } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            encode_latin1(&mut payload, price);
            payload.push(0);
            write_fixed_ascii8(&mut payload, valid_until);
            encode_latin1(&mut payload, contact_url);
            payload.push(0);
            payload.push(*received_as);
            encode_string(&mut payload, text_enc, seller);
            encode_terminator(&mut payload, text_enc);
            encode_string(&mut payload, text_enc, description);
            encode_terminator(&mut payload, text_enc);
            // The MIME + logo block is optional. We emit it whenever
            // logo_data is non-empty, OR when logo_mime is non-empty
            // (a caller may want to declare the slot even with no
            // image — the spec says "These two last fields may be
            // omitted if no picture is attached," implying together).
            if !logo_data.is_empty() || !logo_mime.is_empty() {
                encode_latin1(&mut payload, logo_mime);
                payload.push(0);
                payload.extend_from_slice(logo_data);
            }
            Ok(("COMR".to_string(), payload))
        }
        Id3Frame::SyncedTempo { time_format, codes } => {
            let mut payload = Vec::new();
            payload.push(*time_format);
            for &(tempo, ts) in codes {
                if tempo >= 0xFF {
                    // Two-byte form: $FF + (tempo - 0xFF), capped at
                    // 510 BPM per spec ($FF + $FF = 510).
                    let extra = (tempo - 0xFF).min(0xFF) as u8;
                    payload.push(0xFF);
                    payload.push(extra);
                } else {
                    payload.push(tempo as u8);
                }
                payload.extend_from_slice(&ts.to_be_bytes());
            }
            Ok(("SYTC".to_string(), payload))
        }
        Id3Frame::Rva2 {
            identification,
            channels,
        } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, identification);
            payload.push(0);
            for ch in channels {
                payload.push(ch.channel_type);
                payload.extend_from_slice(&ch.volume_adjustment.to_be_bytes());
                payload.push(ch.bits_peak);
                // Spec: "The peak volume field is always padded to
                // whole bytes." We trust the caller-supplied width;
                // if it's wrong relative to bits_peak the writer is
                // not allowed to silently lengthen the on-wire form.
                let expected = (ch.bits_peak as usize).div_ceil(8);
                if ch.peak.len() == expected {
                    payload.extend_from_slice(&ch.peak);
                } else if ch.peak.len() < expected {
                    // Pad zeros at the front so the value reads as
                    // BE with the right magnitude.
                    let pad = expected - ch.peak.len();
                    payload.resize(payload.len() + pad, 0);
                    payload.extend_from_slice(&ch.peak);
                } else {
                    // Caller over-provided; emit only the low-order
                    // bytes that fit in the declared width.
                    let start = ch.peak.len() - expected;
                    payload.extend_from_slice(&ch.peak[start..]);
                }
            }
            Ok(("RVA2".to_string(), payload))
        }
        Id3Frame::Equ2 {
            interpolation,
            identification,
            points,
        } => {
            let mut payload = Vec::new();
            payload.push(*interpolation);
            encode_latin1(&mut payload, identification);
            payload.push(0);
            for &(freq, adj) in points {
                payload.extend_from_slice(&freq.to_be_bytes());
                payload.extend_from_slice(&adj.to_be_bytes());
            }
            Ok(("EQU2".to_string(), payload))
        }
        Id3Frame::MusicCdId { toc } => Ok(("MCDI".to_string(), toc.clone())),
        Id3Frame::EventTimingCodes {
            time_format,
            events,
        } => {
            let mut payload = Vec::new();
            payload.push(*time_format);
            for &(ev, ts) in events {
                payload.push(ev);
                payload.extend_from_slice(&ts.to_be_bytes());
            }
            Ok(("ETCO".to_string(), payload))
        }
        Id3Frame::SyncedLyrics {
            lang,
            time_format,
            content_type,
            description,
            syncs,
        } => {
            let mut payload = Vec::new();
            payload.push(text_enc);
            payload.extend_from_slice(lang);
            payload.push(*time_format);
            payload.push(*content_type);
            encode_string(&mut payload, text_enc, description);
            encode_terminator(&mut payload, text_enc);
            for (text, ts) in syncs {
                encode_string(&mut payload, text_enc, text);
                encode_terminator(&mut payload, text_enc);
                payload.extend_from_slice(&ts.to_be_bytes());
            }
            Ok(("SYLT".to_string(), payload))
        }
        Id3Frame::PositionSync {
            time_format,
            position,
        } => {
            let mut payload = Vec::new();
            payload.push(*time_format);
            payload.extend_from_slice(&position.to_be_bytes());
            Ok(("POSS".to_string(), payload))
        }
        Id3Frame::RecommendedBuffer {
            buffer_size,
            embedded_info,
            offset_to_next,
        } => {
            let mut payload = Vec::new();
            // Buffer size is a 24-bit BE field. Clamp at 0xFFFFFF to
            // keep the wire form spec-conformant even if a caller
            // supplied a larger value.
            let clamped = (*buffer_size).min(0x00FF_FFFF);
            payload.push(((clamped >> 16) & 0xFF) as u8);
            payload.push(((clamped >> 8) & 0xFF) as u8);
            payload.push((clamped & 0xFF) as u8);
            // Flags byte: only the LSB is defined by spec.
            payload.push(if *embedded_info { 0x01 } else { 0x00 });
            payload.extend_from_slice(&offset_to_next.to_be_bytes());
            Ok(("RBUF".to_string(), payload))
        }
        Id3Frame::Seek {
            min_offset_to_next_tag,
        } => Ok((
            "SEEK".to_string(),
            min_offset_to_next_tag.to_be_bytes().to_vec(),
        )),
        Id3Frame::Signature {
            group_symbol,
            signature,
        } => {
            let mut payload = Vec::with_capacity(1 + signature.len());
            payload.push(*group_symbol);
            payload.extend_from_slice(signature);
            Ok(("SIGN".to_string(), payload))
        }
        Id3Frame::GroupId {
            owner,
            group_symbol,
            data,
        } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            payload.push(*group_symbol);
            payload.extend_from_slice(data);
            Ok(("GRID".to_string(), payload))
        }
        Id3Frame::EncryptionMethod {
            owner,
            method_symbol,
            data,
        } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            payload.push(*method_symbol);
            payload.extend_from_slice(data);
            Ok(("ENCR".to_string(), payload))
        }
        Id3Frame::AudioEncryption {
            owner,
            preview_start,
            preview_length,
            encryption_info,
        } => {
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            payload.extend_from_slice(&preview_start.to_be_bytes());
            payload.extend_from_slice(&preview_length.to_be_bytes());
            payload.extend_from_slice(encryption_info);
            Ok(("AENC".to_string(), payload))
        }
        Id3Frame::LinkedInfo {
            frame_id,
            url,
            additional,
        } => {
            let mut payload = Vec::new();
            // In v2.4 the frame id is 4 bytes; in v2.3 it is 3.
            // We emit per the *target* version so the on-wire layout
            // matches the spec the consumer expects.
            match version {
                Id3Version::V2_3 | Id3Version::V2_2 | Id3Version::V1 => {
                    payload.extend_from_slice(&frame_id[..3]);
                }
                Id3Version::V2_4 => {
                    payload.extend_from_slice(frame_id);
                }
            }
            encode_latin1(&mut payload, url);
            payload.push(0);
            payload.extend_from_slice(additional);
            Ok(("LINK".to_string(), payload))
        }
        Id3Frame::AudioSeekPointIndex {
            indexed_data_start,
            indexed_data_length,
            bits_per_index_point,
            fractions,
        } => {
            // Spec §4.30 fixes the bits-per-point at 8 or 16. Anything
            // else is a caller bug; refuse rather than silently emit an
            // ambiguous frame the parser couldn't reconstruct.
            if *bits_per_index_point != 8 && *bits_per_index_point != 16 {
                return Err(Error::invalid("ASPI bits_per_index_point must be 8 or 16"));
            }
            // The N field is a u16 BE so the writer caps at u16::MAX
            // (65535 points) and refuses anything larger rather than
            // truncate silently.
            if fractions.len() > u16::MAX as usize {
                return Err(Error::invalid("ASPI number of index points exceeds u16"));
            }
            let n = fractions.len() as u16;
            let per_point = if *bits_per_index_point == 8 { 1 } else { 2 };
            let mut payload = Vec::with_capacity(11 + fractions.len() * per_point);
            payload.extend_from_slice(&indexed_data_start.to_be_bytes());
            payload.extend_from_slice(&indexed_data_length.to_be_bytes());
            payload.extend_from_slice(&n.to_be_bytes());
            payload.push(*bits_per_index_point);
            if *bits_per_index_point == 8 {
                // 8-bit form: low byte of each fraction. Clamp at 0xFF
                // so a caller that put a wider value in the `u16` slot
                // gets a defined truncation, not a wrap.
                for &f in fractions {
                    payload.push(f.min(0xFF) as u8);
                }
            } else {
                for &f in fractions {
                    payload.extend_from_slice(&f.to_be_bytes());
                }
            }
            Ok(("ASPI".to_string(), payload))
        }
        Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference,
            bytes_between_reference,
            ms_between_reference,
            bits_for_bytes_deviation,
            bits_for_ms_deviation,
            references,
        } => {
            // Spec §4.7 / §4.6:
            //   - The 24-bit fields cap at 0x00FF_FFFF.
            //   - Per-reference widths must each be ≤ 32 (so the value
            //     fits in u32) AND their sum must be a multiple of 4.
            //   - The descriptor (frames-between byte count + 3-byte
            //     fields + the two deviation widths) is always 10 bytes.
            if *bytes_between_reference > 0x00FF_FFFF {
                return Err(Error::invalid(
                    "MLLT bytes_between_reference exceeds 24-bit field",
                ));
            }
            if *ms_between_reference > 0x00FF_FFFF {
                return Err(Error::invalid(
                    "MLLT ms_between_reference exceeds 24-bit field",
                ));
            }
            if *bits_for_bytes_deviation > 32 || *bits_for_ms_deviation > 32 {
                return Err(Error::invalid(
                    "MLLT per-reference deviation width must fit in u32 (≤ 32 bits)",
                ));
            }
            let total_bits = *bits_for_bytes_deviation as usize + *bits_for_ms_deviation as usize;
            if total_bits % 4 != 0 {
                return Err(Error::invalid(
                    "MLLT bits_for_bytes_deviation + bits_for_ms_deviation must be a multiple of 4",
                ));
            }
            let mut payload = Vec::with_capacity(10 + (total_bits * references.len()).div_ceil(8));
            payload.extend_from_slice(&mpeg_frames_between_reference.to_be_bytes());
            let bb = bytes_between_reference.to_be_bytes();
            payload.extend_from_slice(&bb[1..4]);
            let mb = ms_between_reference.to_be_bytes();
            payload.extend_from_slice(&mb[1..4]);
            payload.push(*bits_for_bytes_deviation);
            payload.push(*bits_for_ms_deviation);
            if total_bits > 0 && !references.is_empty() {
                let mut writer = BitWriter::new();
                for &(bytes_dev, ms_dev) in references {
                    if *bits_for_bytes_deviation < 32
                        && bytes_dev >= 1u32 << *bits_for_bytes_deviation
                    {
                        return Err(Error::invalid(
                            "MLLT reference byte deviation exceeds bits_for_bytes_deviation",
                        ));
                    }
                    if *bits_for_ms_deviation < 32 && ms_dev >= 1u32 << *bits_for_ms_deviation {
                        return Err(Error::invalid(
                            "MLLT reference ms deviation exceeds bits_for_ms_deviation",
                        ));
                    }
                    writer.push(bytes_dev, *bits_for_bytes_deviation as usize);
                    writer.push(ms_dev, *bits_for_ms_deviation as usize);
                }
                payload.extend_from_slice(&writer.into_bytes());
            }
            Ok(("MLLT".to_string(), payload))
        }
        Id3Frame::Reverb {
            reverb_left_ms,
            reverb_right_ms,
            bounces_left,
            bounces_right,
            feedback_ll,
            feedback_lr,
            feedback_rr,
            feedback_rl,
            premix_lr,
            premix_rl,
        } => {
            // Spec v2.3 §4.13 / v2.4 §4.13: fixed 12-byte payload, no
            // encoding byte, no terminator. Layout is byte-aligned and
            // version-independent, so the writer emits the same bytes
            // under any version envelope.
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&reverb_left_ms.to_be_bytes());
            payload.extend_from_slice(&reverb_right_ms.to_be_bytes());
            payload.push(*bounces_left);
            payload.push(*bounces_right);
            payload.push(*feedback_ll);
            payload.push(*feedback_lr);
            payload.push(*feedback_rr);
            payload.push(*feedback_rl);
            payload.push(*premix_lr);
            payload.push(*premix_rl);
            Ok(("RVRB".to_string(), payload))
        }
        Id3Frame::Unknown { id, raw } => {
            // Promote v2.2 ids (3 chars) to their v2.3 equivalents on
            // write so the output is always a well-formed v2.3/v2.4
            // frame. If the id is already 4 chars it passes through.
            let promoted = if id.len() == 3 {
                v22_promote(id).to_string()
            } else {
                id.clone()
            };
            Ok((promoted, raw.clone()))
        }
    }
}

fn encode_comm_like(enc: u8, lang: &[u8; 3], description: &str, text: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(enc);
    payload.extend_from_slice(lang);
    encode_string(&mut payload, enc, description);
    encode_terminator(&mut payload, enc);
    encode_string(&mut payload, enc, text);
    payload
}

fn encode_string(out: &mut Vec<u8>, enc: u8, s: &str) {
    match enc {
        0 => encode_latin1(out, s),
        1 => encode_utf16_bom(out, s),
        2 => encode_utf16_be(out, s),
        _ => out.extend_from_slice(s.as_bytes()),
    }
}

fn encode_terminator(out: &mut Vec<u8>, enc: u8) {
    if enc == 1 || enc == 2 {
        out.push(0);
        out.push(0);
    } else {
        out.push(0);
    }
}

/// Encode an integer counter for `PCNT` / `POPM` as a big-endian
/// byte string. Spec §4.16: at least 32 bits long; if the value
/// exceeds u32::MAX we widen to the smallest BE form that fits, up
/// to 8 bytes (u64). Callers that need a fixed width pad on read
/// via `be_unsigned`.
fn encode_counter(out: &mut Vec<u8>, value: u64) {
    if value <= u32::MAX as u64 {
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        // Find the smallest width >= 5 that fits, then emit BE.
        let mut width = 5usize;
        while width < 8 && value >> (width * 8) != 0 {
            width += 1;
        }
        let bytes = value.to_be_bytes();
        out.extend_from_slice(&bytes[8 - width..]);
    }
}

fn encode_latin1(out: &mut Vec<u8>, s: &str) {
    for ch in s.chars() {
        let c = ch as u32;
        out.push(if c < 256 { c as u8 } else { b'?' });
    }
}

/// Write `s` as exactly 8 ISO-8859-1 bytes — truncate if longer,
/// pad with ASCII spaces if shorter. Used for the spec-mandated
/// fixed-width `YYYYMMDD` date fields in `OWNE` / `COMR`.
fn write_fixed_ascii8(out: &mut Vec<u8>, s: &str) {
    let bytes: Vec<u8> = s
        .chars()
        .map(|c| {
            let v = c as u32;
            if v < 256 {
                v as u8
            } else {
                b'?'
            }
        })
        .collect();
    if bytes.len() >= 8 {
        out.extend_from_slice(&bytes[..8]);
    } else {
        out.extend_from_slice(&bytes);
        for _ in bytes.len()..8 {
            out.push(b' ');
        }
    }
}

fn encode_utf16_bom(out: &mut Vec<u8>, s: &str) {
    // Emit a little-endian BOM, then LE code units.
    out.push(0xFF);
    out.push(0xFE);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
}

fn encode_utf16_be(out: &mut Vec<u8>, s: &str) {
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
}

fn id3v1_genre_index(name: &str) -> Option<u8> {
    if name.is_empty() {
        return None;
    }
    for i in 0..=191u8 {
        if let Some(g) = id3v1_genre(i) {
            if g.eq_ignore_ascii_case(name) {
                return Some(i);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a v2.3 tag carrying a TIT2 title and an APIC front cover.
    fn build_v23_tag_title_and_apic() -> Vec<u8> {
        // TIT2 frame: encoding=0 (latin1) + b"Song Title"
        let title = b"Song Title";
        let mut tit2 = Vec::new();
        tit2.extend_from_slice(b"TIT2");
        let t_size = (1 + title.len()) as u32;
        tit2.extend_from_slice(&t_size.to_be_bytes());
        tit2.extend_from_slice(&[0, 0]); // flags
        tit2.push(0); // encoding
        tit2.extend_from_slice(title);

        // APIC: enc=0 + mime "image/png\0" + picture_type=0x03 +
        // description "\0" + binary "PNGDATA".
        let mut apic = Vec::new();
        let mut apic_body = Vec::new();
        apic_body.push(0u8);
        apic_body.extend_from_slice(b"image/png\0");
        apic_body.push(0x03);
        apic_body.push(0);
        apic_body.extend_from_slice(b"PNGDATA");
        apic.extend_from_slice(b"APIC");
        apic.extend_from_slice(&(apic_body.len() as u32).to_be_bytes());
        apic.extend_from_slice(&[0, 0]);
        apic.extend_from_slice(&apic_body);

        let body = [tit2, apic].concat();
        let size = body.len();
        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(3); // major
        tag.push(0); // revision
        tag.push(0); // flags
                     // synchsafe size
        let s = size as u32;
        tag.push(((s >> 21) & 0x7F) as u8);
        tag.push(((s >> 14) & 0x7F) as u8);
        tag.push(((s >> 7) & 0x7F) as u8);
        tag.push((s & 0x7F) as u8);
        tag.extend_from_slice(&body);
        tag
    }

    #[test]
    fn parse_v23_title_and_apic() {
        let tag = build_v23_tag_title_and_apic();
        let (parsed, consumed) = parse_tag(&tag).unwrap();
        assert_eq!(consumed, tag.len());
        assert_eq!(parsed.version, Id3Version::V2_3);
        // Title frame
        let title = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::Text { id, values } if id == "TIT2" => Some(values.clone()),
            _ => None,
        });
        assert_eq!(title.as_deref(), Some(&["Song Title".to_string()][..]));
        // APIC
        let pic = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::Picture(p) => Some(p.clone()),
            _ => None,
        });
        let pic = pic.expect("APIC frame");
        assert_eq!(pic.mime_type, "image/png");
        assert_eq!(pic.picture_type, PictureType::FrontCover);
        assert_eq!(pic.data, b"PNGDATA");
    }

    #[test]
    fn to_kv_title_artist() {
        let mut tag = build_v23_tag_title_and_apic();
        // Append a TPE1 artist frame.
        let artist = b"An Artist";
        let tpe1_body_len = 1 + artist.len();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"TPE1");
        frame.extend_from_slice(&(tpe1_body_len as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.push(0);
        frame.extend_from_slice(artist);
        // Splice into the body
        let body_len_offset = 6;
        let old_size = synchsafe_u32(
            tag[body_len_offset],
            tag[body_len_offset + 1],
            tag[body_len_offset + 2],
            tag[body_len_offset + 3],
        ) as usize;
        tag.extend_from_slice(&frame);
        let new_size = (old_size + frame.len()) as u32;
        tag[body_len_offset] = ((new_size >> 21) & 0x7F) as u8;
        tag[body_len_offset + 1] = ((new_size >> 14) & 0x7F) as u8;
        tag[body_len_offset + 2] = ((new_size >> 7) & 0x7F) as u8;
        tag[body_len_offset + 3] = (new_size & 0x7F) as u8;
        let (parsed, _) = parse_tag(&tag).unwrap();
        let kv = to_key_value_pairs(&parsed);
        assert!(kv.contains(&("title".to_string(), "Song Title".to_string())));
        assert!(kv.contains(&("artist".to_string(), "An Artist".to_string())));
        let pics = attached_pictures(&parsed);
        assert_eq!(pics.len(), 1);
    }

    #[test]
    fn parse_v22_pic() {
        // v2.2 tag with one PIC frame.
        // PIC body: enc=0, fmt="JPG", type=0x03, description="\0", data="JPGDATA"
        let mut pic_body = Vec::new();
        pic_body.push(0u8);
        pic_body.extend_from_slice(b"JPG");
        pic_body.push(0x03);
        pic_body.push(0); // empty description terminator
        pic_body.extend_from_slice(b"JPGDATA");
        let mut frame = Vec::new();
        frame.extend_from_slice(b"PIC");
        let size = pic_body.len() as u32;
        frame.push(((size >> 16) & 0xFF) as u8);
        frame.push(((size >> 8) & 0xFF) as u8);
        frame.push((size & 0xFF) as u8);
        frame.extend_from_slice(&pic_body);

        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(2);
        tag.push(0);
        tag.push(0);
        let s = frame.len() as u32;
        tag.push(((s >> 21) & 0x7F) as u8);
        tag.push(((s >> 14) & 0x7F) as u8);
        tag.push(((s >> 7) & 0x7F) as u8);
        tag.push((s & 0x7F) as u8);
        tag.extend_from_slice(&frame);

        let (parsed, _) = parse_tag(&tag).unwrap();
        let pic = attached_pictures(&parsed);
        assert_eq!(pic.len(), 1);
        assert_eq!(pic[0].mime_type, "image/jpeg");
        assert_eq!(pic[0].picture_type, PictureType::FrontCover);
        assert_eq!(pic[0].data, b"JPGDATA");
    }

    #[test]
    fn parse_v1_trailer() {
        let mut trailer = vec![0u8; 128];
        trailer[0..3].copy_from_slice(b"TAG");
        let title = b"TinyTitle";
        trailer[3..3 + title.len()].copy_from_slice(title);
        let artist = b"TinyArtist";
        trailer[33..33 + artist.len()].copy_from_slice(artist);
        // v1.1 track number
        trailer[125] = 0;
        trailer[126] = 7;
        trailer[127] = 17; // genre = Rock
        let tag = parse_id3v1(&trailer).unwrap();
        let kv = to_key_value_pairs(&tag);
        assert!(kv.contains(&("title".to_string(), "TinyTitle".to_string())));
        assert!(kv.contains(&("artist".to_string(), "TinyArtist".to_string())));
        assert!(kv.contains(&("track".to_string(), "7".to_string())));
        assert!(kv.contains(&("genre".to_string(), "Rock".to_string())));
    }

    #[test]
    fn v24_per_frame_unsync_and_dli() {
        // Build a v2.4 tag with a single TIT2 frame that has the
        // data-length indicator + unsync flags set. The TIT2 payload
        // (encoding byte + text) contains an 0xFF that we escape.
        let enc_plus_text = [&[0u8][..], b"AB\xFFCD"].concat();
        // Unsynchronise: insert 0x00 after every 0xFF.
        let mut unsynced = Vec::new();
        for &b in &enc_plus_text {
            unsynced.push(b);
            if b == 0xFF {
                unsynced.push(0x00);
            }
        }
        // DLI prefix: 4 synchsafe bytes giving the *pre-unsync* length.
        let dli = (enc_plus_text.len() as u32).to_be_bytes();
        let mut synchsafe_dli = [0u8; 4];
        let v = enc_plus_text.len() as u32;
        synchsafe_dli[0] = ((v >> 21) & 0x7F) as u8;
        synchsafe_dli[1] = ((v >> 14) & 0x7F) as u8;
        synchsafe_dli[2] = ((v >> 7) & 0x7F) as u8;
        synchsafe_dli[3] = (v & 0x7F) as u8;
        let _ = dli;
        let frame_body = [&synchsafe_dli[..], &unsynced[..]].concat();
        let size = frame_body.len() as u32;
        let mut synchsafe_size = [0u8; 4];
        synchsafe_size[0] = ((size >> 21) & 0x7F) as u8;
        synchsafe_size[1] = ((size >> 14) & 0x7F) as u8;
        synchsafe_size[2] = ((size >> 7) & 0x7F) as u8;
        synchsafe_size[3] = (size & 0x7F) as u8;
        let mut frame = Vec::new();
        frame.extend_from_slice(b"TIT2");
        frame.extend_from_slice(&synchsafe_size);
        // Flags: format-flags low byte = 0x01 (DLI) | 0x02 (unsync) = 0x03
        frame.push(0); // status flags
        frame.push(0x03);
        frame.extend_from_slice(&frame_body);

        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(4);
        tag.push(0);
        tag.push(0); // no whole-tag unsync
        let tag_size = frame.len() as u32;
        tag.push(((tag_size >> 21) & 0x7F) as u8);
        tag.push(((tag_size >> 14) & 0x7F) as u8);
        tag.push(((tag_size >> 7) & 0x7F) as u8);
        tag.push((tag_size & 0x7F) as u8);
        tag.extend_from_slice(&frame);

        let (parsed, _) = parse_tag(&tag).unwrap();
        let got = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::Text { id, values } if id == "TIT2" => Some(values.clone()),
            _ => None,
        });
        assert_eq!(got.as_deref(), Some(&["AB\u{FF}CD".to_string()][..]));
    }

    #[test]
    fn whole_tag_unsync_v23() {
        // TIT2 payload containing 0xFF 0x00 → needs one pass of
        // reverse_unsync at the tag level.
        let payload = [&[0u8][..], b"X\xFFY"].concat();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"TIT2");
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&payload);
        // Apply unsync: after every 0xFF insert 0x00.
        let mut unsynced = Vec::new();
        for &b in &frame {
            unsynced.push(b);
            if b == 0xFF {
                unsynced.push(0x00);
            }
        }
        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(3);
        tag.push(0);
        tag.push(0x80); // unsync flag
        let sz = unsynced.len() as u32;
        tag.push(((sz >> 21) & 0x7F) as u8);
        tag.push(((sz >> 14) & 0x7F) as u8);
        tag.push(((sz >> 7) & 0x7F) as u8);
        tag.push((sz & 0x7F) as u8);
        tag.extend_from_slice(&unsynced);
        let (parsed, _) = parse_tag(&tag).unwrap();
        let got = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::Text { id, values } if id == "TIT2" => Some(values.clone()),
            _ => None,
        });
        assert_eq!(got.as_deref(), Some(&["X\u{FF}Y".to_string()][..]));
    }

    #[test]
    fn tag_size_at_head_basic() {
        let tag = build_v23_tag_title_and_apic();
        let size = tag_size_at_head(&tag[0..10]).unwrap();
        assert_eq!(size, tag.len());
    }

    /// Spec-shaped `POPM` payload parsed straight from a hand-crafted
    /// byte sequence: email `"a@b\0"`, rating `0xC4` (=196), counter
    /// `00 00 00 2A` (=42). Confirms the parser walks the §4.17 layout
    /// without going through the writer first.
    #[test]
    fn popm_parse_handcrafted_payload() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"a@b");
        payload.push(0);
        payload.push(0xC4);
        payload.extend_from_slice(&42u32.to_be_bytes());
        match parse_popm(&payload) {
            Id3Frame::Popularimeter {
                email,
                rating,
                counter,
            } => {
                assert_eq!(email, "a@b");
                assert_eq!(rating, 0xC4);
                assert_eq!(counter, 42);
            }
            _ => panic!("expected Popularimeter"),
        }
    }

    /// `be_unsigned` saturates to u64::MAX when the buffer has high
    /// bytes that won't fit in u64. Sanity check the overflow guard.
    #[test]
    fn be_unsigned_saturates() {
        // 9 bytes with a non-zero leading byte -> MAX.
        let mut buf = vec![0u8; 9];
        buf[0] = 1;
        assert_eq!(be_unsigned(&buf), u64::MAX);
        // 9 bytes with the leading byte zero -> just the lower 8.
        let mut buf2 = vec![0u8; 9];
        buf2[8] = 0xAB;
        assert_eq!(be_unsigned(&buf2), 0xAB);
        // Short buffers work too.
        assert_eq!(be_unsigned(&[]), 0);
        assert_eq!(be_unsigned(&[0xFF, 0xFF]), 0xFFFF);
    }

    /// Hand-rolled `SYTC` payload exercising the two-byte $FF tempo
    /// extension. Confirms the parser splits the 2-byte tempo +
    /// 4-byte timestamp form correctly without going through the
    /// writer.
    #[test]
    fn sytc_parse_handcrafted_ff_extension() {
        // time_format = $02 (ms)
        // tempo = $FF $01 -> 256 BPM
        // ts    = 0x0000_1000
        let mut payload = Vec::new();
        payload.push(0x02);
        payload.push(0xFF);
        payload.push(0x01);
        payload.extend_from_slice(&0x0000_1000u32.to_be_bytes());
        match parse_sytc(&payload) {
            Id3Frame::SyncedTempo { time_format, codes } => {
                assert_eq!(time_format, 0x02);
                assert_eq!(codes, vec![(256u16, 0x0000_1000u32)]);
            }
            _ => panic!("expected SyncedTempo"),
        }
    }

    /// Hand-rolled `RVA2` payload with two channels confirms the
    /// parser walks identification + repeating channel records.
    /// Spec §4.11: bits_peak = 0 means "no peak field" — the second
    /// channel exercises that path.
    #[test]
    fn rva2_parse_handcrafted_two_channels() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"alb");
        payload.push(0);
        // Channel 1: master, +2dB ($04 00), 8-bit peak ($80).
        payload.push(0x01);
        payload.extend_from_slice(&1024i16.to_be_bytes());
        payload.push(0x08);
        payload.push(0x80);
        // Channel 2: front-left, -1dB ($FE 00), no peak.
        payload.push(0x03);
        payload.extend_from_slice(&(-512i16).to_be_bytes());
        payload.push(0x00);
        match parse_rva2(&payload) {
            Id3Frame::Rva2 {
                identification,
                channels,
            } => {
                assert_eq!(identification, "alb");
                assert_eq!(channels.len(), 2);
                assert_eq!(channels[0].volume_adjustment, 1024);
                assert_eq!(channels[0].bits_peak, 8);
                assert_eq!(channels[0].peak, vec![0x80]);
                assert_eq!(channels[1].volume_adjustment, -512);
                assert_eq!(channels[1].bits_peak, 0);
                assert!(channels[1].peak.is_empty());
            }
            _ => panic!("expected Rva2"),
        }
    }

    /// `parse_user` on a minimal, well-formed payload.
    #[test]
    fn user_parse_handcrafted() {
        let mut payload = Vec::new();
        payload.push(0); // ISO-8859-1
        payload.extend_from_slice(b"eng");
        payload.extend_from_slice(b"Public domain.");
        match parse_user(&payload) {
            Id3Frame::TermsOfUse { lang, text } => {
                assert_eq!(lang, *b"eng");
                assert_eq!(text, "Public domain.");
            }
            _ => panic!("expected TermsOfUse"),
        }
    }

    /// `parse_encr` on a minimal, well-formed payload: owner +
    /// method symbol + optional encryption-specific data.
    #[test]
    fn encr_parse_handcrafted_payload() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"http://example.org/enc");
        payload.push(0);
        payload.push(0x80); // method symbol
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // method data
        match parse_encr(&payload) {
            Id3Frame::EncryptionMethod {
                owner,
                method_symbol,
                data,
            } => {
                assert_eq!(owner, "http://example.org/enc");
                assert_eq!(method_symbol, 0x80);
                assert_eq!(data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            _ => panic!("expected EncryptionMethod"),
        }
    }

    /// An `ENCR` frame with an empty owner and no method data still
    /// parses to a well-formed `EncryptionMethod` (symbol only).
    #[test]
    fn encr_parse_minimal_symbol_only() {
        // Empty owner ($00) followed by the method symbol byte.
        let payload = [0x00, 0xF0];
        match parse_encr(&payload) {
            Id3Frame::EncryptionMethod {
                owner,
                method_symbol,
                data,
            } => {
                assert!(owner.is_empty());
                assert_eq!(method_symbol, 0xF0);
                assert!(data.is_empty());
            }
            _ => panic!("expected EncryptionMethod"),
        }
    }

    /// `ENCR` round-trips through `write_tag` / `parse_tag` for both
    /// v2.3 and v2.4 (the wire layout is version-independent).
    #[test]
    fn encr_roundtrips_v23_and_v24() {
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let original = Id3Tag {
                version,
                frames: vec![Id3Frame::EncryptionMethod {
                    owner: "mailto:enc@example.org".into(),
                    method_symbol: 0x81,
                    data: vec![0x01, 0x02, 0x03],
                }],
            };
            let bytes = write_tag(&original, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            assert_eq!(parsed.frames.len(), 1);
            match &parsed.frames[0] {
                Id3Frame::EncryptionMethod {
                    owner,
                    method_symbol,
                    data,
                } => {
                    assert_eq!(owner, "mailto:enc@example.org");
                    assert_eq!(*method_symbol, 0x81);
                    assert_eq!(data, &vec![0x01, 0x02, 0x03]);
                }
                other => panic!("expected EncryptionMethod, got {other:?}"),
            }
        }
    }

    /// `parse_aspi` decodes an 8-bit-per-point index. Spec §4.30
    /// header layout: S(4) + L(4) + N(2) + b(1) + N×b/8 fraction bytes.
    #[test]
    fn aspi_parse_handcrafted_8bit() {
        let mut payload = Vec::new();
        // S = 0x0000_0100 (256 byte file offset to start of audio)
        payload.extend_from_slice(&0x0000_0100u32.to_be_bytes());
        // L = 0x0001_0000 (65536 byte indexed audio length)
        payload.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        // N = 4 points
        payload.extend_from_slice(&4u16.to_be_bytes());
        // b = 8 bits per point
        payload.push(8);
        // Fractions: 0x00, 0x40, 0x80, 0xC0 (quarters of the audio)
        payload.extend_from_slice(&[0x00, 0x40, 0x80, 0xC0]);
        match parse_aspi(&payload) {
            Id3Frame::AudioSeekPointIndex {
                indexed_data_start,
                indexed_data_length,
                bits_per_index_point,
                fractions,
            } => {
                assert_eq!(indexed_data_start, 0x0000_0100);
                assert_eq!(indexed_data_length, 0x0001_0000);
                assert_eq!(bits_per_index_point, 8);
                assert_eq!(fractions, vec![0x00, 0x40, 0x80, 0xC0]);
            }
            _ => panic!("expected AudioSeekPointIndex"),
        }
    }

    /// 16-bit-per-point `ASPI` reads two bytes per fraction.
    #[test]
    fn aspi_parse_handcrafted_16bit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_be_bytes()); // S
        payload.extend_from_slice(&0x1000u32.to_be_bytes()); // L
        payload.extend_from_slice(&3u16.to_be_bytes()); // N
        payload.push(16); // b
        payload.extend_from_slice(&[
            0x00, 0x00, // F0 = 0
            0x55, 0x55, // F1 = 0x5555
            0xAA, 0xAA, // F2 = 0xAAAA
        ]);
        match parse_aspi(&payload) {
            Id3Frame::AudioSeekPointIndex {
                bits_per_index_point,
                fractions,
                ..
            } => {
                assert_eq!(bits_per_index_point, 16);
                assert_eq!(fractions, vec![0x0000, 0x5555, 0xAAAA]);
            }
            _ => panic!("expected AudioSeekPointIndex"),
        }
    }

    /// Truncated fraction list (N claims more points than the payload
    /// carries) drops the missing tail rather than panicking.
    #[test]
    fn aspi_parse_truncated_fraction_list_drops_tail() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_be_bytes()); // S
        payload.extend_from_slice(&100u32.to_be_bytes()); // L
        payload.extend_from_slice(&10u16.to_be_bytes()); // N claims 10
        payload.push(8); // b
        payload.extend_from_slice(&[0x10, 0x20, 0x30]); // only 3 fractions present
        match parse_aspi(&payload) {
            Id3Frame::AudioSeekPointIndex { fractions, .. } => {
                assert_eq!(fractions, vec![0x10, 0x20, 0x30]);
            }
            _ => panic!("expected AudioSeekPointIndex"),
        }
    }

    /// A payload shorter than the 11-byte fixed header degenerates to
    /// a zeroed `AudioSeekPointIndex` rather than failing the parse.
    #[test]
    fn aspi_parse_short_header_degenerate() {
        let payload = [0x00, 0x01, 0x02];
        match parse_aspi(&payload) {
            Id3Frame::AudioSeekPointIndex {
                indexed_data_start,
                indexed_data_length,
                bits_per_index_point,
                fractions,
            } => {
                assert_eq!(indexed_data_start, 0);
                assert_eq!(indexed_data_length, 0);
                assert_eq!(bits_per_index_point, 0);
                assert!(fractions.is_empty());
            }
            _ => panic!("expected AudioSeekPointIndex"),
        }
    }

    /// `ASPI` round-trips through `write_tag` / `parse_tag` for v2.4
    /// (the wire layout is byte-aligned and version-independent, but
    /// spec §4.30 declares the frame in v2.4 only).
    #[test]
    fn aspi_roundtrips_v24() {
        for bits in [8u8, 16u8] {
            let fractions: Vec<u16> = (0..5).map(|i| (i as u16) * 0x1000).collect();
            let original = Id3Tag {
                version: Id3Version::V2_4,
                frames: vec![Id3Frame::AudioSeekPointIndex {
                    indexed_data_start: 0x0000_2A00,
                    indexed_data_length: 0x000F_4240,
                    bits_per_index_point: bits,
                    fractions: if bits == 8 {
                        fractions.iter().map(|&f| f >> 8).collect()
                    } else {
                        fractions.clone()
                    },
                }],
            };
            let bytes = write_tag(&original, Id3Version::V2_4).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            assert_eq!(parsed.frames.len(), 1);
            match &parsed.frames[0] {
                Id3Frame::AudioSeekPointIndex {
                    indexed_data_start,
                    indexed_data_length,
                    bits_per_index_point,
                    fractions: got,
                } => {
                    assert_eq!(*indexed_data_start, 0x0000_2A00);
                    assert_eq!(*indexed_data_length, 0x000F_4240);
                    assert_eq!(*bits_per_index_point, bits);
                    let expected: Vec<u16> = if bits == 8 {
                        fractions.iter().map(|&f| f >> 8).collect()
                    } else {
                        fractions.clone()
                    };
                    assert_eq!(got, &expected);
                }
                other => panic!("expected AudioSeekPointIndex, got {other:?}"),
            }
        }
    }

    /// Writing an `ASPI` with a non-conforming bit width is a hard
    /// error — the resulting bytes would be unreadable by a conformant
    /// parser, so we refuse rather than emit them.
    #[test]
    fn aspi_write_rejects_unsupported_bits() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::AudioSeekPointIndex {
                indexed_data_start: 0,
                indexed_data_length: 0,
                bits_per_index_point: 12, // not 8 or 16
                fractions: vec![0, 1, 2],
            }],
        };
        assert!(write_tag(&tag, Id3Version::V2_4).is_err());
    }

    /// `encode_counter` produces 4 bytes for a u32-range value, 5 for
    /// the smallest 33-bit value, and 8 for a u64-range value.
    #[test]
    fn encode_counter_widths() {
        let mut buf = Vec::new();
        encode_counter(&mut buf, 0);
        assert_eq!(buf.len(), 4);
        let mut buf = Vec::new();
        encode_counter(&mut buf, u32::MAX as u64);
        assert_eq!(buf.len(), 4);
        let mut buf = Vec::new();
        encode_counter(&mut buf, (u32::MAX as u64) + 1);
        assert_eq!(buf.len(), 5);
        let mut buf = Vec::new();
        encode_counter(&mut buf, u64::MAX);
        assert_eq!(buf.len(), 8);
    }

    // ── Unsync round-trip ──────────────────────────────────────────────
    //
    // Spec §6.1: `apply_unsync` (writer) inserts a `0x00` after every
    // `0xFF` that would otherwise be followed by the MPEG sync pattern
    // `%111xxxxx`, by a literal `0x00`, or by end-of-buffer. The
    // parser-side `reverse_unsync` removes those `0x00` bytes again.
    // The two should compose to the identity on any input.

    /// `apply_unsync` + `reverse_unsync` is the identity for every
    /// notable byte sequence: empty, sync-pattern, literal `$FF $00`,
    /// trailing `$FF`, and a stream with no `$FF` at all.
    #[test]
    fn unsync_apply_then_reverse_is_identity() {
        let cases: &[&[u8]] = &[
            &[],
            &[0x00, 0x01, 0x02],
            &[0xFF, 0xE0, 0x55], // false MPEG sync
            &[0xFF, 0xFB, 0x10], // another false sync
            &[0xFF, 0x00, 0x42], // literal $FF $00 (must be protected)
            &[0xFF],             // trailing $FF
            &[0x10, 0xFF, 0xFF, 0xE0, 0x00, 0xFF],
            &[0xFF, 0xFF, 0xFF, 0xFF], // run of $FF (first three trail another $FF, last is EOF)
        ];
        for input in cases {
            let encoded = apply_unsync(input);
            let decoded = reverse_unsync(&encoded);
            assert_eq!(
                decoded.as_slice(),
                *input,
                "round-trip failed for {input:?}"
            );
        }
    }

    /// `apply_unsync` never produces a buffer containing a false
    /// synchronisation pattern (`$FF` followed by `%111xxxxx`), a
    /// literal `$FF $00`, or a trailing `$FF`. This is the property
    /// the spec requires of a "completely unsynchronised" tag body.
    #[test]
    fn unsync_apply_eliminates_false_syncs() {
        let inputs: &[&[u8]] = &[
            &[0xFF, 0xE0, 0xFF, 0x00, 0xFF, 0xFB, 0x42],
            &[0xFF; 8],
            &[0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00],
        ];
        for input in inputs {
            let out = apply_unsync(input);
            // No two consecutive bytes (X, Y) such that X==0xFF and (Y & 0xE0)==0xE0
            // or (X==0xFF and Y==0x00 was protected by a *third* zero) — the test
            // here is that re-reversing yields the input, which is the spec-level
            // round-trip guarantee.
            for w in out.windows(2) {
                if w[0] == 0xFF {
                    // After apply_unsync, the byte following any 0xFF in the
                    // output must be either a non-escaped 0x00 (sentinel) or
                    // something whose top 3 bits are NOT all 1.
                    if w[1] != 0x00 {
                        assert!(
                            (w[1] & 0xE0) != 0xE0,
                            "false sync survived: 0xFF 0x{:02X}",
                            w[1]
                        );
                    }
                }
            }
            // Trailing 0xFF must have been escaped (followed by an
            // appended 0x00) — the *first* byte of any trailing 0xFF
            // is at position out.len() - 2 (the 0x00 sentinel sits at
            // out.len() - 1).
            if let Some(&last) = out.last() {
                if !input.is_empty() && input[input.len() - 1] == 0xFF {
                    assert_eq!(
                        last,
                        0x00,
                        "trailing 0xFF was not escaped: {:02X?}",
                        &out[out.len().saturating_sub(4)..]
                    );
                }
            }
        }
    }

    /// `write_tag_with_options(..., UnsyncMode::WholeTag)` produces a
    /// tag whose synchsafe size reflects the post-unsync length, with
    /// the header flag bit 0x80 set, that parses back to a tag
    /// containing the original frame payload byte-for-byte. v2.3 path.
    #[test]
    fn write_then_parse_whole_tag_unsync_v23() {
        // PRIV is a passthrough binary frame, perfect for exercising
        // arbitrary byte sequences (including 0xFF followed by 0xE0).
        let owner = "test@example.com".to_string();
        let payload = vec![0xFF, 0xE0, 0xAA, 0xFF, 0x00, 0x55, 0xFF];
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Private {
                owner: owner.clone(),
                data: payload.clone(),
            }],
        };
        let opts = WriteOptions::new().with_unsync(UnsyncMode::WholeTag);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap();
        // Header unsync flag set.
        assert_eq!(bytes[5] & 0x80, 0x80);
        // No raw false-sync survives in the body (header still
        // contains a literal version byte etc. but the body starts
        // at offset 10 and must obey the rule).
        for w in bytes[10..].windows(2) {
            if w[0] == 0xFF {
                assert!(w[1] == 0x00 || (w[1] & 0xE0) != 0xE0);
            }
        }
        let (parsed, consumed) = parse_tag(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert!(matches!(parsed.frames[0], Id3Frame::Private { .. }));
        if let Id3Frame::Private { owner: o, data: d } = &parsed.frames[0] {
            assert_eq!(o, &owner);
            assert_eq!(d, &payload);
        }
    }

    /// Same property, v2.4 path, whole-tag unsync.
    #[test]
    fn write_then_parse_whole_tag_unsync_v24() {
        let owner = "test@example.com".to_string();
        let payload = vec![0xFF, 0xE0, 0xAA, 0xFF, 0x00, 0x55, 0xFF];
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Private {
                owner: owner.clone(),
                data: payload.clone(),
            }],
        };
        let opts = WriteOptions::new().with_unsync(UnsyncMode::WholeTag);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        assert_eq!(bytes[5] & 0x80, 0x80);
        let (parsed, _) = parse_tag(&bytes).unwrap();
        if let Id3Frame::Private { owner: o, data: d } = &parsed.frames[0] {
            assert_eq!(o, &owner);
            assert_eq!(d, &payload);
        } else {
            panic!("expected Private frame, got {:?}", parsed.frames[0]);
        }
    }

    /// `UnsyncMode::PerFrame` on v2.4 sets format-flag bit 0x02 on
    /// each frame, applies unsync to that frame's payload, and the
    /// parser reverses it correctly. The header flag bit 0x80 is
    /// also set per spec §6.1 (all frames unsynchronised).
    #[test]
    fn write_then_parse_per_frame_unsync_v24() {
        let owner = "owner".to_string();
        let payload_a = vec![0xFF, 0xE0, 0x01];
        let payload_b = vec![0xAA, 0xBB, 0xFF];
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Private {
                    owner: owner.clone(),
                    data: payload_a.clone(),
                },
                Id3Frame::Private {
                    owner: owner.clone(),
                    data: payload_b.clone(),
                },
            ],
        };
        let opts = WriteOptions::new().with_unsync(UnsyncMode::PerFrame);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        // PerFrame deliberately leaves the header flag clear (see
        // write_tag_with_options) so the parser doesn't double-reverse
        // the per-frame unsynced payload.
        assert_eq!(bytes[5] & 0x80, 0);
        // Walk the body and verify each frame header carries the
        // format-flag bit 0x02. v2.4 frame header is 10 bytes:
        // 4 id + 4 size + 1 status + 1 format flags.
        let mut cursor = 10usize;
        let mut frames_seen = 0;
        while cursor + 10 <= bytes.len() {
            if !bytes[cursor..cursor + 4]
                .iter()
                .all(|b| b.is_ascii_alphanumeric())
            {
                break;
            }
            let s0 = bytes[cursor + 4] as u32;
            let s1 = bytes[cursor + 5] as u32;
            let s2 = bytes[cursor + 6] as u32;
            let s3 = bytes[cursor + 7] as u32;
            let fsize =
                ((s0 & 0x7F) << 21) | ((s1 & 0x7F) << 14) | ((s2 & 0x7F) << 7) | (s3 & 0x7F);
            let format_flags = bytes[cursor + 9];
            assert_eq!(format_flags & 0x02, 0x02, "per-frame unsync bit not set");
            cursor += 10 + fsize as usize;
            frames_seen += 1;
        }
        assert_eq!(frames_seen, 2);
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 2);
        if let Id3Frame::Private { data, .. } = &parsed.frames[0] {
            assert_eq!(data, &payload_a);
        } else {
            panic!();
        }
        if let Id3Frame::Private { data, .. } = &parsed.frames[1] {
            assert_eq!(data, &payload_b);
        } else {
            panic!();
        }
    }

    /// Selecting `PerFrame` under a v2.3 target downgrades silently
    /// to `WholeTag` (v2.3 has no per-frame unsync format-flag bit).
    /// The output is still a valid v2.3 tag with the header flag set
    /// and parses back to the original frames.
    #[test]
    fn per_frame_under_v23_downgrades_to_whole_tag() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Private {
                owner: "owner".into(),
                data: vec![0xFF, 0xE0, 0x33],
            }],
        };
        let opts = WriteOptions::new().with_unsync(UnsyncMode::PerFrame);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap();
        assert_eq!(bytes[5] & 0x80, 0x80);
        // v2.3 frame format-flags byte is at offset 10 + (4 + 4 + 1) = 19
        // for the first frame. For a downgraded WholeTag it must be 0.
        // (The body containing the frame may itself have been unsynced,
        // so we don't read past the header — but the first frame header
        // is fixed-position since the body starts at offset 10.)
        // After whole-tag unsync, the frame id may have shifted only if
        // any 0xFF lives in the header — it doesn't, so the frame still
        // begins at offset 10.
        let format_flags = bytes[10 + 4 + 4 + 1];
        assert_eq!(
            format_flags & 0x02,
            0,
            "v2.3 must not set per-frame unsync bit"
        );
        let (parsed, _) = parse_tag(&bytes).unwrap();
        if let Id3Frame::Private { data, .. } = &parsed.frames[0] {
            assert_eq!(data, &[0xFF, 0xE0, 0x33]);
        } else {
            panic!();
        }
    }

    /// `write_tag` (the no-options shorthand) keeps its historical
    /// behaviour: no unsync flag, no unsync transform. Equivalent to
    /// `write_tag_with_options(..., WriteOptions::default())`.
    #[test]
    fn write_tag_default_unchanged() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["hello".into()],
            }],
        };
        let a = write_tag(&tag, Id3Version::V2_4).unwrap();
        let b = write_tag_with_options(&tag, Id3Version::V2_4, &WriteOptions::default()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[5] & 0x80, 0, "default writer must not set unsync flag");
    }

    // -----------------------------------------------------------------
    // ID3v2.4 footer (spec §3.4)
    // -----------------------------------------------------------------

    /// `WriteOptions::with_footer(true)` on v2.4 sets header bit 0x10
    /// and emits a 10-byte "3DI..." trailer that mirrors the header's
    /// version / flags / size. The output round-trips through
    /// `parse_tag` and `consumed` reports header + body + 10-byte
    /// footer.
    #[test]
    fn write_then_parse_footer_v24_default() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["footer-bearing".into()],
            }],
        };
        let opts = WriteOptions::new().with_footer(true);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        // Header footer-flag set.
        assert_eq!(bytes[5] & 0x10, 0x10);
        // Footer is the last 10 bytes; identifier "3DI" + matching
        // header bytes 3..10.
        let footer = &bytes[bytes.len() - 10..];
        assert_eq!(&footer[0..3], b"3DI");
        assert_eq!(footer[3], 4); // major
        assert_eq!(footer[4], 0); // revision
        assert_eq!(footer[5], bytes[5]); // flags match
        assert_eq!(&footer[6..10], &bytes[6..10]); // size match
        let (parsed, consumed) = parse_tag(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(parsed.frames.len(), 1);
        if let Id3Frame::Text { id, values } = &parsed.frames[0] {
            assert_eq!(id, "TIT2");
            assert_eq!(values, &vec!["footer-bearing".to_string()]);
        } else {
            panic!("expected Text frame");
        }
    }

    /// Footer + WholeTag unsync compose: the body is unsynchronised
    /// (header bit 0x80 also set), the footer is *outside* the unsync
    /// region (it lives after the announced synchsafe size), and the
    /// round-trip still recovers the original frames byte-exact.
    #[test]
    fn write_then_parse_footer_v24_with_whole_tag_unsync() {
        let payload = vec![0xFF, 0xE0, 0x55, 0xFF, 0x00, 0xAA];
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Private {
                owner: "owner".into(),
                data: payload.clone(),
            }],
        };
        let opts = WriteOptions::new()
            .with_unsync(UnsyncMode::WholeTag)
            .with_footer(true);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        // Both flags set.
        assert_eq!(bytes[5] & 0x80, 0x80);
        assert_eq!(bytes[5] & 0x10, 0x10);
        // Footer is the trailing 10 bytes; identifier matches.
        assert_eq!(&bytes[bytes.len() - 10..bytes.len() - 7], b"3DI");
        let (parsed, consumed) = parse_tag(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        if let Id3Frame::Private { data, .. } = &parsed.frames[0] {
            assert_eq!(data, &payload);
        } else {
            panic!("expected Private frame");
        }
    }

    /// Footer + extended-header CRC compose: ext-header bit 0x40,
    /// footer bit 0x10, and the writer emits all three regions in the
    /// right order (header → ext-header → frames → footer). The CRC
    /// region (frames + padding, here just frames) is verified by the
    /// parser even though a footer follows; the spec's "data between
    /// the header and footer" wording is honoured because the footer
    /// bytes live *outside* the announced synchsafe size.
    #[test]
    fn write_then_parse_footer_v24_with_crc() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["crc + footer".into()],
            }],
        };
        let opts = WriteOptions::new().with_crc(true).with_footer(true);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        assert_eq!(bytes[5] & 0x40, 0x40); // ext-header
        assert_eq!(bytes[5] & 0x10, 0x10); // footer
        assert_eq!(&bytes[bytes.len() - 10..bytes.len() - 7], b"3DI");
        let (parsed, consumed) = parse_tag(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        if let Id3Frame::Text { id, values } = &parsed.frames[0] {
            assert_eq!(id, "TIT2");
            assert_eq!(values, &vec!["crc + footer".to_string()]);
        } else {
            panic!("expected Text frame");
        }
    }

    /// Requesting a footer on a v2.3 target is rejected: the v2.3
    /// spec doesn't define the footer-flag bit, so silently emitting
    /// one would produce a tag that v2.3-only parsers would
    /// misinterpret (and our own parser would reject on read).
    #[test]
    fn footer_request_on_v23_errors() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let opts = WriteOptions::new().with_footer(true);
        let err = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("v2.4-only") || msg.contains("footer"),
            "unexpected error message: {msg}"
        );
    }

    /// A tag claiming a footer on its v2.3 header byte is rejected on
    /// parse — the spec only defines the footer for v2.4.
    #[test]
    fn parse_rejects_footer_flag_on_v23() {
        // Hand-assemble a minimal v2.3 tag with a single TIT2 frame
        // and the spurious footer flag set on the header.
        let frame = {
            let mut f = Vec::new();
            f.extend_from_slice(b"TIT2");
            // size = 1 (encoding byte) + 1 (text byte) = 2; v2.3 size
            // is a regular u32.
            f.extend_from_slice(&[0, 0, 0, 2]);
            f.extend_from_slice(&[0, 0]); // flags
            f.push(0x00); // encoding = ISO-8859-1
            f.push(b'x');
            f
        };
        let size = frame.len() as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ID3");
        bytes.push(3); // major
        bytes.push(0); // revision
        bytes.push(0x10); // footer flag — illegal on v2.3
        bytes.push(((size >> 21) & 0x7F) as u8);
        bytes.push(((size >> 14) & 0x7F) as u8);
        bytes.push(((size >> 7) & 0x7F) as u8);
        bytes.push((size & 0x7F) as u8);
        bytes.extend_from_slice(&frame);
        // Append 10 "3DI..." trailing bytes so the buffer is long
        // enough to fail on the version check rather than NeedMore.
        bytes.extend_from_slice(b"3DI");
        bytes.push(3);
        bytes.push(0);
        bytes.push(0x10);
        bytes.push(((size >> 21) & 0x7F) as u8);
        bytes.push(((size >> 14) & 0x7F) as u8);
        bytes.push(((size >> 7) & 0x7F) as u8);
        bytes.push((size & 0x7F) as u8);
        let err = parse_tag(&bytes).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("v2.4-only") || msg.contains("v2.4"),
            "unexpected error message: {msg}"
        );
    }

    /// A footer-bearing tag whose trailer is corrupted is rejected
    /// with a specific error (not silently accepted).
    #[test]
    fn parse_rejects_corrupt_footer_magic() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let mut bytes = write_tag_with_options(
            &tag,
            Id3Version::V2_4,
            &WriteOptions::new().with_footer(true),
        )
        .unwrap();
        // Smash the footer identifier.
        let f = bytes.len() - 10;
        bytes[f] = b'X';
        let err = parse_tag(&bytes).unwrap_err();
        assert!(format!("{err}").contains("footer magic"));
    }

    /// Mismatched footer size relative to header size is rejected.
    #[test]
    fn parse_rejects_footer_size_mismatch() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let mut bytes = write_tag_with_options(
            &tag,
            Id3Version::V2_4,
            &WriteOptions::new().with_footer(true),
        )
        .unwrap();
        // Flip the last synchsafe byte of the footer's size field.
        let len = bytes.len();
        bytes[len - 1] ^= 0x01;
        let err = parse_tag(&bytes).unwrap_err();
        assert!(format!("{err}").contains("footer size"));
    }

    /// Mismatched footer flags relative to header flags are rejected.
    #[test]
    fn parse_rejects_footer_flags_mismatch() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let mut bytes = write_tag_with_options(
            &tag,
            Id3Version::V2_4,
            &WriteOptions::new().with_footer(true),
        )
        .unwrap();
        // Twiddle the footer's flag byte (offset len-10+5 = len-5).
        let len = bytes.len();
        bytes[len - 5] ^= 0x40;
        let err = parse_tag(&bytes).unwrap_err();
        assert!(format!("{err}").contains("footer flags"));
    }

    /// A truncated buffer (header announces footer but the 10 trailer
    /// bytes are missing) is reported via `Error::NeedMore` so the
    /// caller can read more — *not* as a parse failure that would
    /// mislead the caller into discarding the file's cursor.
    #[test]
    fn parse_truncated_footer_returns_need_more() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let bytes = write_tag_with_options(
            &tag,
            Id3Version::V2_4,
            &WriteOptions::new().with_footer(true),
        )
        .unwrap();
        // Drop the last 5 footer bytes to simulate a short read.
        let short = &bytes[..bytes.len() - 5];
        match parse_tag(short) {
            Err(Error::NeedMore) => {}
            other => panic!("expected NeedMore, got {other:?}"),
        }
    }

    /// `tag_size_at_head` already reports footer-inclusive totals (the
    /// existing pre-implementation behaviour). Confirm the writer's
    /// output is consistent with that: head-peeking the first 10 bytes
    /// of a footer-bearing tag returns exactly the written length.
    #[test]
    fn tag_size_at_head_includes_footer() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["title".into()],
            }],
        };
        let bytes = write_tag_with_options(
            &tag,
            Id3Version::V2_4,
            &WriteOptions::new().with_footer(true),
        )
        .unwrap();
        let total = tag_size_at_head(&bytes[..10]).unwrap();
        assert_eq!(total, bytes.len());
    }

    /// Default writer (no options) does NOT set the footer flag and
    /// does NOT emit a footer — historical behaviour preserved.
    #[test]
    fn write_tag_default_no_footer() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(
            bytes[5] & 0x10,
            0,
            "default writer must not set footer flag"
        );
        // Footer is not present; the tail bytes are frame data, not "3DI".
        assert!(bytes.len() < 13 || &bytes[bytes.len() - 10..bytes.len() - 7] != b"3DI");
    }

    /// `MLLT` bit writer / reader pinned against a hand-computed byte
    /// sequence. The spec packs each per-reference `(bytes_dev, ms_dev)`
    /// MSB-first across byte boundaries; this test makes the chosen
    /// ordering hard to drift on. Two references at `bits_for_bytes = 12`
    /// and `bits_for_ms = 4` (16 bits each = 2 bytes per reference, so
    /// the result aligns on bytes for easy hand-checking):
    ///
    /// * Ref 0: bytes_dev = 0xABC, ms_dev = 0xD →
    ///   bits = `1010 1011 1100 1101` = `0xAB 0xCD`.
    /// * Ref 1: bytes_dev = 0x123, ms_dev = 0x4 →
    ///   bits = `0001 0010 0011 0100` = `0x12 0x34`.
    #[test]
    fn mllt_bit_packing_pins_msb_first_order() {
        let mut w = BitWriter::new();
        w.push(0xABC, 12);
        w.push(0xD, 4);
        w.push(0x123, 12);
        w.push(0x4, 4);
        let packed = w.into_bytes();
        assert_eq!(packed, vec![0xAB, 0xCD, 0x12, 0x34]);

        let mut r = BitReader::new(&packed);
        assert_eq!(r.take(12), 0xABC);
        assert_eq!(r.take(4), 0xD);
        assert_eq!(r.take(12), 0x123);
        assert_eq!(r.take(4), 0x4);
        assert_eq!(r.remaining(), 0);
    }

    /// `MLLT` encode → decode round-trip for the sub-byte-aligned case
    /// where a reference crosses a byte boundary. `9 + 7 = 16` bits is
    /// a multiple of 4 but neither field aligns; we want the byte
    /// stream to still round-trip exactly.
    #[test]
    fn mllt_subbyte_alignment_roundtrip() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::MpegLocationLookup {
                mpeg_frames_between_reference: 4,
                bytes_between_reference: 0x10_0000,
                ms_between_reference: 0x00_0FA0,
                bits_for_bytes_deviation: 9,
                bits_for_ms_deviation: 7,
                references: vec![(0x1FF, 0x7F), (0x000, 0x00), (0x100, 0x40)],
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let got = parsed
            .frames
            .iter()
            .find_map(|f| match f {
                Id3Frame::MpegLocationLookup {
                    bits_for_bytes_deviation,
                    bits_for_ms_deviation,
                    references,
                    ..
                } => Some((
                    *bits_for_bytes_deviation,
                    *bits_for_ms_deviation,
                    references.clone(),
                )),
                _ => None,
            })
            .expect("MLLT must survive sub-byte round-trip");
        assert_eq!(got.0, 9);
        assert_eq!(got.1, 7);
        assert_eq!(got.2, vec![(0x1FF, 0x7F), (0x000, 0x00), (0x100, 0x40)]);
    }

    /// `RVRB` writer emits exactly 12 bytes pinned to the spec field
    /// order: u16 BE left ms, u16 BE right ms, then eight single bytes
    /// (bounces L/R, four feedback bytes L→L / L→R / R→R / R→L, then
    /// premix L→R / R→L).
    #[test]
    fn rvrb_writer_pinned_bytes() {
        let frame = Id3Frame::Reverb {
            reverb_left_ms: 0x1234,
            reverb_right_ms: 0x5678,
            bounces_left: 0x10,
            bounces_right: 0xFF, // spec: $FF = infinite
            feedback_ll: 0x7F,   // spec example: 50% reduction
            feedback_lr: 0x01,
            feedback_rr: 0x80,
            feedback_rl: 0x02,
            premix_lr: 0xAA,
            premix_rl: 0x55,
        };
        let (id, payload) = encode_frame(Id3Version::V2_4, &frame).unwrap();
        assert_eq!(id, "RVRB");
        assert_eq!(
            payload,
            vec![0x12, 0x34, 0x56, 0x78, 0x10, 0xFF, 0x7F, 0x01, 0x80, 0x02, 0xAA, 0x55]
        );
    }

    /// `RVRB` round-trips through `write_tag` / `parse_tag` for both
    /// v2.3 and v2.4 (the wire layout is byte-aligned and
    /// version-independent).
    #[test]
    fn rvrb_roundtrip_v23_and_v24() {
        let original = Id3Frame::Reverb {
            reverb_left_ms: 250,
            reverb_right_ms: 300,
            bounces_left: 4,
            bounces_right: 4,
            feedback_ll: 0x7F,
            feedback_lr: 0x10,
            feedback_rr: 0x7F,
            feedback_rl: 0x10,
            premix_lr: 0x20,
            premix_rl: 0x20,
        };
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![original.clone()],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            assert_eq!(parsed.frames.len(), 1);
            match (&parsed.frames[0], &original) {
                (
                    Id3Frame::Reverb {
                        reverb_left_ms: a_l,
                        reverb_right_ms: a_r,
                        bounces_left: a_bl,
                        bounces_right: a_br,
                        feedback_ll: a_fll,
                        feedback_lr: a_flr,
                        feedback_rr: a_frr,
                        feedback_rl: a_frl,
                        premix_lr: a_plr,
                        premix_rl: a_prl,
                    },
                    Id3Frame::Reverb {
                        reverb_left_ms: b_l,
                        reverb_right_ms: b_r,
                        bounces_left: b_bl,
                        bounces_right: b_br,
                        feedback_ll: b_fll,
                        feedback_lr: b_flr,
                        feedback_rr: b_frr,
                        feedback_rl: b_frl,
                        premix_lr: b_plr,
                        premix_rl: b_prl,
                    },
                ) => {
                    assert_eq!((a_l, a_r, a_bl, a_br), (b_l, b_r, b_bl, b_br));
                    assert_eq!(
                        (a_fll, a_flr, a_frr, a_frl, a_plr, a_prl),
                        (b_fll, b_flr, b_frr, b_frl, b_plr, b_prl)
                    );
                }
                (other, _) => panic!("expected Reverb after round-trip, got {other:?}"),
            }
        }
    }

    /// Truncated RVRB payloads (< 12 bytes) must NOT silently fabricate
    /// zeroed fields. The parser surfaces the raw bytes through
    /// `Id3Frame::Unknown` so the round-trip preserves the original.
    #[test]
    fn rvrb_short_payload_surfaces_unknown() {
        // 11 bytes — one short of the spec layout.
        let got = parse_rvrb(&[0; 11]);
        match got {
            Id3Frame::Unknown { id, raw } => {
                assert_eq!(id, "RVRB");
                assert_eq!(raw.len(), 11);
            }
            other => panic!("expected Unknown for short RVRB, got {other:?}"),
        }
        // Zero bytes — corner case.
        let got = parse_rvrb(&[]);
        match got {
            Id3Frame::Unknown { id, raw } => {
                assert_eq!(id, "RVRB");
                assert!(raw.is_empty());
            }
            other => panic!("expected Unknown for empty RVRB, got {other:?}"),
        }
    }

    /// RVRB payloads longer than 12 bytes decode the leading 12 and
    /// drop the trailing bytes per spec "skip unknown bytes". The
    /// extra bytes do not appear in the decoded frame so a writer
    /// re-serialises only the canonical 12 — this is a deliberate
    /// non-round-trip when the on-wire form had trailing junk.
    #[test]
    fn rvrb_trailing_bytes_are_dropped() {
        let mut payload = vec![
            0x00, 0x10, 0x00, 0x20, 0x05, 0x05, 0x7F, 0x10, 0x7F, 0x10, 0x40, 0x40,
        ];
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let got = parse_rvrb(&payload);
        match got {
            Id3Frame::Reverb {
                reverb_left_ms,
                reverb_right_ms,
                bounces_left,
                premix_rl,
                ..
            } => {
                assert_eq!(reverb_left_ms, 0x0010);
                assert_eq!(reverb_right_ms, 0x0020);
                assert_eq!(bounces_left, 0x05);
                assert_eq!(premix_rl, 0x40);
            }
            other => panic!("expected Reverb, got {other:?}"),
        }
    }

    /// The v2.2 `REV` id is the 3-char form of v2.3 `RVRB`. The parser
    /// must promote it to the structured variant when the payload is
    /// well-formed, matching the existing v2.2 → v2.3 promotion table
    /// (TT2 → TIT2, PIC → APIC, ...).
    #[test]
    fn rvrb_v22_rev_promotes_to_reverb() {
        let payload = [
            0x00, 0x64, 0x00, 0xC8, 0x03, 0x03, 0x40, 0x10, 0x40, 0x10, 0x20, 0x20,
        ];
        let got = dispatch_v22("REV", &payload);
        match got {
            Id3Frame::Reverb {
                reverb_left_ms,
                reverb_right_ms,
                ..
            } => {
                assert_eq!(reverb_left_ms, 100);
                assert_eq!(reverb_right_ms, 200);
            }
            other => panic!("expected Reverb from REV, got {other:?}"),
        }
    }

    /// `to_key_value_pairs` is the Vorbis-style flat-pair view. RVRB
    /// carries no text values (it is a pure DSP descriptor), so it
    /// must contribute zero pairs — adding a `Reverb` frame to a tag
    /// must not perturb the pair output of an otherwise-empty tag.
    #[test]
    fn rvrb_yields_no_key_value_pairs() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Reverb {
                reverb_left_ms: 50,
                reverb_right_ms: 50,
                bounces_left: 1,
                bounces_right: 1,
                feedback_ll: 0,
                feedback_lr: 0,
                feedback_rr: 0,
                feedback_rl: 0,
                premix_lr: 0,
                premix_rl: 0,
            }],
        };
        assert!(to_key_value_pairs(&tag).is_empty());
    }
}
