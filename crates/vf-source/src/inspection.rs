//! PDF inspection and bounded source diagnostics.
//!
//! # Vertical and rotated text
//!
//! A run whose rendered baseline (pdf-inspector's `TextItem::rotation` in the
//! Display frame, where any re-basing of the page frame is already undone)
//! lies within [`ROTATION_TOLERANCE_DEGREES`] of a right angle carries that
//! quarter turn as its [`Orientation`], and layout reads it in its own
//! reading direction (see [`crate::layout`]). Oblique runs, re-based pages
//! and vertical writing mode fonts stay out of `Usable`:
//!
//! - A run turned by 90°, 180° or 270° is `Usable` on the same terms as a
//!   level run, unless pdf-inspector's super/subscript pass may have joined
//!   it or another turned run on its page (below). Its page reports
//!   `rotated_text_present`.
//! - An oblique run is `Unknown` and leaves the whole page `Unknown`
//!   (`oblique_text_present`): it can be merged into a level neighbour that
//!   then reports the neighbour's angle. Its orientation is the nearest
//!   quarter turn.
//! - A page pdf-inspector re-based because most of its text runs vertically
//!   is `Unknown` as a whole (`page_text_rotated`): its runs were merged and
//!   spaced in the turned frame, and pdf-inspector merges a re-based page
//!   without the clip boundaries it applies elsewhere (an upstream
//!   unsupported case). The one exception is a turn equal to the page's
//!   `/Rotate` (`PageRotation::Ccw`, runs reading bottom to top, on a
//!   `/Rotate 90` page; `PageRotation::Cw` on a `/Rotate 270` page): both are
//!   then the same quarter turn of the sheet, so the frame pdf-inspector
//!   merged runs in is the rendered page up to a shift, as on an upright
//!   page. Clip boundaries are not applied there either.
//! - pdf-inspector ignores a composite font's writing mode and lays vertical
//!   glyphs out horizontally, so a page whose resources hold a vertical-mode
//!   font is `Unknown` as a whole (`vertical_writing_mode_unsupported`).
//!   Fragments cannot be matched to fonts: `TextItem::font_tag` keeps only
//!   the first run's tag after merging, a tag names different fonts in a
//!   page's and a Form XObject's resources, and misplaced vertical glyphs
//!   can be merged into neighbouring runs. A scan stopped by its bounds also
//!   leaves the page `Unknown` (`writing_mode_scan_incomplete`) and never
//!   fails the inspection. A vertical-mode font listed in the resources
//!   counts as used.
//!
//! What pdf-inspector 1.25.2 gives a turned run, from its source:
//!
//! - Box: `run_geometry` takes the axis-aligned device-space box of the
//!   run's advance and a one-em glyph height perpendicular to its baseline,
//!   so a 90° or 270° run is one em wide and its advance tall.
//!   `items_to_display_frame` moves boxes into the Display frame with exact
//!   quarter turns and shifts, so the Display box is the run's box. A run
//!   whose advance was estimated is `Unknown` (`estimated_advance`), as for
//!   level runs.
//! - Merging: `merge_text_items_with_clips` joins runs only while both are
//!   upright (within 45° of 0°) in the frame it works in: the sheet, before
//!   `/Rotate`, unless the page was re-based. A run turned in that frame is
//!   never merged. Runs merged there were joined along their own reading
//!   direction with their own gaps, which a quarter turn of the sheet keeps:
//!   on a `/Rotate 90` page, text upright on the sheet reads at 270° and is
//!   merged as on an upright page. A turned phrase drawn word by word
//!   therefore arrives as one run per word; layout holds such close
//!   neighbours rather than pair them (see [`crate::layout`]).
//! - Super/subscripts: `merge_subscript_items` runs afterwards and ignores
//!   rotation; it takes x as the reading axis and y as the baseline. It
//!   fuses a short digit run into a neighbour as superscript or subscript
//!   digits, widening the neighbour's box along x, or joins a chain of short
//!   glyph runs into one item with a nonzero `baseline_shift`, its text in x
//!   order. It also chains scripts onto scripts and measures such a chain's
//!   shift from the body baseline, so a chain hung on a raised script can
//!   report a zero shift. A turned run showing either sign is `Unknown`
//!   (`rotated_script_join_unverified`), and then so is every other turned
//!   run on its page; this does not change the page's quality. A turned
//!   digit run fused into a level neighbour leaves no trace and is not
//!   detected.
//! - Text order: a run's text is in content-stream order, its advance
//!   direction, and `rotation` is that direction for an unreflected text
//!   matrix. For a reflected (mirrored) matrix such as `[0 1 1 0]`,
//!   `[0 -1 -1 0]` or `[1 0 0 -1]` it is how the glyphs stand, so
//!   mirror-written text reads against its orientation. `TextItem` does not
//!   expose the matrix, so a mirrored run is not detected: it is read in the
//!   reported orientation, an estimated partial span can sit at the wrong
//!   end of its run, and a `Right` or `Below` pair can come out in reverse
//!   order. Level runs share this limit.
//!
//! Not detected: vertical text set glyph by glyph in an upright horizontal
//! font, which carries no rotation and no vertical CMap. True vertical
//! writing (a vertical writing mode font) is not read yet: it needs
//! pdf-inspector to lay vertical glyphs out vertically.
use crate::acroform::{self, AcroForm, AcroFormError, AcroFormLimits};
use crate::metadata;
use crate::source::{Orientation, PageDiagnostic, Quality, SourceFragment};

/// Observed inspection timings and parser load count, without extraction or transport accounting.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InspectionTimings {
    pub metadata_ms: f64,
    pub positions_ms: f64,
    pub quality_ms: f64,
    pub total_ms: f64,
    pub parser_load_calls: u32,
}

use crate::geometry::{PageGeometry, Rect, Rotation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use pdf_inspector::types::ItemType;
use pdf_inspector::{PageRotation, PdfOptions, PositionFrame, ProcessMode, TextItem};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;
use thiserror::Error;

/// A rendered baseline within this many degrees of 0° counts as horizontal;
/// within this many degrees of another right angle it counts as turned.
const ROTATION_TOLERANCE_DEGREES: f64 = 1.0;
/// Form XObject nesting followed below a page's own resources by the
/// writing-mode font scan.
const MAX_FORM_DEPTH: usize = 8;
/// Resource dictionaries, font entries and XObject entries the writing-mode
/// scan visits per page.
const MAX_FONT_SCAN_NODES_PER_PAGE: usize = 4096;
/// `/UseCMap` links followed from one embedded CMap.
const MAX_USECMAP_CHAIN: usize = 4;
/// Embedded CMap program bytes decoded per document.
const MAX_CMAP_BYTES_PER_DOCUMENT: usize = 8 * 1024 * 1024;

/// Application inspection ceilings; these do not sandbox allocations inside the PDF parser.
/// A zero recovery-page limit disables fallback page inspection.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PdfLimits {
    pub max_bytes: usize,
    pub max_pages: u32,
    pub max_items_per_page: usize,
    pub max_recovery_pages: u32,
}

impl Default for PdfLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_pages: 100,
            max_items_per_page: 10_000,
            max_recovery_pages: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, Error)]
pub enum PdfError {
    #[error("PDF input exceeds configured limits")]
    LimitExceeded,
    #[error("PDF container could not be loaded without metadata repair")]
    InvalidContainer,
    #[error("PDF root/page tree is invalid or empty")]
    InvalidPageTree,
    #[error("encrypted PDFs are unsupported")]
    Encrypted,
}

/// Source text, diagnostics, and supported page geometry from one PDF inspection.
/// Usable text is not a guarantee of visual glyph fidelity or semantic correctness.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentSnapshot {
    pub document_id: String,
    pub fragments: Vec<SourceFragment>,
    pub pages: Vec<PageDiagnostic>,
    pub geometries: BTreeMap<u32, PageGeometry>,
    pub timings: InspectionTimings,
}

/// Performs one strict metadata load and normally one Analyze + one positions
/// API call for all supported pages. Recovery is bounded by max_recovery_pages.
/// Limits bound application work and input; they do not sandbox the parser or
/// bound all allocations inside upstream. No OCR or renderer is invoked.
///
/// A run whose rendered baseline is within 1° of a right angle carries that
/// quarter turn as its orientation and may be `Usable` (`rotated_text_present`
/// when turned); once a turned run on a page may have been joined by
/// pdf-inspector's super/subscript pass, every turned run on that page is
/// `Unknown` (`rotated_script_join_unverified`). Mirrored text matrices are
/// not detected (see the module docs). An oblique run
/// (`oblique_text_present`), a page re-based for mostly vertical text unless
/// the turn equals its `/Rotate` (`page_text_rotated`), a page whose
/// resources hold a vertical writing mode font
/// (`vertical_writing_mode_unsupported`) and a page whose font scan hit its
/// bounds (`writing_mode_scan_incomplete`) are `Unknown` as a whole. The font
/// scan reuses the metadata load and is bounded per page and per document.
/// Vertical text set glyph by glyph in a horizontal font is not detected;
/// true vertical writing is not read yet.
pub fn inspect(bytes: &[u8], limits: PdfLimits) -> Result<DocumentSnapshot, PdfError> {
    inspect_inner(bytes, limits, None).map(|(snapshot, _)| snapshot)
}

/// [`inspect`], plus the document's AcroForm fields read from the same
/// metadata load with [`acroform::fields_in`] before upstream loads the PDF.
/// The AcroForm limits take `max_bytes` and `max_pages` from `limits` and
/// the [`AcroFormLimits`] defaults otherwise. An AcroForm error (a limit, an
/// encrypted document `lopdf` decrypted while loading, an invalid field
/// tree) is returned beside the snapshot and never fails the inspection.
pub fn inspect_with_acroform(
    bytes: &[u8],
    limits: PdfLimits,
) -> Result<(DocumentSnapshot, Result<AcroForm, AcroFormError>), PdfError> {
    let acroform = AcroFormLimits {
        max_bytes: limits.max_bytes,
        max_pages: limits.max_pages,
        ..AcroFormLimits::default()
    };
    let (snapshot, form) = inspect_inner(bytes, limits, Some(acroform))?;
    Ok((
        snapshot,
        form.unwrap_or(Err(AcroFormError::InvalidContainer)),
    ))
}

fn inspect_inner(
    bytes: &[u8],
    limits: PdfLimits,
    acroform: Option<AcroFormLimits>,
) -> Result<(DocumentSnapshot, Option<Result<AcroForm, AcroFormError>>), PdfError> {
    use sha2::{Digest, Sha256};

    let start = Instant::now();
    if bytes.len() > limits.max_bytes || limits.max_pages == 0 || limits.max_items_per_page == 0 {
        return Err(PdfError::LimitExceeded);
    }
    let mut timings = InspectionTimings::default();
    let parse_start = Instant::now();
    let doc = metadata::load(bytes).map_err(|_| PdfError::InvalidContainer)?;
    timings.parser_load_calls += 1;
    if doc.trailer.has(b"Encrypt") {
        return Err(PdfError::Encrypted);
    }
    let tree = metadata::page_tree(&doc, limits.max_pages).map_err(|error| match error {
        metadata::Fault::PageLimit => PdfError::LimitExceeded,
        _ => PdfError::InvalidPageTree,
    })?;
    let mut geometries = BTreeMap::new();
    let mut pages = Vec::with_capacity(tree.len());
    let mut font_scan = FontScan::new(&doc);
    let mut writing_modes = BTreeMap::new();
    for (index, object) in tree.iter().enumerate() {
        let page = index as u32 + 1;
        let geometry = metadata::page_metadata(&doc, *object).ok().and_then(|m| {
            // Preserve strict supported scope: partially intersected CropBoxes
            // need separate upstream agreement before being accepted.
            if m.crop_intersected {
                return None;
            }
            let r = |v: [f64; 4]| Rect::try_new(v[0], v[1], v[2], v[3]).ok();
            let rotation = match m.normalized_rotate {
                0 => Rotation::Deg0,
                90 => Rotation::Deg90,
                180 => Rotation::Deg180,
                270 => Rotation::Deg270,
                _ => return None,
            };
            PageGeometry::try_new(
                r(m.media.value)?,
                r(m.crop.value)?,
                rotation,
                m.user_unit.value,
            )
            .ok()
        });
        let mut reasons = vec![
            "text_fidelity_and_occlusion_unverified".into(),
            "native_geometry_unverified".into(),
            "upstream_internal_repair_count_unobservable".into(),
        ];
        if let Some(geometry) = geometry {
            geometries.insert(page, geometry);
            // Pages without geometry yield no fragments, so they need no scan.
            writing_modes.insert(page, font_scan.page(*object));
        } else {
            reasons.push("unsupported_metadata".into());
        }
        pages.push(PageDiagnostic {
            page,
            text_available: false,
            quality: Quality::Unknown,
            reason_codes: reasons,
        });
    }
    timings.metadata_ms = elapsed_ms(parse_start);
    let form = acroform.map(|limits| acroform::fields_in(&doc, limits));
    // Drop the metadata document before upstream loads its own representation.
    drop(doc);
    let analyze_start = Instant::now();
    timings.parser_load_calls += 1;
    let analysis = pdf_inspector::process_pdf_mem_with_options(
        bytes,
        PdfOptions::new().mode(ProcessMode::Analyze),
    );
    timings.quality_ms = elapsed_ms(analyze_start);
    if analysis
        .as_ref()
        .is_ok_and(|a| a.page_count != tree.len() as u32)
    {
        geometries.clear();
        for page in &mut pages {
            page.reason_codes
                .push("metadata_extractor_page_mismatch".into());
        }
    }
    let position_start = Instant::now();
    let filter: HashSet<u32> = geometries.keys().copied().collect();
    let mut items_by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    // Pages pdf-inspector re-based for mostly vertical text. This is the call
    // `extract_text_with_positions_mem_in_frame` wraps, so items are the same.
    let mut turns: HashMap<u32, PageRotation> = HashMap::new();
    if !filter.is_empty() {
        timings.parser_load_calls += 1;
        match pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
            bytes,
            Some(&filter),
            PositionFrame::Display,
        ) {
            Ok((items, page_turns)) => {
                turns.extend(page_turns);
                // Out-of-filter pages imply a disagreement, not invented metadata.
                if items.iter().any(|i| !filter.contains(&i.page)) {
                    geometries.clear();
                    for page in &mut pages {
                        page.reason_codes
                            .push("metadata_extractor_page_mismatch".into());
                    }
                } else {
                    for item in items {
                        items_by_page.entry(item.page).or_default().push(item);
                    }
                }
            }
            Err(_) => {
                let recovery_pages = pages
                    .iter_mut()
                    .filter(|page| filter.contains(&page.page))
                    .enumerate();
                for (attempt, page) in recovery_pages {
                    if attempt >= limits.max_recovery_pages as usize {
                        page.reason_codes
                            .push("positions_recovery_budget_exhausted".into());
                        continue;
                    }
                    timings.parser_load_calls += 1;
                    let one = HashSet::from([page.page]);
                    match pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
                        bytes,
                        Some(&one),
                        PositionFrame::Display,
                    ) {
                        Ok((items, page_turns)) if items.iter().all(|i| i.page == page.page) => {
                            items_by_page.insert(page.page, items);
                            if let Some(&turn) = page_turns.get(&page.page) {
                                turns.insert(page.page, turn);
                            }
                        }
                        _ => page.reason_codes.push("positions_failed".into()),
                    }
                }
            }
        }
    }
    timings.positions_ms = elapsed_ms(position_start);
    let document_id = hex(&Sha256::digest(bytes));
    let mut document_hash = Sha256::new();
    document_hash.update(document_id.as_bytes());
    let mut fragments = Vec::new();
    for page in &mut pages {
        let Some(geometry) = geometries.get(&page.page) else {
            continue;
        };
        let Some(items) = items_by_page.remove(&page.page) else {
            page.reason_codes.push("image_or_no_text".into());
            continue;
        };
        if items.len() > limits.max_items_per_page {
            page.reason_codes.push("item_limit_exceeded".into());
            continue;
        }
        let base_quality = match &analysis {
            Err(_) => {
                page.reason_codes.push("quality_analysis_failed".into());
                Quality::Unknown
            }
            Ok(a) if a.has_encoding_issues || !a.cmap_gaps.is_empty() => {
                // Family names cannot identify a unique font program or scope.
                page.reason_codes
                    .push("document_encoding_or_cmap_gaps_unassigned".into());
                Quality::Suspect
            }
            Ok(a) if a.pages_needing_ocr.contains(&page.page) => {
                page.reason_codes
                    .push("upstream_needs_ocr_no_ocr_performed".into());
                Quality::Suspect
            }
            Ok(_) => Quality::Usable,
        };
        // Conditions that leave every fragment of the page at most Unknown.
        let mut page_unknown = Vec::new();
        match writing_modes.get(&page.page) {
            Some(WritingMode::Vertical) => page_unknown.push("vertical_writing_mode_unsupported"),
            Some(WritingMode::Unverified) => page_unknown.push("writing_mode_scan_incomplete"),
            Some(WritingMode::Horizontal) | None => {}
        }
        if turns
            .get(&page.page)
            .is_some_and(|&turn| !turn_matches_display(turn, geometry.rotation))
        {
            page_unknown.push("page_text_rotated");
        }
        let base_quality = if base_quality == Quality::Usable && !page_unknown.is_empty() {
            Quality::Unknown
        } else {
            base_quality
        };
        page.reason_codes
            .extend(page_unknown.into_iter().map(String::from));
        page.quality = base_quality;
        let mut oblique = false;
        let mut script_joined_turn = false;
        let first_fragment = fragments.len();
        let mut page_hash = document_hash.clone();
        page_hash.update(page.page.to_be_bytes());
        for (item_index, item) in items.into_iter().enumerate() {
            if !matches!(item.item_type, ItemType::Text) || item.text.trim().is_empty() {
                continue;
            }
            let bbox = match geometry.extractor_rect_to_display(
                item.x as f64,
                item.y as f64,
                item.width as f64,
                item.height as f64,
            ) {
                Ok(bbox) => bbox,
                Err(_) => {
                    page.reason_codes.push("invalid_item_geometry".into());
                    if page.quality == Quality::Usable {
                        page.quality = Quality::Unknown;
                    }
                    continue;
                }
            };
            let quality = if item.legacy_symbol_rewrite
                || item.text.chars().any(suspect_character)
                || matches!(item.render_mode, Some(3 | 7))
                || white_paint(&item)
            {
                page.reason_codes.push("suspect_source_run".into());
                Quality::Suspect
            } else if !item.advance_known {
                page.reason_codes.push("estimated_advance".into());
                Quality::Unknown
            } else {
                base_quality
            };
            if quality == Quality::Suspect {
                page.quality = Quality::Suspect;
            } else if quality == Quality::Unknown && page.quality == Quality::Usable {
                page.quality = Quality::Unknown;
            }
            // A turned run is read in its own frame unless the script pass
            // may have joined it; an oblique one taints the page below
            // because it may hide inside a level neighbour.
            let (orientation, oblique_run) = match baseline(item.rotation) {
                Baseline::Right(orientation) => (orientation, false),
                Baseline::Oblique(nearest) => (nearest, true),
            };
            if !orientation.is_upright() || oblique_run {
                page.reason_codes.push("rotated_text_present".into());
            }
            let held = if oblique_run {
                oblique = true;
                true
            } else if !orientation.is_upright() && script_joined(&item) {
                page.reason_codes
                    .push("rotated_script_join_unverified".into());
                script_joined_turn = true;
                true
            } else {
                false
            };
            let quality = if held && quality == Quality::Usable {
                Quality::Unknown
            } else {
                quality
            };
            let mut hash = page_hash.clone();
            hash.update((item_index as u64).to_be_bytes());
            fragments.push(SourceFragment {
                id: hex(&hash.finalize()),
                page: page.page,
                item_index,
                raw_text: item.text,
                bbox,
                quality,
                orientation,
            });
        }
        // The script pass chains scripts of scripts and measures a chain's
        // shift from the body baseline, so a chain hung on a held script can
        // report no shift at all: once one turned run is held, every turned
        // run on the page is.
        if script_joined_turn {
            for fragment in &mut fragments[first_fragment..] {
                if !fragment.orientation.is_upright() && fragment.quality == Quality::Usable {
                    fragment.quality = Quality::Unknown;
                }
            }
        }
        if oblique {
            page.reason_codes.push("oblique_text_present".into());
            if page.quality == Quality::Usable {
                page.quality = Quality::Unknown;
            }
        }
        if page.quality != Quality::Usable {
            for fragment in &mut fragments[first_fragment..] {
                if fragment.quality == Quality::Usable {
                    fragment.quality = page.quality;
                }
            }
        }
        page.text_available = fragments[first_fragment..]
            .iter()
            .any(|f| f.quality == Quality::Usable);
        if fragments.len() == first_fragment {
            page.quality = Quality::Unknown;
            page.reason_codes.push("image_or_no_text".into());
        } else if !page.text_available {
            // Reachable with a Usable page only when every run is held back
            // on its own (a turned run the script pass may have joined).
            if page.quality == Quality::Usable {
                page.quality = Quality::Unknown;
            }
            page.reason_codes.push("no_usable_source_runs".into());
        }
        page.reason_codes.sort();
        page.reason_codes.dedup();
    }
    timings.total_ms = elapsed_ms(start);
    Ok((
        DocumentSnapshot {
            document_id,
            fragments,
            pages,
            geometries,
            timings,
        },
        form,
    ))
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}

fn suspect_character(c: char) -> bool {
    c == '\u{fffd}' || matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
}

fn white_paint(item: &TextItem) -> bool {
    let white = Some([255, 255, 255]);
    match item.render_mode {
        Some(0 | 4) => item.fill_color == white,
        Some(1 | 5) => item.stroke_color == white,
        Some(2 | 6) => item.fill_color == white && item.stroke_color == white,
        _ => false,
    }
}

/// How a run's rendered baseline lies relative to the page's x axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Baseline {
    /// Within the tolerance of a right angle; `Deg0` is ordinary
    /// left-to-right text.
    Right(Orientation),
    /// Any other angle, with the nearest right angle (`Deg0` for a
    /// non-finite one).
    Oblique(Orientation),
}

/// Classify a Display-frame `TextItem::rotation` (degrees, counter-clockwise
/// with y up, so 90° reads bottom to top on the rendered page).
fn baseline(rotation: f32) -> Baseline {
    if !rotation.is_finite() {
        return Baseline::Oblique(Orientation::Deg0);
    }
    let degrees = f64::from(rotation).rem_euclid(360.0);
    let quarter = (degrees / 90.0).round();
    let nearest = match quarter as u8 {
        1 => Orientation::Deg90,
        2 => Orientation::Deg180,
        3 => Orientation::Deg270,
        // 0, or 4 just below 360°.
        _ => Orientation::Deg0,
    };
    if (degrees - quarter * 90.0).abs() <= ROTATION_TOLERANCE_DEGREES {
        Baseline::Right(nearest)
    } else {
        Baseline::Oblique(nearest)
    }
}

/// Whether pdf-inspector's super/subscript pass (`merge_subscript_items`),
/// which ignores rotation, may have joined this run: it fuses digit runs
/// into a neighbour as the digits below, and gives a joined glyph chain a
/// baseline shift.
fn script_joined(item: &TextItem) -> bool {
    item.baseline_shift != 0.0
        || item.text.chars().any(|c| {
            matches!(c,
                '\u{00b2}' | '\u{00b3}' | '\u{00b9}'
                | '\u{2070}' | '\u{2074}'..='\u{2079}'
                | '\u{2080}'..='\u{2089}')
        })
}

/// Whether pdf-inspector's turned frame is the rendered page up to a shift.
/// `Ccw` maps `(x, y)` to `(y, -x)` and `/Rotate 90` turns the sheet the same
/// way clockwise; `Cw` and `/Rotate 270` likewise.
fn turn_matches_display(turn: PageRotation, rotation: Rotation) -> bool {
    matches!(
        (turn, rotation),
        (PageRotation::Upright, _)
            | (PageRotation::Ccw, Rotation::Deg90)
            | (PageRotation::Cw, Rotation::Deg270)
    )
}

/// Writing mode of the fonts a page's resources can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritingMode {
    Horizontal,
    /// A composite font with vertical writing mode is present.
    Vertical,
    /// The scan stopped at a bound or an unreadable structure.
    Unverified,
}

/// The scan could not classify every font.
#[derive(Debug, Clone, Copy)]
struct Unfinished;

/// `Ok(true)` once a vertical writing mode font is found.
type Found = Result<bool, Unfinished>;

/// Bounded scan for vertical writing mode fonts. Only page resources and Form
/// XObjects are followed: pdf-inspector reads no other text. A font in the
/// resources counts as used. Results are memoized per font and CMap object.
struct FontScan<'a> {
    doc: &'a Document,
    fonts: HashMap<ObjectId, Found>,
    cmaps: HashMap<ObjectId, Found>,
    cmap_bytes: usize,
}

/// Per-page bounds: visited nodes and objects (for cycles).
#[derive(Default)]
struct PageScan {
    nodes: usize,
    visited: HashSet<ObjectId>,
}

impl PageScan {
    fn visit(&mut self) -> Result<(), Unfinished> {
        self.nodes += 1;
        if self.nodes > MAX_FONT_SCAN_NODES_PER_PAGE {
            Err(Unfinished)
        } else {
            Ok(())
        }
    }

    /// False when `id` was already seen on this page.
    fn first_visit(&mut self, id: Option<ObjectId>) -> bool {
        id.is_none_or(|id| self.visited.insert(id))
    }
}

impl<'a> FontScan<'a> {
    fn new(doc: &'a Document) -> Self {
        Self {
            doc,
            fonts: HashMap::new(),
            cmaps: HashMap::new(),
            cmap_bytes: 0,
        }
    }

    fn page(&mut self, page: ObjectId) -> WritingMode {
        let mut scan = PageScan::default();
        let found = page_resources(self.doc, page).and_then(|resources| match resources {
            Some(resources) => self.resources(resources, 0, &mut scan),
            None => Ok(false),
        });
        match found {
            Ok(false) => WritingMode::Horizontal,
            Ok(true) => WritingMode::Vertical,
            Err(Unfinished) => WritingMode::Unverified,
        }
    }

    fn resources(&mut self, resources: &'a Object, depth: usize, scan: &mut PageScan) -> Found {
        let Some((resources, id)) = resolve(self.doc, resources)? else {
            return Ok(false);
        };
        if !scan.first_visit(id) {
            return Ok(false);
        }
        scan.visit()?;
        let Object::Dictionary(resources) = resources else {
            return Ok(false);
        };
        if let Some(Object::Dictionary(fonts)) = entry(self.doc, resources, b"Font")? {
            for (_, font) in fonts.iter() {
                scan.visit()?;
                if self.font(font)? {
                    return Ok(true);
                }
            }
        }
        if let Some(Object::Dictionary(xobjects)) = entry(self.doc, resources, b"XObject")? {
            for (_, xobject) in xobjects.iter() {
                scan.visit()?;
                let Some((Object::Stream(form), id)) = resolve(self.doc, xobject)? else {
                    continue;
                };
                if !scan.first_visit(id)
                    || !matches!(entry(self.doc, &form.dict, b"Subtype")?, Some(Object::Name(n)) if n == b"Form")
                {
                    continue;
                }
                // A form without its own resources uses ones already scanned.
                let Ok(inner) = form.dict.get(b"Resources") else {
                    continue;
                };
                if depth >= MAX_FORM_DEPTH {
                    return Err(Unfinished);
                }
                if self.resources(inner, depth + 1, scan)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn font(&mut self, font: &'a Object) -> Found {
        let Some((font, id)) = resolve(self.doc, font)? else {
            return Ok(false);
        };
        if let Some(known) = id.and_then(|id| self.fonts.get(&id)) {
            return *known;
        }
        let found = self.classify_font(font);
        if let Some(id) = id {
            self.fonts.insert(id, found);
        }
        found
    }

    /// Only composite fonts have a writing mode: an `/Encoding` CMap named
    /// `V` or `…-V` (`Identity-V`, `UniJIS-UCS2-V`, `90ms-RKSJ-V`, …), or an
    /// embedded CMap declaring `/WMode 1`.
    fn classify_font(&mut self, font: &'a Object) -> Found {
        let Object::Dictionary(font) = font else {
            return Ok(false);
        };
        let composite = matches!(entry(self.doc, font, b"Subtype")?, Some(Object::Name(n)) if n == b"Type0")
            || font.has(b"DescendantFonts");
        if !composite {
            return Ok(false);
        }
        // Not a standard key, but the one pdf-inspector itself reads.
        if is_one(entry(self.doc, font, b"WMode")?) {
            return Ok(true);
        }
        match font.get(b"Encoding") {
            Ok(encoding) => self.cmap(encoding, 0),
            Err(_) => Ok(false),
        }
    }

    fn cmap(&mut self, encoding: &'a Object, chain: usize) -> Found {
        let Some((encoding, id)) = resolve(self.doc, encoding)? else {
            return Ok(false);
        };
        let cmap = match encoding {
            Object::Name(name) => return Ok(vertical_cmap_name(name)),
            Object::Stream(cmap) => cmap,
            _ => return Ok(false),
        };
        if let Some(known) = id.and_then(|id| self.cmaps.get(&id)) {
            return *known;
        }
        if chain >= MAX_USECMAP_CHAIN {
            return Err(Unfinished);
        }
        let found = self.cmap_stream(cmap, chain);
        if let Some(id) = id {
            self.cmaps.insert(id, found);
        }
        found
    }

    fn cmap_stream(&mut self, cmap: &'a Stream, chain: usize) -> Found {
        if is_one(entry(self.doc, &cmap.dict, b"WMode")?) {
            return Ok(true);
        }
        if let Ok(parent) = cmap.dict.get(b"UseCMap")
            && self.cmap(parent, chain + 1)?
        {
            return Ok(true);
        }
        // The program may declare /WMode although its dictionary omits it.
        let remaining = MAX_CMAP_BYTES_PER_DOCUMENT.saturating_sub(self.cmap_bytes);
        let program = cmap
            .decompressed_content_with_limit(remaining.min(metadata::MAX_STREAM_BYTES))
            .map_err(|_| Unfinished)?;
        self.cmap_bytes = self
            .cmap_bytes
            .saturating_add(program.len().max(cmap.content.len()));
        Ok(declares_vertical_wmode(&program))
    }
}

/// The page's inheritable `/Resources`. The Parent chain was validated by
/// `metadata::page_metadata` for every page that has geometry.
fn page_resources(doc: &Document, page: ObjectId) -> Result<Option<&Object>, Unfinished> {
    let mut id = page;
    for _ in 0..metadata::MAX_DEPTH {
        let Some(Object::Dictionary(node)) = doc.objects.get(&id) else {
            return Ok(None);
        };
        if let Ok(resources) = node.get(b"Resources") {
            return Ok(Some(resources));
        }
        let Ok(Object::Reference(parent)) = node.get(b"Parent") else {
            return Ok(None);
        };
        id = *parent;
    }
    Err(Unfinished)
}

/// Follow references to a direct object and the last reference's id.
/// `Ok(None)` for a dangling reference, which the extractor cannot read either.
fn resolve<'a>(
    doc: &'a Document,
    mut object: &'a Object,
) -> Result<Option<(&'a Object, Option<ObjectId>)>, Unfinished> {
    let mut id = None;
    for _ in 0..=metadata::MAX_DEPTH {
        let Object::Reference(next) = object else {
            return Ok(Some((object, id)));
        };
        id = Some(*next);
        match doc.objects.get(next) {
            Some(target) => object = target,
            None => return Ok(None),
        }
    }
    Err(Unfinished)
}

fn entry<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Result<Option<&'a Object>, Unfinished> {
    match dict.get(key) {
        Ok(value) => Ok(resolve(doc, value)?.map(|(value, _)| value)),
        Err(_) => Ok(None),
    }
}

fn is_one(value: Option<&Object>) -> bool {
    match value {
        Some(Object::Integer(n)) => *n == 1,
        Some(Object::Real(n)) => *n == 1.0,
        _ => false,
    }
}

/// Predefined vertical CMaps are `V` (Adobe-Japan1 JIS) or end in `-V`.
fn vertical_cmap_name(name: &[u8]) -> bool {
    name == b"V" || name.ends_with(b"-V")
}

/// Whether a CMap program contains `/WMode 1`.
fn declares_vertical_wmode(program: &[u8]) -> bool {
    const KEY: &[u8] = b"/WMode";
    let white = |b: u8| matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ');
    let delimiter = |b: u8| white(b) || b"()<>[]{}/%".contains(&b);
    let mut start = 0;
    while let Some(offset) = program[start..]
        .windows(KEY.len())
        .position(|window| window == KEY)
    {
        let after = start + offset + KEY.len();
        let tail = &program[after..];
        let gap = tail.iter().take_while(|&&b| white(b)).count();
        if gap > 0 {
            let value = &tail[gap..];
            let end = value
                .iter()
                .position(|&b| delimiter(b))
                .unwrap_or(value.len());
            if &value[..end] == b"1" {
                return true;
            }
        }
        start = after;
    }
    false
}
