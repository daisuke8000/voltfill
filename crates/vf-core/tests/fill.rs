mod support;
use serde_json::json;
use vf_core::fill::{self, FillStatus};
use vf_core::pdf::{self, PdfLimits};

#[test]
fn fills_a_labelled_value_from_a_real_pdf_text_layer() {
    let bytes = support::pdf_with_text("Ref: AB12");
    let snapshot = pdf::inspect(&bytes, PdfLimits::default()).expect("synthetic PDF must inspect");
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
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/fill-form.json");
    let form = fill::parse_form(&std::fs::read(path).expect("example form must exist"))
        .expect("example form must satisfy the form contract");
    assert_eq!(form.fields.len(), 4);
}
