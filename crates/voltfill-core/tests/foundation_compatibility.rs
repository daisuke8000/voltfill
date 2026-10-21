mod support;
use serde_json::{Value, json};
use voltfill_core::{
    engine::{self, ExtractOptions},
    evaluate::{FixtureEvaluator, RemoteEvaluator},
    jev::{self, transport::*},
    schema,
};

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
            .unwrap()
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
            let v: Value = serde_json::from_slice(request.body.bytes()).unwrap();
            let answers = v["questions"].as_object().unwrap().iter().map(|(id,q)| {
                let criteria=q["criteria"].as_object().unwrap();
                let selected=criteria.iter().find(|(_,v)| *v == "ALPHA42").unwrap().0;
                let probabilities=criteria.keys().map(|key|(key.clone(),json!(if key==selected {1.0} else {0.0}))).collect::<serde_json::Map<_,_>>();
                (id.clone(),json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":1.0}))
            }).collect::<serde_json::Map<_,_>>();
            serde_json::to_vec(&json!({"model":jev::MODEL,"answers":answers,"usage":{"input_tokens":10,"output_tokens":2},"unknown":"preserved"})).unwrap()
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
    value.as_object_mut().unwrap().remove("run_id");
    let timings = value["timings"].as_object_mut().unwrap();
    timings.retain(|key, _| !key.ends_with("_ms"));
    value
}
#[test]
fn extraction_matches_before_foundation_split() {
    log::set_logger(&Targets).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let bytes = support::pdf_with_text("ALPHA42");
    let form = schema::parse(include_bytes!("../../../examples/text-form.json")).unwrap();
    let mut fixture =
        FixtureEvaluator::from_bytes(include_bytes!("../../../examples/text-mock.json")).unwrap();
    let mock = engine::extract(&bytes, &form, &ExtractOptions::default(), &mut fixture).unwrap();
    let mut remote = RemoteEvaluator::with_components(
        Secret::new("synthetic-only".into()).unwrap(),
        true,
        false,
        Wire(0),
        Time::default(),
        AttemptBudget::default(),
        NoopObserver,
    )
    .unwrap();
    let report = engine::extract(&bytes, &form, &ExtractOptions::default(), &mut remote).unwrap();
    let actual = json!({"log_targets":*LOG_TARGETS.lock().unwrap(),"mock":stable(serde_json::to_value(mock).unwrap()),"remote":stable(serde_json::to_value(report).unwrap())});
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/foundation-baseline.json");
    if std::env::var_os("VOLTFILL_CAPTURE_BASELINE").is_some() {
        assert!(!path.exists(), "baseline must never be overwritten");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&actual).unwrap()).unwrap();
    } else {
        let expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(actual, expected);
    }
}
