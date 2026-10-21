//! Blocking HTTPS transport. Construction never sends a request. Production
//! callers use `transport::execute` with explicit remote permission and an
//! injected secret. Development tests never execute a valid network request.
use super::transport::{HttpRequest, HttpResponse, Transport, TransportError};
use reqwest::{
    blocking::{Client, Request},
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER},
    redirect,
};
use std::{
    io::Read,
    time::{Duration, SystemTime},
};

/// Hard transport ceiling in addition to each caller’s lower response limit.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// A trusted operator-supplied HTTPS destination. No environment is read here.
/// Validation excludes credentials, query strings, fragments and ambiguous URL syntax;
/// the caller remains responsible for trusting the destination and its DNS resolution.
#[derive(Clone)]
pub struct Endpoint(reqwest::Url);
impl Endpoint {
    pub fn new(value: &str) -> Result<Self, TransportError> {
        if value.len() > 2048
            || !value.starts_with("https://")
            || value
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b == b'\\')
            || value[8..]
                .split('/')
                .next()
                .is_none_or(|authority| authority.is_empty() || authority.contains('@'))
        {
            return Err(TransportError::BeforeSend);
        }
        let url = reqwest::Url::parse(value).map_err(|_| TransportError::BeforeSend)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(TransportError::BeforeSend);
        }
        Ok(Self(url))
    }
}
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Endpoint([REDACTED])")
    }
}

/// Transport bound to one endpoint, with environment proxies and redirects disabled.
/// Debug deliberately omits the client and request metadata.
#[derive(Clone)]
pub struct HttpTransport {
    client: Client,
    endpoint: Endpoint,
}
impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpTransport([REDACTED])")
    }
}
impl HttpTransport {
    pub fn new(endpoint: Endpoint) -> Result<Self, TransportError> {
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(redirect::Policy::none())
            // Protocol-level retries also consume provider request/rate/budget slots.
            // Only the explicit execute policy may authorize another attempt.
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| TransportError::BeforeSend)?;
        Ok(Self { client, endpoint })
    }
    fn prepare(&self, input: &HttpRequest<'_>) -> Result<Request, TransportError> {
        // Routing, TLS and redirect policy belong to this transport, never a request.
        if input.timeout_ms == 0
            || input.max_response_bytes == 0
            || input.max_response_bytes > MAX_RESPONSE_BYTES
        {
            return Err(TransportError::BeforeSend);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", input.secret.bearer()))
            .map_err(|_| TransportError::BeforeSend)?;
        authorization.set_sensitive(true);
        let content_type = HeaderValue::from_str(&input.body.content_type)
            .map_err(|_| TransportError::BeforeSend)?;
        let mut builder = self
            .client
            .post(self.endpoint.0.clone())
            .timeout(Duration::from_millis(input.timeout_ms))
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, content_type)
            .body(input.body.shared_bytes());
        if input.body.danger_zone_images {
            builder = builder.header("TypeSafe-Danger-Zone-Images", "true");
        }
        let request = builder.build().map_err(|_| TransportError::BeforeSend)?;
        Ok(request)
    }
}
impl Transport for HttpTransport {
    fn send(&mut self, input: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        let request = self.prepare(&input)?;
        // Even connection/time-out failures may occur after a body was sent.
        // Never return provider messages, URLs, body snippets or request Debug.
        let response = self
            .client
            .execute(request)
            .map_err(|_| TransportError::OutcomeUnknown)?;
        let status = response.status().as_u16();
        let now = SystemTime::now();
        let retry_after_ms = response
            .headers()
            .get_all(RETRY_AFTER)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| retry_after(v, now))
            .max();
        let (body, body_error) = if response
            .content_length()
            .is_some_and(|n| n > input.max_response_bytes as u64)
        {
            (Vec::new(), Some(TransportError::ResponseTooLarge))
        } else {
            read_body(response, input.max_response_bytes)
        };
        Ok(HttpResponse {
            status,
            body,
            body_error,
            retry_after_ms,
        })
    }
}
fn read_body(reader: impl Read, limit: usize) -> (Vec<u8>, Option<TransportError>) {
    let mut body = Vec::new();
    let read_error = reader.take(limit as u64 + 1).read_to_end(&mut body).err();
    let error = if body.len() > limit {
        Some(TransportError::ResponseTooLarge)
    } else {
        read_error.map(|_| TransportError::OutcomeUnknown)
    };
    (body, error)
}
fn retry_after(value: &str, now: SystemTime) -> Option<u64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        // Oversized server waits must stop retries, never wrap to an early retry.
        return Some(
            value
                .parse::<u64>()
                .unwrap_or(u64::MAX)
                .saturating_mul(1000),
        );
    }
    let at = httpdate::parse_http_date(value).ok()?;
    let delay = at.duration_since(now).unwrap_or(Duration::ZERO);
    // Round upwards: fractional milliseconds must not cause an early retry.
    Some(u64::try_from(delay.as_nanos().saturating_add(999_999) / 1_000_000).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, multipart::Body, parse_request, transport::Secret};
    const ENDPOINT: &str = "https://synthetic.example.invalid/custom/extract";
    #[test]
    fn builds_bearer_and_json_without_sending() {
        let transport = HttpTransport::new(Endpoint::new(ENDPOINT).unwrap()).unwrap();
        let request=parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,Limits::default()).unwrap();
        let body = Body::json(&request).unwrap();
        let secret = Secret::new("synthetic-token".into()).unwrap();
        let input = HttpRequest {
            secret: &secret,
            body: &body,
            timeout_ms: 1234,
            max_response_bytes: 100,
        };
        let request = transport.prepare(&input).unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().as_str(), ENDPOINT);
        assert_eq!(request.headers()[AUTHORIZATION], "Bearer synthetic-token");
        assert!(request.headers()[AUTHORIZATION].is_sensitive());
        assert_eq!(request.headers()[CONTENT_TYPE], "application/json");
        assert!(
            !request
                .headers()
                .contains_key("TypeSafe-Danger-Zone-Images")
        );
        assert_eq!(request.timeout(), Some(&Duration::from_millis(1234)));
        assert_eq!(request.body().unwrap().as_bytes(), Some(body.bytes()));
        let retry = transport
            .prepare(&HttpRequest {
                timeout_ms: 1000,
                ..input
            })
            .unwrap();
        assert_eq!(retry.body().unwrap().as_bytes(), Some(body.bytes()));
        assert_eq!(retry.headers(), request.headers());
        assert_eq!(retry.timeout(), Some(&Duration::from_millis(1000)));
        drop(body);
        assert_eq!(
            retry.body().unwrap().as_bytes(),
            request.body().unwrap().as_bytes()
        );
    }
    #[test]
    fn endpoint_rejects_unsafe_or_ambiguous_syntax_without_echoing_it() {
        for value in [
            "",
            "http://synthetic.invalid/path",
            "https:///path",
            "https://",
            "https://user:private@synthetic.invalid/path",
            "https://@synthetic.invalid/path",
            "https://synthetic.invalid/path?private=value",
            "https://synthetic.invalid/path#private",
            "https://synthetic.invalid/\nprivate",
            " https://synthetic.invalid/path",
            "https://synthetic.invalid\\private",
            "https://synthetic.invalid:65536/path",
        ] {
            assert_eq!(format!("{:?}", Endpoint::new(value)), "Err(BeforeSend)");
        }
        assert!(Endpoint::new(&format!("https://synthetic.invalid/{}", "x".repeat(2048))).is_err());
        for value in [
            ENDPOINT,
            "https://synthetic.invalid:8443/v2/extract",
            "https://[::1]/custom",
        ] {
            let endpoint = Endpoint::new(value).unwrap();
            assert_eq!(format!("{endpoint:?}"), "Endpoint([REDACTED])");
            let transport = HttpTransport::new(endpoint.clone()).unwrap();
            assert_eq!(transport.endpoint.0.as_str(), value);
            assert_eq!(transport.clone().endpoint.0, endpoint.0);
        }
    }
    #[test]
    fn retry_after_handles_seconds_dates_overflow_and_past() {
        let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(retry_after("12", now), Some(12_000));
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:28:03 GMT", now),
            Some(3000)
        );
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:27:59 GMT", now), Some(0));
        assert_eq!(
            retry_after(
                "Wed, 21 Oct 2015 07:28:01 GMT",
                now + Duration::from_nanos(1)
            ),
            Some(1000)
        );
        assert_eq!(
            retry_after("18446744073709551616000000", now),
            Some(u64::MAX)
        );
        assert_eq!(retry_after("-1", now), None);
        assert_eq!(retry_after("untrusted-provider-message", now), None);
    }
    #[test]
    fn preflight_rejects_weakened_policy_without_executing_network() {
        let transport = HttpTransport::new(Endpoint::new(ENDPOINT).unwrap()).unwrap();
        let request=parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,Limits::default()).unwrap();
        let mut body = Body::json(&request).unwrap();
        let secret = Secret::new("synthetic-token".into()).unwrap();
        for scenario in 0..3 {
            let mut input = HttpRequest {
                secret: &secret,
                body: &body,
                timeout_ms: 100,
                max_response_bytes: 100,
            };
            match scenario {
                0 => input.timeout_ms = 0,
                1 => input.max_response_bytes = 0,
                2 => input.max_response_bytes = usize::MAX,
                _ => unreachable!(),
            }
            assert!(
                matches!(transport.prepare(&input), Err(TransportError::BeforeSend)),
                "scenario {scenario}"
            );
        }
        body.content_type = "application/json\r\nAuthorization: private".into();
        let input = HttpRequest {
            secret: &secret,
            body: &body,
            timeout_ms: 100,
            max_response_bytes: 100,
        };
        assert!(matches!(
            transport.prepare(&input),
            Err(TransportError::BeforeSend)
        ));
    }
    #[test]
    fn bounded_reads_preserve_complete_interrupted_and_oversized_bodies() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "synthetic disconnect",
                ))
            }
        }
        let (body, error) = read_body(std::io::Cursor::new(b"prefix").chain(Broken), 10);
        assert_eq!(body, b"prefix");
        assert_eq!(error, Some(TransportError::OutcomeUnknown));
        let (body, error) = read_body(std::io::Cursor::new(b"12345"), 3);
        assert_eq!(body, b"1234");
        assert_eq!(error, Some(TransportError::ResponseTooLarge));
        assert_eq!(
            read_body(std::io::Cursor::new(b"123"), 3),
            (b"123".to_vec(), None)
        );
    }
}
