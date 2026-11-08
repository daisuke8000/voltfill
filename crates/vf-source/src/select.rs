//! Source spans selected by a Display-space box, for answers that name a
//! region instead of text (for example a model box mapped back with
//! [`ImageTransform::normalized_box_to_display`]).
//!
//! Selected text is always an exact byte range of an existing usable fragment;
//! nothing is synthesized and no substring is cut at a guessed character
//! ratio. A box may separate a fragment only at a *known split*: the
//! label/value pair that [`layout::analyze`] derives from that fragment alone
//! (today the inline `label：value` delimiter split). Its parts keep the
//! layout's character-class boxes and [`Precision::Estimated`].
//!
//! For each usable fragment on the page that the box overlaps:
//!
//! - [`in_box`]: a box containing the whole fragment selects the whole run.
//!   Otherwise every part of a known split must be either contained or
//!   missed: contained parts are selected (all of them select the whole
//!   run). A cut part, a box covering only the gap between parts, or a
//!   partial cover without a known split is [`SelectError::SplitsSourceRun`],
//!   and one such fragment fails the whole call.
//! - [`overlapping`]: a box touching some but not all parts of a known split
//!   selects the touched parts; any other overlap selects the whole run. It
//!   fails only for an invalid box.
//!
//! A whole run is `0..raw_text.len()` with the fragment box and
//! [`Precision::Run`], untrimmed; a known-split part keeps the layout's byte
//! range and estimated box.
//!
//! Results are in reading order: fragments are grouped into bands with the
//! same rule as layout lines (vertical overlap of at least half the smaller
//! height), bands top to bottom, fragments left to right, ties by snapshot
//! index; parts of one fragment are in byte order.
//!
//! Only usable fragments (usable quality, nonblank text, valid box) are
//! considered; others are skipped, never reported as errors. Fragment IDs are
//! not checked for uniqueness: [`crate::inspect`] issues unique IDs.
//! Each call is one scan over the slice, a sort of the selected fragments and
//! a split check of each partially covered fragment.
//!
//! [`ImageTransform::normalized_box_to_display`]: crate::geometry::ImageTransform::normalized_box_to_display
use crate::geometry::Rect;
use crate::layout::{self, Precision, Span};
use crate::source::{Quality, SourceFragment};

/// Edge slack, in Display units, for edges the extractor measured: fragment
/// edges and the outer edges of parts that start or end the fragment.
/// It absorbs f32 rounding of a normalized model box, at most
/// 2^-25 × extent ≈ 4.3e-4 on a 14,400-unit page (the PDF page-size limit
/// at UserUnit 1), and is far below any glyph. An overlap within it counts as
/// touching.
pub const MEASURED_EDGE_SLACK: f64 = 1e-3;

/// Edge slack, as a fraction of the fragment height (an em proxy), for the
/// interior edges of parts positioned by character-class estimates. The
/// layout keeps those estimates within 0.2 em on observed fixtures; staying
/// below 0.5 em (the narrowest estimated character) means a box that leaves
/// out a whole estimated character is never read as containing it.
pub const ESTIMATED_EDGE_SLACK_EM: f64 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("selection box must have finite coordinates and positive extent")]
    InvalidBox,
    /// The box partially covers a fragment in a way no known split explains.
    #[error("selection box cuts source fragment {fragment_id} outside a known split")]
    SplitsSourceRun { fragment_id: String },
}

/// Spans fully inside `bbox` on `page`, in reading order. A fragment the box
/// covers only in part contributes the known-split parts it contains; any
/// other partial cover is an error.
pub fn in_box(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Span>, SelectError> {
    select(fragments, page, bbox, Mode::Contained)
}

/// Spans with a positive-area intersection with `bbox` on `page`, in reading
/// order: whole runs, or only the known-split parts the box touches when it
/// does not touch them all. Never fails for a valid box.
pub fn overlapping(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Span>, SelectError> {
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
) -> Result<Vec<Span>, SelectError> {
    bbox.validate().map_err(|_| SelectError::InvalidBox)?;
    let mut selected = Vec::new();
    for (index, f) in fragments.iter().enumerate() {
        if f.page != page || !usable(f) {
            continue;
        }
        let run = whole(f);
        let spans = match (cover(bbox, f, &run), mode) {
            (Cover::Outside, _) => continue,
            (Cover::Inside, _) => vec![run],
            (Cover::Cut, Mode::Contained) => contained_parts(f, bbox)?,
            (Cover::Cut, Mode::Overlapping) => touched_parts(f, bbox).unwrap_or_else(|| vec![run]),
        };
        selected.push((index, spans));
    }
    Ok(reading_order(fragments, selected))
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

/// How `bbox` covers `part` of `f`. An edge is measured when the part starts
/// or ends with the fragment (decided by byte offsets, not by float equality),
/// otherwise estimated. Each slack is capped at a quarter of the part's
/// extent on its axis, so the two slacks of an axis sum to less than the
/// extent: a box can then never both contain and miss the part.
fn cover(bbox: Rect, f: &SourceFragment, part: &Span) -> Cover {
    let r = part.bbox;
    let estimated = (ESTIMATED_EDGE_SLACK_EM * f.bbox.height()).max(MEASURED_EDGE_SLACK);
    let horizontal = |measured: bool| {
        let slack = if measured {
            MEASURED_EDGE_SLACK
        } else {
            estimated
        };
        slack.min(r.width() / 4.0)
    };
    let left = horizontal(part.start == 0);
    let right = horizontal(part.end == f.raw_text.len());
    // Parts share the fragment's measured top and bottom.
    let vertical = MEASURED_EDGE_SLACK.min(r.height() / 4.0);
    let overlaps = bbox.left < r.right - right
        && bbox.right > r.left + left
        && bbox.top < r.bottom - vertical
        && bbox.bottom > r.top + vertical;
    let contains = bbox.left <= r.left + left
        && bbox.right >= r.right - right
        && bbox.top <= r.top + vertical
        && bbox.bottom >= r.bottom - vertical;
    if !overlaps {
        Cover::Outside
    } else if contains {
        Cover::Inside
    } else {
        Cover::Cut
    }
}

/// The parts of the one label/value pair that layout derives from `f` alone,
/// in byte order. `None` when there is no such pair, more than one, or parts
/// that are not disjoint nonempty slices of `f`: no split is then known.
fn known_parts(f: &SourceFragment) -> Option<Vec<Span>> {
    let pairs = layout::analyze(std::slice::from_ref(f)).pairs;
    let [pair] = pairs.as_slice() else {
        return None;
    };
    let mut parts: Vec<Span> = std::iter::once(&pair.label)
        .chain(&pair.value)
        .cloned()
        .collect();
    parts.sort_by_key(|s| s.start);
    let valid = parts.len() >= 2
        && parts.iter().all(|s| {
            s.fragment_id == f.id
                && s.start < s.end
                && f.raw_text.get(s.start..s.end).is_some()
                && s.bbox.validate().is_ok()
        })
        && parts.windows(2).all(|w| w[0].end <= w[1].start);
    valid.then_some(parts)
}

/// Parts of a partially covered fragment that `bbox` contains, or the whole
/// run when it contains every part.
fn contained_parts(f: &SourceFragment, bbox: Rect) -> Result<Vec<Span>, SelectError> {
    let cut = || SelectError::SplitsSourceRun {
        fragment_id: f.id.clone(),
    };
    let parts = known_parts(f).ok_or_else(cut)?;
    let mut inside = Vec::new();
    for part in &parts {
        match cover(bbox, f, part) {
            Cover::Cut => return Err(cut()),
            Cover::Inside => inside.push(part.clone()),
            Cover::Outside => {}
        }
    }
    if inside.is_empty() {
        // Only the delimiter gap (or surrounding whitespace) is covered.
        return Err(cut());
    }
    if inside.len() == parts.len() {
        return Ok(vec![whole(f)]);
    }
    Ok(inside)
}

/// Parts of a partially covered fragment that `bbox` touches, when it touches
/// some but not all of them; `None` means the whole run.
fn touched_parts(f: &SourceFragment, bbox: Rect) -> Option<Vec<Span>> {
    let parts = known_parts(f)?;
    let total = parts.len();
    let touched: Vec<Span> = parts
        .into_iter()
        .filter(|part| cover(bbox, f, part) != Cover::Outside)
        .collect();
    (!touched.is_empty() && touched.len() < total).then_some(touched)
}

/// Spans of one selected fragment, keyed by its snapshot index.
type Selected = (usize, Vec<Span>);

/// Flatten per-fragment spans in reading order (see the module docs).
fn reading_order(fragments: &[SourceFragment], mut selected: Vec<Selected>) -> Vec<Span> {
    let rect = |index: usize| fragments[index].bbox;
    selected.sort_by(|(a, _), (b, _)| {
        rect(*a)
            .top
            .total_cmp(&rect(*b).top)
            .then(rect(*a).left.total_cmp(&rect(*b).left))
            .then(a.cmp(b))
    });
    let mut bands: Vec<(Rect, Vec<Selected>)> = Vec::new();
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
