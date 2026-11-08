//! Box-to-source selection over synthetic fragments; no PDF parsing, rendering or API calls.
use std::time::Instant;

use vf_source::budget::Budget;
use vf_source::geometry::{AffineTransform, ImageTransform, Padding, PageGeometry, Rect, Rotation};
use vf_source::layout::{self, Binding, LayoutLimits, Placement, Precision, Span};
use vf_source::select::{
    ESTIMATED_EDGE_TOLERANCE_EM, HEIGHT_PER_ESTIMATED_EM, MAX_COPY_COMPARISONS,
    MEASURED_EDGE_SLACK, Role, SelectError, Selected, contains_label, in_box, in_box_within,
    overlapping, overlapping_within, starts_with_label,
};
use vf_source::{Quality, SourceFragment};

fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Rect {
    Rect::new(left, top, right, bottom)
        .expect("fixture coordinates must form a positive finite rectangle")
}

fn fragment(id: &str, page: u32, text: &str, bbox: Rect) -> SourceFragment {
    SourceFragment {
        id: id.into(),
        page,
        item_index: 0,
        raw_text: text.into(),
        bbox,
        quality: Quality::Usable,
    }
}

fn whole(f: &SourceFragment) -> Selected {
    whole_as(f, Role::Whole)
}

/// The whole fragment with `role` (Whole, a bare Label, or Mixed).
fn whole_as(f: &SourceFragment, role: Role) -> Selected {
    Selected {
        span: Span {
            fragment_id: f.id.clone(),
            start: 0,
            end: f.raw_text.len(),
            bbox: f.bbox,
            precision: Precision::Run,
        },
        role,
    }
}

fn label(span: &Span) -> Selected {
    Selected {
        span: span.clone(),
        role: Role::Label,
    }
}

fn value(span: &Span) -> Selected {
    Selected {
        span: span.clone(),
        role: Role::Value,
    }
}

fn texts<'a>(fragments: &'a [SourceFragment], selected: &[Selected]) -> Vec<&'a str> {
    selected
        .iter()
        .map(|s| {
            fragments
                .iter()
                .find(|f| f.id == s.span.fragment_id)
                .and_then(|f| f.raw_text.get(s.span.start..s.span.end))
                .expect("a selected span must be an exact slice of a source fragment")
        })
        .collect()
}

fn roles(selected: &[Selected]) -> Vec<Role> {
    selected.iter().map(|s| s.role).collect()
}

/// The pairs layout derives from `f` alone.
fn layout_pairs(f: &SourceFragment) -> Vec<layout::Pair> {
    layout::analyze(std::slice::from_ref(f), &LayoutLimits::default())
        .expect("one fragment stays within the default layout limits")
        .pairs
}

/// The label and value parts that layout derives from one inline fragment,
/// bound by its delimiter.
fn inline_parts(f: &SourceFragment) -> (Span, Span) {
    let pairs = layout_pairs(f);
    assert_eq!(pairs.len(), 1, "{}", f.raw_text);
    assert_eq!(pairs[0].placement, Placement::Inline);
    assert_eq!(pairs[0].binding, Binding::Delimiter);
    (pairs[0].label.clone(), pairs[0].value[0].clone())
}

/// Both parts of an inline fragment with their roles, in byte order.
fn both_parts(f: &SourceFragment) -> Vec<Selected> {
    let (l, v) = inline_parts(f);
    vec![label(&l), value(&v)]
}

fn with_left(r: Rect, left: f64) -> Rect {
    rect(left, r.top, r.right, r.bottom)
}

/// `r` with every edge moved outward by `by`, as callers expand model boxes.
fn expand(r: Rect, by: f64) -> Rect {
    rect(r.left - by, r.top - by, r.right + by, r.bottom + by)
}

fn cut(id: &str) -> Result<Vec<Selected>, SelectError> {
    Err(SelectError::SplitsSourceRun {
        fragment_id: id.into(),
    })
}

// Coordinates observed from an embedded-TrueType fixture (see layout tests).
// The observed INV-0012 width, 46.4 units at 11 pt, matches Helvetica advances
// (I 0.278, N 0.722, V 0.667, - 0.333, digits 0.556 em).
fn invoice_fragments() -> Vec<SourceFragment> {
    vec![
        fragment(
            "a",
            1,
            "請求番号：INV-0012",
            rect(60.0, 119.0, 161.4, 130.0),
        ),
        fragment(
            "b",
            1,
            "請求番号： INV-0012",
            rect(60.0, 139.0, 163.4, 150.0),
        ),
        fragment(
            "c",
            1,
            "Invoice No: AB-778",
            rect(60.0, 159.0, 154.8, 170.0),
        ),
    ]
}

/// Observed start of `INV-0012` in fragment a: five full-width characters.
const INVOICE_VALUE_LEFT: f64 = 115.0;

#[test]
fn positive_overlap_keeps_the_full_unicode_run_but_touching_and_rounding_do_not() {
    let raw = [fragment(
        "unicode",
        1,
        "東京🦀e\u{301}\n原文全体",
        rect(0.0, 0.0, 10.0, 10.0),
    )];
    // A sliver across the full width overlaps the whole run.
    let sliver = rect(0.0, 10.0 - 2.0 * MEASURED_EDGE_SLACK, 10.0, 11.0);
    let selected = overlapping(&raw, 1, sliver).expect("a valid box must permit overlap lookup");
    assert_eq!(selected, [whole(&raw[0])]);
    assert_eq!(texts(&raw, &selected), [raw[0].raw_text.as_str()]);
    assert_eq!(in_box(&raw, 1, sliver), cut("unicode"));
    // Overlap within the measured-edge slack (here one f64 ulp) counts as touching.
    let just_below_ten = f64::from_bits(10.0_f64.to_bits() - 1);
    for touching in [
        rect(0.0, just_below_ten, 10.0, 20.0),
        rect(just_below_ten, 0.0, 20.0, 10.0),
        rect(10.0, 0.0, 20.0, 10.0),
        rect(0.0, 10.0, 10.0, 20.0),
        rect(10.0, 10.0, 20.0, 20.0),
    ] {
        assert_eq!(overlapping(&raw, 1, touching), Ok(Vec::new()));
        assert_eq!(in_box(&raw, 1, touching), Ok(Vec::new()));
    }
}

#[test]
fn selections_are_in_reading_order_and_skip_other_pages_and_unusable_source() {
    // Snapshot order differs from visual left-to-right order and identifier order.
    let mut raw = vec![
        fragment("z", 1, "同じ値", rect(20.0, 0.0, 30.0, 10.0)),
        fragment("a", 1, "同じ値", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("other_page", 2, "他ページ", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("suspect", 1, "不確か", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("unknown", 1, "不明", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("blank", 1, " \n ", rect(0.0, 0.0, 10.0, 10.0)),
    ];
    raw[3].quality = Quality::Suspect;
    raw[4].quality = Quality::Unknown;
    let page = rect(0.0, 0.0, 30.0, 10.0);
    for selected in [
        overlapping(&raw, 1, page).expect("a valid box must permit overlap lookup"),
        in_box(&raw, 1, page).expect("fully contained runs must be accepted"),
    ] {
        assert_eq!(selected, [whole(&raw[1]), whole(&raw[0])]);
        assert_eq!(texts(&raw, &selected), ["同じ値", "同じ値"]);
    }
}

#[test]
fn page_queries_accept_contained_runs_and_reject_cut_runs() {
    // Deliberately interleave pages and put IDs out of lexical order.
    let at = |id: &str, page: u32, x: f64| fragment(id, page, "same", rect(x, 0.0, x + 10.0, 10.0));
    let raw = [
        at("z", 2, 0.0),
        at("b", 1, 0.0),
        at("a", 2, 20.0),
        at("c", 1, 20.0),
    ];
    for page in [1, 2, 9] {
        for bbox in [
            rect(0.0, 0.0, 30.0, 10.0),
            rect(10.0, 0.0, 20.0, 10.0), // Edges only: no overlap.
            rect(5.0, 0.0, 25.0, 10.0),  // Partial runs must fail in_box.
        ] {
            let mut expected: Vec<_> = raw
                .iter()
                .filter(|f| {
                    let r = f.bbox;
                    f.page == page
                        && bbox.left < r.right
                        && r.left < bbox.right
                        && bbox.top < r.bottom
                        && r.top < bbox.bottom
                })
                .collect();
            expected.sort_by(|a, b| a.bbox.left.total_cmp(&b.bbox.left));
            let expected: Vec<_> = expected.into_iter().map(whole).collect();
            assert_eq!(overlapping(&raw, page, bbox), Ok(expected.clone()));
            let all_inside = raw
                .iter()
                .filter(|f| expected.iter().any(|s| s.span.fragment_id == f.id))
                .all(|f| bbox.contains(f.bbox));
            if all_inside {
                assert_eq!(in_box(&raw, page, bbox), Ok(expected));
            } else {
                assert!(matches!(
                    in_box(&raw, page, bbox),
                    Err(SelectError::SplitsSourceRun { .. })
                ));
            }
        }
        for invalid in [
            Rect {
                left: f64::NAN,
                top: 0.0,
                right: 30.0,
                bottom: 10.0,
            },
            Rect {
                left: 30.0,
                top: 0.0,
                right: 0.0,
                bottom: 10.0,
            },
        ] {
            assert_eq!(
                overlapping(&raw, page, invalid),
                Err(SelectError::InvalidBox)
            );
            assert_eq!(in_box(&raw, page, invalid), Err(SelectError::InvalidBox));
        }
    }
}

#[test]
fn identical_values_at_distinct_locations_keep_their_own_provenance() {
    let raw = [
        fragment("a", 1, "日本語", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("b", 1, "日本語", rect(20.0, 0.0, 30.0, 10.0)),
    ];
    assert_eq!(in_box(&raw, 1, rect(5.0, 0.0, 15.0, 10.0)), cut("a"));
    assert_eq!(
        overlapping(&raw, 1, rect(5.0, 0.0, 15.0, 10.0)),
        Ok(vec![whole(&raw[0])])
    );
    assert_eq!(
        in_box(&raw, 1, rect(0.0, 0.0, 10.0, 10.0)),
        Ok(vec![whole(&raw[0])])
    );
    assert_eq!(
        in_box(&raw, 1, rect(20.0, 0.0, 30.0, 10.0)),
        Ok(vec![whole(&raw[1])])
    );
}

#[test]
fn value_box_over_a_cjk_inline_pair_selects_only_the_value() {
    let raw = invoice_fragments();
    let a = &raw[0];
    let (_, v) = inline_parts(a);
    assert_eq!(v.precision, Precision::Estimated);
    for bbox in [
        // The observed value box, as a model would see it, expanded by 0.5.
        expand(with_left(a.bbox, INVOICE_VALUE_LEFT), 0.5),
        // A box reaching into the full-width delimiter.
        expand(with_left(a.bbox, 110.0), 0.5),
        // Layout's own estimate (116.3) is within the tolerance of the
        // earliest plausible start (115.0).
        v.bbox,
    ] {
        for selected in [in_box(&raw, 1, bbox), overlapping(&raw, 1, bbox)] {
            let selected = selected.expect("a box at a known split must be accepted");
            assert_eq!(selected, [value(&v)]);
            assert_eq!(texts(&raw, &selected), ["INV-0012"]);
        }
    }
    // With a space after the delimiter, a box reaching into the delimiter
    // still selects the value.
    let b = &raw[1];
    let (_, v) = inline_parts(b);
    let into_delimiter = expand(with_left(b.bbox, 110.0), 0.5);
    assert_eq!(in_box(&raw, 1, into_delimiter), Ok(vec![value(&v)]));
    // Layout's value start for b (119.9) assumes a half-em space; with the
    // observed Helvetica-like metrics the space is 3.1 units, so `I` starts
    // at 118.1 and a box from 119.4 lies inside it. It used to select the
    // whole value; the space's width is not bounded tightly enough to say.
    let at_estimate = expand(with_left(b.bbox, v.bbox.left), 0.5);
    assert_eq!(in_box(&raw, 1, at_estimate), cut("b"));
    assert_eq!(overlapping(&raw, 1, at_estimate), Ok(vec![value(&v)]));
}

#[test]
fn label_box_selects_the_label_and_a_full_box_both_roles() {
    let raw = invoice_fragments();
    let (l, _) = inline_parts(&raw[0]);
    let selected = in_box(&raw, 1, rect(60.0, 119.0, 110.0, 130.0))
        .expect("a box ending in the delimiter must select the label");
    assert_eq!(selected, [label(&l)]);
    assert_eq!(texts(&raw, &selected), ["請求番号"]);
    // A box over the whole fragment returns both parts, never the whole run,
    // so a caller cannot mistake the label for the value.
    let selected = in_box(&raw, 1, rect(59.0, 118.0, 162.0, 131.0))
        .expect("a box over the whole fragment must be accepted");
    assert_eq!(selected, both_parts(&raw[0]));
    assert_eq!(texts(&raw, &selected), ["請求番号", "INV-0012"]);
    // Containing every part, though not the leading whitespace, is the same.
    let padded = [fragment(
        "p",
        1,
        "  備考：午前",
        rect(50.0, 200.0, 120.0, 211.0),
    )];
    let (l, v) = inline_parts(&padded[0]);
    assert!(l.bbox.left > padded[0].bbox.left + 10.0);
    let bbox = rect(l.bbox.left - 1.0, 199.0, v.bbox.right + 1.0, 212.0);
    for selected in [in_box(&padded, 1, bbox), overlapping(&padded, 1, bbox)] {
        let selected = selected.expect("a box over every part must be accepted");
        assert_eq!(selected, both_parts(&padded[0]));
        assert_eq!(texts(&padded, &selected), ["備考", "午前"]);
    }
}

/// Finding: a box over a whole `label：value` line returned one run, so a
/// caller could fill the label as the value.
#[test]
fn whole_line_boxes_return_label_and_value_roles() {
    let raw = invoice_fragments();
    for f in &raw {
        let bbox = expand(f.bbox, 0.5);
        for selected in [in_box(&raw, 1, bbox), overlapping(&raw, 1, bbox)] {
            let selected = selected.expect("a whole-line box must be accepted");
            assert_eq!(selected, both_parts(f));
            assert_eq!(roles(&selected), [Role::Label, Role::Value]);
        }
    }
    let line = expand(raw[2].bbox, 0.5);
    assert_eq!(
        texts(&raw, &in_box(&raw, 1, line).expect("whole line")),
        ["Invoice No", "AB-778"]
    );
    // A bare label run and a value run in separate fragments: the label run
    // ends with its only delimiter, so it is a Label (it used to be Whole,
    // which left ruling it out to the caller); the value run is Whole.
    let runs = [
        fragment("l", 1, "請求番号：", rect(60.0, 119.0, 115.0, 130.0)),
        fragment("v", 1, "INV-0012", rect(115.0, 119.0, 161.4, 130.0)),
    ];
    let selected = in_box(&runs, 1, rect(59.5, 118.5, 161.9, 130.5)).expect("two whole runs");
    assert_eq!(selected, [whole_as(&runs[0], Role::Label), whole(&runs[1])]);
}

/// `Total: 12000` in an 11-pt proportional font: advances from Helvetica.
fn total_fragment() -> SourceFragment {
    // T o t a l : space 1 2 0 0 0, in 1/1000 em.
    let advances = [611, 556, 278, 556, 222, 278, 278, 556, 556, 556, 556, 556];
    let width: i32 = advances.iter().sum();
    fragment(
        "t",
        1,
        "Total: 12000",
        rect(60.0, 119.0, 60.0 + f64::from(width) * 0.011, 130.0),
    )
}

/// Finding: with a proportional Latin font, layout's estimate is 0.46 em off,
/// so a box over `Total: 1` selected only `Total` and a box over `2000`
/// selected `12000`.
#[test]
fn proportional_latin_boundaries_are_not_trusted_for_partial_boxes() {
    let raw = [total_fragment()];
    let f = &raw[0];
    let (l, v) = inline_parts(f);
    // True positions: `Total` ends at 84.45, `12000` spans 90.57–121.15 and
    // `1` ends at 96.69. Layout's estimate starts the value 0.46 em late.
    let (label_end, value_start, one_end) = (84.453, 90.569, 96.685);
    assert!(v.bbox.left - value_start > 0.4 * 11.0, "{}", v.bbox.left);
    for bbox in [
        rect(f.bbox.left, f.bbox.top, one_end, f.bbox.bottom), // `Total: 1`
        rect(one_end, f.bbox.top, f.bbox.right, f.bbox.bottom), // `2000`
        rect(f.bbox.left, f.bbox.top, label_end, f.bbox.bottom), // `Total`
        // `12000`: refused by design, since no box edge can be shown to
        // separate the label from the value in mostly-Latin text.
        rect(value_start, f.bbox.top, f.bbox.right, f.bbox.bottom),
        v.bbox,
        l.bbox,
    ] {
        assert_eq!(in_box(&raw, 1, expand(bbox, 0.5)), cut("t"), "{bbox:?}");
        // overlapping cannot exclude either part.
        assert_eq!(
            overlapping(&raw, 1, expand(bbox, 0.5)),
            Ok(both_parts(f)),
            "{bbox:?}"
        );
    }
    // The text-derived split is still exact for a box over the whole run.
    let selected = in_box(&raw, 1, expand(f.bbox, 0.5)).expect("whole run");
    assert_eq!(selected, both_parts(f));
    assert_eq!(texts(&raw, &selected), ["Total", "12000"]);
    // Value-only boxes over the Latin fixture are refused for the same reason.
    let latin = invoice_fragments();
    let (_, v) = inline_parts(&latin[2]);
    assert_eq!(in_box(&latin, 1, expand(v.bbox, 0.5)), cut("c"));
}

#[test]
fn boxes_cutting_outside_a_known_split_are_rejected() {
    let raw = invoice_fragments();
    let f = &raw[0];
    let (l, v) = inline_parts(f);
    for bbox in [
        rect(115.0, 119.0, 140.0, 130.0), // Middle of the value.
        rect(70.0, 119.0, 161.4, 130.0),  // Middle of the label.
        rect(115.0, 119.0, 161.4, 125.0), // Upper part of the line only.
        // Only the delimiter between the parts.
        rect(l.bbox.right + 0.5, 119.0, v.bbox.left - 0.5, 130.0),
    ] {
        assert_eq!(in_box(&raw, 1, bbox), cut("a"), "{bbox:?}");
    }
    // An interior edge allows ESTIMATED_EDGE_TOLERANCE_EM past the earliest
    // plausible value start...
    let em = f.bbox.height();
    let tolerance = ESTIMATED_EDGE_TOLERANCE_EM * em;
    let inside = with_left(v.bbox, INVOICE_VALUE_LEFT + 0.9 * tolerance);
    assert_eq!(in_box(&raw, 1, inside), Ok(vec![value(&v)]));
    let beyond = with_left(v.bbox, INVOICE_VALUE_LEFT + 1.1 * tolerance);
    assert_eq!(in_box(&raw, 1, beyond), cut("a"));
    // ...so 0.2 em past layout's estimate is a cut. This box used to select
    // INV-0012 although it starts after the whole `I` (115.0–118.1).
    let past_i = with_left(v.bbox, v.bbox.left + 0.2 * em);
    assert!(past_i.left > INVOICE_VALUE_LEFT + 0.278 * em);
    assert_eq!(in_box(&raw, 1, past_i), cut("a"));
    let missing_first = with_left(v.bbox, v.bbox.left + 0.5 * em);
    assert_eq!(in_box(&raw, 1, missing_first), cut("a"));
    // Measured edges get only MEASURED_EDGE_SLACK.
    let run = [fragment(
        "r",
        1,
        "東京都架空区サンプル町1-2-3",
        rect(200.0, 119.0, 346.7, 130.0),
    )];
    let near = rect(200.0, 119.0, 346.7 - MEASURED_EDGE_SLACK / 2.0, 130.0);
    assert_eq!(in_box(&run, 1, near), Ok(vec![whole(&run[0])]));
    let short = rect(200.0, 119.0, 346.7 - 0.1 * em, 130.0);
    assert_eq!(in_box(&run, 1, short), cut("r"));
}

#[test]
fn overlapping_selects_the_parts_a_box_may_touch() {
    let raw = invoice_fragments();
    let parts: Vec<_> = raw.iter().map(inline_parts).collect();
    // A column from inside the full-width delimiters of a and b.
    let column = rect(112.0, 115.0, 170.0, 155.0);
    for selected in [overlapping(&raw, 1, column), in_box(&raw, 1, column)] {
        let selected = selected.expect("a value column must be accepted");
        assert_eq!(selected, [value(&parts[0].1), value(&parts[1].1)]);
        assert_eq!(texts(&raw, &selected), ["INV-0012", "INV-0012"]);
    }
    // A column from 118 starts inside a's `I`: in_box refuses it, while
    // overlapping still reports a's value, and both parts of the Latin line,
    // whose label end it cannot rule out.
    let late = rect(118.0, 115.0, 170.0, 175.0);
    assert_eq!(in_box(&raw, 1, late), cut("a"));
    assert_eq!(
        overlapping(&raw, 1, late),
        Ok(vec![
            value(&parts[0].1),
            value(&parts[1].1),
            label(&parts[2].0),
            value(&parts[2].1),
        ])
    );
    // Touching labels and values selects both parts; in_box rejects the cut labels.
    let wide = rect(80.0, 115.0, 170.0, 175.0);
    assert_eq!(
        overlapping(&raw, 1, wide),
        Ok(raw.iter().flat_map(both_parts).collect::<Vec<_>>())
    );
    assert!(matches!(
        in_box(&raw, 1, wide),
        Err(SelectError::SplitsSourceRun { .. })
    ));
    // A box on the delimiter alone touches no part: all parts, inclusively.
    let (l, v) = &parts[0];
    let gap = rect(l.bbox.right + 0.5, 119.0, v.bbox.left - 0.5, 130.0);
    assert_eq!(overlapping(&raw, 1, gap), Ok(both_parts(&raw[0])));
}

/// Finding: the inner slack scaled with the height while positions come from
/// the width, so tall, condensed or vertical fragments accepted wrong parts.
#[test]
fn implausible_fragment_shapes_only_select_whole_content() {
    let text = "請求番号：INV-0012";
    let cases = [
        // Twice as tall as the em: the old 0.25-height slack (5.5) let a box
        // from 121.0, inside `N`, select INV-0012.
        (rect(60.0, 108.0, 161.4, 130.0), vec![121.0, 115.0, 116.3]),
        // 1.42 times the width-derived em: outside the plausible range (it
        // used to be inside, where the em was too uncertain anyway).
        (rect(60.0, 114.0, 161.4, 130.0), vec![115.0, 116.3, 110.0]),
        // Condensed to 70 %: the value starts at 98.5; 1.39 times the em.
        (rect(60.0, 119.0, 131.0, 130.0), vec![98.5, 101.0, 104.0]),
    ];
    for (bbox, value_lefts) in cases {
        let raw = [fragment("x", 1, text, bbox)];
        for left in value_lefts {
            let value_box = expand(with_left(bbox, left), 0.5);
            assert_eq!(in_box(&raw, 1, value_box), cut("x"), "{bbox:?} {left}");
        }
        let label_box = expand(rect(60.0, bbox.top, 104.0, bbox.bottom), 0.5);
        assert_eq!(in_box(&raw, 1, label_box), cut("x"), "{bbox:?}");
        assert_eq!(in_box(&raw, 1, expand(bbox, 0.5)), Ok(both_parts(&raw[0])));
    }
    // A vertical run, 11 wide and 101 tall: x positions say nothing about
    // where the label ends. The old code returned 請求番号 for the left half.
    let raw = [fragment("x", 1, text, rect(60.0, 119.0, 71.0, 220.0))];
    for bbox in [
        rect(59.5, 118.5, 65.5, 220.5),
        rect(65.5, 118.5, 71.5, 220.5),
        rect(59.5, 118.5, 71.5, 170.0),
    ] {
        assert_eq!(in_box(&raw, 1, bbox), cut("x"), "{bbox:?}");
        assert_eq!(overlapping(&raw, 1, bbox), Ok(both_parts(&raw[0])));
    }
    assert_eq!(
        in_box(&raw, 1, expand(raw[0].bbox, 0.5)),
        Ok(both_parts(&raw[0]))
    );
}

/// Finding: times and ratios with a full-width colon were known splits.
#[test]
fn digits_around_either_colon_are_not_a_split() {
    // `比率 1：2` used to be Whole. It has the shape of `受付 10：00～17：00`
    // (a label, a space, then a digit-guarded colon), so it is now Mixed:
    // a colon between digits is exempt only in text made of numbers.
    for (text, width, role) in [
        ("10：30", 33.0, Role::Whole),
        ("１０：３０", 55.0, Role::Whole),
        ("1：2", 22.0, Role::Whole),
        ("比率 1：2", 49.5, Role::Mixed),
        ("10 : 30", 30.0, Role::Whole),
    ] {
        let raw = [fragment(
            "x",
            1,
            text,
            rect(60.0, 119.0, 60.0 + width, 130.0),
        )];
        let right_half = rect(60.0 + width / 2.0 + 1.0, 118.5, 60.5 + width, 130.5);
        assert_eq!(in_box(&raw, 1, right_half), cut("x"), "{text}");
        assert_eq!(
            overlapping(&raw, 1, right_half),
            Ok(vec![whole_as(&raw[0], role)])
        );
        assert_eq!(
            in_box(&raw, 1, expand(raw[0].bbox, 0.5)),
            Ok(vec![whole_as(&raw[0], role)]),
            "{text}"
        );
    }
    // A time after a label still splits at the label's delimiter.
    let raw = [fragment(
        "x",
        1,
        "時間：9:00",
        rect(60.0, 119.0, 114.4, 130.0),
    )];
    let (l, v) = inline_parts(&raw[0]);
    let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole run");
    assert_eq!(selected, [label(&l), value(&v)]);
    assert_eq!(texts(&raw, &selected), ["時間", "9:00"]);
    let value_box = expand(rect(93.0, 119.0, 114.4, 130.0), 0.5);
    assert_eq!(in_box(&raw, 1, value_box), Ok(vec![value(&v)]));
}

/// Finding: any pair layout returns was trusted. Whitespace is not a
/// delimiter, so these stay whole whether or not layout splits them; the
/// test deliberately makes no assertion about layout's pairs.
#[test]
fn whitespace_separated_texts_stay_whole() {
    for (text, bbox, first_word_right) in [
        ("佐藤 花子", rect(60.0, 135.0, 115.6, 148.0), 86.0),
        (
            "発行日 2026年11月8日",
            rect(60.0, 119.0, 182.0, 130.0),
            93.0,
        ),
    ] {
        let raw = [fragment("x", 1, text, bbox)];
        let first_word = rect(59.5, bbox.top - 0.5, first_word_right, bbox.bottom + 0.5);
        assert_eq!(in_box(&raw, 1, first_word), cut("x"), "{text}");
        assert_eq!(
            in_box(&raw, 1, expand(bbox, 0.5)),
            Ok(vec![whole(&raw[0])]),
            "{text}"
        );
    }
    // Whitespace around one delimiter is still a known split.
    let raw = [fragment(
        "x",
        1,
        "備考 ： 午前",
        rect(60.0, 119.0, 126.0, 130.0),
    )];
    let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole run");
    assert_eq!(roles(&selected), [Role::Label, Role::Value]);
    assert_eq!(texts(&raw, &selected), ["備考", "午前"]);
}

/// Finding: overprinted copies (faux bold) were both returned, and a box
/// edge between them failed the call.
#[test]
fn overprinted_copies_are_selected_once() {
    let text = "請求番号：INV-0012";
    let original = rect(60.0, 119.0, 161.4, 130.0);
    let offset = rect(60.3, 119.2, 161.7, 130.2);
    // Contains the original, cuts the offset copy by 0.2 on the right.
    let between = rect(59.5, 118.5, 161.5, 130.5);
    let around = expand(offset, 0.5);
    for (first, second) in [(original, offset), (offset, original)] {
        let raw = [
            fragment("p", 1, text, first),
            fragment("q", 1, text, second),
        ];
        let chosen = if first == original { &raw[0] } else { &raw[1] };
        assert_eq!(in_box(&raw, 1, between), Ok(both_parts(chosen)));
        // When the box holds both copies, the first in snapshot order wins.
        for selected in [in_box(&raw, 1, around), overlapping(&raw, 1, around)] {
            assert_eq!(selected, Ok(both_parts(&raw[0])));
        }
        let middle = rect(100.0, 118.5, 140.0, 130.5);
        assert_eq!(in_box(&raw, 1, middle), cut("p"));
    }
    // Copies 2 units apart (more than 0.1 em at 11 pt) are separate fragments.
    let raw = [
        fragment("p", 1, "同じ値", rect(60.0, 119.0, 93.0, 130.0)),
        fragment("q", 1, "同じ値", rect(62.0, 119.0, 95.0, 130.0)),
    ];
    assert_eq!(
        in_box(&raw, 1, rect(59.5, 118.5, 95.5, 130.5)),
        Ok(vec![whole(&raw[0]), whole(&raw[1])])
    );
    assert_eq!(in_box(&raw, 1, rect(59.5, 118.5, 93.5, 130.5)), cut("q"));
}

/// Finding: an expanded box touching only a neighbour's leading or trailing
/// blank space failed the whole call.
#[test]
fn blank_margins_of_neighbours_do_not_cut_them() {
    let raw = [
        fragment("due", 1, "支払期限 ", rect(68.0, 119.0, 115.0, 130.0)),
        fragment("v", 1, "INV-0012", rect(115.0, 119.0, 161.4, 130.0)),
        // A 3.06-unit space, then five full-width characters.
        fragment("next", 1, " 担当：佐藤", rect(161.4, 119.0, 219.46, 130.0)),
        fragment("plain", 1, " 担当者", rect(161.4, 139.0, 197.46, 150.0)),
    ];
    let expanded = expand(raw[1].bbox, 0.5);
    for selected in [in_box(&raw, 1, expanded), overlapping(&raw, 1, expanded)] {
        assert_eq!(selected, Ok(vec![whole(&raw[1])]));
    }
    let below = rect(114.5, 118.5, 162.0, 150.5);
    assert_eq!(in_box(&raw, 1, below), Ok(vec![whole(&raw[1])]));
    // Reaching into a neighbour's text still cuts it.
    let into_next = rect(114.5, 118.5, 170.0, 130.5);
    assert_eq!(in_box(&raw, 1, into_next), cut("next"));
    let into_due = rect(100.0, 118.5, 161.9, 130.5);
    assert_eq!(in_box(&raw, 1, into_due), cut("due"));
}

#[test]
fn multi_line_box_returns_spans_in_reading_order() {
    // Snapshot order: second line, first line, then a postal code left of the
    // first line with a slightly lower top.
    let raw = [
        fragment("l2", 1, "テストビル5階", rect(200.0, 135.0, 272.1, 146.0)),
        fragment(
            "l1",
            1,
            "東京都架空区サンプル町1-2-3",
            rect(200.0, 119.0, 346.7, 130.0),
        ),
        fragment("zip", 1, "〒100-0001", rect(120.0, 120.0, 190.0, 129.5)),
        fragment("label", 1, "住所", rect(60.0, 119.0, 82.0, 130.0)),
    ];
    let bbox = rect(110.0, 115.0, 360.0, 150.0);
    for selected in [in_box(&raw, 1, bbox), overlapping(&raw, 1, bbox)] {
        let selected = selected.expect("whole runs inside the box must be accepted");
        assert_eq!(
            texts(&raw, &selected),
            ["〒100-0001", "東京都架空区サンプル町1-2-3", "テストビル5階"]
        );
        assert!(
            selected
                .iter()
                .all(|s| s.span.precision == Precision::Run && s.role == Role::Whole)
        );
    }
}

#[test]
fn boxes_on_other_pages_or_empty_regions_select_nothing() {
    let raw = invoice_fragments();
    let everything = rect(0.0, 0.0, 600.0, 800.0);
    assert_eq!(in_box(&raw, 2, everything), Ok(Vec::new()));
    assert_eq!(overlapping(&raw, 2, everything), Ok(Vec::new()));
    let empty = rect(300.0, 300.0, 400.0, 400.0);
    assert_eq!(in_box(&raw, 1, empty), Ok(Vec::new()));
    assert_eq!(overlapping(&raw, 1, empty), Ok(Vec::new()));
    assert_eq!(in_box(&[], 1, everything), Ok(Vec::new()));
}

#[test]
fn unusable_fragments_are_ignored_even_when_cut() {
    let mut raw = vec![
        fragment(
            "suspect",
            1,
            "請求番号：INV-0012",
            rect(60.0, 119.0, 161.4, 130.0),
        ),
        fragment("unknown", 1, "A", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("blank", 1, "  ", rect(0.0, 0.0, 10.0, 10.0)),
        fragment("nan", 1, "B", rect(0.0, 0.0, 10.0, 10.0)),
    ];
    raw[0].quality = Quality::Suspect;
    raw[1].quality = Quality::Unknown;
    raw[3].bbox.right = f64::NAN;
    for bbox in [rect(5.0, 0.0, 15.0, 10.0), rect(115.0, 119.0, 140.0, 130.0)] {
        assert_eq!(in_box(&raw, 1, bbox), Ok(Vec::new()));
        assert_eq!(overlapping(&raw, 1, bbox), Ok(Vec::new()));
    }
}

/// A model box returned as f32: each edge rounded one f32 step inward when
/// not exact, the worst case for containment.
fn model_box(transform: &ImageTransform, display: Rect) -> Rect {
    let (w, h) = transform
        .sent_dimensions()
        .expect("fixture image dimensions must be valid");
    let (w, h) = (f64::from(w), f64::from(h));
    let to_raster = transform.display_to_raster;
    let sx = f64::from(transform.resized_width) / transform.crop.width();
    let sy = f64::from(transform.resized_height) / transform.crop.height();
    let x = |v: f64| {
        ((to_raster.a * v + to_raster.e - transform.crop.left) * sx
            + f64::from(transform.padding.left))
            / w
    };
    let y = |v: f64| {
        ((to_raster.d * v + to_raster.f - transform.crop.top) * sy
            + f64::from(transform.padding.top))
            / h
    };
    let up = |v: f64| {
        let r = v as f32;
        f64::from(if f64::from(r) < v { r.next_up() } else { r })
    };
    let down = |v: f64| {
        let r = v as f32;
        f64::from(if f64::from(r) > v { r.next_down() } else { r })
    };
    let normalized = rect(
        up(x(display.left)),
        up(y(display.top)),
        down(x(display.right)),
        down(y(display.bottom)),
    );
    transform
        .normalized_box_to_display(normalized)
        .expect("an inward-rounded box inside the content must map back")
}

#[test]
fn f32_model_boxes_on_a_rotated_page_round_trip_to_source_spans() {
    let a4 = rect(0.0, 0.0, 595.0, 842.0);
    let page = PageGeometry::new(a4, a4, Rotation::Deg90, 1.0)
        .expect("an A4 page at UserUnit 1 must be valid");
    let bounds = page.display_bounds().expect("page bounds must be valid");
    assert_eq!(bounds, rect(0.0, 0.0, 842.0, 595.0));
    let item = |id: &str, text: &str, x: f64, top: f64, width: f64| {
        let bbox = page
            .extractor_rect_to_display(x, bounds.bottom - top - 11.0, width, 11.0)
            .expect("fixture text items must lie inside the page");
        fragment(id, 1, text, bbox)
    };
    let raw = [
        item("invoice", "請求番号：INV-0012", 60.0, 119.0, 101.4),
        item(
            "address",
            "東京都架空区サンプル町1-2-3",
            200.0,
            135.0,
            146.7,
        ),
    ];
    let scale = 150.0 / 72.0;
    let transform = ImageTransform {
        display_bounds: bounds,
        raster_width: 1755,
        raster_height: 1240,
        display_to_raster: AffineTransform {
            a: scale,
            b: 0.0,
            c: 0.0,
            d: scale,
            e: 0.0,
            f: 0.0,
        },
        crop: rect(0.0, 0.0, 1755.0, 1240.0),
        resized_width: 1003,
        resized_height: 709,
        padding: Padding {
            left: 7,
            top: 13,
            right: 3,
            bottom: 5,
        },
    };
    transform
        .validate()
        .expect("fixture image transform must be valid");
    // Whole runs: rounding moved the box inside the run; only the slack
    // accepts it. The invoice run returns both of its parts.
    let expected = [both_parts(&raw[0]), vec![whole(&raw[1])]];
    for (f, expected) in raw.iter().zip(expected) {
        let bbox = model_box(&transform, f.bbox);
        assert!(!bbox.contains(f.bbox), "{bbox:?}");
        assert_eq!(in_box(&raw, 1, bbox), Ok(expected));
    }
    // A model sees the value where it is drawn: five full-width characters
    // (55 units) after the run start. Layout's estimate is accepted too.
    let (_, v) = inline_parts(&raw[0]);
    let drawn = with_left(raw[0].bbox, raw[0].bbox.left + 55.0);
    for display in [drawn, v.bbox] {
        let bbox = model_box(&transform, display);
        let selected = in_box(&raw, 1, bbox).expect("a rounded value box must select the value");
        assert_eq!(selected, [value(&v)]);
        assert_eq!(texts(&raw, &selected), ["INV-0012"]);
    }
    let both = model_box(
        &transform,
        rect(
            raw[0].bbox.left,
            raw[0].bbox.top,
            raw[1].bbox.right,
            raw[1].bbox.bottom,
        ),
    );
    let mut expected = both_parts(&raw[0]);
    expected.push(whole(&raw[1]));
    assert_eq!(overlapping(&raw, 1, both), Ok(expected));
}

/// Helvetica advances in em for printable ASCII; wide characters one em,
/// zero-width characters none, anything else half an em.
fn advance_em(c: char) -> f64 {
    const ASCII: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    match c as u32 {
        code @ 0x20..=0x7E => f64::from(ASCII[(code - 0x20) as usize]) / 1000.0,
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F | 0xFEFF | 0xE0100..=0xE01EF => 0.0,
        0x3000..=0x9FFF | 0xFF00..=0xFF60 => 1.0,
        _ => 0.5,
    }
}

/// `text` drawn at 11 pt from x 60, top 119, with horizontal scale `scale`
/// (`Tz`): the extractor still reports an 11-unit height.
fn drawn(id: &str, text: &str, scale: f64) -> SourceFragment {
    let width: f64 = text.chars().map(advance_em).sum::<f64>() * 11.0 * scale;
    fragment(id, 1, text, rect(60.0, 119.0, 60.0 + width, 130.0))
}

/// x where the text after `prefix` starts in a fragment made by [`drawn`].
fn drawn_x(prefix: &str, scale: f64) -> f64 {
    60.0 + prefix.chars().map(advance_em).sum::<f64>() * 11.0 * scale
}

fn assert_nothing_fillable(selected: &[Selected], context: &str) {
    assert!(
        selected.iter().all(|s| !s.role.is_fillable()),
        "{context}: {selected:?}"
    );
}

/// Finding N1: a run holding two fields returned a Value (or, after layout
/// stopped splitting runs with two delimiters, a Whole) containing the next
/// field.
#[test]
fn runs_holding_two_fields_are_never_fillable() {
    for (text, label_prefix) in [
        ("TEL：03-1234-5678　FAX：03-1234-5679", "TEL："),
        ("電話：03-1234-5678　FAX：03-1234-5679", "電話："),
        ("Phone：555-1234 Fax：555-9876", "Phone："),
        ("氏名：佐藤花子　電話：03-1234-5678", "氏名："),
        // A second delimiter that layout does not split at, in the value...
        ("氏名：佐藤花子　電話∶03-1234-5678", "氏名："),
        ("番号：No.1：2026年度", "番号："),
        ("金額：Total = 12000", "金額："),
        // ...or in the label: the value would belong to FAX, not TEL.
        ("TEL∶03-1234-5678　FAX：03-1234-5679", "TEL∶"),
    ] {
        let raw = [drawn("x", text, 1.0)];
        let line = expand(raw[0].bbox, 0.5);
        for selected in [in_box(&raw, 1, line), overlapping(&raw, 1, line)] {
            let selected = selected.expect("a whole-line box must be accepted");
            assert_eq!(selected, [whole_as(&raw[0], Role::Mixed)], "{text}");
            assert_nothing_fillable(&selected, text);
        }
        // A box placed exactly over the first value cuts the run.
        let value = rect(
            drawn_x(label_prefix, 1.0) - 0.5,
            118.5,
            raw[0].bbox.right + 0.5,
            130.5,
        );
        assert_eq!(in_box(&raw, 1, value), cut("x"), "{text}");
        assert_eq!(
            overlapping(&raw, 1, value),
            Ok(vec![whole_as(&raw[0], Role::Mixed)])
        );
    }
    // Digits around a colon, a URL and a date-time stay one clean value. (A
    // URL with a port after a label, `URL：https://host:8080/`, has a second
    // delimiter for layout and is Mixed.)
    for (text, expected) in [
        ("時間：9:00", "9:00"),
        ("日時：2026/11/08 10:30", "2026/11/08 10:30"),
        ("URL：https://example.com/?a=1", "https://example.com/?a=1"),
    ] {
        let raw = [drawn("x", text, 1.0)];
        let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole line");
        assert_eq!(roles(&selected), [Role::Label, Role::Value], "{text}");
        assert_eq!(texts(&raw, &selected)[1], expected);
    }
    // Without a second delimiter, the Value is the rest of the run and may
    // carry the next field: the caller rejects it with contains_label.
    let text = "請求日：2026/11/08 支払期限 2026/12/31";
    let raw = [drawn("x", text, 1.0)];
    let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole line");
    assert_eq!(selected, both_parts(&raw[0]));
    let value = texts(&raw, &selected)[1];
    assert_eq!(value, "2026/11/08 支払期限 2026/12/31");
    assert!(contains_label(value, "支払期限"));
    assert!(contains_label(value, "支払 期限"));
    assert!(!contains_label(value, "請求日"));
    assert!(!contains_label(value, " \u{200B}"));
}

/// Finding N2: Whole could be a label and its value.
#[test]
fn whole_runs_with_a_separator_are_mixed() {
    for text in [
        "請求番号∶INV-0012",  // U+2236
        "請求番号︰INV-0012", // U+FE30
        "請求番号﹕INV-0012", // U+FE55
        "請求番号︓INV-0012", // U+FE13
        "請求番号　INV-0012", // U+3000
        "Total = 12000",
        "合計＝12,000円",
        "受付 10：00～17：00",
        "No.1：2026年度",
        "午前10:30",
        "佐藤　花子", // U+3000 inside a value run: refused too (recall cost)
    ] {
        let raw = [drawn("x", text, 1.0)];
        let line = expand(raw[0].bbox, 0.5);
        for selected in [in_box(&raw, 1, line), overlapping(&raw, 1, line)] {
            assert_eq!(selected, Ok(vec![whole_as(&raw[0], Role::Mixed)]), "{text}");
        }
    }
    // Bare labels: clean text, then one delimiter at the end.
    for text in [
        "請求番号：",
        "氏　　名：",
        "Invoice No: ",
        "No.1：",
        "備考：\u{200B}",
    ] {
        let raw = [drawn("x", text, 1.0)];
        assert_eq!(
            in_box(&raw, 1, expand(raw[0].bbox, 0.5)),
            Ok(vec![whole_as(&raw[0], Role::Label)]),
            "{text}"
        );
    }
    // Clean runs stay Whole; the caller rules out labels it knows.
    for text in [
        "10：30",
        "9:00 〜 18:00",
        "2026/11/08 10:30",
        "https://example.com:8080/?a=1",
        "佐藤 花子",
        "発行日 2026年11月8日",
    ] {
        let raw = [drawn("x", text, 1.0)];
        assert_eq!(
            in_box(&raw, 1, expand(raw[0].bbox, 0.5)),
            Ok(vec![whole(&raw[0])]),
            "{text}"
        );
    }
    assert!(starts_with_label("発行日 2026年11月8日", "発行日"));
    assert!(starts_with_label("発 行 日\u{200B}2026年", "発行日"));
    assert!(!starts_with_label("2026年11月8日", "発行日"));
    assert!(!starts_with_label("発行日", ""));
    assert!(Role::Whole.is_fillable() && Role::Value.is_fillable());
    assert!(!Role::Label.is_fillable() && !Role::Mixed.is_fillable());
}

/// Finding N3: a condensed run (`Tz`) overstates the em, so a box missing
/// the first value character still selected the value.
#[test]
fn condensed_runs_select_only_with_a_whole_line_box() {
    let text = "ご請求書番号：INV-0012";
    let prefix = "ご請求書番号：";
    for scale in [0.85, 0.9, 0.95] {
        let raw = [drawn("x", text, scale)];
        let f = &raw[0];
        let height_per_em = f.bbox.height() / (f.bbox.width() / 11.0);
        assert!(
            height_per_em > *HEIGHT_PER_ESTIMATED_EM.end(),
            "{scale}: {height_per_em}"
        );
        let value_left = drawn_x(prefix, scale);
        let i_right = value_left + 0.278 * 11.0 * scale;
        // Reviewer's case at 90 %: the box from 132.10 starts after `I`.
        if scale == 0.9 {
            assert!((value_left - 129.3).abs() < 1e-9 && i_right < 132.1);
            let past_i = rect(132.10, 119.0, f.bbox.right, 130.0);
            assert_eq!(in_box(&raw, 1, past_i), cut("x"));
        }
        for left in [i_right + 0.05, value_left - 0.5] {
            let value_box = expand(with_left(f.bbox, left), 0.0);
            assert_eq!(in_box(&raw, 1, value_box), cut("x"), "{scale} {left}");
        }
        let selected = in_box(&raw, 1, expand(f.bbox, 0.5)).expect("whole line");
        assert_eq!(selected, both_parts(f));
        assert_eq!(texts(&raw, &selected), ["ご請求書番号", "INV-0012"]);
    }
    // Drawn at full width, the same run keeps its known split for a value box.
    let raw = [drawn("x", text, 1.0)];
    let (_, v) = inline_parts(&raw[0]);
    let value_left = drawn_x(prefix, 1.0);
    let correct = expand(with_left(raw[0].bbox, value_left), 0.5);
    assert_eq!(in_box(&raw, 1, correct), Ok(vec![value(&v)]));
    let past_i = with_left(raw[0].bbox, value_left + 0.278 * 11.0 + 0.05);
    assert_eq!(in_box(&raw, 1, past_i), cut("x"));
}

/// Finding N4: grouping depended on snapshot order, so three chained copies
/// (60~61 and 61~62 within 1.1, 60 and 62 not) were returned twice.
#[test]
fn chained_copies_are_one_group_in_any_snapshot_order() {
    let at =
        |id: &str, left: f64| fragment(id, 1, "INV-0012", rect(left, 119.0, left + 46.4, 130.0));
    let lefts = [60.0, 61.0, 62.0];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let raw: Vec<_> = order
            .iter()
            .map(|&i| at(&format!("c{i}"), lefts[i]))
            .collect();
        // Contains every copy: the first in snapshot order, once.
        let all = rect(59.5, 118.5, 108.9, 130.5);
        assert_eq!(in_box(&raw, 1, all), Ok(vec![whole(&raw[0])]), "{order:?}");
        assert_eq!(
            overlapping(&raw, 1, all),
            Ok(vec![whole(&raw[0])]),
            "{order:?}"
        );
        // Contains the copy at 60 and cuts the others: no error, once.
        let first = rect(59.5, 118.5, 106.9, 130.5);
        let at_60 = raw
            .iter()
            .find(|f| f.bbox.left == 60.0)
            .expect("the copy at 60 is in every order");
        assert_eq!(in_box(&raw, 1, first), Ok(vec![whole(at_60)]), "{order:?}");
    }
}

/// Finding N5: zero-width and combining characters shift the class-weight
/// estimates, and `備考：` + U+200B gave an invisible Value.
#[test]
fn zero_width_characters_unbound_edges_and_never_fill_alone() {
    // `請求番号：WM-0012` is wide enough that one more half-em weight keeps
    // the run inside HEIGHT_PER_ESTIMATED_EM: only the zero-width rule
    // refuses the value box below.
    let plain = [drawn("x", "請求番号：WM-0012", 1.0)];
    let value_box = expand(with_left(plain[0].bbox, drawn_x("請求番号：", 1.0)), 0.5);
    let (_, v) = inline_parts(&plain[0]);
    assert_eq!(in_box(&plain, 1, value_box), Ok(vec![value(&v)]));
    for mark in ['\u{301}', '\u{200B}', '\u{FE0F}', '\u{FEFF}', '\u{E0100}'] {
        let text = format!("請求番号：W{mark}M-0012");
        let raw = [fragment("x", 1, &text, plain[0].bbox)];
        let height_per_em = 11.0 / (raw[0].bbox.width() / 9.0);
        assert!(HEIGHT_PER_ESTIMATED_EM.contains(&height_per_em), "{text:?}");
        assert_eq!(in_box(&raw, 1, value_box), cut("x"), "{text:?}");
        let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole line");
        assert_eq!(selected, both_parts(&raw[0]), "{text:?}");
    }
    // An invisible value is no split: the run is a bare label.
    let raw = [drawn("x", "備考：\u{200B}", 1.0)];
    for selected in [
        in_box(&raw, 1, expand(raw[0].bbox, 0.5)),
        overlapping(&raw, 1, expand(raw[0].bbox, 0.5)),
    ] {
        let selected = selected.expect("whole line");
        assert_eq!(selected, [whole_as(&raw[0], Role::Label)]);
        assert_nothing_fillable(&selected, "備考：U+200B");
    }
    // A fragment with nothing visible is skipped like a blank one.
    let invisible = [fragment(
        "z",
        1,
        "\u{200B}\u{FEFF}\u{301}",
        rect(60.0, 119.0, 70.0, 130.0),
    )];
    let cutting = rect(65.0, 119.0, 80.0, 130.0);
    assert_eq!(in_box(&invisible, 1, cutting), Ok(Vec::new()));
    assert_eq!(overlapping(&invisible, 1, cutting), Ok(Vec::new()));
}

/// Finding N6 (documented limit): a value of one or a few narrow characters
/// pulls the width-derived em below the height, so the run looks condensed
/// and only a whole-line box selects it.
#[test]
fn single_narrow_character_values_need_a_whole_line_box() {
    for (text, expected) in [("番号：I", "I"), ("担当：I-1", "I-1")] {
        let raw = [drawn("x", text, 1.0)];
        let value_box = expand(with_left(raw[0].bbox, drawn_x("番号：", 1.0)), 0.5);
        assert_eq!(in_box(&raw, 1, value_box), cut("x"), "{text}");
        let selected = in_box(&raw, 1, expand(raw[0].bbox, 0.5)).expect("whole line");
        assert_eq!(selected, both_parts(&raw[0]));
        assert_eq!(texts(&raw, &selected)[1], expected);
    }
}

/// Finding N7: copy search is bounded. Two interleaved same-text columns
/// whose tops differ by 1e-4 put thousands of non-copies in each band; the
/// call fails with a typed error instead of taking quadratic time. A page of
/// ordinary copies stays far below the limit (see tests/select_perf.rs).
#[test]
fn copy_search_beyond_its_limit_is_a_typed_error() {
    // About n²/2 comparisons, well over the limit.
    let n = 3_000;
    assert!(n * n / 2 > 4 * MAX_COPY_COMPARISONS);
    let raw: Vec<_> = (0..n)
        .map(|i| {
            let (left, top) = (60.0 + 10.0 * (i % 2) as f64, 119.0 + 1e-4 * i as f64);
            fragment(
                &format!("c{i}"),
                1,
                "INV-0012",
                rect(left, top, left + 46.4, top + 11.0),
            )
        })
        .collect();
    let all = rect(59.5, 118.5, 116.9, 131.0);
    assert_eq!(in_box(&raw, 1, all), Err(SelectError::LimitExceeded));
    assert_eq!(overlapping(&raw, 1, all), Err(SelectError::LimitExceeded));
    // A quarter as many (about 280,000 comparisons) stay within the limit:
    // one copy per column.
    let half = &raw[..n / 4];
    let selected = in_box(half, 1, all).expect("within the copy-comparison limit");
    assert_eq!(selected, [whole(&half[0]), whole(&half[1])]);
}

/// A fragment layout does not split (several delimiters, a port colon after a
/// label, an empty label side) has no known split: whatever the box, select
/// never returns any of it with a fillable role.
#[test]
fn fragments_layout_does_not_split_are_never_fillable() {
    // Text, the text before the first delimiter, and through it.
    for (text, before, through) in [
        ("TEL：03-1234-5678　FAX：03-1234-5679", "TEL", "TEL："),
        ("請求番号：INV-0012：A", "請求番号", "請求番号："),
        ("氏名：佐藤花子　電話：03-1234-5678", "氏名", "氏名："),
        ("URL：https://example.com:8080/", "URL", "URL："),
        ("：INV-0012", "", "："),
        (": INV-0012", "", ": "),
    ] {
        let raw = [drawn("x", text, 1.0)];
        assert!(layout_pairs(&raw[0]).is_empty(), "{text}");
        let r = raw[0].bbox;
        let (start, split) = (drawn_x(before, 1.0), drawn_x(through, 1.0));
        let (top, bottom) = (r.top - 0.5, r.bottom + 0.5);
        let boxes = [
            expand(r, 0.5),
            expand(r, 3.0),
            // Label side, value side (at and past the split), the delimiter
            // alone, and the right half.
            rect(r.left - 0.5, top, split - 0.5, bottom),
            rect(split - 0.5, top, r.right + 0.5, bottom),
            rect(split + 0.5, top, r.right + 0.5, bottom),
            rect(start - 0.5, top, split + 0.5, bottom),
            rect(0.5 * (r.left + r.right), top, r.right + 0.5, bottom),
        ];
        for bbox in boxes {
            let context = format!("{text} {bbox:?}");
            match in_box(&raw, 1, bbox) {
                Ok(selected) => assert_nothing_fillable(&selected, &context),
                Err(error) => assert_eq!(
                    error,
                    SelectError::SplitsSourceRun {
                        fragment_id: "x".into()
                    },
                    "{context}"
                ),
            }
            let selected = overlapping(&raw, 1, bbox).expect("a valid box overlaps");
            assert_nothing_fillable(&selected, &context);
        }
    }
}

/// select charges its work, layout's one-fragment split included, to the
/// caller's budget: any budget short of what a call needs fails with a typed
/// error instead of returning a different (unsplit) result, and a passed
/// deadline fails before any work.
#[test]
fn select_work_is_charged_to_the_callers_budget() {
    let raw = invoice_fragments();
    let line = expand(raw[0].bbox, 0.5);
    let expected = in_box(&raw, 1, line).expect("whole line");
    assert_eq!(expected, both_parts(&raw[0]));
    let mut budget = Budget::new(u64::MAX, None);
    assert_eq!(
        in_box_within(&raw, 1, line, &mut budget),
        Ok(expected.clone())
    );
    let needed = u64::MAX - budget.remaining();
    assert!(needed > 0);
    for max_work in 0..needed {
        let mut short = Budget::new(max_work, None);
        assert_eq!(
            in_box_within(&raw, 1, line, &mut short),
            Err(SelectError::BudgetExceeded),
            "{max_work} of {needed} units"
        );
    }
    let mut exact = Budget::new(needed, None);
    assert_eq!(in_box_within(&raw, 1, line, &mut exact), Ok(expected));
    assert_eq!(exact.remaining(), 0);
    let mut late = Budget::new(u64::MAX, Some(Instant::now()));
    assert_eq!(
        in_box_within(&raw, 1, line, &mut late),
        Err(SelectError::DeadlineExceeded)
    );
    assert_eq!(
        overlapping_within(&raw, 1, line, &mut late),
        Err(SelectError::DeadlineExceeded)
    );
    assert_eq!(late.remaining(), u64::MAX);
    // An invalid box is reported before the deadline.
    let invalid = Rect {
        left: 10.0,
        top: 10.0,
        right: 5.0,
        bottom: 20.0,
    };
    assert_eq!(
        in_box_within(&raw, 1, invalid, &mut late),
        Err(SelectError::InvalidBox)
    );
}
