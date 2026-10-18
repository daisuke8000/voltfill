//! Best-effort extraction from a single bounded source snapshot. All acceptance
//! profiles here are evaluation policies, never calibrated production claims.
use crate::{
    candidates::CandidateIndex,
    evaluate::{self, EvaluationCall, EvaluationContext, Evaluator},
    geometry::Rect,
    jev::{self, Answer, Content, Question, RequestDraft, multipart::PngImage},
    model::{
        Candidate, Evidence, ExtractionReport, FieldResult, Quality, Status, Strategy, UsageSummary,
    },
    pdf::{self, PdfLimits},
    schema::{FieldDefinition, FormDefinition, Modality, OutputType, QuestionDefinition, Resolver},
    validate,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationProfile {
    pub min_confidence: f64,
    pub noul_true_min: f64,
    pub noul_false_max: f64,
}
impl EvaluationProfile {
    /// Deliberately synthetic thresholds for deterministic fixture exercises.
    /// They are not a validated Jev/PDF quality or confidence calibration.
    pub fn synthetic() -> Self {
        Self {
            min_confidence: 0.99,
            noul_true_min: 0.99,
            noul_false_max: 0.01,
        }
    }
    fn validate(self) -> bool {
        [self.min_confidence, self.noul_true_min, self.noul_false_max]
            .iter()
            .all(|n| n.is_finite() && (0.0..=1.0).contains(n))
            && self.noul_false_max < self.noul_true_min
    }
    fn presence(self, n: f64) -> Option<bool> {
        if n >= self.noul_true_min {
            Some(true)
        } else if n <= self.noul_false_max {
            Some(false)
        } else {
            None
        }
    }
}
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub model: String,
    pub strategy: Strategy,
    pub pdf_limits: PdfLimits,
    pub max_requests: u32,
    pub max_concurrency: usize,
    pub max_batch_questions: usize,
    pub total_deadline_ms: u64,
    pub evaluation_profile: Option<EvaluationProfile>,
    /// Snapshot applied by `with_execution_config`, retained for reproducibility.
    pub execution_config: Option<jev::transport::ExecutionConfig>,
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    pub render_config: Option<crate::render::RenderConfig>,
}
impl Default for ExtractOptions {
    fn default() -> Self {
        let execution = jev::transport::ExecutionConfig::default();
        Self {
            model: jev::MODEL.into(),
            strategy: Strategy::TextFirst,
            pdf_limits: PdfLimits::default(),
            max_requests: execution.max_requests,
            max_concurrency: execution.max_concurrency,
            max_batch_questions: execution.max_batch_questions,
            total_deadline_ms: execution.total_deadline_ms,
            evaluation_profile: None,
            execution_config: None,
            #[cfg(any(feature = "pdfium", feature = "hayro"))]
            render_config: None,
        }
    }
}
impl ExtractOptions {
    fn resolution_profile(&self) -> EvaluationProfile {
        self.evaluation_profile.unwrap_or(EvaluationProfile {
            min_confidence: 0.0,
            noul_true_min: 0.5,
            noul_false_max: 0.5_f64.next_down(),
        })
    }
    /// Apply the same validated execution configuration used by RemoteEvaluator.
    pub fn with_execution_config(
        mut self,
        config: &jev::transport::ExecutionConfig,
    ) -> Result<Self, EngineError> {
        config.validate().map_err(|_| EngineError::Config)?;
        self.execution_config = Some(config.clone());
        self.max_requests = config.max_requests;
        self.total_deadline_ms = config.total_deadline_ms;
        self.max_concurrency = config.max_concurrency;
        self.max_batch_questions = config.max_batch_questions;
        Ok(self)
    }
}
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid extraction schema")]
    Schema,
    #[error("invalid extraction configuration")]
    Config,
    #[error("{0}")]
    Pdf(#[from] pdf::PdfError),
    #[error("invalid source provenance")]
    Provenance,
    #[error("{source}")]
    WithResponses {
        source: Box<EngineError>,
        jev_responses: Vec<crate::model::JevResponse>,
    },
}
impl EngineError {
    pub fn jev_responses(&self) -> &[crate::model::JevResponse] {
        match self {
            Self::WithResponses { jev_responses, .. } => jev_responses,
            _ => &[],
        }
    }
}
struct PageResult {
    status: Status,
    value: Option<Value>,
    evidence: Vec<Evidence>,
    reason: &'static str,
    origin: &'static str,
}
impl PageResult {
    fn pending(status: Status, reason: &'static str) -> Self {
        Self {
            status,
            value: None,
            evidence: Vec::new(),
            reason,
            origin: "source_text",
        }
    }
}

pub fn extract(
    bytes: &[u8],
    form: &FormDefinition,
    options: &ExtractOptions,
    evaluator: &mut impl Evaluator,
) -> Result<ExtractionReport, EngineError> {
    extract_inner(bytes, form, options, evaluator).map_err(|source| {
        let jev_responses = evaluator.take_responses();
        if jev_responses.is_empty() {
            source
        } else {
            EngineError::WithResponses {
                source: Box::new(source),
                jev_responses,
            }
        }
    })
}

fn extract_inner(
    bytes: &[u8],
    form: &FormDefinition,
    options: &ExtractOptions,
    evaluator: &mut impl Evaluator,
) -> Result<ExtractionReport, EngineError> {
    let started = Instant::now();
    form.validate()
        .and_then(|_| form.validate_strategy(options.strategy))
        .map_err(|_| EngineError::Schema)?;
    if jev::validate_model(&options.model).is_err()
        || (options.strategy != Strategy::TextFirst && !jev::supports_images(&options.model))
        || !(1..=4).contains(&options.max_concurrency)
        || !(1..=255).contains(&options.max_batch_questions)
        || options.max_requests == 0
        || options.total_deadline_ms == 0
        || options.evaluation_profile.is_some_and(|p| !p.validate())
        || options
            .execution_config
            .as_ref()
            .is_some_and(|config| config.validate().is_err())
    {
        return Err(EngineError::Config);
    }
    let mut snapshot = pdf::inspect(bytes, options.pdf_limits)?;
    let candidate_start = Instant::now();
    let index = CandidateIndex::new(&snapshot.fragments).map_err(|_| EngineError::Provenance)?;
    snapshot.timings.candidates_ms = candidate_start.elapsed().as_secs_f64() * 1000.0;
    let mut evidence = Vec::new();
    let mut usage = UsageSummary::default();
    let evaluation_started = Instant::now();
    let outcomes = evaluate_waves(
        bytes,
        form,
        options,
        evaluator,
        &index,
        &snapshot.pages,
        &snapshot.geometries,
        &mut snapshot.timings,
        started,
        &mut usage,
    );
    snapshot.timings.evaluation_ms += (evaluation_started.elapsed().as_secs_f64() * 1000.0
        - snapshot.timings.resolve_ms
        - snapshot.timings.render_ms)
        .max(0.0);
    let resolve_started = Instant::now();
    let fields = form
        .fields
        .iter()
        .zip(outcomes)
        .map(|(field, outcomes)| combine(field, outcomes, &mut evidence))
        .collect();
    snapshot.timings.resolve_ms += resolve_started.elapsed().as_secs_f64() * 1000.0;
    snapshot.timings.requests = usage.attempts;
    let form_bytes = serde_json::to_vec(form).map_err(|_| EngineError::Schema)?;
    let evaluation_profile = options
        .evaluation_profile
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| EngineError::Config)?;
    let mut configuration = serde_json::json!({"model":options.model,"strategy":options.strategy,"pdf_limits":{
        "max_bytes":options.pdf_limits.max_bytes,"max_pages":options.pdf_limits.max_pages,"max_items_per_page":options.pdf_limits.max_items_per_page,"max_recovery_pages":options.pdf_limits.max_recovery_pages},
        "execution_config":options.execution_config,"max_requests":options.max_requests,"max_concurrency":options.max_concurrency,"max_batch_questions":options.max_batch_questions,"total_deadline_ms":options.total_deadline_ms,"evaluation_profile":evaluation_profile});
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    {
        configuration["render"]=options.render_config.as_ref().map(|config|serde_json::json!({"renderer":config.renderer_name(),"pixels_per_point":config.pixels_per_point,"max_pixels":config.max_pixels,
        "max_output_bytes":config.max_output_bytes,"max_input_bytes":config.max_input_bytes,"max_pages":config.max_pages})).unwrap_or(Value::Null);
    }
    #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
    {
        configuration["render"] = Value::Null;
    }
    let configuration_id =
        hash(&serde_json::to_vec(&configuration).map_err(|_| EngineError::Config)?);
    snapshot.timings.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut report = ExtractionReport {
        schema_version: 1,
        run_id: run_id(),
        configuration_id,
        evaluation_profile,
        evaluation_only: true,
        document_id: snapshot.document_id,
        form_id: hash(&form_bytes),
        model: options.model.clone(),
        strategy: options.strategy,
        fields,
        evidence,
        pages: snapshot.pages,
        timings: snapshot.timings,
        usage,
        evaluation_backend: evaluator.backend().into(),
        jev_responses: Vec::new(),
    };
    if !consistent(&report, &index) {
        return Err(EngineError::Provenance);
    }
    report.jev_responses = evaluator.take_responses();
    Ok(report)
}

fn wire_question(
    q: &QuestionDefinition,
    page: u32,
    index: &CandidateIndex<'_>,
    selected: &BTreeSet<String>,
) -> Result<Question, &'static str> {
    if let Some(source) = &q.candidate_source {
        if q.input.modality == Modality::Image {
            return Err("image_only_source_candidates_unsupported");
        }
        let candidates = index
            .scoped(&[page], source.max_candidates)
            .map_err(|_| "candidate_limit_exceeded")?;
        let mut criteria = BTreeMap::new();
        for candidate in candidates.into_iter().filter(|c| !selected.contains(&c.id)) {
            criteria.insert(
                candidate.id.clone(),
                Content::Text(
                    index
                        .text(candidate)
                        .map_err(|_| "candidate_provenance_invalid")?,
                ),
            );
        }
        criteria.insert("none".into(), Content::Text(source.none_option.clone()));
        Ok(Question::Choice {
            instructions: q.instructions.clone(),
            criteria,
        })
    } else {
        q.wire_question().map_err(|_| "question_contract_invalid")
    }
}
struct PreparedImage {
    png: [PngImage; 1],
    transform: crate::geometry::ImageTransform,
}
struct PageJob {
    field: usize,
    page: u32,
    answers: BTreeMap<String, Answer>,
    selected: BTreeSet<String>,
    notes: BTreeSet<&'static str>,
    outcome: Option<PageResult>,
}
struct PreparedBatch {
    request: jev::Request,
    page: u32,
    field: usize,
    cross_field: bool,
    image: bool,
    reverse: BTreeMap<String, (usize, String)>,
}

// Build at most one request at a time. Oversized groups are split by the caller;
// an invalid individual question affects its field/page, never unrelated jobs.
fn prepare_batch(
    members: &[(usize, usize)],
    jobs: &[PageJob],
    form: &FormDefinition,
    index: &CandidateIndex<'_>,
    images: &BTreeMap<u32, Result<PreparedImage, &'static str>>,
    namespaced: bool,
    options: &ExtractOptions,
) -> Result<PreparedBatch, &'static str> {
    let first = &jobs[members[0].0];
    let modality = form.fields[first.field].questions[members[0].1]
        .input
        .modality;
    let mut questions = BTreeMap::new();
    let mut reverse = BTreeMap::new();
    for &(job, question) in members {
        let field = &form.fields[jobs[job].field];
        let q = &field.questions[question];
        let id = if namespaced {
            format!("f{}q{question}", jobs[job].field)
        } else {
            q.id.clone()
        };
        questions.insert(
            id.clone(),
            wire_question(q, first.page, index, &jobs[job].selected)?,
        );
        reverse.insert(id, (job, q.id.clone()));
    }
    let mut state = BTreeMap::new();
    let mut assets = BTreeMap::new();
    if modality != Modality::Image {
        let source = index
            .candidates()
            .iter()
            .filter(|candidate| candidate.page == first.page)
            .map(|candidate| index.text(candidate).map(Content::Text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "candidate_provenance_invalid")?;
        state.insert("source_fragments".into(), Content::Array(source));
    }
    if modality != Modality::Text {
        let image = images
            .get(&first.page)
            .and_then(|image| image.as_ref().ok())
            .ok_or("image_binding_unavailable")?;
        let reference = format!("<voltfill-image-{}>", image.png[0].sha256());
        state.insert("image".into(), Content::Text(reference.clone()));
        assets.insert(
            reference,
            jev::Asset {
                part: "image".into(),
            },
        );
    }
    let default_config = Default::default();
    let config = options.execution_config.as_ref().unwrap_or(&default_config);
    let request = jev::build_request(
        RequestDraft {
            model: options.model.clone(),
            state: Content::Object(state),
            questions,
            assets,
        },
        config.json_limits(),
    )
    .map_err(|_| "question_contract_invalid")?;
    let estimate = jev::planning::estimate(&request, &config.context);
    if !estimate.fits(&config.context, config.input_tokens_per_attempt) {
        return Err("input_token_limit_exceeded");
    }
    Ok(PreparedBatch {
        request,
        page: first.page,
        field: first.field,
        cross_field: members
            .iter()
            .any(|(job, _)| jobs[*job].field != first.field),
        image: modality != Modality::Text,
        reverse,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_image(
    bytes: &[u8],
    page: u32,
    geometry: &crate::geometry::PageGeometry,
    options: &ExtractOptions,
    evaluator: &impl Evaluator,
    timings: &mut crate::model::Timings,
) -> Result<PreparedImage, &'static str> {
    let _ = (bytes, options, &timings);
    let image = if let Some(image) = evaluator.fixture_image(page) {
        if evaluator.backend() != "synthetic_fixture" {
            return Err("synthetic_image_backend_required");
        }
        image.clone()
    } else {
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        {
            let Some(config) = &options.render_config else {
                return Err("image_binding_unavailable");
            };
            timings.render_calls += 1;
            let image =
                crate::render::render(bytes, page, config).map_err(|_| "render_unavailable")?;
            crate::evaluate::EvaluationImage {
                png: image.png,
                width: image.width,
                height: image.height,
                transform: image.transform,
            }
        }
        #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
        {
            return Err("image_binding_unavailable");
        }
    };
    let bounds = geometry
        .display_bounds()
        .map_err(|_| "image_transform_invalid")?;
    if image.transform.display_bounds != bounds
        || image.transform.sent_dimensions().ok() != Some((image.width, image.height))
        || image.transform.validate().is_err()
    {
        return Err("image_transform_invalid");
    }
    let png = PngImage::new("image".into(), image.png).map_err(|_| "image_limits_or_encoding")?;
    let (width, height) = png.dimensions();
    if (width, height) != (image.width, image.height) {
        return Err("image_transform_invalid");
    }
    Ok(PreparedImage {
        png: [png],
        transform: image.transform,
    })
}

fn early_outcome(
    field: &FieldDefinition,
    answers: &BTreeMap<String, Answer>,
    options: &ExtractOptions,
) -> Option<PageResult> {
    options.evaluation_profile?;
    if let Resolver::TextInBox {
        presence_question_id,
        ..
    } = &field.resolver
        && let Some(Answer::Noul { noul }) = answers.get(presence_question_id)
        && options.resolution_profile().presence(*noul) == Some(false)
    {
        if options.strategy == Strategy::Hybrid && field.questions.iter().filter(|q| q.candidate_source.is_some())
                    .any(|q| matches!(answers.get(&q.id), Some(Answer::Choice { choice, .. }) if choice != "none")) {
                    return Some(PageResult::pending(Status::Ambiguous, "hybrid_presence_disagreement"));
                }
        return Some(PageResult::pending(
            Status::NotFound,
            "presence_observed_absent",
        ));
    }
    None
}

// One answer advances one ordinary wave; all transport budgets remain shared.
fn advance_selections(
    job: &mut PageJob,
    field: &FieldDefinition,
    index: &CandidateIndex<'_>,
    options: &ExtractOptions,
) {
    let Resolver::SelectedCandidates {
        question_id,
        max_selections,
    } = &field.resolver
    else {
        return;
    };
    let q = &field.questions[0];
    let Some(source) = q.candidate_source.as_ref() else {
        job.outcome = Some(PageResult::pending(
            Status::Error,
            "question_contract_invalid",
        ));
        return;
    };
    let candidates = match index.scoped(&[job.page], source.max_candidates) {
        Ok(candidates) => candidates,
        Err(_) => {
            job.outcome = Some(PageResult::pending(
                Status::Unsupported,
                "candidate_limit_exceeded",
            ));
            return;
        }
    };
    let answer = &job.answers[question_id];
    let Answer::Choice { choice, .. } = answer else {
        job.outcome = Some(PageResult::pending(
            Status::Error,
            "resolver_answer_type_invalid",
        ));
        return;
    };
    if choice != "none"
        && !candidates
            .iter()
            .any(|c| c.id == *choice && !job.selected.contains(&c.id))
    {
        job.outcome = Some(PageResult::pending(
            Status::Error,
            "candidate_reference_invalid",
        ));
    } else {
        if choice != "none" {
            job.selected.insert(choice.clone());
        }
        if !unique_choice(answer) {
            job.notes.insert("choice_not_unique_maximum");
        }
        if options
            .evaluation_profile
            .is_some_and(|p| evaluate::confidence(answer).is_none_or(|c| c < p.min_confidence))
        {
            job.notes
                .insert("confidence_unknown_or_below_evaluation_profile");
        }
        if choice == "none" {
            job.outcome = Some(PageResult::pending(
                if job.selected.is_empty() {
                    Status::NotFound
                } else {
                    Status::Extracted
                },
                if job.selected.is_empty() {
                    "no_candidate_selected"
                } else {
                    "evaluation_only"
                },
            ));
        } else if job.selected.len() == candidates.len() {
            job.outcome = Some(PageResult::pending(Status::Extracted, "evaluation_only"));
        } else if job.selected.len() >= *max_selections {
            job.outcome = Some(PageResult::pending(
                Status::Ambiguous,
                "selection_limit_reached",
            ));
        } else {
            job.answers.remove(question_id);
        }
    }
}

fn settle_complete_jobs(
    jobs: &mut [PageJob],
    form: &FormDefinition,
    index: &CandidateIndex<'_>,
    images: &BTreeMap<u32, Result<PreparedImage, &'static str>>,
    options: &ExtractOptions,
    timings: &mut crate::model::Timings,
) {
    for job in jobs.iter_mut().filter(|job| job.outcome.is_none()) {
        let field = &form.fields[job.field];
        if job.answers.len() == field.questions.len() {
            let resolve_started = Instant::now();
            if matches!(field.resolver, Resolver::SelectedCandidates { .. }) {
                advance_selections(job, field, index, options);
            } else {
                job.outcome = early_outcome(field, &job.answers, options).or_else(|| {
                    Some(resolve_page(
                        field,
                        job.page,
                        index,
                        images.get(&job.page).and_then(|image| image.as_ref().ok()),
                        &job.answers,
                        options,
                    ))
                });
            }
            timings.resolve_ms += resolve_started.elapsed().as_secs_f64() * 1000.0;
        }
    }
}

// Only one bounded page window is prepared. Page images and their PNG bindings
// are shared by borrowed evaluation calls, then released before the next window.
#[allow(clippy::too_many_arguments)]
fn evaluate_waves(
    bytes: &[u8],
    form: &FormDefinition,
    options: &ExtractOptions,
    evaluator: &mut impl Evaluator,
    index: &CandidateIndex<'_>,
    pages: &[crate::model::PageDiagnostic],
    geometries: &BTreeMap<u32, crate::geometry::PageGeometry>,
    timings: &mut crate::model::Timings,
    started: Instant,
    usage: &mut UsageSummary,
) -> Vec<Vec<PageResult>> {
    let mut outcomes: Vec<Vec<PageResult>> = (0..form.fields.len()).map(|_| Vec::new()).collect();
    let all_pages: BTreeSet<_> = pages.iter().map(|p| p.page).collect();
    let selected: Vec<BTreeSet<u32>> = form
        .fields
        .iter()
        .enumerate()
        .map(|(field_index, field)| {
            let mut selected = BTreeSet::new();
            let mut missing = false;
            for page in &field.pages {
                if all_pages.contains(page) {
                    selected.insert(*page);
                } else {
                    missing = true;
                }
            }
            if missing {
                let reason = if options.strategy == Strategy::Hybrid
                    && (!matches!(field.resolver, Resolver::TextInBox { .. })
                        || !field.questions.iter().any(|q| q.candidate_source.is_some()))
                {
                    "hybrid_requires_box_and_candidate_agreement"
                } else {
                    "incomplete_source_scope"
                };
                outcomes[field_index].push(PageResult::pending(Status::Unsupported, reason));
            }
            selected
        })
        .collect();
    let requested: Vec<_> = all_pages.iter().copied().collect();
    fn exhausted(
        options: &ExtractOptions,
        usage: &UsageSummary,
        started: Instant,
    ) -> Option<&'static str> {
        if usage.totals_overflowed {
            Some("usage_totals_overflowed")
        } else if started.elapsed().as_millis() >= options.total_deadline_ms as u128 {
            Some("total_deadline_exceeded")
        } else if usage.attempts >= options.max_requests {
            Some("request_budget_exceeded")
        } else {
            None
        }
    }
    let namespaced = evaluator.supports_batching();
    for window in requested.chunks(options.max_concurrency) {
        let mut jobs = Vec::new();
        let mut images = BTreeMap::new();
        for &page in window {
            let exhausted = exhausted(options, usage, started);
            let healthy = pages
                .iter()
                .find(|p| p.page == page)
                .is_some_and(|p| p.text_available && p.quality == Quality::Usable)
                && geometries.contains_key(&page);
            for (field_index, field) in form.fields.iter().enumerate() {
                if !field.pages.is_empty() && !selected[field_index].contains(&page) {
                    continue;
                }
                let outcome = if options.strategy == Strategy::Hybrid
                    && (!matches!(field.resolver, Resolver::TextInBox { .. })
                        || !field.questions.iter().any(|q| q.candidate_source.is_some()))
                {
                    Some(PageResult::pending(
                        Status::Unsupported,
                        "hybrid_requires_box_and_candidate_agreement",
                    ))
                } else if !healthy {
                    Some(PageResult::pending(
                        Status::Unsupported,
                        "incomplete_source_scope",
                    ))
                } else {
                    exhausted.map(|reason| PageResult::pending(Status::Error, reason))
                };
                jobs.push(PageJob {
                    field: field_index,
                    page,
                    answers: BTreeMap::new(),
                    selected: BTreeSet::new(),
                    notes: BTreeSet::new(),
                    outcome,
                });
            }
            if jobs.iter().any(|job| {
                job.page == page
                    && job.outcome.is_none()
                    && form.fields[job.field]
                        .questions
                        .iter()
                        .any(|q| q.input.modality != Modality::Text)
            }) {
                let image_started = Instant::now();
                let image =
                    prepare_image(bytes, page, &geometries[&page], options, evaluator, timings);
                timings.render_ms += image_started.elapsed().as_secs_f64() * 1000.0;
                images.insert(page, image);
            }
        }
        loop {
            let mut ready: BTreeMap<(u32, u8, usize), Vec<(usize, usize)>> = BTreeMap::new();
            for (job_index, job) in jobs
                .iter_mut()
                .enumerate()
                .filter(|(_, job)| job.outcome.is_none())
            {
                let field = &form.fields[job.field];
                if let Some(reason) = images.get(&job.page).and_then(|image| image.as_ref().err())
                    && field
                        .questions
                        .iter()
                        .any(|q| q.input.modality != Modality::Text)
                {
                    job.outcome = Some(PageResult::pending(Status::Unsupported, reason));
                    continue;
                }
                if matches!(field.resolver, Resolver::SelectedCandidates { .. }) {
                    let Some(source) = field.questions[0].candidate_source.as_ref() else {
                        job.outcome = Some(PageResult::pending(
                            Status::Error,
                            "question_contract_invalid",
                        ));
                        continue;
                    };
                    if index
                        .scoped(&[job.page], source.max_candidates)
                        .is_ok_and(|c| c.is_empty())
                    {
                        job.outcome = Some(PageResult::pending(
                            Status::NotFound,
                            "no_candidate_selected",
                        ));
                        continue;
                    }
                }
                let mut count = 0;
                for (q_index, q) in field.questions.iter().enumerate().filter(|(_, q)| {
                    !job.answers.contains_key(&q.id)
                        && q.depends_on.iter().all(|id| job.answers.contains_key(id))
                }) {
                    let modality = match q.input.modality {
                        Modality::Text => 0,
                        Modality::Image => 1,
                        Modality::Both => 2,
                    };
                    ready
                        .entry((job.page, modality, if namespaced { 0 } else { job.field }))
                        .or_default()
                        .push((job_index, q_index));
                    count += 1;
                }
                if count == 0 {
                    job.outcome = Some(PageResult::pending(
                        Status::Error,
                        "question_dependency_failure",
                    ));
                }
            }
            if ready.is_empty() {
                break;
            }
            let mut pending: VecDeque<Vec<(usize, usize)>> = ready
                .into_values()
                .flat_map(|members| {
                    members
                        .chunks(options.max_batch_questions)
                        .map(<[_]>::to_vec)
                        .collect::<Vec<_>>()
                })
                .collect();
            while !pending.is_empty() {
                if let Some(reason) = exhausted(options, usage, started) {
                    for job in jobs.iter_mut().filter(|job| job.outcome.is_none()) {
                        job.outcome = Some(PageResult::pending(Status::Error, reason));
                    }
                    break;
                }
                let capacity = options
                    .max_concurrency
                    .min((options.max_requests - usage.attempts) as usize);
                let mut prepared = Vec::new();
                while prepared.len() < capacity {
                    let Some(mut members) = pending.pop_front() else {
                        break;
                    };
                    members.retain(|(job, _)| jobs[*job].outcome.is_none());
                    if members.is_empty() {
                        continue;
                    }
                    match prepare_batch(&members, &jobs, form, index, &images, namespaced, options)
                    {
                        Ok(batch) => prepared.push(batch),
                        Err(_) if members.len() > 1 => {
                            let rest = members.split_off(members.len() / 2);
                            pending.push_front(rest);
                            pending.push_front(members);
                        }
                        Err(reason) => {
                            let status =
                                if ["question_contract_invalid", "candidate_provenance_invalid"]
                                    .contains(&reason)
                                {
                                    Status::Error
                                } else {
                                    Status::Unsupported
                                };
                            jobs[members[0].0].outcome = Some(PageResult::pending(status, reason));
                        }
                    }
                }
                if prepared.is_empty() {
                    continue;
                }
                let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed >= options.total_deadline_ms {
                    for job in jobs.iter_mut().filter(|job| job.outcome.is_none()) {
                        job.outcome = Some(PageResult::pending(
                            Status::Error,
                            "total_deadline_exceeded",
                        ));
                    }
                    break;
                }
                let calls: Vec<_> = prepared
                    .iter()
                    .enumerate()
                    .map(|(position, batch)| EvaluationCall {
                        request: &batch.request,
                        images: if batch.image {
                            images
                                .get(&batch.page)
                                .and_then(|image| image.as_ref().ok())
                                .map_or(&[], |image| image.png.as_slice())
                        } else {
                            &[]
                        },
                        context: EvaluationContext {
                            // A merged request is not attributed to its first field.
                            // Exact field/question attribution stays in `reverse`.
                            field_id: if batch.cross_field {
                                ""
                            } else {
                                &form.fields[batch.field].id
                            },
                            page: batch.page,
                            remaining_requests: options.max_requests
                                - usage.attempts
                                - position as u32,
                            remaining_ms: options.total_deadline_ms - elapsed,
                        },
                    })
                    .collect();
                let results = evaluator.evaluate_many(&calls);
                if results.len() != prepared.len() {
                    for batch in &prepared {
                        for (job, _) in batch.reverse.values() {
                            jobs[*job].outcome = Some(PageResult::pending(
                                Status::Error,
                                "evaluation_result_count_invalid",
                            ));
                        }
                    }
                    continue;
                }
                let mut late_jobs = BTreeSet::new();
                for (batch, result) in prepared.iter().zip(results) {
                    usage.add(&result.usage);
                    let late = started.elapsed().as_millis() >= options.total_deadline_ms as u128;
                    let failure = match &result.result {
                        Err(_) if late => Some("total_deadline_exceeded"),
                        Err(evaluate::EvaluationError::Timeout) => Some("evaluation_timeout"),
                        Err(_) => Some("evaluation_failed"),
                        _ => None,
                    };
                    if let Some(reason) = failure {
                        for (job, _) in batch.reverse.values() {
                            jobs[*job].outcome = Some(PageResult::pending(Status::Error, reason));
                        }
                        continue;
                    }
                    let Ok(response) = result.result else {
                        for (job, _) in batch.reverse.values() {
                            jobs[*job].outcome =
                                Some(PageResult::pending(Status::Error, "evaluation_failed"));
                        }
                        continue;
                    };
                    if response.answers.len() != batch.reverse.len()
                        || batch
                            .reverse
                            .keys()
                            .any(|id| !response.answers.contains_key(id))
                    {
                        for (job, _) in batch.reverse.values() {
                            jobs[*job].outcome =
                                Some(PageResult::pending(Status::Error, "answer_id_mismatch"));
                        }
                        continue;
                    }
                    if late {
                        late_jobs.extend(batch.reverse.values().map(|(job, _)| *job));
                    }
                    for diagnostic in &response.diagnostics {
                        if let jev::Diagnostic::ProbabilitySumNotOne(id) = diagnostic
                            && let Some((job, _)) = batch.reverse.get(id)
                        {
                            jobs[*job].notes.insert("probability_sum_not_one");
                        }
                    }
                    for (id, answer) in response.answers {
                        let (job, question) = &batch.reverse[&id];
                        match answer {
                            Ok(answer) => {
                                jobs[*job].answers.insert(question.clone(), answer);
                            }
                            Err(_) => {
                                jobs[*job].outcome = Some(PageResult::pending(
                                    Status::Error,
                                    "answer_contract_invalid",
                                ));
                            }
                        }
                    }
                }
                settle_complete_jobs(&mut jobs, form, index, &images, options, timings);
                for job in late_jobs {
                    let outcome = jobs[job].outcome.get_or_insert_with(|| {
                        PageResult::pending(Status::Error, "total_deadline_exceeded")
                    });
                    outcome.status = Status::Error;
                    outcome.reason = "total_deadline_exceeded";
                }
            }
            for job in jobs.iter_mut().filter(|job| job.outcome.is_none()) {
                let field = &form.fields[job.field];
                job.outcome = early_outcome(field, &job.answers, options);
            }
        }
        for job in jobs {
            let field = &form.fields[job.field];
            let mut outcome = job.outcome.unwrap_or_else(|| {
                PageResult::pending(Status::Error, "question_dependency_failure")
            });
            // Preserve every confirmed selection even when a later round fails.
            if !job.selected.is_empty() {
                let selected: Vec<_> = index
                    .candidates()
                    .iter()
                    .filter(|c| job.selected.contains(&c.id))
                    .collect();
                match source_value(field, &selected, index, None, options) {
                    Ok((value, origin, evidence)) => {
                        outcome.value = Some(value);
                        outcome.origin = origin;
                        outcome.evidence = evidence;
                    }
                    Err(reason) => outcome = PageResult::pending(Status::Error, reason),
                }
            }
            outcomes[job.field].push(outcome);
            for note in job.notes {
                outcomes[job.field].push(PageResult::pending(Status::Ambiguous, note));
            }
        }
    }
    outcomes
}

fn source_value(
    field: &FieldDefinition,
    selected: &[&Candidate],
    index: &CandidateIndex<'_>,
    image: Option<&PreparedImage>,
    options: &ExtractOptions,
) -> Result<(Value, &'static str, Vec<Evidence>), &'static str> {
    let mut evidence = Vec::new();
    let mut values = Vec::with_capacity(selected.len());
    let mut origin = "source_text";
    let image_id = image.map(|image| image.png[0].sha256());
    for candidate in selected {
        let raw = match index.text(candidate) {
            Ok(raw) => raw,
            Err(_) => {
                return Err("candidate_provenance_invalid");
            }
        };
        let mut source = match index.evidence(
            candidate,
            &field.id,
            field.resolver.question_id(),
            options.strategy,
        ) {
            Ok(source) => source,
            Err(_) => {
                return Err("candidate_provenance_invalid");
            }
        };
        if let Some(image) = image {
            source.image_id = image_id.clone();
            source.image_transform = Some(image.transform.clone());
        }
        evidence.push(source);
        let normalized = validate::normalize(&raw, field.normalization);
        if normalized != raw {
            origin = "normalized_source";
        }
        values.push(Value::String(normalized));
    }
    let value = if field.output_type == OutputType::StringArray {
        Value::Array(values)
    } else {
        let Some(value) = values.into_iter().next() else {
            return Err("candidate_provenance_invalid");
        };
        value
    };
    Ok((value, origin, evidence))
}

fn resolve_page(
    field: &FieldDefinition,
    page: u32,
    index: &CandidateIndex<'_>,
    image: Option<&PreparedImage>,
    answers: &BTreeMap<String, Answer>,
    options: &ExtractOptions,
) -> PageResult {
    let Some(answer) = answers.get(field.resolver.question_id()) else {
        return PageResult::pending(Status::Error, "answer_missing");
    };
    let array_output = field.output_type == OutputType::StringArray;
    let mut selected = Vec::new();
    let mut status = Status::Extracted;
    let mut reason = "evaluation_only";
    if !unique_choice(answer) {
        status = Status::Ambiguous;
        reason = "choice_not_unique_maximum";
    }
    if let Some(profile) = options.evaluation_profile
        && !matches!(answer, Answer::Noul { .. })
        && evaluate::confidence(answer).is_none_or(|c| c < profile.min_confidence)
    {
        status = Status::Ambiguous;
        reason = "confidence_unknown_or_below_evaluation_profile";
    }
    let mut mapping = None;
    match (&field.resolver, answer) {
        (Resolver::SelectedCandidate { .. }, Answer::Choice { choice, .. }) => {
            if choice == "none" {
                return PageResult::pending(
                    if status == Status::Ambiguous {
                        status
                    } else {
                        Status::NotFound
                    },
                    if status == Status::Ambiguous {
                        reason
                    } else {
                        "no_candidate_selected"
                    },
                );
            }
            let candidate = index
                .candidates()
                .iter()
                .find(|c| c.page == page && c.id == *choice);
            let Some(candidate) = candidate else {
                return PageResult::pending(Status::Error, "candidate_reference_invalid");
            };
            selected.push(candidate);
        }
        (
            Resolver::TextInBox {
                presence_question_id,
                ..
            },
            Answer::Box { r#box, .. },
        ) => {
            let Some(Answer::Noul { noul }) = answers.get(presence_question_id) else {
                return PageResult::pending(Status::Error, "presence_answer_invalid");
            };
            if let Some(profile) = options.evaluation_profile {
                match profile.presence(*noul) {
                    Some(false) => {
                        return PageResult::pending(Status::NotFound, "presence_observed_absent");
                    }
                    None => return PageResult::pending(Status::Ambiguous, "presence_uncertain"),
                    _ => {}
                }
            }
            let Some(image) = image else {
                return PageResult::pending(Status::Unsupported, "image_binding_unavailable");
            };
            let normalized = Rect {
                left: r#box.left,
                top: r#box.top,
                right: r#box.right,
                bottom: r#box.bottom,
            };
            let bbox = match image.transform.normalized_box_to_display(normalized) {
                Ok(bbox) => bbox,
                Err(_) => return PageResult::pending(Status::Ambiguous, "box_transform_invalid"),
            };
            let candidates = match if array_output {
                index.overlapping(page, bbox)
            } else {
                index.in_box(page, bbox)
            } {
                Ok(candidates) => candidates,
                Err(_) => return PageResult::pending(Status::Ambiguous, "box_splits_source_run"),
            };
            if candidates.is_empty() {
                if options.strategy==Strategy::Hybrid && field.questions.iter().filter(|q|q.candidate_source.is_some())
                    .any(|q|matches!(answers.get(&q.id),Some(Answer::Choice {choice,..}) if choice!="none")) {
                    return PageResult::pending(Status::Ambiguous,"hybrid_source_disagreement");
                }
                return PageResult::pending(Status::NotFound, "box_has_no_source_text");
            }
            if !array_output && candidates.len() != 1 {
                return PageResult::pending(
                    Status::Ambiguous,
                    "box_has_multiple_source_candidates",
                );
            }
            selected = candidates;
            if options.strategy == Strategy::Hybrid {
                for q in field
                    .questions
                    .iter()
                    .filter(|q| q.candidate_source.is_some())
                {
                    let Some(Answer::Choice {
                        choice, confidence, ..
                    }) = answers.get(&q.id)
                    else {
                        return PageResult::pending(Status::Error, "hybrid_choice_missing");
                    };
                    if !unique_choice(&answers[&q.id]) {
                        status = Status::Ambiguous;
                        reason = "choice_maximum_tied";
                    }
                    if !selected.iter().any(|c| c.id == *choice)
                        || options
                            .evaluation_profile
                            .is_some_and(|profile| *confidence < profile.min_confidence)
                    {
                        status = Status::Ambiguous;
                        reason = "hybrid_source_disagreement";
                    }
                }
            }
        }
        (
            Resolver::SelectedChoice {
                mapping: values, ..
            },
            Answer::Choice { choice, .. },
        ) => mapping = values.get(choice).cloned(),
        (Resolver::NoulToBoolean { .. }, Answer::Noul { noul }) => {
            let profile = options.resolution_profile();
            mapping = profile.presence(*noul).map(Value::Bool);
        }
        (
            Resolver::ScoreToLevel { levels, .. },
            Answer::Score {
                score,
                probabilities,
                ..
            },
        ) => {
            let key = (*score as usize).to_string();
            if score.fract() == 0.0
                && probabilities.get(&key).is_some_and(|p| {
                    probabilities
                        .iter()
                        .all(|(other, value)| other == &key || p > value)
                        && options
                            .evaluation_profile
                            .is_none_or(|profile| *p >= profile.min_confidence)
                })
            {
                mapping = levels.get(*score as usize).cloned();
            }
        }
        _ => return PageResult::pending(Status::Error, "resolver_answer_type_invalid"),
    }
    let mut evidence = Vec::new();
    let (value, origin) = if !selected.is_empty() {
        match source_value(field, &selected, index, image, options) {
            Ok((value, origin, sources)) => {
                evidence = sources;
                (value, origin)
            }
            Err(reason) => return PageResult::pending(Status::Error, reason),
        }
    } else if let Some(value) = mapping {
        let mut context: Option<Evidence> = None;
        for candidate in index
            .candidates()
            .iter()
            .filter(|candidate| candidate.page == page)
        {
            let source = match index.evidence(
                candidate,
                &field.id,
                field.resolver.question_id(),
                options.strategy,
            ) {
                Ok(source) => source,
                Err(_) => {
                    return PageResult::pending(Status::Error, "candidate_provenance_invalid");
                }
            };
            if let Some(existing) = context.as_mut() {
                existing.spans.extend(source.spans);
                existing.raw_fragments.extend(source.raw_fragments);
                existing.display_bbox = Rect {
                    left: existing.display_bbox.left.min(source.display_bbox.left),
                    top: existing.display_bbox.top.min(source.display_bbox.top),
                    right: existing.display_bbox.right.max(source.display_bbox.right),
                    bottom: existing.display_bbox.bottom.max(source.display_bbox.bottom),
                };
            } else {
                context = Some(source);
            }
        }
        let Some(mut context) = context else {
            return PageResult::pending(Status::Unsupported, "mapping_source_scope_unavailable");
        };
        let question = field.resolver.question_id();
        context.id = format!(
            "ev_mapping_{}:{}_{}:{}_{}",
            field.id.len(),
            field.id,
            question.len(),
            question,
            page
        );
        context.candidate_id = None;
        if let Some(image) = image {
            context.image_id = Some(image.png[0].sha256());
            context.image_transform = Some(image.transform.clone());
        }
        evidence.push(context);
        (value, "evaluated_mapping")
    } else {
        return PageResult::pending(
            Status::Ambiguous,
            "mapping_requires_discrete_unambiguous_answer",
        );
    };
    // Array validators describe the complete field across pages, not each page's subset.
    let validators = if array_output {
        &[][..]
    } else {
        &field.validators
    };
    if validate::validate(&value, field.output_type, validators).is_err() {
        return PageResult {
            status: Status::Ambiguous,
            value: Some(value),
            evidence,
            reason: "value_validation_failed",
            origin,
        };
    }
    PageResult {
        status,
        value: Some(value),
        evidence,
        reason,
        origin,
    }
}

fn combine(
    field: &FieldDefinition,
    outcomes: Vec<PageResult>,
    evidence: &mut Vec<Evidence>,
) -> FieldResult {
    let mut result = FieldResult::pending(&field.id, Status::Ambiguous, "unresolved");
    let source_based = matches!(
        field.resolver,
        Resolver::SelectedCandidate { .. }
            | Resolver::SelectedCandidates { .. }
            | Resolver::TextInBox { .. }
    );
    result.value_origin = if source_based {
        "source_text"
    } else {
        "evaluated_mapping"
    };
    let mut values = Vec::new();
    let mut reasons = Vec::new();
    let mut has_error = false;
    let mut incomplete = false;
    let mut ambiguous = false;
    for outcome in outcomes {
        has_error |= outcome.status == Status::Error;
        incomplete |= outcome.status == Status::Unsupported;
        ambiguous |= outcome.status == Status::Ambiguous;
        reasons.push(outcome.reason.to_owned());
        if outcome.origin == "normalized_source" {
            result.value_origin = "normalized_source";
        }
        if let Some(value) = outcome.value {
            values.push(value);
        }
        for source in outcome.evidence {
            result.evidence_ids.push(source.id.clone());
            evidence.push(source);
        }
    }
    let array_output = field.output_type == OutputType::StringArray;
    let has_values = !values.is_empty();
    if array_output && has_values {
        result.value = Some(Value::Array(
            values
                .drain(..)
                .filter_map(|value| match value {
                    Value::Array(values) => Some(values),
                    _ => None,
                })
                .flatten()
                .collect(),
        ));
    }
    if !array_output
        && has_values
        && values.windows(2).all(|v| v[0] == v[1])
        && (!source_based || values.len() == 1)
    {
        result.value = values.first().cloned();
    }
    result.status = if has_error {
        Status::Error
    } else if incomplete {
        if !has_values {
            Status::Unsupported
        } else {
            Status::Ambiguous
        }
    } else if ambiguous {
        Status::Ambiguous
    } else if !has_values {
        Status::NotFound
    } else if array_output {
        Status::Extracted
    } else if source_based && values.len() != 1 || values.windows(2).any(|v| v[0] != v[1]) {
        reasons.push("multiple_source_matches".into());
        Status::Ambiguous
    } else {
        result.value = values.into_iter().next();
        Status::Extracted
    };
    if array_output
        && result.value.as_ref().is_some_and(|value| {
            validate::validate(value, field.output_type, &field.validators).is_err()
        })
    {
        if result.status == Status::Extracted {
            result.status = Status::Ambiguous;
        }
        reasons.push("value_validation_failed".into());
    }
    reasons.sort();
    reasons.dedup();
    result.reason_codes = reasons;
    result
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn consistent(report: &ExtractionReport, index: &CandidateIndex<'_>) -> bool {
    if !report.evaluation_only {
        return false;
    }
    let mut ids = BTreeMap::new();
    for source in &report.evidence {
        if ids.insert(source.id.as_str(), source).is_some()
            || source.spans.is_empty()
            || source.display_bbox.validate().is_err()
            || source.image_id.is_some() != source.image_transform.is_some()
            || source
                .image_transform
                .as_ref()
                .is_some_and(|t| t.validate().is_err())
        {
            return false;
        }
        let Ok(raw) = index.raw_fragments(&source.spans) else {
            return false;
        };
        if raw
            .iter()
            .copied()
            .ne(source.raw_fragments.iter().map(String::as_str))
        {
            return false;
        }
    }
    let mut referenced = BTreeSet::new();
    for field in &report.fields {
        if field.status == Status::Extracted && field.value.is_none()
            || field.value.as_ref().is_some_and(Value::is_null)
            || field.value.is_some() && field.evidence_ids.is_empty()
        {
            return false;
        }
        for id in &field.evidence_ids {
            if !referenced.insert(id.as_str())
                || ids
                    .get(id.as_str())
                    .is_none_or(|source| source.field_id != field.field_id)
            {
                return false;
            }
        }
    }
    referenced.len() == ids.len()
}

// A probability disagreement is an evaluation annotation, never a replacement choice.
fn unique_choice(answer: &Answer) -> bool {
    let Answer::Choice {
        choice,
        probabilities,
        ..
    } = answer
    else {
        return true;
    };
    let Some(selected) = probabilities.get(choice) else {
        return false;
    };
    selected.is_finite()
        && probabilities
            .iter()
            .all(|(key, p)| key == choice || p < selected)
}

fn run_id() -> String {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "run_{epoch:x}_{:x}_{:x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{PageGeometry, Rotation},
        model::{PageDiagnostic, SourceFragment, Timings},
    };
    use serde_json::json;

    struct Script {
        steps: VecDeque<Result<(&'static str, bool), evaluate::EvaluationError>>,
        requests: Vec<Value>,
    }
    impl Evaluator for Script {
        fn supports_batching(&self) -> bool {
            true
        }
        fn evaluate(
            &mut self,
            request: &jev::Request,
            _: &[PngImage],
            _context: &EvaluationContext<'_>,
        ) -> Result<jev::Response, evaluate::EvaluationError> {
            self.requests
                .push(serde_json::from_slice(request.bytes()).unwrap());
            let (choice, tied) = self.steps.pop_front().expect("unexpected request")?;
            let choice = if let Some(choice) = choice.strip_prefix("late:") {
                std::thread::sleep(std::time::Duration::from_millis(_context.remaining_ms + 1));
                choice
            } else {
                choice
            };
            let rounded = choice.starts_with("rounded:");
            let choice = choice.strip_prefix("rounded:").unwrap_or(choice);
            let mut diagnostics = Vec::new();
            let mut answers = BTreeMap::new();
            for (id, question) in &request.draft().questions {
                let Question::Choice {
                    criteria,
                    instructions,
                } = question
                else {
                    panic!("choice required")
                };
                assert!(
                    matches!(instructions, Content::Text(text) if text == "Return every matching source run; choose one remaining match or none.")
                );
                let probabilities = criteria
                    .keys()
                    .map(|id| {
                        (
                            id.clone(),
                            if id == choice {
                                if tied { 0.495 } else { 0.99 }
                            } else if tied && (id == "none" || choice == "none") {
                                0.495
                            } else {
                                0.0
                            },
                        )
                    })
                    .collect();
                if rounded {
                    diagnostics.push(jev::Diagnostic::ProbabilitySumNotOne(id.clone()));
                }
                answers.insert(
                    id.clone(),
                    Ok(Answer::Choice {
                        choice: choice.into(),
                        probabilities,
                        confidence: 0.9,
                    }),
                );
            }
            Ok(jev::Response {
                model: jev::MODEL.into(),
                answers,
                usage: jev::Usage {
                    input_tokens: jev::Nullable(None),
                    output_tokens: jev::Nullable(None),
                },
                diagnostics,
            })
        }
    }
    fn form(cap: usize) -> FormDefinition {
        crate::schema::parse(&serde_json::to_vec(&json!({"schema_version":1,"fields":[{
            "id":"matches","label":"Matches","output_type":"string_array","pages":[1],
            "questions":[{"id":"pick","type":"choice","instructions":"Return every matching source run; choose one remaining match or none.","input":{"modality":"text"},"candidate_source":{"kind":"text_spans","none_option":"No remaining match"}}],
            "resolver":{"kind":"selected_candidates","question_id":"pick","max_selections":cap}
        }]})).unwrap()).unwrap()
    }
    fn run(
        form: &FormDefinition,
        options: &ExtractOptions,
        steps: Vec<Result<(&'static str, bool), evaluate::EvaluationError>>,
    ) -> (Vec<FieldResult>, Vec<Evidence>, UsageSummary, Script) {
        let fragments: Vec<_> = (1..=2)
            .flat_map(|page| {
                (1..=3).map(move |i| SourceFragment {
                    id: format!("p{page}r{i}"),
                    page,
                    item_index: i,
                    raw_text: if i < 3 { "same text" } else { "other" }.into(),
                    bbox: Rect::new(0.0, i as f64 * 10.0, 50.0, i as f64 * 10.0 + 5.0).unwrap(),
                    quality: Quality::Usable,
                })
            })
            .collect();
        let index = CandidateIndex::new(&fragments).unwrap();
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0).unwrap();
        let geometries = (1..=2)
            .map(|page| {
                (
                    page,
                    PageGeometry::new(bounds, bounds, Rotation::Deg0, 1.0).unwrap(),
                )
            })
            .collect();
        let pages = (1..=2)
            .map(|page| PageDiagnostic {
                page,
                text_available: true,
                quality: Quality::Usable,
                reason_codes: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut evaluator = Script {
            steps: steps.into(),
            requests: Vec::new(),
        };
        let mut usage = UsageSummary::default();
        let outcomes = evaluate_waves(
            &[],
            form,
            options,
            &mut evaluator,
            &index,
            &pages,
            &geometries,
            &mut Timings::default(),
            Instant::now(),
            &mut usage,
        );
        let mut evidence = Vec::new();
        let fields = form
            .fields
            .iter()
            .zip(outcomes)
            .map(|(field, outcomes)| combine(field, outcomes, &mut evidence))
            .collect();
        (fields, evidence, usage, evaluator)
    }
    #[test]
    fn multiple_source_order_none_and_single_compatibility() {
        let options = ExtractOptions::default();
        let (fields, evidence, usage, script) = run(
            &form(8),
            &options,
            vec![
                Ok(("c_p1r2", false)),
                Ok(("c_p1r1", false)),
                Ok(("none", false)),
            ],
        );
        assert_eq!(fields[0].status, Status::Extracted);
        assert_eq!(fields[0].value, Some(json!(["same text", "same text"])));
        assert_eq!(
            evidence
                .iter()
                .map(|e| e.candidate_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["c_p1r1", "c_p1r2"]
        );
        assert_eq!(usage.attempts, 3);
        let (rounded_fields, rounded_evidence, rounded_usage, _) = run(
            &form(8),
            &options,
            vec![
                Ok(("rounded:c_p1r2", false)),
                Ok(("c_p1r1", false)),
                Ok(("none", false)),
            ],
        );
        assert_eq!(rounded_fields[0].status, Status::Ambiguous);
        assert!(
            rounded_fields[0]
                .reason_codes
                .contains(&"probability_sum_not_one".into())
        );
        assert_eq!(
            rounded_fields[0].value,
            Some(json!(["same text", "same text"]))
        );
        assert_eq!((rounded_evidence.len(), rounded_usage.attempts), (2, 3));
        assert!(
            script.requests[1]["questions"]["f0q0"]["criteria"]
                .get("c_p1r2")
                .is_none()
        );
        assert!(
            script.requests[2]["questions"]["f0q0"]["criteria"]
                .get("c_p1r1")
                .is_none()
        );
        let (fields, _, _, _) = run(&form(8), &options, vec![Ok(("none", false))]);
        assert_eq!(fields[0].status, Status::NotFound);
        let mut single = form(8);
        single.fields[0].output_type = OutputType::String;
        single.fields[0].resolver = Resolver::SelectedCandidate {
            question_id: "pick".into(),
        };
        let (fields, evidence, usage, _) = run(&single, &options, vec![Ok(("c_p1r2", false))]);
        assert_eq!(fields[0].value, Some(json!("same text")));
        assert_eq!((evidence.len(), usage.attempts), (1, 1));
        let (fields, evidence, _, _) = run(&single, &options, vec![Ok(("c_p1r2", true))]);
        assert_eq!(fields[0].status, Status::Ambiguous);
        assert_eq!(fields[0].value, Some(json!("same text")));
        assert_eq!(evidence.len(), 1);
    }
    #[test]
    fn multiple_partial_values_survive_caps_failures_and_ambiguous_answers() {
        let options = ExtractOptions::default();
        for (steps, expected, reason) in [
            (
                vec![
                    Ok(("c_p1r2", false)),
                    Err(evaluate::EvaluationError::Timeout),
                ],
                Status::Error,
                "evaluation_timeout",
            ),
            (
                vec![
                    Ok(("c_p1r2", false)),
                    Err(evaluate::EvaluationError::Protocol),
                ],
                Status::Error,
                "evaluation_failed",
            ),
            (
                vec![Ok(("c_p1r2", false)), Ok(("c_p1r2", false))],
                Status::Error,
                "candidate_reference_invalid",
            ),
            (
                vec![Ok(("c_p1r2", false)), Ok(("none", true))],
                Status::Ambiguous,
                "choice_not_unique_maximum",
            ),
        ] {
            // For none ties, explicitly supply a competing probability below.
            let (fields, evidence, _, _) = run(&form(8), &options, steps);
            assert_eq!(fields[0].status, expected);
            assert_eq!(fields[0].value, Some(json!(["same text"])));
            assert!(fields[0].reason_codes.contains(&reason.into()));
            assert_eq!(evidence.len(), 1);
        }
        let (fields, evidence, _, _) = run(&form(1), &options, vec![Ok(("c_p1r1", false))]);
        assert_eq!(fields[0].status, Status::Ambiguous);
        assert!(
            fields[0]
                .reason_codes
                .contains(&"selection_limit_reached".into())
        );
        assert_eq!(evidence.len(), 1);
        let (fields, _, usage, _) = run(
            &form(3),
            &options,
            vec![
                Ok(("c_p1r2", false)),
                Ok(("c_p1r1", false)),
                Ok(("c_p1r3", false)),
            ],
        );
        assert_eq!(fields[0].status, Status::Extracted);
        assert_eq!(usage.attempts, 3); // No invalid none-only probe.
        let deadline_options = ExtractOptions {
            total_deadline_ms: 20,
            ..Default::default()
        };
        let (fields, evidence, _, _) = run(
            &form(8),
            &deadline_options,
            vec![Ok(("late:c_p1r1", false))],
        );
        assert_eq!(fields[0].status, Status::Error);
        assert_eq!(fields[0].value, Some(json!(["same text"])));
        assert!(
            fields[0]
                .reason_codes
                .contains(&"total_deadline_exceeded".into())
        );
        assert_eq!(evidence.len(), 1);
        let (fields, evidence, usage, _) = run(
            &form(8),
            &options,
            vec![
                Ok(("c_p1r2", true)),
                Ok(("c_p1r1", false)),
                Ok(("none", false)),
            ],
        );
        assert_eq!(fields[0].status, Status::Ambiguous);
        assert_eq!(fields[0].value, Some(json!(["same text", "same text"])));
        assert_eq!((evidence.len(), usage.attempts), (2, 3));
    }
    #[test]
    fn multiple_batches_and_pages_share_request_budget() {
        let mut form = form(8);
        form.fields[0].pages = vec![1, 2];
        let mut second = form.fields[0].clone();
        second.id = "second".into();
        form.fields.push(second);
        let options = ExtractOptions {
            max_requests: 1,
            ..Default::default()
        };
        let (fields, evidence, usage, script) = run(&form, &options, vec![Ok(("c_p1r1", false))]);
        assert_eq!(usage.attempts, 1);
        assert_eq!(
            script.requests[0]["questions"].as_object().unwrap().len(),
            2
        );
        assert!(fields.iter().all(|f| f.status == Status::Error
            && f.value == Some(json!(["same text"]))
            && f.reason_codes.contains(&"request_budget_exceeded".into())));
        assert_eq!(evidence.len(), 2);
    }
}
