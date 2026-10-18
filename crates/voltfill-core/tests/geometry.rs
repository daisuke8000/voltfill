//! Mathematical fixtures only: these do not establish PDFium device agreement.
use voltfill_core::geometry::{
    AffineTransform, GeometryError, ImageTransform, Padding, PageGeometry, Point, Rect, Rotation,
};

fn rect(l: f64, t: f64, r: f64, b: f64) -> Rect {
    Rect::new(l, t, r, b).unwrap()
}

// P0-04's 1e-6 PDF-unit tolerance is only a test comparison tolerance, not a
// padding allowance or a claim about extractor glyph ink/native pixel accuracy.
fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 1e-6, "{actual} != {expected}");
}
fn point_close(actual: Point, expected: Point) {
    close(actual.x, expected.x);
    close(actual.y, expected.y);
}
fn rect_close(actual: Rect, expected: Rect) {
    close(actual.left, expected.left);
    close(actual.top, expected.top);
    close(actual.right, expected.right);
    close(actual.bottom, expected.bottom);
}

fn page(rotation: Rotation) -> PageGeometry {
    PageGeometry::new(
        rect(-50.0, -40.0, 600.0, 800.0),
        rect(10.0, 20.0, 210.0, 120.0),
        rotation,
        1.0,
    )
    .unwrap()
}

#[test]
fn nonzero_crop_origins_and_all_rotations_match_known_markers() {
    // Physical marker P=(40,60) is 30 right/40 up from the crop lower-left.
    let cases = [
        (Rotation::Deg0, Point { x: 30.0, y: 60.0 }, (200.0, 100.0)),
        (Rotation::Deg90, Point { x: 40.0, y: 30.0 }, (100.0, 200.0)),
        (
            Rotation::Deg180,
            Point { x: 170.0, y: 40.0 },
            (200.0, 100.0),
        ),
        (
            Rotation::Deg270,
            Point { x: 60.0, y: 170.0 },
            (100.0, 200.0),
        ),
    ];
    for (rotation, expected, (w, h)) in cases {
        let geometry = page(rotation);
        point_close(
            geometry.pdf_to_display(Point { x: 40.0, y: 60.0 }).unwrap(),
            expected,
        );
        // Inverse checked against independently specified PDF marker.
        point_close(
            geometry.display_to_pdf(expected).unwrap(),
            Point { x: 40.0, y: 60.0 },
        );
        assert_eq!(geometry.display_bounds().unwrap(), rect(0.0, 0.0, w, h));
        for marker in [
            Point { x: 10.0, y: 20.0 },
            Point { x: 210.0, y: 120.0 },
            Point {
                x: 40.125,
                y: 60.625,
            },
        ] {
            point_close(
                geometry
                    .display_to_pdf(geometry.pdf_to_display(marker).unwrap())
                    .unwrap(),
                marker,
            );
        }
    }
}

#[test]
fn rectangle_envelopes_cover_rotated_corners() {
    let pdf = rect(40.0, 60.0, 80.0, 90.0);
    for (rotation, expected) in [
        (Rotation::Deg0, rect(30.0, 30.0, 70.0, 60.0)),
        (Rotation::Deg90, rect(40.0, 30.0, 70.0, 70.0)),
        (Rotation::Deg180, rect(130.0, 40.0, 170.0, 70.0)),
        (Rotation::Deg270, rect(30.0, 130.0, 60.0, 170.0)),
    ] {
        let geometry = page(rotation);
        assert_eq!(geometry.pdf_rect_to_display(pdf).unwrap(), expected);
        assert_eq!(geometry.display_rect_to_pdf(expected).unwrap(), pdf);
    }
    // Non-axis-aligned affine mapping requires all four corners, not a diagonal.
    let transform = AffineTransform {
        a: 1.0,
        b: 1.0,
        c: -1.0,
        d: 1.0,
        e: 100.0,
        f: 0.0,
    };
    assert_eq!(
        transform.map_rect(rect(10.0, 20.0, 30.0, 40.0)).unwrap(),
        rect(70.0, 30.0, 110.0, 70.0)
    );
}

#[test]
fn extractor_display_bounds_are_flipped_once() {
    let rotated = page(Rotation::Deg90); // Display dimensions 100 x 200.
    assert_eq!(
        rotated
            .extractor_rect_to_display(12.0, 30.0, 20.0, 10.0)
            .unwrap(),
        rect(12.0, 160.0, 32.0, 170.0)
    );
    assert!(
        rotated
            .extractor_rect_to_display(95.0, 30.0, 20.0, 10.0)
            .is_err()
    );
    assert!(
        rotated
            .extractor_rect_to_display(12.0, 30.0, -20.0, 10.0)
            .is_err()
    );
}

fn ledger() -> ImageTransform {
    // D 200x100 -> R 400x300; crop (40,60)-(240,180), resize to
    // 100x120, then left/right=50 and top/bottom=40 => I 200x200.
    ImageTransform {
        display_bounds: rect(0.0, 0.0, 200.0, 100.0),
        raster_width: 400,
        raster_height: 300,
        display_to_raster: AffineTransform {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 3.0,
            e: 0.0,
            f: 0.0,
        },
        crop: rect(40.0, 60.0, 240.0, 180.0),
        resized_width: 100,
        resized_height: 120,
        padding: Padding {
            left: 50,
            top: 40,
            right: 50,
            bottom: 40,
        },
    }
}

#[test]
fn normalized_image_box_undoes_actual_crop_resize_and_padding() {
    let image = ledger();
    assert_eq!(image.sent_dimensions().unwrap(), (200, 200));
    // N=(.30,.35)-(.50,.65) => I=(60,70)-(100,130)
    // => R=(60,90)-(140,150) => D=(30,30)-(70,50).
    rect_close(
        image
            .normalized_box_to_display(rect(0.30, 0.35, 0.50, 0.65))
            .unwrap(),
        rect(30.0, 30.0, 70.0, 50.0),
    );
    rect_close(
        image
            .normalized_box_to_display(rect(0.25, 0.20, 0.75, 0.80))
            .unwrap(),
        rect(20.0, 20.0, 120.0, 60.0),
    );
}

#[test]
fn normalized_boxes_recover_the_same_pdf_region_for_every_rotation() {
    // Independent sent-image boxes for the same P=(40,60)-(80,90) marker.
    // Full-page raster at 2x, resized to 200x200 and padded to 400x400.
    for (rotation, box_) in [
        (Rotation::Deg0, rect(0.325, 0.40, 0.425, 0.55)),
        (Rotation::Deg90, rect(0.45, 0.325, 0.60, 0.425)),
        (Rotation::Deg180, rect(0.575, 0.45, 0.675, 0.60)),
        (Rotation::Deg270, rect(0.40, 0.575, 0.55, 0.675)),
    ] {
        let page = page(rotation);
        let bounds = page.display_bounds().unwrap();
        let raster_width = (bounds.width() * 2.0) as u32;
        let raster_height = (bounds.height() * 2.0) as u32;
        let image = ImageTransform {
            display_bounds: bounds,
            raster_width,
            raster_height,
            display_to_raster: AffineTransform {
                a: 2.0,
                b: 0.0,
                c: 0.0,
                d: 2.0,
                e: 0.0,
                f: 0.0,
            },
            crop: rect(0.0, 0.0, raster_width as f64, raster_height as f64),
            resized_width: 200,
            resized_height: 200,
            padding: Padding {
                left: 100,
                top: 100,
                right: 100,
                bottom: 100,
            },
        };
        let display = image.normalized_box_to_display(box_).unwrap();
        rect_close(
            page.display_rect_to_pdf(display).unwrap(),
            rect(40.0, 60.0, 80.0, 90.0),
        );
    }
}

#[test]
fn image_box_rejects_padding_partial_padding_and_out_of_range() {
    for box_ in [
        rect(0.0, 0.0, 0.1, 0.1),
        rect(0.2, 0.3, 0.4, 0.4),
        rect(0.3, 0.7, 0.4, 0.9),
    ] {
        assert_eq!(
            ledger().normalized_box_to_display(box_),
            Err(GeometryError::PaddingIntersection)
        );
    }
    assert_eq!(
        ledger().normalized_box_to_display(rect(-0.01, 0.3, 0.4, 0.4)),
        Err(GeometryError::OutOfBounds)
    );
    assert_eq!(
        ledger().normalized_box_to_display(rect(0.3, 0.3, 1.01, 0.4)),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn invalid_rectangles_and_page_metadata_are_rejected() {
    for coords in [
        (0.0, 0.0, 0.0, 1.0),
        (2.0, 0.0, 1.0, 1.0),
        (0.0, 0.0, 1.0, f64::NAN),
        (0.0, 0.0, f64::INFINITY, 1.0),
        (-f64::MAX, 0.0, f64::MAX, 1.0),
    ] {
        assert!(Rect::new(coords.0, coords.1, coords.2, coords.3).is_err());
    }
    let geometry = page(Rotation::Deg0);
    assert!(geometry.pdf_to_display(Point { x: 0.0, y: 30.0 }).is_err());
    assert!(
        geometry
            .display_to_pdf(Point {
                x: f64::NAN,
                y: 30.0
            })
            .is_err()
    );
    assert!(
        geometry
            .display_to_pdf(Point { x: 201.0, y: 30.0 })
            .is_err()
    );
    assert!(PageGeometry::new(geometry.media_box, geometry.crop_box, Rotation::Deg0, 2.0).is_err());
    assert!(PageGeometry::new(geometry.crop_box, geometry.media_box, Rotation::Deg0, 1.0).is_err());
}

#[test]
fn ledger_revalidates_untrusted_fields_and_checked_dimensions() {
    let mut image = ledger();
    image.padding.right = u32::MAX;
    assert_eq!(image.validate(), Err(GeometryError::InvalidImageDimensions));
    image = ledger();
    image.resized_width = 0;
    assert_eq!(image.validate(), Err(GeometryError::InvalidImageDimensions));
    image = ledger();
    image.display_to_raster.d = 0.0;
    assert_eq!(image.validate(), Err(GeometryError::InvalidTransform));
    image = ledger();
    image.display_to_raster.e = f64::NAN;
    assert_eq!(image.validate(), Err(GeometryError::InvalidTransform));
    image = ledger();
    image.crop.right = 401.0;
    assert_eq!(image.validate(), Err(GeometryError::OutOfBounds));
    let invalid = Rect {
        left: 0.4,
        top: 0.4,
        right: 0.3,
        bottom: 0.5,
    };
    assert_eq!(
        ledger().normalized_box_to_display(invalid),
        Err(GeometryError::InvalidCoordinates)
    );
    let mut geometry = page(Rotation::Deg0);
    geometry.user_unit = f64::NAN;
    assert!(geometry.pdf_to_display(Point { x: 40.0, y: 60.0 }).is_err());
}

#[test]
fn renderer_margin_is_not_valid_page_content() {
    let mut image = ledger();
    image.display_to_raster.e = 20.0;
    image.raster_width = 420;
    image.crop = rect(0.0, 0.0, 200.0, 120.0);
    // The raster crop includes a renderer margin to the left of canonical x=0.
    assert_eq!(
        image.normalized_box_to_display(rect(0.25, 0.20, 0.30, 0.30)),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn shear_cannot_turn_an_outside_source_into_box_evidence() {
    let mut image = ledger();
    image.display_to_raster.b = 0.;
    image.display_to_raster.c = 1.;
    assert_eq!(image.validate(), Err(GeometryError::InvalidTransform));
}
