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

## What is supported

- **ID3v1 / ID3v1.1** — parse + write 128-byte trailers, Winamp's
  genre byte range, v1.1 track number.
- **ID3v2.2** — parse only (read-only legacy). Frame ids are promoted
  to their v2.3 equivalents (`TT2 -> TIT2`, `PIC -> APIC`, ...).
- **ID3v2.3 / ID3v2.4** — parse + write. Whole-tag unsync, per-frame
  unsync, data-length indicator, and extended headers are handled on
  read; the extended-header CRC-32 is verified against the spec-defined
  region (frames-only in v2.3, frames + padding in v2.4) and parse
  fails on mismatch. The writer can emit a CRC-bearing extended header
  via `WriteOptions::with_crc(true)`. Footer-bearing tags are sized
  correctly.
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

## License

MIT - see [LICENSE](LICENSE).
