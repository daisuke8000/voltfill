use serde_json::{Value, json};
use voltfill_core::{
    model::Strategy,
    schema::{self, SchemaError},
};
fn form() -> Value {
    serde_json::from_slice(include_bytes!("../../../examples/text-form.json")).unwrap()
}
fn check(value: Value) -> Result<schema::FormDefinition, SchemaError> {
    schema::parse(&serde_json::to_vec(&value).unwrap())
}
#[test]
fn valid_dynamic_schema_preserves_question_and_limits_strategy() {
    let form = check(form()).unwrap();
    assert_eq!(form.fields[0].id, "reference");
    assert!(form.validate_strategy(Strategy::TextFirst).is_ok());
    assert_eq!(
        form.validate_strategy(Strategy::VisionFirst),
        Err(SchemaError::Strategy)
    );
    let serialized = serde_json::to_value(form).unwrap();
    assert_eq!(
        serialized["fields"][0]["questions"][0]["instructions"],
        json!("Choose the reference printed on the page, or none if absent.")
    );
}
#[test]
fn duplicate_keys_unknown_fields_and_unbounded_input_rejected() {
    assert!(schema::parse(br#"{"schema_version":1,"schema_version":1,"fields":[]}"#).is_err());
    let mut f = form();
    f["silent_default"] = json!(true);
    assert!(check(f).is_err());
    let mut f = form();
    f["schema_version"] = json!(2);
    assert_eq!(check(f).unwrap_err(), SchemaError::Version);
    assert_eq!(
        schema::parse(&vec![b' '; schema::MAX_SCHEMA_BYTES + 1]).unwrap_err(),
        SchemaError::Limit
    );
}
#[test]
fn graph_requires_existing_acyclic_unique_questions() {
    let mut f = form();
    f["fields"][0]["questions"][0]["depends_on"] = json!(["missing"]);
    assert_eq!(check(f).unwrap_err(), SchemaError::Dependency);
    let mut f = form();
    f["fields"][0]["questions"][0]["depends_on"] = json!(["pick_reference"]);
    assert_eq!(check(f).unwrap_err(), SchemaError::Dependency);
    let mut f = form();
    let q = f["fields"][0]["questions"][0].clone();
    f["fields"][0]["questions"].as_array_mut().unwrap().push(q);
    assert_eq!(check(f).unwrap_err(), SchemaError::Identifier);
}
#[test]
fn invalid_resolver_candidate_overflow_and_pages_rejected() {
    let mut f = form();
    f["fields"][0]["resolver"]["question_id"] = json!("unknown");
    assert!(check(f).is_err());
    let mut f = form();
    f["fields"][0]["questions"][0]["candidate_source"]["max_candidates"] = json!(255);
    assert_eq!(check(f).unwrap_err(), SchemaError::Contract);
    let mut f = form();
    f["fields"][0]["pages"] = json!([0]);
    assert_eq!(check(f).unwrap_err(), SchemaError::Contract);
    let mut f = form();
    f["fields"][0]["output_type"] = json!("bool");
    assert_eq!(check(f).unwrap_err(), SchemaError::Contract);
}
#[test]
fn static_choice_requires_complete_mapping_and_box_requires_presence_gate() {
    let mut f = form();
    f["fields"][0]["output_type"] = json!("enum");
    let q = &mut f["fields"][0]["questions"][0];
    q.as_object_mut().unwrap().remove("candidate_source");
    q["criteria"] = json!({"a":null,"b":null});
    f["fields"][0]["resolver"] = json!({"kind":"selected_choice","question_id":"pick_reference","mapping":{"a":"A","b":"B"}});
    assert!(check(f.clone()).is_ok());
    f["fields"][0]["resolver"]["mapping"]
        .as_object_mut()
        .unwrap()
        .remove("b");
    assert!(check(f).is_err());
}

#[test]
fn mapping_values_must_match_output_type_before_evaluation() {
    let mut f = form();
    f["fields"][0]["output_type"] = json!("bool");
    let q = &mut f["fields"][0]["questions"][0];
    q.as_object_mut().unwrap().remove("candidate_source");
    q["criteria"] = json!({"yes":null,"no":null});
    f["fields"][0]["resolver"] = json!({"kind":"selected_choice","question_id":"pick_reference","mapping":{"yes":"true","no":"false"}});
    assert!(check(f.clone()).is_err());
    f["fields"][0]["resolver"]["mapping"] = json!({"yes":true,"no":false});
    assert!(check(f).is_ok());
}

#[test]
fn score_mapping_and_box_output_types_are_rejected_before_evaluation() {
    let mut f = form();
    f["fields"][0]["output_type"] = json!("enum");
    f["fields"][0]["questions"] = json!([{ "id":"score", "type":"score", "instructions":"Rate the evidence", "criteria":["bad","good"],"input":{"modality":"text"}}]);
    f["fields"][0]["resolver"] =
        json!({"kind":"score_to_level","question_id":"score","levels":["low",{}]});
    assert!(check(f).is_err());
    let mut f = form();
    f["fields"][0]["output_type"] = json!("bool");
    f["fields"][0]["questions"] = json!([{ "id":"box", "type":"box", "instructions":"Locate the value", "input":{"modality":"image"}}, {"id":"present","type":"noul","instructions":"Is it present?","input":{"modality":"image"}}]);
    f["fields"][0]["resolver"] =
        json!({"kind":"text_in_box","question_id":"box","presence_question_id":"present"});
    assert!(check(f).is_err());
}

#[test]
fn string_arrays_are_source_runs_not_mapping_outputs() {
    let mut f = form();
    f["fields"][0]["output_type"] = json!("string_array");
    f["fields"][0]["questions"] = json!([{ "id":"q", "type":"choice", "instructions":"Choose", "criteria":{"a":"A","b":"B"},"input":{"modality":"text"}}]);
    f["fields"][0]["resolver"] =
        json!({"kind":"selected_choice","question_id":"q","mapping":{"a":["A"],"b":["B"]}});
    assert!(check(f.clone()).is_err());
    f["fields"][0]["questions"] = json!([{ "id":"q", "type":"score", "instructions":"Rate", "criteria":["A","B"],"input":{"modality":"text"}}]);
    f["fields"][0]["resolver"] =
        json!({"kind":"score_to_level","question_id":"q","levels":[["A"],["B"]]});
    assert!(check(f).is_err());
}

#[test]
fn multiple_candidates_require_bounded_text_array_and_one_independent_question() {
    let mut f = form();
    f["fields"][0]["output_type"] = json!("string_array");
    f["fields"][0]["resolver"] =
        json!({"kind":"selected_candidates","question_id":"pick_reference","max_selections":8});
    let parsed = check(f.clone()).unwrap();
    assert!(parsed.validate_strategy(Strategy::TextFirst).is_ok());
    assert!(parsed.validate_strategy(Strategy::Hybrid).is_err());
    assert!(parsed.validate_strategy(Strategy::VisionFirst).is_err());
    for cap in [0, 255] {
        let mut bad = f.clone();
        bad["fields"][0]["resolver"]["max_selections"] = json!(cap);
        assert!(check(bad).is_err());
    }
    let mut bad = f.clone();
    bad["fields"][0]["resolver"]
        .as_object_mut()
        .unwrap()
        .remove("max_selections");
    assert!(check(bad).is_err());
    let mut bad = f.clone();
    bad["fields"][0]["output_type"] = json!("string");
    assert!(check(bad).is_err());
    let mut bad = f.clone();
    bad["fields"][0]["questions"][0]["input"]["modality"] = json!("both");
    assert!(check(bad).is_err());
    let mut q = f["fields"][0]["questions"][0].clone();
    q["id"] = json!("another");
    f["fields"][0]["questions"].as_array_mut().unwrap().push(q);
    assert!(check(f).is_err());
}
