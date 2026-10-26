use jev_client::multipart::Body;
use jev_client::transport::{
    self, AttemptBudget, Clock, Execution, Failure, HttpRequest, HttpResponse, NoopObserver,
    Policy, Secret, Transport, TransportError,
};
use jev_client::{self as jev, PngImage};
use std::collections::VecDeque;

#[derive(Default)]
struct Wire {
    replies: VecDeque<HttpResponse>,
    sent: Vec<Vec<u8>>,
}
impl Transport for Wire {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        self.sent.push(request.body.bytes().to_vec());
        self.replies.pop_front().ok_or(TransportError::BeforeSend)
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
fn request() -> jev::Request {
    jev::parse_request(
        br#"{"model":"jev-1.14.0","state":"@page","assets":{"@page":{"part":"page"}},"questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,
        jev::Limits::default(),
    )
    .expect("synthetic image reference must bind to its declared asset")
}
fn reply(status: u16) -> HttpResponse {
    let body = br#"{"model":"jev-1.14.0","answers":{"q":{"type":"noul","noul":1}},"usage":{"input_tokens":900,"output_tokens":5}}"#;
    HttpResponse {
        status,
        body: if status == 200 {
            body.to_vec()
        } else {
            b"busy".to_vec()
        },
        body_error: None,
        retry_after_ms: None,
    }
}
fn policy() -> Policy {
    Policy {
        allow_remote: true,
        allow_preview_images: true,
        ..Default::default()
    }
}
fn run(
    wire: &mut Wire,
    clock: &mut Time,
    budget: &mut AttemptBudget,
    make: &mut dyn FnMut() -> Result<Vec<PngImage>, ()>,
) -> Execution {
    transport::execute_deferred(
        &request(),
        make,
        "deferred-boundary",
        Some(
            &Secret::new("synthetic-only".into())
                .expect("synthetic bearer contains only visible ASCII"),
        ),
        policy(),
        jev::Limits::default(),
        wire,
        clock,
        budget,
        &mut NoopObserver,
    )
}

#[test]
fn deferred_images_are_made_once_and_a_retry_resends_the_same_body() {
    let mut wire = Wire {
        replies: VecDeque::from([reply(503), reply(200)]),
        ..Default::default()
    };
    let mut made = 0;
    let execution = run(
        &mut wire,
        &mut Time::default(),
        &mut AttemptBudget::default(),
        &mut || {
            made += 1;
            Ok(vec![
                PngImage::new("page".into(), png())
                    .expect("synthetic PNG must pass structural validation"),
            ])
        },
    );
    assert!(execution.result.is_ok());
    assert_eq!(made, 1);
    assert_eq!(wire.sent.len(), 2);
    assert_eq!(wire.sent[0], wire.sent[1]);
    let images =
        [PngImage::new("page".into(), png())
            .expect("synthetic PNG must pass structural validation")];
    let ready = jev::multipart::encode(&request(), &images, "deferred-boundary")
        .expect("prepared image must match the declared multipart asset");
    assert_eq!(wire.sent[0], Body::bytes(&ready));
}

#[test]
fn images_that_cannot_be_made_return_the_slot_unsent() {
    let mut budget = AttemptBudget::default();
    let mut clock = Time::default();
    let mut wire = Wire {
        replies: VecDeque::from([reply(200)]),
        ..Default::default()
    };
    let failed = run(&mut wire, &mut clock, &mut budget, &mut || Err(()));
    assert_eq!(failed.result.err(), Some(Failure::ImagesUnavailable));
    assert!(failed.attempts.is_empty() && wire.sent.is_empty());
    let snapshot = budget.snapshot();
    assert_eq!((snapshot.reserved, snapshot.known_cost), (0, 0));
    assert!(snapshot.last_completion.is_none() && !snapshot.stopped);
    let sent = run(&mut wire, &mut clock, &mut budget, &mut || {
        Ok(vec![
            PngImage::new("page".into(), png())
                .expect("synthetic PNG must pass structural validation"),
        ])
    });
    assert!(sent.result.is_ok());
    assert_eq!((clock.0, wire.sent.len()), (0, 1));
}
