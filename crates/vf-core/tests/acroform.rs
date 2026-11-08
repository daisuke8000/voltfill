//! AcroForm fill over synthetic, offline PDFs: text runs from the eval
//! builder plus AcroForm fields added with lopdf.
#[allow(dead_code)]
#[path = "eval/pdf.rs"]
mod pdf;

use lopdf::content::{Content, Operation};
use lopdf::{
    Dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, StringFormat,
    dictionary,
};
use pdf::{PAGE_HEIGHT, Run};
use serde_json::{Value, json};
use vf_core::acroform::{
    AcroFormHold, AcroFormSummary, METHOD, MatchedBy, Unavailable, fill_with_acroform,
};
use vf_core::fill::{self, FieldFill, FillError, FillLimits, FillResult, FillStatus, Form};
use vf_source::acroform::{
    AcroFormError, AcroFormLimit, AcroFormLimits, FieldIssue, FieldKind, XfaPresence, fields,
};
use vf_source::geometry::Rect;
use vf_source::layout::{Placement, Precision};
use vf_source::{Orientation, PdfLimits, Quality, inspect_with_acroform};

/// A widget box in Display coordinates: left, top, right, bottom.
const BOX: [f64; 4] = [300.0, 85.0, 450.0, 105.0];

fn run(x: f64, y: f64, text: &str) -> Run {
    Run {
        id: None,
        page: 1,
        x: Some(x),
        right: None,
        y,
        size: 10.0,
        text: text.into(),
    }
}

fn utf16(text: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

fn literal(bytes: &[u8]) -> Object {
    Object::String(bytes.to_vec(), StringFormat::Literal)
}

/// A Display box as a PDF `/Rect`.
fn pdf_rect([left, top, right, bottom]: [f64; 4]) -> Object {
    vec![
        Object::Real(left as f32),
        Object::Real((PAGE_HEIGHT - bottom) as f32),
        Object::Real(right as f32),
        Object::Real((PAGE_HEIGHT - top) as f32),
    ]
    .into()
}

/// A terminal field merged with its widget at `BOX`.
fn field(name: &str, kind: &str, extra: Dictionary) -> Dictionary {
    let mut dict = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "T" => utf16(name),
        "FT" => kind,
        "Rect" => pdf_rect(BOX),
    };
    for (key, value) in extra.iter() {
        dict.set(key.clone(), value.clone());
    }
    dict
}

fn text_field(name: &str, value: &str) -> Dictionary {
    field(name, "Tx", dictionary! { "V" => utf16(value) })
}

/// Add top-level `fields` (listed in page 1's /Annots unless `/NoAnnot`) and
/// an /AcroForm with `form` entries to a PDF of `runs`.
fn build(runs: &[Run], fields: Vec<Dictionary>, form: Dictionary) -> Vec<u8> {
    let doc = Document::load_mem(&pdf::build(runs)).expect("load the synthetic PDF");
    let mut out = Vec::new();
    with_form(doc, fields, form)
        .save_to(&mut out)
        .expect("save the synthetic PDF");
    out
}

/// [`build`] on a loaded document, returned unsaved.
fn with_form(mut doc: Document, fields: Vec<Dictionary>, form: Dictionary) -> Document {
    let page = *doc.get_pages().get(&1).expect("page 1");
    let mut annots = Vec::new();
    let mut ids = Vec::new();
    for mut dict in fields {
        let listed = dict.remove(b"NoAnnot").is_none();
        let id = doc.add_object(dict);
        if listed {
            annots.push(Object::Reference(id));
        }
        ids.push(Object::Reference(id));
    }
    doc.get_object_mut(page)
        .and_then(Object::as_dict_mut)
        .expect("page dictionary")
        .set("Annots", annots);
    let mut acroform = dictionary! { "Fields" => ids };
    for (key, value) in form.iter() {
        acroform.set(key.clone(), value.clone());
    }
    let acroform = doc.add_object(acroform);
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("catalog reference");
    doc.get_object_mut(root)
        .and_then(Object::as_dict_mut)
        .expect("catalog")
        .set("AcroForm", acroform);
    doc
}

fn form(fields: Value) -> Form {
    fill::parse_form(&serde_json::to_vec(&json!({ "fields": fields })).expect("form JSON"))
        .expect("valid form")
}

fn label(id: &str, label: &str, kind: &str) -> Form {
    form(json!([{ "id": id, "label": label, "type": kind }]))
}

fn run_fill(bytes: &[u8], form: &Form) -> FillResult {
    let (snapshot, acroform) =
        inspect_with_acroform(bytes, PdfLimits::default()).expect("inspect the synthetic PDF");
    fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        form,
        &FillLimits::default(),
    )
    .expect("the small document stays within the fill limits")
}

fn one(bytes: &[u8], form: &Form) -> FieldFill {
    let result = run_fill(bytes, form);
    result.fields.into_values().next().expect("one field")
}

fn reasons(fill: &FieldFill) -> Vec<AcroFormHold> {
    assert_eq!(fill.acroform.len(), 1, "{fill:?}");
    fill.acroform[0].reasons.clone()
}

fn assert_not_filled(fill: &FieldFill) {
    assert_ne!(fill.status, FillStatus::Filled, "{fill:?}");
    assert_eq!(fill.raw, None);
    assert_eq!(fill.value, None);
    assert_eq!(fill.method, None);
}

#[test]
fn exact_last_partial_name_commits_the_value_with_widget_evidence() {
    let bytes = pdf::build(&[]);
    let mut doc = Document::load_mem(&bytes).expect("load");
    let page = *doc.get_pages().get(&1).expect("page 1");
    let parent = doc.new_object_id();
    let mut child = text_field("請求番号", "INV-001");
    child.set("Parent", parent);
    let child = doc.add_object(child);
    doc.objects.insert(
        parent,
        Object::Dictionary(
            dictionary! { "T" => literal(b"invoice"), "Kids" => vec![child.into()] },
        ),
    );
    doc.get_object_mut(page)
        .and_then(Object::as_dict_mut)
        .expect("page")
        .set("Annots", vec![Object::Reference(child)]);
    let acroform = doc.add_object(dictionary! { "Fields" => vec![parent.into()] });
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .expect("root");
    doc.get_object_mut(root)
        .and_then(Object::as_dict_mut)
        .expect("catalog")
        .set("AcroForm", acroform);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save");

    let result = run_fill(&bytes, &label("number", "請求番号", "text"));
    assert_eq!(
        result.acroform,
        Some(AcroFormSummary::Read {
            fields: 1,
            xfa: XfaPresence::Absent
        })
    );
    let fill = &result.fields["number"];
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.method, Some(METHOD));
    assert_eq!(fill.raw.as_deref(), Some("INV-001"));
    assert_eq!(fill.value, Some(json!("INV-001")));
    assert_eq!(fill.evidence.len(), 1);
    let evidence = &fill.evidence[0];
    assert_eq!(evidence.page, 1);
    assert_eq!(evidence.precision, Precision::Widget);
    assert_eq!(evidence.acroform_field.as_deref(), Some("invoice.請求番号"));
    assert_eq!(evidence.fragment_id, "");
    assert_eq!((evidence.start, evidence.end), (0, 7));
    let Rect {
        left,
        top,
        right,
        bottom,
    } = evidence.bbox;
    for (got, want) in [left, top, right, bottom].into_iter().zip(BOX) {
        assert!((got - want).abs() < 0.05, "{:?}", evidence.bbox);
    }
    assert_eq!(fill.acroform[0].field, "invoice.請求番号");
    assert_eq!(fill.acroform[0].matched_by, MatchedBy::Name);
    assert!(fill.acroform[0].reasons.is_empty());

    let json = serde_json::to_value(&result).expect("serialize");
    assert_eq!(
        json["acroform"],
        json!({"status": "read", "fields": 1, "xfa": "absent"})
    );
    assert_eq!(json["fields"]["number"]["method"], "acroform");
    assert_eq!(
        json["fields"]["number"]["evidence"][0]["precision"],
        "widget"
    );
    assert_eq!(
        json["fields"]["number"]["evidence"][0]["acroform_field"],
        "invoice.請求番号"
    );
}

#[test]
fn alternate_name_matches_and_typed_values_are_normalized() {
    let mut date = text_field("f1", "2026/11/08");
    date.set("TU", utf16("発行日"));
    let bytes = build(&[], vec![date], dictionary! {});
    let fill = one(&bytes, &label("date", "発行日", "date"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("2026/11/08"));
    assert_eq!(fill.value, Some(json!("2026-11-08")));
    assert_eq!(fill.acroform[0].matched_by, MatchedBy::AlternateName);

    // Hints match too, and amounts are normalized.
    let bytes = build(&[], vec![text_field("合計", "¥12,100")], dictionary! {});
    let form =
        form(json!([{ "id": "total", "label": "請求額", "type": "amount", "hints": ["合計"] }]));
    let fill = one(&bytes, &form);
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.value, Some(json!(12100)));
}

#[test]
fn a_label_that_does_not_match_exactly_is_not_a_match() {
    for name in ["請求日", "請求番号等", "請求番号（旧）"] {
        let bytes = build(&[], vec![text_field(name, "INV-001")], dictionary! {});
        let fill = one(&bytes, &label("number", "請求番号", "text"));
        assert_eq!(fill.status, FillStatus::NoText, "{name}");
        assert!(fill.acroform.is_empty(), "{name}");
        let json = serde_json::to_value(&fill).expect("serialize");
        assert!(json.get("acroform").is_none(), "{name}");
    }
}

#[test]
fn absent_empty_and_default_only_values_are_never_committed() {
    let cases = [
        (text_field("氏名", ""), AcroFormHold::ValueEmpty),
        (text_field("氏名", " \u{3000}"), AcroFormHold::ValueEmpty),
        (
            field("氏名", "Tx", dictionary! {}),
            AcroFormHold::ValueAbsent,
        ),
        (
            field("氏名", "Tx", dictionary! { "DV" => utf16("山田太郎") }),
            AcroFormHold::ValueAbsent,
        ),
    ];
    for (dict, reason) in cases {
        let bytes = build(&[], vec![dict], dictionary! {});
        let fill = one(&bytes, &label("name", "氏名", "text"));
        assert_not_filled(&fill);
        assert_eq!(reasons(&fill), [reason]);
    }
}

#[test]
fn pdf_doc_values_with_high_bytes_are_held_as_possible_mojibake() {
    // Shift_JIS "山田" read as PDFDocEncoding.
    let dict = field("氏名", "Tx", dictionary! { "V" => literal(b"\x8eR\x93c") });
    let bytes = build(&[], vec![dict], dictionary! {});
    let fill = one(&bytes, &label("name", "氏名", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::FieldIssues]);
    assert_eq!(fill.acroform[0].issues, [FieldIssue::PdfDocHighBytes]);

    // ASCII PDFDocEncoding is fine.
    let dict = field("氏名", "Tx", dictionary! { "V" => literal(b"Taro") });
    let bytes = build(&[], vec![dict], dictionary! {});
    assert_eq!(
        one(&bytes, &label("name", "氏名", "text")).status,
        FillStatus::Filled
    );
}

#[test]
fn hidden_unplaced_and_missing_widgets_are_held() {
    let hidden = field("氏名", "Tx", dictionary! { "V" => utf16("山田"), "F" => 2 });
    let unplaced = field(
        "氏名",
        "Tx",
        dictionary! { "V" => utf16("山田"), "NoAnnot" => true },
    );
    let bare = dictionary! { "T" => utf16("氏名"), "FT" => "Tx", "V" => utf16("山田") };
    for (dict, reason) in [
        (hidden, AcroFormHold::HiddenWidgets),
        (unplaced, AcroFormHold::WidgetWithoutRect),
        (bare, AcroFormHold::NoWidget),
    ] {
        let bytes = build(&[], vec![dict], dictionary! {});
        let fill = one(&bytes, &label("name", "氏名", "text"));
        assert_not_filled(&fill);
        assert_eq!(reasons(&fill), [reason]);
    }
}

#[test]
fn hybrid_xfa_forms_are_never_committed() {
    let bytes = build(
        &[],
        vec![text_field("氏名", "山田")],
        dictionary! { "XFA" => Object::Array(vec![]) },
    );
    let result = run_fill(&bytes, &label("name", "氏名", "text"));
    assert_eq!(
        result.acroform,
        Some(AcroFormSummary::Read {
            fields: 1,
            xfa: XfaPresence::Hybrid
        })
    );
    let fill = &result.fields["name"];
    assert_not_filled(fill);
    assert_eq!(reasons(fill), [AcroFormHold::XfaPresent]);
}

#[test]
fn several_matching_fields_are_offered_as_candidates_but_never_committed() {
    let mut first = text_field("a", "山田太郎");
    first.set("TU", utf16("氏名"));
    let mut second = text_field("b", "佐藤花子");
    second.set("TU", utf16("氏名"));
    let bytes = build(&[], vec![first, second], dictionary! {});
    let fill = one(&bytes, &label("name", "氏名", "text"));
    assert_not_filled(&fill);
    assert_eq!(fill.status, FillStatus::Ambiguous);
    assert_eq!(fill.acroform.len(), 2);
    assert!(
        fill.acroform
            .iter()
            .all(|m| m.reasons == [AcroFormHold::SeveralFields])
    );
    let raws: Vec<&str> = fill.candidates.iter().map(|c| c.raw.as_str()).collect();
    assert_eq!(raws, ["山田太郎", "佐藤花子"]);
    // Candidates carry the fully qualified name and /TU, so assist can tell
    // the fields apart.
    let labels: Vec<&str> = fill.candidates.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["a / 氏名", "b / 氏名"]);
    assert!(
        fill.candidates
            .iter()
            .all(|c| c.placement == Placement::Widget
                && c.holds.is_empty()
                && c.evidence[0].precision == Precision::Widget)
    );

    // Even when they agree, two fields never fill one value.
    let bytes = build(
        &[],
        vec![text_field("氏名", "山田"), {
            let mut d = text_field("x", "山田");
            d.set("TU", utf16("氏名"));
            d
        }],
        dictionary! {},
    );
    assert_eq!(
        one(&bytes, &label("name", "氏名", "text")).status,
        FillStatus::Ambiguous
    );
}

#[test]
fn text_layer_text_inside_the_widget_must_read_as_the_value() {
    // A stale appearance flattened into the page: the box shows another value.
    let stale = build(
        &[run(305.0, 100.0, "INV-000")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let fill = one(&stale, &label("number", "請求番号", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::AppearanceMismatch]);

    // The same text inside the box (up to NFKC and whitespace) is consistent.
    let shown = build(
        &[run(305.0, 100.0, "ＩＮＶ－００１")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let fill = one(&shown, &label("number", "請求番号", "text"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("INV-001"));
}

#[test]
fn an_agreeing_text_layer_value_adds_its_evidence() {
    let bytes = build(
        &[run(72.0, 200.0, "請求番号：INV-001")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("number", "請求番号", "text"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.method, Some(METHOD));
    assert_eq!(fill.raw.as_deref(), Some("INV-001"));
    assert_eq!(fill.evidence.len(), 2);
    assert_eq!(fill.evidence[0].precision, Precision::Widget);
    assert_eq!(fill.evidence[1].acroform_field, None);
    assert_eq!(fill.evidence[1].text, "INV-001");
    assert_ne!(fill.evidence[1].fragment_id, "");

    // Typed values agree by their normalized value.
    let bytes = build(
        &[run(72.0, 200.0, "合計：12,100円")],
        vec![text_field("合計", "¥12,100")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("total", "合計", "amount"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("¥12,100"));
    assert_eq!(fill.value, Some(json!(12100)));
    assert_eq!(fill.evidence.len(), 2);
}

#[test]
fn a_disagreeing_text_layer_value_leaves_the_field_ambiguous() {
    let bytes = build(
        &[run(72.0, 200.0, "請求番号：INV-002")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("number", "請求番号", "text"));
    assert_not_filled(&fill);
    assert_eq!(fill.status, FillStatus::Ambiguous);
    assert!(fill.evidence.is_empty() && fill.candidates.is_empty());
    assert_eq!(reasons(&fill), [AcroFormHold::TextLayerConflict]);
    assert_eq!(fill.acroform[0].raw.as_deref(), Some("INV-001"));
}

#[test]
fn an_acroform_value_fills_a_field_the_text_layer_left_open() {
    // Two text-layer values disagree; the one named field decides.
    let bytes = build(
        &[
            run(72.0, 200.0, "氏名：山田太郎"),
            run(72.0, 300.0, "氏名：佐藤花子"),
        ],
        vec![text_field("氏名", "山田太郎")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("name", "氏名", "text"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("山田太郎"));
    assert_eq!(fill.evidence.len(), 1);
    assert!(fill.candidates.is_empty());
}

#[test]
fn typed_values_must_match_the_grammar_and_carry_no_scale() {
    let bytes = build(&[], vec![text_field("合計", "未定")], dictionary! {});
    let fill = one(&bytes, &label("total", "合計", "amount"));
    assert_eq!(fill.status, FillStatus::Invalid);
    assert_eq!(fill.raw.as_deref(), Some("未定"));
    assert_eq!(fill.value, None);
    assert_eq!(fill.evidence[0].precision, Precision::Widget);
    assert_eq!(reasons(&fill), [AcroFormHold::InvalidValue]);

    // A unit note anywhere on the document may scale the value.
    let bytes = build(
        &[run(72.0, 400.0, "（単位：千円）")],
        vec![text_field("合計", "12,100")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("total", "合計", "amount"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::ScaledUnit]);

    // So may the field's alternate name.
    let mut dict = text_field("sales", "12,100");
    dict.set("TU", utf16("売上高（千円）"));
    let bytes = build(&[], vec![dict], dictionary! {});
    let form =
        form(json!([{ "id": "s", "label": "売上高", "type": "number", "hints": ["sales"] }]));
    let fill = one(&bytes, &form);
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::ScaledUnit]);

    // Multi-line values are held.
    let bytes = build(
        &[],
        vec![text_field("住所", "東京都\n千代田区")],
        dictionary! {},
    );
    let fill = one(&bytes, &label("address", "住所", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::MultiLine]);
}

#[test]
fn choices_fill_text_fields_with_the_export_value() {
    let opt = vec![
        Object::Array(vec![utf16("13"), utf16("東京都")]),
        Object::Array(vec![utf16("大阪府"), utf16("大阪府")]),
        utf16("北海道"),
    ];
    let choice = |value: &str| {
        field(
            "都道府県",
            "Ch",
            dictionary! { "V" => utf16(value), "Opt" => opt.clone() },
        )
    };
    // An export code that differs from the display text is never committed.
    let bytes = build(&[], vec![choice("13")], dictionary! {});
    let fill = one(&bytes, &label("pref", "都道府県", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::ExportDiffersFromDisplay]);
    assert_eq!(fill.acroform[0].raw.as_deref(), Some("13"));
    assert_eq!(fill.acroform[0].display.as_deref(), Some("東京都"));
    let json = serde_json::to_value(&fill).expect("serialize");
    assert_eq!(json["acroform"][0]["display"], json!("東京都"));
    assert_eq!(
        json["acroform"][0]["reasons"],
        json!(["export_differs_from_display"])
    );

    // An export value equal to its display text, or a plain option, commits.
    for value in ["大阪府", "北海道"] {
        let bytes = build(&[], vec![choice(value)], dictionary! {});
        let fill = one(&bytes, &label("pref", "都道府県", "text"));
        assert_eq!(fill.status, FillStatus::Filled, "{value}");
        assert_eq!(fill.raw.as_deref(), Some(value));
        assert_eq!(fill.acroform[0].kind, FieldKind::Choice);
    }
    let bytes = build(&[], vec![choice("大阪府")], dictionary! {});

    // A choice never fills a typed field, and two selections are held.
    let fill = one(&bytes, &label("pref", "都道府県", "number"));
    assert_eq!(reasons(&fill), [AcroFormHold::KindUnsupported]);
    let several = field(
        "都道府県",
        "Ch",
        dictionary! { "V" => vec![utf16("a"), utf16("b")], "Ff" => 1 << 21 },
    );
    let bytes = build(&[], vec![several], dictionary! {});
    let fill = one(&bytes, &label("pref", "都道府県", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::SeveralValues]);
}

#[test]
fn checkboxes_have_no_form_type_and_are_skipped() {
    // Only the appearance state names matter here.
    let appearance = dictionary! { "N" => dictionary! { "Yes" => 0, "Off" => 0 } };
    let dict = field(
        "同意",
        "Btn",
        dictionary! { "V" => Object::Name(b"Yes".to_vec()), "AP" => appearance },
    );
    let bytes = build(&[], vec![dict], dictionary! {});
    let fill = one(&bytes, &label("agree", "同意", "text"));
    assert_not_filled(&fill);
    assert_eq!(fill.acroform[0].kind, FieldKind::Checkbox);
    assert_eq!(fill.acroform[0].raw.as_deref(), Some("Yes"));
    assert_eq!(reasons(&fill), [AcroFormHold::KindUnsupported]);
}

#[test]
fn unreadable_acroforms_fall_back_to_the_text_layer() {
    let bytes = build(
        &[run(72.0, 200.0, "請求番号：INV-002")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let form = label("number", "請求番号", "text");
    let (snapshot, _) = inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    let text_only = fill::fill_with_limits(
        &snapshot.document_id,
        &snapshot.fragments,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    let limited = fields(
        &bytes,
        AcroFormLimits {
            max_text_bytes: 4,
            ..AcroFormLimits::default()
        },
    );
    assert_eq!(
        limited,
        Err(AcroFormError::LimitExceeded(AcroFormLimit::TextBytes))
    );
    for (acroform, summary) in [
        (
            limited,
            json!({"status": "unavailable", "reason": "limit_exceeded", "limit": "text_bytes"}),
        ),
        (
            Err(AcroFormError::Encrypted),
            json!({"status": "unavailable", "reason": "encrypted"}),
        ),
        (
            Err(AcroFormError::InvalidFieldTree),
            json!({"status": "unavailable", "reason": "invalid_field_tree"}),
        ),
    ] {
        let result = fill_with_acroform(
            &snapshot.document_id,
            &snapshot.fragments,
            &acroform,
            &form,
            &FillLimits::default(),
        )
        .expect("fill");
        assert_eq!(result.fields, text_only.fields);
        assert_eq!(result.fields["number"].raw.as_deref(), Some("INV-002"));
        let json = serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["acroform"], summary);
    }
    let encrypted = fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &Err(AcroFormError::Encrypted),
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    assert_eq!(
        encrypted.acroform,
        Some(AcroFormSummary::Unavailable {
            reason: Unavailable::Encrypted,
            limit: None
        })
    );
}

#[test]
fn table_fields_are_never_matched_or_filled_from_the_acroform() {
    let runs = [
        run(60.0, 200.0, "品名"),
        run(300.0, 200.0, "金額"),
        run(60.0, 218.0, "りんご"),
        run(300.0, 218.0, "¥300"),
        run(60.0, 236.0, "みかん"),
        run(300.0, 236.0, "¥400"),
    ];
    let form = form(json!([
        {
            "id": "items", "label": "明細", "type": "table",
            "columns": [
                { "id": "name", "label": "品名" },
                { "id": "amount", "label": "金額", "type": "amount" },
            ],
        },
        { "id": "number", "label": "請求番号" },
    ]));
    // AcroForm fields named after the table and its columns, and one scalar.
    let bytes = build(
        &runs,
        vec![
            text_field("明細", "りんご"),
            text_field("品名", "りんご"),
            text_field("金額", "¥300"),
            text_field("請求番号", "INV-001"),
        ],
        dictionary! {},
    );
    let with = run_fill(&bytes, &form);
    let (snapshot, _) = inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    let text_only = fill::fill_with_limits(
        &snapshot.document_id,
        &snapshot.fragments,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    let items = &with.fields["items"];
    assert_eq!(items.status, FillStatus::Filled, "{items:?}");
    assert_ne!(items.method, Some(METHOD));
    assert!(items.acroform.is_empty());
    assert_eq!(items.rows.as_ref().map(Vec::len), Some(2));
    assert_eq!(items, &text_only.fields["items"]);
    assert_eq!(
        serde_json::to_value(items).expect("serialize"),
        serde_json::to_value(&text_only.fields["items"]).expect("serialize")
    );
    // Scalar fields of the same form still use the AcroForm.
    let number = &with.fields["number"];
    assert_eq!(number.status, FillStatus::Filled, "{number:?}");
    assert_eq!(number.method, Some(METHOD));
    assert_eq!(number.raw.as_deref(), Some("INV-001"));
    assert!(with.acroform.is_some());
}

#[test]
fn documents_without_an_acroform_serialize_exactly_as_text_layer_fill() {
    let bytes = pdf::build(&[
        run(72.0, 200.0, "請求番号：INV-002"),
        run(72.0, 300.0, "氏名 山田"),
        run(60.0, 400.0, "品名"),
        run(300.0, 400.0, "金額"),
        run(60.0, 418.0, "りんご"),
        run(300.0, 418.0, "¥300"),
        run(60.0, 436.0, "みかん"),
        run(300.0, 436.0, "¥400"),
    ]);
    let form = form(json!([
        { "id": "number", "label": "請求番号" },
        { "id": "name", "label": "氏名" },
        {
            "id": "items", "label": "明細", "type": "table",
            "columns": [
                { "id": "name", "label": "品名" },
                { "id": "amount", "label": "金額", "type": "amount" },
            ],
        },
    ]));
    let (snapshot, acroform) =
        inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    assert!(!acroform.as_ref().expect("read").present);
    let text_only = fill::fill_with_limits(
        &snapshot.document_id,
        &snapshot.fragments,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    let with = fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    assert_eq!(with.fields["items"].status, FillStatus::Filled);
    assert_eq!(with.fields["items"].rows.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        serde_json::to_vec(&with).expect("serialize"),
        serde_json::to_vec(&text_only).expect("serialize")
    );
}

#[test]
fn acroform_matching_spends_the_fill_budget() {
    let bytes = build(&[], vec![text_field("請求番号", "INV-001")], dictionary! {});
    let (snapshot, acroform) =
        inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    let result = fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        &label("number", "請求番号", "text"),
        &FillLimits {
            max_work: 8,
            ..FillLimits::default()
        },
    );
    assert_eq!(result.err(), Some(FillError::BudgetExceeded));
    let result = fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        &label("number", "請求番号", "text"),
        &FillLimits {
            deadline: Some(std::time::Instant::now()),
            ..FillLimits::default()
        },
    );
    assert_eq!(result.err(), Some(FillError::DeadlineExceeded));
}

#[test]
fn password_fields_are_never_committed() {
    let stored = field(
        "暗証番号",
        "Tx",
        dictionary! { "V" => utf16("1234"), "Ff" => 1 << 13 },
    );
    let empty = field("暗証番号", "Tx", dictionary! { "Ff" => 1 << 13 });
    for (dict, reason, issues) in [
        (
            stored,
            AcroFormHold::FieldIssues,
            vec![FieldIssue::PasswordValueWithheld],
        ),
        (empty, AcroFormHold::ValueAbsent, Vec::new()),
    ] {
        let bytes = build(&[], vec![dict], dictionary! {});
        let fill = one(&bytes, &label("pin", "暗証番号", "text"));
        assert_not_filled(&fill);
        assert_eq!(reasons(&fill), [reason]);
        assert_eq!(fill.acroform[0].issues, issues);
        assert_eq!(fill.acroform[0].raw, None);
    }
}

fn named(tu: &str, name: &str, value: &str) -> Dictionary {
    let mut dict = text_field(name, value);
    dict.set("TU", utf16(tu));
    dict
}

#[test]
fn several_fields_keep_a_text_layer_value_only_when_all_agree() {
    let runs = [run(72.0, 200.0, "氏名：山田太郎")];
    let form = label("name", "氏名", "text");
    let differ = build(
        &runs,
        vec![
            named("氏名", "a", "佐藤花子"),
            named("氏名", "b", "山田太郎"),
        ],
        dictionary! {},
    );
    let fill = one(&differ, &form);
    assert_not_filled(&fill);
    assert_eq!(fill.status, FillStatus::Ambiguous);
    assert!(fill.candidates.is_empty() && fill.evidence.is_empty());

    let agree = build(
        &runs,
        vec![
            named("氏名", "a", "山田太郎"),
            named("氏名", "b", "山田 太郎"),
        ],
        dictionary! {},
    );
    let fill = one(&agree, &form);
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.method, Some("label_match"));
    assert_eq!(fill.raw.as_deref(), Some("山田太郎"));
    assert_eq!(fill.acroform.len(), 2);
}

#[test]
fn offered_acroform_candidates_come_before_text_layer_candidates() {
    let bytes = build(
        &[
            run(72.0, 200.0, "氏名：山田太郎"),
            run(72.0, 300.0, "氏名：佐藤花子"),
        ],
        vec![
            named("氏名", "a", "鈴木一郎"),
            named("氏名", "b", "田中次郎"),
        ],
        dictionary! {},
    );
    let fill = one(&bytes, &label("name", "氏名", "text"));
    assert_eq!(fill.status, FillStatus::Ambiguous);
    let found: Vec<(&str, Placement)> = fill
        .candidates
        .iter()
        .map(|c| (c.raw.as_str(), c.placement))
        .collect();
    assert_eq!(
        found[..2],
        [
            ("鈴木一郎", Placement::Widget),
            ("田中次郎", Placement::Widget)
        ]
    );
    assert_eq!(found.len(), 4);
    assert!(found[2..].iter().all(|(_, p)| *p != Placement::Widget));
}

/// `doc` encrypted (RC4, 128-bit) with an empty user password, which `lopdf`
/// decrypts while loading.
fn encrypted(mut doc: Document) -> Vec<u8> {
    let id = Object::String(b"0123456789abcdef".to_vec(), StringFormat::Literal);
    doc.trailer.set("ID", vec![id.clone(), id]);
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "",
        key_length: 128,
        permissions: Permissions::all(),
    })
    .expect("encryption state");
    doc.encrypt(&state).expect("encrypt");
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save");
    out
}

#[test]
fn encrypted_documents_without_an_acroform_serialize_exactly_as_text_layer_fill() {
    let runs = [run(72.0, 200.0, "請求番号：INV-002")];
    let doc = Document::load_mem(&pdf::build(&runs)).expect("load");
    let bytes = encrypted(doc);
    assert!(Document::load_mem(&bytes).expect("load").was_encrypted());
    let form = label("number", "請求番号", "text");
    let (snapshot, acroform) =
        inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    assert!(!acroform.as_ref().expect("no AcroForm error").present);
    let text_only = fill::fill_with_limits(
        &snapshot.document_id,
        &snapshot.fragments,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    let with = fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        &form,
        &FillLimits::default(),
    )
    .expect("fill");
    let json = serde_json::to_vec(&with).expect("serialize");
    assert_eq!(json, serde_json::to_vec(&text_only).expect("serialize"));
    assert!(
        serde_json::to_value(&with)
            .expect("json")
            .get("acroform")
            .is_none()
    );

    // With an AcroForm, the same encryption makes it unavailable.
    let doc = Document::load_mem(&pdf::build(&runs)).expect("load");
    let doc = with_form(doc, vec![text_field("請求番号", "INV-001")], dictionary! {});
    let result = run_fill(&encrypted(doc), &form);
    assert_eq!(
        result.acroform,
        Some(AcroFormSummary::Unavailable {
            reason: Unavailable::Encrypted,
            limit: None
        })
    );
    assert!(result.fields["number"].acroform.is_empty());
}

#[test]
fn a_field_whose_name_and_alternate_name_match_two_items_fills_neither() {
    let mut dict = text_field("発行日", "2026/11/08");
    dict.set("TU", utf16("支払期限"));
    let bytes = build(&[], vec![dict], dictionary! {});
    let both = form(json!([
        { "id": "issued", "label": "発行日", "type": "date" },
        { "id": "due", "label": "支払期限", "type": "date" },
    ]));
    let result = run_fill(&bytes, &both);
    for (id, by) in [
        ("issued", MatchedBy::Name),
        ("due", MatchedBy::AlternateName),
    ] {
        let fill = &result.fields[id];
        assert_not_filled(fill);
        assert_eq!(fill.acroform[0].matched_by, by, "{id}");
        assert_eq!(
            reasons(fill),
            [AcroFormHold::FieldMatchesSeveralItems],
            "{id}"
        );
        assert!(fill.candidates.is_empty(), "{id}");
    }
    let json = serde_json::to_value(&result).expect("serialize");
    assert_eq!(
        json["fields"]["due"]["acroform"][0]["reasons"],
        json!(["field_matches_several_items"])
    );

    // Matched by one form field only, the same field commits.
    let fill = one(&bytes, &label("issued", "発行日", "date"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.value, Some(json!("2026-11-08")));
}

#[test]
fn a_field_that_fails_a_rule_for_one_item_still_holds_the_other() {
    // The /TU item cannot take the date (invalid amount), yet the field
    // matches it, so the name item must not be filled from it either.
    let mut dict = text_field("発行日", "2026/11/08");
    dict.set("TU", utf16("金額"));
    let bytes = build(&[], vec![dict], dictionary! {});
    let both = form(json!([
        { "id": "issued", "label": "発行日", "type": "date" },
        { "id": "amt", "label": "金額", "type": "amount" },
    ]));
    let result = run_fill(&bytes, &both);
    let issued = &result.fields["issued"];
    assert_not_filled(issued);
    assert_eq!(reasons(issued), [AcroFormHold::FieldMatchesSeveralItems]);
    let amt = &result.fields["amt"];
    assert_not_filled(amt);
    assert!(
        matches!(amt.status, FillStatus::NotFound | FillStatus::NoText),
        "{amt:?}"
    );
    assert!(amt.evidence.is_empty());
    assert_eq!(
        reasons(amt),
        [
            AcroFormHold::FieldMatchesSeveralItems,
            AcroFormHold::InvalidValue
        ]
    );
    assert_eq!(amt.acroform[0].raw.as_deref(), Some("2026/11/08"));

    // A choice whose /TU matches a typed item (kind unsupported there).
    let mut choice = field(
        "都道府県",
        "Ch",
        dictionary! { "V" => utf16("大阪府"), "Opt" => vec![utf16("大阪府")] },
    );
    choice.set("TU", utf16("件数"));
    let bytes = build(&[], vec![choice], dictionary! {});
    let both = form(json!([
        { "id": "pref", "label": "都道府県", "type": "text" },
        { "id": "count", "label": "件数", "type": "number" },
    ]));
    let result = run_fill(&bytes, &both);
    let pref = &result.fields["pref"];
    assert_not_filled(pref);
    assert_eq!(reasons(pref), [AcroFormHold::FieldMatchesSeveralItems]);
    assert!(pref.candidates.is_empty());
    let count = &result.fields["count"];
    assert_not_filled(count);
    assert_eq!(
        reasons(count),
        [
            AcroFormHold::FieldMatchesSeveralItems,
            AcroFormHold::KindUnsupported
        ]
    );

    // The choice alone commits for the text item.
    let fill = one(&bytes, &label("pref", "都道府県", "text"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("大阪府"));
}

#[test]
fn an_acroform_without_fields_serializes_exactly_as_text_layer_fill() {
    let runs = [run(72.0, 200.0, "請求番号：INV-002")];
    let form = label("number", "請求番号", "text");
    let text_only = |bytes: &[u8]| {
        let (snapshot, _) = inspect_with_acroform(bytes, PdfLimits::default()).expect("inspect");
        fill::fill_with_limits(
            &snapshot.document_id,
            &snapshot.fragments,
            &form,
            &FillLimits::default(),
        )
        .expect("fill")
    };
    // `/AcroForm << /Fields [] >>` and an empty `/AcroForm << >>`.
    let with_fields = build(&runs, vec![], dictionary! {});
    let mut doc = Document::load_mem(&with_fields).expect("load");
    let acroform = doc
        .catalog()
        .and_then(|catalog| catalog.get(b"AcroForm"))
        .and_then(Object::as_reference)
        .expect("AcroForm reference");
    doc.get_object_mut(acroform)
        .and_then(Object::as_dict_mut)
        .expect("AcroForm")
        .remove(b"Fields");
    let empty = save(doc);
    for bytes in [with_fields, empty] {
        let (_, acroform) = inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
        let acroform_read = acroform.as_ref().expect("read");
        assert!(acroform_read.present && acroform_read.fields.is_empty());
        let result = run_fill(&bytes, &form);
        assert_eq!(result.acroform, None);
        assert_eq!(result.fields["number"].status, FillStatus::Filled);
        let json = serde_json::to_value(&result).expect("serialize");
        assert!(json.get("acroform").is_none(), "{json}");
        assert!(json["fields"]["number"].get("acroform").is_none());
        assert_eq!(
            serde_json::to_vec(&result).expect("serialize"),
            serde_json::to_vec(&text_only(&bytes)).expect("serialize")
        );
    }

    // An XFA-only form is still reported: its values may be in the XFA.
    let bytes = build(
        &runs,
        vec![],
        dictionary! { "XFA" => Object::Array(vec![]) },
    );
    assert_eq!(
        run_fill(&bytes, &form).acroform,
        Some(AcroFormSummary::Read {
            fields: 0,
            xfa: XfaPresence::XfaOnly
        })
    );
}

#[test]
fn text_inside_the_widget_that_cannot_be_compared_is_unverified() {
    let bytes = build(
        &[run(305.0, 100.0, "INV-001")],
        vec![text_field("請求番号", "INV-001")],
        dictionary! {},
    );
    let form = label("number", "請求番号", "text");
    let (snapshot, acroform) =
        inspect_with_acroform(&bytes, PdfLimits::default()).expect("inspect");
    let fill_with = |fragments: &[vf_source::SourceFragment]| {
        fill_with_acroform(
            &snapshot.document_id,
            fragments,
            &acroform,
            &form,
            &FillLimits::default(),
        )
        .expect("fill")
        .fields
        .remove("number")
        .expect("field")
    };
    assert_eq!(fill_with(&snapshot.fragments).status, FillStatus::Filled);
    // The same text in another quality or turned is not compared.
    for change in [
        |f: &mut vf_source::SourceFragment| f.quality = Quality::Suspect,
        |f: &mut vf_source::SourceFragment| f.quality = Quality::Unknown,
        |f: &mut vf_source::SourceFragment| f.orientation = Orientation::Deg90,
    ] {
        let mut fragments = snapshot.fragments.clone();
        fragments.iter_mut().for_each(change);
        let fill = fill_with(&fragments);
        assert_not_filled(&fill);
        assert_eq!(reasons(&fill), [AcroFormHold::AppearanceUnverified]);
    }
}

/// Page 1 of `runs` turned by `/Rotate 90`. With `upright`, each run is drawn
/// turned on the sheet so that it reads upright once displayed, at the
/// Display position the run gives; otherwise runs keep their sheet position
/// and read top to bottom.
fn rotated_page(runs: &[Run], upright: bool) -> Document {
    let mut doc = Document::load_mem(&pdf::build(runs)).expect("load");
    let page = *doc.get_pages().get(&1).expect("page 1");
    if upright {
        let content = Content::decode(&doc.get_page_content(page)).expect("content");
        let operations: Vec<_> = content
            .operations
            .into_iter()
            .map(|op| {
                if op.operator != "Td" {
                    return op;
                }
                let [x, y] = [0, 1].map(|i| op.operands[i].as_float().expect("Td operand"));
                // Display (dx, dy) of a /Rotate 90 page is sheet (dy, dx).
                let matrix = [0.0, 1.0, -1.0, 0.0, PAGE_HEIGHT as f32 - y, x];
                Operation::new("Tm", matrix.iter().map(|v| Object::Real(*v)).collect())
            })
            .collect();
        let bytes = Content { operations }.encode().expect("encode");
        doc.change_page_content(page, bytes).expect("content");
    }
    doc.get_object_mut(page)
        .and_then(Object::as_dict_mut)
        .expect("page")
        .set("Rotate", 90);
    doc
}

/// [`text_field`] whose widget shows at `BOX` on a `/Rotate 90` page.
fn rotated_text_field(name: &str, value: &str) -> Dictionary {
    let [left, top, right, bottom] = BOX;
    let mut dict = text_field(name, value);
    dict.set(
        "Rect",
        [top, left, bottom, right]
            .iter()
            .map(|v| Object::Real(*v as f32))
            .collect::<Vec<_>>(),
    );
    dict
}

fn save(mut doc: Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save");
    out
}

#[test]
fn the_appearance_check_works_in_display_coordinates_on_a_rotated_page() {
    let form = label("number", "請求番号", "text");
    let widget = || vec![rotated_text_field("請求番号", "INV-001")];
    let fragment = |bytes: &[u8]| {
        let (snapshot, acroform) =
            inspect_with_acroform(bytes, PdfLimits::default()).expect("inspect");
        let rect = acroform.expect("read").fields[0].widgets[0].rect;
        let [left, top, right, bottom] = BOX;
        assert_eq!(
            rect,
            Some(Rect::try_new(left, top, right, bottom).expect("box"))
        );
        assert_eq!(snapshot.fragments.len(), 1, "{:?}", snapshot.fragments);
        snapshot.fragments[0].clone()
    };

    // Text upright on the sheet reads top to bottom on the page: usable,
    // inside the widget, but turned, so it is not compared.
    let doc = rotated_page(&[run(88.0, PAGE_HEIGHT - 310.0, "INV-000")], false);
    let bytes = save(with_form(doc, widget(), dictionary! {}));
    let turned = fragment(&bytes);
    assert_eq!(turned.quality, Quality::Usable);
    assert_ne!(turned.orientation, Orientation::Deg0);
    let fill = one(&bytes, &form);
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::AppearanceUnverified]);

    // Text upright on the page inside the widget is compared.
    let doc = rotated_page(&[run(305.0, 100.0, "INV-000")], true);
    let bytes = save(with_form(doc, widget(), dictionary! {}));
    let upright = fragment(&bytes);
    assert_eq!(
        (upright.quality, upright.orientation),
        (Quality::Usable, Orientation::Deg0)
    );
    let fill = one(&bytes, &form);
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::AppearanceMismatch]);

    let doc = rotated_page(&[run(305.0, 100.0, "INV-001")], true);
    let bytes = save(with_form(doc, widget(), dictionary! {}));
    let fill = one(&bytes, &form);
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("INV-001"));
    let [left, top, right, bottom] = BOX;
    assert_eq!(
        fill.evidence[0].bbox,
        Rect::try_new(left, top, right, bottom).expect("box")
    );
}

#[test]
fn every_line_break_makes_a_value_multi_line() {
    for c in [
        '\n', '\r', '\u{000B}', '\u{000C}', '\u{0085}', '\u{2028}', '\u{2029}',
    ] {
        let bytes = build(
            &[],
            vec![text_field("氏名", &format!("山田{c}太郎"))],
            dictionary! {},
        );
        let fill = one(&bytes, &label("name", "氏名", "text"));
        assert_not_filled(&fill);
        assert_eq!(reasons(&fill), [AcroFormHold::MultiLine], "{c:?}");
    }
}

#[test]
fn a_value_longer_than_max_len_is_held() {
    let with_max = |max: i64| {
        field(
            "番号",
            "Tx",
            dictionary! { "V" => utf16("１２３４５"), "MaxLen" => max },
        )
    };
    let bytes = build(&[], vec![with_max(4)], dictionary! {});
    let fill = one(&bytes, &label("number", "番号", "text"));
    assert_not_filled(&fill);
    assert_eq!(reasons(&fill), [AcroFormHold::ExceedsMaxLen]);

    // MaxLen counts characters, not bytes.
    let bytes = build(&[], vec![with_max(5)], dictionary! {});
    let fill = one(&bytes, &label("number", "番号", "text"));
    assert_eq!(fill.status, FillStatus::Filled);
    assert_eq!(fill.raw.as_deref(), Some("１２３４５"));
}
