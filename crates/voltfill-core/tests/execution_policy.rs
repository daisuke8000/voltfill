use std::{cell::Cell, rc::Rc};
use voltfill_core::jev::{
    self,
    transport::{
        AttemptBudget, Clock, ExecutionConfig, Failure, HttpRequest, HttpResponse, NoopObserver,
        Policy, Secret, Transport, TransportError, execute_observed,
    },
};

#[derive(Clone, Default)]
struct TestClock(Rc<Cell<u64>>);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep_ms(&mut self, duration: u64) {
        self.0.set(self.now_ms() + duration);
    }
}

struct Responses {
    clock: TestClock,
    starts: Vec<u64>,
    retry_after: Option<u64>,
}
impl Transport for Responses {
    fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        self.starts.push(self.clock.now_ms());
        self.clock.sleep_ms(100);
        let success = self.starts.len() == 3;
        Ok(HttpResponse {
            body_error: None,
            status: if success { 200 } else { 503 },
            body: if success {
                br#"{"model":"jev-1.14.0","answers":{"q":{"type":"noul","noul":1.0}},"usage":{"input_tokens":10,"output_tokens":2}}"#.to_vec()
            } else {
                b"{}".to_vec()
            },
            retry_after_ms: if self.starts.len() == 1 {
                self.retry_after
            } else {
                None
            },
        })
    }
}

fn run(policy: Policy, retry_after: Option<u64>) -> (Vec<u64>, Result<jev::Response, Failure>) {
    let request = jev::parse_request(
        br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,
        jev::Limits::default(),
    )
    .unwrap();
    let mut clock = TestClock::default();
    let mut wire = Responses {
        clock: clock.clone(),
        starts: Vec::new(),
        retry_after,
    };
    let secret = Secret::new("synthetic-test-only".into()).unwrap();
    let execution = execute_observed(
        &request,
        &[],
        "unused-json-boundary",
        Some(&secret),
        policy,
        jev::Limits::default(),
        &mut wire,
        &mut clock,
        &mut AttemptBudget::default(),
        &mut NoopObserver,
    );
    (wire.starts, execution.result)
}

fn policy() -> Policy {
    Policy {
        allow_remote: true,
        initial_backoff_ms: 4_000,
        backoff_multiplier: 2,
        max_backoff_ms: 6_000,
        jitter_ms: 0,
        ..Default::default()
    }
}

#[test]
fn exponential_backoff_changes_real_send_times_and_respects_cap() {
    let (starts, result) = run(policy(), None);
    assert!(result.is_ok());
    // Each response takes 100 ms. Exponential delays are 4s then capped at 6s.
    assert_eq!(starts, [0, 4_100, 10_200]);
    let (starts, result) = run(
        Policy {
            backoff_multiplier: 3,
            max_backoff_ms: 15_000,
            ..policy()
        },
        None,
    );
    assert!(result.is_ok());
    assert_eq!(starts, [0, 4_100, 16_200]);
}

#[test]
fn provider_retry_after_is_not_truncated_by_backoff_cap_or_deadline() {
    let (starts, result) = run(policy(), Some(9_000));
    assert!(result.is_ok());
    assert_eq!(starts, [0, 9_100, 15_200]);
    let (starts, result) = run(
        Policy {
            total_deadline_ms: 8_000,
            ..policy()
        },
        Some(9_000),
    );
    assert_eq!(result.unwrap_err(), Failure::Deadline);
    assert_eq!(starts, [0]);
}

#[test]
fn jitter_stays_within_configured_bound_without_shortening_backoff() {
    let (starts, result) = run(
        Policy {
            jitter_ms: 300,
            ..policy()
        },
        None,
    );
    assert!(result.is_ok());
    assert_eq!(starts.len(), 3);
    assert!((4_000..=4_300).contains(&(starts[1] - starts[0] - 100)));
    assert_eq!(starts[2] - starts[1] - 100, 6_000);
}

#[test]
fn invalid_allocations_are_rejected_before_any_attempt_is_reserved() {
    let base = ExecutionConfig::default();
    let invalid = [
        ExecutionConfig {
            requests_per_second: base.organization.requests_per_second + 1,
            ..base.clone()
        },
        ExecutionConfig {
            input_tokens_per_second: base.organization.input_tokens_per_second + 1,
            ..base.clone()
        },
        ExecutionConfig {
            input_tokens_per_second: 1,
            ..base.clone()
        },
        ExecutionConfig {
            max_backoff_ms: base.initial_backoff_ms - 1,
            ..base.clone()
        },
        ExecutionConfig {
            attempt_timeout_ms: base.total_deadline_ms + 1,
            ..base.clone()
        },
    ];
    for config in invalid {
        let budget = AttemptBudget::default();
        assert_eq!(budget.configure(&config), Err(Failure::Config));
        assert_eq!(budget.snapshot().posts, 0);
        assert_eq!(budget.snapshot().reserved, 0);
    }
    // Typos must not silently fall back to an unrelated default.
    assert!(serde_json::from_str::<ExecutionConfig>(r#"{"max_concurency":4}"#).is_err());
}

#[test]
fn overflowing_provider_usage_retains_reservation_and_stops_shared_admission() {
    let mut budget = AttemptBudget::default();
    let mut clock = TestClock::default();
    struct Overflow;
    impl Transport for Overflow {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse { body_error: None, status:200, body:format!(r#"{{"model":"jev-1.14.0","answers":{{"q":{{"type":"noul","noul":1.0}}}},"usage":{{"input_tokens":{},"output_tokens":0}}}}"#,u64::MAX).into_bytes(), retry_after_ms:None })
        }
    }
    let request=jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#,jev::Limits::default()).unwrap();
    let secret = Secret::new("synthetic-test-only".into()).unwrap();
    for expected in [Failure::Budget, Failure::Cancelled] {
        let result = execute_observed(
            &request,
            &[],
            "boundary",
            Some(&secret),
            policy(),
            jev::Limits::default(),
            &mut Overflow,
            &mut clock,
            &mut budget,
            &mut NoopObserver,
        );
        assert_eq!(result.result.unwrap_err(), expected);
        let after = budget.snapshot();
        assert!(after.stopped);
        assert!(after.reserved > 0);
        assert_eq!(after.known_cost, 0);
        assert_eq!(after.posts, 1);
    }
}

#[test]
fn configured_price_and_completion_gap_apply_to_every_attempt() {
    let request = jev::parse_request(br#"{"model":"jev-1.14.0","state":"synthetic","questions":{"q":{"type":"noul","instructions":"Present?"}}}"#, jev::Limits::default()).unwrap();
    let secret = Secret::new("synthetic-test-only".into()).unwrap();
    let config = ExecutionConfig {
        input_token_price_nano_usd: 73,
        completion_gap_ms: 5_000,
        ..Default::default()
    };
    let mut budget = AttemptBudget::default();
    budget.configure(&config).unwrap();
    let mut clock = TestClock::default();
    let mut wire = Responses {
        clock: clock.clone(),
        starts: vec![],
        retry_after: None,
    };
    let result = execute_observed(
        &request,
        &[],
        "boundary",
        Some(&secret),
        Policy {
            allow_remote: true,
            ..Default::default()
        },
        config.json_limits(),
        &mut wire,
        &mut clock,
        &mut budget,
        &mut NoopObserver,
    );
    assert!(result.result.is_ok());
    assert_eq!(wire.starts, [0, 5_100, 10_200]);
    assert_eq!(budget.snapshot().known_cost, 10 * 73);
    assert_eq!(budget.snapshot().reserved, 2 * 64_000 * 73);
    // Reservation uses the configured price before a transport may run.
    let mut budget = AttemptBudget::default();
    budget
        .configure(&ExecutionConfig {
            max_cost_nano_usd: 64_000 * 73 - 1,
            ..config
        })
        .unwrap();
    wire.starts.clear();
    let result = execute_observed(
        &request,
        &[],
        "boundary",
        Some(&secret),
        Policy {
            allow_remote: true,
            ..Default::default()
        },
        jev::Limits::default(),
        &mut wire,
        &mut clock,
        &mut budget,
        &mut NoopObserver,
    );
    assert_eq!(result.result.unwrap_err(), Failure::Budget);
    assert!(wire.starts.is_empty());
}
