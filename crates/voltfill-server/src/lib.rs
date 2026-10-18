#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod cli;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use voltfill_core::{
    engine::{self, EvaluationProfile, ExtractOptions},
    evaluate::{FixtureEvaluator, MonotonicClock, RemoteEvaluator},
    jev::{
        http::{Endpoint, HttpTransport},
        transport::{AttemptBudget, Clock, ExecutionConfig, Secret, TransportError},
    },
    model::{ExtractionReport, JevResponse, Strategy},
    schema,
};

enum Backend {
    Fixture(Vec<u8>),
    Remote(Endpoint, Arc<Secret>),
}
pub struct ServerConfig {
    backend: Backend,
    pub execution: ExecutionConfig,
    pub resources: voltfill_core::resources::ResourceLimits,
    pub upload_timeout_ms: u64,
    pub model: String,
    pub allow_preview_images: bool,
    pub api_key: Option<Secret>,
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    pub render: Option<voltfill_core::render::RenderConfig>,
}
impl ServerConfig {
    pub fn fixture(bytes: Vec<u8>) -> Result<Self, &'static str> {
        FixtureEvaluator::from_bytes(&bytes).map_err(|_| "invalid_mock")?;
        Ok(Self {
            backend: Backend::Fixture(bytes),
            execution: ExecutionConfig::default(),
            resources: Default::default(),
            upload_timeout_ms: 30_000,
            model: voltfill_core::jev::MODEL.into(),
            allow_preview_images: false,
            api_key: None,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render: None,
        })
    }
    pub fn remote(endpoint: Endpoint, secret: Secret) -> Self {
        Self {
            backend: Backend::Remote(endpoint, Arc::new(secret)),
            execution: ExecutionConfig::default(),
            resources: Default::default(),
            upload_timeout_ms: 30_000,
            model: voltfill_core::jev::MODEL.into(),
            allow_preview_images: false,
            api_key: None,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render: None,
        }
    }
    pub fn requires_authentication(&self) -> bool {
        self.api_key.is_some()
    }
}
struct Admission {
    next: Instant,
    paused: bool,
}
#[derive(Clone)]
pub struct AppState {
    config: Arc<ServerConfig>,
    transport: Option<HttpTransport>,
    jobs: Arc<Semaphore>,
    admission: Arc<Mutex<Admission>>,
}
impl AppState {
    /// Construct outside an async runtime; blocking client shutdown stays outside it too.
    #[allow(unused_mut)]
    pub fn new(mut config: ServerConfig) -> Result<Self, &'static str> {
        config
            .execution
            .validate()
            .map_err(|_| "invalid_execution_config")?;
        config.resources.validate()?;
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        if let Some(render) = &mut config.render {
            config.resources.apply_render(render);
        }
        if !(1..=3_600_000).contains(&config.upload_timeout_ms) {
            return Err("invalid_upload_timeout");
        }
        voltfill_core::jev::validate_model(&config.model).map_err(|_| "invalid_model")?;
        let transport = match &config.backend {
            Backend::Remote(endpoint, _) => {
                Some(HttpTransport::new(endpoint.clone()).map_err(|_| "transport_setup_failed")?)
            }
            Backend::Fixture(_) => None,
        };
        Ok(Self {
            config: Arc::new(config),
            transport,
            jobs: Arc::new(Semaphore::new(1)),
            admission: Arc::new(Mutex::new(Admission {
                next: Instant::now(),
                paused: false,
            })),
        })
    }
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/extract", post(extract))
        .layer(DefaultBodyLimit::max(
            state.config.resources.pdf.max_bytes + schema::MAX_SCHEMA_BYTES + 128 * 1024,
        ))
        .with_state(state)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "text_first")]
    strategy: Strategy,
    #[serde(default)]
    evaluation_profile: Option<EvaluationProfile>,
}
fn text_first() -> Strategy {
    Strategy::TextFirst
}
impl Default for Options {
    fn default() -> Self {
        Self {
            model: None,
            strategy: text_first(),
            evaluation_profile: None,
        }
    }
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error":code,"jev_responses":[]}))).into_response()
}
fn busy(seconds: u64) -> Response {
    let mut response = error(StatusCode::TOO_MANY_REQUESTS, "server_busy");
    if let Ok(value) = seconds.to_string().parse() {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
fn authorized(request: &Request, key: Option<&Secret>) -> bool {
    let Some(key) = key else {
        return true;
    };
    let Some(actual) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let expected = key.bearer().as_bytes();
    let actual = actual.as_bytes();
    let mut difference = actual.len() ^ expected.len();
    for (index, byte) in expected.iter().enumerate() {
        difference |= usize::from(*byte ^ actual.get(index).copied().unwrap_or(0));
    }
    difference == 0
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
#[derive(Clone)]
struct JobClock {
    clock: MonotonicClock,
    cancelled: Arc<AtomicBool>,
}
impl Clock for JobClock {
    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }
    fn sleep_ms(&mut self, ms: u64) {
        let end = self.clock.now_ms().saturating_add(ms);
        while self.clock.now_ms() < end && !self.cancelled() {
            self.clock
                .sleep_ms(end.saturating_sub(self.clock.now_ms()).min(25));
        }
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

type Upload = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

async fn bounded_upload(
    upload: impl std::future::Future<Output = Result<Upload, (StatusCode, &'static str)>>,
    deadline: Duration,
) -> Result<Upload, (StatusCode, &'static str)> {
    tokio::time::timeout(deadline, upload)
        .await
        .unwrap_or(Err((StatusCode::REQUEST_TIMEOUT, "upload_timeout")))
}

async fn read_upload(
    request: Request,
    state: &AppState,
) -> Result<Upload, (StatusCode, &'static str)> {
    let mut multipart = match Multipart::from_request(request, state).await {
        Ok(value) => value,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "invalid_multipart")),
    };
    let (mut pdf, mut definition, mut options) = (None, None, None);
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(rejection) => return Err((rejection.status(), "invalid_or_oversized_upload")),
        };
        let name = field.name().unwrap_or("").to_owned();
        let limit = match name.as_str() {
            "pdf" if pdf.is_none() => state.config.resources.pdf.max_bytes,
            "schema" if definition.is_none() => schema::MAX_SCHEMA_BYTES,
            "options" if options.is_none() => 64 * 1024,
            _ => return Err((StatusCode::BAD_REQUEST, "unknown_or_duplicate_part")),
        };
        let mut field = field;
        let mut bytes = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len().saturating_add(chunk.len()) > limit {
                        return Err((StatusCode::PAYLOAD_TOO_LARGE, "part_too_large"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(rejection) => return Err((rejection.status(), "invalid_or_oversized_upload")),
            }
        }
        match name.as_str() {
            "pdf" => pdf = Some(bytes),
            "schema" => definition = Some(bytes),
            _ => options = Some(bytes),
        }
    }
    Ok((pdf, definition, options))
}

async fn extract(State(state): State<AppState>, request: Request) -> Response {
    if !authorized(&request, state.config.api_key.as_ref()) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if state.config.api_key.is_none() && request.headers().contains_key(header::ORIGIN) {
        return error(StatusCode::FORBIDDEN, "browser_authentication_required");
    }
    let Ok(permit) = state.jobs.clone().try_acquire_owned() else {
        return busy(2);
    };
    {
        let admission = state.admission.lock().unwrap_or_else(|e| e.into_inner());
        if admission.paused {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "remote_paused_unknown_outcome",
            );
        }
        let remaining = admission.next.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            return busy(remaining.as_secs().saturating_add(1));
        }
    }
    let (pdf, definition, options) = match bounded_upload(
        read_upload(request, &state),
        Duration::from_millis(state.config.upload_timeout_ms),
    )
    .await
    {
        Ok(upload) => upload,
        Err((status, code)) => return error(status, code),
    };
    let (Some(pdf), Some(definition)) = (pdf, definition) else {
        return error(StatusCode::BAD_REQUEST, "missing_pdf_or_schema");
    };
    let form = match schema::parse(&definition) {
        Ok(form) => form,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_schema"),
    };
    drop(definition);
    let options = match options
        .map(|b| serde_json::from_slice::<Options>(&b))
        .transpose()
    {
        Ok(value) => value.unwrap_or_default(),
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_options"),
    };
    if form.validate_strategy(options.strategy).is_err() {
        return error(StatusCode::BAD_REQUEST, "strategy_schema_mismatch");
    }
    let model = options.model.unwrap_or_else(|| state.config.model.clone());
    if voltfill_core::jev::validate_model(&model).is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid_model");
    }
    if options.strategy != Strategy::TextFirst && !voltfill_core::jev::supports_images(&model) {
        return error(StatusCode::BAD_REQUEST, "model_does_not_support_images");
    }
    if matches!(state.config.backend, Backend::Remote(_, _))
        && form
            .fields
            .iter()
            .flat_map(|f| &f.questions)
            .any(|q| q.input.modality != schema::Modality::Text)
    {
        if !state.config.allow_preview_images {
            return error(StatusCode::FORBIDDEN, "image_sending_disabled");
        }
        #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
        return error(StatusCode::SERVICE_UNAVAILABLE, "pdfium_feature_disabled");
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        {
            let Some(render) = &state.config.render else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "rendering_required");
            };
            if render.validate().is_err() {
                return error(StatusCode::SERVICE_UNAVAILABLE, "invalid_render_config");
            }
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancellation = CancelOnDrop(cancelled.clone());
    log::info!(target:"voltfill_server","{}",json!({"event":"extraction_started","pdf_bytes":pdf.len(),"fields":form.fields.len()}));
    let job_state = state.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let config = &job_state.config;
        let options = ExtractOptions {
            model,
            pdf_limits: config.resources.pdf,
            strategy: options.strategy,
            evaluation_profile: options.evaluation_profile,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render_config: config.render.clone(),
            ..Default::default()
        }
        .with_execution_config(&config.execution);
        let result = match options {
            Err(error) => Err(error),
            Ok(options) => match &config.backend {
                Backend::Fixture(bytes) => match FixtureEvaluator::from_bytes(bytes) {
                    Ok(mut evaluator) => engine::extract(&pdf, &form, &options, &mut evaluator),
                    Err(_) => Err(engine::EngineError::Config),
                },
                Backend::Remote(_, secret) => {
                    let transport = job_state.transport.as_ref().cloned();
                    match transport
                        .and_then(|transport| {
                            RemoteEvaluator::with_components(
                                secret.clone(),
                                true,
                                config.allow_preview_images,
                                transport,
                                JobClock {
                                    clock: MonotonicClock::default(),
                                    cancelled,
                                },
                                AttemptBudget::default(),
                                voltfill_core::jev::transport::NoopObserver,
                            )
                            .ok()
                        })
                        .and_then(|e| e.with_execution_config(&config.execution).ok())
                    {
                        Some(mut evaluator) => {
                            engine::extract(&pdf, &form, &options, &mut evaluator)
                        }
                        None => Err(engine::EngineError::Config),
                    }
                }
            },
        };
        if matches!(config.backend, Backend::Remote(_, _)) {
            finish_remote(&job_state, &result);
        }
        match result {
            Ok(report) => {
                log::info!(target:"voltfill_server","{}",json!({"event":"extraction_completed","fields":report.fields.len(),"attempts":report.usage.attempts}));
                Json(report).into_response()
            }
            Err(error) => extraction_error(error),
        }
    })
    .await;
    match joined {
        Ok(response) => response,
        Err(_) => {
            state
                .admission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .paused = true;
            error(StatusCode::INTERNAL_SERVER_ERROR, "worker_failed")
        }
    }
}
fn finish_remote(state: &AppState, result: &Result<ExtractionReport, engine::EngineError>) {
    let (records, unknown) = match result {
        Ok(report) => (
            &report.jev_responses[..],
            report.usage.unknown_usage_attempts > 0,
        ),
        Err(error) => (error.jev_responses(), !error.jev_responses().is_empty()),
    };
    let mut admission = state.admission.lock().unwrap_or_else(|e| e.into_inner());
    admission.paused |= unknown;
    if records.is_empty() {
        return;
    }
    let wait = records
        .iter()
        .filter_map(|r| r.retry_after_ms)
        .max()
        .unwrap_or(0)
        .max(state.config.execution.completion_gap_ms);
    admission.paused |= records.iter().any(|r| {
        matches!(
            r.transport_error,
            Some(TransportError::OutcomeUnknown | TransportError::ResponseTooLarge)
        )
    });
    match Instant::now().checked_add(Duration::from_millis(wait)) {
        Some(next) => admission.next = next,
        None => admission.paused = true,
    }
}

#[derive(serde::Serialize)]
struct ExtractionError<'a> {
    error: &'static str,
    jev_responses: &'a [JevResponse],
}
fn extraction_error(error: engine::EngineError) -> Response {
    let status = match &error {
        engine::EngineError::Schema | engine::EngineError::Config | engine::EngineError::Pdf(_) => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ExtractionError {
            error: "extraction_failed",
            jev_responses: error.jev_responses(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;
    use voltfill_core::model::{JevResponse, ResponseBody};

    fn state() -> AppState {
        AppState::new(
            ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec())
                .unwrap(),
        )
        .unwrap()
    }

    fn record(body: Vec<u8>, retry_after_ms: Option<u64>) -> JevResponse {
        JevResponse {
            body_complete: true,
            field_id: "synthetic".into(),
            page: 1,
            question_ids: vec!["question".into()],
            http_status: Some(429),
            retry_after_ms,
            transport_error: None,
            body: Some(ResponseBody::from(body)),
        }
    }

    async fn assert_error(response: Response, status: StatusCode, code: &str) {
        assert_eq!(response.status(), status);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({"error":code,"jev_responses":[]}));
    }

    async fn request(state: AppState) -> Response {
        router(state)
            .oneshot(Request::post("/v1/extract").body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn admission_rejects_busy_cooldown_and_paused_before_reading_body() {
        let state = state();
        let permit = state.jobs.clone().try_acquire_owned().unwrap();
        let response = request(state.clone()).await;
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
        assert_error(response, StatusCode::TOO_MANY_REQUESTS, "server_busy").await;
        drop(permit);

        state.admission.lock().unwrap().next = Instant::now() + Duration::from_secs(10);
        let response = request(state.clone()).await;
        let retry_after: u64 = response.headers()[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=11).contains(&retry_after));
        assert_error(response, StatusCode::TOO_MANY_REQUESTS, "server_busy").await;
        assert_eq!(state.jobs.available_permits(), 1);

        state.admission.lock().unwrap().paused = true;
        assert_error(
            request(state.clone()).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "remote_paused_unknown_outcome",
        )
        .await;
        assert_eq!(state.jobs.available_permits(), 1);
    }

    #[tokio::test]
    async fn upload_deadline_releases_admission_permit() {
        let state = state();
        let permit = state.jobs.clone().try_acquire_owned().unwrap();
        let upload = async move {
            let _permit = permit;
            std::future::pending::<Result<Upload, (StatusCode, &'static str)>>().await
        };
        let response = bounded_upload(upload, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert_error(
            error(response.0, response.1),
            StatusCode::REQUEST_TIMEOUT,
            "upload_timeout",
        )
        .await;
        assert_eq!(state.jobs.available_permits(), 1);
    }

    #[tokio::test]
    async fn error_envelope_preserves_all_original_response_bodies() {
        let raw = "  {\"error\":\"synthetic rate limit\"}\n";
        let responses = vec![
            record(raw.as_bytes().to_vec(), Some(12_000)),
            record(vec![0xff, 0], None),
        ];
        let expected = serde_json::to_value(&responses).unwrap();
        let error = engine::EngineError::WithResponses {
            source: Box::new(engine::EngineError::Provenance),
            jev_responses: responses,
        };
        let state = state();
        let result = Err(error);
        finish_remote(&state, &result);
        {
            let admission = state.admission.lock().unwrap();
            assert!(admission.paused);
            assert!(
                admission.next.saturating_duration_since(Instant::now()) > Duration::from_secs(11)
            );
        }
        let Err(error) = result else { unreachable!() };
        let response = extraction_error(error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "extraction_failed");
        assert_eq!(body["jev_responses"], expected);
        assert_eq!(body["jev_responses"][0]["body"], raw);
        assert_eq!(body["jev_responses"][1]["body"], json!([255, 0]));
    }
}
