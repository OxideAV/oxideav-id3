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
//! * `AENC` audio encryption / `LINK` linked information.
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
    let _revision = buf[4];
    let flags = buf[5];
    let size = synchsafe_u32(buf[6], buf[7], buf[8], buf[9]) as usize;
    let total = ID3V2_HEADER_SIZE + size + if flags & 0x10 != 0 { 10 } else { 0 };
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

    // Extended header: 6 bytes in v2.3 (size is non-synchsafe), 6+
    // bytes in v2.4 (first 4 bytes are synchsafe size INCLUDING those
    // 4 bytes). We just skip it — none of the fields affect frame
    // parsing for our purposes.
    if flags & 0x40 != 0 {
        body = skip_extended_header(version, body)?;
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
            // ETCO / SYLT / POSS / RBUF / SEEK / SIGN / GRID / AENC /
            // LINK carry structured or binary payloads that do not
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
            | Id3Frame::AudioEncryption { .. }
            | Id3Frame::LinkedInfo { .. }
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

fn skip_extended_header(version: Id3Version, body: &[u8]) -> Result<&[u8]> {
    match version {
        Id3Version::V2_3 => {
            if body.len() < 4 {
                return Err(Error::invalid("ID3v2.3 extended header truncated"));
            }
            let ext_size = regular_u32(body[0], body[1], body[2], body[3]) as usize;
            // v2.3: ext_size does NOT include itself, so skip 4 + ext_size.
            let total = 4 + ext_size;
            if total > body.len() {
                return Err(Error::invalid("ID3v2.3 extended header overflows tag"));
            }
            Ok(&body[total..])
        }
        Id3Version::V2_4 => {
            if body.len() < 4 {
                return Err(Error::invalid("ID3v2.4 extended header truncated"));
            }
            let ext_size = synchsafe_u32(body[0], body[1], body[2], body[3]) as usize;
            // v2.4: ext_size INCLUDES itself, so skip ext_size bytes total.
            if ext_size < 4 || ext_size > body.len() {
                return Err(Error::invalid("ID3v2.4 extended header size invalid"));
            }
            Ok(&body[ext_size..])
        }
        _ => Ok(body),
    }
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
        "AENC" => parse_aenc(payload),
        "LINK" => parse_link(payload),
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

    let mut body = Vec::new();
    for frame in &tag.frames {
        write_frame(target_version, frame, &mut body)?;
    }

    let size = body.len();
    if size >= 1 << 28 {
        return Err(Error::invalid(
            "ID3v2 tag body exceeds the 28-bit synchsafe size limit",
        ));
    }

    let mut out = Vec::with_capacity(ID3V2_HEADER_SIZE + size);
    out.extend_from_slice(b"ID3");
    out.push(major);
    out.push(0); // revision
    out.push(0); // flags: no unsync, no extended header, no footer, no experimental
    let s = size as u32;
    out.push(((s >> 21) & 0x7F) as u8);
    out.push(((s >> 14) & 0x7F) as u8);
    out.push(((s >> 7) & 0x7F) as u8);
    out.push((s & 0x7F) as u8);
    out.extend_from_slice(&body);
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

fn write_frame(version: Id3Version, frame: &Id3Frame, out: &mut Vec<u8>) -> Result<()> {
    let (id, payload) = encode_frame(version, frame)?;
    let mut id4 = [0u8; 4];
    let id_bytes = id.as_bytes();
    if id_bytes.len() != 4 || !id_bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::invalid(format!("invalid frame id for writer: {id}")));
    }
    id4.copy_from_slice(id_bytes);
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
    out.extend_from_slice(&[0, 0]); // status + format flags
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
}
