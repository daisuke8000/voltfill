//! Reproducible synthetic microbenchmark for canonical Evidence lookup.
//! Not a PDF/Jev benchmark; output contains timings/counts only.
use serde_json::json;
use std::hint;
use std::time::Instant;
use voltfill_core::candidates::CandidateIndex;
use voltfill_core::geometry::Rect;
use voltfill_core::model::{Quality, SourceFragment, Strategy};
fn main() {
    let mut samples = Vec::new();
    for count in [1_000, 5_000, 10_000] {
        let fragments: Vec<_> = (0..count)
            .map(|i| SourceFragment {
                id: format!("synthetic_{i:08}"),
                page: 1,
                item_index: i,
                raw_text: "synthetic".into(),
                bbox: Rect::new(0., 0., 10., 10.)
                    .expect("benchmark rectangle must have positive finite extent"),
                quality: Quality::Usable,
            })
            .collect();
        for repetition in 0..3 {
            let start = Instant::now();
            let index = CandidateIndex::new(&fragments)
                .expect("benchmark fragments must have unique IDs and valid geometry");
            let build_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            for candidate in index.candidates() {
                let evidence = index
                    .evidence(candidate, "field", "question", Strategy::TextFirst)
                    .expect("unchanged indexed candidates must retain valid provenance");
                assert_eq!(evidence.page, 1);
                hint::black_box(evidence);
            }
            samples.push(json!({"candidates":count,"repetition":repetition,"index_build_ms":build_ms,"evidence_ms":start.elapsed().as_secs_f64()*1000.}));
        }
    }
    println!(
        "{}",
        json!({"build":"debug (unless caller overrides)","samples":samples,"limits":"Synthetic in-memory fragments; no PDF parser, API, native or whole-engine latency measurement."})
    );
}
