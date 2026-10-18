//! Fault injection through the real RemoteEvaluator used by CLI and the campaign.
use serde_json::{Value, json};
use std::{cell::Cell, collections::VecDeque, rc::Rc};
use voltfill_core::{
    evaluate::{EvaluationContext, Evaluator, RemoteEvaluator},
    jev::{
        self, Answer, Response,
        multipart::PngImage,
        transport::{
            self, Attempt, AttemptBudget, AttemptObserver, Clock, Failure, HttpRequest,
            HttpResponse, Secret, Transport, TransportError,
        },
    },
};

#[derive(Clone, Default)]
struct FakeClock(Rc<Cell<u64>>);
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }
}
struct Wire {
    clock: FakeClock,
    outcomes: VecDeque<(u64, Result<HttpResponse, TransportError>)>,
    bodies: Vec<Vec<u8>>,
    starts: Vec<u64>,
    timeouts: Vec<u64>,
}
impl Transport for Wire {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        self.starts.push(self.clock.now_ms());
        self.timeouts.push(request.timeout_ms);
        self.bodies.push(request.body.bytes().to_vec());
        let (elapsed, response) = self
            .outcomes
            .pop_front()
            .expect("unexpected extra HTTP attempt");
        self.clock.0.set(self.clock.now_ms() + elapsed);
        response
    }
}
#[derive(Default)]
struct Audit {
    rows: Vec<Value>,
    reservations: Vec<(u32, u64)>,
}
impl AttemptObserver for Audit {
    fn before_send(
        &mut self,
        _: &jev::multipart::Body,
        budget: &AttemptBudget,
        _: u64,
        _: &transport::AttemptContext<'_>,
    ) -> Result<(), Failure> {
        self.reservations
            .push((budget.snapshot().posts, budget.snapshot().reserved));
        Ok(())
    }
    fn after_send(
        &mut self,
        attempt: &Attempt,
        _: Option<&Response>,
        budget: &AttemptBudget,
        _: &transport::AttemptContext<'_>,
    ) -> Result<(), Failure> {
        self.rows.push(json!({"known_input":attempt.input_tokens,"unknown_billing":attempt.billing_unknown,"errors":attempt.answer_errors.values().map(|e|format!("{e:?}")).collect::<Vec<_>>(),"failure":attempt.failure.map(|e|format!("{e:?}")),"retryable":attempt.retryable,"known_cost":budget.snapshot().known_cost,"reserved":budget.snapshot().reserved}));
        Ok(())
    }
}
type Remote = RemoteEvaluator<Wire, FakeClock, Audit>;
fn remote(outcomes: Vec<(u64, Result<HttpResponse, TransportError>)>) -> Remote {
    let clock = FakeClock::default();
    RemoteEvaluator::with_components(
        Secret::new("synthetic-test-secret".into()).unwrap(),
        true,
        true,
        Wire {
            clock: clock.clone(),
            outcomes: outcomes.into(),
            bodies: Vec::new(),
            starts: Vec::new(),
            timeouts: Vec::new(),
        },
        clock,
        AttemptBudget::default(),
        Audit::default(),
    )
    .unwrap()
}
fn http(
    status: u16,
    body: Value,
    retry_after_ms: Option<u64>,
) -> (u64, Result<HttpResponse, TransportError>) {
    (
        100,
        Ok(HttpResponse {
            body_error: None,
            status,
            body: serde_json::to_vec(&body).unwrap(),
            retry_after_ms,
        }),
    )
}
fn context(requests: u32, ms: u64) -> EvaluationContext<'static> {
    EvaluationContext {
        field_id: "synthetic_field",
        page: 1,
        remaining_requests: requests,
        remaining_ms: ms,
    }
}
fn noul_request() -> jev::Request {
    jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic-only","questions":{"q":{"type":"noul","instructions":"present"}}}"#,jev::Limits::default()).unwrap()
}
fn noul_reservation() -> u64 {
    jev::planning::estimate(&noul_request(), &Default::default()).request_tokens * 42
}
fn good() -> Value {
    json!({"model":jev::MODEL,"answers":{"q":{"type":"noul","noul":1.0}},"usage":{"input_tokens":10,"output_tokens":2}})
}
fn choice_request() -> jev::Request {
    jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic-only","questions":{"q":{"type":"choice","instructions":"pick","criteria":{"a":"A","b":"B"}}}}"#,jev::Limits::default()).unwrap()
}
fn choice(valid: bool, input: u64) -> Value {
    json!({"model":jev::MODEL,"answers":{"q":{"type":"choice","choice":if valid{"a"}else{"invented"},"probabilities":{"a":if valid{0.9}else{0.1},"b":if valid{0.1}else{0.9}},"confidence":0.95}},"usage":{"input_tokens":input,"output_tokens":342}})
}
#[test]
fn real_remote_retries_429_retry_after_5xx_and_unknown_with_independent_reservations() {
    for first in [
        http(429, json!({"private_error":"do-not-log"}), Some(5_000)),
        http(503, json!({}), None),
        (100, Err(TransportError::OutcomeUnknown)),
    ] {
        let long_wait = first
            .1
            .as_ref()
            .ok()
            .and_then(|v| v.retry_after_ms)
            .unwrap_or(2_000);
        let mut r = remote(vec![first, http(200, good(), None)]);
        let response = r
            .evaluate(&noul_request(), &[], &context(3, 30_000))
            .unwrap();
        assert!(matches!(
            response.answers["q"],
            Ok(Answer::Noul { noul: 1.0 })
        ));
        assert_eq!(r.transport.starts, [0, 100 + long_wait]);
        assert_eq!(r.budget.snapshot().posts, 2);
        assert_eq!(r.budget.snapshot().known_cost, 420);
        assert_eq!(r.budget.snapshot().reserved, noul_reservation());
        assert_eq!(
            r.observer.reservations,
            [(1, noul_reservation()), (2, 2 * noul_reservation())]
        );
        assert_eq!(r.last_usage().attempts, 2);
        assert_eq!(r.last_usage().unknown_usage_attempts, 1);
        assert!(r.transport.bodies.windows(2).all(|pair| pair[0] == pair[1]));
        let records = r.take_responses();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].http_status, Some(200));
        if records[0].http_status.is_none() {
            assert_eq!(
                records[0].transport_error,
                Some(TransportError::OutcomeUnknown)
            );
            assert!(records[0].body.is_none());
        } else {
            assert!(records[0].body.is_some());
            assert!(records[0].transport_error.is_none());
        }
    }
}
#[test]
fn valid_envelope_invalid_choice_retries_without_relaxing_validator_or_unknown_billing() {
    let mut r = remote(vec![
        http(200, choice(false, 100), None),
        http(200, choice(true, 10), None),
    ]);
    let response = r
        .evaluate(&choice_request(), &[], &context(3, 30_000))
        .unwrap();
    assert!(response.answers["q"].is_ok());
    assert_eq!(r.last_usage().attempts, 2);
    assert_eq!(r.last_usage().known_input_tokens, 110);
    assert_eq!(r.last_usage().unknown_usage_attempts, 0);
    assert_eq!(r.observer.rows[0]["errors"], json!(["Keys"]));
    assert_eq!(r.observer.rows[0]["known_cost"], 4_200);
    assert_eq!(r.observer.rows[0]["reserved"], 0);
    assert_eq!(r.budget.snapshot().known_cost, 110 * 42);
    assert_eq!(r.budget.snapshot().reserved, 0);
    let mut exhausted = remote(
        (0..3)
            .map(|_| http(200, choice(false, 100), None))
            .collect(),
    );
    let response = exhausted
        .evaluate(&choice_request(), &[], &context(3, 30_000))
        .unwrap();
    assert_eq!(
        response.answers["q"].as_ref().unwrap_err(),
        &jev::Error::Keys
    );
    assert_eq!(exhausted.transport.bodies.len(), 3);
    assert_eq!(exhausted.budget.snapshot().known_cost, 3 * 100 * 42);
    assert_eq!(exhausted.budget.snapshot().reserved, 0);
}
#[test]
fn permanent_failures_never_retry_and_remaining_requests_caps_each_http_attempt() {
    for first in [
        http(401, json!({}), None),
        http(403, json!({}), None),
        http(
            200,
            json!({"model":"wrong-model","answers":{},"usage":{"input_tokens":10,"output_tokens":2}}),
            None,
        ),
        http(
            200,
            json!({"model":jev::MODEL,"answers":{},"usage":{"input_tokens":10,"output_tokens":2}}),
            None,
        ),
    ] {
        let mut r = remote(vec![first]);
        assert!(
            r.evaluate(&noul_request(), &[], &context(3, 30_000))
                .is_err()
        );
        assert_eq!(r.transport.bodies.len(), 1);
    }
    let mut r = remote((0..3).map(|_| http(503, json!({}), None)).collect());
    assert!(
        r.evaluate(&noul_request(), &[], &context(2, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 2);
    assert_eq!(r.last_usage().attempts, 2);
    assert_eq!(r.budget.snapshot().reserved, 2 * noul_reservation());
    let mut r = remote((0..3).map(|_| http(503, json!({}), None)).collect());
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 3);
    assert_eq!(r.last_usage().attempts, 3);
}
#[test]
fn retry_after_deadline_attempt_timeout_and_cross_call_budget_are_shared() {
    let mut r = remote(vec![http(429, json!({}), Some(30_000))]);
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 1);
    assert_eq!(r.clock.now_ms(), 100);
    let mut r = remote(vec![
        (10_000, Err(TransportError::OutcomeUnknown)),
        http(200, good(), None),
    ]);
    r.evaluate(&noul_request(), &[], &context(3, 30_000))
        .unwrap();
    assert_eq!(r.transport.starts, [0, 12_000]);
    assert_eq!(r.transport.timeouts, [10_000, 10_000]);
    assert_eq!(r.budget.snapshot().reserved, noul_reservation());
    let mut r = remote(vec![http(200, good(), None), http(200, good(), None)]);
    r.budget.set_limits(1, 200_000_000);
    r.evaluate(&noul_request(), &[], &context(3, 30_000))
        .unwrap();
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 1);
    let mut r = remote(vec![http(200, good(), None)]);
    r.budget.set_limits(128, noul_reservation() - 1);
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert!(r.transport.bodies.is_empty());
}
#[test]
fn arbitrary_clock_origin_and_idle_before_first_call_do_not_expire_a_new_session() {
    let mut r = remote(vec![http(200, good(), None)]);
    r.clock.0.set(9_000_000);
    r.evaluate(&noul_request(), &[], &context(3, 30_000))
        .unwrap();
    assert_eq!(r.transport.starts, [9_000_000]);
    r.clock.0.set(9_000_000 + 600_000);
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 1);
}

#[test]
fn underestimated_usage_stops_future_sends_and_preserves_the_original_body() {
    let mut body = good();
    body["usage"]["input_tokens"] = json!(100_000);
    let mut r = remote(vec![http(200, body.clone(), None)]);
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert!(r.budget.snapshot().stopped);
    assert_eq!(r.budget.snapshot().known_cost, 100_000 * 42);
    assert!(
        r.evaluate(&noul_request(), &[], &context(3, 30_000))
            .is_err()
    );
    assert_eq!(r.transport.bodies.len(), 1);
    let records = r.take_responses();
    assert_eq!(records.len(), 1);
    assert!(records[0].body_complete);
    let Some(voltfill_core::model::ResponseBody::Text(original)) = &records[0].body else {
        panic!("missing original");
    };
    assert_eq!(serde_json::from_str::<Value>(original).unwrap(), body);
}
fn png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (
            b"IHDR",
            [
                300u32.to_be_bytes().as_slice(),
                400u32.to_be_bytes().as_slice(),
                &[8, 6, 0, 0, 0],
            ]
            .concat(),
        ),
        (b"IDAT", vec![0]),
        (b"IEND", Vec::new()),
    ] {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(&[0; 4]);
    }
    bytes
}
#[test]
fn actual_remote_mixed_four_question_batch_retries_the_identical_bound_image_once() {
    let request=jev::parse_request(br#"{"model":"jev-1.14.0","state":{"text":"synthetic","image":"@page"},"assets":{"@page":{"part":"page"}},"questions":{"n":{"type":"noul","instructions":"present"},"c":{"type":"choice","instructions":"pick","criteria":{"a":"A","b":"B"}},"s":{"type":"score","instructions":"score","criteria":["bad","good"]},"b":{"type":"box","instructions":"locate the image"}}}"#,jev::Limits::default()).unwrap();
    let images = [PngImage::new("page".into(), png()).unwrap()];
    let summary = transport::input_summary(&request, &images);
    assert_eq!(summary.input_kind, "both");
    assert_eq!(summary.question_count, 4);
    assert_eq!(summary.image_dimensions, [(300, 400)]);
    let body = json!({"model":jev::MODEL,"answers":{"n":{"type":"noul","noul":1.0},"c":{"type":"choice","choice":"a","probabilities":{"a":1.0,"b":0.0},"confidence":1.0},"s":{"type":"score","score":1.0,"legend":{"0":"bad","1":"good"},"probabilities":{"0":0.0,"1":1.0},"confidence":1.0},"b":{"type":"box","box":{"left":0.1,"top":0.1,"right":0.8,"bottom":0.8},"confidence":1.0}},"usage":{"input_tokens":1000,"output_tokens":3}});
    let mut r = remote(vec![
        http(503, json!({}), None),
        http(200, body.clone(), None),
    ]);

    let reservation = jev::planning::estimate(&request, &Default::default()).request_tokens * 42;

    let response = r.evaluate(&request, &images, &context(3, 30_000)).unwrap();
    assert_eq!(response.answers.len(), 4);
    assert!(response.answers.values().all(|a| a.is_ok()));
    assert_eq!(r.transport.bodies.len(), 2);
    assert_eq!(r.transport.bodies[0], r.transport.bodies[1]);
    assert_eq!(r.budget.snapshot().known_cost, 42_000);
    assert_eq!(r.budget.snapshot().reserved, reservation);
    let records = r.take_responses();
    assert_eq!(records.len(), 2);
    let Some(voltfill_core::model::ResponseBody::Text(original)) = &records[1].body else {
        panic!("complete original response missing");
    };
    assert_eq!(serde_json::from_str::<Value>(original).unwrap(), body);
}

#[test]
fn configured_json_limit_rechecks_prebuilt_text_and_image_requests_before_send() {
    #[derive(Clone)]
    struct NeverSend;
    impl Transport for NeverSend {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            panic!("JSON limit must reject before sending");
        }
    }
    let image_request = jev::parse_request(br#"{"model":"jev-1.14.0","state":"@page","assets":{"@page":{"part":"page"}},"questions":{"q":{"type":"box","instructions":"locate"}}}"#, jev::Limits::default()).unwrap();
    for (request, images) in [
        (noul_request(), Vec::new()),
        (
            image_request,
            vec![PngImage::new("page".into(), png()).unwrap()],
        ),
    ] {
        let mut evaluator = RemoteEvaluator::with_components(
            Secret::new("synthetic".into()).unwrap(),
            true,
            true,
            NeverSend,
            voltfill_core::evaluate::MonotonicClock::default(),
            AttemptBudget::default(),
            transport::NoopObserver,
        )
        .unwrap()
        .with_execution_config(&transport::ExecutionConfig {
            max_json_bytes: request.bytes().len() - 1,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(
            evaluator.evaluate(&request, &images, &context(3, 30_000)),
            Err(voltfill_core::evaluate::EvaluationError::Protocol)
        ));
        assert_eq!(evaluator.budget.snapshot().posts, 0);
        assert!(evaluator.take_responses().is_empty());
    }
}
