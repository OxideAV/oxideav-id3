# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Typed `TKEY` initial-key accessor `Id3Frame::initial_key()` + the
  `MusicalKey` / `KeyAccidental` enums (spec v2.3 §4.2.1 / v2.4 §4.2.3).
  The frame "contains the musical key in which the sound starts",
  "represented as a string with a maximum length of three characters";
  the ground keys are `A`..`G`, the halfkeys `b` / `#`, minor `m`, and
  "off key is represented with an `o` only" (e.g. the spec example
  `Dbm`). The accessor decodes that grammar to `MusicalKey::Key { tonic,
  accidental, minor }`, `MusicalKey::OffKey`, or `MusicalKey::Custom` for
  any value outside the grammar (tonic not in `A`..`G`, unknown / out-of-
  order trailing character, or a value past the three-character maximum)
  so a forward-compatible or non-conforming source surfaces structurally
  rather than being dropped, matching the posture of `file_type()` /
  `media_type()`. The grammar paragraph is identical across v2.2 (`TKE`),
  v2.3, and v2.4 so the accessor is version-independent; the raw
  `Id3Frame::Text::values` is unchanged and round-trips losslessly
  through `write_tag`. Three new lib tests (spec-example grammar coverage
  for natural / minor / flat / sharp keys and the `o` off-key; non-
  conforming inputs collapsing to `Custom`; accessor `None` on non-`TKEY`
  frames) plus one integration round-trip test (write → parse under both
  v2.3 and v2.4 envelopes preserving both the typed view and the raw
  value) cover the matrix.
- **ID3v2.2 write support** (`write_tag(&tag, Id3Version::V2_2)` / the
  `write_tag_with_options` path), completing the v2.2 round-trip — the
  format was previously parse-only. Each four-char id is demoted to its
  three-character v2.2 id (the inverse of the existing v2.2→v2.3
  promotion table), the six-byte v2.2 frame header (3-char id + 3-byte
  big-endian size, no flags; spec `id3v2-00` §3.2) is emitted, and the
  v2.2 PIC layout is reconstructed (fixed three-character image-format
  code per §4.15 in place of v2.3 APIC's NUL-terminated MIME). The §4
  frame set is closed, so a frame with no v2.2 equivalent (a v2.4-only
  addition or an unrecognised `Unknown` id) is skipped rather than
  emitted under an id a conformant v2.2 reader could not interpret. v2.2
  predates the extended header, footer, and per-frame flags byte, so the
  writer rejects `with_crc` / `with_footer` / `with_update` /
  `with_restrictions` / `with_compression` under a v2.2 target;
  `UnsyncMode::WholeTag` is supported via the header bit-7 unsync flag
  and `PerFrame` collapses to whole-tag as it does for v2.3. Six new
  round-trip tests cover text/comment/lyrics/URL/picture frames, the PIC
  three-char image format, whole-tag unsync over a false-sync payload,
  v2.4-only-frame skipping, the post-v2.2 option rejections, and a
  structured `POPM` frame; the fuzz target now also drives the v2.2
  write+reparse path for panic-freedom.
- Typed `TFLT` file-type accessor `Id3Frame::file_type()` + `FileType`
  enum (spec v2.3 §4.2.1 / v2.4 §4.2.3). The frame "indicates which type
  of audio this tag defines" via a predefined code optionally followed by
  `/`-separated refinements, "in a similar way to the predefined types in
  the `TMED` frame, but without parentheses". Because the wire form is
  identical in both versions (`MPG/3` → `code="MPG"`, `refinements=["3"]`)
  and carries no parentheses or v2.3 free-text refinement, a single bare
  grammar covers both dialects; the only version difference is the
  v2.4-added `MIME` top-level code, which the predefined table resolves
  under either envelope. The four predefined codes (MIME/MPG/VQF/PCM)
  resolve to their spec descriptions; an out-of-table code surfaces as
  `FileType::Predefined { name: None, .. }` so a forward-compatible
  reference is preserved structurally rather than dropped, and a value
  with an empty top-level segment surfaces as `FileType::Custom`. The raw
  `Id3Frame::Text::values` is unchanged and round-trips losslessly,
  mirroring the `TMED` `media_type()` accessor.
- Typed `TMED` media-type accessor `Id3Frame::media_type()` + `MediaType`
  enum (spec v2.3 §4.6.3 / v2.4 §4.2.3). The frame "describes from which
  media the sound originated — either a text string or a reference to the
  predefined media types found in the list below." The accessor
  normalises both version dialects onto one vocabulary, mirroring the
  `TCON` `content_types()` accessor: v2.3 wraps a reference in `(...)`
  optionally followed by a free-text refinement (`(MC) with four
  channels` → `media="MC"`, `text=" with four channels"`; `(VID/PAL/VHS)`
  → `media="VID"`, `refinements=["PAL","VHS"]`) with a `((` escape for a
  literal-`(` free-text name, while v2.4 drops the parentheses (the spec
  example `VID/PAL/VHS` parses to the same reference). The 15 predefined
  top-level codes (DIG/ANA/CD/LD/TT/MD/DAT/DCC/DVD/TV/VID/RAD/TEL/MC/REE)
  resolve to their spec descriptions; an out-of-table code surfaces as
  `MediaType::Predefined { name: None, .. }` so a forward-compatible
  reference is preserved rather than dropped. The raw text value is
  unchanged and round-trips losslessly through `write_tag`.
- Complete ID3v2.2.0 §4 frame-table read support. The v2.2 walker
  (3-char ids, 3-byte sizes, no frame flags) previously dispatched
  only text / URL / `COM` / `ULT` / `PIC` / `REV` / `EQU`; it now maps
  every declared v2.2 frame onto the crate's typed surface: `UFI` §4.1
  → `Ufid`, `IPL` §4.4 → `Ipls`, `MCI` §4.5 → `MusicCdId`, `ETC` §4.6
  → `EventTimingCodes`, `MLL` §4.7 → `MpegLocationLookup`, `STC` §4.8
  → `SyncedTempo`, `SLT` §4.10 → `SyncedLyrics`, `GEO` §4.16 → `Geob`,
  `CNT` §4.17 → `PlayCounter`, `POP` §4.18 → `Popularimeter`, `BUF`
  §4.19 → `RecommendedBuffer`, `CRA` §4.21 → `AudioEncryption` (all
  byte-identical to their v2.3 descendants' payload layouts), plus two
  v2.2-specific walkers — `RVA` §4.12 (right/left volume-change fields
  are unconditional; presence is not keyed on the inc/dec sign bits
  the way v2.3 `RVAD` gates its front block, so a both-decrement frame
  keeps its data) and `LNK` §4.22 (the linked frame identifier is
  always exactly 3 bytes, so a URL whose first byte is an uppercase
  id-class character can never be folded into the identifier the way
  the v2.3/v2.4 3-vs-4-byte heuristic would). `CRM` §4.20 (encrypted
  meta frame) has no v2.3/v2.4 descendant and is preserved verbatim
  via `Id3Frame::Unknown`. The `v22_promote` conversion table now
  covers the full §4 id list so pass-through writes promote correctly.
- Frame-level zlib compression, both directions (spec v2.3 §3.3
  format flag `i` / v2.4 §4.1.2 format flag `k`). `parse_tag` now
  inflates compressed frames transparently in both dialects — v2.3's
  4-byte big-endian decompressed-size header addition and v2.4's
  mandatory data-length indicator (a compressed v2.4 frame without
  the DLI bit is rejected per the spec's "requires the 'Data Length
  Indicator' bit to be set as well") — and dispatches the recovered
  payload structurally. The announced decompressed size is
  authoritative: an inflate-length mismatch drops the frame (earlier
  frames survive, matching the corrupted-frame posture), and the
  announce doubles as the allocation cap under a 64 MiB per-frame
  ceiling so a zlib bomb can't force a huge allocation. On the
  writer side `WriteOptions::with_compression(true)` deflates every
  frame and emits the per-version flag + size field; it composes
  with per-frame unsync (compression first, unsync second, mirroring
  the parse order), the extended-header CRC (computed over the
  post-compression frame bytes), and whole-tag unsync. The zlib
  stream comes from `compcol` (the workspace-wide compression
  collection), `default-features = false`, `features = ["zlib"]`.
- Typed accessor `Id3Frame::content_types()` for the `TCON`
  content-type (genre) frame (spec v2.3 §4.2.1 / v2.4 §4.2.3). Decodes
  the frame's one-or-several content-type references into a
  `Vec<ContentType>`, normalising both version dialects onto one
  vocabulary: v2.3's parenthesised grammar (`(21)` numeric ID3v1 genre
  reference, `(RX)` / `(CR)` Remix / Cover keyword references,
  `(4)Eurodisco` numeric reference + free-text refinement, `(51)(39)`
  multiple references in one string, the `((`-escape for a literal-`(`
  custom genre, and a non-conforming unclosed `(` surfacing the
  remainder as free text rather than dropping it) and v2.4's bare form
  (numeric string → genre, bare `RX` / `CR` keywords, NUL-separated
  values as independent references). The new `ContentType` enum carries
  `Genre { index: u8, name: Option<&'static str> }` (numeric reference
  resolved against the same Winamp-extended ID3v1 genre table the v1
  trailer uses; `name: None` for an out-of-table index so a
  forward-compatible reference surfaces structurally rather than being
  dropped), `Remix`, `Cover`, and `Custom(String)` for free-text
  genres. The accessor walks the parser's already-NUL-split `values`
  and applies the per-value grammar to each, returning `None` for any
  non-`TCON` frame and `Some(Vec::new())` for a present-but-empty
  `TCON`. The raw `Id3Frame::Text::values` is unchanged and round-trips
  losslessly through `write_tag`, so the typed view never costs callers
  the ability to preserve the exact on-wire string — matching the
  forward-compatible posture already published by `etco_event_types()`,
  `sytc_tempo_codes()`, and the per-byte enum accessors. Four new tests
  (lib-level grammar coverage of the parenthesised + bare forms, the
  `((`-escape, an unclosed `(`, and bare numeric / keyword / free-text
  values; integration accessor decoding of the v2.3 parenthesised form
  including out-of-table `name: None` and cross-variant `None`; v2.4
  bare-form decoding including a present-but-empty frame; writer→parser
  round-trip preserving the typed view under both v2.3 and v2.4
  envelopes) cover the matrix.
- Typed accessor `Id3Frame::sytc_tempo_codes()` for the per-record
  tempo values carried by a `SYTC` payload (spec v2.4 §4.7). Decodes
  to a `Vec<Option<SytcTempo>>` whose length equals the source
  `codes` vector so positional indexing stays stable when zipped
  against the raw timestamps; each element is `Some(SytcTempo)` for a
  spec-defined value (the two reserved-meaning bytes `$00`/`$01` plus
  the `2..=510` BPM range) and `None` for any value beyond `510` so a
  non-conforming source surfaces structurally rather than mapping to
  a guessed variant. The new `SytcTempo` enum mirrors the spec
  categorical meanings: `BeatFree` (`$00`, "a beat-free time period,
  which is not the same as a music-free time period"), `SingleStroke`
  (`$01`, "one single beat-stroke followed by a beat-free period"),
  and `Bpm(u16)` (the BPM verbatim in the spec range `2..=510`). The
  wire-level one-byte vs `$FF $xx` two-byte split is already
  normalised in `Id3Frame::SyncedTempo::codes` so the typed view stays
  at the logical layer; `from_wire` / `to_wire` form a bijection over
  the spec range — matching the contract already published by
  `EtcoEventType`, `Rva2ChannelType`, `Equ2Interpolation`,
  `SyltContentType`, `CommercialDelivery`, `TimestampUnit`, and
  `Restrictions`. The wire layout is byte-aligned and
  version-independent (only the v2.4 frames doc lists `SYTC`, but the
  on-wire layout is identical between v2.3 and v2.4 envelopes and
  this crate's parser accepts it under both) so the accessor is
  effectively version-independent, matching the cross-version posture
  of `timestamp_unit()` and `etco_event_types()`. The raw
  `SyncedTempo::codes: Vec<(u16, u32)>` field is unchanged and
  round-trips losslessly through `write_tag` for every value the wire
  format can represent (`0..=510`), so the typed view never costs
  callers the ability to preserve forward-compatible payloads. Three
  new round-trip tests (wire bijection over the spec range plus
  beyond-spec-range `None`; accessor decoding of a mixed payload with
  positional indexing stability + cross-variant `None` on a `Text`
  frame; writer→parser preserves every wire-representable value under
  both v2.3 and v2.4 envelopes — single-byte and `$FF`-extension
  forms — so the typed accessor surfaces the same vector after
  round-trip) cover the matrix.
- Typed accessor `Id3Frame::etco_event_types()` for the per-event
  "type of event" bytes carried by an `ETCO` payload (spec v2.3 §4.6 /
  v2.4 §4.5). Decodes to a `Vec<Option<EtcoEventType>>` whose length
  equals the source `events` vector so positional indexing stays
  stable when zipped against the raw timestamps; each element is
  `Some(EtcoEventType)` for a spec-defined byte and `None` for a byte
  in either reserved range (`$17..=$DF`, `$F0..=$FC`) so a
  non-conforming source surfaces structurally rather than mapping to a
  guessed variant. The new `EtcoEventType` enum mirrors the spec's
  value table verbatim: 23 named events `$00..=$16` ("padding" through
  "profanity end"), a `NotPredefinedSync(u8)` variant that carries the
  low nibble of the `$E0..=$EF` user-defined synchronisation slot
  (`0..=15`), the `$FD` / `$FE` audio-end markers, and the `$FF`
  continuation marker the spec describes as "one more byte of events
  follows". `from_wire` / `to_wire` form a bijection over the spec
  range — matching the contract already published by
  `Rva2ChannelType`, `Equ2Interpolation`, `SyltContentType`,
  `CommercialDelivery`, `TimestampUnit`, and `Restrictions`. The
  event-type table is identical between v2.3 and v2.4 (reproduced
  bit-for-bit in both version docs) so the accessor is
  version-independent, matching the cross-version posture of
  `timestamp_unit()` and `commercial_delivery()`. The raw
  `EventTimingCodes::events: Vec<(u8, u32)>` field is unchanged and
  round-trips losslessly through `write_tag` for every byte value —
  including bytes in the reserved ranges — so the typed view never
  costs callers the ability to preserve forward-compatible payloads.
  Three new round-trip tests (wire bijection over the spec range plus
  reserved-byte rejection for both `$17..=$DF` and `$F0..=$FC`;
  accessor decoding of a mixed payload with positional indexing
  stability + cross-variant `None` on a `Text` frame; writer→parser
  preserves every byte value under both v2.3 and v2.4 envelopes) cover
  the matrix.
- Typed accessor `Id3Frame::equ2_interpolation()` for the
  interpolation-method byte that opens an `EQU2` payload (spec v2.4
  §4.12). Decodes to a spec-shaped `Equ2Interpolation` enum with the
  two §4.12 values: `Band` (`$00`, no interpolation between adjustment
  points — a renderer jumps from one adjustment level to the next in
  the middle between two adjustment points) and `Linear` (`$01`, a
  renderer interpolates linearly between adjacent adjustment points).
  `from_wire` / `to_wire` form a bijection over the spec range
  `$00..=$01` and return `None` for any reserved byte so a
  non-conforming source surfaces structurally rather than mapping to a
  guessed variant — matching the contract already published by
  `Rva2ChannelType`, `SyltContentType`, `CommercialDelivery`,
  `TimestampUnit`, and `Restrictions`. The underlying
  `Id3Frame::Equ2::interpolation: u8` field is unchanged and
  round-trips losslessly through `write_tag` for both spec-named and
  reserved bytes, so the typed view never costs callers the ability to
  preserve forward-compatible payloads. EQU2 is v2.4-only per spec
  (the v2.4 frames doc lists `EQU2` and v2.3 carried `EQUA` instead
  with an unrelated per-band inc/dec bitfield rather than a
  curve-level interpolation choice), so the accessor is version-locked
  to v2.4 by virtue of its source variant. Four new round-trip tests
  (wire bijection over `$00..=$01` plus reserved-byte rejection
  `$02..=$FF`; accessor decoding both spec values + reserved-byte
  collapse to `None` + cross-variant `None` on a `Text` frame; v2.4
  writer→parser preserves both `Band` and `Linear` so the typed
  accessor surfaces the same variant after round-trip; reserved-byte
  raw field round-trips through `write_tag` while the typed view
  collapses to `None`) cover the matrix.
- Typed accessor `Rva2Channel::channel_type_typed()` for the
  `type_of_channel` byte that opens each per-channel record inside an
  `RVA2` payload (spec v2.4 §4.11). Decodes to a spec-shaped
  `Rva2ChannelType` enum with the nine §4.11 values: `Other` /
  `MasterVolume` / `FrontRight` / `FrontLeft` / `BackRight` /
  `BackLeft` / `FrontCentre` / `BackCentre` / `Subwoofer`; `from_wire`
  / `to_wire` form a bijection over the spec range `$00..=$08` and
  return `None` for any reserved byte so a non-conforming source
  surfaces structurally rather than mapping to a guessed variant —
  matching the contract already published by `SyltContentType`,
  `CommercialDelivery`, `TimestampUnit`, and `Restrictions`. The
  underlying `Rva2Channel::channel_type: u8` field is unchanged and
  round-trips losslessly through `write_tag` for both spec-named and
  reserved bytes, so the typed view never costs callers the ability
  to preserve forward-compatible payloads. The wire byte is identical
  between v2.3 and v2.4 (v2.3 carries the v2.4-introduced `RVA2`
  frame on tags that have been upgraded; the spec layout is
  byte-aligned and version-independent), so the accessor is
  version-independent and mirrors the cross-version posture of
  `Id3Frame::timestamp_unit`, `Id3Frame::sylt_content_type`, and
  `Id3Frame::commercial_delivery`. Round-trip tests cover writer →
  parser under both v2.3 and v2.4 envelopes across master / front /
  back / centre / subwoofer / reserved channels plus exhaustive
  wire-bijection coverage over the `$00..=$08` spec range and
  reserved-byte rejection over `$09..=$FF`.
- Typed accessors `Id3Frame::sylt_content_type()` and
  `Id3Frame::commercial_delivery()` for the enumerated bytes carried
  by `SYLT` (spec v2.3 §4.10 / v2.4 §4.9, "Content type") and `COMR`
  (spec v2.3 §4.25 / v2.4 §4.24, "Received as"). Both decode to
  spec-shaped enums (`SyltContentType` with the nine §4.9 values:
  `Other` / `Lyrics` / `TextTranscription` / `MovementPartName` /
  `Events` / `Chord` / `Trivia` / `UrlsToWebpages` / `UrlsToImages`;
  `CommercialDelivery` with the nine §4.24 values: `Other` /
  `StandardCdAlbum` / `CompressedAudioOnCd` / `FileOverInternet` /
  `StreamOverInternet` / `NoteSheets` / `NoteSheetsInBook` /
  `MusicOnOtherMedia` / `NonMusicalMerchandise`); both enums expose
  `from_wire` / `to_wire` forming a bijection over the spec range
  `$00..=$08` and returning `None` for any reserved byte so a
  non-conforming source surfaces structurally rather than mapping to
  a guessed variant — matching the contract `TimestampUnit` and
  `Restrictions` already publish. Both accessors return `None` for
  any other `Id3Frame` variant, matching the cross-variant posture of
  `Id3Frame::timestamp_unit`. The COMR mapping is identical between
  v2.3 and v2.4 so the accessor is version-independent; the SYLT
  byte's spec range extended from `$06` (v2.3 §4.10) to `$08` in
  v2.4 §4.9, but the wire byte is unambiguous so the accessor
  ignores the cross-version section-number rename. Round-trip tests
  cover both accessors under both v2.3 and v2.4 envelopes plus
  exhaustive wire-bijection coverage over the `$00..=$08` spec range
  and the reserved-byte rejection over `$09..=$FF`.
- `to_key_value_pairs` now maps the v2.4 spec §4.2 text frames the
  prior table dropped to Vorbis-style keys: §4.2.5 timestamp class
  (`TDEN` → `encodingtime`, `TDTG` → `taggingtime`); §4.2.3
  informational (`TMOO` → `mood`, `TFLT` → `filetype`, `TLEN` →
  `length`); §4.2.4 rights/radio (`TOWN` → `owner`, `TPRO` →
  `producednotice`, `TRSN` → `radiostation`, `TRSO` →
  `radiostationowner`); §4.2.5 sort-order (`TSOA` → `albumsort`,
  `TSOP` → `artistsort`, `TSOT` → `titlesort`); §4.2.1 set-subtitle
  (`TSST` → `setsubtitle`); §4.2.5 misc (`TDLY` → `playlistdelay`,
  `TOFN` → `originalfilename`); plus the v2.3-only date/size frames
  v2.4 folded into `TDRC` or removed (`TDAT` → `date_ddmm` per spec
  §TDAT DDMM shape — distinct from `TYER`'s `date` so the two don't
  collide; `TIME` → `time_hhmm`; `TRDA` → `recordingdates`; `TSIZ`
  → `size`). Unknown `T???` frames still fall through to the
  lowercased frame id (catch-all unchanged).
- Typed accessors `Id3Frame::involved_people()` and
  `Id3Frame::musician_credits()` for the spec §4.2.2 pair-list text
  frames (`TIPL`, `TMCL`) and the v2.3 `IPLS` structural frame.
  Folds the parser's flat NUL-split `values` back into
  `(role, name)` / `(instrument, performer)` pairs; surfaces both
  `TIPL` and `IPLS` via the same `involved_people()` entry point so
  callers handle either source version without matching on the
  underlying variant (matching the cross-version posture of
  `timestamp_unit()`); keeps `TMCL` separate via `musician_credits()`
  since the logical mapping is distinct (instrument-to-musician
  vs function-to-name) even though the wire layout is identical;
  surfaces non-conforming odd-count sources as a pair with an empty
  second component rather than dropping the trailing entry; returns
  `Some(Vec::new())` for a present-but-empty frame so callers can
  distinguish from a frame-absent `None`.
- Criterion bench harness at `benches/id3.rs` covering the three
  public surfaces a typical caller exercises on an MP3-resident tag:
  `bench_parse_minimal_v24` drives `tag_size_at_head` → `parse_tag` →
  `to_key_value_pairs` over a hand-built ~135-byte v2.4 tag (TIT2 /
  TPE1 / TALB text frames + one short COMM, exercising spec §3.1
  header walk, §4.1 frame walk, §4.2 text-frame decode, and §4.10
  comment-frame decode); `bench_parse_apic_heavy_v24` drives
  `parse_tag` over a ~64 KiB v2.4 tag whose dominant cost is the
  inner 60 KiB APIC picture copy (spec §4.14 — encoding byte +
  NUL-terminated MIME + picture-type byte + description + picture
  bytes) so MiB/s reflects the picture-copy bandwidth ceiling rather
  than structural overhead; `bench_write_text_v24` round-trips the
  minimal-v24 fixture through `write_tag` with the default
  `WriteOptions`, isolating the write surface from the parse cost
  (the parse step lives outside the timed region) and reporting
  throughput against the produced output length; `bench_parse_id3v1`
  parses the 128-byte trailer (spec layout: `TAG` + 30 title + 30
  artist + 30 album + 4 year + 28 comment + 1 zero + 1 track + 1
  genre) as the baseline-floor measurement. Every fixture is
  hand-built in the bench from the wire layout described in
  `docs/container/id3/id3v2.4.0-structure.html` and
  `docs/container/id3/id3v2.4.0-frames.html`; the 60 KiB picture
  payload is seeded by a fixed-seed xorshift32 so the compiler
  cannot constant-fold the per-iteration copy out of the timed
  region. Criterion drives all four scenarios via a single
  `criterion_group!` in `benches/id3.rs`; `[[bench]]` table with
  `harness = false` in `Cargo.toml` and Criterion is added as a
  `[dev-dependencies]` pin to `0.5` matching the other OxideAV crates
  with benches. Run with `cargo bench -p oxideav-id3 --bench id3`.
  README now carries a `## Benchmarks` section publishing the
  baseline numbers from this machine so future rounds can quote
  deltas against a fixed reference point.

- v2.4 extended-header sub-fields surfaced via the new
  `parse_tag_with_extended_header` entry point and emitted via two
  new `WriteOptions` builders (`with_update`, `with_restrictions`).
  Spec §3.2 defines three optional ext-header sub-fields: `b` "Tag
  is an update", `c` CRC-32, and `d` restrictions. Previously the
  crate parsed and verified only `c` (CRC); `b` and `d` were
  consumed structurally to keep parsing valid but their values were
  dropped. They now decode into a typed `ExtendedHeader` struct
  carrying `is_update: bool`, `crc: Option<u32>`, and
  `restrictions: Option<Restrictions>`. The five restrictions
  sub-fields each get their own enum so a caller can pattern-match
  the advisory limits without parsing the wire byte by hand:
  `TagSizeRestriction` (bits 7..=6, `%pp`),
  `TextEncodingRestriction` (bit 5, `%q`),
  `TextFieldsRestriction` (bits 4..=3, `%rr`),
  `ImageEncodingRestriction` (bit 2, `%s`), and
  `ImageSizeRestriction` (bits 1..=0, `%tt`).
  `Restrictions::{from_wire, to_wire}` is a bijection over all 256
  bytes — the typed sub-fields cover every bit position with no
  gaps. The writer emits an extended header whenever any of `crc`,
  `is_update`, or `restrictions` is set, with the attached-data area
  laid out in the spec-mandated `b, c, d` flag-bit order. Both
  v2.4-only sub-fields (`is_update` + `restrictions`) reject a v2.3
  target with `Error::unsupported`, matching the `with_footer`
  v2.4-only rejection pattern. `parse_tag` is unchanged — it
  internally consumes the verified extended header and returns the
  same `(Id3Tag, usize)` pair as before, so existing callers keep
  working; `parse_tag_with_extended_header` is the additive richer
  sibling.

### Fixed

- The v2.2 header compression bit (§3.1 flag bit 6) was misread as the
  v2.3/v2.4 extended-header flag, silently parsing a tag body the spec
  says to skip ("Since no compression scheme has been decided yet, the
  ID3 decoder (for now) should just ignore the entire tag if the
  compression bit is set"). Both `parse_tag` and
  `parse_tag_with_extended_header` now return the v2.2 version
  envelope with an empty frame list and the correct consumed size so
  container callers can still seek past the tag.
- The v2.3 frame parser previously ignored the frame-flags bytes
  entirely, so a v2.3 frame with any format flag set had its payload
  dispatched at the wrong offset (grouping) or its raw
  ciphertext/deflate bytes fed to a structural parser (encryption /
  compression). The §3.3 header additions are now consumed in spec
  order — decompressed size, then encryption-method byte, then
  group-identifier byte — with grouped frames parsing their real
  payload and encrypted frames surfacing as `Id3Frame::Unknown` with
  the method byte + ciphertext preserved, matching the existing v2.4
  posture.
- v2.4 extended-header CRC writer was silently truncating CRC bit 31
  when serialising the 32-bit CRC into the spec's 5-byte synchsafe
  encoding. Spec §3.2 specifies 5×7-bit bytes = 35 bits, of which
  the top 4 bits of the top synchsafe byte carry CRC bits 31..=28
  (the remaining 3 bits are reserved zero). The writer masked the
  top byte with `0x07` instead of `0x0F`, which clipped CRC bit 31.
  Round-trip with `WriteOptions::with_crc(true)` happened to work
  for tags whose body CRC had bit 31 clear, but ~half of all bodies
  hit the bug and the parser would reject them with "CRC mismatch".
  Mask is now `0x0F` and a regression test (cycles through bodies
  until it lands on a top-bit-set CRC) asserts the round-trip.

- `IPLS` involved-people-list frame (spec v2.3 §4.4). Replaces the
  previous opaque `Id3Frame::Unknown { id: "IPLS", .. }` fallthrough
  with a structured `Id3Frame::Ipls { pairs }` variant. `pairs` is a
  `Vec<(String, String)>` of `(involvement, involvee)` so a writer can
  never emit an odd count — the spec pairing is fundamental, each
  involvement (role, e.g. `producer`, `mixing engineer`, `guitar`)
  names exactly one involvee. The on-wire layout per spec is a single
  encoding byte followed by alternating NUL-terminated strings in the
  declared encoding: `involvement_0\0 involvee_0\0 involvement_1\0
  involvee_1\0 …`. The writer follows the crate's default text
  encoding (UTF-16 with BOM for v2.3 / UTF-8 for the v2.4 envelope
  selector), so arbitrary Unicode role/name strings round-trip
  losslessly. The parser tolerates a non-conforming source that omits
  the final involvee terminator by folding the dangling involvement
  into a pair with an empty involvee — the truncation surfaces
  structurally rather than crashing or silently dropping the string.
  An empty payload surfaces as `Id3Frame::Unknown` so the wire bytes
  round-trip untouched, matching the `EQUA` empty-payload behaviour
  (the spec encoding byte is mandatory). A payload that's only the
  encoding byte parses to an empty pair list (spec lets the pair list
  follow but doesn't forbid zero pairs). IPLS is v2.3-only — v2.4
  dropped it in favour of the `TIPL` text frame (involved-people list)
  and the new `TMCL` musician-credits list, both of which are ordinary
  text frames the existing `Id3Frame::Text` variant already handles —
  so the writer returns `Error::unsupported` under an
  `Id3Version::V2_4` envelope mirroring the `RVAD` / `EQUA` v2.3-only
  contract. Spec also notes "There may only be one `IPLS` frame in
  each tag" — uniqueness is a caller-level concern, matching how the
  crate treats `EQU2` / `MCDI` / `MLLT` / `RVRB` / `RVAD` / `EQUA`.
  Nine new lib tests cover the matrix (UTF-16 BOM writer pinned to a
  hand-computed exact byte sequence; latin1 two-pair parser through
  the encoding-0 path; v2.3 round-trip with three pairs; dangling
  trailing involvement folds into an empty-involvee pair;
  empty-payload Unknown fallback; encoding-byte-only payload yields
  empty pair list; v2.4 writer rejection; zero-pair `to_key_value_pairs`
  invariant; empty pair list round-trip). Two new integration
  round-trip tests (`roundtrip_ipls_v23_multi_pair_unicode` exercising
  five pairs including a Japanese role/name pair through the public
  `write_tag` + `parse_tag` surface, and
  `roundtrip_ipls_writer_rejects_v24` pinning the v2.3-only writer
  contract through the public `write_tag` surface) finish the
  coverage.

- `EQUA` equalisation frame (spec v2.3 §4.13). Replaces the previous
  opaque `Id3Frame::Unknown { id: "EQUA", .. }` fallthrough with a
  structured `Id3Frame::Equa { adjustment_bits, bands }` variant.
  `adjustment_bits` is the per-band magnitude width in bits ($10 = 16
  is the spec-listed norm for MPEG audio; writer rejects the
  spec-forbidden $00). Each `EquaBand` carries the spec's
  increment-decrement bit (`true` for `1 = increment`, `false` for
  `0 = decrement` — stored on the wire as the most-significant bit of
  the 2-byte BE frequency word), a 15-bit `frequency` in Hz
  (0..=32767), and an unsigned big-endian `adjustment` magnitude whose
  width is `ceil(adjustment_bits / 8)` bytes. The writer enforces the
  two spec ordering rules at write time — "the equalisation bands
  should be ordered increasingly with reference to frequency" and "a
  frequency should only be described once in the frame" — by requiring
  the `bands` list to be sorted strictly increasing by frequency; the
  parser preserves wire order so callers can detect a non-conforming
  source. Sub-byte adjustment widths zero-pad at the high end per spec
  "padded in the beginning (highest bits) when not a multiple of
  eight"; over-wide adjustments and frequencies whose top bit collides
  with the inc/dec flag (`>= 0x8000`) are rejected with
  `Error::invalid`. EQUA is v2.3-only — v2.4 dropped it in favour of
  `EQU2` (the v2.4 frames doc lists `EQU2` and does not mention
  `EQUA`), so the writer returns `Error::unsupported` under an
  `Id3Version::V2_4` envelope mirroring the `RVAD` v2.3-only contract.
  v2.2 `EQU` (3-char id) promotes to the same `Equa` variant via the
  dispatch + `v22_promote` table, matching how v2.2 `REV` promotes to
  `RVRB`. Fourteen new lib tests cover the matrix (two-band writer
  pinned to a hand-computed 9-byte sequence; multi-band 16-bit
  round-trip including the 15-bit frequency boundary; sub-byte
  12-bit-width round-trip; `adjustment_bits = $00` writer rejection;
  v2.4 emission rejection; unsorted-bands rejection; duplicate-
  frequency rejection; frequency-overflow rejection; over-wide
  adjustment rejection; empty-payload Unknown fallback; short-trailing
  band drop; zero-pair invariant for `to_key_value_pairs`; v2.2 `EQU`
  → `Equa` dispatch; `v22_promote("EQU") == "EQUA"`). Two new
  integration round-trip tests (`roundtrip_equa_v23_multi_band_16bit`
  exercising five bands across the full 15-bit frequency range and
  `roundtrip_equa_writer_rejects_v24` pinning the v2.3-only writer
  contract through the public `write_tag` surface) finish the
  coverage. Spec notes "There may only be one 'EQUA' frame in each
  tag" — uniqueness is a caller-level concern, matching how the crate
  treats `EQU2` / `MCDI` / `MLLT` / `RVRB` / `RVAD`.

- `RVAD` relative volume adjustment frame (spec v2.3 §4.12). Replaces
  the previous opaque `Id3Frame::Unknown { id: "RVAD", .. }`
  fallthrough with a structured `Id3Frame::Rvad` variant. Carries the
  spec's raw inc/dec bitfield (top two bits reserved `%00`; bits 0..=5
  declare per-channel presence + sign per "1 is increment and 0 is
  decrement"), the `bits_used` byte (per-field width in bits;
  spec-forbidden `$00` rejected by the writer), and four optional
  channel blocks in the spec's wire order — front (`right` then
  `left`), back (`right_back` then `left_back`), centre, bass — each
  exposed as nested `Option<RvadFrontChannels>` / `RvadBackChannels` /
  `RvadChannel` so a caller can distinguish "block absent" from "block
  present, peak omitted". Each `RvadChannel` carries an unsigned
  big-endian `volume_delta` magnitude (sign comes from the parent
  bitfield) and an optional unsigned big-endian `peak`, both
  zero-padded to `ceil(bits_used / 8)` bytes when `bits_used` is not a
  multiple of 8 per spec "padded in the beginning (highest bits)". The
  on-wire layout per block is **all deltas first, then all optional
  peaks** — not interleaved per channel — matching the spec's listing
  order (right delta, left delta, then right peak, left peak); a block
  with `peak.is_empty()` on every channel writes no peak bytes,
  surfacing the spec's "completely omitted" form. The writer rejects
  inc/dec bitfield vs `Option`-block mismatches (e.g. bit 2 set but
  `back == None`), out-of-spec extension orderings (e.g. bass without
  centre, or any extension without front), volume_delta or peak
  exceeding the declared `bits_used` width, the reserved `%00` top
  two bits being non-zero, and most importantly emission under
  `Id3Version::V2_4` — v2.4 dropped `RVAD` entirely in favour of
  `RVA2` (the v2.4 frames doc lists `RVA2` and does not mention
  `RVAD`), so the writer returns `Error::unsupported` mirroring the
  `with_footer` + `V2_3` rejection pattern. Ten new lib tests
  (front-only writer pinned to a hand-computed 10-byte sequence;
  front+back round-trip; six-channel round-trip exercising centre +
  bass extensions; peak-omitted minimal wire form with bytes pinned;
  sub-byte width round-trip at `bits_used = 12`; `bits_used = $00`
  writer rejection; v2.4 emission rejection; inc/dec ↔ block-Option
  mismatch rejection; short-payload Unknown fallback for both 0-byte
  and 1-byte inputs; zero-pair invariant for `to_key_value_pairs`)
  plus two new integration round-trip tests
  (`roundtrip_rvad_v23_all_channels_12bit` exercising all six channel
  blocks at sub-byte width; `roundtrip_rvad_writer_rejects_v24`
  pinning the v2.3-only writer contract through the public
  `write_tag` surface) cover the matrix. Spec notes "There may only
  be one 'RVAD' frame in each tag" — uniqueness is a caller-level
  concern, matching how the crate treats `MCDI` / `MLLT` / `RVRB`.

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
