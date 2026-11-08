use serde_json::json;
use voltfill_core::jev;
use voltfill_core::jev::planning::{self, ContextLimits};

fn request(value: serde_json::Value) -> jev::Request {
    jev::parse_request(
        &serde_json::to_vec(&value).expect("request fixture must serialize"),
        jev::Limits::default(),
    )
    .expect("estimate fixture must be a valid Jev request")
}

#[test]
fn context_counts_shared_state_once_and_tests_each_question_separately() {
    let request = request(
        json!({"model":jev::MODEL,"state":"共有原文".repeat(100),"questions":{
            "a":{"type":"noul","instructions":"A".repeat(300)},
            "b":{"type":"noul","instructions":"B".repeat(300)}
        }}),
    );
    let limits = ContextLimits::default();
    let estimate = planning::estimate(&request, &limits);
    assert!(estimate.request_tokens > estimate.largest_question_tokens);
    assert_eq!(estimate.request_tokens, request.bytes().len() as u64 + 256);
    assert!(estimate.fits(&limits, estimate.request_tokens));
    assert!(!estimate.fits(&limits, estimate.request_tokens - 1));
    assert!(!estimate.fits(
        &ContextLimits {
            max_question_tokens: estimate.largest_question_tokens - 1,
            ..limits
        },
        u64::MAX
    ));
}

#[test]
fn repeated_images_in_values_are_counted_but_asset_keys_are_not() {
    let request = request(
        json!({"model":jev::MODEL,"state":{"image":"<img>","again":["<img>"]},
        "assets":{"<img>":{"part":"image"}},"questions":{
            "a":{"type":"box","instructions":"Locate <img>"},
            "b":{"type":"noul","instructions":"Present?"}
        }}),
    );
    let low = planning::estimate(
        &request,
        &ContextLimits {
            image_tokens_per_image: 100,
            ..Default::default()
        },
    );
    let high = planning::estimate(
        &request,
        &ContextLimits {
            image_tokens_per_image: 200,
            ..Default::default()
        },
    );
    assert_eq!(high.request_tokens - low.request_tokens, 300);
    assert_eq!(
        high.largest_question_tokens - low.largest_question_tokens,
        300
    );
}

#[test]
fn a_measured_byte_ratio_divides_only_the_text_estimate() {
    let request = request(
        json!({"model":jev::MODEL,"state":{"image":"<img>","text":"原文".repeat(200)},
        "assets":{"<img>":{"part":"image"}},"questions":{
            "a":{"type":"noul","instructions":"Present?"}
        }}),
    );
    let bytes = request.bytes().len() as u64;
    let default = planning::estimate(&request, &ContextLimits::default());
    let lowered = planning::estimate(
        &request,
        &ContextLimits {
            text_tokens_per_100_bytes: 60,
            ..Default::default()
        },
    );
    assert_eq!(default.request_tokens, bytes + 256 + 16_000);
    assert_eq!(
        lowered.request_tokens,
        (bytes * 60).div_ceil(100) + 256 + 16_000
    );
    let text = default.largest_question_tokens - 256 - 16_000;
    assert_eq!(
        lowered.largest_question_tokens,
        (text * 60).div_ceil(100) + 256 + 16_000
    );
}

#[test]
fn the_byte_ratio_is_read_validated_and_left_out_of_the_default_identity() {
    use voltfill_core::jev::transport::ExecutionConfig;
    let parse = |value: serde_json::Value| serde_json::from_value::<ExecutionConfig>(value);
    let config = parse(json!({"context":{"text_tokens_per_100_bytes":25}}))
        .expect("minimum supported text ratio must deserialize");
    assert_eq!(config.context.text_tokens_per_100_bytes, 25);
    assert!(config.validate().is_ok());
    for value in [0, 24, 101] {
        let config = parse(json!({"context":{"text_tokens_per_100_bytes":value}}))
            .expect("numeric ratios must deserialize before range validation");
        assert!(config.validate().is_err());
    }
    assert!(parse(json!({"context":{"text_tokens_per_100_byte":60}})).is_err());
    let omitted = serde_json::to_value(ExecutionConfig::default())
        .expect("default execution configuration must serialize");
    let explicit = serde_json::to_value(
        parse(json!({"context":{"text_tokens_per_100_bytes":100}}))
            .expect("explicit default text ratio must deserialize"),
    )
    .expect("explicit default execution configuration must serialize");
    assert_eq!(omitted, explicit);
    assert!(
        omitted["context"]
            .get("text_tokens_per_100_bytes")
            .is_none()
    );
    assert_eq!(
        serde_json::to_value(&config).expect("non-default text ratio must serialize")["context"]["text_tokens_per_100_bytes"],
        25
    );
}
