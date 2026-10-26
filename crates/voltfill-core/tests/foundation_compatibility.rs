mod support;
use serde_json::{Value, json};
use voltfill_core::engine::{self, ExtractOptions};
use voltfill_core::evaluate::{FixtureEvaluator, RemoteEvaluator};
use voltfill_core::jev::transport::{
    AttemptBudget, Clock, HttpRequest, HttpResponse, NoopObserver, Secret, Transport,
    TransportError,
};
use voltfill_core::{jev, schema};

#[derive(Default)]
struct Time(u64);
impl Clock for Time {
    fn now_ms(&self) -> u64 {
        self.0
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.0 += ms;
    }
}
struct Targets;
static LOG_TARGETS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());
impl log::Log for Targets {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        LOG_TARGETS
            .lock()
            .expect("the log-target capture must not have panicked")
            .insert(record.target().to_owned());
    }
    fn flush(&self) {}
}
struct Wire(u32);
impl Transport for Wire {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        self.0 += 1;
        let body = if self.0 == 1 {
            br#"{ "error": "synthetic retry", "unknown": [1,2] }"#.to_vec()
        } else {
            let v: Value = serde_json::from_slice(request.body.bytes())
                .expect("the evaluator must send a JSON request to the scripted transport");
            let answers = v["questions"].as_object().expect("the request must contain a questions object").iter().map(|(id,q)| {
                let criteria=q["criteria"].as_object().expect("candidate questions must contain a criteria object");
                let selected=criteria.iter().find(|(_,v)| *v == "ALPHA42").expect("the ALPHA42 source candidate must be offered").0;
                let probabilities=criteria.keys().map(|key|(key.clone(),json!(if key==selected {1.0} else {0.0}))).collect::<serde_json::Map<_,_>>();
                (id.clone(),json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":1.0}))
            }).collect::<serde_json::Map<_,_>>();
            serde_json::to_vec(&json!({"model":jev::MODEL,"answers":answers,"usage":{"input_tokens":10,"output_tokens":2},"unknown":"preserved"})).expect("the synthetic response must serialize as JSON")
        };
        Ok(HttpResponse {
            status: if self.0 == 1 { 503 } else { 200 },
            body,
            body_error: None,
            retry_after_ms: None,
        })
    }
}
fn stable(mut value: Value) -> Value {
    value
        .as_object_mut()
        .expect("an extraction report must serialize as an object")
        .remove("run_id");
    let timings = value["timings"]
        .as_object_mut()
        .expect("an extraction report must contain a timings object");
    timings.retain(|key, _| !key.ends_with("_ms"));
    value
}
#[test]
fn extraction_matches_before_foundation_split() {
    log::set_logger(&Targets).expect("this integration-test binary must install its logger once");
    log::set_max_level(log::LevelFilter::Info);
    let bytes = support::pdf_with_text("ALPHA42");
    let form = schema::parse(include_bytes!("../../../examples/text-form.json"))
        .expect("the checked-in text form must satisfy the schema");
    let mut fixture =
        FixtureEvaluator::from_bytes(include_bytes!("../../../examples/text-mock.json"))
            .expect("the checked-in text mock must initialize the fixture evaluator");
    let mock = engine::extract(&bytes, &form, &ExtractOptions::default(), &mut fixture)
        .expect("fixture extraction must succeed on the synthetic PDF");
    let mut remote = RemoteEvaluator::with_components(
        Secret::new("synthetic-only".into())
            .expect("the synthetic bearer contains only permitted characters"),
        true,
        false,
        Wire(0),
        Time::default(),
        AttemptBudget::default(),
        NoopObserver,
    )
    .expect("the scripted transport and default budget must initialize the evaluator");
    let report = engine::extract(&bytes, &form, &ExtractOptions::default(), &mut remote)
        .expect("scripted extraction must succeed after the retry");
    let actual = json!({"log_targets":*LOG_TARGETS.lock().expect("the log-target capture must not have panicked"),"mock":stable(serde_json::to_value(mock).expect("the fixture report must serialize as JSON")),"remote":stable(serde_json::to_value(report).expect("the scripted report must serialize as JSON"))});
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/foundation-baseline.json");
    let expected: Value = serde_json::from_slice(
        &std::fs::read(path).expect("read the checked-in foundation compatibility baseline"),
    )
    .expect("the foundation compatibility baseline must be JSON");
    assert_eq!(actual, expected);
}
