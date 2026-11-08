//! Serial execution boundary. The opt-in HTTP implementation is in `super::http`.
//! A real transport must enforce TLS verification, no redirects, streaming response
//! byte limits and the supplied timeout before reading the full response body.
use super::multipart::{self, Body, PngImage};
use super::planning::{self, ContextLimits};
use super::{Asset, Content, Error, Limits, Request, Response, ResponseBody};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// Caller-injected bearer value whose Debug representation is always redacted.
pub struct Secret(String);
impl Secret {
    /// Compatibility name for [`Self::try_new`].
    pub fn new(value: String) -> Result<Self, Failure> {
        Self::try_new(value)
    }

    /// Validate a caller-supplied bearer value without reading the environment.
    pub fn try_new(value: String) -> Result<Self, Failure> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Failure::Credentials);
        }
        Ok(Self(value))
    }
    /// Only the transport should expose this to an Authorization header.
    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// One bounded transport attempt; the transport must enforce both timeout and body limits.
pub struct HttpRequest<'request> {
    pub secret: &'request Secret,
    pub body: &'request Body,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
}
/// Received HTTP status and original body, including a prefix when reading fails.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Received prefix and metadata are retained when reading the body fails.
    pub body_error: Option<TransportError>,
    /// Delta in milliseconds, parsed by the transport from Retry-After.
    /// HTTP date form requires a wall-clock-aware transport; never ignore it.
    pub retry_after_ms: Option<u64>,
}
/// Safe failure categories; uncertainty after sending must not be reported as BeforeSend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TransportError {
    BeforeSend,
    OutcomeUnknown,
    ResponseTooLarge,
}
/// Injection boundary for a real bounded HTTP implementation or a synthetic test transport.
/// Return received prefixes through HttpResponse even when the body is incomplete.
pub trait Transport {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError>;
}
/// A credential-free record emitted before response parsing or observer callbacks.
#[derive(Debug, serde::Serialize)]
pub struct ResponseRecord {
    pub http_status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub transport_error: Option<TransportError>,
    pub body: Option<ResponseBody>,
    pub body_complete: bool,
}

/// Calls the sink once per send, before parsing or observer callbacks. The sink
/// receives original body bytes and safe outcome metadata, never headers or secrets.
/// Records arrive in send-completion order; the caller can attach its own context.
pub struct RecordingTransport<'transport, T, F> {
    inner: &'transport mut T,
    sink: F,
}
impl<'transport, T, F: FnMut(ResponseRecord)> RecordingTransport<'transport, T, F> {
    #[must_use]
    pub fn new(inner: &'transport mut T, sink: F) -> Self {
        Self { inner, sink }
    }
}
impl<T: Transport, F: FnMut(ResponseRecord)> Transport for RecordingTransport<'_, T, F> {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        let result = self.inner.send(request);
        let (http_status, body, retry_after_ms, transport_error) = match &result {
            Ok(response) => (
                Some(response.status),
                Some(response.body.clone().into()),
                response.retry_after_ms,
                response.body_error,
            ),
            Err(error) => (None, None, None, Some(*error)),
        };
        (self.sink)(ResponseRecord {
            http_status,
            body_complete: body.is_some() && transport_error.is_none(),
            body,
            retry_after_ms,
            transport_error,
        });
        result
    }
}

/// Monotonic time, bounded waiting, and cancellation used by admission and retry logic.
pub trait Clock {
    /// Monotonic milliseconds, relative to an arbitrary origin.
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, duration: u64);
    fn cancelled(&self) -> bool {
        false
    }
}
/// Per-execution permission, timeout, retry, and reservation limits.
/// Remote sends and preview images are disabled by default.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub allow_remote: bool,
    pub allow_preview_images: bool,
    pub max_attempts: u32,
    pub total_deadline_ms: u64,
    pub attempt_timeout_ms: u64,
    pub max_total_body_bytes: u64,
    pub estimated_input_tokens_per_attempt: u64,
    pub max_estimated_input_tokens: u64,
    pub initial_backoff_ms: u64,
    pub backoff_multiplier: u32,
    pub max_backoff_ms: u64,
    /// Bounded caller-supplied jitter, kept deterministic in offline tests.
    pub jitter_ms: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_remote: false,
            allow_preview_images: false,
            max_attempts: 3,
            total_deadline_ms: 30_000,
            attempt_timeout_ms: 10_000,
            max_total_body_bytes: 32 * 1024 * 1024,
            estimated_input_tokens_per_attempt: 64_000,
            max_estimated_input_tokens: 192_000,
            initial_backoff_ms: 250,
            backoff_multiplier: 2,
            max_backoff_ms: 10_000,
            jitter_ms: 0,
        }
    }
}
/// Execution failure without provider text, HTTP headers, or authentication material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Disabled,
    Credentials,
    PreviewDisabled,
    Config,
    Deadline,
    Cancelled,
    Budget,
    Redirect,
    Http(u16),
    Transport(TransportError),
    Protocol(Error),
    Observer,
    /// Image preparation failed before sending.
    ImagesUnavailable,
}
pub const INPUT_RESERVATION: u64 = 64_000;
pub const ATTEMPT_RESERVATION_NANO_USD: u64 = INPUT_RESERVATION * 42;
// Provider limits are per second, so a completed attempt's tokens are held for at least
// that long before they stop counting against the next admission. One second leaves no
// margin for how the provider aligns its window, so each 429 multiplies the gap a budget
// holds by `completion_gap_backoff`, up to `max_completion_gap_ms`.
pub const MIN_COMPLETION_GAP_MS: u64 = 1_000;
pub const DEFAULT_COMPLETION_GAP_MS: u64 = 2_000;
pub const DEFAULT_MAX_COMPLETION_GAP_MS: u64 = 60_000;
pub const SESSION_DEADLINE_MS: u64 = 600_000;
/// Organization-wide admission ceilings shared by every worker using one AttemptBudget.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrganizationLimits {
    pub requests_per_second: u32,
    pub input_tokens_per_second: u64,
}
impl Default for OrganizationLimits {
    fn default() -> Self {
        Self {
            requests_per_second: 80,
            input_tokens_per_second: 100_000,
        }
    }
}
/// Serializable execution limits; validate before applying them to a shared budget.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionConfig {
    pub max_attempts: u32,
    pub max_concurrency: usize,
    pub max_batch_questions: usize,
    pub max_requests: u32,
    pub total_deadline_ms: u64,
    pub attempt_timeout_ms: u64,
    pub initial_backoff_ms: u64,
    pub backoff_multiplier: u32,
    pub max_backoff_ms: u64,
    pub jitter_ms: u64,
    pub max_cost_nano_usd: u64,
    /// Estimated cost per input token; not a provider billing guarantee.
    pub input_token_price_nano_usd: u64,
    pub completion_gap_ms: u64,
    /// Factor applied to the gap on a 429; 1 keeps the gap fixed.
    pub completion_gap_backoff: u32,
    pub max_completion_gap_ms: u64,
    pub max_json_bytes: usize,
    pub max_total_body_bytes: u64,
    pub requests_per_second: u32,
    pub input_tokens_per_second: u64,
    pub input_tokens_per_attempt: u64,
    pub context: ContextLimits,
    pub organization: OrganizationLimits,
}
impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_concurrency: 2,
            max_batch_questions: 32,
            max_requests: 128,
            total_deadline_ms: 30_000,
            attempt_timeout_ms: 10_000,
            initial_backoff_ms: 250,
            backoff_multiplier: 2,
            max_backoff_ms: 10_000,
            jitter_ms: 250,
            max_cost_nano_usd: 200_000_000,
            input_token_price_nano_usd: 42,
            completion_gap_ms: DEFAULT_COMPLETION_GAP_MS,
            completion_gap_backoff: 2,
            max_completion_gap_ms: DEFAULT_MAX_COMPLETION_GAP_MS,
            max_json_bytes: Limits::default().max_bytes,
            max_total_body_bytes: 32 * 1024 * 1024,
            requests_per_second: 80,
            input_tokens_per_second: 100_000,
            input_tokens_per_attempt: 64_000,
            context: ContextLimits::default(),
            organization: OrganizationLimits::default(),
        }
    }
}
impl ExecutionConfig {
    /// The gap after a 429: exponential backoff from the gap in use.
    #[must_use]
    pub fn backed_off_gap(&self, gap_ms: u64) -> u64 {
        gap_ms
            .saturating_mul(u64::from(self.completion_gap_backoff))
            .min(self.max_completion_gap_ms)
    }
    #[must_use]
    pub fn json_limits(&self) -> Limits {
        Limits {
            max_bytes: self.max_json_bytes,
            ..Limits::default()
        }
    }
    pub fn validate(&self) -> Result<(), Failure> {
        if !(1..=3).contains(&self.max_attempts)
            || self.max_concurrency == 0
            || !(1..=255).contains(&self.max_batch_questions)
            || !(1..=10_000).contains(&self.max_requests)
            || self.total_deadline_ms == 0
            || self.total_deadline_ms > 3_600_000
            || self.attempt_timeout_ms == 0
            || self.attempt_timeout_ms > self.total_deadline_ms
            || self.initial_backoff_ms > 60_000
            || !(2..=16).contains(&self.backoff_multiplier)
            || self.max_backoff_ms < self.initial_backoff_ms
            || self.max_backoff_ms > 60_000
            || self.jitter_ms > 10_000
            || self.jitter_ms > self.max_backoff_ms
            || self.max_cost_nano_usd == 0
            || self.max_cost_nano_usd > 100_000_000_000
            || !(1..=1_000_000_000).contains(&self.input_token_price_nano_usd)
            || self.completion_gap_ms < MIN_COMPLETION_GAP_MS
            || !(self.completion_gap_ms..=SESSION_DEADLINE_MS).contains(&self.max_completion_gap_ms)
            || !(1..=16).contains(&self.completion_gap_backoff)
            || !(1..=Limits::default().max_bytes).contains(&self.max_json_bytes)
            || !(1..=32 * 1024 * 1024).contains(&self.max_total_body_bytes)
            || self.organization.requests_per_second == 0
            || self.organization.input_tokens_per_second == 0
            || self.requests_per_second == 0
            || self.requests_per_second > self.organization.requests_per_second
            || self.input_tokens_per_second == 0
            || self.input_tokens_per_second > self.organization.input_tokens_per_second
            || self.input_tokens_per_attempt == 0
            || self.input_tokens_per_attempt > 1_000_000
            || self.input_tokens_per_attempt > self.input_tokens_per_second
            || !self.context.validate()
        {
            return Err(Failure::Config);
        }
        Ok(())
    }
}
#[derive(Debug, serde::Serialize)]
pub struct InputSummary {
    pub input_kind: &'static str,
    pub question_count: usize,
    pub image_dimensions: Vec<(u32, u32)>,
    pub image_sha256: Vec<String>,
}
#[must_use]
pub fn input_summary(request: &Request, images: &[PngImage]) -> InputSummary {
    fn text(content: &Content, references: &BTreeMap<String, Asset>) -> bool {
        match content {
            Content::Text(value) => !value.trim().is_empty() && !references.contains_key(value),
            Content::Array(values) => values.iter().any(|value| text(value, references)),
            Content::Object(values) => values.values().any(|value| text(value, references)),
            Content::Null => false,
            _ => true,
        }
    }
    InputSummary {
        input_kind: if images.is_empty() {
            "text"
        } else if text(&request.draft().state, &request.draft().assets) {
            "both"
        } else {
            "image"
        },
        question_count: request.draft().questions.len(),
        image_dimensions: images.iter().map(PngImage::dimensions).collect(),
        image_sha256: images.iter().map(PngImage::sha256).collect(),
    }
}

/// Immutable accounting view. Dollar reservations outlive uncertain outcomes.
#[derive(Debug, Clone, Copy)]
pub struct BudgetSnapshot {
    pub posts: u32,
    pub reserved: u64,
    pub known_cost: u64,
    pub last_completion: Option<u64>,
    pub session_started_ms: Option<u64>,
    pub stopped: bool,
    pub max_posts: u32,
    pub max_cost_nano_usd: u64,
}
impl Default for BudgetSnapshot {
    fn default() -> Self {
        Self {
            posts: 0,
            reserved: 0,
            known_cost: 0,
            last_completion: None,
            session_started_ms: None,
            stopped: false,
            max_posts: 128,
            max_cost_nano_usd: 200_000_000,
        }
    }
}
struct TokenReservation {
    id: u32,
    tokens: u64,
    expires: Option<u64>,
    committed: bool,
}
struct BudgetState {
    value: BudgetSnapshot,
    config: ExecutionConfig,
    tokens: Vec<TokenReservation>,
    starts: VecDeque<u64>,
    cooldown_until: u64,
    gap_ms: u64,
    /// Until when the last doubled gap is held. Workers in flight together get
    /// their 429s together; those are one signal and double the gap once.
    backoff_until: u64,
    rng: u64,
}
#[derive(Clone)]
pub struct AttemptBudget {
    state: Arc<Mutex<BudgetState>>,
}
impl std::fmt::Debug for AttemptBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptBudget")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}
impl Default for AttemptBudget {
    fn default() -> Self {
        Self::from_snapshot(BudgetSnapshot::default())
    }
}
impl AttemptBudget {
    #[must_use]
    pub fn from_snapshot(value: BudgetSnapshot) -> Self {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );
        let config = ExecutionConfig {
            max_requests: value.max_posts,
            max_cost_nano_usd: value.max_cost_nano_usd,
            total_deadline_ms: SESSION_DEADLINE_MS,
            ..Default::default()
        };
        Self {
            state: Arc::new(Mutex::new(BudgetState {
                value,
                config,
                tokens: Vec::new(),
                starts: Default::default(),
                cooldown_until: value
                    .last_completion
                    .map_or(0, |at| at.saturating_add(DEFAULT_COMPLETION_GAP_MS)),
                gap_ms: DEFAULT_COMPLETION_GAP_MS,
                backoff_until: 0,
                rng: hasher.finish(),
            })),
        }
    }
    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state.value.stopped = true;
            state
        })
    }
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        self.lock().value
    }
    /// Permanently stop admission for all clones of this shared budget.
    pub fn stop(&self) {
        self.lock().value.stopped = true;
    }
    pub fn set_limits(&self, max_posts: u32, max_cost_nano_usd: u64) {
        let mut s = self.lock();
        s.value.max_posts = max_posts;
        s.value.max_cost_nano_usd = max_cost_nano_usd;
    }
    /// Set a nonzero shared concurrency ceiling without resetting usage or reservations.
    pub fn set_worker_limit(&self, max: usize) -> Result<(), Failure> {
        if max == 0 {
            return Err(Failure::Config);
        }
        let mut s = self.lock();
        s.config.max_concurrency = max;
        Ok(())
    }
    /// Apply validated limits only before any posts or token reservations exist.
    /// Previously recorded completion time still constrains the earliest next admission.
    pub fn configure(&self, config: &ExecutionConfig) -> Result<(), Failure> {
        config.validate()?;
        let mut s = self.lock();
        if s.value.posts != 0 || !s.tokens.is_empty() {
            return Err(Failure::Config);
        }
        s.cooldown_until = s.cooldown_until.max(
            s.value
                .last_completion
                .map_or(0, |at| at.saturating_add(config.completion_gap_ms)),
        );
        s.config = config.clone();
        s.gap_ms = config.completion_gap_ms;
        s.value.max_posts = config.max_requests;
        s.value.max_cost_nano_usd = config.max_cost_nano_usd;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_test_input_quota(&self, tokens: u64) {
        self.lock().config.input_tokens_per_second = tokens;
    }
    fn available_state(s: &BudgetState, tokens: u64) -> Result<(), Failure> {
        if s.value.stopped {
            return Err(Failure::Cancelled);
        }
        if tokens == 0 || tokens > s.config.input_tokens_per_attempt {
            return Err(Failure::Config);
        }
        if s.value.posts >= s.value.max_posts
            || s.value
                .known_cost
                .saturating_add(s.value.reserved)
                .saturating_add(tokens.saturating_mul(s.config.input_token_price_nano_usd))
                > s.value.max_cost_nano_usd
        {
            return Err(Failure::Budget);
        }
        Ok(())
    }
    fn available(&self, tokens: u64) -> Result<(), Failure> {
        Self::available_state(&self.lock(), tokens)
    }
    fn session_deadline(&self, now: u64) -> u64 {
        let mut s = self.lock();
        let start = *s.value.session_started_ms.get_or_insert(now);
        start.saturating_add(s.config.total_deadline_ms)
    }
    fn admission(&self, now: u64, reservation: u64) -> Result<Result<u32, u64>, Failure> {
        let mut s = self.lock();
        Self::available_state(&s, reservation)?;
        s.tokens
            .retain(|token| token.expires.is_none_or(|expires| expires > now));
        while s
            .starts
            .front()
            .is_some_and(|at| at.saturating_add(1000) <= now)
        {
            s.starts.pop_front();
        }
        let mut wait = s.cooldown_until.saturating_sub(now);
        if s.starts.len() >= s.config.requests_per_second as usize {
            wait = wait.max(s.starts[0].saturating_add(1000).saturating_sub(now));
        }
        let tokens = s
            .tokens
            .iter()
            .fold(0u64, |sum, t| sum.saturating_add(t.tokens));
        let active = s
            .tokens
            .iter()
            .filter(|token| token.expires.is_none())
            .count();
        if active >= s.config.max_concurrency
            || tokens.saturating_add(reservation) > s.config.input_tokens_per_second
        {
            wait = wait.max(
                s.tokens
                    .iter()
                    .filter_map(|token| token.expires)
                    .min()
                    .map_or(25, |at| at.saturating_sub(now).max(1)),
            );
        }
        if wait > 0 {
            return Ok(Err(wait));
        }
        s.value.posts += 1;
        let id = s.value.posts;
        let tokens = reservation;
        s.value.reserved += tokens * s.config.input_token_price_nano_usd;
        s.tokens.push(TokenReservation {
            id,
            tokens,
            expires: None,
            committed: false,
        });
        Ok(Ok(id))
    }
    #[cfg(test)]
    fn reserve<C: Clock, O: AttemptObserver>(
        &self,
        clock: &mut C,
        deadline: u64,
        observer: &mut O,
    ) -> Result<(u32, u64), Failure> {
        let tokens = self.lock().config.input_tokens_per_attempt;
        self.reserve_tokens(clock, deadline, observer, tokens)
    }
    fn reserve_tokens<C: Clock, O: AttemptObserver>(
        &self,
        clock: &mut C,
        deadline: u64,
        observer: &mut O,
        tokens: u64,
    ) -> Result<(u32, u64), Failure> {
        let started = clock.now_ms();
        let mut announced = None;
        loop {
            if clock.cancelled() {
                return Err(Failure::Cancelled);
            }
            if clock.now_ms() >= deadline {
                return Err(Failure::Deadline);
            }
            match self.admission(clock.now_ms(), tokens)? {
                Ok(id) => return Ok((id, clock.now_ms().saturating_sub(started))),
                Err(wait) => {
                    if wait >= deadline.saturating_sub(clock.now_ms()) {
                        return Err(Failure::Deadline);
                    }
                    if announced.is_none_or(|at: u64| clock.now_ms().saturating_sub(at) >= 1000) {
                        log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"admission_wait","duration_ms":wait}));
                        observer.waiting(wait).inspect_err(|_| self.stop())?;
                        announced = Some(clock.now_ms());
                    }
                    if wait >= deadline.saturating_sub(clock.now_ms()) {
                        return Err(Failure::Deadline);
                    }
                    clock.sleep_ms(wait);
                }
            }
        }
    }
    fn commit_start<C: Clock, O: AttemptObserver>(
        &self,
        id: u32,
        clock: &mut C,
        deadline: u64,
        observer: &mut O,
    ) -> Result<u64, Failure> {
        let started = clock.now_ms();
        let mut announced = None;
        loop {
            let now = clock.now_ms();
            if clock.cancelled() {
                return Err(Failure::Cancelled);
            }
            if now >= deadline {
                return Err(Failure::Deadline);
            }
            let wait = {
                let mut s = self.lock();
                if s.value.stopped {
                    return Err(Failure::Cancelled);
                }
                while s
                    .starts
                    .front()
                    .is_some_and(|at| at.saturating_add(1000) <= now)
                {
                    s.starts.pop_front();
                }
                let mut wait = s.cooldown_until.saturating_sub(now);
                if s.starts.len() >= s.config.requests_per_second as usize {
                    wait = wait.max(s.starts[0].saturating_add(1000).saturating_sub(now));
                }
                if wait == 0 {
                    let token = s
                        .tokens
                        .iter_mut()
                        .find(|token| token.id == id)
                        .ok_or(Failure::Config)?;
                    if token.committed {
                        return Err(Failure::Config);
                    }
                    token.committed = true;
                    s.starts.push_back(now);
                    return Ok(clock.now_ms().saturating_sub(started));
                }
                wait
            };
            if wait >= deadline.saturating_sub(clock.now_ms()) {
                return Err(Failure::Deadline);
            }
            if announced.is_none_or(|at: u64| clock.now_ms().saturating_sub(at) >= 1000) {
                observer.waiting(wait).inspect_err(|_| self.stop())?;
                announced = Some(clock.now_ms());
            }
            if wait >= deadline.saturating_sub(clock.now_ms()) {
                return Err(Failure::Deadline);
            }
            clock.sleep_ms(wait);
        }
    }
    // Keep the post counted to preserve unique slot IDs and the post cap.
    fn release_unsent(&self, id: u32) -> Result<(), Failure> {
        let mut s = self.lock();
        let index = (s.tokens.iter())
            .position(|t| t.id == id && !t.committed && t.expires.is_none())
            .ok_or(Failure::Config)?;
        let token = s.tokens.remove(index);
        let cost = token
            .tokens
            .saturating_mul(s.config.input_token_price_nano_usd);
        let Some(reserved) = s.value.reserved.checked_sub(cost) else {
            s.value.stopped = true;
            return Err(Failure::Config);
        };
        s.value.reserved = reserved;
        Ok(())
    }
    fn complete_slot(
        &self,
        id: u32,
        now: u64,
        input: Option<u64>,
        failure: Option<Failure>,
        retry_after_ms: u64,
    ) -> Result<(), Failure> {
        let mut s = self.lock();
        if failure == Some(Failure::Http(429)) && now >= s.backoff_until {
            s.gap_ms = s.config.backed_off_gap(s.gap_ms);
            s.backoff_until = now.saturating_add(s.gap_ms);
        }
        let expiry = now.saturating_add(s.gap_ms.max(retry_after_ms));
        let token = s
            .tokens
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Failure::Config)?;
        if token.expires.is_some() {
            return Err(Failure::Config);
        }
        let reservation = token.tokens;
        token.expires = Some(expiry);
        s.value.last_completion = Some(now);
        if failure.is_some_and(|f| {
            matches!(
                f,
                Failure::Http(429 | 500..=599)
                    | Failure::Transport(TransportError::OutcomeUnknown)
                    | Failure::Deadline
            )
        }) {
            s.cooldown_until = s.cooldown_until.max(expiry);
        }
        if let Some(input) = input {
            let Some(cost) = input.checked_mul(s.config.input_token_price_nano_usd) else {
                s.value.stopped = true;
                return Err(Failure::Budget);
            };
            let Some(known) = s.value.known_cost.checked_add(cost) else {
                s.value.stopped = true;
                return Err(Failure::Budget);
            };
            let Some(reserved) = s
                .value
                .reserved
                .checked_sub(reservation * s.config.input_token_price_nano_usd)
            else {
                s.value.stopped = true;
                return Err(Failure::Config);
            };
            s.value.reserved = reserved;
            s.value.known_cost = known;
            if input > reservation || known.saturating_add(reserved) > s.value.max_cost_nano_usd {
                s.value.stopped = true;
                return Err(Failure::Budget);
            }
            // The provider's per-second window counts billed usage, so the gap holds the
            // reported usage; estimates run well above it and would admit far fewer.
            if let Some(token) = s.tokens.iter_mut().find(|t| t.id == id) {
                token.tokens = input;
            }
        }
        Ok(())
    }
    fn jitter(&self, bound: u64) -> u64 {
        let mut s = self.lock();
        s.rng = s
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        if bound == 0 { 0 } else { s.rng % (bound + 1) }
    }
}
struct AttemptTicket {
    budget: AttemptBudget,
    id: u32,
    done: bool,
    admitted_ms: u64,
    wall: std::time::Instant,
}
impl AttemptTicket {
    fn complete(
        &mut self,
        now: u64,
        input: Option<u64>,
        failure: Option<Failure>,
        retry_after: u64,
    ) -> Result<(), Failure> {
        self.done = true;
        self.budget
            .complete_slot(self.id, now, input, failure, retry_after)
    }
}
impl Drop for AttemptTicket {
    fn drop(&mut self) {
        if !self.done {
            let now = self.admitted_ms.saturating_add(
                self.wall
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            );
            let _ = self.budget.complete_slot(
                self.id,
                now,
                None,
                Some(Failure::Transport(TransportError::OutcomeUnknown)),
                0,
            );
        }
    }
}
/// Safe accounting and failure metadata for one admitted attempt, never a response-body log.
#[derive(Debug)]
pub struct Attempt {
    pub slot: u32,
    pub queue_wait_ms: u64,
    pub elapsed_ms: u64,
    pub backoff_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub billing_unknown: bool,
    pub failure: Option<Failure>,
    pub answer_errors: BTreeMap<String, Error>,
    pub http_status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub retryable: bool,
}
/// Final protocol result together with every attempt and total elapsed monotonic time.
#[derive(Debug)]
#[must_use = "inspect the result and attempts so failures and usage are not discarded"]
pub struct Execution {
    pub result: Result<Response, Failure>,
    pub attempts: Vec<Attempt>,
    pub total_ms: u64,
}
/// Optional auditing around every real HTTP attempt. Callback failures stop
/// further sends. Callbacks never receive credentials, headers or raw response bodies.
pub struct AttemptContext<'attempt> {
    pub slot: u32,
    pub question_ids: &'attempt [String],
    pub input: &'attempt InputSummary,
}
/// Audit callbacks may stop execution but do not replace a RecordingTransport body sink.
pub trait AttemptObserver {
    fn waiting(&mut self, _duration_ms: u64) -> Result<(), Failure> {
        Ok(())
    }
    fn before_send(
        &mut self,
        _body: &Body,
        _budget: &AttemptBudget,
        _timeout_cap_ms: u64,
        _context: &AttemptContext<'_>,
    ) -> Result<(), Failure> {
        Ok(())
    }
    fn after_send(
        &mut self,
        _attempt: &Attempt,
        _response: Option<&Response>,
        _budget: &AttemptBudget,
        _context: &AttemptContext<'_>,
    ) -> Result<(), Failure> {
        Ok(())
    }
}
/// Observer for callers that do not need additional admission or completion callbacks.
pub struct NoopObserver;
impl AttemptObserver for NoopObserver {}

#[must_use]
pub fn retryable_answer(error: Error) -> bool {
    matches!(
        error,
        Error::Answer
            | Error::Type
            | Error::Range
            | Error::Keys
            | Error::Distribution
            | Error::Legend
    )
}
fn retryable_failure(failure: Failure) -> bool {
    matches!(
        failure,
        Failure::Http(429 | 529 | 500..=599)
            | Failure::Transport(TransportError::OutcomeUnknown)
            | Failure::Deadline
    )
}
/// Execute with a fresh budget and no observer; default policy does not permit sending.
/// Reuse a shared budget through execute_observed when coordinating workers or requests.
#[allow(clippy::too_many_arguments)]
pub fn execute<T: Transport, C: Clock>(
    request: &Request,
    images: &[PngImage],
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
) -> Execution {
    execute_observed(
        request,
        images,
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        &mut AttemptBudget::default(),
        &mut NoopObserver,
    )
}
/// Execute using the supplied shared budget and observer for every attempt.
/// Wrap the transport in [`RecordingTransport`] to retain bodies on all error paths.
#[allow(clippy::too_many_arguments)]
pub fn execute_observed<T: Transport, C: Clock, O: AttemptObserver>(
    request: &Request,
    images: &[PngImage],
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
    budget: &mut AttemptBudget,
    observer: &mut O,
) -> Execution {
    execute_with(
        request,
        Images::Ready(images),
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        budget,
        observer,
    )
}

/// Prepare images at most once after admission; retries reuse the body.
/// If `make_images` fails, releases the unsent slot and returns [`Failure::ImagesUnavailable`].
#[allow(clippy::too_many_arguments)]
pub fn execute_deferred<T: Transport, C: Clock, O: AttemptObserver>(
    request: &Request,
    make_images: &mut dyn FnMut() -> Result<Vec<PngImage>, ()>,
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
    budget: &mut AttemptBudget,
    observer: &mut O,
) -> Execution {
    execute_with(
        request,
        Images::Deferred(make_images),
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        budget,
        observer,
    )
}

enum Images<'images> {
    Ready(&'images [PngImage]),
    Deferred(&'images mut dyn FnMut() -> Result<Vec<PngImage>, ()>),
}

#[allow(clippy::too_many_arguments)]
fn execute_with<T: Transport, C: Clock, O: AttemptObserver>(
    request: &Request,
    mut images: Images<'_>,
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
    budget: &mut AttemptBudget,
    observer: &mut O,
) -> Execution {
    let started = clock.now_ms();
    let mut attempts = Vec::<Attempt>::new();
    let result = (|| {
        if !policy.allow_remote {
            return Err(Failure::Disabled);
        }
        let secret = secret.ok_or(Failure::Credentials)?;
        if !(1..=3).contains(&policy.max_attempts)
            || policy.total_deadline_ms == 0
            || policy.attempt_timeout_ms == 0
            || policy.estimated_input_tokens_per_attempt == 0
        {
            return Err(Failure::Config);
        }
        limits.validate().map_err(Failure::Protocol)?;
        if request.bytes().len() > limits.max_bytes {
            return Err(Failure::Protocol(Error::Bytes));
        }
        let prepare = |images: &[PngImage]| {
            let body = if images.is_empty() {
                Body::json(request)
            } else {
                if !policy.allow_preview_images {
                    return Err(Failure::PreviewDisabled);
                }
                multipart::encode(request, images, boundary)
            }
            .map_err(Failure::Protocol)?;
            Ok((body, input_summary(request, images)))
        };
        let mut prepared = match &images {
            Images::Ready(images) => Some(prepare(images)?),
            Images::Deferred(_) if !policy.allow_preview_images => {
                return Err(Failure::PreviewDisabled);
            }
            Images::Deferred(_) => None,
        };
        let config = budget.lock().config.clone();
        let estimate = planning::estimate(request, &config.context);
        if !estimate.fits(&config.context, policy.estimated_input_tokens_per_attempt) {
            return Err(Failure::Protocol(Error::Request));
        }
        let planned = |input: &InputSummary| {
            log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"request_planned","input_kind":input.input_kind,"question_count":input.question_count,"image_count":input.image_dimensions.len(),"estimated_input_tokens":estimate.request_tokens,"estimated_largest_question_tokens":estimate.largest_question_tokens,"reserved_input_tokens":policy.estimated_input_tokens_per_attempt}));
        };
        if let Some((_, input)) = &prepared {
            planned(input);
        }
        let question_ids: Vec<_> = request.draft().questions.keys().cloned().collect();
        let session_deadline = budget.session_deadline(started);
        let deadline = started
            .checked_add(policy.total_deadline_ms)
            .ok_or(Failure::Config)?
            .min(session_deadline);
        let mut body_spent = 0u64;
        let mut tokens = 0u64;
        let mut retry_ready = started;
        for index in 0..policy.max_attempts {
            budget.available(policy.estimated_input_tokens_per_attempt)?;
            if clock.cancelled() {
                return Err(Failure::Cancelled);
            }
            let wait = retry_ready.saturating_sub(clock.now_ms());
            if wait > 0 {
                if wait >= deadline.saturating_sub(clock.now_ms()) {
                    return Err(Failure::Deadline);
                }
                log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"waiting","duration_ms":wait}));
                observer.waiting(wait).inspect_err(|_| budget.stop())?;
                if wait >= deadline.saturating_sub(clock.now_ms()) {
                    return Err(Failure::Deadline);
                }
                let before = clock.now_ms();
                clock.sleep_ms(wait);
                if let Some(last) = attempts.last_mut() {
                    last.backoff_ms = clock.now_ms().saturating_sub(before);
                }
            }
            let remaining = deadline.saturating_sub(clock.now_ms());
            if remaining == 0 {
                return Err(Failure::Deadline);
            }
            if let Some((body, _)) = &prepared {
                body_spent = body_spent
                    .checked_add(body.bytes().len() as u64)
                    .ok_or(Failure::Budget)?;
            }
            tokens = tokens
                .checked_add(policy.estimated_input_tokens_per_attempt)
                .ok_or(Failure::Budget)?;
            if body_spent > policy.max_total_body_bytes
                || tokens > policy.max_estimated_input_tokens
            {
                return Err(Failure::Budget);
            }
            let (slot, mut queue_wait_ms) = budget.reserve_tokens(
                clock,
                deadline,
                observer,
                policy.estimated_input_tokens_per_attempt,
            )?;
            let mut ticket = AttemptTicket {
                budget: budget.clone(),
                id: slot,
                done: false,
                admitted_ms: clock.now_ms(),
                wall: std::time::Instant::now(),
            };
            if prepared.is_none() {
                let made = match &mut images {
                    Images::Deferred(make) => make().map_err(|()| Failure::ImagesUnavailable),
                    Images::Ready(_) => Err(Failure::Config),
                };
                let ready = made
                    .and_then(|made| prepare(&made))
                    .and_then(|(body, input)| {
                        body_spent = body.bytes().len() as u64;
                        match body_spent > policy.max_total_body_bytes {
                            true => Err(Failure::Budget),
                            false => Ok((body, input)),
                        }
                    });
                match ready {
                    Ok(ready) => {
                        planned(&ready.1);
                        prepared = Some(ready);
                    }
                    Err(failure) => {
                        // If release fails, Drop settles the reservation as possibly sent.
                        if budget.release_unsent(slot).is_ok() {
                            ticket.done = true;
                        }
                        return Err(failure);
                    }
                }
            }
            let Some((body, input)) = &prepared else {
                return Err(Failure::Config);
            };

            log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"attempt_reserved","session_attempt":slot,"attempt_in_batch":index+1,"batch_cap":policy.max_attempts,"input_kind":input.input_kind,"question_count":input.question_count,"image_count":input.image_dimensions.len(),"image_dimensions":input.image_dimensions}));
            let trace = AttemptContext {
                slot,
                question_ids: &question_ids,
                input,
            };
            observer
                .before_send(
                    body,
                    budget,
                    remaining.min(policy.attempt_timeout_ms),
                    &trace,
                )
                .inspect_err(|_| budget.stop())?;
            // A durable audit/progress callback can block. No callback occurs
            // between this fresh check and the actual send.
            let remaining = deadline.saturating_sub(clock.now_ms());
            if remaining == 0 {
                return Err(Failure::Deadline);
            }
            if clock.cancelled() {
                return Err(Failure::Cancelled);
            }
            queue_wait_ms =
                queue_wait_ms.saturating_add(budget.commit_start(slot, clock, deadline, observer)?);
            let remaining = deadline.saturating_sub(clock.now_ms());
            if remaining == 0 {
                return Err(Failure::Deadline);
            }
            let timeout = remaining.min(policy.attempt_timeout_ms);
            let attempt_started = clock.now_ms();
            let wire = transport.send(HttpRequest {
                secret,
                body,
                timeout_ms: timeout,
                max_response_bytes: limits.max_bytes,
            });
            let mut attempt = Attempt {
                slot,
                queue_wait_ms,
                elapsed_ms: clock.now_ms().saturating_sub(attempt_started),
                backoff_ms: 0,
                input_tokens: None,
                output_tokens: None,
                billing_unknown: true,
                failure: None,
                answer_errors: Default::default(),
                http_status: None,
                retry_after_ms: None,
                retryable: false,
            };
            let mut response = None;
            match wire {
                Ok(wire) => {
                    attempt.http_status = Some(wire.status);
                    attempt.retry_after_ms = wire.retry_after_ms;
                    if let Some(error) = wire.body_error {
                        attempt.failure = Some(match error {
                            TransportError::ResponseTooLarge => Failure::Protocol(Error::Bytes),
                            other => Failure::Transport(other),
                        });
                    } else if wire.body.len() > limits.max_bytes {
                        attempt.failure = Some(Failure::Protocol(Error::Bytes));
                    } else if (300..400).contains(&wire.status) {
                        attempt.failure = Some(Failure::Redirect);
                    } else if (200..300).contains(&wire.status) {
                        match super::parse_response(&wire.body, request, limits) {
                            Ok(parsed) => {
                                attempt.input_tokens = parsed.usage.input_tokens.0;
                                attempt.output_tokens = parsed.usage.output_tokens.0;
                                attempt.billing_unknown = attempt.input_tokens.is_none();
                                attempt.answer_errors = parsed
                                    .answers
                                    .iter()
                                    .filter_map(|(id, value)| {
                                        value.as_ref().err().map(|error| (id.clone(), *error))
                                    })
                                    .collect();
                                response = Some(parsed);
                            }
                            Err(error) => attempt.failure = Some(Failure::Protocol(error)),
                        }
                    } else {
                        attempt.failure = Some(Failure::Http(wire.status));
                    }
                }
                Err(TransportError::ResponseTooLarge) => {
                    attempt.failure = Some(Failure::Protocol(Error::Bytes))
                }
                Err(error) => attempt.failure = Some(Failure::Transport(error)),
            }
            if clock.cancelled() {
                attempt.failure = Some(Failure::Cancelled);
            } else if clock.now_ms().saturating_sub(attempt_started) >= timeout {
                attempt.failure = Some(Failure::Deadline);
            }
            let accounted = ticket.complete(
                clock.now_ms(),
                attempt.input_tokens,
                attempt.failure,
                attempt.retry_after_ms.unwrap_or(0),
            );
            drop(ticket);
            if let Err(failure) = accounted {
                attempt.failure = Some(failure);
            }
            attempt.retryable = attempt.failure.map_or_else(
                || {
                    !attempt.answer_errors.is_empty()
                        && attempt
                            .answer_errors
                            .values()
                            .all(|error| retryable_answer(*error))
                },
                retryable_failure,
            );
            log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"response_complete","session_attempt":slot,"elapsed_ms":attempt.elapsed_ms,"queue_wait_ms":attempt.queue_wait_ms,"http_status":attempt.http_status,"invalid_answers":attempt.answer_errors.len(),"retryable":attempt.retryable,"known_input_tokens":attempt.input_tokens,"estimated_known_cost_nano_usd":budget.snapshot().known_cost,"unknown_reserved_nano_usd":budget.snapshot().reserved}));
            if let Some(failure) = attempt.failure {
                log::warn!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"attempt_failure","session_attempt":slot,"category":format!("{failure:?}")}));
            }
            for error in attempt.answer_errors.values() {
                log::warn!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"answer_invalid","session_attempt":slot,"code":format!("{error:?}")}));
            }
            let observed = observer.after_send(&attempt, response.as_ref(), budget, &trace);
            let failure = attempt.failure;
            let retryable = attempt.retryable;
            let retry_after = attempt.retry_after_ms.unwrap_or(0);
            attempts.push(attempt);
            if observed.is_err() {
                budget.stop();
                return if let Some(failure) = failure {
                    Err(failure)
                } else if response
                    .as_ref()
                    .is_some_and(|r| r.answers.values().any(|a| a.is_err()))
                {
                    response.ok_or(Failure::Observer)
                } else {
                    Err(Failure::Observer)
                };
            }
            if !retryable || index + 1 == policy.max_attempts {
                return if let Some(failure) = failure {
                    Err(failure)
                } else {
                    Ok(response.ok_or(Failure::Config)?)
                };
            }
            let exponential = policy
                .initial_backoff_ms
                .saturating_mul(u64::from(policy.backoff_multiplier).saturating_pow(index))
                .min(policy.max_backoff_ms);
            let jitter = budget.jitter(policy.jitter_ms.min(10_000));
            let backoff = exponential
                .saturating_add(jitter)
                .min(policy.max_backoff_ms)
                .max(retry_after);
            log::info!(target:"voltfill_core::jev","{}",serde_json::json!({"event":"retry_wait","session_attempt":slot,"retry_index":index+1,"exponential_ms":exponential,"jitter_ms":jitter,"retry_after_ms":retry_after,"delay_ms":backoff}));
            retry_ready = clock.now_ms().saturating_add(backoff);
            if retry_ready >= deadline {
                return Err(Failure::Deadline);
            }
        }
        Err(Failure::Budget)
    })();
    Execution {
        result,
        attempts,
        total_ms: clock.now_ms().saturating_sub(started),
    }
}

#[cfg(test)]
mod admission_commit_tests {
    use super::*;
    #[derive(Default)]
    struct TestClock(u64);
    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.0 += ms;
        }
    }
    #[test]
    fn callback_delayed_ticket_rechecks_shared_cooldown_and_stop() {
        let budget = AttemptBudget::default();
        budget.set_test_input_quota(128_000);
        let mut clock = TestClock::default();
        let (delayed, _) = budget
            .reserve(&mut clock, 30_000, &mut NoopObserver)
            .expect("the first ticket fits the shared input quota");
        let (other, _) = budget
            .reserve(&mut clock, 30_000, &mut NoopObserver)
            .expect("the second ticket fits the remaining shared input quota");
        budget
            .commit_start(other, &mut clock, 30_000, &mut NoopObserver)
            .expect("the other worker may start before the delayed callback resumes");
        // Another worker returns while this ticket's pre-send observer is blocked.
        budget
            .complete_slot(other, clock.now_ms(), None, Some(Failure::Http(429)), 9_000)
            .expect("completing the started ticket must record its rate-limit cooldown");
        let waited = budget
            .commit_start(delayed, &mut clock, 30_000, &mut NoopObserver)
            .expect("the delayed ticket must start after waiting for the shared cooldown");
        assert_eq!(waited, 9_000);
        assert_eq!(clock.now_ms(), 9_000);
        budget.stop();
        assert_eq!(
            budget.commit_start(delayed, &mut clock, 30_000, &mut NoopObserver),
            Err(Failure::Cancelled)
        );
    }
    #[test]
    fn the_gap_backoff_and_its_cap_are_configured() {
        let config = ExecutionConfig {
            completion_gap_ms: MIN_COMPLETION_GAP_MS,
            completion_gap_backoff: 3,
            max_completion_gap_ms: 5_000,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        assert_eq!(config.backed_off_gap(1_000), 3_000);
        assert_eq!(config.backed_off_gap(3_000), 5_000);
        let fixed = ExecutionConfig {
            completion_gap_backoff: 1,
            ..Default::default()
        };
        assert_eq!(fixed.backed_off_gap(2_000), 2_000);
        let below_start = ExecutionConfig {
            max_completion_gap_ms: 1_500,
            ..Default::default()
        };
        assert!(below_start.validate().is_err());
    }
    #[test]
    fn a_completed_attempt_holds_its_reported_usage_not_its_reservation() {
        let budget = AttemptBudget::default();
        budget.set_test_input_quota(10_000);
        let mut clock = TestClock::default();
        let send = |clock: &mut TestClock, tokens, input| {
            let (id, _) = budget
                .reserve_tokens(clock, 30_000, &mut NoopObserver, tokens)
                .expect("the token reservation must fit before the test deadline");
            budget
                .commit_start(id, clock, 30_000, &mut NoopObserver)
                .expect("a reserved ticket must obtain start authorization");
            budget
                .complete_slot(id, clock.now_ms(), input, None, 0)
                .expect("a started ticket must accept completion usage");
        };
        send(&mut clock, 8_000, Some(4_000));
        send(&mut clock, 6_000, Some(4_000));
        assert_eq!(clock.now_ms(), 0);
        // Usage that was never reported keeps its whole reservation for the gap.
        send(&mut clock, 2_000, None);
        send(&mut clock, 8_000, Some(4_000));
        assert_eq!(clock.now_ms(), DEFAULT_COMPLETION_GAP_MS);
    }
    #[test]
    fn each_rate_limit_response_doubles_the_completion_gap() {
        let budget = AttemptBudget::default();
        let config = ExecutionConfig {
            completion_gap_ms: MIN_COMPLETION_GAP_MS,
            input_tokens_per_second: 10_000,
            input_tokens_per_attempt: 10_000,
            ..Default::default()
        };
        budget
            .configure(&config)
            .expect("the rate-limit test configuration satisfies execution limits");
        let mut clock = TestClock::default();
        let send = |clock: &mut TestClock, input, failure| {
            let (id, _) = budget
                .reserve_tokens(clock, 30_000, &mut NoopObserver, 8_000)
                .expect("the reservation must fit after any required completion gap");
            budget
                .commit_start(id, clock, 30_000, &mut NoopObserver)
                .expect("the reserved ticket must start before the deadline");
            budget
                .complete_slot(id, clock.now_ms(), input, failure, 0)
                .expect("completion must account for usage or a rate-limit response");
        };
        send(&mut clock, None, Some(Failure::Http(429)));
        send(&mut clock, Some(8_000), None);
        assert_eq!(clock.now_ms(), 2 * MIN_COMPLETION_GAP_MS);
        send(&mut clock, Some(4_000), None);
        assert_eq!(clock.now_ms(), 4 * MIN_COMPLETION_GAP_MS);
    }
    #[test]
    fn rate_limit_responses_within_one_backoff_double_the_gap_once() {
        let budget = AttemptBudget::default();
        let config = ExecutionConfig {
            completion_gap_ms: MIN_COMPLETION_GAP_MS,
            max_concurrency: 4,
            ..Default::default()
        };
        budget
            .configure(&config)
            .expect("four workers are within the configured concurrency limit");
        let mut clock = TestClock::default();
        let ids: Vec<_> = (0..4)
            .map(|_| {
                let (id, _) = budget
                    .reserve_tokens(&mut clock, 30_000, &mut NoopObserver, 8_000)
                    .expect("each of four reservations fits the shared token quota");
                budget
                    .commit_start(id, &mut clock, 30_000, &mut NoopObserver)
                    .expect("each reserved worker may start before any rate-limit response");
                id
            })
            .collect();
        for id in ids {
            budget
                .complete_slot(id, clock.now_ms(), None, Some(Failure::Http(429)), 0)
                .expect("each started worker must record its rate-limit response");
        }
        assert_eq!(budget.lock().gap_ms, 2 * MIN_COMPLETION_GAP_MS);
    }
    #[test]
    fn delayed_callbacks_cannot_bunch_actual_start_authorizations() {
        let budget = AttemptBudget::default();
        let config = ExecutionConfig {
            requests_per_second: 1,
            input_tokens_per_attempt: 1_000,
            ..Default::default()
        };
        budget
            .configure(&config)
            .expect("the one-request-per-second configuration is valid");
        let mut clock = TestClock::default();
        let (first, _) = budget
            .reserve(&mut clock, 30_000, &mut NoopObserver)
            .expect("the first delayed callback may reserve a ticket");
        clock.0 = 1_500;
        let (second, _) = budget
            .reserve(&mut clock, 30_000, &mut NoopObserver)
            .expect("the second delayed callback may reserve after the rate window");
        // Both callbacks resume together long after their reservation times.
        clock.0 = 3_000;
        budget
            .commit_start(first, &mut clock, 30_000, &mut NoopObserver)
            .expect("the first resumed callback may start at the current time");
        assert_eq!(clock.now_ms(), 3_000);
        budget
            .commit_start(second, &mut clock, 30_000, &mut NoopObserver)
            .expect("the second resumed callback must wait for a new start slot");
        assert_eq!(clock.now_ms(), 4_000);
        assert_eq!(
            budget.lock().starts.iter().copied().collect::<Vec<_>>(),
            vec![4_000]
        );
    }
}
