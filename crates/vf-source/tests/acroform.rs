//! AcroForm extraction over synthetic, offline PDFs built with lopdf.
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};
use vf_source::acroform::{
    AcroForm, AcroFormError, AcroFormLimit, AcroFormLimits, ChoiceValue, FieldIssue, FieldKind,
    FieldValue, FormField, SkipReason, ValueIssue, WidgetIssue, XfaPresence, fields, fields_in,
};
use vf_source::geometry::Rect;

struct TestPdf {
    doc: Document,
    pages_id: ObjectId,
    pages: Vec<ObjectId>,
}

impl TestPdf {
    fn new(pages: usize) -> Self {
        Self::with_pages(vec![dictionary! {}; pages])
    }

    /// US Letter pages; each extra dictionary overrides or adds page entries.
    fn with_pages(extras: Vec<Dictionary>) -> Self {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let pages = extras
            .into_iter()
            .map(|extra| {
                let mut page = dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                    "Annots" => Vec::<Object>::new(),
                };
                for (key, value) in extra.iter() {
                    page.set(key.clone(), value.clone());
                }
                doc.add_object(page)
            })
            .collect();
        Self {
            doc,
            pages_id,
            pages,
        }
    }

    fn add(&mut self, dict: Dictionary) -> ObjectId {
        self.doc.add_object(dict)
    }

    fn reserve(&mut self) -> ObjectId {
        self.doc.new_object_id()
    }

    fn set(&mut self, id: ObjectId, dict: Dictionary) {
        self.doc.objects.insert(id, Object::Dictionary(dict));
    }

    fn annotate(&mut self, page: usize, annotation: ObjectId) {
        let page = self.pages[page];
        self.doc
            .get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .get_mut(b"Annots")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(annotation.into());
    }

    /// A widget dictionary listed in `page`'s /Annots, merged with `extra`.
    fn widget(&mut self, page: usize, rect: [i64; 4], extra: Dictionary) -> ObjectId {
        let mut dict = widget_dict(rect);
        for (key, value) in extra.iter() {
            dict.set(key.clone(), value.clone());
        }
        let id = self.add(dict);
        self.annotate(page, id);
        id
    }

    /// An /AP dictionary whose normal appearance has the given state names.
    fn appearance(&mut self, states: &[&str]) -> Dictionary {
        let stream = self.doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        let mut normal = Dictionary::new();
        for state in states {
            normal.set(*state, stream);
        }
        dictionary! { "N" => normal }
    }

    fn finish(mut self, acroform: Option<Dictionary>) -> Vec<u8> {
        let kids: Vec<Object> = self.pages.iter().map(|id| (*id).into()).collect();
        let count = self.pages.len() as i64;
        self.doc.objects.insert(
            self.pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
        );
        let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => self.pages_id };
        if let Some(form) = acroform {
            let form = self.doc.add_object(form);
            catalog.set("AcroForm", form);
        }
        let catalog = self.doc.add_object(catalog);
        self.doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        self.doc.save_to(&mut bytes).unwrap();
        bytes
    }
}

fn widget_dict(rect: [i64; 4]) -> Dictionary {
    dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Rect" => rect.iter().map(|v| Object::from(*v)).collect::<Vec<_>>(),
    }
}

fn form(fields: &[ObjectId]) -> Dictionary {
    dictionary! { "Fields" => refs(fields) }
}

fn refs(ids: &[ObjectId]) -> Vec<Object> {
    ids.iter().map(|id| (*id).into()).collect()
}

fn literal(bytes: &[u8]) -> Object {
    Object::String(bytes.to_vec(), StringFormat::Literal)
}

fn utf16(text: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

fn name(text: &str) -> Object {
    Object::Name(text.as_bytes().to_vec())
}

fn extract(bytes: &[u8]) -> AcroForm {
    fields(bytes, AcroFormLimits::default()).unwrap()
}

fn field<'a>(form: &'a AcroForm, name: &str) -> &'a FormField {
    form.fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| panic!("field {name} missing from {form:#?}"))
}

fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Rect {
    Rect::try_new(left, top, right, bottom).unwrap()
}

fn text(value: &str) -> Option<FieldValue> {
    Some(FieldValue::Text(value.into()))
}

fn state(value: &str) -> Option<FieldValue> {
    Some(FieldValue::State(value.into()))
}

#[test]
fn text_field_decodes_utf16be_value_and_maps_widget_to_display() {
    let mut pdf = TestPdf::new(1);
    let invoice = pdf.widget(
        0,
        [300, 720, 100, 700],
        dictionary! {
            "T" => literal(b"invoice_no"),
            "TU" => utf16("請求書番号"),
            "FT" => "Tx",
            "V" => utf16("請求 INV-2026-001"),
            "MaxLen" => 20,
        },
    );
    let form = extract(&pdf.finish(Some(form(&[invoice]))));

    assert!(form.present);
    assert_eq!(form.xfa, XfaPresence::Absent);
    assert!(form.skipped.is_empty());
    assert_eq!(form.fields.len(), 1);
    let field = &form.fields[0];
    assert_eq!(field.name, "invoice_no");
    assert_eq!(field.alternate_name.as_deref(), Some("請求書番号"));
    assert_eq!(field.kind, FieldKind::Text);
    assert_eq!(field.value, text("請求 INV-2026-001"));
    assert_eq!(field.default_value, None);
    assert_eq!(field.max_len, Some(20));
    assert_eq!(field.object, Some(invoice));
    assert!(!field.read_only && !field.required);
    assert!(field.issues.is_empty());
    let [widget] = field.widgets.as_slice() else {
        panic!("one widget expected: {field:#?}");
    };
    assert_eq!(widget.object, Some(invoice));
    assert_eq!(widget.page, Some(1));
    // The /Rect corners are given in reverse order and normalized.
    assert_eq!(widget.pdf_rect, Some(rect(100.0, 700.0, 300.0, 720.0)));
    assert_eq!(widget.rect, Some(rect(100.0, 72.0, 300.0, 92.0)));
    assert!(!widget.hidden);
    assert_eq!(widget.on_state, None);
    assert!(widget.issues.is_empty());
}

#[test]
fn text_values_decode_pdf_doc_encoding_and_utf8_and_reject_inexact_bytes() {
    let mut pdf = TestPdf::new(1);
    // Streams are indirect objects; a text stream value is referenced.
    let stream = pdf
        .doc
        .add_object(Stream::new(dictionary! {}, b"text".to_vec()));
    let fields = [
        ("pdfdoc", literal(b"Total \x80 \x8B caf\xE9")),
        ("utf8", literal(b"\xEF\xBB\xBF\xE5\x80\xA4")),
        ("undefined_byte", literal(b"bad\x7Fbyte")),
        ("odd_utf16", literal(b"\xFE\xFF\x00\x41\x00")),
        ("number", Object::Integer(42)),
        ("stream", stream.into()),
    ]
    .map(|(t, v)| {
        pdf.widget(
            0,
            [10, 10, 20, 20],
            dictionary! { "T" => literal(t.as_bytes()), "FT" => "Tx", "V" => v },
        )
    });
    let form = extract(&pdf.finish(Some(form(&fields))));

    assert_eq!(field(&form, "pdfdoc").value, text("Total • ‰ café"));
    // High PDFDocEncoding bytes may be another encoding (Shift_JIS) misread.
    assert_eq!(field(&form, "pdfdoc").issues, [FieldIssue::PdfDocHighBytes]);
    assert_eq!(field(&form, "utf8").value, text("値"));
    assert!(field(&form, "utf8").issues.is_empty());
    for (name, issue) in [
        ("undefined_byte", ValueIssue::Undecodable),
        ("odd_utf16", ValueIssue::Undecodable),
        ("number", ValueIssue::UnexpectedType),
        ("stream", ValueIssue::UnsupportedStream),
    ] {
        let field = field(&form, name);
        assert_eq!(field.value, None, "{name}");
        assert_eq!(field.issues, [FieldIssue::Value(issue)], "{name}");
    }
}

#[test]
fn nested_fields_inherit_type_flags_and_value_through_kids() {
    let mut pdf = TestPdf::new(1);
    let customer = pdf.reserve();
    let name_field = pdf.reserve();
    let name_widget = pdf.widget(
        0,
        [100, 600, 300, 620],
        dictionary! { "Parent" => name_field },
    );
    pdf.set(
        name_field,
        dictionary! { "T" => literal(b"name"), "Parent" => customer, "Kids" => refs(&[name_widget]) },
    );
    let code = pdf.widget(
        0,
        [100, 560, 300, 580],
        dictionary! { "T" => literal(b"code"), "Parent" => customer, "V" => literal(b"C-9") },
    );
    pdf.set(
        customer,
        dictionary! {
            "T" => literal(b"customer"),
            "FT" => "Tx",
            "Ff" => 1,
            "V" => literal(b"ACME"),
            "Kids" => refs(&[name_field, code]),
        },
    );
    let order = pdf.reserve();
    let line = pdf.reserve();
    let amount = pdf.widget(
        0,
        [100, 500, 300, 520],
        dictionary! { "T" => literal(b"amount"), "V" => literal(b"1,200") },
    );
    pdf.set(
        line,
        dictionary! { "T" => literal(b"line"), "Kids" => refs(&[amount]) },
    );
    pdf.set(
        order,
        dictionary! { "T" => literal(b"order"), "FT" => "Tx", "MaxLen" => 8, "Kids" => refs(&[line]) },
    );
    let form = extract(&pdf.finish(Some(form(&[customer, order]))));

    let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["customer.name", "customer.code", "order.line.amount"]
    );
    let inherited = field(&form, "customer.name");
    assert_eq!(inherited.value, text("ACME"));
    assert!(inherited.read_only);
    assert_eq!(inherited.object, Some(name_field));
    assert_eq!(inherited.widgets.len(), 1);
    assert_eq!(inherited.widgets[0].object, Some(name_widget));
    assert_eq!(inherited.widgets[0].page, Some(1));
    let own = field(&form, "customer.code");
    assert_eq!(own.value, text("C-9"));
    assert!(own.read_only);
    let deep = field(&form, "order.line.amount");
    assert_eq!(deep.kind, FieldKind::Text);
    assert_eq!(deep.value, text("1,200"));
    assert_eq!(deep.max_len, Some(8));
    assert!(!deep.read_only);
}

#[test]
fn choice_fields_report_selected_export_values_with_display_text() {
    let mut pdf = TestPdf::new(1);
    let options = vec![
        vec![literal(b"JP"), literal(b"Japan")].into(),
        vec![literal(b"US"), utf16("United States")].into(),
        literal(b"Other"),
    ];
    let country = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! {
            "T" => literal(b"country"),
            "FT" => "Ch",
            "Ff" => 1 << 17,
            "Opt" => options.clone(),
            "V" => literal(b"JP"),
            "DV" => literal(b"US"),
        },
    );
    let tags = pdf.widget(
        0,
        [10, 30, 20, 40],
        dictionary! {
            "T" => literal(b"tags"),
            "FT" => "Ch",
            "Ff" => 1 << 21,
            "Opt" => vec![literal(b"a"), literal(b"b"), literal(b"c")],
            "V" => vec![literal(b"a"), literal(b"c")],
            "I" => vec![0.into(), 1.into()],
        },
    );
    let editable = pdf.widget(
        0,
        [10, 50, 20, 60],
        dictionary! {
            "T" => literal(b"editable"),
            "FT" => "Ch",
            "Ff" => (1 << 17) | (1 << 18),
            "Opt" => options,
            "V" => literal(b"Free text"),
        },
    );
    let unselected = pdf.widget(
        0,
        [10, 70, 20, 80],
        dictionary! { "T" => literal(b"unselected"), "FT" => "Ch", "I" => vec![0.into()] },
    );
    let form = extract(&pdf.finish(Some(form(&[country, tags, editable, unselected]))));

    let choice = |pairs: &[(&str, Option<&str>)]| {
        Some(FieldValue::Choice(
            pairs
                .iter()
                .map(|(value, display)| ChoiceValue {
                    value: (*value).into(),
                    display: display.map(Into::into),
                })
                .collect(),
        ))
    };
    let country = field(&form, "country");
    assert_eq!(country.kind, FieldKind::Choice);
    assert_eq!(country.value, choice(&[("JP", Some("Japan"))]));
    assert_eq!(
        country.default_value,
        choice(&[("US", Some("United States"))])
    );
    assert_eq!(
        field(&form, "tags").value,
        choice(&[("a", None), ("c", None)])
    );
    assert_eq!(
        field(&form, "editable").value,
        choice(&[("Free text", None)])
    );
    // /I alone never produces a value.
    assert_eq!(field(&form, "unselected").value, None);
}

#[test]
fn checkboxes_report_on_and_off_state_names() {
    let mut pdf = TestPdf::new(1);
    let yes = pdf.appearance(&["Yes", "Off"]);
    let accept = pdf.appearance(&["Off", "Accept"]);
    let agree = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "T" => literal(b"agree"), "FT" => "Btn", "V" => name("Yes"), "AS" => name("Yes"), "AP" => yes.clone() },
    );
    let decline = pdf.widget(
        0,
        [30, 10, 40, 20],
        dictionary! { "T" => literal(b"decline"), "FT" => "Btn", "V" => name("Off"), "AP" => accept },
    );
    let unset = pdf.widget(
        0,
        [50, 10, 60, 20],
        dictionary! { "T" => literal(b"unset"), "FT" => "Btn", "DV" => name("Off"), "AP" => yes.clone() },
    );
    let string_state = pdf.widget(
        0,
        [70, 10, 80, 20],
        dictionary! { "T" => literal(b"string_state"), "FT" => "Btn", "V" => literal(b"Yes"), "AP" => yes },
    );
    let form = extract(&pdf.finish(Some(form(&[agree, decline, unset, string_state]))));

    let agree = field(&form, "agree");
    assert_eq!(agree.kind, FieldKind::Checkbox);
    assert_eq!(agree.value, state("Yes"));
    assert_eq!(agree.widgets[0].on_state.as_deref(), Some("Yes"));
    let decline = field(&form, "decline");
    assert_eq!(decline.value, state("Off"));
    assert_eq!(decline.widgets[0].on_state.as_deref(), Some("Accept"));
    let unset = field(&form, "unset");
    assert_eq!(unset.value, None);
    assert_eq!(unset.default_value, state("Off"));
    let string_state = field(&form, "string_state");
    assert_eq!(string_state.value, None);
    assert_eq!(
        string_state.issues,
        [FieldIssue::Value(ValueIssue::UnexpectedType)]
    );
}

#[test]
fn radio_group_reports_selected_state_and_each_widget_on_state() {
    let mut pdf = TestPdf::new(1);
    let plan = pdf.reserve();
    let basic_ap = pdf.appearance(&["Basic", "Off"]);
    let pro_ap = pdf.appearance(&["Pro", "Off"]);
    let basic = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "Parent" => plan, "AS" => name("Off"), "AP" => basic_ap },
    );
    let pro = pdf.widget(
        0,
        [30, 10, 40, 20],
        dictionary! { "Parent" => plan, "AS" => name("Pro"), "AP" => pro_ap },
    );
    pdf.set(
        plan,
        dictionary! {
            "T" => literal(b"plan"),
            "FT" => "Btn",
            "Ff" => (1 << 15) | (1 << 14),
            "V" => name("Pro"),
            "Kids" => refs(&[basic, pro]),
        },
    );
    let form = extract(&pdf.finish(Some(form(&[plan]))));

    let [plan_field] = form.fields.as_slice() else {
        panic!("one radio group expected: {form:#?}");
    };
    assert_eq!(plan_field.name, "plan");
    assert_eq!(plan_field.kind, FieldKind::Radio);
    assert_eq!(plan_field.value, state("Pro"));
    assert_eq!(plan_field.object, Some(plan));
    let on_states: Vec<_> = plan_field
        .widgets
        .iter()
        .map(|w| (w.object, w.on_state.as_deref()))
        .collect();
    assert_eq!(
        on_states,
        [(Some(basic), Some("Basic")), (Some(pro), Some("Pro"))]
    );
}

#[test]
fn flags_and_unsupported_field_types() {
    let mut pdf = TestPdf::new(1);
    let mut field_widget = |t: &[u8], extra: Dictionary| {
        let mut dict = dictionary! { "T" => literal(t) };
        for (key, value) in extra.iter() {
            dict.set(key.clone(), value.clone());
        }
        pdf.widget(0, [10, 10, 20, 20], dict)
    };
    let ids = [
        field_widget(
            b"locked",
            dictionary! { "FT" => "Tx", "Ff" => 1, "V" => literal(b"x") },
        ),
        field_widget(b"required", dictionary! { "FT" => "Tx", "Ff" => 2 }),
        field_widget(
            b"secret",
            dictionary! { "FT" => "Tx", "Ff" => 1 << 13, "V" => literal(b"hunter2"), "DV" => literal(b"x") },
        ),
        field_widget(b"submit", dictionary! { "FT" => "Btn", "Ff" => 1 << 16 }),
        field_widget(b"signature", dictionary! { "FT" => "Sig" }),
        field_widget(b"untyped", dictionary! {}),
        field_widget(b"odd", dictionary! { "FT" => "Zz" }),
        field_widget(b"bad\x7Fname", dictionary! { "FT" => "Tx" }),
        field_widget(b"a.b", dictionary! { "FT" => "Tx", "TU" => 7 }),
        field_widget(b"neg", dictionary! { "FT" => "Tx", "MaxLen" => -1 }),
    ];
    let form = extract(&pdf.finish(Some(form(&ids))));

    let locked = field(&form, "locked");
    assert!(locked.read_only && !locked.required);
    let required = field(&form, "required");
    assert!(required.required && !required.read_only);
    let secret = field(&form, "secret");
    assert_eq!(secret.value, None);
    assert_eq!(secret.default_value, None);
    assert_eq!(secret.issues, [FieldIssue::PasswordValueWithheld]);
    assert_eq!(
        field(&form, "a.b").issues,
        [
            FieldIssue::PeriodInPartialName,
            FieldIssue::InvalidAlternateName
        ]
    );
    assert_eq!(field(&form, "neg").issues, [FieldIssue::InvalidMaxLen]);
    let skipped: Vec<_> = form
        .skipped
        .iter()
        .map(|s| (s.name.as_deref(), s.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            (Some("submit"), SkipReason::PushButton),
            (Some("signature"), SkipReason::Signature),
            (Some("untyped"), SkipReason::MissingType),
            (Some("odd"), SkipReason::UnsupportedType),
            (None, SkipReason::UndecodableName),
        ]
    );
    assert_eq!(form.fields.len(), 5);
}

#[test]
fn one_field_reports_widgets_on_different_pages() {
    let mut pdf = TestPdf::new(2);
    let total = pdf.reserve();
    let first = pdf.widget(0, [400, 100, 500, 120], dictionary! { "Parent" => total });
    let second = pdf.widget(1, [400, 700, 500, 720], dictionary! { "Parent" => total });
    pdf.set(
        total,
        dictionary! { "T" => literal(b"total"), "FT" => "Tx", "V" => literal(b"1,000"), "Kids" => refs(&[first, second]) },
    );
    let form = extract(&pdf.finish(Some(form(&[total]))));

    let total = field(&form, "total");
    assert_eq!(total.value, text("1,000"));
    let placements: Vec<_> = total.widgets.iter().map(|w| (w.page, w.rect)).collect();
    assert_eq!(
        placements,
        [
            (Some(1), Some(rect(400.0, 672.0, 500.0, 692.0))),
            (Some(2), Some(rect(400.0, 72.0, 500.0, 92.0))),
        ]
    );
}

#[test]
fn widget_rects_follow_page_rotation_and_crop_box() {
    let mut pdf = TestPdf::with_pages(vec![
        dictionary! { "Rotate" => 90 },
        dictionary! { "CropBox" => vec![50.into(), 50.into(), 562.into(), 742.into()] },
        dictionary! { "Rotate" => 270 },
    ]);
    let rotated = pdf.widget(
        0,
        [100, 200, 300, 250],
        dictionary! { "T" => literal(b"rotated"), "FT" => "Tx" },
    );
    let cropped = pdf.widget(
        1,
        [100, 700, 300, 720],
        dictionary! { "T" => literal(b"cropped"), "FT" => "Tx" },
    );
    let outside = pdf.widget(
        1,
        [0, 0, 40, 40],
        dictionary! { "T" => literal(b"outside"), "FT" => "Tx", "F" => 2 },
    );
    let counter = pdf.widget(
        2,
        [100, 200, 300, 250],
        dictionary! { "T" => literal(b"counter"), "FT" => "Tx" },
    );
    let form = extract(&pdf.finish(Some(form(&[rotated, cropped, outside, counter]))));

    let only = |name: &str| field(&form, name).widgets[0].clone();
    // Display x is PDF y; Display y is PDF x for a quarter turn clockwise.
    assert_eq!(only("rotated").rect, Some(rect(200.0, 100.0, 250.0, 300.0)));
    // Display width 792 after the turn: x = 792 - y, y = 612 - x.
    assert_eq!(only("counter").rect, Some(rect(542.0, 312.0, 592.0, 512.0)));
    // CropBox origin (50, 50) and visible height 692.
    assert_eq!(only("cropped").rect, Some(rect(50.0, 22.0, 250.0, 42.0)));
    let outside = only("outside");
    assert_eq!(outside.pdf_rect, Some(rect(0.0, 0.0, 40.0, 40.0)));
    assert_eq!(outside.rect, None);
    assert!(outside.hidden);
    assert_eq!(outside.issues, [WidgetIssue::OutsideCropBox]);
}

#[test]
fn kids_cycles_and_shared_nodes_are_rejected_but_parent_loops_are_ignored() {
    let cyclic = {
        let mut pdf = TestPdf::new(1);
        let a = pdf.reserve();
        let b = pdf.reserve();
        pdf.set(
            a,
            dictionary! { "T" => literal(b"a"), "FT" => "Tx", "Kids" => refs(&[b]) },
        );
        pdf.set(
            b,
            dictionary! { "T" => literal(b"b"), "Parent" => a, "Kids" => refs(&[a]) },
        );
        pdf.finish(Some(form(&[a])))
    };
    let self_loop = {
        let mut pdf = TestPdf::new(1);
        let a = pdf.reserve();
        pdf.set(
            a,
            dictionary! { "T" => literal(b"a"), "FT" => "Tx", "Kids" => refs(&[a]) },
        );
        pdf.finish(Some(form(&[a])))
    };
    let shared = {
        let mut pdf = TestPdf::new(1);
        let leaf = pdf.add(dictionary! { "T" => literal(b"leaf"), "FT" => "Tx" });
        let a = pdf.add(dictionary! { "T" => literal(b"a"), "Kids" => refs(&[leaf]) });
        let b = pdf.add(dictionary! { "T" => literal(b"b"), "Kids" => refs(&[leaf]) });
        pdf.finish(Some(form(&[a, b])))
    };
    for bytes in [cyclic, self_loop, shared] {
        assert_eq!(
            fields(&bytes, AcroFormLimits::default()),
            Err(AcroFormError::InvalidFieldTree)
        );
    }

    let mut pdf = TestPdf::new(1);
    let a = pdf.reserve();
    let b = pdf.reserve();
    pdf.set(
        a,
        dictionary! { "T" => literal(b"a"), "FT" => "Tx", "Parent" => b, "Kids" => refs(&[b]) },
    );
    pdf.set(
        b,
        dictionary! { "T" => literal(b"b"), "Parent" => a, "V" => literal(b"ok") },
    );
    let form = extract(&pdf.finish(Some(form(&[a]))));
    assert_eq!(field(&form, "a.b").value, text("ok"));
}

#[test]
fn missing_values_and_objects_are_never_synthesized() {
    let mut pdf = TestPdf::new(1);
    let missing: ObjectId = (9_999, 0);
    let group = pdf.reserve();
    let unnamed = pdf.add(dictionary! { "FT" => "Tx", "V" => literal(b"orphan") });
    let blank = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "T" => literal(b"blank"), "Parent" => group, "DV" => literal(b"default") },
    );
    // A widget that no page lists in /Annots.
    let floating = pdf.add({
        let mut dict = widget_dict([10, 30, 20, 40]);
        dict.set("T", literal(b"floating"));
        dict.set("V", Object::Reference(missing));
        dict
    });
    pdf.set(
        group,
        dictionary! {
            "T" => literal(b"group"),
            "FT" => "Tx",
            "Kids" => vec![
                blank.into(),
                Object::Reference(missing),
                Object::Integer(3),
                unnamed.into(),
                floating.into(),
            ],
        },
    );
    let top_widget = pdf.widget(0, [10, 50, 20, 60], dictionary! { "FT" => "Tx" });
    let form = extract(&pdf.finish(Some(form(&[group, top_widget]))));

    let blank = field(&form, "group.blank");
    assert_eq!(blank.value, None);
    assert_eq!(blank.default_value, text("default"));
    assert!(blank.issues.is_empty());
    let floating = field(&form, "group.floating");
    assert_eq!(floating.value, None);
    assert_eq!(floating.widgets[0].page, None);
    assert_eq!(floating.widgets[0].rect, None);
    assert_eq!(
        floating.widgets[0].pdf_rect,
        Some(rect(10.0, 30.0, 20.0, 40.0))
    );
    assert_eq!(
        floating.widgets[0].issues,
        [WidgetIssue::NotInPageAnnotations]
    );
    let skipped: Vec<_> = form.skipped.iter().map(|s| (s.object, s.reason)).collect();
    assert_eq!(
        skipped,
        [
            (Some(top_widget), SkipReason::MissingName),
            (Some(missing), SkipReason::MissingObject),
            (None, SkipReason::NotDictionary),
            (Some(unnamed), SkipReason::MissingName),
        ]
    );
}

#[test]
fn invalid_rects_and_ambiguous_pages_keep_the_field_value() {
    let mut pdf = TestPdf::new(2);
    let flat = pdf.widget(
        0,
        [10, 10, 10, 20],
        dictionary! { "T" => literal(b"flat"), "FT" => "Tx", "V" => literal(b"v") },
    );
    let twice = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "T" => literal(b"twice"), "FT" => "Tx", "V" => literal(b"w") },
    );
    pdf.annotate(1, twice);
    let form = extract(&pdf.finish(Some(form(&[flat, twice]))));

    let flat = field(&form, "flat");
    assert_eq!(flat.value, text("v"));
    assert_eq!(flat.widgets[0].pdf_rect, None);
    assert_eq!(flat.widgets[0].issues, [WidgetIssue::InvalidRect]);
    let twice = field(&form, "twice");
    assert_eq!(twice.value, text("w"));
    assert_eq!(twice.widgets[0].page, None);
    assert_eq!(twice.widgets[0].issues, [WidgetIssue::OnMultiplePages]);
}

#[test]
fn xfa_presence_is_detected_without_parsing() {
    let xfa = || Stream::new(dictionary! {}, b"<xdp:xdp/>".to_vec());

    let mut pdf = TestPdf::new(1);
    let stream = pdf.doc.add_object(xfa());
    let only = pdf.finish(Some(
        dictionary! { "Fields" => Vec::<Object>::new(), "XFA" => stream },
    ));
    let only = extract(&only);
    assert!(only.present);
    assert_eq!(only.xfa, XfaPresence::XfaOnly);
    assert!(only.fields.is_empty());

    let mut pdf = TestPdf::new(1);
    let stream = pdf.doc.add_object(xfa());
    let id = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "T" => literal(b"x"), "FT" => "Tx", "V" => literal(b"1") },
    );
    let mut acroform = form(&[id]);
    acroform.set("XFA", vec![literal(b"template"), stream.into()]);
    let hybrid = extract(&pdf.finish(Some(acroform)));
    assert_eq!(hybrid.xfa, XfaPresence::Hybrid);
    assert_eq!(field(&hybrid, "x").value, text("1"));

    let none = extract(&TestPdf::new(1).finish(None));
    assert!(!none.present);
    assert_eq!(none.xfa, XfaPresence::Absent);
    assert!(none.fields.is_empty() && none.skipped.is_empty());
}

#[test]
fn limits_fail_the_whole_extraction() {
    let chain = {
        let mut pdf = TestPdf::new(2);
        let leaf = pdf.widget(
            0,
            [10, 10, 20, 20],
            dictionary! { "T" => literal(b"c"), "V" => literal(b"v") },
        );
        let middle = pdf.add(dictionary! { "T" => literal(b"b"), "Kids" => refs(&[leaf]) });
        let top =
            pdf.add(dictionary! { "T" => literal(b"a"), "FT" => "Tx", "Kids" => refs(&[middle]) });
        pdf.finish(Some(form(&[top])))
    };
    let shared_value = {
        let mut pdf = TestPdf::new(1);
        let value = pdf.doc.add_object(literal(&[b'x'; 1_000]));
        let ids: Vec<_> = (0..50)
            .map(|i| {
                pdf.add(dictionary! { "T" => literal(format!("f{i}").as_bytes()), "FT" => "Tx", "V" => value })
            })
            .collect();
        pdf.finish(Some(form(&ids)))
    };
    let limited = |change: fn(&mut AcroFormLimits)| {
        let mut limits = AcroFormLimits::default();
        change(&mut limits);
        limits
    };
    let cases: [(&[u8], AcroFormLimits, AcroFormLimit); 9] = [
        (&chain, limited(|l| l.max_depth = 2), AcroFormLimit::Depth),
        (&chain, limited(|l| l.max_nodes = 2), AcroFormLimit::Nodes),
        (&chain, limited(|l| l.max_pages = 1), AcroFormLimit::Pages),
        (
            &chain,
            limited(|l| l.max_annotations = 0),
            AcroFormLimit::Annotations,
        ),
        (&chain, limited(|l| l.max_bytes = 100), AcroFormLimit::Bytes),
        (&chain, limited(|l| l.max_depth = 0), AcroFormLimit::Depth),
        (&chain, limited(|l| l.max_nodes = 0), AcroFormLimit::Nodes),
        (&chain, limited(|l| l.max_pages = 0), AcroFormLimit::Pages),
        (
            &shared_value,
            limited(|l| l.max_text_bytes = 10_000),
            AcroFormLimit::TextBytes,
        ),
    ];
    for (bytes, limits, limit) in cases {
        assert_eq!(
            fields(bytes, limits),
            Err(AcroFormError::LimitExceeded(limit)),
            "{limits:?}"
        );
    }
    // The same inputs succeed within the default limits.
    assert_eq!(extract(&chain).fields[0].name, "a.b.c");
    assert_eq!(extract(&shared_value).fields.len(), 50);
}

#[test]
fn preloaded_document_matches_and_encryption_is_rejected() {
    let mut pdf = TestPdf::new(1);
    let id = pdf.widget(
        0,
        [10, 10, 20, 20],
        dictionary! { "T" => literal(b"x"), "FT" => "Tx", "V" => literal(b"1") },
    );
    let bytes = pdf.finish(Some(form(&[id])));
    let mut doc = vf_source::metadata::load(&bytes).unwrap();
    assert_eq!(
        fields_in(&doc, AcroFormLimits::default()),
        fields(&bytes, AcroFormLimits::default())
    );
    doc.trailer
        .set("Encrypt", dictionary! { "Filter" => "Standard" });
    assert_eq!(
        fields_in(&doc, AcroFormLimits::default()),
        Err(AcroFormError::Encrypted)
    );
    assert_eq!(
        fields(b"not a pdf", AcroFormLimits::default()),
        Err(AcroFormError::InvalidContainer)
    );
}
