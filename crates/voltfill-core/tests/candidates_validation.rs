use serde_json::json;
use voltfill_core::{
    candidates::CandidateIndex,
    geometry::Rect,
    model::{Quality, SourceFragment, SourceSpan},
    schema::{Normalization, OutputType, Validator},
    validate,
};
fn fragment(id: &str, page: u32, text: &str, x: f64) -> SourceFragment {
    SourceFragment {
        id: id.into(),
        page,
        item_index: 0,
        raw_text: text.into(),
        bbox: Rect::new(x, 0., x + 10., 10.).unwrap(),
        quality: Quality::Usable,
    }
}
#[test]
fn identical_values_at_distinct_locations_keep_provenance_and_overflow_is_not_truncation() {
    let raw = [
        fragment("a", 1, "日本語", 0.),
        fragment("b", 1, "日本語", 20.),
    ];
    let index = CandidateIndex::new(&raw).unwrap();
    assert_eq!(index.candidates().len(), 2);
    assert_ne!(index.candidates()[0].id, index.candidates()[1].id);
    assert_eq!(index.text(&index.candidates()[0]).unwrap(), "日本語");
    assert!(index.scoped(&[1], 1).is_err());
    assert!(index.scoped(&[2], 1).unwrap().is_empty());
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
            .in_box(1, Rect::new(5., 0., 15., 10.).unwrap())
            .is_err()
    );
    assert_eq!(
        index
            .in_box(1, Rect::new(0., 0., 10., 10.).unwrap())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn suspect_and_unknown_source_are_not_candidates() {
    let mut raw = vec![fragment("a", 1, "A", 0.), fragment("b", 1, "B", 20.)];
    raw[0].quality = Quality::Unknown;
    raw[1].quality = Quality::Suspect;
    assert!(CandidateIndex::new(&raw).unwrap().candidates().is_empty());
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
    let index = CandidateIndex::new(&raw).unwrap();
    let c = &index.candidates()[0];
    let a = index
        .evidence(c, "field_a", "q", voltfill_core::model::Strategy::TextFirst)
        .unwrap();
    let b = index
        .evidence(c, "field_b", "q", voltfill_core::model::Strategy::TextFirst)
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.question_id, b.question_id);
    let a = index
        .evidence(c, "a_b", "c", voltfill_core::model::Strategy::TextFirst)
        .unwrap();
    let b = index
        .evidence(c, "a", "b_c", voltfill_core::model::Strategy::TextFirst)
        .unwrap();
    assert_ne!(a.id, b.id);
}

#[test]
fn forged_candidate_page_bbox_and_source_quality_are_rejected() {
    let raw = [fragment("a", 1, "source", 0.)];
    let index = CandidateIndex::new(&raw).unwrap();
    let canonical = &index.candidates()[0];
    let mut forged = canonical.clone();
    forged.page = 999;
    assert!(
        index
            .evidence(&forged, "f", "q", voltfill_core::model::Strategy::TextFirst)
            .is_err()
    );
    let mut forged = canonical.clone();
    forged.bbox = Rect::new(20., 20., 30., 30.).unwrap();
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
    let other = CandidateIndex::new(std::slice::from_ref(&suspect)).unwrap();
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
    assert_eq!(normalized.as_array().unwrap().len(), 3);
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
