//! What a span's rect means.

use pdf::file::FileOptions;
use pdf_render::render_page;
use pdf_render::tracer::{DrawItem, Tracer};

fn fonts() {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        std::env::set_var(
            "STANDARD_FONTS",
            "/home/hematite/task-clones/pdf_render/standard_fonts",
        );
    }
}

/// A span's rect is one em high in *its own text space*, and the transform
/// carries the text matrix — not the font size. `foreword-tj-lead.pdf` is the
/// case that tells the two apart: it draws its body with `1 Tf` and a text
/// matrix scaled by 3.514, so the rect has to come out 3.5 points tall on the
/// page. While it was a literal 1.0, every line of it was a tenth of a line
/// high and the tree could not group the runs into lines.
#[test]
fn a_span_is_one_em_high_on_the_page() {
    fonts();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/foreword-tj-lead.pdf");
    let file = FileOptions::cached().open(path).unwrap();
    let resolver = file.resolver();
    let page = file.pages().next().unwrap().unwrap();

    let mut clip_paths = vec![];
    let cache = pdf_text::new_trace_cache();
    let mut tracer = Tracer::<pdf_text::Encoder>::new(&cache, &mut clip_paths);
    render_page(&mut tracer, &resolver, &page, Default::default()).unwrap();

    let mut checked = 0;
    for item in tracer.finish() {
        if let DrawItem::Text(t, _) = item {
            if !t.text.starts_with("On January") {
                continue;
            }
            // `1 Tf` with the size in the matrix: the em, on the page, is the
            // matrix scale of the transform.
            let scale = t.transform.matrix.m11();
            assert_eq!(t.font_size, 1.0, "{:?}", t.text);
            let rect = t.transform * t.rect;
            let em = t.font_size * scale;
            assert!(
                (rect.height() - em).abs() < 1e-3,
                "rect is {:?} high, one em is {em}",
                rect.height()
            );
            // and it sits on the baseline: from one em below it up to it
            assert!((rect.max_y() - t.transform.vector.y()).abs() < 1e-3);
            checked += 1;
        }
    }
    assert_eq!(checked, 1, "the first line of the foreword was not found");
}
