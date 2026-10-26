//! Pure geometry contracts; these alone do not establish agreement with a native renderer.
//! Canonical Display uses a top-left origin and downward y. PDF user coordinates
//! use upward y; `Rect::top` there means the minimum y, not the visual top.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GeometryError {
    #[error("coordinates must be finite and rectangles must have positive extent")]
    InvalidCoordinates,
    #[error("geometry lies outside its coordinate scope")]
    OutOfBounds,
    #[error("only UserUnit=1 is supported")]
    UnsupportedUserUnit,
    #[error("transform is singular or non-finite")]
    InvalidTransform,
    #[error("image dimensions are zero or overflow")]
    InvalidImageDimensions,
    #[error("box intersects image padding")]
    PaddingIntersection,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    fn validate(self) -> Result<Self, GeometryError> {
        if self.x.is_finite() && self.y.is_finite() {
            Ok(self)
        } else {
            Err(GeometryError::InvalidCoordinates)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    /// Compatibility name for [`Self::try_new`].
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Result<Self, GeometryError> {
        Self::try_new(left, top, right, bottom)
    }

    /// Require finite coordinates and a rectangle with positive extent.
    pub fn try_new(left: f64, top: f64, right: f64, bottom: f64) -> Result<Self, GeometryError> {
        let rect = Self {
            left,
            top,
            right,
            bottom,
        };
        rect.validate()?;
        Ok(rect)
    }

    pub fn validate(self) -> Result<(), GeometryError> {
        if [
            self.left,
            self.top,
            self.right,
            self.bottom,
            self.width(),
            self.height(),
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.left < self.right
            && self.top < self.bottom
        {
            Ok(())
        } else {
            Err(GeometryError::InvalidCoordinates)
        }
    }

    #[must_use]
    pub fn width(self) -> f64 {
        self.right - self.left
    }
    #[must_use]
    pub fn height(self) -> f64 {
        self.bottom - self.top
    }

    #[must_use]
    /// Include touching boundaries; return false if either rectangle is invalid.
    pub fn contains(self, other: Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && other.left >= self.left
            && other.top >= self.top
            && other.right <= self.right
            && other.bottom <= self.bottom
    }

    fn contains_point(self, point: Point) -> bool {
        point.x >= self.left
            && point.x <= self.right
            && point.y >= self.top
            && point.y <= self.bottom
    }

    fn map(
        self,
        mut transform: impl FnMut(Point) -> Result<Point, GeometryError>,
    ) -> Result<Self, GeometryError> {
        self.validate()?;
        let corners = [
            Point {
                x: self.left,
                y: self.top,
            },
            Point {
                x: self.right,
                y: self.top,
            },
            Point {
                x: self.right,
                y: self.bottom,
            },
            Point {
                x: self.left,
                y: self.bottom,
            },
        ];
        let mut left = f64::INFINITY;
        let mut top = f64::INFINITY;
        let mut right = f64::NEG_INFINITY;
        let mut bottom = f64::NEG_INFINITY;
        for corner in corners {
            let mapped = transform(corner)?.validate()?;
            left = left.min(mapped.x);
            top = top.min(mapped.y);
            right = right.max(mapped.x);
            bottom = bottom.max(mapped.y);
        }
        Self::new(left, top, right, bottom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageGeometry {
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotation: Rotation,
    pub user_unit: f64,
}

impl PageGeometry {
    /// CropBox must be contained by MediaBox; unsupported metadata is not repaired.
    /// Compatibility name for [`Self::try_new`].
    pub fn new(
        media_box: Rect,
        crop_box: Rect,
        rotation: Rotation,
        user_unit: f64,
    ) -> Result<Self, GeometryError> {
        Self::try_new(media_box, crop_box, rotation, user_unit)
    }

    /// Validate supported page geometry without repairing unsupported metadata.
    pub fn try_new(
        media_box: Rect,
        crop_box: Rect,
        rotation: Rotation,
        user_unit: f64,
    ) -> Result<Self, GeometryError> {
        let page = Self {
            media_box,
            crop_box,
            rotation,
            user_unit,
        };
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), GeometryError> {
        self.media_box.validate()?;
        self.crop_box.validate()?;
        if self.user_unit != 1.0 {
            return Err(GeometryError::UnsupportedUserUnit);
        }
        if !self.media_box.contains(self.crop_box) {
            return Err(GeometryError::OutOfBounds);
        }
        Ok(())
    }

    /// Return the rotated CropBox extent in top-left-origin Display coordinates.
    pub fn display_bounds(&self) -> Result<Rect, GeometryError> {
        self.validate()?;
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        let (w, h) = match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => (w, h),
            Rotation::Deg90 | Rotation::Deg270 => (h, w),
        };
        Rect::try_new(0.0, 0.0, w, h)
    }

    /// Convert a PDF user-space point inside CropBox to Display coordinates.
    pub fn pdf_to_display(&self, point: Point) -> Result<Point, GeometryError> {
        self.validate()?;
        point.validate()?;
        if !self.crop_box.contains_point(point) {
            return Err(GeometryError::OutOfBounds);
        }
        let (x, y) = (point.x - self.crop_box.left, point.y - self.crop_box.top);
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        let (x, y) = match self.rotation {
            Rotation::Deg0 => (x, h - y),
            Rotation::Deg90 => (y, x),
            Rotation::Deg180 => (w - x, y),
            Rotation::Deg270 => (h - y, w - x),
        };
        Point { x, y }.validate()
    }

    /// Convert a point inside Display bounds back to PDF user space.
    pub fn display_to_pdf(&self, point: Point) -> Result<Point, GeometryError> {
        point.validate()?;
        if !self.display_bounds()?.contains_point(point) {
            return Err(GeometryError::OutOfBounds);
        }
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        let (x, y) = match self.rotation {
            Rotation::Deg0 => (point.x, h - point.y),
            Rotation::Deg90 => (point.y, point.x),
            Rotation::Deg180 => (w - point.x, point.y),
            Rotation::Deg270 => (w - point.y, h - point.x),
        };
        Point {
            x: x + self.crop_box.left,
            y: y + self.crop_box.top,
        }
        .validate()
    }

    pub fn pdf_rect_to_display(&self, rect: Rect) -> Result<Rect, GeometryError> {
        rect.map(|p| self.pdf_to_display(p))
    }

    pub fn display_rect_to_pdf(&self, rect: Rect) -> Result<Rect, GeometryError> {
        rect.map(|p| self.display_to_pdf(p))
    }

    /// Display TextItems are already rotated and cropped upstream. Only flip y.
    /// These are item bounds, not an assertion about glyph ink or baseline height.
    pub fn extractor_rect_to_display(
        &self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<Rect, GeometryError> {
        Rect::try_new(x, y, x + width, y + height)?;
        let bounds = self.display_bounds()?;
        let rect = Rect::try_new(x, bounds.bottom - y - height, x + width, bounds.bottom - y)?;
        if !bounds.contains(rect) {
            return Err(GeometryError::OutOfBounds);
        }
        Ok(rect)
    }
}

/// Explicit renderer-reported mapping: x'=a*x+c*y+e, y'=b*x+d*y+f.
/// This module does not infer a native transform from DPI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AffineTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl AffineTransform {
    pub fn inverse(self) -> Result<Self, GeometryError> {
        if ![self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(GeometryError::InvalidTransform);
        }
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det == 0.0 {
            return Err(GeometryError::InvalidTransform);
        }
        let inverse = Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        };
        if ![
            inverse.a, inverse.b, inverse.c, inverse.d, inverse.e, inverse.f,
        ]
        .iter()
        .all(|v| v.is_finite())
        {
            return Err(GeometryError::InvalidTransform);
        }
        Ok(inverse)
    }

    pub fn map_point(self, point: Point) -> Result<Point, GeometryError> {
        self.inverse()?;
        point.validate()?;
        Point {
            x: self.a * point.x + self.c * point.y + self.e,
            y: self.b * point.x + self.d * point.y + self.f,
        }
        .validate()
    }

    pub fn map_rect(self, rect: Rect) -> Result<Rect, GeometryError> {
        rect.map(|point| self.map_point(point))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Padding {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// Ledger for an actual raster crop, rounded resize dimensions, then padding.
/// All public/deserialized fields are revalidated at conversion boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageTransform {
    pub display_bounds: Rect,
    pub raster_width: u32,
    pub raster_height: u32,
    pub display_to_raster: AffineTransform,
    pub crop: Rect,
    pub resized_width: u32,
    pub resized_height: u32,
    pub padding: Padding,
}

impl ImageTransform {
    pub fn validate(&self) -> Result<(), GeometryError> {
        self.display_bounds.validate()?;
        self.display_to_raster.inverse()?;
        // Rectangular containment requires an axis-aligned Display-to-raster map.
        if self.display_to_raster.b != 0.0
            || self.display_to_raster.c != 0.0
            || self.display_to_raster.a <= 0.0
            || self.display_to_raster.d <= 0.0
        {
            return Err(GeometryError::InvalidTransform);
        }
        self.crop.validate()?;
        if self.raster_width == 0
            || self.raster_height == 0
            || self.resized_width == 0
            || self.resized_height == 0
        {
            return Err(GeometryError::InvalidImageDimensions);
        }
        let raster = Rect::try_new(
            0.0,
            0.0,
            self.raster_width as f64,
            self.raster_height as f64,
        )?;
        if !raster.contains(self.crop)
            || !raster.contains(self.display_to_raster.map_rect(self.display_bounds)?)
        {
            return Err(GeometryError::OutOfBounds);
        }
        self.sent_dimensions()?;
        Ok(())
    }

    pub fn sent_dimensions(&self) -> Result<(u32, u32), GeometryError> {
        let w = self
            .resized_width
            .checked_add(self.padding.left)
            .and_then(|v| v.checked_add(self.padding.right));
        let h = self
            .resized_height
            .checked_add(self.padding.top)
            .and_then(|v| v.checked_add(self.padding.bottom));
        match (w, h) {
            (Some(w), Some(h))
                if w > 0 && h > 0 && self.resized_width > 0 && self.resized_height > 0 =>
            {
                Ok((w, h))
            }
            _ => Err(GeometryError::InvalidImageDimensions),
        }
    }

    /// Rejects partial padding intersections as well as boxes wholly in padding.
    /// Bounds are strict; no clamp turns an invalid response into a valid Box.
    pub fn normalized_box_to_display(&self, normalized: Rect) -> Result<Rect, GeometryError> {
        self.validate()?;
        normalized.validate()?;
        if !Rect::try_new(0.0, 0.0, 1.0, 1.0)?.contains(normalized) {
            return Err(GeometryError::OutOfBounds);
        }
        let (w, h) = self.sent_dimensions()?;
        let image = Rect::try_new(
            normalized.left * w as f64,
            normalized.top * h as f64,
            normalized.right * w as f64,
            normalized.bottom * h as f64,
        )?;
        let content = Rect::try_new(
            self.padding.left as f64,
            self.padding.top as f64,
            self.padding.left as f64 + self.resized_width as f64,
            self.padding.top as f64 + self.resized_height as f64,
        )?;
        if !content.contains(image) {
            return Err(GeometryError::PaddingIntersection);
        }
        let sx = self.crop.width() / self.resized_width as f64;
        let sy = self.crop.height() / self.resized_height as f64;
        let raster = Rect::try_new(
            (image.left - content.left) * sx + self.crop.left,
            (image.top - content.top) * sy + self.crop.top,
            (image.right - content.left) * sx + self.crop.left,
            (image.bottom - content.top) * sy + self.crop.top,
        )?;
        let mut display = self.display_to_raster.inverse()?.map_rect(raster)?;
        // The response and padding bounds above are strict. Only arithmetic
        // roundoff from the validated affine round trip may cross a source edge.
        let edge = |value: f64, low: f64, high: f64| {
            let tolerance = 16.0 * f64::EPSILON * (high - low).max(1.0);
            if value < low && low - value <= tolerance {
                low
            } else if value > high && value - high <= tolerance {
                high
            } else {
                value
            }
        };
        display.left = edge(
            display.left,
            self.display_bounds.left,
            self.display_bounds.right,
        );
        display.right = edge(
            display.right,
            self.display_bounds.left,
            self.display_bounds.right,
        );
        display.top = edge(
            display.top,
            self.display_bounds.top,
            self.display_bounds.bottom,
        );
        display.bottom = edge(
            display.bottom,
            self.display_bounds.top,
            self.display_bounds.bottom,
        );
        display.validate()?;
        if !self.display_bounds.contains(display) {
            return Err(GeometryError::OutOfBounds);
        }
        Ok(display)
    }
}
