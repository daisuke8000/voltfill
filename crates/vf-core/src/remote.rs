//! Shared bounded execution for the remote fill steps ([`crate::assist`] and
//! [`crate::images`]): one budget, one deadline and one body-byte cap across
//! every batch and retry, every received attempt body recorded before parsing,
//! and failures mapped to per-field outcomes.
use crate::assist::{AssistError, AssistOutcome, AssistResponse, AssistUsage};
use crate::fill::FieldType;
use vf_client::multipart::{self, PngImage};
use vf_client::planning;
use vf_client::transport::{
    self, AttemptBudget, Clock, ExecutionConfig, Failure, NoopObserver, Policy, RecordingTransport,
    Secret, Transport,
};
use vf_client::{Request, Response};

/// Multipart boundary for image requests (JSON requests carry none).
pub(crate) const BOUNDARY: &str = "voltfill_fill_boundary";

/// Who a request was sent for, attached to each of its recorded attempts.
pub(crate) struct Attribution {
    pub field_ids: Vec<String>,
    pub question_ids: Vec<String>,
    pub page: Option<u32>,
}

/// One remote fill run: every step and batch shares this budget.
pub(crate) struct Session<'run, T, C> {
    config: &'run ExecutionConfig,
    secret: &'run Secret,
    transport: &'run mut T,
    clock: &'run mut C,
    budget: AttemptBudget,
    started: u64,
    body_spent: u64,
    pub usage: AssistUsage,
    pub responses: Vec<AssistResponse>,
}

impl<'run, T: Transport, C: Clock> Session<'run, T, C> {
    /// Configure the shared budget; the total deadline starts now.
    pub fn new(
        config: &'run ExecutionConfig,
        secret: &'run Secret,
        transport: &'run mut T,
        clock: &'run mut C,
    ) -> Result<Self, AssistError> {
        let budget = AttemptBudget::default();
        budget.configure(config).map_err(|_| AssistError::Config)?;
        let started = clock.now_ms();
        Ok(Self {
            config,
            secret,
            transport,
            clock,
            budget,
            started,
            body_spent: 0,
            usage: AssistUsage::default(),
            responses: Vec::new(),
        })
    }

    pub fn config(&self) -> &ExecutionConfig {
        self.config
    }

    /// Whether a new batch may be sent: `Ok(elapsed ms)`, or the outcome that
    /// settles it without sending.
    pub fn gate(&self) -> Result<u64, AssistOutcome> {
        // Once a completion has stopped the shared budget, vf-client would
        // cancel every later batch; nothing more is sent.
        if self.budget.snapshot().stopped {
            return Err(AssistOutcome::BudgetExhausted);
        }
        let elapsed = self.clock.now_ms().saturating_sub(self.started);
        if elapsed >= self.config.total_deadline_ms {
            return Err(AssistOutcome::DeadlineExceeded);
        }
        Ok(elapsed)
    }

    /// Whether no further request can be admitted: the request cap is used
    /// up or no body bytes remain. Lets a caller skip work (such as
    /// rendering) for a batch that could not be sent.
    pub fn exhausted(&self) -> bool {
        let budget = self.budget.snapshot();
        budget.posts >= budget.max_posts || self.body_spent >= self.config.max_total_body_bytes
    }

    /// Send one built request (with its page image, if any) under the shared
    /// limits. `elapsed` is the value [`Self::gate`] returned for this batch.
    /// Every received attempt is recorded with `attribution` before parsing.
    pub fn send(
        &mut self,
        request: &Request,
        images: &[PngImage],
        elapsed: u64,
        attribution: Attribution,
    ) -> Result<Response, AssistOutcome> {
        let config = self.config;
        // vf-client bounds body bytes per execution; the remainder carries the
        // cap across batches. A JSON body is exactly the request bytes; an
        // image body is the encoded multipart body.
        let size = if images.is_empty() {
            request.bytes().len() as u64
        } else {
            match multipart::encode(request, images, BOUNDARY) {
                Ok(body) => body.bytes().len() as u64,
                Err(vf_client::Error::Bytes) => return Err(AssistOutcome::RequestTooLarge),
                Err(_) => return Err(AssistOutcome::RequestFailed),
            }
        };
        let remaining_body = config.max_total_body_bytes.saturating_sub(self.body_spent);
        if size > remaining_body {
            return Err(AssistOutcome::BudgetExhausted);
        }
        let tokens = planning::estimate(request, &config.context).request_tokens;
        let policy = Policy {
            allow_remote: true,
            allow_preview_images: !images.is_empty(),
            max_attempts: config.max_attempts,
            total_deadline_ms: config.total_deadline_ms - elapsed,
            attempt_timeout_ms: config.attempt_timeout_ms,
            max_total_body_bytes: remaining_body,
            estimated_input_tokens_per_attempt: tokens,
            max_estimated_input_tokens: tokens.saturating_mul(u64::from(config.max_attempts)),
            initial_backoff_ms: config.initial_backoff_ms,
            backoff_multiplier: config.backoff_multiplier,
            max_backoff_ms: config.max_backoff_ms,
            jitter_ms: config.jitter_ms,
        };
        let mut records = Vec::new();
        let execution = {
            let mut recording = RecordingTransport::new(&mut *self.transport, |record| {
                records.push(record);
            });
            transport::execute_observed(
                request,
                images,
                BOUNDARY,
                Some(self.secret),
                policy,
                config.json_limits(),
                &mut recording,
                &mut *self.clock,
                &mut self.budget,
                &mut NoopObserver,
            )
        };
        self.responses
            .extend(records.into_iter().map(|record| AssistResponse {
                field_ids: attribution.field_ids.clone(),
                question_ids: attribution.question_ids.clone(),
                page: attribution.page,
                http_status: record.http_status,
                retry_after_ms: record.retry_after_ms,
                transport_error: record.transport_error,
                body: record.body,
                body_complete: record.body_complete,
            }));
        self.body_spent = self
            .body_spent
            .saturating_add(size.saturating_mul(execution.attempts.len() as u64));
        for attempt in &execution.attempts {
            self.usage.record(attempt);
        }
        execution.result.map_err(|failure| match failure {
            // Completion accounting is the only source of an attempt failing
            // with `Budget`, and it only runs on a parsed response. vf-client
            // drops that response; re-parsing the recorded body would bypass
            // the budget, so the fields stay as they were.
            Failure::Budget
                if execution
                    .attempts
                    .last()
                    .is_some_and(|a| a.failure == Some(Failure::Budget)) =>
            {
                AssistOutcome::UsageExceededBudget
            }
            Failure::Budget => AssistOutcome::BudgetExhausted,
            Failure::Cancelled if self.budget.snapshot().stopped => AssistOutcome::BudgetExhausted,
            Failure::Cancelled => AssistOutcome::Cancelled,
            Failure::Deadline => AssistOutcome::DeadlineExceeded,
            _ => AssistOutcome::RequestFailed,
        })
    }
}

/// The field type as named in requests.
pub(crate) fn kind(field_type: FieldType) -> &'static str {
    match field_type {
        FieldType::Text => "文字列",
        FieldType::Date => "日付",
        FieldType::Amount => "金額",
        FieldType::Number => "数値",
        FieldType::Table => "表",
    }
}

/// At most `max` characters, with `…` when clipped. Requests only.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut clipped: String = text.chars().take(max).collect();
        clipped.push('…');
        clipped
    }
}
