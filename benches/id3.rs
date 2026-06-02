//! Criterion benchmarks for the ID3 parser + writer hot paths.
//!
//! Round 209 (depth-mode benchmarks): four scenarios cover the three
//! public surfaces a typical caller exercises on an MP3-resident tag.
//! Each input fixture is hand-built from the wire layout described in
//! `docs/container/id3/id3v2.4.0-structure.html` (ID3v2.4 structure §3
//! — header + extended-header + frame layout) and
//! `docs/container/id3/id3v2.4.0-frames.html` (ID3v2.4 frames §4 — per
//! frame body shape). Randomised picture payload bytes are produced by
//! a fixed-seed xorshift so the compiler cannot constant-fold the
//! 60 KiB copy out of the timed region.
//!
//! Scenarios:
//!
//!   - **bench_parse_minimal_v24**: a hand-built ~120-byte v2.4 tag
//!     with TIT2 / TPE1 / TALB text frames plus one short COMM. Drives
//!     `tag_size_at_head` → `parse_tag` → `to_key_value_pairs`. This
//!     is the structural-overhead floor — frame-header walk, synchsafe
//!     size decode, text decode, key/value flattening.
//!
//!   - **bench_parse_apic_heavy_v24**: a ~64 KiB v2.4 tag whose
//!     dominant cost is a 60 KiB APIC frame (synthetic xorshift bytes;
//!     `image/jpeg` MIME, front-cover picture-type). Drives
//!     `parse_tag` against a tag where the inner picture copy
//!     dominates the per-call cost — the right shape for measuring
//!     bytes-per-second throughput of the APIC parse path.
//!
//!   - **bench_write_text_v24**: round-trips the minimal-v24 fixture
//!     through `write_tag` with the default `WriteOptions`. The bench
//!     parses once outside the timed region and then re-serialises the
//!     resulting `Id3Tag` per iteration. Throughput uses the produced
//!     output length so MiB/s reflects the actual write surface.
//!
//!   - **bench_parse_id3v1**: the 128-byte trailer parse — the
//!     baseline-floor number. Drives `parse_id3v1` over a hand-built
//!     trailer per ID3v1 spec (`TAG` + 30 title + 30 artist + 30 album
//!     + 4 year + 28 comment + 1 zero + 1 track + 1 genre).
//!
//! Run with:
//!     cargo bench -p oxideav-id3 --bench id3

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

use oxideav_id3::{
    parse_id3v1, parse_tag, tag_size_at_head, to_key_value_pairs, write_tag, Id3Tag, Id3Version,
    WriteOptions,
};

// ---------- deterministic byte source ----------

/// Tiny xorshift32 PRNG used only to defeat compiler constant-folding
/// on the 60 KiB APIC payload. The numeric value of any individual
/// byte is unimportant; only the structural absence of long
/// compressible runs matters.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

fn synthetic_picture_bytes(seed: u32, len: usize) -> Vec<u8> {
    let mut state = seed | 1; // xorshift32 needs nonzero state
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let w = xorshift32(&mut state);
        out.push((w & 0xFF) as u8);
        out.push(((w >> 8) & 0xFF) as u8);
        out.push(((w >> 16) & 0xFF) as u8);
        out.push(((w >> 24) & 0xFF) as u8);
    }
    out.truncate(len);
    out
}

// ---------- ID3v2.4 byte-layout helpers (from spec §3) ----------

/// Encode `n` as 4 synchsafe bytes (each carries 7 data bits, MSB = 0)
/// per ID3v2.4 spec §3.1.
fn synchsafe4(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

/// Build a v2.4 tag header (10 bytes) wrapping a body of length
/// `body_len`. Header layout per spec §3.1: `"ID3" + ver_major + ver_rev
/// + flags + 4 synchsafe size bytes`.
fn v24_header(body_len: usize) -> [u8; 10] {
    let s = synchsafe4(body_len as u32);
    [b'I', b'D', b'3', 0x04, 0x00, 0x00, s[0], s[1], s[2], s[3]]
}

/// Build a v2.4 frame header (10 bytes) per spec §4.1: 4-byte frame ID,
/// 4 synchsafe size bytes (frame payload length excluding the header),
/// 2 flag bytes.
fn v24_frame_header(id: &[u8; 4], payload_len: usize) -> [u8; 10] {
    let s = synchsafe4(payload_len as u32);
    [
        id[0], id[1], id[2], id[3], s[0], s[1], s[2], s[3], 0x00, 0x00,
    ]
}

/// Build a v2.4 text frame (`T***`) per spec §4.2: 1 encoding byte
/// (`$03` = UTF-8) followed by the value bytes.
fn v24_text_frame(id: &[u8; 4], value: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + value.len());
    payload.push(0x03); // UTF-8
    payload.extend_from_slice(value.as_bytes());
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.extend_from_slice(&v24_frame_header(id, payload.len()));
    frame.extend_from_slice(&payload);
    frame
}

/// Build a v2.4 `COMM` frame per spec §4.10: 1 encoding byte + 3 lang
/// bytes + NUL-terminated description + actual comment text.
fn v24_comm_frame(lang: &[u8; 3], description: &str, text: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 3 + description.len() + 1 + text.len());
    payload.push(0x03); // UTF-8
    payload.extend_from_slice(lang);
    payload.extend_from_slice(description.as_bytes());
    payload.push(0x00); // description terminator (single NUL in UTF-8)
    payload.extend_from_slice(text.as_bytes());
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.extend_from_slice(&v24_frame_header(b"COMM", payload.len()));
    frame.extend_from_slice(&payload);
    frame
}

/// Build a v2.4 `APIC` frame per spec §4.14: 1 encoding byte ($03 =
/// UTF-8) + NUL-terminated MIME + 1 picture-type byte + NUL-terminated
/// description + picture bytes.
fn v24_apic_frame(
    mime: &str,
    picture_type: u8,
    description: &str,
    picture_bytes: &[u8],
) -> Vec<u8> {
    let mut payload =
        Vec::with_capacity(1 + mime.len() + 1 + 1 + description.len() + 1 + picture_bytes.len());
    payload.push(0x03); // UTF-8
    payload.extend_from_slice(mime.as_bytes());
    payload.push(0x00); // MIME terminator
    payload.push(picture_type);
    payload.extend_from_slice(description.as_bytes());
    payload.push(0x00); // description terminator (single NUL in UTF-8)
    payload.extend_from_slice(picture_bytes);
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.extend_from_slice(&v24_frame_header(b"APIC", payload.len()));
    frame.extend_from_slice(&payload);
    frame
}

/// Assemble the minimal v2.4 fixture used by scenarios 1 and 3.
fn fixture_minimal_v24() -> Vec<u8> {
    let mut body = Vec::with_capacity(128);
    body.extend_from_slice(&v24_text_frame(b"TIT2", "Benchmark Track Title"));
    body.extend_from_slice(&v24_text_frame(b"TPE1", "Benchmark Artist"));
    body.extend_from_slice(&v24_text_frame(b"TALB", "Benchmark Album"));
    body.extend_from_slice(&v24_comm_frame(
        b"eng",
        "",
        "round 209 bench harness fixture",
    ));
    let mut out = Vec::with_capacity(10 + body.len());
    out.extend_from_slice(&v24_header(body.len()));
    out.extend_from_slice(&body);
    out
}

/// Assemble the APIC-heavy v2.4 fixture used by scenario 2.
fn fixture_apic_heavy_v24() -> Vec<u8> {
    let picture_bytes = synthetic_picture_bytes(0xC0DECA7E, 60 * 1024);
    let mut body = Vec::with_capacity(64 * 1024);
    body.extend_from_slice(&v24_text_frame(b"TIT2", "APIC-heavy Track"));
    body.extend_from_slice(&v24_text_frame(b"TPE1", "APIC-heavy Artist"));
    body.extend_from_slice(&v24_apic_frame(
        "image/jpeg",
        0x03, // FrontCover
        "",
        &picture_bytes,
    ));
    let mut out = Vec::with_capacity(10 + body.len());
    out.extend_from_slice(&v24_header(body.len()));
    out.extend_from_slice(&body);
    out
}

/// Assemble the 128-byte ID3v1 trailer used by scenario 4. Layout per
/// the ID3v1 spec: `TAG` + 30 title + 30 artist + 30 album + 4 year +
/// 28 comment + 1 zero + 1 track + 1 genre.
fn fixture_id3v1_trailer() -> Vec<u8> {
    let mut trailer = vec![0u8; 128];
    trailer[..3].copy_from_slice(b"TAG");
    let title = b"Benchmark Track Title";
    trailer[3..3 + title.len()].copy_from_slice(title);
    let artist = b"Benchmark Artist";
    trailer[33..33 + artist.len()].copy_from_slice(artist);
    let album = b"Benchmark Album";
    trailer[63..63 + album.len()].copy_from_slice(album);
    let year = b"2026";
    trailer[93..97].copy_from_slice(year);
    let comment = b"round 209 bench fixture";
    trailer[97..97 + comment.len()].copy_from_slice(comment);
    trailer[125] = 0; // ID3v1.1 zero separator
    trailer[126] = 7; // track 7
    trailer[127] = 17; // Rock
    trailer
}

// ---------- benches ----------

fn bench_parse_minimal_v24(c: &mut Criterion) {
    let fixture = fixture_minimal_v24();
    let mut group = c.benchmark_group("parse_minimal_v24");
    group.throughput(Throughput::Bytes(fixture.len() as u64));
    group.bench_function("parse", |b| {
        b.iter(|| {
            let head: &[u8; 10] = fixture[..10].try_into().unwrap();
            let total = tag_size_at_head(black_box(head)).unwrap();
            assert_eq!(total, fixture.len());
            let (tag, consumed) = parse_tag(black_box(&fixture)).unwrap();
            let kv = to_key_value_pairs(black_box(&tag));
            black_box((consumed, kv));
        });
    });
    group.finish();
}

fn bench_parse_apic_heavy_v24(c: &mut Criterion) {
    let fixture = fixture_apic_heavy_v24();
    let mut group = c.benchmark_group("parse_apic_heavy_v24");
    group.throughput(Throughput::Bytes(fixture.len() as u64));
    group.bench_function("parse", |b| {
        b.iter(|| {
            let (tag, consumed) = parse_tag(black_box(&fixture)).unwrap();
            black_box((tag, consumed));
        });
    });
    group.finish();
}

fn bench_write_text_v24(c: &mut Criterion) {
    let fixture = fixture_minimal_v24();
    // Parse once outside the timed region so the bench measures only
    // the write path.
    let (tag, _) = parse_tag(&fixture).expect("minimal v24 fixture parses");
    // Pre-compute the serialised length so Throughput::Bytes reflects
    // the actual output size produced inside the timed region.
    let probe = write_tag(&tag, Id3Version::V2_4).expect("minimal v24 fixture serialises");
    let serialised_len = probe.len();
    let _ = WriteOptions::default(); // verifies the import is used
    let mut group = c.benchmark_group("write_text_v24");
    group.throughput(Throughput::Bytes(serialised_len as u64));
    group.bench_function("write", |b| {
        b.iter(|| {
            let bytes = write_tag(black_box(&tag), Id3Version::V2_4).unwrap();
            black_box(bytes);
        });
    });
    group.finish();
    // Defeat the dead-code lint on the unused tag once the closure
    // captures by reference.
    black_box::<Id3Tag>(tag);
}

fn bench_parse_id3v1(c: &mut Criterion) {
    let trailer = fixture_id3v1_trailer();
    let mut group = c.benchmark_group("parse_id3v1");
    group.throughput(Throughput::Bytes(trailer.len() as u64));
    group.bench_function("parse", |b| {
        b.iter(|| {
            let tag = parse_id3v1(black_box(&trailer)).unwrap();
            black_box(tag);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_parse_minimal_v24,
    bench_parse_apic_heavy_v24,
    bench_write_text_v24,
    bench_parse_id3v1,
);
criterion_main!(benches);
