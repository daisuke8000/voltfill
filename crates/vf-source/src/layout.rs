//! Layout hypotheses over source fragments: lines and label/value pairs.
//!
//! Pairs are candidates, not decisions: a later stage matches them against
//! form labels. Every value is an exact byte range of an existing fragment,
//! so text is never synthesized. Sub-run boxes are estimated from
//! character classes (full width = 1 em, otherwise 0.5 em) and marked
//! [`Precision::Estimated`]; they are never reported as measured.
//!
//! Supported: horizontal text, inline `label：value` / `label: value` in one
//! fragment, value to the right on the same line, value directly below, and
//! value continuation over following lines in the same column.
//! Not supported yet: values inside multi-row tables (only flagged, see
//! [`Placement::Column`]), AcroForm values, vertical text, and splits without
//! a delimiter (for example `発行日 2026年…`).
//!
//! Hypotheses are conservative ("a wrong value is worse than an empty one"):
//! - A fragment with more than one `label：` delimiter (for example
//!   `TEL：03-1234-5678 FAX：03-1234-5679`) is not split, because the end of
//!   each value cannot be told apart from the start of the next label. Such a
//!   fragment, a `label：value` fragment and a bare `label：` fragment are
//!   never read as the value of another label.
//! - A value runs over at most [`MAX_VALUE_LINES`] lines; a longer run is
//!   dropped rather than cut off. Continuation stops at a line that is itself
//!   a `label：value` pair or ends with a delimiter.
//! - A value directly below a label is taken only when the cells to the
//!   right of the label and of the value (if any) line up as one column, so
//!   the label cell of the next row (`役職` with its own value `課長`) is not
//!   read as the value of a label whose own value cell is blank.
//! - A header label above two or more data rows of a table yields
//!   [`Placement::Column`] hypotheses, which never determine a single value.
//!
//! Work and memory are bounded: [`LayoutLimits::max_pairs`] caps the number
//! of hypotheses per document and each value holds at most
//! [`MAX_VALUE_LINES`] spans.
use crate::geometry::Rect;
use crate::source::{Quality, SourceFragment};
use serde::Serialize;

/// Most lines one value may span: its first line plus continuation lines.
/// A value that would run longer is dropped, not truncated.
pub const MAX_VALUE_LINES: usize = 5;
/// Most data rows reported under one column header ([`Placement::Column`]).
pub const MAX_COLUMN_ROWS: usize = 5;
/// Default ceiling on label/value hypotheses for one document.
pub const DEFAULT_MAX_PAIRS: usize = 50_000;

/// Resource limits for [`analyze`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutLimits {
    /// Label/value hypotheses allowed for one document.
    pub max_pairs: usize,
}

impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            max_pairs: DEFAULT_MAX_PAIRS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("layout exceeded its label/value hypothesis limit")]
    LimitExceeded,
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
    /// Value starts directly below the label.
    Below,
    /// One data row under a column header that has two or more data rows
    /// (one pair per row, at most [`MAX_COLUMN_ROWS`]). A column never
    /// determines a single value on its own.
    Column,
}

/// A label/value hypothesis. `value` holds one span per line, in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pair {
    pub page: u32,
    pub label: Span,
    pub value: Vec<Span>,
    pub placement: Placement,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub pairs: Vec<Pair>,
}

/// Build lines and label/value hypotheses from usable fragments.
///
/// Work is O(n log n) for line grouping plus a bounded scan per hypothesis
/// (at most [`MAX_VALUE_LINES`] or [`MAX_COLUMN_ROWS`] lines each); the
/// result holds at most `limits.max_pairs` pairs.
///
/// # Errors
/// [`LayoutError::LimitExceeded`] as soon as the document would produce more
/// than `limits.max_pairs` hypotheses.
pub fn analyze(fragments: &[SourceFragment], limits: &LayoutLimits) -> Result<Layout, LayoutError> {
    let lines = group_lines(fragments);
    let kinds: Vec<Kind> = fragments.iter().map(classify).collect();
    let page = Page {
        fragments,
        kinds: &kinds,
        lines: &lines,
    };
    let mut pairs = Pairs {
        list: Vec::new(),
        max: limits.max_pairs,
    };
    for (i, line) in lines.iter().enumerate() {
        for &f in &line.fragments {
            if let Kind::Inline { at, after } = kinds[f] {
                pairs.push(inline_pair(&fragments[f], at, after))?;
            }
        }
        for window in line.fragments.windows(2) {
            let (label, value) = (window[0], window[1]);
            if !kinds[label].can_label() || kinds[value] != Kind::Plain {
                continue;
            }
            if let Some(value) = page.value(i, value) {
                pairs.push(Pair {
                    page: line.page,
                    label: label_span(&fragments[label]),
                    value: value.spans,
                    placement: Placement::Right,
                })?;
            }
        }
        if lines.get(i + 1).is_some_and(|next| next.page == line.page) {
            for position in 0..line.fragments.len() {
                if kinds[line.fragments[position]].can_label() {
                    page.below(i, position, &mut pairs)?;
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

/// A value's spans with the index and box of its last line.
struct Value {
    spans: Vec<Span>,
    line: usize,
    bbox: Rect,
}

/// The analyzed fragments with their kinds and lines.
struct Page<'a> {
    fragments: &'a [SourceFragment],
    kinds: &'a [Kind],
    lines: &'a [Line],
}

impl Page<'_> {
    /// The value starting at fragment `first` on line `line`, with follow-on
    /// lines in the same column. A line is merged only when it has a single
    /// plain fragment aligned with the value, so stacked label rows
    /// (小計 / 合計) and `label：` lines stay separate. `None` when the value
    /// would exceed [`MAX_VALUE_LINES`].
    fn value(&self, line: usize, first: usize) -> Option<Value> {
        let start = &self.fragments[first];
        let mut value = Value {
            spans: vec![whole(start)],
            line,
            bbox: start.bbox,
        };
        for (index, next_line) in self.lines.iter().enumerate().skip(line + 1) {
            if next_line.page != start.page {
                break;
            }
            let height = value.bbox.height();
            let [next] = next_line.fragments[..] else {
                break;
            };
            let next_fragment = &self.fragments[next];
            if next_line.bbox.top - value.bbox.bottom > height
                || (next_fragment.bbox.left - start.bbox.left).abs() > 0.5 * height
                || self.kinds[next] != Kind::Plain
            {
                break;
            }
            if value.spans.len() >= MAX_VALUE_LINES {
                return None;
            }
            value.spans.push(whole(next_fragment));
            value.line = index;
            value.bbox = next_fragment.bbox;
        }
        Some(value)
    }

    /// Below (or Column) hypotheses for the label at `position` on line `index`.
    fn below(&self, index: usize, position: usize, pairs: &mut Pairs) -> Result<(), LayoutError> {
        let line = &self.lines[index];
        let next = &self.lines[index + 1];
        let label = &self.fragments[line.fragments[position]];
        let Some(slot) = self.below_slot(label, next) else {
            return Ok(());
        };
        let value_fragment = &self.fragments[next.fragments[slot]];
        let height = label.bbox.height().max(value_fragment.bbox.height());
        // The cells right of the label and of the value must form one column:
        // a label row with a value cell of its own, or a value that is the
        // label cell of a row with its own value, is not label-above-value.
        let right_of_label = line.fragments.get(position + 1);
        let right_of_value = next.fragments.get(slot + 1);
        match (right_of_label, right_of_value) {
            (None, None) => {}
            (Some(&a), Some(&b))
                if same_column(self.fragments[a].bbox, self.fragments[b].bbox, height) => {}
            _ => return Ok(()),
        }
        let Some(value) = self.value(index + 1, next.fragments[slot]) else {
            return Ok(());
        };
        let rows = if next.fragments.len() > 1 {
            self.column_rows(value.line, value_fragment, value.bbox)
        } else {
            Vec::new()
        };
        let placement = if rows.is_empty() {
            Placement::Below
        } else {
            Placement::Column
        };
        pairs.push(Pair {
            page: line.page,
            label: label_span(label),
            value: value.spans,
            placement,
        })?;
        for row in rows {
            pairs.push(Pair {
                page: line.page,
                label: label_span(label),
                value: vec![whole(&self.fragments[row])],
                placement: Placement::Column,
            })?;
        }
        Ok(())
    }

    /// The slot on `next` of the first plain fragment that starts under
    /// `label` and is close below it. Binary search keeps wide lines linear.
    fn below_slot(&self, label: &SourceFragment, next: &Line) -> Option<usize> {
        let reach = label.bbox.height().max(next.bbox.height());
        let start = next
            .fragments
            .partition_point(|&f| self.fragments[f].bbox.left < label.bbox.left - reach);
        next.fragments[start..]
            .iter()
            .take_while(|&&f| self.fragments[f].bbox.left <= label.bbox.left + reach)
            .position(|&f| {
                let v = &self.fragments[f];
                let height = label.bbox.height().max(v.bbox.height());
                self.kinds[f] == Kind::Plain
                    && (v.bbox.left - label.bbox.left).abs() <= height
                    && v.bbox.top - label.bbox.bottom <= height
                    && v.bbox.top >= label.bbox.bottom - 0.5 * height
            })
            .map(|offset| start + offset)
    }

    /// Further data rows in the column of `cell`, after line `after`. Rows
    /// must follow without a line gap and hold a plain cell in the column.
    fn column_rows(&self, after: usize, cell: &SourceFragment, last: Rect) -> Vec<usize> {
        let mut rows = Vec::new();
        let mut previous = last;
        for line in self.lines.iter().skip(after + 1) {
            if rows.len() + 1 >= MAX_COLUMN_ROWS || line.page != cell.page {
                break;
            }
            let height = cell.bbox.height().max(previous.height());
            if line.bbox.top - previous.bottom > height {
                break;
            }
            let Some(found) = self.in_column(line, cell.bbox, height) else {
                break;
            };
            rows.push(found);
            previous = self.fragments[found].bbox;
        }
        rows
    }

    /// The first fragment of `line` in the column of `column`, if it is plain.
    fn in_column(&self, line: &Line, column: Rect, height: f64) -> Option<usize> {
        // Cells of one line do not overlap, so only the fragment just before
        // the search window can reach into the column from the left.
        let start = line
            .fragments
            .partition_point(|&f| self.fragments[f].bbox.left < column.left - height)
            .saturating_sub(1);
        let found = line.fragments[start..]
            .iter()
            .take_while(|&&f| self.fragments[f].bbox.left <= column.right + height)
            .copied()
            .find(|&f| same_column(column, self.fragments[f].bbox, height))?;
        (self.kinds[found] == Kind::Plain).then_some(found)
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

fn group_lines(fragments: &[SourceFragment]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..fragments.len())
        .filter(|&i| usable(&fragments[i]))
        .collect();
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
    for line in &mut lines {
        line.fragments
            .sort_by(|&a, &b| fragments[a].bbox.left.total_cmp(&fragments[b].bbox.left));
    }
    lines
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

fn whole(f: &SourceFragment) -> Span {
    let start = f.raw_text.len() - f.raw_text.trim_start().len();
    let end = f.raw_text.trim_end().len();
    span(f, start, end)
}

/// A label fragment without a trailing delimiter, e.g. `請求番号：` → `請求番号`.
fn label_span(f: &SourceFragment) -> Span {
    let full = whole(f);
    let text = &f.raw_text[full.start..full.end];
    let trimmed = text
        .trim_end_matches(DELIMITERS)
        .trim_end_matches(char::is_whitespace);
    if trimmed.is_empty() || trimmed.len() == text.len() {
        return full;
    }
    span(f, full.start, full.start + trimmed.len())
}

/// A span over `start..end` (byte offsets on char boundaries) of `f`.
fn span(f: &SourceFragment, start: usize, end: usize) -> Span {
    let text = &f.raw_text;
    if start == 0 && end == text.len() {
        return Span {
            fragment_id: f.id.clone(),
            start,
            end,
            bbox: f.bbox,
            precision: Precision::Run,
        };
    }
    let weight = |s: &str| s.chars().map(char_em).sum::<f64>();
    let total = weight(text);
    let (left, right) = if total > 0.0 {
        let width = f.bbox.width();
        (
            f.bbox.left + width * weight(&text[..start]) / total,
            f.bbox.left + width * weight(&text[..end]) / total,
        )
    } else {
        (f.bbox.left, f.bbox.right)
    };
    Span {
        fragment_id: f.id.clone(),
        start,
        end,
        bbox: Rect {
            left,
            top: f.bbox.top,
            right: right.max(left + f64::EPSILON),
            bottom: f.bbox.bottom,
        },
        precision: Precision::Estimated,
    }
}

/// The `label：value` pair of an [`Kind::Inline`] fragment.
fn inline_pair(f: &SourceFragment, at: usize, after: usize) -> Pair {
    let text = &f.raw_text;
    let label_start = text.len() - text.trim_start().len();
    let label_end = text[..at].trim_end().len();
    let value_start = after + (text[after..].len() - text[after..].trim_start().len());
    let value_end = text.trim_end().len();
    Pair {
        page: f.page,
        label: span(f, label_start, label_end),
        value: vec![span(f, value_start, value_end)],
        placement: Placement::Inline,
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
    fn stacked_lines_stay_linear_and_long_values_are_dropped() {
        // Before: every line was a label whose value ran over all following
        // lines (n(n+1)/2 spans). Now a value longer than the cap is dropped.
        let n = 3000;
        let fragments = stacked(n);
        let layout = analyze(&fragments);
        let spans: usize = layout.pairs.iter().map(|p| p.value.len()).sum();
        assert!(
            layout.pairs.len() <= MAX_VALUE_LINES,
            "{}",
            layout.pairs.len()
        );
        assert!(spans <= MAX_VALUE_LINES * MAX_VALUE_LINES, "{spans}");
        assert!(
            layout
                .pairs
                .iter()
                .all(|p| p.value.len() <= MAX_VALUE_LINES)
        );
        // A label with exactly MAX_VALUE_LINES value lines keeps all of them.
        let fragments = stacked(MAX_VALUE_LINES + 1);
        let layout = analyze(&fragments);
        assert_eq!(layout.pairs[0].value.len(), MAX_VALUE_LINES);
        assert_eq!(layout.pairs[0].label.fragment_id, "f0");
        // One more line and that label has no value at all (never truncated).
        let fragments = stacked(MAX_VALUE_LINES + 2);
        let layout = analyze(&fragments);
        assert!(layout.pairs.iter().all(|p| p.label.fragment_id != "f0"));
    }

    #[test]
    fn pair_limit_is_a_typed_error() {
        let fragments = [
            frag("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
            frag("b", "発行日：2026年11月8日", [60.0, 139.0, 179.8, 150.0]),
            frag("c", "TEL：03-1234-5678", [60.0, 159.0, 150.0, 170.0]),
        ];
        let limits = |max_pairs| LayoutLimits { max_pairs };
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
}
