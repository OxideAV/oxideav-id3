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

## [0.0.5](https://github.com/OxideAV/oxideav-id3/compare/v0.0.4...v0.0.5) - 2026-04-19

### Other

- drop Cargo.lock — this crate is a library
- bump oxideav-core / oxideav-codec dep examples to "0.1"
- bump to oxideav-core 0.1.1 + codec 0.1.1
- bump oxideav-core + oxideav-codec deps to "0.1"
