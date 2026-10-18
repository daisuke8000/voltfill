//! Strict PDF metadata resolution.
//! Preserves raw values and inheritance origins separately from visible bounds.
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};
use std::collections::HashSet;

pub const MAX_DEPTH: usize = 32;
pub const MAX_STREAM_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    MissingObject(ObjectId),
    ReferenceCycle(ObjectId),
    ReferenceDepth,
    ParentCycle(ObjectId),
    ParentDepth,
    InvalidParent,
    InvalidNodeType,
    ExpectedDictionary,
    MissingMediaBox,
    InvalidBox,
    InvalidNumber,
    InvalidRotate,
    UnsupportedRotate(i64),
    UnsupportedUserUnit,
    DisjointCrop,
    InvalidCatalog,
    InvalidPageTree,
    PageLimit,
}

#[derive(Debug, Clone)]
pub enum Origin {
    Explicit {
        owner: ObjectId,
        parent_depth: usize,
        raw: Object,
        references: Vec<ObjectId>,
    },
    Default(&'static str),
}

#[derive(Debug, Clone)]
pub struct Field<T> {
    pub value: T,
    pub origin: Origin,
}

#[derive(Debug)]
pub struct PageMetadata {
    pub page_object: ObjectId,
    pub media: Field<[f64; 4]>,
    pub crop: Field<[f64; 4]>,
    pub rotate: Field<i64>,
    pub normalized_rotate: i64,
    pub user_unit: Field<f64>,
    pub visible: [f64; 4],
    pub crop_intersected: bool,
}

/// Strict mode and a per-loading-stream limit; not a whole-process memory bound.
pub fn load(bytes: &[u8]) -> Result<Document, lopdf::Error> {
    Document::load_mem_with_options(
        bytes,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(MAX_STREAM_BYTES),
            ..Default::default()
        },
    )
}

fn deref<'a>(
    doc: &'a Document,
    mut value: &'a Object,
    trace: &mut Vec<ObjectId>,
) -> Result<&'a Object, Fault> {
    let mut seen = HashSet::new();
    while let Object::Reference(id) = value {
        if !seen.insert(*id) {
            return Err(Fault::ReferenceCycle(*id));
        }
        if seen.len() > MAX_DEPTH {
            return Err(Fault::ReferenceDepth);
        }
        trace.push(*id);
        // Public object map avoids get_object's implicit dereference policy.
        value = doc.objects.get(id).ok_or(Fault::MissingObject(*id))?;
    }
    Ok(value)
}

fn dictionary(doc: &Document, id: ObjectId) -> Result<&Dictionary, Fault> {
    match doc.objects.get(&id).ok_or(Fault::MissingObject(id))? {
        Object::Dictionary(d) => Ok(d),
        _ => Err(Fault::ExpectedDictionary),
    }
}

fn ancestors(doc: &Document, page: ObjectId) -> Result<Vec<(ObjectId, &Dictionary)>, Fault> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut id = page;
    loop {
        if !seen.insert(id) {
            return Err(Fault::ParentCycle(id));
        }
        if chain.len() >= MAX_DEPTH {
            return Err(Fault::ParentDepth);
        }
        let d = dictionary(doc, id)?;
        let expected: &[u8] = if chain.is_empty() { b"Page" } else { b"Pages" };
        if !matches!(d.get(b"Type"), Ok(Object::Name(name)) if name == expected) {
            return Err(Fault::InvalidNodeType);
        }
        chain.push((id, d));
        match d.get(b"Parent") {
            Err(_) => return Ok(chain),
            Ok(raw @ Object::Reference(_)) => {
                let mut trace = Vec::new();
                let resolved = deref(doc, raw, &mut trace)?;
                if !matches!(resolved, Object::Dictionary(_)) {
                    return Err(Fault::InvalidParent);
                }
                id = *trace.last().ok_or(Fault::InvalidParent)?;
            }
            Ok(_) => return Err(Fault::InvalidParent),
        }
    }
}

fn number(doc: &Document, raw: &Object, refs: &mut Vec<ObjectId>) -> Result<f64, Fault> {
    let value = match deref(doc, raw, refs)? {
        Object::Integer(n) => *n as f64,
        Object::Real(n) => f64::from(*n),
        _ => return Err(Fault::InvalidNumber),
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Fault::InvalidNumber)
    }
}

fn rect(doc: &Document, raw: &Object, refs: &mut Vec<ObjectId>) -> Result<[f64; 4], Fault> {
    let Object::Array(a) = deref(doc, raw, refs)? else {
        return Err(Fault::InvalidBox);
    };
    if a.len() != 4 {
        return Err(Fault::InvalidBox);
    }
    let mut r = [0.0; 4];
    for (out, value) in r.iter_mut().zip(a) {
        *out = number(doc, value, refs)?;
    }
    if r[0] >= r[2] || r[1] >= r[3] {
        return Err(Fault::InvalidBox);
    }
    Ok(r)
}

fn rotation(doc: &Document, raw: &Object, refs: &mut Vec<ObjectId>) -> Result<i64, Fault> {
    let Object::Integer(n) = deref(doc, raw, refs)? else {
        return Err(Fault::InvalidRotate);
    };
    if n % 90 != 0 {
        return Err(Fault::UnsupportedRotate(*n));
    }
    Ok(*n)
}

fn inherited<T>(
    doc: &Document,
    chain: &[(ObjectId, &Dictionary)],
    key: &[u8],
    parse: impl Fn(&Document, &Object, &mut Vec<ObjectId>) -> Result<T, Fault>,
) -> Result<Option<Field<T>>, Fault> {
    for (depth, (owner, d)) in chain.iter().enumerate() {
        if let Ok(raw) = d.get(key) {
            let mut references = Vec::new();
            let value = parse(doc, raw, &mut references)?;
            return Ok(Some(Field {
                value,
                origin: Origin::Explicit {
                    owner: *owner,
                    parent_depth: depth,
                    raw: raw.clone(),
                    references,
                },
            }));
        }
    }
    Ok(None)
}

pub fn page_metadata(doc: &Document, page: ObjectId) -> Result<PageMetadata, Fault> {
    // Validate the entire Parent path even when the child overrides every field.
    let chain = ancestors(doc, page)?;
    let media = inherited(doc, &chain, b"MediaBox", rect)?.ok_or(Fault::MissingMediaBox)?;
    let crop = inherited(doc, &chain, b"CropBox", rect)?.unwrap_or_else(|| Field {
        value: media.value,
        origin: Origin::Default("CropBox absent on complete ancestry; use resolved MediaBox"),
    });
    let rotate = inherited(doc, &chain, b"Rotate", rotation)?.unwrap_or(Field {
        value: 0,
        origin: Origin::Default("Rotate absent on complete ancestry; use 0"),
    });
    // UserUnit is a page entry, not an inheritable page-tree attribute.
    let user_unit = inherited(doc, &chain[..1], b"UserUnit", number)?.unwrap_or(Field {
        value: 1.0,
        origin: Origin::Default("UserUnit absent on page; use 1"),
    });
    if user_unit.value != 1.0 {
        return Err(Fault::UnsupportedUserUnit);
    }
    let m = media.value;
    let c = crop.value;
    let visible = [
        m[0].max(c[0]),
        m[1].max(c[1]),
        m[2].min(c[2]),
        m[3].min(c[3]),
    ];
    if visible[0] >= visible[2] || visible[1] >= visible[3] {
        return Err(Fault::DisjointCrop);
    }
    Ok(PageMetadata {
        page_object: page,
        media,
        crop,
        normalized_rotate: rotate.value.rem_euclid(360),
        rotate,
        user_unit,
        visible,
        crop_intersected: visible != c,
    })
}

/// Enumerate only pages reachable from Catalog/Pages in Kids order. Reject
/// cycles, repeated nodes, wrong Parents, inaccurate Counts and excessive work.
/// A loose Page object in the object map is never a page of this document.
pub fn page_tree(doc: &Document, max_pages: u32) -> Result<Vec<ObjectId>, Fault> {
    let root = doc
        .trailer
        .get(b"Root")
        .map_err(|_| Fault::InvalidCatalog)?;
    let catalog = match deref(doc, root, &mut Vec::new())? {
        Object::Dictionary(d) => d,
        _ => return Err(Fault::InvalidCatalog),
    };
    if !matches!(catalog.get(b"Type"), Ok(Object::Name(n)) if n == b"Catalog") {
        return Err(Fault::InvalidCatalog);
    }
    let pages = catalog.get(b"Pages").map_err(|_| Fault::InvalidPageTree)?;
    let Object::Reference(root_id) = pages else {
        return Err(Fault::InvalidPageTree);
    };
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    visit(doc, *root_id, None, 0, max_pages, &mut seen, &mut result)?;
    if result.is_empty() {
        return Err(Fault::InvalidPageTree);
    }
    Ok(result)
}

fn visit(
    doc: &Document,
    id: ObjectId,
    parent: Option<ObjectId>,
    depth: usize,
    max_pages: u32,
    seen: &mut HashSet<ObjectId>,
    result: &mut Vec<ObjectId>,
) -> Result<usize, Fault> {
    if depth >= MAX_DEPTH || !seen.insert(id) {
        return Err(Fault::InvalidPageTree);
    }
    // Even empty internal nodes must not make traversal unbounded.
    if seen.len()
        > (max_pages as usize)
            .saturating_mul(MAX_DEPTH)
            .saturating_add(1)
    {
        return Err(Fault::PageLimit);
    }
    let d = dictionary(doc, id)?;
    match (parent, d.get(b"Parent")) {
        (None, Err(_)) => {}
        (Some(expected), Ok(Object::Reference(actual))) if *actual == expected => {}
        _ => return Err(Fault::InvalidPageTree),
    }
    match d.get(b"Type") {
        Ok(Object::Name(n)) if n == b"Page" && parent.is_some() => {
            if result.len() >= max_pages as usize {
                return Err(Fault::PageLimit);
            }
            result.push(id);
            Ok(1)
        }
        Ok(Object::Name(n)) if n == b"Pages" => {
            let Object::Integer(count) = d.get(b"Count").map_err(|_| Fault::InvalidPageTree)?
            else {
                return Err(Fault::InvalidPageTree);
            };
            if *count < 0 || *count > max_pages as i64 {
                return Err(Fault::PageLimit);
            }
            let kids = d.get(b"Kids").map_err(|_| Fault::InvalidPageTree)?;
            let Object::Array(kids) = deref(doc, kids, &mut Vec::new())? else {
                return Err(Fault::InvalidPageTree);
            };
            if kids.len() > (max_pages as usize).saturating_mul(MAX_DEPTH) {
                return Err(Fault::PageLimit);
            }
            let mut actual = 0;
            for kid in kids {
                let Object::Reference(kid) = kid else {
                    return Err(Fault::InvalidPageTree);
                };
                actual += visit(doc, *kid, Some(id), depth + 1, max_pages, seen, result)?;
            }
            if actual != *count as usize {
                return Err(Fault::InvalidPageTree);
            }
            Ok(actual)
        }
        _ => Err(Fault::InvalidNodeType),
    }
}
