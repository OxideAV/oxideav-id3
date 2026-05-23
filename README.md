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

## What is supported

- **ID3v1 / ID3v1.1** — parse + write 128-byte trailers, Winamp's
  genre byte range, v1.1 track number.
- **ID3v2.2** — parse only (read-only legacy). Frame ids are promoted
  to their v2.3 equivalents (`TT2 -> TIT2`, `PIC -> APIC`, ...).
- **ID3v2.3 / ID3v2.4** — parse + write. Whole-tag unsync, per-frame
  unsync, data-length indicator, and extended headers are handled on
  read. Footer-bearing tags are sized correctly.
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
  optional group-dependent data), `AENC` audio encryption (owner +
  preview start/length + opaque encryption-info), `LINK` linked
  information (3-byte v2.3 / 4-byte v2.4 frame-id + URL + spec-shaped
  additional data). All twenty-one round-trip both directions for
  v2.3 and v2.4.
- Everything else surfaces as `Id3Frame::Unknown { id, raw }` with the
  payload preserved so it can be written back untouched.

## License

MIT - see [LICENSE](LICENSE).
