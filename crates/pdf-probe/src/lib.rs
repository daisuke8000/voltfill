//! Native-free PDF snapshots. Usable means no observed issue, never verified
//! glyph fidelity, semantic ToUnicode correctness, or absence of overpainting.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod geometry;
pub mod metadata;
mod source;
pub use source::{PageDiagnostic, Quality, SourceFragment};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InspectionTimings {
    pub metadata_ms: f64,
    pub positions_ms: f64,
    pub quality_ms: f64,
    pub total_ms: f64,
    pub parser_load_calls: u32,
}

use crate::geometry::{PageGeometry, Rect, Rotation};
use pdf_inspector::types::ItemType;
use pdf_inspector::{
    PdfOptions, PositionFrame, ProcessMode, TextItem, extract_text_with_positions_mem_in_frame,
    process_pdf_mem_with_options,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::time::Instant;
use thiserror::Error;

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

#[derive(Debug, Error)]
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
pub fn inspect(bytes: &[u8], limits: PdfLimits) -> Result<DocumentSnapshot, PdfError> {
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
    for (index, object) in tree.iter().enumerate() {
        let page = index as u32 + 1;
        let geometry = metadata::page_metadata(&doc, *object).ok().and_then(|m| {
            // Preserve strict supported scope: partially intersected CropBoxes
            // need separate upstream agreement before being accepted.
            if m.crop_intersected {
                return None;
            }
            let r = |v: [f64; 4]| Rect::new(v[0], v[1], v[2], v[3]).ok();
            let rotation = match m.normalized_rotate {
                0 => Rotation::Deg0,
                90 => Rotation::Deg90,
                180 => Rotation::Deg180,
                270 => Rotation::Deg270,
                _ => return None,
            };
            PageGeometry::new(
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
    // Drop the metadata document before upstream loads its own representation.
    drop(doc);
    let analyze_start = Instant::now();
    timings.parser_load_calls += 1;
    let analysis =
        process_pdf_mem_with_options(bytes, PdfOptions::new().mode(ProcessMode::Analyze));
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
    if !filter.is_empty() {
        timings.parser_load_calls += 1;
        match extract_text_with_positions_mem_in_frame(bytes, Some(&filter), PositionFrame::Display)
        {
            Ok(items) => {
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
                for (attempt, page) in pages
                    .iter_mut()
                    .filter(|p| filter.contains(&p.page))
                    .enumerate()
                {
                    if attempt >= limits.max_recovery_pages as usize {
                        page.reason_codes
                            .push("positions_recovery_budget_exhausted".into());
                        continue;
                    }
                    timings.parser_load_calls += 1;
                    let one = HashSet::from([page.page]);
                    match extract_text_with_positions_mem_in_frame(
                        bytes,
                        Some(&one),
                        PositionFrame::Display,
                    ) {
                        Ok(items) if items.iter().all(|i| i.page == page.page) => {
                            items_by_page.insert(page.page, items);
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
        page.quality = base_quality;
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
            let mut hash = page_hash.clone();
            hash.update((item_index as u64).to_be_bytes());
            fragments.push(SourceFragment {
                id: hex(&hash.finalize()),
                page: page.page,
                item_index,
                raw_text: item.text,
                bbox,
                quality,
            });
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
            page.reason_codes.push("no_usable_source_runs".into());
        }
        page.reason_codes.sort();
        page.reason_codes.dedup();
    }
    timings.total_ms = elapsed_ms(start);
    Ok(DocumentSnapshot {
        document_id,
        fragments,
        pages,
        geometries,
        timings,
    })
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
