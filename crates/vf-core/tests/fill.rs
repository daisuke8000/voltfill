mod support;
use serde_json::json;
use vf_core::fill::{self, FillStatus};
use vf_source::{Orientation, PdfLimits, Quality, inspect};

#[test]
fn fills_a_labelled_value_from_a_real_pdf_text_layer() {
    let bytes = support::pdf_with_text("Ref: AB12");
    let snapshot = inspect(&bytes, PdfLimits::default()).expect("synthetic PDF must inspect");
    let form = fill::parse_form(
        json!({"fields":[
            {"id":"reference","label":"Ref"},
            {"id":"missing","label":"Total","type":"amount"}
        ]})
        .to_string()
        .as_bytes(),
    )
    .expect("form must parse");
    let result = fill::fill(&snapshot.document_id, &snapshot.fragments, &form);
    assert!(result.evaluation_only);
    assert_eq!(result.document_id, snapshot.document_id);
    let reference = &result.fields["reference"];
    assert_eq!(reference.status, FillStatus::Filled);
    assert_eq!(reference.raw.as_deref(), Some("AB12"));
    let evidence = &reference.evidence[0];
    let fragment = snapshot
        .fragments
        .iter()
        .find(|f| f.id == evidence.fragment_id)
        .expect("evidence must point at a source fragment");
    // The value is an exact slice of the source text, never synthesized.
    assert_eq!(&fragment.raw_text[evidence.start..evidence.end], "AB12");
    assert!(fragment.bbox.contains(evidence.bbox));
    assert_eq!(result.fields["missing"].status, FillStatus::NotFound);
}

#[test]
fn documented_example_form_parses() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/forms/invoice.json"
    );
    let form = fill::parse_form(&std::fs::read(path).expect("example form must exist"))
        .expect("example form must satisfy the form contract");
    assert_eq!(form.fields.len(), 4);
}

/// Each phrase drawn word by word along one reading line (the gap before
/// each word in points), as producers draw turned labels.
const PHRASES: [&[(&str, f64)]; 3] = [
    &[("Invoice", 0.0), ("No", 3.0), ("A-1", 3.0)],
    &[("請求番号", 0.0), ("：", 0.0), ("INV-001", 3.0)],
    &[("Name", 0.0), ("Alice", 4.0), ("Smith", 4.0)],
];

/// [`PHRASES`] with the text matrix's linear part `linear`, the first
/// phrase starting at `start` and each next one `step` further, plus enough
/// upright body text that pdf-inspector keeps the page frame.
fn word_by_word_pdf(linear: [f64; 4], start: (f64, f64), step: (f64, f64)) -> Vec<u8> {
    let mut runs: Vec<support::Run<'_>> = (0..12)
        .map(|i| {
            let y = 760.0 - 12.0 * f64::from(i);
            ([1.0, 0.0, 0.0, 1.0], (250.0, y), "Body text line here")
        })
        .collect();
    let [a, b, ..] = linear;
    for (i, words) in PHRASES.iter().enumerate() {
        let i = i as f64;
        let mut along = 0.0;
        for (j, &(text, gap)) in words.iter().enumerate() {
            if j > 0 {
                along += gap;
            }
            let (x, y) = (start.0 + step.0 * i, start.1 + step.1 * i);
            runs.push((linear, (x + a * along, y + b * along), text));
            along += 12.0 * text.chars().count() as f64;
        }
    }
    support::pdf_with_runs(&runs)
}

#[test]
fn turned_words_drawn_one_by_one_never_fill_a_piece_of_their_phrase() {
    let form = fill::parse_form(
        json!({"fields":[
            {"id":"invoice","label":"Invoice No"},
            {"id":"number","label":"請求番号"},
            {"id":"name","label":"Name"}
        ]})
        .to_string()
        .as_bytes(),
    )
    .expect("form must parse");
    let run = |bytes: Vec<u8>| {
        let snapshot = inspect(&bytes, PdfLimits::default()).expect("synthetic PDF must inspect");
        // On turned pages every word reaches layout as its own usable run.
        let turned = snapshot
            .fragments
            .iter()
            .filter(|f| f.orientation != Orientation::Deg0)
            .collect::<Vec<_>>();
        assert!(
            turned.is_empty()
                || turned.len() == 9 && turned.iter().all(|f| f.quality == Quality::Usable),
            "{turned:?}"
        );
        fill::fill(&snapshot.document_id, &snapshot.fragments, &form)
    };
    // Upright, pdf-inspector merges each phrase into one run first.
    let upright = run(word_by_word_pdf(
        [1.0, 0.0, 0.0, 1.0],
        (80.0, 500.0),
        (0.0, -40.0),
    ));
    let number = &upright.fields["number"];
    assert_eq!(number.status, FillStatus::Filled, "{number:?}");
    assert_eq!(number.raw.as_deref(), Some("INV-001"));
    for id in ["invoice", "name"] {
        assert_eq!(upright.fields[id].status, FillStatus::NotFound, "{id}");
    }
    let turned = [
        ("90°", [0.0, 1.0, -1.0, 0.0], (440.0, 150.0), (40.0, 0.0)),
        ("180°", [-1.0, 0.0, 0.0, -1.0], (560.0, 400.0), (0.0, 40.0)),
        ("270°", [0.0, -1.0, 1.0, 0.0], (180.0, 650.0), (-40.0, 0.0)),
    ];
    for (case, linear, start, step) in turned {
        let result = run(word_by_word_pdf(linear, start, step));
        for (id, field) in &result.fields {
            // Not filled, or filled as the upright page fills it.
            assert!(
                field.status != FillStatus::Filled || field.raw == upright.fields[id].raw,
                "{case} {id}: {field:?}"
            );
            // No piece of a phrase is offered as a value either.
            for candidate in &field.candidates {
                assert!(
                    !["：", "No", "Alice", "A-1", "INV-001", "Smith"]
                        .contains(&candidate.raw.as_str()),
                    "{case} {id}: {candidate:?}"
                );
            }
            assert_eq!(field.status, FillStatus::NotFound, "{case} {id}: {field:?}");
        }
    }
}
