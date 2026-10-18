//! Construction-only integration checks. No call to Transport::send occurs.
//! Request/header/Retry-After preparation is tested in the http module itself.
use voltfill_core::jev::{
    http::{Endpoint, HttpTransport},
    transport::Secret,
};

#[test]
fn transport_constructs_without_network_or_credentials() {
    let transport =
        HttpTransport::new(Endpoint::new("https://synthetic.example.invalid/extract").unwrap())
            .unwrap();
    assert_eq!(format!("{transport:?}"), "HttpTransport([REDACTED])");
}

#[test]
fn injected_secret_rejects_header_injection_and_redacts_debug() {
    for invalid in ["", "invalid\r\nsecret", "secret with spaces", "秘密"] {
        assert!(Secret::new(invalid.into()).is_err());
    }
    let secret = Secret::new("synthetic-token".into()).unwrap();
    assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
}
