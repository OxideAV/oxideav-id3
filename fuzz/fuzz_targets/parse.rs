#![no_main]

//! Panic-freedom fuzz target for the `oxideav-id3` parser.
//!
//! Feeds arbitrary attacker-controlled bytes through every public
//! decoder entry point and asserts that none of them panic, abort,
//! debug-overflow, or index out of bounds. The return values are
//! intentionally discarded — the contract under test is *the call
//! returns*, not what it returns.
//!
//! Classic ID3 parser danger spots this target drives:
//!
//! * **Synchsafe size unpack** — the 4-byte synchsafe field on the
//!   header has 28 usable bits, so the announced body can be up to
//!   256 MiB; `parse_tag` must not allocate `header.size` bytes
//!   without cross-checking it against `buf.len()`. The header is
//!   always byte-0..10 of the input, so the first 32 fuzz bytes alone
//!   pump the size field through every reachable combination.
//! * **Frame size > tag size** — each v2.3/v2.4 frame header
//!   declares a 4-byte size that the parser must clamp against the
//!   remaining body, not the input length. A frame announcing
//!   `0xFFFFFFFF` bytes in a 64-byte body must not OOB-index.
//! * **Extended header bounds** — when the v2.3/v2.4 ext-header flag
//!   bit (0x40) is set, the parser pulls a 4-byte size off the body
//!   start. v2.3 uses a regular u32 (un-bounded); v2.4 uses synchsafe
//!   *including the size field itself*. A malformed announce must
//!   bail with `Err(Error::invalid)` rather than slicing past
//!   `body.len()`.
//! * **Encryption + compression flag combos (v2.4)** — the per-frame
//!   format flags carry `data-length indicator`, `unsync`,
//!   `encryption`, `compression`, and `grouping` bits. Compounding
//!   them (e.g. grouping + encryption + data-length-indicator with a
//!   1-byte payload) must not underflow the payload slice when each
//!   sub-step trims the front.
//! * **GEOB length fields** — `GEOB` carries `enc + MIME\0 + filename
//!   (encoded\0) + description (encoded\0) + bytes`. A payload that
//!   omits any of the four NUL terminators must still decode (with
//!   the missing tail folding to empty / data taking the remainder),
//!   not panic.
//! * **SYLT terminators** — `SYLT` walks `(text\0 + 4-byte stamp)`
//!   sync entries forever until the payload runs out. A truncated
//!   final entry (text but no 4-byte stamp) must stop the walk, not
//!   index past the slice.
//! * **Unsynchronisation reverse** — `0xFF 0x00` collapses to `0xFF`
//!   on whole-tag (v2.2/v2.3) or per-frame (v2.4) bases. A payload
//!   that's a single `0xFF` at the tail must not look past the slice
//!   when checking for the trailing `0x00`.
//!
//! The target also runs the downstream APIs (`to_key_value_pairs`,
//! `attached_pictures`, `write_tag` for both v2.3 and v2.4) on every
//! Tag the parser returns so the encoder side gets fuzz coverage too.
//! Re-parsing the writer's output and asserting *that* re-parse
//! produces a structurally-equal Tag is **not** asserted — the
//! writer normalises frame ordering and drops fields it doesn't know
//! how to express, so the relationship is structural-not-bit-equal
//! and would over-constrain the fuzzer.

use libfuzzer_sys::fuzz_target;
use oxideav_id3::{
    attached_pictures, parse_id3v1, parse_tag, tag_size_at_head, to_key_value_pairs, write_id3v1,
    write_tag, write_tag_with_options, Id3Version, UnsyncMode, WriteOptions,
};

fuzz_target!(|data: &[u8]| {
    // 1. Header peek. Must never panic; returns `None` for non-ID3
    //    inputs without touching anything past byte 10.
    let _ = tag_size_at_head(data);
    // Slice variants smaller than 10 must also return cleanly.
    for take in 0..data.len().min(10) {
        let _ = tag_size_at_head(&data[..take]);
    }

    // 2. ID3v2 whole-tag parse. The danger spots listed in the file
    //    header all live behind this entry point.
    let parsed = parse_tag(data);

    // 3. ID3v1 trailer parse. The contract is "either return a tag or
    //    None"; the parser must not panic on a 128-byte input that
    //    happens to start with `TAG` but has garbage everywhere else.
    let _ = parse_id3v1(data);
    // Also exercise the explicit last-128-bytes pattern callers use:
    // tagged trailers usually live at the file end, so test from there.
    if data.len() >= 128 {
        let _ = parse_id3v1(&data[data.len() - 128..]);
    }

    // 4. Downstream APIs. Anything the parser accepted must survive
    //    a `to_key_value_pairs` + `attached_pictures` walk; both are
    //    pure functions over the parsed structure.
    let Ok((tag, _consumed)) = parsed else {
        return;
    };
    let _ = to_key_value_pairs(&tag);
    let _ = attached_pictures(&tag);
    let _ = write_id3v1(&tag);

    // 5. Writer. v2.2 input is not writeable (the encoder targets 2.3
    //    or 2.4) so promote the parsed tag's version to v2.3 first
    //    before exercising the writer paths.
    let writeable = match tag.version {
        Id3Version::V2_2 | Id3Version::V2_3 | Id3Version::V2_4 => tag.clone(),
        Id3Version::V1 => return,
    };
    // Both writer targets must finish without panicking. The output is
    // discarded; we are not asserting any round-trip property because
    // the writer canonicalises (drops unknown flags, reorders fields)
    // and that's by design.
    let _ = write_tag(&writeable, Id3Version::V2_3);
    let _ = write_tag(&writeable, Id3Version::V2_4);

    // 6. Unsync-mode write paths. Each `write_tag_with_options` call
    //    must finish without panicking under either unsync strategy,
    //    on either target version. Whole-tag unsync on a body
    //    containing many `0xFF` sequences exercises `apply_unsync`'s
    //    quadratic-allocation-safety path; per-frame unsync (v2.4)
    //    drives `apply_unsync` once per frame. Re-parsing the output
    //    must also panic-freedom-pass — a writer-induced bad header
    //    that the parser then OOB-reads would be caught here.
    let modes = [UnsyncMode::None, UnsyncMode::WholeTag, UnsyncMode::PerFrame];
    let versions = [Id3Version::V2_3, Id3Version::V2_4];
    for &mode in &modes {
        for &ver in &versions {
            for &crc in &[false, true] {
                for &footer in &[false, true] {
                    // 7. Extended-header CRC × unsync × footer
                    //    combinations. With `crc=true` the writer
                    //    prepends a CRC-bearing extended header; with
                    //    `footer=true` (v2.4 only — v2.3 will error
                    //    out, which is fine) the writer appends a
                    //    10-byte trailer and sets the tag-header's
                    //    footer-present bit. The parser walks the
                    //    optional extended header, verifies the CRC,
                    //    and additionally verifies the footer's
                    //    identifier/version/flags/size mirror the
                    //    header. Every (mode, ver, crc, footer)
                    //    combination must remain panic-free.
                    let opts = WriteOptions::new()
                        .with_unsync(mode)
                        .with_crc(crc)
                        .with_footer(footer);
                    if let Ok(bytes) = write_tag_with_options(&writeable, ver, &opts) {
                        let _ = parse_tag(&bytes);
                    }
                }
            }
        }
    }
});
