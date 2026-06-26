//! parse -> write -> parse round-trip tests for ID3v2.3, ID3v2.4, and
//! ID3v1. Each test builds a tag as a Rust value, serialises it with
//! the writer, re-parses the bytes, and asserts the result matches the
//! original (modulo format-specific normalisation).

use oxideav_core::{AttachedPicture, PictureType};
use oxideav_id3::{
    attached_pictures, parse_id3v1, parse_tag, parse_tag_with_extended_header, to_key_value_pairs,
    write_id3v1, write_tag, write_tag_with_options, CommercialDelivery, ContentType,
    Equ2Interpolation, EquaBand, EtcoEventType, FileType, Id3Frame, Id3Tag, Id3Version,
    ImageEncodingRestriction, ImageSizeRestriction, KeyAccidental, MediaType, MusicalKey,
    PopmRating, Restrictions, Rva2Channel, Rva2ChannelType, RvadBackChannels, RvadChannel,
    RvadFrontChannels, SyltContentType, SytcTempo, TagSizeRestriction, TextEncodingRestriction,
    TextFieldsRestriction, TimestampUnit, UnsyncMode, WriteOptions,
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

/// The iTunes-proprietary binary-id frames `GRP1` (grouping), `MVNM`
/// (movement name) and `MVIN` (movement index) are not defined by the
/// staged id3.org specs and do not begin with `T`/`W`, so the parser
/// keeps them as `Unknown` rather than inventing typed semantics from a
/// non-spec source. This pins the contract that they nonetheless survive
/// a parse -> write -> parse round trip with their bodies byte-for-byte
/// intact: an `Unknown` frame's id and raw payload are emitted verbatim,
/// so a downstream tool that *does* know the iTunes layout can still read
/// them and our writer never silently drops them.
#[test]
fn roundtrip_preserves_itunes_binary_frames_verbatim() {
    let bodies: &[(&str, &[u8])] = &[
        ("GRP1", b"\x00Symphony No. 5\x00"),
        ("MVNM", b"\x00Allegro con brio\x00"),
        ("MVIN", b"\x001/4\x00"),
    ];
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let mut frames = vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["x".into()],
        }];
        for (id, raw) in bodies {
            frames.push(Id3Frame::Unknown {
                id: (*id).into(),
                raw: raw.to_vec(),
            });
        }
        let tag = Id3Tag { version, frames };
        let bytes = write_tag(&tag, version).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        for (id, raw) in bodies {
            let got = parsed.frames.iter().find_map(|f| match f {
                Id3Frame::Unknown { id: fid, raw } if fid == id => Some(raw.clone()),
                _ => None,
            });
            assert_eq!(
                got.as_deref(),
                Some(*raw),
                "iTunes frame {id} did not round-trip verbatim under {version:?}",
            );
        }
    }
}

/// The iTunes sort-order frames `TSO2` (album-artist sort) and `TSOC`
/// (composer sort) mirror the spec-defined `TSOA`/`TSOP`/`TSOT` sort
/// frames but are iTunes additions absent from the staged specs. Because
/// they begin with `T`, the generic text-frame path parses them as
/// ordinary `Text` frames (the encoding byte + string layout common to
/// every `T***` frame), so they survive a parse -> write -> parse round
/// trip as a typed text value rather than opaque bytes, and the flat
/// key/value projection surfaces them under their lowercased id (`tso2`
/// / `tsoc`). This is strictly more useful than the `Unknown` path and
/// requires no non-spec knowledge — only the universal text-frame
/// structure the specs define.
#[test]
fn roundtrip_itunes_sort_text_frames() {
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![
                Id3Frame::Text {
                    id: "TIT2".into(),
                    values: vec!["x".into()],
                },
                Id3Frame::Text {
                    id: "TSO2".into(),
                    values: vec!["Beethoven, Ludwig van".into()],
                },
                Id3Frame::Text {
                    id: "TSOC".into(),
                    values: vec!["Beethoven, Ludwig van".into()],
                },
            ],
        };
        let bytes = write_tag(&tag, version).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        for id in ["TSO2", "TSOC"] {
            let got = parsed.frames.iter().find_map(|f| match f {
                Id3Frame::Text { id: fid, values } if fid == id => values.first().cloned(),
                _ => None,
            });
            assert_eq!(
                got.as_deref(),
                Some("Beethoven, Ludwig van"),
                "{id} did not round-trip as text under {version:?}",
            );
        }
        // Flat projection exposes them under the lowercased id fallback.
        let kv = to_key_value_pairs(&parsed);
        assert!(kv
            .iter()
            .any(|(k, v)| k == "tso2" && v == "Beethoven, Ludwig van"));
        assert!(kv
            .iter()
            .any(|(k, v)| k == "tsoc" && v == "Beethoven, Ludwig van"));
    }
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

/// `SYLT` time-stamp-format byte preserves its logical unit when a
/// tag authored under one major version is re-serialised under the
/// other (v2.3 §4.10 vs v2.4 §4.9 define the byte identically — `$01`
/// = MPEG frames, `$02` = milliseconds). Writing v2.3 and re-parsing
/// as v2.4 (and the reverse) must surface the same
/// [`TimestampUnit`] from the typed accessor, and the raw wire byte
/// must be unchanged.
#[test]
fn sylt_timestamp_unit_roundtrips_across_v23_and_v24() {
    for &wire_format in &[1u8, 2u8] {
        for (src, dst) in [
            (Id3Version::V2_3, Id3Version::V2_4),
            (Id3Version::V2_4, Id3Version::V2_3),
        ] {
            let tag = Id3Tag {
                version: src,
                frames: vec![Id3Frame::SyncedLyrics {
                    lang: *b"eng",
                    time_format: wire_format,
                    content_type: 0x01,
                    description: "x".into(),
                    syncs: vec![("hi".into(), 12_345)],
                }],
            };
            // Write under the source version then re-parse under the
            // destination version envelope (parse_tag reads the version
            // from the wire header, not a caller-supplied hint).
            let bytes = write_tag(&tag, dst).unwrap();
            let (parsed, _) = parse_tag(&bytes).unwrap();
            let frame = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::SyncedLyrics { .. }))
                .expect("SYLT survived re-serialise");
            let raw = match frame {
                Id3Frame::SyncedLyrics { time_format, .. } => *time_format,
                _ => unreachable!(),
            };
            assert_eq!(raw, wire_format, "raw byte preserved {src:?} -> {dst:?}");
            let unit = frame.timestamp_unit().expect("known unit");
            let expected_unit = match wire_format {
                1 => TimestampUnit::MpegFrames,
                2 => TimestampUnit::Milliseconds,
                _ => unreachable!(),
            };
            assert_eq!(unit, expected_unit, "typed unit {src:?} -> {dst:?}");
            assert_eq!(unit.to_wire(), wire_format, "to_wire round-trips");
        }
    }
}

/// Reserved `time_stamp_format` wire values (anything other than `$01`
/// / `$02` per spec) surface as `None` from the typed accessor — the
/// raw byte is still preserved on the variant so a writer can round-
/// trip an exotic source, but the typed accessor does not invent a
/// unit.
#[test]
fn sylt_timestamp_unit_none_for_reserved_byte() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::SyncedLyrics {
            lang: *b"eng",
            time_format: 0x05, // reserved
            content_type: 0x01,
            description: String::new(),
            syncs: vec![],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let frame = parsed
        .frames
        .iter()
        .find(|f| matches!(f, Id3Frame::SyncedLyrics { .. }))
        .unwrap();
    assert!(frame.timestamp_unit().is_none());
    // And the typed accessor returns None on unrelated frame variants.
    let text = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["x".into()],
    };
    assert!(text.timestamp_unit().is_none());
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

/// `GRID` group-identification-registration round-trip with
/// group-dependent data. Exercises both v2.3 and v2.4 (the wire layout
/// is identical across versions).
#[test]
fn roundtrip_grid() {
    let extra = vec![0xAB, 0xCD, 0xEF, 0x12, 0x34];
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::GroupId {
                owner: "https://example.com/group".into(),
                group_symbol: 0x90,
                data: extra.clone(),
            }],
        };
        let bytes = write_tag(&tag, version).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        let got = parsed.frames.iter().find_map(|f| match f {
            Id3Frame::GroupId {
                owner,
                group_symbol,
                data,
            } => Some((owner.clone(), *group_symbol, data.clone())),
            _ => None,
        });
        assert_eq!(
            got,
            Some((
                "https://example.com/group".to_string(),
                0x90u8,
                extra.clone()
            ))
        );
    }
}

/// `GRID` with no group-dependent data — owner + symbol only, the
/// minimum legal frame. The empty-data branch must round-trip cleanly.
#[test]
fn roundtrip_grid_empty_data() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::GroupId {
            owner: "tag@example.org".into(),
            group_symbol: 0x80,
            data: Vec::new(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::GroupId {
            owner,
            group_symbol,
            data,
        } => Some((owner.clone(), *group_symbol, data.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some(("tag@example.org".to_string(), 0x80u8, Vec::<u8>::new()))
    );
}

/// `GRID` parsed straight from hand-built wire bytes — confirms the
/// exact spec layout (owner $00, group symbol $xx, group-dependent
/// data) is decoded, independent of our own writer.
#[test]
fn grid_parse_raw_bytes() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"GRID");
    let mut body = Vec::new();
    body.extend_from_slice(b"owner@x.test"); // owner identifier
    body.push(0x00); // NUL terminator
    body.push(0xF0); // group symbol (top of $80-F0 range)
    body.extend_from_slice(&[0x00, 0xFF, 0x7E]); // group-dependent data
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
        Id3Frame::GroupId {
            owner,
            group_symbol,
            data,
        } => Some((owner.clone(), *group_symbol, data.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some(("owner@x.test".to_string(), 0xF0u8, vec![0x00u8, 0xFF, 0x7E]))
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

/// `ASPI` round-trip in v2.4 — a 100-point 16-bit index covering a
/// notional VBR MP3 file. The shape is byte-aligned so the bytes
/// written by `write_tag` parse back to the same fractions.
#[test]
fn roundtrip_aspi_v24_16bit() {
    // 100 evenly-distributed fractions across the full u16 range, the
    // shape a real ASPI emitter would produce per spec §4.30.
    let fractions: Vec<u16> = (0..100u32)
        .map(|i| ((i * (u16::MAX as u32)) / 99) as u16)
        .collect();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::AudioSeekPointIndex {
            indexed_data_start: 4096,
            indexed_data_length: 12_345_678,
            bits_per_index_point: 16,
            fractions: fractions.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::AudioSeekPointIndex {
            indexed_data_start,
            indexed_data_length,
            bits_per_index_point,
            fractions,
        } => Some((
            *indexed_data_start,
            *indexed_data_length,
            *bits_per_index_point,
            fractions.clone(),
        )),
        _ => None,
    });
    assert_eq!(got, Some((4096u32, 12_345_678u32, 16u8, fractions)));
}

/// `ASPI` round-trip in v2.4 with the 8-bit precision recommended for
/// short files (under 5 minutes of audio per spec §4.30).
#[test]
fn roundtrip_aspi_v24_8bit() {
    let fractions: Vec<u16> = (0..10u16).map(|i| i * 25).collect();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::AudioSeekPointIndex {
            indexed_data_start: 0,
            indexed_data_length: 30_000,
            bits_per_index_point: 8,
            fractions: fractions.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::AudioSeekPointIndex {
            bits_per_index_point,
            fractions,
            ..
        } => Some((*bits_per_index_point, fractions.clone())),
        _ => None,
    });
    assert_eq!(got, Some((8u8, fractions)));
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

// ---------------------------------------------------------------------------
// MLLT round-trip (spec v2.3 §4.7 / v2.4 §4.6)
// ---------------------------------------------------------------------------

/// `MLLT` round-trip in v2.4 with the byte-aligned `8 + 8 = 16`-bit
/// per-reference layout that a real encoder would emit for a coarse
/// jump table. The 100 references span the full u8 range in both
/// deviation fields so a flipped MSB-first bit order would show up.
#[test]
fn roundtrip_mllt_v24_8plus8_bits() {
    let references: Vec<(u32, u32)> = (0..100u32)
        .map(|i| ((i * 251) % 256, (i * 17) % 256))
        .collect();
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference: 2,
            bytes_between_reference: 65_536,
            ms_between_reference: 1_500,
            bits_for_bytes_deviation: 8,
            bits_for_ms_deviation: 8,
            references: references.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference,
            bytes_between_reference,
            ms_between_reference,
            bits_for_bytes_deviation,
            bits_for_ms_deviation,
            references,
        } => Some((
            *mpeg_frames_between_reference,
            *bytes_between_reference,
            *ms_between_reference,
            *bits_for_bytes_deviation,
            *bits_for_ms_deviation,
            references.clone(),
        )),
        _ => None,
    });
    assert_eq!(got, Some((2u16, 65_536u32, 1_500u32, 8u8, 8u8, references)));
}

/// `MLLT` round-trip in v2.3 with the spec's example `12 + 4 = 16`-bit
/// shape — exercises sub-byte packing where neither field aligns on a
/// byte boundary. 17 references → 17 * 16 = 272 bits = 34 bytes of
/// reference area, no trailing padding needed.
#[test]
fn roundtrip_mllt_v23_12plus4_bits_subbyte_packing() {
    // bytes_dev fits in 12 bits (0..=4095), ms_dev in 4 bits (0..=15).
    let references: Vec<(u32, u32)> = vec![
        (0, 0),
        (1, 1),
        (0xFFF, 0xF),
        (0x800, 0x8),
        (0x123, 0x4),
        (0xABC, 0xD),
        (0x7FF, 0x7),
        (0xFFE, 0xE),
        (0x000, 0xF),
        (0xFFF, 0x0),
        (0x555, 0xA),
        (0xAAA, 0x5),
        (0x100, 0x2),
        (0x200, 0x4),
        (0x300, 0x6),
        (0x400, 0x8),
        (0x500, 0xA),
    ];
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference: 1,
            bytes_between_reference: 0x00FF_FFFF,
            ms_between_reference: 0x00FF_FFFF,
            bits_for_bytes_deviation: 12,
            bits_for_ms_deviation: 4,
            references: references.clone(),
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference,
            bytes_between_reference,
            ms_between_reference,
            bits_for_bytes_deviation,
            bits_for_ms_deviation,
            references,
        } => Some((
            *mpeg_frames_between_reference,
            *bytes_between_reference,
            *ms_between_reference,
            *bits_for_bytes_deviation,
            *bits_for_ms_deviation,
            references.clone(),
        )),
        _ => None,
    });
    assert_eq!(
        got,
        Some((1u16, 0x00FF_FFFFu32, 0x00FF_FFFFu32, 12u8, 4u8, references))
    );
}

/// `MLLT` writer must refuse a per-reference width sum that's not a
/// multiple of four (spec §4.7 / §4.6). A reader can't reliably align
/// a non-conforming stream.
#[test]
fn mllt_writer_rejects_non_multiple_of_four_total_bits() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference: 2,
            bytes_between_reference: 1000,
            ms_between_reference: 1000,
            // 7 + 8 = 15 bits per reference, not a multiple of 4.
            bits_for_bytes_deviation: 7,
            bits_for_ms_deviation: 8,
            references: vec![(1, 1)],
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("multiple of 4"),
        "expected multiple-of-4 rejection, got: {msg}"
    );
}

/// `MLLT` writer must refuse a 24-bit-field value that doesn't fit.
#[test]
fn mllt_writer_rejects_24bit_overflow() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference: 1,
            bytes_between_reference: 0x0100_0000, // 1 over the 24-bit cap
            ms_between_reference: 0,
            bits_for_bytes_deviation: 8,
            bits_for_ms_deviation: 8,
            references: vec![],
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("24-bit"),
        "expected 24-bit overflow, got: {msg}"
    );
}

/// `MLLT` writer must refuse a per-reference deviation value that's
/// wider than the declared per-reference width.
#[test]
fn mllt_writer_rejects_reference_over_width() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::MpegLocationLookup {
            mpeg_frames_between_reference: 1,
            bytes_between_reference: 0,
            ms_between_reference: 0,
            bits_for_bytes_deviation: 4,
            bits_for_ms_deviation: 4,
            // Declared as 4 bits (max 0xF) but value is 0x10.
            references: vec![(0x10, 0)],
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("byte deviation"),
        "expected byte-deviation overflow, got: {msg}"
    );
}

/// `MLLT` parser: a payload shorter than the 10-byte descriptor is
/// preserved as `Unknown` rather than parsed into a half-built MLLT.
#[test]
fn mllt_parser_short_descriptor_is_unknown() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"MLLT");
    let body = vec![0x00u8; 9]; // 1 byte short of the 10-byte descriptor
    let size = body.len() as u32;
    frame.push(((size >> 21) & 0x7F) as u8);
    frame.push(((size >> 14) & 0x7F) as u8);
    frame.push(((size >> 7) & 0x7F) as u8);
    frame.push((size & 0x7F) as u8);
    frame.extend_from_slice(&[0, 0]);
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
    let mut saw_unknown = false;
    for f in &parsed.frames {
        if let Id3Frame::Unknown { id, raw } = f {
            assert_eq!(id, "MLLT");
            assert_eq!(raw, &body);
            saw_unknown = true;
        }
    }
    assert!(saw_unknown, "MLLT short descriptor should land in Unknown");
}

/// `MLLT` parser: a non-conforming total bit width (>32 in either
/// field) leaves references empty rather than truncating into u32.
#[test]
fn mllt_parser_rejects_excessive_bit_width() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"MLLT");
    // Descriptor + four bytes of reference area; widths set to 33 and
    // 7 — sum is 40 (multiple of 4) but byte-dev width exceeds u32.
    let mut body = vec![
        0x00, 0x02, // mpeg_frames_between_reference = 2
        0x00, 0x00, 0x10, // bytes = 0x10
        0x00, 0x00, 0x20, // ms    = 0x20
        33,   // bits_for_bytes_deviation (over u32 cap)
        7,    // bits_for_ms_deviation
    ];
    body.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    let size = body.len() as u32;
    frame.push(((size >> 21) & 0x7F) as u8);
    frame.push(((size >> 14) & 0x7F) as u8);
    frame.push(((size >> 7) & 0x7F) as u8);
    frame.push((size & 0x7F) as u8);
    frame.extend_from_slice(&[0, 0]);
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
        Id3Frame::MpegLocationLookup {
            bits_for_bytes_deviation,
            bits_for_ms_deviation,
            references,
            ..
        } => Some((
            *bits_for_bytes_deviation,
            *bits_for_ms_deviation,
            references.clone(),
        )),
        _ => None,
    });
    // Descriptor is captured, references are empty (we refuse to
    // interpret a >32-bit width).
    assert_eq!(got, Some((33u8, 7u8, vec![])));
}

// ---------------------------------------------------------------------------
// Extended-header CRC round-trip (spec §3.2, v2.3 + v2.4)
// ---------------------------------------------------------------------------

fn small_text_tag(version: Id3Version) -> Id3Tag {
    Id3Tag {
        version,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["CRC Test".into()],
            },
            Id3Frame::Text {
                id: "TPE1".into(),
                values: vec!["The CRC".into()],
            },
        ],
    }
}

/// v2.3 extended-header CRC: emitting `WriteOptions::with_crc(true)`
/// must set the header's bit 6, lay down a 10-byte ext header with
/// flags `0x80 00`, padding size = 0, and a regular-u32 CRC; the
/// parser must verify the CRC against the frame area and round-trip
/// the frame contents losslessly.
#[test]
fn roundtrip_extended_header_crc_v23() {
    let tag = small_text_tag(Id3Version::V2_3);
    let opts = WriteOptions::new().with_crc(true);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap();

    // Header bit 6 (extended header) must be set, bit 7 (unsync) must
    // not (we did not request unsync).
    assert_eq!(bytes[5] & 0x40, 0x40);
    assert_eq!(bytes[5] & 0x80, 0x00);

    // Extended header layout: size (4 bytes, regular, excludes itself) = 10
    let ext_size = u32::from_be_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
    assert_eq!(ext_size, 10);
    // Flags: bit 15 of the 2-byte field = CRC present.
    assert_eq!(bytes[14] & 0x80, 0x80);
    assert_eq!(bytes[15], 0x00);
    // Size of padding = 0 (writer emits no padding).
    let padding_size = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    assert_eq!(padding_size, 0);

    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.version, Id3Version::V2_3);
    assert_eq!(parsed.frames.len(), 2);
    let titles: Vec<String> = parsed
        .frames
        .iter()
        .filter_map(|f| match f {
            Id3Frame::Text { id, values } if id == "TIT2" => values.first().cloned(),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["CRC Test".to_string()]);
}

/// v2.4 extended-header CRC: must set bit 6, lay down a 12-byte ext
/// header with synchsafe size = 12, flag-count = 1, flags = 0x20,
/// data-length = 5, then the 5-byte synchsafe CRC.
#[test]
fn roundtrip_extended_header_crc_v24() {
    let tag = small_text_tag(Id3Version::V2_4);
    let opts = WriteOptions::new().with_crc(true);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();

    assert_eq!(bytes[5] & 0x40, 0x40);
    assert_eq!(bytes[5] & 0x80, 0x00);

    // ext_size synchsafe: 4 bytes, includes itself.
    let ext_size = ((bytes[10] as u32 & 0x7F) << 21)
        | ((bytes[11] as u32 & 0x7F) << 14)
        | ((bytes[12] as u32 & 0x7F) << 7)
        | (bytes[13] as u32 & 0x7F);
    assert_eq!(ext_size, 12);
    assert_eq!(bytes[14], 0x01); // number of flag bytes
    assert_eq!(bytes[15], 0x20); // flags: c = CRC present
    assert_eq!(bytes[16], 0x05); // CRC attached-data length

    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.version, Id3Version::V2_4);
    assert_eq!(parsed.frames.len(), 2);
}

/// Composing CRC + whole-tag unsync must still round-trip: the writer
/// inserts the extended header BEFORE running unsync over the
/// `(ext_header || frames)` concatenation, and the parser reverses
/// unsync first so the CRC verifies against the pre-unsync bytes
/// (matching v2.3 §3.2's "calculated before unsynchronisation").
#[test]
fn roundtrip_extended_header_crc_with_unsync_v23() {
    // Use a frame value that contains an $FF byte so unsync actually
    // mutates the body.
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Private {
            owner: "test@oxideav.io".into(),
            data: vec![0xFF, 0xE0, 0xFF, 0x00, 0xFF],
        }],
    };
    let opts = WriteOptions::new()
        .with_crc(true)
        .with_unsync(UnsyncMode::WholeTag);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap();
    // Both flags set.
    assert_eq!(bytes[5] & 0xC0, 0xC0);

    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Private { owner, data } => Some((owner.clone(), data.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some(("test@oxideav.io".into(), vec![0xFF, 0xE0, 0xFF, 0x00, 0xFF]))
    );
}

#[test]
fn roundtrip_extended_header_crc_with_unsync_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Private {
            owner: "v24@oxideav.io".into(),
            data: vec![0xFF, 0xFA, 0xFF, 0x00, 0x10],
        }],
    };
    let opts = WriteOptions::new()
        .with_crc(true)
        .with_unsync(UnsyncMode::WholeTag);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Private { owner, data } => Some((owner.clone(), data.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some(("v24@oxideav.io".into(), vec![0xFF, 0xFA, 0xFF, 0x00, 0x10]))
    );
}

/// A corrupted CRC must fail the parse — silent acceptance would defeat
/// the purpose of having a CRC.
#[test]
fn parse_rejects_bad_crc_v23() {
    let tag = small_text_tag(Id3Version::V2_3);
    let opts = WriteOptions::new().with_crc(true);
    let mut bytes = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap();
    // Flip one bit of the stored CRC (last 4 bytes of the 14-byte ext
    // header area: bytes[20..24]).
    bytes[20] ^= 0xFF;
    assert!(parse_tag(&bytes).is_err());
}

#[test]
fn parse_rejects_bad_crc_v24() {
    let tag = small_text_tag(Id3Version::V2_4);
    let opts = WriteOptions::new().with_crc(true);
    let mut bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    // Flip a byte of the 5-byte synchsafe CRC at bytes[17..22]. Keep
    // the synchsafe high bit (0x80) clear or the resulting decoded
    // value just changes — either way the CRC won't match.
    bytes[17] ^= 0x01;
    assert!(parse_tag(&bytes).is_err());
}

/// Round-tripping a tag whose body contains a frame whose payload
/// itself contains the false-sync pattern: with both CRC and per-frame
/// unsync, the frame payload is unsynchronised but the CRC covers the
/// pre-unsync frame headers + bodies, so the parser must reverse
/// per-frame unsync as it walks frames AND verify the (un-touched, in
/// this mode) extended header CRC against the original frame-area
/// bytes. The writer puts the extended header in front of the original
/// frame stream and applies only per-frame unsync on the individual
/// frame payloads, not the ext header itself.
#[test]
fn roundtrip_extended_header_crc_with_perframe_unsync_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Private {
            owner: "perframe".into(),
            data: vec![0xFF, 0xE0, 0x00, 0xFF, 0xF8],
        }],
    };
    let opts = WriteOptions::new()
        .with_crc(true)
        .with_unsync(UnsyncMode::PerFrame);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let got = parsed.frames.iter().find_map(|f| match f {
        Id3Frame::Private { owner, data } => Some((owner.clone(), data.clone())),
        _ => None,
    });
    assert_eq!(
        got,
        Some(("perframe".into(), vec![0xFF, 0xE0, 0x00, 0xFF, 0xF8]))
    );
}

/// Default `WriteOptions` (no CRC) must not emit an extended header
/// and the header's bit 6 must stay clear — the historical
/// `write_tag` shorthand must be byte-identical to
/// `write_tag_with_options(..., WriteOptions::default())`.
#[test]
fn default_options_emit_no_extended_header() {
    let tag = small_text_tag(Id3Version::V2_4);
    let a = write_tag(&tag, Id3Version::V2_4).unwrap();
    let b = write_tag_with_options(&tag, Id3Version::V2_4, &WriteOptions::default()).unwrap();
    assert_eq!(a, b);
    assert_eq!(a[5] & 0x40, 0); // no extended-header bit
}

/// ID3v2.4 footer end-to-end: write_tag_with_options(footer=true)
/// produces a tag whose final 10 bytes are "3DI..." mirroring the
/// header's flags+size, and parse_tag round-trips the frames AND
/// reports a `consumed` byte count that includes the footer (so a
/// caller seeking to the next audio byte advances correctly).
#[test]
fn roundtrip_footer_v24_text() {
    let tag = small_text_tag(Id3Version::V2_4);
    let opts = WriteOptions::new().with_footer(true);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    assert_eq!(bytes[5] & 0x10, 0x10, "header footer-flag must be set");
    let (parsed, consumed) = parse_tag(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    // The synchsafe size in the header excludes the footer; consumed
    // therefore equals (header_size 10) + (announced body size) + 10
    // (footer). Pin that down to make the convention regression-proof.
    let size = ((bytes[6] as u32 & 0x7F) << 21)
        | ((bytes[7] as u32 & 0x7F) << 14)
        | ((bytes[8] as u32 & 0x7F) << 7)
        | (bytes[9] as u32 & 0x7F);
    assert_eq!(consumed, 10 + size as usize + 10);
    // Frame survived intact.
    assert_eq!(parsed.frames.len(), tag.frames.len());
}

/// Footer composes with whole-tag unsync: header bits 0x80 and 0x10
/// are both set, the footer lives *after* the unsynced body, and the
/// round-trip recovers the original frame payload byte-for-byte even
/// when the payload contains a false-sync trigger (`0xFF 0xE0`).
#[test]
fn roundtrip_footer_v24_with_unsync() {
    let payload = vec![0xFF, 0xE0, 0xAB, 0xFF, 0x00, 0xCD];
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Private {
            owner: "owner@example.com".into(),
            data: payload.clone(),
        }],
    };
    let opts = WriteOptions::new()
        .with_unsync(UnsyncMode::WholeTag)
        .with_footer(true);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    assert_eq!(bytes[5] & 0x80, 0x80);
    assert_eq!(bytes[5] & 0x10, 0x10);
    assert_eq!(&bytes[bytes.len() - 10..bytes.len() - 7], b"3DI");
    let (parsed, consumed) = parse_tag(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    match &parsed.frames[0] {
        Id3Frame::Private { data, .. } => assert_eq!(data, &payload),
        other => panic!("expected Private frame, got {other:?}"),
    }
}

/// Footer + extended-header CRC compose cleanly: both flag bits set,
/// the writer emits header → ext-header → frames → footer in order,
/// the parser verifies the CRC over the frames region and validates
/// the footer separately, and the original frames round-trip.
#[test]
fn roundtrip_footer_v24_with_crc() {
    let tag = small_text_tag(Id3Version::V2_4);
    let opts = WriteOptions::new().with_crc(true).with_footer(true);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &opts).unwrap();
    assert_eq!(bytes[5] & 0x40, 0x40);
    assert_eq!(bytes[5] & 0x10, 0x10);
    let (parsed, consumed) = parse_tag(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(parsed.frames.len(), tag.frames.len());
}

/// Requesting a footer on a v2.3 target is rejected at write time —
/// the spec defines the footer only for v2.4. The error message
/// mentions "v2.4" so consumers can disambiguate from the v2.2
/// "not supported" message.
#[test]
fn write_footer_on_v23_errors() {
    let tag = small_text_tag(Id3Version::V2_3);
    let opts = WriteOptions::new().with_footer(true);
    let err = write_tag_with_options(&tag, Id3Version::V2_3, &opts).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("v2.4"), "unexpected error: {msg}");
}

/// `RVRB` reverb (spec v2.3 §4.13 / v2.4 §4.13) round-trip through
/// `write_tag` → `parse_tag`. The on-wire layout is byte-aligned and
/// version-independent, so the same `Reverb` value survives both
/// envelopes byte-for-byte.
#[test]
fn roundtrip_rvrb_v23_and_v24() {
    let original = Id3Frame::Reverb {
        reverb_left_ms: 0x0064,
        reverb_right_ms: 0x00C8,
        bounces_left: 0x04,
        bounces_right: 0x04,
        feedback_ll: 0x7F,
        feedback_lr: 0x10,
        feedback_rr: 0x7F,
        feedback_rl: 0x10,
        premix_lr: 0x20,
        premix_rl: 0x20,
    };
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![original.clone()],
        };
        let bytes = write_tag(&tag, version).unwrap();
        let (parsed, _) = parse_tag(&bytes).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        match &parsed.frames[0] {
            Id3Frame::Reverb {
                reverb_left_ms,
                reverb_right_ms,
                bounces_left,
                bounces_right,
                feedback_ll,
                feedback_lr,
                feedback_rr,
                feedback_rl,
                premix_lr,
                premix_rl,
            } => {
                assert_eq!(*reverb_left_ms, 0x0064);
                assert_eq!(*reverb_right_ms, 0x00C8);
                assert_eq!(*bounces_left, 0x04);
                assert_eq!(*bounces_right, 0x04);
                assert_eq!(*feedback_ll, 0x7F);
                assert_eq!(*feedback_lr, 0x10);
                assert_eq!(*feedback_rr, 0x7F);
                assert_eq!(*feedback_rl, 0x10);
                assert_eq!(*premix_lr, 0x20);
                assert_eq!(*premix_rl, 0x20);
            }
            other => panic!("expected Reverb, got {other:?}"),
        }
    }
}

/// The spec edge values for `RVRB` byte fields — `$FF` bounces means
/// infinite, `$FF` feedback means 100% return, `$FF` premix means
/// fully cross-mixed — must round-trip exactly. The 16-bit ms fields
/// are also exercised at their u16 extreme (`0xFFFF`).
#[test]
fn roundtrip_rvrb_extreme_values() {
    let original = Id3Frame::Reverb {
        reverb_left_ms: 0xFFFF,
        reverb_right_ms: 0x0000,
        bounces_left: 0xFF,
        bounces_right: 0x00,
        feedback_ll: 0xFF,
        feedback_lr: 0x00,
        feedback_rr: 0xFF,
        feedback_rl: 0x00,
        premix_lr: 0xFF,
        premix_rl: 0xFF,
    };
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![original.clone()],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    match &parsed.frames[0] {
        Id3Frame::Reverb {
            reverb_left_ms,
            bounces_left,
            feedback_ll,
            premix_lr,
            premix_rl,
            ..
        } => {
            assert_eq!(*reverb_left_ms, 0xFFFF);
            assert_eq!(*bounces_left, 0xFF);
            assert_eq!(*feedback_ll, 0xFF);
            assert_eq!(*premix_lr, 0xFF);
            assert_eq!(*premix_rl, 0xFF);
        }
        other => panic!("expected Reverb, got {other:?}"),
    }
}

/// `RVAD` (spec v2.3 §4.12) round-trips through `write_tag` /
/// `parse_tag` under v2.3 with front + back + centre + bass blocks
/// all populated. Sub-byte `bits_used = 12` exercises the zero-pad
/// width handling.
#[test]
fn roundtrip_rvad_v23_all_channels_12bit() {
    let original = Id3Frame::Rvad {
        increment_decrement: 0b0011_1111, // all six channel bits set
        bits_used: 12,                    // 2 bytes per field (sub-byte width)
        front: Some(RvadFrontChannels {
            right: RvadChannel {
                volume_delta: vec![0x01, 0x23],
                peak: vec![0x04, 0x56],
            },
            left: RvadChannel {
                volume_delta: vec![0x07, 0x89],
                peak: vec![0x0A, 0xBC],
            },
        }),
        back: Some(RvadBackChannels {
            right_back: RvadChannel {
                volume_delta: vec![0x00, 0x11],
                peak: vec![0x00, 0x22],
            },
            left_back: RvadChannel {
                volume_delta: vec![0x00, 0x33],
                peak: vec![0x00, 0x44],
            },
        }),
        center: Some(RvadChannel {
            volume_delta: vec![0x0F, 0xFF],
            peak: vec![0x0F, 0xFE],
        }),
        bass: Some(RvadChannel {
            volume_delta: vec![0x00, 0x01],
            peak: vec![0x00, 0x02],
        }),
    };
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![original],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    match &parsed.frames[0] {
        Id3Frame::Rvad {
            increment_decrement,
            bits_used,
            front,
            back,
            center,
            bass,
        } => {
            assert_eq!(*increment_decrement, 0b0011_1111);
            assert_eq!(*bits_used, 12);
            let f = front.as_ref().expect("front");
            assert_eq!(f.right.volume_delta, vec![0x01, 0x23]);
            assert_eq!(f.right.peak, vec![0x04, 0x56]);
            assert_eq!(f.left.volume_delta, vec![0x07, 0x89]);
            assert_eq!(f.left.peak, vec![0x0A, 0xBC]);
            let b = back.as_ref().expect("back");
            assert_eq!(b.right_back.volume_delta, vec![0x00, 0x11]);
            assert_eq!(b.right_back.peak, vec![0x00, 0x22]);
            assert_eq!(b.left_back.volume_delta, vec![0x00, 0x33]);
            assert_eq!(b.left_back.peak, vec![0x00, 0x44]);
            let c = center.as_ref().expect("centre");
            assert_eq!(c.volume_delta, vec![0x0F, 0xFF]);
            assert_eq!(c.peak, vec![0x0F, 0xFE]);
            let ba = bass.as_ref().expect("bass");
            assert_eq!(ba.volume_delta, vec![0x00, 0x01]);
            assert_eq!(ba.peak, vec![0x00, 0x02]);
        }
        other => panic!("expected Rvad after round-trip, got {other:?}"),
    }
}

/// `RVAD` is v2.3-only. Emitting it under a `V2_4` envelope must
/// fail rather than producing a frame v2.4 readers would not parse.
#[test]
fn roundtrip_rvad_writer_rejects_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Rvad {
            increment_decrement: 0b0000_0011,
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
                left: RvadChannel {
                    volume_delta: vec![0x00, 0x40],
                    peak: vec![0x00, 0x80],
                },
            }),
            back: None,
            center: None,
            bass: None,
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("v2.3"));
}

/// `EQUA` multi-band round-trip at the spec-norm 16-bit adjustment
/// width. Exercises the writer's frequency ordering + the 15-bit
/// frequency boundary + multiple inc/dec sign combinations through the
/// public `write_tag` surface.
#[test]
fn roundtrip_equa_v23_multi_band_16bit() {
    let original = Id3Frame::Equa {
        adjustment_bits: 16,
        bands: vec![
            EquaBand {
                increment: true,
                frequency: 50,
                adjustment: vec![0x00, 0x80],
            },
            EquaBand {
                increment: false,
                frequency: 250,
                adjustment: vec![0x01, 0x00],
            },
            EquaBand {
                increment: true,
                frequency: 4_000,
                adjustment: vec![0x02, 0x00],
            },
            EquaBand {
                increment: false,
                frequency: 16_000,
                adjustment: vec![0x00, 0x40],
            },
            EquaBand {
                increment: true,
                frequency: 32_767,
                adjustment: vec![0xFF, 0xFF],
            },
        ],
    };
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![original.clone()],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    match &parsed.frames[0] {
        Id3Frame::Equa {
            adjustment_bits,
            bands,
        } => {
            assert_eq!(*adjustment_bits, 16);
            assert_eq!(bands.len(), 5);
            assert!(bands[0].increment);
            assert_eq!(bands[0].frequency, 50);
            assert!(!bands[1].increment);
            assert_eq!(bands[1].frequency, 250);
            assert_eq!(bands[2].frequency, 4_000);
            assert_eq!(bands[3].frequency, 16_000);
            assert_eq!(bands[4].frequency, 32_767);
            assert_eq!(bands[4].adjustment, vec![0xFF, 0xFF]);
        }
        other => panic!("expected Equa after round-trip, got {other:?}"),
    }
}

/// `EQUA` is v2.3-only. Emitting it under a `V2_4` envelope must fail
/// rather than producing a frame v2.4 readers would not understand
/// (v2.4 dropped `EQUA` in favour of `EQU2`).
#[test]
fn roundtrip_equa_writer_rejects_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Equa {
            adjustment_bits: 16,
            bands: vec![EquaBand {
                increment: true,
                frequency: 100,
                adjustment: vec![0x00, 0x80],
            }],
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("v2.3"));
}

/// `IPLS` round-trip through the public `write_tag` + `parse_tag`
/// surface for a multi-pair tag. Pairs survive the writer + parser
/// trip with their role/name strings intact, the v2.3 default
/// encoding (UTF-16 with BOM) carries arbitrary Unicode roles and
/// names safely, and the pair ordering is preserved.
#[test]
fn roundtrip_ipls_v23_multi_pair_unicode() {
    let original = Id3Frame::Ipls {
        pairs: vec![
            ("producer".to_string(), "Alice Bloggs".to_string()),
            ("guitar".to_string(), "Bob Smith".to_string()),
            ("ヴォーカル".to_string(), "山田 太郎".to_string()),
            ("mixing engineer".to_string(), "Carol Jones".to_string()),
            ("mastering".to_string(), "David Müller".to_string()),
        ],
    };
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![original.clone()],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    match &parsed.frames[0] {
        Id3Frame::Ipls { pairs } => {
            assert_eq!(pairs.len(), 5);
            assert_eq!(pairs[0].0, "producer");
            assert_eq!(pairs[0].1, "Alice Bloggs");
            assert_eq!(pairs[1].0, "guitar");
            assert_eq!(pairs[1].1, "Bob Smith");
            assert_eq!(pairs[2].0, "ヴォーカル");
            assert_eq!(pairs[2].1, "山田 太郎");
            assert_eq!(pairs[3].0, "mixing engineer");
            assert_eq!(pairs[3].1, "Carol Jones");
            assert_eq!(pairs[4].0, "mastering");
            assert_eq!(pairs[4].1, "David Müller");
        }
        other => panic!("expected Ipls after round-trip, got {other:?}"),
    }
}

/// `IPLS` is v2.3-only. Emitting it under a `V2_4` envelope must fail
/// (v2.4 replaced it with the `TIPL` text frame; the writer surfaces
/// the rejection at the `write_tag` boundary rather than silently
/// emitting an unrecognised id).
#[test]
fn roundtrip_ipls_writer_rejects_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Ipls {
            pairs: vec![("producer".to_string(), "Alice".to_string())],
        }],
    };
    let err = write_tag(&tag, Id3Version::V2_4).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("v2.3"));
}

// ---------------------------------------------------------------------------
// v2.3 / v2.4 extended-header sub-fields: `is_update` + restrictions byte
// ---------------------------------------------------------------------------

/// `parse_tag_with_extended_header` returns the default `ExtendedHeader`
/// when no extended header is present (the tag-header flag bit 0x40 is
/// clear). All sub-fields are `false` / `None`.
#[test]
fn ext_header_default_when_no_ext_header() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Plain".into()],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (_parsed, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert!(!ext.is_update);
    assert_eq!(ext.crc, None);
    assert_eq!(ext.restrictions, None);
}

/// `parse_tag` still works after the extended-header refactor.
/// `parse_tag_with_extended_header` and `parse_tag` must agree on the
/// frames they recover from the same bytes.
#[test]
fn ext_header_parse_tag_still_agrees() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Agree".into()],
        }],
    };
    let bytes = write_tag_with_options(
        &tag,
        Id3Version::V2_4,
        &WriteOptions::new().with_crc(true).with_update(true),
    )
    .unwrap();
    let (a, _) = parse_tag(&bytes).unwrap();
    let (b, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert_eq!(a.version, b.version);
    assert_eq!(a.frames.len(), b.frames.len());
    assert!(ext.is_update);
    assert!(ext.crc.is_some());
}

/// Round-trip the v2.4 "Tag is an update" flag: written via
/// [`WriteOptions::with_update`] and recovered via
/// [`parse_tag_with_extended_header`].
#[test]
fn ext_header_is_update_v24_roundtrip() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TPE1".into(),
            values: vec!["Updater".into()],
        }],
    };
    let bytes = write_tag_with_options(
        &tag,
        Id3Version::V2_4,
        &WriteOptions::new().with_update(true),
    )
    .unwrap();
    // The tag-header flag bit 0x40 must be set since we emitted an
    // extended header (the parser-side gate keys off this bit).
    assert_eq!(bytes[5] & 0x40, 0x40);
    let (_, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert!(ext.is_update);
    assert_eq!(ext.crc, None);
    assert_eq!(ext.restrictions, None);
}

/// `is_update` is a v2.4-only extended-header sub-field; v2.3 has no
/// slot for it. Requesting it under a v2.3 target must fail loudly
/// rather than silently dropping the flag.
#[test]
fn ext_header_is_update_v23_rejected() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Nope".into()],
        }],
    };
    let err = write_tag_with_options(
        &tag,
        Id3Version::V2_3,
        &WriteOptions::new().with_update(true),
    )
    .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("v2.4-only"));
}

/// Round-trip every value of every restrictions sub-field. Writing a
/// fully-saturated restrictions byte (`%11111111`) and re-parsing must
/// recover the exact same typed sub-fields. The wire byte uses every
/// reserved bit position so any sub-field decode bug surfaces here.
#[test]
fn ext_header_restrictions_saturated_v24_roundtrip() {
    let restrictions = Restrictions {
        tag_size: TagSizeRestriction::Max32Frames4Kb,
        text_encoding: TextEncodingRestriction::Iso8859OrUtf8,
        text_fields: TextFieldsRestriction::Max30Chars,
        image_encoding: ImageEncodingRestriction::PngOrJpeg,
        image_size: ImageSizeRestriction::Exactly64x64,
    };
    assert_eq!(restrictions.to_wire(), 0xFF);
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Restricted".into()],
        }],
    };
    let bytes = write_tag_with_options(
        &tag,
        Id3Version::V2_4,
        &WriteOptions::new().with_restrictions(Some(restrictions)),
    )
    .unwrap();
    let (_, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert_eq!(ext.restrictions, Some(restrictions));
}

/// The default restrictions byte is `0x00` — all sub-fields at their
/// "no restriction" / "unrestricted" zero values. Round-trip the
/// zero byte explicitly so a regression in the bit layout surfaces.
#[test]
fn ext_header_restrictions_zero_byte_roundtrips() {
    let restrictions = Restrictions::default();
    assert_eq!(restrictions.to_wire(), 0x00);
    let recovered = Restrictions::from_wire(0x00);
    assert_eq!(recovered, restrictions);
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Zero".into()],
        }],
    };
    let bytes = write_tag_with_options(
        &tag,
        Id3Version::V2_4,
        &WriteOptions::new().with_restrictions(Some(restrictions)),
    )
    .unwrap();
    let (_, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert_eq!(ext.restrictions, Some(Restrictions::default()));
}

/// `Restrictions::from_wire` must individually decode each
/// non-default sub-field. Walk through one set bit at a time and
/// confirm the right sub-field decodes to the right enum variant
/// while the others stay at their default.
#[test]
fn ext_header_restrictions_per_subfield_isolation() {
    // p (tag size, bits 7..=6) = %01
    let r = Restrictions::from_wire(0b0100_0000);
    assert_eq!(r.tag_size, TagSizeRestriction::Max64Frames128Kb);
    assert_eq!(r.text_encoding, TextEncodingRestriction::default());
    assert_eq!(r.text_fields, TextFieldsRestriction::default());
    assert_eq!(r.image_encoding, ImageEncodingRestriction::default());
    assert_eq!(r.image_size, ImageSizeRestriction::default());

    // q (text encoding, bit 5)
    let r = Restrictions::from_wire(0b0010_0000);
    assert_eq!(r.tag_size, TagSizeRestriction::default());
    assert_eq!(r.text_encoding, TextEncodingRestriction::Iso8859OrUtf8);

    // r (text fields, bits 4..=3) = %10
    let r = Restrictions::from_wire(0b0001_0000);
    assert_eq!(r.text_fields, TextFieldsRestriction::Max128Chars);

    // s (image encoding, bit 2)
    let r = Restrictions::from_wire(0b0000_0100);
    assert_eq!(r.image_encoding, ImageEncodingRestriction::PngOrJpeg);

    // t (image size, bits 1..=0) = %01
    let r = Restrictions::from_wire(0b0000_0001);
    assert_eq!(r.image_size, ImageSizeRestriction::Max256x256);
}

/// `Restrictions::to_wire` is the exact inverse of
/// `Restrictions::from_wire`. Verify the round-trip across all 256
/// possible bytes — the typed sub-fields cover every bit position
/// without overlap or gap.
#[test]
fn ext_header_restrictions_byte_bijection() {
    for b in 0u8..=255 {
        let r = Restrictions::from_wire(b);
        assert_eq!(r.to_wire(), b, "byte {b:#04x} did not round-trip");
    }
}

/// Restrictions is v2.4-only. v2.3 must reject the option loudly
/// rather than silently dropping it, matching the `is_update` and
/// `with_footer` rejection pattern.
#[test]
fn ext_header_restrictions_v23_rejected() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["Nope".into()],
        }],
    };
    let err = write_tag_with_options(
        &tag,
        Id3Version::V2_3,
        &WriteOptions::new().with_restrictions(Some(Restrictions::default())),
    )
    .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("v2.4-only"));
}

/// Compose every v2.4 extended-header sub-field at once: CRC,
/// is_update, restrictions, footer, and per-frame unsync. The
/// resulting tag must round-trip both the frames and every
/// extended-header sub-field.
#[test]
fn ext_header_all_subfields_compose_v24() {
    let restrictions = Restrictions {
        tag_size: TagSizeRestriction::Max32Frames40Kb,
        text_encoding: TextEncodingRestriction::Iso8859OrUtf8,
        text_fields: TextFieldsRestriction::Max1024Chars,
        image_encoding: ImageEncodingRestriction::PngOrJpeg,
        image_size: ImageSizeRestriction::Max256x256,
    };
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Composed".into()],
            },
            Id3Frame::Text {
                id: "TPE1".into(),
                values: vec!["All Flags".into()],
            },
        ],
    };
    let bytes = write_tag_with_options(
        &tag,
        Id3Version::V2_4,
        &WriteOptions::new()
            .with_crc(true)
            .with_update(true)
            .with_restrictions(Some(restrictions))
            .with_footer(true)
            .with_unsync(UnsyncMode::PerFrame),
    )
    .unwrap();
    let (parsed, ext, _) = parse_tag_with_extended_header(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 2);
    assert!(ext.is_update);
    assert!(ext.crc.is_some());
    assert_eq!(ext.restrictions, Some(restrictions));
    // Footer present + identifier
    assert_eq!(&bytes[bytes.len() - 10..bytes.len() - 7], b"3DI");
}

/// The crc-only extended header (no `is_update`, no restrictions)
/// remains unchanged from the previous wire layout: 12-byte
/// extended-header for v2.4 (4-byte size = 12, 1-byte flag-count,
/// 1-byte flags = 0x20, 1-byte CRC data-length = 5, 5-byte
/// synchsafe CRC). Regression guard so the new options don't
/// accidentally bloat the crc-only emission.
#[test]
fn ext_header_crc_only_v24_size_unchanged() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![],
    };
    let bytes = write_tag_with_options(&tag, Id3Version::V2_4, &WriteOptions::new().with_crc(true))
        .unwrap();
    // tag header (10) + ext header (12) = 22 bytes total when
    // there are no frames.
    assert_eq!(bytes.len(), 22);
    // ext flags = 0x20 (CRC only)
    assert_eq!(bytes[10 + 5], 0x20);
}

/// Regression guard for the v2.4 extended-header CRC synchsafe-encoding
/// of values whose bit 31 is set. The synchsafe encoder used to mask
/// the top synchsafe byte with `0x07` instead of `0x0F`, which
/// silently truncated bit 31 of the CRC; the parser would then
/// compute a CRC with bit 31 set and reject the round-trip with a
/// "CRC mismatch" error. This test asserts the writer and the parser
/// agree even when the frame-bytes' CRC happens to have bit 31 set.
#[test]
fn ext_header_crc_top_bit_survives_synchsafe_encoding() {
    // Build tags one frame at a time until we land on a body whose
    // CRC has bit 31 set. The CRC depends on the frame bytes, so a
    // distinct title string is enough to flip across many values
    // quickly. Bound the search at 256 attempts so a regression in
    // crc32_iso3309 cannot hang the test.
    let mut found = false;
    for n in 0u32..256 {
        let title = format!("ext-header-crc-top-bit-{n:08x}");
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Text {
                id: "TIT2".into(),
                values: vec![title.clone()],
            }],
        };
        let bytes =
            write_tag_with_options(&tag, Id3Version::V2_4, &WriteOptions::new().with_crc(true))
                .unwrap();
        let parsed = parse_tag_with_extended_header(&bytes).unwrap();
        let crc = parsed.1.crc.expect("CRC must be present after write");
        if crc & 0x8000_0000 != 0 {
            // CRC top bit was set on the wire; the synchsafe encoder
            // must have preserved it or the round-trip above would
            // have failed with a CRC mismatch.
            found = true;
            break;
        }
    }
    assert!(
        found,
        "did not find a body with a top-bit-set CRC in 256 attempts; \
         either the search bound is too tight or crc32_iso3309 is broken"
    );
}

/// `TIPL` (v2.4 §4.2.2 involved-people list) carries `(role, name)`
/// pairs in a single text frame: encoding byte + alternating
/// NUL-terminated strings (`role_0\0 name_0\0 role_1\0 name_1\0 …`).
/// The typed accessor folds the parser's flat `values` back into pairs
/// so callers don't have to repeat the `chunks_exact(2)` boilerplate.
#[test]
fn tipl_involved_people_pairs_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TIPL".into(),
            values: vec![
                "producer".into(),
                "Alice".into(),
                "mixing engineer".into(),
                "Bob".into(),
            ],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let frame = parsed
        .frames
        .iter()
        .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TIPL"))
        .expect("TIPL survived round-trip");
    let pairs = frame
        .involved_people()
        .expect("TIPL surfaces via involved_people");
    assert_eq!(
        pairs,
        vec![
            ("producer".to_string(), "Alice".to_string()),
            ("mixing engineer".to_string(), "Bob".to_string()),
        ]
    );
    // Non-TIPL/IPLS frames must return None.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert!(other.involved_people().is_none());
    assert!(other.musician_credits().is_none());
}

/// `TMCL` (v2.4 §4.2.2 musician-credits list) carries
/// `(instrument, performer)` pairs in the same wire layout as `TIPL`.
/// Spec §4.2.2: "Every odd field is an instrument and every even is an
/// artist or a comma delimited list of artists."
#[test]
fn tmcl_musician_credits_pairs_v24() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Text {
            id: "TMCL".into(),
            values: vec![
                "guitar".into(),
                "Alice, Bob".into(),
                "drums".into(),
                "Carol".into(),
            ],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let frame = parsed
        .frames
        .iter()
        .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TMCL"))
        .expect("TMCL survived round-trip");
    let pairs = frame
        .musician_credits()
        .expect("TMCL surfaces via musician_credits");
    assert_eq!(
        pairs,
        vec![
            ("guitar".to_string(), "Alice, Bob".to_string()),
            ("drums".to_string(), "Carol".to_string()),
        ]
    );
    // TMCL is *not* TIPL: involved_people should NOT surface it.
    assert!(frame.involved_people().is_none());
    // And TIPL should not surface via musician_credits even though the
    // wire layout is identical — the two carry different logical maps
    // per spec §4.2.2.
    let tipl = Id3Frame::Text {
        id: "TIPL".into(),
        values: vec!["producer".into(), "Alice".into()],
    };
    assert!(tipl.musician_credits().is_none());
}

/// `IPLS` (v2.3 §4.4) carries the same role-to-name mapping as v2.4's
/// `TIPL`; surfacing both through one accessor lets a caller handle
/// either source version without matching on the underlying variant.
#[test]
fn ipls_involved_people_pairs_v23() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Ipls {
            pairs: vec![
                ("producer".to_string(), "Alice".to_string()),
                ("mixing engineer".to_string(), "Bob".to_string()),
            ],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_3).unwrap();
    let (parsed, _) = parse_tag(&bytes).unwrap();
    let frame = parsed
        .frames
        .iter()
        .find(|f| matches!(f, Id3Frame::Ipls { .. }))
        .expect("IPLS survived round-trip");
    let pairs = frame
        .involved_people()
        .expect("IPLS surfaces via involved_people");
    assert_eq!(
        pairs,
        vec![
            ("producer".to_string(), "Alice".to_string()),
            ("mixing engineer".to_string(), "Bob".to_string()),
        ]
    );
}

/// A non-conforming `TIPL` with an odd count (final role carries no
/// name) folds the trailing entry into a pair with an empty name,
/// matching how `IPLS` already surfaces the same truncation on the
/// parser side. Information is preserved structurally rather than
/// dropped or made to panic.
#[test]
fn tipl_odd_count_folds_trailing_role() {
    let tipl = Id3Frame::Text {
        id: "TIPL".into(),
        values: vec!["producer".into(), "Alice".into(), "mixing engineer".into()],
    };
    let pairs = tipl.involved_people().expect("TIPL pairs");
    assert_eq!(
        pairs,
        vec![
            ("producer".to_string(), "Alice".to_string()),
            ("mixing engineer".to_string(), String::new()),
        ]
    );
}

/// An empty `TIPL` (frame present, no entries) returns
/// `Some(Vec::new())` so the caller can still distinguish "frame
/// present but empty" from "frame absent" (`None`).
#[test]
fn tipl_empty_distinguishes_from_absent() {
    let empty = Id3Frame::Text {
        id: "TIPL".into(),
        values: vec![],
    };
    let pairs = empty.involved_people().expect("present but empty");
    assert!(pairs.is_empty());

    let absent = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert!(absent.involved_people().is_none());
}

/// `SyltContentType::from_wire` covers every spec value `$00..=$08`
/// and refuses any reserved byte by returning `None`. Round-trips
/// through `to_wire` recover the original byte for every variant —
/// the bijection over the spec range matches the contract on
/// [`TimestampUnit`] and [`Restrictions`].
#[test]
fn sylt_content_type_wire_bijection() {
    let spec_pairs = [
        (0u8, SyltContentType::Other),
        (1, SyltContentType::Lyrics),
        (2, SyltContentType::TextTranscription),
        (3, SyltContentType::MovementPartName),
        (4, SyltContentType::Events),
        (5, SyltContentType::Chord),
        (6, SyltContentType::Trivia),
        (7, SyltContentType::UrlsToWebpages),
        (8, SyltContentType::UrlsToImages),
    ];
    for (wire, typed) in spec_pairs {
        assert_eq!(SyltContentType::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }
    // Anything outside the spec range surfaces structurally as None.
    for reserved in 9u8..=255 {
        assert!(
            SyltContentType::from_wire(reserved).is_none(),
            "reserved SYLT content_type ${reserved:02x} unexpectedly decoded"
        );
    }
}

/// `Id3Frame::sylt_content_type` decodes the content-type byte of a
/// `SyncedLyrics` frame and returns `None` for any other variant or
/// any reserved wire byte. The accessor mirrors the cross-variant
/// posture of [`Id3Frame::timestamp_unit`].
#[test]
fn sylt_content_type_accessor_decodes_lyrics() {
    let frame = Id3Frame::SyncedLyrics {
        lang: *b"eng",
        time_format: 2,
        content_type: 1,
        description: "lyrics".into(),
        syncs: vec![("Hello".into(), 0)],
    };
    assert_eq!(frame.sylt_content_type(), Some(SyltContentType::Lyrics));

    let chord = Id3Frame::SyncedLyrics {
        lang: *b"eng",
        time_format: 2,
        content_type: 5,
        description: "chords".into(),
        syncs: vec![("Bb".into(), 0)],
    };
    assert_eq!(chord.sylt_content_type(), Some(SyltContentType::Chord));

    let reserved = Id3Frame::SyncedLyrics {
        lang: *b"eng",
        time_format: 2,
        content_type: 9,
        description: "future".into(),
        syncs: vec![],
    };
    assert_eq!(reserved.sylt_content_type(), None);

    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.sylt_content_type(), None);
}

/// A round-trip writer→parser preserves the SYLT content_type byte so
/// the typed accessor sees the same variant after re-parsing.
#[test]
fn sylt_content_type_roundtrips_v23_and_v24() {
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::SyncedLyrics {
                lang: *b"eng",
                time_format: 2,
                content_type: SyltContentType::MovementPartName.to_wire(),
                description: "movement".into(),
                syncs: vec![("Adagio".into(), 0), ("Allegro".into(), 4_000)],
            }],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let kind = parsed
            .frames
            .iter()
            .find_map(Id3Frame::sylt_content_type)
            .expect("SYLT content type surfaces after round-trip");
        assert_eq!(kind, SyltContentType::MovementPartName);
    }
}

/// `CommercialDelivery::from_wire` covers every spec value
/// `$00..=$08` and rejects any reserved byte. The `to_wire`
/// counterpart recovers the original byte for every variant.
#[test]
fn commercial_delivery_wire_bijection() {
    let spec_pairs = [
        (0u8, CommercialDelivery::Other),
        (1, CommercialDelivery::StandardCdAlbum),
        (2, CommercialDelivery::CompressedAudioOnCd),
        (3, CommercialDelivery::FileOverInternet),
        (4, CommercialDelivery::StreamOverInternet),
        (5, CommercialDelivery::NoteSheets),
        (6, CommercialDelivery::NoteSheetsInBook),
        (7, CommercialDelivery::MusicOnOtherMedia),
        (8, CommercialDelivery::NonMusicalMerchandise),
    ];
    for (wire, typed) in spec_pairs {
        assert_eq!(CommercialDelivery::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }
    for reserved in 9u8..=255 {
        assert!(
            CommercialDelivery::from_wire(reserved).is_none(),
            "reserved COMR received_as ${reserved:02x} unexpectedly decoded"
        );
    }
}

/// `Id3Frame::commercial_delivery` returns `Some(mode)` for a COMR
/// frame whose `received_as` is in the spec range and `None` for any
/// other variant or any reserved byte. Round-tripping a COMR through
/// the writer/parser preserves the byte so the accessor sees the same
/// variant after parsing.
#[test]
fn commercial_delivery_accessor_and_roundtrip() {
    let comr = Id3Frame::Commercial {
        price: "USD9.99".into(),
        valid_until: "20300101".into(),
        contact_url: "https://shop.example/contact".into(),
        received_as: CommercialDelivery::FileOverInternet.to_wire(),
        seller: "Example Music Shop".into(),
        description: "Single-track download".into(),
        logo_mime: String::new(),
        logo_data: Vec::new(),
    };
    assert_eq!(
        comr.commercial_delivery(),
        Some(CommercialDelivery::FileOverInternet)
    );

    let reserved = Id3Frame::Commercial {
        price: "USD0".into(),
        valid_until: "20300101".into(),
        contact_url: "https://shop.example".into(),
        received_as: 200,
        seller: "Shop".into(),
        description: "Desc".into(),
        logo_mime: String::new(),
        logo_data: Vec::new(),
    };
    assert_eq!(reserved.commercial_delivery(), None);

    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.commercial_delivery(), None);

    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![comr.clone()],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let mode = parsed
            .frames
            .iter()
            .find_map(Id3Frame::commercial_delivery)
            .expect("COMR commercial_delivery surfaces after round-trip");
        assert_eq!(mode, CommercialDelivery::FileOverInternet);
    }
}

/// `Rva2ChannelType::from_wire` covers every spec value `$00..=$08`
/// and refuses any reserved byte by returning `None`. Round-trips
/// through `to_wire` recover the original byte for every variant —
/// the bijection over the spec range matches the contract on
/// [`SyltContentType`] and [`CommercialDelivery`].
#[test]
fn rva2_channel_type_wire_bijection() {
    let spec_pairs = [
        (0u8, Rva2ChannelType::Other),
        (1, Rva2ChannelType::MasterVolume),
        (2, Rva2ChannelType::FrontRight),
        (3, Rva2ChannelType::FrontLeft),
        (4, Rva2ChannelType::BackRight),
        (5, Rva2ChannelType::BackLeft),
        (6, Rva2ChannelType::FrontCentre),
        (7, Rva2ChannelType::BackCentre),
        (8, Rva2ChannelType::Subwoofer),
    ];
    for (wire, typed) in spec_pairs {
        assert_eq!(Rva2ChannelType::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }
    // Anything outside the spec range surfaces structurally as None.
    for reserved in 9u8..=255 {
        assert!(
            Rva2ChannelType::from_wire(reserved).is_none(),
            "reserved RVA2 channel_type ${reserved:02x} unexpectedly decoded"
        );
    }
}

/// `Rva2Channel::channel_type_typed` decodes the channel-type byte of
/// an `RVA2` channel entry and returns `None` for any reserved wire
/// byte. The raw `channel_type: u8` field is preserved verbatim so a
/// non-conforming source still round-trips through the writer.
#[test]
fn rva2_channel_type_accessor_decodes_named_channels() {
    let master = Rva2Channel {
        channel_type: Rva2ChannelType::MasterVolume.to_wire(),
        volume_adjustment: 1024,
        bits_peak: 8,
        peak: vec![0x80],
    };
    assert_eq!(
        master.channel_type_typed(),
        Some(Rva2ChannelType::MasterVolume)
    );

    let sub = Rva2Channel {
        channel_type: Rva2ChannelType::Subwoofer.to_wire(),
        volume_adjustment: -512,
        bits_peak: 0,
        peak: Vec::new(),
    };
    assert_eq!(sub.channel_type_typed(), Some(Rva2ChannelType::Subwoofer));

    // Reserved byte preserves losslessly but the typed view collapses
    // to None per spec.
    let reserved = Rva2Channel {
        channel_type: 0x42,
        volume_adjustment: 0,
        bits_peak: 0,
        peak: Vec::new(),
    };
    assert_eq!(reserved.channel_type_typed(), None);
    assert_eq!(reserved.channel_type, 0x42);
}

/// A round-trip writer→parser preserves every RVA2 channel-type byte
/// — both the spec-named variants and reserved bytes — so the typed
/// accessor sees the same variant after re-parsing under both v2.3
/// and v2.4 envelopes (the wire layout is byte-aligned and identical
/// between versions per the v2.4 §4.11 frame definition).
#[test]
fn rva2_channel_type_roundtrips_v23_and_v24() {
    let channels = vec![
        Rva2Channel {
            channel_type: Rva2ChannelType::MasterVolume.to_wire(),
            volume_adjustment: 1024,
            bits_peak: 8,
            peak: vec![0x80],
        },
        Rva2Channel {
            channel_type: Rva2ChannelType::FrontLeft.to_wire(),
            volume_adjustment: -512,
            bits_peak: 0,
            peak: Vec::new(),
        },
        Rva2Channel {
            channel_type: Rva2ChannelType::Subwoofer.to_wire(),
            volume_adjustment: 0,
            bits_peak: 8,
            peak: vec![0xC0],
        },
        // Reserved byte: round-trips losslessly even though the
        // typed view is None.
        Rva2Channel {
            channel_type: 0x42,
            volume_adjustment: 256,
            bits_peak: 0,
            peak: Vec::new(),
        },
    ];
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::Rva2 {
                identification: "mix".into(),
                channels: channels.clone(),
            }],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let parsed_channels = parsed
            .frames
            .iter()
            .find_map(|f| match f {
                Id3Frame::Rva2 { channels, .. } => Some(channels.clone()),
                _ => None,
            })
            .expect("RVA2 frame surfaces after round-trip");
        assert_eq!(parsed_channels, channels);
        assert_eq!(
            parsed_channels[0].channel_type_typed(),
            Some(Rva2ChannelType::MasterVolume)
        );
        assert_eq!(
            parsed_channels[1].channel_type_typed(),
            Some(Rva2ChannelType::FrontLeft)
        );
        assert_eq!(
            parsed_channels[2].channel_type_typed(),
            Some(Rva2ChannelType::Subwoofer)
        );
        assert_eq!(parsed_channels[3].channel_type_typed(), None);
        assert_eq!(parsed_channels[3].channel_type, 0x42);
    }
}

/// `Equ2Interpolation::from_wire` covers every spec value `$00..=$01`
/// and rejects any reserved byte. The `to_wire` counterpart recovers
/// the original byte for every variant — `(from_wire, to_wire)` is a
/// bijection over the spec range, matching the contract published by
/// `SyltContentType`, `CommercialDelivery`, `Rva2ChannelType`, and
/// `Restrictions`.
#[test]
fn equ2_interpolation_wire_bijection() {
    let spec_pairs = [
        (0u8, Equ2Interpolation::Band),
        (1, Equ2Interpolation::Linear),
    ];
    for (wire, typed) in spec_pairs {
        assert_eq!(Equ2Interpolation::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }
    // Anything outside the spec range surfaces structurally as None.
    for reserved in 2u8..=255 {
        assert!(
            Equ2Interpolation::from_wire(reserved).is_none(),
            "reserved EQU2 interpolation method ${reserved:02x} unexpectedly decoded"
        );
    }
}

/// `Id3Frame::equ2_interpolation` decodes the interpolation-method
/// byte of an `Equ2` frame and returns `None` for any other variant or
/// any reserved wire byte. Mirrors the cross-variant posture of
/// [`Id3Frame::sylt_content_type`] and
/// [`Id3Frame::commercial_delivery`].
#[test]
fn equ2_interpolation_accessor_decodes_band_and_linear() {
    let band = Id3Frame::Equ2 {
        interpolation: 0,
        identification: "stage".into(),
        points: vec![(2_000, 256), (10_000, -512)],
    };
    assert_eq!(band.equ2_interpolation(), Some(Equ2Interpolation::Band));

    let linear = Id3Frame::Equ2 {
        interpolation: 1,
        identification: "studio".into(),
        points: vec![(440 * 2, 1024)],
    };
    assert_eq!(linear.equ2_interpolation(), Some(Equ2Interpolation::Linear));

    // Reserved (out-of-spec) interpolation byte surfaces as None — the
    // raw `interpolation: u8` field still round-trips losslessly, so a
    // non-conforming source preserves its byte through write.
    let reserved = Id3Frame::Equ2 {
        interpolation: 0x42,
        identification: "future".into(),
        points: vec![],
    };
    assert_eq!(reserved.equ2_interpolation(), None);

    // Any other frame variant returns None too.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.equ2_interpolation(), None);
}

/// A round-trip writer→parser preserves the EQU2 interpolation byte so
/// the typed accessor sees the same variant after re-parsing. EQU2 is
/// v2.4-only per spec; the writer accepts it under a v2.4 envelope.
#[test]
fn equ2_interpolation_roundtrips_v24() {
    for typed in [Equ2Interpolation::Band, Equ2Interpolation::Linear] {
        let tag = Id3Tag {
            version: Id3Version::V2_4,
            frames: vec![Id3Frame::Equ2 {
                interpolation: typed.to_wire(),
                identification: "spec-§4.12-roundtrip".into(),
                points: vec![(880, 256), (4_400, -256), (12_000, 0)],
            }],
        };
        let bytes = write_tag(&tag, Id3Version::V2_4).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let kind = parsed
            .frames
            .iter()
            .find_map(Id3Frame::equ2_interpolation)
            .expect("EQU2 interpolation surfaces after round-trip");
        assert_eq!(kind, typed);
    }
}

/// The raw `interpolation: u8` field round-trips losslessly through
/// `write_tag` for both spec-named and reserved bytes, so the typed
/// view never costs callers the ability to preserve forward-compatible
/// payloads — mirrors the contract pinned for `Rva2Channel::channel_type`.
#[test]
fn equ2_interpolation_preserves_reserved_byte_through_roundtrip() {
    let tag = Id3Tag {
        version: Id3Version::V2_4,
        frames: vec![Id3Frame::Equ2 {
            interpolation: 0x77,
            identification: "reserved".into(),
            points: vec![(2_000, 100)],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_4).expect("write");
    let (parsed, _) = parse_tag(&bytes).expect("parse");
    let raw = parsed
        .frames
        .iter()
        .find_map(|f| match f {
            Id3Frame::Equ2 { interpolation, .. } => Some(*interpolation),
            _ => None,
        })
        .expect("EQU2 round-trips");
    assert_eq!(raw, 0x77);
    // And the typed view collapses to None.
    assert_eq!(
        parsed.frames.iter().find_map(Id3Frame::equ2_interpolation),
        None
    );
}

/// `PopmRating::from_wire` / `to_wire` form a bijection over all 256
/// byte values: `$00` ↔ `Unknown` and every `$01..=$FF` ↔ `Rated(n)`
/// carrying that magnitude. Unlike the enumerated-variant accessors the
/// rating byte has no reserved range, so the decode is total — there is
/// no `None` arm to exercise here.
#[test]
fn popm_rating_wire_bijection() {
    assert_eq!(PopmRating::from_wire(0), PopmRating::Unknown);
    assert_eq!(PopmRating::Unknown.to_wire(), 0);
    assert!(!PopmRating::Unknown.is_rated());

    for n in 1u8..=255 {
        let typed = PopmRating::from_wire(n);
        assert_eq!(typed, PopmRating::Rated(n));
        assert_eq!(typed.to_wire(), n);
        assert!(typed.is_rated());
    }
}

/// `Id3Frame::popm_rating` decodes the rating byte of a `Popularimeter`
/// frame: `$00` → `Unknown` per the spec sentinel and any other value →
/// `Rated(n)` where `1` is worst and `255` is best. Any other frame
/// variant returns `None`.
#[test]
fn popm_rating_accessor_decodes_unknown_and_rated() {
    let unknown = Id3Frame::Popularimeter {
        email: "nobody@example.com".into(),
        rating: 0,
        counter: 0,
    };
    assert_eq!(unknown.popm_rating(), Some(PopmRating::Unknown));

    let worst = Id3Frame::Popularimeter {
        email: "critic@example.com".into(),
        rating: 1,
        counter: 3,
    };
    assert_eq!(worst.popm_rating(), Some(PopmRating::Rated(1)));

    let best = Id3Frame::Popularimeter {
        email: "fan@example.com".into(),
        rating: 255,
        counter: 9_001,
    };
    assert_eq!(best.popm_rating(), Some(PopmRating::Rated(255)));

    // Any other frame variant returns None.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.popm_rating(), None);
}

/// A round-trip writer→parser preserves the POPM rating byte so the
/// typed accessor sees the same `PopmRating` after re-parsing, across
/// both v2.3 and v2.4 (the rating semantic is identical in both docs).
#[test]
fn popm_rating_roundtrips_v23_and_v24() {
    for raw in [0u8, 1, 128, 196, 255] {
        for version in [Id3Version::V2_3, Id3Version::V2_4] {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Popularimeter {
                    email: "rater@example.com".into(),
                    rating: raw,
                    counter: 7,
                }],
            };
            let bytes = write_tag(&tag, version).expect("write");
            let (parsed, _) = parse_tag(&bytes).expect("parse");
            let typed = parsed
                .frames
                .iter()
                .find_map(Id3Frame::popm_rating)
                .expect("POPM rating surfaces after round-trip");
            assert_eq!(typed, PopmRating::from_wire(raw));
            assert_eq!(typed.to_wire(), raw);
        }
    }
}

/// `EtcoEventType::from_wire` covers every spec-named value
/// `$00..=$16` and round-trips back through `to_wire`. The continuation
/// marker `$FF` and the two audio-end markers `$FD` / `$FE` also
/// round-trip, and the user-defined synchronisation range `$E0..=$EF`
/// decodes to `NotPredefinedSync(slot)` where the slot is the low
/// nibble of the wire byte. The two reserved ranges (`$17..=$DF` and
/// `$F0..=$FC`) surface as `None` so a non-conforming or future byte
/// surfaces structurally rather than mapping to a guessed variant —
/// matching the contract published by `SyltContentType`,
/// `CommercialDelivery`, `Rva2ChannelType`, `Equ2Interpolation`, and
/// `Restrictions`.
#[test]
fn etco_event_type_wire_bijection() {
    let spec_named = [
        (0x00u8, EtcoEventType::Padding),
        (0x01, EtcoEventType::EndOfInitialSilence),
        (0x02, EtcoEventType::IntroStart),
        (0x03, EtcoEventType::MainPartStart),
        (0x04, EtcoEventType::OutroStart),
        (0x05, EtcoEventType::OutroEnd),
        (0x06, EtcoEventType::VerseStart),
        (0x07, EtcoEventType::RefrainStart),
        (0x08, EtcoEventType::InterludeStart),
        (0x09, EtcoEventType::ThemeStart),
        (0x0A, EtcoEventType::VariationStart),
        (0x0B, EtcoEventType::KeyChange),
        (0x0C, EtcoEventType::TimeChange),
        (0x0D, EtcoEventType::MomentaryUnwantedNoise),
        (0x0E, EtcoEventType::SustainedNoise),
        (0x0F, EtcoEventType::SustainedNoiseEnd),
        (0x10, EtcoEventType::IntroEnd),
        (0x11, EtcoEventType::MainPartEnd),
        (0x12, EtcoEventType::VerseEnd),
        (0x13, EtcoEventType::RefrainEnd),
        (0x14, EtcoEventType::ThemeEnd),
        (0x15, EtcoEventType::Profanity),
        (0x16, EtcoEventType::ProfanityEnd),
        (0xFD, EtcoEventType::AudioEnd),
        (0xFE, EtcoEventType::AudioFileEnds),
        (0xFF, EtcoEventType::Continuation),
    ];
    for (wire, typed) in spec_named {
        assert_eq!(EtcoEventType::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }

    // User-defined synchronisation range carries the low nibble as the
    // slot index and round-trips to the matching $E0..=$EF byte.
    for slot in 0u8..=15 {
        let wire = 0xE0 | slot;
        let typed = EtcoEventType::NotPredefinedSync(slot);
        assert_eq!(EtcoEventType::from_wire(wire), Some(typed));
        assert_eq!(typed.to_wire(), wire);
    }

    // Reserved ranges $17..=$DF and $F0..=$FC surface as None.
    for reserved in 0x17u8..=0xDF {
        assert!(
            EtcoEventType::from_wire(reserved).is_none(),
            "reserved ETCO event type ${reserved:02x} unexpectedly decoded"
        );
    }
    for reserved in 0xF0u8..=0xFC {
        assert!(
            EtcoEventType::from_wire(reserved).is_none(),
            "reserved ETCO event type ${reserved:02x} unexpectedly decoded"
        );
    }
}

/// `Id3Frame::etco_event_types` decodes the per-event type bytes of an
/// `EventTimingCodes` frame: one positional `Option<EtcoEventType>` per
/// source event, `Some(_)` for spec-defined bytes and `None` for the
/// reserved ranges. Returns `None` for any other frame variant.
#[test]
fn etco_event_types_accessor_decodes_mixed_payload() {
    let frame = Id3Frame::EventTimingCodes {
        time_format: 2, // milliseconds
        events: vec![
            (0x02, 1_000),    // intro start
            (0x06, 5_000),    // verse start
            (0xE3, 12_500),   // user sync slot 3
            (0x42, 18_000),   // reserved → None
            (0xFD, 180_000),  // audio end
            (0xFE, 180_500),  // audio file ends
            (0xFF, u32::MAX), // continuation marker
        ],
    };
    let decoded = frame.etco_event_types().expect("ETCO accessor surfaces");
    assert_eq!(
        decoded,
        vec![
            Some(EtcoEventType::IntroStart),
            Some(EtcoEventType::VerseStart),
            Some(EtcoEventType::NotPredefinedSync(3)),
            None,
            Some(EtcoEventType::AudioEnd),
            Some(EtcoEventType::AudioFileEnds),
            Some(EtcoEventType::Continuation),
        ],
    );

    // Length matches the source `events` length so positional indexing
    // stays stable when zipped against the raw timestamps.
    let raw_events = match &frame {
        Id3Frame::EventTimingCodes { events, .. } => events,
        _ => unreachable!(),
    };
    assert_eq!(decoded.len(), raw_events.len());

    // A non-ETCO variant returns None outright.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.etco_event_types(), None);
}

/// A round-trip writer→parser preserves every ETCO event-type byte —
/// spec-named, user-defined, end markers, and reserved — so the typed
/// accessor surfaces the same decoded vector after re-parsing. The
/// event-type table is identical in v2.3 and v2.4 (the table is
/// reproduced bit-for-bit in both version docs); this test covers both
/// envelopes.
#[test]
fn etco_event_types_roundtrip_v23_and_v24() {
    let events = vec![
        (0x00u8, 0u32),  // padding
        (0x02, 750),     // intro start
        (0x08, 1_200),   // interlude start
        (0x0B, 1_500),   // key change
        (0xE0, 1_800),   // user sync slot 0
        (0xEF, 1_900),   // user sync slot 15
        (0x55, 2_000),   // reserved-range round-trip
        (0xF5, 2_100),   // reserved-range round-trip
        (0xFD, 178_000), // audio end
        (0xFE, 178_500), // audio file ends
        (0xFF, 178_500), // continuation marker
    ];
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::EventTimingCodes {
                time_format: 2,
                events: events.clone(),
            }],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let decoded = parsed
            .frames
            .iter()
            .find_map(Id3Frame::etco_event_types)
            .expect("ETCO surfaces after round-trip");
        assert_eq!(
            decoded,
            vec![
                Some(EtcoEventType::Padding),
                Some(EtcoEventType::IntroStart),
                Some(EtcoEventType::InterludeStart),
                Some(EtcoEventType::KeyChange),
                Some(EtcoEventType::NotPredefinedSync(0)),
                Some(EtcoEventType::NotPredefinedSync(15)),
                None,
                None,
                Some(EtcoEventType::AudioEnd),
                Some(EtcoEventType::AudioFileEnds),
                Some(EtcoEventType::Continuation),
            ],
            "version {version:?} did not round-trip the typed view",
        );
        // The raw `events` vector also round-trips losslessly for every
        // byte (reserved bytes preserved verbatim through write).
        let raw_events = parsed
            .frames
            .iter()
            .find_map(|f| match f {
                Id3Frame::EventTimingCodes { events, .. } => Some(events.clone()),
                _ => None,
            })
            .expect("ETCO event vector surfaces");
        assert_eq!(raw_events, events, "version {version:?} raw events lost");
    }
}

/// `SytcTempo::from_wire` / `to_wire` form a bijection over the spec
/// range (`$00` BeatFree, `$01` SingleStroke, `2..=510` Bpm); values
/// `511..=u16::MAX` are outside the spec range and surface as `None`.
#[test]
fn sytc_tempo_wire_bijection() {
    assert_eq!(SytcTempo::from_wire(0), Some(SytcTempo::BeatFree));
    assert_eq!(SytcTempo::BeatFree.to_wire(), 0);
    assert_eq!(SytcTempo::from_wire(1), Some(SytcTempo::SingleStroke));
    assert_eq!(SytcTempo::SingleStroke.to_wire(), 1);

    // Boundary BPM values from the spec range walk through both ends
    // of both wire-encoding forms (single-byte 2..=254 + $FF-extension
    // 255..=510).
    for bpm in [2u16, 3, 120, 200, 254, 255, 256, 300, 509, 510] {
        assert_eq!(SytcTempo::from_wire(bpm), Some(SytcTempo::Bpm(bpm)));
        assert_eq!(SytcTempo::Bpm(bpm).to_wire(), bpm);
    }

    // Values beyond the spec range surface as None (the wire format
    // can't represent them, but the parser preserves the raw u16).
    for reserved in [511u16, 600, 1000, 0xFFFE, u16::MAX] {
        assert!(
            SytcTempo::from_wire(reserved).is_none(),
            "value {reserved} unexpectedly decoded outside spec range",
        );
    }
}

/// `Id3Frame::sytc_tempo_codes` decodes the per-record tempo values of
/// a `SyncedTempo` frame: one positional `Option<SytcTempo>` per
/// source code, `Some(_)` for spec-defined values (the reserved-meaning
/// $00/$01 plus 2..=510 BPM) and `None` for any value outside the spec
/// range. Returns `None` for any other frame variant.
#[test]
fn sytc_tempo_codes_accessor_decodes_mixed_payload() {
    let frame = Id3Frame::SyncedTempo {
        time_format: 2, // milliseconds
        codes: vec![
            (0u16, 0u32),      // beat-free at t=0
            (1, 500),          // single-stroke
            (120, 1_000),      // 120 BPM (single-byte wire form)
            (300, 5_500),      // 300 BPM ($FF $2D wire form)
            (510, 12_000_000), // upper edge of $FF extension
            (700, 13_000_000), // outside spec range → None
        ],
    };
    let decoded = frame.sytc_tempo_codes().expect("SYTC accessor surfaces");
    assert_eq!(
        decoded,
        vec![
            Some(SytcTempo::BeatFree),
            Some(SytcTempo::SingleStroke),
            Some(SytcTempo::Bpm(120)),
            Some(SytcTempo::Bpm(300)),
            Some(SytcTempo::Bpm(510)),
            None,
        ],
    );

    // Length matches the source `codes` length so positional indexing
    // stays stable when zipped against the raw timestamps.
    let raw_codes = match &frame {
        Id3Frame::SyncedTempo { codes, .. } => codes,
        _ => unreachable!(),
    };
    assert_eq!(decoded.len(), raw_codes.len());

    // A non-SYTC variant returns None outright.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.sytc_tempo_codes(), None);
}

/// A round-trip writer→parser preserves every SYTC tempo value the
/// wire format can represent (`0..=510`), so the typed accessor
/// surfaces the same decoded vector after re-parsing. The wire layout
/// is byte-aligned and version-independent; this test covers both v2.3
/// and v2.4 envelopes.
#[test]
fn sytc_tempo_codes_roundtrip_v23_and_v24() {
    let codes = vec![
        (0u16, 0u32),     // beat-free
        (1, 500),         // single stroke
        (60, 1_000),      // single-byte BPM
        (200, 2_000),     // single-byte BPM
        (254, 3_000),     // last single-byte BPM
        (255, 4_000),     // first $FF-extension BPM
        (256, 5_000),     // $FF-extension BPM
        (510, 6_000_000), // last $FF-extension BPM
    ];
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::SyncedTempo {
                time_format: 2,
                codes: codes.clone(),
            }],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let decoded = parsed
            .frames
            .iter()
            .find_map(Id3Frame::sytc_tempo_codes)
            .expect("SYTC surfaces after round-trip");
        assert_eq!(
            decoded,
            vec![
                Some(SytcTempo::BeatFree),
                Some(SytcTempo::SingleStroke),
                Some(SytcTempo::Bpm(60)),
                Some(SytcTempo::Bpm(200)),
                Some(SytcTempo::Bpm(254)),
                Some(SytcTempo::Bpm(255)),
                Some(SytcTempo::Bpm(256)),
                Some(SytcTempo::Bpm(510)),
            ],
            "version {version:?} did not round-trip the typed view",
        );
        // The raw `codes` vector also round-trips losslessly for every
        // value the wire format can represent.
        let raw_codes = parsed
            .frames
            .iter()
            .find_map(|f| match f {
                Id3Frame::SyncedTempo { codes, .. } => Some(codes.clone()),
                _ => None,
            })
            .expect("SYTC code vector surfaces");
        assert_eq!(raw_codes, codes, "version {version:?} raw codes lost");
    }
}

/// `Id3Frame::content_types` decodes the v2.3 parenthesised `TCON`
/// grammar (spec v2.3 §4.2.1): numeric ID3v1 genre references `(21)`,
/// the `(RX)` / `(CR)` keyword references, multiple references in one
/// string `(51)(39)`, a numeric reference plus a free-text refinement
/// `(4)Eurodisco`, the `((`-escaped literal-`(` custom genre, and an
/// out-of-table numeric index surfacing as `name: None`.
#[test]
fn content_types_accessor_parses_v23_parenthesised() {
    // `(21)` → Ska (index 21 in the Winamp-extended ID3v1 table).
    let single = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["(21)".into()],
    };
    assert_eq!(
        single.content_types().expect("TCON surfaces"),
        vec![ContentType::Genre {
            index: 21,
            name: Some("Ska"),
        }],
    );

    // `(RX)` Remix and `(CR)` Cover keyword references.
    let keywords = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["(RX)(CR)".into()],
    };
    assert_eq!(
        keywords.content_types().expect("TCON surfaces"),
        vec![ContentType::Remix, ContentType::Cover],
    );

    // `(51)(39)` → two numeric references in one string.
    let multi = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["(51)(39)".into()],
    };
    assert_eq!(
        multi.content_types().expect("TCON surfaces"),
        vec![
            ContentType::Genre {
                index: 51,
                name: Some("Techno-Industrial"),
            },
            ContentType::Genre {
                index: 39,
                name: Some("Noise"),
            },
        ],
    );

    // `(4)Eurodisco` → numeric reference plus a free-text refinement.
    let refined = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["(4)Eurodisco".into()],
    };
    assert_eq!(
        refined.content_types().expect("TCON surfaces"),
        vec![
            ContentType::Genre {
                index: 4,
                name: Some("Disco"),
            },
            ContentType::Custom("Eurodisco".into()),
        ],
    );

    // `((I can figure out any genre)` → `((` escapes a literal leading
    // `(`; the refinement keeps a single `(`.
    let escaped = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["((I can figure out any genre)".into()],
    };
    assert_eq!(
        escaped.content_types().expect("TCON surfaces"),
        vec![ContentType::Custom("(I can figure out any genre)".into())],
    );

    // An out-of-table numeric index surfaces structurally with
    // `name: None` rather than being dropped.
    let out_of_table = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["(200)".into()],
    };
    assert_eq!(
        out_of_table.content_types().expect("TCON surfaces"),
        vec![ContentType::Genre {
            index: 200,
            name: None,
        }],
    );

    // A non-TCON frame returns None outright.
    let other = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Song".into()],
    };
    assert_eq!(other.content_types(), None);
}

/// `Id3Frame::content_types` decodes the v2.4 bare `TCON` form (spec
/// v2.4 §4.2.3): a bare numeric string is a genre reference, `RX` / `CR`
/// are bare keyword references, NUL-separated values are independent
/// references, and any non-numeric non-keyword value is free text.
#[test]
fn content_types_accessor_parses_v24_bare() {
    // Bare `"21"` numeric reference + a `"Eurodisco"` custom value, as
    // the parser splits a `"21\0Eurodisco"` v2.4 frame.
    let frame = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["21".into(), "Eurodisco".into()],
    };
    assert_eq!(
        frame.content_types().expect("TCON surfaces"),
        vec![
            ContentType::Genre {
                index: 21,
                name: Some("Ska"),
            },
            ContentType::Custom("Eurodisco".into()),
        ],
    );

    // Bare `RX` / `CR` keyword references across two NUL-split values.
    let keywords = Id3Frame::Text {
        id: "TCON".into(),
        values: vec!["RX".into(), "CR".into()],
    };
    assert_eq!(
        keywords.content_types().expect("TCON surfaces"),
        vec![ContentType::Remix, ContentType::Cover],
    );

    // A present-but-empty TCON yields an empty reference list.
    let empty = Id3Frame::Text {
        id: "TCON".into(),
        values: vec![],
    };
    assert_eq!(empty.content_types(), Some(Vec::new()));
}

/// A round-trip writer→parser preserves the raw `TCON` string, so the
/// typed accessor surfaces the same content-type references after
/// re-parsing under both v2.3 and v2.4 envelopes. The writer joins
/// multi-value text frames with `/` for v2.3 and NUL for v2.4; both
/// produce a value list the accessor re-flattens onto the same vector.
#[test]
fn content_types_roundtrip_v23_and_v24() {
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        // A single parenthesised v2.3-style value round-trips under
        // either envelope (no embedded NUL, so the writer keeps it as a
        // single value).
        let tag = Id3Tag {
            version,
            frames: vec![Id3Frame::Text {
                id: "TCON".into(),
                values: vec!["(21)".into()],
            }],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let decoded = parsed
            .frames
            .iter()
            .find_map(Id3Frame::content_types)
            .expect("TCON surfaces after round-trip");
        assert_eq!(
            decoded,
            vec![ContentType::Genre {
                index: 21,
                name: Some("Ska"),
            }],
            "version {version:?} did not round-trip the typed view",
        );
    }
}

fn tmed(values: Vec<&str>) -> Id3Frame {
    Id3Frame::Text {
        id: "TMED".into(),
        values: values.into_iter().map(str::to_string).collect(),
    }
}

#[test]
fn media_type_accessor_parses_v23_parenthesised() {
    // Bare predefined reference (spec example "(CD/A)" → CD + /A).
    assert_eq!(
        tmed(vec!["(CD/A)"]).media_type().expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "CD".into(),
            name: Some("CD"),
            refinements: vec!["A".into()],
            text: None,
        }],
    );

    // Multi-refinement reference (spec example "(VID/PAL/VHS)").
    assert_eq!(
        tmed(vec!["(VID/PAL/VHS)"])
            .media_type()
            .expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "VID".into(),
            name: Some("Video"),
            refinements: vec!["PAL".into(), "VHS".into()],
            text: None,
        }],
    );

    // Reference followed by a free-text refinement (spec example
    // "(MC) with four channels").
    assert_eq!(
        tmed(vec!["(MC) with four channels"])
            .media_type()
            .expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "MC".into(),
            name: Some("MC (normal cassette)"),
            refinements: vec![],
            text: Some(" with four channels".into()),
        }],
    );

    // "((" escapes a literal '(' beginning a free-text media name; the
    // escape collapses to a single leading '('.
    assert_eq!(
        tmed(vec!["((my own studio reel"])
            .media_type()
            .expect("TMED surfaces"),
        vec![MediaType::Custom("(my own studio reel".into())],
    );

    // An out-of-table top-level code surfaces structurally with name: None
    // rather than being dropped (forward-compatible reference).
    assert_eq!(
        tmed(vec!["(NEWMEDIA/X)"])
            .media_type()
            .expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "NEWMEDIA".into(),
            name: None,
            refinements: vec!["X".into()],
            text: None,
        }],
    );

    // A non-TMED frame returns None.
    assert_eq!(
        Id3Frame::Text {
            id: "TIT2".into(),
            values: vec!["x".into()],
        }
        .media_type(),
        None,
    );
}

#[test]
fn media_type_accessor_parses_v24_bare() {
    // v2.4 dropped the parentheses — the spec's own example "VID/PAL/VHS".
    assert_eq!(
        tmed(vec!["VID/PAL/VHS"])
            .media_type()
            .expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "VID".into(),
            name: Some("Video"),
            refinements: vec!["PAL".into(), "VHS".into()],
            text: None,
        }],
    );

    // Top-level code only, no refinement.
    assert_eq!(
        tmed(vec!["DIG"]).media_type().expect("TMED surfaces"),
        vec![MediaType::Predefined {
            media: "DIG".into(),
            name: Some("Other digital media"),
            refinements: vec![],
            text: None,
        }],
    );

    // A v2.4 NUL list yields one reference per value.
    assert_eq!(
        tmed(vec!["CD/DD", "RAD/FM"])
            .media_type()
            .expect("TMED surfaces"),
        vec![
            MediaType::Predefined {
                media: "CD".into(),
                name: Some("CD"),
                refinements: vec!["DD".into()],
                text: None,
            },
            MediaType::Predefined {
                media: "RAD".into(),
                name: Some("Radio"),
                refinements: vec!["FM".into()],
                text: None,
            },
        ],
    );

    // An empty value (degenerate) surfaces as Custom("") rather than
    // panicking or being silently dropped.
    assert_eq!(
        tmed(vec![""]).media_type().expect("TMED surfaces"),
        vec![MediaType::Custom(String::new())],
    );
}

#[test]
fn media_type_roundtrips_v23_and_v24() {
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        // A single value with no embedded NUL round-trips as one value
        // under either envelope, so the typed view is preserved.
        let tag = Id3Tag {
            version,
            frames: vec![tmed(vec!["(VID/PAL/VHS)"])],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let decoded = parsed
            .frames
            .iter()
            .find_map(Id3Frame::media_type)
            .expect("TMED surfaces after round-trip");
        assert_eq!(
            decoded,
            vec![MediaType::Predefined {
                media: "VID".into(),
                name: Some("Video"),
                refinements: vec!["PAL".into(), "VHS".into()],
                text: None,
            }],
            "version {version:?} did not round-trip the typed view",
        );
    }
}

fn tflt(values: Vec<&str>) -> Id3Frame {
    Id3Frame::Text {
        id: "TFLT".into(),
        values: values.into_iter().map(str::to_string).collect(),
    }
}

#[test]
fn file_type_accessor_parses_predefined() {
    // Top-level code with a refinement (spec example "MPG" + "/3").
    assert_eq!(
        tflt(vec!["MPG/3"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "MPG".into(),
            name: Some("MPEG Audio"),
            refinements: vec!["3".into()],
        }],
    );

    // MPEG 2.5 refinement keeps its dotted form verbatim.
    assert_eq!(
        tflt(vec!["MPG/2.5"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "MPG".into(),
            name: Some("MPEG Audio"),
            refinements: vec!["2.5".into()],
        }],
    );

    // Top-level code only, no refinement.
    assert_eq!(
        tflt(vec!["PCM"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "PCM".into(),
            name: Some("Pulse Code Modulated audio"),
            refinements: vec![],
        }],
    );

    // VQF top-level code.
    assert_eq!(
        tflt(vec!["VQF"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "VQF".into(),
            name: Some("Transform-domain Weighted Interleave Vector Quantization"),
            refinements: vec![],
        }],
    );
}

#[test]
fn file_type_accessor_v24_mime_and_unknown_codes() {
    // v2.4-added "MIME" top-level code resolves under either envelope.
    assert_eq!(
        tflt(vec!["MIME"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "MIME".into(),
            name: Some("MIME type follows"),
            refinements: vec![],
        }],
    );

    // An out-of-table top-level code surfaces structurally with
    // name: None so a forward-compatible reference is preserved.
    assert_eq!(
        tflt(vec!["OGG/Q5"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Predefined {
            code: "OGG".into(),
            name: None,
            refinements: vec!["Q5".into()],
        }],
    );

    // A value whose top-level segment is empty surfaces as Custom.
    assert_eq!(
        tflt(vec!["/3"]).file_type().expect("TFLT surfaces"),
        vec![FileType::Custom("/3".into())],
    );

    // An empty value (degenerate) surfaces as Custom("") rather than
    // panicking or being silently dropped.
    assert_eq!(
        tflt(vec![""]).file_type().expect("TFLT surfaces"),
        vec![FileType::Custom(String::new())],
    );

    // A NUL list yields one reference per value.
    assert_eq!(
        tflt(vec!["MPG/1", "PCM"])
            .file_type()
            .expect("TFLT surfaces"),
        vec![
            FileType::Predefined {
                code: "MPG".into(),
                name: Some("MPEG Audio"),
                refinements: vec!["1".into()],
            },
            FileType::Predefined {
                code: "PCM".into(),
                name: Some("Pulse Code Modulated audio"),
                refinements: vec![],
            },
        ],
    );

    // A non-TFLT frame returns None.
    assert_eq!(tmed(vec!["(CD)"]).file_type(), None);
}

#[test]
fn file_type_roundtrips_v23_and_v24() {
    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        let tag = Id3Tag {
            version,
            frames: vec![tflt(vec!["MPG/3"])],
        };
        let bytes = write_tag(&tag, version).expect("write");
        let (parsed, _) = parse_tag(&bytes).expect("parse");
        let decoded = parsed
            .frames
            .iter()
            .find_map(Id3Frame::file_type)
            .expect("TFLT surfaces after round-trip");
        assert_eq!(
            decoded,
            vec![FileType::Predefined {
                code: "MPG".into(),
                name: Some("MPEG Audio"),
                refinements: vec!["3".into()],
            }],
            "version {version:?} did not round-trip the typed view",
        );
    }
}

// ---------------------------------------------------------------------------
// ID3v2.2 writer (spec id3v2-00)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_v22_common_frames() {
    // Build a tag with the canonical four-char ids; the v2.2 writer
    // demotes each to its three-char id, and the parser promotes them
    // back, so the round-trip is the identity at the logical level.
    let tag = make_tag(Id3Version::V2_2);
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write v2.2");

    // Header: "ID3", major 2, revision 0, flags 0 (no unsync here).
    assert_eq!(&bytes[0..3], b"ID3");
    assert_eq!(bytes[3], 2);
    assert_eq!(bytes[4], 0);
    assert_eq!(bytes[5], 0);

    // First frame header is six bytes: three-char id "TT2" + 3-byte BE
    // size. Confirm the writer emitted the demoted id, not "TIT2".
    assert_eq!(&bytes[10..13], b"TT2");

    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse v2.2");
    assert_eq!(consumed, bytes.len());
    assert_eq!(parsed.version, Id3Version::V2_2);

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

    // The picture round-trips through the v2.2 PIC layout (3-char
    // "JPG" image format reconstructed back to image/jpeg by the
    // parser).
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
fn v22_pic_emits_three_char_image_format() {
    // A PNG picture must serialise to the v2.2 PIC "PNG" image-format
    // code (not a NUL-terminated MIME string as v2.3 APIC would).
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::Picture(AttachedPicture {
            mime_type: "image/png".into(),
            picture_type: PictureType::BackCover,
            description: String::new(),
            data: vec![0x89, 0x50, 0x4E, 0x47],
        })],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write v2.2");
    // Frame header at offset 10: "PIC" + 3-byte size, then payload:
    // encoding byte (1) + "PNG" image format + picture-type byte.
    assert_eq!(&bytes[10..13], b"PIC");
    let payload = &bytes[16..];
    assert_eq!(payload[0], 1, "encoding byte should be UCS-2");
    assert_eq!(&payload[1..4], b"PNG", "3-char image format");
    assert_eq!(payload[4], PictureType::BackCover as u8);

    let (parsed, _) = parse_tag(&bytes).expect("re-parse");
    let pics = attached_pictures(&parsed);
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].mime_type, "image/png");
    assert_eq!(pics[0].picture_type, PictureType::BackCover);
    assert_eq!(pics[0].data, vec![0x89, 0x50, 0x4E, 0x47]);
}

#[test]
fn v22_whole_tag_unsync_roundtrips() {
    // A data payload carrying a false sync (0xFF 0xF0) must survive
    // whole-tag unsync under the v2.2 envelope.
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::Picture(AttachedPicture {
            mime_type: "image/jpeg".into(),
            picture_type: PictureType::Other,
            description: String::new(),
            data: vec![0xFF, 0xF0, 0x00, 0xFF, 0xFB, 0x90],
        })],
    };
    let opts = WriteOptions::new().with_unsync(UnsyncMode::WholeTag);
    let bytes = write_tag_with_options(&tag, Id3Version::V2_2, &opts).expect("write");
    // Header unsync flag (bit 7) set.
    assert_eq!(bytes[5] & 0x80, 0x80);

    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse");
    assert_eq!(consumed, bytes.len());
    let pics = attached_pictures(&parsed);
    assert_eq!(pics[0].data, vec![0xFF, 0xF0, 0x00, 0xFF, 0xFB, 0x90]);
}

#[test]
fn v22_skips_frames_without_v22_equivalent() {
    // SEEK is a v2.4-only frame with no v2.2 demotion; it must be
    // dropped rather than emitted under a truncated id, while the
    // text frame beside it survives.
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![
            Id3Frame::Text {
                id: "TIT2".into(),
                values: vec!["Keep me".into()],
            },
            Id3Frame::Unknown {
                id: "SEEK".into(),
                raw: vec![0, 0, 0, 0],
            },
        ],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write");
    // Only the demoted "TT2" frame should appear; "SEE"/"SEEK" must not.
    assert_eq!(&bytes[10..13], b"TT2");
    assert!(
        !bytes.windows(3).any(|w| w == b"SEE"),
        "v2.4-only SEEK must be skipped, not truncated"
    );
    let (parsed, _) = parse_tag(&bytes).expect("re-parse");
    assert_eq!(
        find_text(&parsed, "TIT2"),
        Some(&["Keep me".to_string()][..])
    );
    assert_eq!(parsed.frames.len(), 1);
}

#[test]
fn v22_rejects_v23plus_only_options() {
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![],
    };
    // CRC / footer / compression / update / restrictions are all
    // post-v2.2 features and must be rejected loudly.
    for opts in [
        WriteOptions::new().with_crc(true),
        WriteOptions::new().with_footer(true),
        WriteOptions::new().with_compression(true),
        WriteOptions::new().with_update(true),
        WriteOptions::new().with_restrictions(Some(Restrictions {
            tag_size: TagSizeRestriction::Max64Frames128Kb,
            text_encoding: TextEncodingRestriction::Unrestricted,
            text_fields: TextFieldsRestriction::Unrestricted,
            image_encoding: ImageEncodingRestriction::Unrestricted,
            image_size: ImageSizeRestriction::Unrestricted,
        })),
    ] {
        assert!(
            write_tag_with_options(&tag, Id3Version::V2_2, &opts).is_err(),
            "post-v2.2 option should be rejected"
        );
    }
}

#[test]
fn v22_structural_frame_roundtrips() {
    // A structured non-text frame (POPM) whose §4 body is shared with
    // v2.3 demotes to "POP" and round-trips through the v2.2 envelope.
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::Popularimeter {
            email: "rater@example.com".into(),
            rating: 196,
            counter: 4242,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write");
    assert_eq!(&bytes[10..13], b"POP");
    let (parsed, _) = parse_tag(&bytes).expect("re-parse");
    let popm = parsed
        .frames
        .iter()
        .find_map(|f| match f {
            Id3Frame::Popularimeter {
                email,
                rating,
                counter,
            } => Some((email.clone(), *rating, *counter)),
            _ => None,
        })
        .expect("POPM survives");
    assert_eq!(popm, ("rater@example.com".to_string(), 196, 4242));
}

/// The `CRM` encrypted-meta frame is ID3v2.2-only (§4.20) — v2.3+
/// replaced it with `ENCR` + per-frame encryption — so it is the one
/// structural frame whose parser (`parse_crm`) and serialiser have no
/// v2.3/v2.4 counterpart. This pins the parse → write → parse symmetry
/// through the v2.2 envelope: owner id, content/explanation, and the
/// opaque encrypted block (which may itself contain a NUL) all survive
/// byte-for-byte, and the writer emits the three-char "CRM" id with no
/// encoding byte (the frame predates one).
#[test]
fn roundtrip_v22_crm_encrypted_meta() {
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::EncryptedMeta {
            owner: "owner@example.com".into(),
            content: "explanation text".into(),
            // A block whose bytes include a $00 — the parser splits on
            // exactly two terminators, so an embedded NUL in the
            // encrypted block must not be treated as a field boundary.
            encrypted: vec![0xDE, 0xAD, 0x00, 0xBE, 0xEF],
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write v2.2");
    // Frame header: three-char "CRM" id (no encoding byte in the body).
    assert_eq!(&bytes[10..13], b"CRM");

    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse v2.2");
    assert_eq!(consumed, bytes.len());
    let got = parsed
        .frames
        .iter()
        .find_map(|f| match f {
            Id3Frame::EncryptedMeta {
                owner,
                content,
                encrypted,
            } => Some((owner.clone(), content.clone(), encrypted.clone())),
            _ => None,
        })
        .expect("CRM survives");
    assert_eq!(
        got,
        (
            "owner@example.com".to_string(),
            "explanation text".to_string(),
            vec![0xDE, 0xAD, 0x00, 0xBE, 0xEF],
        )
    );
}

/// Regression: an ID3v2.2 `RVA` frame must survive parse → write →
/// parse through the v2.2 envelope. The bug was that the v2.2 writer
/// routed the parsed `Rvad` through the v2.3 `RVAD` encoder, which keys
/// front-channel presence on the inc/dec *sign* bits — so a
/// both-decrement frame (inc/dec `$00`, which §4.12 still carries with
/// both front magnitudes) was rejected with an
/// "inc/dec front bits and `front` channel block disagree" error rather
/// than written. The dedicated v2.2 `encode_rva_v22` path fixes it:
/// v2.2 lists the front fields unconditionally, so the round trip holds
/// for every inc/dec combination including all-decrement.
#[test]
fn roundtrip_v22_rva_both_decrement() {
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::Rvad {
            increment_decrement: 0x00, // both channels decrement
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x01, 0x00],
                    peak: vec![0x7F, 0xFF],
                },
                left: RvadChannel {
                    volume_delta: vec![0x02, 0x00],
                    peak: vec![0x7E, 0x00],
                },
            }),
            back: None,
            center: None,
            bass: None,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write v2.2 RVA");
    // Three-char "RVA" id, not "RVAD".
    assert_eq!(&bytes[10..13], b"RVA");
    let (parsed, consumed) = parse_tag(&bytes).expect("re-parse v2.2 RVA");
    assert_eq!(consumed, bytes.len());
    match &parsed.frames[0] {
        Id3Frame::Rvad {
            increment_decrement,
            bits_used,
            front,
            back,
            center,
            bass,
        } => {
            assert_eq!(*increment_decrement, 0x00);
            assert_eq!(*bits_used, 16);
            let front = front.as_ref().expect("front block");
            assert_eq!(front.right.volume_delta, vec![0x01, 0x00]);
            assert_eq!(front.left.volume_delta, vec![0x02, 0x00]);
            assert_eq!(front.right.peak, vec![0x7F, 0xFF]);
            assert_eq!(front.left.peak, vec![0x7E, 0x00]);
            assert!(back.is_none() && center.is_none() && bass.is_none());
        }
        other => panic!("expected Rvad, got {other:?}"),
    }
}

/// A v2.3 `RVAD` with back/centre/bass channels written to v2.2 emits
/// only the front pair: v2.2 §4.12 defines no higher channels, so they
/// are an intentional, spec-bounded loss (the front data is preserved
/// exactly). The inc/dec byte's higher bits are still written verbatim;
/// the re-parse keeps `back`/`center`/`bass` as `None` because the v2.2
/// layout has no slots for them. This documents the down-conversion is
/// lossy-but-valid rather than an error.
#[test]
fn roundtrip_v23_rvad_back_channels_to_v22_keeps_front() {
    let tag = Id3Tag {
        version: Id3Version::V2_3,
        frames: vec![Id3Frame::Rvad {
            increment_decrement: 0b0000_1111, // front + back, all increment
            bits_used: 16,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x10, 0x00],
                    peak: vec![0x20, 0x00],
                },
                left: RvadChannel {
                    volume_delta: vec![0x11, 0x00],
                    peak: vec![0x21, 0x00],
                },
            }),
            back: Some(RvadBackChannels {
                right_back: RvadChannel {
                    volume_delta: vec![0x30, 0x00],
                    peak: vec![0x40, 0x00],
                },
                left_back: RvadChannel {
                    volume_delta: vec![0x31, 0x00],
                    peak: vec![0x41, 0x00],
                },
            }),
            center: None,
            bass: None,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write v2.2");
    assert_eq!(&bytes[10..13], b"RVA");
    let (parsed, _) = parse_tag(&bytes).expect("re-parse");
    match &parsed.frames[0] {
        Id3Frame::Rvad {
            front,
            back,
            center,
            bass,
            ..
        } => {
            let front = front.as_ref().expect("front survives");
            assert_eq!(front.right.volume_delta, vec![0x10, 0x00]);
            assert_eq!(front.left.peak, vec![0x21, 0x00]);
            // Back/centre/bass have no v2.2 wire form.
            assert!(back.is_none() && center.is_none() && bass.is_none());
        }
        other => panic!("expected Rvad, got {other:?}"),
    }
}

/// The v2.2 `RVA` "peaks completely omitted" form (§4.12) also
/// round-trips: a frame with empty peak vecs writes only the two
/// volume-change fields and re-parses with empty peaks. Covers the
/// 8-bit field width too (one byte per field).
#[test]
fn roundtrip_v22_rva_omitted_peaks_8bit() {
    let tag = Id3Tag {
        version: Id3Version::V2_2,
        frames: vec![Id3Frame::Rvad {
            increment_decrement: 0x03, // both increment
            bits_used: 8,
            front: Some(RvadFrontChannels {
                right: RvadChannel {
                    volume_delta: vec![0x05],
                    peak: vec![],
                },
                left: RvadChannel {
                    volume_delta: vec![0x06],
                    peak: vec![],
                },
            }),
            back: None,
            center: None,
            bass: None,
        }],
    };
    let bytes = write_tag(&tag, Id3Version::V2_2).expect("write");
    // body = inc/dec + bits + right-delta + left-delta = 4 bytes; no peaks.
    let frame_size = u32::from_be_bytes([0, bytes[13], bytes[14], bytes[15]]);
    assert_eq!(frame_size, 4);
    let (parsed, _) = parse_tag(&bytes).expect("re-parse");
    match &parsed.frames[0] {
        Id3Frame::Rvad { front, .. } => {
            let front = front.as_ref().unwrap();
            assert_eq!(front.right.volume_delta, vec![0x05]);
            assert_eq!(front.left.volume_delta, vec![0x06]);
            assert!(front.right.peak.is_empty());
            assert!(front.left.peak.is_empty());
        }
        other => panic!("expected Rvad, got {other:?}"),
    }
}

#[test]
fn roundtrip_tkey_initial_key_typed_view() {
    // The TKEY initial-key typed accessor decodes the spec grammar
    // (ground key A..G + optional b/# halfkey + optional m minor, the
    // standalone "o" off-key) and survives a full write -> parse cycle
    // under both v2.3 and v2.4 envelopes via the public surface.
    let cases: &[(&str, MusicalKey)] = &[
        (
            "Dbm",
            MusicalKey::Key {
                tonic: 'D',
                accidental: Some(KeyAccidental::Flat),
                minor: true,
            },
        ),
        (
            "F#",
            MusicalKey::Key {
                tonic: 'F',
                accidental: Some(KeyAccidental::Sharp),
                minor: false,
            },
        ),
        (
            "C",
            MusicalKey::Key {
                tonic: 'C',
                accidental: None,
                minor: false,
            },
        ),
        ("o", MusicalKey::OffKey),
        // A non-conforming value is preserved verbatim.
        ("Z9", MusicalKey::Custom("Z9".to_string())),
    ];

    for version in [Id3Version::V2_3, Id3Version::V2_4] {
        for (wire, expected) in cases {
            let tag = Id3Tag {
                version,
                frames: vec![Id3Frame::Text {
                    id: "TKEY".into(),
                    values: vec![(*wire).to_string()],
                }],
            };
            let bytes = write_tag(&tag, version).expect("write");
            let (parsed, _) = parse_tag(&bytes).expect("re-parse");
            let tkey = parsed
                .frames
                .iter()
                .find(|f| matches!(f, Id3Frame::Text { id, .. } if id == "TKEY"))
                .expect("TKEY survives");
            // The typed accessor reconstructs the expected key.
            assert_eq!(
                tkey.initial_key(),
                Some(vec![expected.clone()]),
                "wire {wire:?} under {version:?}"
            );
            // The raw value also round-trips losslessly.
            assert_eq!(
                find_text(&parsed, "TKEY"),
                Some([(*wire).to_string()].as_slice())
            );
        }
    }

    // The accessor returns None for any other frame.
    let tit2 = Id3Frame::Text {
        id: "TIT2".into(),
        values: vec!["Dbm".into()],
    };
    assert_eq!(tit2.initial_key(), None);
}
