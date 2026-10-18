//! Optional page rendering with explicit backend selection. Resource ceilings
//! cover the requested raster and PNG output, not parser-internal allocations.
use crate::geometry::{
    AffineTransform, ImageTransform, Padding, PageGeometry, Point, Rect, Rotation,
};
use crate::pdf::metadata;
#[cfg(feature = "pdfium")]
use firecrawl_pdfium::{PagePoint, Pdfium, PixelFormat};
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone)]
pub enum RenderBackend {
    Pdfium(PathBuf),
    Hayro,
}

#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub backend: RenderBackend,
    pub pixels_per_point: f64,
    pub max_pixels: u64,
    pub max_output_bytes: u64,
    pub max_input_bytes: usize,
    pub max_pages: u32,
}

impl RenderConfig {
    pub fn new(library_path: impl Into<PathBuf>) -> Self {
        Self::with_backend(RenderBackend::Pdfium(library_path.into()))
    }

    pub fn hayro() -> Self {
        Self::with_backend(RenderBackend::Hayro)
    }

    fn with_backend(backend: RenderBackend) -> Self {
        Self {
            backend,
            pixels_per_point: 2.0,
            max_pixels: 16_000_000,
            max_output_bytes: 64 * 1024 * 1024,
            max_input_bytes: 16 * 1024 * 1024,
            max_pages: 100,
        }
    }

    pub fn renderer_name(&self) -> &'static str {
        match self.backend {
            RenderBackend::Pdfium(_) => "pdfium",
            RenderBackend::Hayro => "hayro",
        }
    }

    /// Check configuration without loading or executing a native library.
    pub fn validate(&self) -> Result<(), RenderError> {
        match &self.backend {
            RenderBackend::Pdfium(path) => {
                if !cfg!(feature = "pdfium") {
                    return Err(RenderError::BackendUnavailable);
                }
                if !path.is_absolute() {
                    return Err(RenderError::InvalidLibraryPath);
                }
                if !path.is_file() {
                    return Err(RenderError::MissingLibrary);
                }
            }
            RenderBackend::Hayro if !cfg!(feature = "hayro") => {
                return Err(RenderError::BackendUnavailable);
            }
            RenderBackend::Hayro => {}
        }
        if !self.pixels_per_point.is_finite()
            || self.pixels_per_point <= 0.0
            || self.max_pages == 0
            || self.max_pixels == 0
            || self.max_output_bytes == 0
            || self.max_input_bytes == 0
        {
            return Err(RenderError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct RenderedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub transform: ImageTransform,
    pub geometry: PageGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RenderError {
    #[error("selected renderer is not enabled in this build")]
    BackendUnavailable,
    #[error("renderer requires an unavailable font or encountered unsupported content")]
    UnsupportedContent,
    #[error("native library requires an explicit absolute file path")]
    InvalidLibraryPath,
    #[error("configured native library is missing")]
    MissingLibrary,
    #[error("configured native library could not be loaded or lacks required symbols")]
    InvalidLibrary,
    #[error("a different native library is already initialized")]
    LibraryConflict,
    #[error("render input, dimensions, or output exceed configured limits")]
    LimitExceeded,
    #[error("render configuration is invalid")]
    InvalidConfig,
    #[error("PDF container or selected page is invalid")]
    InvalidPdf,
    #[error("selected page geometry is unsupported or differs between parsers")]
    UnsupportedGeometry,
    #[error("native page rendering failed")]
    NativeFailure,
    #[error("PNG encoding failed or exceeded output limit")]
    EncodingFailure,
}

/// Render exactly one 1-based page, without ambient library discovery, OCR,
/// annotations, forms, extra rotation, crop, resize, or padding. The returned
/// ledger can be extended explicitly for subsequent image preparation.
pub fn render(
    bytes: &[u8],
    page: u32,
    config: &RenderConfig,
) -> Result<RenderedImage, RenderError> {
    config.validate()?;
    if bytes.len() > config.max_input_bytes {
        return Err(RenderError::LimitExceeded);
    }
    if page == 0 {
        return Err(RenderError::InvalidPdf);
    }
    let metadata_doc = metadata::load(bytes).map_err(|_| RenderError::InvalidPdf)?;
    if metadata_doc.trailer.has(b"Encrypt") {
        return Err(RenderError::InvalidPdf);
    }
    let tree = metadata::page_tree(&metadata_doc, config.max_pages)
        .map_err(|_| RenderError::InvalidPdf)?;
    let id = tree
        .get((page - 1) as usize)
        .ok_or(RenderError::InvalidPdf)?;
    let m = metadata::page_metadata(&metadata_doc, *id)
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    if m.crop_intersected {
        return Err(RenderError::UnsupportedGeometry);
    }
    let r = |v: [f64; 4]| {
        Rect::new(v[0], v[1], v[2], v[3]).map_err(|_| RenderError::UnsupportedGeometry)
    };
    let rotation = match m.normalized_rotate {
        0 => Rotation::Deg0,
        90 => Rotation::Deg90,
        180 => Rotation::Deg180,
        270 => Rotation::Deg270,
        _ => return Err(RenderError::UnsupportedGeometry),
    };
    let geometry = PageGeometry::new(
        r(m.media.value)?,
        r(m.crop.value)?,
        rotation,
        m.user_unit.value,
    )
    .map_err(|_| RenderError::UnsupportedGeometry)?;
    let bounds = geometry
        .display_bounds()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    let width = dimension(bounds.width(), config.pixels_per_point)?;
    let height = dimension(bounds.height(), config.pixels_per_point)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(RenderError::LimitExceeded)?;
    let byte_count = pixels.checked_mul(4).ok_or(RenderError::LimitExceeded)?;
    if pixels > config.max_pixels
        || byte_count > config.max_output_bytes
        || byte_count > usize::MAX as u64
        || u64::from(width) * 4 > i32::MAX as u64
    {
        return Err(RenderError::LimitExceeded);
    }
    let page_count = tree.len();
    drop(metadata_doc);
    let (png, transform) = match &config.backend {
        #[cfg(feature = "pdfium")]
        RenderBackend::Pdfium(path) => render_pdfium(
            bytes, page, path, &geometry, width, height, page_count, config,
        )?,
        #[cfg(feature = "hayro")]
        RenderBackend::Hayro => {
            render_hayro(bytes, page, &geometry, width, height, page_count, config)?
        }
        #[allow(unreachable_patterns)]
        _ => return Err(RenderError::BackendUnavailable),
    };
    Ok(RenderedImage {
        png,
        width,
        height,
        transform,
        geometry,
    })
}

#[cfg(feature = "hayro")]
#[allow(clippy::too_many_arguments)]
fn render_hayro(
    bytes: &[u8],
    page: u32,
    geometry: &PageGeometry,
    width: u32,
    height: u32,
    page_count: usize,
    config: &RenderConfig,
) -> Result<(Vec<u8>, ImageTransform), RenderError> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::kurbo::Affine;
    use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    // Vello's target dimensions are u16. Reject rather than truncate.
    let w = u16::try_from(width).map_err(|_| RenderError::LimitExceeded)?;
    let h = u16::try_from(height).map_err(|_| RenderError::LimitExceeded)?;
    let doc = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).map_err(|_| RenderError::InvalidPdf)?;
    if doc.pages().len() != page_count {
        return Err(RenderError::UnsupportedGeometry);
    }
    let selected = doc
        .pages()
        .get((page - 1) as usize)
        .ok_or(RenderError::InvalidPdf)?;
    let crop = selected.intersected_crop_box();
    if !near(crop.x0, geometry.crop_box.left)
        || !near(crop.y0, geometry.crop_box.top)
        || !near(crop.x1, geometry.crop_box.right)
        || !near(crop.y1, geometry.crop_box.bottom)
    {
        return Err(RenderError::UnsupportedGeometry);
    }
    // Use the renderer mapping, then verify it against canonical geometry.
    // Non-unit UserUnit is rejected by PageGeometry before reaching this path.
    let bounds = geometry
        .display_bounds()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    use hayro::hayro_interpret::util::TransformExt;
    let affine = Affine::scale_non_uniform(
        f64::from(width) / bounds.width(),
        f64::from(height) / bounds.height(),
    ) * selected.initial_transform(true).to_kurbo();
    let transform = image_transform(
        geometry,
        |p| {
            let p = affine * hayro::kurbo::Point::new(p.x, p.y);
            Point { x: p.x, y: p.y }
        },
        width,
        height,
    )?;
    let unsupported = Arc::new(AtomicBool::new(false));
    let font_failure = unsupported.clone();
    let warning_failure = unsupported.clone();
    let settings = InterpreterSettings {
        // No ambient font discovery, substituted fonts or external resources.
        font_resolver: Arc::new(move |_| {
            font_failure.store(true, Ordering::Relaxed);
            None
        }),
        warning_sink: Arc::new(move |_| {
            warning_failure.store(true, Ordering::Relaxed);
        }),
        render_annotations: false,
        ..Default::default()
    };
    let mut ctx = RenderContext::new(w, h);
    hayro::render_into(
        selected,
        &hayro::RenderCache::new(),
        &settings,
        &hayro::RenderSettings::default(),
        &mut ctx,
        affine,
    );
    if unsupported.load(Ordering::Relaxed) {
        return Err(RenderError::UnsupportedContent);
    }
    ctx.flush();
    let mut pixmap = Pixmap::new(w, h);
    ctx.render_with(
        &mut pixmap,
        &mut Resources::default(),
        RasterizerSettings {
            target_init: TargetInit::Clear(hayro::vello_cpu::color::palette::css::WHITE),
            ..Default::default()
        },
    );
    let png = encode_png(
        pixmap.data_as_u8_slice(),
        width,
        height,
        config.max_output_bytes,
    )?;
    Ok((png, transform))
}

#[cfg(feature = "pdfium")]
#[allow(clippy::too_many_arguments)]
fn render_pdfium(
    bytes: &[u8],
    page: u32,
    path: &std::path::Path,
    geometry: &PageGeometry,
    width: u32,
    height: u32,
    page_count: usize,
    config: &RenderConfig,
) -> Result<(Vec<u8>, ImageTransform), RenderError> {
    let bounds = geometry
        .display_bounds()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    let library = Pdfium::load_from_path(path).map_err(|error| match error {
        firecrawl_pdfium::Error::AlreadyLoaded { .. } => RenderError::LibraryConflict,
        _ => RenderError::InvalidLibrary,
    })?;
    let doc = library
        .load_document(bytes.to_vec(), None)
        .map_err(|_| RenderError::InvalidPdf)?;
    if doc.page_count() != page_count {
        return Err(RenderError::UnsupportedGeometry);
    }
    let native_page = doc
        .page((page - 1) as usize)
        .map_err(|_| RenderError::NativeFailure)?;
    let native_box = native_page
        .bounding_box()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    let crop = geometry.crop_box;
    // The epsilon accommodates native f32 page metrics; it is not a semantic
    // padding allowance and does not replace pixel-marker fixture comparisons.
    if !near(native_page.width() as f64, bounds.width())
        || !near(native_page.height() as f64, bounds.height())
        || !near(native_box.left, crop.left)
        || !near(native_box.bottom, crop.top)
        || !near(native_box.right, crop.right)
        || !near(native_box.top, crop.bottom)
    {
        return Err(RenderError::UnsupportedGeometry);
    }
    let native_config = firecrawl_pdfium::RenderConfig::new()
        .exact(width, height)
        .pixel_format(PixelFormat::Rgba8)
        .annotations(false)
        .form_fields(false)
        .max_output_bytes(config.max_output_bytes);
    // Query the device mapping before allocating the bitmap so unsupported
    // coordinate contracts cannot produce a misleading image ledger.
    let mapping = native_page
        .transform_for(&native_config)
        .map_err(|_| RenderError::NativeFailure)?;
    let transform = image_transform(
        geometry,
        |p| {
            let pixel = mapping.page_to_pixel(PagePoint::new(p.x, p.y));
            Point {
                x: pixel.x,
                y: pixel.y,
            }
        },
        width,
        height,
    )?;
    let bitmap = native_page
        .render(&native_config)
        .map_err(|_| RenderError::NativeFailure)?;
    if bitmap.width() != width
        || bitmap.height() != height
        || bitmap.pixels().len() as u64 != u64::from(width) * u64::from(height) * 4
        || bitmap.transform() != &mapping
    {
        return Err(RenderError::NativeFailure);
    }
    Ok((
        encode_png(bitmap.pixels(), width, height, config.max_output_bytes)?,
        transform,
    ))
}

fn encode_png(pixels: &[u8], width: u32, height: u32, limit: u64) -> Result<Vec<u8>, RenderError> {
    let mut output = LimitedOutput {
        bytes: Vec::new(),
        limit,
    };
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|_| RenderError::EncodingFailure)?;
        writer
            .write_image_data(pixels)
            .map_err(|_| RenderError::EncodingFailure)?;
        writer.finish().map_err(|_| RenderError::EncodingFailure)?;
    }
    Ok(output.bytes)
}

fn dimension(points: f64, scale: f64) -> Result<u32, RenderError> {
    let pixels = (points * scale).round();
    if !pixels.is_finite() || pixels < 1.0 || pixels > i32::MAX as f64 {
        return Err(RenderError::LimitExceeded);
    }
    Ok(pixels as u32)
}

fn near(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-4
}

fn image_transform(
    geometry: &PageGeometry,
    page_to_pixel: impl Fn(Point) -> Point,
    width: u32,
    height: u32,
) -> Result<ImageTransform, RenderError> {
    let bounds = geometry
        .display_bounds()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    let map = |x, y| {
        let p = geometry
            .display_to_pdf(Point { x, y })
            .map_err(|_| RenderError::UnsupportedGeometry)?;
        Ok::<_, RenderError>(page_to_pixel(p))
    };
    let origin = map(0.0, 0.0)?;
    let right = map(bounds.right, 0.0)?;
    let bottom = map(0.0, bounds.bottom)?;
    let mut mapping = AffineTransform {
        a: (right.x - origin.x) / bounds.width(),
        b: (right.y - origin.y) / bounds.width(),
        c: (bottom.x - origin.x) / bounds.height(),
        d: (bottom.y - origin.y) / bounds.height(),
        e: origin.x,
        f: origin.y,
    };
    let raster = Rect::new(0.0, 0.0, width as f64, height as f64)
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    let actual = mapping
        .map_rect(bounds)
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    if mapping.a <= 0.0
        || mapping.d <= 0.0
        || !near(mapping.b, 0.0)
        || !near(mapping.c, 0.0)
        || !near(mapping.e, 0.0)
        || !near(mapping.f, 0.0)
        || !near(right.x, f64::from(width))
        || !near(right.y, 0.0)
        || !near(bottom.x, 0.0)
        || !near(bottom.y, f64::from(height))
        || !near(actual.left, raster.left)
        || !near(actual.top, raster.top)
        || !near(actual.right, raster.right)
        || !near(actual.bottom, raster.bottom)
    {
        return Err(RenderError::UnsupportedGeometry);
    }
    // The verified map is axis aligned and fills this viewport. Remove only
    // floating-point cancellation at its edges; response Boxes stay strict.
    let scale = |pixels: u32, points: f64| {
        let ratio = f64::from(pixels) / points;
        if ratio * points > f64::from(pixels) {
            ratio.next_down()
        } else {
            ratio
        }
    };
    mapping.a = scale(width, bounds.width());
    mapping.d = scale(height, bounds.height());
    mapping.b = 0.0;
    mapping.c = 0.0;
    mapping.e = 0.0;
    mapping.f = 0.0;
    let transform = ImageTransform {
        display_bounds: bounds,
        raster_width: width,
        raster_height: height,
        display_to_raster: mapping,
        crop: raster,
        resized_width: width,
        resized_height: height,
        padding: Padding {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    };
    transform
        .validate()
        .map_err(|_| RenderError::UnsupportedGeometry)?;
    Ok(transform)
}

struct LimitedOutput {
    bytes: Vec<u8>,
    limit: u64,
}
impl Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if (self.bytes.len() as u64).saturating_add(bytes.len() as u64) > self.limit {
            return Err(std::io::Error::other("encoded image limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_scale_preserves_valid_viewport_edge_boxes() {
        for height in [281., 299.] {
            let bounds = Rect::new(0., 0., 280., height).unwrap();
            let geometry = PageGeometry::new(bounds, bounds, Rotation::Deg0, 1.).unwrap();
            let raster_height = (height * 1.5_f64).round() as u32;
            let transform = image_transform(
                &geometry,
                |p| Point {
                    x: p.x * 1.5,
                    y: (height - p.y) * f64::from(raster_height) / height,
                },
                420,
                raster_height,
            )
            .unwrap();
            let actual = transform
                .normalized_box_to_display(Rect::new(0., 0., 1., 1.).unwrap())
                .unwrap();
            assert!((actual.right - 280.).abs() < 1e-10);
            assert!((actual.bottom - height).abs() < 1e-10);
            assert!(
                transform
                    .normalized_box_to_display(Rect::new(0., 0., 1.000000000000001, 1.).unwrap())
                    .is_err()
            );
        }
    }
    #[test]
    fn reflected_and_swapped_native_maps_cannot_become_valid_ledgers() {
        let bounds = Rect::new(0., 0., 100., 100.).unwrap();
        let geometry = PageGeometry::new(bounds, bounds, Rotation::Deg0, 1.).unwrap();
        assert_eq!(
            image_transform(
                &geometry,
                |p| Point {
                    x: 100. - p.x,
                    y: 100. - p.y
                },
                100,
                100
            )
            .unwrap_err(),
            RenderError::UnsupportedGeometry
        );
        assert_eq!(
            image_transform(
                &geometry,
                |p| Point {
                    x: 100. - p.y,
                    y: p.x
                },
                100,
                100
            )
            .unwrap_err(),
            RenderError::UnsupportedGeometry
        );
    }
}
