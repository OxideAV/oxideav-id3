# oxideav-id3

Pure-Rust **ID3** metadata tag parser and writer — ID3v1 / ID3v1.1
trailers and ID3v2 (2.2 / 2.3 / 2.4) headers. Handles whole-tag and
per-frame unsynchronisation, the v2.4 data-length indicator, and
extended headers. Zero C dependencies.

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace)
framework but usable standalone.

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-id3  = "0.0"
```

## Reading a tag

For an MP3 file the tag usually sits at the head of the file. Peek the
first 10 bytes to size the read, pull the whole tag into memory, then
parse:

```rust
use oxideav_id3::{parse_tag, tag_size_at_head, to_key_value_pairs, attached_pictures};

let mut file = std::fs::File::open("song.mp3")?;
let mut head = [0u8; 10];
use std::io::Read;
file.read_exact(&mut head)?;

if let Some(total) = tag_size_at_head(&head) {
    let mut buf = vec![0u8; total];
    buf[..10].copy_from_slice(&head);
    file.read_exact(&mut buf[10..])?;
    let (tag, _consumed) = parse_tag(&buf)?;

    // Flat (key, value) pairs with the Vorbis-comment-style keys the
    // rest of the workspace uses.
    for (k, v) in to_key_value_pairs(&tag) {
        println!("{k} = {v}");
    }

    // Any APIC / PIC frames as AttachedPicture values.
    for pic in attached_pictures(&tag) {
        println!("{} ({:?}, {} bytes)", pic.mime_type, pic.picture_type, pic.data.len());
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

ID3v1 trailers (128 bytes at the end of a file) are handled by a
separate entry point:

```rust
let last_128 = &file_bytes[file_bytes.len() - 128..];
if let Some(tag) = oxideav_id3::parse_id3v1(last_128) {
    // tag.version == Id3Version::V1
}
```

## Writing a tag

Build an `Id3Tag` in memory and serialise it with `write_tag` (for
v2.3 / v2.4) or `write_id3v1` (for the 128-byte trailer):

```rust
use oxideav_core::{AttachedPicture, PictureType};
use oxideav_id3::{write_tag, write_id3v1, Id3Frame, Id3Tag, Id3Version};

let tag = Id3Tag {
    version: Id3Version::V2_4,
    frames: vec![
        Id3Frame::Text { id: "TIT2".into(), values: vec!["Song".into()] },
        Id3Frame::Text { id: "TPE1".into(), values: vec!["Artist".into()] },
        Id3Frame::Text { id: "TALB".into(), values: vec!["Album".into()] },
        Id3Frame::Picture(AttachedPicture {
            mime_type: "image/jpeg".into(),
            picture_type: PictureType::FrontCover,
            description: String::new(),
            data: std::fs::read("cover.jpg")?,
        }),
    ],
};

let header_bytes = write_tag(&tag, Id3Version::V2_4)?;   // starts with b"ID3"
let trailer_bytes = write_id3v1(&tag);                   // 128 bytes, starts with b"TAG"
# Ok::<(), Box<dyn std::error::Error>>(())
```

Text frames are emitted UTF-8 in v2.4 and UTF-16-with-BOM in v2.3 so
non-ASCII values survive both. Multi-value text frames use NUL in v2.4
and `/` in v2.3, matching what the parser splits on. `Id3Frame::Unknown`
payloads round-trip verbatim so frames the parser did not understand
are preserved on write.

### Unsynchronisation on write

`write_tag` defaults to no unsynchronisation — the body is written
verbatim. To produce a tag that hides the MPEG sync pattern from
naive decoders (spec §6.1), build a `WriteOptions` and call
`write_tag_with_options`:

```rust
use oxideav_id3::{write_tag_with_options, Id3Tag, Id3Version, UnsyncMode, WriteOptions};

# let tag = Id3Tag { version: Id3Version::V2_4, frames: vec![] };
// Whole-tag unsync: header flag 0x80 set, the entire serialised body
// is passed through the unsync transform (`0xFF` → `0xFF 0x00`
// whenever the next byte would otherwise form a false sync, be a
// literal `0x00`, or run off the end).
let opts = WriteOptions::new().with_unsync(UnsyncMode::WholeTag);
let bytes_v24 = write_tag_with_options(&tag, Id3Version::V2_4, &opts)?;

// Per-frame unsync (v2.4-only): each frame's payload is
// unsynchronised independently and the per-frame format-flag bit
// 0x02 is set. Selecting `PerFrame` under a v2.3 target falls back
// to `WholeTag` silently (v2.3 has no per-frame format-flag bit
// for unsync).
let per_frame = WriteOptions::new().with_unsync(UnsyncMode::PerFrame);
let bytes_per_frame = write_tag_with_options(&tag, Id3Version::V2_4, &per_frame)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`parse_tag` reverses unsync transparently regardless of which mode
produced the bytes, so the round-trip `write_tag_with_options →
parse_tag` is the identity on the tag's frame contents for all
three `UnsyncMode` values.

### Extended-header CRC

`WriteOptions::with_crc(true)` adds an ID3v2 extended header carrying
a CRC-32 [ISO-3309] over the frame area (spec §3.2 in both v2.3 and
v2.4):

```rust
use oxideav_id3::{write_tag_with_options, Id3Tag, Id3Version, WriteOptions};

# let tag = Id3Tag { version: Id3Version::V2_4, frames: vec![] };
let opts = WriteOptions::new().with_crc(true);
let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The writer emits 14 bytes of extended header for v2.3 (size=10
exclusive of itself, flags `0x80 0x00`, size-of-padding = 0, regular
4-byte CRC) and 12 bytes for v2.4 (synchsafe size = 12 inclusive,
flag-count = 1, flags = 0x20, data-length = 5, 5-byte synchsafe CRC).
`parse_tag` recognises both forms, verifies the stored CRC against the
spec-defined region — frames-only for v2.3, frames + padding for v2.4 —
and returns an error on mismatch rather than silently accepting a
corrupted tag. `WriteOptions::with_crc(true)` and
`WriteOptions::with_unsync(...)` compose: the writer computes the CRC
on the pre-unsync frame bytes (matching v2.3's "calculated before
unsynchronisation"), then runs unsync over the concatenated extended
header + frames; the parser reverses unsync first and so verifies the
CRC against the same byte sequence.

### ID3v2.4 footer

`WriteOptions::with_footer(true)` emits the 10-byte trailer described
in spec §3.4 — a copy of the 10-byte header but with identifier `3DI`
instead of `ID3`, used to locate a tag that was appended after the
audio data on a reverse scan from end-of-file:

```rust
use oxideav_id3::{write_tag_with_options, Id3Tag, Id3Version, WriteOptions};

# let tag = Id3Tag { version: Id3Version::V2_4, frames: vec![] };
let opts = WriteOptions::new().with_footer(true);
let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts)?;
// bytes[5] & 0x10 == 0x10 (footer-present flag set)
// &bytes[bytes.len() - 10..bytes.len() - 7] == b"3DI"
# Ok::<(), Box<dyn std::error::Error>>(())
```

Footer is a v2.4-only construct. Requesting it on a v2.3 target
returns `Error::unsupported` rather than silently dropping the flag.
On the parse side, `parse_tag` requires the trailer's `3DI` identifier
and validates that the footer's version, flags, and synchsafe size
match the header byte-for-byte — a corrupted, version-mismatched, or
size-mismatched trailer is rejected with a specific error. A buffer
that announces a footer but is short of the 10 trailer bytes returns
`Error::NeedMore` so callers can read more. `with_footer` composes
freely with `with_crc` and `with_unsync(WholeTag | PerFrame)`: the
footer lives outside the announced synchsafe body size, so unsync
never touches it and the CRC region is unchanged. `tag_size_at_head`
already reports footer-inclusive totals so a one-shot 10-byte file
peek still sizes the right number of bytes to read.

### Extended-header sub-fields (v2.4)

ID3v2.4 §3.2 defines two optional extended-header sub-fields beyond
the CRC: the `b` "Tag is an update" flag and the `d` restrictions
byte. Both are surfaced as typed `WriteOptions` builders on emission
and recovered from a tag via the richer `parse_tag_with_extended_header`
entry point:

```rust
use oxideav_id3::{
    parse_tag_with_extended_header, write_tag_with_options, Id3Tag, Id3Version,
    ImageEncodingRestriction, ImageSizeRestriction, Restrictions, TagSizeRestriction,
    TextEncodingRestriction, TextFieldsRestriction, WriteOptions,
};

# let tag = Id3Tag { version: Id3Version::V2_4, frames: vec![] };
let restrictions = Restrictions {
    tag_size: TagSizeRestriction::Max64Frames128Kb,
    text_encoding: TextEncodingRestriction::Iso8859OrUtf8,
    text_fields: TextFieldsRestriction::Max128Chars,
    image_encoding: ImageEncodingRestriction::PngOrJpeg,
    image_size: ImageSizeRestriction::Max256x256,
};
let opts = WriteOptions::new()
    .with_crc(true)
    .with_update(true)
    .with_restrictions(Some(restrictions));
let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts)?;

let (tag, ext, _consumed) = parse_tag_with_extended_header(&bytes)?;
assert!(ext.is_update);
assert_eq!(ext.crc.is_some(), true);
assert_eq!(ext.restrictions, Some(restrictions));
# Ok::<(), Box<dyn std::error::Error>>(())
```

`parse_tag` is unchanged; the new `parse_tag_with_extended_header`
returns the same `(Id3Tag, usize)` plus an `ExtendedHeader` carrying
`is_update`, the verified `crc`, and the typed `Restrictions`. Per
spec the restrictions byte is advisory ("does not affect how the
tag is decoded, merely how it was restricted before encoding"); the
parser preserves it losslessly without enforcing the limits and the
writer emits whatever the caller supplied. Both `with_update` and
`with_restrictions(Some(_))` are v2.4-only — a v2.3 target returns
`Error::unsupported` rather than silently dropping the request.

### Frame-level compression

Both ID3v2 dialects define a per-frame compression flag over the zlib
deflate stream: v2.3 format flag `i` (spec §3.3, with 4 big-endian
bytes of decompressed size appended to the frame header) and v2.4
format flag `k` (spec §4.1.2, which makes the data-length-indicator
bit mandatory and carries the decompressed size as a 32-bit synchsafe
integer). `parse_tag` inflates flagged frames transparently in both
versions and then dispatches the recovered payload structurally, so a
compressed `TIT2` parses to the same `Id3Frame::Text` a plain one
does. The announced decompressed size is authoritative: a stream that
inflates to any other length is rejected as corruption (the frame is
dropped, earlier frames survive), and the announce doubles as the
allocation cap — bounded by a 64 MiB per-frame ceiling — so a zlib
bomb can't force a huge allocation. A v2.4 compressed frame missing
the spec-mandated data-length indicator is treated as malformed.

On the writer side, `WriteOptions::with_compression(true)` deflates
every frame's payload and emits the per-version flag + size field:

```rust
use oxideav_id3::{write_tag_with_options, Id3Tag, Id3Version, WriteOptions};

# let tag = Id3Tag { version: Id3Version::V2_4, frames: vec![] };
let opts = WriteOptions::new().with_compression(true);
let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts)?;
// per-frame format flags carry 0x08 (compression) | 0x01 (DLI) in
// v2.4, or 0x80 in v2.3.
# Ok::<(), Box<dyn std::error::Error>>(())
```

Compression is applied to every frame unconditionally for
deterministic output (the spec gives no size policy; a tiny text
frame may grow by the ~11-byte zlib envelope) and composes with the
other options: per-frame unsync runs after compression, the
extended-header CRC covers the post-compression frame bytes, and
whole-tag unsync wraps the finished body — `parse_tag` reverses the
layers in the opposite order so the round-trip stays the identity.

The same change wired up the rest of the v2.3 format-flags byte,
which was previously ignored wholesale: the grouping-identity byte
(flag `k`) is stripped per the spec's addition ordering rather than
corrupting the payload offset, and an encrypted frame (flag `j`)
surfaces as `Id3Frame::Unknown` with the method byte + ciphertext
preserved — matching the v2.4 posture — instead of dispatching
ciphertext to a structural parser.

## Vorbis-style flat-pair view

`to_key_value_pairs(&tag)` projects an `Id3Tag` onto a
`Vec<(String, String)>` keyed in the Vorbis-comment convention the
rest of the workspace uses (`title`, `artist`, `album`, `date`,
`genre`, `track`, `composer`, …). The mapping covers the v2.4
spec §4.2.1 / §4.2.2 / §4.2.3 / §4.2.4 / §4.2.5 text frames —
including the timestamp-class frames (`TDEN` → `encodingtime`,
`TDTG` → `taggingtime`), the §4.2.4 rights / radio frames (`TOWN` →
`owner`, `TPRO` → `producednotice`, `TRSN` → `radiostation`,
`TRSO` → `radiostationowner`), the §4.2.5 sort-order frames
(`TSOA` → `albumsort`, `TSOP` → `artistsort`, `TSOT` → `titlesort`),
`TSST` → `setsubtitle`, `TMOO` → `mood`, `TFLT` → `filetype`,
`TLEN` → `length`, `TDLY` → `playlistdelay`, `TOFN` →
`originalfilename` — plus the v2.3-only `TDAT` → `date_ddmm`,
`TIME` → `time_hhmm`, `TRDA` → `recordingdates`, `TSIZ` → `size`
that v2.4 folded into `TDRC` or removed. Unknown `T???` frames
still fall through to the lowercased frame id so a Vorbis consumer
never silently drops data.

## What is supported

- **ID3v1 / ID3v1.1** — parse + write 128-byte trailers, Winamp's
  genre byte range, v1.1 track number.
- **ID3v2.2** — parse only (read-only legacy), covering the complete
  ID3v2.2.0 §4 frame table. Frame ids are promoted to their v2.3
  equivalents (`TT2 -> TIT2`, `PIC -> APIC`, ...): all `T**` text and
  `W**` URL ids plus `UFI`, `IPL`, `MCI`, `ETC`, `MLL`, `STC`, `SLT`,
  `ULT`, `COM`, `RVA`, `EQU`, `REV`, `PIC`, `GEO`, `CNT`, `POP`,
  `BUF`, `CRA`, and `LNK` land in the same typed `Id3Frame` variants
  their 4-char descendants use. Two need v2.2-specific walkers: `RVA`
  (§4.12) carries its right/left volume-change fields unconditionally
  — presence is not keyed on the inc/dec sign bits the way v2.3
  `RVAD` gates its front block — and `LNK` (§4.22) always carries a
  3-byte linked-frame id, so no 3-vs-4-byte heuristic applies. `CRM`
  (encrypted meta frame, §4.20) has no v2.3/v2.4 descendant and is
  preserved verbatim via `Id3Frame::Unknown`. The v2.2 header
  compression bit (§3.1 flag bit 6, a tag-wide scheme the spec never
  defined: "the ID3 decoder (for now) should just ignore the entire
  tag") yields an empty frame list while still reporting the correct
  consumed size so container callers can seek past the tag.
- **ID3v2.3 / ID3v2.4** — parse + write. Whole-tag unsync, per-frame
  unsync, data-length indicator, and extended headers are handled on
  read; the extended-header CRC-32 is verified against the spec-defined
  region (frames-only in v2.3, frames + padding in v2.4) and parse
  fails on mismatch. Frame-level zlib compression is decoded in both
  dialects (v2.3 §3.3 flag `i` / v2.4 §4.1.2 flag `k`) and emitted via
  `WriteOptions::with_compression(true)`; the v2.3 grouping-identity
  and encryption header additions are stripped per spec order. The
  writer can emit a CRC-bearing extended header via
  `WriteOptions::with_crc(true)`, the v2.4 "Tag is an update"
  flag via `with_update(true)`, and the v2.4 restrictions byte via
  `with_restrictions(Some(_))`. All three sub-fields surface through
  `parse_tag_with_extended_header` as a typed `ExtendedHeader`
  struct. Footer-bearing tags are sized correctly.
- Common frames: `T***` text, `TXXX` user-defined text, `W***` URL,
  `WXXX` user-defined URL, `COMM` comment, `USLT` lyrics, `APIC` /
  `PIC` attached picture.
- Structured non-text frames: `POPM` popularimeter (email + rating
  byte + play counter, wide-counter aware), `PCNT` play counter,
  `PRIV` private frame (owner + binary payload), `GEOB` general
  encapsulated object (MIME / filename / description / bytes),
  `UFID` unique file identifier (owner + binary id), `USER` terms
  of use (language + free text), `OWNE` ownership (currency-prefixed
  price + 8-byte purchase date + seller), `COMR` commercial offer
  (price + validity date + contact URL + delivery method + seller +
  description + optional company logo), `SYTC` synchronised tempo
  codes (time-format + `(BPM, timestamp)` pairs with the spec's
  `$FF`-extension form for BPMs above 255), `RVA2` relative volume
  adjustment 2 (per-channel Q9.7 dB + variable-width peak), `EQU2`
  equalisation 2 (interpolation byte + `(frequency, adjustment)`
  pairs in spec units), `MCDI` music CD identifier (opaque TOC),
  `ETCO` event timing codes (time-format + `(event_type, timestamp)`
  pairs), `SYLT` synchronised lyrics (language + time-format +
  content-type + `(syllable, timestamp)` pairs, both v2.3-UTF-16
  and v2.4-UTF-8), `POSS` position synchronisation (time-format +
  32-bit position), `RBUF` recommended buffer size (24-bit buffer
  + embedded-info flag + 32-bit next-tag offset, auto-clamped on
  write), `SEEK` seek frame (32-bit next-tag offset), `SIGN`
  signature frame (group-symbol byte + binary signature), `GRID`
  group identification registration (owner + group-symbol byte +
  optional group-dependent data), `ENCR` encryption method
  registration (owner + method-symbol byte + optional
  encryption-specific data), `AENC` audio encryption (owner +
  preview start/length + opaque encryption-info), `LINK` linked
  information (3-byte v2.3 / 4-byte v2.4 frame-id + URL + spec-shaped
  additional data), `ASPI` audio seek point index (v2.4 §4.30:
  indexed-data start + length + 16-bit N + 8/16 bits-per-point + N
  `Fi` fractions; writer refuses non-8/16 bit widths since a
  conformant parser couldn't reconstruct them), `MLLT` MPEG location
  lookup table (v2.3 §4.7 / v2.4 §4.6: u16 mpeg-frames-between-ref +
  3 × 24-bit fields + two bit-width bytes + N references packed
  MSB-first across byte boundaries with `(bytes_dev_bits +
  ms_dev_bits)` constrained to a multiple of 4; widths capped at 32
  bits per field so each reference fits in `(u32, u32)`; writer
  rejects non-multiple-of-four sums, 24-bit-field overflows, and
  per-reference values that exceed the declared width), `RVRB` reverb
  (v2.3 §4.13 / v2.4 §4.13: fixed twelve-byte payload — u16 BE delays
  left/right, u8 bounce counts left/right with `$FF` = infinite, four
  u8 feedback bytes L→L / L→R / R→R / R→L on the `$00..$FF` 0..100%
  scale, two u8 premix bytes L→R / R→L; v2.2 `REV` promotes to the
  same structured variant), `RVAD` relative volume adjustment
  (v2.3 §4.12: shared inc/dec bitfield carrying both presence and sign
  per channel + `bits_used` width byte + spec-ordered blocks for front
  / back / centre / bass, each block writing all deltas first then all
  optional peaks; writer pads sub-byte widths on the high end per
  spec, refuses `bits_used = $00`, refuses inc/dec bitfield vs
  `Option` block mismatches and out-of-spec extension orderings, and
  refuses to serialise under a v2.4 envelope since v2.4 dropped `RVAD`
  in favour of `RVA2`), `EQUA` equalisation (v2.3 §4.13: 1-byte
  `adjustment_bits` width prefix + `(increment_decrement bit, 15-bit
  frequency, ceil(adjustment_bits/8)-byte BE adjustment magnitude)`
  bands in strictly-ascending frequency order; writer enforces the
  ordering + uniqueness rules per spec "ordered increasingly with
  reference to frequency" and "a frequency should only be described
  once in the frame", refuses `adjustment_bits = $00`, refuses
  frequencies that collide with the inc/dec bit (`>= 0x8000`), refuses
  over-wide adjustments, and refuses to serialise under a v2.4
  envelope since v2.4 dropped `EQUA` in favour of `EQU2`; v2.2 `EQU`
  promotes to the same structured variant), `IPLS` involved people list
  (v2.3 §4.4: encoding byte + alternating NUL-terminated
  `(involvement, involvee)` pairs in the declared encoding; pairs are
  stored as `Vec<(String, String)>` so a writer can never emit an odd
  count, a non-conforming trailing involvement with no involvee folds
  into a pair with an empty involvee, and the writer refuses to
  serialise under a v2.4 envelope since v2.4 dropped `IPLS` in favour
  of the `TIPL` text frame and the new `TMCL` musician-credits list).
  All twenty-eight round-trip both directions for v2.3 and v2.4 except
  `RVAD`, `EQUA`, and `IPLS` which are v2.3-only by spec (ASPI is
  v2.4-only per spec but the wire layout is byte-aligned and
  version-independent; RVRB is byte-aligned and version-independent as
  well).
- Everything else surfaces as `Id3Frame::Unknown { id, raw }` with the
  payload preserved so it can be written back untouched.
- `Id3Frame::timestamp_unit()` returns a typed `TimestampUnit`
  (`MpegFrames` / `Milliseconds`) for the time-stamp-format byte
  carried by `ETCO`, `SYTC`, `SYLT`, and `POSS`. The wire byte is
  identical between v2.3 and v2.4 (spec §4.10 vs §4.9 for SYLT, and
  the matching sections for the other three), so the logical unit
  round-trips losslessly when a tag is re-serialised under the other
  version.
- `Id3Frame::involved_people()` returns the spec §4.2.2
  `(role, name)` pairs carried by the v2.4 `TIPL` text frame
  (involved-people list) and the v2.3 `IPLS` structural frame as a
  single `Vec<(String, String)>`. The on-wire layout for `TIPL` is the
  text-frame encoding byte followed by alternating NUL-terminated
  strings (`role_0\0 name_0\0 …`); the existing text-frame parser
  already splits on NUL into `values`, and the accessor folds adjacent
  entries back into pairs. A non-conforming odd-count source (trailing
  role with no name) folds into a pair with an empty name, matching
  how the parser surfaces the same truncation on the `IPLS` side.
  Surfacing both variants through one accessor lets a caller handle
  either source version without matching on the underlying variant,
  matching the cross-version posture of `timestamp_unit()`.
- `Id3Frame::musician_credits()` returns the spec §4.2.2
  `(instrument, performer)` pairs carried by the v2.4 `TMCL` text
  frame. The wire layout matches `TIPL` but the logical mapping is
  distinct (instrument-to-musician rather than function-to-name) so
  the two accessors stay separate; `musician_credits()` returns `None`
  on `TIPL` / `IPLS` and `involved_people()` returns `None` on `TMCL`.
  `TMCL` is v2.4-only — v2.3's `IPLS` mixes both kinds of pair into a
  single frame, so a caller migrating a v2.3 tag to v2.4 reads the
  union via `involved_people()` from `IPLS`, splits roles vs
  instruments by inspection, then writes back as separate `TIPL` and
  `TMCL` text frames.
- `Id3Frame::sylt_content_type()` returns a typed `SyltContentType`
  (`Other` / `Lyrics` / `TextTranscription` / `MovementPartName` /
  `Events` / `Chord` / `Trivia` / `UrlsToWebpages` / `UrlsToImages`)
  for the `SYLT` content-type byte (spec v2.3 §4.10 / v2.4 §4.9). The
  byte sits between the time-stamp format and the content descriptor
  and categorises what kind of synchronised text the frame carries
  (song lyrics, chord names, movement labels, …); the typed accessor
  lets callers route on the categorical meaning without re-decoding
  the raw `u8`. `from_wire` / `to_wire` form a bijection over the
  spec range `$00..=$08` — any reserved byte surfaces structurally as
  `None` rather than mapping to a guessed variant, matching the
  cross-version posture of [`Id3Frame::timestamp_unit`].
- `Id3Frame::commercial_delivery()` returns a typed
  `CommercialDelivery` (`Other` / `StandardCdAlbum` /
  `CompressedAudioOnCd` / `FileOverInternet` / `StreamOverInternet` /
  `NoteSheets` / `NoteSheetsInBook` / `MusicOnOtherMedia` /
  `NonMusicalMerchandise`) for the `COMR` "received as" byte (spec
  v2.3 §4.25 / v2.4 §4.24). The byte describes how the audio is
  delivered when bought; the wire mapping is identical between v2.3
  and v2.4 so the accessor is version-independent. As with the SYLT
  accessor, `from_wire` / `to_wire` are a bijection over the spec
  range `$00..=$08` and any reserved byte surfaces as `None`.
- `Rva2Channel::channel_type_typed()` returns a typed
  `Rva2ChannelType` (`Other` / `MasterVolume` / `FrontRight` /
  `FrontLeft` / `BackRight` / `BackLeft` / `FrontCentre` /
  `BackCentre` / `Subwoofer`) for the channel-type byte that opens
  each per-channel record inside an `RVA2` payload (spec v2.4 §4.11).
  The byte names the channel the volume adjustment applies to; the
  wire layout is byte-aligned and version-independent so the accessor
  is shared between v2.3 and v2.4. As with the SYLT and COMR
  accessors, `from_wire` / `to_wire` are a bijection over the spec
  range `$00..=$08` and any reserved byte surfaces as `None`. The
  raw `Rva2Channel::channel_type: u8` field is unchanged and
  round-trips losslessly through the writer, so a non-conforming
  source can still preserve a forward-compatible payload while the
  typed view collapses to `None`.
- `Id3Frame::equ2_interpolation()` returns a typed
  `Equ2Interpolation` (`Band` / `Linear`) for the 1-byte interpolation
  method that opens the `EQU2` payload (spec v2.4 §4.12). The byte
  describes which curve a renderer should draw between two adjacent
  `(frequency, adjustment)` points (`$00` Band = no interpolation,
  jump in the middle between two adjustment points; `$01` Linear =
  interpolate linearly). EQU2 is v2.4-only per spec — v2.3 carried
  `EQUA` instead with an unrelated per-band inc/dec bitfield — so the
  accessor is version-locked to v2.4 by virtue of its source variant.
  As with the SYLT, COMR, and RVA2 channel-type accessors,
  `from_wire` / `to_wire` form a bijection over the spec range
  `$00..=$01` and any reserved byte surfaces as `None`. The raw
  `Id3Frame::Equ2::interpolation: u8` field is unchanged and
  round-trips losslessly through the writer, so a non-conforming
  source can still preserve a forward-compatible payload while the
  typed view collapses to `None`.
- `Id3Frame::etco_event_types()` returns a typed
  `Vec<Option<EtcoEventType>>` for the per-event "type of event" bytes
  carried by an `ETCO` payload (spec v2.3 §4.6 / v2.4 §4.5). The
  returned vector matches the source `events` vector positionally so a
  caller can `.zip` it against the raw timestamps without losing the
  per-event ordering. `EtcoEventType` mirrors the spec's value table
  verbatim — 23 named events `$00..=$16` (`Padding` /
  `EndOfInitialSilence` / `IntroStart` / `MainPartStart` /
  `OutroStart` / `OutroEnd` / `VerseStart` / `RefrainStart` /
  `InterludeStart` / `ThemeStart` / `VariationStart` / `KeyChange` /
  `TimeChange` / `MomentaryUnwantedNoise` / `SustainedNoise` /
  `SustainedNoiseEnd` / `IntroEnd` / `MainPartEnd` / `VerseEnd` /
  `RefrainEnd` / `ThemeEnd` / `Profanity` / `ProfanityEnd`), a
  `NotPredefinedSync(u8)` variant for the `$E0..=$EF` user-defined
  synchronisation range (the inner `u8` carries the low nibble as the
  slot index `0..=15`), the `$FD` / `$FE` audio-end markers
  (`AudioEnd` / `AudioFileEnds`), and the `$FF` continuation marker
  (`Continuation`) the spec describes as "one more byte of events
  follows". The wire byte is identical between v2.3 and v2.4 — the
  event-type table is reproduced bit-for-bit in both version docs —
  so the accessor is version-independent. As with the SYLT, COMR,
  RVA2 channel-type, and EQU2 interpolation accessors, `from_wire` /
  `to_wire` form a bijection over the spec range; bytes in the two
  reserved ranges (`$17..=$DF`, `$F0..=$FC`) surface as `None`. The
  raw `Id3Frame::EventTimingCodes::events: Vec<(u8, u32)>` field is
  unchanged and round-trips losslessly through the writer for every
  byte value — including reserved bytes — so the typed view never
  costs callers the ability to preserve forward-compatible payloads.
- `Id3Frame::sytc_tempo_codes()` returns a typed
  `Vec<Option<SytcTempo>>` for the per-record tempo values carried by
  a `SYTC` payload (spec v2.4 §4.7). The returned vector matches the
  source `codes` vector positionally so a caller can `.zip` it against
  the raw timestamps without losing per-record ordering. `SytcTempo`
  carries the three categorical meanings the spec assigns to the
  tempo byte: `BeatFree` (`$00`, "a beat-free time period, which is
  not the same as a music-free time period"), `SingleStroke` (`$01`,
  "one single beat-stroke followed by a beat-free period"), and
  `Bpm(u16)` (the actual BPM in the spec-defined range `2..=510`).
  The wire-level one-byte vs `$FF $xx` two-byte split is already
  normalised in `Id3Frame::SyncedTempo::codes` so the typed view
  stays at the logical layer. `from_wire` / `to_wire` form a
  bijection over the spec range; any value beyond `510` (which the
  wire format cannot represent but the raw field could carry)
  surfaces as `None`, matching the contract on the SYLT, COMR, RVA2
  channel-type, EQU2 interpolation, and ETCO event-type accessors.
  The raw `Id3Frame::SyncedTempo::codes: Vec<(u16, u32)>` field is
  unchanged and round-trips losslessly through the writer for every
  value the wire format can represent, so the typed view never costs
  callers the ability to preserve forward-compatible payloads.
- `Id3Frame::content_types()` returns a typed `Vec<ContentType>` for
  the `TCON` content-type (genre) frame (spec v2.3 §4.2.1 / v2.4
  §4.2.3). The frame carries one or several content-type references in
  one string; the two version dialects share a vocabulary but frame it
  differently and the accessor normalises both. v2.3 references are
  parenthesised — `(21)` is an ID3v1 numeric genre reference, `(RX)` /
  `(CR)` the Remix / Cover keywords, `(4)Eurodisco` a numeric reference
  plus a free-text refinement, `(51)(39)` two references in one string,
  and `((` an escape for a literal-`(` custom genre. v2.4 dropped the
  parentheses — a numeric content type is a bare number, `RX` / `CR`
  are bare keywords, and the text-frame NUL list separates multiple
  references. `ContentType` collapses both onto `Genre { index, name }`
  (numeric reference resolved against the same Winamp-extended ID3v1
  genre table the v1 trailer uses, `name: None` for an out-of-table
  index so a forward-compatible reference surfaces structurally rather
  than being dropped), `Remix`, `Cover`, and `Custom(String)` for
  free-text genres. The raw `Id3Frame::Text::values` is unchanged and
  round-trips losslessly through the writer, so the typed view never
  costs callers the ability to preserve the exact on-wire string,
  matching the forward-compatible posture of `etco_event_types()` and
  `sytc_tempo_codes()`.
- `Id3Frame::media_type()` returns a typed `Vec<MediaType>` for the
  `TMED` media-type frame (spec v2.3 §4.6.3 / v2.4 §4.2.3). The frame
  "describes from which media the sound originated" as either free text
  or a reference to the spec's predefined media list, and the two version
  dialects frame the reference differently — the accessor normalises both,
  exactly as `content_types()` does for `TCON`. v2.3 wraps a reference in
  `(...)` optionally followed by a free-text refinement (`(MC) with four
  channels` → `Predefined { media: "MC", text: Some(" with four
  channels") }`; `(VID/PAL/VHS)` → `media: "VID"`, `refinements: ["PAL",
  "VHS"]`), with `((` escaping a literal-`(` free-text name; v2.4 dropped
  the parentheses so its bare example `VID/PAL/VHS` parses to the same
  reference. The 15 predefined top-level codes
  (`DIG`/`ANA`/`CD`/`LD`/`TT`/`MD`/`DAT`/`DCC`/`DVD`/`TV`/`VID`/`RAD`/`TEL`/`MC`/`REE`)
  resolve to their spec descriptions via `MediaType::Predefined { name }`;
  an out-of-table top-level code surfaces structurally with `name: None`
  so a forward-compatible reference is preserved rather than dropped, and
  a free-text value surfaces as `MediaType::Custom`. The raw
  `Id3Frame::Text::values` is unchanged and round-trips losslessly through
  the writer, matching the posture of `content_types()`.
- `Id3Frame::language()` returns a typed `Language` for the three-byte
  language field carried by the language-tagged frames (`COMM`, `USLT`,
  `USER`, `SYLT`), per the structure doc's "three byte language field …
  according to ISO-639-2 … should be represented in lower case … 'XXX'
  if not known". The view has three states: `Unknown` for the `XXX`
  sentinel (matched case-insensitively), `Code([u8; 3])` for a
  well-formed three-letter code normalised to lower case (so `Eng` /
  `eng` / `ENG` all compare equal, with `as_code()` exposing the
  `&str`), and `Malformed([u8; 3])` for anything else — non-alphabetic
  bytes or the all-`$00` padding written for an absent language — with
  the raw bytes preserved verbatim. The field is identical across v2.3
  and v2.4 (only the v2.4-specific lower-case recommendation and `XXX`
  sentinel apply, both folded into the typed view), so the accessor is
  version-independent; `from_wire` / `to_wire` round-trip the decoder's
  own outputs, and the underlying `lang: [u8; 3]` field is untouched so
  callers keep the exact on-wire bytes. Matches the cross-version,
  non-destructive posture of `timestamp_unit()`.

## Fuzzing

A [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) harness lives
in `fuzz/`. The `parse` target drives attacker-controlled bytes through
`tag_size_at_head`, `parse_tag`, `parse_id3v1`, `to_key_value_pairs`,
`attached_pictures`, `write_id3v1`, and both `write_tag` targets
(v2.3 + v2.4) — every public surface that turns bytes into an `Id3Tag`
or back out. The contract is panic-freedom on any input: a malformed
stream yields `Err(_)` / `None`, never an OOB index, debug-overflow
panic, or 256-MiB allocation from a synchsafe-size announce. Sustained
runs (≥ 15 M iterations) under libFuzzer find nothing.

A curated seed corpus under `fuzz/corpus/parse/` (minimal v2.2 / v2.3 /
v2.4 text tags, a mixed COMM/UFID/TXXX/APIC v2.4 tag, a v2.3
extended-header tag, a whole-tag-unsync v2.4 tag, and an ID3v1 trailer)
drives the fuzzer straight into the real parse paths rather than
spending budget rediscovering the `ID3` magic. A daily scheduled `Fuzz`
CI workflow (`.github/workflows/fuzz.yml`) runs the target for a
30-minute budget.

```sh
cd fuzz && cargo +nightly fuzz run parse
```

## Benchmarks

A Criterion harness lives at `benches/id3.rs` and covers the three
public surfaces a typical caller exercises on an MP3-resident tag:
`parse_tag` (under both structural-overhead and picture-copy
shapes), `write_tag`, and `parse_id3v1`. Every input fixture is
hand-built in the bench from the wire layout described in spec §3
and §4, and the 60 KiB APIC payload is seeded by a fixed-seed
xorshift so the compiler cannot constant-fold the per-iteration copy.

Host: `aarch64-darwin`, release profile, Criterion default sampling
(100 samples × 3 s measurement window). Numbers below are the
median of one local run; deltas vs this baseline are what later
rounds will quote.

| Bench                       | Drives                                                | Median time | Throughput |
| --------------------------- | ----------------------------------------------------- | ----------: | ---------: |
| `parse_minimal_v24/parse`   | `tag_size_at_head` → `parse_tag` → `to_key_value_pairs` (~135 B tag) |   966 ns | 139 MiB/s |
| `parse_apic_heavy_v24/parse`| `parse_tag` over a ~60 KiB APIC frame (memcpy-bound) |    1.58 µs |  36 GiB/s |
| `write_text_v24/write`      | `write_tag` round-trip of the minimal-v24 fixture    |     486 ns | 277 MiB/s |
| `parse_id3v1/parse`         | `parse_id3v1` over the 128-byte trailer              |     512 ns | 238 MiB/s |

Run with:

```sh
cargo bench -p oxideav-id3 --bench id3
```

## License

MIT - see [LICENSE](LICENSE).
