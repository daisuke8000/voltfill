use serde_json::json;
use voltfill_core::candidates::CandidateIndex;
use voltfill_core::geometry::Rect;
use voltfill_core::model::{Quality, SourceFragment, SourceSpan};
use voltfill_core::schema::{Normalization, OutputType, Validator};
use voltfill_core::validate;
fn fragment(id: &str, page: u32, text: &str, x: f64) -> SourceFragment {
    SourceFragment {
        id: id.into(),
        page,
        item_index: 0,
        raw_text: text.into(),
        bbox: Rect::new(x, 0., x + 10., 10.)
            .expect("test rectangle must have positive finite extent"),
        quality: Quality::Usable,
    }
}

#[test]
fn page_queries_preserve_snapshot_order_limits_and_rectangle_errors() {
    // Deliberately interleave pages and put IDs out of lexical order.
    let raw = [
        fragment("z", 2, "same", 0.),
        fragment("b", 1, "same", 0.),
        fragment("a", 2, "same", 20.),
        fragment("c", 1, "same", 20.),
    ];
    let index =
        CandidateIndex::new(&raw).expect("source fixture IDs and usable geometry must be valid");
    for pages in [vec![], vec![2], vec![9], vec![2, 1], vec![2, 2]] {
        let expected: Vec<_> = index
            .candidates()
            .iter()
            .filter(|c| pages.contains(&c.page))
            .collect();
        assert_eq!(
            index
                .scoped(&pages, expected.len())
                .expect("exact candidate limit must admit the expected pages"),
            expected
        );
        if !expected.is_empty() {
            assert!(index.scoped(&pages, expected.len() - 1).is_err());
        }
    }
    for page in [1, 2, 9] {
        for bbox in [
            Rect::new(0., 0., 30., 10.).expect("test rectangle must have positive finite extent"),
            Rect::new(10., 0., 20., 10.).expect("test rectangle must have positive finite extent"), // Edges only: no overlap.
            Rect::new(5., 0., 25., 10.).expect("test rectangle must have positive finite extent"), // Partial runs must fail in_box.
        ] {
            let expected: Vec<_> = index
                .candidates()
                .iter()
                .filter(|c| {
                    c.page == page
                        && bbox.left < c.bbox.right
                        && c.bbox.left < bbox.right
                        && bbox.top < c.bbox.bottom
                        && c.bbox.top < bbox.bottom
                })
                .collect();
            assert_eq!(
                index
                    .overlapping(page, bbox)
                    .expect("valid box must permit overlap lookup"),
                expected
            );
            if expected.iter().all(|c| bbox.contains(c.bbox)) {
                assert_eq!(
                    index
                        .in_box(page, bbox)
                        .expect("fully contained runs must be accepted"),
                    expected
                );
            } else {
                assert!(index.in_box(page, bbox).is_err());
            }
        }
        let invalid = Rect {
            left: f64::NAN,
            top: 0.,
            right: 30.,
            bottom: 10.,
        };
        assert!(index.overlapping(page, invalid).is_err());
        assert!(index.in_box(page, invalid).is_err());
    }
}
#[test]
fn identical_values_at_distinct_locations_keep_provenance_and_overflow_is_not_truncation() {
    let raw = [
        fragment("a", 1, "日本語", 0.),
        fragment("b", 1, "日本語", 20.),
    ];
    let index =
        CandidateIndex::new(&raw).expect("source fixture IDs and usable geometry must be valid");
    assert_eq!(index.candidates().len(), 2);
    assert_ne!(index.candidates()[0].id, index.candidates()[1].id);
    assert_eq!(
        index
            .text(&index.candidates()[0])
            .expect("canonical candidate must resolve its original text"),
        "日本語"
    );
    assert!(index.scoped(&[1], 1).is_err());
    assert!(
        index
            .scoped(&[2], 1)
            .expect("missing page must return an empty selection")
            .is_empty()
    );
    assert!(
        index
            .raw_fragments(&[SourceSpan {
                fragment_id: "a".into(),
                start: 1,
                end: 3
            }])
            .is_err()
    );
    assert!(
        index
            .in_box(
                1,
                Rect::new(5., 0., 15., 10.)
                    .expect("test rectangle must have positive finite extent")
            )
            .is_err()
    );
    assert_eq!(
        index
            .in_box(
                1,
                Rect::new(0., 0., 10., 10.)
                    .expect("test rectangle must have positive finite extent")
            )
            .expect("box containing the complete source run must resolve")
            .len(),
        1
    );
}
#[test]
fn suspect_and_unknown_source_are_not_candidates() {
    let mut raw = vec![fragment("a", 1, "A", 0.), fragment("b", 1, "B", 20.)];
    raw[0].quality = Quality::Unknown;
    raw[1].quality = Quality::Suspect;
    assert!(
        CandidateIndex::new(&raw)
            .expect("source fixture IDs and usable geometry must be valid")
            .candidates()
            .is_empty()
    );
}
#[test]
fn only_radical_blocks_are_unified() {
    assert_eq!(
        validate::unify_cjk_radicals("\u{2F08}\u{2F64}\u{2E9F}\u{2EF3}"),
        "\u{4EBA}\u{7528}\u{6BCD}\u{9F9F}"
    );
    assert_eq!(validate::unify_cjk_radicals("\u{2FD5}"), "\u{9FA0}");
    let kept = "\u{2E80}\u{2FD6}\u{2FDF}\u{2FF0}\u{FF21}\u{2460}\u{3231}\u{F900}人";
    assert_eq!(validate::unify_cjk_radicals(kept), kept);
}

#[test]
fn unset_radical_unification_is_omitted_from_serialization() {
    let field = json!({"id":"a","label":"A","output_type":"string","questions":[
        {"id":"q","type":"noul","instructions":"Present?","input":{"modality":"text"}}],
        "resolver":{"kind":"noul_to_boolean","question_id":"q"}});
    let parsed: voltfill_core::schema::FieldDefinition = serde_json::from_value(field.clone())
        .expect("field fixture must deserialize with radical folding disabled");
    assert!(!parsed.unify_cjk_radicals);
    assert!(
        serde_json::to_value(&parsed)
            .expect("field definition must serialize")
            .get("unify_cjk_radicals")
            .is_none()
    );
    let mut on = field;
    on["unify_cjk_radicals"] = json!(true);
    let parsed: voltfill_core::schema::FieldDefinition =
        serde_json::from_value(on).expect("explicit radical folding option must deserialize");
    assert!(parsed.unify_cjk_radicals);
    assert_eq!(
        serde_json::to_value(&parsed).expect("field definition must serialize")["unify_cjk_radicals"],
        true
    );
}

#[test]
fn normalization_does_not_change_original_and_date_decimal_are_not_guessed() {
    let raw = "  日本語\n  A ";
    assert_eq!(
        validate::normalize(raw, Normalization::CollapseWhitespace),
        "日本語 A"
    );
    assert_eq!(raw, "  日本語\n  A ");
    for good in ["2024-02-29", "2000-02-29", "2026-12-31"] {
        assert!(validate::validate(&json!(good), OutputType::Date, &[]).is_ok());
    }
    for bad in [
        "2025-02-29",
        "1900-02-29",
        "2026-13-01",
        "10/11/2026",
        "0000-01-01",
        "2026-+1-01",
        "+026-01-01",
        "2026-01-+1",
    ] {
        assert!(validate::validate(&json!(bad), OutputType::Date, &[]).is_err());
    }
    for good in ["0", "-12.50", "123456789012345678901234567890.123456789"] {
        assert!(validate::validate(&json!(good), OutputType::Decimal, &[]).is_ok());
    }
    for bad in ["01", "1,000", "1e3", "NaN", ".1", "1.", "+2"] {
        assert!(validate::validate(&json!(bad), OutputType::Decimal, &[]).is_err());
    }
    assert!(validate::validate(&json!(""), OutputType::String, &[Validator::NonEmpty]).is_err());
    assert!(
        validate::validate(
            &json!("日本語"),
            OutputType::String,
            &[Validator::MaxChars { count: 2 }]
        )
        .is_err()
    );
}

#[test]
fn evidence_ids_include_field_scope() {
    let raw = [fragment("same", 1, "value", 0.)];
    let index =
        CandidateIndex::new(&raw).expect("source fixture IDs and usable geometry must be valid");
    let c = &index.candidates()[0];
    let a = index
        .evidence(c, "field_a", "q", voltfill_core::model::Strategy::TextFirst)
        .expect("canonical candidate must produce field-scoped Evidence");
    let b = index
        .evidence(c, "field_b", "q", voltfill_core::model::Strategy::TextFirst)
        .expect("canonical candidate must produce field-scoped Evidence");
    assert_ne!(a.id, b.id);
    assert_eq!(a.question_id, b.question_id);
    let a = index
        .evidence(c, "a_b", "c", voltfill_core::model::Strategy::TextFirst)
        .expect("canonical candidate must produce field-scoped Evidence");
    let b = index
        .evidence(c, "a", "b_c", voltfill_core::model::Strategy::TextFirst)
        .expect("canonical candidate must produce field-scoped Evidence");
    assert_ne!(a.id, b.id);
}

#[test]
fn forged_candidate_page_bbox_and_source_quality_are_rejected() {
    let raw = [fragment("a", 1, "source", 0.)];
    let index =
        CandidateIndex::new(&raw).expect("source fixture IDs and usable geometry must be valid");
    let canonical = &index.candidates()[0];
    let mut forged = canonical.clone();
    forged.page = 999;
    assert!(
        index
            .evidence(&forged, "f", "q", voltfill_core::model::Strategy::TextFirst)
            .is_err()
    );
    let mut forged = canonical.clone();
    forged.bbox =
        Rect::new(20., 20., 30., 30.).expect("test rectangle must have positive finite extent");
    assert!(
        index
            .evidence(&forged, "f", "q", voltfill_core::model::Strategy::TextFirst)
            .is_err()
    );
    for id in ["a", "c_missing", "c_c_a"] {
        let mut forged = canonical.clone();
        forged.id = id.into();
        assert!(
            index
                .evidence(&forged, "f", "q", voltfill_core::model::Strategy::TextFirst)
                .is_err()
        );
    }
    let mut forged = canonical.clone();
    forged.spans[0].end -= 1;
    assert!(
        index
            .evidence(&forged, "f", "q", voltfill_core::model::Strategy::TextFirst)
            .is_err()
    );
    let mut suspect = raw[0].clone();
    suspect.quality = Quality::Suspect;
    let other = CandidateIndex::new(std::slice::from_ref(&suspect))
        .expect("suspect source must be excluded without invalidating the index");
    assert!(
        other
            .evidence(
                canonical,
                "f",
                "q",
                voltfill_core::model::Strategy::TextFirst
            )
            .is_err()
    );
}

#[test]
fn string_arrays_validate_every_element_without_truncating_or_joining() {
    let raw = [" 日本語 ", " A\n B ", " 日本語 "];
    let normalized = json!(
        raw.iter()
            .map(|s| validate::normalize(s, Normalization::CollapseWhitespace))
            .collect::<Vec<_>>()
    );
    assert_eq!(normalized, json!(["日本語", "A B", "日本語"]));
    assert_eq!(raw, [" 日本語 ", " A\n B ", " 日本語 "]);
    let rules = [Validator::NonEmpty, Validator::MaxChars { count: 3 }];
    assert!(validate::validate(&normalized, OutputType::StringArray, &rules).is_ok());
    assert_eq!(
        normalized
            .as_array()
            .expect("array normalization must retain the array shape")
            .len(),
        3
    );
    for value in [
        json!([]),
        json!([""]),
        json!(["ok", " \n"]),
        json!(["ok", "長すぎる値"]),
    ] {
        assert!(validate::validate(&value, OutputType::StringArray, &rules).is_err());
    }
    for value in [
        json!("text"),
        json!(["ok", 1]),
        json!([null]),
        json!([["nested"]]),
    ] {
        assert_eq!(
            validate::value_type(&value, OutputType::StringArray),
            Err(validate::ValidationError::Type)
        );
    }
    assert!(validate::validate(&json!([]), OutputType::StringArray, &[]).is_ok());
    assert!(validate::value_type(&json!(["text"]), OutputType::String).is_err());
    assert!(validate::value_type(&json!("text"), OutputType::String).is_ok());
}
