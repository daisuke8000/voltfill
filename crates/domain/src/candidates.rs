use crate::geometry::Rect;
use crate::model::{Candidate, Evidence, Quality, SourceFragment, SourceSpan, Strategy};
use std::collections::BTreeMap;

/// Borrows a source snapshot and indexes usable runs without changing their order or text.
pub struct CandidateIndex<'source> {
    fragments: BTreeMap<&'source str, &'source SourceFragment>,
    candidates: Vec<Candidate>,
    candidate_positions: BTreeMap<&'source str, usize>,
    page_positions: BTreeMap<u32, Vec<usize>>,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum CandidateError {
    #[error("candidate provenance is invalid")]
    Provenance,
    #[error("candidate limit exceeded")]
    Limit,
}

impl<'source> CandidateIndex<'source> {
    /// Reject duplicate source IDs and invalid geometry on usable, nonempty runs.
    /// Compatibility name for [`Self::try_new`].
    pub fn new(fragments: &'source [SourceFragment]) -> Result<Self, CandidateError> {
        Self::try_new(fragments)
    }

    /// Index usable source runs after validating IDs and geometry.
    pub fn try_new(fragments: &'source [SourceFragment]) -> Result<Self, CandidateError> {
        let mut by_id = BTreeMap::new();
        let mut candidates = Vec::new();
        let mut candidate_positions = BTreeMap::new();
        let mut page_positions: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for f in fragments {
            if by_id.insert(f.id.as_str(), f).is_some() {
                return Err(CandidateError::Provenance);
            }
            if f.quality != Quality::Usable || f.raw_text.trim().is_empty() {
                continue;
            }
            f.bbox.validate().map_err(|_| CandidateError::Provenance)?;
            candidate_positions.insert(f.id.as_str(), candidates.len());
            page_positions
                .entry(f.page)
                .or_default()
                .push(candidates.len());
            candidates.push(Candidate {
                id: format!("c_{}", f.id),
                page: f.page,
                spans: vec![SourceSpan {
                    fragment_id: f.id.clone(),
                    start: 0,
                    end: f.raw_text.len(),
                }],
                bbox: f.bbox,
            });
        }
        Ok(Self {
            fragments: by_id,
            candidates,
            candidate_positions,
            page_positions,
        })
    }

    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Select requested pages in source order, rejecting results larger than `limit`.
    /// An empty page slice selects no candidates.
    pub fn scoped(&self, pages: &[u32], limit: usize) -> Result<Vec<&Candidate>, CandidateError> {
        if let [page] = pages {
            let positions = self.page_positions.get(page).map_or(&[][..], Vec::as_slice);
            if positions.len() > limit {
                return Err(CandidateError::Limit);
            }
            return Ok(positions.iter().map(|&i| &self.candidates[i]).collect());
        }
        let found: Vec<_> = self
            .candidates
            .iter()
            .filter(|c| pages.contains(&c.page))
            .take(limit.saturating_add(1))
            .collect();
        if found.len() > limit {
            return Err(CandidateError::Limit);
        }
        Ok(found)
    }

    /// Borrow exact UTF-8 slices, rejecting missing, empty, or invalid source spans.
    pub fn raw_fragments(&self, spans: &[SourceSpan]) -> Result<Vec<&'source str>, CandidateError> {
        spans
            .iter()
            .map(|s| {
                self.fragments
                    .get(s.fragment_id.as_str())
                    .and_then(|f| f.raw_text.get(s.start..s.end))
                    .filter(|s| !s.is_empty())
                    .ok_or(CandidateError::Provenance)
            })
            .collect()
    }

    pub fn text(&self, candidate: &Candidate) -> Result<String, CandidateError> {
        Ok(self.raw_fragments(&candidate.spans)?.join("\n"))
    }

    /// Create provenance only for an unchanged candidate from this index.
    pub fn evidence(
        &self,
        candidate: &Candidate,
        field_id: &str,
        question_id: &str,
        strategy: Strategy,
    ) -> Result<Evidence, CandidateError> {
        // Full-run candidate IDs are c_<fragment ID>; borrow the original ID
        // as the lookup key instead of cloning every ID into a second index.
        let position = candidate
            .id
            .strip_prefix("c_")
            .and_then(|id| self.candidate_positions.get(id))
            .ok_or(CandidateError::Provenance)?;
        let canonical = &self.candidates[*position];
        if candidate != canonical {
            return Err(CandidateError::Provenance);
        }
        Ok(Evidence {
            id: format!(
                "ev_{}_{}:{}_{}:{}",
                candidate.id,
                field_id.len(),
                field_id,
                question_id.len(),
                question_id
            ),
            field_id: field_id.into(),
            page: candidate.page,
            candidate_id: Some(candidate.id.clone()),
            spans: candidate.spans.clone(),
            raw_fragments: self
                .raw_fragments(&candidate.spans)?
                .into_iter()
                .map(str::to_owned)
                .collect(),
            display_bbox: candidate.bbox,
            bbox_precision: "text_run_approximate",
            question_id: question_id.into(),
            strategy,
            image_id: None,
            image_transform: None,
        })
    }

    /// Return whole source runs with a positive-area intersection, in snapshot order.
    pub fn overlapping(&self, page: u32, bbox: Rect) -> Result<Vec<&Candidate>, CandidateError> {
        bbox.validate().map_err(|_| CandidateError::Provenance)?;
        Ok(self
            .on_page(page)
            .filter(|c| {
                let r = c.bbox;
                bbox.left < r.right
                    && r.left < bbox.right
                    && bbox.top < r.bottom
                    && r.top < bbox.bottom
            })
            .collect())
    }

    /// Never split a text run by a guessed character-width ratio.
    pub fn in_box(&self, page: u32, bbox: Rect) -> Result<Vec<&Candidate>, CandidateError> {
        bbox.validate().map_err(|_| CandidateError::Provenance)?;
        let intersects = |r: Rect| {
            bbox.left < r.right && r.left < bbox.right && bbox.top < r.bottom && r.top < bbox.bottom
        };
        let mut result = Vec::new();
        for c in self.on_page(page) {
            if intersects(c.bbox) {
                if !bbox.contains(c.bbox) {
                    return Err(CandidateError::Provenance);
                }
                result.push(c);
            }
        }
        Ok(result)
    }

    fn on_page(&self, page: u32) -> impl Iterator<Item = &Candidate> {
        self.page_positions
            .get(&page)
            .into_iter()
            .flatten()
            .map(|&i| &self.candidates[i])
    }
}
