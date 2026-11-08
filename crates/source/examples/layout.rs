//! Print the layout hypotheses for a PDF: `cargo run -p source --example layout -- INPUT.pdf`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: layout INPUT.pdf")?;
    let snapshot = source::inspect(&std::fs::read(path)?, source::PdfLimits::default())?;
    let layout = source::layout::analyze(&snapshot.fragments);
    for pair in &layout.pairs {
        let text = |s: &source::layout::Span| {
            snapshot
                .fragments
                .iter()
                .find(|f| f.id == s.fragment_id)
                .map_or("", |f| &f.raw_text[s.start..s.end])
                .to_owned()
        };
        let value: Vec<String> = pair.value.iter().map(text).collect();
        println!(
            "{:?}\t{}\t=> {}\t[{:?}]",
            pair.placement,
            text(&pair.label),
            value.join(" / "),
            pair.value[0].precision
        );
    }
    Ok(())
}
