use super::*;
use axum::body::{self, Body};
use serde_json::Value;
use tower::ServiceExt;

fn state_with(edit: impl FnOnce(&mut ServerConfig)) -> AppState {
    let mut config = ServerConfig::default();
    edit(&mut config);
    AppState::try_new(config).expect("the test config is valid")
}
fn state() -> AppState {
    state_with(|_| {})
}

async fn json_body(response: Response) -> Value {
    let bytes = body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("the response body fits the test read limit");
    serde_json::from_slice(&bytes).expect("the response is JSON")
}
async fn assert_error(response: Response, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status, "{code}");
    assert_eq!(
        json_body(response).await,
        json!({"error":code,"jev_responses":[]})
    );
}

/// One page per content stream. Objects: catalog, page tree, font, then a page and
/// its content.
fn streams_pdf(streams: &[String]) -> Vec<u8> {
    let kids: Vec<String> = (0..streams.len())
        .map(|i| format!("{} 0 R", 4 + 2 * i))
        .collect();
    let mut objects = vec![
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        format!(
            "<</Type/Pages/Kids[{}]/Count {}>>",
            kids.join(" "),
            streams.len()
        ),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_owned(),
    ];
    for (i, stream) in streams.iter().enumerate() {
        objects.push(format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents {} 0 R/Resources<</Font<</F1 3 0 R>>>>>>",
            5 + 2 * i
        ));
        objects.push(format!(
            "<</Length {}>>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf += &format!("{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = pdf.len();
    pdf += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        pdf += &format!("{offset:010} 00000 n \n");
    }
    pdf += &format!(
        "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    pdf.into_bytes()
}
/// One text line per page.
fn pages_pdf(texts: &[&str]) -> Vec<u8> {
    let streams: Vec<String> = texts
        .iter()
        .map(|text| format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET"))
        .collect();
    streams_pdf(&streams)
}
fn text_pdf(text: &str) -> Vec<u8> {
    pages_pdf(&[text])
}
/// One page of `rows` stacked two-cell rows in a tiny font, so up to 10,000 text
/// items (`PdfLimits::max_items_per_page`) fit the page.
fn stacked_rows_pdf(
    rows: usize,
    label: impl Fn(usize) -> String,
    value: impl Fn(usize) -> String,
) -> Vec<u8> {
    let mut stream = String::from("BT /F1 0.12 Tf 72 785 Td");
    for i in 0..rows {
        stream += &format!(
            " ({}) Tj 200 0 Td ({}) Tj -200 -0.156 Td",
            label(i),
            value(i)
        );
    }
    streams_pdf(&[stream + " ET"])
}
/// One page of `lines` one-item lines in one column, alternating `Note` and `x`.
fn stacked_lines_pdf(lines: usize) -> Vec<u8> {
    let mut stream = String::from("BT /F1 0.3 Tf 72 785 Td");
    for i in 0..lines {
        stream += &format!(" ({}) Tj 0 -0.39 Td", if i % 2 == 0 { "Note" } else { "x" });
    }
    streams_pdf(&[stream + " ET"])
}

fn multipart(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, content) in parts {
        body.extend(
            format!("--b\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n").bytes(),
        );
        body.extend(*content);
        body.extend(b"\r\n");
    }
    body.extend(b"--b--\r\n");
    body
}

const FORM: &[u8] = br#"{"fields":[
    {"id":"reference","label":"Ref"},
    {"id":"missing","label":"Total","type":"amount"}
]}"#;
/// Multipart parts as (name, content), in sending order.
type Parts<'a> = &'a [(&'a str, &'a [u8])];

async fn post_raw(
    state: &AppState,
    body: Body,
    headers: &[(header::HeaderName, &str)],
) -> Response {
    let mut request =
        Request::post("/v1/fill").header(header::CONTENT_TYPE, "multipart/form-data; boundary=b");
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    router(state.clone())
        .oneshot(request.body(body).expect("a valid local request"))
        .await
        .expect("the router answers")
}
async fn post_fill(
    state: &AppState,
    parts: Parts<'_>,
    headers: &[(header::HeaderName, &str)],
) -> Response {
    post_raw(state, Body::from(multipart(parts)), headers).await
}

async fn assert_filled(response: Response) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["evaluation_only"], true);
    assert_eq!(body["document_id"].as_str().map(str::len), Some(64));
    let reference = &body["fields"]["reference"];
    assert_eq!(reference["status"], "filled");
    assert_eq!(reference["raw"], "AB12");
    assert_eq!(reference["value"], "AB12");
    assert_eq!(reference["evidence"][0]["text"], "AB12");
    assert_eq!(reference["evidence"][0]["page"], 1);
    assert_eq!(body["fields"]["missing"]["status"], "not_found");
}
/// The job slot is free again and serves a normal fill.
async fn assert_slot_reusable(state: &AppState) {
    assert_eq!(state.jobs.available_permits(), 1);
    let pdf = text_pdf("Ref: AB12");
    assert_filled(post_fill(state, &[("pdf", &pdf), ("form", FORM)], &[]).await).await;
}
async fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "condition not reached");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn fill_returns_the_labelled_source_value_and_health_answers() {
    let state = state();
    let pdf = text_pdf("Ref: AB12");
    assert_filled(post_fill(&state, &[("form", FORM), ("pdf", &pdf)], &[]).await).await;
    assert_eq!(state.jobs.available_permits(), 1);
    let health = router(state)
        .oneshot(
            Request::get("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("the router answers");
    assert_eq!(json_body(health).await, json!({"status":"ok"}));
}

#[tokio::test]
async fn fill_returns_the_same_json_as_the_library_including_holds() {
    // A label and a value without a delimiter on one line: a held candidate.
    let pdf = streams_pdf(&["BT /F1 24 Tf 72 700 Td (Ref) Tj 80 0 Td (AB12) Tj ET".to_owned()]);
    let state = state();
    let response = post_fill(&state, &[("pdf", &pdf), ("form", FORM)], &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let snapshot = vf_source::inspect(&pdf, state.config.pdf).expect("inspects");
    let form = fill::parse_form(FORM).expect("parses");
    let direct = fill::fill_with_limits(
        &snapshot.document_id,
        &snapshot.fragments,
        &form,
        &fill::FillLimits::default(),
    )
    .expect("the small document stays within the limits");
    let direct = serde_json::to_vec(&direct).expect("serializes");
    assert_eq!(
        body,
        serde_json::from_slice::<Value>(&direct).expect("parses")
    );
    let reference = &body["fields"]["reference"];
    assert_eq!(reference["status"], "ambiguous");
    assert!(
        reference["candidates"][0]["holds"]
            .as_array()
            .is_some_and(|h| !h.is_empty()),
        "{reference}"
    );
    assert_eq!(state.jobs.available_permits(), 1);
}

#[tokio::test]
async fn fill_rejects_bad_uploads_and_applies_the_configured_pdf_limits() {
    let pdf = text_pdf("Ref: AB12");
    let two_pages = pages_pdf(&["Ref: AB12", "Ref: CD34"]);
    let oversized_form = vec![b' '; MAX_FORM_BYTES + 1];
    let state = state();
    let cases: [(Parts, StatusCode, &str); 9] = [
        (
            &[("pdf", &pdf)],
            StatusCode::BAD_REQUEST,
            "missing_pdf_or_form",
        ),
        (
            &[("form", FORM)],
            StatusCode::BAD_REQUEST,
            "missing_pdf_or_form",
        ),
        (
            &[("pdf", &pdf), ("schema", FORM)],
            StatusCode::BAD_REQUEST,
            "unknown_or_duplicate_part",
        ),
        (
            &[("pdf", &pdf), ("form", FORM), ("form", FORM)],
            StatusCode::BAD_REQUEST,
            "unknown_or_duplicate_part",
        ),
        (
            &[("pdf", &pdf), ("form", b"{")],
            StatusCode::BAD_REQUEST,
            "invalid_form",
        ),
        (
            &[("pdf", &pdf), ("form", br#"{"fields":[]}"#)],
            StatusCode::BAD_REQUEST,
            "invalid_form",
        ),
        (
            &[
                ("pdf", &pdf),
                ("form", br#"{"fields":[{"id":"a","label":"x","extra":1}]}"#),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_form",
        ),
        (
            &[("pdf", &pdf), ("form", &oversized_form)],
            StatusCode::PAYLOAD_TOO_LARGE,
            "part_too_large",
        ),
        (
            &[("pdf", b"not a pdf"), ("form", FORM)],
            StatusCode::BAD_REQUEST,
            "pdf_inspection_failed",
        ),
    ];
    for (parts, status, code) in cases {
        assert_error(post_fill(&state, parts, &[]).await, status, code).await;
        assert_eq!(state.jobs.available_permits(), 1, "{code}");
    }
    // Lowered PDF limits: bytes per part, then pages.
    let small = state_with(|c| c.pdf.max_bytes = pdf.len() - 1);
    let one_page = state_with(|c| c.pdf.max_pages = 1);
    for (state, pdf, status, code) in [
        (
            &small,
            &pdf,
            StatusCode::PAYLOAD_TOO_LARGE,
            "part_too_large",
        ),
        (
            &one_page,
            &two_pages,
            StatusCode::PAYLOAD_TOO_LARGE,
            "input_too_large",
        ),
    ] {
        assert_error(
            post_fill(state, &[("pdf", pdf), ("form", FORM)], &[]).await,
            status,
            code,
        )
        .await;
        assert_eq!(state.jobs.available_permits(), 1, "{code}");
    }
}

#[tokio::test]
async fn fill_rejects_malformed_and_oversized_multipart() {
    let state = state_with(|c| c.pdf.max_bytes = 1024);
    // A preamble before the first boundary counts toward the whole-upload limit
    // although no part exceeds its own.
    let mut oversized = vec![b'x'; upload_limit(&state) + 1];
    oversized.extend(multipart(&[("pdf", b"%PDF")]));
    let plain = router(state.clone())
        .oneshot(
            Request::post("/v1/fill")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("the router answers");
    assert_error(plain, StatusCode::BAD_REQUEST, "invalid_multipart").await;
    let truncated = b"--b\r\nContent-Disposition: form-data; name=\"pdf\"\r\n\r\n%PDF".to_vec();
    for (body, status) in [
        (truncated, StatusCode::BAD_REQUEST),
        (oversized, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        assert_error(
            post_raw(&state, Body::from(body), &[]).await,
            status,
            "invalid_or_oversized_upload",
        )
        .await;
        assert_eq!(state.jobs.available_permits(), 1);
    }
}

#[tokio::test]
async fn fill_requires_the_configured_bearer_key_and_browser_authentication() {
    let pdf = text_pdf("Ref: AB12");
    let parts: Parts = &[("pdf", &pdf), ("form", FORM)];
    let origin = (header::ORIGIN, "https://app.example");
    assert_error(
        post_fill(&state(), parts, std::slice::from_ref(&origin)).await,
        StatusCode::FORBIDDEN,
        "browser_authentication_required",
    )
    .await;
    let state = state_with(|c| {
        c.api_key = Some(Secret::new("synthetic-server-key".into()).expect("valid bearer"))
    });
    for headers in [
        &[][..],
        &[(header::AUTHORIZATION, "Bearer synthetic-server-kex")][..],
        &[(header::AUTHORIZATION, "synthetic-server-key")][..],
    ] {
        assert_error(
            post_fill(&state, parts, headers).await,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        )
        .await;
    }
    let authorized = [
        (header::AUTHORIZATION, "Bearer synthetic-server-key"),
        origin,
    ];
    assert_filled(post_fill(&state, parts, &authorized).await).await;
}

#[tokio::test]
async fn fill_limits_answer_errors_never_partial_results_and_free_the_slot() {
    // Before the layout bounds, n stacked lines gave n(n+1)/2 value spans.
    let state = state();
    let note: &[u8] = br#"{"fields":[{"id":"note","label":"Note"}]}"#;
    let started = Instant::now();
    let response = post_fill(
        &state,
        &[("pdf", &stacked_lines_pdf(2000)), ("form", note)],
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("fits");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(bytes.len() < 20_000, "{}", bytes.len());
    let body: Value = serde_json::from_slice(&bytes).expect("JSON");
    let candidates = body["fields"]["note"]["candidates"]
        .as_array()
        .expect("ambiguous candidates");
    assert_eq!(body["fields"]["note"]["status"], "ambiguous");
    assert!(!candidates.is_empty() && candidates.len() <= 5);
    assert!(candidates.iter().all(|c| {
        c["evidence"]
            .as_array()
            .is_some_and(|e| e.len() <= vf_source::layout::MAX_VALUE_LINES)
    }));
    assert_slot_reusable(&state).await;

    // 5,000 stacked rows (10,000 items) exceed the 50,000 layout hypotheses; the
    // largest form (512 fields, 16 aliases each) against 4,000 rows exceeds the
    // matching budget.
    let hints: Vec<String> = (0..16).map(|h| format!("Alias{h:02}")).collect();
    let fields: Vec<Value> = (0..fill::MAX_FIELDS)
        .map(|f| json!({"id": format!("f{f}"), "label": format!("Item{f:03}"), "hints": hints}))
        .collect();
    let large_form = serde_json::to_vec(&json!({ "fields": fields })).expect("serializes");
    assert!(large_form.len() <= MAX_FORM_BYTES);
    let cases: [(Vec<u8>, &[u8], &str); 2] = [
        (
            stacked_rows_pdf(5000, |_| "A".into(), |_| "1".into()),
            br#"{"fields":[{"id":"a","label":"A"}]}"#,
            "layout_limit_exceeded",
        ),
        (
            stacked_rows_pdf(4000, |i| format!("Label{i:05}"), |i| format!("V{i:05}")),
            &large_form,
            "fill_budget_exceeded",
        ),
    ];
    for (pdf, form, code) in cases {
        let started = Instant::now();
        assert_error(
            post_fill(&state, &[("pdf", &pdf), ("form", form)], &[]).await,
            StatusCode::PAYLOAD_TOO_LARGE,
            code,
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(30), "{code}");
        assert_slot_reusable(&state).await;
    }

    // Inspection is not interrupted but counts: 2,000 lines take longer than 1 ms.
    let state = state_with(|c| c.fill_deadline = Duration::from_millis(1));
    assert_error(
        post_fill(
            &state,
            &[("pdf", &stacked_lines_pdf(2000)), ("form", note)],
            &[],
        )
        .await,
        StatusCode::GATEWAY_TIMEOUT,
        "fill_deadline_exceeded",
    )
    .await;
    assert_eq!(state.jobs.available_permits(), 1);
    for (failure, status) in [
        (
            fill::FillError::LayoutLimitExceeded,
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            fill::FillError::BudgetExceeded,
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            fill::FillError::DeadlineExceeded,
            StatusCode::GATEWAY_TIMEOUT,
        ),
    ] {
        assert_eq!(fill_error(failure).status(), status, "{}", failure.code());
    }
}

#[tokio::test]
async fn a_panicking_fill_worker_answers_worker_failed_and_frees_the_slot() {
    let state = state();
    let permit = state
        .jobs
        .clone()
        .try_acquire_owned()
        .expect("an idle slot");
    let response = run_fill_job(permit, || -> Response {
        panic!("synthetic fill worker panic")
    })
    .await;
    assert_error(response, StatusCode::INTERNAL_SERVER_ERROR, "worker_failed").await;
    assert_slot_reusable(&state).await;
}

async fn write_all(stream: &tokio::net::TcpStream, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        stream.writable().await.expect("writable");
        match stream.try_write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
    }
}
/// The response head, within 5 s.
async fn response_head(stream: &tokio::net::TcpStream) -> String {
    let read = async {
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            stream.readable().await.expect("readable");
            match stream.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
        String::from_utf8_lossy(&head).into_owned()
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("no response")
}
async fn server(state: &AppState) -> tokio::net::TcpStream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("bound address");
    tokio::spawn(axum::serve(listener, router(state.clone())).into_future());
    tokio::net::TcpStream::connect(address)
        .await
        .expect("connect")
}
fn upload_head(length: usize) -> String {
    format!(
        "POST /v1/fill HTTP/1.1\r\nhost: test\r\ncontent-type: multipart/form-data; boundary=b\r\ncontent-length: {length}\r\n\r\n"
    )
}

#[tokio::test]
async fn busy_rejection_arrives_after_an_upload_near_the_limit_or_its_deadline() {
    let state = state();
    let _permit = state
        .jobs
        .clone()
        .try_acquire_owned()
        .expect("an idle slot");
    let body = vec![b'x'; upload_limit(&state) - 1024];
    let stream = server(&state).await;
    write_all(&stream, upload_head(body.len()).as_bytes()).await;
    write_all(&stream, &body).await;
    let response = response_head(&stream).await;
    assert!(response.starts_with("HTTP/1.1 429"), "{response}");
    assert!(
        response.to_ascii_lowercase().contains("retry-after: 2"),
        "{response}"
    );

    let state = state_with(|c| c.upload_timeout_ms = 300);
    let _permit = state
        .jobs
        .clone()
        .try_acquire_owned()
        .expect("an idle slot");
    let stream = server(&state).await;
    write_all(&stream, upload_head(1024).as_bytes()).await;
    write_all(&stream, b"partial").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.readable())
            .await
            .is_err(),
        "answered before the upload deadline"
    );
    assert!(response_head(&stream).await.starts_with("HTTP/1.1 429"));
}

#[tokio::test]
async fn busy_rejection_skips_the_upload_it_cannot_help() {
    let state = state();
    let _permit = state
        .jobs
        .clone()
        .try_acquire_owned()
        .expect("an idle slot");
    let expect = upload_head(1024).replace("\r\n\r\n", "\r\nexpect: 100-continue\r\n\r\n");
    let oversized = upload_head(upload_limit(&state) + 1);
    for head in [expect, oversized] {
        let stream = server(&state).await;
        write_all(&stream, head.as_bytes()).await;
        assert!(response_head(&stream).await.starts_with("HTTP/1.1 429"));
    }
    let _slots = state
        .discards
        .clone()
        .try_acquire_many_owned(MAX_DISCARDS as u32)
        .expect("all slots");
    let stream = server(&state).await;
    write_all(&stream, upload_head(1024).as_bytes()).await;
    assert!(response_head(&stream).await.starts_with("HTTP/1.1 429"));
}

#[tokio::test]
async fn a_fill_holds_the_slot_until_its_worker_ends() {
    let state = state();
    let pdf = text_pdf("Ref: AB12");
    let body = multipart(&[("pdf", &pdf), ("form", FORM)]);
    let (first, rest) = body.split_at(body.len() / 2);
    let stream = server(&state).await;
    write_all(&stream, upload_head(body.len()).as_bytes()).await;
    write_all(&stream, first).await;
    until(|| state.jobs.available_permits() == 0).await;
    let response = post_fill(&state, &[("pdf", &pdf), ("form", FORM)], &[]).await;
    assert_eq!(response.headers()[header::RETRY_AFTER], "2");
    assert_error(response, StatusCode::TOO_MANY_REQUESTS, "server_busy").await;
    write_all(&stream, rest).await;
    let response = response_head(&stream).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    until(|| state.jobs.available_permits() == 1).await;
    assert_slot_reusable(&state).await;
}

#[tokio::test]
async fn a_stalled_upload_times_out_and_frees_the_slot() {
    let state = state_with(|c| c.upload_timeout_ms = 300);
    let stream = server(&state).await;
    write_all(&stream, upload_head(1024).as_bytes()).await;
    write_all(
        &stream,
        b"--b\r\nContent-Disposition: form-data; name=\"pdf\"\r\n\r\n%PDF",
    )
    .await;
    let response = response_head(&stream).await;
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    until(|| state.jobs.available_permits() == 1).await;
}
