mod support;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use voltfill_core::pdf::{self, PdfLimits};

#[test]
fn source_ids_keep_the_original_document_page_and_item_encoding() {
    let mut pdf = lopdf::Document::load_mem(&support::pdf_with_text("ALPHA "))
        .expect("synthetic source PDF must load");
    let first = pdf.get_pages()[&1];
    let page = pdf
        .get_object(first)
        .expect("first page object must exist")
        .as_dict()
        .expect("page must be a dictionary")
        .clone();
    let parent = page
        .get(b"Parent")
        .expect("page must have a Parent")
        .as_reference()
        .expect("Parent must be an object reference");
    let content = page
        .get(b"Contents")
        .expect("page must have Contents")
        .as_reference()
        .expect("Contents must be an object reference");
    pdf.get_object_mut(content)
        .expect("referenced content object must exist")
        .as_stream_mut()
        .expect("content object must be a stream")
        .set_content(
            b"BT /F1 12 Tf 80 240 Td (ALPHA) Tj 0 -20 Td ( ) Tj 0 -20 Td (ALPHA) Tj ET".to_vec(),
        );
    let second = pdf.add_object(page);
    let pages = pdf
        .get_object_mut(parent)
        .expect("page tree root must exist")
        .as_dict_mut()
        .expect("page tree root must be a dictionary");
    pages.set("Kids", vec![lopdf::Object::Reference(first), second.into()]);
    pages.set("Count", 2);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes)
        .expect("two-page synthetic PDF must serialize");
    let snapshot =
        pdf::inspect(&bytes, PdfLimits::default()).expect("supported synthetic pages must inspect");
    let document_id: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(snapshot.document_id, document_id);
    assert_eq!(snapshot.fragments.len(), 4);
    let mut ids = BTreeSet::new();
    for fragment in &snapshot.fragments {
        // The pre-optimization ID contract, including the original item ordinal.
        let mut hash = Sha256::new();
        hash.update(document_id.as_bytes());
        hash.update(fragment.page.to_be_bytes());
        hash.update((fragment.item_index as u64).to_be_bytes());
        let expected: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(fragment.id, expected);
        assert!(ids.insert(&fragment.id));
        assert_eq!(fragment.raw_text, "ALPHA");
    }
    assert_eq!(
        snapshot
            .fragments
            .iter()
            .map(|f| (f.page, f.item_index))
            .collect::<Vec<_>>(),
        vec![(1, 0), (1, 1), (2, 0), (2, 1)]
    );
}
