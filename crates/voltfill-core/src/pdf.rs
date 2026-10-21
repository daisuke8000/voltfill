use crate::{
    geometry::PageGeometry,
    model::{PageDiagnostic, SourceFragment, Timings},
};
pub use pdf_probe::{PdfError, PdfLimits, metadata};
use std::collections::BTreeMap;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentSnapshot {
    pub document_id: String,
    pub fragments: Vec<SourceFragment>,
    pub pages: Vec<PageDiagnostic>,
    pub geometries: BTreeMap<u32, PageGeometry>,
    pub timings: Timings,
}

pub fn inspect(bytes: &[u8], limits: PdfLimits) -> Result<DocumentSnapshot, PdfError> {
    let snapshot = pdf_probe::inspect(bytes, limits)?;
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
