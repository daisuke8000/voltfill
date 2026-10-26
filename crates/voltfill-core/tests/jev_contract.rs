//! Synthetic contract tests. No requests are sent to Jev or any other server.
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use voltfill_core::jev::transport::{
    self, Clock, Execution, Failure, HttpRequest, HttpResponse, Policy, Secret, Transport,
    TransportError,
};
use voltfill_core::jev::{
    self, Answer, Content, Diagnostic, Error, Limits, Question, Request, multipart,
};

fn request_value() -> Value {
    json!({"model":"jev-1.14.0", "state":{"text":"synthetic blue square", "image":"<image>"},
    "questions": {
        "present":{"type":"noul", "instructions":"Is a square present?", "criteria":{"true":"present","false":"absent"}},
        "pick":{"type":"choice", "instructions":{"task":"Select color"}, "criteria":{"blue":{"label":"blue"},"none":null}},
        "rating":{"type":"score", "instructions":"Rate clarity", "criteria":[{"clarity":"low"},{"clarity":"high"}]},
        "location":{"type":"box", "instructions":"Locate square in the image"}},
    "assets":{"<image>":{"part":"image-1"}}})
}
fn response_value() -> Value {
    json!({"model":"jev-1.14.0", "answers": {
        "present":{"type":"noul","noul":0.8},
        "pick":{"type":"choice","choice":"blue","probabilities":{"blue":0.75,"none":0.25},"confidence":0.5},
        "rating":{"type":"score","score":0.75,"legend":{"0":{"clarity":"low"},"1":{"clarity":"high"}},"probabilities":{"0":0.25,"1":0.75},"confidence":0.5},
        "location":{"type":"box","box":{"left":0.1,"top":0.2,"right":0.7,"bottom":0.8},"confidence":0.6}},
    "usage":{"input_tokens":120,"output_tokens":0}})
}
fn encode(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).expect("synthetic JSON values must serialize")
}
fn request() -> Request {
    jev::parse_request(&encode(&request_value()), Limits::default())
        .expect("the four-question image request satisfies the wire contract")
}
fn text_request() -> Request {
    jev::parse_request(
        &encode(&json!({"model":"jev-1.14.0","state":"synthetic text",
        "questions":{"present":{"type":"noul","instructions":"Is there a square?"}}})),
        Limits::default(),
    )
    .expect("the text-only noul request satisfies the wire contract")
}
fn text_response() -> Vec<u8> {
    encode(
        &json!({"model":"jev-1.14.0","answers":{"present":{"type":"noul","noul":0.8}},"usage":{"input_tokens":5,"output_tokens":0}}),
    )
}

#[test]
fn requested_versions_and_aliases_validate_resolved_models_and_image_capabilities() {
    for requested in ["jev-1.13.0", "jev-1.14.0", "jev-latest", "jev-preview"] {
        let mut draft = text_request().draft().clone();
        draft.model = requested.into();
        let request = jev::build_request(draft, Limits::default())
            .expect("supported model versions and aliases must build text requests");
        assert_eq!(
            serde_json::from_slice::<Value>(request.bytes())
                .expect("a built request must contain valid JSON")["model"],
            requested
        );
        for resolved in [
            "jev-1.13.0",
            "jev-1.14.0",
            "jev-1.15.0",
            "jev-latest",
            "other",
        ] {
            let body =
                encode(&json!({"model":resolved,"answers":{"present":{"type":"noul","noul":1.0}}}));
            let parsed = jev::parse_response(&body, &request, Limits::default());
            let valid = if requested == "jev-latest" || requested == "jev-preview" {
                resolved.starts_with("jev-1.")
            } else {
                resolved == requested
            };
            assert_eq!(parsed.is_ok(), valid, "{requested} -> {resolved}");
            if let Ok(parsed) = parsed {
                assert_eq!(parsed.model, resolved);
            }
        }
    }
    for model in [
        "jev-1.13.0",
        "jev-latest",
        "jev-preview",
        "jev-1.14.0",
        "jev-1.15.0",
    ] {
        let mut draft = request().draft().clone();
        draft.model = model.into();
        assert_eq!(
            jev::build_request(draft, Limits::default()).is_ok(),
            model == "jev-1.14.0" || model == "jev-1.15.0"
        );
        // Image Noul is gated even without a Box question.
        let mut draft = request().draft().clone();
        draft.model = model.into();
        draft.questions.retain(|id, _| id == "present");
        assert_eq!(
            jev::build_request(draft, Limits::default()).is_ok(),
            jev::supports_images(model)
        );
    }
    for invalid in [
        "",
        "other",
        "jev-1.14",
        "jev-1.14.0.0",
        "jev-1.14.x",
        "jev-1.14.0\n",
    ] {
        assert!(jev::validate_model(invalid).is_err());
    }
}

#[test]
fn preserves_user_questions_and_validates_all_four_answer_types() {
    let request = request();
    assert_eq!(
        serde_json::from_slice::<Value>(request.bytes())
            .expect("a built request must preserve valid JSON content"),
        request_value()
    );
    let parsed = jev::parse_response(&encode(&response_value()), &request, Limits::default())
        .expect("the four-answer response satisfies envelope validation");
    assert!(parsed.answers.values().all(Result::is_ok));
    assert!(matches!(
        parsed.answers["present"],
        Ok(Answer::Noul { noul: 0.8 })
    ));
    assert_eq!(parsed.usage.input_tokens.0, Some(120));
    assert_eq!(parsed.usage.output_tokens.0, Some(0));
    assert!(parsed.diagnostics.is_empty());
}

#[test]
fn d20_missing_null_and_zero_remain_distinct() {
    let request = request();
    let mut response = response_value();
    response["answers"]["location"]
        .as_object_mut()
        .expect("the synthetic box answer is an object")
        .remove("confidence");
    response
        .as_object_mut()
        .expect("the synthetic response is an object")
        .remove("usage");
    let parsed = jev::parse_response(&encode(&response), &request, Limits::default())
        .expect("missing optional usage and confidence must not reject the envelope");
    assert!(parsed.answers["location"].is_ok());
    assert_eq!(parsed.usage.input_tokens.0, None);
    assert!(parsed.diagnostics.contains(&Diagnostic::MissingUsage));
    assert!(
        parsed
            .diagnostics
            .contains(&Diagnostic::MissingConfidence("location".into()))
    );
    response["usage"] = json!({"input_tokens":null,"output_tokens":0});
    response["answers"]["location"]["confidence"] = Value::Null;
    let parsed = jev::parse_response(&encode(&response), &request, Limits::default())
        .expect("null usage counters and confidence must not reject the envelope");
    assert_eq!(parsed.usage.input_tokens.0, None);
    assert_eq!(parsed.usage.output_tokens.0, Some(0));
    assert!(parsed.diagnostics.contains(&Diagnostic::NullInputTokens));
    assert!(
        parsed
            .diagnostics
            .contains(&Diagnostic::NullConfidence("location".into()))
    );
    response["usage"] = json!({});
    let parsed = jev::parse_response(&encode(&response), &request, Limits::default())
        .expect("empty usage must retain missing-counter diagnostics");
    assert!(parsed.diagnostics.contains(&Diagnostic::MissingInputTokens));
    assert!(
        parsed
            .diagnostics
            .contains(&Diagnostic::MissingOutputTokens)
    );
    response["usage"] = Value::Null;
    assert!(
        jev::parse_response(&encode(&response), &request, Limits::default())
            .expect("null usage must retain a diagnostic instead of rejecting the envelope")
            .diagnostics
            .contains(&Diagnostic::NullUsage)
    );
    for invalid in [
        json!({"input_tokens":-1}),
        json!({"input_tokens":0.5}),
        json!([]),
        json!("unknown"),
    ] {
        response["usage"] = invalid;
        assert_eq!(
            jev::parse_response(&encode(&response), &request, Limits::default())
                .expect_err("invalid usage values must reject the envelope"),
            Error::Usage
        );
    }
}

#[test]
fn semantic_answer_failure_retains_independent_answers() {
    let request = request();
    for (id, key, invalid) in [
        ("present", "noul", json!(1.1)),
        ("pick", "choice", json!("invented")),
        ("rating", "score", json!(0.2)),
        ("rating", "legend", json!({"0":"rewritten","1":"rewritten"})),
        (
            "location",
            "box",
            json!({"left":0.9,"top":0.2,"right":0.7,"bottom":0.8}),
        ),
        ("location", "confidence", json!(1.01)),
        ("present", "type", json!("unknown")),
    ] {
        let mut response = response_value();
        response["answers"][id][key] = invalid;
        let parsed = jev::parse_response(&encode(&response), &request, Limits::default())
            .expect("an invalid individual answer must not reject the complete envelope");
        for (answer_id, answer) in parsed.answers {
            assert_eq!(answer.is_err(), answer_id == id, "{id}/{key}");
        }
    }
}

#[test]
fn protocol_integrity_is_batch_wide_and_bounded() {
    let request = request();
    let mut response = response_value();
    response["model"] = json!("jev-latest");
    assert_eq!(
        jev::parse_response(&encode(&response), &request, Limits::default())
            .expect_err("an unresolved response model alias must be rejected"),
        Error::Model
    );
    response = response_value();
    response["answers"]
        .as_object_mut()
        .expect("the synthetic answers field is an object")
        .remove("present");
    assert_eq!(
        jev::parse_response(&encode(&response), &request, Limits::default())
            .expect_err("missing answer IDs must reject the envelope"),
        Error::IdSet
    );
    for raw in [
        br#"{"x":1,"\u0078":2}"#.as_slice(),
        b"{\"x\":1e400}",
        b"{}{}",
    ] {
        assert_eq!(
            jev::json::parse(raw, Limits::default())
                .expect_err("duplicate keys or malformed JSON must be rejected"),
            Error::Json
        );
    }
    let duplicate = String::from_utf8(encode(&response_value()))
        .expect("JSON serialization must produce UTF-8 for duplicate-key mutation")
        .replace("\"present\":", "\"present\":{},\"\\u0070resent\":");
    assert_eq!(
        jev::parse_response(duplicate.as_bytes(), &request, Limits::default())
            .expect_err("escaped duplicate answer keys must be rejected"),
        Error::Json
    );
    assert_eq!(
        jev::json::parse(
            b"{} ",
            Limits {
                max_bytes: 2,
                ..Limits::default()
            }
        )
        .expect_err("JSON bytes above the limit must be rejected"),
        Error::Bytes
    );
    assert_eq!(
        jev::json::parse(
            b"[[[]]]",
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        )
        .expect_err("JSON nesting above the limit must be rejected"),
        Error::Depth
    );
}

#[test]
fn rejects_rust_nonfinite_and_question_cardinality() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut draft = request().draft().clone();
        draft.state = Content::Object(BTreeMap::from([("n".into(), Content::Float(value))]));
        assert_eq!(
            jev::build_request(draft, Limits::default())
                .expect_err("nonfinite content must be rejected before serialization"),
            Error::Nonfinite
        );
    }
    for (count, valid) in [(0, false), (1, true), (255, true), (256, false)] {
        let mut draft = text_request().draft().clone();
        draft.questions.insert(
            "choice".into(),
            Question::Choice {
                instructions: Content::Text("Choose".into()),
                criteria: (0..count).map(|n| (n.to_string(), Content::Null)).collect(),
            },
        );
        assert_eq!(jev::build_request(draft, Limits::default()).is_ok(), valid);
    }
    for (count, valid) in [(1, false), (2, true), (10, true), (11, false)] {
        let mut draft = text_request().draft().clone();
        draft.questions.insert(
            "score".into(),
            Question::Score {
                instructions: Content::Text("Rate".into()),
                criteria: (0..count).map(|n| Content::Text(n.to_string())).collect(),
            },
        );
        assert_eq!(jev::build_request(draft, Limits::default()).is_ok(), valid);
    }
}

// PNG structure fixture only: its placeholder pixels are never decoded or sent.
fn png(width: u32, height: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]);
    }
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut bytes, b"IHDR", &ihdr);
    chunk(&mut bytes, b"IDAT", &[0]);
    chunk(&mut bytes, b"IEND", &[]);
    bytes
}
#[test]
fn multipart_encodes_exact_parts_and_bounds_image_metadata() {
    let request = request();
    let image = multipart::PngImage::new("image-1".into(), png(280, 1568))
        .expect("PNG dimensions at the supported boundary must validate");
    let body = multipart::encode(&request, &[image], "synthetic-boundary")
        .expect("matching image parts and a safe boundary must encode as multipart");
    assert!(body.danger_zone_images);
    assert_eq!(
        body.content_type,
        "multipart/form-data; boundary=synthetic-boundary"
    );
    assert!(
        body.bytes().starts_with(
            b"--synthetic-boundary\r\nContent-Disposition: form-data; name=\"request\""
        )
    );
    assert!(
        body.bytes()
            .windows(request.bytes().len())
            .any(|w| w == request.bytes())
    );
    assert!(body.bytes().ends_with(b"--synthetic-boundary--\r\n"));
    for (width, height) in [(279, 280), (280, 1569)] {
        assert!(multipart::PngImage::new("image-1".into(), png(width, height)).is_err());
    }
    let sides = 200..=2000;
    assert!(
        multipart::PngImage::with_sides("image-1".into(), png(200, 2000), sides.clone()).is_ok()
    );
    assert!(multipart::PngImage::with_sides("image-1".into(), png(199, 280), sides).is_err());
    assert!(multipart::PngImage::new("bad\r\nname".into(), png(280, 280)).is_err());
    assert!(multipart::PngImage::new("request".into(), png(280, 280)).is_err());
    assert!(multipart::Body::json(&request).is_err());
    let image = multipart::PngImage::new("wrong-part".into(), png(280, 280))
        .expect("a safe unmatched part name is valid until request binding validation");
    assert!(multipart::encode(&request, &[image], "boundary").is_err());
}

#[derive(Clone, Default)]
struct FakeClock {
    now: Rc<Cell<u64>>,
    cancelled: bool,
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }
    fn cancelled(&self) -> bool {
        self.cancelled
    }
}
#[derive(Default)]
struct FakeTransport {
    responses: VecDeque<Result<HttpResponse, TransportError>>,
    bodies: Vec<Vec<u8>>,
}
impl Transport for FakeTransport {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        assert!(request.timeout_ms > 0);
        assert!(request.max_response_bytes > 0);
        assert_eq!(request.secret.bearer(), "synthetic-token");
        self.bodies.push(request.body.bytes().to_vec());
        self.responses.pop_front().expect("unexpected attempt")
    }
}
fn http(
    status: u16,
    body: Vec<u8>,
    retry_after_ms: Option<u64>,
) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        body_error: None,
        status,
        body,
        retry_after_ms,
    })
}
fn run(
    transport: &mut FakeTransport,
    clock: &mut FakeClock,
    secret: Option<&Secret>,
    policy: Policy,
) -> Execution {
    transport::execute(
        &text_request(),
        &[],
        "",
        secret,
        policy,
        Limits::default(),
        transport,
        clock,
    )
}
fn policy() -> Policy {
    Policy {
        allow_remote: true,
        max_attempts: 3,
        max_total_body_bytes: 1_000_000,
        max_estimated_input_tokens: 192_000,
        initial_backoff_ms: 10,
        ..Policy::default()
    }
}

#[test]
fn transport_default_deny_missing_key_redirect_and_redaction() {
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut transport = FakeTransport::default();
    let mut clock = FakeClock::default();
    assert_eq!(
        run(&mut transport, &mut clock, Some(&secret), Policy::default())
            .result
            .expect_err("remote execution must require explicit permission"),
        Failure::Disabled
    );
    assert_eq!(
        run(&mut transport, &mut clock, None, policy())
            .result
            .expect_err("remote execution must require caller-supplied credentials"),
        Failure::Credentials
    );
    assert!(transport.bodies.is_empty());
    transport
        .responses
        .push_back(http(302, b"private body".to_vec(), None));
    assert_eq!(
        run(&mut transport, &mut clock, Some(&secret), policy())
            .result
            .expect_err("redirect responses must not be followed"),
        Failure::Redirect
    );
    assert_eq!(transport.bodies.len(), 1);
    assert!(!format!("{secret:?}").contains("synthetic-token"));
    assert!(!format!("{:?}", text_request()).contains("synthetic text"));
}

#[test]
fn retries_stop_at_deadline_budget_and_cancellation() {
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut clock = FakeClock::default();
    let mut transport = FakeTransport {
        responses: VecDeque::from([http(429, Vec::new(), Some(10_000))]),
        ..FakeTransport::default()
    };
    assert_eq!(
        run(
            &mut transport,
            &mut clock,
            Some(&secret),
            Policy {
                total_deadline_ms: 10_000,
                ..policy()
            }
        )
        .result
        .expect_err("Retry-After beyond the deadline must stop execution"),
        Failure::Deadline
    );
    assert_eq!(clock.now_ms(), 0);
    assert_eq!(transport.bodies.len(), 1);
    let mut transport = FakeTransport {
        responses: VecDeque::from([http(500, Vec::new(), None)]),
        ..FakeTransport::default()
    };
    let capped = Policy {
        max_estimated_input_tokens: 64_000,
        ..policy()
    };
    assert_eq!(
        run(&mut transport, &mut clock, Some(&secret), capped)
            .result
            .expect_err("the request cap must stop additional retries"),
        Failure::Budget
    );
    assert_eq!(transport.bodies.len(), 1);
    clock.cancelled = true;
    assert_eq!(
        run(&mut transport, &mut clock, Some(&secret), policy())
            .result
            .expect_err("a cancelled clock must stop execution"),
        Failure::Cancelled
    );
    assert_eq!(transport.bodies.len(), 1);
}

#[test]
fn response_limit_and_invalid_success_never_adopt_values() {
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut clock = FakeClock::default();
    let mut transport = FakeTransport {
        responses: VecDeque::from([http(200, vec![b' '; Limits::default().max_bytes + 1], None)]),
        ..FakeTransport::default()
    };
    assert_eq!(
        run(&mut transport, &mut clock, Some(&secret), policy())
            .result
            .expect_err("an oversized response must fail the byte limit"),
        Failure::Protocol(Error::Bytes)
    );
    let mut transport = FakeTransport {
        responses: VecDeque::from([http(200, b"private-invalid-json".to_vec(), None)]),
        ..FakeTransport::default()
    };
    let result = run(&mut transport, &mut clock, Some(&secret), policy());
    assert_eq!(
        result
            .result
            .expect_err("malformed successful response JSON must fail protocol validation"),
        Failure::Protocol(Error::Json)
    );
    assert_eq!(transport.bodies.len(), 1);
}

#[test]
fn deadline_covers_api_time_and_applies_remaining_timeout() {
    struct SlowTransport {
        now: Rc<Cell<u64>>,
        observed_timeout: u64,
    }
    impl Transport for SlowTransport {
        fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            self.observed_timeout = request.timeout_ms;
            self.now.set(self.now.get() + 100);
            http(200, text_response(), None)
        }
    }
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut clock = FakeClock::default();
    let mut transport = SlowTransport {
        now: clock.now.clone(),
        observed_timeout: 0,
    };
    let execution = transport::execute(
        &text_request(),
        &[],
        "",
        Some(&secret),
        Policy {
            total_deadline_ms: 100,
            ..policy()
        },
        Limits::default(),
        &mut transport,
        &mut clock,
    );
    assert_eq!(transport.observed_timeout, 100);
    assert_eq!(
        execution
            .result
            .expect_err("a response arriving at the deadline must fail execution"),
        Failure::Deadline
    );
    assert_eq!(execution.total_ms, 100);
    assert_eq!(execution.attempts[0].elapsed_ms, 100);
    assert_eq!(execution.attempts[0].input_tokens, Some(5));
    assert!(!execution.attempts[0].billing_unknown);
}

#[test]
fn invalid_images_preview_and_total_body_budget_do_not_send() {
    assert!(
        multipart::PngImage::new("image-1".into(), vec![0; multipart::MAX_IMAGE_BYTES + 1])
            .is_err()
    );
    let mut animated = png(280, 280);
    animated.splice(33..33, [0, 0, 0, 0, b'a', b'c', b'T', b'L', 0, 0, 0, 0]);
    assert!(multipart::PngImage::new("image-1".into(), animated).is_err());
    let mut truncated = png(280, 280);
    truncated.pop();
    assert!(multipart::PngImage::new("image-1".into(), truncated).is_err());
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut transport = FakeTransport::default();
    let mut clock = FakeClock::default();
    let image = multipart::PngImage::new("image-1".into(), png(280, 280))
        .expect("the square PNG meets the minimum image dimensions");
    let execution = transport::execute(
        &request(),
        &[image],
        "boundary",
        Some(&secret),
        policy(),
        Limits::default(),
        &mut transport,
        &mut clock,
    );
    assert_eq!(
        execution
            .result
            .expect_err("image execution must require preview permission"),
        Failure::PreviewDisabled
    );
    assert_eq!(
        run(
            &mut transport,
            &mut clock,
            Some(&secret),
            Policy {
                max_total_body_bytes: 1,
                ..policy()
            }
        )
        .result
        .expect_err("a zero request cap must prevent execution"),
        Failure::Budget
    );
    assert!(transport.bodies.is_empty());
}

#[test]
fn transport_streaming_limit_failure_is_terminal_and_billing_unknown() {
    let secret = Secret::new("synthetic-token".into())
        .expect("the synthetic bearer contains only permitted characters");
    let mut clock = FakeClock::default();
    let mut transport = FakeTransport {
        responses: VecDeque::from([Err(TransportError::ResponseTooLarge)]),
        ..FakeTransport::default()
    };
    let execution = run(&mut transport, &mut clock, Some(&secret), policy());
    assert_eq!(
        execution
            .result
            .expect_err("a transport size failure must report the byte limit"),
        Failure::Protocol(Error::Bytes)
    );
    assert_eq!(transport.bodies.len(), 1);
    assert!(execution.attempts[0].billing_unknown);
}

#[test]
fn choice_preserves_rounded_tied_and_nonmaximum_api_selections() {
    let request = request();
    for (blue, none, selected) in [
        (0.74, 0.25, "blue"),
        (0.5, 0.5, "blue"),
        (0.75, 0.25, "none"),
    ] {
        let mut response = response_value();
        response["answers"]["pick"]["choice"] = json!(selected);
        response["answers"]["pick"]["probabilities"] = json!({"blue":blue,"none":none});
        let parsed = jev::parse_response(&encode(&response), &request, Limits::default())
            .expect("rounded or tied probabilities must not reject the response envelope");
        assert_eq!(
            parsed
                .diagnostics
                .contains(&Diagnostic::ProbabilitySumNotOne("pick".into())),
            blue + none != 1.0
        );
        let Ok(Answer::Choice {
            choice,
            probabilities,
            ..
        }) = parsed.answers["pick"].as_ref()
        else {
            panic!("the valid choice answer must preserve the API selection")
        };
        assert_eq!(choice, selected);
        assert_eq!(probabilities["blue"], blue);
        assert_eq!(probabilities["none"], none);
    }
}
