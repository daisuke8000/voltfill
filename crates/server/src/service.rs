//! HTTP extraction service, admission, and document lifetime management.
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use voltfill_core::engine::{self, DocumentCache, EvaluationProfile, ExtractOptions};
use voltfill_core::evaluate::{FixtureEvaluator, MonotonicClock, RemoteEvaluator};
use voltfill_core::jev::http::{Endpoint, HttpTransport};
use voltfill_core::jev::transport::{
    AttemptBudget, Clock, ExecutionConfig, Secret, TransportError,
};
use voltfill_core::model::{ExtractionReport, JevResponse, Strategy};
use voltfill_core::schema;

enum Backend {
    Fixture(Vec<u8>),
    Remote(Endpoint, Arc<Secret>),
}
pub struct ServerConfig {
    backend: Backend,
    pub execution: ExecutionConfig,
    pub resources: voltfill_core::resources::ResourceLimits,
    pub upload_timeout_ms: u64,
    /// Off by default: reuse keeps the last document's text and page images in
    /// memory after the request ends, until `reuse_document_ttl_ms` passes.
    pub reuse_document: bool,
    pub reuse_document_ttl_ms: u64,
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
            reuse_document: false,
            reuse_document_ttl_ms: 60_000,
            model: voltfill_core::jev::MODEL.into(),
            allow_preview_images: false,
            api_key: None,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render: None,
        })
    }
    #[must_use]
    pub fn remote(endpoint: Endpoint, secret: Secret) -> Self {
        Self {
            backend: Backend::Remote(endpoint, Arc::new(secret)),
            execution: ExecutionConfig::default(),
            resources: Default::default(),
            upload_timeout_ms: 30_000,
            reuse_document: false,
            reuse_document_ttl_ms: 60_000,
            model: voltfill_core::jev::MODEL.into(),
            allow_preview_images: false,
            api_key: None,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render: None,
        }
    }
    #[must_use]
    pub fn requires_authentication(&self) -> bool {
        self.api_key.is_some()
    }
}
struct DocumentReuse {
    cache: Arc<Mutex<DocumentCache>>,
    wake: Condvar,
}
/// Upper bound on how long the janitor sleeps, which is also how soon it notices
/// that the server state is gone and exits.
const JANITOR_POLL: Duration = Duration::from_secs(1);
fn locked(cache: &Mutex<DocumentCache>) -> MutexGuard<'_, DocumentCache> {
    cache.lock().unwrap_or_else(|e| e.into_inner())
}
/// Releases the cached document at its deadline on a thread of its own, so release
/// depends on neither the request that filled the cache nor any async runtime.
fn run_janitor(reuse: Weak<DocumentReuse>) {
    while let Some(reuse) = reuse.upgrade() {
        let mut cache = locked(&reuse.cache);
        let wait = cache
            .expire_due(Instant::now())
            .map_or(JANITOR_POLL, |left| left.min(JANITOR_POLL));
        drop(
            reuse
                .wake
                .wait_timeout(cache, wait)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
}
/// Clears the cache if the job unwinds, so a failed job cannot leave a document behind.
struct CacheLease<'cache>(MutexGuard<'cache, DocumentCache>);
impl Drop for CacheLease<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.clear();
        }
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
    discards: Arc<Semaphore>,
    admission: Arc<Mutex<Admission>>,
    reuse: Option<Arc<DocumentReuse>>,
}
impl AppState {
    /// Construct outside an async runtime; blocking client shutdown stays outside it too.
    /// Compatibility name for [`Self::try_new`].
    pub fn new(config: ServerConfig) -> Result<Self, &'static str> {
        Self::try_new(config)
    }

    /// Validate server limits and initialize shared admission and optional cache cleanup.
    /// Construct outside an async runtime; blocking client shutdown stays outside it too.
    #[allow(unused_mut)]
    pub fn try_new(mut config: ServerConfig) -> Result<Self, &'static str> {
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
        if !(1..=3_600_000).contains(&config.reuse_document_ttl_ms) {
            return Err("invalid_reuse_document_ttl");
        }
        voltfill_core::jev::validate_model(&config.model).map_err(|_| "invalid_model")?;
        let transport = match &config.backend {
            Backend::Remote(endpoint, _) => Some(
                HttpTransport::try_new(endpoint.clone()).map_err(|_| "transport_setup_failed")?,
            ),
            Backend::Fixture(_) => None,
        };
        let reuse = config.reuse_document.then(|| {
            Arc::new(DocumentReuse {
                cache: Arc::new(Mutex::new(DocumentCache::with_ttl(Duration::from_millis(
                    config.reuse_document_ttl_ms,
                )))),
                wake: Condvar::new(),
            })
        });
        if let Some(reuse) = &reuse {
            let weak = Arc::downgrade(reuse);
            std::thread::Builder::new()
                .name("voltfill-document-janitor".into())
                .spawn(move || run_janitor(weak))
                .map_err(|_| "janitor_start_failed")?;
        }
        Ok(Self {
            config: Arc::new(config),
            transport,
            jobs: Arc::new(Semaphore::new(1)),
            discards: Arc::new(Semaphore::new(MAX_DISCARDS)),
            admission: Arc::new(Mutex::new(Admission {
                next: Instant::now(),
                paused: false,
            })),
            reuse,
        })
    }
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/health",
            routing::get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/v1/extract", routing::post(extract))
        .layer(DefaultBodyLimit::max(upload_limit(&state)))
        .with_state(state)
}
fn upload_limit(state: &AppState) -> usize {
    state.config.resources.pdf.max_bytes + schema::MAX_SCHEMA_BYTES + 128 * 1024
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

// Answering while the client is still uploading and then closing the connection shows
// up on the client as a reset rather than the response, and a client cannot tell a
// reset from a request that reached the provider. Reading the rest of the upload first
// lets the rejection arrive. It is skipped where it cannot help or would only cost:
// a client waiting for 100-continue has sent nothing and gets the answer cleanly, a
// declared length over the limit ends in a reset anyway, and rejected uploads already
// being read must not hold any number of connections.
const MAX_DISCARDS: usize = 4;

async fn reject_upload(request: Request, state: &AppState, response: Response) -> Response {
    let limit = upload_limit(state);
    let headers = request.headers();
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if headers.contains_key(header::EXPECT) || declared.is_some_and(|n| n > limit as u64) {
        return response;
    }
    let Ok(_slot) = state.discards.clone().try_acquire_owned() else {
        return response;
    };
    discard_upload(
        request.into_body(),
        limit,
        Duration::from_millis(state.config.upload_timeout_ms),
    )
    .await;
    response
}

async fn discard_upload(body: axum::body::Body, limit: usize, deadline: Duration) {
    use axum::body::HttpBody;
    let mut body = std::pin::pin!(body);
    let read = async {
        let mut seen = 0usize;
        while let Some(Ok(frame)) = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
            seen = seen.saturating_add(frame.data_ref().map_or(0, |data| data.len()));
            if seen > limit {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(deadline, read).await;
}

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
    let rejection = match state.jobs.clone().try_acquire_owned() {
        Err(_) => Err(busy(2)),
        Ok(permit) => {
            let admission = state.admission.lock().unwrap_or_else(|e| e.into_inner());
            let remaining = admission.next.saturating_duration_since(Instant::now());
            if admission.paused {
                Err(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "remote_paused_unknown_outcome",
                ))
            } else if !remaining.is_zero() {
                Err(busy(remaining.as_secs().saturating_add(1)))
            } else {
                Ok(permit)
            }
        }
    };
    let permit = match rejection {
        Ok(permit) => permit,
        Err(response) => return reject_upload(request, &state, response).await,
    };
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
        let cache_handle = job_state.reuse.as_ref().map(|r| r.cache.clone());
        let mut lease = cache_handle.as_deref().map(|c| CacheLease(locked(c)));
        let cache = lease.as_mut().map(|l| &mut *l.0);
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
                    Ok(mut evaluator) => {
                        engine::extract_reusing(&pdf, &form, &options, &mut evaluator, cache)
                    }
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
                            engine::extract_reusing(&pdf, &form, &options, &mut evaluator, cache)
                        }
                        None => Err(engine::EngineError::Config),
                    }
                }
            },
        };
        if matches!(config.backend, Backend::Remote(_, _)) {
            finish_remote(&job_state, &result);
        }
        drop(lease);
        if let Some(reuse) = &job_state.reuse {
            reuse.wake.notify_one();
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
struct ExtractionError<'response> {
    error: &'static str,
    jev_responses: &'response [JevResponse],
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
    use axum::body::{self, Body};
    use tower::ServiceExt;
    use voltfill_core::model::{JevResponse, ResponseBody};

    fn state() -> AppState {
        AppState::try_new(
            ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec())
                .expect("the checked-in mock must initialize the fixture server config"),
        )
        .expect("the default fixture server config must initialize app state")
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
        let bytes = body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("the error response body must fit the test read limit");
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).expect("the error response must be JSON");
        assert_eq!(body, json!({"error":code,"jev_responses":[]}));
    }

    async fn request(state: AppState) -> Response {
        router(state)
            .oneshot(
                Request::post("/v1/extract")
                    .body(Body::empty())
                    .expect("construct the local empty extraction request"),
            )
            .await
            .expect("the router must return an HTTP response")
    }

    #[tokio::test]
    async fn admission_rejects_busy_cooldown_and_paused_before_parsing_upload() {
        let state = state();
        let permit = state
            .jobs
            .clone()
            .try_acquire_owned()
            .expect("the idle server must have an available job permit");
        let response = request(state.clone()).await;
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
        assert_error(response, StatusCode::TOO_MANY_REQUESTS, "server_busy").await;
        drop(permit);

        state
            .admission
            .lock()
            .expect("the admission state must not have been poisoned in this test")
            .next = Instant::now() + Duration::from_secs(10);
        let response = request(state.clone()).await;
        let retry_after: u64 = response.headers()[header::RETRY_AFTER]
            .to_str()
            .expect("Retry-After must be a textual header value")
            .parse()
            .expect("Retry-After must contain an unsigned seconds count");
        assert!((1..=11).contains(&retry_after));
        assert_error(response, StatusCode::TOO_MANY_REQUESTS, "server_busy").await;
        assert_eq!(state.jobs.available_permits(), 1);

        state
            .admission
            .lock()
            .expect("the admission state must not have been poisoned in this test")
            .paused = true;
        assert_error(
            request(state.clone()).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "remote_paused_unknown_outcome",
        )
        .await;
        assert_eq!(state.jobs.available_permits(), 1);
    }

    async fn write_all(stream: &tokio::net::TcpStream, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            stream.writable().await?;
            match stream.try_write(bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn read_head(stream: &tokio::net::TcpStream) -> String {
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            stream
                .readable()
                .await
                .expect("the local response socket must become readable");
            match stream.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    async fn busy_server(state: &AppState) -> tokio::net::TcpStream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the local test server to an available loopback port");
        let address = listener
            .local_addr()
            .expect("the bound listener must expose its assigned address");
        tokio::spawn(axum::serve(listener, router(state.clone())).into_future());
        tokio::net::TcpStream::connect(address)
            .await
            .expect("connect to the local test server")
    }

    fn upload_head(length: usize) -> String {
        format!(
            "POST /v1/extract HTTP/1.1\r\nhost: test\r\ncontent-type: multipart/form-data; boundary=b\r\ncontent-length: {length}\r\n\r\n"
        )
    }

    fn busy_state(upload_timeout_ms: u64) -> AppState {
        let mut config =
            ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec())
                .expect("the checked-in mock must initialize the fixture server config");
        config.upload_timeout_ms = upload_timeout_ms;
        AppState::try_new(config).expect("the requested upload deadline must initialize app state")
    }

    async fn response_head(stream: &tokio::net::TcpStream) -> String {
        tokio::time::timeout(Duration::from_secs(5), read_head(stream))
            .await
            .expect("no response")
    }

    #[tokio::test]
    async fn busy_rejection_arrives_after_an_upload_near_the_limit() {
        let state = busy_state(30_000);
        let _permit = state
            .jobs
            .clone()
            .try_acquire_owned()
            .expect("the idle server must have an available job permit");
        let body = vec![b'x'; upload_limit(&state) - 1024];
        let stream = busy_server(&state).await;
        write_all(&stream, upload_head(body.len()).as_bytes())
            .await
            .expect("write the near-limit upload headers to the local server");
        write_all(&stream, &body)
            .await
            .expect("write the near-limit upload body to the local server");
        let response = response_head(&stream).await;
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");
        assert!(
            response.to_ascii_lowercase().contains("retry-after: 2"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn busy_rejection_of_a_stalled_upload_waits_for_the_upload_deadline() {
        let state = busy_state(300);
        let _permit = state
            .jobs
            .clone()
            .try_acquire_owned()
            .expect("the idle server must have an available job permit");
        let stream = busy_server(&state).await;
        write_all(&stream, upload_head(1024).as_bytes())
            .await
            .expect("write the stalled upload headers to the local server");
        write_all(&stream, b"partial")
            .await
            .expect("write the partial upload body to the local server");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), stream.readable())
                .await
                .is_err(),
            "answered before the upload deadline"
        );
        let response = response_head(&stream).await;
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");
    }

    #[tokio::test]
    async fn busy_rejection_skips_the_upload_it_cannot_help() {
        let state = busy_state(30_000);
        let _permit = state
            .jobs
            .clone()
            .try_acquire_owned()
            .expect("the idle server must have an available job permit");

        let stream = busy_server(&state).await;
        let head = upload_head(1024).replace("\r\n\r\n", "\r\nexpect: 100-continue\r\n\r\n");
        write_all(&stream, head.as_bytes())
            .await
            .expect("write the Expect header to the local server");
        let response = response_head(&stream).await;
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");

        let stream = busy_server(&state).await;
        write_all(&stream, upload_head(upload_limit(&state) + 1).as_bytes())
            .await
            .expect("write the oversized upload headers to the local server");
        let response = response_head(&stream).await;
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");

        let _slots = state
            .discards
            .clone()
            .try_acquire_many_owned(MAX_DISCARDS as u32)
            .expect("the idle server must have all discard slots available");
        let stream = busy_server(&state).await;
        write_all(&stream, upload_head(1024).as_bytes())
            .await
            .expect("write the upload headers while discard slots are occupied");
        let response = response_head(&stream).await;
        assert!(response.starts_with("HTTP/1.1 429"), "{response}");
    }

    #[tokio::test]
    async fn upload_deadline_releases_admission_permit() {
        let state = state();
        let permit = state
            .jobs
            .clone()
            .try_acquire_owned()
            .expect("the idle server must have an available job permit");
        let upload = async move {
            let _permit = permit;
            std::future::pending::<Result<Upload, (StatusCode, &'static str)>>().await
        };
        let response = bounded_upload(upload, Duration::from_millis(1))
            .await
            .expect_err("the pending upload must exceed its one-millisecond deadline");
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
        let expected = serde_json::to_value(&responses)
            .expect("the text and binary response records must serialize as JSON");
        let error = engine::EngineError::WithResponses {
            source: Box::new(engine::EngineError::Provenance),
            jev_responses: responses,
        };
        let state = state();
        let result = Err(error);
        finish_remote(&state, &result);
        {
            let admission = state
                .admission
                .lock()
                .expect("the admission state must not have been poisoned in this test");
            assert!(admission.paused);
            assert!(
                admission.next.saturating_duration_since(Instant::now()) > Duration::from_secs(11)
            );
        }
        let Err(error) = result else { unreachable!() };
        let response = extraction_error(error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("the extraction error response must fit the test read limit");
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).expect("the extraction error response must be JSON");
        assert_eq!(body["error"], "extraction_failed");
        assert_eq!(body["jev_responses"], expected);
        assert_eq!(body["jev_responses"][0]["body"], raw);
        assert_eq!(body["jev_responses"][1]["body"], json!([255, 0]));
    }

    fn text_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_owned(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_owned(),
            format!("<</Length {}>>\nstream\n{stream}\nendstream", stream.len()),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        pdf
    }

    async fn extract_pdf(state: &AppState, pdf: &[u8], options: &str) -> Response {
        let mut body = Vec::new();
        for (name, content) in [
            ("pdf", pdf),
            ("options", options.as_bytes()),
            (
                "schema",
                &include_bytes!("../../../examples/text-form.json")[..],
            ),
        ] {
            body.extend(
                format!("--b\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n").bytes(),
            );
            body.extend(content);
            body.extend(b"\r\n");
        }
        body.extend(b"--b--\r\n");
        router(state.clone())
            .oneshot(
                Request::post("/v1/extract")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=b")
                    .body(Body::from(body))
                    .expect("construct the local multipart extraction request"),
            )
            .await
            .expect("the router must return a response to the multipart request")
    }

    fn reuse_state(ttl_ms: u64) -> (AppState, Arc<DocumentReuse>) {
        let mut config =
            ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec())
                .expect("the checked-in mock must initialize the fixture server config");
        config.reuse_document = true;
        config.reuse_document_ttl_ms = ttl_ms;
        let state = AppState::try_new(config)
            .expect("the enabled reuse configuration must initialize app state");
        let reuse = state
            .reuse
            .clone()
            .expect("enabling reuse must allocate a document cache");
        (state, reuse)
    }

    fn cached(reuse: &DocumentReuse) -> bool {
        !locked(&reuse.cache).is_empty()
    }

    async fn until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "condition not reached");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // Holding the cache lock is how the test keeps the job inside its blocking section.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn reused_document_is_dropped_after_the_ttl_even_if_the_client_disconnects() {
        let (state, reuse) = reuse_state(1000);
        let held = locked(&reuse.cache);
        // The job clones the cache handle just before it blocks on the lock held here.
        let waiting = Arc::strong_count(&reuse.cache) + 1;
        let request = tokio::spawn({
            let state = state.clone();
            async move { extract_pdf(&state, &text_pdf("ALPHA42"), "{}").await }
        });
        until(|| Arc::strong_count(&reuse.cache) >= waiting).await;
        request.abort();
        let _ = request.await;
        drop(held);
        until(|| state.jobs.available_permits() == 1).await;
        assert!(cached(&reuse));
        until(|| !cached(&reuse)).await;
    }

    #[tokio::test]
    async fn a_job_failing_before_it_opens_the_document_does_not_extend_its_lifetime() {
        let (state, reuse) = reuse_state(1500);
        let response = extract_pdf(&state, &text_pdf("ALPHA42"), "{}").await;
        assert_eq!(response.status(), StatusCode::OK);
        let filled = Instant::now();
        tokio::time::sleep(Duration::from_millis(900)).await;
        let invalid = r#"{"evaluation_profile":{"min_confidence":2.0,"noul_true_min":0.9,"noul_false_max":0.1}}"#;
        let response = extract_pdf(&state, &text_pdf("ALPHA42"), invalid).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(cached(&reuse));
        tokio::time::sleep(
            (filled + Duration::from_millis(2100)).saturating_duration_since(Instant::now()),
        )
        .await;
        assert!(!cached(&reuse));
    }

    #[test]
    fn janitor_releases_the_document_without_traffic_or_a_runtime() {
        let (state, reuse) = reuse_state(1000);
        let runtime = tokio::runtime::Runtime::new()
            .expect("create the runtime used to populate the document cache");
        let response = runtime.block_on(extract_pdf(&state, &text_pdf("ALPHA42"), "{}"));
        assert_eq!(response.status(), StatusCode::OK);
        drop(runtime);
        assert!(cached(&reuse));
        let deadline = Instant::now() + Duration::from_secs(5);
        while cached(&reuse) {
            assert!(Instant::now() < deadline, "document was not released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn janitor_exits_when_the_server_state_is_dropped() {
        let (state, reuse) = reuse_state(60_000);
        let weak = Arc::downgrade(&reuse);
        drop((state, reuse));
        let deadline = Instant::now() + Duration::from_secs(5);
        while weak.upgrade().is_some() {
            assert!(Instant::now() < deadline, "janitor still holds the state");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn reuse_is_off_by_default_and_the_ttl_is_range_checked() {
        let config = || {
            ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec())
                .expect("the checked-in mock must initialize the fixture server config")
        };
        assert!(
            AppState::try_new(config())
                .expect("the default fixture server config must initialize app state")
                .reuse
                .is_none()
        );
        for ttl in [0, 3_600_001] {
            let mut config = config();
            config.reuse_document_ttl_ms = ttl;
            assert_eq!(
                AppState::try_new(config).err(),
                Some("invalid_reuse_document_ttl")
            );
        }
    }
}
