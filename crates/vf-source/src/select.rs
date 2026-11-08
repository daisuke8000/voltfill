//! Source spans selected by a Display-space box, for answers that name a
//! region instead of text (for example a model box mapped back with
//! [`ImageTransform::normalized_box_to_display`]). Planned as the source side
//! of an image fallback for form fill; nothing in fill uses it yet.
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
//! - Any other fragment is returned whole, `0..raw_text.len()`, untrimmed,
//!   with the fragment box and [`Precision::Run`]:
//!   - as [`Role::Whole`] when its text is *clean* (below) and has no
//!     U+3000 IDEOGRAPHIC SPACE inside;
//!   - as [`Role::Label`] when it is a bare label: clean text followed by one
//!     colon-like delimiter at the end of its visible text (`請求番号：`);
//!   - as [`Role::Mixed`] otherwise: it may hold a label with its value, or
//!     several fields, and select cannot tell where one ends. Examples:
//!     `TEL：03-1234-5678　FAX：03-1234-5679` (layout does not split a run
//!     with two delimiters), `請求番号∶INV-0012` (a lookalike colon),
//!     `請求番号　INV-0012` (U+3000), `Total = 12000`, `No.1：2026年度` and
//!     `受付 10：00～17：00` (digits around the colon, but not a time).
//!
//! U+3000 makes a whole run `Mixed` because Japanese layouts use it to space
//! a label from its value; the cost is that a standalone value run such as
//! `佐藤　花子` is refused too. Inside a known split's label or value it is
//! allowed: the delimiter already separates them.
//!
//! ## Clean text
//!
//! Text is clean when it has at least one visible character (not whitespace,
//! a control, or a zero-width or combining character, see
//! [Edge positions](#edge-positions)) and no delimiter character. Delimiter
//! characters are the colons `:` `：`, their lookalikes `∶` (U+2236), `︰`
//! (U+FE30), `﹕` (U+FE55), `︓` (U+FE13), `꞉` (U+A789), `˸` (U+02F8), `ː`
//! (U+02D0), `⁚` (U+205A), `⦂` (U+2982), `׃` (U+05C3), and the equals signs
//! `=` `＝` `﹦`. Like layout's exceptions, with a stricter rule for digits,
//! these are not delimiters:
//!
//! - a colon followed by `//`, and any character of a whitespace-separated
//!   token that starts with a URL scheme (`https://example.com:8080/?a=1`);
//! - a colon whose nearest visible neighbours are digits (ASCII or full
//!   width) on both sides, provided every visible character of the text is a
//!   digit, a colon, or one of `/ ／ - － ‐ ‑ – — − ~ ～ 〜 . ． , ，`: times,
//!   ratios and date-times such as `10：30`, `10 : 30`, `9:00 〜 18:00` and
//!   `2026/11/08 10:30`. Any other character in the text makes such a colon a
//!   delimiter (`No.1：2026年度`, `受付 10：00～17：00`, `午前10:30`).
//!
//! # Fill values
//!
//! A fill layer may take text only from spans whose role
//! [is fillable](Role::is_fillable), `Whole` and `Value`, returned by
//! [`in_box`]; never from `Label` or `Mixed` spans and never from
//! [`overlapping`]. A fillable span holds no delimiter beyond its own split,
//! but select does not know the form, so the caller must still reject:
//!
//! - a `Whole` whose text starts with the form label or holds another form
//!   label ([`starts_with_label`], [`contains_label`]): a run with no
//!   delimiter can be a label on its own (`請求番号` followed by a separate
//!   value run) or a label and value separated by a space
//!   (`発行日 2026年11月8日`);
//! - a `Value` that holds another form label ([`contains_label`]): a `Value`
//!   is the rest of its run after the delimiter, so a run without a second
//!   delimiter can still carry the next field
//!   (`請求日：2026/11/08 支払期限 2026/12/31` yields
//!   `2026/11/08 支払期限 2026/12/31`);
//! - a `Value` returned together with the `Label` of the same run unless that
//!   `Label` matches the form label: the value belongs to that label (a box
//!   over `佐藤花子　電話：03-1234-5678` drawn for 氏名 yields the value of
//!   電話).
//!
//! # Known splits
//!
//! A known split is the one label/value pair that [`layout::analyze_within`]
//! derives from that fragment alone, accepted only when all of these hold:
//!
//! - layout binds the label and value by a delimiter inside this fragment:
//!   the pair is [`Placement::Inline`] with [`Binding::Delimiter`] and exactly
//!   one value span (one line), and both spans are nonempty, ordered,
//!   char-boundary ranges of this fragment. On one fragment layout yields
//!   only inline pairs, all delimiter-bound, so today the binding check
//!   rejects nothing the placement check accepts; it keeps a position-only
//!   ([`Binding::Geometric`]) split from becoming parts should layout ever
//!   produce one. Layout does not split a fragment with several delimiters
//!   (`TEL：…　FAX：…`) or an empty label side (`：INV-0012`), so such a
//!   fragment has no known split and is returned whole as `Mixed` (or a bare
//!   `Label`), never fillable;
//! - the label and value texts have no surrounding whitespace, the text
//!   before the label and after the value is whitespace only, and the text
//!   between them, trimmed, is exactly one delimiter of this module's own
//!   set (`：` or `:`). A delimiter or a whitespace-only split that layout may
//!   add later is refused here until this set is updated, so arbitrary
//!   substrings (`佐藤 花子`, `発行日 2026年11月8日`) never become parts;
//! - the label's last and the value's first character are not both digits
//!   (ASCII or full width), so times and ratios with either colon
//!   (`10：30`, `1：2`) stay whole. Layout skips only ASCII `9:00` today and
//!   should adopt the same rule;
//! - the label and the value are both clean: no second delimiter on either
//!   side (`番号：No.1：2026年度`, `氏名：佐藤花子　電話∶03-1234-5678`), and
//!   a visible character in each (`備考：` followed by U+200B is a bare
//!   label, not a split).
//!
//! Layout's work for the split is charged to the call's [`Budget`]; running
//! out of it or passing its deadline fails the call (see
//! [Duplicates, order and cost](#duplicates-order-and-cost)) and is never
//! taken for "no split".
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
//!   candidate display only, and fails only for an invalid box or
//!   [`SelectError::LimitExceeded`].
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
//! contain or to miss a part; otherwise the part is cut.
//!
//! Interior edges are unbounded, so [`in_box`] selects the fragment only with
//! a box containing the whole content (as its parts, when a split is known),
//! when either of these holds:
//!
//! - the height is not plausible for one horizontal line of its width:
//!   height / width-derived em outside [`HEIGHT_PER_ESTIMATED_EM`]. Above its
//!   upper end the run is likely condensed (`Tz`, 文字の縮小): pdf-inspector
//!   reports the height as font size × max(horizontal, vertical scale), so
//!   both the height and the width-derived em overstate the advance em, and
//!   a box missing the first value character would still clear the
//!   interval. Tall boxes, rotated or vertical runs fall outside too;
//! - the text has a character without its own advance: a control, a
//!   combining mark (U+0300–U+036F and the other combining blocks), a kana
//!   voicing mark (U+3099–U+309A), a zero-width or format character
//!   (U+200B–U+200F, U+202A–U+202E, U+2060–U+206F, U+FEFF, U+00AD) or a
//!   variation selector (U+FE00–U+FE0F, U+E0100–U+E01EF). Fragments without
//!   any visible character are not usable at all.
//!
//! Worked examples, both 11 units high:
//!
//! - `請求番号：INV-0012` at x 60–161.4: weights 9, so the em is 11–11.27.
//!   The value starts after five full-width characters, at 115.0–116.3; the
//!   label ends at 104.0–105.1. A box over the rest of the line whose left
//!   edge lies between x 103.4 (105.07 − 0.15 em) and 116.6 (115.0 + 0.15 em)
//!   selects only the value; the observed value start is 115.0.
//! - `Total: 12000` in a proportional 11-unit font at x 60–121.1: the
//!   width-derived em is 10.19, so height / em is 1.08 and the run is treated
//!   as possibly condensed. Even with a bounded em, the value would start
//!   somewhere in 74.3–111.0 and the label end in 70.2–106.9 (layout
//!   estimates 95.7 for a true 90.6, 0.46 em off). Every partial box is
//!   `SplitsSourceRun`; only a box over the whole run selects its label and
//!   value. Value-only boxes over mostly-Latin `label: value` runs are
//!   therefore refused by design.
//!
//! Limits, until fragments carry the horizontal scale and per-glyph advances:
//!
//! - condensation is detected only from the width: a run condensed by a few
//!   percent (scale above about 0.96 for `ご請求書番号：INV-0012`), or one whose
//!   narrow characters are wider than half an em on average so that they
//!   mask the condensation (caps such as `WWW-MMM`), keeps bounded edges, and
//!   a box edge inside its first value character can then be accepted;
//! - proportional CJK metrics (for example `palt`) and letter spacing are
//!   not modelled; they can move a boundary beyond its interval;
//! - a value of one or a few narrow characters (`番号：I`, `担当：I-1`) pulls
//!   the width-derived em below the height, so such runs look condensed and
//!   a box over the value alone is refused even when correctly placed; a box
//!   over the whole run still returns the label and the value.
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
//! Overprinted copies (faux bold) are one selection. Copies are looked for
//! among the usable fragments on the page *near the box*: those that
//! intersect the box grown on every side by [`DUPLICATE_EDGE_TOLERANCE_EM`] ×
//! their own height. Two of them are linked when they have the same text and
//! all four edges within [`DUPLICATE_EDGE_TOLERANCE_EM`] × the smaller
//! height, and the connected sets of links (found with a union-find, so the
//! snapshot order does not matter) are the groups. The growth includes every
//! copy linked to a fragment the box touches; a chain of copies that leaves
//! that region and comes back forms separate groups.
//!
//! The box is tested against each copy of a group in snapshot order and the
//! first copy it selects is returned; a group whose copies it only misses or
//! cuts is skipped, and it is an error only when the box cuts every copy. The
//! copies differ by less than any glyph, so a box edge between them is not a
//! cut.
//!
//! Results are in reading order: fragments are grouped into bands with the
//! same rule as layout lines (vertical overlap of at least half the smaller
//! height), bands top to bottom, fragments left to right, ties by snapshot
//! index; parts of one fragment are in byte order.
//!
//! Only usable fragments (usable quality, a visible character, valid box) are
//! considered; others are skipped, never reported as errors. Fragment IDs are
//! not checked for uniqueness: [`crate::inspect`] issues unique IDs.
//!
//! Cost per call, for n fragments of which m are near the box: one pass over
//! the n fragments, O(m log m) to sort the m by text and box, and at most
//! [`MAX_COPY_COMPARISONS`] comparisons of a fragment with an earlier
//! same-text one in its top band (union-find with path compression, so each
//! is nearly constant). Comparisons are skipped over identical boxes, over
//! fragments already in the same group, and over same-top fragments too far
//! to the left, so ordinary pages, chains of copies and identical copies
//! need O(m). A page that needs more (for example thousands of distinct
//! same-text boxes in one band that are not copies) fails with
//! [`SelectError::LimitExceeded`] instead of taking quadratic time. Copies
//! the box intersects are then analyzed, identical boxes of a group once, in
//! time linear in their text until one is selected, and the selected
//! fragments are sorted.
//!
//! The work is charged to a [`Budget`], the type layout and fill use, so one
//! work budget and deadline can cover layout, fill and select:
//! [`in_box_within`] and [`overlapping_within`] take the caller's budget,
//! while [`in_box`] and [`overlapping`] use a fresh one of
//! [`layout::DEFAULT_MAX_WORK`] units without a deadline. Units are spent
//! before the work they pay for: one per fragment of the snapshot; the text
//! bytes of each fragment on the page near the box (checked for a visible
//! character); one per sort comparison for the m fragments and again for the
//! selected ones (n·⌈log2(n + 1)⌉ each, as layout charges; a comparison by
//! text reads at most the shorter text, which this does not count); one per
//! copy comparison; and, for each copy the box is tested against,
//! [`ANALYSIS_UNITS_PER_BYTE`] per byte of its text and ID plus layout's
//! units for its one-fragment split ([`layout::analyze_within`]). The
//! deadline is checked when the call starts and at least every
//! [`crate::budget::CHECK_INTERVAL`] units. Running out of units or passing
//! the deadline fails the call with [`SelectError::BudgetExceeded`] or
//! [`SelectError::DeadlineExceeded`]; no partial result is returned. The
//! units bound the work; they do not guarantee a running time.
//!
//! [`MAX_COPY_COMPARISONS`] stays a per-call cap beside the budget: the
//! budget bounds the total over calls and adds the deadline, while the cap
//! keeps a single box over an adversarial page from taking quadratic time
//! however large the caller's budget is.
//!
//! [`ImageTransform::normalized_box_to_display`]: crate::geometry::ImageTransform::normalized_box_to_display
use std::ops::RangeInclusive;

use serde::Serialize;

use crate::budget::{Budget, BudgetError};
use crate::geometry::Rect;
use crate::layout::{self, Binding, LayoutError, Placement, Precision, Span};
use crate::source::{Orientation, Quality, SourceFragment};

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

/// Plausible fragment height per width-derived em for one horizontal line of
/// uncondensed text. Outside it, interior edges are unbounded. The upper end
/// is the condensed-run check: unscaled `請求番号： INV-0012` and `時間：9:00`
/// in Helvetica-like metrics are at 1.011, while `ご請求書番号：INV-0012`
/// condensed to 95 % is at 1.032 and to 90 % at 1.089.
pub const HEIGHT_PER_ESTIMATED_EM: RangeInclusive<f64> = 0.7..=1.02;

/// Overprinted copies: same text and every edge within this fraction of the
/// smaller fragment height (1.1 Display units at 11 pt).
pub const DUPLICATE_EDGE_TOLERANCE_EM: f64 = 0.1;

/// Most comparisons between same-text fragments one call makes while looking
/// for overprinted copies (see the module docs); beyond it the call fails
/// with [`SelectError::LimitExceeded`]. A per-call cap on top of the
/// [`Budget`]: however large the caller's budget, one box never takes
/// quadratic time.
pub const MAX_COPY_COMPARISONS: usize = 1_000_000;

/// Work units per byte of a fragment's text and ID for each copy the box is
/// tested against: select's own passes (character classes, cleanliness, the
/// edge bounds of every tested range, spans copied into the result) are a
/// small fixed number per byte, rounded up to this. Layout's units for the
/// one-fragment split are charged on top (see [`layout::analyze_within`]).
pub const ANALYSIS_UNITS_PER_BYTE: u64 = 32;

/// Delimiters of a known split; mirrors layout's inline delimiters.
const DELIMITERS: [char; 2] = ['：', ':'];

/// Colon-like characters: layout's delimiters and lookalikes it does not
/// split at. Any of them may separate a label from a value.
const COLON_LIKE: [char; 12] = [
    ':', '：', '∶', '︰', '﹕', '︓', '꞉', '˸', 'ː', '⁚', '⦂', '׃',
];

/// Equals signs, which may separate a label from a value (`Total = 12000`).
const EQUALS_LIKE: [char; 3] = ['=', '＝', '﹦'];

/// Characters other than digits and colons that may join the parts of a
/// time, ratio or date-time.
const NUMERIC_JOINERS: [char; 16] = [
    '/', '／', '-', '－', '‐', '‑', '–', '—', '−', '~', '～', '〜', '.', '．', ',', '，',
];

/// What a selected span is within its fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A whole fragment of clean text with no U+3000 inside (see the module
    /// docs). Fillable, but it may still be a label on its own or a label and
    /// value separated by a space.
    Whole,
    /// The label part of a known split, or a whole fragment that is a bare
    /// label (`請求番号：`). Never a fill value.
    Label,
    /// The value part of a known split: the rest of the run after the
    /// delimiter. Fillable, but it may still carry the next field when no
    /// delimiter separates it.
    Value,
    /// A whole fragment that may hold a label with its value, or several
    /// fields, that select cannot separate. Never a fill value.
    Mixed,
}

impl Role {
    /// Whether a span with this role may become a fill value: `Whole` and
    /// `Value`, and only from [`in_box`], after the caller checks in the
    /// module docs.
    pub fn is_fillable(self) -> bool {
        matches!(self, Self::Whole | Self::Value)
    }
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
    /// Looking for overprinted copies near the box needed more than
    /// [`MAX_COPY_COMPARISONS`] comparisons.
    #[error("selection exceeded its overprinted-copy comparison limit")]
    LimitExceeded,
    /// The [`Budget`] ran out of work units.
    #[error("selection exceeded its work budget")]
    BudgetExceeded,
    /// The [`Budget`]'s deadline passed.
    #[error("selection exceeded its deadline")]
    DeadlineExceeded,
}

impl From<BudgetError> for SelectError {
    fn from(error: BudgetError) -> Self {
        match error {
            BudgetError::WorkExceeded => Self::BudgetExceeded,
            BudgetError::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }
}

/// Spans fully inside `bbox` on `page`, in reading order. A fragment the box
/// covers only in part contributes the parts of its known split that the box
/// contains; any other partial cover is an error, as are
/// [`SelectError::LimitExceeded`] and the budget errors. See the module docs
/// for which spans may become fill values. Runs within a fresh [`Budget`] of
/// [`layout::DEFAULT_MAX_WORK`] units without a deadline; see
/// [`in_box_within`].
pub fn in_box(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Selected>, SelectError> {
    in_box_within(fragments, page, bbox, &mut default_budget())
}

/// [`in_box`], spending work from `budget` (units in the module docs).
///
/// # Errors
/// As [`in_box`]; [`SelectError::BudgetExceeded`] when the budget runs out
/// and [`SelectError::DeadlineExceeded`] once its deadline has passed. No
/// partial result is returned.
pub fn in_box_within(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
    budget: &mut Budget,
) -> Result<Vec<Selected>, SelectError> {
    select(fragments, page, bbox, Mode::Contained, budget)
}

/// Spans `bbox` may touch on `page`, in reading order: whole runs, or the
/// parts of a known split the box may touch. Inclusive, for diagnostics and
/// candidate display: it must not be used to produce fill values. Fails only
/// for an invalid box, [`SelectError::LimitExceeded`] or a budget error.
/// Runs within a fresh [`Budget`] as [`in_box`] does.
pub fn overlapping(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
) -> Result<Vec<Selected>, SelectError> {
    overlapping_within(fragments, page, bbox, &mut default_budget())
}

/// [`overlapping`], spending work from `budget` (units in the module docs).
///
/// # Errors
/// As [`overlapping`]; budget errors as for [`in_box_within`].
pub fn overlapping_within(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
    budget: &mut Budget,
) -> Result<Vec<Selected>, SelectError> {
    select(fragments, page, bbox, Mode::Overlapping, budget)
}

fn default_budget() -> Budget {
    Budget::new(layout::DEFAULT_MAX_WORK, None)
}

/// Comparisons charged for sorting `n` items: n·⌈log2(n + 1)⌉, as layout
/// charges its sorts.
fn sort_units(n: usize) -> u64 {
    let n = u64::try_from(n).unwrap_or(u64::MAX);
    n.saturating_mul(u64::from(u64::BITS - n.leading_zeros()))
}

/// Bytes as work units.
fn byte_units(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// Whether `text` starts with `label`, ignoring whitespace and invisible
/// characters in both. select does not normalize: pass both in the caller's
/// own normalized form (for example NFKC and lowercase, as fill compares
/// labels). A label without visible characters never matches.
pub fn starts_with_label(text: &str, label: &str) -> bool {
    let label = visible_text(label);
    !label.is_empty() && visible_text(text).starts_with(&label)
}

/// Whether `text` holds `label` anywhere, ignoring whitespace and invisible
/// characters in both; normalization as for [`starts_with_label`].
pub fn contains_label(text: &str, label: &str) -> bool {
    let label = visible_text(label);
    !label.is_empty() && visible_text(text).contains(&label)
}

fn visible_text(text: &str) -> String {
    text.chars().filter(|&c| is_visible(c)).collect()
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
    budget: &mut Budget,
) -> Result<Vec<Selected>, SelectError> {
    bbox.validate().map_err(|_| SelectError::InvalidBox)?;
    budget.check_deadline()?;
    let mut selected = Vec::new();
    for copies in overprint_groups(fragments, page, bbox, budget)? {
        // Prefer a copy the box selects, then one it misses; report a cut
        // only when the box cuts every copy.
        let mut missed = false;
        let mut cut = None;
        let mut chosen = None;
        let mut tested: Vec<Rect> = Vec::new();
        for index in copies {
            let r = fragments[index].bbox;
            if !intersects(bbox, r) {
                missed = true;
                continue;
            }
            // Copies share their text, so an identical box gives the same
            // answer; at most a few distinct boxes per group are expected.
            if tested.len() < 8 {
                if tested.contains(&r) {
                    continue;
                }
                tested.push(r);
            }
            // A budget error fails the call; it is never taken for a cut.
            let source = Source::new(&fragments[index], budget)?;
            match source.select(bbox, mode) {
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
    reading_order(fragments, selected, budget)
}

/// Positive-area intersection without slack: when false, the box misses the
/// fragment's content under any slack, so the fragment needs no analysis.
fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn grow(r: Rect, by: f64) -> Rect {
    Rect {
        left: r.left - by,
        top: r.top - by,
        right: r.right + by,
        bottom: r.bottom + by,
    }
}

/// Upright (`Orientation::Deg0`) usable fragments only: coverage is measured
/// along the horizontal advance, so a turned fragment is never selected.
fn usable(f: &SourceFragment) -> bool {
    f.quality == Quality::Usable
        && f.orientation == Orientation::Deg0
        && f.raw_text.chars().any(is_visible)
        && f.bbox.validate().is_ok()
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

/// Usable fragments on `page` near `bbox`, grouped into overprinted copies
/// (see the module docs): each group in snapshot order, groups by their first
/// index.
fn overprint_groups(
    fragments: &[SourceFragment],
    page: u32,
    bbox: Rect,
    budget: &mut Budget,
) -> Result<Vec<Vec<usize>>, SelectError> {
    budget.spend(byte_units(fragments.len()))?;
    let mut candidates: Vec<usize> = Vec::new();
    for (i, f) in fragments.iter().enumerate() {
        // The text is read (by `usable`) only for fragments near the box.
        if f.page == page
            && intersects(
                grow(bbox, DUPLICATE_EDGE_TOLERANCE_EM * f.bbox.height()),
                f.bbox,
            )
        {
            budget.spend(byte_units(f.raw_text.len()))?;
            if usable(f) {
                candidates.push(i);
            }
        }
    }
    let n = candidates.len();
    let at = |place: usize| &fragments[candidates[place]];
    // Positions into `candidates`, by text and box: within one text, tops
    // ascend, same tops by left, and identical boxes are adjacent.
    let mut order: Vec<usize> = (0..n).collect();
    budget.spend(sort_units(n))?;
    order.sort_by(|&a, &b| {
        let (fa, fb) = (&fragments[candidates[a]], &fragments[candidates[b]]);
        fa.raw_text
            .cmp(&fb.raw_text)
            .then(fa.bbox.top.total_cmp(&fb.bbox.top))
            .then(fa.bbox.left.total_cmp(&fb.bbox.left))
            .then(fa.bbox.right.total_cmp(&fb.bbox.right))
            .then(fa.bbox.bottom.total_cmp(&fb.bbox.bottom))
            .then(a.cmp(&b))
    });
    let mut sets = DisjointSets::new(n);
    // For each place in `order`, the first place of its run with the same
    // text, the same text and top, the same text and box, and the same set
    // when it was processed (sets only merge, so that run stays one set).
    let (mut text_start, mut top_start) = (vec![0; n], vec![0; n]);
    let (mut box_start, mut set_start) = (vec![0; n], vec![0; n]);
    let mut comparisons_left = MAX_COPY_COMPARISONS;
    for k in 0..n {
        let b = at(order[k]);
        let (mut same_text, mut same_top, mut same_box) = (false, false, false);
        if k > 0 {
            let a = at(order[k - 1]);
            same_text = a.raw_text == b.raw_text;
            same_top = same_text && a.bbox.top == b.bbox.top;
            same_box = same_text && a.bbox == b.bbox;
        }
        text_start[k] = if same_text { text_start[k - 1] } else { k };
        top_start[k] = if same_top { top_start[k - 1] } else { k };
        box_start[k] = if same_box { box_start[k - 1] } else { k };
        if same_box {
            sets.union(order[k - 1], order[k]);
        } else {
            // The pairwise tolerance never exceeds this fraction of b's height.
            let band = DUPLICATE_EDGE_TOLERANCE_EM * b.bbox.height();
            let mut root = sets.find(order[k]);
            let mut p = k;
            while p > text_start[k] {
                p -= 1;
                let a = at(order[p]);
                if b.bbox.top - a.bbox.top > band {
                    break;
                }
                comparisons_left = comparisons_left
                    .checked_sub(1)
                    .ok_or(SelectError::LimitExceeded)?;
                budget.spend(1)?;
                if sets.find(order[p]) == root {
                    p = set_start[p];
                } else if same_place(a.bbox, b.bbox) {
                    sets.union(order[p], order[k]);
                    root = sets.find(order[k]);
                    p = set_start[p];
                } else if a.bbox.top == b.bbox.top && b.bbox.left - a.bbox.left > band {
                    // Earlier boxes with this top are further left still.
                    p = top_start[p];
                } else {
                    p = box_start[p];
                }
            }
        }
        set_start[k] = if k > text_start[k] && sets.find(order[k - 1]) == sets.find(order[k]) {
            set_start[k - 1]
        } else {
            k
        };
    }
    let mut group_of: Vec<Option<usize>> = vec![None; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (position, &index) in candidates.iter().enumerate() {
        let root = sets.find(position);
        match group_of[root] {
            Some(group) => groups[group].push(index),
            None => {
                group_of[root] = Some(groups.len());
                groups.push(vec![index]);
            }
        }
    }
    Ok(groups)
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

/// Union-find over `0..n` with path compression and union by size.
struct DisjointSets {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl DisjointSets {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut node = x;
        while self.parent[node] != root {
            let next = self.parent[node];
            self.parent[node] = root;
            node = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        let (big, small) = if self.size[a] >= self.size[b] {
            (a, b)
        } else {
            (b, a)
        };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
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
    fn new(f: &'a SourceFragment, budget: &mut Budget) -> Result<Self, SelectError> {
        budget.spend(
            ANALYSIS_UNITS_PER_BYTE.saturating_mul(byte_units(f.raw_text.len() + f.id.len())),
        )?;
        let r = f.bbox;
        let weight = weight(&f.raw_text);
        let height_em = r.height();
        let width_em = if weight > 0.0 {
            r.width() / weight
        } else {
            height_em
        };
        let plausible = HEIGHT_PER_ESTIMATED_EM.contains(&(height_em / width_em))
            && !f.raw_text.chars().any(has_no_advance);
        let em = plausible.then(|| (height_em.min(width_em), height_em.max(width_em)));
        let tolerance =
            (ESTIMATED_EDGE_TOLERANCE_EM * height_em.min(width_em)).max(MEASURED_EDGE_SLACK);
        let parts = match known_split(f, budget)? {
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
                let end = text.trim_end().len();
                vec![Part {
                    role: text.get(start..end).map_or(Role::Mixed, whole_role),
                    span: whole(f),
                    range: (start, end),
                    estimate: None,
                }]
            }
        };
        Ok(Self {
            f,
            weight,
            em,
            tolerance,
            parts,
        })
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

/// Combining marks, kana voicing marks, zero-width and format characters and
/// variation selectors: no advance or glyph of their own.
fn is_zero_width(c: char) -> bool {
    matches!(c as u32,
        0x00AD
        | 0x0300..=0x036F
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x200B..=0x200F
        | 0x202A..=0x202E
        | 0x2060..=0x206F
        | 0x20D0..=0x20FF
        | 0x3099..=0x309A
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
        | 0xFEFF
        | 0xE0100..=0xE01EF)
}

/// A character whose advance the class weights cannot estimate.
fn has_no_advance(c: char) -> bool {
    c.is_control() || is_zero_width(c)
}

fn is_visible(c: char) -> bool {
    !(c.is_whitespace() || has_no_advance(c))
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('０'..='９').contains(&c)
}

/// Whether `text` is clean (see the module docs).
fn is_clean(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let url = url_chars(&chars);
    let mut visible = false;
    let mut numeric_shape = true;
    let mut numeric_colon = false;
    for (k, &c) in chars.iter().enumerate() {
        if !is_visible(c) {
            continue;
        }
        visible = true;
        numeric_shape &=
            !url[k] && (is_digit(c) || COLON_LIKE.contains(&c) || NUMERIC_JOINERS.contains(&c));
        if url[k] {
            continue;
        }
        if EQUALS_LIKE.contains(&c) {
            return false;
        }
        if COLON_LIKE.contains(&c) {
            if chars.get(k + 1..k + 3) == Some(&['/', '/'][..]) {
                continue;
            }
            let before = chars[..k].iter().rev().find(|&&c| is_visible(c));
            let after = chars[k + 1..].iter().find(|&&c| is_visible(c));
            if !(before.is_some_and(|&c| is_digit(c)) && after.is_some_and(|&c| is_digit(c))) {
                return false;
            }
            numeric_colon = true;
        }
    }
    visible && (numeric_shape || !numeric_colon)
}

/// Marks the characters of whitespace-separated tokens that start with a URL
/// scheme (`scheme://`, the scheme an ASCII letter then letters, digits, `+`,
/// `-` or `.`).
fn url_chars(chars: &[char]) -> Vec<bool> {
    let mut url = vec![false; chars.len()];
    let mut start = 0;
    while start < chars.len() {
        if chars[start].is_whitespace() {
            start += 1;
            continue;
        }
        let end = chars[start..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| start + offset);
        let token = &chars[start..end];
        let scheme = token
            .iter()
            .position(|&c| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .unwrap_or(token.len());
        if scheme > 0
            && token[0].is_ascii_alphabetic()
            && token.get(scheme..scheme + 3) == Some(&[':', '/', '/'][..])
        {
            url[start..end].fill(true);
        }
        start = end;
    }
    url
}

/// The role of a whole fragment whose trimmed text is `content`.
fn whole_role(content: &str) -> Role {
    if is_clean(content) && !content.contains('\u{3000}') {
        return Role::Whole;
    }
    let visible = content.trim_end_matches(|c: char| !is_visible(c));
    match visible.chars().next_back() {
        Some(last) if COLON_LIKE.contains(&last) => {
            let label = &visible[..visible.len() - last.len_utf8()];
            if is_clean(label) {
                Role::Label
            } else {
                Role::Mixed
            }
        }
        _ => Role::Mixed,
    }
}

/// The label and value of the one delimiter-bound inline pair layout derives
/// from `f` alone, when it passes the checks in the module docs. Layout's
/// work is charged to `budget`; a budget or deadline error fails the call.
fn known_split(
    f: &SourceFragment,
    budget: &mut Budget,
) -> Result<Option<(Span, Span)>, SelectError> {
    // One fragment yields at most one pair, so the hypothesis limit is never
    // hit; if it were, the fragment would have no known split (whole_role
    // then never makes delimiter text fillable).
    let pairs = match layout::analyze_within(std::slice::from_ref(f), 1, budget) {
        Ok(layout) => layout.pairs,
        Err(LayoutError::LimitExceeded) => return Ok(None),
        Err(LayoutError::BudgetExceeded) => return Err(SelectError::BudgetExceeded),
        Err(LayoutError::DeadlineExceeded) => return Err(SelectError::DeadlineExceeded),
    };
    Ok(split_parts(f, &pairs))
}

/// The checks of the module docs on the pairs layout derived from `f` alone.
fn split_parts(f: &SourceFragment, pairs: &[layout::Pair]) -> Option<(Span, Span)> {
    let [pair] = pairs else {
        return None;
    };
    let [value] = pair.value.as_slice() else {
        return None;
    };
    let label = &pair.label;
    let text = f.raw_text.as_str();
    let shape = pair.placement == Placement::Inline
        && pair.binding == Binding::Delimiter
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
    let clean = is_clean(label_text) && is_clean(value_text);
    (one_delimiter && trimmed && !number && clean).then(|| (label.clone(), value.clone()))
}

/// Spans of one selected fragment, keyed by its snapshot index.
type Item = (usize, Vec<Selected>);

/// Flatten per-fragment spans in reading order (see the module docs).
fn reading_order(
    fragments: &[SourceFragment],
    mut selected: Vec<Item>,
    budget: &mut Budget,
) -> Result<Vec<Selected>, SelectError> {
    // The sort here and the per-band sorts below compare O(k log k) pairs.
    budget.spend(sort_units(selected.len()).saturating_mul(2))?;
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
    Ok(bands
        .into_iter()
        .flat_map(|(_, mut items)| {
            items.sort_by(|(a, _), (b, _)| rect(*a).left.total_cmp(&rect(*b).left).then(a.cmp(b)));
            items
        })
        .flat_map(|(_, spans)| spans)
        .collect())
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
