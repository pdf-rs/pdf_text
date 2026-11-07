use std::collections::HashSet;

use entry::Flow;
use pdf::{backend::Backend, object::{Page, Resolve}, PdfError};
use pdf_render::{tracer::{TraceCache, Tracer, DrawItem, NullEncoder}, TextSpan, Fill, render_pattern, render_page, FillMode};

/// The encoder text extraction runs with: none at all.
///
/// `pdf_render` builds glyph outlines while it loads fonts; extracting text
/// needs the metrics and unicode maps but never the paths, so the null
/// encoder throws the outlines away instead of feeding them into vello/-
/// pathfinder.
pub type Encoder = NullEncoder;

/// A span of text as produced by [`run`]. Everything below is written against
/// this encoder, so callers do not have to name it.
pub type Span = TextSpan<Encoder>;

mod tree;
pub mod util;
pub mod text;
pub mod entry;

pub fn run<B: Backend>(file: &pdf::file::CachedFile<B>, page: &Page, resolve: &impl Resolve) -> Result<Flow, PdfError> {
    let mut document = text::Stats::new();
    run_with_stats(file, page, resolve, &mut document)
}

/// Like [`run`], but keeps statistics about how the text of a document is
/// spaced. Pass the same [`text::Stats`] for every page of one file: at first
/// a page measures its own spacing, and as pages accumulate the document's
/// spacing becomes available to pages that hold too little text to tell
/// (a cover, a page that is one heading plus a figure).
pub fn run_with_stats<B: Backend>(
    file: &pdf::file::CachedFile<B>,
    page: &Page,
    resolve: &impl Resolve,
    document: &mut text::Stats,
) -> Result<Flow, PdfError> {
    let cache: TraceCache<Encoder> = TraceCache::new();
    run_cached(file, page, resolve, document, &cache)
}

/// A [`TraceCache`] to share between the pages of one document.
///
/// Loading a font is by far the most expensive part of extracting a page
/// (seven times the cost of running the page's content stream itself), and
/// the cache is what makes it happen once per font instead of once per page:
/// pass the same cache to [`run_cached`] for every page of a file.
pub fn new_trace_cache() -> TraceCache<Encoder> {
    TraceCache::new()
}

/// Like [`run_with_stats`], with a font cache shared across the document.
pub fn run_cached<B: Backend>(
    file: &pdf::file::CachedFile<B>,
    page: &Page,
    resolve: &impl Resolve,
    document: &mut text::Stats,
    cache: &TraceCache<Encoder>,
) -> Result<Flow, PdfError> {
    let mut clip_paths = vec![];
    let mut tracer = Tracer::<Encoder>::new(cache, &mut clip_paths);

    render_page(&mut tracer, resolve, &page, Default::default())?;

    let bbox = tracer.view_box();

    let items = tracer.finish();
    let mut patterns = HashSet::new();
    for item in items.iter() {
        if let DrawItem::Vector(ref v) = item {
            if let Some(FillMode { color: Fill::Pattern(id), .. }) = v.fill {
                patterns.insert(id);
            }
            if let Some((FillMode { color: Fill::Pattern(id), .. }, _)) = v.stroke {
                patterns.insert(id);
            }
        }
    }

    let mut spans = vec![];
    let mut lines = vec![];
    let mut visit_item = |item| {
        match item {
            DrawItem::Text(mut t, _) => {
                // A span's rect is given in text space (origin on the
                // baseline, one em high); everything here works in page
                // coordinates, so place it with the span's own transform.
                t.rect = t.transform * t.rect;
                if bbox.intersects(t.rect) {
                    spans.push(t);
                }
            }
            DrawItem::Vector(path) if bbox.intersects(path.outline.bounds()) => {
                for contour in path.outline.contours() {
                    use pathfinder_content::{outline::ContourIterFlags, segment::SegmentKind};
                    for segment in contour.iter(ContourIterFlags::empty()) {
                        match segment.kind {
                            SegmentKind::Line => lines.push([
                                segment.baseline.from_x(),
                                segment.baseline.from_y(),
                                segment.baseline.to_x(),
                                segment.baseline.to_y()
                            ]),
                            _ => {}
                        }
                    }
                }

            }
            _ => {}
        }
    };

    for &p in patterns.iter() {
        let pattern = match resolve.get(p) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("failed to load pattern: {:?}", e);
                continue;
            }
        };
        let mut pat_tracer = Tracer::new(&cache, &mut clip_paths);

        render_pattern(&mut pat_tracer, &*pattern, resolve)?;
        let pat_items = pat_tracer.finish();
        for item in pat_items {
            visit_item(item);
        }
    }

    for item in items {
        visit_item(item);
    }

    // Measure this page's spacing before the spans are consumed.
    let mut page_stats = text::Stats::new();
    page_stats.observe(spans.iter());

    let root = tree::build(&spans, bbox, &lines);
    let mut flow = Flow::new();
    let ctx = text::Context::new(Some(&page_stats), Some(&*document));
    tree::items(&mut flow, &spans, &root, bbox.min_x(), ctx);
    document.merge(&page_stats);
    Ok(flow)
}