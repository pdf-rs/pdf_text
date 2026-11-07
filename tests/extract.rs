use pdf::file::FileOptions;

fn fonts() {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        std::env::set_var(
            "STANDARD_FONTS",
            "/home/hematite/task-clones/pdf_render/standard_fonts",
        );
    }
}

fn text_of(flow: &pdf_text::entry::Flow) -> String {
    flow.runs
        .iter()
        .flat_map(|run| run.lines.iter())
        .flat_map(|line| line.words.iter())
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every page must produce a span placed in page coordinates: a span's own
/// rect is in text space, so without the transform it lands at the origin and
/// the page comes out empty.
#[test]
fn text_is_extracted_and_placed_on_the_page() {
    fonts();
    let path = std::env::var("PDF")
        .unwrap_or_else(|_| "/home/hematite/task-clones/pdf/files/libreoffice.pdf".into());
    let file = FileOptions::cached().open(&path).unwrap();
    let resolver = file.resolver();
    let mut pages = 0;
    for (i, page) in file.pages().enumerate().take(3) {
        let page = page.unwrap();
        let flow = pdf_text::run(&file, &page, &resolver).unwrap();
        let text = text_of(&flow);
        assert!(
            text.contains(&format!("Contents of page {}", i + 1)),
            "page {}: {text:?}",
            i + 1
        );
        // The line must have been placed on the page, not at its origin.
        let word = flow.runs[0].lines[0].words[0].rect;
        assert!(word.y > 10.0, "page {}: word at y {}", i + 1, word.y);
        assert!(word.x < 100.0, "page {}: word at x {}", i + 1, word.x);
        pages += 1;
    }
    assert_eq!(pages, 3);
}
