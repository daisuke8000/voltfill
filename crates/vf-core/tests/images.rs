//! Image fallback (`jev_box`) end to end over real PDF text layers, with a
//! scripted transport and, unless the `hayro` feature renders real pages, a
//! synthetic page image. Nothing is sent over a network.
mod support;

use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use vf_client::ResponseBody;
use vf_client::transport::{HttpRequest, HttpResponse, TransportError};
use vf_core::acroform::{self, AcroFormHold};
use vf_core::assist::{
    AssistError, AssistOptions, AssistOutcome, AssistedFill, Clock, ExecutionConfig, MODEL, Secret,
    Transport,
};
use vf_core::fill::{self, FieldFill, FillLimits, FillStatus, Form};
use vf_core::images::{
    self, ImageError, ImageInputs, ImageOptions, ImageOutcome, PageImage, PageRenderer,
    RenderFailed,
};
use vf_source::acroform::{AcroForm, FieldKind, FieldValue, Widget, XfaPresence};
use vf_source::geometry::{AffineTransform, ImageTransform, Padding, Rect};
use vf_source::layout::Precision;
use vf_source::{DocumentSnapshot, Orientation, PdfLimits, Quality, SourceFragment, inspect};

const SECRET: &str = "synthetic_secret_canary";
const PNG_CANARY: &str = "PNG_PIXEL_CANARY_p";
const PAGE: (f64, f64) = (612.0, 792.0);

/// A structurally valid PNG (IHDR, IDAT, IEND) whose pixel data names the page.
fn png(width: u32, height: u32, page: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        bytes.extend((data.len() as u32).to_be_bytes());
        bytes.extend(kind);
        bytes.extend(data);
        bytes.extend([0; 4]);
    };
    let mut header = width.to_be_bytes().to_vec();
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", format!("{PNG_CANARY}{page}").as_bytes());
    chunk(b"IEND", &[]);
    bytes
}

/// Letter at 1.5 pixels per point, as vf-render produces it: no crop or padding.
fn transform() -> ImageTransform {
    ImageTransform {
        display_bounds: Rect {
            left: 0.0,
            top: 0.0,
            right: PAGE.0,
            bottom: PAGE.1,
        },
        raster_width: 918,
        raster_height: 1188,
        display_to_raster: AffineTransform {
            a: 1.5,
            b: 0.0,
            c: 0.0,
            d: 1.5,
            e: 0.0,
            f: 0.0,
        },
        crop: Rect {
            left: 0.0,
            top: 0.0,
            right: 918.0,
            bottom: 1188.0,
        },
        resized_width: 918,
        resized_height: 1188,
        padding: Padding {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    }
}

#[derive(Default)]
struct Pages {
    rendered: Rc<RefCell<Vec<u32>>>,
    failing: Vec<u32>,
    side: Option<u32>,
    count: Option<usize>,
    planned: Option<(u32, u32)>,
}
impl PageRenderer for Pages {
    fn render(&mut self, page: u32) -> Result<PageImage, RenderFailed> {
        self.rendered.borrow_mut().push(page);
        if self.failing.contains(&page) {
            return Err(RenderFailed);
        }
        let png = match self.side {
            Some(side) => png(side, side, page),
            None => png(918, 1188, page),
        };
        Ok(PageImage {
            png,
            transform: transform(),
        })
    }
    fn page_count(&self) -> Option<usize> {
        self.count
    }
    fn planned_size(&self, _: u32) -> Option<(u32, u32)> {
        self.planned
    }
}

#[derive(Clone, Default)]
struct Time(Rc<Cell<u64>>);
impl Clock for Time {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep_ms(&mut self, duration: u64) {
        self.0.set(self.0.get() + duration);
    }
}

/// What the transport saw for one send.
struct Sent {
    content_type: String,
    body: Vec<u8>,
    request: Value,
    page: Option<u32>,
}

/// A scripted reply for the next send; `Answer` answers every question.
enum Script {
    Answer,
    Status(u16, &'static [u8]),
    Truncated(&'static [u8]),
    Usage(u64),
    Extra(Value),
    /// Answer every choice question with `none`.
    ChooseNone,
}

/// Per page and field prefix (`i0`): the noul answer and the normalized box.
type Locate = Rc<dyn Fn(u32, &str) -> (f64, [f64; 4])>;

struct Wire {
    sent: Rc<RefCell<Vec<Sent>>>,
    locate: Locate,
    scripts: VecDeque<Script>,
}

/// The `request` part of a multipart body.
fn request_part(body: &[u8]) -> Value {
    let text = String::from_utf8_lossy(body);
    let start = text.find("\r\n\r\n").expect("a part header") + 4;
    let end = text[start..]
        .find("\r\n--voltfill_fill_boundary")
        .expect("the request part ends at the boundary");
    serde_json::from_str(&text[start..start + end]).expect("the request part is JSON")
}

/// The page a synthetic image names; real renders are single-page here.
fn page_of(body: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(body);
    let Some(at) = text.find(PNG_CANARY).map(|at| at + PNG_CANARY.len()) else {
        return 1;
    };
    text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("the image names its page")
}

impl Transport for Wire {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        let body = request.body.bytes().to_vec();
        let content_type = request.body.content_type.clone();
        let (json, page) = if content_type == "application/json" {
            (serde_json::from_slice(&body).expect("JSON request"), None)
        } else {
            (request_part(&body), Some(page_of(&body)))
        };
        self.sent.borrow_mut().push(Sent {
            content_type,
            body,
            request: json.clone(),
            page,
        });
        let reply = |status: u16, body: Vec<u8>, error: Option<TransportError>| {
            Ok(HttpResponse {
                status,
                body,
                body_error: error,
                retry_after_ms: None,
            })
        };
        let mut none = false;
        let (usage, extra) = match self.scripts.pop_front().unwrap_or(Script::Answer) {
            Script::Answer => (120, json!({})),
            Script::ChooseNone => {
                none = true;
                (120, json!({}))
            }
            Script::Status(code, body) => return reply(code, body.to_vec(), None),
            Script::Truncated(body) => {
                return reply(200, body.to_vec(), Some(TransportError::OutcomeUnknown));
            }
            Script::Usage(tokens) => (tokens, json!({})),
            Script::Extra(extra) => (120, extra),
        };
        let mut answers = serde_json::Map::new();
        for (id, question) in json["questions"].as_object().expect("questions") {
            let answer = match question["type"].as_str().expect("typed question") {
                // Every choice is a tie, so the choice step leaves the field.
                "choice" if none => {
                    let criteria = question["criteria"].as_object().expect("criteria");
                    let probabilities: serde_json::Map<String, Value> = criteria
                        .keys()
                        .map(|k| (k.clone(), json!(if k == "none" { 1.0 } else { 0.0 })))
                        .collect();
                    json!({"type":"choice","choice":"none","probabilities":probabilities,"confidence":0.9})
                }
                "choice" => {
                    let criteria = question["criteria"].as_object().expect("criteria");
                    let share = 1.0 / criteria.len() as f64;
                    let probabilities: serde_json::Map<String, Value> =
                        criteria.keys().map(|k| (k.clone(), json!(share))).collect();
                    let first = criteria.keys().next().expect("an option").clone();
                    json!({"type":"choice","choice":first,"probabilities":probabilities,"confidence":0.5})
                }
                kind => {
                    let page = page.expect("image questions come with an image");
                    let field = id.split('_').next().expect("question ids are i<n>_<kind>");
                    let (noul, [left, top, right, bottom]) = (self.locate)(page, field);
                    if kind == "noul" {
                        json!({"type":"noul","noul":noul})
                    } else {
                        json!({"type":"box","box":{"left":left,"top":top,"right":right,"bottom":bottom},"confidence":0.9})
                    }
                }
            };
            answers.insert(id.clone(), answer);
        }
        let mut body = json!({
            "model": json["model"],
            "answers": answers,
            "usage": {"input_tokens": usage, "output_tokens": 3},
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            body.extend(extra.clone());
        }
        reply(
            200,
            serde_json::to_vec(&body).expect("the scripted body serializes"),
            None,
        )
    }
}

fn form(fields: Value) -> Form {
    fill::parse_form(json!({ "fields": fields }).to_string().as_bytes())
        .expect("the synthetic form is valid")
}

fn snapshot(pages: &[&[(f64, f64, &str)]]) -> DocumentSnapshot {
    inspect(&support::pdf_with_pages(pages), PdfLimits::default())
        .expect("the synthetic PDF inspects")
}

fn fragment<'s>(snapshot: &'s DocumentSnapshot, page: u32, text: &str) -> &'s SourceFragment {
    snapshot
        .fragments
        .iter()
        .find(|f| f.page == page && f.raw_text == text)
        .unwrap_or_else(|| panic!("a fragment {text:?} on page {page}"))
}

/// The normalized box of a Display box on the synthetic Letter page image.
fn normalized(bbox: Rect) -> [f64; 4] {
    [
        bbox.left / PAGE.0,
        bbox.top / PAGE.1,
        bbox.right / PAGE.0,
        bbox.bottom / PAGE.1,
    ]
}

struct Run {
    result: Result<AssistedFill, ImageError>,
    before: vf_core::fill::FillResult,
    sent: Vec<Sent>,
    rendered: Vec<u32>,
}

fn run_with(
    snapshot: &DocumentSnapshot,
    form: &Form,
    options: &AssistOptions,
    image_options: &ImageOptions,
    pages: Pages,
    locate: Locate,
    scripts: Vec<Script>,
) -> Run {
    let before = fill::fill(&snapshot.document_id, &snapshot.fragments, form);
    run_after(
        before,
        snapshot,
        form,
        (options, image_options, pages),
        locate,
        scripts,
    )
}

/// The choice and image steps over a given offline result `before`.
fn run_after(
    before: vf_core::fill::FillResult,
    snapshot: &DocumentSnapshot,
    form: &Form,
    (options, image_options, mut pages): (&AssistOptions, &ImageOptions, Pages),
    locate: Locate,
    scripts: Vec<Script>,
) -> Run {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let rendered = pages.rendered.clone();
    let mut wire = Wire {
        sent: sent.clone(),
        locate,
        scripts: scripts.into(),
    };
    let secret = Secret::try_new(SECRET.into()).expect("synthetic bearer");
    let result = images::assist_with_images(
        before.clone(),
        form,
        options,
        ImageInputs {
            snapshot,
            renderer: &mut pages,
            options: image_options,
        },
        &secret,
        &mut wire,
        &mut Time::default(),
    );
    let sent = std::mem::take(&mut *sent.borrow_mut());
    let rendered = rendered.borrow().clone();
    Run {
        result,
        before,
        sent,
        rendered,
    }
}

fn options(execution: ExecutionConfig) -> AssistOptions {
    AssistOptions {
        allow_remote: true,
        model: MODEL.into(),
        execution,
    }
}

fn run(
    snapshot: &DocumentSnapshot,
    form: &Form,
    locate: Locate,
    scripts: Vec<Script>,
    execution: ExecutionConfig,
) -> Run {
    run_with(
        snapshot,
        form,
        &options(execution),
        &ImageOptions::default(),
        Pages::default(),
        locate,
        scripts,
    )
}

fn done(run: &Run) -> &AssistedFill {
    run.result.as_ref().expect("valid inputs never fail")
}

fn body_text(body: Option<&ResponseBody>) -> &str {
    match body.expect("a received body is kept") {
        ResponseBody::Text(text) => text,
        ResponseBody::Bytes(_) => panic!("synthetic bodies are UTF-8"),
    }
}

/// `Order` labels `AB12` on page 1; the form calls it `Reference`, so the
/// offline fill finds nothing.
const ORDER: &[(f64, f64, &str)] = &[(72.0, 700.0, "Order"), (200.0, 700.0, "AB12")];

fn reference_form(field_type: &str) -> Form {
    form(json!([{"id":"ref","label":"Reference","type":field_type}]))
}

fn assert_unchanged(after: &FieldFill, before: &FieldFill) {
    assert_eq!(after, before, "the field keeps its offline result");
    assert!(after.located.is_none());
}

#[test]
fn a_value_box_fills_from_the_text_layer_with_the_model_box() {
    let snapshot = snapshot(&[ORDER]);
    let value = fragment(&snapshot, 1, "AB12").clone();
    let form = reference_form("text");
    let target = normalized(value.bbox);
    let run = run(
        &snapshot,
        &form,
        Rc::new(move |_, _| (0.9, target)),
        Vec::new(),
        ExecutionConfig::default(),
    );
    assert_eq!(run.before.fields["ref"].status, FillStatus::NotFound);
    let out = done(&run);
    let field = &out.fill.fields["ref"];
    assert_eq!(field.status, FillStatus::Filled);
    assert_eq!(field.method, Some(images::METHOD));
    assert_eq!(field.raw.as_deref(), Some("AB12"));
    assert_eq!(field.value, Some(json!("AB12")));
    assert_eq!(field.evidence.len(), 1);
    let evidence = &field.evidence[0];
    assert_eq!(evidence.fragment_id, value.id);
    assert_eq!((evidence.start, evidence.end), (0, 4));
    assert_eq!(evidence.text, "AB12");
    assert_eq!(evidence.bbox, value.bbox);
    assert_eq!(evidence.precision, Precision::Run);
    let located = field.located.as_ref().expect("the model box is kept");
    assert_eq!(located.source, "model");
    assert_eq!(located.page, 1);
    assert!((located.bbox.left - value.bbox.left).abs() < 1e-9);
    assert!((located.bbox.bottom - value.bbox.bottom).abs() < 1e-9);
    assert!((located.normalized.right - target[2]).abs() < 1e-12);
    assert!(out.assist.is_empty(), "nothing was ambiguous");
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::Selected);
    assert_eq!(record.page, Some(1));
    assert_eq!(
        (
            record.presence_question_id.as_str(),
            record.box_question_id.as_str()
        ),
        ("i0_present", "i0_box")
    );
    assert_eq!(record.pages.len(), 1);
    assert_eq!(record.pages[0].presence, Some(0.9));
    assert!(record.pages[0].bbox.is_some());
    assert_eq!(run.rendered, [1]);

    // One multipart request: the page image once, the field's label, type
    // and the two questions; no text-layer content.
    assert_eq!(run.sent.len(), 1);
    let sent = &run.sent[0];
    assert!(sent.content_type.starts_with("multipart/form-data"));
    assert_eq!(sent.page, Some(1));
    let request = &sent.request;
    assert_eq!(request["model"], MODEL);
    let reference = request["state"]["image"]
        .as_str()
        .expect("an image reference");
    assert!(reference.starts_with("<voltfill-image-"));
    assert_eq!(request["assets"][reference]["part"], "image");
    let questions = request["questions"].as_object().expect("questions");
    assert_eq!(
        questions.keys().collect::<Vec<_>>(),
        ["i0_box", "i0_present"]
    );
    assert_eq!(questions["i0_present"]["type"], "noul");
    assert_eq!(questions["i0_box"]["type"], "box");
    for question in questions.values() {
        let instructions = question["instructions"].as_str().expect("text");
        assert!(instructions.contains("「Reference」（文字列）"));
        assert!(!instructions.contains("<voltfill-image-"));
    }
    let raw = String::from_utf8_lossy(&sent.body);
    assert!(raw.contains("PNG_PIXEL_CANARY_p1"));
    let part = request.to_string();
    assert!(!part.contains("AB12") && !part.contains("Order") && !raw.contains(SECRET));

    // The received body is kept with its page and questions.
    assert_eq!(out.jev_responses.len(), 1);
    let response = &out.jev_responses[0];
    assert_eq!(response.page, Some(1));
    assert_eq!(response.field_ids, ["ref"]);
    assert_eq!(response.question_ids, ["i0_present", "i0_box"]);
    assert!(response.body_complete);
    let body: Value = serde_json::from_str(body_text(response.body.as_ref())).expect("JSON");
    assert_eq!(body["answers"]["i0_box"]["confidence"], 0.9);
    assert_eq!(out.usage.attempts, 1);
    assert!(out.fill.evaluation_only);

    let json = serde_json::to_value(out).expect("the result serializes");
    assert_eq!(json["fields"]["ref"]["method"], "jev_box");
    assert_eq!(json["fields"]["ref"]["located"]["source"], "model");
    assert_eq!(json["image_assist"]["ref"]["outcome"], "selected");
    assert_eq!(json["jev_responses"][0]["page"], 1);
}

#[test]
fn a_whole_line_box_over_label_and_value_fills_the_value_after_a_tied_choice() {
    let snapshot = snapshot(&[&[(72.0, 700.0, "Ref: AB12"), (72.0, 660.0, "Ref: CD34")]]);
    let line = fragment(&snapshot, 1, "Ref: AB12").bbox;
    let form = form(json!([{"id":"ref","label":"Ref"}]));
    let run = run(
        &snapshot,
        &form,
        Rc::new(move |_, _| (0.8, normalized(line))),
        Vec::new(),
        ExecutionConfig::default(),
    );
    assert_eq!(run.before.fields["ref"].status, FillStatus::Ambiguous);
    let out = done(&run);
    assert_eq!(out.assist["ref"].outcome, AssistOutcome::NotUniqueMaximum);
    let field = &out.fill.fields["ref"];
    assert_eq!(field.status, FillStatus::Filled);
    assert_eq!(field.method, Some(images::METHOD));
    assert_eq!(field.raw.as_deref(), Some("AB12"));
    assert_eq!(field.evidence[0].precision, Precision::Estimated);
    assert_eq!(field.candidates, run.before.fields["ref"].candidates);
    assert_eq!(out.image_assist["ref"].outcome, ImageOutcome::Selected);
    // The choice step and the image step share one result.
    assert_eq!(run.sent.len(), 2);
    assert_eq!(run.sent[0].content_type, "application/json");
    assert_eq!(out.jev_responses[0].page, None);
    assert_eq!(out.jev_responses[1].page, Some(1));
    assert_eq!(out.usage.attempts, 2);
}

#[test]
fn label_only_mixed_and_empty_boxes_are_not_filled() {
    // A box over the label run alone.
    let snapshot_a = snapshot(&[&[
        (72.0, 700.0, "Ref"),
        (200.0, 700.0, "AB12"),
        (72.0, 660.0, "Ref"),
        (200.0, 660.0, "CD34"),
    ]]);
    let label = snapshot_a
        .fragments
        .iter()
        .find(|f| f.raw_text == "Ref")
        .expect("a label run")
        .bbox;
    let ref_form = form(json!([{"id":"ref","label":"Ref"}]));
    // A run that may hold a label and its value.
    let snapshot_b = snapshot(&[&[(72.0, 700.0, "Total = 12000")]]);
    let mixed = fragment(&snapshot_b, 1, "Total = 12000").bbox;
    let total_form = form(json!([{"id":"ref","label":"Total","type":"amount"}]));
    // An empty area of the right page.
    let snapshot_c = snapshot(&[ORDER]);
    for (snapshot, form, area, outcome) in [
        (
            &snapshot_a,
            &ref_form,
            normalized(label),
            ImageOutcome::LabelInValue,
        ),
        (
            &snapshot_b,
            &total_form,
            normalized(mixed),
            ImageOutcome::MixedText,
        ),
        (
            &snapshot_c,
            &reference_form("text"),
            [0.7, 0.1, 0.8, 0.15],
            ImageOutcome::NoValueInBox,
        ),
    ] {
        let run = run(
            snapshot,
            form,
            Rc::new(move |_, _| (0.9, area)),
            Vec::new(),
            ExecutionConfig::default(),
        );
        assert_ne!(run.before.fields["ref"].status, FillStatus::Filled);
        let out = done(&run);
        assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
        let record = &out.image_assist["ref"];
        assert_eq!(record.outcome, outcome);
        assert!(!outcome.is_failure());
        assert_eq!(record.page, Some(1));
        assert!(record.pages[0].bbox.is_some());
    }
}

#[test]
fn a_box_on_a_page_without_the_value_is_not_filled() {
    let snapshot = snapshot(&[ORDER, &[(72.0, 400.0, "Notes")]]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    // Page 2 is said to hold the value, at page 1's position.
    let run = run(
        &snapshot,
        &reference_form("text"),
        Rc::new(move |page, _| {
            if page == 2 {
                (0.9, value)
            } else {
                (0.1, value)
            }
        }),
        Vec::new(),
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::NoValueInBox);
    assert_eq!(record.page, Some(2));
    assert_eq!(record.pages[0].outcome, ImageOutcome::Absent);
    assert_eq!(record.pages[0].presence, Some(0.1));
    assert!(record.pages[0].bbox.is_none());
    assert_eq!(run.rendered, [1, 2]);
    assert_eq!(
        run.sent.iter().map(|s| s.page).collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
}

#[test]
fn absent_everywhere_or_present_on_two_pages_is_not_filled() {
    let snapshot = snapshot(&[ORDER, ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    for (noul, outcome) in [
        (0.2, ImageOutcome::Absent),
        (0.9, ImageOutcome::PresentOnMultiplePages),
    ] {
        let run = run(
            &snapshot,
            &reference_form("text"),
            Rc::new(move |_, _| (noul, value)),
            Vec::new(),
            ExecutionConfig::default(),
        );
        let out = done(&run);
        assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
        let record = &out.image_assist["ref"];
        assert_eq!(record.outcome, outcome);
        assert_eq!(record.page, None);
        assert_eq!(record.pages.len(), 2);
    }
}

#[test]
fn a_value_the_type_grammar_rejects_is_not_filled() {
    let snapshot = snapshot(&[ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let run = run(
        &snapshot,
        &reference_form("amount"),
        Rc::new(move |_, _| (0.9, value)),
        Vec::new(),
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    assert_eq!(out.image_assist["ref"].outcome, ImageOutcome::TypeMismatch);
}

#[test]
fn a_scanned_page_is_located_but_never_filled() {
    let snapshot = snapshot(&[&[]]);
    assert!(snapshot.fragments.is_empty());
    let form = form(json!([{"id":"no","label":"Invoice No"}]));
    let area = [0.1, 0.1, 0.3, 0.15];
    let run = run(
        &snapshot,
        &form,
        Rc::new(move |_, _| (0.95, area)),
        Vec::new(),
        ExecutionConfig::default(),
    );
    assert_eq!(run.before.fields["no"].status, FillStatus::NoText);
    let out = done(&run);
    let field = &out.fill.fields["no"];
    assert_eq!(field.status, FillStatus::NoText);
    assert!(field.method.is_none() && field.raw.is_none() && field.value.is_none());
    assert!(field.evidence.is_empty());
    let located = field.located.as_ref().expect("the model box is the result");
    assert_eq!(located.source, "model");
    assert_eq!(located.page, 1);
    assert!((located.bbox.left - 0.1 * PAGE.0).abs() < 1e-9);
    assert!((located.bbox.bottom - 0.15 * PAGE.1).abs() < 1e-9);
    assert!((located.normalized.top - 0.1).abs() < 1e-12);
    assert_eq!(out.image_assist["no"].outcome, ImageOutcome::Located);
    assert_eq!(run.sent.len(), 1);
    let json = serde_json::to_value(out).expect("serializes");
    assert_eq!(json["fields"]["no"]["status"], "no_text");
    assert!(json["fields"]["no"].get("raw").is_none());
}

#[test]
fn the_shared_budget_stops_later_pages_and_their_fields_stay_empty() {
    let snapshot = snapshot(&[ORDER, &[(72.0, 400.0, "Notes")]]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let locate: Locate = Rc::new(move |page, _| (if page == 1 { 0.9 } else { 0.1 }, value));
    // One request: page 1 answers, page 2 is neither rendered nor sent.
    let run_one = run(
        &snapshot,
        &reference_form("text"),
        locate.clone(),
        Vec::new(),
        ExecutionConfig {
            max_requests: 1,
            ..Default::default()
        },
    );
    let out = done(&run_one);
    assert_eq!(run_one.sent.len(), 1);
    assert_eq!(run_one.rendered, [1]);
    assert_unchanged(&out.fill.fields["ref"], &run_one.before.fields["ref"]);
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::BudgetExhausted);
    assert!(record.outcome.is_failure());
    assert_eq!(record.page, Some(2));
    assert_eq!(record.pages[0].outcome, ImageOutcome::Selected);

    // Reported usage above the reservation stops the budget; the answer is
    // not used and its body is kept.
    let run_over = run(
        &snapshot,
        &reference_form("text"),
        locate.clone(),
        vec![Script::Usage(900_000)],
        ExecutionConfig::default(),
    );
    let out = done(&run_over);
    assert_eq!(run_over.sent.len(), 1);
    assert!(body_text(out.jev_responses[0].body.as_ref()).contains("900000"));
    assert_unchanged(&out.fill.fields["ref"], &run_over.before.fields["ref"]);
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::UsageExceededBudget);
    // The field's outcome is decided, so page 2 is neither rendered nor asked.
    assert_eq!(record.pages.len(), 1);
    assert_eq!(run_over.rendered, [1]);

    // The page image's tokens are part of each reservation: a per-attempt
    // cap below the image allowance sends nothing.
    let run_small = run(
        &snapshot,
        &reference_form("text"),
        locate,
        Vec::new(),
        ExecutionConfig {
            input_tokens_per_attempt: 10_000,
            ..Default::default()
        },
    );
    let out = done(&run_small);
    assert!(run_small.sent.is_empty());
    // The token fit is checked before any page is rendered.
    assert!(run_small.rendered.is_empty());
    assert_eq!(out.image_assist["ref"].pages.len(), 1);
    assert_eq!(
        out.image_assist["ref"].outcome,
        ImageOutcome::RequestTooLarge
    );
}

#[test]
fn every_received_body_is_kept_with_its_page() {
    let snapshot = snapshot(&[ORDER, &[(72.0, 400.0, "Notes")]]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let run = run(
        &snapshot,
        &reference_form("text"),
        Rc::new(move |page, _| (if page == 1 { 0.9 } else { 0.1 }, value)),
        vec![
            Script::Status(503, b"busy page one"),
            Script::Answer,
            Script::Truncated(b"{\"model\":\"jev-1.14.0\",\"ans"),
            Script::Status(200, &[b'n', b'o', 0xff]),
        ],
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_eq!(run.sent.len(), 4);
    // Retries resend the same body.
    assert_eq!(run.sent[0].body, run.sent[1].body);
    assert_eq!(run.sent[2].body, run.sent[3].body);
    let responses = &out.jev_responses;
    assert_eq!(
        responses.iter().map(|r| r.page).collect::<Vec<_>>(),
        [Some(1), Some(1), Some(2), Some(2)]
    );
    assert_eq!(responses[0].http_status, Some(503));
    assert_eq!(body_text(responses[0].body.as_ref()), "busy page one");
    assert!(!responses[2].body_complete);
    assert_eq!(
        responses[2].transport_error,
        Some(TransportError::OutcomeUnknown)
    );
    assert_eq!(
        body_text(responses[2].body.as_ref()),
        "{\"model\":\"jev-1.14.0\",\"ans"
    );
    assert!(matches!(
        responses[3].body.as_ref(),
        Some(ResponseBody::Bytes(b)) if b == &[b'n', b'o', 0xff]
    ));
    assert_eq!(out.usage.attempts, 4);
    // Page 2 failed, so page 1's value is not used.
    assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::RequestFailed);
    assert_eq!(record.page, Some(2));
    assert_eq!(record.pages[0].outcome, ImageOutcome::Selected);
}

#[test]
fn pages_that_cannot_be_rendered_or_sent_are_not_sent() {
    let snapshot = snapshot(&[ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let locate: Locate = Rc::new(move |_, _| (0.9, value));
    for (pages, outcome) in [
        (
            Pages {
                failing: vec![1],
                ..Default::default()
            },
            ImageOutcome::RenderFailed,
        ),
        (
            Pages {
                side: Some(100),
                ..Default::default()
            },
            ImageOutcome::ImageRejected,
        ),
    ] {
        let run = run_with(
            &snapshot,
            &reference_form("text"),
            &options(ExecutionConfig::default()),
            &ImageOptions::default(),
            pages,
            locate.clone(),
            Vec::new(),
        );
        let out = done(&run);
        assert!(run.sent.is_empty() && out.jev_responses.is_empty());
        assert_eq!(run.rendered, [1]);
        assert_eq!(out.image_assist["ref"].outcome, outcome);
        assert!(outcome.is_failure());
        assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    }
}

#[test]
fn invalid_inputs_fail_before_anything_is_rendered_or_sent() {
    let two_pages = snapshot(&[ORDER, ORDER]);
    let locate: Locate = Rc::new(|_, _| (0.9, [0.1, 0.1, 0.2, 0.2]));
    let form = reference_form("text");
    let defaults = ImageOptions::default();
    let cases: Vec<(AssistOptions, ImageOptions, Pages, ImageError)> = vec![
        (
            AssistOptions {
                allow_remote: false,
                ..options(ExecutionConfig::default())
            },
            defaults.clone(),
            Pages::default(),
            ImageError::Assist(AssistError::RemoteDisabled),
        ),
        (
            AssistOptions {
                model: "jev-latest".into(),
                ..options(ExecutionConfig::default())
            },
            defaults.clone(),
            Pages::default(),
            ImageError::ModelWithoutImages,
        ),
        (
            options(ExecutionConfig::default()),
            ImageOptions {
                max_pages: 0,
                ..ImageOptions::default()
            },
            Pages::default(),
            ImageError::Options,
        ),
        (
            options(ExecutionConfig::default()),
            ImageOptions {
                max_pages: 1,
                ..ImageOptions::default()
            },
            Pages::default(),
            ImageError::PageLimit,
        ),
        (
            options(ExecutionConfig::default()),
            defaults.clone(),
            Pages {
                planned: Some((1224, 1584)),
                ..Default::default()
            },
            ImageError::ImageSize,
        ),
        // The renderer's own page count, before the snapshot is consulted.
        (
            options(ExecutionConfig::default()),
            ImageOptions {
                max_pages: 2,
                ..ImageOptions::default()
            },
            Pages {
                count: Some(3),
                ..Default::default()
            },
            ImageError::PageLimit,
        ),
    ];
    for (assist_options, image_options, pages, error) in cases {
        let run = run_with(
            &two_pages,
            &form,
            &assist_options,
            &image_options,
            pages,
            locate.clone(),
            Vec::new(),
        );
        assert_eq!(run.result.err(), Some(error));
        assert!(run.sent.is_empty() && run.rendered.is_empty());
    }
    // A snapshot of another document.
    let other = snapshot(&[&[(72.0, 700.0, "Other")]]);
    let before = fill::fill(&other.document_id, &other.fragments, &form);
    let secret = Secret::try_new(SECRET.into()).expect("synthetic bearer");
    let result = images::assist_with_images(
        before,
        &form,
        &options(ExecutionConfig::default()),
        ImageInputs {
            snapshot: &two_pages,
            renderer: &mut Pages::default(),
            options: &defaults,
        },
        &secret,
        &mut Wire {
            sent: Rc::default(),
            locate,
            scripts: VecDeque::new(),
        },
        &mut Time::default(),
    );
    assert_eq!(result.err(), Some(ImageError::Snapshot));
}

#[test]
fn fields_with_a_value_are_not_sent_and_nothing_is_rendered_without_pending_fields() {
    let snapshot = snapshot(&[&[(72.0, 700.0, "Ref: AB12")]]);
    let form = form(json!([{"id":"ref","label":"Ref"}]));
    let run = run(
        &snapshot,
        &form,
        Rc::new(|_, _| (0.9, [0.1, 0.1, 0.2, 0.2])),
        Vec::new(),
        ExecutionConfig::default(),
    );
    assert_eq!(run.before.fields["ref"].status, FillStatus::Filled);
    let out = done(&run);
    assert!(run.sent.is_empty() && run.rendered.is_empty());
    assert!(out.image_assist.is_empty());
    assert_eq!(out.fill.fields["ref"], run.before.fields["ref"]);
    let json = serde_json::to_value(out).expect("serializes");
    assert!(json.get("image_assist").is_none());
}

/// A one-page Letter snapshot whose text layer is replaced by hand-made
/// fragments (Japanese text and exact geometry), page 1.
fn manual(fragments: &[(&str, &str, [f64; 4])]) -> DocumentSnapshot {
    let mut snapshot = snapshot(&[&[(72.0, 700.0, "x")]]);
    snapshot.fragments = fragments
        .iter()
        .enumerate()
        .map(
            |(i, (id, text, [left, top, right, bottom]))| SourceFragment {
                id: (*id).into(),
                page: 1,
                item_index: i,
                raw_text: (*text).into(),
                bbox: Rect {
                    left: *left,
                    top: *top,
                    right: *right,
                    bottom: *bottom,
                },
                quality: Quality::Usable,
                orientation: Orientation::Deg0,
            },
        )
        .collect();
    snapshot
}

/// The normalized box of a Display area `[left, top, right, bottom]`.
fn area([left, top, right, bottom]: [f64; 4]) -> [f64; 4] {
    normalized(Rect {
        left,
        top,
        right,
        bottom,
    })
}

/// Locate: the given field prefix (`i0`) is present at `target`, every other
/// field is absent.
fn only(field: &'static str, target: [f64; 4]) -> Locate {
    Rc::new(move |_, f| {
        if f == field {
            (0.9, target)
        } else {
            (0.1, target)
        }
    })
}

fn default_run(snapshot: &DocumentSnapshot, form: &Form, locate: Locate) -> Run {
    run(
        snapshot,
        form,
        locate,
        Vec::new(),
        ExecutionConfig::default(),
    )
}

#[test]
fn a_value_box_inside_another_labels_inline_run_is_refused() {
    // E1: `電話：03-1234-5678`, 11 units high; the value starts near x 93.
    let snapshot = manual(&[("tel", "電話：03-1234-5678", [60.0, 119.0, 161.4, 130.0])]);
    let form = form(json!([{"id":"name","label":"氏名"},{"id":"tel","label":"電話"}]));
    for left in [58.0, 92.0, 93.0, 94.0] {
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([left, 117.0, 163.0, 132.0])),
        );
        assert_eq!(run.before.fields["tel"].status, FillStatus::Filled);
        let out = done(&run);
        assert_unchanged(&out.fill.fields["name"], &run.before.fields["name"]);
        assert_eq!(
            out.image_assist["name"].outcome,
            ImageOutcome::OtherLabel,
            "box from x {left}"
        );
        assert_eq!(out.fill.fields["tel"], run.before.fields["tel"]);
    }
    // `所属` is not a form label at all; the value still belongs to it.
    let snapshot = manual(&[("dept", "所属：経理部", [60.0, 119.0, 126.0, 130.0])]);
    let form = self::form(json!([{"id":"name","label":"氏名"}]));
    for left in [58.0, 92.0] {
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([left, 117.0, 128.0, 132.0])),
        );
        let out = done(&run);
        assert_unchanged(&out.fill.fields["name"], &run.before.fields["name"]);
        assert_eq!(out.image_assist["name"].outcome, ImageOutcome::OtherLabel);
    }
}

#[test]
fn a_value_box_inside_the_fields_own_inline_run_is_accepted() {
    let snapshot = manual(&[
        ("a", "請求番号：INV-0012", [60.0, 119.0, 161.4, 130.0]),
        ("b", "請求番号：INV-0099", [60.0, 159.0, 161.4, 170.0]),
    ]);
    let form = form(json!([{"id":"no","label":"請求番号"}]));
    // The worked example of select: a box from x 115.0 holds the value only.
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([114.0, 117.0, 163.0, 132.0])),
    );
    assert_eq!(run.before.fields["no"].status, FillStatus::Ambiguous);
    let out = done(&run);
    let field = &out.fill.fields["no"];
    assert_eq!(field.status, FillStatus::Filled);
    assert_eq!(field.method, Some(images::METHOD));
    assert_eq!(field.raw.as_deref(), Some("INV-0012"));
    let evidence = &field.evidence[0];
    assert_eq!((evidence.start, evidence.end), (15, 23));
    assert_eq!(evidence.precision, Precision::Estimated);
    // A hint is the field's label too.
    let form = self::form(json!([{"id":"no","label":"Invoice No","hints":["請求番号"]}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([114.0, 117.0, 163.0, 132.0])),
    );
    let out = done(&run);
    assert_eq!(out.fill.fields["no"].raw.as_deref(), Some("INV-0012"));
}

#[test]
fn another_fields_layout_value_is_refused() {
    // E2: `Order` labels `AB12`; boxed for `Reference` of a form that has `Order`.
    let snapshot = snapshot(&[ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let form = form(json!([{"id":"order","label":"Order"},{"id":"ref","label":"Reference"}]));
    let run = default_run(&snapshot, &form, only("i1", value));
    let out = done(&run);
    assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    assert_eq!(
        out.image_assist["ref"].outcome,
        ImageOutcome::OtherFieldValue
    );
    assert_eq!(out.image_assist["order"].outcome, ImageOutcome::Absent);
}

/// E3: a two-row table under `品名` and `金額`, 11 units high.
fn table() -> DocumentSnapshot {
    manual(&[
        ("h1", "品名", [60.0, 100.0, 82.0, 111.0]),
        ("h2", "金額", [300.0, 100.0, 322.0, 111.0]),
        ("c1", "部品A", [60.0, 120.0, 98.5, 131.0]),
        ("c2", "¥1,000", [300.0, 120.0, 333.0, 131.0]),
        ("c3", "部品B", [60.0, 140.0, 98.5, 151.0]),
        ("c4", "¥2,000", [300.0, 140.0, 333.0, 151.0]),
    ])
}

#[test]
fn a_table_cell_of_another_field_or_one_span_for_two_fields_is_refused() {
    let snapshot = table();
    let cell = area([298.0, 118.0, 335.0, 133.0]);
    let form = form(json!([
        {"id":"total","label":"合計","type":"amount"},
        {"id":"tax","label":"消費税","type":"amount"},
        {"id":"price","label":"金額","type":"amount"}
    ]));
    let run = default_run(
        &snapshot,
        &form,
        Rc::new(move |_, f| (if f == "i2" { 0.1 } else { 0.9 }, cell)),
    );
    let out = done(&run);
    for id in ["total", "tax"] {
        assert_unchanged(&out.fill.fields[id], &run.before.fields[id]);
        assert_eq!(
            out.image_assist[id].outcome,
            ImageOutcome::OtherFieldValue,
            "{id}"
        );
    }
    // Without the column's own field, a cell still belongs to its header
    // (`金額`), not to a field the form names differently.
    let form = self::form(json!([{"id":"total","label":"合計","type":"amount"}]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    let out = done(&run);
    assert_unchanged(&out.fill.fields["total"], &run.before.fields["total"]);
    assert_eq!(
        out.image_assist["total"].outcome,
        ImageOutcome::UnmatchedLabel
    );
    // A cell of a column the form names is accepted for that field.
    let form = self::form(json!([{"id":"price","label":"金額","type":"amount"}]));
    let last = area([298.0, 138.0, 335.0, 153.0]);
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, last)));
    // Offline: a column is position only, and two rows make it ambiguous.
    assert_eq!(run.before.fields["price"].status, FillStatus::Ambiguous);
    let out = done(&run);
    assert_eq!(out.fill.fields["price"].value, Some(json!(2000)));
    assert_eq!(out.fill.fields["price"].method, Some(images::METHOD));
    // A lone run (no layout pair) boxed for two fields fills neither.
    let snapshot = manual(&[("v", "¥1,000", [300.0, 120.0, 333.0, 131.0])]);
    let form = self::form(json!([
        {"id":"total","label":"合計","type":"amount"},
        {"id":"tax","label":"消費税","type":"amount"}
    ]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    let out = done(&run);
    for id in ["total", "tax"] {
        assert_unchanged(&out.fill.fields[id], &run.before.fields[id]);
        let record = &out.image_assist[id];
        assert_eq!(record.outcome, ImageOutcome::SharedSpan, "{id}");
        assert_eq!(record.pages[0].outcome, ImageOutcome::Selected);
    }
    // One of them alone is filled: the documented residual risk of a run
    // that no layout pair ties to a label.
    let form = self::form(json!([{"id":"total","label":"合計","type":"amount"}]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    let out = done(&run);
    assert_eq!(out.fill.fields["total"].value, Some(json!(1000)));
}

/// The `questions` of every image request, in send order.
fn image_questions(run: &Run) -> Vec<Vec<String>> {
    run.sent
        .iter()
        .filter(|sent| sent.page.is_some())
        .map(|sent| {
            sent.request["questions"]
                .as_object()
                .expect("questions")
                .keys()
                .cloned()
                .collect()
        })
        .collect()
}

#[test]
fn table_fields_are_never_asked_or_filled_by_the_image_step() {
    let snapshot = table();
    let cell = area([298.0, 138.0, 335.0, 153.0]);
    // No header row has every column: the table field is not found, and
    // still neither rendered for nor sent, however the model would answer.
    let items = json!({"id":"items","label":"明細","type":"table","columns":[
        {"id":"name","label":"品名"},{"id":"qty","label":"数量","type":"number"}
    ]});
    let form = form(json!([items]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    assert_eq!(run.before.fields["items"].status, FillStatus::NotFound);
    let out = done(&run);
    assert!(run.sent.is_empty() && run.rendered.is_empty());
    assert!(out.image_assist.is_empty() && out.assist.is_empty());
    assert_unchanged(&out.fill.fields["items"], &run.before.fields["items"]);
    // Beside a scalar field, only the scalar field is asked about.
    let form = self::form(json!([
        items,
        {"id":"price","label":"金額","type":"amount"}
    ]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    let out = done(&run);
    assert_eq!(image_questions(&run), [["i0_box", "i0_present"]]);
    assert_eq!(out.image_assist.keys().collect::<Vec<_>>(), ["price"]);
    assert_unchanged(&out.fill.fields["items"], &run.before.fields["items"]);
    assert_eq!(out.fill.fields["price"].value, Some(json!(2000)));
    // A filled table's cells are Evidence a scalar field may not reuse.
    let form = self::form(json!([
        {"id":"items","label":"明細","type":"table","columns":[
            {"id":"name","label":"品名"},{"id":"amount","label":"金額","type":"amount"}
        ]},
        {"id":"price","label":"金額","type":"amount"}
    ]));
    let run = default_run(&snapshot, &form, Rc::new(move |_, _| (0.9, cell)));
    assert_eq!(run.before.fields["items"].status, FillStatus::Filled);
    let out = done(&run);
    assert_eq!(image_questions(&run), [["i0_box", "i0_present"]]);
    assert_eq!(out.fill.fields["items"], run.before.fields["items"]);
    assert_unchanged(&out.fill.fields["price"], &run.before.fields["price"]);
    assert_eq!(out.image_assist["price"].outcome, ImageOutcome::FilledField);
}

#[test]
fn a_value_delimiter_bound_to_another_label_is_refused() {
    // `Ref:` binds `AB12` by its delimiter, so offline fill fills `Ref`;
    // `Code` (hint `Refs`, a partial label) stays ambiguous. The value
    // belongs to `Ref`, a label `Code` only partly matches.
    let snapshot = snapshot(&[&[(72.0, 700.0, "Ref:"), (130.0, 700.0, "AB12")]]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let form = form(json!([
        {"id":"ref","label":"Ref"},
        {"id":"code","label":"Code","hints":["Refs"]}
    ]));
    let run = default_run(&snapshot, &form, only("i0", value));
    assert_eq!(run.before.fields["ref"].status, FillStatus::Filled);
    assert_eq!(run.before.fields["code"].status, FillStatus::Ambiguous);
    let out = done(&run);
    assert_eq!(out.assist["code"].outcome, AssistOutcome::NotUniqueMaximum);
    assert_unchanged(&out.fill.fields["code"], &run.before.fields["code"]);
    assert_eq!(out.image_assist["code"].outcome, ImageOutcome::OtherLabel);
}

#[test]
fn the_evidence_of_a_filled_field_is_refused() {
    // `Order` labels `AB12` by position only: both fields are ambiguous
    // offline (`Order No` matches `Order` partly). The choice step fills
    // `order`; `ono` stays tied and its box over the same run is refused:
    // `Order` is not its label, only a partial match (a filled table cell
    // reused as Evidence is `filled_field`, see the table field test).
    let snapshot = snapshot(&[ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let form = form(json!([
        {"id":"order","label":"Order"},
        {"id":"ono","label":"Order No"}
    ]));
    let choice = json!({"answers":{
        "q0":{"type":"choice","choice":"c0","probabilities":{"c0":1.0,"none":0.0},"confidence":0.9},
        "q1":{"type":"choice","choice":"c0","probabilities":{"c0":0.5,"none":0.5},"confidence":0.5}
    }});
    let run = run(
        &snapshot,
        &form,
        only("i0", value),
        vec![Script::Extra(choice)],
        ExecutionConfig::default(),
    );
    for id in ["order", "ono"] {
        assert_eq!(run.before.fields[id].status, FillStatus::Ambiguous, "{id}");
    }
    let out = done(&run);
    assert_eq!(out.assist["order"].outcome, AssistOutcome::Selected);
    assert_eq!(out.assist["ono"].outcome, AssistOutcome::NotUniqueMaximum);
    assert_eq!(out.fill.fields["order"].raw.as_deref(), Some("AB12"));
    assert_unchanged(&out.fill.fields["ono"], &run.before.fields["ono"]);
    assert_eq!(
        out.image_assist["ono"].outcome,
        ImageOutcome::UnmatchedLabel
    );
}

#[test]
fn a_document_label_run_is_not_a_value() {
    // E5: `Order` is the document's label of `AB12`, not a form label.
    let snapshot = snapshot(&[ORDER]);
    let label = normalized(fragment(&snapshot, 1, "Order").bbox);
    let run = default_run(&snapshot, &reference_form("text"), only("i0", label));
    let out = done(&run);
    assert_unchanged(&out.fill.fields["ref"], &run.before.fields["ref"]);
    assert_eq!(out.image_assist["ref"].outcome, ImageOutcome::DocumentLabel);
    // A table header too.
    let snapshot = table();
    let header = area([298.0, 98.0, 324.0, 113.0]);
    let form = form(json!([{"id":"total","label":"合計"}]));
    let run = default_run(&snapshot, &form, only("i0", header));
    let out = done(&run);
    assert_eq!(
        out.image_assist["total"].outcome,
        ImageOutcome::DocumentLabel
    );
}

#[test]
fn a_form_label_counts_only_as_a_token() {
    // E6: `No.` is not in `Nomura`.
    let snapshot = snapshot(&[&[(72.0, 700.0, "Agent"), (200.0, 700.0, "Nomura")]]);
    let value = normalized(fragment(&snapshot, 1, "Nomura").bbox);
    let form = form(json!([{"id":"name","label":"Name"},{"id":"no","label":"No."}]));
    let run = default_run(&snapshot, &form, only("i0", value));
    let out = done(&run);
    assert_eq!(out.fill.fields["name"].raw.as_deref(), Some("Nomura"));
    assert_eq!(out.image_assist["name"].outcome, ImageOutcome::Selected);
    // `No. 12` holds it.
    let snapshot = self::snapshot(&[&[(72.0, 700.0, "Agent"), (200.0, 700.0, "No. 12")]]);
    let value = normalized(fragment(&snapshot, 1, "No. 12").bbox);
    let run = default_run(&snapshot, &form, only("i0", value));
    let out = done(&run);
    assert_eq!(out.image_assist["name"].outcome, ImageOutcome::LabelInValue);
}

#[test]
fn values_offline_fill_would_not_commit_are_refused() {
    // A unit note anywhere scales every amount.
    let snapshot = manual(&[
        ("note", "（単位：千円）", [60.0, 80.0, 137.0, 91.0]),
        ("v", "12,000", [300.0, 119.0, 333.0, 130.0]),
    ]);
    let form = form(json!([{"id":"total","label":"合計","type":"amount"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([298.0, 117.0, 335.0, 132.0])),
    );
    let out = done(&run);
    assert_unchanged(&out.fill.fields["total"], &run.before.fields["total"]);
    assert_eq!(out.image_assist["total"].outcome, ImageOutcome::ScaledValue);
    // Two phone numbers in one text value.
    let snapshot = manual(&[(
        "v",
        "03-1234-5678 03-9999-0000",
        [300.0, 119.0, 438.0, 130.0],
    )]);
    let form = self::form(json!([{"id":"tel","label":"電話"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([298.0, 117.0, 440.0, 132.0])),
    );
    let out = done(&run);
    assert_unchanged(&out.fill.fields["tel"], &run.before.fields["tel"]);
    assert_eq!(out.image_assist["tel"].outcome, ImageOutcome::SeveralValues);
}

#[test]
fn a_field_whose_candidates_jev_rejected_is_not_asked_again() {
    // E7: the choice step answers `none`.
    let snapshot = snapshot(&[&[(72.0, 700.0, "Ref: AB12"), (72.0, 660.0, "Ref: CD34")]]);
    let line = normalized(fragment(&snapshot, 1, "Ref: AB12").bbox);
    let form = form(json!([{"id":"ref","label":"Ref"}]));
    let run = run(
        &snapshot,
        &form,
        Rc::new(move |_, _| (0.9, line)),
        vec![Script::ChooseNone],
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_eq!(out.assist["ref"].outcome, AssistOutcome::NoneSelected);
    let field = &out.fill.fields["ref"];
    assert_eq!(field.status, FillStatus::NotFound);
    assert!(field.raw.is_none() && field.located.is_none());
    assert!(out.image_assist.is_empty());
    assert_eq!(run.sent.len(), 1, "only the choice request");
    assert!(run.rendered.is_empty());
}

#[test]
fn decided_fields_are_not_asked_again_and_their_pages_not_rendered() {
    // E8: page 1 cannot be rendered, so the field cannot be filled.
    let four: Vec<&[(f64, f64, &str)]> = vec![ORDER; 4];
    let snapshot = snapshot(&four);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let run = run_with(
        &snapshot,
        &reference_form("text"),
        &options(ExecutionConfig::default()),
        &ImageOptions::default(),
        Pages {
            failing: vec![1],
            ..Default::default()
        },
        Rc::new(move |_, _| (0.1, value)),
        Vec::new(),
    );
    let out = done(&run);
    assert_eq!(run.rendered, [1]);
    assert!(run.sent.is_empty() && out.jev_responses.is_empty());
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::RenderFailed);
    assert_eq!(record.pages.len(), 1);

    // Present on two pages decides the field; the third page is skipped.
    let three: Vec<&[(f64, f64, &str)]> = vec![ORDER; 3];
    let snapshot = self::snapshot(&three);
    let run = default_run(
        &snapshot,
        &reference_form("text"),
        Rc::new(move |_, _| (0.9, value)),
    );
    let out = done(&run);
    assert_eq!(run.rendered, [1, 2]);
    assert_eq!(run.sent.len(), 2);
    let record = &out.image_assist["ref"];
    assert_eq!(record.outcome, ImageOutcome::PresentOnMultiplePages);
    assert_eq!(record.pages.len(), 2);

    // A decided field leaves later batches; the other field is still asked.
    let form = form(json!([{"id":"ref","label":"Reference"},{"id":"other","label":"Other"}]));
    let run = self::run(
        &snapshot,
        &form,
        // `Reference` is present on pages 1 and 2, `Other` nowhere.
        Rc::new(move |page, f| {
            if f == "i0" && page <= 2 {
                (0.9, value)
            } else {
                (0.1, value)
            }
        }),
        Vec::new(),
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_eq!(run.rendered, [1, 2, 3]);
    let asked: Vec<Vec<String>> = run
        .sent
        .iter()
        .map(|s| {
            s.request["questions"]
                .as_object()
                .expect("questions")
                .keys()
                .cloned()
                .collect()
        })
        .collect();
    assert_eq!(asked[0], ["i0_box", "i0_present", "i1_box", "i1_present"]);
    assert_eq!(asked[2], ["i1_box", "i1_present"]);
    assert_eq!(
        out.image_assist["ref"].outcome,
        ImageOutcome::PresentOnMultiplePages
    );
    assert_eq!(out.image_assist["other"].pages.len(), 3);
}

/// The only test in this binary that installs a logger.
#[test]
fn logs_never_contain_images_bodies_labels_or_credentials() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    struct Capture {
        bytes: Arc<Mutex<Vec<u8>>>,
        owner: std::thread::ThreadId,
    }
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            if std::thread::current().id() == self.owner {
                let mut bytes = self.bytes.lock().expect("capture lock");
                let _ = writeln!(bytes, "{} {}", record.target(), record.args());
            }
        }
        fn flush(&self) {}
    }
    let captured = Arc::new(Mutex::new(Vec::new()));
    log::set_boxed_logger(Box::new(Capture {
        bytes: captured.clone(),
        owner: std::thread::current().id(),
    }))
    .expect("this test binary installs one logger");
    log::set_max_level(log::LevelFilter::Debug);

    let snapshot = snapshot(&[ORDER]);
    let value = normalized(fragment(&snapshot, 1, "AB12").bbox);
    let form = form(json!([{"id":"ref","label":"LabelCanary","hints":["HintCanary"]}]));
    let run = run(
        &snapshot,
        &form,
        Rc::new(move |_, _| (0.9, value)),
        vec![
            Script::Status(503, b"RETRY_BODY_CANARY"),
            Script::Extra(json!({"note":"RESPONSE_BODY_CANARY"})),
        ],
        ExecutionConfig::default(),
    );
    let out = done(&run);
    assert_eq!(out.fill.fields["ref"].raw.as_deref(), Some("AB12"));
    assert_eq!(out.jev_responses.len(), 2);
    assert!(body_text(out.jev_responses[1].body.as_ref()).contains("RESPONSE_BODY_CANARY"));
    let logs =
        String::from_utf8(captured.lock().expect("capture lock").clone()).expect("logs are UTF-8");
    assert!(logs.contains("vf_client") && logs.contains("\"image_count\":1"));
    for canary in [
        SECRET,
        PNG_CANARY,
        "<voltfill-image-",
        "RETRY_BODY_CANARY",
        "RESPONSE_BODY_CANARY",
        "LabelCanary",
        "HintCanary",
        "AB12",
    ] {
        assert!(!logs.contains(canary), "{canary}");
    }
}

#[cfg(feature = "hayro")]
mod rendered {
    use super::*;
    use vf_core::images::{DocumentRenderer, RenderConfig};

    fn config(pixels_per_point: f64) -> RenderConfig {
        let mut config = RenderConfig::hayro();
        config.pixels_per_point = pixels_per_point;
        config
    }

    #[test]
    fn a_rendered_page_maps_the_answered_box_back_to_its_source_text() {
        let bytes = support::pdf_with_pages(&[ORDER]);
        let snapshot = inspect(&bytes, PdfLimits::default()).expect("inspects");
        let value = fragment(&snapshot, 1, "AB12").bbox;
        let config = config(1.5);
        // The answer a model would give for the value on this exact image.
        let mut probe = DocumentRenderer::open(&bytes, &config).expect("hayro opens the PDF");
        let image = probe.render(1).expect("the page renders");
        assert!(image.png.starts_with(b"\x89PNG"));
        let (width, height) = image.transform.sent_dimensions().expect("valid ledger");
        assert_eq!(probe.planned_size(1), Some((width, height)));
        assert_eq!(probe.page_count(), Some(1));
        let raster = image
            .transform
            .display_to_raster
            .map_rect(value)
            .expect("the value maps to pixels");
        let target = [
            raster.left / f64::from(width),
            raster.top / f64::from(height),
            raster.right / f64::from(width),
            raster.bottom / f64::from(height),
        ];
        let form = reference_form("text");
        let before = fill::fill(&snapshot.document_id, &snapshot.fragments, &form);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let secret = Secret::try_new(SECRET.into()).expect("synthetic bearer");
        let out = images::assist_with_images(
            before,
            &form,
            &options(ExecutionConfig::default()),
            ImageInputs {
                snapshot: &snapshot,
                renderer: &mut DocumentRenderer::open(&bytes, &config).expect("opens"),
                options: &ImageOptions::default(),
            },
            &secret,
            &mut Wire {
                sent: sent.clone(),
                locate: Rc::new(move |_, _| (0.9, target)),
                scripts: VecDeque::new(),
            },
            &mut Time::default(),
        )
        .expect("valid inputs");
        let field = &out.fill.fields["ref"];
        assert_eq!(field.status, FillStatus::Filled);
        assert_eq!(field.method, Some(images::METHOD));
        assert_eq!(field.raw.as_deref(), Some("AB12"));
        let sent = sent.borrow();
        assert_eq!(sent.len(), 1);
        let body = &sent[0].body;
        // The rendered PNG itself is the image part.
        assert!(
            body.windows(image.png.len())
                .any(|w| w == image.png.as_slice())
        );
    }

    #[test]
    fn a_scale_that_makes_images_too_large_is_refused_before_sending() {
        let bytes = support::pdf_with_pages(&[ORDER]);
        let snapshot = inspect(&bytes, PdfLimits::default()).expect("inspects");
        let form = reference_form("text");
        let before = fill::fill(&snapshot.document_id, &snapshot.fragments, &form);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let secret = Secret::try_new(SECRET.into()).expect("synthetic bearer");
        // Letter at 2 pixels per point is 1224 x 1584, above 1568.
        let result = images::assist_with_images(
            before,
            &form,
            &options(ExecutionConfig::default()),
            ImageInputs {
                snapshot: &snapshot,
                renderer: &mut DocumentRenderer::open(&bytes, &config(2.0)).expect("opens"),
                options: &ImageOptions::default(),
            },
            &secret,
            &mut Wire {
                sent: sent.clone(),
                locate: Rc::new(|_, _| (0.9, [0.1, 0.1, 0.2, 0.2])),
                scripts: VecDeque::new(),
            },
            &mut Time::default(),
        );
        assert_eq!(result.err(), Some(ImageError::ImageSize));
        assert!(sent.borrow().is_empty());
    }
}

/// The field kept its offline result and the image step refused with `outcome`.
fn refused(run: &Run, id: &str, outcome: ImageOutcome) {
    let out = done(run);
    assert_unchanged(&out.fill.fields[id], &run.before.fields[id]);
    assert_eq!(out.image_assist[id].outcome, outcome);
}

#[test]
fn a_value_split_over_runs_is_refused() {
    // `〒100-0001` and the rest of the address are separate runs after
    // `住所：`: offline holds `split_value`.
    let snapshot = manual(&[
        ("l", "住所：", [60.0, 119.0, 93.0, 130.0]),
        ("v1", "〒100-0001", [100.0, 119.0, 155.0, 130.0]),
        (
            "v2",
            "東京都千代田区千代田1-1",
            [160.0, 119.0, 286.5, 130.0],
        ),
    ]);
    let form = form(json!([{"id":"addr","label":"住所"}]));
    for (target, outcome) in [
        // The first run, alone or with its label.
        ([98.0, 117.0, 157.0, 132.0], ImageOutcome::SplitValue),
        ([58.0, 117.0, 157.0, 132.0], ImageOutcome::SplitValue),
        // Both runs.
        ([98.0, 117.0, 288.0, 132.0], ImageOutcome::MultipleValues),
        // The second run is paired only with `〒100-0001`, not with `住所`.
        ([158.0, 117.0, 288.0, 132.0], ImageOutcome::UnmatchedLabel),
    ] {
        let run = default_run(&snapshot, &form, only("i0", area(target)));
        assert_eq!(run.before.fields["addr"].status, FillStatus::Ambiguous);
        refused(&run, "addr", outcome);
    }
    // A name split over runs: `山田` `太郎`.
    let snapshot = manual(&[
        ("l", "氏名：", [60.0, 119.0, 93.0, 130.0]),
        ("v1", "山田", [100.0, 119.0, 122.0, 130.0]),
        ("v2", "太郎", [127.0, 119.0, 149.0, 130.0]),
    ]);
    let form = self::form(json!([{"id":"name","label":"氏名"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([98.0, 117.0, 124.0, 132.0])),
    );
    refused(&run, "name", ImageOutcome::SplitValue);
}

#[test]
fn one_line_of_a_multi_line_value_is_refused() {
    // An inline value with a continuation line (offline `multi_line`).
    let snapshot = manual(&[
        ("a", "住所：東京都千代田区", [60.0, 119.0, 170.0, 130.0]),
        ("b", "千代田1-1 ビル5F", [93.0, 137.0, 181.0, 148.0]),
    ]);
    let form = form(json!([{"id":"addr","label":"住所"}]));
    for (target, outcome) in [
        ([92.0, 117.0, 172.0, 132.0], ImageOutcome::MultiLine),
        ([92.0, 135.0, 183.0, 150.0], ImageOutcome::MultiLine),
        ([92.0, 117.0, 183.0, 150.0], ImageOutcome::MultipleValues),
    ] {
        let run = default_run(&snapshot, &form, only("i0", area(target)));
        assert_eq!(run.before.fields["addr"].status, FillStatus::Ambiguous);
        refused(&run, "addr", outcome);
    }
    // When the lines are closer, a box over one line also reaches into the
    // other: refused either way.
    let snapshot = manual(&[
        ("a", "住所：東京都千代田区", [60.0, 119.0, 170.0, 130.0]),
        ("b", "千代田1-1 ビル5F", [93.0, 133.0, 181.0, 144.0]),
    ]);
    for target in [[92.0, 117.0, 172.0, 132.0], [92.0, 117.0, 183.0, 146.0]] {
        let run = default_run(&snapshot, &form, only("i0", area(target)));
        let out = done(&run);
        assert_unchanged(&out.fill.fields["addr"], &run.before.fields["addr"]);
    }
    // A value run after `住所：` with a continuation run below it.
    let snapshot = manual(&[
        ("l", "住所：", [60.0, 119.0, 93.0, 130.0]),
        ("v1", "東京都千代田区", [100.0, 119.0, 177.0, 130.0]),
        ("v2", "千代田1-1", [100.0, 137.0, 149.5, 148.0]),
    ]);
    for target in [
        [98.0, 117.0, 179.0, 132.0],
        [98.0, 135.0, 179.0, 150.0],
        [58.0, 117.0, 179.0, 132.0],
    ] {
        let run = default_run(&snapshot, &form, only("i0", area(target)));
        assert_eq!(run.before.fields["addr"].status, FillStatus::Ambiguous);
        refused(&run, "addr", ImageOutcome::MultiLine);
    }
}

#[test]
fn a_value_bound_to_another_label_by_a_delimiter_across_runs_is_refused() {
    // `電話：` binds the next run by its delimiter, as `電話：03-…` does in
    // one run: the value belongs to `電話`, whatever field the box is for.
    let snapshot = manual(&[
        ("l", "電話：", [60.0, 119.0, 93.0, 130.0]),
        ("v", "03-1234-5678", [100.0, 119.0, 166.0, 130.0]),
    ]);
    let value = area([98.0, 117.0, 168.0, 132.0]);
    for label in ["FAX", "氏名"] {
        let form = form(json!([{"id":"f","label":label}]));
        let run = default_run(&snapshot, &form, only("i0", value));
        assert_eq!(run.before.fields["f"].status, FillStatus::NotFound);
        refused(&run, "f", ImageOutcome::OtherLabel);
    }
    let snapshot = manual(&[("t", "電話：03-1234-5678", [60.0, 119.0, 161.4, 130.0])]);
    let form = self::form(json!([{"id":"f","label":"FAX"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([92.0, 117.0, 163.0, 132.0])),
    );
    refused(&run, "f", ImageOutcome::OtherLabel);
    // A whitespace-only split (`FAX 03-…`, FAX being a form label) is
    // refused for another field too: its run starts with a form label.
    let snapshot = manual(&[("t", "FAX 03-1234-5678", [60.0, 119.0, 170.0, 130.0])]);
    let form = self::form(json!([{"id":"tel","label":"電話"},{"id":"fax","label":"FAX"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([58.0, 117.0, 172.0, 132.0])),
    );
    assert_eq!(run.before.fields["fax"].status, FillStatus::Ambiguous);
    let out = done(&run);
    assert_unchanged(&out.fill.fields["tel"], &run.before.fields["tel"]);
    assert_eq!(out.image_assist["tel"].outcome, ImageOutcome::LabelInValue);
}

#[test]
fn a_field_with_offline_candidates_takes_only_a_value_paired_with_its_label() {
    // `合計` is ambiguous offline (two inline values); the box points at
    // the value of `小計`.
    let snapshot = manual(&[
        ("a", "合計：1,000", [60.0, 119.0, 115.0, 130.0]),
        ("b", "合計：2,000", [60.0, 139.0, 115.0, 150.0]),
        ("c", "小計", [60.0, 159.0, 82.0, 170.0]),
        ("d", "¥3,000", [200.0, 159.0, 233.0, 170.0]),
    ]);
    let form = form(json!([{"id":"total","label":"合計","type":"amount"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([198.0, 157.0, 235.0, 172.0])),
    );
    assert_eq!(run.before.fields["total"].status, FillStatus::Ambiguous);
    refused(&run, "total", ImageOutcome::UnmatchedLabel);
    // A value below its own label (position only) is accepted.
    let snapshot = manual(&[
        ("l", "住所：", [60.0, 119.0, 93.0, 130.0]),
        ("v", "東京都千代田区", [60.0, 133.0, 137.0, 144.0]),
    ]);
    let form = self::form(json!([{"id":"addr","label":"住所"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([58.0, 117.0, 139.0, 146.0])),
    );
    assert_eq!(run.before.fields["addr"].status, FillStatus::Ambiguous);
    let out = done(&run);
    assert_eq!(
        out.fill.fields["addr"].raw.as_deref(),
        Some("東京都千代田区")
    );
    assert_eq!(out.image_assist["addr"].outcome, ImageOutcome::Selected);
}

#[test]
fn table_cells_belong_to_their_column_header() {
    // A cell of the `単価` column boxed for `合計`: followed by another cell
    // on its row, it is refused as a possibly split value first.
    let snapshot = manual(&[
        ("h1", "品名", [60.0, 100.0, 82.0, 111.0]),
        ("h2", "単価", [200.0, 100.0, 222.0, 111.0]),
        ("h3", "金額", [300.0, 100.0, 322.0, 111.0]),
        ("c1", "部品A", [60.0, 120.0, 98.5, 131.0]),
        ("c2", "¥500", [200.0, 120.0, 222.0, 131.0]),
        ("c3", "¥1,000", [300.0, 120.0, 333.0, 131.0]),
    ]);
    let form = form(json!([{"id":"total","label":"合計","type":"amount"}]));
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([198.0, 118.0, 224.0, 133.0])),
    );
    refused(&run, "total", ImageOutcome::SplitValue);
    // A stacked list: the first line is its header, later lines its cells.
    let snapshot = manual(&[
        ("a", "山田太郎", [60.0, 119.0, 104.0, 130.0]),
        ("b", "佐藤花子", [60.0, 139.0, 104.0, 150.0]),
        ("c", "鈴木一郎", [60.0, 159.0, 104.0, 170.0]),
    ]);
    let form = self::form(json!([{"id":"name","label":"担当者"}]));
    for (top, outcome) in [
        (119.0, ImageOutcome::DocumentLabel),
        (139.0, ImageOutcome::UnmatchedLabel),
        (159.0, ImageOutcome::UnmatchedLabel),
    ] {
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([58.0, top - 2.0, 106.0, top + 13.0])),
        );
        refused(&run, "name", outcome);
    }
}

#[test]
fn a_value_that_only_layout_position_ties_to_an_unknown_label_is_the_residual_risk() {
    // Neither `電話番号` nor a lone run is tied to any form field, and
    // `FAX` has no offline candidate: the box is trusted (documented).
    let snapshot = manual(&[
        ("l", "電話番号", [60.0, 119.0, 104.0, 130.0]),
        ("v", "03-1234-5678", [120.0, 119.0, 186.0, 130.0]),
    ]);
    let form = form(json!([{"id":"fax","label":"FAX"}]));
    let value = area([118.0, 117.0, 188.0, 132.0]);
    let run = default_run(&snapshot, &form, only("i0", value));
    assert_eq!(run.before.fields["fax"].status, FillStatus::NotFound);
    let out = done(&run);
    assert_eq!(out.fill.fields["fax"].raw.as_deref(), Some("03-1234-5678"));
    let snapshot = manual(&[("v", "03-1234-5678", [120.0, 119.0, 186.0, 130.0])]);
    let run = default_run(&snapshot, &form, only("i0", value));
    let out = done(&run);
    assert_eq!(out.fill.fields["fax"].raw.as_deref(), Some("03-1234-5678"));
}

#[test]
fn turned_text_is_never_selected_and_blocks_a_box_it_touches() {
    // A page whose only text is turned is handled like a scanned page.
    let mut snapshot = manual(&[("t", "請求番号INV-0012", [300.0, 100.0, 311.0, 200.0])]);
    snapshot.fragments[0].orientation = Orientation::Deg90;
    let form = form(json!([{"id":"ref","label":"Reference"}]));
    let target = area([298.0, 98.0, 313.0, 202.0]);
    let run = default_run(&snapshot, &form, only("i0", target));
    let out = done(&run);
    let field = &out.fill.fields["ref"];
    assert_eq!(field.status, run.before.fields["ref"].status);
    assert!(field.raw.is_none() && field.method.is_none());
    assert!(field.located.is_some());
    assert_eq!(out.image_assist["ref"].outcome, ImageOutcome::Located);
    // With upright text on the page, a box touching turned text is refused
    // even when it also covers an upright value.
    let mut snapshot = manual(&[
        ("t", "INV-0099", [300.0, 100.0, 311.0, 160.0]),
        ("v", "AB12", [320.0, 140.0, 345.0, 151.0]),
    ]);
    snapshot.fragments[0].orientation = Orientation::Deg90;
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([309.0, 138.0, 347.0, 153.0])),
    );
    refused(&run, "ref", ImageOutcome::TurnedText);
    // Away from it, the upright value is selected.
    let run = default_run(
        &snapshot,
        &form,
        only("i0", area([318.0, 138.0, 347.0, 153.0])),
    );
    let out = done(&run);
    assert_eq!(out.fill.fields["ref"].raw.as_deref(), Some("AB12"));
}

#[test]
fn a_passed_image_deadline_stops_before_anything_is_rendered_or_sent() {
    let snapshot = snapshot(&[ORDER]);
    let past = std::time::Instant::now();
    let options = ImageOptions {
        limits: fill::FillLimits {
            deadline: Some(past),
            ..fill::FillLimits::default()
        },
        ..ImageOptions::default()
    };
    let run = run_with(
        &snapshot,
        &reference_form("text"),
        &self::options(ExecutionConfig::default()),
        &options,
        Pages::default(),
        Rc::new(|_, _| (0.9, [0.0, 0.0, 1.0, 1.0])),
        Vec::new(),
    );
    assert_eq!(
        run.result.err(),
        Some(ImageError::Fill(fill::FillError::DeadlineExceeded))
    );
    assert!(run.sent.is_empty() && run.rendered.is_empty());
}

#[test]
fn a_scale_unit_in_any_label_over_the_value_is_refused() {
    let value = area([118.0, 117.0, 158.0, 132.0]);
    // A position-only pair: `売上高（千円）` left of `1,234`. Offline fill
    // commits none of these fields (`収益` has no candidate at all), and no
    // field may take the unscaled number.
    let snapshot = manual(&[
        ("l", "売上高（千円）", [60.0, 119.0, 104.0, 130.0]),
        ("v", "1,234", [120.0, 119.0, 147.5, 130.0]),
    ]);
    for label in ["売上高", "売上高（千円）", "収益"] {
        let form = self::form(json!([{"id":"f","label":label,"type":"amount"}]));
        let run = default_run(&snapshot, &form, only("i0", value));
        assert_ne!(run.before.fields["f"].status, FillStatus::Filled, "{label}");
        if label == "収益" {
            assert!(run.before.fields["f"].candidates.is_empty());
        }
        refused(&run, "f", ImageOutcome::ScaledValue);
    }
    // Number fields are scaled alike.
    let form = self::form(json!([{"id":"f","label":"収益","type":"number"}]));
    let run = default_run(&snapshot, &form, only("i0", value));
    refused(&run, "f", ImageOutcome::ScaledValue);
    // A table column header states the scale for its cells.
    let snapshot = manual(&[
        ("h1", "品名", [60.0, 100.0, 82.0, 111.0]),
        ("h2", "金額（千円）", [300.0, 100.0, 355.0, 111.0]),
        ("c1", "部品A", [60.0, 120.0, 98.5, 131.0]),
        ("c2", "1,000", [300.0, 120.0, 327.5, 131.0]),
    ]);
    let cell = area([298.0, 118.0, 330.0, 133.0]);
    for label in ["金額", "金額（千円）"] {
        let form = self::form(json!([{"id":"f","label":label,"type":"amount"}]));
        let run = default_run(&snapshot, &form, only("i0", cell));
        refused(&run, "f", ImageOutcome::ScaledValue);
    }
    // Delimiter pairs across runs, twice: `売上高（千円）：` then `1,234`.
    let snapshot = manual(&[
        ("l1", "売上高（千円）：", [60.0, 119.0, 115.0, 130.0]),
        ("v1", "1,234", [120.0, 119.0, 147.5, 130.0]),
        ("l2", "売上高（千円）：", [60.0, 139.0, 115.0, 150.0]),
        ("v2", "5,678", [120.0, 139.0, 147.5, 150.0]),
    ]);
    for label in ["売上高", "売上高（千円）", "収益"] {
        let form = self::form(json!([{"id":"f","label":label,"type":"amount"}]));
        let run = default_run(&snapshot, &form, only("i0", value));
        if label == "売上高（千円）" {
            // Offline already rejects its own scaled values: not asked.
            assert_eq!(run.before.fields["f"].status, FillStatus::Invalid);
            assert!(run.sent.is_empty() && done(&run).image_assist.is_empty());
            assert_eq!(done(&run).fill.fields["f"], run.before.fields["f"]);
            continue;
        }
        refused(&run, "f", ImageOutcome::ScaledValue);
    }
    // In one run, `売上高（千円）：1,234`, the run's label scales it too.
    let snapshot = manual(&[("t", "売上高（千円）：1,234", [60.0, 119.0, 177.0, 130.0])]);
    for label in ["売上高", "収益"] {
        let form = self::form(json!([{"id":"f","label":label,"type":"amount"}]));
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([147.0, 117.0, 179.0, 132.0])),
        );
        refused(&run, "f", ImageOutcome::ScaledValue);
    }
}

#[test]
fn a_partial_or_one_sided_label_is_not_the_fields_label() {
    // `金額（税抜）` and `金額（税込）` both match `金額` one-sidedly: offline
    // lists them, ambiguous. A box over either value is not `金額`'s.
    let snapshot = manual(&[
        ("l1", "金額（税抜）", [60.0, 119.0, 115.0, 130.0]),
        ("v1", "¥1,000", [200.0, 119.0, 233.0, 130.0]),
        ("l2", "金額（税込）", [60.0, 139.0, 115.0, 150.0]),
        ("v2", "¥1,100", [200.0, 139.0, 233.0, 150.0]),
    ]);
    let form = self::form(json!([{"id":"f","label":"金額","type":"amount"}]));
    for top in [119.0, 139.0] {
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([198.0, top - 2.0, 235.0, top + 13.0])),
        );
        assert_eq!(run.before.fields["f"].status, FillStatus::Ambiguous);
        refused(&run, "f", ImageOutcome::UnmatchedLabel);
    }
    // `電話番号` and `請求番号` both match `番号` partly.
    let snapshot = manual(&[
        ("l1", "電話番号", [60.0, 119.0, 104.0, 130.0]),
        ("v1", "03-1234-5678", [200.0, 119.0, 266.0, 130.0]),
        ("l2", "請求番号", [60.0, 139.0, 104.0, 150.0]),
        ("v2", "INV-0012", [200.0, 139.0, 244.0, 150.0]),
    ]);
    let form = self::form(json!([{"id":"f","label":"番号"}]));
    for (top, right) in [(119.0, 268.0), (139.0, 246.0)] {
        let run = default_run(
            &snapshot,
            &form,
            only("i0", area([198.0, top - 2.0, right, top + 13.0])),
        );
        assert_eq!(run.before.fields["f"].status, FillStatus::Ambiguous);
        refused(&run, "f", ImageOutcome::UnmatchedLabel);
    }
}

#[test]
fn the_deadline_stops_later_batches_without_sending() {
    // Three fields without offline candidates, one per request; the first
    // answer arrives after the 400 ms deadline.
    let snapshot = snapshot(&[ORDER]);
    let form = self::form(json!([
        {"id":"a","label":"Reference"},
        {"id":"b","label":"Customer"},
        {"id":"c","label":"Due"}
    ]));
    let execution = ExecutionConfig {
        max_batch_questions: 2,
        ..ExecutionConfig::default()
    };
    let slept = Rc::new(Cell::new(false));
    let locate: Locate = Rc::new(move |_, _| {
        if !slept.replace(true) {
            std::thread::sleep(std::time::Duration::from_millis(600));
        }
        (0.9, [0.0, 0.0, 1.0, 1.0])
    });
    let options = ImageOptions {
        limits: fill::FillLimits {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(400)),
            ..fill::FillLimits::default()
        },
        ..ImageOptions::default()
    };
    let run = run_with(
        &snapshot,
        &form,
        &self::options(execution),
        &options,
        Pages::default(),
        locate,
        Vec::new(),
    );
    assert_eq!(image_questions(&run), [["i0_box", "i0_present"]]);
    assert_eq!(run.sent.len(), 1);
    for id in ["a", "b", "c"] {
        assert_eq!(run.before.fields[id].status, FillStatus::NotFound, "{id}");
        refused(&run, id, ImageOutcome::DeadlineExceeded);
    }
}

/// A text AcroForm field with one visible widget at the Display box `rect`
/// on page 1.
fn acroform_field(
    name: &str,
    alternate: Option<&str>,
    value: &str,
    [left, top, right, bottom]: [f64; 4],
) -> vf_source::acroform::FormField {
    vf_source::acroform::FormField {
        name: name.into(),
        alternate_name: alternate.map(Into::into),
        kind: FieldKind::Text,
        value: Some(FieldValue::Text(value.into())),
        default_value: None,
        read_only: false,
        required: false,
        max_len: None,
        object: None,
        widgets: vec![Widget {
            object: None,
            page: Some(1),
            pdf_rect: None,
            rect: Some(Rect {
                left,
                top,
                right,
                bottom,
            }),
            hidden: false,
            on_state: None,
            issues: Vec::new(),
        }],
        issues: Vec::new(),
    }
}

/// Offline fill with the AcroForm `fields`, then the choice and image steps.
fn run_acroform(
    snapshot: &DocumentSnapshot,
    form: &Form,
    fields: Vec<vf_source::acroform::FormField>,
    locate: Locate,
) -> Run {
    let pdf = AcroForm {
        present: true,
        xfa: XfaPresence::Absent,
        fields,
        skipped: Vec::new(),
    };
    let before = acroform::fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &Ok(pdf),
        form,
        &FillLimits::default(),
    )
    .expect("the synthetic form fills");
    run_after(
        before,
        snapshot,
        form,
        (
            &options(ExecutionConfig::default()),
            &ImageOptions::default(),
            Pages::default(),
        ),
        locate,
        Vec::new(),
    )
}

/// A widget box with its flattened value printed inside.
const WIDGET: [f64; 4] = [300.0, 85.0, 450.0, 105.0];

#[test]
fn a_field_filled_by_acroform_is_not_asked_and_its_widget_text_is_filled_evidence() {
    // `氏名` is filled from its AcroForm field (the text inside the widget
    // reads as /V). `ref` matches nothing offline and is asked; its box over
    // the text printed in the filled widget is that field's value.
    let snapshot = manual(&[("v", "山田太郎", [310.0, 88.0, 360.0, 102.0])]);
    let form = form(json!([
        {"id":"name","label":"氏名"},
        {"id":"ref","label":"Reference"}
    ]));
    let run = run_acroform(
        &snapshot,
        &form,
        vec![acroform_field("氏名", None, "山田太郎", WIDGET)],
        only("i0", area([308.0, 86.0, 362.0, 104.0])),
    );
    let name = &run.before.fields["name"];
    assert_eq!(name.status, FillStatus::Filled);
    assert_eq!(name.method, Some(acroform::METHOD));
    assert_eq!(run.before.fields["ref"].status, FillStatus::NotFound);
    let out = done(&run);
    assert_eq!(image_questions(&run), [["i0_box", "i0_present"]]);
    assert_eq!(out.image_assist.keys().collect::<Vec<_>>(), ["ref"]);
    assert_eq!(out.fill.fields["name"], *name);
    refused(&run, "ref", ImageOutcome::FilledField);
}

#[test]
fn a_field_held_for_a_text_layer_conflict_is_not_asked() {
    // The text layer fills `佐藤花子`, the AcroForm field `山田太郎`: the
    // field is ambiguous without candidates (`text_layer_conflict`). The
    // image step does not override that hold, whatever the model would box.
    let snapshot = manual(&[("p", "氏名：佐藤花子", [72.0, 300.0, 180.0, 314.0])]);
    let value = area([110.0, 300.0, 180.0, 314.0]);
    let form = form(json!([{"id":"name","label":"氏名"}]));
    let run = run_acroform(
        &snapshot,
        &form,
        vec![acroform_field("氏名", None, "山田太郎", WIDGET)],
        Rc::new(move |_, _| (0.9, value)),
    );
    let name = &run.before.fields["name"];
    assert_eq!(name.status, FillStatus::Ambiguous);
    assert_eq!(name.acroform[0].reasons, [AcroFormHold::TextLayerConflict]);
    let out = done(&run);
    assert!(run.sent.is_empty() && run.rendered.is_empty());
    assert!(out.image_assist.is_empty() && out.assist.is_empty());
    assert_unchanged(&out.fill.fields["name"], name);
}

#[test]
fn fields_held_for_an_acroform_field_matching_several_items_are_not_asked() {
    // One AcroForm field's name matches `氏名` and its /TU `Reference`: it
    // fills neither (`field_matches_several_items`), and neither is asked.
    // `code` has no AcroForm match and is asked as usual.
    let snapshot = manual(&[("v", "山田太郎", [72.0, 300.0, 140.0, 314.0])]);
    let value = area([70.0, 298.0, 142.0, 316.0]);
    let form = form(json!([
        {"id":"name","label":"氏名"},
        {"id":"ref","label":"Reference"},
        {"id":"code","label":"Code"}
    ]));
    let run = run_acroform(
        &snapshot,
        &form,
        vec![acroform_field(
            "氏名",
            Some("Reference"),
            "山田太郎",
            WIDGET,
        )],
        Rc::new(move |_, _| (0.9, value)),
    );
    for id in ["name", "ref"] {
        let held = &run.before.fields[id];
        assert_eq!(held.status, FillStatus::NotFound, "{id}");
        assert!(
            held.acroform[0]
                .reasons
                .contains(&AcroFormHold::FieldMatchesSeveralItems),
            "{id}"
        );
    }
    let out = done(&run);
    assert_eq!(image_questions(&run), [["i0_box", "i0_present"]]);
    assert_eq!(out.image_assist.keys().collect::<Vec<_>>(), ["code"]);
    for id in ["name", "ref"] {
        assert_unchanged(&out.fill.fields[id], &run.before.fields[id]);
    }
}
