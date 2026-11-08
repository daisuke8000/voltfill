//! Form fill (L3/L4): match layout pairs to form fields by label and type.
//!
//! Deterministic and offline. A field is filled only when exactly one value
//! is supported by the best label match; otherwise it stays empty with its
//! candidates ("a wrong value is worse than an empty one"). Values are exact
//! source text; `value` is a deterministic normalization of that text.
//! Results are evaluation-only and carry no calibrated probability.
//!
//! Conservative rules:
//! - Labels compare by base text and bracketed qualifiers: `金額（税抜）`
//!   never matches `金額（税込）`, and a qualifier on only one side lowers the
//!   score by the qualifier's share of the label (`(変更前) 住所` does not
//!   reach the partial-match threshold for `住所`; `消費税（10%）` does for
//!   `消費税`).
//! - A header above two or more data rows ([`Placement::Column`]) leaves the
//!   field ambiguous, even when the rows agree. A column whose first row
//!   reads as one of the form's labels is a label column of a label/value
//!   grid and is ignored.
//! - A multi-line value ends before a line that reads as one of the form's
//!   labels.
//! - Amounts use the number attached to a currency mark; dates must be one
//!   whole date (a trailing weekday is allowed), never a range or a date with
//!   a qualifier such as `以降`.
//!
//! Work is bounded by [`FillLimits`]: layout hypotheses per document, work
//! units (label comparisons and value spans examined) over all fields, and an
//! optional deadline. Exceeding a limit is a typed [`FillError`] from
//! [`fill_with_limits`], never a partial result.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;
use vf_source::SourceFragment;
use vf_source::geometry::Rect;
use vf_source::layout::{self, LayoutLimits, Pair, Placement, Precision};

pub const MAX_FORM_BYTES: usize = 1024 * 1024;
pub const MAX_FIELDS: usize = 512;
const MAX_TEXT_CHARS: usize = 200;
const MAX_HINTS: usize = 16;
/// Candidates reported for a field that was not filled.
const MAX_CANDIDATES: usize = 5;
/// Minimum label similarity for a partial (containment) match.
const MIN_PARTIAL: f64 = 0.5;
/// Default work units for one document over all fields: one per label
/// comparison and one per value span examined.
pub const DEFAULT_MAX_WORK: u64 = 20_000_000;
/// Work units between deadline checks.
const DEADLINE_CHECK_INTERVAL: u64 = 4096;

/// Resource limits for [`fill_with_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillLimits {
    /// Label/value hypotheses allowed for one document.
    pub max_pairs: usize,
    /// Work units allowed over all fields (see [`DEFAULT_MAX_WORK`]).
    pub max_work: u64,
    /// Optional wall-clock deadline, checked during matching.
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
    Ambiguous,
    NotFound,
    NoText,
    Invalid,
    /// Not evaluated because the document exceeded the default
    /// [`FillLimits`]. Only [`fill`] reports this; [`fill_with_limits`]
    /// returns a [`FillError`] instead.
    LimitExceeded,
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
/// `limits.max_pairs` hypotheses, [`FillError::BudgetExceeded`] when matching
/// needs more than `limits.max_work` units, and
/// [`FillError::DeadlineExceeded`] once `limits.deadline` has passed.
pub fn fill_with_limits(
    document_id: &str,
    fragments: &[SourceFragment],
    form: &Form,
    limits: &FillLimits,
) -> Result<FillResult, FillError> {
    let mut budget = Budget {
        remaining: limits.max_work,
        deadline: limits.deadline,
        until_check: DEADLINE_CHECK_INTERVAL,
    };
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
    let layout_limits = LayoutLimits {
        max_pairs: limits.max_pairs,
    };
    let pairs = layout::analyze(fragments, &layout_limits)
        .map_err(|_| FillError::LayoutLimitExceeded)?
        .pairs;
    budget.check_deadline()?;
    let texts: HashMap<&str, &SourceFragment> =
        fragments.iter().map(|f| (f.id.as_str(), f)).collect();
    // A value that reads as one of the form's own labels is a neighbouring label, not a value.
    let labels: BTreeSet<String> = form
        .fields
        .iter()
        .flat_map(|f| std::iter::once(&f.label).chain(&f.hints))
        .map(|s| normalize_label(s).base)
        .collect();
    budget.spend(pairs.len() as u64)?;
    let index = PairLabels::new(&pairs, &texts);
    let context = Context {
        pairs: &pairs,
        index: &index,
        texts: &texts,
        labels: &labels,
    };
    let mut fields = BTreeMap::new();
    for field in &form.fields {
        fields.insert(field.id.clone(), context.fill_field(field, &mut budget)?);
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

/// Remaining work units and the deadline.
struct Budget {
    remaining: u64,
    deadline: Option<Instant>,
    until_check: u64,
}

impl Budget {
    fn spend(&mut self, units: u64) -> Result<(), FillError> {
        self.remaining = self
            .remaining
            .checked_sub(units)
            .ok_or(FillError::BudgetExceeded)?;
        if units >= self.until_check {
            self.until_check = DEADLINE_CHECK_INTERVAL;
            self.check_deadline()
        } else {
            self.until_check -= units;
            Ok(())
        }
    }

    fn check_deadline(&self) -> Result<(), FillError> {
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => Err(FillError::DeadlineExceeded),
            _ => Ok(()),
        }
    }
}

/// Pair labels normalized once per document and grouped by normalized text,
/// so each field compares against every distinct label once.
struct PairLabels {
    groups: Vec<(Label, Vec<usize>)>,
    /// Each pair's label text; `None` when its span does not resolve.
    texts: Vec<Option<String>>,
}

impl PairLabels {
    fn new(pairs: &[Pair], texts: &HashMap<&str, &SourceFragment>) -> Self {
        let mut groups: Vec<(Label, Vec<usize>)> = Vec::new();
        let mut positions: HashMap<Label, usize> = HashMap::new();
        let mut label_texts = Vec::with_capacity(pairs.len());
        for (i, pair) in pairs.iter().enumerate() {
            let text = slice(texts, &pair.label);
            if let Some(text) = &text {
                let label = normalize_label(text);
                if let Some(&at) = positions.get(&label) {
                    groups[at].1.push(i);
                } else {
                    positions.insert(label.clone(), groups.len());
                    groups.push((label, vec![i]));
                }
            }
            label_texts.push(text);
        }
        Self {
            groups,
            texts: label_texts,
        }
    }
}

struct Context<'a> {
    pairs: &'a [Pair],
    index: &'a PairLabels,
    texts: &'a HashMap<&'a str, &'a SourceFragment>,
    labels: &'a BTreeSet<String>,
}

impl Context<'_> {
    fn fill_field(&self, field: &FormField, budget: &mut Budget) -> Result<FieldFill, FillError> {
        let names: Vec<Label> = std::iter::once(&field.label)
            .chain(&field.hints)
            .map(|s| normalize_label(s))
            .filter(|label| !label.base.is_empty())
            .collect();
        let mut matched: Vec<(usize, f64)> = Vec::new();
        for (label, members) in &self.index.groups {
            budget.spend(names.len() as u64)?;
            let score = names
                .iter()
                .map(|name| label_score(name, label))
                .fold(0.0, f64::max);
            if score >= MIN_PARTIAL {
                matched.extend(members.iter().map(|&pair| (pair, score)));
            }
        }
        // Document order first, so equal scores keep a deterministic order.
        matched.sort_unstable_by_key(|&(pair, _)| pair);
        let mut candidates = Vec::new();
        // The header of a column whose first row reads as a form label: that
        // column lists labels (a label/value grid), not data rows.
        let mut label_column: Option<&layout::Span> = None;
        for (index, score) in matched {
            let pair = &self.pairs[index];
            budget.spend(pair.value.len() as u64)?;
            if pair.placement == Placement::Column {
                if self.first_column_row(index) {
                    label_column = pair
                        .value
                        .first()
                        .is_some_and(|span| self.reads_as_label(span))
                        .then_some(&pair.label);
                } else if label_column == Some(&pair.label) {
                    continue;
                }
            }
            if let Some(candidate) = self.candidate(field, index, score) {
                candidates.push(candidate);
            }
        }
        Ok(decide(field, candidates))
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

    fn reads_as_label(&self, span: &layout::Span) -> bool {
        slice(self.texts, span)
            .is_some_and(|text| self.labels.contains(&normalize_label(&text).base))
    }

    fn candidate(&self, field: &FormField, index: usize, score: f64) -> Option<FillCandidate> {
        let pair = &self.pairs[index];
        let label = self.index.texts[index].clone()?;
        let mut evidence = Vec::with_capacity(pair.value.len());
        for (line, span) in pair.value.iter().enumerate() {
            let text = slice(self.texts, span)?;
            if self.labels.contains(&normalize_label(&text).base) {
                // A first line that reads as a form label is a neighbouring
                // label; a later one starts the next field's block.
                if line == 0 {
                    return None;
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
        if self.labels.contains(&normalize_label(&raw).base) {
            return None;
        }
        Some(FillCandidate {
            value: normalize_value(field.field_type, &raw),
            label,
            raw,
            score,
            placement: pair.placement,
            evidence,
        })
    }
}

/// Fill, leave ambiguous or report invalid from the label-matched candidates.
fn decide(field: &FormField, mut candidates: Vec<FillCandidate>) -> FieldFill {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.dedup_by(|a, b| a.evidence == b.evidence);
    let Some(first) = candidates.first() else {
        return empty(FillStatus::NotFound);
    };
    // Typed fields only accept candidates whose text normalizes to that type.
    let typed: Vec<&FillCandidate> = candidates
        .iter()
        .filter(|c| field.field_type == FieldType::Text || c.value.is_some())
        .collect();
    let best_score = typed.first().map_or(first.score, |c| c.score);
    let same_score = |c: &FillCandidate| (c.score - best_score).abs() < 1e-9;
    // A header over several data rows never determines one value, even
    // when the rows agree or only one row fits the type.
    let column_rows = candidates
        .iter()
        .filter(|c| c.placement == Placement::Column && same_score(c))
        .count();
    if column_rows >= 2 {
        let mut result = empty(FillStatus::Ambiguous);
        result.candidates = candidates.into_iter().take(MAX_CANDIDATES).collect();
        return result;
    }
    let Some(best) = typed.first() else {
        // A labelled value was found but it does not parse as the field type.
        let mut result = empty(FillStatus::Invalid);
        result.raw = Some(first.raw.clone());
        result.evidence = first.evidence.clone();
        result.candidates = candidates.into_iter().take(MAX_CANDIDATES).collect();
        return result;
    };
    let top: Vec<&&FillCandidate> = typed.iter().filter(|c| same_score(c)).collect();
    let agreed = top
        .iter()
        .all(|c| c.value == best.value && c.raw == best.raw)
        || field.field_type != FieldType::Text && top.iter().all(|c| c.value == best.value);
    if agreed {
        FieldFill {
            status: FillStatus::Filled,
            method: Some("label_match"),
            raw: Some(best.raw.clone()),
            value: best
                .value
                .clone()
                .or_else(|| Some(Value::String(best.raw.clone()))),
            evidence: best.evidence.clone(),
            candidates: Vec::new(),
        }
    } else {
        let mut result = empty(FillStatus::Ambiguous);
        result.candidates = typed.into_iter().take(MAX_CANDIDATES).cloned().collect();
        result
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
/// becomes qualifiers. A label that is only bracketed text keeps it as base.
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
    Label {
        base_chars: base.chars().count(),
        qualifier_chars: qualifiers.iter().map(|q| q.chars().count()).sum(),
        base,
        qualifiers,
    }
}

/// Base similarity, then qualifiers: equal qualifiers keep the score,
/// different ones on both sides never match, and a qualifier on one side
/// scales the score by that label's base share.
fn label_score(a: &Label, b: &Label) -> f64 {
    let (short, long) = if a.base_chars <= b.base_chars {
        (a, b)
    } else {
        (b, a)
    };
    // Containment scores short/long, so shorter ratios never reach MIN_PARTIAL.
    if short.base_chars == 0 || (short.base_chars as f64) < MIN_PARTIAL * long.base_chars as f64 {
        return 0.0;
    }
    let base = if a.base == b.base {
        1.0
    } else if long.base.contains(short.base.as_str()) {
        short.base_chars as f64 / long.base_chars as f64
    } else {
        return 0.0;
    };
    if a.qualifiers == b.qualifiers {
        return base;
    }
    match (a.qualifiers.is_empty(), b.qualifiers.is_empty()) {
        (true, _) => base * b.base_share(),
        (_, true) => base * a.base_share(),
        (false, false) => 0.0,
    }
}

/// Deterministic normalization; `None` when the text does not fit the type.
fn normalize_value(field_type: FieldType, raw: &str) -> Option<Value> {
    match field_type {
        FieldType::Text => None,
        FieldType::Date => parse_date(&raw.nfkc().collect::<String>()).map(Value::String),
        FieldType::Amount | FieldType::Number => parse_number(&numeric_text(raw)?, field_type),
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
/// [`parse_era_letter_date`]. Only a weekday may follow (`(日)`, `（月）`,
/// `日曜日`); ranges and qualifiers such as `以降` or `まで` are `None`.
fn parse_date(text: &str) -> Option<String> {
    let text = text.trim();
    if ERA_NAMES.iter().any(|name| text.contains(name)) {
        // Never digit-scan mixed notations such as `2026年(令和8年)11月8日`.
        return parse_era_name_date(text);
    }
    if let Some(date) = parse_era_letter_date(text) {
        return Some(date);
    }
    parse_gregorian_date(text)
}

/// `2026年11月8日` (spaces allowed between parts) or a four-digit year,
/// month and day joined by one separator kind (`/`, `-` or `.`).
fn parse_gregorian_date(text: &str) -> Option<String> {
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
    if !weekday_only(rest) {
        return None;
    }
    calendar_date(year, month, day)
}

/// Text after a date that keeps its meaning: nothing, or a weekday such as
/// `(日)`, `(日曜日)`, `日曜日` or `(Sun)`.
fn weekday_only(rest: &str) -> bool {
    const DAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
    const ENGLISH: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let rest = rest.trim();
    if rest.is_empty() {
        return true;
    }
    let (inner, bracketed) = match rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        Some(inner) => (inner.trim(), true),
        None => (rest, false),
    };
    let named = DAYS.iter().any(|day| {
        inner
            .strip_prefix(day)
            .is_some_and(|tail| tail == "曜" || tail == "曜日")
    });
    let short = DAYS.contains(&inner) || ENGLISH.contains(&inner.to_ascii_lowercase().as_str());
    named || bracketed && short
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
/// then year, month and day; only a weekday may follow.
fn parse_era_name_date(text: &str) -> Option<String> {
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
    if !weekday_only(rest) {
        return None;
    }
    era_date(era, year, month, day)
}

/// `R8.11.8`, `H31/4/30`, `S55.3.1`: the whole text, one separator kind.
fn parse_era_letter_date(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let letter = chars.next()?;
    let era = ERAS.iter().find(|era| era.letter == Some(letter))?;
    let (year, rest) = leading_digits(chars.as_str(), 1, 2)?;
    let separator = rest.chars().next().filter(|c| matches!(c, '.' | '/'))?;
    let (month, rest) = leading_digits(rest.strip_prefix(separator)?, 1, 2)?;
    let (day, rest) = leading_digits(rest.strip_prefix(separator)?, 1, 2)?;
    if !rest.is_empty() {
        return None;
    }
    era_date(era, year, month, day)
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
fn era_date(era: &Era, year: u32, month: u32, day: u32) -> Option<String> {
    if year == 0 {
        return None;
    }
    let date = (era.first.0 + year - 1, month, day);
    let in_era = date >= era.first && era.last.is_none_or(|last| date <= last);
    in_era.then(|| calendar_date(date.0, month, day)).flatten()
}

/// `YYYY-MM-DD` for a valid calendar date with a four-digit year.
fn calendar_date(year: u32, month: u32, day: u32) -> Option<String> {
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => return None,
    };
    ((1000..=9999).contains(&year) && (1..=days).contains(&day))
        .then(|| format!("{year:04}-{month:02}-{day:02}"))
}

const CURRENCY_MARKS: [char; 3] = ['¥', '$', '€'];

/// A number token: its byte range in the text.
struct Token {
    start: usize,
    end: usize,
}

/// Numbers with optional sign and thousands separators, in text order.
fn number_tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next_digit = chars.peek().is_some_and(|&(_, n)| n.is_ascii_digit());
        if c.is_ascii_digit()
            || (c == '.' || c == ',') && start.is_some() && next_digit
            || c == '-' && start.is_none() && next_digit
        {
            start.get_or_insert(at);
        } else if let Some(start) = start.take() {
            tokens.push(Token { start, end: at });
        }
    }
    if let Some(start) = start {
        tokens.push(Token {
            start,
            end: text.len(),
        });
    }
    tokens
}

/// The number of a token: thousands separators must group by three and a
/// fraction holds digits only.
fn token_number(token: &str) -> Option<Value> {
    let (sign, digits) = token
        .strip_prefix('-')
        .map_or(("", token), |digits| ("-", digits));
    let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if integer.contains(',') {
        let mut groups = integer.split(',');
        let head = groups.next().unwrap_or_default();
        if !(1..=3).contains(&head.len()) || !groups.all(|group| group.len() == 3) {
            return None;
        }
    }
    let integer = integer.replace(',', "");
    if !digits.contains('.')
        && let Ok(n) = format!("{sign}{integer}").parse::<i64>()
    {
        return Some(Value::from(n));
    }
    format!("{sign}{integer}.{fraction}")
        .trim_end_matches('.')
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .and_then(|n| serde_json::Number::from_f64(n).map(Value::Number))
}

/// Amounts take the number attached to a currency mark (`¥1,100`,
/// `1,100円`); several different attached numbers are refused. Without a
/// mark, and for `number` fields, the text must hold exactly one number with
/// only whitespace, a sign or a currency mark before it. A magnitude kanji
/// after a number (`1.5万円`) is refused.
fn parse_number(text: &str, field_type: FieldType) -> Option<Value> {
    let tokens = number_tokens(text);
    if tokens.iter().any(|t| {
        text[t.end..]
            .trim_start()
            .starts_with(['千', '万', '億', '兆'])
    }) {
        return None;
    }
    if field_type == FieldType::Amount {
        let mut attached = None;
        for token in &tokens {
            let Some(value) = currency_value(text, token) else {
                continue;
            };
            let value = value?;
            if attached.as_ref().is_some_and(|seen| *seen != value) {
                return None;
            }
            attached = Some(value);
        }
        if attached.is_some() {
            return attached;
        }
    }
    let [token] = &tokens[..] else {
        return None;
    };
    // Only whitespace, a sign or a currency mark may precede the number, so
    // labels such as `消費税(10%)` and ordinals such as `第3期` are refused.
    if !text[..token.start]
        .chars()
        .all(|c| c.is_whitespace() || CURRENCY_MARKS.contains(&c) || matches!(c, '+' | '-'))
    {
        return None;
    }
    token_number(&text[token.start..token.end])
}

/// `None` when `token` has no currency mark; otherwise its signed value,
/// `Some(None)` when the number or its sign is unclear (`(¥1,000)`, `△¥1,000`).
fn currency_value(text: &str, token: &Token) -> Option<Option<Value>> {
    let before = text[..token.start].trim_end();
    let suffixed = text[token.end..].trim_start().starts_with('円');
    let prefix = before.strip_suffix(CURRENCY_MARKS);
    if prefix.is_none() && !suffixed {
        return None;
    }
    let number = &text[token.start..token.end];
    let negative = match prefix.and_then(|p| p.trim_end().chars().next_back()) {
        Some('-') => true,
        Some('−' | '△' | '▲' | '(' | '[') => return Some(None),
        _ => false,
    };
    let value = token_number(number);
    Some(if negative {
        value.and_then(|v| negate(&v))
    } else {
        value
    })
}

fn negate(value: &Value) -> Option<Value> {
    if let Some(n) = value.as_i64() {
        return n.checked_neg().map(Value::from);
    }
    value
        .as_f64()
        .and_then(|n| serde_json::Number::from_f64(-n).map(Value::Number))
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
            frag("c", "小計", [330.0, 219.0, 352.0, 230.0]),
            frag("d", "¥11,000", [480.2, 219.0, 520.0, 230.0]),
            frag("e", "合計", [330.0, 259.0, 352.0, 270.0]),
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
        assert_eq!(f["total"].status, FillStatus::Filled);
        assert_eq!(f["total"].value, Some(json!(12100)));
        assert_eq!(f["total"].evidence[0].fragment_id, "f");
        assert_eq!(
            f["address"].raw.as_deref(),
            Some("東京都架空区サンプル町1-2-3\nテストビル5階")
        );
        assert_eq!(f["address"].evidence.len(), 2);
        assert_eq!(f["person"].status, FillStatus::NotFound);
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
        assert_eq!(result.fields["name"].status, FillStatus::Filled);
        assert_eq!(result.fields["name"].raw.as_deref(), Some("佐藤 花子"));
        assert_eq!(result.fields["date"].value, Some(json!("2026-12-01")));
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
        label_score(&normalize_label(a), &normalize_label(b))
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
        assert_eq!(score("請求金額", "ご請求金額"), 0.8);
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
            frag("a", "金額（税抜）", [60.0, 100.0, 120.0, 110.0]),
            frag("b", "¥10,000", [200.0, 100.0, 240.0, 110.0]),
            frag("c", "金額（税込）", [60.0, 130.0, 120.0, 140.0]),
            frag("d", "¥11,000", [200.0, 130.0, 240.0, 140.0]),
            frag("e", "金額", [60.0, 160.0, 80.0, 170.0]),
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
            "R8.11.8(日)",
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
            ("2026-11-08 日曜日", "2026-11-08"),
            ("2026.11.08 (Sun)", "2026-11-08"),
            ("令和8年11月8日（日曜日）", "2026-11-08"),
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
    fn amounts_use_the_currency_attached_number() {
        let amount = |s: &str| normalize_value(FieldType::Amount, s);
        let number = |s: &str| normalize_value(FieldType::Number, s);
        // Before: 10 and 3 (the first number).
        assert_eq!(amount("10% ¥1,100"), Some(json!(1100)));
        assert_eq!(amount("3点 ¥12,100"), Some(json!(12100)));
        assert_eq!(amount("¥1,100（10%対象）"), Some(json!(1100)));
        assert_eq!(amount("12,100 円"), Some(json!(12100)));
        assert_eq!(amount("¥12,100円"), Some(json!(12100)));
        assert_eq!(amount("-¥1,000"), Some(json!(-1000)));
        assert_eq!(amount("$12.50"), Some(json!(12.5)));
        assert_eq!(amount("12,100"), Some(json!(12100)));
        // Several different currency numbers, or several bare numbers: refused.
        assert_eq!(amount("¥1,000 + ¥100"), None);
        assert_eq!(amount("10% 1,100"), None);
        assert_eq!(amount("△¥1,000"), None);
        assert_eq!(amount("(¥1,000)"), None);
        assert_eq!(amount("1.5万円"), None);
        assert_eq!(amount("¥1,00"), None);
        // Number fields hold exactly one number.
        assert_eq!(number("3点 ¥12,100"), None);
        assert_eq!(number("12 個"), Some(json!(12)));
        assert_eq!(number("50 m²"), Some(json!(50)));
        assert_eq!(number("10²"), None);
        assert_eq!(number("3個 (1個あたり500円)"), None);
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
        assert_eq!(result.fields["tax"].value, Some(json!(1100)));
        assert_eq!(result.fields["total"].value, Some(json!(12100)));
        assert_eq!(result.fields["count"].status, FillStatus::Invalid);
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
        assert_eq!(
            result.fields["company"].raw.as_deref(),
            Some("株式会社サンプル")
        );
        assert_eq!(result.fields["person"].raw.as_deref(), Some("佐藤 花子"));
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
        // One data row under the header is filled as before.
        let result = fill("doc", &fragments[..6], &table);
        assert_eq!(result.fields["item"].raw.as_deref(), Some("りんご"));
        assert_eq!(result.fields["quantity"].value, Some(json!(3)));
        assert_eq!(result.fields["amount"].value, Some(json!(300)));
    }

    #[test]
    fn label_grid_with_a_subset_form_keeps_right_values() {
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
        assert_eq!(result.fields["person"].raw.as_deref(), Some("佐藤 花子"));
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
        // Before: address ran on over 電話番号 and the number.
        assert_eq!(
            result.fields["address"].raw.as_deref(),
            Some("東京都架空区1-2-3\nテストビル5階")
        );
        assert_eq!(result.fields["address"].evidence.len(), 2);
        assert_eq!(result.fields["phone"].raw.as_deref(), Some("03-1234-5678"));
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
