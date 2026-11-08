//! Release-mode timing for select's overprint grouping (finding N7); ignored
//! by default. Run with
//! `cargo test --release -p vf-source --test select_perf -- --ignored --nocapture`.
use std::time::{Duration, Instant};

use vf_source::geometry::Rect;
use vf_source::select::{SelectError, in_box};
use vf_source::{Quality, SourceFragment};

/// The per-page item limit of `PdfLimits::default()`.
const COPIES: usize = 10_000;

/// Generous for release builds; the measured times are printed.
const BUDGET: Duration = Duration::from_millis(20);

/// `COPIES` runs of the same text; copy `i` at `(left, top)` from `place(i)`.
fn copies(place: impl Fn(usize) -> (f64, f64)) -> Vec<SourceFragment> {
    (0..COPIES)
        .map(|i| {
            let (left, top) = place(i);
            SourceFragment {
                id: format!("c{i}"),
                page: 1,
                item_index: i,
                raw_text: "INV-0012".into(),
                bbox: Rect::new(left, top, left + 46.4, top + 11.0)
                    .expect("fixture coordinates must form a positive finite rectangle"),
                quality: Quality::Usable,
            }
        })
        .collect()
}

fn around(fragments: &[SourceFragment]) -> Rect {
    let (mut left, mut top) = (f64::INFINITY, f64::INFINITY);
    let (mut right, mut bottom) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for f in fragments {
        left = left.min(f.bbox.left);
        top = top.min(f.bbox.top);
        right = right.max(f.bbox.right);
        bottom = bottom.max(f.bbox.bottom);
    }
    Rect::new(left - 0.5, top - 0.5, right + 0.5, bottom + 0.5).expect("valid box")
}

/// The fastest of a few runs, after asserting what one run returns.
fn time(
    fragments: &[SourceFragment],
    bbox: Rect,
    expected: Result<usize, SelectError>,
) -> Duration {
    assert_eq!(in_box(fragments, 1, bbox).map(|v| v.len()), expected);
    (0..5)
        .map(|_| {
            let start = Instant::now();
            let _ = std::hint::black_box(in_box(fragments, 1, bbox));
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// Name, fragments, box (default: around all of them), expected count.
type Case = (
    &'static str,
    Vec<SourceFragment>,
    Option<Rect>,
    Result<usize, SelectError>,
);

#[test]
#[ignore = "release-mode timing"]
fn overprint_grouping_stays_linear_on_10k_copies() {
    let cut = |id: &str| SelectError::SplitsSourceRun {
        fragment_id: id.into(),
    };
    let cases: Vec<Case> = vec![
        // Each copy within 1.1 of its neighbours only: one long chain (the
        // old root walk was quadratic here).
        (
            "chained, 1.0 apart",
            copies(|i| (60.0, 119.0 + i as f64)),
            None,
            Ok(1),
        ),
        ("identical", copies(|_| (60.0, 119.0)), None, Ok(1)),
        (
            "identical, every copy cut",
            copies(|_| (60.0, 119.0)),
            Some(Rect::new(59.5, 118.5, 90.0, 130.5).expect("valid box")),
            Err(cut("c0")),
        ),
        // Distinct boxes, all within one top band and linked.
        (
            "dense, 1e-4 apart",
            copies(|i| (60.0, 119.0 + 1e-4 * i as f64)),
            None,
            Ok(1),
        ),
        // A row of same-text cells 50 apart on one top: not copies.
        (
            "row of cells",
            copies(|i| (50.0 * i as f64, 119.0)),
            None,
            Ok(COPIES),
        ),
        // Two interleaved columns 10 apart whose tops differ by 1e-4: each
        // fragment shares its band with thousands of same-text boxes that
        // are not its copies, so the call fails fast instead of taking
        // quadratic time.
        (
            "interleaved, adversarial",
            copies(|i| (60.0 + 10.0 * (i % 2) as f64, 119.0 + 1e-4 * i as f64)),
            None,
            Err(SelectError::LimitExceeded),
        ),
    ];
    for (name, fragments, bbox, expected) in cases {
        let bbox = bbox.unwrap_or_else(|| around(&fragments));
        let elapsed = time(&fragments, bbox, expected);
        println!("{name}: {elapsed:?}");
        assert!(elapsed < BUDGET, "{name}: {elapsed:?}");
    }
}
