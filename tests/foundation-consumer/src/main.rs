use jev_client::{Asset, Content, Limits, PngImage, Question, RequestDraft, build_request};
use pdf_probe::{PdfLimits, inspect};
use pdf_render::{RenderConfig, render};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = include_bytes!("../image-only.pdf");
    let snapshot = inspect(bytes, PdfLimits::default())?;
    assert_eq!(snapshot.pages.len(), 1);
    assert!(snapshot.fragments.is_empty());
    assert!(!snapshot.pages[0].text_available);
    let bounds = snapshot.geometries[&1].display_bounds()?;
    assert_eq!((bounds.width(), bounds.height()), (280.0, 280.0));
    let config = match std::env::args_os().nth(1) {
        Some(path) => RenderConfig::new(path),
        None => RenderConfig::hayro(),
    };
    let image = render(bytes, 1, &config)?;
    assert_eq!((image.width, image.height), (560, 560));
    let png = PngImage::new("page".into(), image.png)?;
    assert_eq!(png.dimensions(), (560, 560));
    let request = build_request(
        RequestDraft {
            model: "jev-1.14.0".into(),
            state: Content::Text("Inspect <page>".into()),
            questions: BTreeMap::from([(
                "visible".into(),
                Question::Noul {
                    instructions: Content::Text("Is a red image visible?".into()),
                    criteria: None,
                },
            )]),
            assets: BTreeMap::from([(
                "<page>".into(),
                Asset {
                    part: "page".into(),
                },
            )]),
        },
        Limits::default(),
    )?;
    let body = jev_client::multipart::encode(&request, &[png], "consumer-boundary")?;
    assert!(body.danger_zone_images);
    assert!(!body.bytes().is_empty());
    println!("Inspected and rendered an image-only PDF; built a Jev request without sending.");
    Ok(())
}
