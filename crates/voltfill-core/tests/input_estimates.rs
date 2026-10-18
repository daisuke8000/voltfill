use serde_json::json;
use voltfill_core::jev::{
    self,
    planning::{self, ContextLimits},
};

fn request(value: serde_json::Value) -> jev::Request {
    jev::parse_request(&serde_json::to_vec(&value).unwrap(), jev::Limits::default()).unwrap()
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
