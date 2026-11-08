//! AcroForm field extraction.
//!
//! Fillable PDFs bind each value to a field name, so a field value is a
//! high-certainty source for form fill. This module reads the interactive form
//! dictionary (`/AcroForm`) with the same strict `lopdf` load as
//! [`crate::inspect`] and reports every terminal field with its exact value and
//! the display rectangles of its widget annotations. It never renders,
//! generates or repairs a value, and it does not parse XFA.
//!
//! # Field tree
//!
//! Traversal starts at `/AcroForm/Fields` and follows `/Kids` depth first in
//! array order. Inheritable entries (`FT`, `Ff`, `V`, `DV`, `MaxLen`) are
//! carried down that Kids path; `/Parent` entries are never followed, so a
//! `/Parent` loop cannot affect the result. A kid with `/T` is a child field. A
//! kid without `/T` whose `/Subtype` is `/Widget` is a widget of its parent. A
//! field is terminal, and reported, when it has a widget (its own merged widget
//! dictionary or widget kids) or has no kids. The fully qualified name joins
//! the partial names (`/T`) of the path with `.`.
//!
//! Signature fields (`/FT /Sig`), push buttons and fields without a supported
//! `/FT` are listed in [`AcroForm::skipped`], as are kid entries that refer to a
//! missing object (the null object, per the PDF specification), are not
//! dictionaries, or have neither `/T` nor `/Subtype /Widget`.
//!
//! # Values
//!
//! - [`FieldValue::Text`] (`/FT /Tx`): the `/V` text string.
//! - [`FieldValue::Choice`] (`/FT /Ch`): each `/V` text string, in order. When a
//!   value equals the export value of an `[export display]` pair in the field's
//!   own `/Opt`, the pair's display text is attached. `/I` is never consulted.
//! - [`FieldValue::State`] (`/FT /Btn` checkbox or radio): the `/V` name, such as
//!   the on-state name or `Off`. Names are decoded as strict UTF-8. Text strings
//!   are not accepted for buttons, and button `/Opt` export values are not
//!   resolved.
//!
//! A missing or null `/V` is `None`; the default value `/DV` is reported
//! separately in [`FormField::default_value`] and is never substituted for
//! `/V`. Text field values stored as streams are not decoded. A password text
//! field must not store its value; any `/V` or `/DV` found on one is withheld.
//! An entry that cannot be decoded exactly yields `None` plus a
//! [`FieldIssue`], never a partial or repaired string. A text or choice `/V`
//! read as PDFDocEncoding (no byte order mark) with any byte at or above
//! `0x80` is still reported, with [`FieldIssue::PdfDocHighBytes`]: such bytes
//! may be text in another encoding (for example Shift_JIS) that
//! PDFDocEncoding misreads.
//!
//! # Text-string decoding
//!
//! Text strings (`/T`, `/TU`, `/V`, `/DV`, `/Opt`) are decoded strictly:
//!
//! - `FE FF` byte order mark: UTF-16BE. An odd byte count or an unpaired
//!   surrogate is undecodable.
//! - `EF BB BF` byte order mark: UTF-8. Invalid UTF-8 is undecodable.
//! - Otherwise PDFDocEncoding. A byte the encoding leaves undefined (for
//!   example `0x7F`, `0x9F`, `0xAD` and most C0 controls) is undecodable rather
//!   than dropped.
//! - A decoded string containing U+001B (a language escape sequence) is
//!   undecodable; escape sequences are not stripped.
//!
//! # Geometry
//!
//! A widget's page is the page whose `/Annots` array references the widget
//! object; `/P` is not trusted. The widget `/Rect` is normalized and converted
//! to Display coordinates with the page's [`PageGeometry`], using the same
//! supported scope as [`crate::inspect`]. Widgets that are not in any page's
//! `/Annots`, lie outside the CropBox or are on unsupported pages keep their
//! PDF rectangle and get a [`WidgetIssue`] instead of a Display rectangle.
//!
//! # Limits and errors
//!
//! [`AcroFormLimits`] bounds input bytes, pages, field-tree depth, field and
//! widget dictionaries, page annotation entries and the total decoded text.
//! Exceeding any limit fails the whole extraction with
//! [`AcroFormError::LimitExceeded`]; results are never silently truncated. A
//! field tree that revisits an object (a `/Kids` cycle or a node shared by two
//! parents) or has a non-array `/Fields` or `/Kids` is
//! [`AcroFormError::InvalidFieldTree`].
//!
//! Encrypted PDFs are rejected with [`AcroFormError::Encrypted`]. Besides the
//! trailer `/Encrypt` check of [`crate::inspect`], this also rejects documents
//! that `lopdf` decrypted with an empty password while loading, because their
//! strings may be only partially decrypted.
//!
//! # XFA
//!
//! An `/XFA` entry is detected and reported through [`AcroForm::xfa`]; its
//! content is not parsed. In a hybrid form the XFA data may be authoritative
//! for XFA-aware viewers, so AcroForm values there deserve extra caution.
use crate::geometry::{PageGeometry, Rect, Rotation};
use crate::metadata;
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use thiserror::Error;

const FF_READ_ONLY: u32 = 1;
const FF_REQUIRED: u32 = 1 << 1;
const FF_PASSWORD: u32 = 1 << 13;
const FF_RADIO: u32 = 1 << 15;
const FF_PUSHBUTTON: u32 = 1 << 16;
const ANNOT_HIDDEN: i64 = 1 << 1;
const ANNOT_NO_VIEW: i64 = 1 << 5;
/// A checkbox or radio widget normally has `Off` and one on-state.
const MAX_APPEARANCE_STATES: usize = 8;

/// Application ceilings for AcroForm extraction. Like [`crate::PdfLimits`],
/// these bound application work and do not sandbox the `lopdf` parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcroFormLimits {
    /// Input size accepted by [`fields`].
    pub max_bytes: usize,
    /// Pages in the page tree. Must be non-zero.
    pub max_pages: u32,
    /// Nesting levels of field dictionaries; top-level fields are level 1.
    /// Must be non-zero.
    pub max_depth: usize,
    /// `/Fields` and `/Kids` entries visited, fields and widgets alike.
    /// Must be non-zero.
    pub max_nodes: usize,
    /// Entries across all page `/Annots` arrays.
    pub max_annotations: usize,
    /// Total decoded text, counted as UTF-8 bytes plus one per string, across
    /// names, values, options and on-state names, including copies.
    pub max_text_bytes: usize,
}

impl Default for AcroFormLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_pages: 100,
            max_depth: metadata::MAX_DEPTH,
            max_nodes: 10_000,
            max_annotations: 50_000,
            max_text_bytes: 4 * 1024 * 1024,
        }
    }
}

impl AcroFormLimits {
    fn validate(&self) -> Result<(), AcroFormError> {
        let zero = if self.max_pages == 0 {
            Some(AcroFormLimit::Pages)
        } else if self.max_depth == 0 {
            Some(AcroFormLimit::Depth)
        } else if self.max_nodes == 0 {
            Some(AcroFormLimit::Nodes)
        } else {
            None
        };
        zero.map_or(Ok(()), |limit| Err(AcroFormError::LimitExceeded(limit)))
    }
}

/// The [`AcroFormLimits`] entry that was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcroFormLimit {
    Bytes,
    Pages,
    Depth,
    Nodes,
    Annotations,
    TextBytes,
}

impl fmt::Display for AcroFormLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bytes => "max_bytes",
            Self::Pages => "max_pages",
            Self::Depth => "max_depth",
            Self::Nodes => "max_nodes",
            Self::Annotations => "max_annotations",
            Self::TextBytes => "max_text_bytes",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AcroFormError {
    #[error("AcroForm input exceeds the configured {0} limit")]
    LimitExceeded(AcroFormLimit),
    #[error("PDF container could not be loaded without metadata repair")]
    InvalidContainer,
    #[error("PDF root/page tree is invalid or empty")]
    InvalidPageTree,
    #[error("encrypted PDFs are unsupported")]
    Encrypted,
    #[error("AcroForm field tree is invalid")]
    InvalidFieldTree,
}

/// Fields of one PDF's interactive form.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AcroForm {
    /// The catalog has an `/AcroForm` dictionary.
    pub present: bool,
    pub xfa: XfaPresence,
    /// Terminal fields in depth-first `/Fields` and `/Kids` order.
    pub fields: Vec<FormField>,
    /// Field-tree entries that were not reported as fields.
    pub skipped: Vec<SkippedNode>,
}

/// Whether the form carries an XFA description, which is never parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XfaPresence {
    Absent,
    /// XFA present and at least one AcroForm field was reported.
    Hybrid,
    /// XFA present and no AcroForm field was reported.
    XfaOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FormField {
    /// Fully qualified name: partial names joined with `.`.
    pub name: String,
    /// The field's own `/TU`.
    pub alternate_name: Option<String>,
    pub kind: FieldKind,
    /// The exact (possibly inherited) `/V`; `None` when absent or undecodable.
    pub value: Option<FieldValue>,
    /// The exact (possibly inherited) `/DV`, never substituted for `value`.
    pub default_value: Option<FieldValue>,
    pub read_only: bool,
    pub required: bool,
    /// The (possibly inherited) `/MaxLen` of a text field.
    pub max_len: Option<u32>,
    /// The field dictionary's object, if it is an indirect object.
    pub object: Option<ObjectId>,
    pub widgets: Vec<Widget>,
    pub issues: Vec<FieldIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Choice,
    Checkbox,
    Radio,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    Text(String),
    Choice(Vec<ChoiceValue>),
    /// A checkbox or radio state name such as the on-state name or `Off`.
    State(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChoiceValue {
    /// The exact `/V` text string.
    pub value: String,
    /// Display text of the `/Opt` pair whose export value equals `value`.
    pub display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Widget {
    pub object: Option<ObjectId>,
    /// 1-based page whose `/Annots` references this widget.
    pub page: Option<u32>,
    /// Normalized `/Rect` in PDF user space (`top` is the minimum y).
    pub pdf_rect: Option<Rect>,
    /// `/Rect` in Display coordinates of `page`.
    pub rect: Option<Rect>,
    /// The annotation's Hidden or NoView flag is set.
    pub hidden: bool,
    /// The single non-`Off` normal appearance state of a checkbox or radio
    /// widget. The widget shows as selected when it equals the field value.
    pub on_state: Option<String>,
    pub issues: Vec<WidgetIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldIssue {
    Value(ValueIssue),
    DefaultValue(ValueIssue),
    /// A password field stored `/V` or `/DV`; both are withheld undecoded.
    PasswordValueWithheld,
    InvalidAlternateName,
    InvalidMaxLen,
    /// A partial name contains `.`, so the qualified name is ambiguous.
    PeriodInPartialName,
    /// The text or choice `/V` is a PDFDocEncoding string (no byte order
    /// mark) with a byte at or above `0x80`. The decoded value is still
    /// reported, but the bytes may be text in another encoding (for example
    /// Shift_JIS) that PDFDocEncoding misreads.
    PdfDocHighBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueIssue {
    /// The entry is not exactly decodable text or a UTF-8 name.
    Undecodable,
    /// The entry's type is not defined for the field kind.
    UnexpectedType,
    /// A text stream value; streams are not decoded.
    UnsupportedStream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetIssue {
    NotInPageAnnotations,
    OnMultiplePages,
    MissingRect,
    InvalidRect,
    UnsupportedPageGeometry,
    OutsideCropBox,
    AmbiguousOnState,
    UndecodableOnState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedNode {
    pub object: Option<ObjectId>,
    /// The qualified name, when it was decoded.
    pub name: Option<String>,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Signature,
    PushButton,
    MissingType,
    UnsupportedType,
    UndecodableName,
    /// Neither `/T` nor `/Subtype /Widget`, or a top-level widget without `/T`.
    MissingName,
    MissingObject,
    NotDictionary,
}

/// Load `bytes` once with the strict metadata loader and extract its fields.
pub fn fields(bytes: &[u8], limits: AcroFormLimits) -> Result<AcroForm, AcroFormError> {
    if bytes.len() > limits.max_bytes {
        return Err(AcroFormError::LimitExceeded(AcroFormLimit::Bytes));
    }
    limits.validate()?;
    let doc = metadata::load(bytes).map_err(|_| AcroFormError::InvalidContainer)?;
    fields_in(&doc, limits)
}

/// Extract fields from a document loaded with [`metadata::load`], so a caller
/// that already holds the document does not parse the PDF again.
/// `limits.max_bytes` is not applied here.
pub fn fields_in(doc: &Document, limits: AcroFormLimits) -> Result<AcroForm, AcroFormError> {
    limits.validate()?;
    if doc.trailer.has(b"Encrypt") || doc.was_encrypted() {
        return Err(AcroFormError::Encrypted);
    }
    let tree = metadata::page_tree(doc, limits.max_pages).map_err(|fault| match fault {
        metadata::Fault::PageLimit => AcroFormError::LimitExceeded(AcroFormLimit::Pages),
        _ => AcroFormError::InvalidPageTree,
    })?;
    let catalog = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|root| resolve(doc, root))
        .and_then(|root| root.as_dict().ok())
        .ok_or(AcroFormError::InvalidPageTree)?;
    let Some(form) = entry(doc, catalog, b"AcroForm") else {
        return Ok(AcroForm {
            present: false,
            xfa: XfaPresence::Absent,
            fields: Vec::new(),
            skipped: Vec::new(),
        });
    };
    let Object::Dictionary(form) = form else {
        return Err(AcroFormError::InvalidFieldTree);
    };
    let xfa = entry(doc, form, b"XFA").is_some();
    let mut walker = Walker {
        doc,
        limits,
        pages: PageIndex::new(doc, &tree, limits.max_annotations)?,
        text_budget: limits.max_text_bytes,
        visited: HashSet::new(),
        nodes: 0,
        fields: Vec::new(),
        skipped: Vec::new(),
    };
    walker.walk(form)?;
    let xfa = match (xfa, walker.fields.is_empty()) {
        (false, _) => XfaPresence::Absent,
        (true, true) => XfaPresence::XfaOnly,
        (true, false) => XfaPresence::Hybrid,
    };
    Ok(AcroForm {
        present: true,
        xfa,
        fields: walker.fields,
        skipped: walker.skipped,
    })
}

/// Follow a bounded reference chain. Missing objects, null, and over-long or
/// cyclic chains resolve to `None`, the null object.
fn resolve<'a>(doc: &'a Document, mut value: &'a Object) -> Option<&'a Object> {
    for _ in 0..=metadata::MAX_DEPTH {
        match value {
            Object::Reference(id) => value = doc.objects.get(id)?,
            Object::Null => return None,
            other => return Some(other),
        }
    }
    None
}

fn entry<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    dict.get(key).ok().and_then(|value| resolve(doc, value))
}

fn is_widget(dict: &Dictionary) -> bool {
    matches!(dict.get(b"Subtype"), Ok(Object::Name(name)) if name == b"Widget")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    Page(u32),
    Ambiguous,
}

struct PageIndex {
    geometries: Vec<Option<PageGeometry>>,
    annotations: HashMap<ObjectId, Placement>,
}

impl PageIndex {
    fn new(
        doc: &Document,
        tree: &[ObjectId],
        max_annotations: usize,
    ) -> Result<Self, AcroFormError> {
        let mut annotations = HashMap::new();
        let mut seen = 0usize;
        let mut geometries = Vec::with_capacity(tree.len());
        for (index, id) in tree.iter().enumerate() {
            let page = index as u32 + 1;
            geometries.push(page_geometry(doc, *id));
            let Some(Object::Dictionary(dict)) = doc.objects.get(id) else {
                continue;
            };
            let Some(Object::Array(list)) = entry(doc, dict, b"Annots") else {
                continue;
            };
            for annotation in list {
                seen += 1;
                if seen > max_annotations {
                    return Err(AcroFormError::LimitExceeded(AcroFormLimit::Annotations));
                }
                // Widgets are referenced from /Kids, so only indirect ones can match.
                if let Object::Reference(annotation) = annotation {
                    annotations
                        .entry(*annotation)
                        .and_modify(|placement| {
                            if *placement != Placement::Page(page) {
                                *placement = Placement::Ambiguous;
                            }
                        })
                        .or_insert(Placement::Page(page));
                }
            }
        }
        Ok(Self {
            geometries,
            annotations,
        })
    }

    fn geometry(&self, page: u32) -> Option<&PageGeometry> {
        self.geometries
            .get((page as usize).checked_sub(1)?)
            .and_then(Option::as_ref)
    }
}

/// The supported page scope of [`crate::inspect`]: no partially intersected
/// CropBox, quarter-turn rotation and UserUnit 1.
fn page_geometry(doc: &Document, page: ObjectId) -> Option<PageGeometry> {
    let m = metadata::page_metadata(doc, page).ok()?;
    if m.crop_intersected {
        return None;
    }
    let rect = |v: [f64; 4]| Rect::try_new(v[0], v[1], v[2], v[3]).ok();
    let rotation = match m.normalized_rotate {
        0 => Rotation::Deg0,
        90 => Rotation::Deg90,
        180 => Rotation::Deg180,
        270 => Rotation::Deg270,
        _ => return None,
    };
    PageGeometry::try_new(
        rect(m.media.value)?,
        rect(m.crop.value)?,
        rotation,
        m.user_unit.value,
    )
    .ok()
}

/// Normalize a `/Rect` given by any two opposite corners.
fn pdf_rect(doc: &Document, raw: &Object) -> Option<Rect> {
    let Object::Array(values) = resolve(doc, raw)? else {
        return None;
    };
    let [x1, y1, x2, y2] = values.as_slice() else {
        return None;
    };
    let number = |value: &Object| match resolve(doc, value)? {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(n) => Some(f64::from(*n)),
        _ => None,
    };
    let (x1, y1, x2, y2) = (number(x1)?, number(y1)?, number(x2)?, number(y2)?);
    Rect::try_new(x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)).ok()
}

#[derive(Clone, Copy)]
struct Node<'a> {
    object: Option<ObjectId>,
    dict: &'a Dictionary,
}

enum Entry<'a> {
    Field(Node<'a>),
    Widget(Node<'a>),
    Skip(Option<ObjectId>, SkipReason),
}

/// Inheritable state along the `/Kids` path, shared by all children.
#[derive(Default)]
struct Scope<'a> {
    name: Option<String>,
    period_in_name: bool,
    ft: Option<&'a Object>,
    ff: Option<&'a Object>,
    v: Option<&'a Object>,
    dv: Option<&'a Object>,
    max_len: Option<&'a Object>,
}

struct Pending<'a> {
    node: Node<'a>,
    depth: usize,
    parent: Rc<Scope<'a>>,
}

enum Outcome {
    Absent,
    Value(FieldValue),
    Issue(ValueIssue),
}

struct Walker<'a> {
    doc: &'a Document,
    limits: AcroFormLimits,
    pages: PageIndex,
    text_budget: usize,
    visited: HashSet<ObjectId>,
    nodes: usize,
    fields: Vec<FormField>,
    skipped: Vec<SkippedNode>,
}

impl<'a> Walker<'a> {
    fn walk(&mut self, form: &'a Dictionary) -> Result<(), AcroFormError> {
        let entries = match form.get(b"Fields").ok().map(|f| resolve(self.doc, f)) {
            None | Some(None) => &[][..],
            Some(Some(Object::Array(entries))) => entries.as_slice(),
            Some(Some(_)) => return Err(AcroFormError::InvalidFieldTree),
        };
        let root = Rc::new(Scope::default());
        let mut top = Vec::new();
        for raw in entries {
            match self.classify(raw)? {
                Entry::Field(node) => top.push(node),
                Entry::Widget(node) => self.skip(node.object, None, SkipReason::MissingName),
                Entry::Skip(object, reason) => self.skip(object, None, reason),
            }
        }
        let mut stack: Vec<Pending<'a>> = top
            .into_iter()
            .rev()
            .map(|node| Pending {
                node,
                depth: 1,
                parent: Rc::clone(&root),
            })
            .collect();
        while let Some(pending) = stack.pop() {
            self.visit(pending, &mut stack)?;
        }
        Ok(())
    }

    /// Count and resolve one `/Fields` or `/Kids` entry; a repeated object is
    /// a cycle or a shared node and invalidates the tree.
    fn classify(&mut self, raw: &'a Object) -> Result<Entry<'a>, AcroFormError> {
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            return Err(AcroFormError::LimitExceeded(AcroFormLimit::Nodes));
        }
        let object = match raw {
            Object::Reference(id) => {
                if !self.visited.insert(*id) {
                    return Err(AcroFormError::InvalidFieldTree);
                }
                Some(*id)
            }
            _ => None,
        };
        let Some(resolved) = resolve(self.doc, raw) else {
            return Ok(Entry::Skip(object, SkipReason::MissingObject));
        };
        let Object::Dictionary(dict) = resolved else {
            return Ok(Entry::Skip(object, SkipReason::NotDictionary));
        };
        let node = Node { object, dict };
        Ok(if dict.has(b"T") {
            Entry::Field(node)
        } else if is_widget(dict) {
            Entry::Widget(node)
        } else {
            Entry::Skip(object, SkipReason::MissingName)
        })
    }

    fn visit(
        &mut self,
        pending: Pending<'a>,
        stack: &mut Vec<Pending<'a>>,
    ) -> Result<(), AcroFormError> {
        let Pending {
            node,
            depth,
            parent,
        } = pending;
        let partial = match entry(self.doc, node.dict, b"T") {
            Some(Object::String(raw, _)) => self.text(raw)?,
            _ => None,
        };
        let Some(partial) = partial else {
            self.skip(node.object, None, SkipReason::UndecodableName);
            return Ok(());
        };
        let name = match &parent.name {
            Some(prefix) => format!("{prefix}.{partial}"),
            None => partial.clone(),
        };
        self.charge(name.len())?;
        let own = |key: &[u8]| node.dict.get(key).ok();
        let scope = Rc::new(Scope {
            name: Some(name),
            period_in_name: parent.period_in_name || partial.contains('.'),
            ft: own(b"FT").or(parent.ft),
            ff: own(b"Ff").or(parent.ff),
            v: own(b"V").or(parent.v),
            dv: own(b"DV").or(parent.dv),
            max_len: own(b"MaxLen").or(parent.max_len),
        });
        let kids = match node.dict.get(b"Kids").ok().map(|k| resolve(self.doc, k)) {
            None | Some(None) => &[][..],
            Some(Some(Object::Array(kids))) => kids.as_slice(),
            Some(Some(_)) => return Err(AcroFormError::InvalidFieldTree),
        };
        let mut widgets = Vec::new();
        if is_widget(node.dict) {
            widgets.push(node);
        }
        let mut children = Vec::new();
        for raw in kids {
            match self.classify(raw)? {
                Entry::Field(child) => {
                    if depth >= self.limits.max_depth {
                        return Err(AcroFormError::LimitExceeded(AcroFormLimit::Depth));
                    }
                    children.push(child);
                }
                Entry::Widget(widget) => widgets.push(widget),
                Entry::Skip(object, reason) => self.skip(object, None, reason),
            }
        }
        if !widgets.is_empty() || kids.is_empty() {
            self.emit(node, &scope, &widgets)?;
        }
        stack.extend(children.into_iter().rev().map(|child| Pending {
            node: child,
            depth: depth + 1,
            parent: Rc::clone(&scope),
        }));
        Ok(())
    }

    fn emit(
        &mut self,
        node: Node<'a>,
        scope: &Scope<'a>,
        widgets: &[Node<'a>],
    ) -> Result<(), AcroFormError> {
        let name = scope.name.clone().unwrap_or_default();
        self.charge(name.len())?;
        let flags = match scope.ff.and_then(|ff| resolve(self.doc, ff)) {
            // Flags are a 32-bit field; keep the bit pattern of signed writers.
            Some(Object::Integer(n)) => *n as u32,
            _ => 0,
        };
        let kind = match scope.ft.and_then(|ft| resolve(self.doc, ft)) {
            None => Err(SkipReason::MissingType),
            Some(Object::Name(ft)) => match ft.as_slice() {
                b"Tx" => Ok(FieldKind::Text),
                b"Ch" => Ok(FieldKind::Choice),
                b"Btn" if flags & FF_PUSHBUTTON != 0 => Err(SkipReason::PushButton),
                b"Btn" if flags & FF_RADIO != 0 => Ok(FieldKind::Radio),
                b"Btn" => Ok(FieldKind::Checkbox),
                b"Sig" => Err(SkipReason::Signature),
                _ => Err(SkipReason::UnsupportedType),
            },
            Some(_) => Err(SkipReason::UnsupportedType),
        };
        let kind = match kind {
            Ok(kind) => kind,
            Err(reason) => {
                self.skip(node.object, Some(name), reason);
                return Ok(());
            }
        };
        let mut issues = Vec::new();
        if scope.period_in_name {
            issues.push(FieldIssue::PeriodInPartialName);
        }
        let alternate_name = match entry(self.doc, node.dict, b"TU") {
            None => None,
            Some(Object::String(raw, _)) => {
                let text = self.text(raw)?;
                if text.is_none() {
                    issues.push(FieldIssue::InvalidAlternateName);
                }
                text
            }
            Some(_) => {
                issues.push(FieldIssue::InvalidAlternateName);
                None
            }
        };
        let max_len = match (kind, scope.max_len.and_then(|m| resolve(self.doc, m))) {
            (FieldKind::Text, Some(Object::Integer(n))) => match u32::try_from(*n) {
                Ok(n) => Some(n),
                Err(_) => {
                    issues.push(FieldIssue::InvalidMaxLen);
                    None
                }
            },
            (FieldKind::Text, Some(_)) => {
                issues.push(FieldIssue::InvalidMaxLen);
                None
            }
            _ => None,
        };
        let opt = entry(self.doc, node.dict, b"Opt");
        let password = kind == FieldKind::Text && flags & FF_PASSWORD != 0;
        let stored = |raw: Option<&'a Object>| raw.and_then(|r| resolve(self.doc, r)).is_some();
        let (value, default_value) = if password {
            if stored(scope.v) || stored(scope.dv) {
                issues.push(FieldIssue::PasswordValueWithheld);
            }
            (None, None)
        } else {
            let value = match self.value(kind, scope.v, opt)? {
                Outcome::Absent => None,
                Outcome::Value(value) => Some(value),
                Outcome::Issue(issue) => {
                    issues.push(FieldIssue::Value(issue));
                    None
                }
            };
            if matches!(value, Some(FieldValue::Text(_) | FieldValue::Choice(_)))
                && self.pdf_doc_high_bytes(scope.v)
            {
                issues.push(FieldIssue::PdfDocHighBytes);
            }
            let default_value = match self.value(kind, scope.dv, opt)? {
                Outcome::Absent => None,
                Outcome::Value(value) => Some(value),
                Outcome::Issue(issue) => {
                    issues.push(FieldIssue::DefaultValue(issue));
                    None
                }
            };
            (value, default_value)
        };
        let buttons = matches!(kind, FieldKind::Checkbox | FieldKind::Radio);
        let widgets = widgets
            .iter()
            .map(|widget| self.widget(*widget, buttons))
            .collect::<Result<_, _>>()?;
        self.fields.push(FormField {
            name,
            alternate_name,
            kind,
            value,
            default_value,
            read_only: flags & FF_READ_ONLY != 0,
            required: flags & FF_REQUIRED != 0,
            max_len,
            object: node.object,
            widgets,
            issues,
        });
        Ok(())
    }

    fn value(
        &mut self,
        kind: FieldKind,
        raw: Option<&'a Object>,
        opt: Option<&'a Object>,
    ) -> Result<Outcome, AcroFormError> {
        let Some(value) = raw.and_then(|raw| resolve(self.doc, raw)) else {
            return Ok(Outcome::Absent);
        };
        match (kind, value) {
            (FieldKind::Text, Object::String(raw, _)) => Ok(self
                .text(raw)?
                .map_or(Outcome::Issue(ValueIssue::Undecodable), |text| {
                    Outcome::Value(FieldValue::Text(text))
                })),
            (FieldKind::Text, Object::Stream(_)) => {
                Ok(Outcome::Issue(ValueIssue::UnsupportedStream))
            }
            (FieldKind::Choice, Object::String(..) | Object::Array(_)) => self.choice(value, opt),
            (FieldKind::Checkbox | FieldKind::Radio, Object::Name(raw)) => Ok(self
                .name(raw)?
                .map_or(Outcome::Issue(ValueIssue::Undecodable), |state| {
                    Outcome::Value(FieldValue::State(state))
                })),
            _ => Ok(Outcome::Issue(ValueIssue::UnexpectedType)),
        }
    }

    fn choice(
        &mut self,
        value: &'a Object,
        opt: Option<&'a Object>,
    ) -> Result<Outcome, AcroFormError> {
        let raws = match value {
            Object::Array(items) => items.as_slice(),
            single => std::slice::from_ref(single),
        };
        let mut selected = Vec::with_capacity(raws.len().min(64));
        for raw in raws {
            let Some(Object::String(raw, _)) = resolve(self.doc, raw) else {
                return Ok(Outcome::Issue(ValueIssue::UnexpectedType));
            };
            let Some(text) = self.text(raw)? else {
                return Ok(Outcome::Issue(ValueIssue::Undecodable));
            };
            selected.push(text);
        }
        let options = if selected.is_empty() {
            Vec::new()
        } else {
            self.options(opt)?
        };
        let mut values = Vec::with_capacity(selected.len());
        for value in selected {
            let display = options
                .iter()
                .find(|(export, _)| *export == value)
                .and_then(|(_, display)| display.clone());
            if let Some(display) = &display {
                self.charge(display.len())?;
            }
            values.push(ChoiceValue { value, display });
        }
        Ok(Outcome::Value(FieldValue::Choice(values)))
    }

    /// Decode `/Opt` as (export, display) pairs. Malformed or undecodable
    /// entries are left out; they only cost display text, never a value.
    fn options(
        &mut self,
        opt: Option<&'a Object>,
    ) -> Result<Vec<(String, Option<String>)>, AcroFormError> {
        let Some(Object::Array(entries)) = opt else {
            return Ok(Vec::new());
        };
        let mut options = Vec::new();
        for raw in entries {
            match resolve(self.doc, raw) {
                Some(Object::String(export, _)) => {
                    if let Some(export) = self.text(export)? {
                        options.push((export, None));
                    }
                }
                Some(Object::Array(pair)) => {
                    let strings = pair
                        .iter()
                        .map(|item| match resolve(self.doc, item) {
                            Some(Object::String(raw, _)) => Some(raw.as_slice()),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some([export, display]) = strings.as_deref()
                        && let (Some(export), Some(display)) =
                            (self.text(export)?, self.text(display)?)
                    {
                        options.push((export, Some(display)));
                    }
                }
                _ => {}
            }
        }
        Ok(options)
    }

    fn widget(&mut self, node: Node<'a>, buttons: bool) -> Result<Widget, AcroFormError> {
        let mut issues = Vec::new();
        let page = match node
            .object
            .and_then(|id| self.pages.annotations.get(&id).copied())
        {
            Some(Placement::Page(page)) => Some(page),
            Some(Placement::Ambiguous) => {
                issues.push(WidgetIssue::OnMultiplePages);
                None
            }
            None => {
                issues.push(WidgetIssue::NotInPageAnnotations);
                None
            }
        };
        let pdf_rect = match node
            .dict
            .get(b"Rect")
            .ok()
            .and_then(|r| resolve(self.doc, r))
        {
            None => {
                issues.push(WidgetIssue::MissingRect);
                None
            }
            Some(raw) => {
                let rect = pdf_rect(self.doc, raw);
                if rect.is_none() {
                    issues.push(WidgetIssue::InvalidRect);
                }
                rect
            }
        };
        let rect = match (page, pdf_rect) {
            (Some(page), Some(pdf_rect)) => match self.pages.geometry(page) {
                None => {
                    issues.push(WidgetIssue::UnsupportedPageGeometry);
                    None
                }
                Some(geometry) => {
                    let rect = geometry.pdf_rect_to_display(pdf_rect).ok();
                    if rect.is_none() {
                        issues.push(WidgetIssue::OutsideCropBox);
                    }
                    rect
                }
            },
            _ => None,
        };
        let hidden = matches!(
            entry(self.doc, node.dict, b"F"),
            Some(Object::Integer(flags)) if flags & (ANNOT_HIDDEN | ANNOT_NO_VIEW) != 0
        );
        let on_state = if buttons {
            self.on_state(node.dict, &mut issues)?
        } else {
            None
        };
        Ok(Widget {
            object: node.object,
            page,
            pdf_rect,
            rect,
            hidden,
            on_state,
            issues,
        })
    }

    fn on_state(
        &mut self,
        dict: &'a Dictionary,
        issues: &mut Vec<WidgetIssue>,
    ) -> Result<Option<String>, AcroFormError> {
        let Some(Object::Dictionary(appearance)) = entry(self.doc, dict, b"AP") else {
            return Ok(None);
        };
        let Some(Object::Dictionary(normal)) = entry(self.doc, appearance, b"N") else {
            return Ok(None);
        };
        // Check the size before iterating: one dictionary may be shared by many widgets.
        if normal.len() > MAX_APPEARANCE_STATES {
            issues.push(WidgetIssue::AmbiguousOnState);
            return Ok(None);
        }
        let states: Vec<&[u8]> = normal
            .iter()
            .map(|(key, _)| key.as_slice())
            .filter(|key| *key != b"Off")
            .collect();
        match states.as_slice() {
            [] => Ok(None),
            [state] => {
                let state = self.name(state)?;
                if state.is_none() {
                    issues.push(WidgetIssue::UndecodableOnState);
                }
                Ok(state)
            }
            _ => {
                issues.push(WidgetIssue::AmbiguousOnState);
                Ok(None)
            }
        }
    }

    /// Whether a decoded text or choice `/V` holds a PDFDocEncoding string
    /// with a byte at or above `0x80` ([`FieldIssue::PdfDocHighBytes`]).
    fn pdf_doc_high_bytes(&self, raw: Option<&'a Object>) -> bool {
        let high = |object: &Object| {
            matches!(
                resolve(self.doc, object),
                Some(Object::String(bytes, _))
                    if !bytes.starts_with(b"\xFE\xFF")
                        && !bytes.starts_with(b"\xEF\xBB\xBF")
                        && bytes.iter().any(|byte| *byte >= 0x80)
            )
        };
        match raw.and_then(|raw| resolve(self.doc, raw)) {
            Some(Object::Array(items)) => items.iter().any(high),
            Some(value) => high(value),
            None => false,
        }
    }

    fn skip(&mut self, object: Option<ObjectId>, name: Option<String>, reason: SkipReason) {
        self.skipped.push(SkippedNode {
            object,
            name,
            reason,
        });
    }

    fn charge(&mut self, len: usize) -> Result<(), AcroFormError> {
        self.text_budget = self
            .text_budget
            .checked_sub(len.saturating_add(1))
            .ok_or(AcroFormError::LimitExceeded(AcroFormLimit::TextBytes))?;
        Ok(())
    }

    /// A charged text string; `None` when it is not exactly decodable.
    fn text(&mut self, raw: &[u8]) -> Result<Option<String>, AcroFormError> {
        let Some(text) = decode_text_string(raw) else {
            return Ok(None);
        };
        self.charge(text.len())?;
        Ok(Some(text))
    }

    /// A charged name; `None` when its bytes are not UTF-8.
    fn name(&mut self, raw: &[u8]) -> Result<Option<String>, AcroFormError> {
        let Ok(name) = std::str::from_utf8(raw) else {
            return Ok(None);
        };
        self.charge(name.len())?;
        Ok(Some(name.to_owned()))
    }
}

/// Strict PDF text-string decoding; see the module documentation.
fn decode_text_string(raw: &[u8]) -> Option<String> {
    let text = if let Some(utf16) = raw.strip_prefix(b"\xFE\xFF") {
        let (units, rest) = utf16.as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        char::decode_utf16(units.iter().map(|unit| u16::from_be_bytes(*unit)))
            .collect::<Result<String, _>>()
            .ok()?
    } else if let Some(utf8) = raw.strip_prefix(b"\xEF\xBB\xBF") {
        String::from_utf8(utf8.to_vec()).ok()?
    } else {
        raw.iter()
            .map(|byte| pdf_doc_char(*byte))
            .collect::<Option<String>>()?
    };
    (!text.contains('\u{1b}')).then_some(text)
}

/// PDFDocEncoding (ISO 32000-2, Annex D); `None` for undefined codes.
fn pdf_doc_char(byte: u8) -> Option<char> {
    const ACCENTS: [char; 8] = [
        '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}',
        '\u{02DC}',
    ];
    const SPECIALS: [char; 31] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017E}',
    ];
    match byte {
        0x09 | 0x0A | 0x0D | 0x20..=0x7E => Some(char::from(byte)),
        0x18..=0x1F => Some(ACCENTS[usize::from(byte - 0x18)]),
        0x80..=0x9E => Some(SPECIALS[usize::from(byte - 0x80)]),
        0xA0 => Some('\u{20AC}'),
        0xAD => None,
        0xA1..=0xFF => Some(char::from(byte)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_doc_encoding_agrees_with_lopdf_on_every_defined_byte() {
        for byte in 0..=u8::MAX {
            let ours = pdf_doc_char(byte);
            let theirs = lopdf::decode_text_string(&Object::string_literal(vec![byte])).unwrap();
            match ours {
                Some(c) => assert_eq!(theirs, c.to_string(), "byte {byte:#04x}"),
                // lopdf drops undefined bytes; strict decoding rejects them instead.
                None => assert_eq!(theirs, "", "byte {byte:#04x}"),
            }
        }
    }

    #[test]
    fn text_strings_decode_strictly() {
        assert_eq!(decode_text_string(b"Total \x80 \x8B").unwrap(), "Total • ‰");
        assert_eq!(
            decode_text_string(b"\xFE\xFF\x8A\xCB\x6C\x42").unwrap(),
            "請求"
        );
        assert_eq!(
            decode_text_string(b"\xFE\xFF\xD8\x3D\xDE\x00").unwrap(),
            "😀"
        );
        assert_eq!(decode_text_string("\u{FEFF}é".as_bytes()).unwrap(), "é");
        assert_eq!(decode_text_string(b"").unwrap(), "");
        assert_eq!(decode_text_string(b"\xFE\xFF").unwrap(), "");
        for undecodable in [
            &b"A\x7FB"[..],
            b"\x9F",
            b"\xAD",
            b"\x00",
            b"\xFE\xFF\x00\x41\x00",
            b"\xFE\xFF\xD8\x3D",
            b"\xEF\xBB\xBF\xC3",
            b"\xFE\xFF\x00\x1B\x00\x6A\x00\x61\x00\x1B\x00\x41",
        ] {
            assert_eq!(decode_text_string(undecodable), None, "{undecodable:?}");
        }
    }
}
