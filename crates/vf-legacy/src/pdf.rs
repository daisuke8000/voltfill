use crate::geometry::PageGeometry;
use crate::model::{PageDiagnostic, Quality, SourceFragment, Timings};
use std::collections::BTreeMap;
pub use vf_source::{PdfError, PdfLimits, metadata};

/// Compatibility snapshot using core timings while retaining the original PDF inspection data.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentSnapshot {
    pub document_id: String,
    pub fragments: Vec<SourceFragment>,
    pub pages: Vec<PageDiagnostic>,
    pub geometries: BTreeMap<u32, PageGeometry>,
    pub timings: Timings,
}

/// Inspect locally without a renderer or Jev request, adapting the timing
/// representation and keeping turned text out (see [`upright_only`]).
pub fn inspect(bytes: &[u8], limits: PdfLimits) -> Result<DocumentSnapshot, PdfError> {
    let mut snapshot = vf_source::inspect(bytes, limits)?;
    upright_only(&mut snapshot.fragments, &mut snapshot.pages);
    Ok(DocumentSnapshot {
        document_id: snapshot.document_id,
        fragments: snapshot.fragments,
        pages: snapshot.pages,
        geometries: snapshot.geometries,
        timings: Timings {
            metadata_ms: snapshot.timings.metadata_ms,
            positions_ms: snapshot.timings.positions_ms,
            quality_ms: snapshot.timings.quality_ms,
            total_ms: snapshot.timings.total_ms,
            parser_load_calls: snapshot.timings.parser_load_calls,
            ..Default::default()
        },
    })
}

/// Keep this crate's input as it was before turned text became usable in
/// `vf_source`: a turned usable run is `Unknown`, and a page left with no
/// usable run has no text available, is `Unknown` and reports
/// `no_usable_source_runs`. This crate is frozen for the Text-versus-Vision
/// comparison.
fn upright_only(fragments: &mut [SourceFragment], pages: &mut [PageDiagnostic]) {
    for fragment in fragments.iter_mut() {
        if !fragment.orientation.is_upright() && fragment.quality == Quality::Usable {
            fragment.quality = Quality::Unknown;
        }
    }
    for page in pages.iter_mut().filter(|p| p.text_available) {
        let usable = fragments
            .iter()
            .any(|f| f.page == page.page && f.quality == Quality::Usable);
        if !usable {
            page.text_available = false;
            if page.quality == Quality::Usable {
                page.quality = Quality::Unknown;
            }
            page.reason_codes.push("no_usable_source_runs".into());
            page.reason_codes.sort();
            page.reason_codes.dedup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use vf_source::Orientation;

    fn fragment(id: &str, page: u32, orientation: Orientation) -> SourceFragment {
        SourceFragment {
            id: id.into(),
            page,
            item_index: 0,
            raw_text: "Ref: X-9".into(),
            bbox: Rect::new(0., 0., 10., 10.).expect("test rectangle is valid"),
            quality: Quality::Usable,
            orientation,
        }
    }

    fn page(page: u32) -> PageDiagnostic {
        PageDiagnostic {
            page,
            text_available: true,
            quality: Quality::Usable,
            reason_codes: vec!["rotated_text_present".into()],
        }
    }

    #[test]
    fn turned_runs_stay_out_of_legacy_input() {
        let mut fragments = vec![
            fragment("a", 1, Orientation::Deg0),
            fragment("b", 1, Orientation::Deg90),
            fragment("c", 2, Orientation::Deg180),
            fragment("d", 2, Orientation::Deg270),
        ];
        let mut pages = vec![page(1), page(2)];
        upright_only(&mut fragments, &mut pages);
        let qualities: Vec<Quality> = fragments.iter().map(|f| f.quality).collect();
        assert_eq!(
            qualities,
            [
                Quality::Usable,
                Quality::Unknown,
                Quality::Unknown,
                Quality::Unknown
            ]
        );
        assert!(pages[0].text_available);
        assert_eq!(pages[0].quality, Quality::Usable);
        assert_eq!(pages[0].reason_codes, ["rotated_text_present"]);
        assert!(!pages[1].text_available);
        assert_eq!(pages[1].quality, Quality::Unknown);
        assert_eq!(
            pages[1].reason_codes,
            ["no_usable_source_runs", "rotated_text_present"]
        );
    }
}
