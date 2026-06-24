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
//! Frame-level zlib compression is decoded in both dialects — the
//! v2.3 format flag (§3.3 flag `i`, with the 4-byte decompressed-size
//! header addition) and the v2.4 format flag (§4.1.2 flag `k`, with
//! the mandatory data-length indicator) — and the v2.3 encryption /
//! grouping-identity header additions are stripped per spec order so
//! a flagged frame's payload is never dispatched off-by-N.
//! [`WriteOptions::with_compression`] emits compressed frames on the
//! writer side.
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
//! * The full ID3v2.2.0 §4 frame table — every declared 3-char id
//!   maps onto the typed variants below (`UFI`/`IPL`/`MCI`/`ETC`/
//!   `MLL`/`STC`/`SLT`/`COM`/`RVA`/`EQU`/`REV`/`PIC`/`GEO`/`CNT`/
//!   `POP`/`BUF`/`CRA`/`LNK` plus all text and URL ids), except `CRM`
//!   (encrypted meta frame, v2.2 §4.20 — no v2.3/v2.4 descendant)
//!   which is preserved via [`Id3Frame::Unknown`]. The v2.2 header
//!   compression bit (§3.1 flag bit 6) makes the parser ignore the
//!   entire tag body per spec while still reporting the correct
//!   consumed size.
//! * `POPM` popularimeter (email + rating + play counter).
//! * `PCNT` play counter.
//! * `PRIV` private frame (owner id + opaque bytes).
//! * `GEOB` general encapsulated object.
//! * `UFID` unique file identifier.
//! * `USER` terms-of-use frame.
//! * `OWNE` ownership / `COMR` commercial.
//! * `SYTC` synchronised tempo codes.
//! * `RVA2` / `EQU2` relative volume + equalisation (v2.4).
//! * `RVAD` relative volume adjustment (v2.3 §4.12 — front/back/centre/bass).
//! * `EQUA` equalisation (v2.3 §4.13 — adjustment-bits + interpolated bands).
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
    /// `RVAD` relative volume adjustment (spec v2.3 §4.12). The v2.3
    /// predecessor of v2.4's `RVA2`: per-channel signed volume deltas
    /// where the sign comes from a shared inc/dec bitfield and each
    /// magnitude is an unsigned big-endian integer whose width is
    /// `ceil(bits_used / 8)` bytes. The wire order is fixed by spec —
    /// front (right, then left), then optional back (right-back, then
    /// left-back), then optional centre, then optional bass — and a
    /// channel block appears iff at least one of its bits is set in
    /// `increment_decrement` (bit 0 = right, 1 = left, 2 = right-back,
    /// 3 = left-back, 4 = centre, 5 = bass; top two bits reserved
    /// `%00`).
    ///
    /// Peak fields follow each delta with the same width. The spec
    /// allows them to be omitted ("if no other data follows, be
    /// completely omitted") — that's surfaced as `peak.is_empty()`
    /// while `volume_delta` carries the `ceil(bits_used / 8)`-byte
    /// magnitude. `bits_used` may not be `$00` per spec; the writer
    /// rejects it. This frame is v2.3-only: v2.4 dropped it in favour
    /// of `RVA2` (which the v2.4 frames doc describes without listing
    /// `RVAD`), so the writer returns `Error::unsupported` when asked
    /// to serialise an `Rvad` under a `V2_4` envelope, matching the
    /// `with_footer` + `V2_3` rejection pattern.
    Rvad {
        /// Raw inc/dec bitfield (top two bits reserved `%00`). Bits
        /// 0..=5 declare which channels are present and whether each
        /// delta is positive (`1` = increment) or negative
        /// (`0` = decrement). The bitfield drives both presence and
        /// sign; we keep the raw byte so callers can inspect reserved
        /// bits and so the writer round-trips bit-for-bit.
        increment_decrement: u8,
        /// Volume-description width in bits per spec ("normally `$10`
        /// (16 bits) for MPEG 2 layer I, II and III and MPEG 2.5").
        /// Must be non-zero. The on-wire byte width per delta or peak
        /// is `ceil(bits_used / 8)`; the high bits are zero-padded
        /// when `bits_used` is not a multiple of 8.
        bits_used: u8,
        /// Front-channel block (right then left). `Some` iff
        /// `increment_decrement & 0b0000_0011 != 0`. Both magnitudes
        /// always present together (the spec lists `right` and `left`
        /// unconditionally as the first block).
        front: Option<RvadFrontChannels>,
        /// Back-channel block (right-back then left-back). `Some` iff
        /// `increment_decrement & 0b0000_1100 != 0`.
        back: Option<RvadBackChannels>,
        /// Centre channel. `Some` iff `increment_decrement & 0b0001_0000 != 0`.
        center: Option<RvadChannel>,
        /// Bass channel. `Some` iff `increment_decrement & 0b0010_0000 != 0`.
        bass: Option<RvadChannel>,
    },
    /// `EQUA` equalisation (spec v2.3 §4.13). The v2.3 predecessor of
    /// v2.4's `EQU2`: an interpolated equalisation curve described as a
    /// sequence of `(frequency, adjustment)` bands. Each band's
    /// `frequency` is a 15-bit unsigned integer in Hz (0..=32767) and
    /// its `adjustment` is an unsigned big-endian magnitude whose width
    /// in bytes is `ceil(adjustment_bits / 8)`. The sign of the
    /// adjustment is carried by `EquaBand::increment` (`true` for the
    /// spec's `1 = increment`, `false` for `0 = decrement`) — the spec
    /// stores that bit as the most-significant bit of the 16-bit
    /// frequency word, so the wire byte order is
    /// `[(inc<<7) | (freq>>8 & 0x7F), freq & 0xFF, adjustment_bytes...]`.
    ///
    /// Spec rules carried into the writer:
    ///
    /// * `adjustment_bits` may not be `$00` — rejected with
    ///   [`Error::invalid`]. The parser accepts `$00` and surfaces a
    ///   zero-byte `adjustment` per band so a non-conforming source
    ///   surfaces structurally rather than crashing.
    /// * `bands` must be sorted by `frequency` strictly increasing and
    ///   carry no duplicates (spec: "A frequency should only be
    ///   described once in the frame"); the writer rejects an
    ///   unsorted-or-duplicated list. The parser preserves wire order
    ///   so a caller can detect a non-conforming source.
    /// * Each `EquaBand::adjustment` must not exceed `ceil(adjustment_bits / 8)`
    ///   bytes on write; sub-width values are zero-padded at the high
    ///   end per spec "padded in the beginning (highest bits) when not
    ///   a multiple of eight".
    ///
    /// This frame is v2.3-only: v2.4 dropped it in favour of `EQU2`
    /// (the v2.4 frames doc lists `EQU2` and does not mention `EQUA`),
    /// so the writer returns [`Error::unsupported`] when asked to
    /// serialise an `Equa` under a `V2_4` envelope, matching the
    /// `RVAD` v2.3-only contract.
    Equa {
        /// Number of bits used per adjustment field per spec — `$10`
        /// (16 bits) is the spec-listed norm for MPEG audio. May not
        /// be `$00`. The on-wire byte width per adjustment is
        /// `ceil(adjustment_bits / 8)`; sub-byte widths zero-pad the
        /// high bits.
        adjustment_bits: u8,
        /// Equalisation bands in ascending frequency order. The spec
        /// requires the list to be sorted strictly increasing by
        /// frequency and to contain no duplicates; a reader interpolates
        /// adjustments between adjacent bands.
        bands: Vec<EquaBand>,
    },
    /// `IPLS` involved people list (spec v2.3 §4.4). A flat list of
    /// `(involvement, involvee)` pairs describing the role and the
    /// person filling it. The spec body is a single encoding byte
    /// followed by alternating NUL-terminated strings:
    /// `involvement_0\0 involvee_0\0 involvement_1\0 involvee_1\0 …`.
    /// Each pair carries one role/name binding (e.g. `producer\0Alice\0`,
    /// `mixing engineer\0Bob\0`).
    ///
    /// The spec also says "There may only be one `IPLS` frame in each
    /// tag" — uniqueness is a caller-level concern, matching how the
    /// crate treats `EQU2` / `MCDI` / `MLLT` / `RVRB` / `RVAD` / `EQUA`.
    ///
    /// `IPLS` is v2.3-only: v2.4 dropped it in favour of `TIPL`
    /// (involved-people-list, §4.2.2) and `TMCL` (musician-credits
    /// list), both of which are ordinary text frames the existing
    /// `Id3Frame::Text` variant already handles. The writer returns
    /// [`Error::unsupported`] when asked to serialise an `Ipls` under
    /// a `V2_4` envelope, matching the `RVAD` / `EQUA` v2.3-only
    /// contract.
    ///
    /// Pairs are stored as `Vec<(String, String)>` rather than a flat
    /// `Vec<String>` so a writer can never emit an odd count (the spec
    /// pairing is fundamental: each involvement names exactly one
    /// involvee). The parser folds a dangling final involvement (a
    /// non-conforming source that omits the trailing involvee) into a
    /// pair with an empty involvee, surfacing the truncation without
    /// crashing.
    Ipls { pairs: Vec<(String, String)> },
    /// `CRM` encrypted meta frame (ID3v2.2 §4.20). A v2.2-only frame
    /// that wraps one or more encrypted ID3v2 frames. It has no
    /// v2.3/v2.4 descendant — v2.3+ split its responsibilities across
    /// `ENCR` (encryption-method registration) and `AENC`/per-frame
    /// encryption flags. The structural fields are exposed verbatim:
    ///
    /// * `owner` — NUL-terminated ISO-8859-1 owner identifier. Per spec
    ///   this is "a terminated string with a URL containing an email
    ///   address" identifying the organisation responsible for the
    ///   encrypted block, so questions can be directed to it.
    /// * `content` — NUL-terminated ISO-8859-1 content/explanation
    ///   describing what is encrypted and why.
    /// * `encrypted` — the opaque encrypted datablock. It is preserved
    ///   verbatim; this crate carries no decryption plugins, so the
    ///   block is never interpreted (the spec defers the cipher to the
    ///   plugin keyed by `owner`).
    EncryptedMeta {
        owner: String,
        content: String,
        encrypted: Vec<u8>,
    },
    /// Any frame whose id we don't parse structurally (RGAD, CHAP,
    /// ...). The payload is preserved verbatim so callers or later
    /// versions can recognise it without needing to reparse.
    Unknown { id: String, raw: Vec<u8> },
}

/// One band of an `EQUA` equalisation curve (spec v2.3 §4.13). The
/// `increment` bit is stored on the wire as the most-significant bit
/// of the 2-byte big-endian frequency word; `frequency` carries the
/// low 15 bits (0..=32767 Hz). `adjustment` is an unsigned big-endian
/// magnitude — the sign comes from `increment` per the spec's "1 is
/// increment and 0 is decrement". The byte width of `adjustment`
/// matches the parent `Equa::adjustment_bits` rounded up; sub-byte
/// widths zero-pad the high bits when serialised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquaBand {
    /// Spec: the sign of the volume adjustment for this band. `true`
    /// for the spec's `1 = increment` (positive), `false` for
    /// `0 = decrement` (negative). Stored as the MSB of the on-wire
    /// 16-bit frequency word.
    pub increment: bool,
    /// 15-bit frequency in Hz (0..=32767). Values with the top bit
    /// set (>= 0x8000) are rejected by the writer since they collide
    /// with the increment/decrement bit.
    pub frequency: u16,
    /// Big-endian unsigned magnitude of the adjustment for this band.
    /// Width = `ceil(parent.adjustment_bits / 8)`. The sign comes from
    /// `increment`. The writer zero-pads sub-width values on the high
    /// end and rejects over-wide values.
    pub adjustment: Vec<u8>,
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

/// One `RVAD` channel entry (spec v2.3 §4.12). The volume delta is
/// stored as an unsigned big-endian magnitude — the sign lives in
/// the parent `Rvad::increment_decrement` bitfield where bit `n`
/// being `1` means the channel's delta is positive and `0` means
/// negative. Both fields are zero-padded to whole bytes when the
/// parent `bits_used` is not a multiple of 8 (high bits zero per
/// spec); the byte widths are always `ceil(bits_used / 8)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RvadChannel {
    /// Big-endian unsigned magnitude of the volume change. Width =
    /// `ceil(bits_used / 8)`. The on-wire value is the absolute
    /// adjustment; sign comes from the parent's inc/dec bitfield.
    pub volume_delta: Vec<u8>,
    /// Big-endian unsigned peak volume for this channel. Width =
    /// `ceil(bits_used / 8)`. Empty (`Vec::new()`) when the spec's
    /// "completely omitted" form applies — the parser surfaces an
    /// empty `peak` when the wire data ran out before the peak
    /// position, and the writer omits the bytes when `peak` is empty.
    pub peak: Vec<u8>,
}

/// Front-channel pair for `RVAD`. The spec lists `right` before
/// `left` in the on-wire layout (§4.12), so the struct mirrors that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RvadFrontChannels {
    /// Right channel (inc/dec bit 0). Magnitude in `volume_delta`,
    /// sign from the parent bitfield's bit 0.
    pub right: RvadChannel,
    /// Left channel (inc/dec bit 1). Magnitude in `volume_delta`,
    /// sign from the parent bitfield's bit 1.
    pub left: RvadChannel,
}

/// Back-channel pair for `RVAD`. Spec §4.12: "Relative volume change,
/// right back" precedes "left back" on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RvadBackChannels {
    /// Right-back channel (inc/dec bit 2).
    pub right_back: RvadChannel,
    /// Left-back channel (inc/dec bit 3).
    pub left_back: RvadChannel,
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

/// Typed view of the three-byte language field carried by the frames
/// whose content is language-tagged (`COMM`, `USLT`, `USER`, `SYLT`).
///
/// Spec wording (v2.4 structure doc): "The three byte language field,
/// present in several frames, is used to describe the language of the
/// frame's content, according to ISO-639-2. The language should be
/// represented in lower case. If the language is not known the string
/// 'XXX' should be used." The v2.3 structure doc carries the same
/// field with the ISO-639-2 reference but without the lower-case
/// recommendation or the explicit `XXX` sentinel — those are v2.4
/// additions, so an upper-case code on a v2.3 source is conformant
/// there while merely discouraged under v2.4.
///
/// Returned by [`Id3Frame::language`]. The three states separate the
/// two meaningful cases the spec calls out from everything else:
///
/// * [`Language::Unknown`] — the `XXX` sentinel. Matched
///   case-insensitively (`XXX`, `xxx`, or any mixed case) because the
///   v2.4 lower-case recommendation applies to ordinary codes and
///   real-world tags carry the sentinel in either case; the typed view
///   collapses them to one "language not known" state.
/// * [`Language::Code`] — a well-formed three-letter code, i.e. all
///   three bytes are ASCII letters and the code is not the `XXX`
///   sentinel. The stored bytes are normalised to lower case per the
///   v2.4 recommendation, so `Eng`, `eng`, and `ENG` all surface as
///   the same `Code(*b"eng")` and compare equal regardless of the
///   wire casing.
/// * [`Language::Malformed`] — anything else: bytes outside the ASCII
///   letter range, including the all-`$00` padding written when a
///   frame's language is absent or truncated. The raw bytes are
///   preserved verbatim so a caller can inspect or round-trip them
///   without the typed view silently rewriting non-conforming input.
///
/// The view is non-destructive: it never invents a code for malformed
/// input and never discards the original bytes, mirroring the posture
/// of the other typed accessors (e.g. [`TimestampUnit`]) that surface
/// `None` / a raw fallback rather than guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// The `XXX` "language not known" sentinel (matched
    /// case-insensitively per the doc above).
    Unknown,
    /// A well-formed three-letter ISO-639-2 code, normalised to lower
    /// case. The inner bytes are guaranteed to be three ASCII lower-case
    /// letters and are never the `XXX` sentinel.
    Code([u8; 3]),
    /// Wire bytes that are neither the `XXX` sentinel nor a three-letter
    /// alphabetic code (e.g. all-`$00` padding or digits). The raw
    /// bytes are preserved exactly as they appeared on the wire.
    Malformed([u8; 3]),
}

impl Language {
    /// Decode a raw three-byte language field into the typed view.
    ///
    /// All-letter input that equals `XXX` (any case) maps to
    /// [`Language::Unknown`]; any other all-ASCII-letter input maps to
    /// [`Language::Code`] with the bytes lower-cased per the v2.4
    /// recommendation; everything else maps to [`Language::Malformed`]
    /// with the bytes preserved verbatim.
    pub fn from_wire(bytes: [u8; 3]) -> Self {
        if !bytes.iter().all(|b| b.is_ascii_alphabetic()) {
            return Language::Malformed(bytes);
        }
        let lower = [
            bytes[0].to_ascii_lowercase(),
            bytes[1].to_ascii_lowercase(),
            bytes[2].to_ascii_lowercase(),
        ];
        if lower == *b"xxx" {
            Language::Unknown
        } else {
            Language::Code(lower)
        }
    }

    /// Encode this view back to a three-byte wire field.
    ///
    /// [`Language::Unknown`] serialises to the upper-case `XXX` sentinel
    /// spelt out in the v2.4 doc; [`Language::Code`] serialises to its
    /// stored lower-case bytes; [`Language::Malformed`] serialises to
    /// the preserved raw bytes. `from_wire` ∘ `to_wire` is the identity
    /// for `Unknown` and `Code`, and for any `Malformed` whose raw
    /// bytes are not coincidentally a valid code (i.e. it round-trips a
    /// value the decoder itself produced).
    pub fn to_wire(self) -> [u8; 3] {
        match self {
            Language::Unknown => *b"XXX",
            Language::Code(code) => code,
            Language::Malformed(raw) => raw,
        }
    }

    /// The lower-case ISO-639-2 code as a string slice when this is a
    /// well-formed [`Language::Code`]; `None` for [`Language::Unknown`]
    /// and [`Language::Malformed`]. Always valid UTF-8 because a
    /// `Code` is by construction three ASCII letters.
    pub fn as_code(&self) -> Option<&str> {
        match self {
            Language::Code(code) => std::str::from_utf8(code).ok(),
            _ => None,
        }
    }
}

/// Typed view of the `SYLT` "content type" byte (spec v2.3 §4.10 /
/// v2.4 §4.9). The byte sits between the `time_stamp_format` and the
/// content descriptor; its nine spec-defined values describe what
/// kind of text the synchronised payload carries (lyrics, chord
/// names, event labels, …). Returned by [`Id3Frame::sylt_content_type`];
/// see [`SyltContentType::from_wire`] for the wire mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyltContentType {
    /// `$00` per spec — "other" (catch-all when none of the more
    /// specific labels fits).
    Other,
    /// `$01` per spec — "lyrics" (song lyrics, the typical SYLT use).
    Lyrics,
    /// `$02` per spec — "text transcription" (e.g. dialogue
    /// transcribed for an audiobook).
    TextTranscription,
    /// `$03` per spec — "movement/part name" (e.g. `"Adagio"`).
    MovementPartName,
    /// `$04` per spec — "events" (e.g. `"Don Quijote enters the
    /// stage"`).
    Events,
    /// `$05` per spec — "chord" (e.g. `"Bb F Fsus"`).
    Chord,
    /// `$06` per spec — "trivia/'pop up' information".
    Trivia,
    /// `$07` per spec — "URLs to webpages". V2.4-only per the v2.4
    /// frames doc's nine-value list; v2.3's spec §4.10 stops at `$06`.
    UrlsToWebpages,
    /// `$08` per spec — "URLs to images". V2.4-only per the v2.4
    /// frames doc's nine-value list; v2.3's spec §4.10 stops at `$06`.
    UrlsToImages,
}

impl SyltContentType {
    /// Decode a raw SYLT `content_type` byte. Returns `None` for any
    /// value outside the `$00..=$08` range — a non-conforming source
    /// (reserved byte) surfaces structurally rather than mapping to a
    /// guessed variant.
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(SyltContentType::Other),
            1 => Some(SyltContentType::Lyrics),
            2 => Some(SyltContentType::TextTranscription),
            3 => Some(SyltContentType::MovementPartName),
            4 => Some(SyltContentType::Events),
            5 => Some(SyltContentType::Chord),
            6 => Some(SyltContentType::Trivia),
            7 => Some(SyltContentType::UrlsToWebpages),
            8 => Some(SyltContentType::UrlsToImages),
            _ => None,
        }
    }

    /// Encode this content type back to the raw wire byte (`$00..=$08`).
    pub fn to_wire(self) -> u8 {
        match self {
            SyltContentType::Other => 0,
            SyltContentType::Lyrics => 1,
            SyltContentType::TextTranscription => 2,
            SyltContentType::MovementPartName => 3,
            SyltContentType::Events => 4,
            SyltContentType::Chord => 5,
            SyltContentType::Trivia => 6,
            SyltContentType::UrlsToWebpages => 7,
            SyltContentType::UrlsToImages => 8,
        }
    }
}

/// Typed view of the `COMR` "received as" byte (spec v2.3 §4.25 /
/// v2.4 §4.24). The byte sits between the contact URL and the seller
/// name; its nine spec-defined values describe the delivery mode of
/// the purchase. The enum mirrors the spec list verbatim and is
/// surfaced via [`Id3Frame::commercial_delivery`]; see
/// [`CommercialDelivery::from_wire`] for the wire mapping. The
/// mapping is identical between v2.3 and v2.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommercialDelivery {
    /// `$00` per spec — "Other" (catch-all for delivery modes the
    /// enumeration doesn't cover).
    Other,
    /// `$01` per spec — "Standard CD album with other songs".
    StandardCdAlbum,
    /// `$02` per spec — "Compressed audio on CD".
    CompressedAudioOnCd,
    /// `$03` per spec — "File over the Internet".
    FileOverInternet,
    /// `$04` per spec — "Stream over the Internet".
    StreamOverInternet,
    /// `$05` per spec — "As note sheets".
    NoteSheets,
    /// `$06` per spec — "As note sheets in a book with other sheets".
    NoteSheetsInBook,
    /// `$07` per spec — "Music on other media".
    MusicOnOtherMedia,
    /// `$08` per spec — "Non-musical merchandise".
    NonMusicalMerchandise,
}

impl CommercialDelivery {
    /// Decode a raw COMR `received_as` byte. Returns `None` for any
    /// value outside the `$00..=$08` range so a reserved byte surfaces
    /// structurally rather than mapping to a guessed variant.
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(CommercialDelivery::Other),
            1 => Some(CommercialDelivery::StandardCdAlbum),
            2 => Some(CommercialDelivery::CompressedAudioOnCd),
            3 => Some(CommercialDelivery::FileOverInternet),
            4 => Some(CommercialDelivery::StreamOverInternet),
            5 => Some(CommercialDelivery::NoteSheets),
            6 => Some(CommercialDelivery::NoteSheetsInBook),
            7 => Some(CommercialDelivery::MusicOnOtherMedia),
            8 => Some(CommercialDelivery::NonMusicalMerchandise),
            _ => None,
        }
    }

    /// Encode this delivery mode back to the raw wire byte
    /// (`$00..=$08`).
    pub fn to_wire(self) -> u8 {
        match self {
            CommercialDelivery::Other => 0,
            CommercialDelivery::StandardCdAlbum => 1,
            CommercialDelivery::CompressedAudioOnCd => 2,
            CommercialDelivery::FileOverInternet => 3,
            CommercialDelivery::StreamOverInternet => 4,
            CommercialDelivery::NoteSheets => 5,
            CommercialDelivery::NoteSheetsInBook => 6,
            CommercialDelivery::MusicOnOtherMedia => 7,
            CommercialDelivery::NonMusicalMerchandise => 8,
        }
    }
}

/// Typed view of the `RVA2` "type of channel" byte (spec v2.4 §4.11).
/// The byte opens each per-channel record inside an `RVA2` payload and
/// names the channel the volume adjustment applies to. The enum
/// mirrors the spec's nine-value table verbatim and is surfaced via
/// [`Rva2Channel::channel_type_typed`]; see [`Rva2ChannelType::from_wire`]
/// for the wire mapping. Mirrors the contract on [`SyltContentType`]
/// and [`CommercialDelivery`]: `from_wire` / `to_wire` form a
/// bijection over the spec range `$00..=$08` and any reserved byte
/// returns `None` so a non-conforming source surfaces structurally
/// rather than mapping to a guessed variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rva2ChannelType {
    /// `$00` per spec — "Other" (catch-all when none of the named
    /// channels fits).
    Other,
    /// `$01` per spec — "Master volume" (single global adjustment
    /// rather than a per-channel one).
    MasterVolume,
    /// `$02` per spec — "Front right".
    FrontRight,
    /// `$03` per spec — "Front left".
    FrontLeft,
    /// `$04` per spec — "Back right".
    BackRight,
    /// `$05` per spec — "Back left".
    BackLeft,
    /// `$06` per spec — "Front centre".
    FrontCentre,
    /// `$07` per spec — "Back centre".
    BackCentre,
    /// `$08` per spec — "Subwoofer".
    Subwoofer,
}

impl Rva2ChannelType {
    /// Decode a raw RVA2 `type_of_channel` byte. Returns `None` for
    /// any value outside the spec range `$00..=$08` so a reserved
    /// byte surfaces structurally rather than mapping to a guessed
    /// variant.
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Rva2ChannelType::Other),
            1 => Some(Rva2ChannelType::MasterVolume),
            2 => Some(Rva2ChannelType::FrontRight),
            3 => Some(Rva2ChannelType::FrontLeft),
            4 => Some(Rva2ChannelType::BackRight),
            5 => Some(Rva2ChannelType::BackLeft),
            6 => Some(Rva2ChannelType::FrontCentre),
            7 => Some(Rva2ChannelType::BackCentre),
            8 => Some(Rva2ChannelType::Subwoofer),
            _ => None,
        }
    }

    /// Encode this channel type back to the raw wire byte
    /// (`$00..=$08`).
    pub fn to_wire(self) -> u8 {
        match self {
            Rva2ChannelType::Other => 0,
            Rva2ChannelType::MasterVolume => 1,
            Rva2ChannelType::FrontRight => 2,
            Rva2ChannelType::FrontLeft => 3,
            Rva2ChannelType::BackRight => 4,
            Rva2ChannelType::BackLeft => 5,
            Rva2ChannelType::FrontCentre => 6,
            Rva2ChannelType::BackCentre => 7,
            Rva2ChannelType::Subwoofer => 8,
        }
    }
}

impl Rva2Channel {
    /// Typed accessor for the channel-type byte (spec v2.4 §4.11).
    /// Returns `Some(kind)` when the wire byte is one of the
    /// spec-defined `$00..=$08` values (Other, Master volume, Front
    /// right, Front left, Back right, Back left, Front centre, Back
    /// centre, Subwoofer), and `None` for any reserved byte. The raw
    /// `channel_type: u8` field still round-trips losslessly, so a
    /// non-conforming source preserves its byte through write — only
    /// the typed view collapses to `None`. Mirrors the contract on
    /// [`Id3Frame::sylt_content_type`] and
    /// [`Id3Frame::commercial_delivery`].
    pub fn channel_type_typed(&self) -> Option<Rva2ChannelType> {
        Rva2ChannelType::from_wire(self.channel_type)
    }
}

/// Typed view of the `EQU2` "interpolation method" byte (spec v2.4
/// §4.12). The byte sits at the very start of the EQU2 payload, just
/// before the identification string, and names the curve a renderer
/// should draw between two adjacent `(frequency, adjustment)` points.
/// The spec defines exactly two values; the enum mirrors them verbatim
/// and is surfaced via [`Id3Frame::equ2_interpolation`]; see
/// [`Equ2Interpolation::from_wire`] for the wire mapping. Mirrors the
/// contract on [`SyltContentType`], [`CommercialDelivery`], and
/// [`Rva2ChannelType`]: `from_wire` / `to_wire` form a bijection over
/// the spec range `$00..=$01` and any reserved byte returns `None` so a
/// non-conforming source surfaces structurally rather than mapping to a
/// guessed variant. EQU2 is v2.4-only per spec — v2.3 carried the
/// `EQUA` frame instead, which uses an unrelated per-band inc/dec
/// bitfield rather than a curve-level interpolation choice — so the
/// accessor is version-locked to v2.4 by virtue of its source variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Equ2Interpolation {
    /// `$00` per spec — "Band": no interpolation between adjustment
    /// points; a renderer jumps from one adjustment level to the next
    /// in the middle between two adjustment points.
    Band,
    /// `$01` per spec — "Linear": a renderer interpolates linearly
    /// between adjacent adjustment points.
    Linear,
}

impl Equ2Interpolation {
    /// Decode a raw EQU2 `interpolation method` byte. Returns `None`
    /// for any value outside the spec range `$00..=$01` so a reserved
    /// byte surfaces structurally rather than mapping to a guessed
    /// variant.
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Equ2Interpolation::Band),
            1 => Some(Equ2Interpolation::Linear),
            _ => None,
        }
    }

    /// Encode this interpolation method back to the raw wire byte
    /// (`$00..=$01`).
    pub fn to_wire(self) -> u8 {
        match self {
            Equ2Interpolation::Band => 0,
            Equ2Interpolation::Linear => 1,
        }
    }
}

/// Typed view of the `POPM` "rating" byte (spec v2.3 §4.18 / v2.4
/// §4.17). The spec states verbatim: "The rating is 1-255 where 1 is
/// worst and 255 is best. 0 is unknown." This enum surfaces that two-
/// state semantic — a categorical [`PopmRating::Unknown`] for the
/// reserved `$00` and a [`PopmRating::Rated`] carrying the raw
/// `1..=255` magnitude — without losing the round-trip through the raw
/// `u8` field. The underlying [`Id3Frame::Popularimeter::rating`] is
/// unchanged, so the exact on-wire byte still serialises through
/// [`write_tag`].
///
/// The rating byte is identical between v2.3 and v2.4 (the wording is
/// reproduced verbatim in both version docs), so the accessor is
/// version-independent, matching the cross-version posture of
/// [`Id3Frame::etco_event_types`] and [`Id3Frame::timestamp_unit`].
/// No normalisation onto a star scale is performed: the spec defines
/// only the `1`-worst / `255`-best ordering and the `0`-unknown
/// sentinel, and any bucketing into N stars is an out-of-spec
/// convention, so [`PopmRating::Rated`] preserves the raw byte and
/// leaves any scaling to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopmRating {
    /// `$00` per spec — "0 is unknown": no rating has been expressed.
    Unknown,
    /// `$01..=$FF` per spec — a concrete rating where `1` is worst and
    /// `255` is best. The inner byte is the raw on-wire magnitude,
    /// guaranteed non-zero.
    Rated(u8),
}

impl PopmRating {
    /// Decode a raw `POPM` rating byte. `$00` maps to
    /// [`PopmRating::Unknown`] per the spec sentinel; every other byte
    /// (`$01..=$FF`) maps to [`PopmRating::Rated`] carrying that
    /// magnitude. Total over all 256 byte values — there is no reserved
    /// range — so this returns `Self` rather than `Option<Self>`,
    /// unlike the enumerated-variant accessors (`from_wire` on
    /// [`Equ2Interpolation`] etc.) which reject reserved bytes.
    pub fn from_wire(value: u8) -> Self {
        match value {
            0 => PopmRating::Unknown,
            n => PopmRating::Rated(n),
        }
    }

    /// Encode this rating back to the raw wire byte: `$00` for
    /// [`PopmRating::Unknown`], otherwise the carried `1..=255`
    /// magnitude. Forms a bijection with [`PopmRating::from_wire`] over
    /// all 256 byte values.
    pub fn to_wire(self) -> u8 {
        match self {
            PopmRating::Unknown => 0,
            PopmRating::Rated(n) => n,
        }
    }

    /// `true` when a concrete rating has been expressed (`$01..=$FF`),
    /// `false` for the `$00` "unknown" sentinel. Convenience over
    /// matching [`PopmRating::Rated`].
    pub fn is_rated(self) -> bool {
        matches!(self, PopmRating::Rated(_))
    }
}

/// Typed view of the `SYTC` "tempo" byte (spec v2.4 §4.7). Each
/// per-tempo record in a `SYTC` payload opens with a one- or two-byte
/// tempo descriptor: a single byte for tempos `$00..=$FE`, or `$FF`
/// followed by an extension byte whose value is added to `$FF` to give
/// a tempo in the range `2 - 510` BPM. The spec reserves `$00` to
/// describe "a beat-free time period, which is not the same as a
/// music-free time period" and `$01` to indicate "one single
/// beat-stroke followed by a beat-free period"; values `2..=510` are
/// the actual BPM. This enum surfaces those three categorical meanings
/// without losing the round-trip through the raw `u16` field — the
/// underlying [`Id3Frame::SyncedTempo::codes`] is unchanged.
///
/// Wire ranges per spec:
///
/// * `$00` — [`SytcTempo::BeatFree`]: a beat-free time period.
/// * `$01` — [`SytcTempo::SingleStroke`]: one beat-stroke followed by
///   a beat-free period.
/// * `2..=510` — [`SytcTempo::Bpm`]: the BPM verbatim. The on-wire
///   encoding uses a single byte for `2..=254` and the two-byte
///   `$FF $xx` extension form for `255..=510` (`$FF + $xx` summed),
///   but both encode to the same logical BPM and the parser already
///   normalises the extension into a single `u16` in
///   [`Id3Frame::SyncedTempo::codes`]; this enum stays at the logical
///   layer.
/// * `511..=u16::MAX` — outside the spec range and reserved for the
///   parser to surface only when a producer wrote a wider value than
///   the on-wire encoding allows. [`SytcTempo::from_wire`] returns
///   `None` so a non-conforming source surfaces structurally rather
///   than mapping to a guessed variant — matching the contract on
///   [`SyltContentType`], [`CommercialDelivery`], [`Rva2ChannelType`],
///   [`Equ2Interpolation`], [`TimestampUnit`], and
///   [`Restrictions`]. The accessor is surfaced via
///   [`Id3Frame::sytc_tempo_codes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SytcTempo {
    /// `$00` per spec — "beat-free time period, which is not the same
    /// as a music-free time period".
    BeatFree,
    /// `$01` per spec — "one single beat-stroke followed by a
    /// beat-free period".
    SingleStroke,
    /// `2..=510` per spec — the BPM verbatim. The on-wire encoding
    /// uses a single byte for `2..=254` and the `$FF $xx` two-byte
    /// extension form (summed) for `255..=510`; both encode to the
    /// same logical BPM and this enum stays at the logical layer.
    Bpm(u16),
}

impl SytcTempo {
    /// Decode a raw SYTC tempo value (already normalised by the
    /// parser from the one- or two-byte wire form into a single `u16`).
    /// Returns `None` for any value outside the spec range `0..=510`
    /// so a non-conforming source surfaces structurally rather than
    /// mapping to a guessed variant.
    pub fn from_wire(value: u16) -> Option<Self> {
        match value {
            0 => Some(SytcTempo::BeatFree),
            1 => Some(SytcTempo::SingleStroke),
            2..=510 => Some(SytcTempo::Bpm(value)),
            _ => None,
        }
    }

    /// Encode this typed tempo back to the raw value carried in
    /// [`Id3Frame::SyncedTempo::codes`]. The wire-level one-byte vs
    /// `$FF` two-byte split is the writer's responsibility — see
    /// [`write_tag`] — so this returns the logical `u16` only.
    pub fn to_wire(self) -> u16 {
        match self {
            SytcTempo::BeatFree => 0,
            SytcTempo::SingleStroke => 1,
            SytcTempo::Bpm(bpm) => bpm,
        }
    }
}

/// Typed view of one `TCON` "Content type" reference (spec v2.3 §4.2.1
/// `TCON` / v2.4 §4.2.3 `TCON`). The genre frame carries one or several
/// content-type references in a single string. The two version dialects
/// differ in framing but share the underlying vocabulary:
///
/// * v2.3 stores references parenthesised: an ID3v1 numeric genre is
///   `"("` + a number from the appendix-A list + `")"` (e.g. `"(21)"`),
///   optionally followed by a free-text refinement (e.g. `"(4)Eurodisco"`).
///   Several references can sit in one string (`"(51)(39)"`). A literal
///   `"("` opening a free-text refinement is escaped by doubling it
///   (`"((I can figure out any genre)"`). The spec also defines two
///   keyword references — `"(RX)"` Remix and `"(CR)"` Cover.
/// * v2.4 dropped the parentheses: a numeric content type is a bare
///   numeric string and `"RX"` / `"CR"` are bare keyword strings, with
///   multiple references separated by the text-frame NUL list (so each
///   appears as its own entry in [`Id3Frame::Text::values`]).
///
/// This enum collapses both dialects onto the same vocabulary. It is
/// surfaced via [`Id3Frame::content_types`]; see that accessor for how
/// the two framings are parsed into a single `Vec<ContentType>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentType {
    /// A reference to the ID3v1 numeric genre list (appendix A). `index`
    /// is the raw numeric value; `name` is the resolved genre string from
    /// the same Winamp-extended table [`parse_id3v1`] uses, or `None`
    /// when the number falls outside that table (a forward-compatible
    /// numeric reference a future genre list might define).
    Genre {
        /// The numeric genre reference (`"21"` → `21`).
        index: u8,
        /// The resolved genre name, or `None` for an out-of-table index.
        name: Option<&'static str>,
    },
    /// The `RX` keyword reference per spec — "Remix".
    Remix,
    /// The `CR` keyword reference per spec — "Cover".
    Cover,
    /// A free-text content type the spec lets a producer "define their
    /// own": a v2.3 refinement after a parenthesised reference, a v2.3
    /// `((`-escaped custom string, or any v2.4 bare value that is neither
    /// a pure number nor an `RX` / `CR` keyword. The inner string is the
    /// refinement text with any `((` escape already collapsed to a single
    /// leading `(`.
    Custom(String),
}

impl ContentType {
    /// Resolve a numeric genre reference into a [`ContentType::Genre`],
    /// looking the name up in the same Winamp-extended ID3v1 genre table
    /// used by [`parse_id3v1`].
    fn from_genre_index(index: u8) -> ContentType {
        ContentType::Genre {
            index,
            name: id3v1_genre(index),
        }
    }
}

/// Parse a single TCON value string into its content-type references.
///
/// Handles the v2.3 parenthesised grammar (`(21)`, `(RX)`, `(CR)`,
/// `(4)Eurodisco`, the `((` escape) and falls back to the v2.4 bare
/// interpretation (a pure numeric string, the `RX` / `CR` keywords, or
/// free text) when the value does not open with a `(`. References are
/// appended to `out` in left-to-right wire order.
fn parse_tcon_value(value: &str, out: &mut Vec<ContentType>) {
    let bytes = value.as_bytes();
    let mut i = 0;
    // Walk leading parenthesised references per spec v2.3 §4.2.1.
    while i < bytes.len() && bytes[i] == b'(' {
        // "((" escapes a literal '(' that begins a free-text refinement.
        if i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            let refinement = format!("({}", &value[i + 2..]);
            out.push(ContentType::Custom(refinement));
            return;
        }
        // Find the closing ')'. A '(' with no ')' is non-conforming;
        // surface the remainder as free text rather than dropping it.
        let Some(rel_close) = value[i + 1..].find(')') else {
            out.push(ContentType::Custom(value[i..].to_string()));
            return;
        };
        let close = i + 1 + rel_close;
        let inner = &value[i + 1..close];
        push_bare_reference(inner, out);
        i = close + 1;
    }
    // Anything left after the parenthesised references is a free-text
    // refinement (v2.3) or — when there were no parentheses at all — a
    // bare v2.4 value.
    if i < bytes.len() {
        let rest = &value[i..];
        if i == 0 {
            push_bare_reference(rest, out);
        } else {
            out.push(ContentType::Custom(rest.to_string()));
        }
    }
}

/// Interpret a bare (unparenthesised) reference token: a pure numeric
/// string maps to a numeric genre, `RX` / `CR` to the keyword variants,
/// and anything else to free text. Empty tokens are ignored.
fn push_bare_reference(token: &str, out: &mut Vec<ContentType>) {
    if token.is_empty() {
        return;
    }
    match token {
        "RX" => out.push(ContentType::Remix),
        "CR" => out.push(ContentType::Cover),
        _ => {
            if let Ok(index) = token.parse::<u8>() {
                out.push(ContentType::from_genre_index(index));
            } else {
                out.push(ContentType::Custom(token.to_string()));
            }
        }
    }
}

/// Typed view of one `TMED` "Media type" reference (spec v2.3 §4.6.3 /
/// v2.4 §4.2.3). The frame "describes from which media the sound
/// originated" and is "either a text string or a reference to the
/// predefined media types found in the list below."
///
/// The two dialects frame the reference differently and this enum
/// collapses both onto one vocabulary, mirroring [`ContentType`]:
///
/// * v2.3 wraps a predefined reference in `"("` and `")"` and lets it be
///   "optionally followed by a text refinement, e.g. `(MC) with four
///   channels`". A leading `"("` in a free-text refinement is escaped by
///   doubling it (`"(("`) "in the same way as in the `TCO` frame".
///   Predefined `/`-refinements are appended after the top-level code,
///   e.g. `"(CD/A)"` or `"(VID/PAL/VHS)"`.
/// * v2.4 dropped the parentheses: a predefined reference is the bare
///   slash-separated string, e.g. the spec's own example `"VID/PAL/VHS"`.
///
/// Surfaced via [`Id3Frame::media_type`]; see that accessor for how the
/// two framings parse into a single `Vec<MediaType>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaType {
    /// A reference to the spec's predefined media-type list.
    Predefined {
        /// The top-level media-type code (`"CD"`, `"VID"`, `"MC"`, …).
        /// `name` resolves it against the spec's predefined table, or is
        /// `None` for a code the table does not define (a
        /// forward-compatible reference surfaces structurally rather than
        /// being dropped, matching [`ContentType::Genre`]).
        media: String,
        /// The resolved top-level media-type description, or `None` for an
        /// out-of-table code.
        name: Option<&'static str>,
        /// The `/`-separated predefined refinement codes that followed the
        /// top-level code in wire order (`["PAL", "VHS"]` for
        /// `VID/PAL/VHS`). Empty when the reference carried no refinement.
        refinements: Vec<String>,
        /// A v2.3 free-text refinement that followed the closing `")"`
        /// (`" with four channels"` for `(MC) with four channels`), with
        /// any `"(("` escape already collapsed to a single leading `"("`.
        /// Always `None` for a v2.4 bare reference (v2.4 carries no
        /// post-reference text in this frame).
        text: Option<String>,
    },
    /// A free-text media type the producer wrote in place of a predefined
    /// reference: a v2.3 `"(("`-escaped literal-`(` string, or any value
    /// that is not a predefined-reference form. The inner string has any
    /// `"(("` escape already collapsed to a single leading `"("`.
    Custom(String),
}

impl MediaType {
    /// Resolve a top-level media-type code into a [`MediaType::Predefined`]
    /// carrying the spec's description, with the given refinements/text.
    fn predefined(media: &str, refinements: Vec<String>, text: Option<String>) -> MediaType {
        MediaType::Predefined {
            media: media.to_string(),
            name: media_type_name(media),
            refinements,
            text,
        }
    }
}

/// Resolve a `TMED` top-level media-type code to its predefined
/// description (spec v2.3 §4.6.3 / v2.4 §4.2.3 list), or `None` for a
/// code outside the predefined table.
fn media_type_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "DIG" => "Other digital media",
        "ANA" => "Other analogue media",
        "CD" => "CD",
        "LD" => "Laserdisc",
        "TT" => "Turntable records",
        "MD" => "MiniDisc",
        "DAT" => "DAT",
        "DCC" => "DCC",
        "DVD" => "DVD",
        "TV" => "Television",
        "VID" => "Video",
        "RAD" => "Radio",
        "TEL" => "Telephone",
        "MC" => "MC (normal cassette)",
        "REE" => "Reel",
        _ => return None,
    })
}

/// Parse a single TMED value string into its media-type reference.
///
/// Handles the v2.3 parenthesised grammar (`(CD/A)`, `(VID/PAL/VHS)`,
/// `(MC) with four channels`, the `((` escape) and falls back to the
/// v2.4 bare interpretation (`VID/PAL/VHS`) when the value does not open
/// with a `(`. The reference is appended to `out`.
fn parse_tmed_value(value: &str, out: &mut Vec<MediaType>) {
    let bytes = value.as_bytes();
    if bytes.first() == Some(&b'(') {
        // "((" escapes a literal '(' that begins a free-text refinement.
        if bytes.get(1) == Some(&b'(') {
            out.push(MediaType::Custom(format!("({}", &value[2..])));
            return;
        }
        // Find the closing ')'. A '(' with no ')' is non-conforming;
        // surface the remainder as free text rather than dropping it.
        let Some(rel_close) = value[1..].find(')') else {
            out.push(MediaType::Custom(value.to_string()));
            return;
        };
        let close = 1 + rel_close;
        let inner = &value[1..close];
        let text = &value[close + 1..];
        let text = if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
        push_media_reference(inner, text, out);
    } else {
        // v2.4 bare reference (or a plain producer-written text string).
        push_media_reference(value, None, out);
    }
}

/// Interpret a media reference token (`"VID/PAL/VHS"`, `"CD/A"`, `"MC"`)
/// into a [`MediaType::Predefined`]. The first `/`-segment is the
/// top-level media code; the rest are refinement codes. An empty token,
/// or one whose first segment is empty, surfaces as
/// [`MediaType::Custom`] so a non-conforming source is preserved rather
/// than collapsed to an empty reference.
fn push_media_reference(token: &str, text: Option<String>, out: &mut Vec<MediaType>) {
    let mut parts = token.split('/');
    let media = parts.next().unwrap_or("");
    if media.is_empty() {
        // No top-level code — treat the whole thing as free text. Re-glue
        // any text refinement that followed a (degenerate) reference.
        let mut s = token.to_string();
        if let Some(t) = text {
            s.push_str(&t);
        }
        out.push(MediaType::Custom(s));
        return;
    }
    let refinements: Vec<String> = parts.map(str::to_string).collect();
    out.push(MediaType::predefined(media, refinements, text));
}

/// Typed view of the `TFLT` "File type" frame (spec v2.3 §4.2.1 / v2.4
/// §4.2.3). The frame "indicates which type of audio this tag defines"
/// as a predefined type — optionally followed by `/`-separated
/// refinements — "in a similar way to the predefined types in the
/// `TMED` frame, but without parentheses".
///
/// Unlike [`MediaType`], `TFLT` carries no parentheses and no v2.3
/// free-text refinement: the wire form is identical in both versions
/// (v2.4 only adds the `MIME` top-level code), so a single bare
/// grammar covers both dialects.
///
/// Surfaced via [`Id3Frame::file_type`]; see that accessor for how a
/// value parses into a single `Vec<FileType>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileType {
    /// A reference to the spec's predefined file-type list.
    Predefined {
        /// The top-level file-type code (`"MPG"`, `"VQF"`, `"PCM"`,
        /// `"MIME"`). `name` resolves it against the spec's predefined
        /// table, or is `None` for a code the table does not define (a
        /// forward-compatible reference surfaces structurally rather than
        /// being dropped, matching [`MediaType::Predefined`]).
        code: String,
        /// The resolved top-level file-type description, or `None` for an
        /// out-of-table code.
        name: Option<&'static str>,
        /// The `/`-separated refinement codes that followed the top-level
        /// code in wire order (`["3"]` for `MPG/3`). Empty when the
        /// reference carried no refinement. Per spec the refinements are
        /// only defined for `MPG` (`/1`, `/2`, `/3`, `/2.5`, `/AAC`); a
        /// refinement on any other code is preserved verbatim.
        refinements: Vec<String>,
    },
    /// A free-text file type the producer wrote in place of a predefined
    /// reference — any value whose top-level segment is empty (a
    /// non-conforming source preserved rather than collapsed).
    Custom(String),
}

impl FileType {
    /// Resolve a top-level file-type code into a [`FileType::Predefined`]
    /// carrying the spec's description, with the given refinements.
    fn predefined(code: &str, refinements: Vec<String>) -> FileType {
        FileType::Predefined {
            code: code.to_string(),
            name: file_type_name(code),
            refinements,
        }
    }
}

/// Resolve a `TFLT` top-level file-type code to its predefined
/// description (spec v2.3 §4.2.1 / v2.4 §4.2.3 list), or `None` for a
/// code outside the predefined table. `MIME` is v2.4-only per spec but
/// the byte-form is version-independent so the table resolves it under
/// either envelope.
fn file_type_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "MIME" => "MIME type follows",
        "MPG" => "MPEG Audio",
        "VQF" => "Transform-domain Weighted Interleave Vector Quantization",
        "PCM" => "Pulse Code Modulated audio",
        _ => return None,
    })
}

/// Parse a single TFLT value string into its file-type reference. The
/// first `/`-segment is the top-level file-type code; the rest are
/// refinement codes. An empty value, or one whose first segment is
/// empty, surfaces as [`FileType::Custom`] so a non-conforming source
/// is preserved rather than collapsed to an empty reference.
fn parse_tflt_value(value: &str, out: &mut Vec<FileType>) {
    let mut parts = value.split('/');
    let code = parts.next().unwrap_or("");
    if code.is_empty() {
        out.push(FileType::Custom(value.to_string()));
        return;
    }
    let refinements: Vec<String> = parts.map(str::to_string).collect();
    out.push(FileType::predefined(code, refinements));
}

/// The accidental on a [`MusicalKey::Key`] tonic (spec v2.3 §4.2.1 /
/// v2.4 §4.2.3 `TKEY`). The spec defines exactly two "halfkeys":
/// `"b"` (flat) and `"#"` (sharp). A natural key (no accidental) is
/// the absence of either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAccidental {
    /// `"b"` per spec — the tonic is flattened a halfkey.
    Flat,
    /// `"#"` per spec — the tonic is sharpened a halfkey.
    Sharp,
}

/// Typed view of the `TKEY` "Initial key" frame (spec v2.3 §4.2.1 /
/// v2.4 §4.2.3). The frame "contains the musical key in which the sound
/// starts", "represented as a string with a maximum length of three
/// characters". Per spec the ground keys are `"A"`..`"G"`, the halfkeys
/// are `"b"` and `"#"`, minor is `"m"`, and "Off key is represented with
/// an `"o"` only" — e.g. `"Dbm"` is D-flat minor and `"o"` is off-key.
///
/// The byte-form is identical across v2.2 (`TKE`), v2.3, and v2.4 — the
/// grammar paragraph is reproduced verbatim in all three version docs —
/// so the accessor is version-independent, matching the cross-version
/// posture of [`Id3Frame::content_types`] and [`Id3Frame::media_type`].
/// Surfaced via [`Id3Frame::initial_key`].
///
/// Spec-conforming values decode to a structured variant; anything that
/// does not match the spec grammar (a tonic outside `A`..`G`, an unknown
/// trailing character, or a length over the spec's three-character
/// maximum) surfaces as [`MusicalKey::Custom`] so a forward-compatible or
/// non-conforming source is preserved rather than dropped — matching the
/// posture of [`FileType::Custom`]. The raw [`Id3Frame::Text::values`] is
/// unchanged and round-trips losslessly through [`write_tag`], so the
/// typed view never costs a caller the ability to preserve the exact
/// on-wire string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MusicalKey {
    /// The spec's `"o"` off-key sentinel — "Off key is represented with
    /// an `"o"` only". Carries no tonic.
    OffKey,
    /// A structured key: a ground tonic `A`..`G`, an optional flat / sharp
    /// accidental, and a minor flag.
    Key {
        /// The ground key, an uppercase `'A'`..`'G'` per spec.
        tonic: char,
        /// The `"b"` / `"#"` halfkey, or `None` for a natural tonic.
        accidental: Option<KeyAccidental>,
        /// `true` when the trailing `"m"` minor marker is present.
        minor: bool,
    },
    /// A value that does not match the spec grammar — preserved verbatim
    /// so a non-conforming or forward-compatible source surfaces
    /// structurally rather than being dropped.
    Custom(String),
}

/// Parse a single `TKEY` value string into a [`MusicalKey`] per the spec
/// grammar (ground key `A`..`G`, optional `b` / `#` halfkey, optional
/// `m` minor; the standalone `o` off-key sentinel). Any value that
/// violates the grammar — including a value longer than the spec's
/// three-character maximum — surfaces as [`MusicalKey::Custom`].
fn parse_tkey_value(value: &str) -> MusicalKey {
    if value == "o" {
        return MusicalKey::OffKey;
    }
    // The spec caps the field at three characters; a longer value cannot
    // be a conforming key, so preserve it verbatim.
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() || chars.len() > 3 {
        return MusicalKey::Custom(value.to_string());
    }
    let mut iter = chars.iter().copied();
    let tonic = iter.next().unwrap();
    if !('A'..='G').contains(&tonic) {
        return MusicalKey::Custom(value.to_string());
    }
    let mut accidental = None;
    let mut minor = false;
    let mut pending = iter.next();
    if let Some(c) = pending {
        accidental = match c {
            'b' => Some(KeyAccidental::Flat),
            '#' => Some(KeyAccidental::Sharp),
            _ => None,
        };
        if accidental.is_some() {
            pending = iter.next();
        }
    }
    if let Some(c) = pending {
        if c == 'm' {
            minor = true;
            pending = iter.next();
        }
    }
    // Any leftover character means the value does not match the grammar.
    if pending.is_some() {
        return MusicalKey::Custom(value.to_string());
    }
    MusicalKey::Key {
        tonic,
        accidental,
        minor,
    }
}

/// Typed view of the `TRCK` "Track number/Position in set" and `TPOS`
/// "Part of a set" frames (spec v2.3 §4.2.1 / v2.4 §4.2.1). Both frames
/// share an identical grammar: "a numeric string … This MAY be extended
/// with a `"/"` character and a numeric string containing the total
/// number" — e.g. `"4/9"` for `TRCK` (track 4 of 9) and `"1/2"` for
/// `TPOS` (part 1 of 2).
///
/// Surfaced via [`Id3Frame::track_number`] (for `TRCK`) and
/// [`Id3Frame::part_of_set`] (for `TPOS`). The wire grammar is identical
/// across v2.2 (`TRK` / `TPA`), v2.3, and v2.4, so the accessors are
/// version-independent, matching the cross-version posture of
/// [`Id3Frame::content_types`] and [`Id3Frame::initial_key`].
///
/// A value that matches the grammar — a numeric `number`, optionally
/// followed by `/` and a numeric `total` — decodes to
/// [`TrackPosition::Numbered`]. Anything else (a non-numeric segment, a
/// leading/empty number, more than one `/`, or a value that overflows a
/// `u32`) surfaces as [`TrackPosition::Malformed`] with the raw string
/// preserved, so a forward-compatible or non-conforming source surfaces
/// structurally rather than being dropped — matching the posture of
/// [`MusicalKey::Custom`] and [`FileType::Custom`]. The raw
/// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
/// through [`write_tag`], so the typed view never costs a caller the
/// ability to preserve the exact on-wire string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackPosition {
    /// A spec-conforming numeric position with an optional total.
    Numbered {
        /// The order number (`4` for `"4/9"`), the leading numeric
        /// string per spec.
        number: u32,
        /// The total after the `"/"` (`Some(9)` for `"4/9"`), or `None`
        /// when the value carried only the number.
        total: Option<u32>,
    },
    /// A value that does not match the spec grammar — preserved verbatim
    /// so a non-conforming or forward-compatible source surfaces
    /// structurally rather than being dropped.
    Malformed(String),
}

/// Parse a single `TRCK` / `TPOS` value string into a [`TrackPosition`]
/// per the spec grammar (a numeric string optionally extended with a
/// `"/"` and a second numeric string). Any value that violates the
/// grammar — a non-numeric segment, an empty number, more than one `"/"`,
/// or a number that overflows a `u32` — surfaces as
/// [`TrackPosition::Malformed`].
fn parse_track_position(value: &str) -> TrackPosition {
    let mut parts = value.split('/');
    let number_str = parts.next().unwrap_or("");
    let total_str = parts.next();
    // The grammar allows at most one `/`; a second separator means the
    // value is not a conforming track/total pair.
    if parts.next().is_some() {
        return TrackPosition::Malformed(value.to_string());
    }
    let number = match parse_decimal_u32(number_str) {
        Some(n) => n,
        None => return TrackPosition::Malformed(value.to_string()),
    };
    let total = match total_str {
        None => None,
        Some(s) => match parse_decimal_u32(s) {
            Some(t) => Some(t),
            None => return TrackPosition::Malformed(value.to_string()),
        },
    };
    TrackPosition::Numbered { number, total }
}

/// Parse a non-empty ASCII-decimal string into a `u32`, returning `None`
/// for an empty string, a non-digit character, or an overflow. Used by
/// [`parse_track_position`] to enforce the spec's "numeric string"
/// requirement without accepting `+`/`-` signs or whitespace that a
/// permissive integer parser would tolerate.
fn parse_decimal_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok()
}

/// Typed view of the `TSRC` "ISRC" frame (spec v2.3 §4.2.1 / v2.4
/// §4.2.1). The frame "should contain the International Standard
/// Recording Code [ISRC] (12 characters)". The spec body fixes only the
/// length — twelve characters — and cites `[ISRC]` (ISO 3901) for the
/// code's meaning without reproducing its internal field layout, so this
/// view validates exactly the constraint the ID3 spec itself states: a
/// twelve-character ASCII value.
///
/// Surfaced via [`Id3Frame::isrc`]. The wire form is a plain text-frame
/// value, identical across v2.2 (`TRC`), v2.3, and v2.4, so the accessor
/// is version-independent — matching the cross-version posture of
/// [`Id3Frame::track_number`] and [`Id3Frame::initial_key`].
///
/// A value that is exactly twelve ASCII characters decodes to
/// [`Isrc::Code`]; anything else — a value of the wrong length, an empty
/// value, or one carrying a non-ASCII byte — surfaces as
/// [`Isrc::Malformed`] with the raw string preserved, so a
/// forward-compatible or non-conforming source surfaces structurally
/// rather than being dropped (matching [`TrackPosition::Malformed`] and
/// [`MusicalKey::Custom`]). The raw [`Id3Frame::Text::values`] is
/// unchanged and round-trips losslessly through [`write_tag`], so the
/// typed view never costs a caller the ability to preserve the exact
/// on-wire string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Isrc {
    /// A spec-conforming twelve-character ASCII ISRC value.
    Code(String),
    /// A value that does not match the spec's twelve-ASCII-character
    /// constraint — preserved verbatim so a non-conforming or
    /// forward-compatible source surfaces structurally rather than being
    /// dropped.
    Malformed(String),
}

/// Parse a single `TSRC` value string into an [`Isrc`] per the spec's
/// "12 characters" constraint. A value of exactly twelve ASCII
/// characters is [`Isrc::Code`]; anything else (wrong length, empty, or a
/// non-ASCII byte) surfaces as [`Isrc::Malformed`]. "Twelve characters"
/// is counted as twelve `char`s; since a conforming value is ASCII, the
/// `is_ascii` guard makes the `char` count equal the byte count.
fn parse_tsrc_value(value: &str) -> Isrc {
    if value.is_ascii() && value.len() == 12 {
        Isrc::Code(value.to_string())
    } else {
        Isrc::Malformed(value.to_string())
    }
}

/// Typed view of a numeric-string duration in milliseconds, carried by
/// the `TLEN` "Length" frame (spec v2.3 §4.2.1 / v2.4 §4.2.1) and the
/// `TDLY` "Playlist delay" frame (same sections). The spec defines
/// `TLEN` as "the length of the audio file in milliseconds, represented
/// as a numeric string" and `TDLY` as "the numbers of milliseconds of
/// silence that should be inserted before this audio … represented as a
/// numeric string".
///
/// Surfaced via [`Id3Frame::length_ms`] and [`Id3Frame::playlist_delay_ms`].
/// The wire form is a plain text-frame value, identical across v2.2
/// (`TLE` / `TDY`), v2.3, and v2.4, so the accessors are
/// version-independent — matching the cross-version posture of
/// [`Id3Frame::track_number`] and [`Id3Frame::isrc`].
///
/// A value that is a non-empty ASCII-decimal string decodes to
/// [`DurationMs::Millis`]; anything else — an empty value, a sign, a
/// decimal point, embedded whitespace, a non-digit byte, or a value that
/// overflows a `u64` — surfaces as [`DurationMs::Malformed`] with the raw
/// string preserved, so a forward-compatible or non-conforming source
/// surfaces structurally rather than being dropped (matching
/// [`TrackPosition::Malformed`] and [`Isrc::Malformed`]). The raw
/// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
/// through [`write_tag`], so the typed view never costs a caller the
/// ability to preserve the exact on-wire string. A `u64` holds any
/// physically meaningful duration (`u64::MAX` ms is ~584 million years).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurationMs {
    /// A spec-conforming numeric-string duration in milliseconds.
    Millis(u64),
    /// A value that does not match the spec's "numeric string"
    /// constraint — preserved verbatim so a non-conforming or
    /// forward-compatible source surfaces structurally rather than being
    /// dropped.
    Malformed(String),
}

/// Parse a single `TLEN` / `TDLY` value string into a [`DurationMs`] per
/// the spec's "numeric string" requirement. A non-empty ASCII-decimal
/// string is [`DurationMs::Millis`]; anything else (empty, sign, decimal
/// point, whitespace, non-digit byte, or `u64` overflow) surfaces as
/// [`DurationMs::Malformed`]. The decimal guard rejects `+`/`-` signs and
/// surrounding whitespace that a permissive integer parser would
/// tolerate, matching [`parse_decimal_u32`].
fn parse_duration_ms(value: &str) -> DurationMs {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return DurationMs::Malformed(value.to_string());
    }
    match value.parse::<u64>() {
        Ok(n) => DurationMs::Millis(n),
        Err(_) => DurationMs::Malformed(value.to_string()),
    }
}

/// Typed view of the `TBPM` "BPM (beats per minute)" frame (spec v2.3
/// §4.2.1 / v2.4 §4.2.1). The spec defines the frame as "the number of
/// beats per minute in the main part of the audio. The BPM is an integer
/// and represented as a numerical string."
///
/// Surfaced via [`Id3Frame::bpm`]. The wire form is a plain text-frame
/// value, identical across v2.2 (`TBP`), v2.3, and v2.4, so the accessor
/// is version-independent — matching the cross-version posture of
/// [`Id3Frame::length_ms`].
///
/// A non-empty ASCII-decimal string decodes to [`Bpm::Beats`]; anything
/// else — an empty value, a sign, a decimal point, embedded whitespace, a
/// non-digit byte, or a value that overflows a `u32` — surfaces as
/// [`Bpm::Malformed`] with the raw string preserved, so a
/// forward-compatible or non-conforming source surfaces structurally
/// rather than being dropped. The spec mandates an integer ("the BPM is
/// an integer"), so a fractional value such as `"128.5"` is *not*
/// conforming and surfaces as [`Bpm::Malformed`]. The raw
/// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
/// through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bpm {
    /// A spec-conforming integer beats-per-minute value.
    Beats(u32),
    /// A value that does not match the spec's "integer … numerical
    /// string" constraint — preserved verbatim.
    Malformed(String),
}

/// Parse a single `TBPM` value string into a [`Bpm`] per the spec's
/// "integer … numerical string" requirement. Reuses [`parse_decimal_u32`]
/// so the decimal guard is identical to the track/position grammar — a
/// sign, decimal point, whitespace, or non-digit byte yields
/// [`Bpm::Malformed`].
fn parse_bpm_value(value: &str) -> Bpm {
    match parse_decimal_u32(value) {
        Some(n) => Bpm::Beats(n),
        None => Bpm::Malformed(value.to_string()),
    }
}

/// Typed view of the `ETCO` "type of event" byte (spec v2.3 §4.6 /
/// v2.4 §4.5). The byte sits at the start of each per-event record in
/// an `ETCO` payload — one event-type byte followed by a 32-bit
/// big-endian timestamp — and names the audio milestone the timestamp
/// marks (intro start, verse end, audio file ends, …). The enum mirrors
/// the spec's value table verbatim and is surfaced via
/// [`Id3Frame::etco_event_types`]; see [`EtcoEventType::from_wire`]
/// for the wire mapping. The mapping is identical between v2.3 and
/// v2.4 — the event-type table is reproduced bit-for-bit in both
/// version docs — so the accessor is version-independent.
///
/// Wire ranges per spec:
///
/// * `$00..=$16` — 23 spec-named events from "padding (has no
///   meaning)" through "profanity end".
/// * `$17..=$DF` — reserved for future use; surfaces as `None`.
/// * `$E0..=$EF` — "not predefined synch 0-F", carried as
///   [`EtcoEventType::NotPredefinedSync`] with the low nibble of the
///   wire byte (`0..=15`) so a caller can route on the specific user
///   slot without losing the byte.
/// * `$F0..=$FC` — reserved for future use; surfaces as `None`.
/// * `$FD` — "audio end (start of silence)".
/// * `$FE` — "audio file ends".
/// * `$FF` — "one more byte of events follows" (continuation marker:
///   the spec notes "all the following bytes with the value `$FF` have
///   the same function"). A `$FF` byte surfaces as
///   [`EtcoEventType::Continuation`] — its meaning is documented in
///   the spec, distinct from a reserved byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtcoEventType {
    /// `$00` per spec — "padding (has no meaning)".
    Padding,
    /// `$01` per spec — "end of initial silence".
    EndOfInitialSilence,
    /// `$02` per spec — "intro start".
    IntroStart,
    /// `$03` per spec — "main part start".
    MainPartStart,
    /// `$04` per spec — "outro start".
    OutroStart,
    /// `$05` per spec — "outro end".
    OutroEnd,
    /// `$06` per spec — "verse start".
    VerseStart,
    /// `$07` per spec — "refrain start".
    RefrainStart,
    /// `$08` per spec — "interlude start".
    InterludeStart,
    /// `$09` per spec — "theme start".
    ThemeStart,
    /// `$0A` per spec — "variation start".
    VariationStart,
    /// `$0B` per spec — "key change".
    KeyChange,
    /// `$0C` per spec — "time change".
    TimeChange,
    /// `$0D` per spec — "momentary unwanted noise (Snap, Crackle &
    /// Pop)".
    MomentaryUnwantedNoise,
    /// `$0E` per spec — "sustained noise".
    SustainedNoise,
    /// `$0F` per spec — "sustained noise end".
    SustainedNoiseEnd,
    /// `$10` per spec — "intro end".
    IntroEnd,
    /// `$11` per spec — "main part end".
    MainPartEnd,
    /// `$12` per spec — "verse end".
    VerseEnd,
    /// `$13` per spec — "refrain end".
    RefrainEnd,
    /// `$14` per spec — "theme end".
    ThemeEnd,
    /// `$15` per spec — "profanity".
    Profanity,
    /// `$16` per spec — "profanity end".
    ProfanityEnd,
    /// `$E0..=$EF` per spec — "not predefined synch 0-F", a
    /// user-defined synchronisation event whose slot index is the low
    /// nibble of the wire byte (`0..=15`). The spec example: "you
    /// might want to synchronise your music to something, like setting
    /// off an explosion on-stage, activating a screensaver etc.". The
    /// nibble is preserved here so a caller can route on the specific
    /// user slot without re-decoding the raw `u8`.
    NotPredefinedSync(u8),
    /// `$FD` per spec — "audio end (start of silence)".
    AudioEnd,
    /// `$FE` per spec — "audio file ends".
    AudioFileEnds,
    /// `$FF` per spec — "one more byte of events follows" (the
    /// continuation marker; the spec adds "all the following bytes
    /// with the value `$FF` have the same function"). Surfaces as a
    /// dedicated variant rather than `None` because the byte has a
    /// documented meaning even though it does not itself name an
    /// audio milestone.
    Continuation,
}

impl EtcoEventType {
    /// Decode a raw ETCO `type of event` byte. Returns `None` for any
    /// value in the spec's reserved ranges (`$17..=$DF`, `$F0..=$FC`)
    /// so a non-conforming or future byte surfaces structurally rather
    /// than mapping to a guessed variant. User-defined synchronisation
    /// bytes (`$E0..=$EF`) decode to [`EtcoEventType::NotPredefinedSync`]
    /// carrying the low nibble as the slot index (`0..=15`), and the
    /// continuation marker `$FF` decodes to
    /// [`EtcoEventType::Continuation`].
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(EtcoEventType::Padding),
            0x01 => Some(EtcoEventType::EndOfInitialSilence),
            0x02 => Some(EtcoEventType::IntroStart),
            0x03 => Some(EtcoEventType::MainPartStart),
            0x04 => Some(EtcoEventType::OutroStart),
            0x05 => Some(EtcoEventType::OutroEnd),
            0x06 => Some(EtcoEventType::VerseStart),
            0x07 => Some(EtcoEventType::RefrainStart),
            0x08 => Some(EtcoEventType::InterludeStart),
            0x09 => Some(EtcoEventType::ThemeStart),
            0x0A => Some(EtcoEventType::VariationStart),
            0x0B => Some(EtcoEventType::KeyChange),
            0x0C => Some(EtcoEventType::TimeChange),
            0x0D => Some(EtcoEventType::MomentaryUnwantedNoise),
            0x0E => Some(EtcoEventType::SustainedNoise),
            0x0F => Some(EtcoEventType::SustainedNoiseEnd),
            0x10 => Some(EtcoEventType::IntroEnd),
            0x11 => Some(EtcoEventType::MainPartEnd),
            0x12 => Some(EtcoEventType::VerseEnd),
            0x13 => Some(EtcoEventType::RefrainEnd),
            0x14 => Some(EtcoEventType::ThemeEnd),
            0x15 => Some(EtcoEventType::Profanity),
            0x16 => Some(EtcoEventType::ProfanityEnd),
            0xE0..=0xEF => Some(EtcoEventType::NotPredefinedSync(value & 0x0F)),
            0xFD => Some(EtcoEventType::AudioEnd),
            0xFE => Some(EtcoEventType::AudioFileEnds),
            0xFF => Some(EtcoEventType::Continuation),
            _ => None,
        }
    }

    /// Encode this event type back to the raw wire byte. Spec-named
    /// events serialise to their `$00..=$16` byte; the user-defined
    /// synchronisation slot serialises to `$E0 | (slot & 0x0F)` (the
    /// `slot` value is masked to its low nibble so an out-of-range
    /// slot wraps to the spec's `0..=15` rather than colliding with
    /// the surrounding spec-named events). `AudioEnd` /
    /// `AudioFileEnds` / `Continuation` round-trip to `$FD` / `$FE` /
    /// `$FF` respectively.
    pub fn to_wire(self) -> u8 {
        match self {
            EtcoEventType::Padding => 0x00,
            EtcoEventType::EndOfInitialSilence => 0x01,
            EtcoEventType::IntroStart => 0x02,
            EtcoEventType::MainPartStart => 0x03,
            EtcoEventType::OutroStart => 0x04,
            EtcoEventType::OutroEnd => 0x05,
            EtcoEventType::VerseStart => 0x06,
            EtcoEventType::RefrainStart => 0x07,
            EtcoEventType::InterludeStart => 0x08,
            EtcoEventType::ThemeStart => 0x09,
            EtcoEventType::VariationStart => 0x0A,
            EtcoEventType::KeyChange => 0x0B,
            EtcoEventType::TimeChange => 0x0C,
            EtcoEventType::MomentaryUnwantedNoise => 0x0D,
            EtcoEventType::SustainedNoise => 0x0E,
            EtcoEventType::SustainedNoiseEnd => 0x0F,
            EtcoEventType::IntroEnd => 0x10,
            EtcoEventType::MainPartEnd => 0x11,
            EtcoEventType::VerseEnd => 0x12,
            EtcoEventType::RefrainEnd => 0x13,
            EtcoEventType::ThemeEnd => 0x14,
            EtcoEventType::Profanity => 0x15,
            EtcoEventType::ProfanityEnd => 0x16,
            EtcoEventType::NotPredefinedSync(slot) => 0xE0 | (slot & 0x0F),
            EtcoEventType::AudioEnd => 0xFD,
            EtcoEventType::AudioFileEnds => 0xFE,
            EtcoEventType::Continuation => 0xFF,
        }
    }
}

/// Typed view of a single price element carried by the `OWNE` "price
/// paid" field (spec v2.3 §4.24 / v2.4 §4.23) and the `COMR` "price
/// string" field (spec v2.3 §4.25 / v2.4 §4.24).
///
/// Spec wording (`OWNE`): "The first three characters of this field
/// contains the currency used for the transaction, encoded according
/// to ISO-4217 alphabetic currency code. Concatenated to this is the
/// actual price paid, as a numerical string using \".\" as the decimal
/// separator." `COMR`'s "price string" reuses the same per-element
/// grammar ("one three character currency code … followed by a
/// numerical value where \".\" is used as decimal separator"), and a
/// `COMR` field may concatenate several such elements separated by
/// `/` (with at most one element per currency).
///
/// A well-formed element is surfaced as [`Price::Element`] with the
/// three-letter currency split from the trailing amount; anything too
/// short to carry a three-character currency code, or whose currency
/// bytes are not three ASCII letters, surfaces as [`Price::Malformed`]
/// with the raw bytes preserved verbatim — matching the
/// forward-compatible, non-destructive posture of [`Language`] and the
/// other typed views. The accessors leave the underlying `price`
/// strings untouched so the exact on-wire bytes still round-trip
/// through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Price {
    /// A spec-conforming element: a three-letter ISO-4217 alphabetic
    /// currency code (normalised to upper case for comparison; the
    /// spec gives ISO-4217 codes in upper case) plus the trailing
    /// numerical amount string exactly as it appeared on the wire
    /// (decimal separator `.` preserved, not parsed into a number so
    /// no precision is lost and a malformed-but-present amount is kept).
    Element {
        /// The three-letter currency code, upper-cased.
        currency: [u8; 3],
        /// The numerical amount string, verbatim after the currency.
        amount: String,
    },
    /// A non-conforming element: fewer than three leading bytes, or a
    /// leading three bytes that are not all ASCII letters. The raw
    /// element string is preserved so the caller can still inspect it.
    Malformed(String),
}

impl Price {
    /// Decode a single price element (no `/` separators) into the typed
    /// view. The leading three ASCII-letter bytes are the ISO-4217
    /// currency; the remainder is the amount string. An element that is
    /// too short or whose currency bytes are not all ASCII letters maps
    /// to [`Price::Malformed`] with the raw string preserved.
    pub fn from_element(element: &str) -> Price {
        let bytes = element.as_bytes();
        if bytes.len() >= 3 && bytes[..3].iter().all(|b| b.is_ascii_alphabetic()) {
            let currency = [
                bytes[0].to_ascii_uppercase(),
                bytes[1].to_ascii_uppercase(),
                bytes[2].to_ascii_uppercase(),
            ];
            Price::Element {
                currency,
                amount: element[3..].to_string(),
            }
        } else {
            Price::Malformed(element.to_string())
        }
    }

    /// The three-letter currency code as a string slice when this is a
    /// well-formed [`Price::Element`]; `None` for [`Price::Malformed`].
    /// Always valid UTF-8 because an `Element` currency is by
    /// construction three ASCII letters.
    pub fn currency(&self) -> Option<&str> {
        match self {
            Price::Element { currency, .. } => std::str::from_utf8(currency).ok(),
            Price::Malformed(_) => None,
        }
    }

    /// The numerical amount string for a well-formed [`Price::Element`];
    /// `None` for [`Price::Malformed`]. Returned verbatim — not parsed
    /// into a floating-point number — so no precision is lost.
    pub fn amount(&self) -> Option<&str> {
        match self {
            Price::Element { amount, .. } => Some(amount),
            Price::Malformed(_) => None,
        }
    }
}

/// Typed view of an ID3v2 `YYYYMMDD` date string.
///
/// Two structural frames carry an 8-character date string in this exact
/// format: the `OWNE` "Date of purch." field (spec v2.3 §4.24 / v2.4
/// §4.23 — "an 8 character date string (YYYYMMDD)") and the `COMR`
/// "Valid until" field (spec v2.3 §4.25 / v2.4 §4.24 — "an 8 character
/// date string in the format YYYYMMDD, describing for how long the price
/// is valid"). The same `YYYYMMDD` shape is the leading component of the
/// v2.3-only timestamp text frames (`TDAT` carries `DDMM`, not this
/// field) but those are surfaced as plain text; this typed view is for
/// the two structural frames whose date field is spec-fixed at eight
/// characters.
///
/// A well-formed value — exactly eight ASCII digits — surfaces as
/// [`Id3Date::Ymd`] with the year / month / day split out as numbers
/// (`"20240615"` → `year: 2024, month: 6, day: 15`). The split is purely
/// positional per the spec grammar: the parser does **not** range-check
/// the month or day (a `"20241300"` source surfaces `month: 13` rather
/// than being rejected) because the spec defines the field as a fixed
/// `YYYYMMDD` digit string with no validity constraint, and forcing
/// calendar validity here would drop a forward-compatible-but-odd
/// source. Anything that is not exactly eight ASCII digits — a short
/// or long string, an empty field (the spec's absent form), or a value
/// with a non-digit byte — surfaces as [`Id3Date::Malformed`] with the
/// raw string preserved verbatim.
///
/// The raw date `String` on the frame is left untouched, so the exact
/// on-wire bytes still round-trip through [`write_tag`]; this mirrors the
/// forward-compatible, non-destructive posture of [`Price`] and
/// [`TrackPosition`]. The wire grammar is reproduced verbatim across
/// v2.3 and v2.4, so the accessors are version-independent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id3Date {
    /// A spec-shaped `YYYYMMDD` value: exactly eight ASCII digits, split
    /// positionally. The fields are not calendar-validated — `month` and
    /// `day` carry whatever the eight digits encode so a non-conforming
    /// source is preserved structurally rather than dropped.
    Ymd {
        /// Four-digit year (`0000..=9999`).
        year: u16,
        /// Two-digit month component (`00..=99`, not range-checked).
        month: u8,
        /// Two-digit day component (`00..=99`, not range-checked).
        day: u8,
    },
    /// A value that is not exactly eight ASCII digits — too short, too
    /// long, empty, or containing a non-digit byte. The raw string is
    /// preserved so the caller can still inspect it.
    Malformed(String),
}

impl Id3Date {
    /// Decode an 8-character `YYYYMMDD` date string into the typed view.
    /// A value of exactly eight ASCII digits is split positionally into
    /// [`Id3Date::Ymd`]; anything else maps to [`Id3Date::Malformed`]
    /// with the raw string preserved.
    pub fn from_field(date: &str) -> Id3Date {
        let bytes = date.as_bytes();
        if bytes.len() == 8 && bytes.iter().all(|b| b.is_ascii_digit()) {
            let d = |i: usize| (bytes[i] - b'0') as u16;
            let year = d(0) * 1000 + d(1) * 100 + d(2) * 10 + d(3);
            let month = (d(4) * 10 + d(5)) as u8;
            let day = (d(6) * 10 + d(7)) as u8;
            Id3Date::Ymd { year, month, day }
        } else {
            Id3Date::Malformed(date.to_string())
        }
    }

    /// The year for a well-formed [`Id3Date::Ymd`]; `None` for
    /// [`Id3Date::Malformed`].
    pub fn year(&self) -> Option<u16> {
        match self {
            Id3Date::Ymd { year, .. } => Some(*year),
            Id3Date::Malformed(_) => None,
        }
    }

    /// The month component for a well-formed [`Id3Date::Ymd`]; `None` for
    /// [`Id3Date::Malformed`]. Not calendar-validated — may be `00` or
    /// `>12` if the source carried such digits.
    pub fn month(&self) -> Option<u8> {
        match self {
            Id3Date::Ymd { month, .. } => Some(*month),
            Id3Date::Malformed(_) => None,
        }
    }

    /// The day component for a well-formed [`Id3Date::Ymd`]; `None` for
    /// [`Id3Date::Malformed`]. Not calendar-validated — may be `00` or
    /// `>31` if the source carried such digits.
    pub fn day(&self) -> Option<u8> {
        match self {
            Id3Date::Ymd { day, .. } => Some(*day),
            Id3Date::Malformed(_) => None,
        }
    }
}

/// Typed view of the v2.3-only `TYER` "Year" frame (spec v2.3 §4.2.1).
///
/// The spec defines `TYER` as "a numeric string with a year of the
/// recording. This frame is always four characters long (until the year
/// 10000)." v2.4 dropped this frame, folding the year into the `TDRC`
/// timestamp (see [`Id3Timestamp`]); so this view is **v2.3-only** by
/// virtue of its source frame id.
///
/// Surfaced via [`Id3Frame::year`]. A value of exactly four ASCII digits
/// decodes to [`Id3Year::Year`]; anything else — a value of the wrong
/// length, an empty field (the spec's absent form), or one carrying a
/// non-digit byte — surfaces as [`Id3Year::Malformed`] with the raw
/// string preserved, so a forward-compatible or non-conforming source
/// surfaces structurally rather than being dropped (matching
/// [`Id3Date::Malformed`]). The raw [`Id3Frame::Text::values`] is
/// unchanged and round-trips losslessly through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id3Year {
    /// A spec-shaped four-digit year (`0000..=9999`).
    Year(u16),
    /// A value that is not exactly four ASCII digits — preserved verbatim.
    Malformed(String),
}

impl Id3Year {
    /// Decode a `TYER` value string into the typed view. Exactly four
    /// ASCII digits is [`Id3Year::Year`]; anything else is
    /// [`Id3Year::Malformed`] with the raw string preserved.
    fn from_field(value: &str) -> Id3Year {
        let bytes = value.as_bytes();
        if bytes.len() == 4 && bytes.iter().all(|b| b.is_ascii_digit()) {
            let d = |i: usize| (bytes[i] - b'0') as u16;
            Id3Year::Year(d(0) * 1000 + d(1) * 100 + d(2) * 10 + d(3))
        } else {
            Id3Year::Malformed(value.to_string())
        }
    }
}

/// Typed view of the v2.3-only `TDAT` "Date" frame (spec v2.3 §4.2.1).
///
/// The spec defines `TDAT` as "a numeric string in the DDMM format
/// containing the date for the recording. This field is always four
/// characters long." v2.4 dropped this frame, folding the date into the
/// `TDRC` timestamp; so this view is **v2.3-only** by virtue of its
/// source frame id. Note the field order is **day then month** (`DDMM`,
/// e.g. `"1506"` = 15 June), distinct from the `YYYYMMDD` [`Id3Date`].
///
/// Surfaced via [`Id3Frame::date_ddmm`]. A value of exactly four ASCII
/// digits decodes to [`DayMonth::DayMonth`], split positionally and
/// **not** calendar-validated (`"3199"` surfaces `day: 31, month: 99`)
/// per the same forward-compatible posture as [`Id3Date::Ymd`]. Anything
/// else surfaces as [`DayMonth::Malformed`] with the raw string
/// preserved. The raw [`Id3Frame::Text::values`] is unchanged and
/// round-trips losslessly through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DayMonth {
    /// A spec-shaped `DDMM` value, split positionally and not
    /// calendar-validated.
    DayMonth {
        /// Two-digit day component (`00..=99`, not range-checked).
        day: u8,
        /// Two-digit month component (`00..=99`, not range-checked).
        month: u8,
    },
    /// A value that is not exactly four ASCII digits — preserved verbatim.
    Malformed(String),
}

impl DayMonth {
    /// Decode a `TDAT` (`DDMM`) value string into the typed view. Exactly
    /// four ASCII digits splits positionally into [`DayMonth::DayMonth`];
    /// anything else is [`DayMonth::Malformed`].
    fn from_field(value: &str) -> DayMonth {
        let bytes = value.as_bytes();
        if bytes.len() == 4 && bytes.iter().all(|b| b.is_ascii_digit()) {
            let d = |i: usize| bytes[i] - b'0';
            DayMonth::DayMonth {
                day: d(0) * 10 + d(1),
                month: d(2) * 10 + d(3),
            }
        } else {
            DayMonth::Malformed(value.to_string())
        }
    }
}

/// Typed view of the v2.3-only `TIME` "Time" frame (spec v2.3 §4.2.1).
///
/// The spec defines `TIME` as "a numeric string in the HHMM format
/// containing the time for the recording. This field is always four
/// characters long." v2.4 dropped this frame, folding the time into the
/// `TDRC` timestamp; so this view is **v2.3-only** by virtue of its
/// source frame id.
///
/// Surfaced via [`Id3Frame::time_hhmm`]. A value of exactly four ASCII
/// digits decodes to [`HourMinute::HourMinute`], split positionally and
/// **not** range-validated (`"2599"` surfaces `hour: 25, minute: 99`).
/// Anything else surfaces as [`HourMinute::Malformed`] with the raw
/// string preserved. The raw [`Id3Frame::Text::values`] is unchanged and
/// round-trips losslessly through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HourMinute {
    /// A spec-shaped `HHMM` value, split positionally and not
    /// range-validated.
    HourMinute {
        /// Two-digit hour component (`00..=99`, not range-checked).
        hour: u8,
        /// Two-digit minute component (`00..=99`, not range-checked).
        minute: u8,
    },
    /// A value that is not exactly four ASCII digits — preserved verbatim.
    Malformed(String),
}

impl HourMinute {
    /// Decode a `TIME` (`HHMM`) value string into the typed view. Exactly
    /// four ASCII digits splits positionally into
    /// [`HourMinute::HourMinute`]; anything else is
    /// [`HourMinute::Malformed`].
    fn from_field(value: &str) -> HourMinute {
        let bytes = value.as_bytes();
        if bytes.len() == 4 && bytes.iter().all(|b| b.is_ascii_digit()) {
            let d = |i: usize| bytes[i] - b'0';
            HourMinute::HourMinute {
                hour: d(0) * 10 + d(1),
                minute: d(2) * 10 + d(3),
            }
        } else {
            HourMinute::Malformed(value.to_string())
        }
    }
}

/// Typed view of the v2.3-only `TSIZ` "Size" frame (spec v2.3 §4.2.1).
///
/// The spec defines `TSIZ` as "the size of the audiofile in bytes,
/// excluding the ID3v2 tag, represented as a numeric string." v2.4
/// dropped this frame entirely (a parser can determine the audio size
/// from the file length minus the tag size), so this view is **v2.3-only**
/// by virtue of its source frame id.
///
/// Surfaced via [`Id3Frame::size_bytes`]. A non-empty ASCII-decimal value
/// decodes to [`SizeBytes::Bytes`]`(u64)`; anything else — an empty value,
/// a sign, a decimal point, whitespace, a non-digit byte, or a `u64`
/// overflow — surfaces as [`SizeBytes::Malformed`] with the raw string
/// preserved (matching the forward-compatible posture of [`DurationMs`]).
/// The raw [`Id3Frame::Text::values`] is unchanged and round-trips
/// losslessly through [`write_tag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SizeBytes {
    /// A spec-conforming numeric-string byte count.
    Bytes(u64),
    /// A value that does not match the spec's "numeric string"
    /// constraint — preserved verbatim.
    Malformed(String),
}

impl SizeBytes {
    /// Decode a `TSIZ` value string into the typed view. A non-empty
    /// ASCII-decimal string is [`SizeBytes::Bytes`]; anything else (empty,
    /// sign, decimal point, whitespace, non-digit byte, or `u64` overflow)
    /// is [`SizeBytes::Malformed`].
    fn from_field(value: &str) -> SizeBytes {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return SizeBytes::Malformed(value.to_string());
        }
        match value.parse::<u64>() {
            Ok(n) => SizeBytes::Bytes(n),
            Err(_) => SizeBytes::Malformed(value.to_string()),
        }
    }
}

/// Typed view of an ID3v2.4 timestamp string.
///
/// The v2.4 "TDxx" date frames — `TDEN` (encoding time), `TDOR` (original
/// release time), `TDRC` (recording time), `TDRL` (release time), and
/// `TDTG` (tagging time) — all carry a timestamp "based on a subset of
/// ISO 8601" defined once in the structure document
/// (`id3v2.4.0-structure.html`): "When being as precise as possible the
/// format of a time string is `yyyy-MM-ddTHH:mm:ss` … but the precision
/// may be reduced by removing as many time indicators as wanted. Hence
/// valid timestamps are `yyyy`, `yyyy-MM`, `yyyy-MM-dd`,
/// `yyyy-MM-ddTHH`, `yyyy-MM-ddTHH:mm` and `yyyy-MM-ddTHH:mm:ss`. All
/// time stamps are UTC."
///
/// The six precision levels collapse onto one [`Id3Timestamp::DateTime`]
/// variant whose `month`/`day`/`hour`/`minute`/`second` are `Option`s,
/// each present exactly when the corresponding indicator survived the
/// precision reduction. The split is purely positional and structural:
/// the parser checks the separator grammar (`-` between date components,
/// `T` before the time, `:` between time components) and that every
/// numeric field is the right number of ASCII digits, but it does **not**
/// calendar-validate (a `"2024-13-40"` source surfaces `month: 13,
/// day: 40` rather than being rejected) because the spec fixes the
/// digit grammar with no validity constraint and forcing calendar
/// validity here would drop a forward-compatible-but-odd source. This
/// matches the positional, non-range-checking posture of [`Id3Date`].
///
/// The spec also allows a duration (the slash character "as described in
/// 8601") and multiple non-contiguous dates ("use multiple strings, if
/// allowed by the frame definition"). A duration carried in a single
/// value string contains a `/` which does not match the point-in-time
/// grammar, so it surfaces as [`Id3Timestamp::Malformed`] with the raw
/// string preserved; multiple non-contiguous dates arrive as separate
/// text-frame values, so the frame-level accessors return a
/// `Vec<Id3Timestamp>` over every value in wire order. Anything that
/// does not match one of the six precision forms — wrong separators,
/// wrong digit counts, trailing bytes, an embedded duration slash, or
/// an empty value — surfaces as [`Id3Timestamp::Malformed`] with the raw
/// string preserved verbatim.
///
/// The raw [`Id3Frame::Text::values`] is left untouched, so the exact
/// on-wire bytes still round-trip through [`write_tag`]; this mirrors the
/// forward-compatible, non-destructive posture of [`Id3Date`] and
/// [`TrackPosition`]. The "TDxx" timestamp frames are v2.4-only (v2.3
/// carried `TYER`/`TDAT`/`TIME`/`TRDA` instead), so these accessors are
/// version-locked to v2.4 by virtue of their source frame ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id3Timestamp {
    /// A spec-shaped timestamp at one of the six precision levels. `year`
    /// is always present (it is the least-precise valid form); each
    /// finer component is `Some` exactly when its indicator was present
    /// in the source and `None` otherwise. A component is never `Some`
    /// while a coarser one is `None` — the precision reduction only ever
    /// removes trailing indicators — so e.g. `hour` implies `day` and
    /// `month` are `Some`.
    DateTime {
        /// Four-digit year (`0000..=9999`). Always present.
        year: u16,
        /// Two-digit month (`00..=99`, not range-checked); `None` if the
        /// source stopped at `yyyy`.
        month: Option<u8>,
        /// Two-digit day (`00..=99`, not range-checked); `None` if the
        /// source stopped at `yyyy-MM` or coarser.
        day: Option<u8>,
        /// Two-digit hour out of 24 (`00..=99`, not range-checked);
        /// `None` if the source stopped at `yyyy-MM-dd` or coarser.
        hour: Option<u8>,
        /// Two-digit minute (`00..=99`, not range-checked); `None` if the
        /// source stopped at `yyyy-MM-ddTHH` or coarser.
        minute: Option<u8>,
        /// Two-digit second (`00..=99`, not range-checked); `None` unless
        /// the source carried the full `yyyy-MM-ddTHH:mm:ss` form.
        second: Option<u8>,
    },
    /// A value that does not match any of the six precision forms — wrong
    /// separators, wrong digit counts, trailing bytes, an embedded
    /// duration slash, or an empty value. The raw string is preserved so
    /// the caller can still inspect it.
    Malformed(String),
}

impl Id3Timestamp {
    /// Decode one timestamp value string into the typed view per the
    /// structure-doc ISO 8601 subset. A value matching one of the six
    /// precision forms is [`Id3Timestamp::DateTime`]; anything else maps
    /// to [`Id3Timestamp::Malformed`] with the raw string preserved.
    pub fn from_field(value: &str) -> Id3Timestamp {
        match parse_iso8601_subset(value) {
            Some(ts) => ts,
            None => Id3Timestamp::Malformed(value.to_string()),
        }
    }

    /// The year for a well-formed [`Id3Timestamp::DateTime`]; `None` for
    /// [`Id3Timestamp::Malformed`].
    pub fn year(&self) -> Option<u16> {
        match self {
            Id3Timestamp::DateTime { year, .. } => Some(*year),
            Id3Timestamp::Malformed(_) => None,
        }
    }

    /// The month for a well-formed [`Id3Timestamp::DateTime`] that carried
    /// month precision; `None` otherwise. Not calendar-validated.
    pub fn month(&self) -> Option<u8> {
        match self {
            Id3Timestamp::DateTime { month, .. } => *month,
            Id3Timestamp::Malformed(_) => None,
        }
    }

    /// The day for a well-formed [`Id3Timestamp::DateTime`] that carried
    /// day precision; `None` otherwise. Not calendar-validated.
    pub fn day(&self) -> Option<u8> {
        match self {
            Id3Timestamp::DateTime { day, .. } => *day,
            Id3Timestamp::Malformed(_) => None,
        }
    }

    /// The hour for a well-formed [`Id3Timestamp::DateTime`] that carried
    /// hour precision; `None` otherwise. Not range-validated.
    pub fn hour(&self) -> Option<u8> {
        match self {
            Id3Timestamp::DateTime { hour, .. } => *hour,
            Id3Timestamp::Malformed(_) => None,
        }
    }

    /// The minute for a well-formed [`Id3Timestamp::DateTime`] that
    /// carried minute precision; `None` otherwise. Not range-validated.
    pub fn minute(&self) -> Option<u8> {
        match self {
            Id3Timestamp::DateTime { minute, .. } => *minute,
            Id3Timestamp::Malformed(_) => None,
        }
    }

    /// The second for a well-formed [`Id3Timestamp::DateTime`] that
    /// carried full second precision; `None` otherwise. Not
    /// range-validated.
    pub fn second(&self) -> Option<u8> {
        match self {
            Id3Timestamp::DateTime { second, .. } => *second,
            Id3Timestamp::Malformed(_) => None,
        }
    }
}

/// Parse the structure-doc ISO 8601 subset for a single point-in-time
/// timestamp. Returns `Some(Id3Timestamp::DateTime { .. })` for one of
/// the six valid precision forms and `None` for anything else (so the
/// caller folds it into [`Id3Timestamp::Malformed`]). Validates the
/// separator grammar and per-field digit counts but does not
/// calendar-validate the numeric components.
fn parse_iso8601_subset(value: &str) -> Option<Id3Timestamp> {
    // Read exactly `n` ASCII digits from `b` starting at `pos`, returning
    // the parsed value and the new position. Fails if fewer than `n`
    // digits are available or a non-digit is encountered.
    fn take_digits(b: &[u8], pos: usize, n: usize) -> Option<(u32, usize)> {
        if pos + n > b.len() {
            return None;
        }
        let mut acc: u32 = 0;
        for &byte in &b[pos..pos + n] {
            if !byte.is_ascii_digit() {
                return None;
            }
            acc = acc * 10 + (byte - b'0') as u32;
        }
        Some((acc, pos + n))
    }
    // Require literal byte `sep` at `pos`, returning the next position.
    fn take_sep(b: &[u8], pos: usize, sep: u8) -> Option<usize> {
        if b.get(pos) == Some(&sep) {
            Some(pos + 1)
        } else {
            None
        }
    }

    let b = value.as_bytes();
    // year (always required, exactly 4 digits)
    let (year, p) = take_digits(b, 0, 4)?;
    let year = year as u16;
    if p == b.len() {
        return Some(Id3Timestamp::DateTime {
            year,
            month: None,
            day: None,
            hour: None,
            minute: None,
            second: None,
        });
    }
    // "-MM"
    let p = take_sep(b, p, b'-')?;
    let (month, p) = take_digits(b, p, 2)?;
    let month = month as u8;
    if p == b.len() {
        return Some(Id3Timestamp::DateTime {
            year,
            month: Some(month),
            day: None,
            hour: None,
            minute: None,
            second: None,
        });
    }
    // "-dd"
    let p = take_sep(b, p, b'-')?;
    let (day, p) = take_digits(b, p, 2)?;
    let day = day as u8;
    if p == b.len() {
        return Some(Id3Timestamp::DateTime {
            year,
            month: Some(month),
            day: Some(day),
            hour: None,
            minute: None,
            second: None,
        });
    }
    // "THH"
    let p = take_sep(b, p, b'T')?;
    let (hour, p) = take_digits(b, p, 2)?;
    let hour = hour as u8;
    if p == b.len() {
        return Some(Id3Timestamp::DateTime {
            year,
            month: Some(month),
            day: Some(day),
            hour: Some(hour),
            minute: None,
            second: None,
        });
    }
    // ":mm"
    let p = take_sep(b, p, b':')?;
    let (minute, p) = take_digits(b, p, 2)?;
    let minute = minute as u8;
    if p == b.len() {
        return Some(Id3Timestamp::DateTime {
            year,
            month: Some(month),
            day: Some(day),
            hour: Some(hour),
            minute: Some(minute),
            second: None,
        });
    }
    // ":ss"
    let p = take_sep(b, p, b':')?;
    let (second, p) = take_digits(b, p, 2)?;
    let second = second as u8;
    // Any trailing bytes (e.g. a duration slash, a timezone suffix, or a
    // fractional-second part the subset does not define) make this not a
    // valid point-in-time timestamp.
    if p != b.len() {
        return None;
    }
    Some(Id3Timestamp::DateTime {
        year,
        month: Some(month),
        day: Some(day),
        hour: Some(hour),
        minute: Some(minute),
        second: Some(second),
    })
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

    /// Typed accessor for the three-byte language field carried by the
    /// language-tagged frames (`COMM`, `USLT`, `USER`, `SYLT`). Returns
    /// `Some(lang)` decoded via [`Language::from_wire`] for those four
    /// variants, and `None` for every other variant — letting a caller
    /// reach the content language uniformly without matching each
    /// frame's struct shape.
    ///
    /// The wire bytes are interpreted per the structure doc's
    /// "three byte language field … according to ISO-639-2 … should be
    /// represented in lower case … 'XXX' if not known". The typed view
    /// distinguishes the `XXX` sentinel ([`Language::Unknown`]) from a
    /// well-formed code ([`Language::Code`], lower-cased) and from
    /// non-conforming bytes ([`Language::Malformed`], preserved). The
    /// field is identical across v2.3 and v2.4, so this accessor is
    /// version-independent — only the v2.4-specific lower-case
    /// recommendation and explicit sentinel come into play, both of
    /// which the typed view applies uniformly. Mirrors the
    /// cross-version, non-destructive posture of
    /// [`Id3Frame::timestamp_unit`].
    pub fn language(&self) -> Option<Language> {
        let bytes = match self {
            Id3Frame::Comment { lang, .. }
            | Id3Frame::Lyrics { lang, .. }
            | Id3Frame::TermsOfUse { lang, .. }
            | Id3Frame::SyncedLyrics { lang, .. } => *lang,
            _ => return None,
        };
        Some(Language::from_wire(bytes))
    }

    /// Typed accessor for the spec §4.2.2 "involved persons" pairs
    /// carried by the v2.4 `TIPL` text frame (involved-people list,
    /// role-to-name mapping) and the v2.3 `IPLS` structural frame.
    /// Returns `Some(pairs)` for both, `None` for any other variant.
    ///
    /// Spec wording (v2.4 §4.2.2, `TIPL`):
    /// "The 'Involved people list' is very similar to the musician
    /// credits list, but maps between functions, like producer, and
    /// names." The on-wire layout is the text-frame encoding byte
    /// followed by alternating NUL-terminated strings —
    /// `role_0\0 name_0\0 role_1\0 name_1\0 …`. The existing text-frame
    /// parser already splits on NUL into `values`; this accessor folds
    /// adjacent entries back into `(role, name)` pairs. A non-conforming
    /// odd-count source (trailing role with no name) folds into a pair
    /// with an empty name, matching how [`Id3Frame::Ipls`] surfaces the
    /// same truncation on the parser side.
    ///
    /// The v2.3 → v2.4 evolution drops `IPLS` in favour of `TIPL` (and
    /// adds `TMCL`, see [`Id3Frame::musician_credits`]); presenting both
    /// through one accessor lets callers handle either source version
    /// without matching on the underlying variant, matching the
    /// cross-version posture of [`Id3Frame::timestamp_unit`]. For a
    /// `TIPL` text frame whose `values` is empty, returns
    /// `Some(Vec::new())` so the caller can still distinguish "frame
    /// present but empty" from "frame absent".
    pub fn involved_people(&self) -> Option<Vec<(String, String)>> {
        match self {
            Id3Frame::Text { id, values } if id == "TIPL" => Some(pair_alternating(values)),
            Id3Frame::Ipls { pairs } => Some(pairs.clone()),
            _ => None,
        }
    }

    /// Typed accessor for the spec §4.2.2 "musician credits" pairs
    /// carried by the v2.4 `TMCL` text frame. Returns `Some(pairs)` of
    /// `(instrument, performer)` for `TMCL`, and `None` for any other
    /// variant — including `TIPL` / `IPLS`, which encode a *different*
    /// mapping (function-to-name rather than instrument-to-musician)
    /// and surface via [`Id3Frame::involved_people`] instead.
    ///
    /// Spec wording (v2.4 §4.2.2, `TMCL`):
    /// "The 'Musician credits list' is intended as a mapping between
    /// instruments and the musician that played it. Every odd field is
    /// an instrument and every even is an artist or a comma delimited
    /// list of artists." The wire layout matches `TIPL`: an encoding
    /// byte followed by alternating NUL-terminated strings. As with
    /// `involved_people`, a non-conforming odd-count source folds into
    /// a pair with an empty performer rather than crashing.
    ///
    /// `TMCL` is v2.4-only — v2.3's `IPLS` mixes both kinds of pair into
    /// a single frame, so there is no v2.3-side variant to surface here.
    /// A caller migrating a v2.3 tag to v2.4 reads the union via
    /// `involved_people` from `IPLS`, splits roles vs instruments by
    /// inspection, then writes back as separate `TIPL` and `TMCL` text
    /// frames.
    pub fn musician_credits(&self) -> Option<Vec<(String, String)>> {
        match self {
            Id3Frame::Text { id, values } if id == "TMCL" => Some(pair_alternating(values)),
            _ => None,
        }
    }

    /// Typed accessor for the `SYLT` "content type" byte (spec v2.3
    /// §4.10 / v2.4 §4.9). Returns `Some(kind)` when the wire byte is
    /// one of the spec-defined `$00..=$08` values, `None` for any
    /// other variant or any reserved wire byte. Lets callers route on
    /// the categorical meaning (lyrics vs. chord vs. event labels,
    /// …) without re-decoding the raw `u8`.
    ///
    /// Mirrors the cross-version posture of [`Id3Frame::timestamp_unit`]:
    /// the wire byte is shared between v2.3 and v2.4 except that v2.3
    /// stops at `$06` (Trivia) while v2.4 adds `$07` (URLs to webpages)
    /// and `$08` (URLs to images). A v2.3 source carrying `$07` or
    /// `$08` is rare and not strictly conformant — the accessor still
    /// surfaces the typed variant since the wire byte is unambiguous,
    /// matching how `timestamp_unit` ignores the cross-version
    /// section-number rename.
    pub fn sylt_content_type(&self) -> Option<SyltContentType> {
        match self {
            Id3Frame::SyncedLyrics { content_type, .. } => {
                SyltContentType::from_wire(*content_type)
            }
            _ => None,
        }
    }

    /// Typed accessor for the `COMR` "received as" byte (spec v2.3
    /// §4.25 / v2.4 §4.24). Returns `Some(mode)` when the wire byte
    /// is one of the spec-defined `$00..=$08` delivery modes (Other,
    /// CD album, file over Internet, stream, note sheets, …),
    /// `None` for any other variant or any reserved wire byte. Lets
    /// callers route on the categorical delivery mode without
    /// re-decoding the raw `u8`. The wire byte is identical between
    /// v2.3 and v2.4 so the accessor is version-independent.
    pub fn commercial_delivery(&self) -> Option<CommercialDelivery> {
        match self {
            Id3Frame::Commercial { received_as, .. } => CommercialDelivery::from_wire(*received_as),
            _ => None,
        }
    }

    /// Typed accessor for the `COMR` "price string" field (spec v2.3
    /// §4.25 / v2.4 §4.24). Returns `Some(prices)` for a
    /// [`Id3Frame::Commercial`] frame and `None` for any other variant.
    ///
    /// Spec wording (v2.4 §4.24): "A price is constructed by one three
    /// character currency code, encoded according to ISO 4217 …
    /// followed by a numerical value where \".\" is used as decimal
    /// separator. In the price string several prices may be
    /// concatenated, separated by a \"/\" character, but there may only
    /// be one currency of each type." This accessor splits the stored
    /// `price` string on `/` and decodes each element via
    /// [`Price::from_element`]; the returned vector preserves wire
    /// order. An empty `price` string yields `Some(Vec::new())` so the
    /// caller can distinguish "frame present, no price" from "frame
    /// absent". The spec's "one currency of each type" invariant is not
    /// enforced — a non-conforming source carrying a duplicate currency
    /// surfaces both elements rather than dropping data, matching the
    /// forward-compatible posture of the other typed views. The
    /// underlying `price` string is untouched, so the exact on-wire
    /// bytes still round-trip through [`write_tag`].
    pub fn commercial_prices(&self) -> Option<Vec<Price>> {
        match self {
            Id3Frame::Commercial { price, .. } => {
                if price.is_empty() {
                    Some(Vec::new())
                } else {
                    Some(price.split('/').map(Price::from_element).collect())
                }
            }
            _ => None,
        }
    }

    /// Typed accessor for the `OWNE` "price paid" field (spec v2.3
    /// §4.24 / v2.4 §4.23). Returns `Some(price)` for an
    /// [`Id3Frame::Ownership`] frame and `None` for any other variant.
    ///
    /// Spec wording (v2.4 §4.23): "The frame begins … with a 'price
    /// paid' field. The first three characters of this field contains
    /// the currency used for the transaction, encoded according to ISO
    /// 4217 alphabetic currency code. Concatenated to this is the
    /// actual price paid, as a numerical string using \".\" as the
    /// decimal separator." Unlike `COMR`, the `OWNE` field carries a
    /// single price element (no `/` concatenation — "the actual price
    /// paid"), so this accessor decodes the whole `price` string as one
    /// [`Price`] via [`Price::from_element`]. The underlying `price`
    /// string is untouched, so the exact on-wire bytes still round-trip
    /// through [`write_tag`].
    pub fn ownership_price(&self) -> Option<Price> {
        match self {
            Id3Frame::Ownership { price, .. } => Some(Price::from_element(price)),
            _ => None,
        }
    }

    /// Typed accessor for the `OWNE` "Date of purch." field (spec v2.3
    /// §4.24 / v2.4 §4.23). Returns `Some(date)` for an
    /// [`Id3Frame::Ownership`] frame and `None` for any other variant.
    ///
    /// Spec wording: the price-paid field is "followed by an 8 character
    /// date string (YYYYMMDD)". This accessor decodes that field via
    /// [`Id3Date::from_field`]: a well-formed eight-digit value splits into
    /// [`Id3Date::Ymd`] and anything else surfaces as
    /// [`Id3Date::Malformed`] with the raw string preserved. The wire
    /// grammar is identical between v2.3 and v2.4 so the accessor is
    /// version-independent, and the underlying `date` string is untouched
    /// so the exact on-wire bytes still round-trip through [`write_tag`],
    /// matching the forward-compatible posture of
    /// [`Id3Frame::ownership_price`].
    pub fn ownership_date(&self) -> Option<Id3Date> {
        match self {
            Id3Frame::Ownership { date, .. } => Some(Id3Date::from_field(date)),
            _ => None,
        }
    }

    /// Typed accessor for the `COMR` "Valid until" field (spec v2.3
    /// §4.25 / v2.4 §4.24). Returns `Some(date)` for an
    /// [`Id3Frame::Commercial`] frame and `None` for any other variant.
    ///
    /// Spec wording: the price string is "followed by an 8 character date
    /// string in the format YYYYMMDD, describing for how long the price
    /// is valid". This accessor decodes that field via
    /// [`Id3Date::from_field`] with the same eight-digit grammar as
    /// [`Id3Frame::ownership_date`] — a well-formed value splits into
    /// [`Id3Date::Ymd`] and anything else surfaces as
    /// [`Id3Date::Malformed`]. The wire grammar is identical between v2.3
    /// and v2.4 so the accessor is version-independent, and the
    /// underlying `valid_until` string is untouched so the exact on-wire
    /// bytes still round-trip through [`write_tag`], matching the
    /// forward-compatible posture of [`Id3Frame::commercial_prices`].
    pub fn commercial_valid_until(&self) -> Option<Id3Date> {
        match self {
            Id3Frame::Commercial { valid_until, .. } => Some(Id3Date::from_field(valid_until)),
            _ => None,
        }
    }

    /// Typed accessor for the `EQU2` "interpolation method" byte (spec
    /// v2.4 §4.12). Returns `Some(method)` when the wire byte is one of
    /// the spec-defined `$00` (Band) / `$01` (Linear) values, and
    /// `None` for any other variant or any reserved wire byte. Lets
    /// callers route on the categorical interpolation choice without
    /// re-decoding the raw `u8`. EQU2 is v2.4-only per spec (the v2.4
    /// frames doc lists `EQU2` and v2.3 carried `EQUA` instead, which
    /// uses a per-band inc/dec bitfield rather than a curve-level
    /// interpolation choice), so the accessor is version-locked to v2.4
    /// by virtue of its source variant. Mirrors the contract on
    /// [`Id3Frame::sylt_content_type`] and
    /// [`Id3Frame::commercial_delivery`].
    pub fn equ2_interpolation(&self) -> Option<Equ2Interpolation> {
        match self {
            Id3Frame::Equ2 { interpolation, .. } => Equ2Interpolation::from_wire(*interpolation),
            _ => None,
        }
    }

    /// Typed accessor for the `POPM` "rating" byte (spec v2.3 §4.18 /
    /// v2.4 §4.17). Returns `Some(rating)` for an
    /// [`Id3Frame::Popularimeter`] frame and `None` for any other
    /// variant. The byte is mapped through [`PopmRating::from_wire`]:
    /// `$00` becomes [`PopmRating::Unknown`] per the spec sentinel
    /// ("0 is unknown") and every other value becomes
    /// [`PopmRating::Rated`] carrying the raw `1..=255` magnitude where
    /// "1 is worst and 255 is best". Because the rating byte has no
    /// reserved range — all 256 values are meaningful — the inner
    /// result is `PopmRating` directly rather than `Option<PopmRating>`,
    /// distinguishing it from the enumerated-variant accessors such as
    /// [`Id3Frame::equ2_interpolation`] which reject reserved bytes.
    ///
    /// The raw `rating: u8` field is untouched, so the exact on-wire
    /// byte still round-trips through [`write_tag`]. The wording is
    /// reproduced verbatim in both the v2.3 and v2.4 docs, so the
    /// accessor is version-independent, matching the cross-version
    /// posture of [`Id3Frame::etco_event_types`].
    pub fn popm_rating(&self) -> Option<PopmRating> {
        match self {
            Id3Frame::Popularimeter { rating, .. } => Some(PopmRating::from_wire(*rating)),
            _ => None,
        }
    }

    /// Typed accessor for the `ETCO` per-event "type of event" bytes
    /// (spec v2.3 §4.6 / v2.4 §4.5). Returns `Some(types)` for an
    /// `EventTimingCodes` frame and `None` for any other variant; each
    /// element of the inner `Vec` is the typed decoding of that event's
    /// wire byte — `Some(EtcoEventType)` for a spec-defined byte
    /// (including the user-defined `$E0..=$EF` synchronisation slots,
    /// the `$FD` / `$FE` audio-end markers, and the `$FF` continuation
    /// marker) and `None` for a byte in either reserved range
    /// (`$17..=$DF`, `$F0..=$FC`). The wire byte is identical between
    /// v2.3 and v2.4 (the event-type table is reproduced bit-for-bit
    /// in both version docs) so the accessor is version-independent,
    /// matching the cross-version posture of
    /// [`Id3Frame::timestamp_unit`] and
    /// [`Id3Frame::commercial_delivery`].
    ///
    /// The raw `events: Vec<(u8, u32)>` field is unchanged and
    /// round-trips losslessly through [`write_tag`] for every byte
    /// value — including reserved bytes — so the typed view never
    /// costs callers the ability to preserve a forward-compatible
    /// payload. The 32-bit timestamp is left untouched here: a caller
    /// that wants the categorical event plus its time can `.zip` the
    /// returned vector against the raw `events.iter().map(|(_, ts)| ts)`.
    /// `Vec` length equals the source `events` length so positional
    /// indexing stays stable across the two views.
    pub fn etco_event_types(&self) -> Option<Vec<Option<EtcoEventType>>> {
        match self {
            Id3Frame::EventTimingCodes { events, .. } => Some(
                events
                    .iter()
                    .map(|(byte, _)| EtcoEventType::from_wire(*byte))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Typed accessor for the `SYTC` per-record "tempo" values (spec
    /// v2.4 §4.7). Returns `Some(tempos)` for a [`Id3Frame::SyncedTempo`]
    /// frame and `None` for any other variant; each element of the inner
    /// `Vec` is the typed decoding of that record's raw `u16` —
    /// `Some(SytcTempo)` for a spec-defined value (the two §4.7
    /// reserved-meaning bytes `$00` / `$01` plus the `2..=510` BPM
    /// range) and `None` for any value outside the spec range (`511..=`
    /// `u16::MAX`) so a non-conforming source surfaces structurally
    /// rather than mapping to a guessed variant. The accessor stays at
    /// the logical layer — the wire-level one-byte vs `$FF` two-byte
    /// split is already normalised in [`Id3Frame::SyncedTempo::codes`].
    /// `SYTC` is declared once per tag and once per the spec table
    /// (only the v2.4 frames doc lists it, but the wire layout is
    /// byte-aligned and version-independent so a v2.3 producer could
    /// emit the same frame; this crate's parser accepts it under both
    /// envelopes), so the accessor is effectively version-independent —
    /// matching the cross-version posture of
    /// [`Id3Frame::timestamp_unit`] and [`Id3Frame::etco_event_types`].
    ///
    /// The raw `codes: Vec<(u16, u32)>` field is unchanged and
    /// round-trips losslessly through [`write_tag`] for every value
    /// the wire format can represent (`0..=510`), so the typed view
    /// never costs callers the ability to preserve a forward-compatible
    /// payload. The 32-bit timestamp is left untouched here: a caller
    /// that wants the categorical tempo plus its time can `.zip` the
    /// returned vector against the raw `codes.iter().map(|(_, ts)| ts)`.
    /// The returned `Vec` length equals the source `codes` length so
    /// positional indexing stays stable across the two views.
    pub fn sytc_tempo_codes(&self) -> Option<Vec<Option<SytcTempo>>> {
        match self {
            Id3Frame::SyncedTempo { codes, .. } => Some(
                codes
                    .iter()
                    .map(|(bpm, _)| SytcTempo::from_wire(*bpm))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Decode the `TCON` "Content type" (genre) frame into its typed
    /// content-type references (spec v2.3 §4.2.1 / v2.4 §4.2.3). Returns
    /// `None` for any frame that is not a `TCON` text frame; returns
    /// `Some(Vec::new())` for a present-but-empty `TCON`.
    ///
    /// TCON carries one or several content-type references in a single
    /// string. The two version dialects share a vocabulary but frame it
    /// differently, and this accessor normalises both onto
    /// [`ContentType`]:
    ///
    /// * v2.3 references are parenthesised — `"(21)"` is a numeric ID3v1
    ///   genre reference, `"(RX)"` / `"(CR)"` the Remix / Cover keywords,
    ///   `"(4)Eurodisco"` a numeric reference plus a free-text
    ///   refinement, `"(51)(39)"` two references in one string, and
    ///   `"((..."` a `((`-escaped literal-`(` free-text genre.
    /// * v2.4 dropped the parentheses — a numeric content type is a bare
    ///   number, `"RX"` / `"CR"` are bare keywords, and the text-frame
    ///   NUL list separates multiple references (so each is a separate
    ///   entry in [`Id3Frame::Text::values`]).
    ///
    /// The accessor walks the parser's already-NUL-split `values` and
    /// applies [`parse_tcon_value`] to each, so both dialects flatten to
    /// the same `Vec<ContentType>` in left-to-right wire order. Numeric
    /// references resolve their name against the same Winamp-extended
    /// ID3v1 genre table [`parse_id3v1`] uses; an out-of-table number
    /// surfaces structurally as [`ContentType::Genre`] with `name: None`
    /// rather than being dropped, matching the forward-compatible
    /// posture of the per-byte typed accessors
    /// ([`Id3Frame::etco_event_types`], [`Id3Frame::sytc_tempo_codes`]).
    /// The raw [`Id3Frame::Text::values`] is unchanged and round-trips
    /// losslessly through [`write_tag`], so the typed view never costs a
    /// caller the ability to preserve the exact on-wire string.
    pub fn content_types(&self) -> Option<Vec<ContentType>> {
        match self {
            Id3Frame::Text { id, values } if id == "TCON" => {
                let mut out = Vec::new();
                for value in values {
                    parse_tcon_value(value, &mut out);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Typed view of the `TMED` "Media type" frame (spec v2.3 §4.6.3 /
    /// v2.4 §4.2.3). The frame "describes from which media the sound
    /// originated" — "either a text string or a reference to the
    /// predefined media types found in the list below".
    ///
    /// Returns `None` for any frame other than `TMED`. For `TMED` it
    /// returns the references in left-to-right wire order, normalising
    /// both version dialects onto [`MediaType`]:
    ///
    /// * v2.3 wraps a reference in `"("` / `")"`, optionally followed by a
    ///   free-text refinement — `(MC) with four channels` parses to
    ///   [`MediaType::Predefined`] `media = "MC"`, `text = Some(" with
    ///   four channels")`. `(VID/PAL/VHS)` parses to `media = "VID"`,
    ///   `refinements = ["PAL", "VHS"]`. A `"(("`-escaped value surfaces
    ///   as [`MediaType::Custom`] with the escape collapsed.
    /// * v2.4 dropped the parentheses, so the spec's bare example
    ///   `VID/PAL/VHS` parses to the same `Predefined` reference.
    ///
    /// Each value in the parser's already-NUL-split
    /// [`Id3Frame::Text::values`] yields one reference. A top-level code
    /// outside the spec's predefined table resolves to
    /// [`MediaType::Predefined`] with `name: None` so a forward-compatible
    /// reference surfaces structurally rather than being dropped, matching
    /// the posture of [`Id3Frame::content_types`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`], so the typed view never costs a caller the
    /// ability to preserve the exact on-wire string.
    pub fn media_type(&self) -> Option<Vec<MediaType>> {
        match self {
            Id3Frame::Text { id, values } if id == "TMED" => {
                let mut out = Vec::new();
                for value in values {
                    parse_tmed_value(value, &mut out);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Typed view of the `TFLT` "File type" frame (spec v2.3 §4.2.1 /
    /// v2.4 §4.2.3). The frame "indicates which type of audio this tag
    /// defines" via a predefined code optionally followed by
    /// `/`-separated refinements, "in a similar way to the predefined
    /// types in the `TMED` frame, but without parentheses".
    ///
    /// Returns `None` for any frame other than `TFLT`. For `TFLT` it
    /// returns one [`FileType`] per [`Id3Frame::Text::values`] entry in
    /// wire order. Because the frame never uses parentheses and carries
    /// no v2.3 free-text refinement, the same bare grammar covers both
    /// version dialects — `MPG/3` → [`FileType::Predefined`] `code =
    /// "MPG"`, `refinements = ["3"]`. The only version difference is the
    /// v2.4-added `MIME` top-level code, which the predefined table
    /// resolves under either envelope since the byte-form is identical.
    ///
    /// A top-level code outside the spec's predefined table resolves to
    /// [`FileType::Predefined`] with `name: None` so a forward-compatible
    /// reference surfaces structurally rather than being dropped, matching
    /// the posture of [`Id3Frame::media_type`]. A value whose top-level
    /// segment is empty surfaces as [`FileType::Custom`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`], so the typed view never costs a caller the
    /// ability to preserve the exact on-wire string.
    pub fn file_type(&self) -> Option<Vec<FileType>> {
        match self {
            Id3Frame::Text { id, values } if id == "TFLT" => {
                let mut out = Vec::new();
                for value in values {
                    parse_tflt_value(value, &mut out);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Typed view of the `TKEY` "Initial key" frame (spec v2.3 §4.2.1 /
    /// v2.4 §4.2.3). The frame "contains the musical key in which the
    /// sound starts", represented as a string of at most three
    /// characters: a ground key `"A"`..`"G"`, an optional `"b"` / `"#"`
    /// halfkey, an optional `"m"` minor marker, or the standalone `"o"`
    /// off-key sentinel.
    ///
    /// Returns `None` for any frame other than `TKEY`. For `TKEY` it
    /// returns one [`MusicalKey`] per [`Id3Frame::Text::values`] entry in
    /// wire order (a conformant tag carries a single value, but the
    /// text-frame parser splits on NUL so the accessor tolerates a
    /// multi-value source). A value that does not match the spec grammar
    /// surfaces as [`MusicalKey::Custom`] so a forward-compatible or
    /// non-conforming source is preserved rather than dropped. The wire
    /// grammar is identical across v2.2 (`TKE`), v2.3, and v2.4 so the
    /// accessor is version-independent, matching the posture of
    /// [`Id3Frame::content_types`] and [`Id3Frame::media_type`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn initial_key(&self) -> Option<Vec<MusicalKey>> {
        match self {
            Id3Frame::Text { id, values } if id == "TKEY" => {
                Some(values.iter().map(|v| parse_tkey_value(v)).collect())
            }
            _ => None,
        }
    }

    /// Typed view of the `TRCK` "Track number/Position in set" frame (spec
    /// v2.3 §4.2.1 / v2.4 §4.2.1). The frame is "a numeric string
    /// containing the order number of the audio-file on its original
    /// recording", which "MAY be extended with a `"/"` character and a
    /// numeric string containing the total number of tracks/elements on
    /// the original recording. E.g. `"4/9"`".
    ///
    /// Returns `None` for any frame other than `TRCK`. For `TRCK` it
    /// returns the parsed [`TrackPosition`] for the frame's first value (a
    /// conformant tag carries a single value). A value that does not match
    /// the spec grammar surfaces as [`TrackPosition::Malformed`] so a
    /// forward-compatible or non-conforming source is preserved rather
    /// than dropped. The wire grammar is identical across v2.2 (`TRK`),
    /// v2.3, and v2.4 so the accessor is version-independent, matching the
    /// posture of [`Id3Frame::initial_key`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn track_number(&self) -> Option<TrackPosition> {
        match self {
            Id3Frame::Text { id, values } if id == "TRCK" => Some(parse_track_position(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the `TPOS` "Part of a set" frame (spec v2.3 §4.2.1 /
    /// v2.4 §4.2.1). The frame is "a numeric string that describes which
    /// part of a set the audio came from", whose value "MAY be extended
    /// with a `"/"` character and a numeric string containing the total
    /// number of parts in the set. E.g. `"1/2"`".
    ///
    /// Returns `None` for any frame other than `TPOS`. For `TPOS` it
    /// returns the parsed [`TrackPosition`] for the frame's first value.
    /// `TPOS` shares the `TRCK` grammar verbatim, so it decodes through the
    /// same [`TrackPosition`] view; a non-conforming value surfaces as
    /// [`TrackPosition::Malformed`]. Version-independent (wire grammar
    /// identical across v2.2 `TPA`, v2.3, and v2.4), matching
    /// [`Id3Frame::track_number`]. The raw [`Id3Frame::Text::values`] is
    /// unchanged and round-trips losslessly through [`write_tag`].
    pub fn part_of_set(&self) -> Option<TrackPosition> {
        match self {
            Id3Frame::Text { id, values } if id == "TPOS" => Some(parse_track_position(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the `TSRC` "ISRC" frame (spec v2.3 §4.2.1 / v2.4
    /// §4.2.1). The frame "should contain the International Standard
    /// Recording Code [ISRC] (12 characters)".
    ///
    /// Returns `None` for any frame other than `TSRC`. For `TSRC` it
    /// returns the parsed [`Isrc`] for the frame's first value (a
    /// conformant tag carries a single value). A value of exactly twelve
    /// ASCII characters decodes to [`Isrc::Code`]; any other length, an
    /// empty value, or a non-ASCII byte surfaces as [`Isrc::Malformed`]
    /// so a forward-compatible or non-conforming source is preserved
    /// rather than dropped. The wire form is identical across v2.2
    /// (`TRC`), v2.3, and v2.4 so the accessor is version-independent,
    /// matching the posture of [`Id3Frame::track_number`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn isrc(&self) -> Option<Isrc> {
        match self {
            Id3Frame::Text { id, values } if id == "TSRC" => Some(parse_tsrc_value(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the `TLEN` "Length" frame (spec v2.3 §4.2.1 / v2.4
    /// §4.2.1), "the length of the audio file in milliseconds, represented
    /// as a numeric string".
    ///
    /// Returns `None` for any frame other than `TLEN`. For `TLEN` it
    /// returns the parsed [`DurationMs`] for the frame's first value (a
    /// conformant tag carries a single value); an empty-`values` frame
    /// decodes to [`DurationMs::Malformed`]`("")` rather than panicking. A
    /// non-empty ASCII-decimal value decodes to [`DurationMs::Millis`];
    /// anything else surfaces as [`DurationMs::Malformed`] with the raw
    /// string preserved so a forward-compatible or non-conforming source
    /// is preserved rather than dropped. The wire form is identical across
    /// v2.2 (`TLE`), v2.3, and v2.4 so the accessor is version-independent,
    /// matching the posture of [`Id3Frame::isrc`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn length_ms(&self) -> Option<DurationMs> {
        match self {
            Id3Frame::Text { id, values } if id == "TLEN" => Some(parse_duration_ms(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the `TDLY` "Playlist delay" frame (spec v2.3 §4.2.1 /
    /// v2.4 §4.2.1), "the numbers of milliseconds of silence that should
    /// be inserted before this audio … represented as a numeric string".
    /// Per spec, "the value zero indicates that this is a part of a
    /// multifile audio track that should be played continuously"; that
    /// semantic surfaces as [`DurationMs::Millis`]`(0)`, leaving the
    /// interpretation to the caller.
    ///
    /// Returns `None` for any frame other than `TDLY`. `TDLY` shares the
    /// `TLEN` numeric-string-milliseconds grammar verbatim, so it decodes
    /// through the same [`DurationMs`] view; a non-conforming value
    /// surfaces as [`DurationMs::Malformed`]. Version-independent (wire
    /// grammar identical across v2.2 `TDY`, v2.3, and v2.4), matching
    /// [`Id3Frame::length_ms`]. The raw [`Id3Frame::Text::values`] is
    /// unchanged and round-trips losslessly through [`write_tag`].
    pub fn playlist_delay_ms(&self) -> Option<DurationMs> {
        match self {
            Id3Frame::Text { id, values } if id == "TDLY" => Some(parse_duration_ms(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the `TBPM` "BPM (beats per minute)" frame (spec v2.3
    /// §4.2.1 / v2.4 §4.2.1), "the number of beats per minute in the main
    /// part of the audio. The BPM is an integer and represented as a
    /// numerical string."
    ///
    /// Returns `None` for any frame other than `TBPM`. For `TBPM` it
    /// returns the parsed [`Bpm`] for the frame's first value; an
    /// empty-`values` frame decodes to [`Bpm::Malformed`]`("")`. A
    /// non-empty ASCII-decimal value decodes to [`Bpm::Beats`]; a
    /// fractional value violates the spec's "integer" requirement and
    /// surfaces as [`Bpm::Malformed`], as does any sign, whitespace, or
    /// non-digit byte. The wire form is identical across v2.2 (`TBP`),
    /// v2.3, and v2.4 so the accessor is version-independent, matching the
    /// posture of [`Id3Frame::length_ms`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn bpm(&self) -> Option<Bpm> {
        match self {
            Id3Frame::Text { id, values } if id == "TBPM" => Some(parse_bpm_value(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the v2.3-only `TYER` "Year" frame (spec v2.3 §4.2.1),
    /// "a numeric string with a year of the recording … always four
    /// characters long".
    ///
    /// Returns `None` for any frame other than `TYER`. For `TYER` it
    /// returns the parsed [`Id3Year`] for the frame's first value; an
    /// empty-`values` frame decodes to [`Id3Year::Malformed`]`("")`. A
    /// four-ASCII-digit value decodes to [`Id3Year::Year`]; anything else
    /// surfaces as [`Id3Year::Malformed`] with the raw string preserved.
    /// `TYER` is **v2.3-only** — v2.4 folded the year into the `TDRC`
    /// timestamp (see [`Id3Frame::recording_time`]) — so the accessor is
    /// version-locked to v2.3 by its source frame id. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn year(&self) -> Option<Id3Year> {
        match self {
            Id3Frame::Text { id, values } if id == "TYER" => Some(Id3Year::from_field(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the v2.3-only `TDAT` "Date" frame (spec v2.3 §4.2.1),
    /// "a numeric string in the DDMM format containing the date for the
    /// recording … always four characters long".
    ///
    /// Returns `None` for any frame other than `TDAT`. For `TDAT` it
    /// returns the parsed [`DayMonth`] for the frame's first value; a
    /// four-ASCII-digit value splits positionally into
    /// [`DayMonth::DayMonth`] (day then month, **not** calendar-validated)
    /// and anything else surfaces as [`DayMonth::Malformed`]. `TDAT` is
    /// **v2.3-only** — v2.4 folded the date into the `TDRC` timestamp — so
    /// the accessor is version-locked to v2.3. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`].
    pub fn date_ddmm(&self) -> Option<DayMonth> {
        match self {
            Id3Frame::Text { id, values } if id == "TDAT" => Some(DayMonth::from_field(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the v2.3-only `TIME` "Time" frame (spec v2.3 §4.2.1),
    /// "a numeric string in the HHMM format containing the time for the
    /// recording … always four characters long".
    ///
    /// Returns `None` for any frame other than `TIME`. For `TIME` it
    /// returns the parsed [`HourMinute`] for the frame's first value; a
    /// four-ASCII-digit value splits positionally into
    /// [`HourMinute::HourMinute`] (**not** range-validated) and anything
    /// else surfaces as [`HourMinute::Malformed`]. `TIME` is **v2.3-only**
    /// — v2.4 folded the time into the `TDRC` timestamp — so the accessor
    /// is version-locked to v2.3. The raw [`Id3Frame::Text::values`] is
    /// unchanged and round-trips losslessly through [`write_tag`].
    pub fn time_hhmm(&self) -> Option<HourMinute> {
        match self {
            Id3Frame::Text { id, values } if id == "TIME" => Some(HourMinute::from_field(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the v2.3-only `TSIZ` "Size" frame (spec v2.3 §4.2.1),
    /// "the size of the audiofile in bytes, excluding the ID3v2 tag,
    /// represented as a numeric string".
    ///
    /// Returns `None` for any frame other than `TSIZ`. For `TSIZ` it
    /// returns the parsed [`SizeBytes`] for the frame's first value; a
    /// non-empty ASCII-decimal value decodes to [`SizeBytes::Bytes`] and
    /// anything else (empty, sign, decimal point, whitespace, non-digit
    /// byte, or `u64` overflow) surfaces as [`SizeBytes::Malformed`].
    /// `TSIZ` is **v2.3-only** — v2.4 dropped it (the audio size is the
    /// file length minus the tag size) — so the accessor is
    /// version-locked to v2.3. The raw [`Id3Frame::Text::values`] is
    /// unchanged and round-trips losslessly through [`write_tag`].
    pub fn size_bytes(&self) -> Option<SizeBytes> {
        match self {
            Id3Frame::Text { id, values } if id == "TSIZ" => Some(SizeBytes::from_field(
                values.first().map(String::as_str).unwrap_or(""),
            )),
            _ => None,
        }
    }

    /// Typed view of the v2.4 "TDxx" timestamp date frames (spec v2.4 §4.2.5).
    ///
    /// Returns `Some(Vec<Id3Timestamp>)` for the five timestamp frames —
    /// `TDEN` encoding time, `TDOR` original release time, `TDRC` recording
    /// time, `TDRL` release time, `TDTG` tagging time — and `None` for every
    /// other frame, including the timestamp-class frame ids when carried
    /// under a non-`Text` variant. Each frame value is decoded through
    /// [`Id3Timestamp::from_field`] per the structure-doc ISO 8601 subset; the
    /// returned vector matches the source `values` positionally so the spec's
    /// "for multiple non-contiguous dates, use multiple strings" arrives as
    /// one `Id3Timestamp` per value in wire order. An empty-`values` frame
    /// yields an empty vector. A value that does not match one of the six
    /// precision forms surfaces as [`Id3Timestamp::Malformed`] so a
    /// forward-compatible or non-conforming source is preserved rather than
    /// dropped, matching the posture of [`Id3Frame::ownership_date`]. The raw
    /// [`Id3Frame::Text::values`] is unchanged and round-trips losslessly
    /// through [`write_tag`]. The TDxx frames are v2.4-only — v2.3 split the
    /// same information across `TYER`/`TDAT`/`TIME`/`TRDA` text frames — so the
    /// accessor is version-locked to v2.4 by virtue of its source frame ids.
    pub fn timestamps(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, values }
                if matches!(id.as_str(), "TDEN" | "TDOR" | "TDRC" | "TDRL" | "TDTG") =>
            {
                Some(values.iter().map(|v| Id3Timestamp::from_field(v)).collect())
            }
            _ => None,
        }
    }

    /// Typed view of the `TDRC` "Recording time" frame (spec v2.4 §4.2.5).
    /// Returns the parsed [`Id3Timestamp`] list for `TDRC` and `None` for
    /// every other frame, routing by frame id over the shared
    /// [`Id3Frame::timestamps`] decoder.
    pub fn recording_time(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, .. } if id == "TDRC" => self.timestamps(),
            _ => None,
        }
    }

    /// Typed view of the `TDRL` "Release time" frame (spec v2.4 §4.2.5).
    /// Returns the parsed [`Id3Timestamp`] list for `TDRL` and `None`
    /// otherwise.
    pub fn release_time(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, .. } if id == "TDRL" => self.timestamps(),
            _ => None,
        }
    }

    /// Typed view of the `TDOR` "Original release time" frame (spec v2.4
    /// §4.2.5). Returns the parsed [`Id3Timestamp`] list for `TDOR` and
    /// `None` otherwise.
    pub fn original_release_time(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, .. } if id == "TDOR" => self.timestamps(),
            _ => None,
        }
    }

    /// Typed view of the `TDEN` "Encoding time" frame (spec v2.4 §4.2.5).
    /// Returns the parsed [`Id3Timestamp`] list for `TDEN` and `None`
    /// otherwise.
    pub fn encoding_time(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, .. } if id == "TDEN" => self.timestamps(),
            _ => None,
        }
    }

    /// Typed view of the `TDTG` "Tagging time" frame (spec v2.4 §4.2.5).
    /// Returns the parsed [`Id3Timestamp`] list for `TDTG` and `None`
    /// otherwise.
    pub fn tagging_time(&self) -> Option<Vec<Id3Timestamp>> {
        match self {
            Id3Frame::Text { id, .. } if id == "TDTG" => self.timestamps(),
            _ => None,
        }
    }
}

/// Fold a flat list of NUL-delimited text-frame entries into
/// `(odd, even)` pairs per spec §4.2.2. A trailing odd entry (a
/// non-conforming source whose final role / instrument carries no
/// partner) is folded into a pair with an empty second component so
/// callers see the truncation structurally rather than losing it.
fn pair_alternating(values: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(values.len() / 2 + values.len() % 2);
    let mut i = 0;
    while i < values.len() {
        let role = values[i].clone();
        let name = if i + 1 < values.len() {
            values[i + 1].clone()
        } else {
            String::new()
        };
        out.push((role, name));
        i += 2;
    }
    out
}

/// Tag-size restriction sub-field of the v2.4 extended-header
/// restrictions byte (spec §3.2 sub-field `p`). Spec §3.2: "presence
/// of these restrictions does not affect how the tag is decoded,
/// merely how it was restricted before encoding."
///
/// Wire encoding lives in bits `7..=6` (`%pp______`) of the
/// restrictions byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TagSizeRestriction {
    /// `%00` — no more than 128 frames and 1 MB total tag size.
    #[default]
    Max128Frames1Mb,
    /// `%01` — no more than 64 frames and 128 KB total tag size.
    Max64Frames128Kb,
    /// `%10` — no more than 32 frames and 40 KB total tag size.
    Max32Frames40Kb,
    /// `%11` — no more than 32 frames and 4 KB total tag size.
    Max32Frames4Kb,
}

impl TagSizeRestriction {
    fn from_bits(b: u8) -> Self {
        match b & 0b11 {
            0 => TagSizeRestriction::Max128Frames1Mb,
            1 => TagSizeRestriction::Max64Frames128Kb,
            2 => TagSizeRestriction::Max32Frames40Kb,
            _ => TagSizeRestriction::Max32Frames4Kb,
        }
    }

    fn to_bits(self) -> u8 {
        match self {
            TagSizeRestriction::Max128Frames1Mb => 0,
            TagSizeRestriction::Max64Frames128Kb => 1,
            TagSizeRestriction::Max32Frames40Kb => 2,
            TagSizeRestriction::Max32Frames4Kb => 3,
        }
    }
}

/// Text-encoding restriction sub-field of the v2.4 extended-header
/// restrictions byte (spec §3.2 sub-field `q`). Lives in bit `5`
/// (`%__q_____`) of the restrictions byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextEncodingRestriction {
    /// `%0` — no restrictions on string encoding.
    #[default]
    Unrestricted,
    /// `%1` — strings are only encoded with ISO-8859-1 or UTF-8.
    Iso8859OrUtf8,
}

impl TextEncodingRestriction {
    fn from_bit(b: u8) -> Self {
        if b & 1 != 0 {
            TextEncodingRestriction::Iso8859OrUtf8
        } else {
            TextEncodingRestriction::Unrestricted
        }
    }

    fn to_bit(self) -> u8 {
        match self {
            TextEncodingRestriction::Unrestricted => 0,
            TextEncodingRestriction::Iso8859OrUtf8 => 1,
        }
    }
}

/// Text-fields size restriction sub-field of the v2.4 extended-header
/// restrictions byte (spec §3.2 sub-field `r`). Lives in bits
/// `4..=3` (`%___rr___`) of the restrictions byte. Per spec the limit
/// counts characters, not bytes — multi-byte encodings are not
/// renormalised before measuring. A multi-string text frame totals
/// its strings before the limit applies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextFieldsRestriction {
    /// `%00` — no restrictions on text-field length.
    #[default]
    Unrestricted,
    /// `%01` — no string longer than 1024 characters.
    Max1024Chars,
    /// `%10` — no string longer than 128 characters.
    Max128Chars,
    /// `%11` — no string longer than 30 characters.
    Max30Chars,
}

impl TextFieldsRestriction {
    fn from_bits(b: u8) -> Self {
        match b & 0b11 {
            0 => TextFieldsRestriction::Unrestricted,
            1 => TextFieldsRestriction::Max1024Chars,
            2 => TextFieldsRestriction::Max128Chars,
            _ => TextFieldsRestriction::Max30Chars,
        }
    }

    fn to_bits(self) -> u8 {
        match self {
            TextFieldsRestriction::Unrestricted => 0,
            TextFieldsRestriction::Max1024Chars => 1,
            TextFieldsRestriction::Max128Chars => 2,
            TextFieldsRestriction::Max30Chars => 3,
        }
    }
}

/// Image-encoding restriction sub-field of the v2.4 extended-header
/// restrictions byte (spec §3.2 sub-field `s`). Lives in bit `2`
/// (`%_____s__`) of the restrictions byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageEncodingRestriction {
    /// `%0` — no restrictions on attached-picture encoding.
    #[default]
    Unrestricted,
    /// `%1` — images are encoded only with PNG or JPEG.
    PngOrJpeg,
}

impl ImageEncodingRestriction {
    fn from_bit(b: u8) -> Self {
        if b & 1 != 0 {
            ImageEncodingRestriction::PngOrJpeg
        } else {
            ImageEncodingRestriction::Unrestricted
        }
    }

    fn to_bit(self) -> u8 {
        match self {
            ImageEncodingRestriction::Unrestricted => 0,
            ImageEncodingRestriction::PngOrJpeg => 1,
        }
    }
}

/// Image-size restriction sub-field of the v2.4 extended-header
/// restrictions byte (spec §3.2 sub-field `t`). Lives in bits
/// `1..=0` (`%______tt`) of the restrictions byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageSizeRestriction {
    /// `%00` — no restrictions on attached-picture pixel size.
    #[default]
    Unrestricted,
    /// `%01` — all images are 256x256 pixels or smaller.
    Max256x256,
    /// `%10` — all images are 64x64 pixels or smaller.
    Max64x64,
    /// `%11` — all images are exactly 64x64 pixels, unless
    /// required otherwise.
    Exactly64x64,
}

impl ImageSizeRestriction {
    fn from_bits(b: u8) -> Self {
        match b & 0b11 {
            0 => ImageSizeRestriction::Unrestricted,
            1 => ImageSizeRestriction::Max256x256,
            2 => ImageSizeRestriction::Max64x64,
            _ => ImageSizeRestriction::Exactly64x64,
        }
    }

    fn to_bits(self) -> u8 {
        match self {
            ImageSizeRestriction::Unrestricted => 0,
            ImageSizeRestriction::Max256x256 => 1,
            ImageSizeRestriction::Max64x64 => 2,
            ImageSizeRestriction::Exactly64x64 => 3,
        }
    }
}

/// Decoded form of the v2.4 extended-header restrictions byte (spec
/// §3.2 sub-field `d`). The wire byte is laid out as `%ppqrrstt`
/// across the five typed sub-fields. The restrictions are advisory:
/// the spec says they describe how the tag was *restricted before
/// encoding*, not how to decode it, so this crate's parser preserves
/// them losslessly without enforcing them, and the writer emits them
/// verbatim when supplied via [`WriteOptions::with_restrictions`].
///
/// Restrictions are a v2.4-only construct (v2.3 has no
/// equivalent extended-header sub-field). The writer rejects
/// [`WriteOptions::with_restrictions`] under a v2.3 target with
/// [`Error::unsupported`], matching the v2.3-only / v2.4-only
/// rejection pattern used for `with_footer` and the `RVAD` / `EQUA`
/// frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restrictions {
    /// Bits `7..=6` — tag-size restriction sub-field `p`.
    pub tag_size: TagSizeRestriction,
    /// Bit `5` — text-encoding restriction sub-field `q`.
    pub text_encoding: TextEncodingRestriction,
    /// Bits `4..=3` — text-fields-size restriction sub-field `r`.
    pub text_fields: TextFieldsRestriction,
    /// Bit `2` — image-encoding restriction sub-field `s`.
    pub image_encoding: ImageEncodingRestriction,
    /// Bits `1..=0` — image-size restriction sub-field `t`.
    pub image_size: ImageSizeRestriction,
}

impl Restrictions {
    /// Decode the wire restrictions byte into typed sub-fields.
    pub fn from_wire(byte: u8) -> Self {
        Self {
            tag_size: TagSizeRestriction::from_bits(byte >> 6),
            text_encoding: TextEncodingRestriction::from_bit(byte >> 5),
            text_fields: TextFieldsRestriction::from_bits(byte >> 3),
            image_encoding: ImageEncodingRestriction::from_bit(byte >> 2),
            image_size: ImageSizeRestriction::from_bits(byte),
        }
    }

    /// Encode the typed sub-fields back to the wire byte (`%ppqrrstt`).
    pub fn to_wire(self) -> u8 {
        (self.tag_size.to_bits() << 6)
            | (self.text_encoding.to_bit() << 5)
            | (self.text_fields.to_bits() << 3)
            | (self.image_encoding.to_bit() << 2)
            | self.image_size.to_bits()
    }
}

/// Decoded form of the v2.3 / v2.4 extended header (spec §3.2).
///
/// The extended header is optional in both v2.3 and v2.4 (and only
/// present when the tag-header flag bit `0x40` is set). When absent
/// the tag carries no extended metadata and this struct's
/// [`Default`] (all `false` / `None`) is the parsed result.
///
/// * `is_update` — v2.4-only "Tag is an update" flag (spec §3.2
///   sub-field `b`). v2.3 has no equivalent; it always parses as
///   `false` for a v2.3 tag.
/// * `crc` — the verified CRC-32 stored in the extended header, when
///   present. v2.3 stores 4 raw bytes; v2.4 stores a 5-byte
///   synchsafe-encoded 35-bit value (upper 4 bits zero). Both decode
///   to the same `u32`. The CRC is verified during parse, so a
///   mismatch is a parse error; this field carries the verified
///   value only.
/// * `restrictions` — v2.4-only restrictions byte decoded into typed
///   sub-fields (`%ppqrrstt`). v2.3 has no equivalent; always `None`
///   for a v2.3 tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtendedHeader {
    /// `true` when the v2.4 extended-header `b` flag bit is set:
    /// "Tag is an update. If this flag is set, the present tag is an
    /// update of a tag found earlier in the present file or stream."
    /// Per spec, "if frames defined as unique are found in the present
    /// tag, they are to override any corresponding ones found in the
    /// earlier tag."
    pub is_update: bool,
    /// `Some(crc)` when an extended-header CRC was present and
    /// verified during parse, `None` otherwise. v2.3 stores 4 raw
    /// bytes; v2.4 stores a 5-byte synchsafe 35-bit value (upper 4
    /// bits are zero by spec, so it fits in a `u32`).
    pub crc: Option<u32>,
    /// `Some(restrictions)` when the v2.4 extended-header `d` flag
    /// bit is set, `None` otherwise (always `None` for v2.3 — the
    /// restrictions sub-field is v2.4-only).
    pub restrictions: Option<Restrictions>,
}

/// Parse an ID3v2 tag from a buffer that starts with the 10-byte
/// header. On success, returns the [`Id3Tag`] and the total number of
/// bytes consumed from `buf` (header + body + optional footer) —
/// callers can seek by that many bytes to reach the next byte after
/// the tag.
///
/// Use [`parse_tag_with_extended_header`] instead when you need to
/// inspect the decoded extended-header (CRC, `is_update`,
/// restrictions byte). This entry point discards the extended header
/// after verifying its CRC.
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

    // v2.2 §3.1: header flag bit 6 means *compression*, not an
    // extended header (v2.2 has none): "Since no compression scheme
    // has been decided yet, the ID3 decoder (for now) should just
    // ignore the entire tag if the compression bit is set." We honour
    // that by returning the version envelope with no frames — the
    // `total` is still correct so a container caller can seek past
    // the tag.
    if matches!(version, Id3Version::V2_2) && flags & 0x40 != 0 {
        return Ok((
            Id3Tag {
                version,
                frames: Vec::new(),
            },
            total,
        ));
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
        let (after, _decoded) = parse_extended_header(version, body)?;
        body = after;
    }

    let frames = parse_frames(version, body);
    Ok((Id3Tag { version, frames }, total))
}

/// Parse an ID3v2 tag from a buffer that starts with the 10-byte
/// header, returning the decoded extended-header structure alongside
/// the tag.
///
/// This is the richer sibling of [`parse_tag`]: it returns the same
/// `(Id3Tag, usize)` plus an [`ExtendedHeader`] carrying the
/// `is_update` flag, the verified CRC-32 (when an extended-header
/// CRC was present), and the typed [`Restrictions`] sub-fields
/// (when the v2.4 restrictions flag was set). For a tag with no
/// extended header (header flag bit `0x40` clear) the returned
/// `ExtendedHeader` is [`ExtendedHeader::default`] — all `false` /
/// `None`. For v2.3 tags the `is_update` and `restrictions` fields
/// are always `false` / `None` since those flag bits are v2.4-only.
pub fn parse_tag_with_extended_header(buf: &[u8]) -> Result<(Id3Tag, ExtendedHeader, usize)> {
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

    if footer_present && !matches!(version, Id3Version::V2_4) {
        return Err(Error::invalid(
            "ID3v2 footer flag (0x10) is v2.4-only; rejected on v2.2/v2.3",
        ));
    }
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

    // v2.2 §3.1 compression bit — same ignore-the-entire-tag posture
    // as [`parse_tag`]; v2.2 has no extended header so the default
    // (all-absent) `ExtendedHeader` is returned.
    if matches!(version, Id3Version::V2_2) && flags & 0x40 != 0 {
        return Ok((
            Id3Tag {
                version,
                frames: Vec::new(),
            },
            ExtendedHeader::default(),
            total,
        ));
    }

    let unsync_whole_tag =
        (flags & 0x80) != 0 && matches!(version, Id3Version::V2_2 | Id3Version::V2_3);
    let unsync_v24_body = (flags & 0x80) != 0 && matches!(version, Id3Version::V2_4);

    let body_owned;
    let mut body: &[u8] = if unsync_whole_tag || unsync_v24_body {
        body_owned = reverse_unsync(body);
        &body_owned
    } else {
        body
    };

    let mut ext_header = ExtendedHeader::default();
    if flags & 0x40 != 0 {
        let (after, decoded) = parse_extended_header(version, body)?;
        body = after;
        ext_header = decoded;
    }

    let frames = parse_frames(version, body);
    Ok((Id3Tag { version, frames }, ext_header, total))
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
            | Id3Frame::Rvad { .. }
            | Id3Frame::Equa { .. }
            | Id3Frame::Ipls { .. }
            | Id3Frame::EncryptedMeta { .. }
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

/// Hard ceiling on a single frame's decompressed payload. Both
/// version dialects carry an attacker-controlled "decompressed size"
/// announce next to the zlib stream (4 regular bytes in v2.3, a
/// 4-byte synchsafe data-length indicator in v2.4); without a cap a
/// 100-byte tag could announce a multi-gigabyte inflate target. 64
/// MiB is far beyond any legitimate single frame (the largest
/// real-world payloads are embedded APIC / GEOB objects of a few
/// MiB) while keeping the worst-case per-frame allocation bounded.
const MAX_DECOMPRESSED_FRAME: usize = 64 << 20;

/// Inflate a zlib-compressed frame payload (spec v2.3 §3.3 format
/// flag `i` / v2.4 §4.1.2 format flag `k`: "compressed using zlib").
///
/// `announced` is the decompressed size the frame header carried
/// alongside the stream. Both spec dialects make the announce
/// authoritative — it is the only way a conformant writer can let a
/// reader pre-size the output — so a stream that inflates to any
/// other length is treated as corruption and rejected, matching the
/// hard-error posture of the extended-header CRC check. The announce
/// also serves as the allocation cap: inflation stops with an error
/// the moment output would exceed it, so a zlib bomb costs at most
/// `min(announced, MAX_DECOMPRESSED_FRAME)` bytes.
fn inflate_frame(data: &[u8], announced: usize) -> Result<Vec<u8>> {
    if announced > MAX_DECOMPRESSED_FRAME {
        return Err(Error::invalid(
            "compressed ID3 frame announces an implausibly large decompressed size",
        ));
    }
    let out = compcol::vec::decompress_to_vec_capped::<compcol::zlib::Zlib>(data, announced as u64)
        .map_err(|e| Error::invalid(format!("compressed ID3 frame: zlib inflate failed: {e:?}")))?;
    if out.len() != announced {
        return Err(Error::invalid(
            "compressed ID3 frame: decompressed size does not match the announced size",
        ));
    }
    Ok(out)
}

/// Deflate a frame payload into the RFC 1950 zlib stream the
/// frame-level compression flag is defined over, at `compcol`'s
/// default compression level.
fn deflate_frame(data: &[u8]) -> Result<Vec<u8>> {
    compcol::vec::compress_to_vec::<compcol::zlib::Zlib>(data)
        .map_err(|e| Error::invalid(format!("ID3 frame zlib deflate failed: {e:?}")))
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
fn parse_extended_header(version: Id3Version, body: &[u8]) -> Result<(&[u8], ExtendedHeader)> {
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
            let mut decoded = ExtendedHeader::default();
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
                decoded.crc = Some(stored);
            }
            Ok((after, decoded))
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
            let mut stored_restrictions: Option<Restrictions> = None;
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
                if name == "restrictions" && data_len == 1 {
                    // Spec §3.2 sub-field `d`: 1-byte payload `%ppqrrstt`
                    // decoded into typed sub-fields.
                    stored_restrictions = Some(Restrictions::from_wire(ext[cursor]));
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
            let decoded = ExtendedHeader {
                is_update: update,
                crc: stored_crc,
                restrictions: stored_restrictions,
            };
            Ok((after, decoded))
        }
        _ => Ok((body, ExtendedHeader::default())),
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
    // 5 synchsafe bytes carry 35 bits (5 * 7). A u32 CRC uses 32
    // bits, so the top three of the 35 are always zero. The
    // top synchsafe byte carries bits 31..=28 of the CRC plus three
    // padding zero bits above them — mask is `0x0F`, not `0x07`,
    // since bit 31 is part of the CRC payload and must survive a
    // large CRC value (top bit set).
    [
        ((v >> 28) & 0x0F) as u8,
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
    let flags = u16::from_be_bytes([buf[8], buf[9]]);
    if 10 + size > buf.len() {
        return Err(Error::invalid("v2.3 frame overflows tag body"));
    }
    let mut payload = &buf[10..10 + size];

    // Format flags (second flags byte, spec §3.3 `%ijk00000`):
    // i (0x80) = compression, j (0x40) = encryption, k (0x20) =
    // grouping identity. The first byte carries the alter-preservation
    // / read-only status bits, which are advisory for a reader.
    let compressed = flags & 0x0080 != 0;
    let encrypted = flags & 0x0040 != 0;
    let grouping = flags & 0x0020 != 0;

    // Spec §3.3: the flag-indicated additions extend the frame header
    // "in the same order as the flags that indicates them. I.e. the
    // four bytes of decompressed size will precede the encryption
    // method byte" — so: decompressed size, then encryption method,
    // then group identifier. They count toward the frame size but are
    // "not subject to encryption or compression".
    let mut decompressed_size = 0usize;
    if compressed {
        if payload.len() < 4 {
            return Err(Error::invalid(
                "v2.3 compressed frame missing the decompressed-size field",
            ));
        }
        decompressed_size = regular_u32(payload[0], payload[1], payload[2], payload[3]) as usize;
        payload = &payload[4..];
    }
    if encrypted {
        // One method byte follows (registered via ENCR). We carry no
        // keys, so preserve the method byte + ciphertext verbatim in
        // an Unknown frame — same posture as the v2.4 path.
        return Ok((
            Id3Frame::Unknown {
                id,
                raw: payload.to_vec(),
            },
            10 + size,
        ));
    }
    if grouping {
        if payload.is_empty() {
            return Err(Error::invalid(
                "v2.3 grouped frame missing the group-identifier byte",
            ));
        }
        payload = &payload[1..];
    }

    let frame = if compressed {
        let inflated = inflate_frame(payload, decompressed_size)?;
        dispatch_v23_v24(&id, &inflated)
    } else {
        dispatch_v23_v24(&id, payload)
    };
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
    if encrypted {
        // One method byte follows the group byte (spec §4.1.2 flag
        // `m`, registered via ENCR). We carry no keys, so preserve the
        // method byte + ciphertext verbatim in an Unknown frame so
        // callers can see it was present.
        return Ok((
            Id3Frame::Unknown {
                id,
                raw: payload.to_vec(),
            },
            10 + size,
        ));
    }
    if compressed {
        // Spec §4.1.2 flag `k`: "compressed using zlib deflate
        // method. If set, this requires the 'Data Length Indicator'
        // bit to be set as well" — the DLI carries the decompressed
        // size that v2.3 stored in its dedicated header field.
        if !data_length_indicator {
            return Err(Error::invalid(
                "v2.4 compressed frame missing the required data-length indicator",
            ));
        }
        if payload.len() < 4 {
            return Err(Error::invalid("v2.4 frame data-length indicator truncated"));
        }
        let announced = synchsafe_u32(payload[0], payload[1], payload[2], payload[3]) as usize;
        let mut data = &payload[4..];
        // Decoding inverts the write order (compress, then unsync):
        // reverse the per-frame unsync first, then inflate.
        let unsync_owned;
        if frame_unsync {
            unsync_owned = reverse_unsync(data);
            data = &unsync_owned;
        }
        let inflated = inflate_frame(data, announced)?;
        return Ok((dispatch_v23_v24(&id, &inflated), 10 + size));
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
        "RVAD" => parse_rvad(payload),
        "EQUA" => parse_equa(payload),
        "IPLS" => parse_ipls(payload),
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
        "EQU" => parse_equa(payload),
        // The remaining ID3v2.2 §4 frame bodies are byte-identical to
        // their v2.3 descendants (only the 6-byte frame header — 3-char
        // id + 3-byte size, no flags — differs), so they share the
        // v2.3 payload parsers:
        //   UFI §4.1 = UFID, IPL §4.4 = IPLS, MCI §4.5 = MCDI,
        //   ETC §4.6 = ETCO, MLL §4.7 = MLLT, STC §4.8 = SYTC,
        //   SLT §4.10 = SYLT, GEO §4.16 = GEOB, CNT §4.17 = PCNT,
        //   POP §4.18 = POPM, BUF §4.19 = RBUF, CRA §4.21 = AENC.
        "UFI" => parse_ufid(payload),
        "IPL" => parse_ipls(payload),
        "MCI" => parse_mcdi(payload),
        "ETC" => parse_etco(payload),
        "MLL" => parse_mllt(payload),
        "STC" => parse_sytc(payload),
        "SLT" => parse_sylt(payload),
        "GEO" => parse_geob(payload),
        "CNT" => parse_pcnt(payload),
        "POP" => parse_popm(payload),
        "BUF" => parse_rbuf(payload),
        "CRA" => parse_aenc(payload),
        // RVA (§4.12) and LNK (§4.22) need v2.2-specific walkers: RVA's
        // right/left fields are unconditional (presence is not keyed on
        // the sign bits) and LNK's linked frame identifier is always
        // exactly 3 bytes (no 3-vs-4 heuristic applies).
        "RVA" => parse_rva_v22(payload),
        "LNK" => parse_link_v22(payload),
        // CRM (encrypted meta frame, §4.20) has no v2.3/v2.4 descendant.
        // We carry no decryption plugins, but the frame's *structure*
        // (owner id + content/explanation + encrypted block) is defined
        // by the spec independently of the cipher, so we expose those
        // fields and preserve the encrypted block verbatim.
        "CRM" => parse_crm(payload),
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
        "EQU" => "EQUA",
        "COM" => "COMM",
        "ULT" => "USLT",
        "PIC" => "APIC",
        "UFI" => "UFID",
        "IPL" => "IPLS",
        "MCI" => "MCDI",
        "ETC" => "ETCO",
        "MLL" => "MLLT",
        "STC" => "SYTC",
        "SLT" => "SYLT",
        "RVA" => "RVAD",
        "GEO" => "GEOB",
        "CNT" => "PCNT",
        "POP" => "POPM",
        "BUF" => "RBUF",
        "CRA" => "AENC",
        "LNK" => "LINK",
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

/// Demote a v2.3/v2.4 four-char frame id to its ID3v2.2 three-char
/// equivalent for the writer. Returns `None` when the frame has no
/// v2.2 form — either because it is a v2.4-only addition (`TDRC`,
/// `RVA2`, `EQU2`, `SEEK`, `SIGN`, `ASPI`, `TMCL`, `TIPL`, …) or a
/// v2.3 frame that v2.2 never defined.
///
/// This is the inverse of [`v22_promote`]'s table, restricted to the
/// frames the ID3v2.2.0 §4 spec actually declares. The §4 frame set
/// is closed, so an id absent from the table maps to `None` and the
/// caller skips the frame rather than emitting an identifier a
/// conformant v2.2 reader could not interpret.
fn demote_to_v22(id: &str) -> Option<&'static str> {
    Some(match id {
        // §4.2 text information frames.
        "TIT1" => "TT1",
        "TIT2" => "TT2",
        "TIT3" => "TT3",
        "TPE1" => "TP1",
        "TPE2" => "TP2",
        "TPE3" => "TP3",
        "TPE4" => "TP4",
        "TCOM" => "TCM",
        "TEXT" => "TXT",
        "TLAN" => "TLA",
        "TCON" => "TCO",
        "TALB" => "TAL",
        "TPOS" => "TPA",
        "TRCK" => "TRK",
        "TSRC" => "TRC",
        "TYER" => "TYE",
        "TDAT" => "TDA",
        "TIME" => "TIM",
        "TRDA" => "TRD",
        "TMED" => "TMT",
        "TFLT" => "TFT",
        "TBPM" => "TBP",
        "TCMP" => "TCP",
        "TCOP" => "TCR",
        "TPUB" => "TPB",
        "TENC" => "TEN",
        "TSSE" => "TSS",
        "TOFN" => "TOF",
        "TLEN" => "TLE",
        "TSIZ" => "TSI",
        "TDLY" => "TDY",
        "TKEY" => "TKE",
        "TOAL" => "TOT",
        "TOPE" => "TOA",
        "TOLY" => "TOL",
        "TORY" => "TOR",
        "TXXX" => "TXX",
        // §4.x structural / binary frames.
        "RVRB" => "REV",
        "EQUA" => "EQU",
        "COMM" => "COM",
        "USLT" => "ULT",
        "APIC" => "PIC",
        "UFID" => "UFI",
        "IPLS" => "IPL",
        "MCDI" => "MCI",
        "ETCO" => "ETC",
        "MLLT" => "MLL",
        "SYTC" => "STC",
        "SYLT" => "SLT",
        "RVAD" => "RVA",
        "GEOB" => "GEO",
        "PCNT" => "CNT",
        "POPM" => "POP",
        "RBUF" => "BUF",
        "AENC" => "CRA",
        "LINK" => "LNK",
        // §4.3 URL link frames.
        "WOAF" => "WAF",
        "WOAR" => "WAR",
        "WOAS" => "WAS",
        "WCOM" => "WCM",
        "WCOP" => "WCP",
        "WPUB" => "WPB",
        "WXXX" => "WXX",
        _ => return None,
    })
}

fn parse_text_frame(id: &str, payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Text {
            id: id.to_string(),
            values: Vec::new(),
        };
    }
    let enc = payload[0];
    // v2.4 §4.2: multi-value text frames are a NUL-separated list,
    // where "null is represented by the termination code for the
    // character encoding" — one byte for ISO-8859-1/UTF-8, two
    // even-aligned bytes for UTF-16/UTF-16BE. Split at the byte level
    // and decode each segment on its own so a per-string UTF-16 BOM is
    // stripped from every value rather than only the first. v2.2/v2.3
    // single-value frames have no embedded NULs so the split is a
    // no-op for them.
    let values = split_text_values(enc, &payload[1..]);
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

/// Parse a `CRM` encrypted-meta payload (ID3v2.2 §4.20). Layout is:
///
/// ```text
/// Owner identifier      <ISO-8859-1 string> $00
/// Content/explanation   <ISO-8859-1 string> $00
/// Encrypted datablock   <binary data>
/// ```
///
/// Both leading strings are ISO-8859-1 (the frame predates any
/// per-frame encoding byte — v2.2 §4.20 lists no text-encoding field).
/// The encrypted block is opaque and preserved verbatim; this parser is
/// structural and never attempts decryption. A payload missing the
/// second terminator folds the remainder into `content` with an empty
/// `encrypted` block rather than erroring.
fn parse_crm(payload: &[u8]) -> Id3Frame {
    let (owner_bytes, after_owner) = split_once_nul_bytes(payload);
    let (content_bytes, encrypted) = split_once_nul_bytes(after_owner);
    Id3Frame::EncryptedMeta {
        owner: latin1_to_string(owner_bytes),
        content: latin1_to_string(content_bytes),
        encrypted: encrypted.to_vec(),
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

/// Parse a v2.2 `LNK` linked-information payload (ID3v2.2 §4.22).
/// Layout:
///
/// ```text
/// Frame identifier    $xx xx xx                    (always 3 bytes)
/// URL                 <ISO-8859-1 textstring> $00
/// Additional ID data  <textstring(s)>
/// ```
///
/// Unlike [`parse_link`] no 3-vs-4-byte id heuristic applies — every
/// v2.2 frame id is exactly three characters, so a URL whose first
/// byte happens to be an uppercase letter or digit can never be
/// misread as a fourth id character. The 4th slot of the shared
/// [`Id3Frame::LinkedInfo`] `frame_id` array is zero-padded, matching
/// the representation [`parse_link`] uses for short v2.3 ids.
fn parse_link_v22(payload: &[u8]) -> Id3Frame {
    if payload.len() < 3 {
        return Id3Frame::LinkedInfo {
            frame_id: [0; 4],
            url: String::new(),
            additional: Vec::new(),
        };
    }
    let frame_id = [payload[0], payload[1], payload[2], 0];
    let (url_bytes, additional_bytes) = split_once_nul_bytes(&payload[3..]);
    Id3Frame::LinkedInfo {
        frame_id,
        url: latin1_to_string(url_bytes),
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
/// Parse an `RVAD` relative-volume-adjustment payload (spec v2.3
/// §4.12). Layout is:
///
/// ```text
/// Increment/decrement              %00xxxxxx
/// Bits used for volume description $xx                  (must be != 0)
/// Relative volume change, right    ceil(bits/8) bytes BE (unsigned magnitude)
/// Relative volume change, left     ceil(bits/8) bytes BE
/// Peak volume right                ceil(bits/8) bytes BE (optional)
/// Peak volume left                 ceil(bits/8) bytes BE (optional)
/// [back-channel block: right-back delta, left-back delta, right-back peak, left-back peak]
/// [centre block: centre delta, centre peak]
/// [bass block: bass delta, bass peak]
/// ```
///
/// The spec presents the wire order as: **all deltas for a block,
/// then all peaks for the same block** (not interleaved
/// `delta + peak` per channel). Front-block presence is gated on
/// `increment_decrement & 0b0000_0011 != 0`; the back / centre / bass
/// blocks are appended extensions gated on bits 2..=5 in the same
/// byte. Peak fields are spec-optional ("if no other data follows,
/// be completely omitted") and are read greedily — the parser
/// consumes as many full peak-width slots as the remaining payload
/// affords. A short trailing peak (e.g. front-right peak present but
/// front-left peak missing) surfaces as the longer one carrying
/// bytes and the shorter as `peak.is_empty()`, preserving wire
/// truthfully.
///
/// A `bits_used` of `$00` is reserved per spec; the parser still
/// accepts it (zero-width fields => empty Vecs) so a non-conforming
/// source surfaces with zero-width channels rather than crashing.
/// The writer rejects `$00`.
///
/// A payload shorter than the 2-byte preamble preserves the raw
/// bytes through `Id3Frame::Unknown { id: "RVAD", .. }` since the
/// inc/dec + bits_used pair is the smallest interpretable form.
fn parse_rvad(payload: &[u8]) -> Id3Frame {
    if payload.len() < 2 {
        return Id3Frame::Unknown {
            id: "RVAD".to_string(),
            raw: payload.to_vec(),
        };
    }
    let increment_decrement = payload[0];
    let bits_used = payload[1];
    let width = (bits_used as usize).div_ceil(8);
    let mut cursor = 2usize;
    // Read a `width`-byte field from the payload, advancing the
    // cursor. Returns `Vec::new()` if the remaining payload is too
    // short, surfacing the "completely omitted" spec form.
    let take_field = |cursor: &mut usize, p: &[u8]| -> Vec<u8> {
        if *cursor + width <= p.len() {
            let v = p[*cursor..*cursor + width].to_vec();
            *cursor += width;
            v
        } else {
            Vec::new()
        }
    };
    // Pull a block of `n` (delta, peak) pairs per spec layout —
    // first the n deltas, then the n peaks. Returns the per-channel
    // Vec.
    let take_block = |cursor: &mut usize, n: usize, p: &[u8]| -> Vec<RvadChannel> {
        let mut deltas = Vec::with_capacity(n);
        for _ in 0..n {
            deltas.push(take_field(cursor, p));
        }
        let mut peaks = Vec::with_capacity(n);
        for _ in 0..n {
            peaks.push(take_field(cursor, p));
        }
        deltas
            .into_iter()
            .zip(peaks)
            .map(|(volume_delta, peak)| RvadChannel { volume_delta, peak })
            .collect()
    };
    let front_present = increment_decrement & 0b0000_0011 != 0;
    let back_present = increment_decrement & 0b0000_1100 != 0;
    let center_present = increment_decrement & 0b0001_0000 != 0;
    let bass_present = increment_decrement & 0b0010_0000 != 0;
    let front = if front_present {
        let mut chans = take_block(&mut cursor, 2, payload);
        let left = chans.pop().unwrap();
        let right = chans.pop().unwrap();
        Some(RvadFrontChannels { right, left })
    } else {
        None
    };
    let back = if back_present {
        let mut chans = take_block(&mut cursor, 2, payload);
        let left_back = chans.pop().unwrap();
        let right_back = chans.pop().unwrap();
        Some(RvadBackChannels {
            right_back,
            left_back,
        })
    } else {
        None
    };
    let center = if center_present {
        let mut chans = take_block(&mut cursor, 1, payload);
        Some(chans.pop().unwrap())
    } else {
        None
    };
    let bass = if bass_present {
        let mut chans = take_block(&mut cursor, 1, payload);
        Some(chans.pop().unwrap())
    } else {
        None
    };
    Id3Frame::Rvad {
        increment_decrement,
        bits_used,
        front,
        back,
        center,
        bass,
    }
}

/// Parse a v2.2 `RVA` relative-volume-adjustment payload (ID3v2.2
/// §4.12). Layout:
///
/// ```text
/// Increment/decrement            %000000xx
/// Bits used for volume descr.    $xx                  (must be != 0)
/// Relative volume change, right  ceil(bits/8) bytes BE (unsigned magnitude)
/// Relative volume change, left   ceil(bits/8) bytes BE
/// Peak volume right              ceil(bits/8) bytes BE (optional)
/// Peak volume left               ceil(bits/8) bytes BE (optional)
/// ```
///
/// The v2.2 frame is the two-channel predecessor of v2.3's `RVAD`,
/// sharing the inc/dec sign bitfield (bit 0 = right, bit 1 = left;
/// `1` = increment, `0` = decrement) and the field widths — but its
/// right/left volume-change fields are listed *unconditionally* in
/// §4.12, so presence is NOT keyed on the sign bits the way
/// [`parse_rvad`] gates its front block: a both-decrement frame
/// (inc/dec `$00`) still carries both magnitudes on the wire. Peak
/// fields "could be left zeroed or completely omitted" per §4.12 and
/// are read greedily, surfacing omission as `peak.is_empty()`.
///
/// The result is surfaced through the shared [`Id3Frame::Rvad`]
/// variant with `front` always populated and `back` / `center` /
/// `bass` always `None` (v2.2 §4.12 defines only the two channels),
/// so callers and the v2.3 writer handle both vintages uniformly.
fn parse_rva_v22(payload: &[u8]) -> Id3Frame {
    if payload.len() < 2 {
        // The inc/dec + bits_used preamble is the smallest
        // interpretable form — preserve anything shorter verbatim
        // (the 3-char wire id promotes on write via `v22_promote`).
        return Id3Frame::Unknown {
            id: "RVA".to_string(),
            raw: payload.to_vec(),
        };
    }
    let increment_decrement = payload[0];
    let bits_used = payload[1];
    let width = (bits_used as usize).div_ceil(8);
    let mut cursor = 2usize;
    let take_field = |cursor: &mut usize| -> Vec<u8> {
        if *cursor + width <= payload.len() {
            let v = payload[*cursor..*cursor + width].to_vec();
            *cursor += width;
            v
        } else {
            Vec::new()
        }
    };
    // Wire order per §4.12: both deltas first, then both peaks.
    let right_delta = take_field(&mut cursor);
    let left_delta = take_field(&mut cursor);
    let right_peak = take_field(&mut cursor);
    let left_peak = take_field(&mut cursor);
    Id3Frame::Rvad {
        increment_decrement,
        bits_used,
        front: Some(RvadFrontChannels {
            right: RvadChannel {
                volume_delta: right_delta,
                peak: right_peak,
            },
            left: RvadChannel {
                volume_delta: left_delta,
                peak: left_peak,
            },
        }),
        back: None,
        center: None,
        bass: None,
    }
}

/// Parse an `EQUA` equalisation payload (spec v2.3 §4.13). Layout is:
///
/// ```text
/// Adjustment bits   $xx                       (must be != $00 per spec)
/// For each band:
///   inc/freq        2 bytes BE                (MSB = increment bit, low 15 = frequency)
///   adjustment      ceil(adjustment_bits / 8) bytes BE (unsigned magnitude)
/// ```
///
/// A payload that is empty preserves the raw bytes through
/// [`Id3Frame::Unknown { id: "EQUA", .. }`][Id3Frame::Unknown] since the
/// single-byte `adjustment_bits` prefix is the smallest interpretable
/// form. `adjustment_bits = $00` is reserved per spec; the parser still
/// accepts it (zero-width `adjustment` => empty Vec per band) so a
/// non-conforming source surfaces structurally rather than crashing.
/// The writer rejects `$00`.
///
/// Spec rule "the equalisation bands should be ordered increasingly
/// with reference to frequency" is checked at write time, not parse
/// time — the parser preserves wire order so a caller can detect a
/// non-conforming source. A trailing band whose adjustment is short of
/// the spec width is dropped (the inc/freq bytes are consumed but no
/// truncated band is emitted), matching the parser's treatment of
/// short fields elsewhere in the crate.
fn parse_equa(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Unknown {
            id: "EQUA".to_string(),
            raw: payload.to_vec(),
        };
    }
    let adjustment_bits = payload[0];
    let width = (adjustment_bits as usize).div_ceil(8);
    let mut bands: Vec<EquaBand> = Vec::new();
    let mut i = 1usize;
    while i + 2 + width <= payload.len() {
        let high = payload[i];
        let low = payload[i + 1];
        i += 2;
        let increment = high & 0x80 != 0;
        let frequency = (((high as u16) & 0x7F) << 8) | (low as u16);
        let adjustment = payload[i..i + width].to_vec();
        i += width;
        bands.push(EquaBand {
            increment,
            frequency,
            adjustment,
        });
    }
    Id3Frame::Equa {
        adjustment_bits,
        bands,
    }
}

/// Parse an `IPLS` involved-people-list payload (spec v2.3 §4.4).
/// Layout: `encoding $xx` followed by alternating NUL-terminated
/// strings in the declared encoding — pair-wise
/// `(involvement, involvee)`. An empty payload (no even encoding
/// byte) surfaces as `Id3Frame::Unknown` so the wire bytes round-trip
/// untouched; otherwise we walk the string list pair-wise. A dangling
/// final involvement (a non-conforming source that omits the trailing
/// involvee) folds into a pair with an empty involvee rather than
/// being dropped, surfacing the truncation structurally.
fn parse_ipls(payload: &[u8]) -> Id3Frame {
    if payload.is_empty() {
        return Id3Frame::Unknown {
            id: "IPLS".to_string(),
            raw: payload.to_vec(),
        };
    }
    let enc = payload[0];
    let mut rest = &payload[1..];
    let mut pairs: Vec<(String, String)> = Vec::new();
    while !rest.is_empty() {
        let (involvement, after_invol) = split_once_nul(enc, rest);
        if after_invol.is_empty() {
            // Truncated trailing involvement with no involvee follow-up
            // — fold into a pair with an empty involvee so a caller can
            // detect the non-conforming source without us silently
            // dropping the dangling string.
            pairs.push((involvement, String::new()));
            break;
        }
        let (involvee, after_pair) = split_once_nul(enc, after_invol);
        pairs.push((involvement, involvee));
        rest = after_pair;
    }
    Id3Frame::Ipls { pairs }
}

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

/// Split a text-frame body into its constituent strings at the
/// encoding-appropriate NUL terminator, decoding each segment on its
/// own. The frames spec (§4.2: "multiple strings, stored as a null
/// separated list, where null is represented by the termination code
/// for the character encoding") makes the separator one byte for
/// ISO-8859-1 (`$00`) and UTF-8 (`$00`) and two even-aligned bytes for
/// UTF-16 (`$00 00`) / UTF-16BE.
///
/// Decoding each segment individually (rather than decoding the whole
/// concatenation and splitting the resulting `String` on `'\u{0}'`)
/// matters for the BOM form (`$01`): the structure spec states each
/// string in a UTF-16 frame carries its own BOM ("All strings in the
/// same frame SHALL have the same byteorder"), so the second and later
/// strings each begin with `$FF $FE` / `$FE $FF`. Decoding the
/// concatenation as one stream would leave every BOM after the first
/// as a literal U+FEFF (ZERO WIDTH NO-BREAK SPACE) glued to the front
/// of that value; per-segment decode strips each one through
/// [`decode_utf16_bom`].
///
/// Empty segments (a leading, trailing, or doubled separator) are
/// dropped so a NUL-padded single value does not surface as a spurious
/// empty string.
fn split_text_values(enc: u8, body: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if enc == 1 || enc == 2 {
        // UTF-16 family: separator is an even-aligned `$00 00`.
        let mut start = 0usize;
        let mut i = 0usize;
        while i + 1 < body.len() {
            if body[i] == 0 && body[i + 1] == 0 {
                if i > start {
                    out.push(decode_text(enc, &body[start..i]));
                }
                i += 2;
                start = i;
            } else {
                i += 2;
            }
        }
        if start < body.len() {
            let seg = &body[start..];
            // A trailing odd byte cannot be part of a UTF-16 unit; the
            // decoder ignores it. Skip an all-NUL tail.
            if seg.iter().any(|&b| b != 0) {
                out.push(decode_text(enc, seg));
            }
        }
    } else {
        // ISO-8859-1 / UTF-8: separator is a single `$00`.
        for seg in body.split(|&b| b == 0) {
            if !seg.is_empty() {
                out.push(decode_text(enc, seg));
            }
        }
    }
    out
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
        // v2.4 §4.2.5 timestamp-class frames the prior table omitted.
        // The wire payload is a free-form timestamp string per the
        // spec; surfacing them as Vorbis-style keys lets a consumer
        // read them without enum-matching on `Id3Frame::Text`.
        "TDEN" => "encodingtime",
        "TDTG" => "taggingtime",
        // v2.4 §4.2.3 informational frames new in v2.4 or previously
        // not mapped — TMOO mood, TFLT file-type (`MPG`, `PCM`, …),
        // TLEN length-in-ms (numeric per v2.3 §TLEN / v2.4 §4.2.3).
        "TMOO" => "mood",
        "TFLT" => "filetype",
        "TLEN" => "length",
        // v2.4 §4.2.4 rights/owner frames.
        "TOWN" => "owner",
        "TPRO" => "producednotice",
        // v2.4 §4.2.4 internet-radio frames (also valid in v2.3).
        "TRSN" => "radiostation",
        "TRSO" => "radiostationowner",
        // v2.4 §4.2.5 sort-order frames (also valid in v2.3 informally;
        // Vorbis convention is `*sort`).
        "TSOA" => "albumsort",
        "TSOP" => "artistsort",
        "TSOT" => "titlesort",
        // v2.4 §4.2.1 set-subtitle.
        "TSST" => "setsubtitle",
        // v2.4 §4.2.5 / v2.3 §TDLY playlist delay (ms between songs).
        "TDLY" => "playlistdelay",
        // v2.4 §4.2.5 / v2.3 §TOFN original filename.
        "TOFN" => "originalfilename",
        // v2.3-only date/time/recording-date/size frames. v2.4 folded
        // TYER/TDAT/TIME into TDRC and removed TRDA/TSIZ; on a v2.3
        // tag these still carry data and would otherwise drop to the
        // generic lowercased-id fallback.
        //
        // TDAT is "DDMM" (4 chars, spec §TDAT) — not the same shape as
        // TYER's "date"=year, so a distinct key avoids collision when
        // both frames are present in the same tag.
        "TDAT" => "date_ddmm",
        "TIME" => "time_hhmm",
        "TRDA" => "recordingdates",
        "TSIZ" => "size",
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
/// `target_version` may be [`Id3Version::V2_2`], [`Id3Version::V2_3`],
/// or [`Id3Version::V2_4`]; [`Id3Version::V1`] is handled by
/// [`write_id3v1`] instead. Frames are written in the order they
/// appear in the tag. Under a v2.2 target each frame is demoted to its
/// three-character v2.2 id and a frame with no v2.2 equivalent (a
/// v2.4-only addition or an unrecognised `Unknown` id) is skipped; the
/// v2.3/v2.4-only [`WriteOptions`] sub-fields (CRC, footer, update
/// flag, restrictions, frame compression) are rejected for v2.2.
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
    /// Set the v2.4 extended-header "Tag is an update" flag (spec
    /// §3.2 sub-field `b`). Default `false`. v2.4-only — the writer
    /// returns [`Error::unsupported`] if requested under a v2.3
    /// target, matching the `with_footer` / `with_restrictions`
    /// v2.4-only rejection pattern.
    ///
    /// Setting `is_update = true` (or `restrictions = Some(...)`)
    /// causes the writer to emit an extended header even when
    /// `crc = false`, so flags can be carried independently of the
    /// CRC.
    pub is_update: bool,
    /// Emit the v2.4 extended-header restrictions byte (spec §3.2
    /// sub-field `d`). Default `None` (no restrictions byte
    /// emitted). v2.4-only — the writer returns
    /// [`Error::unsupported`] if requested under a v2.3 target.
    ///
    /// Per spec the restrictions are advisory: they describe how
    /// the tag was restricted before encoding, not how the parser
    /// should decode it. This crate's parser preserves them
    /// losslessly without enforcing them.
    pub restrictions: Option<Restrictions>,
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
    /// Compress every frame's payload with the zlib deflate stream the
    /// frame-level compression flag is defined over (v2.3 §3.3 format
    /// flag `i` / v2.4 §4.1.2 format flag `k`). Default `false`: the
    /// payload is written verbatim.
    ///
    /// * v2.3 — the frame's format-flags byte gets bit 0x80 set and
    ///   the 4-byte big-endian decompressed size is written between
    ///   the frame header and the zlib stream, per §3.3 ("4 bytes for
    ///   'decompressed size' appended to the frame header").
    /// * v2.4 — format-flag bits 0x08 (compression) and 0x01
    ///   (data-length indicator) are both set, since §4.1.2 makes the
    ///   DLI mandatory under compression; the DLI carries the
    ///   decompressed size as a 32-bit synchsafe integer.
    ///
    /// Compression is applied to every frame unconditionally for
    /// deterministic output — the spec attaches the flag per frame but
    /// gives no size policy, and a tiny text frame may grow by the
    /// ~11-byte zlib envelope. Composes with the other options:
    /// per-frame unsync runs *after* compression (the spec orders
    /// encryption after compression and unsync over the final frame
    /// bytes), the extended-header CRC covers the post-compression
    /// frame bytes, and whole-tag unsync wraps the finished body.
    pub compress: bool,
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

    /// Builder-style setter for the v2.4 extended-header "Tag is an
    /// update" flag (spec §3.2 sub-field `b`). v2.4-only — see
    /// [`WriteOptions::is_update`].
    pub fn with_update(mut self, enabled: bool) -> Self {
        self.is_update = enabled;
        self
    }

    /// Builder-style setter for the v2.4 extended-header restrictions
    /// byte (spec §3.2 sub-field `d`). Pass `Some(r)` to emit the
    /// 1-byte `%ppqrrstt` restrictions advisory, `None` to omit it.
    /// v2.4-only — see [`WriteOptions::restrictions`].
    pub fn with_restrictions(mut self, restrictions: Option<Restrictions>) -> Self {
        self.restrictions = restrictions;
        self
    }

    /// Builder-style setter for frame-level zlib compression (spec
    /// v2.3 §3.3 format flag `i` / v2.4 §4.1.2 format flag `k`). See
    /// [`WriteOptions::compress`] for the per-version on-wire layout.
    pub fn with_compression(mut self, enabled: bool) -> Self {
        self.compress = enabled;
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
    // ID3v2.2 has its own frame-header shape (3-char id + 3-byte BE
    // size, no flags), no extended header, and no per-frame features,
    // so it takes a dedicated serialiser rather than threading
    // version branches through the v2.3/v2.4 body builder.
    if matches!(target_version, Id3Version::V2_2) {
        return write_tag_v22(tag, options);
    }

    let major: u8 = match target_version {
        Id3Version::V2_3 => 3,
        Id3Version::V2_4 => 4,
        Id3Version::V2_2 => unreachable!("v2.2 dispatched above"),
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

    // The "Tag is an update" flag and the restrictions byte are
    // both v2.4-only extended-header sub-fields (spec §3.2 sub-fields
    // `b` and `d`); v2.3 has no slot for them. Reject loudly so a
    // caller asking for either gets a clear error rather than a
    // silent drop.
    if options.is_update && !matches!(target_version, Id3Version::V2_4) {
        return Err(Error::unsupported(
            "ID3v2 extended-header `is_update` flag is v2.4-only; set target_version = V2_4 or clear is_update",
        ));
    }
    if options.restrictions.is_some() && !matches!(target_version, Id3Version::V2_4) {
        return Err(Error::unsupported(
            "ID3v2 extended-header restrictions byte is v2.4-only; set target_version = V2_4 or clear restrictions",
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
        write_frame_with_options(
            target_version,
            frame,
            frame_unsync,
            options.compress,
            &mut frame_bytes,
        )?;
    }

    // Optional extended header. We always emit the minimal CRC form
    // (no update / restrictions data), with size-of-padding = 0 in
    // v2.3 since this writer emits no padding. The CRC is computed on
    // the pre-unsync frame bytes — the v2.3 spec mandates this
    // ("calculated before unsynchronisation"), and for v2.4 it is the
    // natural interpretation since the parser always reverses unsync
    // before walking the extended header.
    // Emit an extended header when any of the optional sub-fields
    // (`crc`, `is_update`, `restrictions`) is requested. v2.3 only
    // recognises the CRC sub-field; the parser-side gate above
    // ensures `is_update` / `restrictions` never reach here under
    // v2.3, so a `crc=false` request alone never produces an ext
    // header.
    let want_ext_header = options.crc || options.is_update || options.restrictions.is_some();
    let ext_header = if want_ext_header {
        Some(build_extended_header(
            target_version,
            &frame_bytes,
            options,
        )?)
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
fn build_extended_header(
    target_version: Id3Version,
    frame_bytes: &[u8],
    options: &WriteOptions,
) -> Result<Vec<u8>> {
    match target_version {
        Id3Version::V2_3 => {
            // v2.3 spec §3.2 defines a single optional extended-header
            // sub-field: a 4-byte CRC. There is no "is_update" / no
            // restrictions byte in v2.3; the caller-facing options
            // for those are gated to v2.4 upstream of this call so
            // they cannot reach here under v2.3.
            if !options.crc {
                // The upstream gate keeps us here only when at least
                // one ext-header sub-field is requested, and under
                // v2.3 the only one available is `crc`.
                return Err(Error::invalid(
                    "ID3v2.3 extended header requires at least the CRC sub-field",
                ));
            }
            let crc = crc32_iso3309(frame_bytes);
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
            // v2.4 spec §3.2: the extended header carries an optional
            // mix of three flag sub-fields, in this fixed order on the
            // wire: `b` (update, 0-byte attached data), `c` (CRC,
            // 5-byte attached data), `d` (restrictions, 1-byte
            // attached data). The `is_update` flag has no attached
            // data and its flag bit alone carries the signal.
            let mut ext_flags: u8 = 0;
            // %0bcd0000
            if options.is_update {
                ext_flags |= 0x40;
            }
            if options.crc {
                ext_flags |= 0x20;
            }
            if options.restrictions.is_some() {
                ext_flags |= 0x10;
            }
            // Body bytes that follow the (size, num_flag_bytes,
            // flags) trio. Per spec the attached data is laid out in
            // flag-bit order — b, c, d — each prefixed by a 1-byte
            // data-length.
            let mut attached: Vec<u8> = Vec::new();
            if options.is_update {
                // Spec §3.2 sub-field `b`: "Flag data length $00".
                attached.push(0x00);
            }
            if options.crc {
                attached.push(0x05);
                let crc = crc32_iso3309(frame_bytes);
                attached.extend_from_slice(&crc32_to_synchsafe5(crc));
            }
            if let Some(restrictions) = options.restrictions {
                attached.push(0x01);
                attached.push(restrictions.to_wire());
            }
            // Total ext-header size INCLUDES the 4 synchsafe size
            // bytes plus the (num_flag_bytes, flags) pair plus the
            // attached data.
            let total = 4 + 2 + attached.len();
            // Synchsafe 28-bit fits any practical ext-header size
            // (the largest legal here is 4 + 2 + 1 + 1 + 6 + 2 = 16
            // bytes), but assert the spec's lower bound anyway.
            if total < 6 {
                return Err(Error::invalid(
                    "ID3v2.4 extended header size underflowed the 6-byte minimum",
                ));
            }
            let s = total as u32;
            let mut out = Vec::with_capacity(total);
            out.push(((s >> 21) & 0x7F) as u8);
            out.push(((s >> 14) & 0x7F) as u8);
            out.push(((s >> 7) & 0x7F) as u8);
            out.push((s & 0x7F) as u8);
            // number of flag bytes — spec says "$01" for the current
            // single-byte flags layout.
            out.push(0x01);
            out.push(ext_flags);
            out.extend_from_slice(&attached);
            Ok(out)
        }
        _ => Err(Error::invalid(
            "extended-header emission requires v2.3 or v2.4",
        )),
    }
}

/// Serialise an [`Id3Tag`] as an ID3v2.2.0 tag (spec `id3v2-00`).
///
/// v2.2 predates almost every extension the v2.3/v2.4 writer carries:
/// the only header flag bits are bit 7 (unsynchronisation) and bit 6
/// (compression, a scheme the spec never defined). There is no
/// extended header, no footer, no per-frame flags byte, and no
/// data-length indicator. Each frame header is six bytes — a
/// three-character id (capital A–Z, 0–9) plus a three-byte big-endian
/// size that excludes the header itself (spec §3.2).
///
/// Frames are demoted to their three-character v2.2 ids via
/// [`demote_to_v22`]; a frame with no v2.2 equivalent (a v2.4-only
/// addition, or an [`Id3Frame::Unknown`] whose id is not a valid v2.2
/// identifier) is skipped rather than emitted under an id a conformant
/// v2.2 reader could not interpret. The writer rejects the
/// v2.3/v2.4-only [`WriteOptions`] sub-fields (CRC, footer, update
/// flag, restrictions, frame compression) so a caller cannot silently
/// lose a requested feature; [`UnsyncMode::PerFrame`] downgrades to
/// [`UnsyncMode::WholeTag`] exactly as it does for v2.3.
fn write_tag_v22(tag: &Id3Tag, options: &WriteOptions) -> Result<Vec<u8>> {
    // None of the post-v2.2 extensions have a v2.2 wire slot. Reject
    // loudly so a caller asking for one gets a clear error instead of
    // a silently-dropped feature, matching the v2.4-only rejections on
    // the v2.3 path.
    if options.crc {
        return Err(Error::unsupported(
            "ID3v2.2 has no extended header; the CRC sub-field is v2.3+ only",
        ));
    }
    if options.footer {
        return Err(Error::unsupported(
            "ID3v2 footer is v2.4-only; cannot be written under a v2.2 target",
        ));
    }
    if options.is_update {
        return Err(Error::unsupported(
            "ID3v2 extended-header `is_update` flag is v2.4-only; cannot be written under a v2.2 target",
        ));
    }
    if options.restrictions.is_some() {
        return Err(Error::unsupported(
            "ID3v2 extended-header restrictions byte is v2.4-only; cannot be written under a v2.2 target",
        ));
    }
    if options.compress {
        return Err(Error::unsupported(
            "ID3v2.2 frame compression is undefined by the spec; cannot be written",
        ));
    }

    // PerFrame unsync has no v2.2 wire slot (there is no per-frame
    // flags byte at all), so it collapses to whole-tag unsync — the
    // same downgrade the v2.3 path applies.
    let whole_tag_unsync = matches!(options.unsync, UnsyncMode::WholeTag | UnsyncMode::PerFrame);

    let mut frame_bytes = Vec::new();
    for frame in &tag.frames {
        write_v22_frame(frame, &mut frame_bytes)?;
    }

    let mut body = frame_bytes;
    if whole_tag_unsync {
        body = apply_unsync(&body);
    }

    let size = body.len();
    if size >= 1 << 28 {
        return Err(Error::invalid(
            "ID3v2 tag body exceeds the 28-bit synchsafe size limit",
        ));
    }

    // Header flags: only bit 7 (unsync) is ever set; bit 6
    // (compression) is left clear since the scheme is undefined.
    let flags: u8 = if whole_tag_unsync { 0x80 } else { 0 };

    let s = size as u32;
    let mut out = Vec::with_capacity(ID3V2_HEADER_SIZE + size);
    out.extend_from_slice(b"ID3");
    out.push(2); // major version
    out.push(0); // revision
    out.push(flags);
    out.push(((s >> 21) & 0x7F) as u8);
    out.push(((s >> 14) & 0x7F) as u8);
    out.push(((s >> 7) & 0x7F) as u8);
    out.push((s & 0x7F) as u8);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Serialise a single frame under an ID3v2.2 envelope, appending the
/// six-byte header (3-char id + 3-byte BE size) plus payload to `out`.
/// Frames with no v2.2 equivalent are skipped (no bytes appended).
fn write_v22_frame(frame: &Id3Frame, out: &mut Vec<u8>) -> Result<()> {
    // Build the (v2.2 id, payload) pair. Everything except the
    // attached picture reuses the v2.3 payload encoder — the §4 frame
    // bodies are byte-identical between v2.2 and v2.3 (only the header
    // differs), which is exactly why the parser shares them. The PIC
    // frame is the one exception: v2.2 carries a fixed three-character
    // image-format code where v2.3's APIC carries a NUL-terminated
    // MIME string (spec §4.15).
    let (id22, payload): (&str, Vec<u8>) = match frame {
        Id3Frame::Picture(pic) => {
            // v2.2 §4.15 PIC: encoding + 3-char image format + picture
            // type + NUL-terminated description + binary data. v2.2
            // text encodings are only $00 (ISO-8859-1) and $01
            // (UCS-2); use $01 so non-ASCII descriptions survive.
            let enc: u8 = 1;
            let mut payload = Vec::new();
            payload.push(enc);
            payload.extend_from_slice(&mime_to_v22_image_format(&pic.mime_type));
            payload.push(pic.picture_type as u8);
            encode_string(&mut payload, enc, &pic.description);
            encode_terminator(&mut payload, enc);
            payload.extend_from_slice(&pic.data);
            ("PIC", payload)
        }
        Id3Frame::EncryptedMeta {
            owner,
            content,
            encrypted,
        } => {
            // v2.2 §4.20 CRM: owner identifier (ISO-8859-1, NUL) +
            // content/explanation (ISO-8859-1, NUL) + encrypted block.
            // No encoding byte — the frame predates one. This is the
            // serialiser counterpart of `parse_crm`.
            let mut payload = Vec::new();
            encode_latin1(&mut payload, owner);
            payload.push(0);
            encode_latin1(&mut payload, content);
            payload.push(0);
            payload.extend_from_slice(encrypted);
            ("CRM", payload)
        }
        Id3Frame::Unknown { id, raw } => {
            // An Unknown frame round-trips verbatim, but only if its id
            // is already a valid three-character v2.2 identifier. A
            // four-char id (parsed from v2.3/v2.4) has no v2.2 demotion
            // here unless it appears in `demote_to_v22`, so a frame the
            // structural parsers didn't recognise is dropped rather
            // than truncated to three bytes.
            match demote_to_v22(id) {
                Some(id22) => (id22, raw.clone()),
                None if is_valid_v22_id(id) => {
                    // Already a v2.2-shaped id (e.g. a v2.2 `CRM` the
                    // parser preserved verbatim). Emit it unchanged.
                    out.extend_from_slice(id.as_bytes());
                    push_v22_size(out, raw.len())?;
                    out.extend_from_slice(raw);
                    return Ok(());
                }
                None => return Ok(()),
            }
        }
        _ => {
            // Reuse the v2.3 encoder for the shared §4 bodies, then
            // demote the four-char id. encode_frame uses v2.3 text
            // conventions (UTF-16-with-BOM, '/' multi-value join) which
            // are valid v2.2 — encoding $01 and the '/' separator both
            // predate v2.2.
            let (id, payload) = encode_frame(Id3Version::V2_3, frame)?;
            match demote_to_v22(&id) {
                Some(id22) => (id22, payload),
                None => return Ok(()),
            }
        }
    };

    out.extend_from_slice(id22.as_bytes());
    push_v22_size(out, payload.len())?;
    out.extend_from_slice(&payload);
    Ok(())
}

/// Append a three-byte big-endian frame size to `out` (ID3v2.2 §3.2),
/// rejecting payloads that overflow the 24-bit field.
fn push_v22_size(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len > 0x00FF_FFFF {
        return Err(Error::invalid(
            "ID3v2.2 frame payload exceeds the 24-bit frame-size field",
        ));
    }
    let s = len as u32;
    out.push(((s >> 16) & 0xFF) as u8);
    out.push(((s >> 8) & 0xFF) as u8);
    out.push((s & 0xFF) as u8);
    Ok(())
}

/// True when `id` is a well-formed ID3v2.2 frame identifier — exactly
/// three characters, each a capital A–Z or digit 0–9 (spec §3.2).
fn is_valid_v22_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 3
        && b.iter()
            .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Map a MIME type to the three-character image-format code an ID3v2.2
/// PIC frame carries (spec §4.15: "Image format is preferably 'PNG'
/// [PNG] or 'JPG' [JFIF]"). The field is a fixed three bytes; the
/// common image types collapse onto the spec's two named codes and
/// anything else is upper-cased and padded/truncated to three bytes so
/// the slot is always exactly filled. This inverts the parser's
/// format→MIME mapping in [`parse_pic`].
fn mime_to_v22_image_format(mime: &str) -> [u8; 3] {
    let lower = mime.to_ascii_lowercase();
    let code: &str = match lower.as_str() {
        "image/jpeg" | "image/jpg" => "JPG",
        "image/png" => "PNG",
        other => other.strip_prefix("image/").unwrap_or(other),
    };
    let mut out = [b' '; 3];
    for (i, b) in code.bytes().take(3).enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    out
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
    compress: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let (id, mut payload) = encode_frame(version, frame)?;
    let mut id4 = [0u8; 4];
    let id_bytes = id.as_bytes();
    if id_bytes.len() != 4 || !id_bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::invalid(format!("invalid frame id for writer: {id}")));
    }
    id4.copy_from_slice(id_bytes);

    let mut format_flags: u8 = 0;
    // Frame-level compression (v2.3 §3.3 flag `i` / v2.4 §4.1.2 flag
    // `k`): deflate the payload and stash the decompressed size for
    // the version-specific header addition below. Compression runs
    // before per-frame unsync, mirroring the parse path which reverses
    // unsync before inflating.
    let mut size_prefix: Option<[u8; 4]> = None;
    if compress {
        let announced = payload.len();
        match version {
            Id3Version::V2_4 => {
                // The data-length indicator is a 32-bit synchsafe
                // integer, so the *decompressed* size must fit in 28
                // bits even if the deflated stream would be smaller.
                if announced >= 1 << 28 {
                    return Err(Error::invalid(
                        "v2.4 compressed frame's decompressed size exceeds synchsafe limit",
                    ));
                }
                let s = announced as u32;
                size_prefix = Some(synchsafe_bytes_u28(s));
                // §4.1.2: compression "requires the 'Data Length
                // Indicator' bit to be set as well".
                format_flags |= 0x08 | 0x01;
            }
            Id3Version::V2_3 => {
                // 4 regular big-endian bytes of decompressed size
                // "appended to the frame header" (§3.3 flag `i`). The
                // v2.3 format-flags byte carries compression in bit 7.
                size_prefix = Some((announced as u32).to_be_bytes());
                format_flags |= 0x80;
            }
            _ => unreachable!("validated in write_tag"),
        }
        payload = deflate_frame(&payload)?;
    }

    let apply_per_frame = per_frame_unsync && matches!(version, Id3Version::V2_4);
    if apply_per_frame {
        payload = apply_unsync(&payload);
        format_flags |= 0x02;
    }

    out.extend_from_slice(&id4);
    let size = payload.len() + size_prefix.map_or(0, |p| p.len());
    match version {
        Id3Version::V2_4 => {
            if size >= 1 << 28 {
                return Err(Error::invalid("v2.4 frame size exceeds synchsafe limit"));
            }
            out.extend_from_slice(&synchsafe_bytes_u28(size as u32));
        }
        Id3Version::V2_3 => {
            let s = size as u32;
            out.extend_from_slice(&s.to_be_bytes());
        }
        _ => unreachable!("validated in write_tag"),
    }
    // Status flags are always 0; the format flags collect the
    // compression / data-length / unsync bits set above.
    out.extend_from_slice(&[0, format_flags]);
    if let Some(prefix) = size_prefix {
        out.extend_from_slice(&prefix);
    }
    out.extend_from_slice(&payload);
    Ok(())
}

/// Encode a 28-bit value as the 4 synchsafe bytes used by v2.4 frame
/// sizes and data-length indicators. Caller must have range-checked
/// `v < 1 << 28`.
fn synchsafe_bytes_u28(v: u32) -> [u8; 4] {
    [
        ((v >> 21) & 0x7F) as u8,
        ((v >> 14) & 0x7F) as u8,
        ((v >> 7) & 0x7F) as u8,
        (v & 0x7F) as u8,
    ]
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
        Id3Frame::Rvad {
            increment_decrement,
            bits_used,
            front,
            back,
            center,
            bass,
        } => {
            // Spec v2.3 §4.12 — frame only exists in v2.3. v2.4
            // replaced it with `RVA2`, so emitting `RVAD` under a
            // v2.4 envelope is a write-time error (matching the
            // `with_footer` + `V2_3` rejection pattern). Callers that
            // want to write a relative volume adjustment into a v2.4
            // tag use the `Rva2` variant.
            if matches!(version, Id3Version::V2_4) {
                return Err(Error::unsupported(
                    "RVAD frame is v2.3-only; use Rva2 under V2_4",
                ));
            }
            // Top two bits of inc/dec are reserved %00 per spec.
            if increment_decrement & 0b1100_0000 != 0 {
                return Err(Error::invalid(
                    "RVAD increment_decrement reserved bits (top two) must be zero",
                ));
            }
            // Spec: "This value may not be $00."
            if *bits_used == 0 {
                return Err(Error::invalid(
                    "RVAD bits_used must be non-zero per spec §4.12",
                ));
            }
            let width = (*bits_used as usize).div_ceil(8);
            let front_present = increment_decrement & 0b0000_0011 != 0;
            let back_present = increment_decrement & 0b0000_1100 != 0;
            let center_present = increment_decrement & 0b0001_0000 != 0;
            let bass_present = increment_decrement & 0b0010_0000 != 0;
            // The inc/dec bitfield and the per-channel `Option`s must
            // agree, otherwise the round-trip parser+writer would
            // produce a different wire form than the caller asked for.
            // Reject the mismatch explicitly so the bug surfaces at
            // the call site instead of silently dropping data.
            if front_present != front.is_some() {
                return Err(Error::invalid(
                    "RVAD inc/dec front bits and `front` channel block disagree",
                ));
            }
            if back_present != back.is_some() {
                return Err(Error::invalid(
                    "RVAD inc/dec back bits and `back` channel block disagree",
                ));
            }
            if center_present != center.is_some() {
                return Err(Error::invalid(
                    "RVAD inc/dec centre bit and `center` channel disagree",
                ));
            }
            if bass_present != bass.is_some() {
                return Err(Error::invalid(
                    "RVAD inc/dec bass bit and `bass` channel disagree",
                ));
            }
            // Spec presents back/centre/bass as extensions appended
            // after the front pair. A higher-tier channel without
            // front channels is not constructible from a spec-
            // conforming stream; reject it so the writer never emits
            // a layout a reader couldn't reassemble.
            if !front_present && (back_present || center_present || bass_present) {
                return Err(Error::invalid(
                    "RVAD back/centre/bass channels require front channels per spec",
                ));
            }
            if !back_present && (center_present || bass_present) {
                return Err(Error::invalid(
                    "RVAD centre/bass channels require back channels per spec",
                ));
            }
            if !center_present && bass_present {
                return Err(Error::invalid(
                    "RVAD bass channel requires centre channel per spec",
                ));
            }
            let mut payload = Vec::new();
            payload.push(*increment_decrement);
            payload.push(*bits_used);
            // Helper: pad-or-validate a single magnitude/peak field to
            // the spec width and append to `payload`. Sub-spec-width
            // values are zero-padded on the high end per spec
            // ("padded in the beginning (highest bits) when 'bits
            // used for volume description' is not a multiple of
            // eight"); over-wide values are rejected since silently
            // truncating would change the value.
            fn append_field(
                payload: &mut Vec<u8>,
                bytes: &[u8],
                width: usize,
                what: &str,
            ) -> Result<()> {
                if bytes.len() > width {
                    return Err(Error::invalid(format!(
                        "RVAD {what} wider than bits_used field width"
                    )));
                }
                if bytes.len() < width {
                    let pad = width - bytes.len();
                    payload.resize(payload.len() + pad, 0);
                }
                payload.extend_from_slice(bytes);
                Ok(())
            }
            // Emit one block per spec: all deltas first, then all
            // peaks. A peak with `is_empty()` is the spec-legal
            // "completely omitted" form. The writer mirrors the
            // parser's all-or-nothing peak handling per block: if any
            // peak in a block is non-empty, all peaks in that block
            // are emitted (filled with the spec-width zero-pad
            // otherwise). This keeps the wire form unambiguous —
            // mixing present-and-omitted peaks within a block has no
            // spec layout.
            let emit_block = |payload: &mut Vec<u8>, chans: &[&RvadChannel]| -> Result<()> {
                for ch in chans {
                    append_field(payload, &ch.volume_delta, width, "volume_delta")?;
                }
                let any_peak = chans.iter().any(|c| !c.peak.is_empty());
                if any_peak {
                    for ch in chans {
                        // Empty peak in a partially-peaked block
                        // pads to zero (matches the parser's
                        // greedy-read symmetry).
                        append_field(payload, &ch.peak, width, "peak")?;
                    }
                }
                Ok(())
            };
            if let Some(f) = front {
                emit_block(&mut payload, &[&f.right, &f.left])?;
            }
            if let Some(b) = back {
                emit_block(&mut payload, &[&b.right_back, &b.left_back])?;
            }
            if let Some(c) = center {
                emit_block(&mut payload, &[c])?;
            }
            if let Some(b) = bass {
                emit_block(&mut payload, &[b])?;
            }
            Ok(("RVAD".to_string(), payload))
        }
        Id3Frame::Equa {
            adjustment_bits,
            bands,
        } => {
            // Spec v2.3 §4.13 — frame only exists in v2.3. v2.4 replaced
            // it with `EQU2`, so emitting `EQUA` under a v2.4 envelope
            // is a write-time error (matching the `RVAD` v2.3-only
            // contract). Callers that want a v2.4 equalisation curve
            // use the `Equ2` variant.
            if matches!(version, Id3Version::V2_4) {
                return Err(Error::unsupported(
                    "EQUA frame is v2.3-only; use Equ2 under V2_4",
                ));
            }
            // Spec: "This value may not be $00."
            if *adjustment_bits == 0 {
                return Err(Error::invalid(
                    "EQUA adjustment_bits must be non-zero per spec §4.13",
                ));
            }
            // Spec: "The equalisation bands should be ordered
            // increasingly with reference to frequency" and "A
            // frequency should only be described once in the frame".
            // Reject both violations at write time so the writer never
            // emits a stream a conforming reader would have to
            // re-sort to interpret.
            for pair in bands.windows(2) {
                if pair[0].frequency >= pair[1].frequency {
                    return Err(Error::invalid(
                        "EQUA bands must be sorted strictly increasing by frequency per spec",
                    ));
                }
            }
            let width = (*adjustment_bits as usize).div_ceil(8);
            let mut payload = Vec::new();
            payload.push(*adjustment_bits);
            for band in bands {
                if band.frequency & 0x8000 != 0 {
                    return Err(Error::invalid(
                        "EQUA band frequency exceeds 15-bit range (collides with inc/dec bit)",
                    ));
                }
                if band.adjustment.len() > width {
                    return Err(Error::invalid(
                        "EQUA band adjustment wider than adjustment_bits field width",
                    ));
                }
                let mut high = ((band.frequency >> 8) & 0x7F) as u8;
                if band.increment {
                    high |= 0x80;
                }
                let low = (band.frequency & 0xFF) as u8;
                payload.push(high);
                payload.push(low);
                // Sub-spec-width adjustments zero-pad at the high end
                // per spec "padded in the beginning (highest bits) when
                // 'bits used for volume description' is not a multiple
                // of eight".
                if band.adjustment.len() < width {
                    let pad = width - band.adjustment.len();
                    payload.resize(payload.len() + pad, 0);
                }
                payload.extend_from_slice(&band.adjustment);
            }
            Ok(("EQUA".to_string(), payload))
        }
        Id3Frame::Ipls { pairs } => {
            // Spec v2.3 §4.4 — frame only exists in v2.3. v2.4 replaced
            // it with the `TIPL` text frame (involved people list) and
            // the new `TMCL` musician credits list. Both are ordinary
            // text frames the existing `Id3Frame::Text` variant
            // handles, so emitting `IPLS` under a v2.4 envelope is a
            // write-time error (matching the `RVAD` / `EQUA` v2.3-only
            // contract). Callers that want a v2.4 involved-people list
            // build an `Id3Frame::Text { id: "TIPL", values: … }` with
            // the spec's role/name pairing flattened into the text
            // value string.
            if matches!(version, Id3Version::V2_4) {
                return Err(Error::unsupported(
                    "IPLS frame is v2.3-only; use TIPL text frame under V2_4",
                ));
            }
            let mut payload = Vec::new();
            payload.push(text_enc);
            for (involvement, involvee) in pairs {
                encode_string(&mut payload, text_enc, involvement);
                encode_terminator(&mut payload, text_enc);
                encode_string(&mut payload, text_enc, involvee);
                encode_terminator(&mut payload, text_enc);
            }
            Ok(("IPLS".to_string(), payload))
        }
        Id3Frame::EncryptedMeta { .. } => {
            // CRM (§4.20) exists only in ID3v2.2 — v2.3 replaced it with
            // ENCR + per-frame encryption flags, and v2.4 kept that
            // model. Emitting CRM under a v2.3/v2.4 envelope would be a
            // malformed frame, so refuse here. The v2.2 writer
            // (`write_v22_frame`) handles this variant directly; this
            // arm is only ever hit for a non-v2.2 target (matching the
            // RVAD-under-v2.4 rejection pattern).
            Err(Error::unsupported(
                "CRM (encrypted meta) frame is ID3v2.2-only; v2.3+ uses ENCR + per-frame encryption",
            ))
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
// v2.3 <-> v2.4 frame-level conversion
// ---------------------------------------------------------------------------

/// Convert an [`Id3Tag`] between the ID3v2.3 and ID3v2.4 frame
/// vocabularies, rewriting the handful of frames whose *identity* (not
/// just their on-wire encoding) changed between the two versions.
///
/// `write_tag` already re-encodes a frame body for the target version —
/// it switches text encodings, picks the right multi-value separator,
/// emits the right frame-header layout. What it does **not** do is
/// rename or restructure a frame whose entire *meaning* moved to a
/// different frame id across the version boundary. The date frames are
/// the canonical example: ID3v2.3 splits the recording date across the
/// separate `TYER` / `TDAT` / `TIME` numeric-string frames (spec v2.3
/// §4.2.1), while ID3v2.4 folds all three into a single `TDRC`
/// "Recording time" ISO 8601 timestamp (spec v2.4 §4.2.5, format defined
/// in the structure document). A v2.3 tag handed straight to
/// `write_tag(_, V2_4)` would keep emitting `TYER`/`TDAT`/`TIME` ids — a
/// conformant v2.4 reader does not recognise those. `convert_tag`
/// bridges that gap.
///
/// The conversion is a pure re-encoding of spec-defined fields into
/// other spec-defined fields; no frame is invented and nothing outside
/// the staged ID3 spec informs the mapping. The frames it rewrites:
///
/// **v2.3 → v2.4**
/// * `TYER` (year, `yyyy`) — optionally combined with `TDAT` (`DDMM`)
///   and `TIME` (`HHMM`) — folds into one `TDRC` timestamp at the
///   highest precision the source provides. A bare `TYER` yields a
///   `yyyy` timestamp; adding `TDAT` extends to `yyyy-MM-dd`; adding
///   `TIME` extends to `yyyy-MM-ddTHH:mm`. The day/month/time parts are
///   only folded in when the year itself is a well-formed four-digit
///   value, since the timestamp grammar is anchored on the year; a
///   malformed `TYER` is preserved verbatim and the `TDAT`/`TIME`
///   companions are dropped (they have no standalone v2.4 home).
/// * `TORY` (original release year, formatted as `TYER`) → `TDOR`
///   (original release time) at year precision.
/// * `IPLS` (involved people list) → `TIPL` text frame carrying the
///   same `(role, name)` pairs as alternating NUL-separated values.
/// * `TRDA` (recording dates, a free-text complement to the numeric
///   date frames) and `TSIZ` (audio size in bytes) have no v2.4
///   successor — the spec dropped both — so they are removed.
///
/// **v2.4 → v2.3**
/// * `TDRC` → `TYER` plus `TDAT`/`TIME` for whatever precision the
///   timestamp carried (a year-only timestamp yields just `TYER`; a
///   day-precision one adds `TDAT`; a minute-or-finer one adds `TIME`).
/// * `TDOR` → `TORY` (year only — `TORY` cannot carry finer precision).
/// * `TIPL` → `IPLS` carrying the same pairs.
/// * `TDEN` / `TDRL` / `TDTG` (encoding / release / tagging time) and
///   `TMCL` (musician credits) have no v2.3 successor and are removed.
///
/// Every other frame is carried through unchanged (the version-specific
/// body re-encoding is `write_tag`'s job). Converting to the version a
/// tag already declares returns a clone with `version` set to the
/// target. A v2.2 or v1 source/target is rejected with
/// [`Error::unsupported`]: this bridge is specifically the v2.3↔v2.4
/// frame-vocabulary delta. (v2.2→v2.3 promotion already happens on
/// parse, where three-char ids are lifted to their four-char
/// descendants.)
pub fn convert_tag(tag: &Id3Tag, target_version: Id3Version) -> Result<Id3Tag> {
    match (tag.version, target_version) {
        (Id3Version::V2_3, Id3Version::V2_4) => Ok(Id3Tag {
            version: Id3Version::V2_4,
            frames: convert_frames_v23_to_v24(&tag.frames),
        }),
        (Id3Version::V2_4, Id3Version::V2_3) => Ok(Id3Tag {
            version: Id3Version::V2_3,
            frames: convert_frames_v24_to_v23(&tag.frames),
        }),
        (Id3Version::V2_3, Id3Version::V2_3) | (Id3Version::V2_4, Id3Version::V2_4) => Ok(Id3Tag {
            version: target_version,
            frames: tag.frames.clone(),
        }),
        _ => Err(Error::unsupported(
            "convert_tag bridges ID3v2.3 <-> ID3v2.4 only",
        )),
    }
}

/// Find the single value of a `T***` text frame by id, if present and
/// non-empty. Returns the first value of the first matching frame.
fn first_text_value<'a>(frames: &'a [Id3Frame], id: &str) -> Option<&'a str> {
    frames.iter().find_map(|f| match f {
        Id3Frame::Text { id: fid, values } if fid == id => values.first().map(|s| s.as_str()),
        _ => None,
    })
}

/// Build the `TDRC`/`TDOR` ISO 8601 timestamp string for the precision
/// the components provide. `year` is mandatory; each finer field is
/// appended only when the coarser ones are all present (the grammar
/// never skips a level). `second` is included only when the source
/// carried it. Two-digit zero padding throughout.
fn format_timestamp(
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
    hour: Option<u8>,
    minute: Option<u8>,
    second: Option<u8>,
) -> String {
    let mut out = format!("{year:04}");
    if let Some(mo) = month {
        out.push_str(&format!("-{mo:02}"));
        if let Some(d) = day {
            out.push_str(&format!("-{d:02}"));
            if let Some(h) = hour {
                out.push_str(&format!("T{h:02}"));
                if let Some(mi) = minute {
                    out.push_str(&format!(":{mi:02}"));
                    if let Some(s) = second {
                        out.push_str(&format!(":{s:02}"));
                    }
                }
            }
        }
    }
    out
}

/// Rewrite a v2.3 frame list into the v2.4 vocabulary per the
/// `convert_tag` mapping.
fn convert_frames_v23_to_v24(frames: &[Id3Frame]) -> Vec<Id3Frame> {
    // Pre-scan the date companions so the TYER fold can reach them.
    let tdat = first_text_value(frames, "TDAT").map(DayMonth::from_field);
    let time = first_text_value(frames, "TIME").map(HourMinute::from_field);

    let mut out = Vec::with_capacity(frames.len());
    for frame in frames {
        match frame {
            Id3Frame::Text { id, values } if id == "TYER" => {
                // Fold TYER (+ TDAT + TIME) into a single TDRC timestamp.
                match values.first().map(|v| Id3Year::from_field(v)) {
                    Some(Id3Year::Year(year)) => {
                        let (month, day) = match &tdat {
                            Some(DayMonth::DayMonth { day, month }) => (Some(*month), Some(*day)),
                            _ => (None, None),
                        };
                        // Time only applies when a date is present (the
                        // timestamp grammar requires day precision before
                        // a time component can appear).
                        let (hour, minute) = match (&day, &time) {
                            (Some(_), Some(HourMinute::HourMinute { hour, minute })) => {
                                (Some(*hour), Some(*minute))
                            }
                            _ => (None, None),
                        };
                        let ts = format_timestamp(year, month, day, hour, minute, None);
                        out.push(Id3Frame::Text {
                            id: "TDRC".to_string(),
                            values: vec![ts],
                        });
                    }
                    // A malformed or absent year cannot anchor a
                    // timestamp; preserve the raw TYER under its own id so
                    // no data is silently dropped.
                    _ => out.push(frame.clone()),
                }
            }
            // TDAT / TIME were consumed into TDRC above when TYER was a
            // valid year; otherwise they have no v2.4 home. Drop them
            // here regardless — a standalone TDAT/TIME with no parseable
            // TYER cannot form a valid timestamp.
            Id3Frame::Text { id, .. } if id == "TDAT" || id == "TIME" => {}
            Id3Frame::Text { id, values } if id == "TORY" => {
                match values.first().map(|v| Id3Year::from_field(v)) {
                    Some(Id3Year::Year(year)) => out.push(Id3Frame::Text {
                        id: "TDOR".to_string(),
                        values: vec![format!("{year:04}")],
                    }),
                    _ => out.push(frame.clone()),
                }
            }
            // TRDA (free-text recording-dates complement) and TSIZ
            // (audio size) were dropped in v2.4 with no successor frame.
            Id3Frame::Text { id, .. } if id == "TRDA" || id == "TSIZ" => {}
            Id3Frame::Ipls { pairs } => {
                // IPLS -> TIPL: same (role, name) pairs as a text frame's
                // alternating NUL-separated values.
                out.push(Id3Frame::Text {
                    id: "TIPL".to_string(),
                    values: flatten_pairs(pairs),
                });
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// Rewrite a v2.4 frame list into the v2.3 vocabulary per the
/// `convert_tag` mapping.
fn convert_frames_v24_to_v23(frames: &[Id3Frame]) -> Vec<Id3Frame> {
    let mut out = Vec::with_capacity(frames.len());
    for frame in frames {
        match frame {
            Id3Frame::Text { id, values } if id == "TDRC" => {
                // Split the TDRC timestamp back into TYER (+ TDAT + TIME).
                match values.first().map(|v| Id3Timestamp::from_field(v)) {
                    Some(Id3Timestamp::DateTime {
                        year,
                        month,
                        day,
                        hour,
                        minute,
                        ..
                    }) => {
                        out.push(Id3Frame::Text {
                            id: "TYER".to_string(),
                            values: vec![format!("{year:04}")],
                        });
                        if let (Some(mo), Some(d)) = (month, day) {
                            out.push(Id3Frame::Text {
                                id: "TDAT".to_string(),
                                values: vec![format!("{d:02}{mo:02}")],
                            });
                        }
                        if let (Some(h), Some(mi)) = (hour, minute) {
                            out.push(Id3Frame::Text {
                                id: "TIME".to_string(),
                                values: vec![format!("{h:02}{mi:02}")],
                            });
                        }
                    }
                    // A malformed timestamp has no clean v2.3 split;
                    // preserve the raw TDRC so no data is dropped.
                    _ => out.push(frame.clone()),
                }
            }
            Id3Frame::Text { id, values } if id == "TDOR" => {
                match values.first().map(|v| Id3Timestamp::from_field(v)) {
                    Some(Id3Timestamp::DateTime { year, .. }) => out.push(Id3Frame::Text {
                        id: "TORY".to_string(),
                        values: vec![format!("{year:04}")],
                    }),
                    _ => out.push(frame.clone()),
                }
            }
            // Encoding / release / tagging time and musician credits have
            // no v2.3 successor frame.
            Id3Frame::Text { id, .. }
                if id == "TDEN" || id == "TDRL" || id == "TDTG" || id == "TMCL" => {}
            Id3Frame::Text { id, values } if id == "TIPL" => {
                // TIPL -> IPLS: same (role, name) pairs.
                out.push(Id3Frame::Ipls {
                    pairs: pair_alternating(values),
                });
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// Flatten `(a, b)` pairs into the alternating `[a0, b0, a1, b1, ...]`
/// value list a `TIPL`/`TMCL` text frame stores. The inverse of
/// [`pair_alternating`].
fn flatten_pairs(pairs: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::with_capacity(pairs.len() * 2);
    for (a, b) in pairs {
        out.push(a.clone());
        out.push(b.clone());
    }
    out
}

impl Id3Tag {
    /// Ergonomic wrapper over [`convert_tag`]: returns a copy of this tag
    /// rewritten into the `target_version` frame vocabulary. See
    /// [`convert_tag`] for the exact frame mapping and version support.
    pub fn to_version(&self, target_version: Id3Version) -> Result<Id3Tag> {
        convert_tag(self, target_version)
    }
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

    /// Assemble a synthetic ID3v2.2.0 tag (spec §3.1 header + §3.2
    /// frame headers: 3-char id + 3-byte BE size, no frame flags)
    /// from `(id, payload)` pairs. `header_flags` is the §3.1 flags
    /// byte (`%xx000000`).
    fn build_v22_tag(header_flags: u8, frames: &[(&[u8; 3], &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, payload) in frames {
            body.extend_from_slice(*id);
            let size = payload.len() as u32;
            body.push(((size >> 16) & 0xFF) as u8);
            body.push(((size >> 8) & 0xFF) as u8);
            body.push((size & 0xFF) as u8);
            body.extend_from_slice(payload);
        }
        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(2); // major
        tag.push(0); // revision
        tag.push(header_flags);
        let s = body.len() as u32;
        tag.push(((s >> 21) & 0x7F) as u8);
        tag.push(((s >> 14) & 0x7F) as u8);
        tag.push(((s >> 7) & 0x7F) as u8);
        tag.push((s & 0x7F) as u8);
        tag.extend_from_slice(&body);
        tag
    }

    /// The common v2.2 text frames (§4.2.1) + COM (§4.11) walk onto
    /// the typed surface and the Vorbis-style key/value projection
    /// under their promoted 4-char ids.
    #[test]
    fn v22_common_frames_to_kv() {
        let tt2 = [&[0u8][..], b"A Title"].concat();
        let tp1 = [&[0u8][..], b"An Artist"].concat();
        let tal = [&[0u8][..], b"An Album"].concat();
        let trk = [&[0u8][..], b"4/9"].concat();
        let tye = [&[0u8][..], b"1998"].concat();
        // COM: enc + lang + short description $00 + text (§4.11).
        let mut com = vec![0u8];
        com.extend_from_slice(b"eng");
        com.push(0);
        com.extend_from_slice(b"a comment");
        let tag = build_v22_tag(
            0,
            &[
                (b"TT2", &tt2),
                (b"TP1", &tp1),
                (b"TAL", &tal),
                (b"TRK", &trk),
                (b"TYE", &tye),
                (b"COM", &com),
            ],
        );
        let (parsed, consumed) = parse_tag(&tag).unwrap();
        assert_eq!(consumed, tag.len());
        assert_eq!(parsed.version, Id3Version::V2_2);
        assert_eq!(parsed.frames.len(), 6);
        let kv = to_key_value_pairs(&parsed);
        assert!(kv.contains(&("title".to_string(), "A Title".to_string())));
        assert!(kv.contains(&("artist".to_string(), "An Artist".to_string())));
        assert!(kv.contains(&("album".to_string(), "An Album".to_string())));
        assert!(kv.contains(&("track".to_string(), "4/9".to_string())));
        assert!(kv.contains(&("date".to_string(), "1998".to_string())));
        assert!(kv.contains(&("comment".to_string(), "a comment".to_string())));
    }

    /// v2.2 §4.1 UFI / §4.17 CNT / §4.18 POP share their v2.3
    /// descendants' payload layout and land in the typed variants.
    #[test]
    fn v22_ufi_cnt_pop() {
        let ufi = [&b"db@example\0"[..], b"\x01\x02\x03"].concat();
        // CNT with a 5-byte (grown) counter per §4.17.
        let cnt = [0x01u8, 0x00, 0x00, 0x00, 0x02];
        let pop = [&b"who@example\0"[..], &[196u8], &[0, 0, 0, 7]].concat();
        let tag = build_v22_tag(0, &[(b"UFI", &ufi), (b"CNT", &cnt), (b"POP", &pop)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        assert_eq!(parsed.frames.len(), 3);
        match &parsed.frames[0] {
            Id3Frame::Ufid { owner, identifier } => {
                assert_eq!(owner, "db@example");
                assert_eq!(identifier, &[1, 2, 3]);
            }
            other => panic!("expected Ufid from v2.2 UFI, got {other:?}"),
        }
        match &parsed.frames[1] {
            Id3Frame::PlayCounter { count } => assert_eq!(*count, 0x01_0000_0002),
            other => panic!("expected PlayCounter from v2.2 CNT, got {other:?}"),
        }
        match &parsed.frames[2] {
            Id3Frame::Popularimeter {
                email,
                rating,
                counter,
            } => {
                assert_eq!(email, "who@example");
                assert_eq!(*rating, 196);
                assert_eq!(*counter, 7);
            }
            other => panic!("expected Popularimeter from v2.2 POP, got {other:?}"),
        }
    }

    /// v2.2 §4.16 GEO and §4.5 MCI map onto `Geob` / `MusicCdId`.
    #[test]
    fn v22_geo_mci() {
        let mut geo = vec![0u8];
        geo.extend_from_slice(b"text/plain\0");
        geo.extend_from_slice(b"notes.txt\0");
        geo.extend_from_slice(b"some notes\0");
        geo.extend_from_slice(b"PAYLOAD");
        let mci = b"\x00\x04TOCDATA1";
        let tag = build_v22_tag(0, &[(b"GEO", &geo), (b"MCI", &mci[..])]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::Geob {
                mime_type,
                filename,
                description,
                data,
            } => {
                assert_eq!(mime_type, "text/plain");
                assert_eq!(filename, "notes.txt");
                assert_eq!(description, "some notes");
                assert_eq!(data, b"PAYLOAD");
            }
            other => panic!("expected Geob from v2.2 GEO, got {other:?}"),
        }
        match &parsed.frames[1] {
            Id3Frame::MusicCdId { toc } => assert_eq!(toc, &mci[..]),
            other => panic!("expected MusicCdId from v2.2 MCI, got {other:?}"),
        }
    }

    /// v2.2 §4.6 ETC / §4.8 STC / §4.10 SLT carry the same payload
    /// shapes as ETCO / SYTC / SYLT.
    #[test]
    fn v22_etc_stc_slt() {
        // ETC: time format $02 (ms), one "intro start" ($02) event at 1500ms.
        let etc = [2u8, 0x02, 0x00, 0x00, 0x05, 0xDC];
        // STC: time format $02, tempo $7B (123 BPM) at 0ms, then the
        // two-byte $FF+$0A extension form (265 BPM) at 2000ms.
        let stc = [
            2u8, 0x7B, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x0A, 0x00, 0x00, 0x07, 0xD0,
        ];
        // SLT: enc 0, lang "eng", time fmt $02, content type $01
        // (lyrics), empty descriptor, one synced syllable.
        let mut slt = vec![0u8];
        slt.extend_from_slice(b"eng");
        slt.push(2);
        slt.push(1);
        slt.push(0); // empty content descriptor
        slt.extend_from_slice(b"Strang\0");
        slt.extend_from_slice(&[0x00, 0x00, 0x01, 0xF4]);
        let tag = build_v22_tag(0, &[(b"ETC", &etc), (b"STC", &stc), (b"SLT", &slt)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::EventTimingCodes {
                time_format,
                events,
            } => {
                assert_eq!(*time_format, 2);
                assert_eq!(events, &[(0x02u8, 1500u32)]);
            }
            other => panic!("expected EventTimingCodes from v2.2 ETC, got {other:?}"),
        }
        match &parsed.frames[1] {
            Id3Frame::SyncedTempo { time_format, codes } => {
                assert_eq!(*time_format, 2);
                assert_eq!(codes, &[(123u16, 0u32), (265u16, 2000u32)]);
            }
            other => panic!("expected SyncedTempo from v2.2 STC, got {other:?}"),
        }
        match &parsed.frames[2] {
            Id3Frame::SyncedLyrics {
                lang,
                time_format,
                content_type,
                description,
                syncs,
            } => {
                assert_eq!(lang, b"eng");
                assert_eq!(*time_format, 2);
                assert_eq!(*content_type, 1);
                assert!(description.is_empty());
                assert_eq!(syncs, &[("Strang".to_string(), 500u32)]);
            }
            other => panic!("expected SyncedLyrics from v2.2 SLT, got {other:?}"),
        }
    }

    /// v2.2 §4.4 IPL maps onto `Ipls` and the `involved_people()`
    /// accessor.
    #[test]
    fn v22_ipl() {
        let mut ipl = vec![0u8];
        ipl.extend_from_slice(b"producer\0Alice\0engineer\0Bob\0");
        let tag = build_v22_tag(0, &[(b"IPL", &ipl)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            f @ Id3Frame::Ipls { pairs } => {
                assert_eq!(
                    pairs,
                    &[
                        ("producer".to_string(), "Alice".to_string()),
                        ("engineer".to_string(), "Bob".to_string()),
                    ]
                );
                assert_eq!(f.involved_people().unwrap().len(), 2);
            }
            other => panic!("expected Ipls from v2.2 IPL, got {other:?}"),
        }
    }

    /// v2.2 §4.19 BUF (offset-to-next-tag omitted per "This field may
    /// be omitted") and §4.21 CRA map onto `RecommendedBuffer` /
    /// `AudioEncryption`.
    #[test]
    fn v22_buf_cra() {
        let buf = [0x00u8, 0x10, 0x00, 0x01]; // 4096-byte buffer, embedded info
        let cra = [&b"scheme@example\0"[..], &[0, 5, 0, 9], b"KEYDATA"].concat();
        let tag = build_v22_tag(0, &[(b"BUF", &buf), (b"CRA", &cra)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::RecommendedBuffer {
                buffer_size,
                embedded_info,
                offset_to_next,
            } => {
                assert_eq!(*buffer_size, 0x1000);
                assert!(*embedded_info);
                assert_eq!(*offset_to_next, 0);
            }
            other => panic!("expected RecommendedBuffer from v2.2 BUF, got {other:?}"),
        }
        match &parsed.frames[1] {
            Id3Frame::AudioEncryption {
                owner,
                preview_start,
                preview_length,
                encryption_info,
            } => {
                assert_eq!(owner, "scheme@example");
                assert_eq!(*preview_start, 5);
                assert_eq!(*preview_length, 9);
                assert_eq!(encryption_info, b"KEYDATA");
            }
            other => panic!("expected AudioEncryption from v2.2 CRA, got {other:?}"),
        }
    }

    /// v2.2 §4.7 MLL shares MLLT's descriptor + bit-packed reference
    /// layout.
    #[test]
    fn v22_mll() {
        let mut mll = Vec::new();
        mll.extend_from_slice(&2u16.to_be_bytes()); // frames between refs
        mll.extend_from_slice(&[0x00, 0x04, 0x00]); // bytes between refs
        mll.extend_from_slice(&[0x00, 0x00, 0x1A]); // ms between refs
        mll.push(8); // bits for bytes deviation
        mll.push(8); // bits for ms deviation
        mll.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // two references
        let tag = build_v22_tag(0, &[(b"MLL", &mll)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::MpegLocationLookup {
                mpeg_frames_between_reference,
                bytes_between_reference,
                ms_between_reference,
                bits_for_bytes_deviation,
                bits_for_ms_deviation,
                references,
            } => {
                assert_eq!(*mpeg_frames_between_reference, 2);
                assert_eq!(*bytes_between_reference, 0x0400);
                assert_eq!(*ms_between_reference, 0x1A);
                assert_eq!(*bits_for_bytes_deviation, 8);
                assert_eq!(*bits_for_ms_deviation, 8);
                assert_eq!(references, &[(0x12, 0x34), (0x56, 0x78)]);
            }
            other => panic!("expected MpegLocationLookup from v2.2 MLL, got {other:?}"),
        }
    }

    /// v2.2 §4.12 RVA lists the right/left volume-change fields
    /// unconditionally — a both-decrement frame (inc/dec `$00`) still
    /// carries both magnitudes, unlike v2.3 RVAD's presence-gated
    /// front block.
    #[test]
    fn v22_rva_both_decrement_keeps_front_block() {
        let rva = [
            0x00u8, // inc/dec: both channels decrement
            0x10,   // 16 bits per field
            0x01, 0x00, // right delta
            0x02, 0x00, // left delta
            0x7F, 0xFF, // right peak
            0x7E, 0x00, // left peak
        ];
        let tag = build_v22_tag(0, &[(b"RVA", &rva)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::Rvad {
                increment_decrement,
                bits_used,
                front,
                back,
                center,
                bass,
            } => {
                assert_eq!(*increment_decrement, 0);
                assert_eq!(*bits_used, 16);
                let front = front.as_ref().expect("v2.2 RVA front block");
                assert_eq!(front.right.volume_delta, vec![0x01, 0x00]);
                assert_eq!(front.left.volume_delta, vec![0x02, 0x00]);
                assert_eq!(front.right.peak, vec![0x7F, 0xFF]);
                assert_eq!(front.left.peak, vec![0x7E, 0x00]);
                assert!(back.is_none() && center.is_none() && bass.is_none());
            }
            other => panic!("expected Rvad from v2.2 RVA, got {other:?}"),
        }
    }

    /// v2.2 §4.12 RVA with the peak fields "completely omitted"
    /// surfaces empty peaks.
    #[test]
    fn v22_rva_omitted_peaks() {
        let rva = [
            0x03u8, // both channels increment
            0x08,   // 8 bits per field
            0x05,   // right delta
            0x06,   // left delta
        ];
        let tag = build_v22_tag(0, &[(b"RVA", &rva)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::Rvad { front, .. } => {
                let front = front.as_ref().expect("v2.2 RVA front block");
                assert_eq!(front.right.volume_delta, vec![0x05]);
                assert_eq!(front.left.volume_delta, vec![0x06]);
                assert!(front.right.peak.is_empty());
                assert!(front.left.peak.is_empty());
            }
            other => panic!("expected Rvad from v2.2 RVA, got {other:?}"),
        }
    }

    /// v2.2 §4.22 LNK always carries a 3-byte linked frame id; a URL
    /// whose first byte is an uppercase id-class character must not be
    /// folded into the identifier (the v2.3/v2.4 heuristic would).
    #[test]
    fn v22_lnk_three_byte_id_uppercase_url() {
        let lnk = [&b"TAL"[..], b"FTP://example/tag.bin\0", b"extra"].concat();
        let tag = build_v22_tag(0, &[(b"LNK", &lnk)]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::LinkedInfo {
                frame_id,
                url,
                additional,
            } => {
                assert_eq!(frame_id, b"TAL\0");
                assert_eq!(url, "FTP://example/tag.bin");
                assert_eq!(additional, b"extra");
            }
            other => panic!("expected LinkedInfo from v2.2 LNK, got {other:?}"),
        }
    }

    /// v2.2 §4.20 CRM has no v2.3/v2.4 descendant but its structure is
    /// fully specified: owner identifier + content/explanation + the
    /// encrypted datablock. The parser types those fields and preserves
    /// the encrypted block verbatim (no decryption attempted).
    #[test]
    fn v22_crm_typed_decode() {
        let crm = b"plugin@example\0why it is locked\0CIPHERTEXT";
        let tag = build_v22_tag(0, &[(b"CRM", &crm[..])]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::EncryptedMeta {
                owner,
                content,
                encrypted,
            } => {
                assert_eq!(owner, "plugin@example");
                assert_eq!(content, "why it is locked");
                assert_eq!(encrypted, b"CIPHERTEXT");
            }
            other => panic!("expected EncryptedMeta from v2.2 CRM, got {other:?}"),
        }
    }

    /// A CRM frame round-trips through the v2.2 writer byte-for-byte:
    /// decode → re-encode → decode yields identical structural fields,
    /// and the serialised bytes match the original frame payload.
    #[test]
    fn v22_crm_roundtrip() {
        let crm = b"owner@org.example\0protected artwork\0\x01\x02\x00\xFF\x00data";
        let tag = build_v22_tag(0, &[(b"CRM", &crm[..])]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        // Re-encode under a v2.2 target and re-parse.
        let written = write_tag(&parsed, Id3Version::V2_2).unwrap();
        let (reparsed, _) = parse_tag(&written).unwrap();
        assert_eq!(reparsed.frames.len(), 1);
        match (&parsed.frames[0], &reparsed.frames[0]) {
            (
                Id3Frame::EncryptedMeta {
                    owner: o1,
                    content: c1,
                    encrypted: e1,
                },
                Id3Frame::EncryptedMeta {
                    owner: o2,
                    content: c2,
                    encrypted: e2,
                },
            ) => {
                assert_eq!(o1, o2);
                assert_eq!(c1, c2);
                assert_eq!(e1, e2);
                assert_eq!(o2, "owner@org.example");
                assert_eq!(c2, "protected artwork");
                assert_eq!(e2, b"\x01\x02\x00\xFF\x00data");
            }
            other => panic!("expected matching EncryptedMeta pair, got {other:?}"),
        }
    }

    /// CRM is ID3v2.2-only: writing an `EncryptedMeta` frame under a
    /// v2.3 or v2.4 target is rejected (no on-wire slot), mirroring the
    /// RVAD-under-v2.4 rejection.
    #[test]
    fn crm_rejected_under_v23_v24() {
        let frame = Id3Frame::EncryptedMeta {
            owner: "x@y".to_string(),
            content: "c".to_string(),
            encrypted: vec![1, 2, 3],
        };
        assert!(encode_frame(Id3Version::V2_3, &frame).is_err());
        assert!(encode_frame(Id3Version::V2_4, &frame).is_err());
        // The v2.2 encoder accepts it.
        let (id, payload) = encode_frame_v22_only(&frame);
        assert_eq!(id, "CRM");
        assert_eq!(payload, b"x@y\0c\0\x01\x02\x03");
    }

    /// Helper: exercise the v2.2 CRM serialiser directly.
    fn encode_frame_v22_only(frame: &Id3Frame) -> (String, Vec<u8>) {
        let mut buf = Vec::new();
        write_v22_frame(frame, &mut buf).unwrap();
        // buf = 3-char id + 3-byte size + payload
        let id = String::from_utf8(buf[0..3].to_vec()).unwrap();
        let payload = buf[6..].to_vec();
        (id, payload)
    }

    /// A CRM whose payload omits the second NUL terminator: per the
    /// structural parser, everything after the owner terminator folds
    /// into `content` and the encrypted block is empty rather than the
    /// frame erroring. The owner is still recovered correctly.
    #[test]
    fn v22_crm_missing_second_terminator() {
        let crm = b"owner@x\0just a description with no second NUL";
        let tag = build_v22_tag(0, &[(b"CRM", &crm[..])]);
        let (parsed, _) = parse_tag(&tag).unwrap();
        match &parsed.frames[0] {
            Id3Frame::EncryptedMeta {
                owner,
                content,
                encrypted,
            } => {
                assert_eq!(owner, "owner@x");
                assert_eq!(content, "just a description with no second NUL");
                assert!(encrypted.is_empty());
            }
            other => panic!("expected EncryptedMeta, got {other:?}"),
        }
    }

    /// Empty owner and content with a non-empty encrypted block survive
    /// a byte-exact v2.2 round-trip (the two leading bytes are bare
    /// NULs).
    #[test]
    fn v22_crm_empty_strings_roundtrip() {
        let frame = Id3Frame::EncryptedMeta {
            owner: String::new(),
            content: String::new(),
            encrypted: vec![0xAA, 0xBB, 0xCC],
        };
        let (id, payload) = encode_frame_v22_only(&frame);
        assert_eq!(id, "CRM");
        assert_eq!(payload, vec![0u8, 0u8, 0xAA, 0xBB, 0xCC]);
        // Re-parse the body to confirm symmetry.
        match parse_v22_frame_body("CRM", &payload) {
            Id3Frame::EncryptedMeta {
                owner,
                content,
                encrypted,
            } => {
                assert!(owner.is_empty());
                assert!(content.is_empty());
                assert_eq!(encrypted, vec![0xAA, 0xBB, 0xCC]);
            }
            other => panic!("expected EncryptedMeta, got {other:?}"),
        }
    }

    /// The encrypted block may contain `$FF $00` byte pairs; a CRM frame
    /// composes with whole-tag unsynchronisation so those bytes survive
    /// the write → parse round-trip unmodified.
    #[test]
    fn v22_crm_unsync_roundtrip() {
        let tag = Id3Tag {
            version: Id3Version::V2_2,
            frames: vec![Id3Frame::EncryptedMeta {
                owner: "o@e".to_string(),
                content: "c".to_string(),
                encrypted: vec![0xFF, 0x00, 0xFF, 0xFB, 0x90],
            }],
        };
        let opts = WriteOptions::new().with_unsync(UnsyncMode::WholeTag);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_2, &opts).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        match &parsed.frames[0] {
            Id3Frame::EncryptedMeta {
                owner,
                content,
                encrypted,
            } => {
                assert_eq!(owner, "o@e");
                assert_eq!(content, "c");
                assert_eq!(encrypted, &vec![0xFF, 0x00, 0xFF, 0xFB, 0x90]);
            }
            other => panic!("expected EncryptedMeta, got {other:?}"),
        }
    }

    /// Test-only shim: route a payload through the v2.2 frame-body
    /// dispatcher so the round-trip assertions above can re-parse a
    /// payload they just encoded.
    fn parse_v22_frame_body(id: &str, payload: &[u8]) -> Id3Frame {
        let mut tag = build_v22_tag(0, &[]);
        // Splice a frame of `id` + payload in before the padding.
        let header_len = ID3V2_HEADER_SIZE;
        let mut body = Vec::new();
        body.extend_from_slice(id.as_bytes());
        body.push(((payload.len() >> 16) & 0xFF) as u8);
        body.push(((payload.len() >> 8) & 0xFF) as u8);
        body.push((payload.len() & 0xFF) as u8);
        body.extend_from_slice(payload);
        let s = body.len() as u32;
        tag.truncate(header_len);
        tag[6] = ((s >> 21) & 0x7F) as u8;
        tag[7] = ((s >> 14) & 0x7F) as u8;
        tag[8] = ((s >> 7) & 0x7F) as u8;
        tag[9] = (s & 0x7F) as u8;
        tag.extend_from_slice(&body);
        let (parsed, _) = parse_tag(&tag).unwrap();
        parsed.frames.into_iter().next().unwrap()
    }

    /// v2.2 §3.1: a set compression bit (header flag bit 6) means the
    /// decoder "should just ignore the entire tag" — no frames, but
    /// the consumed size still spans the whole tag so a container
    /// caller can seek past it.
    #[test]
    fn v22_compression_bit_skips_tag() {
        let tt2 = [&[0u8][..], b"Hidden"].concat();
        let tag = build_v22_tag(0x40, &[(b"TT2", &tt2)]);
        let (parsed, consumed) = parse_tag(&tag).unwrap();
        assert_eq!(parsed.version, Id3Version::V2_2);
        assert!(parsed.frames.is_empty());
        assert_eq!(consumed, tag.len());
        let (parsed2, ext, consumed2) = parse_tag_with_extended_header(&tag).unwrap();
        assert!(parsed2.frames.is_empty());
        assert_eq!(consumed2, tag.len());
        assert!(!ext.is_update && ext.crc.is_none() && ext.restrictions.is_none());
    }

    /// Whole-tag unsynchronisation (v2.2 §5) composes with the v2.2
    /// frame walker: a 3-byte frame size whose payload contains
    /// `$FF $00` pairs is recovered after the reversal.
    #[test]
    fn v22_unsync_whole_tag() {
        // One CNT frame whose counter ($00 00 FF FE = 0xFFFE) contains
        // a false-sync $FF byte that the §5 scheme escapes on the wire.
        let cnt_payload = [0x00u8, 0x00, 0xFF, 0xFE];
        // Unsynchronised body: insert $00 after each $FF.
        let mut body = Vec::new();
        body.extend_from_slice(b"CNT");
        body.extend_from_slice(&[0x00, 0x00, cnt_payload.len() as u8]);
        body.extend_from_slice(&cnt_payload);
        let mut unsynced = Vec::new();
        for &b in &body {
            unsynced.push(b);
            if b == 0xFF {
                unsynced.push(0x00);
            }
        }
        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(2);
        tag.push(0);
        tag.push(0x80); // unsynchronisation flag
        let s = unsynced.len() as u32;
        tag.push(((s >> 21) & 0x7F) as u8);
        tag.push(((s >> 14) & 0x7F) as u8);
        tag.push(((s >> 7) & 0x7F) as u8);
        tag.push((s & 0x7F) as u8);
        tag.extend_from_slice(&unsynced);
        let (parsed, consumed) = parse_tag(&tag).unwrap();
        assert_eq!(consumed, tag.len());
        match &parsed.frames[0] {
            Id3Frame::PlayCounter { count } => assert_eq!(*count, 0xFFFE),
            other => panic!("expected PlayCounter from unsynced v2.2 CNT, got {other:?}"),
        }
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

    /// `RVAD` front-only writer pinned to a hand-computed byte
    /// sequence. Spec v2.3 §4.12: with bits_used = 0x10 (16 bits)
    /// each delta is 2 bytes BE and each peak is 2 bytes BE. The
    /// inc/dec byte 0x03 sets bits 0 + 1, meaning both front channels
    /// present and both deltas positive (increment).
    #[test]
    fn rvad_writer_pinned_bytes_front_only() {
        let frame = Id3Frame::Rvad {
            increment_decrement: 0b0000_0011,
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x12, 0x34],
                    peak: vec![0x56, 0x78],
                },
                left: RvadChannel {
                    volume_delta: vec![0xAB, 0xCD],
                    peak: vec![0xEF, 0x01],
                },
            }),
            back: None,
            center: None,
            bass: None,
        };
        let (id, payload) = encode_frame(Id3Version::V2_3, &frame).unwrap();
        assert_eq!(id, "RVAD");
        // Spec §4.12 layout: inc/dec, bits_used, then per block: all
        // deltas (right, left) followed by all peaks (right, left).
        assert_eq!(
            payload,
            vec![0x03, 0x10, 0x12, 0x34, 0xAB, 0xCD, 0x56, 0x78, 0xEF, 0x01]
        );
    }

    /// `RVAD` round-trip with both front and back channels. The back
    /// pair is appended after the front pair on the wire (spec §4.12).
    #[test]
    fn rvad_roundtrip_front_and_back() {
        let original = Id3Frame::Rvad {
            // bits 0,1 (front: right inc, left inc) + bits 2,3 (back:
            // right-back inc, left-back dec).
            increment_decrement: 0b0000_0111,
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
                left: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
            }),
            back: Some(RvadBackChannels {
                right_back: RvadChannel {
                    volume_delta: vec![0x00, 0x10],
                    peak: vec![0x00, 0x20],
                },
                left_back: RvadChannel {
                    volume_delta: vec![0x00, 0x08],
                    peak: vec![0x00, 0x10],
                },
            }),
            center: None,
            bass: None,
        };
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![original],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        match &parsed.frames[0] {
            Id3Frame::Rvad {
                increment_decrement,
                bits_used,
                front,
                back,
                center,
                bass,
            } => {
                assert_eq!(*increment_decrement, 0b0000_0111);
                assert_eq!(*bits_used, 16);
                let f = front.as_ref().expect("front");
                assert_eq!(f.right.volume_delta, vec![0x00, 0x40]);
                assert_eq!(f.right.peak, vec![0x00, 0x80]);
                assert_eq!(f.left.volume_delta, vec![0x00, 0x40]);
                assert_eq!(f.left.peak, vec![0x00, 0x80]);
                let b = back.as_ref().expect("back");
                assert_eq!(b.right_back.volume_delta, vec![0x00, 0x10]);
                assert_eq!(b.right_back.peak, vec![0x00, 0x20]);
                assert_eq!(b.left_back.volume_delta, vec![0x00, 0x08]);
                assert_eq!(b.left_back.peak, vec![0x00, 0x10]);
                assert!(center.is_none());
                assert!(bass.is_none());
            }
            other => panic!("expected Rvad, got {other:?}"),
        }
    }

    /// Centre + bass extensions round-trip on top of front + back per
    /// the spec's appended-block layout. Bit 4 = centre, bit 5 = bass.
    #[test]
    fn rvad_roundtrip_all_six_channels() {
        let original = Id3Frame::Rvad {
            increment_decrement: 0b0011_1111,
            bits_used: 8, // single-byte deltas to keep the wire tight
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x11],
                    peak: vec![0x21],
                },
                left: RvadChannel {
                    volume_delta: vec![0x12],
                    peak: vec![0x22],
                },
            }),
            back: Some(RvadBackChannels {
                right_back: RvadChannel {
                    volume_delta: vec![0x13],
                    peak: vec![0x23],
                },
                left_back: RvadChannel {
                    volume_delta: vec![0x14],
                    peak: vec![0x24],
                },
            }),
            center: Some(RvadChannel {
                volume_delta: vec![0x15],
                peak: vec![0x25],
            }),
            bass: Some(RvadChannel {
                volume_delta: vec![0x16],
                peak: vec![0x26],
            }),
        };
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![original],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        match &parsed.frames[0] {
            Id3Frame::Rvad {
                increment_decrement,
                bits_used,
                front,
                back,
                center,
                bass,
            } => {
                assert_eq!(*increment_decrement, 0b0011_1111);
                assert_eq!(*bits_used, 8);
                let f = front.as_ref().expect("front");
                assert_eq!(f.right.volume_delta, vec![0x11]);
                assert_eq!(f.right.peak, vec![0x21]);
                assert_eq!(f.left.volume_delta, vec![0x12]);
                assert_eq!(f.left.peak, vec![0x22]);
                let b = back.as_ref().expect("back");
                assert_eq!(b.right_back.volume_delta, vec![0x13]);
                assert_eq!(b.right_back.peak, vec![0x23]);
                assert_eq!(b.left_back.volume_delta, vec![0x14]);
                assert_eq!(b.left_back.peak, vec![0x24]);
                let c = center.as_ref().expect("center");
                assert_eq!(c.volume_delta, vec![0x15]);
                assert_eq!(c.peak, vec![0x25]);
                let ba = bass.as_ref().expect("bass");
                assert_eq!(ba.volume_delta, vec![0x16]);
                assert_eq!(ba.peak, vec![0x26]);
            }
            other => panic!("expected Rvad, got {other:?}"),
        }
    }

    /// Spec: "if no other data follows, [the peak fields] could be
    /// left zeroed or, if no other data follows, be completely
    /// omitted." Front-only with peaks omitted means a 6-byte payload
    /// (2-byte preamble + 2 × 2-byte delta) — no peak bytes on the
    /// wire. The parser surfaces `peak.is_empty()`, and the writer
    /// reproduces the same minimal form.
    #[test]
    fn rvad_peak_omitted_minimal_wire() {
        // Hand-rolled minimal payload: inc/dec=0x03 (front right+left,
        // both increment), bits=16, two 2-byte deltas, no peaks.
        let payload = vec![0x03, 0x10, 0x00, 0x40, 0x00, 0x40];
        let parsed = parse_rvad(&payload);
        match &parsed {
            Id3Frame::Rvad { front, .. } => {
                let f = front.as_ref().expect("front block");
                assert_eq!(f.right.volume_delta, vec![0x00, 0x40]);
                assert!(f.right.peak.is_empty(), "peak omitted");
                assert_eq!(f.left.volume_delta, vec![0x00, 0x40]);
                assert!(f.left.peak.is_empty(), "peak omitted");
            }
            other => panic!("expected Rvad, got {other:?}"),
        }
        // Re-encode and confirm the writer reproduces the 6-byte form.
        let (id, re) = encode_frame(Id3Version::V2_3, &parsed).unwrap();
        assert_eq!(id, "RVAD");
        assert_eq!(re, payload);
    }

    /// Sub-byte `bits_used` widths round up to whole bytes per spec
    /// ("padded in the beginning (highest bits) when 'bits used for
    /// volume description' is not a multiple of eight"). bits=12
    /// gives 2-byte fields with the top 4 bits zero.
    #[test]
    fn rvad_padded_subbyte_width() {
        let frame = Id3Frame::Rvad {
            increment_decrement: 0b0000_0011,
            bits_used: 12,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x0F, 0xFF],
                    peak: vec![0x0F, 0xFF],
                },
                left: RvadChannel {
                    volume_delta: vec![0x00, 0x01],
                    peak: vec![0x00, 0x01],
                },
            }),
            back: None,
            center: None,
            bass: None,
        };
        let (_, payload) = encode_frame(Id3Version::V2_3, &frame).unwrap();
        // 2 preamble + 4 × 2 bytes
        assert_eq!(payload.len(), 2 + 4 * 2);
        match parse_rvad(&payload[..]) {
            Id3Frame::Rvad {
                bits_used, front, ..
            } => {
                assert_eq!(bits_used, 12);
                let f = front.as_ref().expect("front");
                assert_eq!(f.right.volume_delta, vec![0x0F, 0xFF]);
                assert_eq!(f.right.peak, vec![0x0F, 0xFF]);
                assert_eq!(f.left.volume_delta, vec![0x00, 0x01]);
                assert_eq!(f.left.peak, vec![0x00, 0x01]);
            }
            other => panic!("expected Rvad, got {other:?}"),
        }
    }

    /// Spec gating: `bits_used = $00` is reserved per §4.12 ("This
    /// value may not be $00"); the writer rejects it rather than
    /// emitting a degenerate stream.
    #[test]
    fn rvad_writer_rejects_zero_bits() {
        let frame = Id3Frame::Rvad {
            increment_decrement: 0b0000_0011,
            bits_used: 0,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: Vec::new(),
                    peak: Vec::new(),
                },
                left: RvadChannel {
                    volume_delta: Vec::new(),
                    peak: Vec::new(),
                },
            }),
            back: None,
            center: None,
            bass: None,
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("bits_used"));
    }

    /// `RVAD` is v2.3-only — v2.4 dropped it for `RVA2`. Asking the
    /// writer to emit one under a V2_4 envelope must error rather
    /// than silently producing a frame v2.4 readers wouldn't parse.
    #[test]
    fn rvad_writer_rejects_v24() {
        let frame = Id3Frame::Rvad {
            increment_decrement: 0b0000_0011,
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
                left: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
            }),
            back: None,
            center: None,
            bass: None,
        };
        let err = encode_frame(Id3Version::V2_4, &frame).unwrap_err();
        assert!(format!("{err}").contains("v2.3-only"));
    }

    /// The inc/dec bitfield and the per-channel `Option` slots must
    /// stay aligned. Passing a `Some(back)` without setting any of
    /// bits 2 / 3 is a wire-form mismatch — the writer rejects it
    /// rather than silently dropping the block or fabricating bits.
    #[test]
    fn rvad_writer_rejects_block_bitfield_mismatch() {
        let frame = Id3Frame::Rvad {
            // Front bits set; back bits NOT set.
            increment_decrement: 0b0000_0011,
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: Vec::new(),
                },
                left: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: Vec::new(),
                },
            }),
            back: Some(RvadBackChannels {
                right_back: RvadChannel {
                    volume_delta: vec![0x00, 0x10],
                    peak: Vec::new(),
                },
                left_back: RvadChannel {
                    volume_delta: vec![0x00, 0x10],
                    peak: Vec::new(),
                },
            }),
            center: None,
            bass: None,
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("back"));
    }

    /// A short payload (< 2 bytes — not even the inc/dec + bits_used
    /// pair) preserves the raw bytes through `Unknown` since there's
    /// no spec-defined fallback layout. Mirrors the `RVRB` short-form
    /// behaviour.
    #[test]
    fn rvad_short_payload_surfaces_unknown() {
        for short in [&[][..], &[0x03][..]] {
            match parse_rvad(short) {
                Id3Frame::Unknown { id, raw } => {
                    assert_eq!(id, "RVAD");
                    assert_eq!(raw, short);
                }
                other => panic!("expected Unknown for short RVAD, got {other:?}"),
            }
        }
    }

    /// `RVAD` carries DSP descriptors, not text values — it should
    /// not surface in `to_key_value_pairs`, matching the
    /// `RVA2`/`Reverb` precedent.
    #[test]
    fn rvad_yields_no_key_value_pairs() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Rvad {
                increment_decrement: 0b0000_0011,
                bits_used: 16,
                front: Some(RvadFrontChannels {
                    right: RvadChannel {
                        volume_delta: vec![0x00, 0x40],
                        peak: Vec::new(),
                    },
                    left: RvadChannel {
                        volume_delta: vec![0x00, 0x40],
                        peak: Vec::new(),
                    },
                }),
                back: None,
                center: None,
                bass: None,
            }],
        };
        assert!(to_key_value_pairs(&tag).is_empty());
    }

    /// `EQUA` two-band writer pinned to hand-computed bytes. Spec v2.3
    /// §4.13: `adjustment_bits = 0x10` (16 bits) gives 2-byte BE
    /// adjustments. Each band is `inc<<7 | freq_high(7), freq_low,
    /// adj_high, adj_low`. Band 0 is `freq = 0x0100 (256 Hz)` with
    /// `increment` set; band 1 is `freq = 0x4000 (16384 Hz)` with
    /// `increment` cleared.
    #[test]
    fn equa_writer_pinned_bytes_two_bands() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![
                EquaBand {
                    increment: true,
                    frequency: 0x0100,
                    adjustment: vec![0x12, 0x34],
                },
                EquaBand {
                    increment: false,
                    frequency: 0x4000,
                    adjustment: vec![0xAB, 0xCD],
                },
            ],
        };
        let (id, payload) = encode_frame(Id3Version::V2_3, &frame).unwrap();
        assert_eq!(id, "EQUA");
        // adjustment_bits, then for each band: inc/freq-high, freq-low,
        // adjustment bytes BE.
        // Band 0: inc=1, freq=0x0100 → high = 0x81, low = 0x00.
        // Band 1: inc=0, freq=0x4000 → high = 0x40, low = 0x00.
        assert_eq!(
            payload,
            vec![0x10, 0x81, 0x00, 0x12, 0x34, 0x40, 0x00, 0xAB, 0xCD]
        );
    }

    /// `EQUA` round-trip with multiple bands at the spec-norm 16-bit
    /// adjustment width. Bands are emitted in ascending frequency
    /// order and round-trip preserves the inc/dec flag, the 15-bit
    /// frequency, and the full adjustment magnitude.
    #[test]
    fn equa_roundtrip_multi_band_16bit() {
        let original = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![
                EquaBand {
                    increment: true,
                    frequency: 100,
                    adjustment: vec![0x00, 0x80],
                },
                EquaBand {
                    increment: false,
                    frequency: 1000,
                    adjustment: vec![0x01, 0x00],
                },
                EquaBand {
                    increment: true,
                    frequency: 10000,
                    adjustment: vec![0x02, 0x00],
                },
                EquaBand {
                    increment: false,
                    frequency: 0x7FFF,
                    adjustment: vec![0xFF, 0xFF],
                },
            ],
        };
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![original.clone()],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        match &parsed.frames[0] {
            Id3Frame::Equa {
                adjustment_bits,
                bands,
            } => {
                assert_eq!(*adjustment_bits, 16);
                assert_eq!(bands.len(), 4);
                assert!(bands[0].increment);
                assert_eq!(bands[0].frequency, 100);
                assert_eq!(bands[0].adjustment, vec![0x00, 0x80]);
                assert!(!bands[3].increment);
                assert_eq!(bands[3].frequency, 0x7FFF);
                assert_eq!(bands[3].adjustment, vec![0xFF, 0xFF]);
            }
            other => panic!("expected Equa, got {other:?}"),
        }
    }

    /// `EQUA` sub-byte adjustment width round-trip: `adjustment_bits =
    /// 12` rounds up to 2 bytes per spec "padded in the beginning
    /// (highest bits)", and the wire layout preserves that width
    /// faithfully across writer + parser.
    #[test]
    fn equa_roundtrip_sub_byte_width_12bit() {
        let original = Id3Frame::Equa {
            adjustment_bits: 12,
            bands: vec![
                EquaBand {
                    increment: true,
                    frequency: 500,
                    adjustment: vec![0x0A, 0xBC],
                },
                EquaBand {
                    increment: false,
                    frequency: 5000,
                    adjustment: vec![0x01, 0x23],
                },
            ],
        };
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![original],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        match &parsed.frames[0] {
            Id3Frame::Equa {
                adjustment_bits,
                bands,
            } => {
                assert_eq!(*adjustment_bits, 12);
                assert_eq!(bands.len(), 2);
                assert_eq!(bands[0].adjustment, vec![0x0A, 0xBC]);
                assert_eq!(bands[1].adjustment, vec![0x01, 0x23]);
            }
            other => panic!("expected Equa, got {other:?}"),
        }
    }

    /// Writer rejects `adjustment_bits = $00` per spec "This value may
    /// not be $00".
    #[test]
    fn equa_writer_rejects_zero_adjustment_bits() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 0,
            bands: vec![EquaBand {
                increment: true,
                frequency: 100,
                adjustment: Vec::new(),
            }],
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("adjustment_bits"));
    }

    /// Writer rejects emission under `V2_4` since EQUA was dropped in
    /// favour of EQU2 in v2.4. Mirrors the `RVAD` v2.3-only contract.
    #[test]
    fn equa_writer_rejects_v24() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![EquaBand {
                increment: true,
                frequency: 100,
                adjustment: vec![0x00, 0x80],
            }],
        };
        let err = encode_frame(Id3Version::V2_4, &frame).unwrap_err();
        assert!(format!("{err}").contains("v2.3-only"));
    }

    /// Writer rejects out-of-order bands per spec "ordered increasingly
    /// with reference to frequency".
    #[test]
    fn equa_writer_rejects_unsorted_bands() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![
                EquaBand {
                    increment: true,
                    frequency: 1000,
                    adjustment: vec![0x00, 0x80],
                },
                EquaBand {
                    increment: true,
                    frequency: 100,
                    adjustment: vec![0x00, 0x80],
                },
            ],
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("sorted"));
    }

    /// Writer rejects duplicate frequencies per spec "A frequency
    /// should only be described once in the frame". The sort check
    /// uses strictly-increasing so a duplicate trips the same gate.
    #[test]
    fn equa_writer_rejects_duplicate_frequencies() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![
                EquaBand {
                    increment: true,
                    frequency: 1000,
                    adjustment: vec![0x00, 0x80],
                },
                EquaBand {
                    increment: false,
                    frequency: 1000,
                    adjustment: vec![0x00, 0x40],
                },
            ],
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("sorted"));
    }

    /// Writer rejects a frequency that overflows the 15-bit on-wire
    /// field (top bit collides with the inc/dec flag).
    #[test]
    fn equa_writer_rejects_frequency_overflow() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![EquaBand {
                increment: true,
                frequency: 0x8000,
                adjustment: vec![0x00, 0x80],
            }],
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("15-bit"));
    }

    /// Writer rejects an adjustment wider than `ceil(adjustment_bits / 8)`
    /// since silently truncating would change the magnitude.
    #[test]
    fn equa_writer_rejects_over_wide_adjustment() {
        let frame = Id3Frame::Equa {
            adjustment_bits: 8,
            bands: vec![EquaBand {
                increment: true,
                frequency: 100,
                adjustment: vec![0x12, 0x34],
            }],
        };
        let err = encode_frame(Id3Version::V2_3, &frame).unwrap_err();
        assert!(format!("{err}").contains("wider than"));
    }

    /// An empty payload preserves the raw bytes through `Unknown`
    /// since there's no spec-defined fallback layout. Mirrors the
    /// `RVAD` / `RVRB` short-form behaviour.
    #[test]
    fn equa_empty_payload_surfaces_unknown() {
        match parse_equa(&[]) {
            Id3Frame::Unknown { id, raw } => {
                assert_eq!(id, "EQUA");
                assert!(raw.is_empty());
            }
            other => panic!("expected Unknown for empty EQUA, got {other:?}"),
        }
    }

    /// A trailing band whose adjustment is short of the declared width
    /// is dropped — the inc/freq bytes are consumed but the band is
    /// not emitted. Bands that fit are returned in wire order.
    #[test]
    fn equa_short_trailing_band_dropped() {
        // adjustment_bits = 16 → 2-byte adjustments. One complete
        // band (freq 100, inc, adj 0x0080) then a stray inc/freq pair
        // without enough trailing adjustment bytes.
        let payload = vec![0x10, 0x80, 0x64, 0x00, 0x80, 0x00, 0xC8, 0x00];
        match parse_equa(&payload) {
            Id3Frame::Equa {
                adjustment_bits,
                bands,
            } => {
                assert_eq!(adjustment_bits, 16);
                assert_eq!(bands.len(), 1);
                assert!(bands[0].increment);
                assert_eq!(bands[0].frequency, 100);
                assert_eq!(bands[0].adjustment, vec![0x00, 0x80]);
            }
            other => panic!("expected Equa, got {other:?}"),
        }
    }

    /// `EQUA` carries DSP descriptors, not text values — it should not
    /// surface in `to_key_value_pairs`, matching the `EQU2` / `RVAD`
    /// precedent.
    #[test]
    fn equa_yields_no_key_value_pairs() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Equa {
                adjustment_bits: 16,
                bands: vec![EquaBand {
                    increment: true,
                    frequency: 100,
                    adjustment: vec![0x00, 0x80],
                }],
            }],
        };
        assert!(to_key_value_pairs(&tag).is_empty());
    }

    /// A `v2.2` `EQU` payload should dispatch through `parse_equa` and
    /// surface as an `Equa` variant — same wire layout as the v2.3
    /// frame, just with the 3-char id that v2.2 used.
    #[test]
    fn equa_v22_dispatch_promotes_to_equa() {
        // adjustment_bits = 8 (so 1 byte per adjustment), one band
        // freq = 256 with increment, adjustment = 0x40.
        let payload = vec![0x08, 0x81, 0x00, 0x40];
        let got = dispatch_v22("EQU", &payload);
        match got {
            Id3Frame::Equa {
                adjustment_bits,
                bands,
            } => {
                assert_eq!(adjustment_bits, 8);
                assert_eq!(bands.len(), 1);
                assert!(bands[0].increment);
                assert_eq!(bands[0].frequency, 256);
                assert_eq!(bands[0].adjustment, vec![0x40]);
            }
            other => panic!("expected Equa from v2.2 EQU, got {other:?}"),
        }
    }

    /// `v22_promote` resolves the 3-char `EQU` id to `EQUA` so a
    /// caller-facing id is consistent with v2.3.
    #[test]
    fn equa_v22_promotion() {
        assert_eq!(v22_promote("EQU"), "EQUA");
    }

    // ----- IPLS (spec v2.3 §4.4) -----

    /// `IPLS` writer pinned bytes: encoding-1 (UTF-16 BOM-LE) with two
    /// pairs. Each string is UTF-16-LE with a BOM and a 2-byte NUL
    /// terminator, so the wire layout is exact and version-independent
    /// for a given encoding byte. We pin it so a future writer change
    /// surfaces at the bit level rather than only at the round-trip
    /// level.
    #[test]
    fn ipls_writer_pinned_bytes_v23_utf16() {
        let frame = Id3Frame::Ipls {
            pairs: vec![("producer".to_string(), "Alice".to_string())],
        };
        let (id, payload) = encode_frame(Id3Version::V2_3, &frame).unwrap();
        assert_eq!(id, "IPLS");
        // encoding=1, then "producer\0" (UTF-16 BOM-LE + double-NUL),
        // then "Alice\0" (UTF-16 BOM-LE + double-NUL).
        let mut expected = vec![0x01];
        // "producer" in UTF-16-LE with BOM, then double-NUL terminator.
        expected.extend_from_slice(&[0xFF, 0xFE]);
        for ch in "producer".encode_utf16() {
            expected.extend_from_slice(&ch.to_le_bytes());
        }
        expected.extend_from_slice(&[0x00, 0x00]);
        expected.extend_from_slice(&[0xFF, 0xFE]);
        for ch in "Alice".encode_utf16() {
            expected.extend_from_slice(&ch.to_le_bytes());
        }
        expected.extend_from_slice(&[0x00, 0x00]);
        assert_eq!(payload, expected);
    }

    /// `IPLS` round-trip with two pairs through the latin1 encoding
    /// path (encoding byte 0). Round-tripping through the writer + the
    /// parser preserves both the role and the name strings byte-for-byte.
    #[test]
    fn ipls_parser_handles_latin1_two_pairs() {
        // encoding=0 (latin1), then NUL-terminated pairs.
        let mut payload = vec![0x00];
        payload.extend_from_slice(b"producer\0Alice\0mixing engineer\0Bob\0");
        let got = parse_ipls(&payload);
        match got {
            Id3Frame::Ipls { pairs } => {
                assert_eq!(
                    pairs,
                    vec![
                        ("producer".to_string(), "Alice".to_string()),
                        ("mixing engineer".to_string(), "Bob".to_string()),
                    ]
                );
            }
            other => panic!("expected Ipls, got {other:?}"),
        }
    }

    /// `IPLS` round-trip through `write_tag` → `parse_tag` at the
    /// public API layer pins that the v2.3 envelope writes and parses
    /// the frame without losing pair data.
    #[test]
    fn ipls_roundtrip_v23() {
        let original = Id3Frame::Ipls {
            pairs: vec![
                ("producer".to_string(), "Alice Bloggs".to_string()),
                ("guitar".to_string(), "Bob Smith".to_string()),
                ("vocals".to_string(), "Carol Jones".to_string()),
            ],
        };
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![original.clone()],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        match &parsed.frames[0] {
            Id3Frame::Ipls { pairs } => {
                assert_eq!(pairs.len(), 3);
                assert_eq!(pairs[0].0, "producer");
                assert_eq!(pairs[0].1, "Alice Bloggs");
                assert_eq!(pairs[1].0, "guitar");
                assert_eq!(pairs[1].1, "Bob Smith");
                assert_eq!(pairs[2].0, "vocals");
                assert_eq!(pairs[2].1, "Carol Jones");
            }
            other => panic!("expected Ipls after round-trip, got {other:?}"),
        }
    }

    /// A trailing involvement with no involvee folds into a pair with
    /// an empty involvee instead of being silently dropped, surfacing
    /// a non-conforming source without crashing.
    #[test]
    fn ipls_parser_folds_dangling_involvement() {
        let mut payload = vec![0x00];
        // First pair fully present, second pair is just an involvement
        // with no terminator (non-conforming source).
        payload.extend_from_slice(b"producer\0Alice\0lyricist");
        let got = parse_ipls(&payload);
        match got {
            Id3Frame::Ipls { pairs } => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0], ("producer".to_string(), "Alice".to_string()));
                // The dangling "lyricist" without a NUL is treated as
                // the involvement of a pair whose involvee is empty.
                assert_eq!(pairs[1], ("lyricist".to_string(), String::new()));
            }
            other => panic!("expected Ipls, got {other:?}"),
        }
    }

    /// An empty payload surfaces as `Unknown` so the wire bytes
    /// round-trip untouched — matches the spec-required encoding byte
    /// being mandatory and the `EQUA` empty-payload behaviour.
    #[test]
    fn ipls_empty_payload_surfaces_unknown() {
        match parse_ipls(&[]) {
            Id3Frame::Unknown { id, raw } => {
                assert_eq!(id, "IPLS");
                assert!(raw.is_empty());
            }
            other => panic!("expected Unknown for empty IPLS, got {other:?}"),
        }
    }

    /// A payload that's *only* the encoding byte parses to an empty
    /// pair list — the spec says the pair list "follows" the encoding
    /// byte but doesn't forbid zero pairs.
    #[test]
    fn ipls_parser_encoding_byte_only_yields_empty() {
        match parse_ipls(&[0x00]) {
            Id3Frame::Ipls { pairs } => assert!(pairs.is_empty()),
            other => panic!("expected Ipls with empty pairs, got {other:?}"),
        }
    }

    /// `IPLS` is v2.3-only. Emitting it under a `V2_4` envelope must
    /// fail rather than producing a frame v2.4 readers would not
    /// understand (v2.4 dropped `IPLS` in favour of the `TIPL` text
    /// frame).
    #[test]
    fn ipls_writer_rejects_v24() {
        let frame = Id3Frame::Ipls {
            pairs: vec![("producer".to_string(), "Alice".to_string())],
        };
        let err = encode_frame(Id3Version::V2_4, &frame).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("v2.3"));
    }

    /// `IPLS` carries pair-wise text descriptors that don't fit the
    /// flat key/value model `to_key_value_pairs` exposes (a single role
    /// can repeat — two producers, multiple guitarists, etc.). It
    /// should not surface there, matching the `Equa` / `Equ2` / `Rvad`
    /// precedent for structurally non-text frames.
    #[test]
    fn ipls_yields_no_key_value_pairs() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Ipls {
                pairs: vec![("producer".to_string(), "Alice".to_string())],
            }],
        };
        assert!(to_key_value_pairs(&tag).is_empty());
    }

    /// Empty pair list writes only the encoding byte and re-parses to
    /// an empty pair list — pins the zero-pair round-trip invariant.
    #[test]
    fn ipls_empty_pair_list_roundtrip() {
        let frame = Id3Frame::Ipls { pairs: Vec::new() };
        let (id, payload) = encode_frame(Id3Version::V2_3, &frame).unwrap();
        assert_eq!(id, "IPLS");
        // Just the encoding byte (1 = UTF-16 BOM in v2.3 default).
        assert_eq!(payload, vec![0x01]);
        match parse_ipls(&payload) {
            Id3Frame::Ipls { pairs } => assert!(pairs.is_empty()),
            other => panic!("expected empty Ipls, got {other:?}"),
        }
    }

    /// `to_key_value_pairs` surfaces the v2.4 §4.2.5 timestamp-class
    /// text frames the prior table dropped (TDEN encoding time, TDTG
    /// tagging time). Without an explicit mapping these would fall
    /// through the generic-id branch and land as `tden` / `tdtg`,
    /// which a Vorbis-style consumer cannot interpret.
    #[test]
    fn to_key_value_pairs_surfaces_v24_timestamp_frames() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Text {
                    id: "TDEN".into(),
                    values: vec!["2026-06-03T10:15:00".into()],
                },
                Id3Frame::Text {
                    id: "TDTG".into(),
                    values: vec!["2026-06-03T11:00:00".into()],
                },
            ],
        };
        let kv = to_key_value_pairs(&tag);
        assert!(kv
            .iter()
            .any(|(k, v)| k == "encodingtime" && v == "2026-06-03T10:15:00"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "taggingtime" && v == "2026-06-03T11:00:00"));
    }

    /// §4.2.3 informational frames new (TMOO) and previously unmapped
    /// (TFLT file-type, TLEN length-in-ms) round-trip to readable keys.
    #[test]
    fn to_key_value_pairs_surfaces_v24_informational_frames() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Text {
                    id: "TMOO".into(),
                    values: vec!["Mellow".into()],
                },
                Id3Frame::Text {
                    id: "TFLT".into(),
                    values: vec!["MPG/3".into()],
                },
                Id3Frame::Text {
                    id: "TLEN".into(),
                    values: vec!["240000".into()],
                },
            ],
        };
        let kv = to_key_value_pairs(&tag);
        assert!(kv.iter().any(|(k, v)| k == "mood" && v == "Mellow"));
        assert!(kv.iter().any(|(k, v)| k == "filetype" && v == "MPG/3"));
        assert!(kv.iter().any(|(k, v)| k == "length" && v == "240000"));
    }

    /// §4.2.4 rights / owner / internet-radio frames map to descriptive
    /// keys — `owner`, `producednotice`, `radiostation`,
    /// `radiostationowner`.
    #[test]
    fn to_key_value_pairs_surfaces_v24_rights_and_radio_frames() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Text {
                    id: "TOWN".into(),
                    values: vec!["Some Licensee".into()],
                },
                Id3Frame::Text {
                    id: "TPRO".into(),
                    values: vec!["2026 Producer Inc.".into()],
                },
                Id3Frame::Text {
                    id: "TRSN".into(),
                    values: vec!["Echelle Radio".into()],
                },
                Id3Frame::Text {
                    id: "TRSO".into(),
                    values: vec!["Echelle Inc.".into()],
                },
            ],
        };
        let kv = to_key_value_pairs(&tag);
        assert!(kv.iter().any(|(k, v)| k == "owner" && v == "Some Licensee"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "producednotice" && v == "2026 Producer Inc."));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "radiostation" && v == "Echelle Radio"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "radiostationowner" && v == "Echelle Inc."));
    }

    /// §4.2.5 sort-order frames (TSOA / TSOP / TSOT) and §4.2.1 set
    /// subtitle (TSST), plus the previously-unmapped §4.2.5 TDLY
    /// playlist delay and TOFN original filename.
    #[test]
    fn to_key_value_pairs_surfaces_v24_sort_and_aux_frames() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Text {
                    id: "TSOA".into(),
                    values: vec!["Album Sort".into()],
                },
                Id3Frame::Text {
                    id: "TSOP".into(),
                    values: vec!["Artist Sort".into()],
                },
                Id3Frame::Text {
                    id: "TSOT".into(),
                    values: vec!["Title Sort".into()],
                },
                Id3Frame::Text {
                    id: "TSST".into(),
                    values: vec!["Disc One".into()],
                },
                Id3Frame::Text {
                    id: "TDLY".into(),
                    values: vec!["500".into()],
                },
                Id3Frame::Text {
                    id: "TOFN".into(),
                    values: vec!["song.mp3".into()],
                },
            ],
        };
        let kv = to_key_value_pairs(&tag);
        assert!(kv
            .iter()
            .any(|(k, v)| k == "albumsort" && v == "Album Sort"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "artistsort" && v == "Artist Sort"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "titlesort" && v == "Title Sort"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "setsubtitle" && v == "Disc One"));
        assert!(kv.iter().any(|(k, v)| k == "playlistdelay" && v == "500"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "originalfilename" && v == "song.mp3"));
    }

    /// v2.3-only date / time / recording-dates / size text frames that
    /// v2.4 dropped (TYER/TDAT/TIME folded into TDRC; TRDA/TSIZ
    /// removed). On a v2.3 tag these still carry data; the mapping
    /// keeps them addressable without colliding with TYER's `date`
    /// (TDAT uses a distinct `date_ddmm` key per spec §TDAT).
    #[test]
    fn to_key_value_pairs_surfaces_v23_only_date_and_size_frames() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![
                Id3Frame::Text {
                    id: "TYER".into(),
                    values: vec!["1999".into()],
                },
                Id3Frame::Text {
                    id: "TDAT".into(),
                    values: vec!["3112".into()], // DDMM = 31-Dec
                },
                Id3Frame::Text {
                    id: "TIME".into(),
                    values: vec!["2359".into()], // HHMM
                },
                Id3Frame::Text {
                    id: "TRDA".into(),
                    values: vec!["4th-7th June".into()],
                },
                Id3Frame::Text {
                    id: "TSIZ".into(),
                    values: vec!["123456".into()],
                },
            ],
        };
        let kv = to_key_value_pairs(&tag);
        // TYER and TDAT must NOT collide on the same key.
        assert!(kv.iter().any(|(k, v)| k == "date" && v == "1999"));
        assert!(kv.iter().any(|(k, v)| k == "date_ddmm" && v == "3112"));
        assert!(kv.iter().any(|(k, v)| k == "time_hhmm" && v == "2359"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "recordingdates" && v == "4th-7th June"));
        assert!(kv.iter().any(|(k, v)| k == "size" && v == "123456"));
    }

    /// A `T???` frame outside the known table still falls through to
    /// the generic lowercased-id branch — the mapping table additions
    /// don't suppress the catch-all behaviour.
    #[test]
    fn to_key_value_pairs_unknown_t_frame_still_lowercases() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TZZZ".into(),
                values: vec!["custom".into()],
            }],
        };
        let kv = to_key_value_pairs(&tag);
        assert!(kv.iter().any(|(k, v)| k == "tzzz" && v == "custom"));
    }

    /// `parse_tcon_value` handles the v2.3 parenthesised grammar and the
    /// v2.4 bare form within a single value, including the corner cases:
    /// the `((` escape, an unclosed `(`, a trailing free-text refinement
    /// after a numeric reference, and a bare value with no parentheses.
    #[test]
    fn parse_tcon_value_grammar() {
        let parse = |s: &str| {
            let mut out = Vec::new();
            parse_tcon_value(s, &mut out);
            out
        };

        // Numeric reference + keyword references chained in one string.
        assert_eq!(
            parse("(21)(RX)(CR)"),
            vec![
                ContentType::Genre {
                    index: 21,
                    name: Some("Ska"),
                },
                ContentType::Remix,
                ContentType::Cover,
            ],
        );

        // Trailing free-text refinement after a parenthesised reference.
        assert_eq!(
            parse("(4)Eurodisco"),
            vec![
                ContentType::Genre {
                    index: 4,
                    name: Some("Disco"),
                },
                ContentType::Custom("Eurodisco".into()),
            ],
        );

        // `((` escape: a literal leading `(` for a free-text genre.
        assert_eq!(
            parse("((55)((I think...)"),
            vec![ContentType::Custom("(55)((I think...)".into())],
        );

        // An unclosed `(` is non-conforming; the remainder surfaces as
        // free text rather than being dropped.
        assert_eq!(parse("(21"), vec![ContentType::Custom("(21".into())],);

        // A bare value with no parentheses (v2.4): numeric → genre.
        assert_eq!(
            parse("17"),
            vec![ContentType::Genre {
                index: 17,
                name: Some("Rock"),
            }],
        );

        // A bare non-numeric non-keyword value (v2.4) → free text.
        assert_eq!(
            parse("My Genre"),
            vec![ContentType::Custom("My Genre".into())]
        );

        // A bare keyword (v2.4).
        assert_eq!(parse("RX"), vec![ContentType::Remix]);

        // An empty value contributes nothing.
        assert_eq!(parse(""), Vec::<ContentType>::new());
    }

    // -----------------------------------------------------------------
    // Frame-level zlib compression (v2.3 §3.3 flag `i` / v2.4 §4.1.2
    // flag `k`) + the rest of the v2.3 format-flags byte (encryption /
    // grouping additions).
    // -----------------------------------------------------------------

    /// Structural frame equality for tests. `Id3Frame` deliberately
    /// does not implement `PartialEq` (its `AttachedPicture` member
    /// is an oxideav-core type without one), so compare the exact
    /// Debug projections instead — every field of every variant
    /// participates.
    #[track_caller]
    fn assert_frames_eq(got: &[Id3Frame], want: &[Id3Frame]) {
        assert_eq!(format!("{got:?}"), format!("{want:?}"));
    }

    /// A tag whose frames are worth compressing (a long repetitive
    /// PRIV payload) plus a small text frame so both shapes ride
    /// through the same options.
    fn compressible_tag() -> Id3Tag {
        Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Text {
                    id: "TIT2".into(),
                    values: vec!["Ünïcode Title".into()],
                },
                Id3Frame::Private {
                    owner: "example".into(),
                    data: b"highly compressible ".repeat(64),
                },
            ],
        }
    }

    /// `with_compression(true)` round-trips through `parse_tag` for
    /// both writable versions, sets the right per-version format-flag
    /// bits, and stores the decompressed size in the right shape
    /// (4 regular BE bytes in v2.3, a synchsafe DLI in v2.4).
    #[test]
    fn compressed_write_roundtrip_v23_and_v24() {
        let tag = compressible_tag();
        let opts = WriteOptions::new().with_compression(true);

        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let bytes = write_tag_with_options(&tag, version, &opts).unwrap();

            // First frame header starts right after the 10-byte tag
            // header: id(4) size(4) status(1) format(1).
            let format_flags = bytes[ID3V2_HEADER_SIZE + 9];
            let announce = &bytes[ID3V2_HEADER_SIZE + 10..ID3V2_HEADER_SIZE + 14];
            // The first frame is TIT2; its decompressed payload is the
            // encoding byte + encoded title.
            let (_, plain) = encode_frame(version, &tag.frames[0]).unwrap();
            match version {
                Id3Version::V2_3 => {
                    assert_eq!(format_flags, 0x80, "v2.3 compression bit");
                    assert_eq!(
                        regular_u32(announce[0], announce[1], announce[2], announce[3]) as usize,
                        plain.len(),
                        "v2.3 decompressed-size field"
                    );
                }
                Id3Version::V2_4 => {
                    assert_eq!(format_flags, 0x09, "v2.4 compression + DLI bits");
                    assert_eq!(
                        synchsafe_u32(announce[0], announce[1], announce[2], announce[3]) as usize,
                        plain.len(),
                        "v2.4 data-length indicator"
                    );
                }
                _ => unreachable!(),
            }

            // The big PRIV frame must actually have shrunk on the wire.
            assert!(
                bytes.len() < write_tag(&tag, version).unwrap().len(),
                "compressed tag should be smaller than the plain one for {version:?}"
            );

            let (parsed, consumed) = parse_tag(&bytes).unwrap();
            assert_eq!(consumed, bytes.len());
            assert_frames_eq(&parsed.frames, &tag.frames);
        }
    }

    /// Compression composes with per-frame unsync (v2.4): the format
    /// flags carry 0x08 | 0x02 | 0x01 and the parse path reverses
    /// unsync before inflating.
    #[test]
    fn compressed_per_frame_unsync_composes_v24() {
        // An incompressible payload makes the deflate stream carry
        // plenty of high bytes, exercising the unsync escape.
        let mut noise = Vec::with_capacity(4096);
        let mut x: u32 = 0x2545_F491;
        for _ in 0..4096 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            noise.push((x >> 24) as u8);
        }
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Private {
                owner: "noise".into(),
                data: noise,
            }],
        };
        let opts = WriteOptions::new()
            .with_compression(true)
            .with_unsync(UnsyncMode::PerFrame);
        let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
        assert_eq!(bytes[ID3V2_HEADER_SIZE + 9], 0x0B, "compression+unsync+DLI");
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_frames_eq(&parsed.frames, &tag.frames);
    }

    /// Compression composes with the extended-header CRC and
    /// whole-tag unsync: the CRC covers the post-compression frame
    /// bytes and the parser verifies it after reversing unsync.
    #[test]
    fn compressed_crc_whole_tag_unsync_composes() {
        let tag = compressible_tag();
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let opts = WriteOptions::new()
                .with_compression(true)
                .with_crc(true)
                .with_unsync(UnsyncMode::WholeTag);
            let bytes = write_tag_with_options(&tag, version, &opts).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            assert_frames_eq(&parsed.frames, &tag.frames);
        }
    }

    /// Hand-built v2.3 compressed frame (spec §3.3 wire layout, not
    /// our writer's output): format flag 0x80, 4 BE bytes of
    /// decompressed size, then the zlib stream.
    #[test]
    fn v23_handbuilt_compressed_frame_parses() {
        let plain = [&[0u8][..], b"Compressed Title"].concat();
        let zlib = deflate_frame(&plain).unwrap();
        let body = [&(plain.len() as u32).to_be_bytes()[..], &zlib].concat();

        let mut frame = Vec::new();
        frame.extend_from_slice(b"TIT2");
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x80]); // status, format (i bit)
        frame.extend_from_slice(&body);

        let tag = wrap_v23_tag(&frame);
        let (parsed, _) = parse_tag(&tag).unwrap();
        assert_frames_eq(
            &parsed.frames,
            &[Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Compressed Title".into()],
            }],
        );
    }

    /// Wrap raw v2.3 frame bytes in a 10-byte tag header.
    fn wrap_v23_tag(frames: &[u8]) -> Vec<u8> {
        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.extend_from_slice(&[3, 0, 0]);
        let s = frames.len() as u32;
        tag.push(((s >> 21) & 0x7F) as u8);
        tag.push(((s >> 14) & 0x7F) as u8);
        tag.push(((s >> 7) & 0x7F) as u8);
        tag.push((s & 0x7F) as u8);
        tag.extend_from_slice(frames);
        tag
    }

    /// v2.3 grouping-identity flag (k = 0x20): the group-identifier
    /// byte is a header addition, not payload — a grouped TIT2 must
    /// parse to its text, not to garbage shifted by one byte.
    #[test]
    fn v23_grouped_frame_skips_group_byte() {
        let body = [&[0xA5u8, 0x00][..], b"Grouped"].concat();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"TIT2");
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x20]); // status, format (k bit)
        frame.extend_from_slice(&body);

        let (parsed, _) = parse_tag(&wrap_v23_tag(&frame)).unwrap();
        assert_frames_eq(
            &parsed.frames,
            &[Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Grouped".into()],
            }],
        );
    }

    /// v2.3 encryption flag (j = 0x40): without keys the frame
    /// surfaces as Unknown with the method byte + ciphertext
    /// preserved. With compression also set (i | j), the
    /// decompressed-size addition precedes the method byte (§3.3
    /// orders additions by flag order) and is stripped, while the
    /// method byte + data are preserved.
    #[test]
    fn v23_encrypted_frame_surfaces_unknown() {
        // Encrypted-only.
        let body = [&[0x42u8][..], b"ciphertext"].concat();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"GEOB");
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x40]);
        frame.extend_from_slice(&body);
        let (parsed, _) = parse_tag(&wrap_v23_tag(&frame)).unwrap();
        assert_frames_eq(
            &parsed.frames,
            &[Id3Frame::Unknown {
                id: "GEOB".into(),
                raw: body.clone(),
            }],
        );

        // Compressed + encrypted: 4-byte size first, then method byte.
        let body2 = [&1234u32.to_be_bytes()[..], &[0x42], b"ciphertext"].concat();
        let mut frame2 = Vec::new();
        frame2.extend_from_slice(b"GEOB");
        frame2.extend_from_slice(&(body2.len() as u32).to_be_bytes());
        frame2.extend_from_slice(&[0x00, 0xC0]);
        frame2.extend_from_slice(&body2);
        let (parsed2, _) = parse_tag(&wrap_v23_tag(&frame2)).unwrap();
        assert_frames_eq(
            &parsed2.frames,
            &[Id3Frame::Unknown {
                id: "GEOB".into(),
                raw: body.clone(), // size addition stripped, method + data kept
            }],
        );
    }

    /// v2.4 §4.1.2 makes the data-length indicator mandatory under
    /// compression ("this requires the 'Data Length Indicator' bit to
    /// be set as well"). A compressed frame without it is malformed;
    /// frames parsed before it survive, per the parser's
    /// keep-what-we-got posture for corrupted frames.
    #[test]
    fn v24_compressed_without_dli_drops_frame() {
        let good_tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Kept".into()],
            }],
        };
        let mut bytes =
            write_tag_with_options(&good_tag, Id3Version::V2_4, &WriteOptions::new()).unwrap();

        // Append a compressed frame whose format flags claim 0x08
        // without 0x01.
        let zlib = deflate_frame(b"\x00whatever").unwrap();
        let mut bad = Vec::new();
        bad.extend_from_slice(b"TALB");
        bad.extend_from_slice(&synchsafe_bytes_u28(zlib.len() as u32));
        bad.extend_from_slice(&[0x00, 0x08]);
        bad.extend_from_slice(&zlib);
        bytes.extend_from_slice(&bad);
        let new_size = (bytes.len() - ID3V2_HEADER_SIZE) as u32;
        bytes[6..10].copy_from_slice(&synchsafe_bytes_u28(new_size));

        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_frames_eq(&parsed.frames, &good_tag.frames);
    }

    /// The announced decompressed size is authoritative: a stream
    /// that inflates to a different length is rejected as corruption
    /// (the frame is dropped), matching the CRC-mismatch posture.
    #[test]
    fn compressed_announce_mismatch_drops_frame() {
        let plain = [&[0u8][..], b"Mismatch"].concat();
        let zlib = deflate_frame(&plain).unwrap();
        // Announce one byte more than the stream actually inflates to.
        let body = [&((plain.len() + 1) as u32).to_be_bytes()[..], &zlib].concat();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"TIT2");
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x80]);
        frame.extend_from_slice(&body);
        let (parsed, _) = parse_tag(&wrap_v23_tag(&frame)).unwrap();
        assert!(parsed.frames.is_empty());
    }

    /// A zlib bomb can't force a huge allocation: an announce beyond
    /// the 64 MiB per-frame ceiling is rejected before any inflation
    /// happens, and output is capped at the announce otherwise.
    #[test]
    fn compressed_bomb_announce_capped() {
        let zlib = deflate_frame(&vec![0u8; 1024]).unwrap();
        let body = [&u32::MAX.to_be_bytes()[..], &zlib].concat();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"PRIV");
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x80]);
        frame.extend_from_slice(&body);
        let (parsed, _) = parse_tag(&wrap_v23_tag(&frame)).unwrap();
        assert!(parsed.frames.is_empty());

        // Direct check on the helper: cap error, not OOM.
        assert!(inflate_frame(&zlib, MAX_DECOMPRESSED_FRAME + 1).is_err());
        // And inflating with the honest announce succeeds.
        assert_eq!(inflate_frame(&zlib, 1024).unwrap(), vec![0u8; 1024]);
    }

    #[test]
    fn language_from_wire_classifies_three_states() {
        // Well-formed code, lower-cased per the v2.4 recommendation.
        assert_eq!(Language::from_wire(*b"eng"), Language::Code(*b"eng"));
        assert_eq!(Language::from_wire(*b"Eng"), Language::Code(*b"eng"));
        assert_eq!(Language::from_wire(*b"ENG"), Language::Code(*b"eng"));
        assert_eq!(Language::from_wire(*b"fre"), Language::Code(*b"fre"));

        // The XXX "not known" sentinel, matched case-insensitively.
        assert_eq!(Language::from_wire(*b"XXX"), Language::Unknown);
        assert_eq!(Language::from_wire(*b"xxx"), Language::Unknown);
        assert_eq!(Language::from_wire(*b"Xxx"), Language::Unknown);

        // Anything non-alphabetic is preserved verbatim as Malformed,
        // including the all-NUL padding written for an absent language.
        assert_eq!(
            Language::from_wire([0, 0, 0]),
            Language::Malformed([0, 0, 0])
        );
        assert_eq!(Language::from_wire(*b"e1g"), Language::Malformed(*b"e1g"));
        assert_eq!(Language::from_wire(*b"   "), Language::Malformed(*b"   "));
    }

    #[test]
    fn language_to_wire_and_as_code() {
        assert_eq!(Language::Code(*b"deu").to_wire(), *b"deu");
        assert_eq!(Language::Unknown.to_wire(), *b"XXX");
        assert_eq!(Language::Malformed([0, 0, 0]).to_wire(), [0, 0, 0]);

        assert_eq!(Language::Code(*b"deu").as_code(), Some("deu"));
        assert_eq!(Language::Unknown.as_code(), None);
        assert_eq!(Language::Malformed(*b"e1g").as_code(), None);

        // from_wire ∘ to_wire is the identity for the decoder's own
        // outputs (Unknown, Code, and decoder-produced Malformed).
        for v in [
            Language::Code(*b"jpn"),
            Language::Unknown,
            Language::Malformed([0, 0, 0]),
        ] {
            assert_eq!(Language::from_wire(v.to_wire()), v);
        }
    }

    #[test]
    fn frame_language_accessor_spans_tagged_variants() {
        // All four language-tagged frames surface their code uniformly.
        let comm = Id3Frame::Comment {
            lang: *b"eng",
            description: String::new(),
            text: "hi".into(),
        };
        assert_eq!(comm.language(), Some(Language::Code(*b"eng")));

        let uslt = Id3Frame::Lyrics {
            lang: *b"FRE",
            description: String::new(),
            text: "salut".into(),
        };
        assert_eq!(uslt.language(), Some(Language::Code(*b"fre")));

        let user = Id3Frame::TermsOfUse {
            lang: *b"XXX",
            text: "terms".into(),
        };
        assert_eq!(user.language(), Some(Language::Unknown));

        let sylt = Id3Frame::SyncedLyrics {
            lang: [0, 0, 0],
            time_format: 2,
            content_type: 1,
            description: String::new(),
            syncs: Vec::new(),
        };
        assert_eq!(sylt.language(), Some(Language::Malformed([0, 0, 0])));

        // A non-language-tagged variant yields None.
        assert_eq!(Id3Frame::PlayCounter { count: 3 }.language(), None);
    }

    #[test]
    fn language_accessor_survives_comm_roundtrip() {
        // Build a COMM frame, serialise + re-parse a whole tag, and
        // confirm the typed language survives the wire round-trip.
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Comment {
                lang: *b"eng",
                description: "d".into(),
                text: "body".into(),
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let comm = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Comment { .. }))
            .unwrap();
        assert_eq!(comm.language(), Some(Language::Code(*b"eng")));
    }

    #[test]
    fn price_from_element_splits_currency_and_amount() {
        // Spec: first three characters = ISO-4217 currency, the
        // remainder = numerical amount with "." as decimal separator.
        assert_eq!(
            Price::from_element("USD8.99"),
            Price::Element {
                currency: *b"USD",
                amount: "8.99".into(),
            }
        );
        // Lower-case currency normalises to upper case for comparison.
        assert_eq!(
            Price::from_element("eur9.50"),
            Price::Element {
                currency: *b"EUR",
                amount: "9.50".into(),
            }
        );
        // A whole-number amount (no decimal point) is preserved verbatim.
        assert_eq!(
            Price::from_element("JPY1000"),
            Price::Element {
                currency: *b"JPY",
                amount: "1000".into(),
            }
        );
        // Exactly three chars = currency with an empty amount string.
        assert_eq!(
            Price::from_element("GBP"),
            Price::Element {
                currency: *b"GBP",
                amount: String::new(),
            }
        );

        // currency()/amount() expose the parts for a well-formed element.
        let p = Price::from_element("USD8.99");
        assert_eq!(p.currency(), Some("USD"));
        assert_eq!(p.amount(), Some("8.99"));
    }

    #[test]
    fn price_from_element_preserves_malformed() {
        // Too short to carry a three-character currency code.
        assert_eq!(Price::from_element("US"), Price::Malformed("US".into()));
        assert_eq!(Price::from_element(""), Price::Malformed(String::new()));
        // Leading three bytes not all ASCII letters.
        assert_eq!(
            Price::from_element("1SD8.99"),
            Price::Malformed("1SD8.99".into())
        );
        let m = Price::from_element("12");
        assert_eq!(m.currency(), None);
        assert_eq!(m.amount(), None);
    }

    #[test]
    fn commercial_prices_splits_on_slash() {
        // Spec §4.24: "several prices may be concatenated, separated by
        // a '/' character, but there may only be one currency of each
        // type." Wire order is preserved.
        let comr = Id3Frame::Commercial {
            price: "USD8.99/EUR9.50".into(),
            valid_until: "20260101".into(),
            contact_url: "http://x".into(),
            received_as: 0,
            seller: String::new(),
            description: String::new(),
            logo_mime: String::new(),
            logo_data: Vec::new(),
        };
        assert_eq!(
            comr.commercial_prices(),
            Some(vec![
                Price::Element {
                    currency: *b"USD",
                    amount: "8.99".into(),
                },
                Price::Element {
                    currency: *b"EUR",
                    amount: "9.50".into(),
                },
            ])
        );

        // An empty price field yields an empty Vec (frame present,
        // no price) rather than None or a single malformed element.
        let empty = Id3Frame::Commercial {
            price: String::new(),
            valid_until: String::new(),
            contact_url: String::new(),
            received_as: 0,
            seller: String::new(),
            description: String::new(),
            logo_mime: String::new(),
            logo_data: Vec::new(),
        };
        assert_eq!(empty.commercial_prices(), Some(Vec::new()));

        // A non-Commercial variant yields None.
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.commercial_prices(), None);
    }

    #[test]
    fn ownership_price_is_single_element() {
        // Spec §4.23: the OWNE "price paid" field carries a single
        // price element (no "/" concatenation).
        let owne = Id3Frame::Ownership {
            price: "USD8.99".into(),
            date: "20260101".into(),
            seller: "Acme".into(),
        };
        assert_eq!(
            owne.ownership_price(),
            Some(Price::Element {
                currency: *b"USD",
                amount: "8.99".into(),
            })
        );

        // A non-Ownership variant yields None.
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.ownership_price(), None);
    }

    #[test]
    fn price_accessors_survive_wire_roundtrip() {
        // Build COMR + OWNE frames, serialise + re-parse a whole tag,
        // and confirm the typed prices survive the wire round-trip and
        // the raw `price` strings are untouched.
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Commercial {
                    price: "USD8.99/EUR9.50".into(),
                    valid_until: "20260101".into(),
                    contact_url: "http://x".into(),
                    received_as: 3,
                    seller: "Store".into(),
                    description: "Offer".into(),
                    logo_mime: String::new(),
                    logo_data: Vec::new(),
                },
                Id3Frame::Ownership {
                    price: "GBP4.00".into(),
                    date: "20251231".into(),
                    seller: "Owner".into(),
                },
            ],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();

        let comr = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Commercial { .. }))
            .unwrap();
        assert_eq!(
            comr.commercial_prices(),
            Some(vec![
                Price::Element {
                    currency: *b"USD",
                    amount: "8.99".into(),
                },
                Price::Element {
                    currency: *b"EUR",
                    amount: "9.50".into(),
                },
            ])
        );

        let owne = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Ownership { .. }))
            .unwrap();
        assert_eq!(
            owne.ownership_price(),
            Some(Price::Element {
                currency: *b"GBP",
                amount: "4.00".into(),
            })
        );
    }

    #[test]
    fn id3date_splits_eight_digit_string() {
        // Spec OWNE §4.23 / COMR §4.24: "an 8 character date string
        // (YYYYMMDD)". Eight ASCII digits split positionally.
        assert_eq!(
            Id3Date::from_field("20240615"),
            Id3Date::Ymd {
                year: 2024,
                month: 6,
                day: 15,
            }
        );
        // Leading zeros in every component.
        assert_eq!(
            Id3Date::from_field("00010203"),
            Id3Date::Ymd {
                year: 1,
                month: 2,
                day: 3,
            }
        );
        // The split is purely positional: the spec defines the field as
        // a fixed YYYYMMDD digit string with no validity constraint, so
        // an out-of-range month/day surfaces structurally rather than
        // being rejected.
        assert_eq!(
            Id3Date::from_field("20241340"),
            Id3Date::Ymd {
                year: 2024,
                month: 13,
                day: 40,
            }
        );
    }

    #[test]
    fn id3date_rejects_non_eight_digit() {
        // Too short / too long / empty / a non-digit byte all surface as
        // Malformed with the raw string preserved.
        for s in ["2024061", "202406155", "", "2024-615", "2024061x"] {
            assert_eq!(Id3Date::from_field(s), Id3Date::Malformed(s.to_string()));
            assert_eq!(Id3Date::from_field(s).year(), None);
            assert_eq!(Id3Date::from_field(s).month(), None);
            assert_eq!(Id3Date::from_field(s).day(), None);
        }
    }

    #[test]
    fn id3date_component_accessors() {
        let d = Id3Date::from_field("20240615");
        assert_eq!(d.year(), Some(2024));
        assert_eq!(d.month(), Some(6));
        assert_eq!(d.day(), Some(15));
    }

    #[test]
    fn ownership_and_commercial_date_accessors() {
        let owne = Id3Frame::Ownership {
            price: "USD8.99".into(),
            date: "20251231".into(),
            seller: "Acme".into(),
        };
        assert_eq!(
            owne.ownership_date(),
            Some(Id3Date::Ymd {
                year: 2025,
                month: 12,
                day: 31,
            })
        );
        // commercial_valid_until is None on an Ownership frame and vice
        // versa — the accessors route strictly by variant.
        assert_eq!(owne.commercial_valid_until(), None);

        let comr = Id3Frame::Commercial {
            price: "USD8.99".into(),
            valid_until: "20260101".into(),
            contact_url: "http://x".into(),
            received_as: 0,
            seller: String::new(),
            description: String::new(),
            logo_mime: String::new(),
            logo_data: Vec::new(),
        };
        assert_eq!(
            comr.commercial_valid_until(),
            Some(Id3Date::Ymd {
                year: 2026,
                month: 1,
                day: 1,
            })
        );
        assert_eq!(comr.ownership_date(), None);

        // A non-matching variant yields None for both accessors.
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.ownership_date(), None);
        assert_eq!(
            Id3Frame::PlayCounter { count: 1 }.commercial_valid_until(),
            None
        );

        // An empty / absent date field surfaces as Malformed, preserving
        // the raw string rather than guessing a value.
        let owne_empty = Id3Frame::Ownership {
            price: "USD8.99".into(),
            date: String::new(),
            seller: "Acme".into(),
        };
        assert_eq!(
            owne_empty.ownership_date(),
            Some(Id3Date::Malformed(String::new()))
        );
    }

    #[test]
    fn id3date_accessors_survive_wire_roundtrip() {
        // Serialise + re-parse a tag carrying OWNE + COMR, and confirm
        // the typed dates survive and the raw date strings are untouched.
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                Id3Frame::Ownership {
                    price: "GBP4.00".into(),
                    date: "20251231".into(),
                    seller: "Owner".into(),
                },
                Id3Frame::Commercial {
                    price: "USD8.99".into(),
                    valid_until: "20260101".into(),
                    contact_url: "http://x".into(),
                    received_as: 3,
                    seller: "Store".into(),
                    description: "Offer".into(),
                    logo_mime: String::new(),
                    logo_data: Vec::new(),
                },
            ],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();

        let owne = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Ownership { .. }))
            .unwrap();
        assert_eq!(
            owne.ownership_date(),
            Some(Id3Date::Ymd {
                year: 2025,
                month: 12,
                day: 31,
            })
        );
        match owne {
            Id3Frame::Ownership { date, .. } => assert_eq!(date, "20251231"),
            _ => unreachable!(),
        }

        let comr = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Commercial { .. }))
            .unwrap();
        assert_eq!(
            comr.commercial_valid_until(),
            Some(Id3Date::Ymd {
                year: 2026,
                month: 1,
                day: 1,
            })
        );
        match comr {
            Id3Frame::Commercial { valid_until, .. } => assert_eq!(valid_until, "20260101"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn tkey_grammar_covers_spec_examples() {
        // Spec §4.2.1 (v2.3) / §4.2.3 (v2.4): ground key A..G, optional
        // b/# halfkey, optional m minor, the standalone "o" off-key.
        assert_eq!(
            parse_tkey_value("C"),
            MusicalKey::Key {
                tonic: 'C',
                accidental: None,
                minor: false
            }
        );
        assert_eq!(
            parse_tkey_value("Cm"),
            MusicalKey::Key {
                tonic: 'C',
                accidental: None,
                minor: true
            }
        );
        // The spec's worked v2.4 example "Dbm" — D-flat minor.
        assert_eq!(
            parse_tkey_value("Dbm"),
            MusicalKey::Key {
                tonic: 'D',
                accidental: Some(KeyAccidental::Flat),
                minor: true
            }
        );
        // The spec's worked v2.3 example "Cbm" — C-flat minor.
        assert_eq!(
            parse_tkey_value("Cbm"),
            MusicalKey::Key {
                tonic: 'C',
                accidental: Some(KeyAccidental::Flat),
                minor: true
            }
        );
        assert_eq!(
            parse_tkey_value("F#"),
            MusicalKey::Key {
                tonic: 'F',
                accidental: Some(KeyAccidental::Sharp),
                minor: false
            }
        );
        assert_eq!(
            parse_tkey_value("A#m"),
            MusicalKey::Key {
                tonic: 'A',
                accidental: Some(KeyAccidental::Sharp),
                minor: true
            }
        );
        // Off key is "o" only.
        assert_eq!(parse_tkey_value("o"), MusicalKey::OffKey);
    }

    #[test]
    fn tkey_non_conforming_surfaces_custom() {
        // Tonic outside A..G.
        assert_eq!(parse_tkey_value("H"), MusicalKey::Custom("H".to_string()));
        // Empty value.
        assert_eq!(parse_tkey_value(""), MusicalKey::Custom(String::new()));
        // Over the three-character spec maximum.
        assert_eq!(
            parse_tkey_value("Cbmm"),
            MusicalKey::Custom("Cbmm".to_string())
        );
        // A trailing character that isn't part of the grammar.
        assert_eq!(parse_tkey_value("Cx"), MusicalKey::Custom("Cx".to_string()));
        // Minor marker before the accidental is out of order.
        assert_eq!(
            parse_tkey_value("Cmb"),
            MusicalKey::Custom("Cmb".to_string())
        );
        // Lowercase off-key sentinel is exactly "o"; "O" is not.
        assert_eq!(parse_tkey_value("O"), MusicalKey::Custom("O".to_string()));
    }

    #[test]
    fn initial_key_accessor_only_on_tkey() {
        let tkey = Id3Frame::Text {
            id: "TKEY".into(),
            values: vec!["Dbm".into()],
        };
        assert_eq!(
            tkey.initial_key(),
            Some(vec![MusicalKey::Key {
                tonic: 'D',
                accidental: Some(KeyAccidental::Flat),
                minor: true
            }])
        );
        // Any other text frame yields None.
        let tit2 = Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Dbm".into()],
        };
        assert_eq!(tit2.initial_key(), None);
        // A non-text variant yields None.
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.initial_key(), None);
    }

    #[test]
    fn initial_key_survives_tkey_roundtrip() {
        // Serialise a TKEY frame, re-parse the whole tag under both
        // envelopes, and confirm the typed key survives. The raw value
        // is unchanged so the typed view is reconstructed identically.
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Text {
                    id: "TKEY".into(),
                    values: vec!["F#m".into()],
                }],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let tkey = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TKEY"))
                .unwrap();
            assert_eq!(
                tkey.initial_key(),
                Some(vec![MusicalKey::Key {
                    tonic: 'F',
                    accidental: Some(KeyAccidental::Sharp),
                    minor: true
                }])
            );
        }
    }

    #[test]
    fn track_position_grammar_covers_spec_examples() {
        // Spec §4.2.1: "a numeric string … MAY be extended with a "/"
        // character and a numeric string … E.g. "4/9"" (TRCK) and
        // "1/2" (TPOS).
        assert_eq!(
            parse_track_position("4/9"),
            TrackPosition::Numbered {
                number: 4,
                total: Some(9)
            }
        );
        assert_eq!(
            parse_track_position("1/2"),
            TrackPosition::Numbered {
                number: 1,
                total: Some(2)
            }
        );
        // Bare number with no total.
        assert_eq!(
            parse_track_position("7"),
            TrackPosition::Numbered {
                number: 7,
                total: None
            }
        );
        // Multi-digit number and total.
        assert_eq!(
            parse_track_position("12/150"),
            TrackPosition::Numbered {
                number: 12,
                total: Some(150)
            }
        );
        // Leading zeros are still a valid numeric string.
        assert_eq!(
            parse_track_position("03/12"),
            TrackPosition::Numbered {
                number: 3,
                total: Some(12)
            }
        );
    }

    #[test]
    fn track_position_non_conforming_surfaces_malformed() {
        // Empty value (e.g. a TRCK frame the parser left with no value).
        assert_eq!(
            parse_track_position(""),
            TrackPosition::Malformed(String::new())
        );
        // Non-numeric number segment.
        assert_eq!(
            parse_track_position("A"),
            TrackPosition::Malformed("A".to_string())
        );
        // Non-numeric total segment.
        assert_eq!(
            parse_track_position("4/B"),
            TrackPosition::Malformed("4/B".to_string())
        );
        // Empty number before the separator.
        assert_eq!(
            parse_track_position("/9"),
            TrackPosition::Malformed("/9".to_string())
        );
        // Empty total after the separator.
        assert_eq!(
            parse_track_position("4/"),
            TrackPosition::Malformed("4/".to_string())
        );
        // More than one separator is not the spec's number/total pair.
        assert_eq!(
            parse_track_position("1/2/3"),
            TrackPosition::Malformed("1/2/3".to_string())
        );
        // A sign is not part of the "numeric string" grammar.
        assert_eq!(
            parse_track_position("+4"),
            TrackPosition::Malformed("+4".to_string())
        );
        // Whitespace is not a digit.
        assert_eq!(
            parse_track_position("4 / 9"),
            TrackPosition::Malformed("4 / 9".to_string())
        );
        // A value that overflows a u32 is preserved verbatim.
        assert_eq!(
            parse_track_position("4294967296"),
            TrackPosition::Malformed("4294967296".to_string())
        );
    }

    #[test]
    fn track_position_accessors_route_by_frame_id() {
        let trck = Id3Frame::Text {
            id: "TRCK".into(),
            values: vec!["4/9".into()],
        };
        assert_eq!(
            trck.track_number(),
            Some(TrackPosition::Numbered {
                number: 4,
                total: Some(9)
            })
        );
        // TRCK is not TPOS and vice versa.
        assert_eq!(trck.part_of_set(), None);

        let tpos = Id3Frame::Text {
            id: "TPOS".into(),
            values: vec!["1/2".into()],
        };
        assert_eq!(
            tpos.part_of_set(),
            Some(TrackPosition::Numbered {
                number: 1,
                total: Some(2)
            })
        );
        assert_eq!(tpos.track_number(), None);

        // Any other text frame yields None on both accessors.
        let tit2 = Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["4/9".into()],
        };
        assert_eq!(tit2.track_number(), None);
        assert_eq!(tit2.part_of_set(), None);
        // A non-text variant yields None.
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.track_number(), None);
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.part_of_set(), None);
    }

    #[test]
    fn track_position_empty_values_is_malformed() {
        // A TRCK frame whose parser left it with no value at all decodes
        // to Malformed("") rather than panicking on the missing first().
        let trck = Id3Frame::Text {
            id: "TRCK".into(),
            values: vec![],
        };
        assert_eq!(
            trck.track_number(),
            Some(TrackPosition::Malformed(String::new()))
        );
    }

    #[test]
    fn track_position_survives_roundtrip() {
        // Serialise TRCK + TPOS frames, re-parse the whole tag under both
        // envelopes, and confirm the typed views survive (the raw value
        // round-trips so the typed view is reconstructed identically).
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![
                    Id3Frame::Text {
                        id: "TRCK".into(),
                        values: vec!["4/9".into()],
                    },
                    Id3Frame::Text {
                        id: "TPOS".into(),
                        values: vec!["1/2".into()],
                    },
                ],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let trck = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TRCK"))
                .unwrap();
            assert_eq!(
                trck.track_number(),
                Some(TrackPosition::Numbered {
                    number: 4,
                    total: Some(9)
                })
            );
            let tpos = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TPOS"))
                .unwrap();
            assert_eq!(
                tpos.part_of_set(),
                Some(TrackPosition::Numbered {
                    number: 1,
                    total: Some(2)
                })
            );
        }
    }

    #[test]
    fn isrc_accepts_twelve_ascii_characters() {
        // The spec fixes the field at "12 characters"; a twelve-ASCII-char
        // value decodes to a Code carrying the verbatim string.
        let frame = Id3Frame::Text {
            id: "TSRC".into(),
            values: vec!["USRC17607839".into()],
        };
        assert_eq!(frame.isrc(), Some(Isrc::Code("USRC17607839".into())));
    }

    #[test]
    fn isrc_non_twelve_or_non_ascii_is_malformed() {
        // Wrong length (short, long), empty, and a non-ASCII byte all
        // surface structurally as Malformed with the raw value preserved.
        for raw in ["USRC1760783", "USRC176078390", "", "USRC1760783é"] {
            let frame = Id3Frame::Text {
                id: "TSRC".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                frame.isrc(),
                Some(Isrc::Malformed(raw.to_string())),
                "value {raw:?} should be Malformed"
            );
        }
    }

    #[test]
    fn isrc_accessor_only_on_tsrc() {
        // Routes strictly by frame id: a non-TSRC text frame and a
        // non-text frame both return None.
        let other_text = Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["USRC17607839".into()],
        };
        assert_eq!(other_text.isrc(), None);
        let non_text = Id3Frame::PlayCounter { count: 1 };
        assert_eq!(non_text.isrc(), None);
    }

    #[test]
    fn isrc_empty_values_is_malformed() {
        // A TSRC frame with no values decodes to Malformed("") rather than
        // panicking — matching the track-position empty-values contract.
        let frame = Id3Frame::Text {
            id: "TSRC".into(),
            values: vec![],
        };
        assert_eq!(frame.isrc(), Some(Isrc::Malformed(String::new())));
    }

    #[test]
    fn isrc_survives_roundtrip() {
        // Serialise a TSRC frame, re-parse under both envelopes, and
        // confirm the typed view is reconstructed identically (the raw
        // value round-trips losslessly).
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Text {
                    id: "TSRC".into(),
                    values: vec!["GBAYE6800001".into()],
                }],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let tsrc = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TSRC"))
                .unwrap();
            assert_eq!(tsrc.isrc(), Some(Isrc::Code("GBAYE6800001".into())));
        }
    }

    #[test]
    fn length_ms_accepts_numeric_string() {
        // The spec defines TLEN as a millisecond count in a numeric string;
        // a plain decimal value decodes to Millis carrying the integer.
        let frame = Id3Frame::Text {
            id: "TLEN".into(),
            values: vec!["215000".into()],
        };
        assert_eq!(frame.length_ms(), Some(DurationMs::Millis(215_000)));
    }

    #[test]
    fn length_ms_non_numeric_is_malformed() {
        // A sign, decimal point, whitespace, non-digit byte, and empty
        // value all violate the "numeric string" requirement and surface
        // structurally as Malformed with the raw value preserved.
        for raw in ["+5", "-5", "21.5", " 5", "5 ", "5s", "", "abc"] {
            let frame = Id3Frame::Text {
                id: "TLEN".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                frame.length_ms(),
                Some(DurationMs::Malformed(raw.to_string())),
                "value {raw:?} should be Malformed"
            );
        }
    }

    #[test]
    fn length_ms_overflow_is_malformed() {
        // A value past u64::MAX cannot be represented; it surfaces as
        // Malformed rather than wrapping or panicking.
        let raw = "99999999999999999999999999";
        let frame = Id3Frame::Text {
            id: "TLEN".into(),
            values: vec![raw.into()],
        };
        assert_eq!(
            frame.length_ms(),
            Some(DurationMs::Malformed(raw.to_string()))
        );
    }

    #[test]
    fn length_ms_empty_values_is_malformed() {
        // A TLEN frame with no values decodes to Malformed("") rather than
        // panicking, matching the isrc/track-position empty-values contract.
        let frame = Id3Frame::Text {
            id: "TLEN".into(),
            values: vec![],
        };
        assert_eq!(
            frame.length_ms(),
            Some(DurationMs::Malformed(String::new()))
        );
    }

    #[test]
    fn playlist_delay_ms_shares_grammar_and_zero_is_continuous() {
        // TDLY shares the TLEN numeric-string-milliseconds grammar; the
        // spec's "value zero ⇒ multifile continuous" surfaces as Millis(0)
        // rather than a distinct sentinel, leaving the semantic to the
        // caller.
        let zero = Id3Frame::Text {
            id: "TDLY".into(),
            values: vec!["0".into()],
        };
        assert_eq!(zero.playlist_delay_ms(), Some(DurationMs::Millis(0)));
        let delayed = Id3Frame::Text {
            id: "TDLY".into(),
            values: vec!["500".into()],
        };
        assert_eq!(delayed.playlist_delay_ms(), Some(DurationMs::Millis(500)));
        // Non-conforming surfaces as Malformed.
        let bad = Id3Frame::Text {
            id: "TDLY".into(),
            values: vec!["x".into()],
        };
        assert_eq!(
            bad.playlist_delay_ms(),
            Some(DurationMs::Malformed("x".into()))
        );
    }

    #[test]
    fn duration_accessors_route_by_frame_id() {
        // length_ms is None on TDLY and vice versa; both are None on a
        // non-text frame.
        let tlen = Id3Frame::Text {
            id: "TLEN".into(),
            values: vec!["1".into()],
        };
        let tdly = Id3Frame::Text {
            id: "TDLY".into(),
            values: vec!["1".into()],
        };
        assert_eq!(tlen.playlist_delay_ms(), None);
        assert_eq!(tdly.length_ms(), None);
        let non_text = Id3Frame::PlayCounter { count: 1 };
        assert_eq!(non_text.length_ms(), None);
        assert_eq!(non_text.playlist_delay_ms(), None);
    }

    #[test]
    fn length_ms_survives_roundtrip() {
        // Serialise a TLEN frame, re-parse under v2.3 and v2.4, and confirm
        // the typed view is reconstructed identically.
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Text {
                    id: "TLEN".into(),
                    values: vec!["180000".into()],
                }],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let tlen = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TLEN"))
                .unwrap();
            assert_eq!(tlen.length_ms(), Some(DurationMs::Millis(180_000)));
        }
    }

    #[test]
    fn bpm_accepts_integer_numeric_string() {
        // The spec mandates an integer numerical string; a plain decimal
        // value decodes to Beats.
        let frame = Id3Frame::Text {
            id: "TBPM".into(),
            values: vec!["128".into()],
        };
        assert_eq!(frame.bpm(), Some(Bpm::Beats(128)));
    }

    #[test]
    fn bpm_fractional_or_non_numeric_is_malformed() {
        // The spec says "the BPM is an integer", so a fractional value is
        // not conforming; a sign, whitespace, non-digit byte, and empty
        // value are likewise Malformed.
        for raw in ["128.5", "+128", "-1", " 128", "128 ", "fast", ""] {
            let frame = Id3Frame::Text {
                id: "TBPM".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                frame.bpm(),
                Some(Bpm::Malformed(raw.to_string())),
                "value {raw:?} should be Malformed"
            );
        }
    }

    #[test]
    fn bpm_accessor_only_on_tbpm() {
        // Routes strictly by frame id.
        let other_text = Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["128".into()],
        };
        assert_eq!(other_text.bpm(), None);
        let non_text = Id3Frame::PlayCounter { count: 1 };
        assert_eq!(non_text.bpm(), None);
    }

    #[test]
    fn bpm_survives_roundtrip() {
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Text {
                    id: "TBPM".into(),
                    values: vec!["140".into()],
                }],
            };
            let bytes = write_tag(&tag, version).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let tbpm = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TBPM"))
                .unwrap();
            assert_eq!(tbpm.bpm(), Some(Bpm::Beats(140)));
        }
    }

    #[test]
    fn tyer_accepts_four_digit_year() {
        // The spec fixes TYER at four numeric characters; a four-digit
        // value decodes to Year carrying the integer.
        let frame = Id3Frame::Text {
            id: "TYER".into(),
            values: vec!["2024".into()],
        };
        assert_eq!(frame.year(), Some(Id3Year::Year(2024)));
    }

    #[test]
    fn tyer_non_four_digit_is_malformed() {
        // Wrong length (short, long), empty, and a non-digit byte all
        // surface structurally as Malformed with the raw value preserved.
        for raw in ["202", "20245", "", "20x4", "abcd"] {
            let frame = Id3Frame::Text {
                id: "TYER".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                frame.year(),
                Some(Id3Year::Malformed(raw.to_string())),
                "value {raw:?} should be Malformed"
            );
        }
    }

    #[test]
    fn tdat_splits_ddmm_positionally() {
        // TDAT is DDMM (day first): "1506" = 15 June. The split is not
        // calendar-validated — "3199" surfaces day:31, month:99.
        let frame = Id3Frame::Text {
            id: "TDAT".into(),
            values: vec!["1506".into()],
        };
        assert_eq!(
            frame.date_ddmm(),
            Some(DayMonth::DayMonth { day: 15, month: 6 })
        );
        let odd = Id3Frame::Text {
            id: "TDAT".into(),
            values: vec!["3199".into()],
        };
        assert_eq!(
            odd.date_ddmm(),
            Some(DayMonth::DayMonth { day: 31, month: 99 })
        );
        for raw in ["150", "15066", "", "15x6"] {
            let bad = Id3Frame::Text {
                id: "TDAT".into(),
                values: vec![raw.into()],
            };
            assert_eq!(bad.date_ddmm(), Some(DayMonth::Malformed(raw.to_string())));
        }
    }

    #[test]
    fn time_splits_hhmm_positionally() {
        // TIME is HHMM: "0930" = 09:30. Not range-validated — "2599"
        // surfaces hour:25, minute:99.
        let frame = Id3Frame::Text {
            id: "TIME".into(),
            values: vec!["0930".into()],
        };
        assert_eq!(
            frame.time_hhmm(),
            Some(HourMinute::HourMinute {
                hour: 9,
                minute: 30
            })
        );
        let odd = Id3Frame::Text {
            id: "TIME".into(),
            values: vec!["2599".into()],
        };
        assert_eq!(
            odd.time_hhmm(),
            Some(HourMinute::HourMinute {
                hour: 25,
                minute: 99
            })
        );
        for raw in ["093", "09300", "", "09x0"] {
            let bad = Id3Frame::Text {
                id: "TIME".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                bad.time_hhmm(),
                Some(HourMinute::Malformed(raw.to_string()))
            );
        }
    }

    #[test]
    fn tsiz_accepts_numeric_byte_count() {
        // TSIZ is a byte count as a numeric string.
        let frame = Id3Frame::Text {
            id: "TSIZ".into(),
            values: vec!["5242880".into()],
        };
        assert_eq!(frame.size_bytes(), Some(SizeBytes::Bytes(5_242_880)));
        for raw in ["+5", "5.0", " 5", "5 ", "", "5kb"] {
            let bad = Id3Frame::Text {
                id: "TSIZ".into(),
                values: vec![raw.into()],
            };
            assert_eq!(
                bad.size_bytes(),
                Some(SizeBytes::Malformed(raw.to_string()))
            );
        }
    }

    #[test]
    fn v23_date_accessors_route_by_frame_id() {
        // Each accessor is None on the other frames and on a non-text
        // frame; they are version-locked to v2.3 by frame id (a v2.4 tag
        // never carries these ids, but the accessor itself is id-keyed).
        let tyer = Id3Frame::Text {
            id: "TYER".into(),
            values: vec!["2024".into()],
        };
        assert_eq!(tyer.date_ddmm(), None);
        assert_eq!(tyer.time_hhmm(), None);
        assert_eq!(tyer.size_bytes(), None);
        let non_text = Id3Frame::PlayCounter { count: 1 };
        assert_eq!(non_text.year(), None);
        assert_eq!(non_text.date_ddmm(), None);
        assert_eq!(non_text.time_hhmm(), None);
        assert_eq!(non_text.size_bytes(), None);
    }

    #[test]
    fn v23_date_frames_survive_roundtrip() {
        // Serialise the four v2.3 split frames, re-parse under v2.3, and
        // confirm each typed view is reconstructed identically.
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![
                Id3Frame::Text {
                    id: "TYER".into(),
                    values: vec!["1999".into()],
                },
                Id3Frame::Text {
                    id: "TDAT".into(),
                    values: vec!["2412".into()],
                },
                Id3Frame::Text {
                    id: "TIME".into(),
                    values: vec!["2359".into()],
                },
                Id3Frame::Text {
                    id: "TSIZ".into(),
                    values: vec!["1048576".into()],
                },
            ],
        };
        let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let find = |id: &str| {
            parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id: i, .. } if i == id))
                .unwrap()
        };
        assert_eq!(find("TYER").year(), Some(Id3Year::Year(1999)));
        assert_eq!(
            find("TDAT").date_ddmm(),
            Some(DayMonth::DayMonth { day: 24, month: 12 })
        );
        assert_eq!(
            find("TIME").time_hhmm(),
            Some(HourMinute::HourMinute {
                hour: 23,
                minute: 59
            })
        );
        assert_eq!(find("TSIZ").size_bytes(), Some(SizeBytes::Bytes(1_048_576)));
    }

    #[test]
    fn timestamp_all_six_precision_levels() {
        // The structure-doc ISO 8601 subset lists exactly six valid forms;
        // each decodes with the right components Some and the rest None.
        let cases: [(&str, Id3Timestamp); 6] = [
            (
                "2024",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: None,
                    day: None,
                    hour: None,
                    minute: None,
                    second: None,
                },
            ),
            (
                "2024-06",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: None,
                    hour: None,
                    minute: None,
                    second: None,
                },
            ),
            (
                "2024-06-18",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: Some(18),
                    hour: None,
                    minute: None,
                    second: None,
                },
            ),
            (
                "2024-06-18T13",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: Some(18),
                    hour: Some(13),
                    minute: None,
                    second: None,
                },
            ),
            (
                "2024-06-18T13:45",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: Some(18),
                    hour: Some(13),
                    minute: Some(45),
                    second: None,
                },
            ),
            (
                "2024-06-18T13:45:09",
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: Some(18),
                    hour: Some(13),
                    minute: Some(45),
                    second: Some(9),
                },
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                Id3Timestamp::from_field(raw),
                expected,
                "value {raw:?} should decode to the expected precision"
            );
        }
    }

    #[test]
    fn timestamp_component_accessors() {
        // The per-component accessors expose Some/None matching the
        // precision of the source string.
        let ts = Id3Timestamp::from_field("2024-06-18T13:45");
        assert_eq!(ts.year(), Some(2024));
        assert_eq!(ts.month(), Some(6));
        assert_eq!(ts.day(), Some(18));
        assert_eq!(ts.hour(), Some(13));
        assert_eq!(ts.minute(), Some(45));
        assert_eq!(ts.second(), None);
        let coarse = Id3Timestamp::from_field("2024");
        assert_eq!(coarse.year(), Some(2024));
        assert_eq!(coarse.month(), None);
        assert_eq!(coarse.second(), None);
    }

    #[test]
    fn timestamp_not_calendar_validated() {
        // The split is positional: an out-of-range month/day/time is
        // preserved structurally rather than rejected, matching Id3Date.
        assert_eq!(
            Id3Timestamp::from_field("2024-13-40T25:61:99"),
            Id3Timestamp::DateTime {
                year: 2024,
                month: Some(13),
                day: Some(40),
                hour: Some(25),
                minute: Some(61),
                second: Some(99),
            }
        );
    }

    #[test]
    fn timestamp_malformed_inputs() {
        // Wrong separators, wrong digit counts, an embedded duration
        // slash, trailing bytes, and an empty value all surface as
        // Malformed with the raw string preserved.
        for raw in [
            "",
            "24",                    // year too short
            "2024-6",                // month not two digits
            "2024/06",               // wrong separator
            "2024-06-18 13:45",      // space instead of T
            "2024-06-18T13:45:09Z",  // trailing timezone byte
            "2024-06-18T13:45:09.5", // fractional second (not in subset)
            "2024-06-18/2024-06-20", // duration slash (multi-value, not a point)
            "2024-06-18T13:45:",     // trailing separator, no seconds
            "abcd",                  // non-digit year
        ] {
            assert_eq!(
                Id3Timestamp::from_field(raw),
                Id3Timestamp::Malformed(raw.to_string()),
                "value {raw:?} should be Malformed"
            );
        }
    }

    #[test]
    fn timestamp_frame_accessor_routes_by_id() {
        // timestamps() fires on the five TDxx frames and the specific
        // accessors route by their own id; everything else is None.
        for id in ["TDEN", "TDOR", "TDRC", "TDRL", "TDTG"] {
            let frame = Id3Frame::Text {
                id: id.into(),
                values: vec!["2024-06-18".into()],
            };
            assert_eq!(
                frame.timestamps(),
                Some(vec![Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: Some(18),
                    hour: None,
                    minute: None,
                    second: None,
                }])
            );
        }
        let tdrc = Id3Frame::Text {
            id: "TDRC".into(),
            values: vec!["2024".into()],
        };
        assert!(tdrc.recording_time().is_some());
        assert_eq!(tdrc.release_time(), None);
        assert_eq!(tdrc.encoding_time(), None);
        assert_eq!(tdrc.original_release_time(), None);
        assert_eq!(tdrc.tagging_time(), None);

        // A non-timestamp text frame and a non-text frame both yield None.
        let other = Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["2024".into()],
        };
        assert_eq!(other.timestamps(), None);
        assert_eq!(Id3Frame::PlayCounter { count: 1 }.timestamps(), None);
    }

    #[test]
    fn timestamp_multiple_non_contiguous_values() {
        // The spec's "use multiple strings" for non-contiguous dates
        // arrives as one Id3Timestamp per value in wire order; a
        // malformed value among them is preserved positionally.
        let frame = Id3Frame::Text {
            id: "TDRC".into(),
            values: vec!["2024-06".into(), "bogus".into(), "1999".into()],
        };
        assert_eq!(
            frame.timestamps(),
            Some(vec![
                Id3Timestamp::DateTime {
                    year: 2024,
                    month: Some(6),
                    day: None,
                    hour: None,
                    minute: None,
                    second: None,
                },
                Id3Timestamp::Malformed("bogus".into()),
                Id3Timestamp::DateTime {
                    year: 1999,
                    month: None,
                    day: None,
                    hour: None,
                    minute: None,
                    second: None,
                },
            ])
        );
        // An empty-values TDxx frame yields an empty vec, not a panic.
        let empty = Id3Frame::Text {
            id: "TDTG".into(),
            values: vec![],
        };
        assert_eq!(empty.timestamps(), Some(vec![]));
    }

    #[test]
    fn timestamp_survives_roundtrip() {
        // Serialise a TDRC frame under the v2.4 envelope, re-parse, and
        // confirm the typed view is reconstructed identically.
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TDRC".into(),
                values: vec!["2024-06-18T13:45:09".into()],
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let tdrc = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TDRC"))
            .unwrap();
        assert_eq!(
            tdrc.recording_time(),
            Some(vec![Id3Timestamp::DateTime {
                year: 2024,
                month: Some(6),
                day: Some(18),
                hour: Some(13),
                minute: Some(45),
                second: Some(9),
            }])
        );
    }

    // ------------------------------------------------------------------
    // v2.3 <-> v2.4 conversion (convert_tag / Id3Tag::to_version)
    // ------------------------------------------------------------------

    fn text(id: &str, value: &str) -> Id3Frame {
        Id3Frame::Text {
            id: id.to_string(),
            values: vec![value.to_string()],
        }
    }

    fn find_text<'a>(tag: &'a Id3Tag, id: &str) -> Option<&'a Vec<String>> {
        tag.frames.iter().find_map(|f| match f {
            Id3Frame::Text { id: fid, values } if fid == id => Some(values),
            _ => None,
        })
    }

    #[test]
    fn convert_v23_tyer_tdat_time_folds_into_tdrc() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![
                text("TIT2", "Song"),
                text("TYER", "2024"),
                text("TDAT", "1806"), // 18 June (DDMM)
                text("TIME", "1345"), // 13:45 (HHMM)
            ],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(out.version, Id3Version::V2_4);
        // TYER/TDAT/TIME collapse to a single TDRC; none of the v2.3 ids
        // survive.
        assert!(find_text(&out, "TYER").is_none());
        assert!(find_text(&out, "TDAT").is_none());
        assert!(find_text(&out, "TIME").is_none());
        assert_eq!(
            find_text(&out, "TDRC"),
            Some(&vec!["2024-06-18T13:45".to_string()])
        );
        // Untouched frames survive.
        assert_eq!(find_text(&out, "TIT2"), Some(&vec!["Song".to_string()]));
    }

    #[test]
    fn convert_v23_bare_tyer_yields_year_only_tdrc() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "1999")],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(find_text(&out, "TDRC"), Some(&vec!["1999".to_string()]));
    }

    #[test]
    fn convert_v23_tyer_with_date_no_time() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "2001"), text("TDAT", "0203")],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(
            find_text(&out, "TDRC"),
            Some(&vec!["2001-03-02".to_string()])
        );
    }

    #[test]
    fn convert_v23_time_without_date_is_dropped() {
        // A TIME with no TDAT cannot extend a year-only timestamp (the
        // grammar needs day precision first); the time is dropped.
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "2002"), text("TIME", "0930")],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(find_text(&out, "TDRC"), Some(&vec!["2002".to_string()]));
        assert!(find_text(&out, "TIME").is_none());
    }

    #[test]
    fn convert_v23_malformed_tyer_preserved_companions_dropped() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "20xx"), text("TDAT", "0102")],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        // Malformed year can't anchor a timestamp: TYER survives verbatim,
        // its orphaned date companion is dropped (no standalone v2.4 home).
        assert_eq!(find_text(&out, "TYER"), Some(&vec!["20xx".to_string()]));
        assert!(find_text(&out, "TDRC").is_none());
        assert!(find_text(&out, "TDAT").is_none());
    }

    #[test]
    fn convert_v23_tory_to_tdor() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TORY", "1985")],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert!(find_text(&out, "TORY").is_none());
        assert_eq!(find_text(&out, "TDOR"), Some(&vec!["1985".to_string()]));
    }

    #[test]
    fn convert_v23_trda_and_tsiz_dropped() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![
                text("TRDA", "4th-7th June"),
                text("TSIZ", "1048576"),
                text("TIT2", "keep"),
            ],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert!(find_text(&out, "TRDA").is_none());
        assert!(find_text(&out, "TSIZ").is_none());
        assert_eq!(find_text(&out, "TIT2"), Some(&vec!["keep".to_string()]));
    }

    #[test]
    fn convert_v23_ipls_to_tipl() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![Id3Frame::Ipls {
                pairs: vec![
                    ("producer".to_string(), "Alice".to_string()),
                    ("engineer".to_string(), "Bob".to_string()),
                ],
            }],
        };
        let out = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert!(!out
            .frames
            .iter()
            .any(|f| matches!(f, Id3Frame::Ipls { .. })));
        let tipl = out
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TIPL"))
            .unwrap();
        // Round-trips through the typed accessor as the same pairs.
        assert_eq!(
            tipl.involved_people(),
            Some(vec![
                ("producer".to_string(), "Alice".to_string()),
                ("engineer".to_string(), "Bob".to_string()),
            ])
        );
    }

    #[test]
    fn convert_v24_tdrc_splits_to_tyer_tdat_time() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![text("TDRC", "2024-06-18T13:45")],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        assert_eq!(out.version, Id3Version::V2_3);
        assert_eq!(find_text(&out, "TYER"), Some(&vec!["2024".to_string()]));
        assert_eq!(find_text(&out, "TDAT"), Some(&vec!["1806".to_string()]));
        assert_eq!(find_text(&out, "TIME"), Some(&vec!["1345".to_string()]));
        assert!(find_text(&out, "TDRC").is_none());
    }

    #[test]
    fn convert_v24_year_only_tdrc_yields_only_tyer() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![text("TDRC", "1999")],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        assert_eq!(find_text(&out, "TYER"), Some(&vec!["1999".to_string()]));
        assert!(find_text(&out, "TDAT").is_none());
        assert!(find_text(&out, "TIME").is_none());
    }

    #[test]
    fn convert_v24_date_precision_tdrc_no_time() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![text("TDRC", "2001-03-02")],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        assert_eq!(find_text(&out, "TYER"), Some(&vec!["2001".to_string()]));
        assert_eq!(find_text(&out, "TDAT"), Some(&vec!["0203".to_string()]));
        assert!(find_text(&out, "TIME").is_none());
    }

    #[test]
    fn convert_v24_tdor_to_tory() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![text("TDOR", "1985-12")],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        // TORY is year-only; the month is discarded.
        assert_eq!(find_text(&out, "TORY"), Some(&vec!["1985".to_string()]));
        assert!(find_text(&out, "TDOR").is_none());
    }

    #[test]
    fn convert_v24_drops_tden_tdrl_tdtg_tmcl() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![
                text("TDEN", "2024"),
                text("TDRL", "2024"),
                text("TDTG", "2024"),
                Id3Frame::Text {
                    id: "TMCL".to_string(),
                    values: vec!["Guitar".to_string(), "Jimi".to_string()],
                },
                text("TIT2", "keep"),
            ],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        for id in ["TDEN", "TDRL", "TDTG", "TMCL"] {
            assert!(find_text(&out, id).is_none(), "{id} should be dropped");
        }
        assert_eq!(find_text(&out, "TIT2"), Some(&vec!["keep".to_string()]));
    }

    #[test]
    fn convert_v24_tipl_to_ipls() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIPL".to_string(),
                values: vec![
                    "producer".to_string(),
                    "Alice".to_string(),
                    "engineer".to_string(),
                    "Bob".to_string(),
                ],
            }],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        let ipls = out
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Ipls { .. }))
            .unwrap();
        match ipls {
            Id3Frame::Ipls { pairs } => assert_eq!(
                pairs,
                &vec![
                    ("producer".to_string(), "Alice".to_string()),
                    ("engineer".to_string(), "Bob".to_string()),
                ]
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn convert_roundtrip_v23_to_v24_to_v23_is_stable() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![
                text("TIT2", "Song"),
                text("TYER", "2024"),
                text("TDAT", "1806"),
                text("TIME", "1345"),
                text("TORY", "1985"),
            ],
        };
        let v24 = convert_tag(&tag, Id3Version::V2_4).unwrap();
        let back = convert_tag(&v24, Id3Version::V2_3).unwrap();
        assert_eq!(find_text(&back, "TYER"), Some(&vec!["2024".to_string()]));
        assert_eq!(find_text(&back, "TDAT"), Some(&vec!["1806".to_string()]));
        assert_eq!(find_text(&back, "TIME"), Some(&vec!["1345".to_string()]));
        assert_eq!(find_text(&back, "TORY"), Some(&vec!["1985".to_string()]));
        assert_eq!(find_text(&back, "TIT2"), Some(&vec!["Song".to_string()]));
    }

    #[test]
    fn convert_same_version_is_clone_with_version_set() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "2024"), text("TIT2", "Song")],
        };
        let out = convert_tag(&tag, Id3Version::V2_3).unwrap();
        assert_eq!(out.version, Id3Version::V2_3);
        // No folding happens when source == target.
        assert_eq!(find_text(&out, "TYER"), Some(&vec!["2024".to_string()]));
        assert!(find_text(&out, "TDRC").is_none());
    }

    #[test]
    fn convert_to_version_method_matches_free_fn() {
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "2024")],
        };
        let via_method = tag.to_version(Id3Version::V2_4).unwrap();
        let via_fn = convert_tag(&tag, Id3Version::V2_4).unwrap();
        assert_eq!(find_text(&via_method, "TDRC"), find_text(&via_fn, "TDRC"));
    }

    #[test]
    fn convert_rejects_v22_and_v1() {
        let v22 = Id3Tag {
            version: Id3Version::V2_2,
            frames: vec![],
        };
        assert!(convert_tag(&v22, Id3Version::V2_4).is_err());
        let v24 = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![],
        };
        assert!(convert_tag(&v24, Id3Version::V2_2).is_err());
        assert!(convert_tag(&v24, Id3Version::V1).is_err());
    }

    #[test]
    fn convert_then_write_emits_v24_ids() {
        // End-to-end: convert a v2.3 tag, write it as v2.4, re-parse, and
        // confirm the date landed as TDRC on the wire.
        let tag = Id3Tag {
            version: Id3Version::V2_3,
            frames: vec![text("TYER", "2024"), text("TDAT", "1806")],
        };
        let v24 = convert_tag(&tag, Id3Version::V2_4).unwrap();
        let bytes = write_tag(&v24, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert!(parsed
            .frames
            .iter()
            .any(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TDRC")));
        assert!(!parsed
            .frames
            .iter()
            .any(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TYER")));
    }

    // ---- multi-value text-frame splitting (v2.4 §4.2) ----

    /// A UTF-8 (`$03`) text frame carrying two NUL-separated strings
    /// surfaces as two `values`, with no spurious empty entries.
    #[test]
    fn text_frame_utf8_multi_value_split() {
        let mut payload = vec![3u8];
        payload.extend_from_slice("Alpha".as_bytes());
        payload.push(0);
        payload.extend_from_slice("Beta".as_bytes());
        let f = parse_text_frame("TPE1", &payload);
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(values, vec!["Alpha".to_string(), "Beta".to_string()]);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// ISO-8859-1 (`$00`) with a trailing NUL pad must not produce an
    /// empty second value.
    #[test]
    fn text_frame_latin1_trailing_nul_not_a_value() {
        let mut payload = vec![0u8];
        payload.extend_from_slice(b"Solo");
        payload.push(0);
        let f = parse_text_frame("TIT2", &payload);
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(values, vec!["Solo".to_string()]);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// UTF-16-with-BOM (`$01`) multi-value frame: per the structure
    /// spec each string carries its own BOM ("All strings in the same
    /// frame SHALL have the same byteorder"). Both BOMs must be
    /// stripped — the second value must NOT begin with a literal
    /// U+FEFF.
    #[test]
    fn text_frame_utf16_bom_multi_value_strips_each_bom() {
        fn utf16le_with_bom(s: &str) -> Vec<u8> {
            let mut v = vec![0xFF, 0xFE];
            for u in s.encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v
        }
        let mut payload = vec![1u8]; // enc = UTF-16 with BOM
        payload.extend_from_slice(&utf16le_with_bom("First"));
        payload.extend_from_slice(&[0x00, 0x00]); // $00 00 separator
        payload.extend_from_slice(&utf16le_with_bom("Second"));
        let f = parse_text_frame("TCON", &payload);
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(values, vec!["First".to_string(), "Second".to_string()]);
                // Belt-and-braces: no value retains a leading ZWNBSP.
                assert!(values.iter().all(|v| !v.starts_with('\u{FEFF}')));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// UTF-16BE (`$02`, no BOM) multi-value splits on the even-aligned
    /// `$00 00` terminator and decodes each big-endian segment.
    #[test]
    fn text_frame_utf16be_multi_value_split() {
        fn utf16be(s: &str) -> Vec<u8> {
            let mut v = Vec::new();
            for u in s.encode_utf16() {
                v.extend_from_slice(&u.to_be_bytes());
            }
            v
        }
        let mut payload = vec![2u8]; // enc = UTF-16BE
        payload.extend_from_slice(&utf16be("Eins"));
        payload.extend_from_slice(&[0x00, 0x00]);
        payload.extend_from_slice(&utf16be("Zwei"));
        let f = parse_text_frame("TPE1", &payload);
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(values, vec!["Eins".to_string(), "Zwei".to_string()]);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// A single-value UTF-16BE frame whose payload ends on the
    /// `$00 00` terminator must not surface a trailing empty value.
    #[test]
    fn text_frame_utf16be_single_value_with_terminator() {
        let mut payload = vec![2u8];
        for u in "Title".encode_utf16() {
            payload.extend_from_slice(&u.to_be_bytes());
        }
        payload.extend_from_slice(&[0x00, 0x00]);
        let f = parse_text_frame("TIT2", &payload);
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(values, vec!["Title".to_string()]);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// Round-trip a multi-value v2.4 UTF-8 text frame through the
    /// writer and parser: the writer joins on `$00` and the parser
    /// re-splits, so the value list is preserved exactly.
    #[test]
    fn text_frame_v24_multi_value_round_trip() {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TPE1".to_string(),
                values: vec!["One".to_string(), "Two".to_string(), "Three".to_string()],
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let f = parsed
            .frames
            .iter()
            .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TPE1"))
            .unwrap();
        match f {
            Id3Frame::Text { values, .. } => {
                assert_eq!(
                    values,
                    &vec!["One".to_string(), "Two".to_string(), "Three".to_string()]
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
