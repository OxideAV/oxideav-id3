# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

## [0.0.5](https://github.com/OxideAV/oxideav-id3/compare/v0.0.4...v0.0.5) - 2026-04-19

### Other

- drop Cargo.lock — this crate is a library
- bump oxideav-core / oxideav-codec dep examples to "0.1"
- bump to oxideav-core 0.1.1 + codec 0.1.1
- bump oxideav-core + oxideav-codec deps to "0.1"
