use crate::{jev, model::Strategy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
// Only rejects pathological schemas; the request count, deadline and cost budget bound the
// work an extraction actually does.
pub const MAX_FIELDS: usize = 512;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("invalid schema JSON")]
    Json,
    #[error("unsupported schema version")]
    Version,
    #[error("schema resource limit exceeded")]
    Limit,
    #[error("invalid or duplicate identifier")]
    Identifier,
    #[error("invalid question or resolver contract")]
    Contract,
    #[error("invalid question dependency graph")]
    Dependency,
    #[error("schema is incompatible with strategy")]
    Strategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormDefinition {
    pub schema_version: u32,
    pub fields: Vec<FieldDefinition>,
    /// Left out when absent, so a form without it serializes, and hashes into `form_id`,
    /// exactly as before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_context: Option<jev::Content>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinition {
    pub id: String,
    pub label: String,
    pub output_type: OutputType,
    pub questions: Vec<QuestionDefinition>,
    pub resolver: Resolver,
    #[serde(default)]
    pub pages: Vec<u32>,
    #[serde(default)]
    pub validators: Vec<Validator>,
    #[serde(default)]
    pub normalization: Normalization,
    /// Left out when false, so a form that sets it to false serializes, and hashes into
    /// `form_id`, exactly like one that omits it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unify_cjk_radicals: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputType {
    String,
    StringArray,
    Date,
    Decimal,
    Bool,
    Enum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionDefinition {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: QuestionType,
    pub instructions: jev::Content,
    #[serde(default)]
    pub criteria: Option<Value>,
    pub input: Input,
    #[serde(default)]
    pub candidate_source: Option<CandidateSource>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl QuestionDefinition {
    pub(crate) fn wire_question(&self) -> Result<jev::Question, SchemaError> {
        let instructions = self.instructions.clone();
        let criteria = self.criteria.clone().unwrap_or(Value::Null);
        Ok(match self.kind {
            QuestionType::Noul => jev::Question::Noul {
                instructions,
                criteria: serde_json::from_value(criteria).map_err(|_| SchemaError::Contract)?,
            },
            QuestionType::Choice => jev::Question::Choice {
                instructions,
                criteria: serde_json::from_value(criteria).map_err(|_| SchemaError::Contract)?,
            },
            QuestionType::Score => jev::Question::Score {
                instructions,
                criteria: serde_json::from_value(criteria).map_err(|_| SchemaError::Contract)?,
            },
            QuestionType::Box if self.criteria.is_none() => jev::Question::Box { instructions },
            QuestionType::Box => return Err(SchemaError::Contract),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionType {
    Noul,
    Choice,
    Box,
    Score,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub modality: Modality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Image,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSource {
    pub kind: CandidateKind,
    pub none_option: String,
    #[serde(default = "default_candidates")]
    pub max_candidates: usize,
}
fn default_candidates() -> usize {
    254
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    TextSpans,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resolver {
    SelectedCandidate {
        question_id: String,
    },
    SelectedCandidates {
        question_id: String,
        max_selections: usize,
    },
    TextInBox {
        question_id: String,
        presence_question_id: String,
    },
    SelectedChoice {
        question_id: String,
        mapping: BTreeMap<String, Value>,
    },
    NoulToBoolean {
        question_id: String,
    },
    ScoreToLevel {
        question_id: String,
        levels: Vec<Value>,
    },
}
impl Resolver {
    pub fn question_id(&self) -> &str {
        match self {
            Self::SelectedCandidate { question_id }
            | Self::SelectedCandidates { question_id, .. }
            | Self::TextInBox { question_id, .. }
            | Self::SelectedChoice { question_id, .. }
            | Self::NoulToBoolean { question_id }
            | Self::ScoreToLevel { question_id, .. } => question_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Validator {
    NonEmpty,
    MaxChars { count: usize },
    OneOf { values: Vec<Value> },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    #[default]
    None,
    Trim,
    CollapseWhitespace,
}

pub fn parse(bytes: &[u8]) -> Result<FormDefinition, SchemaError> {
    if bytes.len() > MAX_SCHEMA_BYTES {
        return Err(SchemaError::Limit);
    }
    let value = jev::json::parse(bytes, jev::Limits::default()).map_err(|_| SchemaError::Json)?;
    let form: FormDefinition = serde_json::from_value(value).map_err(|_| SchemaError::Json)?;
    form.validate()?;
    Ok(form)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

impl FormDefinition {
    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.schema_version != 1 {
            return Err(SchemaError::Version);
        }
        if self.fields.is_empty() || self.fields.len() > MAX_FIELDS {
            return Err(SchemaError::Limit);
        }
        if let Some(context) = &self.shared_context
            && (!jev::entry(context, false) || !jev::fits_in_state("shared_context", context))
        {
            return Err(SchemaError::Contract);
        }
        let mut ids = BTreeSet::new();
        for field in &self.fields {
            if !identifier(&field.id) || !ids.insert(&field.id) {
                return Err(SchemaError::Identifier);
            }
            if field.label.trim().is_empty()
                || field.questions.is_empty()
                || field.questions.len() > 32
                || field.pages.contains(&0)
            {
                return Err(SchemaError::Contract);
            }
            let mut questions = BTreeMap::new();
            for q in &field.questions {
                if !identifier(&q.id) || questions.insert(&q.id, q).is_some() {
                    return Err(SchemaError::Identifier);
                }
                if !jev::entry(&q.instructions, false) {
                    return Err(SchemaError::Contract);
                }
                if let Some(source) = &q.candidate_source {
                    if q.kind != QuestionType::Choice
                        || q.input.modality == Modality::Image
                        || q.criteria.is_some()
                        || source.none_option.trim().is_empty()
                        || source.max_candidates == 0
                        || source.max_candidates > 254
                    {
                        return Err(SchemaError::Contract);
                    }
                } else {
                    // Reuse the wire validator without changing user content.
                    let question = q.wire_question()?;
                    let draft = jev::RequestDraft {
                        model: jev::MODEL.into(),
                        state: jev::Content::Text("schema validation".into()),
                        questions: BTreeMap::from([("q".into(), question)]),
                        assets: BTreeMap::new(),
                    };
                    // Box asset binding is validated when an image is prepared.
                    if q.kind != QuestionType::Box {
                        jev::build_request(draft, jev::Limits::default())
                            .map_err(|_| SchemaError::Contract)?;
                    } else if q.criteria.is_some() || q.input.modality == Modality::Text {
                        return Err(SchemaError::Contract);
                    }
                }
            }
            let mut done = BTreeSet::new();
            while done.len() < questions.len() {
                let previous = done.len();
                for (id, q) in &questions {
                    if q.depends_on.iter().all(|d| done.contains(d.as_str())) {
                        done.insert(id.as_str());
                    }
                }
                if done.len() == previous {
                    return Err(SchemaError::Dependency);
                }
            }
            let q = questions
                .get(&field.resolver.question_id().to_string())
                .ok_or(SchemaError::Contract)?;
            let correct = match &field.resolver {
                Resolver::SelectedCandidate { .. } => {
                    q.kind == QuestionType::Choice
                        && q.candidate_source.is_some()
                        && matches!(
                            field.output_type,
                            OutputType::String
                                | OutputType::StringArray
                                | OutputType::Date
                                | OutputType::Decimal
                        )
                }
                Resolver::SelectedCandidates { max_selections, .. } => {
                    (1..=254).contains(max_selections)
                        && field.output_type == OutputType::StringArray
                        && field.questions.len() == 1
                        && q.kind == QuestionType::Choice
                        && q.input.modality == Modality::Text
                        && q.candidate_source.is_some()
                        && q.depends_on.is_empty()
                }
                Resolver::TextInBox {
                    presence_question_id,
                    ..
                } => {
                    q.kind == QuestionType::Box
                        && matches!(
                            field.output_type,
                            OutputType::String
                                | OutputType::StringArray
                                | OutputType::Date
                                | OutputType::Decimal
                        )
                        && questions
                            .get(presence_question_id)
                            .is_some_and(|p| p.kind == QuestionType::Noul)
                }
                Resolver::SelectedChoice { mapping, .. } => {
                    q.kind == QuestionType::Choice
                        && field.output_type != OutputType::StringArray
                        && q.candidate_source.is_none()
                        && !mapping.is_empty()
                        && mapping.values().all(|v| {
                            crate::validate::validate(v, field.output_type, &field.validators)
                                .is_ok()
                        })
                        && q.criteria
                            .as_ref()
                            .and_then(Value::as_object)
                            .is_some_and(|c| {
                                c.len() == mapping.len()
                                    && c.keys().all(|k| mapping.contains_key(k))
                            })
                }
                Resolver::NoulToBoolean { .. } => {
                    q.kind == QuestionType::Noul && field.output_type == OutputType::Bool
                }
                Resolver::ScoreToLevel { levels, .. } => {
                    q.kind == QuestionType::Score
                        && field.output_type != OutputType::StringArray
                        && q.criteria
                            .as_ref()
                            .and_then(Value::as_array)
                            .is_some_and(|c| c.len() == levels.len())
                        && levels.iter().all(|v| {
                            crate::validate::validate(v, field.output_type, &field.validators)
                                .is_ok()
                        })
                }
            };
            if !correct {
                return Err(SchemaError::Contract);
            }
        }
        Ok(())
    }

    pub fn validate_strategy(&self, strategy: Strategy) -> Result<(), SchemaError> {
        if strategy != Strategy::TextFirst
            && self
                .fields
                .iter()
                .any(|field| matches!(field.resolver, Resolver::SelectedCandidates { .. }))
        {
            return Err(SchemaError::Strategy);
        }
        for q in self.fields.iter().flat_map(|f| &f.questions) {
            let valid = match strategy {
                Strategy::TextFirst => {
                    q.input.modality == Modality::Text && q.kind != QuestionType::Box
                }
                Strategy::VisionFirst => q.input.modality == Modality::Image,
                Strategy::Hybrid => true,
            };
            if !valid {
                return Err(SchemaError::Strategy);
            }
        }
        Ok(())
    }
}
