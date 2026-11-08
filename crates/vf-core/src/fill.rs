//! Form fill (L3/L4): match layout pairs to form fields by label and type.
//!
//! Deterministic and offline. A field is filled only when exactly one value
//! is supported by the best label match; otherwise it stays empty with its
//! candidates ("a wrong value is worse than an empty one"). Values are exact
//! source text; `value` is a deterministic normalization of that text.
//! Results are evaluation-only and carry no calibrated probability.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;
use vf_source::SourceFragment;
use vf_source::geometry::Rect;
use vf_source::layout::{self, Pair, Placement, Precision};

pub const MAX_FORM_BYTES: usize = 1024 * 1024;
pub const MAX_FIELDS: usize = 512;
const MAX_TEXT_CHARS: usize = 200;
const MAX_HINTS: usize = 16;
/// Candidates reported for a field that was not filled.
const MAX_CANDIDATES: usize = 5;
/// Minimum label similarity for a partial (containment) match.
const MIN_PARTIAL: f64 = 0.5;

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

/// Match the document's layout pairs against every form field.
#[must_use]
pub fn fill(document_id: &str, fragments: &[SourceFragment], form: &Form) -> FillResult {
    let pairs = layout::analyze(fragments).pairs;
    let has_text = fragments
        .iter()
        .any(|f| f.quality == vf_source::Quality::Usable && !f.raw_text.trim().is_empty());
    let texts: BTreeMap<&str, &SourceFragment> =
        fragments.iter().map(|f| (f.id.as_str(), f)).collect();
    // A value that reads as one of the form's own labels is a neighbouring label, not a value.
    let labels: BTreeSet<String> = form
        .fields
        .iter()
        .flat_map(|f| std::iter::once(&f.label).chain(&f.hints))
        .map(|s| normalize_label(s))
        .collect();
    let fields = form
        .fields
        .iter()
        .map(|field| {
            let result = if has_text {
                fill_field(field, &pairs, &texts, &labels)
            } else {
                empty(FillStatus::NoText)
            };
            (field.id.clone(), result)
        })
        .collect();
    FillResult {
        evaluation_only: true,
        document_id: document_id.into(),
        fields,
    }
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

fn fill_field(
    field: &FormField,
    pairs: &[Pair],
    texts: &BTreeMap<&str, &SourceFragment>,
    labels: &BTreeSet<String>,
) -> FieldFill {
    let names: Vec<String> = std::iter::once(&field.label)
        .chain(&field.hints)
        .map(|s| normalize_label(s))
        .filter(|s| !s.is_empty())
        .collect();
    let mut candidates: Vec<FillCandidate> = pairs
        .iter()
        .filter_map(|pair| {
            let label = slice(texts, &pair.label)?;
            let score = names
                .iter()
                .map(|name| similarity(name, &normalize_label(&label)))
                .fold(0.0, f64::max);
            if score < MIN_PARTIAL {
                return None;
            }
            let evidence: Vec<FillEvidence> = pair
                .value
                .iter()
                .map(|span| {
                    let text = slice(texts, span)?;
                    Some(FillEvidence {
                        page: pair.page,
                        fragment_id: span.fragment_id.clone(),
                        start: span.start,
                        end: span.end,
                        text,
                        bbox: span.bbox,
                        precision: span.precision,
                    })
                })
                .collect::<Option<_>>()?;
            let raw = evidence
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if labels.contains(&normalize_label(&raw)) {
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
        })
        .collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.dedup_by(|a, b| a.evidence == b.evidence);

    // Typed fields only accept candidates whose text normalizes to that type.
    let typed: Vec<&FillCandidate> = candidates
        .iter()
        .filter(|c| field.field_type == FieldType::Text || c.value.is_some())
        .collect();
    let Some(best) = typed.first() else {
        if let Some(first) = candidates.first() {
            // A labelled value was found but it does not parse as the field type.
            let mut result = empty(FillStatus::Invalid);
            result.raw = Some(first.raw.clone());
            result.evidence = first.evidence.clone();
            result.candidates = candidates.into_iter().take(MAX_CANDIDATES).collect();
            return result;
        }
        return empty(FillStatus::NotFound);
    };
    let top: Vec<&&FillCandidate> = typed
        .iter()
        .filter(|c| (c.score - best.score).abs() < 1e-9)
        .collect();
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

fn slice(texts: &BTreeMap<&str, &SourceFragment>, span: &layout::Span) -> Option<String> {
    texts
        .get(span.fragment_id.as_str())
        .and_then(|f| f.raw_text.get(span.start..span.end))
        .map(str::to_owned)
}

/// NFKC, drop whitespace, trailing delimiters and bracketed qualifiers.
fn normalize_label(s: &str) -> String {
    let nfkc: String = s.nfkc().collect();
    let mut out = String::new();
    let mut depth = 0usize;
    for c in nfkc.chars() {
        match c {
            '(' | '[' | '「' | '【' | '〔' => depth += 1,
            ')' | ']' | '」' | '】' | '〕' => depth = depth.saturating_sub(1),
            _ if depth > 0 || c.is_whitespace() => {}
            _ => out.push(c),
        }
    }
    out.trim_end_matches([':', '：', '.', '。']).to_lowercase()
}

/// 1.0 for equal labels; for containment, the length ratio; otherwise 0.
fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    if long.contains(short) {
        short.chars().count() as f64 / long.chars().count() as f64
    } else {
        0.0
    }
}

/// Deterministic normalization; `None` when the text does not fit the type.
fn normalize_value(field_type: FieldType, raw: &str) -> Option<Value> {
    let text: String = raw.nfkc().collect();
    match field_type {
        FieldType::Text => None,
        FieldType::Date => parse_date(&text).map(Value::String),
        FieldType::Amount | FieldType::Number => parse_number(&text, field_type),
    }
}

/// `2026年11月8日`, `2026/11/08`, `2026-11-8`, `2026.11.08` → `2026-11-08`;
/// Japanese era dates via [`parse_era_name_date`] and [`parse_era_letter_date`].
fn parse_date(text: &str) -> Option<String> {
    let text = text.trim();
    if ERA_NAMES.iter().any(|name| text.contains(name)) {
        // Never digit-scan mixed notations such as `2026年(令和8年)11月8日`.
        return parse_era_name_date(text);
    }
    if let Some(date) = parse_era_letter_date(text) {
        return Some(date);
    }
    let digits: Vec<u32> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().ok())
        .collect::<Option<_>>()?;
    let [year, month, day, ..] = digits[..] else {
        return None;
    };
    let separators_ok = text.contains('年') && text.contains('月')
        || text.contains('/')
        || text.contains('-')
        || text.contains('.');
    separators_ok
        .then(|| calendar_date(year, month, day))
        .flatten()
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

/// Era names that take a text out of the Gregorian digit scan (明治 is unsupported).
const ERA_NAMES: [&str; 5] = ["令和", "平成", "昭和", "大正", "明治"];

/// `令和8年11月8日`, `令和元年5月1日`, `平成 31年 4月 30日`: the era name first,
/// then year, month and day; trailing text without digits (a weekday) is allowed.
fn parse_era_name_date(text: &str) -> Option<String> {
    let era = ERAS.iter().find(|era| text.starts_with(era.name))?;
    let rest = text[era.name.len()..].trim_start();
    let (year, rest) = match rest.strip_prefix('元') {
        Some(rest) => (1, rest),
        None => leading_number(rest)?,
    };
    let rest = rest.trim_start().strip_prefix('年')?;
    let (month, rest) = leading_number(rest.trim_start())?;
    let rest = rest.trim_start().strip_prefix('月')?;
    let (day, rest) = leading_number(rest.trim_start())?;
    let rest = rest.trim_start().strip_prefix('日')?;
    if rest.contains(|c: char| c.is_ascii_digit()) {
        return None;
    }
    era_date(era, year, month, day)
}

/// `R8.11.8`, `H31/4/30`, `S55.3.1`: the whole text, one separator kind.
fn parse_era_letter_date(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let letter = chars.next()?;
    let era = ERAS.iter().find(|era| era.letter == Some(letter))?;
    let (year, rest) = leading_number(chars.as_str())?;
    let separator = rest.chars().next().filter(|c| matches!(c, '.' | '/'))?;
    let (month, rest) = leading_number(rest.strip_prefix(separator)?)?;
    let (day, rest) = leading_number(rest.strip_prefix(separator)?)?;
    if !rest.is_empty() {
        return None;
    }
    era_date(era, year, month, day)
}

/// One or two leading ASCII digits and the rest of the text.
fn leading_number(text: &str) -> Option<(u32, &str)> {
    let len = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if !(1..=2).contains(&len) {
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

/// The first number in the text, allowing thousands separators and a sign.
/// Amounts additionally require a currency mark or exactly one number.
fn parse_number(text: &str, field_type: FieldType) -> Option<Value> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let next_digit = chars.peek().is_some_and(char::is_ascii_digit);
        if c.is_ascii_digit()
            || (c == '.' || c == ',') && !current.is_empty() && next_digit
            || c == '-' && current.is_empty() && next_digit
        {
            current.push(c);
        } else if !current.is_empty() {
            numbers.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        numbers.push(current);
    }
    let first = numbers.first()?;
    // Only whitespace, a sign or a currency mark may precede the number, so
    // labels such as `消費税(10%)` are not read as numbers.
    let lead = text.find(first.as_str())?;
    if !text[..lead]
        .chars()
        .all(|c| c.is_whitespace() || matches!(c, '¥' | '$' | '€' | '+' | '-'))
    {
        return None;
    }
    if field_type == FieldType::Amount
        && numbers.len() != 1
        && !text.contains(['¥', '$', '€', '円'])
    {
        return None;
    }
    let plain = first.replace(',', "");
    if let Ok(n) = plain.parse::<i64>() {
        return Some(Value::from(n));
    }
    plain
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .and_then(|n| serde_json::Number::from_f64(n).map(Value::Number))
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

    #[test]
    fn labels_normalize_width_space_and_qualifiers() {
        assert_eq!(normalize_label("消費税（10%）"), "消費税");
        assert_eq!(normalize_label("Invoice No:"), "invoiceno");
        assert_eq!(normalize_label("請求　番号："), "請求番号");
        assert_eq!(similarity("金額", "ご請求金額"), 0.4);
        assert_eq!(similarity("請求金額", "ご請求金額"), 0.8);
    }

    #[test]
    fn values_normalize_deterministically() {
        assert_eq!(parse_date("2026/12/01"), Some("2026-12-01".into()));
        assert_eq!(parse_date("2026-2-30"), None);
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
