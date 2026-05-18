//! parse -> write -> parse round-trip tests for ID3v2.3, ID3v2.4, and
//! ID3v1. Each test builds a tag as a Rust value, serialises it with
//! the writer, re-parses the bytes, and asserts the result matches the
//! original (modulo format-specific normalisation).

use oxideav_core::{AttachedPicture, PictureType};
use oxideav_id3::{
    attached_pictures, parse_id3v1, parse_tag, to_key_value_pairs, write_id3v1, write_tag,
    Id3Frame, Id3Tag, Id3Version,
};

fn make_tag(version: Id3Version) -> Id3Tag {
    Id3Tag {
        version,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Round Trip".into()],
            },
            Id3Frame::Text {
                id: "TPE1".into(),
                values: vec!["The Tester".into()],
            },
            Id3Frame::Text {
                id: "TALB".into(),
                values: vec!["Test Album".into()],
            },
            Id3Frame::Text {
                id: "TRCK".into(),
                values: vec!["3/10".into()],
            },
            Id3Frame::Text {
                id: "TCON".into(),
                values: vec!["Rock".into()],
            },
            Id3Frame::Comment {
                lang: *b"eng",
                description: String::new(),
                text: "This is a comment.".into(),
            },
            Id3Frame::UserText {
                description: "REPLAYGAIN_TRACK_GAIN".into(),
                value: "-7.50 dB".into(),
            },
            Id3Frame::Url {
                id: "WOAR".into(),
                url: "https://example.com/artist".into(),
            },
            Id3Frame::Picture(AttachedPicture {
                mime_type: "image/jpeg".into(),
                picture_type: PictureType::FrontCover,
                description: "cover".into(),
                data: vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46],
            }),
            Id3Frame::Lyrics {
                lang: *b"eng",
                description: String::new(),
                text: "La la la".into(),
            },
        ],
    }
}

fn find_text<'a>(tag: &'a Id3Tag, id: &str) -> Option<&'a [String]> {
    tag.frames.iter().find_map(|f| match f {
        Id3Frame::Text { id: i, values } if i == id => Some(values.as_slice()),
        _ => None,
    })
}

fn find_comment(tag: &Id3Tag) -> Option<(&[u8; 3], &str, &str)> {
    tag.frames.iter().find_map(|f| match f {
        Id3Frame::Comment {
            lang,
            description,
            text,
        } => Some((lang, description.as_str(), text.as_str())),
        _ => None,
    })
}

fn find_lyrics(tag: &Id3Tag) -> Option<(&str, &str)> {
    tag.frames.iter().find_map(|f| match f {
        Id3Frame::Lyrics {
            description, text, ..
        } => Some((description.as_str(), text.as_str())),
        _ => None,
    })
}

fn find_user_text<'a>(tag: &'a Id3Tag, desc: &str) -> Option<&'a str> {
    tag.frames.iter().find_map(|f| match f {
        Id3Frame::UserText { description, value } if description == desc => Some(value.as_str()),
        _ => None,
    })
}

fn find_url<'a>(tag: &'a Id3Tag, id: &str) -> Option<&'a str> {
    tag.frames.iter().find_map(|f| match f {
        Id3Frame::Url { id: i, url } if i == id => Some(url.as_str()),
        _ => None,
    })
}

#[test]
fn roundtrip_v23_common_frames() {
    let tag = make_tag(Id3Version::V2_3);
    let bytes = write_tag(&tag, Id3Version::V2_3).expect("write v2.3");
    assert_eq!(&bytes[0..3], b"ID3");
    assert_eq!(bytes[3], 3);
    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse v2.3");
    assert_eq!(consumed, bytes.len());
    assert_eq!(parsed.version, Id3Version::V2_3);

    assert_eq!(
        find_text(&parsed, "TIT2"),
        Some(&["Round Trip".to_string()][..])
    );
    assert_eq!(
        find_text(&parsed, "TPE1"),
        Some(&["The Tester".to_string()][..])
    );
    assert_eq!(
        find_text(&parsed, "TALB"),
        Some(&["Test Album".to_string()][..])
    );
    assert_eq!(find_text(&parsed, "TRCK"), Some(&["3/10".to_string()][..]));
    assert_eq!(find_text(&parsed, "TCON"), Some(&["Rock".to_string()][..]));

    let (lang, desc, text) = find_comment(&parsed).expect("comment");
    assert_eq!(lang, b"eng");
    assert_eq!(desc, "");
    assert_eq!(text, "This is a comment.");

    let (ldesc, ltext) = find_lyrics(&parsed).expect("lyrics");
    assert_eq!(ldesc, "");
    assert_eq!(ltext, "La la la");

    assert_eq!(
        find_user_text(&parsed, "REPLAYGAIN_TRACK_GAIN"),
        Some("-7.50 dB")
    );
    assert_eq!(
        find_url(&parsed, "WOAR"),
        Some("https://example.com/artist")
    );

    let pics = attached_pictures(&parsed);
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].mime_type, "image/jpeg");
    assert_eq!(pics[0].picture_type, PictureType::FrontCover);
    assert_eq!(pics[0].description, "cover");
    assert_eq!(
        pics[0].data,
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46]
    );
}

#[test]
fn roundtrip_v24_common_frames() {
    let tag = make_tag(Id3Version::V2_4);
    let bytes = write_tag(&tag, Id3Version::V2_4).expect("write v2.4");
    assert_eq!(&bytes[0..3], b"ID3");
    assert_eq!(bytes[3], 4);
    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse v2.4");
    assert_eq!(consumed, bytes.len());
    assert_eq!(parsed.version, Id3Version::V2_4);

    assert_eq!(
        find_text(&parsed, "TIT2"),
        Some(&["Round Trip".to_string()][..])
    );
    assert_eq!(
        find_text(&parsed, "TPE1"),
        Some(&["The Tester".to_string()][..])
    );

    let pics = attached_pictures(&parsed);
    assert_eq!(pics.len(), 1);
    assert_eq!(
        pics[0].data,
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46]
    );
}

#[test]
fn roundtrip_v24_multivalue_text() {
    // v2.4 splits multi-value text frames on NUL. Writer must emit NUL
    // separators so the parser recovers both values.
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TPE1".into(),
            values: vec!["Alice".into(), "Bob".into()],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(
        find_text(&parsed, "TPE1"),
        Some(&["Alice".to_string(), "Bob".to_string()][..])
    );
}

#[test]
fn roundtrip_v23_unicode_via_utf16() {
    // v2.3 writer uses UTF-16 with BOM. Ensure non-ASCII titles survive.
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["\u{65E5}\u{672C}\u{8A9E} title".into()],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(
        find_text(&parsed, "TIT2"),
        Some(&["\u{65E5}\u{672C}\u{8A9E} title".to_string()][..])
    );
}

#[test]
fn roundtrip_id3v1_trailer() {
    let tag = Id3Tag {
        version: Id3Version::V1,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Short Title".into()],
            },
            Id3Frame::Text {
                id: "TPE1".into(),
                values: vec!["Someone".into()],
            },
            Id3Frame::Text {
                id: "TALB".into(),
                values: vec!["An Album".into()],
            },
            Id3Frame::Text {
                id: "TYER".into(),
                values: vec!["2024".into()],
            },
            Id3Frame::Text {
                id: "TRCK".into(),
                values: vec!["4".into()],
            },
            Id3Frame::Text {
                id: "TCON".into(),
                values: vec!["Jazz".into()],
            },
            Id3Frame::Comment {
                lang: *b"eng",
                description: String::new(),
                text: "hi".into(),
            },
        ],
    };
    let bytes = write_id3v1(&tag);
    assert_eq!(bytes.len(), 128);
    assert_eq!(&bytes[0..3], b"TAG");
    let parsed = parse_id3v1(&bytes).expect("parse v1");
    let kv = to_key_value_pairs(&parsed);
    assert!(kv.contains(&("title".into(), "Short Title".into())));
    assert!(kv.contains(&("artist".into(), "Someone".into())));
    assert!(kv.contains(&("album".into(), "An Album".into())));
    assert!(kv.contains(&("date".into(), "2024".into())));
    assert!(kv.contains(&("track".into(), "4".into())));
    assert!(kv.contains(&("genre".into(), "Jazz".into())));
    assert!(kv.contains(&("comment".into(), "hi".into())));
}

#[test]
fn roundtrip_preserves_unknown_frames() {
    // Unknown frames must round-trip: write should emit their raw
    // payload verbatim so future code (or other tools) can still read
    // them. Use a synthetic 4-char id that the parser does NOT
    // recognise structurally (so it stays an `Unknown`).
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            },
            Id3Frame::Unknown {
                id: "XBOG".into(),
                raw: b"arbitrary bytes".to_vec(),
            },
        ],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let raw = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Unknown { id, raw } if id == "XBOG" => Some(raw.clone()),
        _ => None,
    });
    assert_eq!(raw.as_deref(), Some(&b"arbitrary bytes"[..]));
}

#[test]
fn tag_size_matches_written_bytes() {
    let tag = make_tag(Id3Version::V2_3);
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let reported = oxideav_id3::tag_size_at_head(&bytes[0..10]).unwrap();
    assert_eq!(reported, bytes.len());
}

/// `POPM` round-trip: email + rating + 4-byte counter survive write
/// → parse without loss, and the parser surfaces `rating` and
/// `rating_count` keys in the Vorbis-style k/v projection.
#[test]
fn roundtrip_popm_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Popularimeter {
            email: "rater@example.com".into(),
            rating: 196,
            counter: 42,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let pop = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Popularimeter {
            email,
            rating,
            counter,
        } => Some((email.clone(), *rating, *counter)),
        _ => None,
    });
    assert_eq!(pop, Some(("rater@example.com".to_string(), 196u8, 42u64)));
    let kv = to_key_value_pairs(&parsed);
    assert!(kv.contains(&("rating:rater@example.com".into(), "196".into())));
    assert!(kv.contains(&("rating_count:rater@example.com".into(), "42".into())));
}

/// `POPM` with the counter wider than 4 bytes survives a round trip.
/// Per spec §4.17 the counter may grow byte-by-byte once it overflows
/// the initial 32-bit form; the writer widens to fit and the parser
/// folds the bytes back into the same `u64`.
#[test]
fn roundtrip_popm_wide_counter() {
    let big = (u32::MAX as u64) + 7;
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Popularimeter {
            email: String::new(),
            rating: 255,
            counter: big,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let pop = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Popularimeter {
            counter, rating, ..
        } => Some((*rating, *counter)),
        _ => None,
    });
    assert_eq!(pop, Some((255u8, big)));
}

/// `PCNT` round-trip: a moderate play count survives, and the
/// k/v projection surfaces `play_count`.
#[test]
fn roundtrip_pcnt_v23() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::PlayCounter { count: 1234 }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let pc = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::PlayCounter { count } => Some(*count),
        _ => None,
    });
    assert_eq!(pc, Some(1234u64));
    let kv = to_key_value_pairs(&parsed);
    assert!(kv.contains(&("play_count".into(), "1234".into())));
}

/// `PRIV` round-trip: owner identifier + opaque binary payload.
#[test]
fn roundtrip_priv() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Private {
            owner: "WM/MediaClassPrimaryID".into(),
            data: vec![0xBC, 0x7D, 0x60, 0xD1, 0x23, 0xE3, 0xE2, 0x4B],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let pv = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Private { owner, data } => Some((owner.clone(), data.clone())),
        _ => None,
    });
    assert_eq!(
        pv,
        Some((
            "WM/MediaClassPrimaryID".to_string(),
            vec![0xBC, 0x7D, 0x60, 0xD1, 0x23, 0xE3, 0xE2, 0x4B]
        ))
    );
}

/// `UFID` round-trip: owner + 16-byte synthetic database id.
#[test]
fn roundtrip_ufid() {
    let id_bytes: Vec<u8> = (0..16).collect();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Ufid {
            owner: "http://musicbrainz.org".into(),
            identifier: id_bytes.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let ufid = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Ufid { owner, identifier } => Some((owner.clone(), identifier.clone())),
        _ => None,
    });
    assert_eq!(ufid, Some(("http://musicbrainz.org".to_string(), id_bytes)));
}

/// `GEOB` round-trip: arbitrary file embedded in the tag survives
/// write → parse with its MIME, filename, description and bytes.
/// Round-tripped under v2.4 (UTF-8) and v2.3 (UTF-16) to exercise
/// both string-encoding paths.
#[test]
fn roundtrip_geob_v24() {
    let payload = b"binary attachment payload\x00\x01\x02".to_vec();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Geob {
            mime_type: "application/octet-stream".into(),
            filename: "notes.bin".into(),
            description: "session notes".into(),
            data: payload.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let g = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Geob {
            mime_type,
            filename,
            description,
            data,
        } => Some((
            mime_type.clone(),
            filename.clone(),
            description.clone(),
            data.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        g,
        Some((
            "application/octet-stream".to_string(),
            "notes.bin".to_string(),
            "session notes".to_string(),
            payload,
        ))
    );
}

#[test]
fn roundtrip_geob_v23_utf16() {
    let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Geob {
            mime_type: "image/png".into(),
            // Non-ASCII filename forces the v2.3 UTF-16-with-BOM path
            // through both the filename and description fields.
            filename: "\u{65E5}\u{672C}.png".into(),
            description: "cover art".into(),
            data: payload.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let g = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Geob {
            mime_type,
            filename,
            description,
            data,
        } => Some((
            mime_type.clone(),
            filename.clone(),
            description.clone(),
            data.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        g,
        Some((
            "image/png".to_string(),
            "\u{65E5}\u{672C}.png".to_string(),
            "cover art".to_string(),
            payload,
        ))
    );
}

/// A truncated `POPM` payload (no rating byte after the email
/// terminator) must not panic — we surface a zero-rated frame.
#[test]
fn popm_truncated_no_rating() {
    // Build the smallest possible truncated POPM frame: just an
    // empty-email NUL terminator, no rating, no counter.
    let mut frame = Vec::new();
    frame.extend_from_slice(b"POPM");
    frame.push(0);
    frame.push(0);
    frame.push(0);
    frame.push(1); // synchsafe size = 1
    frame.extend_from_slice(&[0, 0]); // flags
    frame.push(0); // the NUL-terminator for an empty email
    let mut tag_bytes = Vec::new();
    tag_bytes.extend_from_slice(b"ID3");
    tag_bytes.push(4);
    tag_bytes.push(0);
    tag_bytes.push(0);
    let size = frame.len() as u32;
    tag_bytes.push(((size >> 21) & 0x7F) as u8);
    tag_bytes.push(((size >> 14) & 0x7F) as u8);
    tag_bytes.push(((size >> 7) & 0x7F) as u8);
    tag_bytes.push((size & 0x7F) as u8);
    tag_bytes.extend_from_slice(&frame);
    let (parsed, _) = parse_tag(&tag_bytes).unwrap();
    let pop = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Popularimeter {
            email,
            rating,
            counter,
        } => Some((email.clone(), *rating, *counter)),
        _ => None,
    });
    assert_eq!(pop, Some((String::new(), 0u8, 0u64)));
}
