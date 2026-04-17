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
    // them.
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["x".into()],
            },
            Id3Frame::Unknown {
                id: "PRIV".into(),
                raw: b"arbitrary bytes".to_vec(),
            },
        ],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let priv_raw = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Unknown { id, raw } if id == "PRIV" => Some(raw.clone()),
        _ => None,
    });
    assert_eq!(priv_raw.as_deref(), Some(&b"arbitrary bytes"[..]));
}

#[test]
fn tag_size_matches_written_bytes() {
    let tag = make_tag(Id3Version::V2_3);
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let reported = oxideav_id3::tag_size_at_head(&bytes[0..10]).unwrap();
    assert_eq!(reported, bytes.len());
}
