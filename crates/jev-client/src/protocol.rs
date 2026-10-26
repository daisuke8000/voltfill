//! Jev request and response contracts and their bounded validation.
use crate::{json, multipart};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Default pinned model identifier used when callers do not select another model.
pub const MODEL: &str = "jev-1.14.0";

fn model_version(model: &str) -> Option<[u32; 3]> {
    let mut parts = model.strip_prefix("jev-")?.split('.');
    let mut version = [0; 3];
    for component in &mut version {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 5 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *component = part.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// Accept a versioned Jev identifier or a supported moving alias; this does not contact Jev.
pub fn validate_model(model: &str) -> Result<(), Error> {
    if matches!(model, "jev-latest" | "jev-preview") || model_version(model).is_some() {
        Ok(())
    } else {
        Err(Error::Model)
    }
}

/// Image strategies require an explicit version; moving aliases do not guarantee capabilities.
#[must_use]
pub fn supports_images(model: &str) -> bool {
    model_version(model).is_some_and(|version| version >= [1, 14, 0])
}

/// Local protocol failure category; contains no provider body, endpoint, or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Config,
    Bytes,
    Depth,
    Json,
    Request,
    Nonfinite,
    Envelope,
    Model,
    IdSet,
    Usage,
    Answer,
    Type,
    Range,
    Keys,
    Distribution,
    Legend,
    Multipart,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jev protocol: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Local validation settings, not upstream numerical or resource guarantees.
#[derive(Clone, Copy)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub probability_tolerance: f64,
    pub score_tolerance: f64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 1_048_576,
            max_depth: 32,
            probability_tolerance: 1e-6,
            score_tolerance: 1e-6,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.max_bytes == 0
            || !(1..=64).contains(&self.max_depth)
            || !self.probability_tolerance.is_finite()
            || self.probability_tolerance < 0.0
            || self.probability_tolerance > 1e-3
            || !self.score_tolerance.is_finite()
            || self.score_tolerance < 0.0
            || self.score_tolerance > 1e-3
        {
            return Err(Error::Config);
        }
        Ok(())
    }
}

/// Keeps Rust floats intact until the builder has checked them, before serde_json
/// can silently serialize NaN/Infinity to null. Integers retain their full precision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Text(String),
    Array(Vec<Content>),
    Object(BTreeMap<String, Content>),
}
#[derive(Clone, Copy)]
enum Nullability {
    Allowed,
    Forbidden,
}

impl Content {
    fn check(&self, depth: usize, limit: usize) -> Result<(), Error> {
        if depth > limit {
            return Err(Error::Depth);
        }
        match self {
            Self::Float(n) if !n.is_finite() => return Err(Error::Nonfinite),
            Self::Array(xs) => {
                for x in xs {
                    x.check(depth + 1, limit)?;
                }
            }
            Self::Object(xs) => {
                for x in xs.values() {
                    x.check(depth + 1, limit)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn entry(&self, nullability: Nullability) -> bool {
        match self {
            Self::Null => matches!(nullability, Nullability::Allowed),
            Self::Text(s) => !s.trim().is_empty(),
            Self::Array(xs) => !xs.is_empty(),
            Self::Object(xs) => !xs.is_empty(),
            _ => false,
        }
    }
}

/// Typed wire questions. Use [`build_request`] to validate instructions and criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Question {
    Noul {
        instructions: Content,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<BTreeMap<String, Content>>,
    },
    Choice {
        instructions: Content,
        criteria: BTreeMap<String, Content>,
    },
    Score {
        instructions: Content,
        criteria: Vec<Content>,
    },
    Box {
        instructions: Content,
    },
}
impl Question {
    pub(crate) fn content(&self) -> (&Content, impl Iterator<Item = &Content>) {
        let (instructions, map, slice): (_, _, &[Content]) = match self {
            Self::Noul {
                instructions,
                criteria,
            } => (instructions, criteria.as_ref(), &[]),
            Self::Choice {
                instructions,
                criteria,
            } => (instructions, Some(criteria), &[]),
            Self::Score {
                instructions,
                criteria,
            } => (instructions, None, criteria.as_slice()),
            Self::Box { instructions } => (instructions, None, &[]),
        };
        (
            instructions,
            map.into_iter()
                .flat_map(BTreeMap::values)
                .chain(slice.iter()),
        )
    }
    fn validate(&self, limits: Limits) -> Result<(), Error> {
        let (instructions, descriptions) = self.content();
        instructions.check(1, limits.max_depth)?;
        if !instructions.entry(Nullability::Forbidden) {
            return Err(Error::Request);
        }
        let nullability = match self {
            Self::Choice { .. } => Nullability::Allowed,
            _ => Nullability::Forbidden,
        };
        for description in descriptions {
            description.check(1, limits.max_depth)?;
            if !description.entry(nullability) {
                return Err(Error::Request);
            }
        }
        match self {
            Self::Noul {
                criteria: Some(c), ..
            } if c.is_empty() || c.keys().any(|k| k != "true" && k != "false") => {
                Err(Error::Request)
            }
            Self::Choice { criteria, .. }
                if !(1..=255).contains(&criteria.len())
                    || criteria.keys().any(|k| k.trim().is_empty()) =>
            {
                Err(Error::Request)
            }
            Self::Score { criteria, .. } if !(2..=10).contains(&criteria.len()) => {
                Err(Error::Request)
            }
            _ => Ok(()),
        }
    }
}

/// Associates a symbolic content reference with a multipart image part.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub part: String,
}

/// Unvalidated request content; construction alone does not enforce wire limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestDraft {
    pub model: String,
    pub state: Content,
    pub questions: BTreeMap<String, Question>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, Asset>,
}

/// Private fields prevent bypassing builder checks on the response-validation path.
pub struct Request {
    draft: RequestDraft,
    bytes: Vec<u8>,
}
impl Request {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    #[must_use]
    pub fn draft(&self) -> &RequestDraft {
        &self.draft
    }
}

/// Decode bounded, duplicate-free JSON and validate the complete request.
/// Explicit null criteria are rejected; omitted Noul criteria remain supported.
pub fn parse_request(bytes: &[u8], limits: Limits) -> Result<Request, Error> {
    let value = json::parse(bytes, limits)?;
    // Option would conflate explicit null and omitted Noul criteria. Only omission
    // is generated in this adapter’s strict common subset.
    if let Some(questions) = value.get("questions").and_then(Value::as_object)
        && questions
            .values()
            .any(|q| q.get("criteria") == Some(&Value::Null))
    {
        return Err(Error::Request);
    }
    let draft: RequestDraft = serde_json::from_value(value).map_err(|_| Error::Request)?;
    build_request(draft, limits)
}

/// Validate model capabilities, content, asset bindings, and complete serialized size.
/// Returns a reusable request without performing network I/O.
pub fn build_request(draft: RequestDraft, limits: Limits) -> Result<Request, Error> {
    limits.validate()?;
    validate_model(&draft.model)?;
    if (!draft.assets.is_empty()
        || draft
            .questions
            .values()
            .any(|q| matches!(q, Question::Box { .. })))
        && !supports_images(&draft.model)
    {
        return Err(Error::Model);
    }
    draft.state.check(1, limits.max_depth)?;
    if !draft.state.entry(Nullability::Forbidden)
        || draft.questions.is_empty()
        || draft.questions.keys().any(|k| k.trim().is_empty())
    {
        return Err(Error::Request);
    }
    for question in draft.questions.values() {
        question.validate(limits)?;
    }
    multipart::validate_assets(&draft)?;
    let bytes = serde_json::to_vec(&draft).map_err(|_| Error::Request)?;
    // Bound the complete generated document, not just each content subtree.
    json::preflight(&bytes, limits)?;
    Ok(Request { draft, bytes })
}

/// A nullable wire value. Missing-versus-null provenance is recorded in [`Diagnostic`].
#[derive(Debug, Deserialize, PartialEq, Default)]
#[serde(transparent)]
pub struct Nullable<T>(pub Option<T>);

/// Provider-reported token counts; absent counts remain unknown rather than becoming zero.
#[derive(Debug, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Nullable<u64>,
    #[serde(default)]
    pub output_tokens: Nullable<u64>,
}
/// A Box answer in normalized image coordinates; validation requires positive area.
#[derive(Debug, Deserialize)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
/// A decoded answer, checked against its corresponding question by [`parse_response`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        legend: BTreeMap<String, Value>,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Box {
        r#box: Rect,
        #[serde(default)]
        confidence: Nullable<f64>,
    },
}
/// A valid response envelope with per-question validation results and usage diagnostics.
#[derive(Debug)]
pub struct Response {
    pub model: String,
    pub answers: BTreeMap<String, Result<Answer, Error>>,
    pub usage: Usage,
    pub diagnostics: Vec<Diagnostic>,
}

/// Missing and explicit null remain distinguishable without inventing usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    MissingUsage,
    NullUsage,
    MissingInputTokens,
    NullInputTokens,
    MissingOutputTokens,
    NullOutputTokens,
    MissingConfidence(String),
    NullConfidence(String),
    ProbabilitySumNotOne(String),
}

/// The adapter preserves instructions and criteria verbatim.
pub type QuestionSpec = Question;

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("model", &self.draft.model)
            .field("bytes", &self.bytes.len())
            .field("questions", &self.draft.questions.len())
            .finish()
    }
}

fn unit(n: f64) -> Result<(), Error> {
    if n.is_finite() && (0.0..=1.0).contains(&n) {
        Ok(())
    } else {
        Err(Error::Range)
    }
}
fn distribution<'a>(
    p: &BTreeMap<String, f64>,
    mut keys: impl ExactSizeIterator<Item = &'a str>,
    _limits: Limits,
) -> Result<(), Error> {
    if p.len() != keys.len() || keys.any(|k| !p.contains_key(k)) {
        return Err(Error::Keys);
    }
    for n in p.values() {
        unit(*n)?;
    }
    Ok(())
}

fn validate_answer(value: Value, question: &Question, limits: Limits) -> Result<Answer, Error> {
    let answer: Answer = serde_json::from_value(value).map_err(|_| Error::Answer)?;
    match (&answer, question) {
        (Answer::Noul { noul }, Question::Noul { .. }) => unit(*noul)?,
        (
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            },
            Question::Choice { criteria, .. },
        ) => {
            if !criteria.contains_key(choice) {
                return Err(Error::Keys);
            }
            distribution(probabilities, criteria.keys().map(String::as_str), limits)?;
            unit(*confidence)?;
        }
        (
            Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            },
            Question::Score { criteria, .. },
        ) => {
            let keys: Vec<String> = (0..criteria.len()).map(|n| n.to_string()).collect();
            distribution(probabilities, keys.iter().map(String::as_str), limits)?;
            if legend.len() != criteria.len() {
                return Err(Error::Legend);
            }
            for (i, criterion) in criteria.iter().enumerate() {
                if legend.get(&i.to_string())
                    != Some(&serde_json::to_value(criterion).map_err(|_| Error::Legend)?)
                {
                    return Err(Error::Legend);
                }
            }
            if !score.is_finite() || *score < 0.0 || *score > (criteria.len() - 1) as f64 {
                return Err(Error::Range);
            }
            let expected: f64 = keys
                .iter()
                .enumerate()
                .map(|(i, k)| i as f64 * probabilities[k])
                .sum();
            if (*score - expected).abs() > limits.score_tolerance {
                return Err(Error::Distribution);
            }
            unit(*confidence)?;
        }
        (
            Answer::Box {
                r#box: rect,
                confidence,
            },
            Question::Box { .. },
        ) => {
            for n in [rect.left, rect.top, rect.right, rect.bottom] {
                unit(n)?;
            }
            if rect.left >= rect.right || rect.top >= rect.bottom {
                return Err(Error::Range);
            }
            if let Some(n) = confidence.0 {
                unit(n)?;
            }
        }
        _ => return Err(Error::Type),
    }
    Ok(answer)
}

/// Validate the envelope against the request and retain individual answer failures.
/// Record original bytes before calling this parser, including when parsing fails.
pub fn parse_response(bytes: &[u8], request: &Request, limits: Limits) -> Result<Response, Error> {
    let value = json::parse(bytes, limits)?;
    let Value::Object(mut obj) = value else {
        return Err(Error::Envelope);
    };
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or(Error::Envelope)?;
    if model_version(model).is_none()
        || (model_version(&request.draft.model).is_some() && model != request.draft.model)
        || (!request.draft.assets.is_empty() && !supports_images(model))
    {
        return Err(Error::Model);
    }
    let model = model.to_owned();
    let Some(Value::Object(answers)) = obj.remove("answers") else {
        return Err(Error::Envelope);
    };
    if answers.len() != request.draft.questions.len()
        || request
            .draft
            .questions
            .keys()
            .any(|k| !answers.contains_key(k))
    {
        return Err(Error::IdSet);
    }
    let mut diagnostics = Vec::new();
    let usage = match obj.remove("usage") {
        None => {
            diagnostics.push(Diagnostic::MissingUsage);
            Usage {
                input_tokens: Nullable(None),
                output_tokens: Nullable(None),
            }
        }
        Some(Value::Null) => {
            diagnostics.push(Diagnostic::NullUsage);
            Usage {
                input_tokens: Nullable(None),
                output_tokens: Nullable(None),
            }
        }
        Some(value) if value.is_object() => {
            for (key, missing, null) in [
                (
                    "input_tokens",
                    Diagnostic::MissingInputTokens,
                    Diagnostic::NullInputTokens,
                ),
                (
                    "output_tokens",
                    Diagnostic::MissingOutputTokens,
                    Diagnostic::NullOutputTokens,
                ),
            ] {
                match value.get(key) {
                    None => diagnostics.push(missing),
                    Some(Value::Null) => diagnostics.push(null),
                    _ => {}
                }
            }
            serde_json::from_value(value).map_err(|_| Error::Usage)?
        }
        _ => return Err(Error::Usage),
    };
    for (id, value) in &answers {
        if matches!(request.draft.questions[id], Question::Box { .. }) {
            match value.get("confidence") {
                None => diagnostics.push(Diagnostic::MissingConfidence(id.clone())),
                Some(Value::Null) => diagnostics.push(Diagnostic::NullConfidence(id.clone())),
                _ => {}
            }
        }
    }
    for (id, answer) in &answers {
        if let Some(probabilities) = answer.get("probabilities").and_then(Value::as_object)
            && let Some(values) = probabilities
                .values()
                .map(Value::as_f64)
                .collect::<Option<Vec<f64>>>()
            && (values.iter().sum::<f64>() - 1.0).abs() > limits.probability_tolerance
        {
            diagnostics.push(Diagnostic::ProbabilitySumNotOne(id.clone()));
        }
    }
    let answers: BTreeMap<String, Result<Answer, Error>> = answers
        .into_iter()
        .map(|(id, value)| {
            let answer = validate_answer(value, &request.draft.questions[&id], limits);
            (id, answer)
        })
        .collect();
    Ok(Response {
        model,
        answers,
        usage,
        diagnostics,
    })
}

/// Original received body represented losslessly as UTF-8 text or arbitrary bytes.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Text(String),
    Bytes(Vec<u8>),
}

impl ResponseBody {
    /// Preserve the received bytes, using text only when the entire body is valid UTF-8.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self::Text(text),
            Err(error) => Self::Bytes(error.into_bytes()),
        }
    }
}

impl From<Vec<u8>> for ResponseBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from_bytes(bytes)
    }
}
