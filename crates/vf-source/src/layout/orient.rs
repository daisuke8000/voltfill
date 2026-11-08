//! Turned text: layout in each fragment's own reading frame.
//!
//! The usable fragments of one turned orientation are copied with their
//! boxes turned by the opposite quarter turn, so that they read left to
//! right with lines advancing down, analysed exactly like upright text, and
//! every output box (lines, run and estimated spans) is turned back into
//! Display coordinates. Line indexes are mapped back to the analysed slice.
//!
//! The turns are about the Display origin rather than the page box: they
//! only swap and negate coordinates, so they are exact, a box turned there
//! and back is bit-identical, and no page geometry is needed. Turned
//! coordinates may be negative; the analysis only compares coordinates and
//! their differences, which a turn about another point would change by a
//! shift alone. Fragments of different orientations are never analysed
//! together, so they never pair.
//!
//! Every rule of the upright analysis applies unchanged in the reading
//! frame (delimiter gap in em, split flags per value line, column and
//! below checks), and so do the limits: the pair cap counts all
//! orientations together, and the copies, their analysis and the mapping
//! back spend from one budget with one deadline.
//!
//! pdf-inspector merges upright runs drawn word by word (gaps up to half an
//! em) into one run, but never merges turned runs. Turned runs closer than
//! that on one reading line are therefore read as pieces of one phrase:
//! they are held, and no pair names any of them, `Inline` included (a
//! `label：` piece followed by a close neighbour is a phrase the upright
//! equivalent would have split elsewhere). They are not joined, so every
//! span stays inside one fragment.
use std::collections::HashSet;

use super::{Layout, LayoutError, Line, Pair, Span};
use crate::budget::{Budget, BudgetError};
use crate::geometry::Rect;
use crate::source::{Orientation, Quality, SourceFragment};

/// The largest gap, in em, between two runs pdf-inspector would have merged
/// had they been upright (`merge_text_items_with_clips`: half the font size).
const PHRASE_GAP_EM: f64 = 0.5;

const TURNED: [Orientation; 3] = [Orientation::Deg90, Orientation::Deg180, Orientation::Deg270];

/// Run `frame`, which reads upright fragments only, on `fragments` and then
/// on reading-frame copies of each turned orientation present, all within
/// `max_pairs` hypotheses in total and spending from `budget` (the frame
/// charges its own work; this charges the copies, the phrase check and the
/// mapping back). Results are grouped by page (stable, so upright results
/// come first within a page).
pub(super) fn analyze(
    fragments: &[SourceFragment],
    max_pairs: usize,
    budget: &mut Budget,
    frame: impl Fn(&[SourceFragment], usize, &mut Budget) -> Result<Layout, LayoutError>,
) -> Result<Layout, LayoutError> {
    let mut layout = frame(fragments, max_pairs, budget)?;
    let mut turned_any = false;
    for orientation in TURNED {
        budget.spend(fragments.len() as u64)?;
        let mut indexes = Vec::new();
        let mut copies = Vec::new();
        for (i, f) in fragments.iter().enumerate() {
            if f.orientation != orientation || f.quality != Quality::Usable {
                continue;
            }
            budget.spend(1 + len(&f.id) + len(&f.raw_text))?;
            indexes.push(i);
            copies.push(SourceFragment {
                bbox: to_reading(f.bbox, orientation),
                orientation: Orientation::Deg0,
                ..f.clone()
            });
        }
        if copies.is_empty() {
            continue;
        }
        turned_any = true;
        // The pairs found so far count against the same cap.
        let remaining = max_pairs.saturating_sub(layout.pairs.len());
        let part = frame(&copies, remaining, budget)?;
        let held = phrase_pieces(&copies, &part.lines, budget)?;
        for line in part.lines {
            budget.spend(1 + line.fragments.len() as u64)?;
            layout.lines.push(Line {
                page: line.page,
                fragments: line.fragments.into_iter().map(|i| indexes[i]).collect(),
                bbox: to_display(line.bbox, orientation),
            });
        }
        for pair in part.pairs {
            let mut names_held = false;
            for span in std::iter::once(&pair.label).chain(&pair.value) {
                budget.spend(1 + len(&span.fragment_id))?;
                names_held |= held.contains(span.fragment_id.as_str());
            }
            if names_held {
                continue;
            }
            budget.spend(1 + pair.value.len() as u64)?;
            layout.pairs.push(Pair {
                label: span_to_display(pair.label, orientation),
                value: pair
                    .value
                    .into_iter()
                    .map(|span| span_to_display(span, orientation))
                    .collect(),
                // Placement, binding and split flags were decided in the
                // reading frame by the upright rules; only boxes turn back.
                ..pair
            });
        }
    }
    if turned_any {
        layout.lines.sort_by_key(|line| line.page);
        layout.pairs.sort_by_key(|pair| pair.page);
    }
    Ok(layout)
}

fn len(text: &str) -> u64 {
    u64::try_from(text.len()).unwrap_or(u64::MAX / 4)
}

/// Ids of reading-frame `copies` that lie within [`PHRASE_GAP_EM`] of a
/// neighbour on their line (overlap counts), the em being the larger of the
/// two runs' extents across the reading direction. `lines` hold indexes
/// into `copies` in reading order.
fn phrase_pieces<'a>(
    copies: &'a [SourceFragment],
    lines: &[Line],
    budget: &mut Budget,
) -> Result<HashSet<&'a str>, BudgetError> {
    let mut held = HashSet::new();
    for line in lines {
        budget.spend(1)?;
        // The earlier run reaching furthest along the line.
        let mut reach: Option<&SourceFragment> = None;
        for &index in &line.fragments {
            let next = &copies[index];
            // A visit, and hashing both ids if the pair is held.
            budget.spend(1 + len(&next.id) + reach.map_or(0, |before| len(&before.id)))?;
            if let Some(before) = reach {
                let em = before.bbox.height().max(next.bbox.height());
                if next.bbox.left - before.bbox.right <= PHRASE_GAP_EM * em {
                    held.insert(before.id.as_str());
                    held.insert(next.id.as_str());
                }
            }
            if reach.is_none_or(|before| next.bbox.right > before.bbox.right) {
                reach = Some(next);
            }
        }
    }
    Ok(held)
}

fn span_to_display(span: Span, orientation: Orientation) -> Span {
    Span {
        bbox: to_display(span.bbox, orientation),
        ..span
    }
}

/// Turn a Display box (y down) so text of `orientation` reads left to right
/// with its glyph tops up: 90° text (bottom to top) maps `(x, y)` to
/// `(-y, x)`, 180° text to `(-x, -y)` and 270° text (top to bottom) to
/// `(y, -x)`.
fn to_reading(r: Rect, orientation: Orientation) -> Rect {
    let Rect {
        left,
        top,
        right,
        bottom,
    } = r;
    match orientation {
        Orientation::Deg0 => r,
        Orientation::Deg90 => Rect {
            left: -bottom,
            top: left,
            right: -top,
            bottom: right,
        },
        Orientation::Deg180 => Rect {
            left: -right,
            top: -bottom,
            right: -left,
            bottom: -top,
        },
        Orientation::Deg270 => Rect {
            left: top,
            top: -right,
            right: bottom,
            bottom: -left,
        },
    }
}

/// Undo [`to_reading`].
fn to_display(r: Rect, orientation: Orientation) -> Rect {
    let inverse = match orientation {
        Orientation::Deg90 => Orientation::Deg270,
        Orientation::Deg270 => Orientation::Deg90,
        same => same,
    };
    to_reading(r, inverse)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Orientation; 4] = [
        Orientation::Deg0,
        Orientation::Deg90,
        Orientation::Deg180,
        Orientation::Deg270,
    ];

    #[test]
    fn turns_are_exact_and_inverse() {
        let r = Rect {
            left: 0.1,
            top: 72.3,
            right: 88.7,
            bottom: 216.9,
        };
        for orientation in ALL {
            let turned = to_reading(r, orientation);
            assert!(turned.validate().is_ok(), "{orientation:?}");
            let (width, height) = match orientation {
                Orientation::Deg0 | Orientation::Deg180 => (r.width(), r.height()),
                Orientation::Deg90 | Orientation::Deg270 => (r.height(), r.width()),
            };
            assert_eq!((turned.width(), turned.height()), (width, height));
            assert_eq!(to_display(turned, orientation), r, "{orientation:?}");
        }
    }

    /// The reading start of each orientation lands on the reading frame's
    /// left edge and the glyph tops on its top edge.
    #[test]
    fn reading_frames_put_the_start_left_and_glyph_tops_up() {
        // A tall run: x 100..112, y 300..444 in Display.
        let run = Rect {
            left: 100.0,
            top: 300.0,
            right: 112.0,
            bottom: 444.0,
        };
        // Its first glyph, and the side of the run its glyph tops face.
        let cases = [
            // Bottom to top: starts at the bottom, tops face left.
            (
                Orientation::Deg90,
                [100.0, 432.0, 112.0, 444.0],
                [100.0, 300.0, 101.0, 444.0],
            ),
            // Top to bottom: starts at the top, tops face right.
            (
                Orientation::Deg270,
                [100.0, 300.0, 112.0, 312.0],
                [111.0, 300.0, 112.0, 444.0],
            ),
        ];
        let rect = |[left, top, right, bottom]: [f64; 4]| Rect {
            left,
            top,
            right,
            bottom,
        };
        for (orientation, first, tops) in cases {
            let frame = to_reading(run, orientation);
            assert_eq!(to_reading(rect(first), orientation).left, frame.left);
            assert_eq!(to_reading(rect(tops), orientation).top, frame.top);
        }
        // Right to left, upside down: starts at the right, tops face down.
        let wide = Rect {
            left: 100.0,
            top: 300.0,
            right: 244.0,
            bottom: 312.0,
        };
        let frame = to_reading(wide, Orientation::Deg180);
        let first = rect([232.0, 300.0, 244.0, 312.0]);
        let tops = rect([100.0, 311.0, 244.0, 312.0]);
        assert_eq!(to_reading(first, Orientation::Deg180).left, frame.left);
        assert_eq!(to_reading(tops, Orientation::Deg180).top, frame.top);
    }
}
