//! Source spans selected by a Display-space box, for answers that name a
//! region instead of text (for example a model box mapped back with
//! [`ImageTransform::normalized_box_to_display`]).
//!
//! # Results
//!
//! Every result is a [`Selected`]: an exact byte range of one usable fragment
//! with a [`Role`]. Nothing is synthesized and no substring is cut at a
//! guessed character ratio.
//!
//! - A fragment with a *known split* (below) is always returned as its parts,
//!   [`Role::Label`] and/or [`Role::Value`], with the layout's byte ranges and
//!   estimated boxes ([`Precision::Estimated`]), whether the box covers all of
//!   the fragment or only some of it. The delimiter and the whitespace around
//!   it belong to neither part.
//! - Any other fragment is returned whole as [`Role::Whole`]:
//!   `0..raw_text.len()`, untrimmed, with the fragment box and
//!   [`Precision::Run`].
//!
//! **Fill values.** A fill layer may take text only from `Value` spans and
//! from `Whole` spans returned by [`in_box`]; never from `Label` spans and
//! never from [`overlapping`]. `Whole` only says that no split is known
//! *inside* the run: the run can still be the label of a pair across
//! fragments (a `請求番号：` run followed by a separate value run), so the
//! caller must still rule it out against the form label.
//!
//! # Known splits
//!
//! A known split is the one label/value pair that [`layout::analyze`] derives
//! from that fragment alone, accepted only when all of these hold:
//!
//! - the pair is [`Placement::Inline`] with exactly one value span, and both
//!   spans are nonempty, ordered, char-boundary ranges of this fragment;
//! - the label and value texts have no surrounding whitespace, the text
//!   before the label and after the value is whitespace only, and the text
//!   between them, trimmed, is exactly one delimiter of this module's own
//!   set (`：` or `:`). A delimiter or a whitespace-only split that layout may
//!   add later is refused here until this set is updated, so arbitrary
//!   substrings (`佐藤 花子`, `発行日 2026年11月8日`) never become parts;
//! - the label's last and the value's first character are not both digits
//!   (ASCII or full width), so times and ratios with either colon
//!   (`10：30`, `1：2`) stay whole. Layout skips only ASCII `9:00` today and
//!   should adopt the same rule.
//!
//! # Coverage
//!
//! For each usable fragment on the page, the box is tested against the
//! fragment's content (its text without leading and trailing whitespace) and
//! against each part. A box that misses the content is skipped, so touching
//! only a neighbour's blank margin is harmless.
//!
//! - [`in_box`]: every part must be either contained or missed. Contained
//!   parts are selected. A cut part, a box over the content that contains no
//!   part (for example only the delimiter), or a partial cover of a fragment
//!   without a known split is [`SelectError::SplitsSourceRun`], and one such
//!   fragment fails the whole call.
//! - [`overlapping`]: every part the box may touch, or all parts when it
//!   touches the content but no part. It is inclusive, for diagnostics and
//!   candidate display only, and fails only for an invalid box.
//!
//! # Edge positions
//!
//! Fragment edges are measured; part edges and content edges after leading or
//! before trailing whitespace are interior and only bounded. Each interior
//! edge gets an interval of plausible x positions, computed from both measured
//! fragment edges with these assumptions:
//!
//! - full-width characters (layout's wide class) advance exactly one em;
//!   other characters advance between [`NARROW_ADVANCE_MIN_EM`] and
//!   [`NARROW_ADVANCE_MAX_EM`] em (proportional Latin, spaces, digits,
//!   symbols drawn full width);
//! - the em lies between the fragment height (pdf-inspector reports one em of
//!   height for a horizontal run) and the width-derived estimate
//!   `width / Σ weights` (wide 1, other 0.5, as in layout).
//!
//! The interval is the intersection of the ranges reached from the left and
//! from the right edge, widened to include layout's own estimate. A box edge
//! must clear the whole interval, up to [`ESTIMATED_EDGE_TOLERANCE_EM`], to
//! contain or to miss a part; otherwise the part is cut. A fragment whose
//! height is not plausible for one horizontal line of its width
//! (height / width-derived em outside [`HEIGHT_PER_ESTIMATED_EM`]: tall boxes,
//! rotated or vertical runs, heavily condensed text) has unbounded interior
//! edges: [`in_box`] selects it only with a box containing the whole content
//! (as its parts, when a split is known).
//!
//! Worked examples, both 11 units high:
//!
//! - `請求番号：INV-0012` at x 60–161.4: weights 9, so the em is 11–11.27.
//!   The value starts after five full-width characters, at 115.0–116.3; the
//!   label ends at 104.0–105.1. A box over the rest of the line whose left
//!   edge lies between x 103.4 (105.07 − 0.15 em) and 116.6 (115.0 + 0.15 em)
//!   selects only the value; the observed value start is 115.0.
//! - `Total: 12000` in a proportional 11-unit font at x 60–121.1: the em is
//!   10.19–11, the value starts somewhere in 74.3–111.0 and the label ends in
//!   70.2–106.9 (layout estimates 95.7 for a true 90.6, 0.46 em off). No box
//!   edge separates them, so every partial box is `SplitsSourceRun`; only a
//!   box over the whole run selects its label and value. Value-only boxes over
//!   mostly-Latin `label: value` runs are therefore refused by design.
//!
//! Limits: proportional CJK metrics (for example `palt`), letter spacing and
//! combining marks are not modelled; they can move a boundary beyond its
//! interval.
//!
//! # Caller boxes
//!
//! Measured edges allow only [`MEASURED_EDGE_SLACK`], far below real model
//! box error (a 0–1000 grid on an A4 page is about 0.84 Display units per
//! step). Callers should expand each edge of a model box outward by at least
//! that error, for example one grid step mapped to Display units, and by well
//! under half the gap to adjacent text. An expanded box may reach into a
//! delimiter or a neighbour's blank margin; reaching into a neighbour's text
//! cuts it.
//!
//! # Duplicates, order and cost
//!
//! Overprinted copies (faux bold) are one selection: usable fragments on the
//! page with the same text and all four edges within
//! [`DUPLICATE_EDGE_TOLERANCE_EM`] × the smaller height (chained) form a
//! group. The box is tested against each copy in snapshot order and the first
//! copy it selects is returned; a group whose copies it only misses or cuts is
//! skipped, and it is an error only when the box cuts every copy. The copies
//! differ by less than any glyph, so a box edge between them is not a cut.
//!
//! Results are in reading order: fragments are grouped into bands with the
//! same rule as layout lines (vertical overlap of at least half the smaller
//! height), bands top to bottom, fragments left to right, ties by snapshot
//! index; parts of one fragment are in byte order.
//!
//! Only usable fragments (usable quality, nonblank text, valid box) are
//! considered; others are skipped, never reported as errors. Fragment IDs are
//! not checked for uniqueness: [`crate::inspect`] issues unique IDs.
//! Each call sorts the page's usable fragments by text and top for duplicate
//! detection, comparing each with the same-text fragments in its top band
//! (O(n log n + n·k), k such fragments), then analyzes each fragment whose box
//! intersects the selection box in time linear in its text, and sorts the
//! selected fragments.
//!
//! [`ImageTransform::normalized_box_to_display`]: crate::geometry::ImageTransform::normalized_box_to_display
use std::ops::RangeInclusive;

use serde::Serialize;

use crate::geometry::Rect;
use crate::layout::{self, Placement, Precision, Span};
use crate::source::{Quality, SourceFragment};

/// Edge slack, in Display units, for edges the extractor measured: fragment
/// edges, and part edges where a part starts or ends the fragment. It absorbs
/// f32 rounding of a normalized model box, at most 2^-25 × extent ≈ 4.3e-4 on
/// a 14,400-unit page (the PDF page-size limit at UserUnit 1), and is far
/// below any glyph. An overlap within it counts as touching. It does not
/// absorb model box error: callers expand boxes (see the module docs).
pub const MEASURED_EDGE_SLACK: f64 = 1e-3;

/// Tolerance for interior (estimated) edges, as a fraction of the smaller of
/// the height em and the width-derived em: how far a box edge may cut into a
/// part and still contain it, or overlap it and still miss it. Below
/// [`NARROW_ADVANCE_MIN_EM`] of the same em, so a box that leaves out a whole
/// character never counts as containing it.
pub const ESTIMATED_EDGE_TOLERANCE_EM: f64 = 0.15;

/// Smallest assumed advance of a character outside the wide class, in em
/// (Helvetica `i` and `l` are 0.222, Calibri's space 0.226).
pub const NARROW_ADVANCE_MIN_EM: f64 = 0.2;

/// Largest assumed advance of a character outside the wide class, in em
/// (Helvetica `@` is 1.015; Japanese fonts draw symbols such as `※` and `…`
/// one em wide).
pub const NARROW_ADVANCE_MAX_EM: f64 = 1.05;

/// Plausible fragment height per width-derived em for one horizontal line.
/// Outside it, interior edges are unbounded.
pub const HEIGHT_PER_ESTIMATED_EM: RangeInclusive<f64> = 0.7..=1.6;

/// Overprinted copies: same text and every edge within this fraction of the
/// smaller fragment height (1.1 Display units at 11 pt).
pub const DUPLICATE_EDGE_TOLERANCE_EM: f64 = 0.1;

/// Delimiters of a known split; mirrors layout's inline delimiters.
const DELIMITERS: [char; 2] = ['：', ':'];

/// What a selected span is within its fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A whole fragment with no known split. It may still be a label of a
    /// pair across fragments.
    Whole,
    /// The label part of a known split. Never a fill value.
    Label,
    /// The value part of a known split.
    Value,
}

/// A selected source span and its role.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selected {
    pub span: Span,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("selection box must have finite coordinates and positive extent")]
    InvalidBox,
    /// The box cuts a fragment, or a part of it, or covers only the space
    /// between its parts.
    #[error("selection box cuts source fragment {fragment_id} outside a known split")]
    SplitsSourceRun { fragment_id: String },
}

/// Spans fully inside `bbox` on `page`, in reading order. A fragment the box
/// covers only in part contributes the parts of its known split that the box
/// contains; any other partial cover is an error. See the module docs for
/// which spans may become fill values.
pub fn in_box(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Selected>, SelectError> {
    select(fragments, page, bbox, Mode::Contained)
}

/// Spans `bbox` may touch on `page`, in reading order: whole runs, or the
/// parts of a known split the box may touch. Inclusive, for diagnostics and
/// candidate display: it must not be used to produce fill values. Never fails
/// for a valid box.
pub fn overlapping(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Selected>, SelectError> {
    select(fragments, page, bbox, Mode::Overlapping)
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Contained,
    Overlapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cover {
    Outside,
    Inside,
    Cut,
}

fn select(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
    mode: Mode,
) -> Result<Vec<Selected>, SelectError> {
    bbox.validate().map_err(|_| SelectError::InvalidBox)?;
    let mut selected = Vec::new();
    for copies in overprint_groups(fragments, page) {
        // Prefer a copy the box selects, then one it misses; report a cut
        // only when the box cuts every copy.
        let mut missed = false;
        let mut cut = None;
        let mut chosen = None;
        for index in copies {
            if !intersects(bbox, fragments[index].bbox) {
                missed = true;
                continue;
            }
            match Source::new(&fragments[index]).select(bbox, mode) {
                Ok(Some(items)) => {
                    chosen = Some((index, items));
                    break;
                }
                Ok(None) => missed = true,
                Err(error) => {
                    cut.get_or_insert(error);
                }
            }
        }
        match (chosen, cut) {
            (Some(item), _) => selected.push(item),
            (None, Some(error)) if !missed => return Err(error),
            (None, _) => {}
        }
    }
    Ok(reading_order(fragments, selected))
}

/// Positive-area intersection without slack: when false, the box misses the
/// fragment's content under any slack, so the fragment needs no analysis.
fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn usable(f: &SourceFragment) -> bool {
    f.quality == Quality::Usable && !f.raw_text.trim().is_empty() && f.bbox.validate().is_ok()
}

fn whole(f: &SourceFragment) -> Span {
    Span {
        fragment_id: f.id.clone(),
        start: 0,
        end: f.raw_text.len(),
        bbox: f.bbox,
        precision: Precision::Run,
    }
}

/// Usable fragments on `page`, grouped into overprinted copies (see the
/// module docs): each group in snapshot order, groups by their first index.
fn overprint_groups(fragments: &[SourceFragment], page: u32) -> Vec<Vec<usize>> {
    let candidates: Vec<usize> = (0..fragments.len())
        .filter(|&i| fragments[i].page == page && usable(&fragments[i]))
        .collect();
    let mut order = candidates.clone();
    order.sort_by(|&a, &b| {
        let (fa, fb) = (&fragments[a], &fragments[b]);
        fa.raw_text
            .cmp(&fb.raw_text)
            .then(fa.bbox.top.total_cmp(&fb.bbox.top))
            .then(a.cmp(&b))
    });
    // The lowest index each fragment is a copy of; always lower than itself.
    let mut copy_of: Vec<Option<usize>> = vec![None; fragments.len()];
    for (position, &j) in order.iter().enumerate() {
        let b = &fragments[j];
        // The pairwise tolerance never exceeds this fraction of b's height.
        let band = DUPLICATE_EDGE_TOLERANCE_EM * b.bbox.height();
        for &i in order[..position].iter().rev() {
            let a = &fragments[i];
            if a.raw_text != b.raw_text || b.bbox.top - a.bbox.top > band {
                break;
            }
            if same_place(a.bbox, b.bbox) {
                let (low, high) = (i.min(j), i.max(j));
                copy_of[high] = Some(copy_of[high].map_or(low, |c| c.min(low)));
            }
        }
    }
    let mut group_of: Vec<Option<usize>> = vec![None; fragments.len()];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in candidates {
        let mut root = i;
        while let Some(lower) = copy_of[root] {
            root = lower;
        }
        match group_of[root] {
            Some(group) => groups[group].push(i),
            None => {
                group_of[root] = Some(groups.len());
                groups.push(vec![i]);
            }
        }
    }
    groups
}

fn same_place(a: Rect, b: Rect) -> bool {
    let tolerance = DUPLICATE_EDGE_TOLERANCE_EM * a.height().min(b.height());
    [
        (a.left, b.left),
        (a.top, b.top),
        (a.right, b.right),
        (a.bottom, b.bottom),
    ]
    .iter()
    .all(|(x, y)| (x - y).abs() <= tolerance)
}

/// A part of a fragment: its role, the span returned for it, the byte range
/// its edges are tested at, and layout's estimated box, if any.
struct Part {
    role: Role,
    span: Span,
    range: (usize, usize),
    estimate: Option<Rect>,
}

/// A usable fragment prepared for coverage tests.
struct Source<'a> {
    f: &'a SourceFragment,
    /// Sum of character-class weights (wide 1, other 0.5).
    weight: f64,
    /// Em range for advances, or `None` when interior edges are unbounded.
    em: Option<(f64, f64)>,
    /// Tolerance for interior edges, in Display units.
    tolerance: f64,
    parts: Vec<Part>,
}

impl<'a> Source<'a> {
    fn new(f: &'a SourceFragment) -> Self {
        let r = f.bbox;
        let weight = weight(&f.raw_text);
        let height_em = r.height();
        let width_em = if weight > 0.0 {
            r.width() / weight
        } else {
            height_em
        };
        let plausible = HEIGHT_PER_ESTIMATED_EM.contains(&(height_em / width_em));
        let em = plausible.then(|| (height_em.min(width_em), height_em.max(width_em)));
        let tolerance =
            (ESTIMATED_EDGE_TOLERANCE_EM * height_em.min(width_em)).max(MEASURED_EDGE_SLACK);
        let parts = match known_split(f) {
            Some((label, value)) => [(Role::Label, label), (Role::Value, value)]
                .into_iter()
                .map(|(role, span)| Part {
                    role,
                    range: (span.start, span.end),
                    estimate: Some(span.bbox),
                    span,
                })
                .collect(),
            None => {
                let text = &f.raw_text;
                let start = text.len() - text.trim_start().len();
                vec![Part {
                    role: Role::Whole,
                    span: whole(f),
                    range: (start, text.trim_end().len()),
                    estimate: None,
                }]
            }
        };
        Self {
            f,
            weight,
            em,
            tolerance,
            parts,
        }
    }

    /// `None` when the box misses the content; otherwise the selected parts.
    fn select(&self, bbox: Rect, mode: Mode) -> Result<Option<Vec<Selected>>, SelectError> {
        let (Some(first), Some(last)) = (self.parts.first(), self.parts.last()) else {
            return Ok(None);
        };
        if self.cover(bbox, (first.range.0, last.range.1), None) == Cover::Outside {
            return Ok(None);
        }
        let covers: Vec<Cover> = self
            .parts
            .iter()
            .map(|part| self.cover(bbox, part.range, part.estimate))
            .collect();
        let keep: Vec<bool> = match mode {
            Mode::Contained => {
                if covers.contains(&Cover::Cut) || !covers.contains(&Cover::Inside) {
                    return Err(SelectError::SplitsSourceRun {
                        fragment_id: self.f.id.clone(),
                    });
                }
                covers.iter().map(|&cover| cover == Cover::Inside).collect()
            }
            Mode::Overlapping => {
                let none = covers.iter().all(|&cover| cover == Cover::Outside);
                covers
                    .iter()
                    .map(|&cover| none || cover != Cover::Outside)
                    .collect()
            }
        };
        Ok(Some(
            self.parts
                .iter()
                .zip(keep)
                .filter(|(_, keep)| *keep)
                .map(|(part, _)| Selected {
                    span: part.span.clone(),
                    role: part.role,
                })
                .collect(),
        ))
    }

    /// How `bbox` covers the byte range `start..end`. Each slack is capped at
    /// a quarter of the widest plausible extent, so the two slacks of an axis
    /// sum to less than it and a box can never both contain and miss the range.
    fn cover(&self, bbox: Rect, (start, end): (usize, usize), estimate: Option<Rect>) -> Cover {
        let r = self.f.bbox;
        let left = self.edge(start, estimate.map(|e| e.left));
        let right = self.edge(end, estimate.map(|e| e.right));
        let cap = (right.hi - left.lo) / 4.0;
        let slack = |edge: &Edge| {
            if edge.measured {
                MEASURED_EDGE_SLACK.min(cap)
            } else {
                self.tolerance.min(cap)
            }
        };
        let (slack_left, slack_right) = (slack(&left), slack(&right));
        let vertical = MEASURED_EDGE_SLACK.min(r.height() / 4.0);
        let outside = bbox.right <= left.lo + slack_left
            || bbox.left >= right.hi - slack_right
            || bbox.bottom <= r.top + vertical
            || bbox.top >= r.bottom - vertical;
        let inside = bbox.left <= left.lo + slack_left
            && bbox.right >= right.hi - slack_right
            && bbox.top <= r.top + vertical
            && bbox.bottom >= r.bottom - vertical;
        if outside {
            Cover::Outside
        } else if inside {
            Cover::Inside
        } else {
            Cover::Cut
        }
    }

    /// Plausible x positions of the boundary before byte `at` (see the module
    /// docs), always including the class-weight estimate and layout's.
    fn edge(&self, at: usize, layout_estimate: Option<f64>) -> Edge {
        let (text, r) = (&self.f.raw_text, self.f.bbox);
        if at == 0 {
            return Edge::measured(r.left);
        }
        if at >= text.len() {
            return Edge::measured(r.right);
        }
        let (mut lo, mut hi) = match (self.em, text.get(..at), text.get(at..)) {
            (Some(em), Some(before), Some(after)) => {
                let (before_min, before_max) = advance(before, em);
                let (after_min, after_max) = advance(after, em);
                let lo = (r.left + before_min).max(r.right - after_max).max(r.left);
                let hi = (r.left + before_max).min(r.right - after_min).min(r.right);
                if lo <= hi {
                    (lo, hi)
                } else {
                    (r.left, r.right)
                }
            }
            _ => (r.left, r.right),
        };
        let own = text
            .get(..at)
            .filter(|_| self.weight > 0.0)
            .map(|before| r.left + r.width() * weight(before) / self.weight);
        for x in [own, layout_estimate].into_iter().flatten() {
            lo = lo.min(x);
            hi = hi.max(x);
        }
        Edge {
            lo,
            hi,
            measured: false,
        }
    }
}

/// Plausible x positions of a boundary; `lo == hi` for measured edges.
struct Edge {
    lo: f64,
    hi: f64,
    measured: bool,
}

impl Edge {
    fn measured(x: f64) -> Self {
        Self {
            lo: x,
            hi: x,
            measured: true,
        }
    }
}

/// Smallest and largest total advance of `text` for an em in `em`.
fn advance(text: &str, (em_min, em_max): (f64, f64)) -> (f64, f64) {
    let (wide, other) = text.chars().fold((0.0, 0.0), |(wide, other), c| {
        if is_wide(c) {
            (wide + 1.0, other)
        } else {
            (wide, other + 1.0)
        }
    });
    (
        (wide + other * NARROW_ADVANCE_MIN_EM) * em_min,
        (wide + other * NARROW_ADVANCE_MAX_EM) * em_max,
    )
}

fn weight(text: &str) -> f64 {
    text.chars()
        .map(|c| if is_wide(c) { 1.0 } else { 0.5 })
        .sum()
}

/// Wide East Asian characters; mirrors layout's character classes.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
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
        | 0x20000..=0x3FFFD)
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('０'..='９').contains(&c)
}

/// The label and value of the one inline pair layout derives from `f` alone,
/// when it passes the checks in the module docs.
fn known_split(f: &SourceFragment) -> Option<(Span, Span)> {
    let pairs = layout::analyze(std::slice::from_ref(f)).pairs;
    let [pair] = pairs.as_slice() else {
        return None;
    };
    let [value] = pair.value.as_slice() else {
        return None;
    };
    let label = &pair.label;
    let text = f.raw_text.as_str();
    let shape = pair.placement == Placement::Inline
        && label.fragment_id == f.id
        && value.fragment_id == f.id
        && label.start < label.end
        && label.end <= value.start
        && value.start < value.end
        && label.bbox.validate().is_ok()
        && value.bbox.validate().is_ok();
    if !shape {
        return None;
    }
    let label_text = text.get(label.start..label.end)?;
    let value_text = text.get(value.start..value.end)?;
    let gap = text.get(label.end..value.start)?;
    let mut delimiter = gap.trim().chars();
    let one_delimiter =
        delimiter.next().is_some_and(|c| DELIMITERS.contains(&c)) && delimiter.next().is_none();
    let trimmed = label_text.trim() == label_text
        && value_text.trim() == value_text
        && text.get(..label.start)?.trim().is_empty()
        && text.get(value.end..)?.trim().is_empty();
    let number = label_text.chars().next_back().is_some_and(is_digit)
        && value_text.chars().next().is_some_and(is_digit);
    (one_delimiter && trimmed && !number).then(|| (label.clone(), value.clone()))
}

/// Spans of one selected fragment, keyed by its snapshot index.
type Item = (usize, Vec<Selected>);

/// Flatten per-fragment spans in reading order (see the module docs).
fn reading_order(fragments: &[SourceFragment], mut selected: Vec<Item>) -> Vec<Selected> {
    let rect = |index: usize| fragments[index].bbox;
    selected.sort_by(|(a, _), (b, _)| {
        rect(*a)
            .top
            .total_cmp(&rect(*b).top)
            .then(rect(*a).left.total_cmp(&rect(*b).left))
            .then(a.cmp(b))
    });
    let mut bands: Vec<(Rect, Vec<Item>)> = Vec::new();
    for item in selected {
        let r = rect(item.0);
        match bands.last_mut() {
            Some((band, items)) if same_band(*band, r) => {
                *band = union(*band, r);
                items.push(item);
            }
            _ => bands.push((r, vec![item])),
        }
    }
    bands
        .into_iter()
        .flat_map(|(_, mut items)| {
            items.sort_by(|(a, _), (b, _)| rect(*a).left.total_cmp(&rect(*b).left).then(a.cmp(b)));
            items
        })
        .flat_map(|(_, spans)| spans)
        .collect()
}

/// Vertical overlap of at least half the smaller height, as for layout lines.
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
