//! Box-to-source selection over synthetic fragments; no PDF parsing, rendering or API calls.
use vf_source::geometry::{AffineTransform, ImageTransform, Padding, PageGeometry, Rect, Rotation};
use vf_source::layout::{self, Placement, Precision, Span};
use vf_source::select::{MEASURED_EDGE_SLACK, SelectError, in_box, overlapping};
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

fn whole(f: &SourceFragment) -> Span {
    Span {
        fragment_id: f.id.clone(),
        start: 0,
        end: f.raw_text.len(),
        bbox: f.bbox,
        precision: Precision::Run,
    }
}

fn texts<'a>(fragments: &'a [SourceFragment], spans: &[Span]) -> Vec<&'a str> {
    spans
        .iter()
        .map(|s| {
            fragments
                .iter()
                .find(|f| f.id == s.fragment_id)
                .and_then(|f| f.raw_text.get(s.start..s.end))
                .expect("a selected span must be an exact slice of a source fragment")
        })
        .collect()
}

/// The label and value parts that layout derives from one inline fragment.
fn inline_parts(f: &SourceFragment) -> (Span, Span) {
    let pairs = layout::analyze(std::slice::from_ref(f)).pairs;
    assert_eq!(pairs.len(), 1, "{}", f.raw_text);
    assert_eq!(pairs[0].placement, Placement::Inline);
    (pairs[0].label.clone(), pairs[0].value[0].clone())
}

fn with_left(r: Rect, left: f64) -> Rect {
    rect(left, r.top, r.right, r.bottom)
}

// Coordinates observed from an embedded-TrueType fixture (see layout tests).
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

#[test]
fn positive_overlap_keeps_the_full_unicode_run_but_touching_and_rounding_do_not() {
    let raw = [fragment(
        "unicode",
        1,
        "東京🦀e\u{301}\n原文全体",
        rect(0.0, 0.0, 10.0, 10.0),
    )];
    // A sliver across the full width overlaps every part, split or not.
    let sliver = rect(0.0, 10.0 - 2.0 * MEASURED_EDGE_SLACK, 10.0, 11.0);
    let selected = overlapping(&raw, 1, sliver).expect("a valid box must permit overlap lookup");
    assert_eq!(selected, [whole(&raw[0])]);
    assert_eq!(texts(&raw, &selected), [raw[0].raw_text.as_str()]);
    assert_eq!(
        in_box(&raw, 1, sliver),
        Err(SelectError::SplitsSourceRun {
            fragment_id: "unicode".into()
        })
    );
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
                .filter(|f| expected.iter().any(|s| s.fragment_id == f.id))
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
    assert_eq!(
        in_box(&raw, 1, rect(5.0, 0.0, 15.0, 10.0)),
        Err(SelectError::SplitsSourceRun {
            fragment_id: "a".into()
        })
    );
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
fn value_box_over_an_inline_pair_selects_only_the_estimated_value() {
    let raw = invoice_fragments();
    for (f, value_text) in raw.iter().zip(["INV-0012", "INV-0012", "AB-778"]) {
        let (_, value) = inline_parts(f);
        assert_eq!(value.precision, Precision::Estimated);
        // A box reaching into the delimiter still selects only the value.
        let bbox = rect(
            value.bbox.left - 0.5,
            f.bbox.top - 1.0,
            f.bbox.right + 1.0,
            f.bbox.bottom + 1.0,
        );
        for selected in [in_box(&raw, 1, bbox), overlapping(&raw, 1, bbox)] {
            let selected = selected.expect("a box at a known split must be accepted");
            assert_eq!(selected, std::slice::from_ref(&value));
            assert_eq!(texts(&raw, &selected), [value_text]);
        }
    }
    // The observed value start of fragment a is 115.0; the estimate is close enough.
    let observed = rect(115.0, 119.0, 161.4, 130.0);
    let selected = in_box(&raw, 1, observed).expect("the observed value box must be accepted");
    assert_eq!(texts(&raw, &selected), ["INV-0012"]);
    assert_eq!(selected[0].precision, Precision::Estimated);
}

#[test]
fn label_box_selects_the_label_and_a_full_box_the_untrimmed_run() {
    let raw = invoice_fragments();
    let (label, _) = inline_parts(&raw[0]);
    let selected = in_box(&raw, 1, rect(60.0, 119.0, 110.0, 130.0))
        .expect("a box ending in the delimiter gap must select the label");
    assert_eq!(selected, [label]);
    assert_eq!(texts(&raw, &selected), ["請求番号"]);
    // A box over the whole fragment selects the whole run, delimiter included.
    assert_eq!(
        in_box(&raw, 1, rect(59.0, 118.0, 162.0, 131.0)),
        Ok(vec![whole(&raw[0])])
    );
    // Containing every part, though not the leading whitespace, is the whole run too.
    let padded = [fragment(
        "p",
        1,
        "  備考：午前",
        rect(50.0, 200.0, 120.0, 211.0),
    )];
    let (label, value) = inline_parts(&padded[0]);
    assert!(label.bbox.left > padded[0].bbox.left + 10.0);
    let bbox = rect(label.bbox.left - 1.0, 199.0, value.bbox.right + 1.0, 212.0);
    assert_eq!(in_box(&padded, 1, bbox), Ok(vec![whole(&padded[0])]));
    assert_eq!(overlapping(&padded, 1, bbox), Ok(vec![whole(&padded[0])]));
}

#[test]
fn boxes_cutting_outside_a_known_split_are_rejected() {
    let raw = invoice_fragments();
    let f = &raw[0];
    let (label, value) = inline_parts(f);
    let cut = Err(SelectError::SplitsSourceRun {
        fragment_id: "a".into(),
    });
    for bbox in [
        rect(115.0, 119.0, 140.0, 130.0), // Middle of the value.
        rect(70.0, 119.0, 161.4, 130.0),  // Middle of the label.
        rect(115.0, 119.0, 161.4, 125.0), // Upper part of the line only.
        // Only the delimiter gap between the parts.
        rect(label.bbox.right + 0.5, 119.0, value.bbox.left - 0.5, 130.0),
    ] {
        assert_eq!(in_box(&raw, 1, bbox), cut, "{bbox:?}");
    }
    // Estimated interior edges absorb the layout's documented 0.2 em error...
    let em = f.bbox.height();
    let inside = with_left(value.bbox, value.bbox.left + 0.2 * em);
    assert_eq!(in_box(&raw, 1, inside), Ok(vec![value.clone()]));
    // ...but leaving out a whole estimated half-width character is a cut.
    let missing_first = with_left(value.bbox, value.bbox.left + 0.5 * em);
    assert_eq!(in_box(&raw, 1, missing_first), cut);
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
    assert_eq!(
        in_box(&run, 1, short),
        Err(SelectError::SplitsSourceRun {
            fragment_id: "r".into()
        })
    );
}

#[test]
fn overlapping_selects_touched_parts_or_whole_runs() {
    let raw = invoice_fragments();
    // A column over the values of three inline lines.
    let column = rect(118.0, 115.0, 170.0, 175.0);
    let values: Vec<_> = raw.iter().map(|f| inline_parts(f).1).collect();
    for selected in [overlapping(&raw, 1, column), in_box(&raw, 1, column)] {
        let selected = selected.expect("a value column must be accepted");
        assert_eq!(selected, values);
        assert_eq!(texts(&raw, &selected), ["INV-0012", "INV-0012", "AB-778"]);
    }
    // Touching labels and values selects the whole runs; in_box rejects the cut labels.
    let wide = rect(80.0, 115.0, 170.0, 175.0);
    assert_eq!(
        overlapping(&raw, 1, wide),
        Ok(raw.iter().map(whole).collect::<Vec<_>>())
    );
    assert!(matches!(
        in_box(&raw, 1, wide),
        Err(SelectError::SplitsSourceRun { .. })
    ));
    // A box on the delimiter gap alone touches no part: the whole run, inclusively.
    let (label, value) = inline_parts(&raw[0]);
    let gap = rect(label.bbox.right + 0.5, 119.0, value.bbox.left - 0.5, 130.0);
    assert_eq!(overlapping(&raw, 1, gap), Ok(vec![whole(&raw[0])]));
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
        assert!(selected.iter().all(|s| s.precision == Precision::Run));
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
    for f in &raw {
        let bbox = model_box(&transform, f.bbox);
        // Rounding moved the box inside the run: only the slack accepts it.
        assert!(!bbox.contains(f.bbox), "{bbox:?}");
        assert_eq!(in_box(&raw, 1, bbox), Ok(vec![whole(f)]));
    }
    let (_, value) = inline_parts(&raw[0]);
    let bbox = model_box(&transform, value.bbox);
    let selected = in_box(&raw, 1, bbox).expect("a rounded value box must select the value");
    assert_eq!(selected, [value]);
    assert_eq!(texts(&raw, &selected), ["INV-0012"]);
    let both = model_box(
        &transform,
        rect(
            raw[0].bbox.left,
            raw[0].bbox.top,
            raw[1].bbox.right,
            raw[1].bbox.bottom,
        ),
    );
    assert_eq!(
        overlapping(&raw, 1, both),
        Ok(raw.iter().map(whole).collect::<Vec<_>>())
    );
}
