//! Synthetic, offline evaluation set for form fill.
//!
//! Every case in `tests/eval/cases/*.json` is a page layout (text runs with
//! positions), a form and the expected outcome per field. The runner builds
//! a PDF with [`pdf::build`] (no font file), inspects it with `vf_source`,
//! fills the form with `vf_core` and counts outcomes. It fails on any wrong
//! value, because a wrong value is worse than an empty one.
//!
//! The PDFs are synthetic and the counts are evaluation-only: they are not
//! live accuracy on real documents. Estimated sub-run boxes match the ground
//! truth here only because the synthetic widths follow the estimator's
//! half/full-width classes; real fonts differ.
//!
//! Run with `cargo test -p vf-core --test eval -- --nocapture` to see the table.
#[path = "eval/pdf.rs"]
mod pdf;

use pdf::Run;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use vf_core::fill::{self, FieldFill, FieldType, FillStatus};
use vf_source::layout::Precision;
use vf_source::{DocumentSnapshot, PdfLimits, Quality, inspect};

/// Box tolerance for whole runs: coordinates pass through `f32` in the PDF.
const RUN_TOLERANCE: f64 = 0.05;
/// Box tolerance for estimated sub-run spans, in em.
const ESTIMATE_TOLERANCE_EM: f64 = 0.25;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// What the case covers, for readers of the case file.
    #[allow(dead_code)]
    description: String,
    support: Support,
    /// Why wrong fills in this case are expected with the current code. Its
    /// wrong fills are still counted and printed, but do not fail the run.
    #[serde(default)]
    known_failure: Option<String>,
    runs: Vec<Run>,
    /// Passed verbatim to `fill::parse_form`.
    form: Value,
    expect: BTreeMap<String, Expect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Support {
    /// Values must be filled exactly and abstentions must keep their status.
    Supported,
    /// Not supported yet: a field may be correct or empty, never wrong.
    Unsupported,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// Exact source text the field must hold (lines joined by `\n`).
    #[serde(default)]
    raw: Option<String>,
    /// Normalized value; defaults to `raw` for text fields.
    #[serde(default)]
    value: Option<Value>,
    /// The field must stay empty.
    #[serde(default)]
    abstain: Option<Abstain>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Abstain {
    /// Any status other than `filled`.
    Any,
    NotFound,
    Ambiguous,
    Invalid,
}

impl Abstain {
    fn accepts(self, status: FillStatus) -> bool {
        match self {
            Self::Any => status != FillStatus::Filled,
            Self::NotFound => status == FillStatus::NotFound,
            Self::Ambiguous => status == FillStatus::Ambiguous,
            Self::Invalid => status == FillStatus::Invalid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Filled with the expected raw text and normalized value.
    Correct,
    /// Filled, but the raw text or normalized value differs.
    Wrong,
    /// A value was expected but the field stayed empty.
    Empty,
    /// The field was expected to stay empty and did.
    Abstained,
    /// The field was expected to stay empty but was filled (a wrong fill).
    Unexpected,
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    fields: usize,
    correct: usize,
    wrong: usize,
    empty: usize,
    abstained: usize,
    unexpected: usize,
}

impl Counts {
    fn add(&mut self, outcome: Outcome) {
        self.fields += 1;
        match outcome {
            Outcome::Correct => self.correct += 1,
            Outcome::Wrong => self.wrong += 1,
            Outcome::Empty => self.empty += 1,
            Outcome::Abstained => self.abstained += 1,
            Outcome::Unexpected => self.unexpected += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.fields += other.fields;
        self.correct += other.correct;
        self.wrong += other.wrong;
        self.empty += other.empty;
        self.abstained += other.abstained;
        self.unexpected += other.unexpected;
    }
}

fn load_cases() -> Vec<(String, Case)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/cases");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("case directory must exist")
        .map(|entry| entry.expect("case entry must be readable").path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no evaluation cases found");
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("case file names are UTF-8")
                .to_owned();
            let bytes = std::fs::read(&path).expect("case file must be readable");
            let case: Case = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{name}: invalid case definition: {e}"));
            (name, case)
        })
        .collect()
}

/// Expected `(raw, value)` for a field, or `None` when it must stay empty.
/// Panics on a malformed expectation so authoring mistakes never count as
/// outcomes.
fn expected(name: &str, field: &fill::FormField, expect: &Expect) -> Option<(String, Value)> {
    let id = &field.id;
    match (&expect.raw, &expect.abstain) {
        (Some(raw), None) => {
            let value = match (&expect.value, field.field_type) {
                (Some(value), _) => value.clone(),
                (None, FieldType::Text) => Value::String(raw.clone()),
                (None, _) => panic!("{name}.{id}: typed fields need a normalized value"),
            };
            Some((raw.clone(), value))
        }
        (None, Some(_)) if expect.value.is_none() => None,
        _ => panic!("{name}.{id}: set exactly one of raw (with value) and abstain"),
    }
}

fn classify(fill: &FieldFill, expected: Option<&(String, Value)>) -> Outcome {
    let filled = fill.status == FillStatus::Filled;
    match (expected, filled) {
        (Some((raw, value)), true) => {
            if fill.raw.as_ref() == Some(raw) && fill.value.as_ref() == Some(value) {
                Outcome::Correct
            } else {
                Outcome::Wrong
            }
        }
        (Some(_), false) => Outcome::Empty,
        (None, true) => Outcome::Unexpected,
        (None, false) => Outcome::Abstained,
    }
}

fn status_name(status: FillStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn shown(fill: &FieldFill) -> String {
    match (&fill.raw, &fill.value) {
        (Some(raw), Some(value)) => format!("raw={raw:?} value={value}"),
        (Some(raw), None) => format!("raw={raw:?}"),
        _ => "no value".into(),
    }
}

/// Evidence must be exact slices of existing fragments and sit where the
/// builder placed the text.
fn check_evidence(
    at: &str,
    fill: &FieldFill,
    snapshot: &DocumentSnapshot,
    runs: &[Run],
    problems: &mut Vec<String>,
) {
    if fill.evidence.is_empty() {
        problems.push(format!("{at}: filled without evidence"));
        return;
    }
    let joined: Vec<&str> = fill.evidence.iter().map(|e| e.text.as_str()).collect();
    if fill.raw.as_deref() != Some(joined.join("\n").as_str()) {
        problems.push(format!("{at}: raw does not equal its evidence text"));
    }
    for evidence in &fill.evidence {
        let Some(fragment) = snapshot
            .fragments
            .iter()
            .find(|f| f.id == evidence.fragment_id)
        else {
            problems.push(format!("{at}: evidence names an unknown fragment"));
            continue;
        };
        if fragment.page != evidence.page
            || fragment.raw_text.get(evidence.start..evidence.end) != Some(evidence.text.as_str())
            || !fragment.bbox.contains(evidence.bbox)
        {
            problems.push(format!(
                "{at}: evidence {:?} is not an exact slice inside its fragment",
                evidence.text
            ));
            continue;
        }
        let tolerance = |run: &Run| match evidence.precision {
            Precision::Run => RUN_TOLERANCE,
            Precision::Estimated => ESTIMATE_TOLERANCE_EM * run.size,
        };
        let placed = runs.iter().filter(|r| r.page == evidence.page).any(|run| {
            run.text.match_indices(&evidence.text).any(|(at, _)| {
                let left = run.left() + pdf::text_width(&run.text[..at], run.size);
                let right = left + pdf::text_width(&evidence.text, run.size);
                let t = tolerance(run);
                (evidence.bbox.left - left).abs() <= t
                    && (evidence.bbox.right - right).abs() <= t
                    && (evidence.bbox.top - (run.y - run.size)).abs() <= RUN_TOLERANCE
                    && (evidence.bbox.bottom - run.y).abs() <= RUN_TOLERANCE
            })
        });
        if !placed {
            problems.push(format!(
                "{at}: evidence box {:?} for {:?} does not match where the text was placed",
                evidence.bbox, evidence.text
            ));
        }
    }
}

#[test]
fn builder_text_round_trips_through_inspect() {
    let run = |page, x, y, size, text: &str| Run {
        page,
        x: Some(x),
        right: None,
        y,
        size,
        text: text.into(),
    };
    let runs = [
        run(1, 60.0, 80.0, 18.0, "請求書"),
        run(1, 60.0, 110.0, 10.0, "請求番号：INV-0012"),
        run(1, 60.0, 130.0, 10.0, "〒100-0001 東京都千代田区架空町1-2-3"),
        run(1, 60.0, 150.0, 10.0, "ｶﾌﾞｼｷｶﾞｲｼｬ（ＡＢＣ）"),
        run(1, 300.0, 150.0, 12.0, "¥12,100"),
        run(2, 72.5, 200.0, 9.0, "令和8年11月8日 9:00〜18:00"),
        Run {
            right: Some(520.0),
            x: None,
            ..run(2, 0.0, 220.0, 10.0, "¥1,100")
        },
    ];
    let bytes = pdf::build(&runs);
    assert_eq!(bytes, pdf::build(&runs), "builder must be deterministic");
    let snapshot = inspect(&bytes, PdfLimits::default()).expect("synthetic PDF must inspect");
    assert_eq!(snapshot.pages.len(), 2);
    assert!(snapshot.pages.iter().all(|p| p.text_available));
    assert_eq!(snapshot.fragments.len(), runs.len(), "one fragment per run");
    for run in &runs {
        let fragment = snapshot
            .fragments
            .iter()
            .find(|f| f.raw_text == run.text)
            .unwrap_or_else(|| panic!("{:?} must be extracted exactly", run.text));
        assert_eq!(fragment.page, run.page);
        assert_eq!(fragment.quality, Quality::Usable, "{:?}", run.text);
        // Display frame: top-left origin; the box spans one em above the baseline.
        let expected = [
            run.left(),
            run.y - run.size,
            run.left() + pdf::text_width(&run.text, run.size),
            run.y,
        ];
        let actual = [
            fragment.bbox.left,
            fragment.bbox.top,
            fragment.bbox.right,
            fragment.bbox.bottom,
        ];
        for (a, e) in actual.iter().zip(expected) {
            assert!(
                (a - e).abs() <= RUN_TOLERANCE,
                "{:?}: box {actual:?}, expected {expected:?}",
                run.text
            );
        }
    }
}

#[test]
fn evaluation_set_has_no_wrong_fills() {
    let cases = load_cases();
    let mut problems = Vec::new();
    let mut known = Vec::new();
    let mut notes = Vec::new();
    let mut rows = Vec::new();
    let mut total = Counts::default();
    for (name, case) in &cases {
        let bytes = pdf::build(&case.runs);
        let snapshot = inspect(&bytes, PdfLimits::default())
            .unwrap_or_else(|e| panic!("{name}: synthetic PDF must inspect: {e}"));
        if !snapshot.pages.iter().all(|p| p.text_available) {
            problems.push(format!("{name}: a page has no usable text layer"));
        }
        // Supported cases assume one fragment per run; a merge means the
        // case geometry does not test what it says.
        if case.support == Support::Supported {
            let mut runs: Vec<&str> = case.runs.iter().map(|r| r.text.as_str()).collect();
            let mut fragments: Vec<&str> = snapshot
                .fragments
                .iter()
                .map(|f| f.raw_text.as_str())
                .collect();
            runs.sort_unstable();
            fragments.sort_unstable();
            if runs != fragments {
                problems.push(format!(
                    "{name}: runs were merged or split by extraction; widen the gaps"
                ));
            }
        }
        let form = fill::parse_form(case.form.to_string().as_bytes())
            .unwrap_or_else(|e| panic!("{name}: form must satisfy the form contract: {e}"));
        let mut ids: Vec<&str> = form.fields.iter().map(|f| f.id.as_str()).collect();
        ids.sort_unstable();
        assert!(
            ids.iter()
                .copied()
                .eq(case.expect.keys().map(String::as_str)),
            "{name}: expect must list exactly the form's field ids"
        );
        let result = fill::fill(&snapshot.document_id, &snapshot.fragments, &form);
        assert!(result.evaluation_only);

        let mut counts = Counts::default();
        for field in &form.fields {
            let at = format!("{name}.{}", field.id);
            let expect = &case.expect[&field.id];
            let want = expected(name, field, expect);
            let got = &result.fields[&field.id];
            let outcome = classify(got, want.as_ref());
            counts.add(outcome);
            if got.status == FillStatus::Filled {
                check_evidence(&at, got, &snapshot, &case.runs, &mut problems);
            }
            let status = status_name(got.status);
            match outcome {
                Outcome::Correct => {}
                Outcome::Wrong | Outcome::Unexpected => {
                    let line = match &want {
                        Some((raw, value)) => format!(
                            "{at}: wrong fill {}; expected raw={raw:?} value={value}",
                            shown(got)
                        ),
                        None => format!("{at}: unexpected fill {}; expected empty", shown(got)),
                    };
                    if case.known_failure.is_some() {
                        known.push(line);
                    } else {
                        problems.push(line);
                    }
                }
                Outcome::Empty if case.support == Support::Supported => {
                    problems.push(format!("{at}: regression: expected a value, got {status}"));
                }
                Outcome::Empty => notes.push(format!("{at}: empty ({status})")),
                Outcome::Abstained => {
                    let abstain = expect.abstain.unwrap_or(Abstain::Any);
                    if case.support == Support::Supported && !abstain.accepts(got.status) {
                        problems.push(format!(
                            "{at}: regression: abstained as {status}, expected {abstain:?}"
                        ));
                    }
                }
            }
        }
        if let Some(reason) = &case.known_failure {
            if counts.wrong + counts.unexpected == 0 {
                notes.push(format!(
                    "{name}: known failure no longer reproduces; remove the marker ({reason})"
                ));
            } else {
                known.push(format!("{name}: {reason}"));
            }
        }
        total.merge(counts);
        rows.push((name.as_str(), case, counts));
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nform fill evaluation: {} synthetic cases, evaluation-only (not live accuracy)",
        cases.len()
    );
    let _ = writeln!(
        out,
        "{:<30} {:<11} {:>6} {:>7} {:>5} {:>5} {:>7} {:>10}",
        "case", "support", "fields", "correct", "wrong", "empty", "abstain", "unexpected"
    );
    let row = |out: &mut String, name: &str, support: &str, c: Counts| {
        let _ = writeln!(
            out,
            "{:<30} {:<11} {:>6} {:>7} {:>5} {:>5} {:>7} {:>10}",
            name, support, c.fields, c.correct, c.wrong, c.empty, c.abstained, c.unexpected
        );
    };
    for (name, case, counts) in &rows {
        let support = match (case.support, case.known_failure.is_some()) {
            (Support::Supported, false) => "supported",
            (Support::Unsupported, false) => "unsupported",
            (Support::Supported, true) => "supp/KNOWN",
            (Support::Unsupported, true) => "unsup/KNOWN",
        };
        row(&mut out, name, support, *counts);
    }
    row(&mut out, "total", "", total);
    let expected_values = total.correct + total.wrong + total.empty;
    let filled = total.correct + total.wrong + total.unexpected;
    let _ = writeln!(
        out,
        "filled correctly {}/{} expected values; wrong fills {}/{} filled; correct abstentions {}/{} expected empty",
        total.correct,
        expected_values,
        total.wrong + total.unexpected,
        filled,
        total.abstained,
        total.abstained + total.unexpected,
    );
    for line in &notes {
        let _ = writeln!(out, "  {line}");
    }
    for line in &known {
        let _ = writeln!(out, "  KNOWN {line}");
    }
    for line in &problems {
        let _ = writeln!(out, "  FAIL {line}");
    }
    println!("{out}");
    assert!(
        problems.is_empty(),
        "{} evaluation problem(s); see the table above",
        problems.len()
    );
}
