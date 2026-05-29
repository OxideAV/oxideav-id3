# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `RVRB` reverb frame (spec v2.3 §4.13 / v2.4 §4.13). Replaces the
  previous opaque `Id3Frame::Unknown { id: "RVRB", .. }` fallthrough
  with a structured `Id3Frame::Reverb` variant carrying the ten
  spec-named fields: u16 BE `reverb_left_ms` / `reverb_right_ms`
  delays, u8 `bounces_left` / `bounces_right` counts (where `$FF`
  denotes the spec's "infinite number of bounces"), four u8 feedback
  bytes (`feedback_ll` / `feedback_lr` / `feedback_rr` / `feedback_rl`
  for the four bounce-to-channel routings on the spec's
  `$00 = 0% .. $FF = 100%` scale, where `$7F` produces the worked
  example of "50% volume reduction on the first bounce, 50% of that on
  the second"), and two u8 premix bytes (`premix_lr` / `premix_rl`,
  also `$00..$FF`, with both `$FF` producing mono output when reverb
  is symmetric). The wire payload is exactly twelve bytes with no
  encoding byte and no terminators, and the layout is identical
  between v2.3 and v2.4 — the writer emits the same bytes under either
  version envelope. A v2.2 `REV` frame promotes to the same
  `Reverb` variant on parse (matching the `TT2 → TIT2`, `PIC → APIC`
  table from §3.3 of the v2.2 spec). A payload shorter than 12 bytes
  is preserved verbatim through `Id3Frame::Unknown { id: "RVRB", .. }`
  because the spec layout is exact-size and a truncated frame cannot
  be reconstructed unambiguously. Trailing bytes after the canonical
  12 are dropped on read per spec "unknown bytes in a frame should be
  skipped". `to_key_value_pairs` does not surface `Reverb` (the frame
  carries no text values, only DSP descriptors). Six new lib tests
  (writer pinned to a hand-computed 12-byte sequence; v2.3 + v2.4
  round-trip; short-payload Unknown fallback for both 11-byte and
  zero-byte inputs; trailing-byte drop; v2.2 `REV` promotion to
  `Reverb`; zero-pair invariant for `to_key_value_pairs`) and two new
  integration round-trip tests (`roundtrip_rvrb_v23_and_v24` and
  `roundtrip_rvrb_extreme_values` exercising `$FF` bounces / feedback
  / premix and the `0xFFFF` u16 delay extreme) cover the matrix. Spec
  notes "There may only be one 'RVRB' frame in each tag" — uniqueness
  is left to the caller, matching how the crate treats the analogous
  single-instance constraints on `MCDI` / `MLLT` / `SEEK`.

- `MLLT` MPEG location lookup table frame (spec v2.3 §4.7 / v2.4 §4.6).
  Replaces the previous opaque `Id3Frame::Unknown { id: "MLLT", .. }`
  fallthrough with a structured `Id3Frame::MpegLocationLookup` variant
  carrying the u16 frame-counter increment, the two 24-bit
  "between reference" fields (bytes + milliseconds), the two
  per-reference deviation widths (bits), and the decoded
  `Vec<(u32, u32)>` of `(bytes_dev, ms_dev)` pairs. Per-reference
  packing is MSB-first across byte boundaries via a small bit-reader /
  bit-writer pair; the writer enforces the spec's
  `(bits_for_bytes_deviation + bits_for_ms_deviation) % 4 == 0`
  constraint by returning `Error::invalid` rather than emitting a
  stream a conforming reader could not realign on. The 24-bit fields
  are capped at `0x00FF_FFFF` and per-reference deviation values are
  capped at the declared per-field bit width — out-of-range values
  fail the writer with a specific error. Per-reference widths above
  32 bits are refused on read (descriptor preserved, references
  empty) and on write (returned as `Error::invalid`) since the
  in-memory representation tops out at `u32`. Six new lib tests
  (bit-packing pinned to a hand-computed byte sequence; sub-byte
  alignment round-trip) and six new integration round-trip tests
  (v2.3 `12 + 4`, v2.4 `8 + 8`, four error-path tests: non-mult-of-4
  total bits, 24-bit overflow, reference exceeds declared width, and
  the >32-bit-width parser short-circuit) cover the matrix.

- ID3v2.4 footer support (spec §3.4). The parser used to advance past
  the 10-byte trailer when the header's footer-present bit (0x10) was
  set without verifying any of it; it now requires the trailer's
  `b"3DI"` identifier and validates that the footer's version /
  flags / synchsafe size mirror the header byte-for-byte, returning a
  specific error per failure mode (magic, version, flags, size,
  truncation → `Error::NeedMore`). A footer flag on a v2.2 / v2.3
  header is rejected outright since the spec only defines the footer
  for v2.4. On the writer side, `WriteOptions` grows a `footer` field
  (set via the `with_footer` builder method); when enabled on a v2.4
  target the writer sets header bit 0x10 and appends the 10-byte
  trailer with identifier `b"3DI"` followed by a verbatim copy of the
  header's version / flags / size bytes. Requesting a footer against a
  v2.3 target returns `Error::unsupported` rather than silently
  dropping the flag — appended tags must be v2.4 to be discoverable by
  a reverse scan. `with_footer` composes cleanly with both `with_crc`
  and `with_unsync(WholeTag | PerFrame)`: the footer lives outside the
  announced synchsafe size (matching spec §3.1 "If a footer is present
  this equals to ('total size' - 20) bytes"), so the unsync transform
  never touches it and the CRC region is unchanged. Ten new lib tests
  (footer default / +unsync / +crc round-trips, v2.3-rejection both
  directions, four parser-validation tests for magic / size / flags /
  truncation, plus `tag_size_at_head` consistency) and four new
  integration round-trip tests cover the matrix. The fuzz target's
  write × parse loop now iterates `footer ∈ {false, true}` alongside
  the existing unsync × CRC dimensions.

- Extended-header CRC verification on read + emission on write
  (spec §3.2 in both v2.3 and v2.4). The parser used to skip the
  extended header outright; it now decodes it, walks the v2.4 per-flag
  attached-data area (update / CRC / restrictions, with strict
  data-length validation), and verifies the stored CRC-32 [ISO-3309]
  against the spec-defined region — frames-only in v2.3, frames +
  padding in v2.4. A mismatched CRC is a hard parse error. The writer
  grows a new `WriteOptions::crc` flag (set via the `with_crc` builder
  method); when enabled the writer prepends a CRC-bearing extended
  header (14 bytes for v2.3: size = 10 excl-self, flags `0x80 0x00`,
  padding-size = 0, regular u32 CRC; 12 bytes for v2.4: synchsafe
  size = 12 incl-self, flag-count = 1, flags = 0x20, data-length = 5,
  5-byte synchsafe CRC) and sets the tag-header's extended-header bit.
  `WriteOptions::with_crc` composes cleanly with the existing
  `with_unsync` setter: the CRC is computed on pre-unsync frame
  bytes (matching v2.3's "calculated before unsynchronisation"), then
  unsync is applied over `(ext_header || frames)`; the parser reverses
  unsync first, so the same byte sequence is fed to the CRC check on
  the read side. Seven new round-trip tests cover the per-version
  on-wire shape, default-options-emit-no-extended-header invariant,
  CRC + WholeTag unsync round-trip on both versions, CRC + PerFrame
  unsync on v2.4, and corrupted-CRC parse rejection on both versions.
  Internal CRC-32 implementation is a 9-line bit-by-bit loop
  (polynomial 0xEDB88320, init / xor-out 0xFFFFFFFF) — no new
  dependencies.
- Writer-side unsynchronisation: new public `UnsyncMode` enum
  (`None` / `WholeTag` / `PerFrame`), `WriteOptions` bag, and
  `write_tag_with_options` entry point. `WholeTag` applies spec §6.1
  unsync over the entire serialised body and sets the header flag
  bit 0x80 (works for both v2.3 and v2.4); `PerFrame` is v2.4-only
  and unsynchronises each frame's payload independently with the
  format-flag bit 0x02 set per frame. A new internal `apply_unsync`
  is the byte-for-byte inverse of the existing `reverse_unsync`
  (escapes `$FF` whenever followed by an MPEG sync byte `%111xxxxx`,
  by literal `$00`, or by end-of-buffer per spec §6.1 last
  paragraph). The pre-existing `write_tag` shorthand is unchanged
  (it forwards to `write_tag_with_options` with `UnsyncMode::None`).
  Six new round-trip tests cover identity composition,
  false-sync elimination, v2.3 / v2.4 whole-tag round-trip via
  `parse_tag`, v2.4 per-frame round-trip, and the v2.3 silent
  downgrade of `PerFrame` to `WholeTag`. The cargo-fuzz `parse`
  target now also drives `write_tag_with_options` under both unsync
  modes on both target versions and re-parses the output.
- `cargo-fuzz` target `fuzz/fuzz_targets/parse.rs` drives arbitrary
  bytes through `tag_size_at_head`, `parse_tag`, `parse_id3v1`,
  `to_key_value_pairs`, `attached_pictures`, `write_id3v1`, and
  `write_tag` (v2.3 + v2.4) and asserts panic-freedom on every input.
  Classic spots covered: synchsafe-size overflow, frame-size > tag-size,
  v2.3/v2.4 extended-header bounds, encryption / compression /
  data-length-indicator flag combos, GEOB length fields, SYLT
  terminators. Sustained 15M+ iteration runs under libFuzzer find no
  crashes. Run with `cd fuzz && cargo +nightly fuzz run parse`.
- Daily scheduled `Fuzz` CI workflow
  (`.github/workflows/fuzz.yml`) runs the `parse` target for a
  30-minute budget via the shared `crate-fuzz` reusable workflow, plus
  a curated seven-input seed corpus under `fuzz/corpus/parse/` (minimal
  v2.2 / v2.3 / v2.4 text tags, a mixed COMM/UFID/TXXX/APIC v2.4 tag, a
  v2.3 extended-header tag, a whole-tag-unsync v2.4 tag, and an ID3v1
  trailer). The seeds drive structural coverage ~30x deeper than the
  prior noise corpus; a fresh 60-second two-worker baseline at ~4.4M
  iterations is crash-free.

- New public `TimestampUnit` enum (`MpegFrames` / `Milliseconds`) and
  `Id3Frame::timestamp_unit()` typed accessor surface the
  `time_stamp_format` byte carried by `ETCO`, `SYTC`, `SYLT`, and
  `POSS` frames per spec v2.3 §4.6 / §4.8 / §4.10 / §4.22 (identical
  in v2.4 §4.5 / §4.7 / §4.9 / §4.21). The accessor returns `None`
  for the reserved wire values so callers don't have to invent a
  default. Two new round-trip tests prove the logical unit is
  preserved when a SYLT frame is written under one major-version
  envelope and re-parsed under the other.

- Structural parser + writer for five additional ID3v2.3 / 2.4 frames:
  `POPM` (popularimeter — email, rating, wide-counter), `PCNT` (play
  counter, widens past 32 bits per spec §4.16), `PRIV` (private frame
  with owner identifier), `GEOB` (general encapsulated object), and
  `UFID` (unique file identifier). Each frame surfaces as its own
  `Id3Frame` variant and round-trips through `write_tag` / `parse_tag`
  bit-for-bit on the payload bytes (excluding the encoding-byte choice
  which the writer picks per target version).
- Structural parser + writer for six more ID3v2.3 / 2.4 frames:
  `USER` terms of use (language triplet + free text), `OWNE`
  ownership (currency-prefixed price + 8-byte YYYYMMDD date +
  seller), `COMR` commercial offer (price + valid-until date +
  contact URL + 1-byte delivery method + seller + description +
  optional MIME-typed company logo), `SYTC` synchronised tempo
  codes (time-format byte + `(BPM, timestamp)` pairs with the
  spec's `$FF`-prefix extension for 256..510 BPM), `RVA2` relative
  volume adjustment 2 (identification + per-channel records with
  Q9.7 dB volume + variable-width zero-padded peak), `EQU2`
  equalisation 2 (interpolation byte + identification + sorted
  `(frequency, adjustment)` points). New `Rva2Channel` public
  struct exposes the per-channel RVA2 record shape.
- `to_key_value_pairs` now surfaces `play_count`, `rating[:email]`,
  `rating_count[:email]`, `termsofuse[:lang]`, `ownership_price`,
  `ownership_date`, and `ownership_seller` keys so consumers can
  read these frames without matching on the enum.
- Structural parser + writer for nine more ID3v2.3 / 2.4 frames:
  `MCDI` music CD identifier (opaque CD-DA TOC bytes), `ETCO`
  event timing codes (time-format + `(event_type, timestamp)`
  pairs), `SYLT` synchronised lyrics/text (language + time-format
  + content-type + descriptor + `(syllable, timestamp)` syncs,
  honouring both single-NUL v2.4-UTF-8 and double-NUL v2.3-UTF-16
  terminators inside the sync-record loop), `POSS` position
  synchronisation (time-format + 32-bit position), `RBUF`
  recommended buffer size (24-bit buffer + embedded-info flag +
  32-bit next-tag offset; writer clamps oversized buffer-size to
  the 24-bit field width), `SEEK` seek frame (32-bit next-tag
  offset), `SIGN` signature frame (group-symbol byte + binary
  signature), `AENC` audio encryption (owner + 2-byte preview
  start / length + opaque encryption-info), `LINK` linked
  information (auto-detects 3-byte v2.3 vs 4-byte v2.4 frame ids
  on read, and emits the on-wire form matching the target version
  on write). All nine new variants round-trip through
  `write_tag` / `parse_tag` for v2.3 + v2.4 and lose no data via
  `Id3Frame::Unknown`.
- Structural parser + writer for `GRID` group identification
  registration (v2.3 §4.27 / v2.4 §4.26): NUL-terminated owner
  identifier + 1-byte group symbol ($80-F0 per spec) + optional
  group-dependent data. New `Id3Frame::GroupId` variant round-trips
  through `write_tag` / `parse_tag` for both v2.3 and v2.4 (the wire
  layout is version-independent), including the empty-data minimum
  frame.
- Structural parser + writer for `ENCR` encryption method
  registration (v2.3 §4.25 / v2.4 §4.25): NUL-terminated owner
  identifier + 1-byte method symbol ($80-F0 per spec) + optional
  encryption-specific data. New `Id3Frame::EncryptionMethod` variant
  round-trips through `write_tag` / `parse_tag` for both v2.3 and
  v2.4 (the wire layout is version-independent, identical in shape to
  `GRID`), including the symbol-only minimum frame.
- Structural parser + writer for `ASPI` audio seek point index
  (v2.4 §4.30): 32-bit indexed-data start + 32-bit indexed-data
  length + 16-bit number of index points + 8/16 bits-per-point + N
  `Fi` fraction entries. New `Id3Frame::AudioSeekPointIndex` variant
  round-trips through `write_tag` / `parse_tag` for both the 8-bit
  (short-file) and 16-bit (long-file) precision modes. The writer
  refuses bit widths other than 8 or 16 (a conformant parser cannot
  reconstruct intermediate widths) and caps `N` at `u16::MAX`. The
  parser tolerates a fraction list shorter than the declared `N`
  (the truncated tail is dropped) and a sub-11-byte payload
  (degenerates to a zeroed frame rather than failing the whole tag).
  ASPI is declared v2.4-only per spec but the wire layout is
  byte-aligned and version-independent, so the writer accepts it
  under any version envelope.

## [0.0.5](https://github.com/OxideAV/oxideav-id3/compare/v0.0.4...v0.0.5) - 2026-04-19

### Other

- drop Cargo.lock — this crate is a library
- bump oxideav-core / oxideav-codec dep examples to "0.1"
- bump to oxideav-core 0.1.1 + codec 0.1.1
- bump oxideav-core + oxideav-codec deps to "0.1"
