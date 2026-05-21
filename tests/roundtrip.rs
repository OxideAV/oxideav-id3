//! parse -> write -> parse round-trip tests for ID3v2.3, ID3v2.4, and
//! ID3v1. Each test builds a tag as a Rust value, serialises it with
//! the writer, re-parses the bytes, and asserts the result matches the
//! original (modulo format-specific normalisation).

use oxideav_core::{AttachedPicture, PictureType};
use oxideav_id3::{
    attached_pictures, parse_id3v1, parse_tag, to_key_value_pairs, write_id3v1, write_tag,
    Id3Frame, Id3Tag, Id3Version, Rva2Channel,
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

/// `USER` terms-of-use frame round-trips both directions. v2.3 uses
/// UTF-16-with-BOM internally for the text payload; the language
/// triplet is always plain ASCII.
#[test]
fn roundtrip_user_v23_and_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![
            Id3Frame::TermsOfUse {
                lang: *b"eng",
                text: "All rights reserved.".into(),
            },
            Id3Frame::TermsOfUse {
                lang: *b"jpn",
                text: "\u{3059}\u{3079}\u{3066}\u{306E}\u{6A29}\u{5229}".into(),
            },
        ],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let mut got: Vec<([u8; 3], String)> = parsed
        .frames
        .iter()
        .filter_map(|f| match f {
            Id3Frame::TermsOfUse { lang, text } => Some((*lang, text.clone())),
            _ => None,
        })
        .collect();
    got.sort_by_key(|(l, _)| *l);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, *b"eng");
    assert_eq!(got[0].1, "All rights reserved.");
    assert_eq!(got[1].0, *b"jpn");
    assert_eq!(got[1].1, "\u{3059}\u{3079}\u{3066}\u{306E}\u{6A29}\u{5229}");
    let kv = to_key_value_pairs(&parsed);
    assert!(kv.contains(&("termsofuse:eng".into(), "All rights reserved.".into())));
}

/// `OWNE` ownership frame round-trips through both v2.3 and v2.4.
/// The 8-byte date field is fixed-width with no terminator; the
/// writer pads short input with spaces so the on-wire layout is
/// always parseable.
#[test]
fn roundtrip_owne_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Ownership {
            price: "USD9.99".into(),
            date: "20260114".into(),
            seller: "Example Records Inc.".into(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let own = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Ownership {
            price,
            date,
            seller,
        } => Some((price.clone(), date.clone(), seller.clone())),
        _ => None,
    });
    assert_eq!(
        own,
        Some((
            "USD9.99".to_string(),
            "20260114".to_string(),
            "Example Records Inc.".to_string()
        ))
    );
    let kv = to_key_value_pairs(&parsed);
    assert!(kv.contains(&("ownership_price".into(), "USD9.99".into())));
    assert!(kv.contains(&("ownership_date".into(), "20260114".into())));
    assert!(kv.contains(&("ownership_seller".into(), "Example Records Inc.".into())));
}

/// `OWNE` short-date case: a 6-byte caller string gets space-padded
/// to the spec's 8-byte width on write, and reads back as the same
/// 8-character string. Spec only allows YYYYMMDD; this confirms the
/// writer doesn't silently corrupt the on-wire layout when input is
/// out-of-shape.
#[test]
fn owne_short_date_pads_to_eight() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Ownership {
            price: "EUR1".into(),
            date: "2026".into(),
            seller: "S".into(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let date = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Ownership { date, .. } => Some(date.clone()),
        _ => None,
    });
    assert_eq!(date, Some("2026    ".to_string()));
}

/// `COMR` commercial frame round-trip with the full optional logo
/// block populated. Exercises the price + date + URL + received_as
/// + encoded-string seller/description + MIME + binary logo path.
#[test]
fn roundtrip_comr_with_logo() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Commercial {
            price: "USD9.99/EUR8.99".into(),
            valid_until: "20271231".into(),
            contact_url: "https://example.com/buy".into(),
            received_as: 3, // File over the Internet
            seller: "Example Records".into(),
            description: "Deluxe Edition".into(),
            logo_mime: "image/png".into(),
            logo_data: vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let cm = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Commercial {
            price,
            valid_until,
            contact_url,
            received_as,
            seller,
            description,
            logo_mime,
            logo_data,
        } => Some((
            price.clone(),
            valid_until.clone(),
            contact_url.clone(),
            *received_as,
            seller.clone(),
            description.clone(),
            logo_mime.clone(),
            logo_data.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        cm,
        Some((
            "USD9.99/EUR8.99".to_string(),
            "20271231".to_string(),
            "https://example.com/buy".to_string(),
            3u8,
            "Example Records".to_string(),
            "Deluxe Edition".to_string(),
            "image/png".to_string(),
            vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        ))
    );
}

/// `COMR` without the optional logo block: spec says "These two last
/// fields may be omitted if no picture is attached." The writer
/// drops the MIME + logo entirely when both are empty; the parser
/// reads back the same.
#[test]
fn roundtrip_comr_without_logo() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Commercial {
            price: "JPY1000".into(),
            valid_until: "20261231".into(),
            contact_url: "mailto:sales@example.com".into(),
            received_as: 0,
            seller: "Seller".into(),
            description: "Track".into(),
            logo_mime: String::new(),
            logo_data: Vec::new(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let cm = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Commercial {
            logo_mime,
            logo_data,
            received_as,
            valid_until,
            ..
        } => Some((
            logo_mime.clone(),
            logo_data.clone(),
            *received_as,
            valid_until.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        cm,
        Some((String::new(), Vec::<u8>::new(), 0u8, "20261231".to_string()))
    );
}

/// `SYTC` synchronised-tempo round-trip. Three codes exercising the
/// 1-byte form ($02..=$FE), the reserved $00 (beat-free), and the
/// 2-byte $FF extension form (256..=510 BPM).
#[test]
fn roundtrip_sytc() {
    let codes = vec![
        (0u16, 0u32),      // beat-free at t=0
        (120, 1_000),      // 120 BPM at t=1000 ms
        (300, 5_500),      // 300 BPM ($FF $2D)
        (510, 12_000_000), // upper end of $FF extension
    ];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::SyncedTempo {
            time_format: 0x02, // milliseconds
            codes: codes.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::SyncedTempo { time_format, codes } => Some((*time_format, codes.clone())),
        _ => None,
    });
    assert_eq!(got, Some((0x02u8, codes)));
}

/// `RVA2` round-trip with two channels — master volume at +2 dB and
/// front-right at -3 dB with a 16-bit peak. Confirms the Q9.7
/// encoding survives, the variable-width peak field round-trips,
/// and identification + channels parse back in order.
#[test]
fn roundtrip_rva2_multi_channel() {
    // +2 dB = 2 * 512 = 1024 = $04 00
    // -3 dB = -3 * 512 = -1536 = $FA 00 (two's complement)
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Rva2 {
            identification: "track".into(),
            channels: vec![
                Rva2Channel {
                    channel_type: 0x01, // Master volume
                    volume_adjustment: 1024,
                    bits_peak: 0,
                    peak: Vec::new(),
                },
                Rva2Channel {
                    channel_type: 0x02, // Front right
                    volume_adjustment: -1536,
                    bits_peak: 16,
                    peak: vec![0x12, 0x34],
                },
            ],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Rva2 {
            identification,
            channels,
        } => Some((identification.clone(), channels.clone())),
        _ => None,
    });
    let (id, ch) = got.expect("RVA2");
    assert_eq!(id, "track");
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[0].channel_type, 0x01);
    assert_eq!(ch[0].volume_adjustment, 1024);
    assert_eq!(ch[0].bits_peak, 0);
    assert!(ch[0].peak.is_empty());
    assert_eq!(ch[1].channel_type, 0x02);
    assert_eq!(ch[1].volume_adjustment, -1536);
    assert_eq!(ch[1].bits_peak, 16);
    assert_eq!(ch[1].peak, vec![0x12, 0x34]);
}

/// `RVA2` peak with a non-multiple-of-8 bit width: 12 bits → 2 bytes
/// on the wire per spec ("always padded to whole bytes, setting the
/// most significant bits to zero"). The writer pads, the parser
/// reads the padded form back verbatim.
#[test]
fn roundtrip_rva2_padded_peak() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Rva2 {
            identification: "album".into(),
            channels: vec![Rva2Channel {
                channel_type: 0x01,
                volume_adjustment: 0,
                bits_peak: 12,
                peak: vec![0x0F, 0xFF],
            }],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let ch = parsed
        .frames
        .iter()
        .find_map(|f| match f {
            Id3Frame::Rva2 { channels, .. } => Some(channels.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ch[0].bits_peak, 12);
    assert_eq!(ch[0].peak, vec![0x0F, 0xFF]);
}

/// `EQU2` round-trip: linear interpolation, four band/adjustment
/// points. Frequencies are in 1/2 Hz units (so 2000 = 1000 Hz);
/// adjustments are Q9.7 dB.
#[test]
fn roundtrip_equ2_linear() {
    let pts = vec![
        (200u16, 512i16), // 100 Hz, +1 dB
        (2_000, -1_024),  // 1000 Hz, -2 dB
        (8_000, 0),       // 4000 Hz, flat
        (24_000, 1_024),  // 12000 Hz, +2 dB
    ];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Equ2 {
            interpolation: 1, // Linear
            identification: "headphones".into(),
            points: pts.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Equ2 {
            interpolation,
            identification,
            points,
        } => Some((*interpolation, identification.clone(), points.clone())),
        _ => None,
    });
    assert_eq!(got, Some((1u8, "headphones".to_string(), pts)));
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

/// `MCDI` music CD identifier round-trip. The TOC body is opaque
/// binary so we just confirm it survives parse + write byte-exact.
#[test]
fn roundtrip_mcdi() {
    let toc: Vec<u8> = (0..200u8).collect();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::MusicCdId { toc: toc.clone() }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::MusicCdId { toc } => Some(toc.clone()),
        _ => None,
    });
    assert_eq!(got, Some(toc));
}

/// `ETCO` event timing codes round-trip. Three events: end-of-silence
/// at t=0, intro start at t=1500 ms, outro end at t=180_000.
#[test]
fn roundtrip_etco() {
    let events = vec![(0x01u8, 0u32), (0x02, 1_500), (0x05, 180_000)];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::EventTimingCodes {
            time_format: 0x02, // milliseconds
            events: events.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::EventTimingCodes {
            time_format,
            events,
        } => Some((*time_format, events.clone())),
        _ => None,
    });
    assert_eq!(got, Some((0x02u8, events)));
}

/// `SYLT` synchronised lyrics round-trip in both v2.3 (UTF-16) and
/// v2.4 (UTF-8) so we exercise the encoding-aware terminator length
/// inside the sync-record loop.
#[test]
fn roundtrip_sylt() {
    for &v in &[Id3Version::V2_3, Id3Version::V2_4] {
        let syncs = vec![
            ("Strang".to_string(), 0u32),
            ("ers ".to_string(), 1_000),
            ("in the ".to_string(), 2_000),
            ("night".to_string(), 3_000),
        ];
        let tag = Id3Tag {
            version: v,
            frames: vec![Id3Frame::SyncedLyrics {
                lang: *b"eng",
                time_format: 0x02,
                content_type: 0x01, // lyrics
                description: "Sinatra".into(),
                syncs: syncs.clone(),
            }],
        };
        let bytes = write_tag(&tag, v).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let got = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::SyncedLyrics {
                lang,
                time_format,
                content_type,
                description,
                syncs,
            } => Some((
                *lang,
                *time_format,
                *content_type,
                description.clone(),
                syncs.clone(),
            )),
            _ => None,
        });
        assert_eq!(
            got,
            Some((*b"eng", 0x02u8, 0x01u8, "Sinatra".to_string(), syncs)),
            "version {v:?}"
        );
    }
}

/// `POSS` position synchronisation round-trip — a 45-second-in
/// resume point in milliseconds.
#[test]
fn roundtrip_poss() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::PositionSync {
            time_format: 0x02,
            position: 45_000,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::PositionSync {
            time_format,
            position,
        } => Some((*time_format, *position)),
        _ => None,
    });
    assert_eq!(got, Some((0x02u8, 45_000u32)));
}

/// `RBUF` recommended buffer size round-trip with the embedded-info
/// flag set and a non-zero offset-to-next.
#[test]
fn roundtrip_rbuf() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::RecommendedBuffer {
            buffer_size: 0x12_3456,
            embedded_info: true,
            offset_to_next: 0xCAFE_BABE,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::RecommendedBuffer {
            buffer_size,
            embedded_info,
            offset_to_next,
        } => Some((*buffer_size, *embedded_info, *offset_to_next)),
        _ => None,
    });
    assert_eq!(got, Some((0x0012_3456u32, true, 0xCAFE_BABEu32)));
}

/// `RBUF` buffer-size clamping: a value above 24-bit max gets clamped
/// to 0xFF_FFFF on write (per spec the field is 3 bytes wide).
#[test]
fn rbuf_clamps_oversize_buffer_size() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::RecommendedBuffer {
            buffer_size: 0xFFFF_FFFF,
            embedded_info: false,
            offset_to_next: 0,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::RecommendedBuffer { buffer_size, .. } => Some(*buffer_size),
        _ => None,
    });
    assert_eq!(got, Some(0x00FF_FFFFu32));
}

/// `SEEK` round-trip — a single 32-bit offset.
#[test]
fn roundtrip_seek() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Seek {
            min_offset_to_next_tag: 0x0010_0000,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Seek {
            min_offset_to_next_tag,
        } => Some(*min_offset_to_next_tag),
        _ => None,
    });
    assert_eq!(got, Some(0x0010_0000u32));
}

/// `SIGN` round-trip with a small binary signature.
#[test]
fn roundtrip_sign() {
    let sig = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Signature {
            group_symbol: 0x80,
            signature: sig.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Signature {
            group_symbol,
            signature,
        } => Some((*group_symbol, signature.clone())),
        _ => None,
    });
    assert_eq!(got, Some((0x80u8, sig)));
}

/// `AENC` audio-encryption round-trip with a non-trivial encryption
/// info block.
#[test]
fn roundtrip_aenc() {
    let info = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::AudioEncryption {
            owner: "https://example.com/crypto".into(),
            preview_start: 100,
            preview_length: 50,
            encryption_info: info.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::AudioEncryption {
            owner,
            preview_start,
            preview_length,
            encryption_info,
        } => Some((
            owner.clone(),
            *preview_start,
            *preview_length,
            encryption_info.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        got,
        Some((
            "https://example.com/crypto".to_string(),
            100u16,
            50u16,
            info
        ))
    );
}

/// `LINK` round-trip in v2.4 form (4-byte frame id) — links a TPE1
/// frame from another file.
#[test]
fn roundtrip_link_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::LinkedInfo {
            frame_id: *b"TPE1",
            url: "https://example.com/canonical.mp3".into(),
            additional: Vec::new(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::LinkedInfo {
            frame_id,
            url,
            additional,
        } => Some((*frame_id, url.clone(), additional.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some((
            *b"TPE1",
            "https://example.com/canonical.mp3".to_string(),
            Vec::<u8>::new()
        ))
    );
}

/// `LINK` round-trip in v2.3 form (3-byte frame id). Writing under
/// v2.3 emits a 3-byte id; the parser then promotes it back into the
/// 4-byte `[frame_id]` slot with the trailing byte zero-padded.
#[test]
fn roundtrip_link_v23() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::LinkedInfo {
            frame_id: [b'T', b'P', b'1', 0],
            url: "https://example.com/legacy.mp3".into(),
            additional: Vec::new(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::LinkedInfo {
            frame_id,
            url,
            additional,
        } => Some((*frame_id, url.clone(), additional.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some((
            [b'T', b'P', b'1', 0],
            "https://example.com/legacy.mp3".to_string(),
            Vec::<u8>::new()
        ))
    );
}

/// `ETCO` truncated event stream — a stray trailing byte after the
/// last 5-byte (event, ts) pair must not panic; it gets dropped.
#[test]
fn etco_truncated_trailing_byte_is_dropped() {
    // Build an ETCO frame: time_format + one valid (ev, ts) pair +
    // a single stray byte.
    let mut frame = Vec::new();
    frame.extend_from_slice(b"ETCO");
    let body = vec![
        0x02, // ms
        0x01, 0x00, 0x00, 0x00, 0x10, // ev=$01 @ t=16
        0x05, // stray
    ];
    let size = body.len() as u32;
    frame.push(((size >> 21) & 0x7F) as u8);
    frame.push(((size >> 14) & 0x7F) as u8);
    frame.push(((size >> 7) & 0x7F) as u8);
    frame.push((size & 0x7F) as u8);
    frame.extend_from_slice(&[0, 0]); // flags
    frame.extend_from_slice(&body);

    let mut tag_bytes = Vec::new();
    tag_bytes.extend_from_slice(b"ID3");
    tag_bytes.push(4);
    tag_bytes.push(0);
    tag_bytes.push(0);
    let total = frame.len() as u32;
    tag_bytes.push(((total >> 21) & 0x7F) as u8);
    tag_bytes.push(((total >> 14) & 0x7F) as u8);
    tag_bytes.push(((total >> 7) & 0x7F) as u8);
    tag_bytes.push((total & 0x7F) as u8);
    tag_bytes.extend_from_slice(&frame);

    let (parsed, _) = parse_tag(&tag_bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::EventTimingCodes {
            time_format,
            events,
        } => Some((*time_format, events.clone())),
        _ => None,
    });
    assert_eq!(got, Some((0x02u8, vec![(0x01u8, 16u32)])));
}
