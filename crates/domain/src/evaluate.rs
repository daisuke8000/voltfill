//! Evaluation boundary: synthetic fixtures and explicitly enabled remote Jev.
use crate::geometry::ImageTransform;
use crate::jev::multipart::PngImage;
use crate::jev::transport::{self, Clock, Policy, Secret};
use crate::jev::{self, Answer, Content, Request, Response};
use crate::model::{JevResponse, UsageSummary};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum EvaluationError {
    #[error("synthetic fixture is invalid or does not match this request")]
    Fixture,
    #[error("evaluation timed out")]
    Timeout,
    #[error("evaluation transport failed")]
    Transport,
    #[error("evaluation protocol failed")]
    Protocol,
}
pub struct EvaluationContext<'context> {
    pub field_id: &'context str,
    pub page: u32,
    pub remaining_requests: u32,
    pub remaining_ms: u64,
}
pub struct EvaluationCall<'call> {
    pub request: &'call Request,
    pub images: &'call [PngImage],
    pub context: EvaluationContext<'call>,
}
pub struct EvaluationOutcome {
    pub result: Result<Response, EvaluationError>,
    pub usage: UsageSummary,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub transform: ImageTransform,
}
/// Evaluation boundary for explicitly supplied fixtures or authorized remote execution.
/// Implementations preserve received attempt bodies independently of parsing success.
pub trait Evaluator {
    /// Drain retained attempts, including retries, malformed responses, and transport failures.
    #[must_use]
    fn take_responses(&mut self) -> Vec<JevResponse> {
        Vec::new()
    }
    #[must_use]
    fn supports_batching(&self) -> bool {
        false
    }
    /// Return one outcome per call in input order, with usage attached to each outcome.
    #[must_use]
    fn evaluate_many(&mut self, calls: &[EvaluationCall<'_>]) -> Vec<EvaluationOutcome> {
        calls
            .iter()
            .map(|call| {
                let result = self.evaluate(call.request, call.images, &call.context);
                EvaluationOutcome {
                    result,
                    usage: self.last_usage(),
                }
            })
            .collect()
    }
    fn evaluate(
        &mut self,
        request: &Request,
        images: &[PngImage],
        context: &EvaluationContext<'_>,
    ) -> Result<Response, EvaluationError>;
    #[must_use]
    fn last_usage(&self) -> UsageSummary {
        UsageSummary {
            attempts: 1,
            unknown_usage_attempts: 1,
            ..Default::default()
        }
    }
    #[must_use]
    fn backend(&self) -> &'static str {
        "injected_evaluator"
    }
    /// Synthetic-only images must be supplied explicitly, never invented by Core.
    fn fixture_image(&self, _page: u32) -> Option<&EvaluationImage> {
        None
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    fixture_kind: String,
    answers: BTreeMap<String, Value>,
    #[serde(default)]
    images: BTreeMap<u32, EvaluationImage>,
}
pub struct FixtureEvaluator {
    fixture: Fixture,
    calls: u32,
}
impl FixtureEvaluator {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EvaluationError> {
        let value = jev::json::parse(bytes, jev::Limits::default())
            .map_err(|_| EvaluationError::Fixture)?;
        let fixture: Fixture =
            serde_json::from_value(value).map_err(|_| EvaluationError::Fixture)?;
        if fixture.fixture_kind != "synthetic" {
            return Err(EvaluationError::Fixture);
        }
        for image in fixture.images.values() {
            image
                .transform
                .validate()
                .map_err(|_| EvaluationError::Fixture)?;
            if image
                .transform
                .sent_dimensions()
                .map_err(|_| EvaluationError::Fixture)?
                != (image.width, image.height)
            {
                return Err(EvaluationError::Fixture);
            }
            PngImage::try_new("image".into(), image.png.clone())
                .map_err(|_| EvaluationError::Fixture)?;
        }
        Ok(Self { fixture, calls: 0 })
    }
    #[must_use]
    pub fn calls(&self) -> u32 {
        self.calls
    }
}
impl Evaluator for FixtureEvaluator {
    fn evaluate(
        &mut self,
        request: &Request,
        _images: &[PngImage],
        context: &EvaluationContext<'_>,
    ) -> Result<Response, EvaluationError> {
        self.calls += 1;
        let mut answers = serde_json::Map::new();
        for (id, question) in &request.draft().questions {
            let key = format!("{}.{}.{}", context.field_id, id, context.page);
            let fixture = self
                .fixture
                .answers
                .get(&key)
                .ok_or(EvaluationError::Fixture)?;
            if let Some(error) = fixture.get("error") {
                return Err(if error == "timeout" {
                    EvaluationError::Timeout
                } else {
                    EvaluationError::Fixture
                });
            }
            let answer = if fixture.get("select_text").is_some()
                || fixture.get("select_none").is_some()
            {
                let jev::Question::Choice { criteria, .. } = question else {
                    return Err(EvaluationError::Fixture);
                };
                let selected = if fixture.get("select_none") == Some(&Value::Bool(true)) {
                    if !criteria.contains_key("none") {
                        return Err(EvaluationError::Fixture);
                    }
                    "none"
                } else {
                    let target = fixture
                        .get("select_text")
                        .and_then(Value::as_str)
                        .ok_or(EvaluationError::Fixture)?;
                    let mut matches = criteria
                        .iter()
                        .filter(|(key, content)| {
                            key.as_str() != "none"
                                && matches!(content, Content::Text(text) if text == target)
                        })
                        .map(|(key, _)| key.as_str());
                    match (matches.next(), matches.next()) {
                        (Some(selected), None) => selected,
                        _ => return Err(EvaluationError::Fixture),
                    }
                };
                let probabilities: BTreeMap<_, _> = criteria
                    .keys()
                    .map(|key| (key, if key == selected { 1.0 } else { 0.0 }))
                    .collect();
                json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":1.0})
            } else {
                fixture.clone()
            };
            answers.insert(id.clone(), answer);
        }
        let model = if matches!(request.draft().model.as_str(), "jev-latest" | "jev-preview") {
            jev::MODEL
        } else {
            &request.draft().model
        };
        let bytes=serde_json::to_vec(&json!({"model":model,"answers":answers,"usage":{"input_tokens":null,"output_tokens":null}})).map_err(|_|EvaluationError::Fixture)?;
        jev::parse_response(&bytes, request, jev::Limits::default())
            .map_err(|_| EvaluationError::Protocol)
    }
    fn fixture_image(&self, page: u32) -> Option<&EvaluationImage> {
        self.fixture.images.get(&page)
    }
    fn backend(&self) -> &'static str {
        "synthetic_fixture"
    }
}

#[derive(Clone)]
pub struct MonotonicClock {
    started: Instant,
}
impl Default for MonotonicClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}
impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn sleep_ms(&mut self, duration: u64) {
        std::thread::sleep(std::time::Duration::from_millis(duration));
    }
}
pub const REMOTE_BOUNDARY: &str = "voltfill_jev_image_boundary";
type ManyExecutor<T, C, O> =
    for<'a> fn(&mut RemoteEvaluator<T, C, O>, &[EvaluationCall<'a>]) -> Vec<EvaluationOutcome>;
/// One top-level evaluator owns its session controller. Controller clones are for its
/// scoped workers, not independently configured evaluators: wave allowance updates
/// are serialized by this owner's mutable API. Cloned clocks must share one timeline.
pub struct RemoteEvaluator<
    T = jev::http::HttpTransport,
    C = MonotonicClock,
    O = transport::NoopObserver,
> {
    pub transport: T,
    pub clock: C,
    pub budget: transport::AttemptBudget,
    pub observer: O,
    secret: Arc<Secret>,
    policy: Policy,
    usage: UsageSummary,
    config: transport::ExecutionConfig,
    many: Option<ManyExecutor<T, C, O>>,
    responses: Arc<Mutex<Vec<JevResponse>>>,
}
impl RemoteEvaluator {
    /// Compatibility name for [`Self::try_new`].
    pub fn new(
        endpoint: jev::http::Endpoint,
        secret: Secret,
        allow_remote: bool,
        allow_preview_images: bool,
    ) -> Result<Self, EvaluationError> {
        Self::try_new(endpoint, secret, allow_remote, allow_preview_images)
    }

    /// Build remote evaluation with explicit send and image permissions; does not send a request.
    pub fn try_new(
        endpoint: jev::http::Endpoint,
        secret: Secret,
        allow_remote: bool,
        allow_preview_images: bool,
    ) -> Result<Self, EvaluationError> {
        Self::with_components(
            secret,
            allow_remote,
            allow_preview_images,
            jev::http::HttpTransport::try_new(endpoint).map_err(|_| EvaluationError::Transport)?,
            MonotonicClock {
                started: Instant::now(),
            },
            transport::AttemptBudget::default(),
            transport::NoopObserver,
        )?
        .with_execution_config(&transport::ExecutionConfig::default())
    }
}
impl<T: transport::Transport, C: Clock, O: transport::AttemptObserver> RemoteEvaluator<T, C, O> {
    pub fn with_components(
        secret: impl Into<Arc<Secret>>,
        allow_remote: bool,
        allow_preview_images: bool,
        transport: T,
        clock: C,
        budget: transport::AttemptBudget,
        observer: O,
    ) -> Result<Self, EvaluationError> {
        Ok(Self {
            transport,
            clock,
            budget,
            observer,
            secret: secret.into(),
            policy: Policy {
                allow_remote,
                allow_preview_images,
                ..Default::default()
            },
            usage: Default::default(),
            config: transport::ExecutionConfig::default(),
            many: None,
            responses: Default::default(),
        })
    }
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Result<Self, EvaluationError> {
        if !(1..=3).contains(&max_attempts) {
            return Err(EvaluationError::Protocol);
        }
        self.policy.max_attempts = max_attempts;
        self.config.max_attempts = max_attempts;
        Ok(self)
    }
}
impl<
    T: transport::Transport + Clone + Send,
    C: Clock + Clone + Send,
    O: transport::AttemptObserver + Send,
> RemoteEvaluator<T, C, O>
{
    pub fn with_max_concurrency(mut self, max: usize) -> Result<Self, EvaluationError> {
        if max == 0 {
            return Err(EvaluationError::Protocol);
        }
        self.config.max_concurrency = max;
        self.budget
            .set_worker_limit(max)
            .map_err(|_| EvaluationError::Protocol)?;
        self.many = Some(parallel_many::<T, C, O>);
        Ok(self)
    }
    pub fn with_execution_config(
        mut self,
        config: &transport::ExecutionConfig,
    ) -> Result<Self, EvaluationError> {
        config.validate().map_err(|_| EvaluationError::Protocol)?;
        self.budget
            .configure(config)
            .map_err(|_| EvaluationError::Protocol)?;
        self.config = config.clone();
        self.policy = Policy {
            max_attempts: config.max_attempts,
            max_total_body_bytes: config.max_total_body_bytes,
            total_deadline_ms: config.total_deadline_ms,
            attempt_timeout_ms: config.attempt_timeout_ms,
            initial_backoff_ms: config.initial_backoff_ms,
            backoff_multiplier: config.backoff_multiplier,
            max_backoff_ms: config.max_backoff_ms,
            jitter_ms: config.jitter_ms,
            estimated_input_tokens_per_attempt: config.input_tokens_per_attempt,
            max_estimated_input_tokens: config
                .input_tokens_per_attempt
                .saturating_mul(u64::from(config.max_attempts)),
            ..self.policy
        };
        self.with_max_concurrency(config.max_concurrency)
    }
}
struct UsageObserver<'usage, O> {
    inner: &'usage mut O,
    usage: &'usage mut UsageSummary,
}
impl<O: transport::AttemptObserver> transport::AttemptObserver for UsageObserver<'_, O> {
    fn waiting(&mut self, ms: u64) -> Result<(), transport::Failure> {
        self.inner.waiting(ms)
    }
    fn before_send(
        &mut self,
        body: &jev::multipart::Body,
        budget: &transport::AttemptBudget,
        timeout: u64,
        context: &transport::AttemptContext<'_>,
    ) -> Result<(), transport::Failure> {
        if !self.usage.add(&UsageSummary {
            attempts: 1,
            unknown_usage_attempts: 1,
            ..Default::default()
        }) {
            budget.stop();
            log::error!(target:"voltfill_core::jev","{}",json!({"event":"usage_totals_overflowed","lower_bound":true}));
            return Err(transport::Failure::Budget);
        }
        self.inner.before_send(body, budget, timeout, context)
    }
    fn after_send(
        &mut self,
        attempt: &transport::Attempt,
        response: Option<&Response>,
        budget: &transport::AttemptBudget,
        context: &transport::AttemptContext<'_>,
    ) -> Result<(), transport::Failure> {
        let valid = self.usage.add(&UsageSummary {
            known_input_tokens: attempt.input_tokens.unwrap_or(0),
            known_output_tokens: attempt.output_tokens.unwrap_or(0),
            ..Default::default()
        });
        if attempt.input_tokens.is_some() && attempt.output_tokens.is_some() {
            self.usage.unknown_usage_attempts = self.usage.unknown_usage_attempts.saturating_sub(1);
        }
        let observed = self.inner.after_send(attempt, response, budget, context);
        if !valid {
            budget.stop();
            log::error!(target:"voltfill_core::jev","{}",json!({"event":"usage_totals_overflowed","lower_bound":true}));
            return Err(transport::Failure::Budget);
        }
        observed
    }
}
impl<T: transport::Transport, C: Clock, O: transport::AttemptObserver> Evaluator
    for RemoteEvaluator<T, C, O>
{
    fn take_responses(&mut self) -> Vec<JevResponse> {
        std::mem::take(
            &mut *self
                .responses
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
    fn supports_batching(&self) -> bool {
        true
    }
    fn evaluate(
        &mut self,
        request: &Request,
        images: &[PngImage],
        context: &EvaluationContext<'_>,
    ) -> Result<Response, EvaluationError> {
        let estimate = jev::planning::estimate(request, &self.config.context);
        let policy = Policy {
            max_attempts: self.policy.max_attempts.min(context.remaining_requests),
            total_deadline_ms: self.policy.total_deadline_ms.min(context.remaining_ms),
            estimated_input_tokens_per_attempt: estimate.request_tokens,
            max_estimated_input_tokens: estimate
                .request_tokens
                .saturating_mul(u64::from(self.policy.max_attempts)),
            ..self.policy
        };
        self.usage = Default::default();
        let mut observer = UsageObserver {
            inner: &mut self.observer,
            usage: &mut self.usage,
        };
        let question_ids: Vec<String> = request.draft().questions.keys().cloned().collect();
        let mut recording = transport::RecordingTransport::new(&mut self.transport, |record| {
            self.responses
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(JevResponse {
                    field_id: context.field_id.into(),
                    page: context.page,
                    question_ids: question_ids.clone(),
                    http_status: record.http_status,
                    body: record.body,
                    body_complete: record.body_complete,
                    retry_after_ms: record.retry_after_ms,
                    transport_error: record.transport_error,
                });
        });
        let result = transport::execute_in_scope(
            request,
            images,
            REMOTE_BOUNDARY,
            Some(&self.secret),
            policy,
            self.config.json_limits(),
            &mut recording,
            &mut self.clock,
            &mut self.budget,
            &mut observer,
            transport::RequestScope {
                field_id: context.field_id,
                page: context.page,
            },
        );
        result.result.map_err(|error| match error {
            transport::Failure::Deadline => EvaluationError::Timeout,
            transport::Failure::Protocol(_) => EvaluationError::Protocol,
            _ => EvaluationError::Transport,
        })
    }
    fn evaluate_many(&mut self, calls: &[EvaluationCall<'_>]) -> Vec<EvaluationOutcome> {
        if calls.is_empty() {
            return Vec::new();
        }
        let old = self.budget.snapshot();
        let available = calls
            .iter()
            .map(|call| call.context.remaining_requests)
            .max()
            .unwrap_or(0);
        self.budget.set_limits(
            old.max_posts.min(old.posts.saturating_add(available)),
            old.max_cost_nano_usd,
        );
        let outcomes = if let Some(run) = self.many {
            run(self, calls)
        } else {
            let start = self.clock.now_ms();
            calls
                .iter()
                .map(|call| {
                    let remaining = call
                        .context
                        .remaining_ms
                        .saturating_sub(self.clock.now_ms().saturating_sub(start));
                    let context = EvaluationContext {
                        remaining_ms: remaining,
                        ..call.context
                    };
                    let result = if remaining == 0 {
                        self.usage = Default::default();
                        Err(EvaluationError::Timeout)
                    } else {
                        self.evaluate(call.request, call.images, &context)
                    };
                    EvaluationOutcome {
                        result,
                        usage: self.last_usage(),
                    }
                })
                .collect()
        };
        self.budget.set_limits(old.max_posts, old.max_cost_nano_usd);
        self.usage = Default::default();
        for outcome in &outcomes {
            if !self.usage.add(&outcome.usage) {
                self.budget.stop();
                log::error!(target:"voltfill_core::jev","{}",json!({"event":"usage_totals_overflowed","lower_bound":true}));
            }
        }

        outcomes
    }
    fn last_usage(&self) -> UsageSummary {
        UsageSummary {
            attempts: self.usage.attempts,
            unknown_usage_attempts: self.usage.unknown_usage_attempts,
            known_input_tokens: self.usage.known_input_tokens,
            known_output_tokens: self.usage.known_output_tokens,
            totals_overflowed: self.usage.totals_overflowed,
        }
    }
    fn backend(&self) -> &'static str {
        "jev_remote"
    }
}
struct SharedObserver<'observer, O> {
    inner: Arc<Mutex<&'observer mut O>>,
}
impl<O> Clone for SharedObserver<'_, O> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<O: transport::AttemptObserver> transport::AttemptObserver for SharedObserver<'_, O> {
    fn waiting(&mut self, ms: u64) -> Result<(), transport::Failure> {
        self.inner
            .lock()
            .map_err(|_| transport::Failure::Observer)?
            .waiting(ms)
    }
    fn before_send(
        &mut self,
        body: &jev::multipart::Body,
        budget: &transport::AttemptBudget,
        timeout: u64,
        context: &transport::AttemptContext<'_>,
    ) -> Result<(), transport::Failure> {
        self.inner
            .lock()
            .map_err(|_| transport::Failure::Observer)?
            .before_send(body, budget, timeout, context)
    }
    fn after_send(
        &mut self,
        attempt: &transport::Attempt,
        response: Option<&Response>,
        budget: &transport::AttemptBudget,
        context: &transport::AttemptContext<'_>,
    ) -> Result<(), transport::Failure> {
        self.inner
            .lock()
            .map_err(|_| transport::Failure::Observer)?
            .after_send(attempt, response, budget, context)
    }
}
fn parallel_many<
    T: transport::Transport + Clone + Send,
    C: Clock + Clone + Send,
    O: transport::AttemptObserver + Send,
>(
    remote: &mut RemoteEvaluator<T, C, O>,
    calls: &[EvaluationCall<'_>],
) -> Vec<EvaluationOutcome> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let started = remote.clock.now_ms();
    let next = AtomicUsize::new(0);
    let observer = SharedObserver {
        inner: Arc::new(Mutex::new(&mut remote.observer)),
    };
    let mut indexed = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..remote.config.max_concurrency.min(calls.len()) {
            let mut worker = RemoteEvaluator {
                transport: remote.transport.clone(),
                clock: remote.clock.clone(),
                budget: remote.budget.clone(),
                observer: observer.clone(),
                secret: remote.secret.clone(),
                policy: remote.policy,
                usage: Default::default(),
                config: remote.config.clone(),
                many: None,
                responses: remote.responses.clone(),
            };
            let next = &next;
            // A large configured concurrency can exhaust OS threads; the workers already
            // started drain the shared queue, and with none the caller sees a count mismatch.
            let spawned = std::thread::Builder::new().spawn_scoped(scope, move || {
                let mut rows = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(call) = calls.get(index) else {
                        break;
                    };
                    let remaining = call
                        .context
                        .remaining_ms
                        .saturating_sub(worker.clock.now_ms().saturating_sub(started));
                    let context = EvaluationContext {
                        remaining_ms: remaining,
                        ..call.context
                    };
                    let result = if remaining == 0 {
                        worker.usage = Default::default();
                        Err(EvaluationError::Timeout)
                    } else {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            worker.evaluate(call.request, call.images, &context)
                        })) {
                            Ok(result) => result,
                            Err(_) => {
                                worker.budget.stop();
                                Err(EvaluationError::Transport)
                            }
                        }
                    };
                    rows.push((
                        index,
                        EvaluationOutcome {
                            result,
                            usage: worker.last_usage(),
                        },
                    ));
                }
                rows
            });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(_) => break,
            }
        }
        let mut rows = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(result) => rows.extend(result),
                Err(_) => remote.budget.stop(),
            }
        }
        rows
    });
    indexed.sort_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, outcome)| outcome).collect()
}

/// Only semantic numeric fields are inspected by the engine. Response confidence
/// remains an uncalibrated model statistic, never a probability of PDF fidelity.
pub(crate) fn confidence(answer: &Answer) -> Option<f64> {
    match answer {
        Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } => Some(*confidence),
        Answer::Box { confidence, .. } => confidence.0,
        Answer::Noul { .. } => None,
    }
}

#[cfg(test)]
mod concurrent_tests {
    use super::*;
    use crate::jev::transport::{
        AttemptBudget, HttpRequest, HttpResponse, Transport, TransportError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    #[test]
    fn engine_error_returns_retained_responses_to_caller() {
        let mut remote = remote(false, 1);
        remote
            .evaluate(
                &request(),
                &[],
                &EvaluationContext {
                    field_id: "first",
                    page: 1,
                    remaining_requests: 1,
                    remaining_ms: 30_000,
                },
            )
            .expect("the scripted request must complete before testing extraction failure");
        let form = crate::schema::parse(include_bytes!("../../../examples/text-form.json"))
            .expect("the checked-in text form satisfies the schema");
        let error = crate::engine::extract(
            b"invalid PDF",
            &form,
            &crate::engine::ExtractOptions::default(),
            &mut remote,
        )
        .expect_err("invalid PDF input must fail while retaining received responses");
        let records = error.jev_responses();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].http_status, Some(200));
        let Some(crate::model::ResponseBody::Text(body)) = &records[0].body else {
            panic!("original response missing from error");
        };
        let value: Value = serde_json::from_str(body)
            .expect("the original successful response must retain valid JSON");
        assert_eq!(value["answers"]["q"]["noul"], 1.0);
        assert!(remote.take_responses().is_empty());
    }
    #[test]
    fn original_response_survives_protocol_errors_and_non_utf8() {
        struct Original(Vec<u8>);
        impl Transport for Original {
            fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
                Ok(HttpResponse {
                    body_error: None,
                    status: 200,
                    body: self.0.clone(),
                    retry_after_ms: None,
                })
            }
        }
        for bytes in [
            br#"{ "answers":{"q":{"type":"box","box":{"left":0.1,"top":0.2,"right":0.3,"bottom":0.4},"confidence":0.87}},"unknown":true }"#.to_vec(),
            br#"{"confidence":null,"duplicate":1,"duplicate":2}"#.to_vec(),
            b"not JSON\n\x1b[31m".to_vec(),
            vec![0xff,0xfe,0x00],
        ] {
            let mut remote = RemoteEvaluator::with_components(
                Secret::try_new("synthetic-not-real".into()).expect("the synthetic bearer contains only permitted characters"),true,false,
                Original(bytes.clone()),MonotonicClock { started:Instant::now() },
                AttemptBudget::default(),transport::NoopObserver,
            ).expect("the original-body transport must initialize the evaluator").with_max_attempts(1).expect("one attempt is a valid retry limit");
            let _ = remote.evaluate(&request(),&[],&EvaluationContext {
                field_id:"field",page:1,remaining_requests:1,remaining_ms:30_000,
            });
            let records = remote.take_responses();
            assert_eq!(records.len(),1);
            assert_eq!(records[0].question_ids,["q"]);
            let original = match records[0].body.as_ref().expect("every received response must retain its original body") {
                crate::model::ResponseBody::Text(text) => text.as_bytes(),
                crate::model::ResponseBody::Bytes(bytes) => bytes.as_slice(),
            };
            assert_eq!(original,bytes);
            let encoded = serde_json::to_string(&records[0]).expect("text and binary response records must serialize as JSON");
            assert!(!encoded.contains('\n'));
            assert!(!encoded.contains('\x1b'));
            assert!(remote.take_responses().is_empty());
        }
    }
    #[derive(Clone)]
    struct Wire {
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        barrier: Option<Arc<Barrier>>,
        output: u64,
    }
    impl Transport for Wire {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            if let Some(barrier) = &self.barrier
                && index < barrier_count(self)
            {
                barrier.wait();
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(HttpResponse { body_error: None,status:200,body:serde_json::to_vec(&json!({"model":jev::MODEL,"answers":{"q":{"type":"noul","noul":1.0}},"usage":{"input_tokens":10,"output_tokens":if index==0{self.output}else{1}}})).expect("the scripted usage response must serialize as JSON"),retry_after_ms:None})
        }
    }
    fn barrier_count(wire: &Wire) -> usize {
        if wire.barrier.is_some() { 2 } else { 0 }
    }
    #[derive(Default)]
    struct Audit {
        rows: Vec<(String, u32, u32)>,
    }
    impl transport::AttemptObserver for Audit {
        fn before_send(
            &mut self,
            _: &jev::multipart::Body,
            _: &AttemptBudget,
            _: u64,
            context: &transport::AttemptContext<'_>,
        ) -> Result<(), transport::Failure> {
            self.rows
                .push((context.field_id.into(), context.page, context.slot));
            Ok(())
        }
    }
    fn request() -> Request {
        jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"present"}}}"#,jev::Limits::default()).expect("the synthetic noul request satisfies the wire contract")
    }
    fn calls<'a>(request: &'a Request, remaining: u32, ms: u64) -> Vec<EvaluationCall<'a>> {
        vec![
            EvaluationCall {
                request,
                images: &[],
                context: EvaluationContext {
                    field_id: "first",
                    page: 1,
                    remaining_requests: remaining,
                    remaining_ms: ms,
                },
            },
            EvaluationCall {
                request,
                images: &[],
                context: EvaluationContext {
                    field_id: "second",
                    page: 2,
                    remaining_requests: remaining.saturating_sub(1),
                    remaining_ms: ms,
                },
            },
        ]
    }
    fn remote(barrier: bool, output: u64) -> RemoteEvaluator<Wire, MonotonicClock, Audit> {
        RemoteEvaluator::with_components(
            Secret::try_new("synthetic-only".into())
                .expect("the synthetic bearer contains only permitted characters"),
            true,
            false,
            Wire {
                active: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
                calls: Arc::new(AtomicUsize::new(0)),
                barrier: barrier.then(|| Arc::new(Barrier::new(2))),
                output,
            },
            MonotonicClock {
                started: Instant::now(),
            },
            AttemptBudget::default(),
            Audit::default(),
        )
        .expect("the synchronized transport must initialize the evaluator")
        .with_max_concurrency(2)
        .expect("two workers are a valid concurrency limit")
    }
    fn set_input_quota<T, C, O>(remote: &RemoteEvaluator<T, C, O>, tokens: u64) {
        let old = remote.budget.snapshot();
        let mut config = remote.config.clone();
        config.input_tokens_per_second = tokens;
        config.organization.input_tokens_per_second =
            config.organization.input_tokens_per_second.max(tokens);
        config.input_tokens_per_attempt = config.input_tokens_per_attempt.min(tokens);
        remote
            .budget
            .configure(&config)
            .expect("the input quota remains within its matching organization allocation");
        remote
            .budget
            .set_limits(old.max_posts, old.max_cost_nano_usd);
    }
    #[test]
    fn worker_count_accepts_any_positive_value() {
        assert!(remote(false, 1).with_max_concurrency(0).is_err());
        let mut remote = remote(false, 1)
            .with_max_concurrency(usize::MAX)
            .expect("any positive worker limit must be accepted");
        set_input_quota(&remote, 128_000);
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 2, 30_000));
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.result.is_ok()));
    }
    #[test]
    fn actual_workers_overlap_under_sufficient_test_allocation_and_keep_scopes_separate() {
        let mut remote = remote(true, 1);
        set_input_quota(&remote, 128_000);
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 2, 30_000));
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.result.is_ok()));
        assert_eq!(remote.transport.peak.load(Ordering::SeqCst), 2);
        assert_eq!(remote.budget.snapshot().posts, 2);
        assert_eq!(remote.last_usage().attempts, 2);
        let mut responses = remote.take_responses();
        responses.sort_by_key(|response| response.page);
        assert_eq!(responses.len(), 2);
        assert_eq!(
            (responses[0].field_id.as_str(), responses[0].page),
            ("first", 1)
        );
        assert_eq!(
            (responses[1].field_id.as_str(), responses[1].page),
            ("second", 2)
        );
        let mut rows = remote.observer.rows;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            rows.iter().map(|row| (&*row.0, row.1)).collect::<Vec<_>>(),
            [("first", 1), ("second", 2)]
        );
        assert_ne!(rows[0].2, rows[1].2);
    }
    #[test]
    fn token_quota_serializes_two_actual_request_reservations() {
        let mut remote = remote(false, 1);
        let request = request();
        set_input_quota(
            &remote,
            jev::planning::estimate(&request, &Default::default()).request_tokens,
        );
        let start = Instant::now();
        let results = remote.evaluate_many(&calls(&request, 2, 30_000));
        assert!(results.iter().all(|result| result.result.is_ok()));
        assert_eq!(remote.transport.peak.load(Ordering::SeqCst), 1);
        assert!(start.elapsed().as_millis() >= 2000);
        assert_eq!(remote.transport.calls.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn concurrent_usage_overflow_is_flagged_as_lower_bound_and_stops_future_admission() {
        let mut remote = remote(true, u64::MAX);
        set_input_quota(&remote, 128_000);
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 2, 30_000));
        assert!(results.iter().all(|result| result.result.is_ok()));
        let usage = remote.last_usage();
        assert_eq!(usage.known_input_tokens, 20);
        assert_eq!(usage.known_output_tokens, u64::MAX);
        assert!(usage.totals_overflowed);
        assert_eq!(remote.budget.snapshot().known_cost, 840);
        assert_eq!(remote.budget.snapshot().reserved, 0);
        assert!(remote.budget.snapshot().stopped);
        assert!(
            remote
                .evaluate(
                    &request,
                    &[],
                    &EvaluationContext {
                        field_id: "later",
                        page: 3,
                        remaining_requests: 3,
                        remaining_ms: 30_000
                    }
                )
                .is_err()
        );
        assert_eq!(remote.transport.calls.load(Ordering::SeqCst), 2);
    }
    #[derive(Clone, Default)]
    struct AtomicClock(Arc<std::sync::atomic::AtomicU64>);
    impl Clock for AtomicClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }
    #[derive(Clone)]
    struct SlowWire {
        clock: AtomicClock,
        calls: Arc<AtomicUsize>,
    }
    impl Transport for SlowWire {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.clock.0.fetch_add(100, Ordering::SeqCst);
            Err(TransportError::OutcomeUnknown)
        }
    }
    #[test]
    fn queued_calls_use_original_wave_deadline_not_a_fresh_timeout() {
        let clock = AtomicClock::default();
        let count = Arc::new(AtomicUsize::new(0));
        let mut remote = RemoteEvaluator::with_components(
            Secret::try_new("synthetic".into())
                .expect("the synthetic bearer contains only permitted characters"),
            true,
            false,
            SlowWire {
                clock: clock.clone(),
                calls: count.clone(),
            },
            clock,
            AttemptBudget::default(),
            transport::NoopObserver,
        )
        .expect("the deadline-test transport must initialize the evaluator")
        .with_max_concurrency(1)
        .expect("one worker is a valid concurrency limit");
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 2, 31));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.result.is_err()));
        assert_eq!(results[1].usage.attempts, 0);
    }
    #[derive(Clone)]
    struct AlwaysTransient(Arc<AtomicUsize>);
    impl Transport for AlwaysTransient {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                body_error: None,
                status: 503,
                body: Vec::new(),
                retry_after_ms: None,
            })
        }
    }
    #[test]
    fn shared_wave_request_limit_bounds_all_concurrent_retries() {
        let clock = AtomicClock::default();
        let count = Arc::new(AtomicUsize::new(0));
        let mut remote = RemoteEvaluator::with_components(
            Secret::try_new("synthetic".into())
                .expect("the synthetic bearer contains only permitted characters"),
            true,
            false,
            AlwaysTransient(count.clone()),
            clock,
            AttemptBudget::default(),
            transport::NoopObserver,
        )
        .expect("the retry-test transport must initialize the evaluator")
        .with_max_concurrency(2)
        .expect("two workers are a valid concurrency limit");
        set_input_quota(&remote, 128_000);
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 3, 30_000));
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.result.is_err()));
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(remote.budget.snapshot().posts, 3);
        assert_eq!(remote.last_usage().attempts, 3);
    }
    #[test]
    fn worker_panic_retains_reserved_cost_and_stops_shared_admission() {
        #[derive(Clone)]
        struct PanicWire;
        impl Transport for PanicWire {
            fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
                panic!("synthetic worker failure")
            }
        }
        let mut remote = RemoteEvaluator::with_components(
            Secret::try_new("synthetic".into())
                .expect("the synthetic bearer contains only permitted characters"),
            true,
            false,
            PanicWire,
            MonotonicClock {
                started: Instant::now(),
            },
            AttemptBudget::default(),
            transport::NoopObserver,
        )
        .expect("the panic-test transport must initialize before execution")
        .with_max_concurrency(1)
        .expect("one worker is a valid concurrency limit");
        let request = request();
        let results = remote.evaluate_many(&calls(&request, 2, 30_000));
        assert!(results.iter().all(|result| result.result.is_err()));
        assert!(remote.budget.snapshot().stopped);
        assert_eq!(remote.budget.snapshot().posts, 1);
        assert_eq!(
            remote.budget.snapshot().reserved,
            jev::planning::estimate(&request, &Default::default()).request_tokens * 42
        );
        assert_eq!(remote.last_usage().attempts, 1);
    }
    #[test]
    fn interrupted_http_bodies_preserve_status_wait_and_prefix() {
        struct Partial(transport::TransportError);
        impl transport::Transport for Partial {
            fn send(
                &mut self,
                _: transport::HttpRequest<'_>,
            ) -> Result<transport::HttpResponse, transport::TransportError> {
                Ok(transport::HttpResponse {
                    status: 502,
                    body: b"partial response".to_vec(),
                    body_error: Some(self.0),
                    retry_after_ms: Some(700),
                })
            }
        }
        for failure in [
            transport::TransportError::OutcomeUnknown,
            transport::TransportError::ResponseTooLarge,
        ] {
            let mut remote = RemoteEvaluator::with_components(
                Secret::try_new("synthetic-key".into())
                    .expect("the synthetic bearer contains only permitted characters"),
                true,
                false,
                Partial(failure),
                MonotonicClock::default(),
                AttemptBudget::default(),
                transport::NoopObserver,
            )
            .expect("the incomplete-body transport must initialize the evaluator")
            .with_max_attempts(1)
            .expect("one attempt is a valid retry limit");
            assert!(
                remote
                    .evaluate(
                        &request(),
                        &[],
                        &EvaluationContext {
                            field_id: "field",
                            page: 1,
                            remaining_requests: 3,
                            remaining_ms: 30_000
                        }
                    )
                    .is_err()
            );
            let records = remote.take_responses();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].http_status, Some(502));
            assert_eq!(records[0].retry_after_ms, Some(700));
            assert_eq!(records[0].transport_error, Some(failure));
            assert!(!records[0].body_complete);
            assert_eq!(
                serde_json::to_value(&records[0])
                    .expect("an incomplete response record must serialize without losing its body")
                    ["body"],
                "partial response"
            );
        }
    }
}
