//! Layout hypotheses over source fragments: lines and label/value pairs.
//!
//! Pairs are candidates, not decisions: a later stage matches them against
//! form labels. Every value is an exact byte range of an existing fragment,
//! so text is never synthesized. Sub-run boxes are estimated from
//! character classes (full width = 1 em, otherwise 0.5 em) and marked
//! [`Precision::Estimated`]; they are never reported as measured.
//!
//! Each pair records how its value is tied to its label ([`Binding`]):
//! - [`Binding::Delimiter`]: an inline `label：value` / `label: value` in one
//!   fragment, or a fragment ending with its only delimiter (`label：`)
//!   followed by the next fragment to its right on the same line.
//! - [`Binding::Geometric`]: position only — a right neighbour without a
//!   delimiter, a value directly below the label, or a row of a column.
//!
//! A value to the right (and an inline value) takes continuation lines in
//! its column; a value with more than one line is still one pair, with one
//! span per line. Not supported: values in multi-row tables and stacked
//! lists (only flagged, see [`Placement::Column`]), AcroForm values,
//! vertical text, and splits without a delimiter (for example
//! `発行日 2026年…`).
//!
//! Hypotheses are conservative ("a wrong value is worse than an empty one"):
//! - A fragment with more than one `label：` delimiter (for example
//!   `TEL：03-1234-5678 FAX：03-1234-5679`) is not split, because the end of
//!   each value cannot be told apart from the start of the next label. Such a
//!   fragment, a `label：value` fragment and a bare `label：` fragment are
//!   never read as the value of another label.
//! - A value runs over at most [`MAX_VALUE_LINES`] lines; a longer run is
//!   dropped rather than cut off. Continuation stops at a line that is itself
//!   a `label：value` pair or ends with a delimiter. Continuation lines of a
//!   value to the right start within half a line height of the value's left
//!   edge. Continuation lines of an inline value start anywhere from the
//!   fragment's left edge (a value wrapped back under its label) to 1.5 line
//!   heights right of the value's estimated start.
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
//! search is limited to [`MAX_WINDOW`] fragments, and each fragment's text is
//! read twice (classified and measured) however many pairs use it.
//! [`LayoutLimits::max_work`] and [`LayoutLimits::deadline`] stop the
//! analysis with a typed error (see [`analyze_within`] for the units).
use crate::budget::{Budget, BudgetError};
use crate::geometry::Rect;
use crate::source::{Quality, SourceFragment};
use serde::Serialize;
use std::time::Instant;

/// Most lines one value may span: its first line plus continuation lines.
/// A value that would run longer is dropped, not truncated.
pub const MAX_VALUE_LINES: usize = 5;
/// Most data rows examined (and reported) below one label ([`Placement::Column`]).
pub const MAX_COLUMN_ROWS: usize = 5;
/// Most fragments of one line examined below a label or in a column.
pub const MAX_WINDOW: usize = 16;
/// Default ceiling on label/value hypotheses for one document.
pub const DEFAULT_MAX_PAIRS: usize = 50_000;
/// Default work units for [`analyze`] (see [`analyze_within`]).
pub const DEFAULT_MAX_WORK: u64 = 200_000_000;

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

/// Usable fragments sharing a baseline band, left to right.
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
    /// Label and value in one fragment, split at a delimiter.
    Inline,
    /// Value is the next fragment to the right on the same line.
    Right,
    /// The only data row directly below the label.
    Below,
    /// One data row below a label that has two or more data rows (a table
    /// header or the first line of a stacked list); one pair per non-blank
    /// row, at most [`MAX_COLUMN_ROWS`]. A column never determines a single
    /// value on its own.
    Column,
}

/// How a pair's value is tied to its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// An explicit delimiter: a `label：value` fragment ([`Placement::Inline`]),
    /// or a fragment that ends with its only delimiter (`label：`) and the
    /// next fragment to its right on the same line ([`Placement::Right`]).
    Delimiter,
    /// Position only: a right neighbour without a delimiter, a value below
    /// the label or a row of a column.
    Geometric,
}

/// A label/value hypothesis. `value` holds one span per line, in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pair {
    pub page: u32,
    pub label: Span,
    pub value: Vec<Span>,
    pub placement: Placement,
    pub binding: Binding,
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
/// Work units: one per fragment and per sort comparison while grouping
/// lines; two per byte of fragment text, which is classified and measured
/// once per fragment; one per line and per fragment examined in a search;
/// and one plus the fragment id's length per span written into a pair.
/// Searches are bounded ([`MAX_VALUE_LINES`], [`MAX_COLUMN_ROWS`],
/// [`MAX_WINDOW`]), and a span reuses its fragment's measurements instead of
/// reading the text again, so the units count all work that grows with the
/// input.
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
    budget.check_deadline()?;
    let lines = group_lines(fragments, budget)?;
    let mut measures = Vec::with_capacity(fragments.len());
    for fragment in fragments {
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
        for &f in &line.fragments {
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
            };
            if let Some(value) = page.value(i, first, column, budget)? {
                pairs.push(Pair {
                    page: line.page,
                    label,
                    value,
                    placement: Placement::Inline,
                    binding: Binding::Delimiter,
                })?;
            }
        }
        for window in line.fragments.windows(2) {
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
            };
            if let Some(value) = page.value(i, first, column, budget)? {
                let binding = if measures[label].kind == Kind::Label {
                    Binding::Delimiter
                } else {
                    Binding::Geometric
                };
                pairs.push(Pair {
                    page: line.page,
                    label: page.span(label, START, LABEL_END, budget)?,
                    value,
                    placement: Placement::Right,
                    binding,
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
    Ok(Layout {
        lines,
        pairs: pairs.list,
    })
}

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

/// What a line holds in a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    /// The first fragment of the line in the column.
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
        let fragment = &self.fragments[f];
        budget.spend(1 + fragment.id.len() as u64)?;
        let measure = &self.measures[f];
        let (start, end) = (measure.at[from], measure.at[to]);
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
                fragment.bbox.left + width * measure.em[from] / measure.total,
                fragment.bbox.left + width * measure.em[to] / measure.total,
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

    /// The value starting with `first` on line `line`, with follow-on lines
    /// whose single plain fragment starts in `column`. A line is merged only
    /// when it has a single plain fragment, so stacked label rows (小計 /
    /// 合計) and `label：` lines stay separate. `None` when the value would
    /// exceed [`MAX_VALUE_LINES`].
    fn value(
        &self,
        line: usize,
        first: Span,
        column: Column,
        budget: &mut Budget,
    ) -> Result<Option<Vec<Span>>, BudgetError> {
        let page = self.lines[line].page;
        let mut last = first.bbox;
        let mut value = vec![first];
        for next_line in &self.lines[line + 1..] {
            budget.spend(1)?;
            if next_line.page != page {
                break;
            }
            let height = last.height();
            let [next] = next_line.fragments[..] else {
                break;
            };
            let next_fragment = &self.fragments[next];
            let left = next_fragment.bbox.left;
            if next_line.bbox.top - last.bottom > height
                || left < column.from
                || left > column.to
                || self.kind(next) != Kind::Plain
            {
                break;
            }
            if value.len() >= MAX_VALUE_LINES {
                return Ok(None);
            }
            value.push(self.span(next, START, END, budget)?);
            last = next_fragment.bbox;
        }
        Ok(Some(value))
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
            });
        }
        for cell in cells {
            pairs.push(Pair {
                page: line.page,
                label: self.span(f, START, LABEL_END, budget)?,
                value: vec![self.span(cell, START, END, budget)?],
                placement: Placement::Column,
                binding: Binding::Geometric,
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
    /// [`MAX_COLUMN_ROWS`]) and the plain cells of the non-blank rows. Rows
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
    ) -> Result<(usize, Vec<usize>), BudgetError> {
        let first_line = &self.lines[first];
        let cell = &self.fragments[first_line.fragments[slot]];
        let neighbours: Vec<Rect> = [slot.checked_sub(1), Some(slot + 1)]
            .into_iter()
            .flatten()
            .filter_map(|p| first_line.fragments.get(p))
            .map(|&f| self.fragments[f].bbox)
            .collect();
        let mut rows = 1;
        let mut cells = vec![first_line.fragments[slot]];
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
                Cell::Found(f) if self.kind(f) == Kind::Plain => cells.push(f),
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
                return Ok(Cell::Found(f));
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
    f.quality == Quality::Usable && !f.raw_text.trim().is_empty() && f.bbox.validate().is_ok()
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
            frag("b", "2026/11/08", [200.0, 100.0, 250.0, 110.0]),
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
}
