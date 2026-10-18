//! Local source geometry and synthetic extraction; no API or rendering calls.
use voltfill_core::{
    candidates::CandidateIndex,
    geometry::Rect,
    model::{Quality, SourceFragment, Strategy},
};

fn fragment(id: &str, item_index: usize, text: &str, bbox: Rect) -> SourceFragment {
    SourceFragment {
        id: id.into(),
        page: 1,
        item_index,
        raw_text: text.into(),
        bbox,
        quality: Quality::Usable,
    }
}

#[test]
fn any_positive_overlap_keeps_the_full_unicode_run_but_touching_does_not() {
    let raw = [fragment(
        "unicode",
        0,
        "東京🦀e\u{301}\n原文全体",
        Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
    )];
    let index = CandidateIndex::new(&raw).unwrap();
    let just_below_ten = f64::from_bits(10.0_f64.to_bits() - 1);
    let selected = index
        .overlapping(1, Rect::new(just_below_ten, 0.0, 11.0, 10.0).unwrap())
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(index.text(selected[0]).unwrap(), raw[0].raw_text);
    assert_eq!(selected[0].spans[0].start, 0);
    assert_eq!(selected[0].spans[0].end, raw[0].raw_text.len());
    let evidence = index
        .evidence(selected[0], "f", "box", Strategy::VisionFirst)
        .unwrap();
    assert_eq!(evidence.raw_fragments, vec![raw[0].raw_text.clone()]);
    assert_eq!(evidence.display_bbox, raw[0].bbox);
    for touching in [
        Rect::new(10.0, 0.0, 20.0, 10.0).unwrap(),
        Rect::new(0.0, 10.0, 10.0, 20.0).unwrap(),
        Rect::new(10.0, 10.0, 20.0, 20.0).unwrap(),
    ] {
        assert!(index.overlapping(1, touching).unwrap().is_empty());
    }
}

#[test]
fn overlapping_preserves_source_order_duplicates_and_distinct_evidence() {
    // Source order differs from visual left-to-right order and identifier order.
    let mut raw = vec![
        fragment("z", 2, "同じ値", Rect::new(20.0, 0.0, 30.0, 10.0).unwrap()),
        fragment("a", 7, "同じ値", Rect::new(0.0, 0.0, 10.0, 10.0).unwrap()),
        fragment(
            "other_page",
            0,
            "他ページ",
            Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
        ),
        fragment(
            "suspect",
            8,
            "不確か",
            Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
        ),
        fragment(
            "unknown",
            9,
            "不明",
            Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
        ),
    ];
    raw[2].page = 2;
    raw[3].quality = Quality::Suspect;
    raw[4].quality = Quality::Unknown;
    let index = CandidateIndex::new(&raw).unwrap();
    let selected = index
        .overlapping(1, Rect::new(0.0, 0.0, 30.0, 10.0).unwrap())
        .unwrap();
    assert_eq!(
        selected.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["c_z", "c_a"]
    );
    assert_eq!(
        selected
            .iter()
            .map(|c| index.text(c).unwrap())
            .collect::<Vec<_>>(),
        ["同じ値", "同じ値"]
    );
    let evidence = selected
        .iter()
        .map(|c| {
            index
                .evidence(c, "f", "box", Strategy::VisionFirst)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_ne!(evidence[0].id, evidence[1].id);
    assert_eq!(evidence[0].display_bbox, raw[0].bbox);
    assert_eq!(evidence[1].display_bbox, raw[1].bbox);
    assert_eq!(evidence[0].spans[0].fragment_id, "z");
    assert_eq!(evidence[1].spans[0].fragment_id, "a");
}
