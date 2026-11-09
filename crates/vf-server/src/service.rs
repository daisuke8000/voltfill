//! HTTP form-fill service: `/v1/fill` and `/health`. Nothing here sends to Jev.
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing;
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use vf_core::assist::Secret;
use vf_core::fill::{self, MAX_FORM_BYTES};
use vf_source::PdfLimits;

pub struct ServerConfig {
    pub pdf: PdfLimits,
    pub upload_timeout_ms: u64,
    pub api_key: Option<Secret>,
    /// Deadline of one `/v1/fill` job, counted from the start of its worker. PDF
    /// inspection is not interrupted, but its time counts, as with the CLI's
    /// default `voltfill fill --deadline-ms 30000`.
    fill_deadline: Duration,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            pdf: PdfLimits::default(),
            upload_timeout_ms: 30_000,
            api_key: None,
            fill_deadline: Duration::from_secs(30),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<ServerConfig>,
    /// One job at a time; a second request is answered 429.
    jobs: Arc<Semaphore>,
    discards: Arc<Semaphore>,
}
impl AppState {
    pub fn try_new(config: ServerConfig) -> Result<Self, &'static str> {
        if !(1..=3_600_000).contains(&config.upload_timeout_ms) {
            return Err("invalid_upload_timeout");
        }
        Ok(Self {
            config: Arc::new(config),
            jobs: Arc::new(Semaphore::new(1)),
            discards: Arc::new(Semaphore::new(MAX_DISCARDS)),
        })
    }
    fn upload_timeout(&self) -> Duration {
        Duration::from_millis(self.config.upload_timeout_ms)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/health",
            routing::get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/v1/fill", routing::post(fill_form))
        .layer(DefaultBodyLimit::max(upload_limit(&state)))
        .with_state(state)
}
fn upload_limit(state: &AppState) -> usize {
    state.config.pdf.max_bytes + MAX_FORM_BYTES + 128 * 1024
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error":code,"jev_responses":[]}))).into_response()
}
fn busy() -> Response {
    let mut response = error(StatusCode::TOO_MANY_REQUESTS, "server_busy");
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, header::HeaderValue::from_static("2"));
    response
}
/// Bearer when configured (compared in constant time), and browsers only with a key.
fn unauthenticated(request: &Request, key: Option<&Secret>) -> Option<Response> {
    let Some(key) = key else {
        return request
            .headers()
            .contains_key(header::ORIGIN)
            .then(|| error(StatusCode::FORBIDDEN, "browser_authentication_required"));
    };
    let actual = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .as_bytes();
    let expected = key.bearer().as_bytes();
    let mut difference = actual.len() ^ expected.len();
    for (index, byte) in expected.iter().enumerate() {
        difference |= usize::from(*byte ^ actual.get(index).copied().unwrap_or(0));
    }
    (difference != 0).then(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))
}

// Answering while the client is still uploading and then closing the connection shows
// up on the client as a reset rather than the response. Reading the rest of the upload
// first lets the rejection arrive. It is skipped where it cannot help: a client waiting
// for 100-continue has sent nothing, a declared length over the limit ends in a reset
// anyway, and rejected uploads already being read must not hold any number of
// connections.
const MAX_DISCARDS: usize = 4;

async fn reject_upload(request: Request, state: &AppState, response: Response) -> Response {
    use axum::body::HttpBody;
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
    let mut body = std::pin::pin!(request.into_body());
    let read = async {
        let mut seen = 0usize;
        while let Some(Ok(frame)) = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
            seen = seen.saturating_add(frame.data_ref().map_or(0, |data| data.len()));
            if seen > limit {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(state.upload_timeout(), read).await;
    response
}

/// Read `pdf` and `form` at most once each, within their byte limits; any other
/// part is rejected.
async fn read_upload(
    request: Request,
    state: &AppState,
) -> Result<[Option<Vec<u8>>; 2], (StatusCode, &'static str)> {
    let parts = [
        ("pdf", state.config.pdf.max_bytes),
        ("form", MAX_FORM_BYTES),
    ];
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_multipart"))?;
    let oversized = |status| (status, "invalid_or_oversized_upload");
    let mut upload = [None, None];
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| oversized(e.status()))?
    {
        let name = field.name().unwrap_or("");
        let slot = match parts.iter().position(|(part, _)| *part == name) {
            Some(slot) if upload[slot].is_none() => slot,
            _ => return Err((StatusCode::BAD_REQUEST, "unknown_or_duplicate_part")),
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|e| oversized(e.status()))? {
            if bytes.len().saturating_add(chunk.len()) > parts[slot].1 {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "part_too_large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        upload[slot] = Some(bytes);
    }
    Ok(upload)
}

/// `POST /v1/fill`: the HTTP form of `voltfill fill` without `--remote`. It matches
/// the form to the PDF's own text layer offline under the CLI's work limits and
/// the fill deadline; over a limit it answers an error, never a partial result.
async fn fill_form(State(state): State<AppState>, request: Request) -> Response {
    if let Some(response) = unauthenticated(&request, state.config.api_key.as_ref()) {
        return response;
    }
    let Ok(permit) = state.jobs.clone().try_acquire_owned() else {
        return reject_upload(request, &state, busy()).await;
    };
    let upload = tokio::time::timeout(state.upload_timeout(), read_upload(request, &state))
        .await
        .unwrap_or(Err((StatusCode::REQUEST_TIMEOUT, "upload_timeout")));
    let (pdf, definition) = match upload {
        Ok([Some(pdf), Some(form)]) => (pdf, form),
        Ok(_) => return error(StatusCode::BAD_REQUEST, "missing_pdf_or_form"),
        Err((status, code)) => return error(status, code),
    };
    let Ok(form) = fill::parse_form(&definition) else {
        return error(StatusCode::BAD_REQUEST, "invalid_form");
    };
    let (pdf_limits, deadline) = (state.config.pdf, state.config.fill_deadline);
    log::info!(target:"vf_server","{}",json!({"event":"fill_started","pdf_bytes":pdf.len(),"fields":form.fields.len()}));
    run_fill_job(permit, move || {
        let started = Instant::now();
        let (snapshot, acroform) = match vf_source::inspect_with_acroform(&pdf, pdf_limits) {
            Ok(inspected) => inspected,
            Err(vf_source::PdfError::LimitExceeded) => {
                return error(StatusCode::PAYLOAD_TOO_LARGE, "input_too_large");
            }
            Err(_) => return error(StatusCode::BAD_REQUEST, "pdf_inspection_failed"),
        };
        drop(pdf);
        let limits = fill::FillLimits {
            deadline: Some(started + deadline),
            ..fill::FillLimits::default()
        };
        let result = vf_core::acroform::fill_with_acroform(
            &snapshot.document_id,
            &snapshot.fragments,
            &acroform,
            &form,
            &limits,
        );
        match result {
            Ok(result) => {
                let filled = result
                    .fields
                    .values()
                    .filter(|f| f.status == fill::FillStatus::Filled)
                    .count();
                log::info!(target:"vf_server","{}",json!({"event":"fill_completed","fields":result.fields.len(),"filled":filled}));
                Json(result).into_response()
            }
            Err(failure) => {
                log::info!(target:"vf_server","{}",json!({"event":"fill_failed","error":failure.code()}));
                fill_error(failure)
            }
        }
    })
    .await
}

/// Runs a fill job on the blocking pool. The job permit moves into the worker, so the
/// slot is released when the job returns or unwinds.
async fn run_fill_job(
    permit: tokio::sync::OwnedSemaphorePermit,
    job: impl FnOnce() -> Response + Send + 'static,
) -> Response {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "worker_failed"))
}

/// The work limits answer 413 like the page limit (`input_too_large`); the deadline 504.
fn fill_error(failure: fill::FillError) -> Response {
    let status = match failure {
        fill::FillError::LayoutLimitExceeded | fill::FillError::BudgetExceeded => {
            StatusCode::PAYLOAD_TOO_LARGE
        }
        fill::FillError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
    };
    error(status, failure.code())
}

#[cfg(test)]
mod tests;
