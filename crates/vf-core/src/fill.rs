//! Form fill (L3/L4): match layout pairs to form fields by label and type.
//!
//! Deterministic and offline. Offline fill follows a strict commit policy
//! ("a wrong value is worse than an empty one"): a field is filled only from
//! candidates that meet every condition below. Every other candidate is
//! kept, with a [`Hold`] reason, for later model assistance, and the field
//! stays empty (`ambiguous`, `invalid` or `not_found`). Values are exact
//! source text; `value` is a deterministic normalization of it. Results are
//! evaluation-only and carry no calibrated probability.
//!
//! A candidate may be committed only when all of these hold:
//! - (a) Label: the document label equals the form label or one of its
//!   hints after normalization (NFKC, no whitespace, no trailing delimiter,
//!   lowercase, bracketed text as qualifiers); one leading honorific `ご`,
//!   `御` or `お` is ignored. A label that only contains the form label or is
//!   contained in it ([`Hold::PartialLabel`]) or has a bracketed qualifier on
//!   one side only ([`Hold::OneSidedQualifier`]) is a candidate only;
//!   different qualifiers on both sides never match.
//! - (b) Binding: the value is tied to the label by a delimiter
//!   ([`Binding::Delimiter`]): `label：value` in one fragment, or a `label：`
//!   fragment followed by the value fragment to its right on the same line.
//!   Right neighbours without a delimiter, values below labels, table and
//!   list columns are [`Hold::GeometricOnly`].
//! - (c) One value: the value is a single line ([`Hold::MultiLine`]
//!   otherwise; a multi-line value ends before a line that reads as one of
//!   the form's labels). A text value holds no second value
//!   ([`Hold::SeveralValues`], see [`several_values`]).
//! - (d) Type: date, amount and number values match the type's grammar as a
//!   whole (otherwise the field is `invalid`). Amounts and numbers are not
//!   normalized when the label states a scale unit (`売上高（千円）`) or
//!   any text on the page is a unit note (`（単位：千円）`).
//!
//! Only the best-scoring candidates decide (an exact label over a partial
//! one). The field is filled only when every one of them may be committed
//! and they agree; a held candidate among them leaves the field ambiguous,
//! and a lower-scoring candidate never fills the field. A [`Hold`] states
//! the first failed condition, in the order above.
//!
//! Work is bounded by [`FillLimits`]: layout hypotheses per document, work
//! units over layout and matching (see [`DEFAULT_MAX_WORK`]), and an
//! optional deadline checked at least once per field and every
//! [`vf_source::budget::CHECK_INTERVAL`] units. Exceeding a limit is a typed
//! [`FillError`] from [`fill_with_limits`], never a partial result.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;
use vf_source::SourceFragment;
use vf_source::budget::{Budget, BudgetError};
use vf_source::geometry::Rect;
use vf_source::layout::{self, Binding, LayoutError, Pair, Placement, Precision};

pub const MAX_FORM_BYTES: usize = 1024 * 1024;
pub const MAX_FIELDS: usize = 512;
const MAX_TEXT_CHARS: usize = 200;
const MAX_HINTS: usize = 16;
/// Candidates reported for a field that was not filled.
const MAX_CANDIDATES: usize = 5;
/// Minimum label similarity for a partial (containment) match.
const MIN_PARTIAL: f64 = 0.5;
/// Most digits in a normalized amount or number, so that every accepted
/// value is exact as a JSON number.
const MAX_DIGITS: usize = 15;
/// Default work units for one document: layout analysis (see
/// [`layout::analyze_within`]) plus matching, which spends one unit per byte
/// of text scanned for unit notes, per byte of form label compared with each
/// distinct document label, per byte of value text examined or normalized,
/// and per byte copied into the result.
pub const DEFAULT_MAX_WORK: u64 = 200_000_000;

/// Resource limits for [`fill_with_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillLimits {
    /// Label/value hypotheses allowed for one document.
    pub max_pairs: usize,
    /// Work units allowed for layout and matching (see [`DEFAULT_MAX_WORK`]).
    pub max_work: u64,
    /// Optional wall-clock deadline for layout and matching.
    pub deadline: Option<Instant>,
}

impl Default for FillLimits {
    fn default() -> Self {
        Self {
            max_pairs: layout::DEFAULT_MAX_PAIRS,
            max_work: DEFAULT_MAX_WORK,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum FillError {
    #[error("layout produced more label/value hypotheses than allowed")]
    LayoutLimitExceeded,
    #[error("form fill exceeded its work budget")]
    BudgetExceeded,
    #[error("form fill exceeded its deadline")]
    DeadlineExceeded,
}

impl FillError {
    /// Stable snake_case code for error responses.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::LayoutLimitExceeded => "layout_limit_exceeded",
            Self::BudgetExceeded => "fill_budget_exceeded",
            Self::DeadlineExceeded => "fill_deadline_exceeded",
        }
    }
}

impl From<BudgetError> for FillError {
    fn from(error: BudgetError) -> Self {
        match error {
            BudgetError::WorkExceeded => Self::BudgetExceeded,
            BudgetError::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }
}

impl From<LayoutError> for FillError {
    fn from(error: LayoutError) -> Self {
        match error {
            LayoutError::LimitExceeded => Self::LayoutLimitExceeded,
            LayoutError::BudgetExceeded => Self::BudgetExceeded,
            LayoutError::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    #[default]
    Text,
    Date,
    Amount,
    Number,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormField {
    pub id: String,
    pub label: String,
    #[serde(rename = "type", default)]
    pub field_type: FieldType,
    #[serde(default)]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Form {
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum FormError {
    #[error("form is too large")]
    TooLarge,
    #[error("form is not valid JSON for the form contract")]
    Invalid,
    #[error("form field count is out of range")]
    FieldCount,
    #[error("form field id, label or hint is invalid")]
    FieldText,
    #[error("form field ids must be unique")]
    DuplicateId,
}

/// Parse and validate a form definition.
pub fn parse_form(bytes: &[u8]) -> Result<Form, FormError> {
    if bytes.len() > MAX_FORM_BYTES {
        return Err(FormError::TooLarge);
    }
    let form: Form = serde_json::from_slice(bytes).map_err(|_| FormError::Invalid)?;
    if form.fields.is_empty() || form.fields.len() > MAX_FIELDS {
        return Err(FormError::FieldCount);
    }
    let mut ids = BTreeSet::new();
    for field in &form.fields {
        let text_ok = |s: &str| {
            !s.trim().is_empty()
                && s.chars().count() <= MAX_TEXT_CHARS
                && !s.chars().any(char::is_control)
        };
        if !text_ok(&field.id)
            || !text_ok(&field.label)
            || field.hints.len() > MAX_HINTS
            || !field.hints.iter().all(|h| text_ok(h))
        {
            return Err(FormError::FieldText);
        }
        if !ids.insert(field.id.as_str()) {
            return Err(FormError::DuplicateId);
        }
    }
    Ok(form)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillStatus {
    Filled,
    /// Candidates disagree, or one of the best is held ([`Hold`]).
    Ambiguous,
    /// No value candidate for the field's labels.
    NotFound,
    NoText,
    /// The best candidates do not fit the field type.
    Invalid,
    /// Not evaluated because the document exceeded the default
    /// [`FillLimits`]. Only [`fill`] reports this; [`fill_with_limits`]
    /// returns a [`FillError`] instead.
    LimitExceeded,
}

/// Why a candidate may not be committed offline: the first failed
/// condition of the commit policy (see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// The document label contains the form label or is contained in it
    /// (`再発行日` for `発行日`).
    PartialLabel,
    /// The document label or the form label has a bracketed qualifier the
    /// other lacks (`消費税（10%）` for `消費税`).
    OneSidedQualifier,
    /// The value is tied to its label by position only: a right neighbour
    /// without a delimiter, a value below the label, or a table or list row.
    GeometricOnly,
    /// The value spans more than one line.
    MultiLine,
    /// A text value holds more than one value (see [`several_values`]).
    SeveralValues,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FillEvidence {
    pub page: u32,
    pub fragment_id: String,
    /// Byte range within the fragment's raw text.
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Display coordinates in PDF points (top-left origin).
    pub bbox: Rect,
    pub precision: Precision,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FillCandidate {
    pub label: String,
    pub raw: String,
    pub value: Option<Value>,
    pub score: f64,
    pub placement: Placement,
    /// Set when this candidate may not be filled offline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
    pub evidence: Vec<FillEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldFill {
    pub status: FillStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub evidence: Vec<FillEvidence>,
    /// Ranked alternatives when the field was not filled.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<FillCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FillResult {
    pub evaluation_only: bool,
    pub document_id: String,
    pub fields: BTreeMap<String, FieldFill>,
}

/// Match the document's layout pairs against every form field with the
/// default [`FillLimits`]. When a limit is exceeded every field is reported
/// as [`FillStatus::LimitExceeded`]; use [`fill_with_limits`] for a typed
/// error instead.
#[must_use]
pub fn fill(document_id: &str, fragments: &[SourceFragment], form: &Form) -> FillResult {
    fill_with_limits(document_id, fragments, form, &FillLimits::default()).unwrap_or_else(|_| {
        FillResult {
            evaluation_only: true,
            document_id: document_id.into(),
            fields: form
                .fields
                .iter()
                .map(|field| (field.id.clone(), empty(FillStatus::LimitExceeded)))
                .collect(),
        }
    })
}

/// Match the document's layout pairs against every form field within `limits`.
///
/// # Errors
/// [`FillError::LayoutLimitExceeded`] when the layout would hold more than
/// `limits.max_pairs` hypotheses, [`FillError::BudgetExceeded`] when layout
/// and matching need more than `limits.max_work` units, and
/// [`FillError::DeadlineExceeded`] once `limits.deadline` has passed.
pub fn fill_with_limits(
    document_id: &str,
    fragments: &[SourceFragment],
    form: &Form,
    limits: &FillLimits,
) -> Result<FillResult, FillError> {
    let mut budget = Budget::new(limits.max_work, limits.deadline);
    budget.check_deadline()?;
    let has_text = fragments
        .iter()
        .any(|f| f.quality == vf_source::Quality::Usable && !f.raw_text.trim().is_empty());
    if !has_text {
        return Ok(FillResult {
            evaluation_only: true,
            document_id: document_id.into(),
            fields: form
                .fields
                .iter()
                .map(|field| (field.id.clone(), empty(FillStatus::NoText)))
                .collect(),
        });
    }
    let pairs = layout::analyze_within(fragments, limits.max_pairs, &mut budget)?.pairs;
    budget.spend(fragments.len() as u64)?;
    let texts: HashMap<&str, &SourceFragment> =
        fragments.iter().map(|f| (f.id.as_str(), f)).collect();
    // A unit note anywhere on a page scales the amounts and numbers on it.
    let mut scaled_pages = BTreeSet::new();
    for fragment in fragments {
        budget.spend_bytes(&fragment.raw_text)?;
        if unit_note(&fragment.raw_text) {
            scaled_pages.insert(fragment.page);
        }
    }
    // A value that reads as one of the form's own labels is a neighbouring label, not a value.
    let mut labels = BTreeSet::new();
    for name in form
        .fields
        .iter()
        .flat_map(|f| std::iter::once(&f.label).chain(&f.hints))
    {
        budget.spend_bytes(name)?;
        labels.insert(normalize_label(name).stem);
    }
    let index = PairLabels::new(&pairs, &texts, &mut budget)?;
    let context = Context {
        pairs: &pairs,
        index: &index,
        texts: &texts,
        labels: &labels,
        scaled_pages: &scaled_pages,
    };
    let mut cache = Cache {
        slots: (0..pairs.len()).map(|_| None).collect(),
    };
    let mut fields = BTreeMap::new();
    for field in &form.fields {
        budget.check_deadline()?;
        let fill = context.fill_field(field, &mut cache, &mut budget)?;
        fields.insert(field.id.clone(), fill);
    }
    Ok(FillResult {
        evaluation_only: true,
        document_id: document_id.into(),
        fields,
    })
}

fn empty(status: FillStatus) -> FieldFill {
    FieldFill {
        status,
        method: None,
        raw: None,
        value: None,
        evidence: Vec::new(),
        candidates: Vec::new(),
    }
}

/// Pair labels normalized once per document and grouped by normalized text,
/// so each field compares against every distinct label once.
struct PairLabels {
    groups: Vec<(Label, Vec<usize>)>,
    /// Each pair's label text and group; `None` when its span does not resolve.
    texts: Vec<Option<(String, usize)>>,
}

impl PairLabels {
    fn new(
        pairs: &[Pair],
        texts: &HashMap<&str, &SourceFragment>,
        budget: &mut Budget,
    ) -> Result<Self, BudgetError> {
        let mut groups: Vec<(Label, Vec<usize>)> = Vec::new();
        let mut positions: HashMap<Label, usize> = HashMap::new();
        let mut label_texts = Vec::with_capacity(pairs.len());
        for (i, pair) in pairs.iter().enumerate() {
            budget.spend(1)?;
            let Some(text) = slice(texts, &pair.label) else {
                label_texts.push(None);
                continue;
            };
            // Copy, normalization and hashing each read the text once.
            budget.spend(3 * text.len() as u64)?;
            let label = normalize_label(&text);
            let group = if let Some(&at) = positions.get(&label) {
                groups[at].1.push(i);
                at
            } else {
                positions.insert(label.clone(), groups.len());
                groups.push((label, vec![i]));
                groups.len() - 1
            };
            label_texts.push(Some((text, group)));
        }
        Ok(Self {
            groups,
            texts: label_texts,
        })
    }
}

/// A pair's value prepared once per document and shared by every field.
struct Prepared {
    /// The first value line reads as one of the form's labels.
    first_is_label: bool,
    /// `None` when the pair is not a value candidate.
    value: Option<PreparedValue>,
}

struct PreparedValue {
    raw: String,
    evidence: Vec<FillEvidence>,
    /// More than one value in the text ([`several_values`]).
    several: bool,
    /// The pair label states a scale unit (`千円`, `万円`) or the page has a
    /// unit note (`（単位：千円）`).
    scaled: bool,
    /// Normalized values for date, amount and number fields, on first use.
    typed: [Option<Option<Value>>; 3],
}

struct Cache {
    slots: Vec<Option<Prepared>>,
}

/// A label-matched pair for one field.
#[derive(Debug, Clone)]
struct Candidate {
    pair: usize,
    score: f64,
    hold: Option<Hold>,
    value: Option<Value>,
}

struct Context<'a> {
    pairs: &'a [Pair],
    index: &'a PairLabels,
    texts: &'a HashMap<&'a str, &'a SourceFragment>,
    /// The form's labels and hints ([`Label::stem`]).
    labels: &'a BTreeSet<String>,
    /// Pages with a unit note ([`unit_note`]).
    scaled_pages: &'a BTreeSet<u32>,
}

impl Context<'_> {
    fn fill_field(
        &self,
        field: &FormField,
        cache: &mut Cache,
        budget: &mut Budget,
    ) -> Result<FieldFill, FillError> {
        let mut names = Vec::new();
        for name in std::iter::once(&field.label).chain(&field.hints) {
            budget.spend_bytes(name)?;
            let label = normalize_label(name);
            if !label.base.is_empty() {
                names.push(label);
            }
        }
        let cost: u64 = names.iter().map(|n| n.base.len() as u64 + 1).sum();
        let mut matched: Vec<(usize, Match)> = Vec::new();
        for (label, members) in &self.index.groups {
            budget.spend(cost)?;
            let best = names
                .iter()
                .map(|name| label_score(name, label))
                .fold(Match::NONE, Match::better);
            if best.score >= MIN_PARTIAL {
                budget.spend(members.len() as u64)?;
                matched.extend(members.iter().map(|&pair| (pair, best)));
            }
        }
        // Document order first, so equal scores keep a deterministic order.
        matched.sort_unstable_by_key(|&(pair, _)| pair);
        let mut candidates = Vec::new();
        // The header of a column whose first row reads as a form label: that
        // column lists labels (a label/value grid), not data rows.
        let mut label_column: Option<&layout::Span> = None;
        for (index, found) in matched {
            let pair = &self.pairs[index];
            let prepared = self.prepare(index, cache, budget)?;
            if pair.placement == Placement::Column {
                if self.first_column_row(index) {
                    label_column = prepared.first_is_label.then_some(&pair.label);
                } else if label_column == Some(&pair.label) {
                    continue;
                }
            }
            let Some(value) = prepared.value.as_ref() else {
                continue;
            };
            // The first failed condition of the commit policy, in order.
            let hold = found
                .hold
                .or((pair.binding == Binding::Geometric).then_some(Hold::GeometricOnly))
                .or((value.evidence.len() > 1).then_some(Hold::MultiLine))
                .or((field.field_type == FieldType::Text && value.several)
                    .then_some(Hold::SeveralValues));
            let value = self.typed_value(index, field.field_type, cache, budget)?;
            candidates.push(Candidate {
                pair: index,
                score: found.score,
                hold,
                value,
            });
        }
        self.decide(field, candidates, cache, budget)
    }

    /// Column pairs of one header are consecutive, first row first.
    fn first_column_row(&self, index: usize) -> bool {
        index
            .checked_sub(1)
            .map(|previous| &self.pairs[previous])
            .is_none_or(|previous| {
                previous.placement != Placement::Column || previous.label != self.pairs[index].label
            })
    }

    /// The pair's value, prepared on first use.
    fn prepare<'c>(
        &self,
        index: usize,
        cache: &'c mut Cache,
        budget: &mut Budget,
    ) -> Result<&'c Prepared, BudgetError> {
        let slot = &mut cache.slots[index];
        let prepared = match slot.take() {
            Some(prepared) => prepared,
            None => self.prepare_value(index, budget)?,
        };
        Ok(slot.insert(prepared))
    }

    fn prepare_value(&self, index: usize, budget: &mut Budget) -> Result<Prepared, BudgetError> {
        let pair = &self.pairs[index];
        let rejected = |first_is_label| Prepared {
            first_is_label,
            value: None,
        };
        let Some((_, group)) = &self.index.texts[index] else {
            return Ok(rejected(false));
        };
        let mut evidence = Vec::with_capacity(pair.value.len());
        for (line, span) in pair.value.iter().enumerate() {
            budget.spend(1)?;
            let Some(text) = slice(self.texts, span) else {
                return Ok(rejected(false));
            };
            // Copy and label normalization.
            budget.spend(2 * text.len() as u64)?;
            if self.labels.contains(&normalize_label(&text).stem) {
                // A first line that reads as a form label is a neighbouring
                // label; a later one starts the next field's block.
                if line == 0 {
                    return Ok(rejected(true));
                }
                break;
            }
            evidence.push(FillEvidence {
                page: pair.page,
                fragment_id: span.fragment_id.clone(),
                start: span.start,
                end: span.end,
                text,
                bbox: span.bbox,
                precision: span.precision,
            });
        }
        let raw = evidence
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Join, label normalization, NFKC and the value-shape scan.
        budget.spend(4 * raw.len() as u64)?;
        if self.labels.contains(&normalize_label(&raw).stem) {
            return Ok(rejected(false));
        }
        let several = several_values(&raw.nfkc().collect::<String>());
        let scaled =
            states_scale(&self.index.groups[*group].0) || self.scaled_pages.contains(&pair.page);
        Ok(Prepared {
            first_is_label: false,
            value: Some(PreparedValue {
                raw,
                evidence,
                several,
                scaled,
                typed: [None, None, None],
            }),
        })
    }

    /// The pair's normalized value for `field_type`, computed once per type.
    fn typed_value(
        &self,
        index: usize,
        field_type: FieldType,
        cache: &mut Cache,
        budget: &mut Budget,
    ) -> Result<Option<Value>, BudgetError> {
        let slot = match field_type {
            FieldType::Text => return Ok(None),
            FieldType::Date => 0,
            FieldType::Amount => 1,
            FieldType::Number => 2,
        };
        let Some(value) = cache.slots[index].as_mut().and_then(|p| p.value.as_mut()) else {
            return Ok(None);
        };
        if value.typed[slot].is_none() {
            budget.spend(2 * value.raw.len() as u64)?;
            let normalized = if value.scaled && field_type != FieldType::Date {
                None
            } else {
                normalize_value(field_type, &value.raw)
            };
            value.typed[slot] = Some(normalized);
        }
        Ok(value.typed[slot].clone().flatten())
    }

    /// Fill, leave ambiguous or report invalid. Only the best-scoring
    /// candidates decide: all of them must be unheld and allow the same value.
    fn decide(
        &self,
        field: &FormField,
        mut candidates: Vec<Candidate>,
        cache: &Cache,
        budget: &mut Budget,
    ) -> Result<FieldFill, FillError> {
        // Stable: equal scores keep document order.
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        candidates.dedup_by(|a, b| self.pairs[a.pair].value == self.pairs[b.pair].value);
        let Some(best) = candidates.first() else {
            return Ok(empty(FillStatus::NotFound));
        };
        let top: Vec<&Candidate> = candidates
            .iter()
            .take_while(|c| (c.score - best.score).abs() < 1e-9)
            .collect();
        let raw = |c: &Candidate| {
            cache.slots[c.pair]
                .as_ref()
                .and_then(|p| p.value.as_ref())
                .map_or("", |v| v.raw.as_str())
        };
        // A held candidate (for example a table row) never determines one
        // value, even when the candidates agree or only one fits the type.
        let held = top.iter().any(|c| c.hold.is_some());
        let status = if held {
            FillStatus::Ambiguous
        } else if field.field_type == FieldType::Text {
            if top.iter().all(|c| raw(c) == raw(best)) {
                FillStatus::Filled
            } else {
                FillStatus::Ambiguous
            }
        } else if top.iter().all(|c| c.value.is_none()) {
            FillStatus::Invalid
        } else if top
            .iter()
            .all(|c| c.value.is_some() && c.value == best.value)
        {
            FillStatus::Filled
        } else {
            FillStatus::Ambiguous
        };
        let mut result = empty(status);
        match status {
            FillStatus::Filled => {
                let (raw, evidence) = self.output(best, cache, budget)?;
                result.method = Some("label_match");
                result.value = best
                    .value
                    .clone()
                    .or_else(|| Some(Value::String(raw.clone())));
                result.raw = Some(raw);
                result.evidence = evidence;
            }
            _ => {
                if status == FillStatus::Invalid {
                    let (raw, evidence) = self.output(best, cache, budget)?;
                    result.raw = Some(raw);
                    result.evidence = evidence;
                }
                for candidate in candidates.iter().take(MAX_CANDIDATES) {
                    let (raw, evidence) = self.output(candidate, cache, budget)?;
                    let label = self.index.texts[candidate.pair]
                        .as_ref()
                        .map_or_else(String::new, |(text, _)| text.clone());
                    budget.spend_bytes(&label)?;
                    result.candidates.push(FillCandidate {
                        label,
                        raw,
                        value: candidate.value.clone(),
                        score: candidate.score,
                        placement: self.pairs[candidate.pair].placement,
                        hold: candidate.hold,
                        evidence,
                    });
                }
            }
        }
        Ok(result)
    }

    /// A copy of a candidate's raw text and evidence for the result.
    fn output(
        &self,
        candidate: &Candidate,
        cache: &Cache,
        budget: &mut Budget,
    ) -> Result<(String, Vec<FillEvidence>), BudgetError> {
        let Some(value) = cache.slots[candidate.pair]
            .as_ref()
            .and_then(|p| p.value.as_ref())
        else {
            return Ok((String::new(), Vec::new()));
        };
        budget.spend(2 * value.raw.len() as u64 + value.evidence.len() as u64)?;
        Ok((value.raw.clone(), value.evidence.clone()))
    }
}

fn slice(texts: &HashMap<&str, &SourceFragment>, span: &layout::Span) -> Option<String> {
    texts
        .get(span.fragment_id.as_str())
        .and_then(|f| f.raw_text.get(span.start..span.end))
        .map(str::to_owned)
}

/// A normalized label: base text and its bracketed qualifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Label {
    base: String,
    /// The base without one leading honorific (`ご`, `御`, `お`).
    stem: String,
    qualifiers: Vec<String>,
    base_chars: usize,
    qualifier_chars: usize,
}

impl Label {
    /// The base's share of the whole label, in characters.
    fn base_share(&self) -> f64 {
        self.base_chars as f64 / (self.base_chars + self.qualifier_chars) as f64
    }
}

/// NFKC, drop whitespace and trailing delimiters, lowercase; bracketed text
/// becomes qualifiers. A label that is only bracketed text keeps it as base
/// (`【住所】` = `住所`).
fn normalize_label(s: &str) -> Label {
    let nfkc: String = s.nfkc().collect();
    let mut base = String::new();
    let mut qualifiers = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for c in nfkc.chars() {
        match c {
            '(' | '[' | '「' | '【' | '〔' => depth += 1,
            ')' | ']' | '」' | '】' | '〕' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !current.is_empty() {
                    qualifiers.push(std::mem::take(&mut current).to_lowercase());
                }
            }
            _ if c.is_whitespace() => {}
            _ if depth > 0 => current.push(c),
            _ => base.push(c),
        }
    }
    if !current.is_empty() {
        qualifiers.push(current.to_lowercase());
    }
    let mut base = base.trim_end_matches([':', '：', '.', '。']).to_lowercase();
    if base.is_empty() {
        base = qualifiers.concat();
        qualifiers.clear();
    }
    let stem = base
        .strip_prefix(HONORIFICS)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(&base)
        .to_owned();
    Label {
        base_chars: base.chars().count(),
        qualifier_chars: qualifiers.iter().map(|q| q.chars().count()).sum(),
        base,
        stem,
        qualifiers,
    }
}

/// Leading honorifics ignored when labels are compared (`ご請求額` = `請求額`).
const HONORIFICS: [char; 3] = ['ご', '御', 'お'];

/// A label comparison: its score and why it may not be committed
/// ([`Hold::PartialLabel`] or [`Hold::OneSidedQualifier`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Match {
    score: f64,
    hold: Option<Hold>,
}

impl Match {
    const NONE: Self = Self {
        score: 0.0,
        hold: None,
    };

    /// The higher score; at equal scores, a match without a hold.
    fn better(self, other: Self) -> Self {
        if other.score > self.score
            || other.score == self.score && self.hold.is_some() && other.hold.is_none()
        {
            other
        } else {
            self
        }
    }
}

/// Base similarity, then qualifiers. Equal bases (or equal bases without a
/// leading honorific) score 1; a base contained in the other scores
/// short/long and is [`Hold::PartialLabel`]. Equal qualifiers keep the
/// score, different ones on both sides never match, and a qualifier on one
/// side scales the score by that label's base share and is
/// [`Hold::OneSidedQualifier`] (unless the base is already partial).
fn label_score(a: &Label, b: &Label) -> Match {
    let base = if a.base == b.base || a.stem == b.stem {
        1.0
    } else {
        let (short, long) = if a.base_chars <= b.base_chars {
            (a, b)
        } else {
            (b, a)
        };
        // Containment scores short/long, so shorter ratios never reach MIN_PARTIAL.
        if short.base_chars == 0
            || (short.base_chars as f64) < MIN_PARTIAL * long.base_chars as f64
            || !long.base.contains(short.base.as_str())
        {
            return Match::NONE;
        }
        short.base_chars as f64 / long.base_chars as f64
    };
    let partial = (base < 1.0).then_some(Hold::PartialLabel);
    if a.qualifiers == b.qualifiers {
        return Match {
            score: base,
            hold: partial,
        };
    }
    let share = match (a.qualifiers.is_empty(), b.qualifiers.is_empty()) {
        (true, _) => b.base_share(),
        (_, true) => a.base_share(),
        (false, false) => return Match::NONE,
    };
    Match {
        score: base * share,
        hold: partial.or(Some(Hold::OneSidedQualifier)),
    }
}

/// A page-wide unit note (`（単位：千円）`, `単位：万円`, `単位 百万円`): `単位`,
/// optional spaces or a colon, then a scale (`十` `百` `千` `万` `億` `兆`).
fn unit_note(text: &str) -> bool {
    text.match_indices("単位").any(|(at, unit)| {
        let rest = text[at + unit.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：'));
        ["十", "百", "千", "万", "億", "兆"]
            .iter()
            .any(|scale| rest.starts_with(scale))
    })
}

/// Scale units that make a label's amounts and numbers multiples
/// (`売上高（千円）`, `金額（単位：万円）`, `売上高百万円`).
fn states_scale(label: &Label) -> bool {
    const SCALES: [&str; 5] = ["千", "万", "百万", "億", "兆"];
    let in_base = ["千円", "万円", "億円", "兆円"]
        .iter()
        .any(|unit| label.base.contains(unit));
    in_base
        || label.qualifiers.iter().any(|q| {
            let q = q.trim_start_matches("単位").trim_start_matches([':', '：']);
            SCALES.iter().any(|scale| q.starts_with(scale))
        })
}

/// Deterministic whole-text normalization; `None` when the text is not
/// exactly one value of the type:
/// - date: one date ([`parse_date`]), optionally followed by its weekday in
///   brackets, which must match the date;
/// - amount: one amount ([`parse_amount`]);
/// - number: one number with an optional unit ([`parse_number`]).
fn normalize_value(field_type: FieldType, raw: &str) -> Option<Value> {
    match field_type {
        FieldType::Text => None,
        FieldType::Date => parse_date(&raw.nfkc().collect::<String>()).map(Value::String),
        FieldType::Amount => parse_amount(&numeric_text(raw)?),
        FieldType::Number => parse_number(&numeric_text(raw)?),
    }
}

/// NFKC text for number parsing. A superscript after a letter is a unit
/// exponent (`m²`) and is dropped; any other superscript (`10²`) is refused,
/// because NFKC would turn it into an ordinary digit.
fn numeric_text(raw: &str) -> Option<String> {
    let superscript = |c: char| matches!(c, '¹' | '²' | '³' | '\u{2070}' | '\u{2074}'..='\u{2079}');
    let mut kept = String::with_capacity(raw.len());
    let mut previous = None;
    for c in raw.chars() {
        if superscript(c) {
            if !previous.is_some_and(|p: char| p.is_alphabetic()) {
                return None;
            }
        } else {
            kept.push(c);
            previous = Some(c);
        }
    }
    Some(kept.nfkc().collect())
}

/// One whole date to `YYYY-MM-DD`: `2026年11月8日`, `2026/11/08`, `2026-11-8`,
/// `2026.11.08`, and Japanese era dates via [`parse_era_name_date`] and
/// [`parse_era_letter_date`]. Only the date's own weekday in brackets may
/// follow (`(日)`, `（日曜日）`, `(Sun)`); an unbracketed or mismatching
/// weekday, a range, a time or a qualifier such as `以降` or `まで` is `None`.
fn parse_date(text: &str) -> Option<String> {
    let text = text.trim();
    let (date, rest) = if ERA_NAMES.iter().any(|name| text.contains(name)) {
        // Never digit-scan mixed notations such as `2026年(令和8年)11月8日`.
        parse_era_name_date(text)?
    } else if let Some(found) = parse_era_letter_date(text) {
        found
    } else {
        parse_gregorian_date(text)?
    };
    weekday_matches(rest, date).then(|| format!("{:04}-{:02}-{:02}", date.0, date.1, date.2))
}

/// A valid calendar date (year, month, day).
type Date = (u32, u32, u32);

/// `2026年11月8日` (spaces allowed between parts) or a four-digit year,
/// month and day joined by one separator kind (`/`, `-` or `.`); with the
/// text after it.
fn parse_gregorian_date(text: &str) -> Option<(Date, &str)> {
    let (year, rest) = leading_digits(text, 4, 4)?;
    let (month, day, rest) = if let Some(rest) = rest.trim_start().strip_prefix('年') {
        let (month, rest) = leading_digits(rest.trim_start(), 1, 2)?;
        let rest = rest.trim_start().strip_prefix('月')?;
        let (day, rest) = leading_digits(rest.trim_start(), 1, 2)?;
        (month, day, rest.trim_start().strip_prefix('日')?)
    } else {
        let separator = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '/' | '-' | '.'))?;
        let (month, rest) = leading_digits(&rest[separator.len_utf8()..], 1, 2)?;
        let (day, rest) = leading_digits(rest.strip_prefix(separator)?, 1, 2)?;
        (month, day, rest)
    };
    Some((calendar_date(year, month, day)?, rest))
}

/// Text after a date that keeps its meaning: nothing, or the date's own
/// weekday in brackets: `(日)`, `(日曜)`, `(日曜日)` or `(Sun)`.
fn weekday_matches(rest: &str, (year, month, day): Date) -> bool {
    const DAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
    const ENGLISH: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let rest = rest.trim();
    if rest.is_empty() {
        return true;
    }
    let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    let inner = inner.trim();
    let named = DAYS.iter().position(|day| {
        inner
            .strip_prefix(day)
            .is_some_and(|tail| tail.is_empty() || tail == "曜" || tail == "曜日")
    });
    let english = ENGLISH
        .iter()
        .position(|day| inner.eq_ignore_ascii_case(day));
    named.or(english) == Some(weekday(year, month, day))
}

/// Day of the week, 0 = Sunday (Sakamoto's method).
fn weekday(year: u32, month: u32, day: u32) -> usize {
    const OFFSETS: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let year = if month < 3 { year - 1 } else { year };
    let index = (month as usize).saturating_sub(1).min(11);
    ((year + year / 4 - year / 100 + year / 400 + OFFSETS[index] + day) % 7) as usize
}

/// A Japanese era with its abbreviation and first and last day (inclusive).
struct Era {
    name: &'static str,
    letter: Option<char>,
    first: (u32, u32, u32),
    last: Option<(u32, u32, u32)>,
}

const ERAS: [Era; 4] = [
    Era {
        name: "令和",
        letter: Some('R'),
        first: (2019, 5, 1),
        last: None,
    },
    Era {
        name: "平成",
        letter: Some('H'),
        first: (1989, 1, 8),
        last: Some((2019, 4, 30)),
    },
    Era {
        name: "昭和",
        letter: Some('S'),
        first: (1926, 12, 25),
        last: Some((1989, 1, 7)),
    },
    Era {
        name: "大正",
        letter: None,
        first: (1912, 7, 30),
        last: Some((1926, 12, 24)),
    },
];

/// Era names that take a text out of the Gregorian parse (明治 is unsupported).
const ERA_NAMES: [&str; 5] = ["令和", "平成", "昭和", "大正", "明治"];

/// `令和8年11月8日`, `令和元年5月1日`, `平成 31年 4月 30日`: the era name first,
/// then year, month and day; with the text after it.
fn parse_era_name_date(text: &str) -> Option<(Date, &str)> {
    let era = ERAS.iter().find(|era| text.starts_with(era.name))?;
    let rest = text[era.name.len()..].trim_start();
    let (year, rest) = match rest.strip_prefix('元') {
        Some(rest) => (1, rest),
        None => leading_digits(rest, 1, 2)?,
    };
    let rest = rest.trim_start().strip_prefix('年')?;
    let (month, rest) = leading_digits(rest.trim_start(), 1, 2)?;
    let rest = rest.trim_start().strip_prefix('月')?;
    let (day, rest) = leading_digits(rest.trim_start(), 1, 2)?;
    let rest = rest.trim_start().strip_prefix('日')?;
    Some((era_date(era, year, month, day)?, rest))
}

/// `R8.11.8`, `H31/4/30`, `S55.3.1`: one separator kind; with the text after it.
fn parse_era_letter_date(text: &str) -> Option<(Date, &str)> {
    let mut chars = text.chars();
    let letter = chars.next()?;
    let era = ERAS.iter().find(|era| era.letter == Some(letter))?;
    let (year, rest) = leading_digits(chars.as_str(), 1, 2)?;
    let separator = rest.chars().next().filter(|c| matches!(c, '.' | '/'))?;
    let (month, rest) = leading_digits(rest.strip_prefix(separator)?, 1, 2)?;
    let (day, rest) = leading_digits(rest.strip_prefix(separator)?, 1, 2)?;
    Some((era_date(era, year, month, day)?, rest))
}

/// Between `min` and `max` leading ASCII digits and the rest of the text.
fn leading_digits(text: &str, min: usize, max: usize) -> Option<(u32, &str)> {
    let len = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if !(min..=max).contains(&len) {
        return None;
    }
    Some((text[..len].parse().ok()?, &text[len..]))
}

/// The Gregorian date of an era date, only when it falls inside that era.
fn era_date(era: &Era, year: u32, month: u32, day: u32) -> Option<Date> {
    if year == 0 {
        return None;
    }
    let date = (era.first.0 + year - 1, month, day);
    let in_era = date >= era.first && era.last.is_none_or(|last| date <= last);
    in_era.then(|| calendar_date(date.0, month, day)).flatten()
}

/// A valid calendar date with a four-digit year.
fn calendar_date(year: u32, month: u32, day: u32) -> Option<Date> {
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => return None,
    };
    ((1000..=9999).contains(&year) && (1..=days).contains(&day)).then_some((year, month, day))
}

/// Tax notes allowed after an amount (after NFKC, `（税込）` is `(税込)`).
const TAX_NOTES: [&str; 4] = ["(税込)", "(税抜)", "税込", "税抜"];
/// Currency marks allowed before an amount (after NFKC, `￥` is `¥`).
const CURRENCY_PREFIXES: [char; 3] = ['¥', '$', '€'];
/// Signs that make an amount negative.
const AMOUNT_SIGNS: [char; 4] = ['-', '\u{2212}', '△', '▲'];

/// The whole text as exactly one amount:
/// `[sign] [currency] [sign] number [円] [tax note]`, where
/// - sign is one of `-`, `−` (U+2212), `△`, `▲`; it makes the amount
///   negative, and at most one is allowed. Enclosing parentheses are not a
///   sign in Japanese documents (`（1,000円）` is invalid);
/// - currency is one of `¥` `￥` `$` `€` before the number, or `円` after
///   it, never both;
/// - number is digits with valid thousands separators (`12,100`), no
///   internal spaces, and at most two decimals;
/// - tax note is one of `（税込）` `（税抜）` `(税込)` `(税抜)` `税込` `税抜`.
///
/// Spaces are allowed only between these parts. Ranges, `以上` / `まで` /
/// `約`, rates, several numbers and `万` / `千` units are `None`.
fn parse_amount(text: &str) -> Option<Value> {
    let mut rest = text.trim();
    if let Some(stripped) = TAX_NOTES.iter().find_map(|note| rest.strip_suffix(note)) {
        rest = stripped.trim_end();
    }
    let mut signs = 0;
    if let Some(stripped) = rest.strip_prefix(AMOUNT_SIGNS) {
        rest = stripped.trim_start();
        signs += 1;
    }
    let prefixed = match rest.strip_prefix(CURRENCY_PREFIXES) {
        Some(stripped) => {
            rest = stripped.trim_start();
            true
        }
        None => false,
    };
    if let Some(stripped) = rest.strip_prefix(AMOUNT_SIGNS) {
        rest = stripped.trim_start();
        signs += 1;
    }
    let suffixed = match rest.strip_suffix('円') {
        Some(stripped) => {
            rest = stripped.trim_end();
            true
        }
        None => false,
    };
    if signs > 1 || prefixed && suffixed {
        return None;
    }
    let value = parse_decimal(rest, 2)?;
    if signs == 1 {
        negate(&value)
    } else {
        Some(value)
    }
}

/// Units allowed after a number (after NFKC; `㎡` is `m2`, `m²` is `m`).
const NUMBER_UNITS: [&str; 16] = [
    "個", "点", "件", "名", "人", "枚", "本", "台", "回", "kg", "g", "km", "m", "cm", "mm", "m2",
];

/// The whole text as exactly one number: `[sign] number [unit]`, with sign
/// `-`, `−` (U+2212) or `+`, number as in [`parse_amount`] with any number
/// of decimals, and unit one of [`NUMBER_UNITS`] (spaces allowed before it).
fn parse_number(text: &str) -> Option<Value> {
    let text = text.trim();
    let body = NUMBER_UNITS
        .iter()
        .filter_map(|unit| text.strip_suffix(unit))
        .map(str::trim_end)
        .find(|body| body.ends_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(text);
    let (negative, digits) = if let Some(digits) = body.strip_prefix(['-', '\u{2212}']) {
        (true, digits.trim_start())
    } else {
        (false, body.strip_prefix('+').map_or(body, str::trim_start))
    };
    let value = parse_decimal(digits, MAX_DIGITS)?;
    if negative {
        negate(&value)
    } else {
        Some(value)
    }
}

/// ASCII digits with optional thousands separators (a group of one to three
/// digits not starting with `0`, then groups of three) and an optional
/// fraction of one to `max_fraction` digits; at most [`MAX_DIGITS`] digits in
/// all. An integer part other than `0` has no leading zero (`007`, `0,123`).
/// A fraction of exactly three digits after an ungrouped integer of one to
/// three digits other than `0` (`1.000`, `12.500`) is refused: it may be a
/// European thousands separator.
fn parse_decimal(text: &str, max_fraction: usize) -> Option<Value> {
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (text, None),
    };
    if integer.is_empty() || !integer.chars().all(|c| c.is_ascii_digit() || c == ',') {
        return None;
    }
    if integer.contains(',') {
        let mut groups = integer.split(',');
        let head = groups.next().unwrap_or_default();
        if !(1..=3).contains(&head.len())
            || head.starts_with('0')
            || !groups.all(|group| group.len() == 3)
        {
            return None;
        }
    } else if integer.len() > 1 && integer.starts_with('0') {
        return None;
    }
    let digits = integer.replace(',', "");
    if let Some(fraction) = fraction
        && (fraction.is_empty()
            || fraction.len() > max_fraction
            || !fraction.chars().all(|c| c.is_ascii_digit())
            || fraction.len() == 3
                && !integer.contains(',')
                && integer.len() <= 3
                && integer != "0")
    {
        return None;
    }
    if digits.len() + fraction.map_or(0, str::len) > MAX_DIGITS {
        return None;
    }
    match fraction {
        None => digits.parse::<i64>().ok().map(Value::from),
        Some(fraction) => format!("{digits}.{fraction}")
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
    }
}

fn negate(value: &Value) -> Option<Value> {
    if let Some(n) = value.as_i64() {
        return n.checked_neg().map(Value::from);
    }
    value
        .as_f64()
        .and_then(|n| serde_json::Number::from_f64(-n).map(Value::Number))
}

/// A kind of value that may appear in a text value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Phone,
    Date,
    Amount,
}

/// Marks that join two dates or amounts into one range (after NFKC).
const RANGE_MARKS: [&str; 6] = ["〜", "~", "-", "\u{2212}", "\u{2013}", "\u{2014}"];

/// Whether NFKC `text` holds more than one value ([`Hold::SeveralValues`]).
/// Phone numbers, dates and currency amounts are found by [`value_shapes`];
/// the text holds several values when it has
/// - two phone numbers, whatever joins them (`03-1234-5678 FAX
///   03-1234-5679`, `03-1234-5678、03-1234-5679`);
/// - two dates or two amounts not joined by a range mark alone: a word,
///   `、`, `/` or a space between them (`1,000円 と 500円`); a range such as
///   `2026年11月8日〜2026年12月8日` or `¥1,000-¥2,000` is one value;
/// - a phone number, date or amount right after a word (letters, with
///   spaces, currency marks and signs between them ignored) that itself
///   follows other text: a second label with its value, as in `山田 太郎
///   生年月日 1990年1月1日`. A word at the very start (`TEL 03-1234-5678`)
///   is not a second label.
fn several_values(text: &str) -> bool {
    let mut tokens = value_shapes(text);
    tokens.sort_by_key(|&(_, start, _)| start);
    let range = |end: usize, start: usize| {
        let between = text.get(end..start).unwrap_or_default();
        let between = between
            .trim_start_matches(|c: char| c.is_whitespace() || c == '円')
            .trim_end_matches(|c: char| c.is_whitespace() || CURRENCY_PREFIXES.contains(&c));
        RANGE_MARKS.contains(&between)
    };
    let two_of_a_kind = [Shape::Phone, Shape::Date, Shape::Amount]
        .iter()
        .any(|&shape| {
            tokens
                .iter()
                .filter(|(kind, _, _)| *kind == shape)
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| shape == Shape::Phone || !range(pair[0].2, pair[1].1))
        });
    two_of_a_kind || tokens.iter().any(|&(_, start, _)| after_label(text, start))
}

/// Whether the text before byte `start` ends with a word that follows other
/// text (spaces, currency marks and signs before `start` ignored).
fn after_label(text: &str, start: usize) -> bool {
    let before = text[..start].trim_end_matches(|c: char| {
        c.is_whitespace() || CURRENCY_PREFIXES.contains(&c) || AMOUNT_SIGNS.contains(&c)
    });
    let rest = before.trim_end_matches(char::is_alphabetic);
    rest.len() < before.len() && !rest.trim().is_empty()
}

/// Phone numbers, dates and currency amounts in `text` with their byte ranges.
fn value_shapes(text: &str) -> Vec<(Shape, usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let at = |i: usize| chars.get(i).map_or(text.len(), |&(at, _)| at);
    let mut tokens = Vec::new();
    // Phone numbers ([`phone_at`]), then runs of digits and separators:
    // numeric dates and amounts.
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        let boundary = i == 0 || !chars[i - 1].1.is_ascii_digit();
        if boundary
            && (c.is_ascii_digit() || c == '(')
            && let Some(end) = phone_at(&chars, i)
        {
            tokens.push((Shape::Phone, at(i), at(end)));
            i = end;
            continue;
        }
        if !c.is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len()
            && (chars[i].1.is_ascii_digit() || matches!(chars[i].1, '-' | '/' | '.' | ','))
        {
            i += 1;
        }
        let mut end = i;
        while !chars[end - 1].1.is_ascii_digit() {
            end -= 1;
        }
        let run = &text[at(start)..at(end)];
        let before = text[..at(start)].trim_end();
        let before = before
            .strip_suffix(AMOUNT_SIGNS)
            .unwrap_or(before)
            .trim_end();
        let shape = if numeric_date(run) {
            Some(Shape::Date)
        } else if before.ends_with(CURRENCY_PREFIXES)
            || text[at(end)..].trim_start().starts_with('円')
        {
            Some(Shape::Amount)
        } else {
            None
        };
        if let Some(shape) = shape {
            tokens.push((shape, at(start), at(end)));
        }
    }
    // Kanji dates: `11月8日`, extended back over a year and an era name.
    for (n, &(_, c)) in chars.iter().enumerate() {
        if c != '月' {
            continue;
        }
        let digits_back = |mut j: usize, max: usize| {
            let mut count = 0;
            while j > 0 && count < max && chars[j - 1].1.is_ascii_digit() {
                j -= 1;
                count += 1;
            }
            (j, count)
        };
        let skip_space_back = |mut j: usize| {
            while j > 0 && chars[j - 1].1.is_whitespace() {
                j -= 1;
            }
            j
        };
        let skip_space = |mut j: usize| {
            while j < chars.len() && chars[j].1.is_whitespace() {
                j += 1;
            }
            j
        };
        let (mut start, month_digits) = digits_back(skip_space_back(n), 2);
        let mut j = skip_space(n + 1);
        let day_start = j;
        while j < chars.len() && j - day_start < 2 && chars[j].1.is_ascii_digit() {
            j += 1;
        }
        let day_digits = j - day_start;
        let j = skip_space(j);
        if month_digits == 0 || day_digits == 0 || chars.get(j).is_none_or(|&(_, c)| c != '日') {
            continue;
        }
        let year = skip_space_back(start);
        if year > 0 && chars[year - 1].1 == '年' {
            let digits = skip_space_back(year - 1);
            let (year_start, count) = digits_back(digits, 4);
            if count > 0 {
                start = year_start;
            } else if digits > 0 && chars[digits - 1].1 == '元' {
                start = digits - 1;
            }
            let era = skip_space_back(start);
            if era >= 2 && ERA_NAMES.contains(&&text[at(era - 2)..at(era)]) {
                start = era - 2;
            }
        }
        tokens.push((Shape::Date, at(start), at(j + 1)));
    }
    tokens
}

/// A numeric date such as `2026/11/08`, `R8.11.8` (from its digits) or
/// `2026-11-08`: three digit groups joined by one separator kind, `/` or
/// `.` (`-` only after a four-digit year), month and day of one or two digits.
fn numeric_date(run: &str) -> bool {
    let Some(separator) = run.chars().find(|c| matches!(c, '/' | '.' | '-')) else {
        return false;
    };
    let groups: Vec<&str> = run.split(separator).collect();
    let [year, month, day] = groups[..] else {
        return false;
    };
    let digits =
        |s: &str, max: usize| (1..=max).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    digits(year, 4) && digits(month, 2) && digits(day, 2) && (separator != '-' || year.len() == 4)
}

/// The end (char index) of a phone number starting at char `i`, which is
/// a digit or `(` not preceded by a digit. The number is the longest run of
/// digits, `-`, `(` and `)` (one space allowed after `)`), without trailing
/// separators, and one of:
/// - three or more digit groups joined by hyphens with 9 to 13 digits in
///   all (`03-1234-5678`, `0120-123-456`);
/// - 10 or 11 digits starting with `0` (`0312345678`);
/// - an area code starting with `0` in brackets, then the rest
///   (`(03)1234-5678`, `03(1234)5678`), with 10 or 11 digits in all.
fn phone_at(chars: &[(usize, char)], i: usize) -> Option<usize> {
    let mut run = String::new();
    let mut end = i;
    while let Some(&(_, c)) = chars.get(end) {
        let digit_next = chars.get(end + 1).is_some_and(|&(_, n)| n.is_ascii_digit());
        if c.is_ascii_digit() || matches!(c, '-' | '(' | ')') {
            run.push(c);
        } else if !(c == ' ' && run.ends_with(')') && digit_next) {
            break;
        }
        end += 1;
    }
    // Trailing separators are never part of the number (spaces were only
    // taken before a digit).
    while run.ends_with(|c: char| !c.is_ascii_digit()) {
        run.pop();
        end -= 1;
    }
    is_phone(&run).then_some(end)
}

/// See [`phone_at`].
fn is_phone(run: &str) -> bool {
    let digits = run.chars().filter(char::is_ascii_digit).count();
    let groups_ok = |groups: &[&str]| {
        groups
            .iter()
            .all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()))
    };
    if run.chars().all(|c| c.is_ascii_digit()) {
        return (10..=11).contains(&digits) && run.starts_with('0');
    }
    if !run.contains(['(', ')']) {
        let groups: Vec<&str> = run.split('-').collect();
        return groups.len() >= 3 && groups_ok(&groups) && (9..=13).contains(&digits);
    }
    let bracketed = match run.strip_prefix('(') {
        Some(rest) => rest.split_once(')').and_then(|(area, local)| {
            let (exchange, line) = local.split_once('-')?;
            Some([area, exchange, line])
        }),
        None => run.split_once('(').and_then(|(area, rest)| {
            let (exchange, line) = rest.split_once(')')?;
            Some([area, exchange, line])
        }),
    };
    bracketed.is_some_and(|groups| {
        groups_ok(&groups) && groups[0].starts_with('0') && (10..=11).contains(&digits)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vf_source::Quality;

    fn frag(id: &str, text: &str, [l, t, r, b]: [f64; 4]) -> SourceFragment {
        SourceFragment {
            id: id.into(),
            page: 1,
            item_index: 0,
            raw_text: text.into(),
            bbox: Rect {
                left: l,
                top: t,
                right: r,
                bottom: b,
            },
            quality: Quality::Usable,
        }
    }

    fn form(json: serde_json::Value) -> Form {
        parse_form(json.to_string().as_bytes()).unwrap()
    }

    fn invoice() -> Vec<SourceFragment> {
        vec![
            frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
            frag("b", "発行日：2026年11月8日", [60.0, 139.0, 179.8, 150.0]),
            frag("c", "小計：", [330.0, 219.0, 363.0, 230.0]),
            frag("d", "¥11,000", [480.2, 219.0, 520.0, 230.0]),
            frag("e", "合計：", [330.0, 259.0, 363.0, 270.0]),
            frag("f", "¥12,100", [480.2, 259.0, 520.0, 270.0]),
            frag("g", "住所", [60.0, 319.0, 82.0, 330.0]),
            frag(
                "h",
                "東京都架空区サンプル町1-2-3",
                [200.0, 319.0, 346.7, 330.0],
            ),
            frag("i", "テストビル5階", [200.0, 335.0, 272.1, 346.0]),
        ]
    }

    #[test]
    fn fills_unique_label_matches_with_normalized_values() {
        let result = fill(
            "doc",
            &invoice(),
            &form(json!({"fields":[
                {"id":"no","label":"請求番号"},
                {"id":"issued","label":"発行日","type":"date"},
                {"id":"total","label":"ご請求額","type":"amount","hints":["合計"]},
                {"id":"address","label":"住所"},
                {"id":"person","label":"担当者"}
            ]})),
        );
        assert!(result.evaluation_only);
        let f = &result.fields;
        assert_eq!(f["no"].status, FillStatus::Filled);
        assert_eq!(f["no"].raw.as_deref(), Some("INV-0012"));
        assert_eq!(f["no"].evidence[0].precision, Precision::Estimated);
        assert_eq!(f["issued"].value, Some(json!("2026-11-08")));
        assert_eq!(f["issued"].raw.as_deref(), Some("2026年11月8日"));
        // `合計：` and the amount to its right on the same line: delimiter-bound.
        assert_eq!(f["total"].status, FillStatus::Filled);
        assert_eq!(f["total"].value, Some(json!(12100)));
        assert_eq!(f["total"].evidence[0].fragment_id, "f");
        // Changed (strict policy): 住所 has no delimiter, so its right value
        // (two lines) is a candidate only. Before: filled with both lines.
        let address = &f["address"];
        assert_eq!(address.status, FillStatus::Ambiguous);
        assert_eq!(address.candidates[0].hold, Some(Hold::GeometricOnly));
        assert_eq!(
            address.candidates[0].raw,
            "東京都架空区サンプル町1-2-3\nテストビル5階"
        );
        assert_eq!(f["person"].status, FillStatus::NotFound);
    }

    fn held(result: &FillResult, id: &str) -> Option<Hold> {
        let field = &result.fields[id];
        assert_eq!(field.status, FillStatus::Ambiguous, "{id}");
        assert!(field.value.is_none() && field.evidence.is_empty(), "{id}");
        field.candidates[0].hold
    }

    #[test]
    fn only_delimiter_bound_single_values_commit() {
        let fragments = [
            frag("a", "請求日：", [60.0, 100.0, 100.0, 110.0]),
            frag("b", "2026/11/08", [200.0, 100.0, 250.0, 110.0]),
            frag("c", "支払期限", [60.0, 130.0, 100.0, 140.0]),
            frag("d", "2026/11/30", [200.0, 130.0, 250.0, 140.0]),
            frag("e", "住所：", [60.0, 160.0, 90.0, 170.0]),
            frag("f", "東京都架空区1-2-3", [200.0, 160.0, 290.0, 170.0]),
            frag("g", "テストビル5階", [200.0, 172.0, 270.0, 182.0]),
            frag("h", "備考：東京都架空区4-5-6", [60.0, 200.0, 185.0, 210.0]),
            frag("i", "テストビル6階", [90.0, 212.0, 160.0, 222.0]),
            frag("j", "注文番号", [60.0, 240.0, 100.0, 250.0]),
            frag("k", "ORD-1", [60.0, 254.0, 85.0, 264.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"issued","label":"請求日","type":"date"},
                {"id":"due","label":"支払期限","type":"date"},
                {"id":"address","label":"住所"},
                {"id":"note","label":"備考"},
                {"id":"order","label":"注文番号"}
            ]})),
        );
        assert_eq!(result.fields["issued"].value, Some(json!("2026-11-08")));
        // A right neighbour without a delimiter and a value below: geometric.
        assert_eq!(held(&result, "due"), Some(Hold::GeometricOnly));
        assert_eq!(held(&result, "order"), Some(Hold::GeometricOnly));
        // A delimiter-bound value over two lines, right or inline.
        assert_eq!(held(&result, "address"), Some(Hold::MultiLine));
        assert_eq!(held(&result, "note"), Some(Hold::MultiLine));
        assert_eq!(
            result.fields["note"].candidates[0].raw,
            "東京都架空区4-5-6\nテストビル6階"
        );
    }

    #[test]
    fn partial_labels_are_candidates_only() {
        // Before: each of these filled the form field from the partial label.
        let cases = [
            ("再発行日：2026/11/08", "発行日", "date"),
            ("未入金額：¥5,000", "入金額", "amount"),
            ("非課税対象額：¥2,000", "課税対象額", "amount"),
            // A tax-inclusive amount under a plain 金額 label.
            ("金額：¥11,000（税込）", "税抜金額", "amount"),
            ("前回請求額：¥8,800", "請求額", "amount"),
            ("合計数量：12", "合計", "amount"),
        ];
        for (text, label, kind) in cases {
            let fragments = [frag("a", text, [60.0, 100.0, 200.0, 110.0])];
            let result = fill(
                "doc",
                &fragments,
                &form(json!({"fields":[{"id":"x","label":label,"type":kind}]})),
            );
            assert_eq!(held(&result, "x"), Some(Hold::PartialLabel), "{text}");
        }
        // An exact label still wins over a partial one.
        let fragments = [
            frag("a", "再発行日：2026/11/08", [60.0, 100.0, 200.0, 110.0]),
            frag("b", "発行日：2026/11/01", [60.0, 124.0, 200.0, 134.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"x","label":"発行日","type":"date"}]})),
        );
        assert_eq!(result.fields["x"].value, Some(json!("2026-11-01")));
    }

    #[test]
    fn a_leading_honorific_is_ignored_in_labels() {
        assert_eq!(score("ご住所", "住所"), 1.0);
        assert_eq!(score("お名前", "名前"), 1.0);
        assert_eq!(score("御請求額", "ご請求額"), 1.0);
        // Only one leading honorific (here the rest is a partial match), and
        // never the whole label.
        let twice = label_score(&normalize_label("ごご住所"), &normalize_label("住所"));
        assert_eq!(twice.hold, Some(Hold::PartialLabel));
        assert_eq!(normalize_label("ご").stem, "ご");
        let fragments = [frag(
            "a",
            "ご住所：東京都架空区1-2-3",
            [60.0, 100.0, 200.0, 110.0],
        )];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"x","label":"住所"}]})),
        );
        assert_eq!(result.fields["x"].raw.as_deref(), Some("東京都架空区1-2-3"));
    }

    #[test]
    fn page_unit_notes_leave_amounts_unnormalized() {
        let page = |page: u32, text: &str, top: f64| SourceFragment {
            page,
            ..frag(
                &format!("{page}-{top}"),
                text,
                [60.0, top, 200.0, top + 10.0],
            )
        };
        let fragments = [
            page(1, "（単位：千円）", 80.0),
            page(1, "売上高：1,234", 100.0),
            page(2, "単位 万円", 80.0),
            page(2, "予算：56", 100.0),
            page(3, "経費：¥7,000", 100.0),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"sales","label":"売上高","type":"amount"},
                {"id":"sales_number","label":"売上高","type":"number"},
                {"id":"budget","label":"予算","type":"amount"},
                {"id":"cost","label":"経費","type":"amount"},
                {"id":"sales_text","label":"売上高"}
            ]})),
        );
        let f = &result.fields;
        for id in ["sales", "sales_number", "budget"] {
            assert_eq!(f[id].status, FillStatus::Invalid, "{id}");
        }
        // The note applies to its page only; text keeps the source text.
        assert_eq!(f["cost"].value, Some(json!(7000)));
        assert_eq!(f["sales_text"].raw.as_deref(), Some("1,234"));
        for text in ["（単位：千円）", "単位:百万円", "(単位 億円)"] {
            assert!(unit_note(&text.nfkc().collect::<String>()), "{text}");
            assert!(unit_note(text), "{text}");
        }
        for text in ["単位：円", "単位：個", "単価：1,000円"] {
            assert!(!unit_note(text), "{text}");
        }
    }

    #[test]
    fn competing_values_are_left_empty_with_candidates() {
        let result = fill(
            "doc",
            &invoice(),
            &form(
                json!({"fields":[{"id":"amount","label":"金額","type":"amount","hints":["小計","合計"]}]}),
            ),
        );
        let amount = &result.fields["amount"];
        assert_eq!(amount.status, FillStatus::Ambiguous);
        assert!(amount.value.is_none() && amount.evidence.is_empty());
        let values: Vec<_> = amount.candidates.iter().map(|c| c.value.clone()).collect();
        assert!(values.contains(&Some(json!(11000))) && values.contains(&Some(json!(12100))));
    }

    #[test]
    fn neighbouring_labels_are_not_values() {
        let fragments = [
            frag("a", "お届け先氏名", [60.0, 121.0, 114.0, 130.0]),
            frag("b", "お届け予定日", [300.0, 121.0, 354.0, 130.0]),
            frag("c", "佐藤 花子", [60.0, 135.0, 115.6, 148.0]),
            frag("d", "2026/12/01", [300.0, 135.0, 365.0, 148.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"name","label":"お届け先氏名"},
                {"id":"date","label":"お届け予定日","type":"date"}
            ]})),
        );
        // Changed (strict policy): values below their labels are candidates
        // only (before: filled). The candidates are the values, not the
        // neighbouring label.
        assert_eq!(held(&result, "name"), Some(Hold::GeometricOnly));
        assert_eq!(result.fields["name"].candidates[0].raw, "佐藤 花子");
        assert_eq!(held(&result, "date"), Some(Hold::GeometricOnly));
        assert_eq!(
            result.fields["date"].candidates[0].value,
            Some(json!("2026-12-01"))
        );
    }

    #[test]
    fn type_mismatch_is_invalid_with_raw_kept() {
        let fragments = [frag("a", "発行日：未定", [60.0, 119.0, 130.0, 130.0])];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"d","label":"発行日","type":"date"}]})),
        );
        let d = &result.fields["d"];
        assert_eq!(d.status, FillStatus::Invalid);
        assert_eq!(d.raw.as_deref(), Some("未定"));
        assert!(d.value.is_none());
    }

    #[test]
    fn no_usable_text_is_no_text() {
        let mut fragment = frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]);
        fragment.quality = Quality::Suspect;
        let result = fill(
            "doc",
            &[fragment],
            &form(json!({"fields":[{"id":"no","label":"請求番号"}]})),
        );
        assert_eq!(result.fields["no"].status, FillStatus::NoText);
    }

    fn score(a: &str, b: &str) -> f64 {
        label_score(&normalize_label(a), &normalize_label(b)).score
    }

    #[test]
    fn labels_normalize_width_space_and_qualifiers() {
        // Changed: bracketed text is kept as a qualifier instead of dropped.
        let tax = normalize_label("消費税（10%）");
        assert_eq!(
            (tax.base.as_str(), tax.qualifiers.clone()),
            ("消費税", vec!["10%".to_owned()])
        );
        assert_eq!(normalize_label("Invoice No:").base, "invoiceno");
        assert_eq!(normalize_label("請求　番号：").base, "請求番号");
        assert_eq!(normalize_label("【住所】").base, "住所");
        // Changed: a leading honorific is ignored (before: 0.8, a partial match).
        assert_eq!(score("請求金額", "ご請求金額"), 1.0);
        // Below the partial-match threshold scores as no match.
        assert_eq!(score("金額", "ご請求金額"), 0.0);
    }

    #[test]
    fn differing_bracketed_qualifiers_do_not_match() {
        // Before: qualifiers were dropped, so both pairs below scored 1.0.
        assert_eq!(score("金額（税込）", "金額（税抜）"), 0.0);
        // 2 / (2 + 3) characters: below the partial-match threshold.
        assert!(score("住所", "(変更前) 住所") < MIN_PARTIAL);
        assert_eq!(score("金額（税込）", "金額(税込)"), 1.0);
        // A qualifier on one side only matches below an exact match.
        assert_eq!(score("消費税", "消費税（10%）"), 0.5);
        assert_eq!(score("金額（税込）", "金額"), 0.5);
        let fragments = [
            frag("a", "金額（税抜）", [60.0, 100.0, 120.0, 110.0]),
            frag("b", "¥10,000", [200.0, 100.0, 240.0, 110.0]),
            frag("c", "(変更前) 住所", [60.0, 130.0, 130.0, 140.0]),
            frag("d", "東京都架空区1-2-3", [200.0, 130.0, 300.0, 140.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"gross","label":"金額（税込）","type":"amount"},
                {"id":"address","label":"住所"}
            ]})),
        );
        assert_eq!(result.fields["gross"].status, FillStatus::NotFound);
        assert_eq!(result.fields["address"].status, FillStatus::NotFound);
        // The matching qualifier wins over a one-sided one.
        let fragments = [
            frag("a", "金額（税抜）：", [60.0, 100.0, 130.0, 110.0]),
            frag("b", "¥10,000", [200.0, 100.0, 240.0, 110.0]),
            frag("c", "金額（税込）：", [60.0, 130.0, 130.0, 140.0]),
            frag("d", "¥11,000", [200.0, 130.0, 240.0, 140.0]),
            frag("e", "金額：", [60.0, 160.0, 90.0, 170.0]),
            frag("f", "¥99", [200.0, 160.0, 220.0, 170.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"gross","label":"金額（税込）","type":"amount"}]})),
        );
        assert_eq!(result.fields["gross"].value, Some(json!(11000)));
    }

    #[test]
    fn values_normalize_deterministically() {
        assert_eq!(parse_date("2026/12/01"), Some("2026-12-01".into()));
        assert_eq!(parse_date("2026-2-30"), None);
        // Changed: era dates are supported (see era_dates_convert_within_era_bounds).
        assert_eq!(parse_date("令和8年11月8日"), Some("2026-11-08".into()));
        let amount = |s| normalize_value(FieldType::Amount, s);
        assert_eq!(amount("¥12,100（税込）"), Some(json!(12100)));
        assert_eq!(amount("１２，１００円"), Some(json!(12100)));
        assert_eq!(amount("2 名 3 回"), None);
        assert_eq!(amount("消費税(10%)"), None);
        assert_eq!(normalize_value(FieldType::Number, "第3期"), None);
        assert_eq!(
            normalize_value(FieldType::Number, "3.5 kg"),
            Some(json!(3.5))
        );
    }

    #[test]
    fn era_dates_convert_within_era_bounds() {
        let date = |s: &str| normalize_value(FieldType::Date, s);
        for (text, iso) in [
            ("令和8年11月8日", "2026-11-08"),
            ("令和８年１１月８日", "2026-11-08"),
            ("令和 8年 11月 8日（日）", "2026-11-08"),
            ("㋿8年11月8日", "2026-11-08"),
            ("令和元年5月1日", "2019-05-01"),
            ("令和1年5月1日", "2019-05-01"),
            ("令和6年2月29日", "2024-02-29"),
            ("平成31年4月30日", "2019-04-30"),
            ("㍻元年1月8日", "1989-01-08"),
            ("昭和64年1月7日", "1989-01-07"),
            ("昭和元年12月25日", "1926-12-25"),
            ("大正15年12月24日", "1926-12-24"),
            ("大正元年7月30日", "1912-07-30"),
            ("R8.11.8", "2026-11-08"),
            ("Ｒ０８．１１．０８", "2026-11-08"),
            ("H31.4.30", "2019-04-30"),
            ("H31/4/30", "2019-04-30"),
            ("S55.3.1", "1980-03-01"),
            // Changed: an abbreviation may carry its own weekday in brackets.
            ("R8.11.8(日)", "2026-11-08"),
        ] {
            assert_eq!(date(text), Some(json!(iso)), "{text}");
        }
        for text in [
            // Outside the era: each boundary is checked on both sides.
            "令和元年4月30日",
            "平成31年5月1日",
            "平成元年1月7日",
            "昭和64年1月8日",
            "昭和元年12月24日",
            "大正15年12月25日",
            "大正元年7月29日",
            "H31.5.1",
            "R1.4.30",
            "S1.2.3",
            // Not a calendar date, unsupported or not a single date.
            "令和0年1月1日",
            "令和8年2月29日",
            "令和8年13月1日",
            "明治45年7月29日",
            "令和8年11月",
            "令和100年1月1日",
            "令和8年11月8日〜令和8年12月8日",
            "発行日 令和8年11月8日",
            // Mixed notations are not digit-scanned.
            "2026年(令和8年)11月8日",
            "2026年11月8日(令和8年)",
            // Abbreviations need the whole text, one separator kind and a known letter.
            "R8.11/8",
            "R8.11.8(月)",
            "R8.11.8 日",
            "T2.3.4",
            "r8.11.8",
            "R123.1.1",
        ] {
            assert_eq!(date(text), None, "{text}");
        }
    }

    #[test]
    fn dates_must_be_one_whole_date() {
        let date = |s: &str| normalize_value(FieldType::Date, s);
        for (text, iso) in [
            ("2026年11月8日", "2026-11-08"),
            ("2026 年 11 月 8 日", "2026-11-08"),
            ("2026年11月8日(日)", "2026-11-08"),
            ("2026/11/08（日）", "2026-11-08"),
            ("2026.11.08 (Sun)", "2026-11-08"),
            ("2026.11.08 (sun)", "2026-11-08"),
            ("令和8年11月8日（日曜日）", "2026-11-08"),
            ("2026/11/09(月曜)", "2026-11-09"),
            ("2024年2月29日(木)", "2024-02-29"),
            ("2000/1/1 (土)", "2000-01-01"),
        ] {
            assert_eq!(date(text), Some(json!(iso)), "{text}");
        }
        // Before: each of these returned the first date (2026-11-08).
        for text in [
            "2026年11月8日〜2026年12月8日",
            "2026/11/08 - 2026/12/08",
            "2026/11/08〜",
            "令和8年11月8日以降",
            "令和8年11月8日まで有効",
            "2026年11月8日以前",
            "2026年11月8日から",
            "2026年11月8日頃",
            "R2026.11.8",
            "2026/11-08",
            "2026年11月8",
            "26/11/08",
            "2026/11/08 10:00",
            // Changed: a weekday must be bracketed and must match the date.
            "2026-11-08 日曜日",
            "2026年11月8日(月)",
            "2026/11/08 (Mon)",
            "2026/11/08（日）（日）",
            "2026/11/08(日",
        ] {
            assert_eq!(date(text), None, "{text}");
        }
        let fragments = [frag(
            "a",
            "有効期限：令和8年11月8日まで有効",
            [60.0, 100.0, 260.0, 110.0],
        )];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"d","label":"有効期限","type":"date"}]})),
        );
        assert_eq!(result.fields["d"].status, FillStatus::Invalid);
    }

    #[test]
    fn amounts_must_be_one_whole_amount() {
        let amount = |s: &str| normalize_value(FieldType::Amount, s);
        for (text, value) in [
            ("12,100", json!(12100)),
            ("¥12,100", json!(12100)),
            ("￥ 12,100", json!(12100)),
            ("12,100円", json!(12100)),
            ("12,100 円", json!(12100)),
            ("１２，１００円", json!(12100)),
            ("$12.50", json!(12.5)),
            ("€0.5", json!(0.5)),
            ("¥12,100（税込）", json!(12100)),
            ("¥12,100 (税抜)", json!(12100)),
            ("12,100円税込", json!(12100)),
            ("-¥1,000", json!(-1000)),
            ("¥-1,000", json!(-1000)),
            // Changed: these signs were refused; they are negative amounts.
            ("△¥1,000", json!(-1000)),
            ("▲1,000円", json!(-1000)),
            ("−1,000円", json!(-1000)),
            ("¥0", json!(0)),
            ("0.5円", json!(0.5)),
        ] {
            assert_eq!(amount(text), Some(value), "{text}");
        }
        for text in [
            // Before (first round): the currency-attached number (1100, 12100,
            // 1100, 12100). A rate or count next to an amount is not one amount.
            "10% ¥1,100",
            "3点 ¥12,100",
            "¥1,100（10%対象）",
            "¥12,100円",
            // Before: 12 and 1 (a number split over runs or by a space).
            "¥12, 100",
            "¥1 100",
            // Before: 2000, 5000, 5000, 50000.
            "1,000〜2,000円",
            "5,000円以上",
            "約5,000円",
            "50,000円まで",
            "¥1,000 + ¥100",
            "10% 1,100",
            "1.5万円",
            "¥1,00",
            "¥1,000.123",
            "€1.000",
            "--1,000",
            "-(¥1,000)",
            "△-1,000",
            "¥",
            "1,000円（税込）（税抜）",
            "1,000円(10%税込)",
            "1234567890123456",
            // Changed: parentheses are not a negative sign in Japanese
            // documents (before: -1000 each).
            "(¥1,000)",
            "（1,000円）（税込）",
            "（1,000円）",
            // Leading zeros and a first group with a leading zero.
            "¥007",
            "0,123円",
            "¥000,000",
            "00.5円",
        ] {
            assert_eq!(amount(text), None, "{text}");
        }
        let number = |s: &str| normalize_value(FieldType::Number, s);
        for (text, value) in [
            ("12", json!(12)),
            ("12 個", json!(12)),
            ("3.5 kg", json!(3.5)),
            ("50 m²", json!(50)),
            ("50㎡", json!(50)),
            ("-3", json!(-3)),
            ("+3", json!(3)),
            ("1,234.5678", json!(1234.5678)),
            ("0", json!(0)),
            ("0.125", json!(0.125)),
            ("1234.567", json!(1234.567)),
            ("1,234.567", json!(1234.567)),
        ] {
            assert_eq!(number(text), Some(value), "{text}");
        }
        for text in [
            "3点 ¥12,100",
            "10²",
            "3個 (1個あたり500円)",
            "12,100円",
            "10%",
            "2 名 3 回",
            "1〜3個",
            "約3個",
            "3個以上",
            "0012",
            "0,123",
            "000,000",
            // European grouping or three decimals: cannot be told apart.
            "1.000",
            "12.500 kg",
            "-1.000",
        ] {
            assert_eq!(number(text), None, "{text}");
        }
        let fragments = [
            frag("a", "消費税：10% ¥1,100", [60.0, 100.0, 180.0, 110.0]),
            frag("b", "合計：3点 ¥12,100", [60.0, 120.0, 180.0, 130.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"tax","label":"消費税","type":"amount"},
                {"id":"total","label":"合計","type":"amount"},
                {"id":"count","label":"合計","type":"number"}
            ]})),
        );
        // Changed: these were filled with 1100 and 12100.
        for id in ["tax", "total", "count"] {
            assert_eq!(result.fields[id].status, FillStatus::Invalid, "{id}");
        }
    }

    #[test]
    fn scale_units_in_the_label_leave_numbers_unnormalized() {
        for label in [
            "売上高（千円）",
            "金額(万円)",
            "売上（単位：百万円）",
            "売上高千円",
        ] {
            assert!(states_scale(&normalize_label(label)), "{label}");
        }
        for label in ["売上高", "消費税（10%）", "金額（税込）"] {
            assert!(!states_scale(&normalize_label(label)), "{label}");
        }
        let fragments = [
            frag("a", "売上高（千円）：1,234", [60.0, 100.0, 180.0, 110.0]),
            frag("b", "金額（万円）：56", [60.0, 124.0, 160.0, 134.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"sales","label":"売上高（千円）","type":"amount"},
                {"id":"sales_number","label":"売上高（千円）","type":"number"},
                {"id":"sales_plain","label":"売上高","type":"amount"},
                {"id":"amount","label":"金額","type":"amount"},
                {"id":"sales_text","label":"売上高（千円）"}
            ]})),
        );
        let f = &result.fields;
        // Before: 1234 and 56.
        assert_eq!(f["sales"].status, FillStatus::Invalid);
        assert_eq!(f["sales"].raw.as_deref(), Some("1,234"));
        assert_eq!(f["sales_number"].status, FillStatus::Invalid);
        // Without the qualifier in the form label the pair is held as well.
        assert_eq!(f["sales_plain"].status, FillStatus::Ambiguous);
        assert_eq!(f["amount"].status, FillStatus::Ambiguous);
        // Text fields keep the exact source text.
        assert_eq!(f["sales_text"].raw.as_deref(), Some("1,234"));
    }

    #[test]
    fn one_sided_qualifiers_are_candidates_only() {
        let fragments = [
            frag("a", "消費税（10%）：", [330.0, 100.0, 402.0, 110.0]),
            frag("b", "¥1,000", [480.0, 100.0, 520.0, 110.0]),
            frag("c", "消費税（8%）：", [330.0, 120.0, 397.0, 130.0]),
            frag("d", "¥160", [490.0, 120.0, 520.0, 130.0]),
            frag(
                "e",
                "住所（旧）：大阪府架空市4-5-6",
                [60.0, 160.0, 230.0, 170.0],
            ),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"tax","label":"消費税","type":"amount"},
                {"id":"tax_10","label":"消費税","type":"amount","hints":["消費税（10%）"]},
                {"id":"address","label":"住所"}
            ]})),
        );
        // Before: tax = 160 (the shorter qualifier scored higher).
        let tax = &result.fields["tax"];
        assert_eq!(tax.status, FillStatus::Ambiguous);
        assert!(
            tax.candidates.len() == 2
                && tax
                    .candidates
                    .iter()
                    .all(|c| c.hold == Some(Hold::OneSidedQualifier))
        );
        // An equal qualifier (here from a hint) matches exactly and fills.
        assert_eq!(result.fields["tax_10"].value, Some(json!(1000)));
        // Before: address = 大阪府架空市4-5-6.
        let address = &result.fields["address"];
        assert_eq!(address.status, FillStatus::Ambiguous);
        assert_eq!(address.candidates[0].raw, "大阪府架空市4-5-6");
        let held = serde_json::to_value(&address.candidates[0]).unwrap();
        assert_eq!(held["hold"], json!("one_sided_qualifier"));
    }

    #[test]
    fn only_the_best_candidates_decide() {
        // The exact label says 未定; a partial match (発行日 in 再発行日) has a
        // date. Before: the best *typed* candidate filled 2026-11-08.
        let fragments = [
            frag("a", "発行日：未定", [60.0, 100.0, 130.0, 110.0]),
            frag("b", "再発行日：2026/11/08", [60.0, 124.0, 170.0, 134.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"d","label":"発行日","type":"date"}]})),
        );
        assert_eq!(result.fields["d"].status, FillStatus::Invalid);
        assert_eq!(result.fields["d"].raw.as_deref(), Some("未定"));
        // Two exact labels, one valid and one not: ambiguous, not filled.
        let fragments = [
            frag("a", "発行日：未定", [60.0, 100.0, 130.0, 110.0]),
            frag("b", "発行日：2026/11/08", [60.0, 124.0, 160.0, 134.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[{"id":"d","label":"発行日","type":"date"}]})),
        );
        assert_eq!(result.fields["d"].status, FillStatus::Ambiguous);
    }

    #[test]
    fn several_values_in_one_text_value_are_held() {
        let fragments = [
            frag(
                "a",
                "TEL：03-1234-5678 FAX 03-1234-5679",
                [60.0, 100.0, 230.0, 110.0],
            ),
            frag(
                "b",
                "期間：2026年11月8日〜2026年12月8日",
                [60.0, 124.0, 240.0, 134.0],
            ),
            frag(
                "c",
                "日時：2026/11/08から2026/12/08まで",
                [60.0, 148.0, 240.0, 158.0],
            ),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"phone","label":"電話番号","hints":["TEL"]},
                {"id":"period","label":"期間"},
                {"id":"when","label":"日時"}
            ]})),
        );
        // Before: phone = "03-1234-5678 FAX 03-1234-5679".
        let phone = &result.fields["phone"];
        assert_eq!(phone.status, FillStatus::Ambiguous);
        assert_eq!(phone.candidates[0].hold, Some(Hold::SeveralValues));
        // A range joined by punctuation only is one value.
        assert_eq!(
            result.fields["period"].raw.as_deref(),
            Some("2026年11月8日〜2026年12月8日")
        );
        assert_eq!(result.fields["when"].status, FillStatus::Ambiguous);
        for (text, several) in [
            ("03-1234-5678 FAX 03-1234-5679", true),
            // Changed: two phone numbers are two values, whatever joins them.
            ("03-1234-5678 / 03-1234-5679", true),
            ("0120-123-456 または 03-1234-5678", true),
            ("令和8年11月8日 及び 令和8年12月1日", true),
            ("¥1,000（税込） 送料 ¥500", true),
            ("1,000円 と 500円", true),
            ("2026年11月8日〜2026年12月8日", false),
            ("東京都渋谷区架空1-2-3 101号室", false),
            ("〒150-0001 東京都", false),
            ("架空銀行 本店営業部 普通 1234567", false),
            // Changed: a date after a word that follows other text is a
            // second label with its value.
            ("INV-2026-0042 発行 2026/11/08", true),
            // Before: false (phones without hyphens or with bracketed area
            // codes were not recognized; `、` and `/` were not separators).
            ("0312345678 FAX 0312345679", true),
            ("(03)1234-5678 FAX (03)1234-5679", true),
            ("03(1234)5678 03(1234)5679", true),
            ("03-1234-5678、03-1234-5679", true),
            ("03-1234-5678/03-1234-5679", true),
            ("2026年11月8日、2026年12月1日", true),
            ("¥1,000/¥2,000", true),
            ("山田 太郎 生年月日 1990年1月1日", true),
            ("東京都架空区1-2-3 TEL 03-1234-5678", true),
            // One value: a range, a word at the start, a single number.
            ("¥1,000〜¥2,000", false),
            ("1,000円〜2,000円", false),
            ("2026/11/08 - 2026/12/08", false),
            ("TEL 03-1234-5678", false),
            ("(03)1234-5678", false),
            ("0312345678", false),
            ("お支払いは11月30日まで", false),
            ("1234567890", false),
        ] {
            assert_eq!(several_values(text), several, "{text}");
        }
    }

    #[test]
    fn blank_value_cell_does_not_take_the_next_rows_label() {
        // Label column x=60, value column x=200, rows 18 pt apart, 10 pt font.
        let fragments = [
            frag("a", "会社名", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "株式会社サンプル", [200.0, 100.0, 280.0, 110.0]),
            frag("c", "部署", [60.0, 118.0, 80.0, 128.0]),
            frag("d", "役職", [60.0, 136.0, 80.0, 146.0]),
            frag("e", "課長", [200.0, 136.0, 220.0, 146.0]),
            frag("f", "担当者", [60.0, 154.0, 90.0, 164.0]),
            frag("g", "佐藤 花子", [200.0, 154.0, 250.0, 164.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"company","label":"会社名"},
                {"id":"department","label":"部署"},
                {"id":"person","label":"担当者"}
            ]})),
        );
        // Before: department = 役職.
        assert_eq!(result.fields["department"].status, FillStatus::NotFound);
        // Changed (strict policy): right values without a delimiter are
        // candidates only (before: filled).
        for (id, value) in [("company", "株式会社サンプル"), ("person", "佐藤 花子")] {
            assert_eq!(held(&result, id), Some(Hold::GeometricOnly));
            assert_eq!(result.fields[id].candidates[0].raw, value);
        }
    }

    #[test]
    fn multi_row_table_columns_stay_ambiguous() {
        let mut fragments = vec![
            frag("h1", "品名", [60.0, 100.0, 80.0, 110.0]),
            frag("h2", "数量", [300.0, 100.0, 320.0, 110.0]),
            frag("h3", "金額", [440.0, 100.0, 460.0, 110.0]),
        ];
        for (row, (item, quantity, amount)) in [
            ("りんご", "3", "¥300"),
            ("みかん", "3", "¥500"),
            ("ぶどう", "3", "¥800"),
        ]
        .into_iter()
        .enumerate()
        {
            let top = 118.0 + 18.0 * row as f64;
            fragments.extend([
                frag(&format!("i{row}"), item, [60.0, top, 90.0, top + 10.0]),
                frag(
                    &format!("q{row}"),
                    quantity,
                    [300.0, top, 305.0, top + 10.0],
                ),
                frag(&format!("a{row}"), amount, [440.0, top, 460.0, top + 10.0]),
            ]);
        }
        let table = form(json!({"fields":[
            {"id":"item","label":"品名"},
            {"id":"quantity","label":"数量","type":"number"},
            {"id":"amount","label":"金額","type":"amount"}
        ]}));
        let result = fill("doc", &fragments, &table);
        // Before: each field was filled from the first row.
        for id in ["item", "quantity", "amount"] {
            let field = &result.fields[id];
            assert_eq!(field.status, FillStatus::Ambiguous, "{id}");
            assert!(field.value.is_none() && field.evidence.is_empty(), "{id}");
            assert!(field.candidates.len() >= 2, "{id}");
        }
        // Equal values in every row (数量 3, 3, 3) are still not one value.
        assert!(
            result.fields["quantity"]
                .candidates
                .iter()
                .all(|c| c.value == Some(json!(3)) && c.placement == Placement::Column)
        );
        assert!(
            result.fields["amount"]
                .candidates
                .iter()
                .all(|c| c.hold == Some(Hold::GeometricOnly))
        );
        // Changed (strict policy): one data row under the header is a value
        // below its label, a candidate only (before: filled).
        let result = fill("doc", &fragments[..6], &table);
        for (id, raw) in [("item", "りんご"), ("quantity", "3"), ("amount", "¥300")] {
            assert_eq!(held(&result, id), Some(Hold::GeometricOnly));
            let candidate = &result.fields[id].candidates[0];
            assert_eq!(
                (candidate.raw.as_str(), candidate.placement),
                (raw, Placement::Below)
            );
        }
    }

    #[test]
    fn label_grid_with_a_subset_form_ignores_label_columns() {
        // A two-column label/value grid, rows 18 pt apart; the form lists only
        // some of its labels. The column below 担当者 starts with the form
        // label 電話番号, so it is a column of labels, not data rows.
        let rows = [
            ("会社名", "株式会社サンプル商事"),
            ("部署", "経理部"),
            ("担当者", "佐藤 花子"),
            ("電話番号", "03-5555-0100"),
            ("メール", "keiri@example.com"),
        ];
        let fragments: Vec<SourceFragment> = rows
            .iter()
            .enumerate()
            .flat_map(|(row, (label, value))| {
                let top = 100.0 + 18.0 * row as f64;
                [
                    frag(&format!("l{row}"), label, [60.0, top, 100.0, top + 10.0]),
                    frag(&format!("v{row}"), value, [180.0, top, 280.0, top + 10.0]),
                ]
            })
            .collect();
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"company","label":"会社名"},
                {"id":"person","label":"担当者"},
                {"id":"phone","label":"電話番号"}
            ]})),
        );
        // Changed (strict policy): the right value is a candidate only
        // (before: filled). The label column below 担当者 adds no candidates.
        let person = &result.fields["person"];
        assert_eq!(held(&result, "person"), Some(Hold::GeometricOnly));
        assert_eq!(person.candidates.len(), 1);
        assert_eq!(person.candidates[0].raw, "佐藤 花子");
        assert_eq!(person.candidates[0].placement, Placement::Right);
        // Rows below that are not form labels still compete: left empty.
        assert_eq!(result.fields["company"].status, FillStatus::Ambiguous);
        assert_eq!(result.fields["phone"].status, FillStatus::Ambiguous);
    }

    #[test]
    fn inline_value_does_not_run_into_the_next_label() {
        let fragments = [frag(
            "a",
            "TEL：03-1234-5678 FAX：03-1234-5679",
            [60.0, 100.0, 260.0, 110.0],
        )];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"phone","label":"電話番号","hints":["TEL"]},
                {"id":"fax","label":"FAX"}
            ]})),
        );
        // Before: phone = "03-1234-5678 FAX：03-1234-5679". The fragment is
        // left unsplit, so both fields stay empty.
        assert_eq!(result.fields["phone"].status, FillStatus::NotFound);
        assert_eq!(result.fields["fax"].status, FillStatus::NotFound);
    }

    #[test]
    fn right_value_that_is_a_labelled_pair_is_not_a_value() {
        let fragments = [
            frag("a", "担当者", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "部署：経理部", [200.0, 100.0, 260.0, 110.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"person","label":"担当者"},
                {"id":"department","label":"部署"}
            ]})),
        );
        // Before: person = "部署：経理部".
        assert_eq!(result.fields["person"].status, FillStatus::NotFound);
        assert_eq!(result.fields["department"].raw.as_deref(), Some("経理部"));
    }

    #[test]
    fn stacked_value_lines_end_before_the_next_form_label() {
        // A vertical form: every line in one column, no delimiters.
        let fragments = [
            frag("a", "住所", [60.0, 100.0, 82.0, 110.0]),
            frag("b", "東京都架空区1-2-3", [60.0, 112.0, 160.0, 122.0]),
            frag("c", "テストビル5階", [60.0, 124.0, 132.0, 134.0]),
            frag("d", "電話番号", [60.0, 136.0, 104.0, 146.0]),
            frag("e", "03-1234-5678", [60.0, 148.0, 120.0, 158.0]),
        ];
        let result = fill(
            "doc",
            &fragments,
            &form(json!({"fields":[
                {"id":"address","label":"住所"},
                {"id":"phone","label":"電話番号"}
            ]})),
        );
        // Before (first round): address = the two lines before 電話番号.
        // Changed: lines stacked below a label cannot be told apart from a
        // single-column list (品名 / りんご / みかん), so they are column rows
        // and the field stays ambiguous. 電話番号 has a single value below.
        let address = &result.fields["address"];
        assert_eq!(address.status, FillStatus::Ambiguous);
        assert!(
            address
                .candidates
                .iter()
                .all(|c| c.placement == Placement::Column)
        );
        // Changed (strict policy): 電話番号 has a single value below, a
        // candidate only (before: filled).
        assert_eq!(held(&result, "phone"), Some(Hold::GeometricOnly));
        assert_eq!(result.fields["phone"].candidates[0].raw, "03-1234-5678");
    }

    /// `n` one-fragment lines stacked in one column with the same label text.
    fn stacked(n: usize) -> Vec<SourceFragment> {
        (0..n)
            .map(|i| {
                let top = 12.0 * i as f64;
                frag(&format!("f{i}"), "備考", [60.0, top, 82.0, top + 10.0])
            })
            .collect()
    }

    #[test]
    fn stacked_lines_produce_a_bounded_result() {
        // Before: n(n+1)/2 value spans and a response growing with n².
        let note = form(json!({"fields":[{"id":"note","label":"備考"}]}));
        let result =
            fill_with_limits("doc", &stacked(3000), &note, &FillLimits::default()).unwrap();
        let bytes = serde_json::to_vec(&result).unwrap().len();
        assert!(bytes < 20_000, "{bytes}");
        let note_field = &result.fields["note"];
        assert!(
            note_field
                .candidates
                .iter()
                .all(|c| c.evidence.len() <= layout::MAX_VALUE_LINES)
        );
    }

    #[test]
    fn limits_are_typed_errors() {
        let fragments = invoice();
        let fields = form(json!({"fields":[
            {"id":"no","label":"請求番号"},
            {"id":"total","label":"合計","type":"amount"}
        ]}));
        let run = |limits: FillLimits| fill_with_limits("doc", &fragments, &fields, &limits);
        let ok = run(FillLimits::default()).unwrap();
        assert_eq!(ok.fields["no"].status, FillStatus::Filled);
        let error = run(FillLimits {
            max_pairs: 2,
            ..FillLimits::default()
        })
        .unwrap_err();
        assert_eq!(error, FillError::LayoutLimitExceeded);
        assert_eq!(error.code(), "layout_limit_exceeded");
        let error = run(FillLimits {
            max_work: 10,
            ..FillLimits::default()
        })
        .unwrap_err();
        assert_eq!(error, FillError::BudgetExceeded);
        assert_eq!(error.code(), "fill_budget_exceeded");
        let error = run(FillLimits {
            deadline: Some(Instant::now()),
            ..FillLimits::default()
        })
        .unwrap_err();
        assert_eq!(error, FillError::DeadlineExceeded);
        assert_eq!(error.code(), "fill_deadline_exceeded");
        // Many fields x hints x pairs run out of budget instead of running long.
        let hints: Vec<String> = (0..16).map(|h| format!("項目別名{h:02}")).collect();
        let wide: Vec<serde_json::Value> = (0..512)
            .map(|f| json!({"id": format!("f{f}"), "label": format!("項目{f:03}"), "hints": hints}))
            .collect();
        let wide = form(json!({"fields": wide}));
        let rows: Vec<SourceFragment> = (0..4000)
            .flat_map(|i| {
                let top = 30.0 * i as f64;
                [
                    frag(
                        &format!("l{i}"),
                        &format!("ラベル{i:05}"),
                        [60.0, top, 120.0, top + 10.0],
                    ),
                    frag(
                        &format!("v{i}"),
                        &format!("値{i:05}"),
                        [200.0, top, 260.0, top + 10.0],
                    ),
                ]
            })
            .collect();
        let started = Instant::now();
        let error = fill_with_limits("doc", &rows, &wide, &FillLimits::default()).unwrap_err();
        assert_eq!(error, FillError::BudgetExceeded);
        assert!(started.elapsed().as_secs() < 30);
        // `fill` keeps its signature and reports the limit on every field.
        let result = fill("doc", &rows, &wide);
        assert!(result.evaluation_only);
        assert!(
            result
                .fields
                .values()
                .all(|f| f.status == FillStatus::LimitExceeded && f.value.is_none())
        );
    }

    #[test]
    fn a_large_value_matched_by_many_fields_stays_bounded() {
        // A 600 KB value under one label that all 512 fields match. Before:
        // every field re-normalized the value and the work counted one unit
        // per line, so a 100 ms deadline was never checked (5 s in release,
        // 40 s in debug, then Ok).
        let value = "あ".repeat(200_000);
        let fragments = [
            frag("l", "備考：", [60.0, 100.0, 90.0, 110.0]),
            frag("v", &value, [200.0, 100.0, 500.0, 110.0]),
        ];
        let fields: Vec<serde_json::Value> = (0..MAX_FIELDS)
            .map(|f| json!({"id": format!("f{f}"), "label": "備考"}))
            .collect();
        let many = form(json!({"fields": fields}));
        // Copies into the result count as work: the default budget stops it.
        let error = fill_with_limits("doc", &fragments, &many, &FillLimits::default()).unwrap_err();
        assert_eq!(error, FillError::BudgetExceeded);
        // With an unlimited budget the deadline stops it, checked per field.
        let started = Instant::now();
        let limits = FillLimits {
            max_work: u64::MAX,
            deadline: Some(started + std::time::Duration::from_millis(20)),
            ..FillLimits::default()
        };
        let error = fill_with_limits("doc", &fragments, &many, &limits).unwrap_err();
        assert_eq!(error, FillError::DeadlineExceeded);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        // One field is fine: the value is prepared once and copied once.
        let one = form(json!({"fields":[{"id":"note","label":"備考"}]}));
        let result = fill_with_limits("doc", &fragments, &one, &FillLimits::default()).unwrap();
        assert_eq!(result.fields["note"].raw.as_deref(), Some(value.as_str()));
    }

    #[test]
    fn crowded_layouts_stay_bounded_and_stop_at_the_deadline() {
        // 40,000 plain fragments over 40,000 `い：` fragments at one x.
        // Before: 1.6e9 fragment checks in layout, which never checked the
        // deadline (1.8 s in release before DeadlineExceeded at 100 ms).
        let fragments: Vec<SourceFragment> = (0..40_000)
            .flat_map(|i| {
                [
                    frag(&format!("a{i}"), "あ", [60.0, 100.0, 70.0, 110.0]),
                    frag(&format!("b{i}"), "い：", [60.0, 112.0, 75.0, 122.0]),
                ]
            })
            .collect();
        let target = form(json!({"fields":[{"id":"x","label":"あ"}]}));
        let started = Instant::now();
        let result = fill_with_limits("doc", &fragments, &target, &FillLimits::default()).unwrap();
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
        // The Right pairs あ → あ are not values: あ reads as the form label.
        assert_eq!(result.fields["x"].status, FillStatus::NotFound);
        let error = fill_with_limits(
            "doc",
            &fragments,
            &target,
            &FillLimits {
                max_work: 1_000_000,
                ..FillLimits::default()
            },
        )
        .unwrap_err();
        assert_eq!(error, FillError::BudgetExceeded);
    }

    #[test]
    fn form_validation_bounds_input() {
        assert_eq!(parse_form(b"{}").unwrap_err(), FormError::Invalid);
        assert_eq!(
            parse_form(br#"{"fields":[]}"#).unwrap_err(),
            FormError::FieldCount
        );
        assert_eq!(
            parse_form(br#"{"fields":[{"id":"a","label":"x"},{"id":"a","label":"y"}]}"#)
                .unwrap_err(),
            FormError::DuplicateId
        );
        assert_eq!(
            parse_form(br#"{"fields":[{"id":"a","label":" "}]}"#).unwrap_err(),
            FormError::FieldText
        );
        assert_eq!(
            parse_form(br#"{"fields":[{"id":"a","label":"x","extra":1}]}"#).unwrap_err(),
            FormError::Invalid
        );
    }
}
