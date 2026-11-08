//! Application resource policy. These ceilings do not sandbox PDF parsers.
use crate::pdf::PdfLimits;

/// Deployment limits may lower the supported ceilings; zero is allowed only for recovery.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    pub pdf: PdfLimits,
    pub max_render_pixels: u64,
    pub max_render_output_bytes: u64,
}
impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            pdf: PdfLimits::default(),
            max_render_pixels: 16_000_000,
            max_render_output_bytes: 64 * 1024 * 1024,
        }
    }
}
impl ResourceLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        let ceiling = Self::default();
        if !(1..=ceiling.pdf.max_bytes).contains(&self.pdf.max_bytes)
            || !(1..=ceiling.pdf.max_pages).contains(&self.pdf.max_pages)
            || !(1..=ceiling.pdf.max_items_per_page).contains(&self.pdf.max_items_per_page)
            || self.pdf.max_recovery_pages > ceiling.pdf.max_recovery_pages
            || !(1..=ceiling.max_render_pixels).contains(&self.max_render_pixels)
            || !(1..=ceiling.max_render_output_bytes).contains(&self.max_render_output_bytes)
        {
            return Err("invalid_resource_limits");
        }
        Ok(())
    }
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    pub fn apply_render(&self, render: &mut crate::render::RenderConfig) {
        render.max_input_bytes = render.max_input_bytes.min(self.pdf.max_bytes);
        render.max_pages = render.max_pages.min(self.pdf.max_pages);
        render.max_pixels = render.max_pixels.min(self.max_render_pixels);
        render.max_output_bytes = render.max_output_bytes.min(self.max_render_output_bytes);
    }
}
