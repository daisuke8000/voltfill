//! Synthetic, offline evaluation set for form fill.
//!
//! Every case in `tests/eval/cases/*.json` is a page layout (text runs with
//! positions), a form and the expected outcome per field. The runner builds
//! a PDF with [`pdf::build`] (no font file), inspects it with `vf_source`,
//! fills the form with `vf_core` and counts outcomes.
//!
//! It is the regression gate for "a wrong value is worse than an empty one".
//! It fails on any filled value that differs from the expectation and on any
//! fill of a field that must stay empty, unless that exact output is recorded
//! for the field as `known_wrong`. A `known_wrong` record that no longer
//! reproduces also fails, so the record is updated when the code changes.
//! Supported cases also fail when an expected value is missing or an
//! abstention has another status; unsupported cases may stay empty.
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
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use vf_core::fill::{self, FieldFill, FieldType, FillStatus, FormField};
use vf_source::geometry::Rect;
use vf_source::layout::Precision;
use vf_source::{DocumentSnapshot, PdfLimits, Quality, SourceFragment, inspect};

/// Box tolerance for every evidence span, measured or estimated: coordinates
/// pass through `f32` in the PDF.
const TOLERANCE: f64 = 0.05;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// What the case covers, for readers of the case file.
    #[allow(dead_code)]
    description: String,
    support: Support,
    runs: Vec<Run>,
    /// Texts extraction must return when the case needs runs merged into
    /// one fragment (for example a number split over runs). By default
    /// supported cases expect exactly one fragment per run.
    #[serde(default)]
    fragments: Option<Vec<String>>,
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
    /// Run `id`s, one per line of `raw`, naming the run each line must come
    /// from. Required when a line's text occurs in more than one run.
    #[serde(default)]
    source: Option<Vec<String>>,
    /// The field must stay empty.
    #[serde(default)]
    abstain: Option<Abstain>,
    /// With `abstain`: the `hold` reason of the first (best) candidate, or
    /// `"none"` when that candidate has none. Unset, the reason is not checked.
    #[serde(default)]
    hold: Option<String>,
    /// With `abstain`: every failed condition (`holds`) of the first (best)
    /// candidate, in order. Unset, only `hold` is checked.
    #[serde(default)]
    holds: Option<Vec<String>>,
    /// With `abstain`: the exact `raw` text of the first (best) candidate,
    /// for example the true value of a field that may not be committed
    /// offline. Unset, the candidates are not checked.
    #[serde(default)]
    candidate: Option<String>,
    /// Unsupported cases only: the current code fills this value correctly,
    /// so a regression to empty is reported (as a note, not a failure).
    #[serde(default)]
    currently_correct: bool,
    /// The exact wrong fill the current code produces for this field.
    #[serde(default)]
    known_wrong: Option<KnownWrong>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KnownWrong {
    /// Only wrong fills can be recorded; a missing value never can.
    #[allow(dead_code)]
    status: KnownStatus,
    raw: String,
    /// Normalized value; defaults to `raw` for text fields.
    #[serde(default)]
    value: Option<Value>,
    /// Why the current code produces this output.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KnownStatus {
    Filled,
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

/// A filled `raw` text with its normalized `value`.
#[derive(Debug, Clone, PartialEq)]
struct Output {
    raw: String,
    value: Value,
}

impl std::fmt::Display for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "raw={} value={}",
            Value::from(self.raw.as_str()),
            self.value
        )
    }
}

/// Where a line of text was placed: a run and a byte offset in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Source {
    run: usize,
    offset: usize,
}

enum Want {
    /// Filled with this output, each line from its source.
    Value(Output, Vec<Source>),
    Empty(Abstain),
}

/// A field's expectation, validated against the case.
struct Check<'a> {
    want: Want,
    known: Option<(Output, &'a str)>,
    currently_correct: bool,
    hold: Option<&'a str>,
    holds: Option<&'a [String]>,
    candidate: Option<&'a str>,
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
    /// The field was expected to stay empty but was filled.
    Unexpected,
    /// Filled with exactly the output recorded as `known_wrong`.
    Known,
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    fields: usize,
    /// Fields expected to be filled / to stay empty.
    values: usize,
    empties: usize,
    correct: usize,
    wrong: usize,
    empty: usize,
    abstained: usize,
    unexpected: usize,
    known: usize,
}

impl Counts {
    fn add(&mut self, want: &Want, outcome: Outcome) {
        self.fields += 1;
        match want {
            Want::Value(..) => self.values += 1,
            Want::Empty(_) => self.empties += 1,
        }
        match outcome {
            Outcome::Correct => self.correct += 1,
            Outcome::Wrong => self.wrong += 1,
            Outcome::Empty => self.empty += 1,
            Outcome::Abstained => self.abstained += 1,
            Outcome::Unexpected => self.unexpected += 1,
            Outcome::Known => self.known += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.fields += other.fields;
        self.values += other.values;
        self.empties += other.empties;
        self.correct += other.correct;
        self.wrong += other.wrong;
        self.empty += other.empty;
        self.abstained += other.abstained;
        self.unexpected += other.unexpected;
        self.known += other.known;
    }

    fn filled(self) -> usize {
        self.correct + self.wrong + self.unexpected + self.known
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
            let mut ids = BTreeSet::new();
            for run in &case.runs {
                assert!(
                    !run.text.is_empty() && run.text.trim() == run.text,
                    "{name}: run text {:?} must be non-empty and trimmed",
                    run.text
                );
                if let Some(id) = &run.id {
                    assert!(ids.insert(id), "{name}: duplicate run id {id:?}");
                }
            }
            (name, case)
        })
        .collect()
}

/// Validate a field's expectation. Panics on authoring mistakes so they never
/// count as outcomes.
fn resolve<'a>(at: &str, case: &'a Case, field: &FormField) -> Check<'a> {
    let expect = &case.expect[&field.id];
    let output = |raw: &str, value: &Option<Value>| {
        let value = match (value, field.field_type) {
            (Some(value), _) => value.clone(),
            (None, FieldType::Text) => Value::from(raw),
            (None, _) => panic!("{at}: typed fields need a normalized value"),
        };
        Output {
            raw: raw.to_owned(),
            value,
        }
    };
    let want = match (&expect.raw, expect.abstain) {
        (Some(raw), None) => Want::Value(
            output(raw, &expect.value),
            sources(at, &case.runs, raw, expect.source.as_deref()),
        ),
        (None, Some(abstain)) if expect.value.is_none() && expect.source.is_none() => {
            Want::Empty(abstain)
        }
        _ => panic!("{at}: set either raw (with value and source) or abstain"),
    };
    assert!(
        expect.hold.is_none() && expect.holds.is_none() && expect.candidate.is_none()
            || expect.abstain.is_some(),
        "{at}: hold, holds and candidate are checked for abstentions only"
    );
    if let (Some(hold), Some(holds)) = (&expect.hold, &expect.holds) {
        assert_eq!(
            holds.first(),
            Some(hold),
            "{at}: hold must be the first of holds"
        );
    }
    let known = expect
        .known_wrong
        .as_ref()
        .map(|k| (output(&k.raw, &k.value), k.reason.as_str()));
    if let (Some((known, _)), Want::Value(right, _)) = (&known, &want) {
        assert!(
            known != right,
            "{at}: known_wrong equals the expected value"
        );
    }
    if expect.currently_correct {
        assert!(
            case.support == Support::Unsupported
                && matches!(want, Want::Value(..))
                && known.is_none(),
            "{at}: currently_correct marks expected values of unsupported cases only"
        );
    }
    Check {
        want,
        known,
        currently_correct: expect.currently_correct,
        hold: expect.hold.as_deref(),
        holds: expect.holds.as_deref(),
        candidate: expect.candidate.as_deref(),
    }
}

/// Where each line of an expected value was placed: the only occurrence of
/// the line in the run named by `source`, or in all runs of the case.
fn sources(at: &str, runs: &[Run], raw: &str, named: Option<&[String]>) -> Vec<Source> {
    let lines: Vec<&str> = raw.split('\n').collect();
    if let Some(named) = named {
        assert_eq!(
            named.len(),
            lines.len(),
            "{at}: source needs one run id per line"
        );
    }
    lines
        .iter()
        .enumerate()
        .map(|(n, line)| {
            assert!(!line.is_empty(), "{at}: empty value line");
            let found: Vec<Source> = runs
                .iter()
                .enumerate()
                .filter(|(_, run)| named.is_none_or(|ids| run.id.as_ref() == Some(&ids[n])))
                .flat_map(|(run, r)| {
                    r.text
                        .match_indices(line)
                        .map(move |(offset, _)| Source { run, offset })
                })
                .collect();
            match found[..] {
                [source] => source,
                [] => panic!("{at}: {line:?} is not in the runs (or the named run)"),
                _ => panic!(
                    "{at}: {line:?} occurs {} times; name its run with source",
                    found.len()
                ),
            }
        })
        .collect()
}

/// A byte range of a fragment's text that came from one run.
#[derive(Debug, Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
    run: usize,
}

/// Split a fragment into the runs it was extracted from, left to right.
/// Extraction joins close runs with one space and overlapping runs with none.
fn segments(fragment: &SourceFragment, runs: &[Run]) -> Option<Vec<Segment>> {
    let mut members: Vec<usize> = (0..runs.len())
        .filter(|&i| {
            let run = &runs[i];
            run.page == fragment.page
                && (run.y - fragment.bbox.bottom).abs() <= TOLERANCE
                && run.left() >= fragment.bbox.left - TOLERANCE
                && run.right() <= fragment.bbox.right + TOLERANCE
        })
        .collect();
    members.sort_by(|&a, &b| runs[a].left().total_cmp(&runs[b].left()));
    let text = fragment.raw_text.as_str();
    let mut at = 0;
    let mut segments = Vec::new();
    for run in members {
        let run_text = runs[run].text.as_str();
        if !segments.is_empty() && !text[at..].starts_with(run_text) && text[at..].starts_with(' ')
        {
            at += 1;
        }
        if !text[at..].starts_with(run_text) {
            return None;
        }
        segments.push(Segment {
            start: at,
            end: at + run_text.len(),
            run,
        });
        at += run_text.len();
    }
    (!segments.is_empty() && at == text.len()).then_some(segments)
}

/// Where the builder placed bytes `start..end` of a fragment, and the run and
/// offset the span starts at. `None` when a boundary falls on a joining space.
fn placed(segments: &[Segment], runs: &[Run], start: usize, end: usize) -> Option<(Rect, Source)> {
    let first = segments
        .iter()
        .find(|s| s.start <= start && start < s.end)?;
    let last = segments.iter().find(|s| s.start < end && end <= s.end)?;
    let x = |segment: &Segment, at: usize| {
        let run = &runs[segment.run];
        run.left() + pdf::text_width(&run.text[..at - segment.start], run.size)
    };
    let bbox = Rect {
        left: x(first, start),
        top: segments
            .iter()
            .map(|s| runs[s.run].y - runs[s.run].size)
            .fold(f64::INFINITY, f64::min),
        right: x(last, end),
        bottom: segments
            .iter()
            .map(|s| runs[s.run].y)
            .fold(f64::NEG_INFINITY, f64::max),
    };
    let source = Source {
        run: first.run,
        offset: start - first.start,
    };
    Some((bbox, source))
}

fn close(a: Rect, b: Rect) -> bool {
    [
        (a.left, b.left),
        (a.top, b.top),
        (a.right, b.right),
        (a.bottom, b.bottom),
    ]
    .iter()
    .all(|(x, y)| (x - y).abs() <= TOLERANCE)
}

/// Evidence must be exact slices of existing fragments, marked `run` exactly
/// when it covers a whole (trimmed) fragment, and sit where the builder
/// placed the text. For an expected value it must also come from the
/// expected runs.
fn check_evidence(
    at: &str,
    got: &FieldFill,
    snapshot: &DocumentSnapshot,
    runs: &[Run],
    expected: Option<&[Source]>,
    problems: &mut Vec<String>,
) {
    let joined: Vec<&str> = got.evidence.iter().map(|e| e.text.as_str()).collect();
    if got.evidence.is_empty() || got.raw.as_deref() != Some(joined.join("\n").as_str()) {
        problems.push(format!("{at}: raw does not equal its evidence text"));
    }
    if let Some(expected) = expected
        && expected.len() != got.evidence.len()
    {
        problems.push(format!(
            "{at}: {} evidence spans, expected {}",
            got.evidence.len(),
            expected.len()
        ));
    }
    for (n, evidence) in got.evidence.iter().enumerate() {
        let Some(fragment) = snapshot
            .fragments
            .iter()
            .find(|f| f.id == evidence.fragment_id)
        else {
            problems.push(format!("{at}: evidence names an unknown fragment"));
            continue;
        };
        let text = fragment.raw_text.as_str();
        if fragment.page != evidence.page
            || text.get(evidence.start..evidence.end) != Some(evidence.text.as_str())
            || !fragment.bbox.contains(evidence.bbox)
        {
            problems.push(format!(
                "{at}: evidence {:?} is not an exact slice inside its fragment",
                evidence.text
            ));
            continue;
        }
        let whole = (evidence.start, evidence.end)
            == (text.len() - text.trim_start().len(), text.trim_end().len());
        let precision = if whole {
            Precision::Run
        } else {
            Precision::Estimated
        };
        if evidence.precision != precision || whole && evidence.bbox != fragment.bbox {
            problems.push(format!(
                "{at}: evidence {:?} has precision {:?}, expected {precision:?} \
                 (run exactly for a whole fragment, with the fragment's box)",
                evidence.text, evidence.precision
            ));
        }
        let Some((bbox, source)) =
            segments(fragment, runs).and_then(|s| placed(&s, runs, evidence.start, evidence.end))
        else {
            problems.push(format!(
                "{at}: evidence {:?} cannot be traced to the placed runs",
                evidence.text
            ));
            continue;
        };
        if !close(bbox, evidence.bbox) {
            problems.push(format!(
                "{at}: evidence box {:?} for {:?}, placed at {bbox:?}",
                evidence.bbox, evidence.text
            ));
        }
        if let Some(expected) = expected
            && expected.get(n).is_some_and(|e| *e != source)
        {
            let e = expected[n];
            problems.push(format!(
                "{at}: evidence {:?} comes from run {} byte {}, expected run {} byte {}",
                evidence.text, source.run, source.offset, e.run, e.offset
            ));
        }
    }
}

/// The `hold` reason of the field's first candidate (`"none"` without one;
/// `None` without candidates).
fn first_hold(got: &FieldFill) -> Option<String> {
    got.candidates.first().map(|c| {
        serde_json::to_value(c.hold)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "none".into())
    })
}

/// Every failed condition (`holds`) of the field's first candidate.
fn first_holds(got: &FieldFill) -> Option<Vec<String>> {
    got.candidates.first().map(|c| {
        c.holds
            .iter()
            .filter_map(|h| serde_json::to_value(h).ok())
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    })
}

/// Unfilled fields carry no value; only `invalid` may keep the raw text, and
/// then with its evidence.
fn check_shape(at: &str, got: &FieldFill, problems: &mut Vec<String>) {
    let ok = match got.status {
        FillStatus::Filled => got.raw.is_some() && got.value.is_some() && !got.evidence.is_empty(),
        FillStatus::Invalid => got.value.is_none() && got.raw.is_some() == !got.evidence.is_empty(),
        _ => got.value.is_none() && got.raw.is_none() && got.evidence.is_empty(),
    };
    if !ok {
        problems.push(format!(
            "{at}: malformed output {} with {} evidence spans",
            record(got),
            got.evidence.len()
        ));
    }
}

fn status_name(status: FillStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The output as a `known_wrong`-style JSON record, for pasting into a case.
fn record(got: &FieldFill) -> String {
    let mut out = format!("{{\"status\": {}", Value::from(status_name(got.status)));
    if let Some(raw) = &got.raw {
        let _ = write!(out, ", \"raw\": {}", Value::from(raw.as_str()));
    }
    if let Some(value) = &got.value {
        let _ = write!(out, ", \"value\": {value}");
    }
    out.push('}');
    out
}

fn wanted(want: &Want) -> String {
    match want {
        Want::Value(output, _) => output.to_string(),
        Want::Empty(abstain) => format!("empty ({abstain:?})"),
    }
}

/// Build `runs` into a PDF and check that inspection returns each run as one
/// usable fragment with its exact text and box.
fn assert_round_trip(runs: &[Run]) {
    let bytes = pdf::build(runs);
    assert_eq!(bytes, pdf::build(runs), "builder must be deterministic");
    let snapshot = inspect(&bytes, PdfLimits::default()).expect("synthetic PDF must inspect");
    let pages = runs.iter().map(|r| r.page).max().unwrap_or(1);
    assert_eq!(snapshot.pages.len(), pages as usize);
    assert!(snapshot.pages.iter().all(|p| p.text_available));
    assert_eq!(snapshot.fragments.len(), runs.len(), "one fragment per run");
    for run in runs {
        let fragment = snapshot
            .fragments
            .iter()
            .find(|f| f.raw_text == run.text)
            .unwrap_or_else(|| panic!("{:?} must be extracted exactly", run.text));
        assert_eq!(fragment.page, run.page);
        assert_eq!(fragment.quality, Quality::Usable, "{:?}", run.text);
        // Display frame: top-left origin; the box spans one em above the baseline.
        let expected = Rect {
            left: run.left(),
            top: run.y - run.size,
            right: run.right(),
            bottom: run.y,
        };
        assert!(
            close(fragment.bbox, expected),
            "{:?}: box {:?}, expected {expected:?}",
            run.text,
            fragment.bbox
        );
    }
}

fn run(page: u32, x: f64, y: f64, size: f64, text: &str) -> Run {
    Run {
        id: None,
        page,
        x: Some(x),
        right: None,
        y,
        size,
        text: text.into(),
    }
}

#[test]
fn builder_text_round_trips_through_inspect() {
    assert_round_trip(&[
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
    ]);
}

#[test]
fn builder_maps_non_bmp_characters_through_surrogate_pairs() {
    // U+20BB7 and U+29E3D lie outside the BMP; ToUnicode maps them to
    // UTF-16 surrogate pairs and both count as full width.
    let text = "氏名：\u{20BB7}田 \u{29E3D}子";
    assert_eq!(pdf::text_width(text, 10.0), 75.0);
    let bytes = pdf::build(&[run(1, 60.0, 100.0, 10.0, text)]);
    let needle = b"<D842DFB7>";
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "ToUnicode must hold the surrogate pair for U+20BB7"
    );
    assert_round_trip(&[
        run(1, 60.0, 100.0, 10.0, text),
        run(1, 60.0, 124.0, 10.0, "\u{20BB7}"),
    ]);
}

#[test]
fn evaluation_set_has_no_unexpected_wrong_fills() {
    let cases = load_cases();
    let mut problems = Vec::new();
    let mut known = Vec::new();
    let mut notes = Vec::new();
    let mut rows = Vec::new();
    let mut subtotals = [Counts::default(); 2];
    for (name, case) in &cases {
        let bytes = pdf::build(&case.runs);
        let snapshot = inspect(&bytes, PdfLimits::default())
            .unwrap_or_else(|e| panic!("{name}: synthetic PDF must inspect: {e}"));
        if !snapshot.pages.iter().all(|p| p.text_available) {
            problems.push(format!("{name}: a page has no usable text layer"));
        }
        // Supported cases assume one fragment per run unless they list the
        // merged fragments; any other merge means the case geometry does
        // not test what it says.
        if case.support == Support::Supported || case.fragments.is_some() {
            let mut expected: Vec<&str> = match &case.fragments {
                Some(texts) => texts.iter().map(String::as_str).collect(),
                None => case.runs.iter().map(|r| r.text.as_str()).collect(),
            };
            let mut fragments: Vec<&str> = snapshot
                .fragments
                .iter()
                .map(|f| f.raw_text.as_str())
                .collect();
            expected.sort_unstable();
            fragments.sort_unstable();
            if expected != fragments {
                problems.push(format!(
                    "{name}: extraction returned {fragments:?}, expected {expected:?}; adjust the gaps"
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

        let supported = case.support == Support::Supported;
        let mut counts = Counts::default();
        let mut all_pass = true;
        for field in &form.fields {
            let at = format!("{name}.{}", field.id);
            let check = resolve(&at, case, field);
            let got = &result.fields[&field.id];
            check_shape(&at, got, &mut problems);
            let status = status_name(got.status);
            let output = (got.status == FillStatus::Filled).then(|| Output {
                raw: got.raw.clone().unwrap_or_default(),
                value: got.value.clone().unwrap_or(Value::Null),
            });
            let outcome = match (&check.want, &output) {
                (_, Some(out)) if check.known.as_ref().is_some_and(|(k, _)| k == out) => {
                    Outcome::Known
                }
                (Want::Value(right, _), Some(out)) if out == right => Outcome::Correct,
                (Want::Value(..), Some(_)) => Outcome::Wrong,
                (Want::Value(..), None) => Outcome::Empty,
                (Want::Empty(_), Some(_)) => Outcome::Unexpected,
                (Want::Empty(_), None) => Outcome::Abstained,
            };
            counts.add(&check.want, outcome);
            if got.status == FillStatus::Filled
                || got.status == FillStatus::Invalid && got.raw.is_some()
            {
                let expected = match (&check.want, outcome) {
                    (Want::Value(_, sources), Outcome::Correct) => Some(sources.as_slice()),
                    _ => None,
                };
                check_evidence(&at, got, &snapshot, &case.runs, expected, &mut problems);
            }

            let now = record(got);
            let want = wanted(&check.want);
            if let Some(hold) = check.hold
                && got.status != FillStatus::Filled
                && first_hold(got).as_deref() != Some(hold)
            {
                all_pass = false;
                problems.push(format!(
                    "{at}: first candidate hold {:?}, expected {hold:?}",
                    first_hold(got)
                ));
            }
            if let Some(holds) = check.holds
                && got.status != FillStatus::Filled
                && first_holds(got).as_deref() != Some(holds)
            {
                all_pass = false;
                problems.push(format!(
                    "{at}: first candidate holds {:?}, expected {holds:?}",
                    first_holds(got)
                ));
            }
            let first = got.candidates.first().map(|c| c.raw.as_str());
            if let Some(candidate) = check.candidate
                && got.status != FillStatus::Filled
                && first != Some(candidate)
            {
                all_pass = false;
                problems.push(format!(
                    "{at}: first candidate {first:?}, expected {candidate:?}"
                ));
            }
            if let Some((recorded, _)) = check.known.as_ref().filter(|_| outcome != Outcome::Known)
            {
                all_pass = false;
                problems.push(match outcome {
                    Outcome::Wrong | Outcome::Unexpected => format!(
                        "{at}: wrong fill {now}; expected {want}; recorded known wrong was {recorded}"
                    ),
                    _ => format!(
                        "{at}: known wrong output no longer reproduces — update the case (now {now}; expected {want})"
                    ),
                });
                continue;
            }
            match outcome {
                Outcome::Correct => {
                    if !supported && !check.currently_correct {
                        notes.push(format!(
                            "{at}: now correct; set currently_correct to report regressions"
                        ));
                    }
                }
                Outcome::Known => {
                    all_pass = false;
                    let reason = check.known.as_ref().map_or("", |(_, reason)| reason);
                    known.push(format!("{at}: {now}; expected {want} — {reason}"));
                }
                Outcome::Wrong => {
                    all_pass = false;
                    problems.push(format!("{at}: wrong fill {now}; expected {want}"));
                }
                Outcome::Unexpected => {
                    all_pass = false;
                    problems.push(format!("{at}: unexpected fill {now}; expected {want}"));
                }
                Outcome::Empty => {
                    all_pass = false;
                    if supported {
                        problems.push(format!("{at}: regression: expected {want}, got {now}"));
                    } else if check.currently_correct {
                        notes.push(format!(
                            "{at}: regressed to empty ({status}); was correct {want}"
                        ));
                    } else {
                        notes.push(format!("{at}: empty ({status})"));
                    }
                }
                Outcome::Abstained => {
                    if let Want::Empty(abstain) = check.want
                        && supported
                        && !abstain.accepts(got.status)
                    {
                        all_pass = false;
                        problems.push(format!(
                            "{at}: regression: abstained as {status}, expected {abstain:?}"
                        ));
                    }
                }
            }
        }
        if !supported && all_pass {
            notes.push(format!(
                "{name}: every field passes; consider marking the case supported"
            ));
        }
        subtotals[usize::from(!supported)].merge(counts);
        rows.push((name.as_str(), case.support, counts));
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nform fill evaluation: {} synthetic cases, evaluation-only (not live accuracy)",
        cases.len()
    );
    let header = [
        "fields",
        "correct",
        "wrong",
        "unexpected",
        "known",
        "empty",
        "abstain",
    ];
    let _ = write!(out, "{:<34} {:<11}", "case", "support");
    for column in header {
        let _ = write!(out, " {column:>10}");
    }
    out.push('\n');
    let row = |out: &mut String, name: &str, support: &str, c: Counts| {
        let _ = write!(out, "{name:<34} {support:<11}");
        for n in [
            c.fields,
            c.correct,
            c.wrong,
            c.unexpected,
            c.known,
            c.empty,
            c.abstained,
        ] {
            let _ = write!(out, " {n:>10}");
        }
        out.push('\n');
    };
    for (name, support, counts) in &rows {
        let support = match support {
            Support::Supported => "supported",
            Support::Unsupported => "unsupported",
        };
        row(&mut out, name, support, *counts);
    }
    let mut total = Counts::default();
    for (label, counts) in ["supported", "unsupported"].iter().zip(subtotals) {
        row(&mut out, &format!("subtotal {label}"), "", counts);
        total.merge(counts);
    }
    row(&mut out, "total", "", total);
    for (label, c) in
        ["supported", "unsupported", "total"]
            .iter()
            .zip([subtotals[0], subtotals[1], total])
    {
        let _ = writeln!(
            out,
            "{label}: filled correctly {}/{} expected values; kept empty {}/{} expected empty; \
             wrong fills {}/{} filled ({} recorded known wrong, {} unexpected)",
            c.correct,
            c.values,
            c.abstained,
            c.empties,
            c.wrong + c.unexpected + c.known,
            c.filled(),
            c.known,
            c.wrong + c.unexpected,
        );
    }
    for line in &notes {
        let _ = writeln!(out, "  NOTE {line}");
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
