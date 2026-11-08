//! AcroForm field values as a fill source.
//!
//! A fillable PDF binds each value to a field name, so a terminal AcroForm
//! field read by [`vf_source::acroform`] can fill a form field directly. The
//! value is the PDF's own `/V`, never generated; the default value `/DV` is
//! never used. The commit policy is as strict as offline fill ("a wrong value
//! is worse than an empty one"): [`fill_with_acroform`] fills a form field
//! from an AcroForm field (method [`METHOD`]) only when every rule below
//! holds, and otherwise reports every failed rule as an [`AcroFormHold`] in
//! [`FieldFill::acroform`].
//!
//! # Commit rules
//!
//! - Label: the last partial name of the field's fully qualified name, or its
//!   alternate name `/TU`, equals the form label or one of its hints after the
//!   label normalization of offline fill (condition (a) of [`crate::fill`];
//!   partial labels and one-sided qualifiers never match). Exactly one
//!   AcroForm field may match ([`AcroFormHold::SeveralFields`]), and that
//!   field may match no other form field, whatever rules it fails there: a
//!   field whose name matches one form field and whose `/TU` another fills
//!   neither, even when its value does not fit the other's type
//!   ([`AcroFormHold::FieldMatchesSeveralItems`]).
//! - Document: no XFA ([`AcroFormHold::XfaPresent`]): in a hybrid form an
//!   XFA-aware viewer may show other data.
//! - Form type: table fields are never matched; an AcroForm field holds one
//!   value, so AcroForm fill applies to scalar fields only.
//! - Kind: a text field fills text, date, amount and number fields; a choice
//!   field fills text fields only, with its export value, and only when that
//!   equals the display text of its `/Opt` pair after NFKC with whitespace
//!   removed ([`AcroFormHold::ExportDiffersFromDisplay`]; the display text is
//!   reported in [`AcroFormMatch::display`]). Checkboxes and
//!   radio buttons fill nothing: the form contract has no boolean or choice
//!   type ([`AcroFormHold::KindUnsupported`]).
//! - Field: no [`FieldIssue`] other than one on `/DV`
//!   ([`AcroFormHold::FieldIssues`]). This covers values that could not be
//!   decoded exactly, password fields that store a value, partial names with a
//!   period and PDFDocEncoding values with bytes at or above `0x80`, which may
//!   be Shift_JIS or another encoding misread.
//! - Value: `/V` is present ([`AcroFormHold::ValueAbsent`]), not empty or
//!   whitespace ([`AcroFormHold::ValueEmpty`]), one choice
//!   ([`AcroFormHold::SeveralValues`]), one line ([`AcroFormHold::MultiLine`]:
//!   no LF, CR, VT, FF, NEL, U+2028 or U+2029) and no longer than the field's
//!   `/MaxLen` in characters ([`AcroFormHold::ExceedsMaxLen`]).
//! - Widgets: the field has a widget ([`AcroFormHold::NoWidget`]), one that is
//!   not hidden ([`AcroFormHold::HiddenWidgets`]), and every widget that is not
//!   hidden has a page and a Display rectangle
//!   ([`AcroFormHold::WidgetWithoutRect`]).
//! - Appearance: usable upright text-layer text that a visible widget's
//!   rectangle may touch ([`vf_source::select::overlapping_within`]) reads as
//!   the value after NFKC with whitespace removed (for a choice, as the export
//!   value or the display text). Otherwise the page prints other text in the
//!   box, such as a flattened older value or a label
//!   ([`AcroFormHold::AppearanceMismatch`]). Text in the box that select does
//!   not read (any quality other than usable, or turned text) and a selection
//!   that fails for its own limits cannot be compared
//!   ([`AcroFormHold::AppearanceUnverified`]). Only page content is checked:
//!   the widget's own appearance stream (`/AP`) never enters the text layer,
//!   so a stale appearance (for example under `NeedAppearances`) is not
//!   detected, and a box without text-layer text, the usual unflattened
//!   field, is not checked at all.
//! - Type: for an amount or number field, neither the matched names nor any
//!   usable fragment states a scale unit (`売上高（千円）`, `（単位：千円）`;
//!   [`AcroFormHold::ScaledUnit`]). A unit note split over several fragments
//!   is not detected here. Date, amount and number values must match the
//!   type's grammar as a whole ([`AcroFormHold::InvalidValue`]).
//!
//! # Precedence over the text layer
//!
//! Text-layer fill ([`fill::fill_with_limits`]) runs as before. When the
//! AcroForm value may be committed:
//!
//! - the text layer filled the same value (equal normalized values; for text,
//!   equal after NFKC with whitespace removed): the field is filled with the
//!   AcroForm value and both Evidence entries, AcroForm first;
//! - the text layer filled another value: the field is `ambiguous` without
//!   candidates and the match is held ([`AcroFormHold::TextLayerConflict`]);
//! - the text layer filled nothing: the field is filled with the AcroForm
//!   value and its text-layer candidates are dropped.
//!
//! When several AcroForm fields match, those that fail no other rule are
//! offered as candidates ([`Placement::Widget`], no hold), so assist may
//! choose among them; a text-layer value is kept only when they all agree
//! with it, and the field is `ambiguous` otherwise. When the one matching
//! field fails only [`AcroFormHold::InvalidValue`] and the text layer found
//! nothing, the field is `invalid` with the AcroForm value. Any other held
//! match leaves the text-layer result as it is.
//!
//! # Evidence
//!
//! AcroForm Evidence has one entry per visible widget: the page and the
//! widget rectangle in Display coordinates with [`Precision::Widget`] (the
//! box of the field, not glyph geometry), the value as `text` and
//! `start..end`, an empty `fragment_id` and the field name in
//! [`FillEvidence::acroform_field`].
//!
//! # Limits
//!
//! The AcroForm itself is read within [`vf_source::acroform::AcroFormLimits`]
//! (see [`vf_source::inspect_with_acroform`]). Matching spends units from the
//! fill [`Budget`] and checks its deadline: one unit per byte of names and
//! values normalized, compared or copied, per byte of fragment text scanned
//! for unit notes, and the selection work of the appearance check. Text-layer
//! fill then gets the remaining units. Comparing an AcroForm value with a
//! text-layer value is not counted; both are already bounded. When the
//! AcroForm could not be read (a limit, an encrypted document, an invalid
//! field tree), fill uses the text layer only and reports why in
//! [`FillResult::acroform`]. An AcroForm with neither fields nor XFA is
//! treated as absent: the output is that of the text layer alone.
use crate::fill::{
    self, FieldFill, FieldType, FillCandidate, FillError, FillEvidence, FillLimits, FillResult,
    FillStatus, Form, FormField, rules,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;
use vf_source::acroform::{
    AcroForm, AcroFormError, AcroFormLimit, FieldIssue, FieldKind, FieldValue, XfaPresence,
};
use vf_source::budget::Budget;
use vf_source::geometry::Rect;
use vf_source::layout::{Placement, Precision};
use vf_source::select::{self, SelectError};
use vf_source::{Orientation, Quality, SourceFragment};

/// [`FieldFill::method`] for a value taken from an AcroForm field.
pub const METHOD: &str = "acroform";

/// Whether the document's AcroForm could be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AcroFormSummary {
    /// The AcroForm was read: `fields` terminal fields.
    Read { fields: usize, xfa: XfaPresence },
    /// The AcroForm could not be read; fill used the text layer only.
    Unavailable {
        reason: Unavailable,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<AcroFormLimit>,
    },
}

/// Why the AcroForm could not be read ([`AcroFormError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    LimitExceeded,
    InvalidContainer,
    InvalidPageTree,
    Encrypted,
    InvalidFieldTree,
}

/// A rule of the commit policy an AcroForm field failed (see the module
/// documentation), in the order [`AcroFormMatch::reasons`] lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcroFormHold {
    /// The form also has an XFA description.
    XfaPresent,
    /// More than one AcroForm field matched the form field.
    SeveralFields,
    /// The AcroForm field matches two or more form fields (its name matches
    /// one and its `/TU` another, or several form fields share a label),
    /// whatever other rules it fails for them, so it fills none of them.
    FieldMatchesSeveralItems,
    /// A checkbox or radio button, or a choice for a typed field.
    KindUnsupported,
    /// The field has a [`FieldIssue`] other than one on `/DV`.
    FieldIssues,
    /// No `/V` (a `/DV` is never used).
    ValueAbsent,
    /// `/V` is empty or whitespace.
    ValueEmpty,
    /// More than one choice is selected.
    SeveralValues,
    /// The selected choice's export value differs from its `/Opt` display
    /// text ([`AcroFormMatch::display`]), so the value is a code, not the
    /// text a reader sees.
    ExportDiffersFromDisplay,
    /// `/V` spans more than one line.
    MultiLine,
    /// `/V` has more characters than the field's `/MaxLen`.
    ExceedsMaxLen,
    /// The field has no widget.
    NoWidget,
    /// Every widget is hidden.
    HiddenWidgets,
    /// A visible widget has no page or Display rectangle.
    WidgetWithoutRect,
    /// Usable upright text-layer text inside a visible widget differs from
    /// `/V`.
    AppearanceMismatch,
    /// Text-layer text inside a widget could not be compared: it is not
    /// usable, it is turned, or the selection failed for its own limits.
    AppearanceUnverified,
    /// An amount or number may be stated in a scale unit.
    ScaledUnit,
    /// The value does not match the field type's grammar.
    InvalidValue,
    /// The text layer filled the form field with another value.
    TextLayerConflict,
}

/// How an AcroForm field's name matched the form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    /// The last partial name of the fully qualified name.
    Name,
    /// The alternate name `/TU`.
    AlternateName,
}

/// An AcroForm field that matched a form field, and every rule it failed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AcroFormMatch {
    /// Fully qualified field name.
    pub field: String,
    pub kind: FieldKind,
    pub matched_by: MatchedBy,
    /// The exact `/V`: text, the one selected export value, or a button
    /// state name. Absent when there is none or more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// The `/Opt` display text of the selected export value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<FieldIssue>,
    /// Failed rules in declaration order; empty when the field may be
    /// committed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<AcroFormHold>,
}

/// A value that may be committed (or offered) for one form field.
#[derive(Debug, Clone)]
struct Found {
    label: String,
    raw: String,
    /// The normalized value of a typed field.
    typed: Option<Value>,
    evidence: Vec<FillEvidence>,
}

/// One matched AcroForm field: its report and, when it fails no rule other
/// than those listed in `failed`, its value.
struct Evaluated {
    /// Index of the AcroForm field in [`AcroForm::fields`].
    index: usize,
    report: AcroFormMatch,
    found: Option<Found>,
}

/// Fill `form` from the text layer and the document's AcroForm within
/// `limits`; see the module documentation. Without an AcroForm (`Ok` with
/// `present == false`), or with one that has neither fields nor XFA, the
/// result is exactly [`fill::fill_with_limits`]'s.
///
/// # Errors
/// As [`fill::fill_with_limits`]; AcroForm matching spends from the same
/// work budget and deadline.
pub fn fill_with_acroform(
    document_id: &str,
    fragments: &[SourceFragment],
    acroform: &Result<AcroForm, AcroFormError>,
    form: &Form,
    limits: &FillLimits,
) -> Result<FillResult, FillError> {
    // A form without fields and without XFA (`/AcroForm <<>>`) can fill
    // nothing, so its output stays that of the text layer alone. An XFA-only
    // form is still reported: its values may be in the XFA, which is not read.
    let form_pdf = acroform.as_ref().ok().filter(|found| {
        found.present && (!found.fields.is_empty() || found.xfa != XfaPresence::Absent)
    });
    let summary = match (acroform, form_pdf) {
        (Ok(_), Some(found)) => Some(AcroFormSummary::Read {
            fields: found.fields.len(),
            xfa: found.xfa,
        }),
        (Ok(_), None) => None,
        (Err(error), _) => Some(unavailable(*error)),
    };
    let Some(pdf) = form_pdf else {
        let mut result = fill::fill_with_limits(document_id, fragments, form, limits)?;
        result.acroform = summary;
        return Ok(result);
    };
    let mut budget = Budget::new(limits.max_work, limits.deadline);
    budget.check_deadline()?;
    let matcher = Matcher::new(pdf, fragments, &mut budget)?;
    let mut outcomes = Vec::with_capacity(form.fields.len());
    for field in &form.fields {
        budget.check_deadline()?;
        outcomes.push(matcher.evaluate(field, &mut budget)?);
    }
    hold_fields_matching_several_items(&mut outcomes);
    let mut result = fill::fill_with_limits(
        document_id,
        fragments,
        form,
        &FillLimits {
            max_work: budget.remaining(),
            ..*limits
        },
    )?;
    for (field, evaluated) in form.fields.iter().zip(outcomes) {
        if evaluated.is_empty() {
            continue;
        }
        if let Some(fill) = result.fields.get_mut(&field.id) {
            merge(field, fill, evaluated);
        }
    }
    result.acroform = summary;
    Ok(result)
}

/// Hold every match of an AcroForm field that matches two or more form fields
/// ([`AcroFormHold::FieldMatchesSeveralItems`]), whatever other rules its
/// matches fail: a field that fails a rule for one form field (an invalid
/// date, a choice for a typed field) must not fill another. Table fields
/// match nothing, and in an XFA form every match is already held.
fn hold_fields_matching_several_items(outcomes: &mut [Vec<Evaluated>]) {
    let counted = |e: &Evaluated| !e.report.reasons.contains(&AcroFormHold::XfaPresent);
    let mut items: HashMap<usize, usize> = HashMap::new();
    for evaluated in outcomes.iter() {
        for e in evaluated.iter().filter(|e| counted(e)) {
            *items.entry(e.index).or_default() += 1;
        }
    }
    for evaluated in outcomes.iter_mut() {
        for e in evaluated.iter_mut() {
            if counted(e) && items.get(&e.index).is_some_and(|&n| n > 1) {
                e.report
                    .reasons
                    .push(AcroFormHold::FieldMatchesSeveralItems);
                e.report.reasons.sort_unstable();
            }
        }
    }
}

fn unavailable(error: AcroFormError) -> AcroFormSummary {
    let (reason, limit) = match error {
        AcroFormError::LimitExceeded(limit) => (Unavailable::LimitExceeded, Some(limit)),
        AcroFormError::InvalidContainer => (Unavailable::InvalidContainer, None),
        AcroFormError::InvalidPageTree => (Unavailable::InvalidPageTree, None),
        AcroFormError::Encrypted => (Unavailable::Encrypted, None),
        AcroFormError::InvalidFieldTree => (Unavailable::InvalidFieldTree, None),
    };
    AcroFormSummary::Unavailable { reason, limit }
}

/// A label's base (without one leading honorific) and qualifiers
/// ([`rules::label_key`]).
type LabelKey = (String, Vec<String>);

/// AcroForm fields indexed by label key, with the document facts the rules
/// need.
struct Matcher<'a> {
    pdf: &'a AcroForm,
    fragments: &'a [SourceFragment],
    /// Label key of each field's last partial name and `/TU`, to field
    /// indexes in field order with how they matched.
    index: HashMap<LabelKey, Vec<(usize, MatchedBy)>>,
    /// Some usable fragment is a unit note; computed on first need.
    scaled_document: std::cell::Cell<Option<bool>>,
}

impl<'a> Matcher<'a> {
    fn new(
        pdf: &'a AcroForm,
        fragments: &'a [SourceFragment],
        budget: &mut Budget,
    ) -> Result<Self, FillError> {
        let mut index: HashMap<_, Vec<(usize, MatchedBy)>> = HashMap::new();
        for (i, field) in pdf.fields.iter().enumerate() {
            let partial = field.name.rsplit('.').next().unwrap_or(&field.name);
            let names = std::iter::once((partial, MatchedBy::Name)).chain(
                field
                    .alternate_name
                    .as_deref()
                    .map(|tu| (tu, MatchedBy::AlternateName)),
            );
            for (name, by) in names {
                // NFKC, normalization and hashing each read the name once.
                budget.spend(3 * name.len() as u64 + 1)?;
                let Some(key) = rules::label_key(name) else {
                    continue;
                };
                let entries = index.entry(key).or_default();
                // A field whose name and /TU both match counts once, by name.
                if !entries.iter().any(|(j, _)| *j == i) {
                    entries.push((i, by));
                }
            }
        }
        Ok(Self {
            pdf,
            fragments,
            index,
            scaled_document: std::cell::Cell::new(None),
        })
    }

    /// Every AcroForm field that matches `field`, in field order, evaluated
    /// against the commit rules. Table fields match nothing: an AcroForm
    /// field holds one value, never a table.
    fn evaluate(
        &self,
        field: &FormField,
        budget: &mut Budget,
    ) -> Result<Vec<Evaluated>, FillError> {
        if field.field_type == FieldType::Table {
            return Ok(Vec::new());
        }
        let mut matched: Vec<(usize, MatchedBy)> = Vec::new();
        for name in std::iter::once(&field.label).chain(&field.hints) {
            budget.spend(3 * name.len() as u64 + 1)?;
            let Some(key) = rules::label_key(name) else {
                continue;
            };
            for &(i, by) in self.index.get(&key).map_or(&[][..], Vec::as_slice) {
                match matched.iter_mut().find(|(j, _)| *j == i) {
                    Some(entry) => {
                        if by == MatchedBy::Name {
                            entry.1 = MatchedBy::Name;
                        }
                    }
                    None => matched.push((i, by)),
                }
            }
        }
        matched.sort_unstable_by_key(|&(i, _)| i);
        let several = matched.len() > 1;
        let mut evaluated = Vec::with_capacity(matched.len());
        for (i, by) in matched {
            let mut one = self.rules(field, i, by, budget)?;
            if several {
                one.report.reasons.push(AcroFormHold::SeveralFields);
            }
            one.report.reasons.sort_unstable();
            evaluated.push(one);
        }
        Ok(evaluated)
    }

    fn rules(
        &self,
        field: &FormField,
        index: usize,
        by: MatchedBy,
        budget: &mut Budget,
    ) -> Result<Evaluated, FillError> {
        let pdf = &self.pdf.fields[index];
        let mut reasons = Vec::new();
        if self.pdf.xfa != XfaPresence::Absent {
            reasons.push(AcroFormHold::XfaPresent);
        }
        let supported = match pdf.kind {
            FieldKind::Text => true,
            FieldKind::Choice => field.field_type == FieldType::Text,
            FieldKind::Checkbox | FieldKind::Radio => false,
        };
        if !supported {
            reasons.push(AcroFormHold::KindUnsupported);
        }
        let issues: Vec<FieldIssue> = pdf
            .issues
            .iter()
            .copied()
            .filter(|issue| !matches!(issue, FieldIssue::DefaultValue(_)))
            .collect();
        if !issues.is_empty() {
            reasons.push(AcroFormHold::FieldIssues);
        }
        let (raw, display) = match &pdf.value {
            None => {
                if issues.is_empty() {
                    reasons.push(AcroFormHold::ValueAbsent);
                }
                (None, None)
            }
            Some(FieldValue::Text(text) | FieldValue::State(text)) => (Some(text.clone()), None),
            Some(FieldValue::Choice(values)) => match values.as_slice() {
                [] => {
                    reasons.push(AcroFormHold::ValueAbsent);
                    (None, None)
                }
                [one] => {
                    budget.spend(
                        2 * (one.value.len() + one.display.as_ref().map_or(0, String::len)) as u64,
                    )?;
                    if one
                        .display
                        .as_deref()
                        .is_some_and(|display| compact(display) != compact(&one.value))
                    {
                        reasons.push(AcroFormHold::ExportDiffersFromDisplay);
                    }
                    (Some(one.value.clone()), one.display.clone())
                }
                _ => {
                    reasons.push(AcroFormHold::SeveralValues);
                    (None, None)
                }
            },
        };
        if let Some(raw) = &raw {
            budget.spend(2 * raw.len() as u64 + 1)?;
            if raw.trim().is_empty() {
                reasons.push(AcroFormHold::ValueEmpty);
            } else if raw.contains(LINE_BREAKS) {
                reasons.push(AcroFormHold::MultiLine);
            }
            if pdf
                .max_len
                .is_some_and(|max| raw.chars().count() > max as usize)
            {
                reasons.push(AcroFormHold::ExceedsMaxLen);
            }
        }
        let visible: Vec<_> = pdf.widgets.iter().filter(|w| !w.hidden).collect();
        let boxes: Vec<_> = visible
            .iter()
            .filter_map(|w| Some((w.page?, w.rect?)))
            .collect();
        if pdf.widgets.is_empty() {
            reasons.push(AcroFormHold::NoWidget);
        } else if visible.is_empty() {
            reasons.push(AcroFormHold::HiddenWidgets);
        } else if boxes.len() < visible.len() {
            reasons.push(AcroFormHold::WidgetWithoutRect);
        }
        if let Some(raw) = raw.as_deref().filter(|raw| !raw.trim().is_empty()) {
            let shown = [Some(raw), display.as_deref()];
            for &(page, rect) in &boxes {
                // select reads usable upright text only; anything else in
                // the box cannot be compared, so the value is not verified.
                budget.spend(self.fragments.len() as u64)?;
                if self.fragments.iter().any(|f| {
                    f.page == page
                        && (f.quality != Quality::Usable || f.orientation != Orientation::Deg0)
                        && intersects(rect, f.bbox)
                        && !f.raw_text.trim().is_empty()
                }) {
                    push_once(&mut reasons, AcroFormHold::AppearanceUnverified);
                }
                match select::overlapping_within(self.fragments, page, rect, budget) {
                    Ok(spans) => {
                        let mut text = String::new();
                        for selected in &spans {
                            let span = &selected.span;
                            let Some(part) = self
                                .fragments
                                .iter()
                                .find(|f| f.id == span.fragment_id)
                                .and_then(|f| f.raw_text.get(span.start..span.end))
                            else {
                                continue;
                            };
                            budget.spend(self.fragments.len() as u64 + part.len() as u64)?;
                            text.push_str(part);
                        }
                        if !spans.is_empty() {
                            budget.spend(2 * text.len() as u64 + 4 * raw.len() as u64)?;
                            let text = compact(&text);
                            if !shown.iter().flatten().any(|v| compact(v) == text) {
                                push_once(&mut reasons, AcroFormHold::AppearanceMismatch);
                            }
                        }
                    }
                    Err(SelectError::BudgetExceeded) => return Err(FillError::BudgetExceeded),
                    Err(SelectError::DeadlineExceeded) => {
                        return Err(FillError::DeadlineExceeded);
                    }
                    Err(_) => push_once(&mut reasons, AcroFormHold::AppearanceUnverified),
                }
            }
        }
        let typed = matches!(field.field_type, FieldType::Amount | FieldType::Number);
        // The fully qualified name tells `販売者.氏名` from `購入者.氏名`.
        let label = match &pdf.alternate_name {
            Some(tu) => format!("{} / {tu}", pdf.name),
            None => pdf.name.clone(),
        };
        budget.spend(label.len() as u64 + 1)?;
        if typed && raw.is_some() {
            let names = std::iter::once(pdf.name.rsplit('.').next().unwrap_or(&pdf.name))
                .chain(pdf.alternate_name.as_deref());
            let mut scaled = false;
            for name in names {
                budget.spend(3 * name.len() as u64 + 1)?;
                scaled |= rules::states_scale(name);
            }
            if scaled || self.scaled_document(budget)? {
                reasons.push(AcroFormHold::ScaledUnit);
            }
        }
        let value = match (&raw, field.field_type) {
            (Some(raw), FieldType::Text) => Some(Value::String(raw.trim().to_owned())),
            (Some(_), _) if reasons.contains(&AcroFormHold::ScaledUnit) => None,
            (Some(raw), field_type) => {
                budget.spend(2 * raw.len() as u64)?;
                let value = rules::normalize_value(field_type, raw);
                if value.is_none() && supported {
                    reasons.push(AcroFormHold::InvalidValue);
                }
                value
            }
            (None, _) => None,
        };
        let found = match (&raw, &value) {
            (Some(raw), Some(value)) if !raw.trim().is_empty() => {
                let mut evidence = Vec::with_capacity(boxes.len());
                for &(page, bbox) in &boxes {
                    budget.spend(2 * (raw.len() + pdf.name.len()) as u64 + 1)?;
                    evidence.push(FillEvidence {
                        page,
                        fragment_id: String::new(),
                        start: 0,
                        end: raw.len(),
                        text: raw.clone(),
                        bbox,
                        precision: Precision::Widget,
                        acroform_field: Some(pdf.name.clone()),
                    });
                }
                Some(Found {
                    label: label.clone(),
                    raw: raw.clone(),
                    typed: (field.field_type != FieldType::Text).then(|| value.clone()),
                    evidence,
                })
            }
            // An invalid typed value keeps its raw text and evidence.
            (Some(raw), None) if reasons == [AcroFormHold::InvalidValue] => {
                let evidence = boxes
                    .iter()
                    .map(|&(page, bbox)| FillEvidence {
                        page,
                        fragment_id: String::new(),
                        start: 0,
                        end: raw.len(),
                        text: raw.clone(),
                        bbox,
                        precision: Precision::Widget,
                        acroform_field: Some(pdf.name.clone()),
                    })
                    .collect::<Vec<_>>();
                budget.spend(2 * (raw.len() + pdf.name.len()) as u64 * boxes.len() as u64)?;
                Some(Found {
                    label: label.clone(),
                    raw: raw.clone(),
                    typed: None,
                    evidence,
                })
            }
            _ => None,
        };
        budget.spend(pdf.name.len() as u64 + raw.as_ref().map_or(0, String::len) as u64)?;
        Ok(Evaluated {
            index,
            report: AcroFormMatch {
                field: pdf.name.clone(),
                kind: pdf.kind,
                matched_by: by,
                raw,
                display,
                issues,
                reasons,
            },
            found,
        })
    }

    /// Whether any usable fragment is a unit note, scanned once.
    fn scaled_document(&self, budget: &mut Budget) -> Result<bool, FillError> {
        if let Some(scaled) = self.scaled_document.get() {
            return Ok(scaled);
        }
        let mut scaled = false;
        for fragment in self.fragments {
            if fragment.quality != Quality::Usable {
                continue;
            }
            budget.spend(2 * fragment.raw_text.len() as u64 + 1)?;
            if rules::unit_note(&fragment.raw_text) {
                scaled = true;
                break;
            }
        }
        self.scaled_document.set(Some(scaled));
        Ok(scaled)
    }
}

/// Characters that end a line: a value holding one spans several lines.
const LINE_BREAKS: [char; 7] = [
    '\n', '\r', '\u{000B}', '\u{000C}', '\u{0085}', '\u{2028}', '\u{2029}',
];

/// Positive-area intersection of two Display rectangles.
fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn push_once(reasons: &mut Vec<AcroFormHold>, reason: AcroFormHold) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

/// NFKC without whitespace, for comparing a value with other text.
fn compact(text: &str) -> String {
    text.nfkc().filter(|c| !c.is_whitespace()).collect()
}

/// Apply one form field's matched AcroForm fields to its text-layer result.
fn merge(field: &FormField, fill: &mut FieldFill, mut evaluated: Vec<Evaluated>) {
    let committable = match evaluated.as_slice() {
        [one] if one.report.reasons.is_empty() => one.found.clone(),
        _ => None,
    };
    let agrees = |found: &Found, fill: &FieldFill| match field.field_type {
        FieldType::Text => fill.raw.as_deref().map(compact) == Some(compact(&found.raw)),
        _ => found.typed.is_some() && fill.value == found.typed,
    };
    if let Some(found) = committable {
        if fill.status == FillStatus::Filled && !agrees(&found, fill) {
            *fill = fill::empty(FillStatus::Ambiguous);
            evaluated[0]
                .report
                .reasons
                .push(AcroFormHold::TextLayerConflict);
        } else {
            let text = if fill.status == FillStatus::Filled {
                std::mem::take(&mut fill.evidence)
            } else {
                Vec::new()
            };
            *fill = fill::empty(FillStatus::Filled);
            fill.method = Some(METHOD);
            fill.value = found
                .typed
                .clone()
                .or_else(|| Some(Value::String(found.raw.trim().to_owned())));
            fill.raw = Some(found.raw);
            fill.evidence = found.evidence;
            fill.evidence.extend(text);
        }
    } else {
        // Several matching fields: those failing no other rule are offered.
        let offered: Vec<&Found> = evaluated
            .iter()
            .filter(|e| e.report.reasons == [AcroFormHold::SeveralFields])
            .filter_map(|e| e.found.as_ref())
            .collect();
        if !offered.is_empty() {
            if fill.status == FillStatus::Filled {
                if !offered.iter().all(|found| agrees(found, fill)) {
                    *fill = fill::empty(FillStatus::Ambiguous);
                }
            } else {
                let text = std::mem::take(&mut fill.candidates);
                *fill = fill::empty(FillStatus::Ambiguous);
                fill.candidates = offered.into_iter().map(candidate).collect();
                fill.candidates.extend(text);
            }
        } else if let [one] = evaluated.as_slice()
            && one.report.reasons == [AcroFormHold::InvalidValue]
            && matches!(fill.status, FillStatus::NotFound | FillStatus::NoText)
            && let Some(found) = &one.found
        {
            *fill = fill::empty(FillStatus::Invalid);
            fill.raw = Some(found.raw.clone());
            fill.evidence = found.evidence.clone();
        }
    }
    fill.acroform = evaluated.into_iter().map(|e| e.report).collect();
}

fn candidate(found: &Found) -> FillCandidate {
    FillCandidate {
        label: found.label.clone(),
        raw: found.raw.clone(),
        value: found.typed.clone(),
        score: 1.0,
        placement: Placement::Widget,
        hold: None,
        holds: Vec::new(),
        evidence: found.evidence.clone(),
    }
}
