use pdf::file::FileOptions;

fn fonts() {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        std::env::set_var(
            "STANDARD_FONTS",
            "/home/hematite/task-clones/pdf_render/standard_fonts",
        );
    }
}

/// All words of the page, whitespace normalised.
fn text_of_pdf(path: &str) -> String {
    let file = FileOptions::cached().open(path).unwrap();
    let resolver = file.resolver();
    let page = file.pages().next().unwrap().unwrap();
    let flow = pdf_text::run(&file, &page, &resolver).unwrap();
    flow.runs
        .iter()
        .flat_map(|run| run.lines.iter())
        .flat_map(|line| line.words.iter())
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `TJ` array may open with a kerning adjustment before its first string:
/// the text matrix then points at the start of the *gap*, not at the first
/// glyph. The span has to start where the ink does, otherwise its rect
/// overlaps the text in front of it and the words come out in the wrong
/// order — the Foreword of ISO 32000-1 draws
/// `9.96 0 0 9.96 252.03 694.76 Tm [-4595(r th)…]TJ`, which used to extract as
/// "… Association (AIIM), r the purpose … International fo Organization".
#[test]
fn leading_tj_adjustment_does_not_shift_the_span() {
    fonts();
    let text = text_of_pdf(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/foreword-tj-lead.pdf"
    ));
    assert!(
        text.contains(
            "Association (AIIM), for the purpose of publication by the International \
             Organization for Standardization (ISO)."
        ),
        "{text:?}"
    );
    // The pieces of the split "for" must not end up on the other line.
    assert!(!text.contains("International fo "), "{text:?}");
}

/// Same page, same kind of trap in the middle of a word: `(S)Tj 0.675 0 Td
/// (yste)Tj` followed by a new text matrix for "ms is …".
#[test]
fn text_split_into_several_runs_stays_in_order() {
    fonts();
    let text = text_of_pdf(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/foreword-tj-lead.pdf"
    ));
    assert!(
        text.contains("By agreement with ISO, Adobe Systems is allowed"),
        "{text:?}"
    );
}

/// Words are joined with single spaces, also where one text run ends with a
/// space and the next one starts with a glyph.
#[test]
fn no_empty_words() {
    fonts();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/foreword-tj-lead.pdf");
    let file = FileOptions::cached().open(path).unwrap();
    let resolver = file.resolver();
    let page = file.pages().next().unwrap().unwrap();
    let flow = pdf_text::run(&file, &page, &resolver).unwrap();
    for run in &flow.runs {
        for line in &run.lines {
            for word in &line.words {
                assert!(!word.text.is_empty(), "{:?}", word.text);
                assert!(!word.text.trim().is_empty(), "{:?}", word.text);
            }
        }
    }
}
