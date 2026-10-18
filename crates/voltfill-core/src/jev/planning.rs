//! Local admission estimates, not a provider tokenizer or a billing measurement.
use super::{Content, Request};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextLimits {
    pub max_request_tokens: u64,
    pub max_question_tokens: u64,
    /// Planning allowance per image occurrence; image tokenization is unpublished.
    pub image_tokens_per_image: u64,
}
impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            max_request_tokens: 64_000,
            max_question_tokens: 32_000,
            image_tokens_per_image: 16_000,
        }
    }
}
impl ContextLimits {
    pub fn validate(&self) -> bool {
        (1..=1_000_000).contains(&self.max_request_tokens)
            && (1..=self.max_request_tokens).contains(&self.max_question_tokens)
            && (1..=1_000_000).contains(&self.image_tokens_per_image)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct InputEstimate {
    pub request_tokens: u64,
    pub largest_question_tokens: u64,
}
impl InputEstimate {
    pub fn fits(self, limits: &ContextLimits, attempt_cap: u64) -> bool {
        self.request_tokens <= limits.max_request_tokens.min(attempt_cap)
            && self.largest_question_tokens <= limits.max_question_tokens
    }
}

// Count occurrences in values, including repeated references, never object keys.
fn image_uses(content: &Content, references: &[&str]) -> u64 {
    match content {
        Content::Text(text) => references.iter().fold(0u64, |sum, reference| {
            sum.saturating_add(text.matches(reference).count() as u64)
        }),
        Content::Array(values) => values.iter().fold(0u64, |sum, value| {
            sum.saturating_add(image_uses(value, references))
        }),
        Content::Object(values) => values.values().fold(0u64, |sum, value| {
            sum.saturating_add(image_uses(value, references))
        }),
        _ => 0,
    }
}

pub fn estimate(request: &Request, limits: &ContextLimits) -> InputEstimate {
    // One token per serialized UTF-8 byte plus framing allowance is deliberately
    // conservative for text. Neither this nor image allowances is an exact bound.
    const FRAMING: u64 = 256;
    let draft = request.draft();
    let references: Vec<_> = draft.assets.keys().map(String::as_str).collect();
    let state_images = image_uses(&draft.state, &references);
    let shared_bytes = serde_json::to_vec(&(&draft.model, &draft.state, &draft.assets))
        .map_or(u64::MAX, |bytes| bytes.len() as u64);
    let mut all_images = state_images;
    let mut largest_question_tokens = 0;
    for (id, question) in &draft.questions {
        let (instructions, criteria) = question.content();
        let question_images = criteria.fold(image_uses(instructions, &references), |sum, value| {
            sum.saturating_add(image_uses(value, &references))
        });
        all_images = all_images.saturating_add(question_images);
        let question_bytes =
            serde_json::to_vec(&(id, question)).map_or(u64::MAX, |bytes| bytes.len() as u64);
        largest_question_tokens = largest_question_tokens.max(
            shared_bytes
                .saturating_add(question_bytes)
                .saturating_add(FRAMING)
                .saturating_add(
                    state_images
                        .saturating_add(question_images)
                        .saturating_mul(limits.image_tokens_per_image),
                ),
        );
    }
    InputEstimate {
        request_tokens: (request.bytes().len() as u64)
            .saturating_add(FRAMING)
            .saturating_add(all_images.saturating_mul(limits.image_tokens_per_image)),
        largest_question_tokens,
    }
}
