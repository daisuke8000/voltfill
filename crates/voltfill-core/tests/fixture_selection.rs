use serde_json::{Value, json};
use std::collections::BTreeMap;
use voltfill_core::evaluate::{EvaluationContext, EvaluationError, Evaluator, FixtureEvaluator};
use voltfill_core::jev::{self, Answer, Content, Limits, Question, RequestDraft};

#[test]
fn fixture_selection_requires_one_matching_non_none_choice() {
    struct Case {
        name: &'static str,
        criteria: Value,
        selection: Value,
        expected: Option<&'static str>,
    }
    let cases = [
        Case {
            name: "one matching source choice",
            criteria: json!({"a":"target","none":"absent"}),
            selection: json!({"select_text":"target"}),
            expected: Some("a"),
        },
        Case {
            name: "none label is not a source choice",
            criteria: json!({"a":"other","none":"target"}),
            selection: json!({"select_text":"target"}),
            expected: None,
        },
        Case {
            name: "duplicate source labels are ambiguous",
            criteria: json!({"a":"target","b":"target","none":"absent"}),
            selection: json!({"select_text":"target"}),
            expected: None,
        },
        Case {
            name: "explicit none selection",
            criteria: json!({"a":"target","none":"absent"}),
            selection: json!({"select_none":true}),
            expected: Some("none"),
        },
        Case {
            name: "none selection requires the none choice",
            criteria: json!({"a":"target","b":"other"}),
            selection: json!({"select_none":true}),
            expected: None,
        },
    ];
    for Case {
        name,
        criteria,
        selection,
        expected,
    } in cases
    {
        let request = jev::build_request(
            RequestDraft {
                model: jev::MODEL.into(),
                state: Content::Text("synthetic selection test".into()),
                questions: BTreeMap::from([(
                    "q".into(),
                    Question::Choice {
                        instructions: Content::Text("Choose the matching value".into()),
                        criteria: serde_json::from_value(criteria)
                            .expect("choice criteria must deserialize from the case object"),
                    },
                )]),
                assets: BTreeMap::new(),
            },
            Limits::default(),
        )
        .expect("the bounded choice request must satisfy the wire contract");
        let fixture = json!({"fixture_kind":"synthetic", "answers":{"field.q.1":selection}});
        let mut evaluator = FixtureEvaluator::from_bytes(
            &serde_json::to_vec(&fixture)
                .expect("the synthetic answer selection must serialize as JSON"),
        )
        .expect("the synthetic fixture envelope must initialize the evaluator");
        let result = evaluator.evaluate(
            &request,
            &[],
            &EvaluationContext {
                field_id: "field",
                page: 1,
                remaining_requests: 1,
                remaining_ms: 1000,
            },
        );
        match expected {
            Some(expected) => {
                let response = result.expect(name);
                let Ok(Answer::Choice { choice, .. }) = &response.answers["q"] else {
                    panic!("{name}: expected a valid choice answer");
                };
                assert_eq!(choice, expected, "{name}");
            }
            None => assert!(
                matches!(result, Err(EvaluationError::Fixture)),
                "{name}: fixture must be rejected"
            ),
        }
    }
}
