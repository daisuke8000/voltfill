//! Layout hypotheses over source fragments: lines and label/value pairs.
//!
//! Pairs are candidates, not decisions: a later stage matches them against
//! form labels. Every value is an exact byte range of an existing fragment,
//! so text is never synthesized. Sub-run boxes are estimated from
//! character classes (full width = 1 em, otherwise 0.5 em) and marked
//! [`Precision::Estimated`]; they are never reported as measured.
//!
//! Text is read in each fragment's own reading direction: horizontal, or
//! turned by a right angle (see [`crate::SourceFragment::orientation`]).
//! Each turned orientation is analysed in a turned copy of its fragments
//! (its reading frame) by the same rules, limits and budget as upright text,
//! and every box is turned back: all boxes are Display coordinates, and
//! "right", "below", "left edge" and "line" below mean the reading frame.
//! Fragments of different orientations never pair. Turned runs that lie
//! within half an em of a neighbour on their reading line are pieces of one
//! phrase drawn word by word and take part in no pair (see `orient`).
//!
//! Each pair records how its value is tied to its label ([`Binding`]):
//! - [`Binding::Delimiter`]: an inline `label：value` / `label: value` in one
//!   fragment, or a fragment ending with its only delimiter (`label：`)
//!   followed by the next fragment to its right on the same line, at most
//!   [`MAX_DELIMITER_GAP_EM`] em away.
//! - [`Binding::Geometric`]: position only — a right neighbour without a
//!   delimiter or further than [`MAX_DELIMITER_GAP_EM`] em, a value directly
//!   below the label, or a row of a column.
//! - [`Binding::Whitespace`]: one fragment that starts with a label and
//!   continues after spacing only (`発行日 2026年11月8日`). Layout cannot
//!   tell a label from the first word of a sentence, so these are produced
//!   only by [`analyze_with_labels`], whose caller decides which text is a
//!   label.
//!
//! A value to the right (and an inline value) takes continuation lines in
//! its column; a value with more than one line is still one pair, with one
//! span per line. Rows of multi-row tables and stacked lists are never one
//! value (only flagged, see [`Placement::Column`]); a caller that knows a
//! table's header line reads its rows with [`read_table`]. Not supported:
//! AcroForm values and true vertical writing (vertical writing mode fonts,
//! glyphs stacked one per run).
//!
//! Hypotheses are conservative ("a wrong value is worse than an empty one"):
//! - A fragment with more than one `label：` delimiter (for example
//!   `TEL：03-1234-5678 FAX：03-1234-5679`) is not split, because the end of
//!   each value cannot be told apart from the start of the next label. Such a
//!   fragment, a `label：value` fragment and a bare `label：` fragment are
//!   never read as the value of another label.
//! - A value fragment followed on its line by a plain fragment, at any
//!   distance, is marked [`Pair::split`]: the value may be cut short
//!   (`住所：` `〒100-0001` `東京都千代田区…`) or followed by another value
//!   without a delimiter. Only a `label：` or `label：value` fragment right
//!   after the value starts something else (the next pair).
//! - A value runs over at most [`MAX_VALUE_LINES`] lines; a longer run is
//!   dropped rather than cut off. Continuation stops at a line that is itself
//!   a `label：value` pair or ends with a delimiter. Continuation lines of a
//!   value to the right start within half a line height of the value's left
//!   edge. Continuation lines of an inline value start anywhere from the
//!   fragment's left edge (a value wrapped back under its label) to 1.5 line
//!   heights right of the value's estimated start. A continuation line may
//!   share its line with other fragments (the other column of a two-column
//!   layout): its first fragment in the value's column continues the value,
//!   so the value is never cut at such a line. For a value to the right of
//!   its label, a fragment in the label's column before that one starts the
//!   next row instead (stacked `小計` / `合計` rows).
//! - A value directly below a label is taken only when the cells to the
//!   right of the label and of the value (if any) line up as one column, so
//!   the label cell of the next row (`役職` with its own value `課長`) is not
//!   read as the value of a label whose own value cell is blank. When neither
//!   has a cell to its right, the row just above the label and the row just
//!   below the value must not start a fragment at the label's left edge that
//!   has a cell to its right: then the label sits in the label column of a
//!   label/value grid and the fragment below it is the next row's label
//!   (`部署` and `役職` with both value cells blank).
//! - A label with two or more data rows below it, at the pitch measured from
//!   the label row to the first row, yields [`Placement::Column`] hypotheses,
//!   which never determine a single value. This covers table headers and
//!   stacked single-column lists (`品名` / `りんご` / `みかん`). A row whose
//!   cell in this column is blank still counts when a neighbouring column of
//!   the first row has a cell in it.
//! - Searches below a label or in a column examine at most [`MAX_WINDOW`]
//!   fragments of a line. A more crowded window (overlapping text) yields no
//!   value below and counts as a data row of a column.
//!
//! Work, time and memory are bounded: [`LayoutLimits::max_pairs`] caps the
//! hypotheses per document, each value holds at most [`MAX_VALUE_LINES`]
//! spans and each label at most [`MAX_COLUMN_ROWS`] column rows, every
//! search is limited to [`MAX_WINDOW`] fragments, and each usable fragment's
//! text is read twice (classified and measured) however many pairs use it;
//! with labels, [`analyze_with_labels`] also reads a bounded prefix of each
//! fragment without a delimiter and, when that prefix is a label, the rest
//! of the fragment once more (see there for the units). The limits cover all
//! orientations together: the pair cap counts every orientation's
//! hypotheses, and turned copies, their analysis and the mapping back spend
//! from the same budget. [`LayoutLimits::max_work`] and
//! [`LayoutLimits::deadline`] stop the analysis with a typed error (see
//! [`analyze_within`] for the units).
use crate::budget::{Budget, BudgetError};
use crate::geometry::Rect;
use crate::source::{Orientation, Quality, SourceFragment};
use serde::Serialize;
use std::collections::VecDeque;
use std::time::Instant;

mod orient;
mod table;
pub use table::{
    ColumnRole, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table, TableEnd, Totals, read_table,
};

/// Most lines one value may span: its first line plus continuation lines.
/// A value that would run longer is dropped, not truncated.
pub const MAX_VALUE_LINES: usize = 5;
/// Most data rows examined (and reported) below one label ([`Placement::Column`]).
pub const MAX_COLUMN_ROWS: usize = 5;
/// Most fragments of one line examined below a label or in a column.
pub const MAX_WINDOW: usize = 16;
/// Default ceiling on label/value hypotheses for one document.
pub const DEFAULT_MAX_PAIRS: usize = 50_000;
/// Widest gap, in em (the smaller height of the two fragments), between a
/// `label：` fragment and the next fragment to its right for the delimiter
/// to bind them ([`Binding::Delimiter`]). A wider gap binds by position
/// only ([`Binding::Geometric`]): a blank `氏名：` must not take an unrelated
/// fragment far to its right (a right-aligned company name) as its value.
/// Runs closer than about half an em are usually extracted as one fragment
/// (split by the delimiter instead), so delimiter-bound right values lie
/// roughly 0.5 to 4 em to the right of their labels. The smaller height
/// sets the em, so a large right fragment (a 30 pt title next to a 10 pt
/// `氏名：`) does not widen the reach of a small label.
pub const MAX_DELIMITER_GAP_EM: f64 = 4.0;
/// Default work units for [`analyze`] (see [`analyze_within`]).
pub const DEFAULT_MAX_WORK: u64 = 200_000_000;
/// Most spacing runs of one fragment tried as the end of a label, and most
/// words of a value tried as one label ([`Binding::Whitespace`], see
/// [`analyze_with_labels`]).
pub const MAX_SPACED_SPLITS: usize = 4;
/// Longest label, in bytes, tried before spacing in one fragment, and
/// longest spacing run after it ([`Binding::Whitespace`]); also the longest
/// run of words of a value tried as a label.
pub const MAX_SPACED_LABEL_BYTES: usize = 96;

/// Resource limits for [`analyze`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutLimits {
    /// Label/value hypotheses allowed for one document.
    pub max_pairs: usize,
    /// Work units allowed (see [`analyze_within`]).
    pub max_work: u64,
    /// Optional wall-clock deadline.
    pub deadline: Option<Instant>,
}

impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            max_pairs: DEFAULT_MAX_PAIRS,
            max_work: DEFAULT_MAX_WORK,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("layout exceeded its label/value hypothesis limit")]
    LimitExceeded,
    #[error("layout exceeded its work budget")]
    BudgetExceeded,
    #[error("layout exceeded its deadline")]
    DeadlineExceeded,
}

impl From<BudgetError> for LayoutError {
    fn from(error: BudgetError) -> Self {
        match error {
            BudgetError::WorkExceeded => Self::BudgetExceeded,
            BudgetError::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }
}

/// How a span's box was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// The whole fragment box reported by the extractor.
    Run,
    /// A part of a fragment, positioned by character-class width estimates.
    Estimated,
    /// An AcroForm widget's rectangle: where a field value is shown, not
    /// the geometry of any glyph. Layout never produces it.
    Widget,
}

/// An exact byte range of one source fragment with its box.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub fragment_id: String,
    pub start: usize,
    pub end: usize,
    pub bbox: Rect,
    pub precision: Precision,
}

/// Usable fragments of one orientation sharing a baseline band, in reading
/// order (left to right for upright text).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub page: u32,
    /// Indexes into the analyzed fragment slice.
    pub fragments: Vec<usize>,
    pub bbox: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Label and value in one fragment, split at a delimiter (or, with
    /// [`Binding::Whitespace`], at whitespace).
    Inline,
    /// Value is the next fragment on the same line in reading order (to the
    /// right for upright text).
    Right,
    /// The only data row directly below the label in its reading frame (the
    /// next line in the direction lines advance).
    Below,
    /// One data row below a label that has two or more data rows (a table
    /// header or the first line of a stacked list); one pair per non-blank
    /// row, at most [`MAX_COLUMN_ROWS`]. A column never determines a single
    /// value on its own.
    Column,
    /// An AcroForm field value bound to its label by the field's name, not
    /// by position. Layout never produces it.
    Widget,
}

/// How a pair's value is tied to its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// An explicit delimiter: a `label：value` fragment ([`Placement::Inline`]),
    /// or a fragment that ends with its only delimiter (`label：`) and the
    /// next fragment to its right on the same line, at most
    /// [`MAX_DELIMITER_GAP_EM`] em away ([`Placement::Right`]).
    Delimiter,
    /// Position only: a right neighbour without a delimiter or further than
    /// [`MAX_DELIMITER_GAP_EM`] em, a value below the label or a row of a
    /// column.
    Geometric,
    /// Whitespace only, inside one fragment ([`Placement::Inline`]): a
    /// fragment without a delimiter that starts with a caller-recognized
    /// label (`発行日 2026年11月8日`). See [`analyze_with_labels`].
    Whitespace,
}

/// A label/value hypothesis. `value` holds one span per line, in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pair {
    pub page: u32,
    pub label: Span,
    pub value: Vec<Span>,
    pub placement: Placement,
    pub binding: Binding,
    /// One flag per line of `value`: a plain fragment (not a `label：` or
    /// `label：value` fragment) follows the line's fragment on its line, at
    /// any distance: the value may be cut short (split over runs) or followed
    /// by another value without a delimiter. Per line, so a stage that drops
    /// later lines (a continuation line that is a form label) only counts
    /// the lines it keeps.
    pub split: Vec<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub pairs: Vec<Pair>,
}

/// Build lines and label/value hypotheses from usable fragments within `limits`.
///
/// # Errors
/// As [`analyze_within`].
pub fn analyze(fragments: &[SourceFragment], limits: &LayoutLimits) -> Result<Layout, LayoutError> {
    let mut budget = Budget::new(limits.max_work, limits.deadline);
    analyze_within(fragments, limits.max_pairs, &mut budget)
}

/// Build lines and label/value hypotheses, spending work from `budget`.
///
/// Upright fragments are analysed first, then each turned orientation in
/// its own reading frame; lines and pairs are grouped by page (stable, so
/// upright results come first within a page).
///
/// Work units, per orientation analysed: one per fragment and per sort
/// comparison while grouping lines; two per byte of usable fragment text,
/// which is classified and measured once per fragment; one per line and per
/// fragment examined in a search; and one plus the fragment id's length per
/// span written into a pair. For each turned orientation also one per
/// fragment scanned for it, one plus the id's and the text's length per
/// turned copy, one per line and one plus the ids' lengths per fragment
/// visited for phrase pieces, one plus the id's length per span checked
/// against them, and one per line, line fragment and span turned back. Searches are bounded ([`MAX_VALUE_LINES`],
/// [`MAX_COLUMN_ROWS`], [`MAX_WINDOW`]), and a span reuses its fragment's
/// measurements instead of reading the text again, so the units count all
/// work that grows with the input.
///
/// # Errors
/// [`LayoutError::LimitExceeded`] as soon as the document would produce more
/// than `max_pairs` hypotheses, [`LayoutError::BudgetExceeded`] when the
/// budget runs out and [`LayoutError::DeadlineExceeded`] once its deadline
/// has passed (checked at least every [`crate::budget::CHECK_INTERVAL`]
/// units, and before reading any text longer than that).
pub fn analyze_within(
    fragments: &[SourceFragment],
    max_pairs: usize,
    budget: &mut Budget,
) -> Result<Layout, LayoutError> {
    analyze_with_labels(fragments, max_pairs, None, budget)
}

/// [`analyze_within`], plus [`Binding::Whitespace`] hypotheses when
/// `is_label` is given: it receives candidate label text (a fragment's text
/// up to a spacing run, or one to [`MAX_SPACED_SPLITS`] words of a value
/// with the spacing between them) and answers whether that text is a label.
///
/// Spacing is ASCII space, tab and the ideographic space U+3000; other
/// whitespace (no-break or em spaces) separates neither labels nor words. A
/// fragment without a delimiter is split after a label at each of its first
/// [`MAX_SPACED_SPLITS`] spacing runs that start within
/// [`MAX_SPACED_LABEL_BYTES`] and are at most as long; a longer run ends the
/// search. The rest of the fragment is the value, an exact byte range with
/// an estimated box. A rest that holds another label is not a value: at each
/// of its words, the runs of one to [`MAX_SPACED_SPLITS`] words within
/// [`MAX_SPACED_LABEL_BYTES`] are tried, so `姓 山田 名 太郎` (labels `姓`,
/// `名`), `First Name John Last Name Smith` (`Last Name`) and a header row
/// `品名 数量 単価 金額` (`金額`) give no value. Only the fragment's own rest
/// is checked, not its continuation lines. The value takes continuation
/// lines as an inline value does, but ends before a line that itself starts
/// with a label and spacing, so stacked `label value` lines stay separate.
/// That rule also cuts a wrapped sentence before a line that starts with a
/// label and spacing (`備考 お支払いは` / `合計 金額を…`).
///
/// Each `is_label` call costs one unit per byte passed, charged before the
/// call. Searching a fragment for a label costs the bytes it may read (at
/// most 196: a label and a spacing run of [`MAX_SPACED_LABEL_BYTES`] each
/// and one character), charged before the search; each fragment is searched
/// once. Checking a value for labels costs
/// its rest's bytes, charged before the check, plus the tried runs: at most
/// 11 units per byte of the rest.
///
/// # Errors
/// As [`analyze_within`].
pub fn analyze_with_labels(
    fragments: &[SourceFragment],
    max_pairs: usize,
    mut is_label: Option<&mut dyn FnMut(&str) -> bool>,
    budget: &mut Budget,
) -> Result<Layout, LayoutError> {
    budget.check_deadline()?;
    orient::analyze(fragments, max_pairs, budget, |frame, max, budget| {
        // Reborrow for this orientation's frame only.
        let is_label: Option<&mut dyn FnMut(&str) -> bool> = match is_label.as_mut() {
            Some(f) => Some(&mut **f),
            None => None,
        };
        analyze_frame(frame, max, is_label, budget)
    })
}

/// [`analyze_within`] for upright fragments only: turned ones are skipped
/// here and reach it as reading-frame copies (see `orient`).
fn analyze_frame(
    fragments: &[SourceFragment],
    max_pairs: usize,
    is_label: Option<&mut dyn FnMut(&str) -> bool>,
    budget: &mut Budget,
) -> Result<Layout, LayoutError> {
    budget.check_deadline()?;
    let lines = group_lines(fragments, budget)?;
    let mut measures = Vec::with_capacity(fragments.len());
    for fragment in fragments {
        if !usable(fragment) {
            // Never on a line, so never measured or read.
            measures.push(Measure::UNUSED);
            continue;
        }
        // Classification and measurement each read the text once.
        budget.spend(2 * u64::try_from(fragment.raw_text.len()).unwrap_or(u64::MAX / 2))?;
        measures.push(Measure::new(fragment));
    }
    let page = Page {
        fragments,
        measures: &measures,
        lines: &lines,
    };
    let mut pairs = Pairs {
        list: Vec::new(),
        max: max_pairs,
    };
    for (i, line) in lines.iter().enumerate() {
        budget.spend(1 + line.fragments.len() as u64)?;
        for (position, &f) in line.fragments.iter().enumerate() {
            if !matches!(measures[f].kind, Kind::Inline { .. }) {
                continue;
            }
            let label = page.span(f, START, LABEL_END, budget)?;
            let first = page.span(f, VALUE_START, END, budget)?;
            let height = fragments[f].bbox.height();
            // A wrapped inline value continues under its label or under
            // its own (estimated) start.
            let column = Column {
                from: fragments[f].bbox.left - 0.5 * height,
                to: first.bbox.left + 1.5 * height,
                label: None,
            };
            if let Some((value, split)) = page.value(i, position, first, column, budget)? {
                pairs.push(Pair {
                    page: line.page,
                    label,
                    value,
                    placement: Placement::Inline,
                    binding: Binding::Delimiter,
                    split,
                })?;
            }
        }
        for (position, window) in line.fragments.windows(2).enumerate() {
            let (label, value) = (window[0], window[1]);
            if !measures[label].kind.can_label() || measures[value].kind != Kind::Plain {
                continue;
            }
            let first = page.span(value, START, END, budget)?;
            let left = fragments[value].bbox.left;
            let half = 0.5 * fragments[value].bbox.height();
            let column = Column {
                from: left - half,
                to: left + half,
                label: Some(fragments[label].bbox),
            };
            // `windows(2)` over left-sorted fragments: no other fragment
            // starts between the label and its value.
            let (l, v) = (fragments[label].bbox, fragments[value].bbox);
            // The smaller height: a large right fragment must not widen the
            // reach of a small `label：`.
            let em = l.height().min(v.height());
            let binding = if measures[label].kind == Kind::Label
                && v.left - l.right <= MAX_DELIMITER_GAP_EM * em
            {
                Binding::Delimiter
            } else {
                Binding::Geometric
            };
            if let Some((value, split)) = page.value(i, position + 1, first, column, budget)? {
                pairs.push(Pair {
                    page: line.page,
                    label: page.span(label, START, LABEL_END, budget)?,
                    value,
                    placement: Placement::Right,
                    binding,
                    split,
                })?;
            }
        }
        if lines.get(i + 1).is_some_and(|next| next.page == line.page) {
            for position in 0..line.fragments.len() {
                if measures[line.fragments[position]].kind.can_label() {
                    page.below(i, position, &mut pairs, budget)?;
                }
            }
        }
    }
    if let Some(is_label) = is_label {
        // Each plain fragment is searched for label splits once; the values
        // below only read the result.
        let mut splits = vec![Vec::new(); fragments.len()];
        for line in &lines {
            budget.spend(1 + line.fragments.len() as u64)?;
            for &f in &line.fragments {
                if measures[f].kind == Kind::Plain {
                    splits[f] = page.label_splits(f, is_label, budget)?;
                }
            }
        }
        for (i, line) in lines.iter().enumerate() {
            budget.spend(1 + line.fragments.len() as u64)?;
            for (position, &f) in line.fragments.iter().enumerate() {
                if !splits[f].is_empty() {
                    page.spaced(i, position, &splits, is_label, &mut pairs, budget)?;
                }
            }
        }
    }
    Ok(Layout {
        lines,
        pairs: pairs.list,
    })
}

/// A value's spans and, per line, whether it is [`Page::trailed`].
type Value = (Vec<Span>, Vec<bool>);

struct Pairs {
    list: Vec<Pair>,
    max: usize,
}

impl Pairs {
    fn push(&mut self, pair: Pair) -> Result<(), LayoutError> {
        if self.list.len() >= self.max {
            return Err(LayoutError::LimitExceeded);
        }
        self.list.push(pair);
        Ok(())
    }
}

/// How a fragment may take part in a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No delimiter: a label or a value.
    Plain,
    /// Ends with its only delimiter (`請求日：`): a label, never a value.
    Label,
    /// One `label：value` split at byte `at`; the value starts at or after `after`.
    Inline { at: usize, after: usize },
    /// Several delimiters or an empty label side: neither split nor reused.
    Opaque,
}

impl Kind {
    fn can_label(self) -> bool {
        matches!(self, Self::Plain | Self::Label)
    }
}

const DELIMITERS: &[char] = &['：', ':'];

/// A delimiter that may separate a label from its value. Times (`9:00`,
/// `9：00`) and URL schemes (`https://`) are not delimiters.
fn is_delimiter(text: &str, at: usize, c: char) -> bool {
    if !DELIMITERS.contains(&c) {
        return false;
    }
    let after = at + c.len_utf8();
    let digit_before = text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_digit());
    let digit_after = text[after..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit());
    !(digit_before && digit_after || text[after..].starts_with("//"))
}

/// Whether a fragment has no label delimiter (`請求日：`, `品名：りんご`):
/// it can be a table header cell or a cell, a label or a value. Times
/// (`9:00`) and URL schemes are not delimiters. [`Pair::split`] marks a
/// value line whose fragment is followed on its line by such a fragment.
/// Reads the fragment's text once.
#[must_use]
pub fn is_plain(fragment: &SourceFragment) -> bool {
    classify(fragment) == Kind::Plain
}

fn classify(f: &SourceFragment) -> Kind {
    let text = f.raw_text.as_str();
    let mut delimiters = text
        .char_indices()
        .filter(|&(at, c)| is_delimiter(text, at, c));
    let Some((at, delimiter)) = delimiters.next() else {
        return Kind::Plain;
    };
    if delimiters.next().is_some() || text[..at].trim().is_empty() {
        return Kind::Opaque;
    }
    let after = at + delimiter.len_utf8();
    if text[after..].trim().is_empty() {
        Kind::Label
    } else {
        Kind::Inline { at, after }
    }
}

/// Offset slots of a [`Measure`].
const START: usize = 0;
const END: usize = 1;
const LABEL_END: usize = 2;
const VALUE_START: usize = 3;

/// A fragment's kind and the byte offsets of its parts with the estimated
/// advance before each, computed once so that spans never re-read the text.
#[derive(Debug, Clone, Copy)]
struct Measure {
    kind: Kind,
    /// [`START`] and [`END`] of the trimmed text, [`LABEL_END`] (the label
    /// without its delimiter; for an inline fragment, the end of its label
    /// side) and [`VALUE_START`] (an inline fragment's value; otherwise the
    /// trimmed start).
    at: [usize; 4],
    /// Advance in em before each offset of `at`.
    em: [f64; 4],
    /// Advance in em of the whole text.
    total: f64,
}

impl Measure {
    /// The placeholder for a fragment that is not [`usable`].
    const UNUSED: Self = Self {
        kind: Kind::Opaque,
        at: [0; 4],
        em: [0.0; 4],
        total: 0.0,
    };

    fn new(f: &SourceFragment) -> Self {
        let text = f.raw_text.as_str();
        let kind = classify(f);
        let start = text.len() - text.trim_start().len();
        let end = text.trim_end().len();
        let (label_end, value_start) = match kind {
            Kind::Inline { at, after } => (
                text[..at].trim_end().len(),
                after + (text[after..].len() - text[after..].trim_start().len()),
            ),
            _ => {
                // `請求番号：` → `請求番号`.
                let trimmed = text[start..end]
                    .trim_end_matches(DELIMITERS)
                    .trim_end_matches(char::is_whitespace);
                let label_end = if trimmed.is_empty() {
                    end
                } else {
                    start + trimmed.len()
                };
                (label_end, start)
            }
        };
        let at = [start, end, label_end, value_start];
        let mut em = [0.0; 4];
        let mut total = 0.0;
        for (i, c) in text.char_indices() {
            for (slot, &offset) in at.iter().enumerate() {
                if offset == i {
                    em[slot] = total;
                }
            }
            total += char_em(c);
        }
        for (slot, &offset) in at.iter().enumerate() {
            if offset == text.len() {
                em[slot] = total;
            }
        }
        Self {
            kind,
            at,
            em,
            total,
        }
    }
}

/// The span of `fragment` (measured as `measure`) between two byte
/// offsets, each with the estimated advance in em before it. Costs one unit
/// plus the fragment id's length; the text is not read.
fn span_of(
    fragment: &SourceFragment,
    measure: &Measure,
    (start, em_start): (usize, f64),
    (end, em_end): (usize, f64),
    budget: &mut Budget,
) -> Result<Span, BudgetError> {
    budget.spend(1 + fragment.id.len() as u64)?;
    if start == 0 && end == fragment.raw_text.len() {
        return Ok(Span {
            fragment_id: fragment.id.clone(),
            start,
            end,
            bbox: fragment.bbox,
            precision: Precision::Run,
        });
    }
    let (left, right) = if measure.total > 0.0 {
        let width = fragment.bbox.width();
        (
            fragment.bbox.left + width * em_start / measure.total,
            fragment.bbox.left + width * em_end / measure.total,
        )
    } else {
        (fragment.bbox.left, fragment.bbox.right)
    };
    Ok(Span {
        fragment_id: fragment.id.clone(),
        start,
        end,
        bbox: Rect {
            left,
            top: fragment.bbox.top,
            right: right.max(left + f64::EPSILON),
            bottom: fragment.bbox.bottom,
        },
        precision: Precision::Estimated,
    })
}

/// The span of a fragment's whole text without surrounding whitespace, as
/// a table cell or header. Costs two units per byte (the text is classified
/// and measured) plus one and the fragment id's length.
///
/// # Errors
/// As [`Budget::spend`].
pub fn fragment_span(fragment: &SourceFragment, budget: &mut Budget) -> Result<Span, BudgetError> {
    budget.spend(2 * u64::try_from(fragment.raw_text.len()).unwrap_or(u64::MAX / 2))?;
    let measure = Measure::new(fragment);
    let (start, end) = (
        (measure.at[START], measure.em[START]),
        (measure.at[END], measure.em[END]),
    );
    span_of(fragment, &measure, start, end, budget)
}

/// What a line holds in a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    /// The position in the line of its first fragment in the column.
    Found(usize),
    /// No fragment in the column.
    Blank,
    /// More than [`MAX_WINDOW`] fragments around the column.
    Crowded,
}

/// Where continuation lines of a value may start (left edges, in points).
#[derive(Debug, Clone, Copy)]
struct Column {
    from: f64,
    to: f64,
    /// The label's box for a value to its right: a line with a fragment in
    /// the label's column before the value column starts the next row.
    label: Option<Rect>,
}

/// The analyzed fragments with their measurements and lines.
struct Page<'a> {
    fragments: &'a [SourceFragment],
    measures: &'a [Measure],
    lines: &'a [Line],
}

impl Page<'_> {
    fn kind(&self, f: usize) -> Kind {
        self.measures[f].kind
    }

    /// The span of fragment `f` from offset slot `from` to slot `to`.
    /// Costs one unit plus the fragment id's length; the text is not read.
    fn span(
        &self,
        f: usize,
        from: usize,
        to: usize,
        budget: &mut Budget,
    ) -> Result<Span, BudgetError> {
        let measure = &self.measures[f];
        self.part(
            f,
            (measure.at[from], measure.em[from]),
            (measure.at[to], measure.em[to]),
            budget,
        )
    }

    /// The span of fragment `f` between two byte offsets, each with the
    /// estimated advance in em before it. Costs as [`Page::span`].
    fn part(
        &self,
        f: usize,
        start: (usize, f64),
        end: (usize, f64),
        budget: &mut Budget,
    ) -> Result<Span, BudgetError> {
        span_of(&self.fragments[f], &self.measures[f], start, end, budget)
    }

    /// The value starting with `first` on line `line`, with follow-on lines.
    /// A follow-on line continues the value when its first fragment starting
    /// in `column` is plain, whatever other fragments share the line: in a
    /// two-column layout the next line also holds the other column's text.
    /// The value ends at a line without a fragment in `column`, at a
    /// `label：` or `label：value` fragment there, and (for a value to the
    /// right of its label) at a line with a fragment in the label's column
    /// before the value column: the next row (小計 / 合計). The first line's
    /// fragment is at `position` of line `line`. Also returns, per line,
    /// whether its fragment is [`Page::trailed`] ([`Pair::split`]). `None` when
    /// the value would exceed [`MAX_VALUE_LINES`].
    fn value(
        &self,
        line: usize,
        position: usize,
        first: Span,
        column: Column,
        budget: &mut Budget,
    ) -> Result<Option<Value>, BudgetError> {
        let page = self.lines[line].page;
        let mut last = first.bbox;
        let mut split = vec![self.trailed(&self.lines[line], position)];
        let mut value = vec![first];
        for next_line in &self.lines[line + 1..] {
            budget.spend(1)?;
            let height = last.height();
            if next_line.page != page || next_line.bbox.top - last.bottom > height {
                break;
            }
            let Some(at) = self.continuation(next_line, column, height, budget)? else {
                break;
            };
            if value.len() >= MAX_VALUE_LINES {
                return Ok(None);
            }
            let next = next_line.fragments[at];
            value.push(self.span(next, START, END, budget)?);
            split.push(self.trailed(next_line, at));
            last = self.fragments[next].bbox;
        }
        Ok(Some((value, split)))
    }

    /// Whether the fragment at `position` of `line` is followed on the line
    /// by a plain fragment, at any distance. A gap says nothing safe: a
    /// value split over runs can be 3 em or more apart while the delimiter
    /// still binds its first run.
    fn trailed(&self, line: &Line, position: usize) -> bool {
        line.fragments
            .get(position + 1)
            .is_some_and(|&next| self.kind(next) == Kind::Plain)
    }

    /// The position in `line` of the plain fragment that continues a value
    /// in `column`, if any (see [`Page::value`]). Fragments are sorted by
    /// left edge, so the first one starting at or after `column.from` is the
    /// only candidate.
    fn continuation(
        &self,
        line: &Line,
        column: Column,
        tolerance: f64,
        budget: &mut Budget,
    ) -> Result<Option<usize>, BudgetError> {
        let at = line
            .fragments
            .partition_point(|&f| self.fragments[f].bbox.left < column.from);
        let Some(&next) = line.fragments.get(at) else {
            return Ok(None);
        };
        if self.fragments[next].bbox.left > column.to || self.kind(next) != Kind::Plain {
            return Ok(None);
        }
        if let Some(label) = column.label
            && let Cell::Found(row) = self.column_cell(line, label, 0.5 * tolerance, budget)?
            && row < at
        {
            return Ok(None);
        }
        Ok(Some(at))
    }

    /// Below (or Column) hypotheses for the label at `position` on line `index`.
    fn below(
        &self,
        index: usize,
        position: usize,
        pairs: &mut Pairs,
        budget: &mut Budget,
    ) -> Result<(), LayoutError> {
        let line = &self.lines[index];
        let next = &self.lines[index + 1];
        let f = line.fragments[position];
        let label = &self.fragments[f];
        let Some(slot) = self.below_slot(label, next, budget)? else {
            return Ok(());
        };
        let first = &self.fragments[next.fragments[slot]];
        let height = label.bbox.height().max(first.bbox.height());
        // Rows follow at the pitch measured from the label row to the first
        // row (a large header over small rows has a wider gap), with slack.
        let reach = height.max(next.bbox.top - line.bbox.bottom) + 0.5 * height;
        // The cells right of the label and of the value must form one column:
        // a label row with a value cell of its own, or a value that is the
        // label cell of a row with its own value, is not label-above-value.
        let right_of_label = line.fragments.get(position + 1);
        let right_of_value = next.fragments.get(slot + 1);
        match (right_of_label, right_of_value) {
            (None, None) => {
                if self.in_label_column(index, label, height, reach, budget)? {
                    return Ok(());
                }
            }
            (Some(&a), Some(&b))
                if same_column(self.fragments[a].bbox, self.fragments[b].bbox, height) => {}
            _ => return Ok(()),
        }
        let (rows, cells) = self.data_rows(index + 1, slot, height, reach, budget)?;
        if rows < 2 {
            return pairs.push(Pair {
                page: line.page,
                label: self.span(f, START, LABEL_END, budget)?,
                value: vec![self.span(next.fragments[slot], START, END, budget)?],
                placement: Placement::Below,
                binding: Binding::Geometric,
                split: vec![self.trailed(next, slot)],
            });
        }
        for (cell, split) in cells {
            pairs.push(Pair {
                page: line.page,
                label: self.span(f, START, LABEL_END, budget)?,
                value: vec![self.span(cell, START, END, budget)?],
                placement: Placement::Column,
                binding: Binding::Geometric,
                split: vec![split],
            })?;
        }
        Ok(())
    }

    /// The slot on `next` of the first plain fragment that starts under
    /// `label` and is close below it, among at most [`MAX_WINDOW`] fragments.
    fn below_slot(
        &self,
        label: &SourceFragment,
        next: &Line,
        budget: &mut Budget,
    ) -> Result<Option<usize>, BudgetError> {
        let reach = label.bbox.height().max(next.bbox.height());
        let start = next
            .fragments
            .partition_point(|&f| self.fragments[f].bbox.left < label.bbox.left - reach);
        for (n, &f) in next.fragments[start..].iter().enumerate() {
            let v = &self.fragments[f];
            if n >= MAX_WINDOW || v.bbox.left > label.bbox.left + reach {
                break;
            }
            budget.spend(1)?;
            let height = label.bbox.height().max(v.bbox.height());
            if self.kind(f) == Kind::Plain
                && (v.bbox.left - label.bbox.left).abs() <= height
                && v.bbox.top - label.bbox.bottom <= height
                && v.bbox.top >= label.bbox.bottom - 0.5 * height
            {
                return Ok(Some(start + n));
            }
        }
        Ok(None)
    }

    /// Whether the label on line `index` (with no cell to its right, over a
    /// value with none either) sits in the label column of a label/value
    /// grid: the row just above it or the row just below its value starts a
    /// fragment at the label's left edge that has a cell to its right.
    fn in_label_column(
        &self,
        index: usize,
        label: &SourceFragment,
        height: f64,
        reach: f64,
        budget: &mut Budget,
    ) -> Result<bool, BudgetError> {
        let line = &self.lines[index];
        let value_line = &self.lines[index + 1];
        let above = index
            .checked_sub(1)
            .map(|i| &self.lines[i])
            .filter(|above| above.page == line.page && line.bbox.top - above.bbox.bottom <= reach);
        let below = self.lines.get(index + 2).filter(|below| {
            below.page == line.page && below.bbox.top - value_line.bbox.bottom <= reach
        });
        for neighbour in [above, below].into_iter().flatten() {
            budget.spend(1)?;
            // Fragments are sorted by left edge: the first one at or after
            // `left - height` is the only candidate for alignment.
            let at = neighbour
                .fragments
                .partition_point(|&f| self.fragments[f].bbox.left < label.bbox.left - height);
            let aligned = neighbour
                .fragments
                .get(at)
                .is_some_and(|&f| self.fragments[f].bbox.left <= label.bbox.left + height);
            if aligned && at + 1 < neighbour.fragments.len() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Data rows in the column of the cell at `slot` on line `first`: the
    /// row count (blank and crowded rows included, at most
    /// [`MAX_COLUMN_ROWS`]) and the plain cells of the non-blank rows, each
    /// with whether it is [`Page::trailed`] ([`Pair::split`]). Rows
    /// follow within `reach` of the previous row; a row with no cell in the
    /// column counts only when a neighbouring cell of the first row has one
    /// in it. A `label：` or `label：value` cell ends the column.
    fn data_rows(
        &self,
        first: usize,
        slot: usize,
        height: f64,
        reach: f64,
        budget: &mut Budget,
    ) -> Result<(usize, Vec<(usize, bool)>), BudgetError> {
        let first_line = &self.lines[first];
        let cell = &self.fragments[first_line.fragments[slot]];
        let neighbours: Vec<Rect> = [slot.checked_sub(1), Some(slot + 1)]
            .into_iter()
            .flatten()
            .filter_map(|p| first_line.fragments.get(p))
            .map(|&f| self.fragments[f].bbox)
            .collect();
        let mut rows = 1;
        let mut cells = vec![(first_line.fragments[slot], self.trailed(first_line, slot))];
        let mut previous = first_line.bbox;
        for line in &self.lines[first + 1..] {
            if rows >= MAX_COLUMN_ROWS
                || line.page != first_line.page
                || line.bbox.top - previous.bottom > reach
            {
                break;
            }
            budget.spend(1)?;
            match self.column_cell(line, cell.bbox, height, budget)? {
                Cell::Found(at) if self.kind(line.fragments[at]) == Kind::Plain => {
                    cells.push((line.fragments[at], self.trailed(line, at)));
                }
                Cell::Found(_) => break,
                Cell::Crowded => {}
                Cell::Blank => {
                    let mut aligned = false;
                    for &column in &neighbours {
                        if self.column_cell(line, column, height, budget)? != Cell::Blank {
                            aligned = true;
                            break;
                        }
                    }
                    if !aligned {
                        break;
                    }
                }
            }
            rows += 1;
            previous = line.bbox;
        }
        Ok((rows, cells))
    }

    /// The first fragment of `line` in the column of `column`, among at most
    /// [`MAX_WINDOW`] fragments.
    fn column_cell(
        &self,
        line: &Line,
        column: Rect,
        tolerance: f64,
        budget: &mut Budget,
    ) -> Result<Cell, BudgetError> {
        // Cells of one line do not overlap, so only the fragment just before
        // the search window can reach into the column from the left.
        let start = line
            .fragments
            .partition_point(|&f| self.fragments[f].bbox.left < column.left - tolerance)
            .saturating_sub(1);
        for (n, &f) in line.fragments[start..].iter().enumerate() {
            let bbox = self.fragments[f].bbox;
            if bbox.left > column.right + tolerance {
                break;
            }
            if n >= MAX_WINDOW {
                return Ok(Cell::Crowded);
            }
            budget.spend(1)?;
            if same_column(column, bbox, tolerance) {
                return Ok(Cell::Found(start + n));
            }
        }
        Ok(Cell::Blank)
    }
}

/// Two cells of one column: left or right edges within `tolerance`, or
/// overlapping horizontally (right-aligned numbers under a header).
fn same_column(a: Rect, b: Rect, tolerance: f64) -> bool {
    (a.left - b.left).abs() <= tolerance
        || (a.right - b.right).abs() <= tolerance
        || a.left.max(b.left) < a.right.min(b.right)
}

fn usable(f: &SourceFragment) -> bool {
    f.quality == Quality::Usable
        && f.orientation == Orientation::Deg0
        && !f.raw_text.trim().is_empty()
        && f.bbox.validate().is_ok()
}

fn group_lines(
    fragments: &[SourceFragment],
    budget: &mut Budget,
) -> Result<Vec<Line>, BudgetError> {
    budget.spend(fragments.len() as u64)?;
    let mut order: Vec<usize> = (0..fragments.len())
        .filter(|&i| usable(&fragments[i]))
        .collect();
    // Sorting (here and per line below) compares O(n log n) pairs.
    let n = order.len() as u64;
    let comparisons = n * u64::from(u64::BITS - n.leading_zeros());
    budget.spend(comparisons)?;
    order.sort_by(|&a, &b| {
        let (fa, fb) = (&fragments[a], &fragments[b]);
        fa.page
            .cmp(&fb.page)
            .then(fa.bbox.top.total_cmp(&fb.bbox.top))
            .then(fa.bbox.left.total_cmp(&fb.bbox.left))
    });
    let mut lines: Vec<Line> = Vec::new();
    for i in order {
        let f = &fragments[i];
        match lines.last_mut() {
            Some(line) if line.page == f.page && same_band(line.bbox, f.bbox) => {
                line.fragments.push(i);
                line.bbox = union(line.bbox, f.bbox);
            }
            _ => lines.push(Line {
                page: f.page,
                fragments: vec![i],
                bbox: f.bbox,
            }),
        }
    }
    budget.spend(comparisons)?;
    for line in &mut lines {
        line.fragments
            .sort_by(|&a, &b| fragments[a].bbox.left.total_cmp(&fragments[b].bbox.left));
    }
    Ok(lines)
}

/// Vertical overlap of at least half the smaller height.
fn same_band(a: Rect, b: Rect) -> bool {
    let overlap = a.bottom.min(b.bottom) - a.top.max(b.top);
    overlap >= 0.5 * a.height().min(b.height())
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

/// Estimated advance in em: wide East Asian characters 1, others 0.5.
fn char_em(c: char) -> f64 {
    let wide = matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD);
    if wide { 1.0 } else { 0.5 }
}

/// A whitespace split of a plain fragment: where the label ends and where
/// the value starts (byte offsets), each with the estimated advance in em
/// before it.
type Split = [(usize, f64); 2];

/// Spacing that may separate a label from its value, and the words of a
/// value ([`Binding::Whitespace`]): ASCII space, tab and the ideographic
/// space U+3000. Other whitespace (no-break or em spaces) does not separate.
fn is_spacing(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{3000}')
}

/// Most bytes [`Page::label_splits`] reads from one fragment: a label of
/// [`MAX_SPACED_LABEL_BYTES`], a spacing run of as many bytes and the first
/// character of the value.
const MAX_SPACED_SCAN_BYTES: usize = 2 * MAX_SPACED_LABEL_BYTES + 4;

/// The words of `text` (maximal runs of non-spacing characters, see
/// [`is_spacing`]) as byte ranges.
fn words(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    text.split_inclusive(is_spacing).filter_map(move |piece| {
        let start = at;
        at += piece.len();
        // Only the last character of a piece can be spacing.
        let word = piece.strip_suffix(is_spacing).unwrap_or(piece);
        (!word.is_empty()).then_some((start, start + word.len()))
    })
}

impl Page<'_> {
    /// [`Binding::Whitespace`] hypotheses for the plain fragment `f` on line
    /// `line` (see [`analyze_with_labels`]): one per split in `splits[f]`
    /// whose value holds no label ([`Page::label_in_value`]), with the rest
    /// of the fragment as the value. `splits` holds every fragment's
    /// [`Page::label_splits`], computed once.
    fn spaced(
        &self,
        line: usize,
        position: usize,
        splits: &[Vec<Split>],
        is_label: &mut dyn FnMut(&str) -> bool,
        pairs: &mut Pairs,
        budget: &mut Budget,
    ) -> Result<(), LayoutError> {
        let f = self.lines[line].fragments[position];
        let label_at = self.label_in_value(f, &splits[f], is_label, budget)?;
        let kept = splits[f]
            .iter()
            .filter(|[_, value_start]| label_at.is_none_or(|at| at < value_start.0));
        let mut kept = kept.peekable();
        if kept.peek().is_none() {
            return Ok(());
        }
        // The value ends before a line that is one plain fragment starting
        // with a label and spacing.
        let mut stop = (line + 1 + MAX_VALUE_LINES).min(self.lines.len());
        for next in line + 1..stop {
            budget.spend(1)?;
            if let [g] = self.lines[next].fragments[..]
                && !splits[g].is_empty()
            {
                stop = next;
                break;
            }
        }
        let page = Page {
            fragments: self.fragments,
            measures: self.measures,
            lines: &self.lines[..stop],
        };
        let fragment = &self.fragments[f];
        let measure = &self.measures[f];
        let end = (measure.at[END], measure.em[END]);
        for &[label_end, value_start] in kept {
            let label = page.part(f, (measure.at[START], measure.em[START]), label_end, budget)?;
            let first = page.part(f, value_start, end, budget)?;
            let height = fragment.bbox.height();
            let column = Column {
                from: fragment.bbox.left - 0.5 * height,
                to: first.bbox.left + 1.5 * height,
                label: None,
            };
            if let Some((value, split)) = page.value(line, position, first, column, budget)? {
                pairs.push(Pair {
                    page: self.lines[line].page,
                    label,
                    value,
                    placement: Placement::Inline,
                    binding: Binding::Whitespace,
                    split,
                })?;
            }
        }
        Ok(())
    }

    /// Splits of the plain fragment `f` at one of its first
    /// [`MAX_SPACED_SPLITS`] spacing runs ([`is_spacing`]) that start within
    /// [`MAX_SPACED_LABEL_BYTES`] of its text and are at most as many bytes
    /// long, where `is_label` accepts the text before the run. A longer run
    /// ends the search. Reads at most [`MAX_SPACED_SCAN_BYTES`] of the text,
    /// charged before the scan, and charges each `is_label` call its bytes
    /// before the call.
    fn label_splits(
        &self,
        f: usize,
        is_label: &mut dyn FnMut(&str) -> bool,
        budget: &mut Budget,
    ) -> Result<Vec<Split>, BudgetError> {
        let text = self.fragments[f].raw_text.as_str();
        let measure = &self.measures[f];
        let (start, end) = (measure.at[START], measure.at[END]);
        budget.spend((end - start).min(MAX_SPACED_SCAN_BYTES) as u64)?;
        let mut splits = Vec::new();
        let mut em = measure.em[START];
        let mut run: Option<(usize, f64)> = None;
        let mut runs = 0;
        // The text is trimmed, so every spacing run is followed by a value.
        for (i, c) in text[start..end].char_indices() {
            let at = start + i;
            if is_spacing(c) {
                match run {
                    // The label would be too long.
                    None if i > MAX_SPACED_LABEL_BYTES => break,
                    None => run = Some((at, em)),
                    // The run would be too long.
                    Some((run_start, _))
                        if at + c.len_utf8() - run_start > MAX_SPACED_LABEL_BYTES =>
                    {
                        break;
                    }
                    Some(_) => {}
                }
            } else if let Some(label_end) = run.take() {
                budget.spend((label_end.0 - start) as u64)?;
                if is_label(&text[start..label_end.0]) {
                    splits.push([label_end, (at, em)]);
                }
                runs += 1;
                if runs == MAX_SPACED_SPLITS {
                    break;
                }
            } else if i >= MAX_SPACED_LABEL_BYTES {
                break;
            }
            em += char_em(c);
        }
        Ok(splits)
    }

    /// The last word start in the values of `splits` (the rest of the plain
    /// fragment `f` from its first split's value) at which a label starts:
    /// one of the next [`MAX_SPACED_SPLITS`] runs of words from there, up to
    /// [`MAX_SPACED_LABEL_BYTES`] long and passed with its spacing, that
    /// `is_label` accepts (so `Last Name` is tried as a whole). A split whose
    /// value starts at or before it holds a label. Stops at the first label in
    /// every value. Charges the rest's bytes before the scan and each
    /// `is_label` call its bytes before the call: at most about ten units per
    /// byte of the rest.
    fn label_in_value(
        &self,
        f: usize,
        splits: &[Split],
        is_label: &mut dyn FnMut(&str) -> bool,
        budget: &mut Budget,
    ) -> Result<Option<usize>, BudgetError> {
        let (Some([_, (from, _)]), Some([_, (last, _)])) = (splits.first(), splits.last()) else {
            return Ok(None);
        };
        let text = &self.fragments[f].raw_text[..self.measures[f].at[END]];
        budget.spend((text.len() - from) as u64)?;
        let mut found = None;
        // Starts of the latest words whose run up to the current word may
        // still be a label.
        let mut starts: VecDeque<usize> = VecDeque::with_capacity(MAX_SPACED_SPLITS);
        for (start, end) in words(&text[*from..]) {
            let (start, end) = (from + start, from + end);
            if starts.len() == MAX_SPACED_SPLITS {
                starts.pop_front();
            }
            starts.push_back(start);
            starts.retain(|&s| end - s <= MAX_SPACED_LABEL_BYTES);
            for &s in &starts {
                budget.spend((end - s) as u64)?;
                if is_label(&text[s..end]) {
                    if s >= *last {
                        return Ok(Some(s));
                    }
                    found = found.max(Some(s));
                }
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            orientation: Orientation::Deg0,
        }
    }

    fn analyze(fragments: &[SourceFragment]) -> Layout {
        super::analyze(fragments, &LayoutLimits::default()).unwrap()
    }

    fn texts(fragments: &[SourceFragment], spans: &[Span]) -> Vec<String> {
        spans
            .iter()
            .map(|s| {
                let f = fragments.iter().find(|f| f.id == s.fragment_id).unwrap();
                f.raw_text[s.start..s.end].to_owned()
            })
            .collect()
    }

    /// Layout with whitespace splits after any of `labels` (exact text).
    fn spaced(fragments: &[SourceFragment], labels: &[&str]) -> Layout {
        let mut is_label = |text: &str| labels.contains(&text);
        let mut budget = Budget::new(u64::MAX, None);
        analyze_with_labels(fragments, usize::MAX, Some(&mut is_label), &mut budget).unwrap()
    }

    fn values(fragments: &[SourceFragment], layout: &Layout) -> Vec<(String, Vec<String>)> {
        layout
            .pairs
            .iter()
            .filter(|p| p.binding == Binding::Whitespace)
            .map(|p| {
                (
                    texts(fragments, std::slice::from_ref(&p.label)).remove(0),
                    texts(fragments, &p.value),
                )
            })
            .collect()
    }

    fn find<'a>(layout: &'a Layout, fragments: &[SourceFragment], label: &str) -> Vec<&'a Pair> {
        layout
            .pairs
            .iter()
            .filter(|p| texts(fragments, std::slice::from_ref(&p.label))[0] == label)
            .collect()
    }

    #[test]
    fn inline_delimiter_splits_one_fragment_on_char_boundaries() {
        // Coordinates observed from an embedded-TrueType fixture.
        let fragments = [
            frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
            frag("b", "請求番号： INV-0012", [60.0, 139.0, 163.4, 150.0]),
            frag("c", "Invoice No: AB-778", [60.0, 159.0, 154.8, 170.0]),
        ];
        let layout = analyze(&fragments);
        let inline: Vec<_> = layout
            .pairs
            .iter()
            .filter(|p| p.placement == Placement::Inline)
            .collect();
        assert_eq!(inline.len(), 3);
        for pair in &inline {
            assert_eq!(pair.label.precision, Precision::Estimated);
            assert_eq!(pair.value[0].precision, Precision::Estimated);
        }
        assert_eq!(texts(&fragments, &[inline[0].label.clone()]), ["請求番号"]);
        assert_eq!(texts(&fragments, &inline[0].value), ["INV-0012"]);
        assert_eq!(texts(&fragments, &inline[1].value), ["INV-0012"]);
        assert_eq!(
            texts(&fragments, &[inline[2].label.clone()]),
            ["Invoice No"]
        );
        assert_eq!(texts(&fragments, &inline[2].value), ["AB-778"]);
        // Inline pairs are complete; they are never read as labels or values of other pairs.
        assert_eq!(layout.pairs.len(), 3);
        // True value start is 115.0 for this font; the estimate stays within 0.2 em.
        let value = &inline[0].value[0].bbox;
        assert!((value.left - 115.0).abs() < 0.2 * 11.0, "{}", value.left);
        assert!((value.right - 161.4).abs() < 1e-9);
    }

    #[test]
    fn times_urls_and_empty_sides_are_not_split() {
        let kind = |text: &str| classify(&frag("x", text, [0.0, 0.0, 100.0, 10.0]));
        for text in ["9:00 〜 18:00", "9：00〜18：00", "https://example.invalid"] {
            assert_eq!(kind(text), Kind::Plain, "{text}");
        }
        assert_eq!(kind("備考："), Kind::Label);
        assert_eq!(kind("：値"), Kind::Opaque);
        let fragments = [frag("x", "時間：9:00", [0.0, 0.0, 100.0, 10.0])];
        let layout = analyze(&fragments);
        assert_eq!(layout.pairs.len(), 1);
        assert_eq!(layout.pairs[0].value[0].start, "時間：".len());
        assert_eq!(texts(&fragments, &layout.pairs[0].value), ["9:00"]);
    }

    #[test]
    fn right_values_and_multiline_continuation() {
        let fragments = [
            frag("l1", "住所", [60.0, 119.0, 82.0, 130.0]),
            frag(
                "v1",
                "東京都架空区サンプル町1-2-3",
                [200.0, 119.0, 346.7, 130.0],
            ),
            frag("v2", "テストビル5階", [200.0, 135.0, 272.1, 146.0]),
            frag("l2", "備考", [60.0, 165.0, 82.0, 176.0]),
            frag("v3", "午前中の配達を希望。", [200.0, 165.0, 310.0, 176.0]),
            frag(
                "v4",
                "不在時は宅配ボックスへ。",
                [200.0, 181.0, 332.0, 192.0],
            ),
        ];
        let layout = analyze(&fragments);
        assert_eq!(layout.lines.len(), 4);
        let address = find(&layout, &fragments, "住所");
        assert_eq!(address.len(), 1);
        assert_eq!(address[0].placement, Placement::Right);
        assert_eq!(
            texts(&fragments, &address[0].value),
            ["東京都架空区サンプル町1-2-3", "テストビル5階"]
        );
        assert!(
            address[0]
                .value
                .iter()
                .all(|s| s.precision == Precision::Run)
        );
        let note = find(&layout, &fragments, "備考");
        assert_eq!(
            texts(&fragments, &note[0].value),
            ["午前中の配達を希望。", "不在時は宅配ボックスへ。"]
        );
    }

    #[test]
    fn stacked_label_rows_do_not_merge() {
        let fragments = [
            frag("a", "小計", [330.0, 119.0, 352.0, 130.0]),
            frag("b", "¥11,000", [480.2, 119.0, 520.0, 130.0]),
            frag("c", "消費税(10%)", [330.0, 139.0, 392.3, 150.0]),
            frag("d", "¥1,100", [486.4, 139.0, 520.0, 150.0]),
            frag("e", "合計", [330.0, 159.0, 352.0, 170.0]),
            frag("f", "¥12,100", [480.2, 159.0, 520.0, 170.0]),
        ];
        let layout = analyze(&fragments);
        for (label, value) in [
            ("小計", "¥11,000"),
            ("消費税(10%)", "¥1,100"),
            ("合計", "¥12,100"),
        ] {
            let right: Vec<_> = find(&layout, &fragments, label)
                .into_iter()
                .filter(|p| p.placement == Placement::Right)
                .collect();
            assert_eq!(right.len(), 1, "{label}");
            assert_eq!(texts(&fragments, &right[0].value), [value]);
        }
    }

    #[test]
    fn value_below_label() {
        let fragments = [
            frag("a", "お届け先氏名", [60.0, 121.0, 114.0, 130.0]),
            frag("b", "お届け予定日", [300.0, 121.0, 354.0, 130.0]),
            frag("c", "佐藤 花子", [60.0, 135.0, 115.6, 148.0]),
            frag("d", "2026/12/01", [300.0, 135.0, 365.0, 148.0]),
        ];
        let layout = analyze(&fragments);
        for (label, value) in [
            ("お届け先氏名", "佐藤 花子"),
            ("お届け予定日", "2026/12/01"),
        ] {
            let below: Vec<_> = find(&layout, &fragments, label)
                .into_iter()
                .filter(|p| p.placement == Placement::Below)
                .collect();
            assert_eq!(below.len(), 1, "{label}");
            assert_eq!(texts(&fragments, &below[0].value), [value]);
        }
        // Separate rows with a full line gap are not read as label-above-value.
        let rows = [
            frag("a", "会社名", [60.0, 119.0, 93.0, 130.0]),
            frag("b", "担当者", [60.0, 143.0, 93.0, 154.0]),
        ];
        assert!(analyze(&rows).pairs.is_empty());
    }

    /// `n` one-fragment lines stacked in one column, 10 pt high and 12 pt apart.
    fn stacked(n: usize) -> Vec<SourceFragment> {
        (0..n)
            .map(|i| {
                let top = 12.0 * i as f64;
                frag(&format!("f{i}"), "備考", [60.0, top, 82.0, top + 10.0])
            })
            .collect()
    }

    #[test]
    fn stacked_lines_are_bounded_columns() {
        // Before (first round): every line was a label whose value ran over
        // the following lines and was dropped past MAX_VALUE_LINES. Now a
        // stacked list is a column: at most MAX_COLUMN_ROWS pairs per label.
        let n = 3000;
        let fragments = stacked(n);
        let layout = analyze(&fragments);
        assert!(
            layout.pairs.len() <= n * MAX_COLUMN_ROWS,
            "{}",
            layout.pairs.len()
        );
        assert!(layout.pairs.iter().all(|p| p.value.len() == 1));
        // Only the second-to-last line has a single data row left below it.
        let below: Vec<_> = layout
            .pairs
            .iter()
            .filter(|p| p.placement != Placement::Column)
            .collect();
        assert_eq!(below.len(), 1);
        assert_eq!(below[0].placement, Placement::Below);
        assert_eq!(below[0].label.fragment_id, format!("f{}", n - 2));
        let first: Vec<&str> = layout
            .pairs
            .iter()
            .filter(|p| p.label.fragment_id == "f0")
            .map(|p| p.value[0].fragment_id.as_str())
            .collect();
        assert_eq!(first, ["f1", "f2", "f3", "f4", "f5"]);
    }

    #[test]
    fn right_values_continue_up_to_the_line_cap() {
        // A label at x=60 with value lines at x=200, 12 pt apart.
        let column = |lines: usize| {
            let mut fragments = vec![frag("label", "備考", [60.0, 0.0, 82.0, 10.0])];
            fragments.extend((0..lines).map(|i| {
                let top = 12.0 * i as f64;
                frag(&format!("v{i}"), "行", [200.0, top, 210.0, top + 10.0])
            }));
            fragments
        };
        let right = |fragments: &[SourceFragment]| {
            analyze(fragments)
                .pairs
                .into_iter()
                .find(|p| p.label.fragment_id == "label" && p.placement == Placement::Right)
        };
        // A value with exactly MAX_VALUE_LINES lines keeps all of them.
        let fragments = column(MAX_VALUE_LINES);
        assert_eq!(right(&fragments).unwrap().value.len(), MAX_VALUE_LINES);
        // One more line and that label has no value at all (never truncated).
        assert!(right(&column(MAX_VALUE_LINES + 1)).is_none());
    }

    #[test]
    fn pair_limit_is_a_typed_error() {
        let fragments = [
            frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
            frag("b", "発行日：2026年11月8日", [60.0, 139.0, 179.8, 150.0]),
            frag("c", "TEL：03-1234-5678", [60.0, 159.0, 150.0, 170.0]),
        ];
        let limits = |max_pairs| LayoutLimits {
            max_pairs,
            ..LayoutLimits::default()
        };
        assert_eq!(
            super::analyze(&fragments, &limits(3)).unwrap().pairs.len(),
            3
        );
        assert_eq!(
            super::analyze(&fragments, &limits(2)),
            Err(LayoutError::LimitExceeded)
        );
        assert_eq!(LayoutLimits::default().max_pairs, DEFAULT_MAX_PAIRS);
    }

    /// `n` plain fragments on one line and `n` `label：` fragments on the
    /// line below, all starting at the same x (overlapping text).
    fn crowded(n: usize) -> Vec<SourceFragment> {
        (0..n)
            .flat_map(|i| {
                [
                    frag(&format!("a{i}"), "あ", [60.0, 100.0, 70.0, 110.0]),
                    frag(&format!("b{i}"), "い：", [60.0, 112.0, 75.0, 122.0]),
                ]
            })
            .collect()
    }

    #[test]
    fn crowded_lines_stay_bounded_and_honour_budget_and_deadline() {
        // Before: every label scanned the whole line below (n^2 fragment
        // checks, 1.8 s for n = 40,000 in release). Now each search examines
        // at most MAX_WINDOW fragments, and the work is counted.
        let fragments = crowded(20_000);
        let mut budget = Budget::new(u64::MAX, None);
        let layout = analyze_within(&fragments, usize::MAX, &mut budget).unwrap();
        let used = u64::MAX - budget.remaining();
        // Grouping sorts (n log n), text and spans add a few units per fragment.
        let n = fragments.len() as u64;
        assert!(used < n * 64, "{used}");
        assert!(layout.pairs.iter().all(|p| p.placement == Placement::Right));
        let limits = |max_work, deadline| LayoutLimits {
            max_work,
            deadline,
            ..LayoutLimits::default()
        };
        assert_eq!(
            super::analyze(&fragments, &limits(used - 1, None)),
            Err(LayoutError::BudgetExceeded)
        );
        assert_eq!(
            super::analyze(&fragments, &limits(u64::MAX, Some(Instant::now()))),
            Err(LayoutError::DeadlineExceeded)
        );
        // A crowded window counts as a data row of a column (here the second
        // row of 金額: more than MAX_WINDOW fragments overlap the column's
        // search window without lining up with it).
        let mut table = vec![
            frag("h1", "金額", [60.0, 100.0, 80.0, 110.0]),
            frag("h2", "数量", [200.0, 100.0, 220.0, 110.0]),
            frag("a1", "¥300", [60.0, 118.0, 80.0, 128.0]),
            frag("q1", "3", [200.0, 118.0, 205.0, 128.0]),
            frag("q2", "2", [200.0, 136.0, 205.0, 146.0]),
        ];
        table.extend(
            (0..=MAX_WINDOW).map(|i| frag(&format!("x{i}"), "¥1", [85.0, 136.0, 95.0, 146.0])),
        );
        assert_eq!(
            columns(&table, &["金額", "数量"]),
            [vec!["¥300"], vec!["3", "2"]]
        );
    }

    #[test]
    fn continuation_lines_may_share_their_line_with_another_column() {
        // A two-column layout: the left value continues on a line that also
        // holds the right column's `請求番号：INV-1`. Before: the line had two
        // fragments, so the value ended after its first line (truncated).
        let fragments = [
            frag("a", "住所：東京都千代田区", [60.0, 100.0, 160.0, 110.0]),
            frag("b", "請求日：2026/11/08", [300.0, 100.0, 390.0, 110.0]),
            frag("c", "丸の内1-1-1 サンプルビル", [90.0, 112.0, 210.0, 122.0]),
            frag("d", "請求番号：INV-1", [300.0, 112.0, 375.0, 122.0]),
            frag("e", "担当", [60.0, 150.0, 80.0, 160.0]),
            frag("f", "山田 太郎", [200.0, 150.0, 250.0, 160.0]),
            frag("g", "経理部", [360.0, 150.0, 390.0, 160.0]),
            frag("h", "営業部", [20.0, 162.0, 50.0, 172.0]),
            frag("i", "佐藤 花子", [200.0, 162.0, 250.0, 172.0]),
        ];
        let layout = analyze(&fragments);
        let address = find(&layout, &fragments, "住所");
        assert_eq!(
            texts(&fragments, &address[0].value),
            ["東京都千代田区", "丸の内1-1-1 サンプルビル"]
        );
        // The right column's value ends at its own column's next label.
        let issued = find(&layout, &fragments, "請求日");
        assert_eq!(texts(&fragments, &issued[0].value), ["2026/11/08"]);
        // A right value continues past text left of its label's column.
        let person = find(&layout, &fragments, "担当")
            .into_iter()
            .find(|p| p.placement == Placement::Right)
            .unwrap();
        assert_eq!(texts(&fragments, &person.value), ["山田 太郎", "佐藤 花子"]);
        // A line with a fragment in the label's column is the next row, even
        // when its value lines up with this one (equal-width amounts).
        let rows = [
            frag("a", "小計", [330.0, 100.0, 352.0, 110.0]),
            frag("b", "¥11,000", [480.2, 100.0, 520.0, 110.0]),
            frag("c", "合計", [330.0, 112.0, 352.0, 122.0]),
            frag("d", "¥12,100", [480.2, 112.0, 520.0, 122.0]),
            // Right-aligned labels share the label column by their right edge.
            frag("e", "住所：", [70.0, 140.0, 100.0, 150.0]),
            frag("f", "東京都架空区1-2-3", [200.0, 140.0, 290.0, 150.0]),
            frag("g", "電話番号", [60.0, 152.0, 100.0, 162.0]),
            frag("h", "03-1234-5678", [200.0, 152.0, 260.0, 162.0]),
        ];
        let layout = analyze(&rows);
        for (label, value) in [("小計", "¥11,000"), ("住所", "東京都架空区1-2-3")] {
            let right = find(&layout, &rows, label)
                .into_iter()
                .find(|p| p.placement == Placement::Right)
                .unwrap();
            assert_eq!(texts(&rows, &right.value), [value], "{label}");
        }
    }

    #[test]
    fn a_plain_fragment_after_the_value_marks_it_split() {
        // Before: 住所 → 〒100-0001 and 氏名 → 山田, with nothing marking the
        // rest of the value in the next fragment.
        let fragments = [
            frag("a", "住所：", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "〒100-0001", [100.0, 100.0, 150.0, 110.0]),
            frag("c", "東京都千代田区千代田1-1", [160.0, 100.0, 290.0, 110.0]),
            frag("d", "氏名：山田", [60.0, 124.0, 110.0, 134.0]),
            frag("e", "太郎", [120.0, 124.0, 140.0, 134.0]),
            frag("f", "電話：03-1234-5678", [60.0, 148.0, 150.0, 158.0]),
            frag("g", "FAX：03-1234-5679", [160.0, 148.0, 245.0, 158.0]),
            frag("h", "件名：制作費", [60.0, 172.0, 120.0, 182.0]),
            frag("i", "以上", [150.0, 172.0, 170.0, 182.0]),
            frag("j", "担当", [60.0, 196.0, 80.0, 206.0]),
            frag("k", "山田", [120.0, 196.0, 140.0, 206.0]),
            frag("l", "経理部", [150.0, 196.0, 180.0, 206.0]),
        ];
        let layout = analyze(&fragments);
        let split = |label: &str| {
            let pairs = find(&layout, &fragments, label);
            assert_eq!(pairs.len(), 1, "{label}");
            assert_eq!(pairs[0].split.len(), 1, "{label}");
            (
                texts(&fragments, &pairs[0].value).join("\n"),
                pairs[0].split[0],
            )
        };
        assert_eq!(split("住所"), ("〒100-0001".into(), true));
        assert_eq!(split("氏名"), ("山田".into(), true));
        assert_eq!(split("担当"), ("山田".into(), true));
        // A following `label：value` fragment is the next pair, not the rest
        // of the value.
        assert_eq!(split("電話"), ("03-1234-5678".into(), false));
        // Changed: a plain fragment at any distance (here 3 em). Before:
        // only closer than 3 em, so `氏名：` `山田` and `太郎` 3.1 em away
        // was one value `山田`.
        assert_eq!(split("件名"), ("制作費".into(), true));
        let far = [
            frag("a", "氏名：", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "山田", [100.0, 100.0, 120.0, 110.0]),
            frag("c", "太郎", [151.0, 100.0, 171.0, 110.0]),
        ];
        let layout = analyze(&far);
        assert_eq!(find(&layout, &far, "氏名")[0].split, [true]);
        // Continuation lines count too.
        let lines = [
            frag("a", "備考：", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "午前中の配達を希望。", [100.0, 100.0, 200.0, 110.0]),
            frag("c", "不在時は", [100.0, 112.0, 140.0, 122.0]),
            frag("d", "宅配ボックスへ。", [150.0, 112.0, 230.0, 122.0]),
        ];
        let layout = analyze(&lines);
        let note = find(&layout, &lines, "備考");
        assert_eq!(
            texts(&lines, &note[0].value),
            ["午前中の配達を希望。", "不在時は"]
        );
        // Changed: one flag per line (before: one flag for the value).
        assert_eq!(note[0].split, [false, true]);
    }

    #[test]
    fn continuation_stops_at_label_lines() {
        let fragments = [
            frag("a", "住所", [60.0, 100.0, 82.0, 110.0]),
            frag("b", "東京都架空区1-2-3", [200.0, 100.0, 300.0, 110.0]),
            frag("c", "テストビル5階", [200.0, 112.0, 272.0, 122.0]),
            frag("d", "電話番号：", [200.0, 124.0, 255.0, 134.0]),
            frag("e", "03-1234-5678", [200.0, 136.0, 260.0, 146.0]),
        ];
        let layout = analyze(&fragments);
        let address = find(&layout, &fragments, "住所");
        assert_eq!(
            texts(&fragments, &address[0].value),
            ["東京都架空区1-2-3", "テストビル5階"]
        );
        // `label：` is a label, never a value.
        assert!(
            layout
                .pairs
                .iter()
                .all(|p| { p.value.iter().all(|s| s.fragment_id != "d") })
        );
    }

    #[test]
    fn value_below_must_not_be_the_next_rows_label() {
        // Label column x=60, value column x=200, rows 18 pt apart, 10 pt font.
        // 部署 has a blank value cell; 役職 is the next row's label with 課長.
        let fragments = [
            frag("a", "会社名", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "株式会社サンプル", [200.0, 100.0, 280.0, 110.0]),
            frag("c", "部署", [60.0, 118.0, 80.0, 128.0]),
            frag("d", "役職", [60.0, 136.0, 80.0, 146.0]),
            frag("e", "課長", [200.0, 136.0, 220.0, 146.0]),
        ];
        let layout = analyze(&fragments);
        // Before: 部署 → 役職 (Below).
        assert!(find(&layout, &fragments, "部署").is_empty());
        // 会社名 has a value cell of its own, so 部署 is not its value either.
        let company = find(&layout, &fragments, "会社名");
        assert_eq!(company.len(), 1);
        assert_eq!(company[0].placement, Placement::Right);
        // A two-column header row over one value row is still read below.
        let fragments = [
            frag("a", "部署", [60.0, 118.0, 80.0, 128.0]),
            frag("b", "役職", [200.0, 118.0, 220.0, 128.0]),
            frag("c", "経理部", [60.0, 132.0, 90.0, 142.0]),
            frag("d", "課長", [200.0, 132.0, 220.0, 142.0]),
        ];
        let layout = analyze(&fragments);
        let below = find(&layout, &fragments, "部署")
            .into_iter()
            .find(|p| p.placement == Placement::Below)
            .unwrap();
        assert_eq!(texts(&fragments, &below.value), ["経理部"]);
    }

    #[test]
    fn label_column_with_two_blank_value_cells_has_no_value_below() {
        // 部署 and 役職 both have blank value cells. Geometrically 部署 over
        // 役職 is label-above-value; the rows around them (会社名 and 担当者
        // with values to their right) mark x=60 as a label column.
        let fragments = [
            frag("a", "会社名", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "株式会社サンプル", [180.0, 100.0, 260.0, 110.0]),
            frag("c", "部署", [60.0, 118.0, 80.0, 128.0]),
            frag("d", "役職", [60.0, 136.0, 80.0, 146.0]),
            frag("e", "担当者", [60.0, 154.0, 90.0, 164.0]),
            frag("f", "佐藤 花子", [180.0, 154.0, 230.0, 164.0]),
        ];
        let layout = analyze(&fragments);
        // Before: 部署 → 役職 (Below).
        assert!(find(&layout, &fragments, "部署").is_empty());
        assert!(find(&layout, &fragments, "役職").is_empty());
        // Either neighbour alone is enough: blank rows at the top or bottom.
        for range in [2..6, 0..4] {
            let part = &fragments[range];
            assert!(find(&analyze(part), part, "部署").is_empty());
        }
        // A label over a value with nothing around stays label-above-value.
        let alone = [
            frag("a", "ご注文番号", [60.0, 140.0, 110.0, 150.0]),
            frag("b", "ORD-55821", [60.0, 154.0, 105.0, 164.0]),
        ];
        let layout = analyze(&alone);
        assert_eq!(layout.pairs.len(), 1);
        assert_eq!(layout.pairs[0].placement, Placement::Below);
    }

    #[test]
    fn header_over_several_rows_is_a_column_not_a_value() {
        let mut fragments = vec![
            frag("h1", "品名", [60.0, 100.0, 80.0, 110.0]),
            frag("h2", "数量", [300.0, 100.0, 320.0, 110.0]),
            frag("h3", "金額", [440.0, 100.0, 460.0, 110.0]),
        ];
        for (row, (item, quantity, amount)) in [
            ("りんご", "3", "¥300"),
            ("みかん", "3", "¥500"),
            ("ぶどう", "2", "¥800"),
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
        let layout = analyze(&fragments);
        for (label, cells) in [
            ("品名", ["りんご", "みかん", "ぶどう"]),
            ("数量", ["3", "3", "2"]),
            ("金額", ["¥300", "¥500", "¥800"]),
        ] {
            let pairs = find(&layout, &fragments, label);
            // Before: one Below pair with the first row only.
            assert!(
                pairs.iter().all(|p| p.placement != Placement::Below),
                "{label}"
            );
            let column: Vec<String> = pairs
                .iter()
                .filter(|p| p.placement == Placement::Column)
                .flat_map(|p| texts(&fragments, &p.value))
                .collect();
            assert_eq!(column, cells, "{label}");
        }
        // Right-aligned numbers share a column with their header by overlap.
        let rect = |left, right| Rect {
            left,
            top: 0.0,
            right,
            bottom: 10.0,
        };
        assert!(same_column(rect(300.0, 320.0), rect(315.0, 320.0), 10.0));
        assert!(!same_column(rect(60.0, 80.0), rect(200.0, 220.0), 10.0));
        // A single data row stays an ordinary value below its header.
        let layout = analyze(&fragments[..6]);
        let below = find(&layout, &fragments[..6], "品名");
        assert!(below.iter().any(|p| p.placement == Placement::Below));
    }

    /// Column cells (as text) of each header in `labels`.
    fn columns(fragments: &[SourceFragment], labels: &[&str]) -> Vec<Vec<String>> {
        let layout = analyze(fragments);
        labels
            .iter()
            .map(|label| {
                let pairs = find(&layout, fragments, label);
                assert!(
                    pairs.iter().all(|p| p.placement != Placement::Below),
                    "{label}"
                );
                pairs
                    .iter()
                    .filter(|p| p.placement == Placement::Column)
                    .flat_map(|p| texts(fragments, &p.value))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn row_pitch_is_measured_from_the_header() {
        // A 14 pt header over 10 pt rows 24 pt apart: the gap between rows
        // (14 pt) exceeds the row height. Before: the column stopped after
        // the first row, which was then read below the header.
        let mut fragments = vec![
            frag("h1", "品名", [60.0, 86.0, 88.0, 100.0]),
            frag("h2", "数量", [300.0, 86.0, 328.0, 100.0]),
        ];
        for (row, (item, quantity)) in [("りんご", "3"), ("みかん", "2")].into_iter().enumerate()
        {
            let top = 114.0 + 24.0 * row as f64;
            fragments.extend([
                frag(&format!("i{row}"), item, [60.0, top, 90.0, top + 10.0]),
                frag(
                    &format!("q{row}"),
                    quantity,
                    [300.0, top, 305.0, top + 10.0],
                ),
            ]);
        }
        assert_eq!(
            columns(&fragments, &["品名", "数量"]),
            [vec!["りんご", "みかん"], vec!["3", "2"]]
        );
    }

    #[test]
    fn a_blank_cell_still_counts_as_a_data_row() {
        let fragments = [
            frag("h1", "品名", [60.0, 100.0, 80.0, 110.0]),
            frag("h2", "数量", [300.0, 100.0, 320.0, 110.0]),
            frag("h3", "金額", [440.0, 100.0, 460.0, 110.0]),
            frag("i1", "りんご", [60.0, 118.0, 90.0, 128.0]),
            frag("q1", "3", [300.0, 118.0, 305.0, 128.0]),
            frag("a1", "¥300", [440.0, 118.0, 460.0, 128.0]),
            // 数量 blank in the second row.
            frag("i2", "みかん", [60.0, 136.0, 90.0, 146.0]),
            frag("a2", "¥500", [440.0, 136.0, 460.0, 146.0]),
        ];
        // Before: 数量 → 3 (Below), because column detection stopped at the blank cell.
        assert_eq!(columns(&fragments, &["数量"]), [vec!["3"]]);
        // A following line with nothing in this or a neighbouring column ends the rows.
        let unrelated = [
            frag("h", "数量", [300.0, 100.0, 320.0, 110.0]),
            frag("q", "3", [300.0, 118.0, 305.0, 128.0]),
            frag("n", "備考", [60.0, 136.0, 80.0, 146.0]),
        ];
        let layout = analyze(&unrelated);
        assert_eq!(
            find(&layout, &unrelated, "数量")[0].placement,
            Placement::Below
        );
    }

    #[test]
    fn stacked_single_column_lists_are_columns() {
        // Before: 金額 → "¥1,000" + "¥1,000" and 品名 → "りんご" + "みかん" as one
        // multi-line value below the label.
        let fragments = [
            frag("a", "金額", [60.0, 100.0, 80.0, 110.0]),
            frag("b", "¥1,000", [60.0, 118.0, 90.0, 128.0]),
            frag("c", "¥1,000", [60.0, 136.0, 90.0, 146.0]),
            frag("d", "品名", [60.0, 200.0, 80.0, 210.0]),
            frag("e", "りんご", [60.0, 218.0, 90.0, 228.0]),
            frag("f", "みかん", [60.0, 236.0, 90.0, 246.0]),
        ];
        assert_eq!(
            columns(&fragments, &["金額", "品名"]),
            [vec!["¥1,000", "¥1,000"], vec!["りんご", "みかん"]]
        );
    }

    #[test]
    fn several_delimiters_in_one_fragment_are_not_split() {
        let fragments = [
            frag(
                "a",
                "TEL：03-1234-5678 FAX：03-1234-5679",
                [60.0, 100.0, 260.0, 110.0],
            ),
            frag("b", "担当", [300.0, 100.0, 320.0, 110.0]),
        ];
        assert_eq!(classify(&fragments[0]), Kind::Opaque);
        // Before: TEL → "03-1234-5678 FAX：03-1234-5679". Now no hypothesis
        // uses the fragment, neither split nor as a label or value.
        assert!(analyze(&fragments).pairs.is_empty());
    }

    #[test]
    fn right_value_with_its_own_label_is_not_a_value() {
        let fragments = [
            frag("a", "担当者", [60.0, 100.0, 90.0, 110.0]),
            frag("b", "部署：経理部", [200.0, 100.0, 260.0, 110.0]),
            frag("c", "氏名", [60.0, 130.0, 80.0, 140.0]),
            frag("d", "部署：", [200.0, 130.0, 230.0, 140.0]),
        ];
        let layout = analyze(&fragments);
        // Before: 担当者 → "部署：経理部" (Right).
        assert!(find(&layout, &fragments, "担当者").is_empty());
        assert!(find(&layout, &fragments, "氏名").is_empty());
        let department = find(&layout, &fragments, "部署");
        assert_eq!(department.len(), 1);
        assert_eq!(department[0].placement, Placement::Inline);
        assert_eq!(texts(&fragments, &department[0].value), ["経理部"]);
    }

    #[test]
    fn unusable_fragments_are_ignored() {
        let mut fragment = frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]);
        fragment.quality = Quality::Suspect;
        assert_eq!(analyze(&[fragment]), Layout::default());
    }

    #[test]
    fn bindings_tell_delimiters_from_geometry() {
        let fragments = [
            frag("a", "請求日：", [60.0, 100.0, 100.0, 110.0]),
            frag("b", "2026/11/08", [140.0, 100.0, 190.0, 110.0]),
            frag("c", "担当", [60.0, 130.0, 80.0, 140.0]),
            frag("d", "山田 太郎", [200.0, 130.0, 250.0, 140.0]),
            frag("e", "件名：制作費", [60.0, 160.0, 120.0, 170.0]),
            frag("f", "注文番号", [60.0, 190.0, 100.0, 200.0]),
            frag("g", "ORD-1", [60.0, 204.0, 85.0, 214.0]),
        ];
        let layout = analyze(&fragments);
        let binding = |label: &str| {
            let pairs = find(&layout, &fragments, label);
            assert_eq!(pairs.len(), 1, "{label}");
            (pairs[0].placement, pairs[0].binding)
        };
        assert_eq!(binding("請求日"), (Placement::Right, Binding::Delimiter));
        assert_eq!(binding("担当"), (Placement::Right, Binding::Geometric));
        assert_eq!(binding("件名"), (Placement::Inline, Binding::Delimiter));
        assert_eq!(binding("注文番号"), (Placement::Below, Binding::Geometric));
    }

    #[test]
    fn a_far_right_fragment_is_bound_by_position_only() {
        // A blank `氏名：` and a right-aligned company name on its line.
        // Before: the delimiter bound them (氏名 → 株式会社サンプル商事).
        let pair = |left: f64| {
            let fragments = [
                frag("a", "氏名：", [60.0, 100.0, 90.0, 110.0]),
                frag(
                    "b",
                    "株式会社サンプル商事",
                    [left, 100.0, left + 100.0, 110.0],
                ),
            ];
            let pairs = analyze(&fragments).pairs;
            assert_eq!(pairs.len(), 1);
            (pairs[0].placement, pairs[0].binding)
        };
        assert_eq!(pair(435.0), (Placement::Right, Binding::Geometric));
        // Up to MAX_DELIMITER_GAP_EM em (40 pt at 10 pt) the delimiter binds.
        assert_eq!(pair(130.0), (Placement::Right, Binding::Delimiter));
        assert_eq!(pair(130.5), (Placement::Right, Binding::Geometric));
        // The smaller font sets the em. Before: the larger one, so a big
        // right fragment widened the reach of a small `label：` and these
        // were bound by the delimiter (50 pt and 90 pt from a 10 pt label).
        let binding = |value: SourceFragment| {
            let fragments = [frag("a", "氏名：", [60.0, 100.0, 90.0, 110.0]), value];
            analyze(&fragments).pairs[0].binding
        };
        for value in [
            frag("b", "株式会社サンプル商事", [140.0, 98.0, 280.0, 112.0]),
            frag("b", "請求書", [180.0, 86.0, 280.0, 116.0]),
            frag("b", "¥12,100", [140.0, 92.0, 210.0, 110.0]),
        ] {
            assert_eq!(binding(value), Binding::Geometric);
        }
        // A large label and a small value: the small value sets the em too.
        let fragments = [
            frag("a", "合計：", [60.0, 90.0, 120.0, 110.0]),
            frag("b", "¥12,100", [165.0, 100.0, 200.0, 110.0]),
        ];
        assert_eq!(analyze(&fragments).pairs[0].binding, Binding::Geometric);
        assert_eq!(
            binding(frag("b", "¥12,100", [130.0, 96.0, 200.0, 110.0])),
            Binding::Delimiter
        );
    }

    #[test]
    fn inline_values_take_continuation_lines_in_their_column() {
        // `住所：` is 3 full-width characters: the value starts near x=90.
        let pair = |next: SourceFragment| {
            let fragments = [
                frag("a", "住所：東京都架空区1-2-3", [60.0, 100.0, 185.0, 110.0]),
                next,
            ];
            let layout = analyze(&fragments);
            let address = find(&layout, &fragments, "住所");
            assert_eq!(address.len(), 1);
            texts(&fragments, &address[0].value)
        };
        // Before: the inline value was always one line (a truncated address).
        for left in [90.0, 60.0, 100.0] {
            let next = frag("b", "テストビル5階", [left, 112.0, left + 70.0, 122.0]);
            assert_eq!(pair(next), ["東京都架空区1-2-3", "テストビル5階"], "{left}");
        }
        // Lines further right, after a gap or with their own delimiter stay apart.
        for next in [
            frag("b", "テストビル5階", [200.0, 112.0, 270.0, 122.0]),
            frag("b", "テストビル5階", [90.0, 125.0, 160.0, 135.0]),
            frag("b", "電話番号：03-1234-5678", [60.0, 112.0, 170.0, 122.0]),
        ] {
            assert_eq!(pair(next), ["東京都架空区1-2-3"]);
        }
    }

    /// `labels` short labels over their values, then four wide fragments of
    /// `chars` characters with a trailing space under every label's column.
    fn wide(labels: usize, chars: usize) -> Vec<SourceFragment> {
        let mut fragments = Vec::new();
        for i in 0..labels {
            let x = 10.0 + 0.1 * i as f64;
            fragments.push(frag(&format!("l{i}"), "項目", [x, 100.0, x + 20.0, 110.0]));
            fragments.push(frag(&format!("v{i}"), "値", [x, 118.0, x + 10.0, 128.0]));
        }
        for row in 0..4 {
            let top = 136.0 + 18.0 * row as f64;
            let text = format!("{} ", "あ".repeat(chars));
            fragments.push(frag(
                &format!("w{row}"),
                &text,
                [0.0, top, 595.0, top + 10.0],
            ));
        }
        fragments
    }

    #[test]
    fn spans_reuse_fragment_measurements() {
        // Before: every span of a fragment with surrounding spaces re-read
        // the fragment's text, uncharged. Here each label's column rows end
        // in the four wide fragments.
        let fragments = wide(200, 33_000);
        let bytes: u64 = fragments.iter().map(|f| f.raw_text.len() as u64).sum();
        let mut budget = Budget::new(u64::MAX, None);
        let layout = analyze_within(&fragments, usize::MAX, &mut budget).unwrap();
        let used = u64::MAX - budget.remaining();
        let column: Vec<_> = layout
            .pairs
            .iter()
            .filter(|p| p.label.fragment_id == "l0" && p.placement == Placement::Column)
            .map(|p| p.value[0].fragment_id.as_str())
            .collect();
        assert_eq!(column, ["v0", "w0", "w1", "w2", "w3"]);
        assert!(
            layout
                .pairs
                .iter()
                .filter(|p| p.value[0].fragment_id == "w0")
                .all(|p| p.value[0].precision == Precision::Estimated)
        );
        // Text is read twice in all; the rest is a constant per fragment and pair.
        let pairs = layout.pairs.len() as u64;
        let n = fragments.len() as u64;
        assert!(used <= 2 * bytes + 64 * n + 64 * pairs, "{used}");
        // 12 MB of wide text: before, 24 s in a debug build (the text was
        // re-read for each of the 800 column rows); now the text is read twice.
        let fragments = wide(200, 1_000_000);
        let started = Instant::now();
        let mut budget = Budget::new(u64::MAX, None);
        analyze_within(&fragments, usize::MAX, &mut budget).unwrap();
        assert!(started.elapsed().as_secs() < 6, "{:?}", started.elapsed());
        // A deadline stops the analysis promptly.
        let started = Instant::now();
        let limits = LayoutLimits {
            deadline: Some(started + std::time::Duration::from_millis(50)),
            max_work: u64::MAX,
            ..LayoutLimits::default()
        };
        assert_eq!(
            super::analyze(&fragments, &limits),
            Err(LayoutError::DeadlineExceeded)
        );
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    }

    #[test]
    fn whitespace_splits_follow_caller_labels_with_exact_ranges() {
        // 10 em at 10 pt: `発行日` is 3 em, the space 0.5 em.
        let fragments = vec![
            frag("a", "発行日 2026年11月8日", [60.0, 100.0, 160.0, 110.0]),
            frag("b", "合計\u{3000}¥12,100", [60.0, 124.0, 116.0, 134.0]),
            // Two runs joined by the extractor with an inserted space.
            frag("c", "請求番号 INV-0012", [60.0, 148.0, 129.0, 158.0]),
        ];
        // Without labels there is no whitespace hypothesis at all.
        assert!(analyze(&fragments).pairs.is_empty());
        let layout = spaced(&fragments, &["発行日", "合計", "請求番号"]);
        assert_eq!(
            values(&fragments, &layout),
            [
                ("発行日".to_owned(), vec!["2026年11月8日".to_owned()]),
                ("合計".to_owned(), vec!["¥12,100".to_owned()]),
                ("請求番号".to_owned(), vec!["INV-0012".to_owned()]),
            ]
        );
        let pair = &layout.pairs[0];
        assert_eq!(pair.placement, Placement::Inline);
        assert_eq!((pair.label.start, pair.label.end), (0, "発行日".len()));
        let value = &pair.value[0];
        assert_eq!(
            (value.start, value.end),
            ("発行日 ".len(), fragments[0].raw_text.len())
        );
        assert_eq!(
            (pair.label.precision, value.precision),
            (Precision::Estimated, Precision::Estimated)
        );
        assert!((pair.label.bbox.right - 90.0).abs() < 1e-9);
        assert!((value.bbox.left - 95.0).abs() < 1e-9);
        assert!((value.bbox.right - 160.0).abs() < 1e-9);
        // Only an exact label is split: no other text, and never a fragment
        // with a delimiter (inline, `label：` or opaque).
        let fragments = vec![
            frag("a", "発行日：2026年11月8日", [60.0, 100.0, 165.0, 110.0]),
            frag(
                "b",
                "TEL：03-1234-5678 FAX：03-1234-5679",
                [60.0, 124.0, 230.0, 134.0],
            ),
            frag("c", "再発行日 2026年11月9日", [60.0, 148.0, 170.0, 158.0]),
        ];
        let layout = spaced(&fragments, &["発行日", "TEL", "FAX"]);
        assert!(values(&fragments, &layout).is_empty());
        assert_eq!(layout.pairs.len(), 1);
        assert_eq!(layout.pairs[0].binding, Binding::Delimiter);
    }

    #[test]
    fn whitespace_values_end_before_the_next_label_row() {
        let fragments = vec![
            frag("a", "発行日 2026年11月8日", [60.0, 100.0, 160.0, 110.0]),
            frag("b", "支払期限 2026年12月31日", [60.0, 114.0, 175.0, 124.0]),
            frag("c", "備考 午前中に配達", [60.0, 128.0, 145.0, 138.0]),
        ];
        let lines = |labels: &[&str]| values(&fragments, &spaced(&fragments, labels));
        assert_eq!(
            lines(&["発行日", "支払期限", "備考"]),
            [
                ("発行日".to_owned(), vec!["2026年11月8日".to_owned()]),
                ("支払期限".to_owned(), vec!["2026年12月31日".to_owned()]),
                ("備考".to_owned(), vec!["午前中に配達".to_owned()]),
            ]
        );
        // A row whose label is unknown reads as a continuation line: the
        // value spans two lines (fill holds or drops it, never cuts it).
        assert_eq!(
            lines(&["発行日", "備考"]),
            [
                (
                    "発行日".to_owned(),
                    vec![
                        "2026年11月8日".to_owned(),
                        "支払期限 2026年12月31日".to_owned()
                    ]
                ),
                ("備考".to_owned(), vec!["午前中に配達".to_owned()]),
            ]
        );
    }

    #[test]
    fn whitespace_rests_starting_with_a_label_are_header_rows() {
        let fragments = vec![frag(
            "a",
            "品名 数量 単価 金額",
            [60.0, 100.0, 155.0, 110.0],
        )];
        assert!(values(&fragments, &spaced(&fragments, &["品名", "数量", "金額"])).is_empty());
        // Unknown column titles are a plain value; fill treats it as text.
        assert_eq!(
            values(&fragments, &spaced(&fragments, &["品名"])),
            [("品名".to_owned(), vec!["数量 単価 金額".to_owned()])]
        );
        // Every accepted prefix is a hypothesis of its own.
        let fragments = vec![frag("a", "合計 金額 ¥12,100", [60.0, 100.0, 140.0, 110.0])];
        assert_eq!(
            values(&fragments, &spaced(&fragments, &["合計", "合計 金額"])),
            [
                ("合計".to_owned(), vec!["金額 ¥12,100".to_owned()]),
                ("合計 金額".to_owned(), vec!["¥12,100".to_owned()]),
            ]
        );
    }

    #[test]
    fn whitespace_splits_are_bounded() {
        let fragments = vec![frag("a", "a b c d e f g", [60.0, 100.0, 125.0, 110.0])];
        let ask = |accept: &str| {
            let mut asked = Vec::new();
            let mut is_label = |text: &str| {
                asked.push(text.to_owned());
                text == accept
            };
            let mut budget = Budget::new(u64::MAX, None);
            analyze_with_labels(&fragments, usize::MAX, Some(&mut is_label), &mut budget).unwrap();
            asked
        };
        // Labels end at the first MAX_SPACED_SPLITS runs.
        assert_eq!(ask(""), ["a", "a b", "a b c", "a b c d"]);
        // Each word of the value starts at most MAX_SPACED_SPLITS tries, of
        // one to MAX_SPACED_SPLITS words.
        let value: Vec<&str> = vec![
            "b", "b c", "c", "b c d", "c d", "d", "b c d e", "c d e", "d e", "e", "c d e f",
            "d e f", "e f", "f", "d e f g", "e f g", "f g", "g",
        ];
        assert_eq!(ask("a")[MAX_SPACED_SPLITS..], value);
        // A spacing run longer than MAX_SPACED_LABEL_BYTES ends the search.
        let gap = |n: usize| format!("合計{}1", " ".repeat(n));
        let fragments = vec![
            frag(
                "a",
                &gap(MAX_SPACED_LABEL_BYTES),
                [60.0, 100.0, 600.0, 110.0],
            ),
            frag(
                "b",
                &gap(MAX_SPACED_LABEL_BYTES + 1),
                [60.0, 124.0, 600.0, 134.0],
            ),
        ];
        let layout = spaced(&fragments, &["合計"]);
        assert_eq!(
            values(&fragments, &layout),
            [("合計".to_owned(), vec!["1".to_owned()])]
        );
        assert_eq!(layout.pairs[0].label.fragment_id, "a");
        // A label longer than MAX_SPACED_LABEL_BYTES is not tried, and a long
        // fragment without spacing is read only up to MAX_SPACED_SCAN_BYTES
        // (196, as documented in analyze_with_labels and the README).
        assert_eq!(MAX_SPACED_SCAN_BYTES, 196);
        let long = format!("{} 値", "x".repeat(MAX_SPACED_LABEL_BYTES + 1));
        let text = "あ".repeat(1_000_000);
        let fragments = vec![
            frag("a", &long, [60.0, 100.0, 600.0, 110.0]),
            frag("b", &text, [60.0, 124.0, 600.0, 134.0]),
        ];
        let used = |labels: bool| {
            let mut asked = 0;
            let mut is_label = |_: &str| {
                asked += 1;
                true
            };
            let mut budget = Budget::new(u64::MAX, None);
            let layout = analyze_with_labels(
                &fragments,
                usize::MAX,
                labels.then_some(&mut is_label as &mut dyn FnMut(&str) -> bool),
                &mut budget,
            )
            .unwrap();
            assert!(layout.pairs.is_empty());
            (u64::MAX - budget.remaining(), asked)
        };
        let (without, _) = used(false);
        let (with, asked) = used(true);
        assert_eq!(asked, 0);
        // Charged before reading: all of `a` (shorter than the bound), the
        // bound of `b`, and one unit plus one per fragment for each of the
        // two lines in each of the two passes.
        assert!(long.len() < MAX_SPACED_SCAN_BYTES);
        assert_eq!(
            with - without,
            (long.len() + MAX_SPACED_SCAN_BYTES + 2 * 2 * 2) as u64
        );
    }

    #[test]
    fn whitespace_values_holding_another_label_are_not_values() {
        // Before: each of these offered the rest of the fragment, another
        // label included, as one value (`姓` → `山田 名 太郎`).
        for (text, labels) in [
            ("姓 山田 名 太郎", &["姓", "名"][..]),
            ("姓 山田 名", &["姓", "名"]),
            ("氏名 山田太郎\u{3000}所属 経理部", &["氏名", "所属"]),
            ("会社名 株式会社サンプル 担当 山田", &["会社名", "担当"]),
            // A label of several words is tried as a whole (`Last Name`).
            (
                "First Name John Last Name Smith",
                &["First Name", "Last Name"],
            ),
            // A header row whose first column title is unknown.
            ("品名 数量 単価 金額", &["品名", "金額"]),
        ] {
            let fragments = vec![frag("a", text, [60.0, 100.0, 200.0, 110.0])];
            assert!(
                values(&fragments, &spaced(&fragments, labels)).is_empty(),
                "{text}"
            );
        }
        // A value without another label is kept.
        let fragments = vec![frag("a", "姓 山田", [60.0, 100.0, 95.0, 110.0])];
        assert_eq!(
            values(&fragments, &spaced(&fragments, &["姓", "名"])),
            [("姓".to_owned(), vec!["山田".to_owned()])]
        );
        // Only the splits whose value holds the label are dropped: `金額`
        // starts the value of `合計`, not that of `合計 金額`.
        let fragments = vec![frag("a", "合計 金額 ¥12,100", [60.0, 100.0, 140.0, 110.0])];
        assert_eq!(
            values(
                &fragments,
                &spaced(&fragments, &["合計", "合計 金額", "金額"])
            ),
            [("合計 金額".to_owned(), vec!["¥12,100".to_owned()])]
        );
    }

    #[test]
    fn only_ascii_space_tab_and_ideographic_space_split() {
        let fragments = vec![
            frag("a", "合計 ¥1", [60.0, 100.0, 100.0, 110.0]),
            frag("b", "合計\t¥2", [60.0, 124.0, 100.0, 134.0]),
            frag("c", "合計\u{3000}¥3", [60.0, 148.0, 100.0, 158.0]),
            frag("d", "合計\u{a0}¥4", [60.0, 172.0, 100.0, 182.0]),
            frag("e", "合計\u{2003}¥5", [60.0, 196.0, 100.0, 206.0]),
        ];
        let split: Vec<_> = values(&fragments, &spaced(&fragments, &["合計"]))
            .into_iter()
            .map(|(_, value)| value.concat())
            .collect();
        assert_eq!(split, ["¥1", "¥2", "¥3"]);
        // Inside a value, other spaces do not separate words either.
        let fragments = vec![frag("a", "姓 山田\u{a0}名", [60.0, 100.0, 120.0, 110.0])];
        assert_eq!(
            values(&fragments, &spaced(&fragments, &["姓", "名"])),
            [("姓".to_owned(), vec!["山田\u{a0}名".to_owned()])]
        );
    }

    /// `n` fragments `合計 1` on one line, then five lines of `x`, `spaces`
    /// spaces and `y`.
    fn label_row_over_long_runs(n: usize, spaces: usize) -> Vec<SourceFragment> {
        let mut fragments: Vec<SourceFragment> = (0..n)
            .map(|i| {
                let left = 60.0 + 40.0 * i as f64;
                frag(
                    &format!("l{i}"),
                    "合計 1",
                    [left, 100.0, left + 25.0, 110.0],
                )
            })
            .collect();
        let long = format!("x{}y", " ".repeat(spaces));
        for k in 0..5 {
            let top = 114.0 + 14.0 * f64::from(k);
            fragments.push(frag(
                &format!("c{k}"),
                &long,
                [60.0, top, 600.0, top + 10.0],
            ));
        }
        fragments
    }

    /// Work units of [`analyze_with_labels`] without and with `labels`.
    fn spaced_work(fragments: &[SourceFragment], labels: &[&str]) -> (u64, u64) {
        let used = |with: bool| {
            let mut is_label = |text: &str| labels.contains(&text);
            let mut budget = Budget::new(u64::MAX, None);
            analyze_with_labels(
                fragments,
                usize::MAX,
                with.then_some(&mut is_label as &mut dyn FnMut(&str) -> bool),
                &mut budget,
            )
            .unwrap();
            u64::MAX - budget.remaining()
        };
        (used(false), used(true))
    }

    #[test]
    fn whitespace_work_is_linear_in_the_input() {
        let fragments = label_row_over_long_runs(1_000, 100_000);
        let input: u64 = fragments.iter().map(|f| f.raw_text.len() as u64).sum();
        let (without, with) = spaced_work(&fragments, &["合計"]);
        eprintln!(
            "label row: input {input} bytes, without {without}, with {with} ({:.2}x input)",
            with as f64 / input as f64
        );
        // Before: 5.0e8 units (987x the input): each `合計 1` re-scanned the
        // five long runs below it to their ends.
        assert!(with - without <= input, "{without} -> {with}");
        // The words of a value: the scan reads each byte once, and a byte is
        // in at most 1 + 2 + 3 + 4 tried runs of words (MAX_SPACED_SPLITS),
        // so at most 11 units per byte. Short words (`x x x …`) and words
        // that fill MAX_SPACED_LABEL_BYTES in four (`xxx… xxx…`) come close.
        for word in [1, 23] {
            let text = format!("合計 {}", vec!["x".repeat(word); 25_000].join(" "));
            let fragments = vec![frag("a", &text, [60.0, 100.0, 600.0, 110.0])];
            let input = text.len() as u64;
            let (without, with) = spaced_work(&fragments, &["合計"]);
            eprintln!(
                "value words of {word} bytes: input {input} bytes, without {without}, with {with} \
                 ({:.2} units per byte)",
                (with - without) as f64 / input as f64
            );
            assert!(with - without <= 11 * input, "{without} -> {with}");
        }
    }
}
