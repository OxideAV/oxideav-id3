//! Deterministic mutation smoke test for the attacker-facing parse
//! surfaces added with the chapter / enhanced-tag work.
//!
//! The real fuzzing budget lives in the daily `cargo fuzz` workflow;
//! this test is the fast, deterministic slice of it that runs on every
//! CI push: take well-formed fixtures (a chapterised v2.3/v2.4 tag, an
//! enhanced 355-byte trailer), apply a few thousand cheap xorshift
//! mutations — byte flips, truncations, length-field corruption — and
//! drive every parse + walker + writer entry point over the result.
//! The only assertion is the implicit one: nothing panics, overflows,
//! or indexes out of bounds. Structural correctness is covered by the
//! typed round-trip tests; this file exists so a bounds regression in
//! `parse_chap` / `parse_ctoc` / `parse_id3v1_enhanced` surfaces on
//! the very next push instead of at the next scheduled fuzz run.

use oxideav_id3::{
    parse_id3v1, parse_id3v1_enhanced, parse_tag, tag_size_at_head, to_key_value_pairs,
    write_id3v1_enhanced, write_tag, EnhancedTag, Id3Frame, Id3Tag, Id3Version, Id3v1Tag,
};

/// Small deterministic PRNG (xorshift64*) so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Build a chapter-heavy tag and serialise it under `version`.
fn chapter_fixture(version: Id3Version) -> Vec<u8> {
    let tag = Id3Tag {
        version,
        frames: vec![
            Id3Frame::TableOfContents {
                element_id: "root".into(),
                top_level: true,
                ordered: true,
                child_ids: vec!["part".into(), "c2".into()],
                sub_frames: vec![Id3Frame::Text {
                    id: "TIT2".into(),
                    values: vec!["Everything".into()],
                }],
            },
            Id3Frame::TableOfContents {
                element_id: "part".into(),
                top_level: false,
                ordered: true,
                child_ids: vec!["c1".into()],
                sub_frames: Vec::new(),
            },
            Id3Frame::Chapter {
                element_id: "c1".into(),
                start_time_ms: 0,
                end_time_ms: 60_000,
                start_offset: Some(0xFF00),
                end_offset: None,
                sub_frames: vec![Id3Frame::Text {
                    id: "TIT2".into(),
                    values: vec!["One".into()],
                }],
            },
            Id3Frame::Chapter {
                element_id: "c2".into(),
                start_time_ms: 60_000,
                end_time_ms: 120_000,
                start_offset: None,
                end_offset: Some(0xFFFF_FF00),
                sub_frames: vec![Id3Frame::UserUrl {
                    description: "d".into(),
                    url: "https://example.invalid/x".into(),
                }],
            },
        ],
    };
    write_tag(&tag, version).expect("fixture writes")
}

/// Build a 355-byte enhanced trailer fixture.
fn enhanced_fixture() -> Vec<u8> {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["T".repeat(80)],
            },
            Id3Frame::Text {
                id: "TCON".into(),
                values: vec!["Custom Genre Text".into()],
            },
            Id3Frame::Text {
                id: "TRCK".into(),
                values: vec!["9".into()],
            },
        ],
    };
    write_id3v1_enhanced(&tag)
}

/// Run every parse-side entry point over `data`; discard results.
fn drive(data: &[u8]) {
    let _ = tag_size_at_head(data);
    if let Ok((tag, _)) = parse_tag(data) {
        let _ = to_key_value_pairs(&tag);
        let _ = tag.chapters();
        let _ = tag.tables_of_contents();
        let _ = tag.top_level_toc();
        let _ = tag.ordered_chapters();
        // Whatever parsed must also write (or refuse) panic-free.
        let _ = write_tag(&tag, Id3Version::V2_3);
        let _ = write_tag(&tag, Id3Version::V2_4);
    }
    let _ = parse_id3v1(data);
    let _ = Id3v1Tag::parse(data);
    let _ = EnhancedTag::parse(data);
    if let Some((v1, plus)) = parse_id3v1_enhanced(data) {
        let _ = v1.to_bytes();
        if let Some(p) = &plus {
            let _ = p.to_bytes();
            let _ = p.speed_kind();
            let _ = p.effective_genre(&v1);
        }
        let merged = v1.to_tag_with_enhanced(plus.as_ref());
        let _ = write_id3v1_enhanced(&merged);
    }
}

#[test]
fn mutated_chapter_and_enhanced_fixtures_never_panic() {
    let fixtures = [
        chapter_fixture(Id3Version::V2_3),
        chapter_fixture(Id3Version::V2_4),
        enhanced_fixture(),
    ];
    let mut rng = XorShift(0x0BAD_5EED_CAFE_F00D);
    for fixture in &fixtures {
        // The pristine fixture first.
        drive(fixture);
        for _ in 0..4000 {
            let mut data = fixture.clone();
            // 1..=4 point mutations: flip a byte to a random value.
            let flips = (rng.next() % 4) as usize + 1;
            for _ in 0..flips {
                let idx = (rng.next() as usize) % data.len();
                data[idx] = rng.next() as u8;
            }
            // A third of the time, also truncate.
            if rng.next() % 3 == 0 {
                let keep = (rng.next() as usize) % (data.len() + 1);
                data.truncate(keep);
            }
            drive(&data);
        }
        // Systematic single-byte truncations of the pristine bytes:
        // every prefix must parse or fail cleanly.
        for take in 0..fixture.len() {
            drive(&fixture[..take]);
        }
    }
}
