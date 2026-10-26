use jev_client::transport::{
    self, Attempt, AttemptBudget, AttemptContext, AttemptObserver, Clock, Failure, HttpRequest,
    HttpResponse, Policy, Secret, Transport, TransportError,
};
use jev_client::{self as jev, RecordingTransport, ResponseBody};
use std::collections::VecDeque;

struct Wire(VecDeque<Result<HttpResponse, TransportError>>);
impl Transport for Wire {
    fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        self.0
            .pop_front()
            .expect("transport must not exceed the queued attempts")
    }
}
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
fn request() -> jev::Request {
    jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,jev::Limits::default()).expect("synthetic Noul request must satisfy the wire contract")
}
fn response(body: &[u8], error: Option<TransportError>) -> HttpResponse {
    HttpResponse {
        status: 200,
        body: body.to_vec(),
        body_error: error,
        retry_after_ms: Some(25),
    }
}
#[test]
fn records_every_send_in_order_without_extraction_context() {
    let mut wire = Wire(VecDeque::from([
        Ok(response(b"  original body\n", None)),
        Ok(response(
            &[255, 0, 254],
            Some(TransportError::OutcomeUnknown),
        )),
        Err(TransportError::BeforeSend),
    ]));
    let request = request();
    let body = jev::multipart::Body::json(&request).expect("text-only request must encode as JSON");
    let secret =
        Secret::new("synthetic-only".into()).expect("synthetic bearer contains only visible ASCII");
    let mut records = Vec::new();
    {
        let mut transport = RecordingTransport::new(&mut wire, |record| records.push(record));
        for attempt in 0..3 {
            let result = transport.send(HttpRequest {
                secret: &secret,
                body: &body,
                timeout_ms: 100,
                max_response_bytes: 1024,
            });
            if attempt == 2 {
                assert_eq!(result.err(), Some(TransportError::BeforeSend));
            }
        }
    }
    assert_eq!(records.len(), 3);
    assert!(matches!(&records[0].body, Some(ResponseBody::Text(s)) if s == "  original body\n"));
    assert!(records[0].body_complete);
    assert_eq!(records[0].http_status, Some(200));
    assert_eq!(records[0].retry_after_ms, Some(25));
    assert!(matches!(&records[1].body, Some(ResponseBody::Bytes(b)) if b == &[255,0,254]));
    assert_eq!(
        records[1].transport_error,
        Some(TransportError::OutcomeUnknown)
    );
    assert!(!records[1].body_complete);
    assert!(records[2].body.is_none());
    assert!(records[2].http_status.is_none());
    assert!(!records[2].body_complete);
}
struct Reject;
impl AttemptObserver for Reject {
    fn after_send(
        &mut self,
        _: &Attempt,
        _: Option<&jev::Response>,
        _: &AttemptBudget,
        _: &AttemptContext<'_>,
    ) -> Result<(), Failure> {
        Err(Failure::Observer)
    }
}
#[test]
fn originals_survive_observer_failure_and_disabled_execution_never_records() {
    let original =
        br#"{ "model":"jev-1.14.0", "answers":{"q":{"type":"noul","noul":1}}, "unknown":[1,2] }"#;
    let mut wire = Wire(VecDeque::from([Ok(response(original, None))]));
    let secret =
        Secret::new("synthetic-only".into()).expect("synthetic bearer contains only visible ASCII");
    let request = request();
    let mut records = Vec::new();
    let mut recorder = RecordingTransport::new(&mut wire, |r| records.push(r));
    let disabled = transport::execute(
        &request,
        &[],
        "boundary",
        Some(&secret),
        Policy::default(),
        jev::Limits::default(),
        &mut recorder,
        &mut Time::default(),
    );
    assert_eq!(disabled.result.err(), Some(Failure::Disabled));
    let result = transport::execute_observed(
        &request,
        &[],
        "boundary",
        Some(&secret),
        Policy {
            allow_remote: true,
            ..Default::default()
        },
        jev::Limits::default(),
        &mut recorder,
        &mut Time::default(),
        &mut AttemptBudget::default(),
        &mut Reject,
    );
    assert_eq!(result.result.err(), Some(Failure::Observer));
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0].body, Some(ResponseBody::Text(s)) if s.as_bytes() == original));
}
