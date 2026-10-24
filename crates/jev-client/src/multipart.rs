//! Asset binding and bounded PNG multipart encoding. No file IO or HTTP.
use super::{Content, Error, Question, Request, RequestDraft};
use std::collections::BTreeSet;

fn references(value: &Content, reference: &str) -> bool {
    match value {
        Content::Text(s) => s.contains(reference),
        Content::Array(xs) => xs.iter().any(|x| references(x, reference)),
        Content::Object(xs) => xs.values().any(|x| references(x, reference)),
        _ => false,
    }
}

// Conservative local rule, not a reimplementation of upstream replacement.
// Walk UTF-8 character boundaries, including overlapping occurrences of one ref.
fn overlapping_references(value: &Content, refs: &[&str]) -> bool {
    match value {
        Content::Text(text) => {
            let mut previous: Option<(usize, &str)> = None;
            for (start, _) in text.char_indices() {
                for &reference in refs {
                    if text[start..].starts_with(reference) {
                        if let Some((end, other)) = previous
                            && start < end
                            && reference != other
                        {
                            return true;
                        }
                        previous = Some((start + reference.len(), reference));
                    }
                }
            }
            false
        }
        Content::Array(xs) => xs.iter().any(|x| overlapping_references(x, refs)),
        Content::Object(xs) => xs.values().any(|x| overlapping_references(x, refs)),
        _ => false,
    }
}

pub(crate) fn validate_assets(request: &RequestDraft) -> Result<(), Error> {
    let has_box = request
        .questions
        .values()
        .any(|q| matches!(q, Question::Box { .. }));
    // Local binding restriction: every Box in a request refers to the sole image.
    if has_box && request.assets.len() != 1 {
        return Err(Error::Multipart);
    }
    if request.assets.keys().map(|r| r.len()).sum::<usize>() > 65_536 {
        return Err(Error::Multipart);
    }
    let mut parts = BTreeSet::new();
    for (reference, asset) in &request.assets {
        if reference.trim().is_empty()
            || reference.len() > 1024
            || asset.part.trim().is_empty()
            || asset.part == "request"
            || !parts.insert(&asset.part)
        {
            return Err(Error::Multipart);
        }
        // Avoid ambiguous substring replacement; not an upstream universal rule.
        if request
            .assets
            .keys()
            .any(|other| reference != other && other.contains(reference))
        {
            return Err(Error::Multipart);
        }
        let used = references(&request.state, reference)
            || request.questions.values().any(|q| {
                let (instructions, mut criteria) = q.content();
                references(instructions, reference) || criteria.any(|v| references(v, reference))
            });
        if !used {
            return Err(Error::Multipart);
        }
    }
    for question in request.questions.values() {
        if let Question::Box { instructions } = question {
            let reference = request.assets.keys().next().ok_or(Error::Multipart)?;
            if !references(&request.state, reference) && !references(instructions, reference) {
                return Err(Error::Multipart);
            }
        }
    }
    let refs: Vec<_> = request.assets.keys().map(String::as_str).collect();
    if overlapping_references(&request.state, &refs)
        || request.questions.values().any(|q| {
            let (instructions, mut criteria) = q.content();
            overlapping_references(instructions, &refs)
                || criteria.any(|v| overlapping_references(v, &refs))
        })
    {
        return Err(Error::Multipart);
    }
    Ok(())
}

pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// Jev's accepted image side lengths in pixels.
pub const IMAGE_SIDE_PX: std::ops::RangeInclusive<u32> = 280..=1568;

/// Encoded PNG; dimensions are read from its IHDR, never supplied separately.
/// This checks PNG chunk framing, not full pixel decoding or server acceptance.
pub struct PngImage {
    part: String,
    bytes: Vec<u8>,
    dimensions: (u32, u32),
    sha256: std::sync::OnceLock<String>,
}
impl std::fmt::Debug for PngImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PngImage")
            .field("bytes", &self.bytes.len())
            .finish()
    }
}
impl PngImage {
    pub fn sha256(&self) -> String {
        self.sha256
            .get_or_init(|| {
                use sha2::Digest;
                sha2::Sha256::digest(&self.bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            })
            .clone()
    }
    /// Validated IHDR dimensions only; does not expose image bytes or filenames.
    pub fn dimensions(&self) -> (u32, u32) {
        self.dimensions
    }
    pub fn new(part: String, bytes: Vec<u8>) -> Result<Self, Error> {
        Self::with_sides(part, bytes, IMAGE_SIDE_PX)
    }
    /// For a caller whose model accepts other side lengths than Jev's default.
    pub fn with_sides(
        part: String,
        bytes: Vec<u8>,
        sides: std::ops::RangeInclusive<u32>,
    ) -> Result<Self, Error> {
        if bytes.len() > MAX_IMAGE_BYTES
            || !safe_name(&part)
            || part == "request"
            || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err(Error::Multipart);
        }
        let mut offset = 8usize;
        let mut seen_data = false;
        let mut first = true;
        let mut dimensions = None;
        loop {
            let header = bytes
                .get(offset..offset.saturating_add(8))
                .ok_or(Error::Multipart)?;
            let len =
                u32::from_be_bytes(header[..4].try_into().map_err(|_| Error::Multipart)?) as usize;
            let kind = &header[4..8];
            let end = offset
                .checked_add(12)
                .and_then(|v| v.checked_add(len))
                .ok_or(Error::Multipart)?;
            let chunk = bytes.get(offset..end).ok_or(Error::Multipart)?;
            if first {
                if kind != b"IHDR" || len != 13 {
                    return Err(Error::Multipart);
                }
                let width =
                    u32::from_be_bytes(chunk[8..12].try_into().map_err(|_| Error::Multipart)?);
                let height =
                    u32::from_be_bytes(chunk[12..16].try_into().map_err(|_| Error::Multipart)?);
                if !sides.contains(&width) || !sides.contains(&height) {
                    return Err(Error::Multipart);
                }
                first = false;
                dimensions = Some((width, height));
            } else if kind == b"IHDR" || kind == b"acTL" {
                return Err(Error::Multipart);
            }
            if kind == b"IDAT" {
                seen_data = true;
            }
            if kind == b"IEND" {
                if len != 0 || end != bytes.len() || !seen_data {
                    return Err(Error::Multipart);
                }
                break;
            }
            offset = end;
        }
        Ok(Self {
            part,
            bytes,
            dimensions: dimensions.ok_or(Error::Multipart)?,
            sha256: std::sync::OnceLock::new(),
        })
    }
}
fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 70
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Fully encoded, bounded HTTP body. Debug intentionally omits document content.
pub struct Body {
    pub content_type: String,
    pub danger_zone_images: bool,
    bytes: bytes::Bytes,
}
impl Body {
    pub(crate) fn shared_bytes(&self) -> bytes::Bytes {
        self.bytes.clone()
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn json(request: &Request) -> Result<Self, Error> {
        if !request.draft().assets.is_empty() || request.bytes().len() > MAX_BODY_BYTES {
            return Err(Error::Multipart);
        }
        Ok(Self {
            content_type: "application/json".into(),
            danger_zone_images: false,
            bytes: bytes::Bytes::copy_from_slice(request.bytes()),
        })
    }
}
impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// One page/image binding per request. Boundary is injected for reproducible tests.
pub fn encode(request: &Request, images: &[PngImage], boundary: &str) -> Result<Body, Error> {
    if !safe_name(boundary) || images.is_empty() {
        return Err(Error::Multipart);
    }
    let actual: BTreeSet<_> = images.iter().map(|i| i.part.as_str()).collect();
    let expected: BTreeSet<_> = request
        .draft()
        .assets
        .values()
        .map(|i| i.part.as_str())
        .collect();
    if actual != expected || actual.len() != images.len() {
        return Err(Error::Multipart);
    }
    let collision = |bytes: &[u8]| {
        bytes
            .windows(boundary.len())
            .any(|w| w == boundary.as_bytes())
    };
    if collision(request.bytes()) || images.iter().any(|i| collision(&i.bytes)) {
        return Err(Error::Multipart);
    }
    let headers = 256 * (images.len() + 2);
    let payload = images.iter().map(|i| i.bytes.len()).sum::<usize>();
    let mut bytes = Vec::with_capacity(
        (request.bytes().len())
            .saturating_add(payload)
            .saturating_add(headers)
            .min(MAX_BODY_BYTES),
    );
    let mut append = |data: &[u8]| -> Result<(), Error> {
        if data.len() > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
            return Err(Error::Bytes);
        }
        bytes.extend_from_slice(data);
        Ok(())
    };
    append(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n").as_bytes())?;
    append(request.bytes())?;
    append(b"\r\n")?;
    for image in images {
        append(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n", image.part).as_bytes())?;
        append(&image.bytes)?;
        append(b"\r\n")?;
    }
    append(format!("--{boundary}--\r\n").as_bytes())?;
    Ok(Body {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        danger_zone_images: true,
        bytes: bytes.into(),
    })
}
