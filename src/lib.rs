use std::collections::HashSet;

use flow::Flow;
use pathfinder_geometry::transform2d::Transform2F;
use pdf::{backend::Backend, object::{Page, Resolve}, PdfError};
use pdf_render::{tracer::{TraceCache, Tracer, DrawItem}, Fill, render_pattern, render_page, FillMode, font::OutlineBuilder};

mod node;
mod util;
mod text;
mod classify;
pub mod flow;

pub fn run<B: Backend>(file: &pdf::file::CachedFile<B>, page: &Page, resolve: &impl Resolve, transform: Transform2F, without_header_and_footer: bool) -> Result<Flow, PdfError> {
    let mut cache = TraceCache::new(OutlineBuilder::default());

    let mut clip_paths = vec![];
    let mut tracer = Tracer::new(&mut cache, &mut clip_paths);

    //Get text, pattern, image by the Tracer backend.
    render_page(&mut tracer, resolve, &page, transform)?;

    let bbox = tracer.view_box();

    let items: Vec<DrawItem<OutlineBuilder>> = tracer.finish();
    //Get all patterns which may have lines and texts inside.
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
            DrawItem::Text(t, _) if bbox.intersects(t.rect) => {
                spans.push(t);
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

    // Analyze patterns to get lines and texts.
    for &p in patterns.iter() {
        let pattern = match resolve.get(p) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("failed to load pattern: {:?}", e);
                continue;
            }
        };
        let mut pat_tracer = Tracer::new(&mut cache, &mut clip_paths);

        render_pattern(&mut pat_tracer, &*pattern, resolve)?;
        let pat_items = pat_tracer.finish();
        for item in pat_items {
            visit_item(item);
        }
    }

    // After this loop, all the text and lines are ready for further processing.
    for item in items {
        visit_item(item);
    }

    let root = node::build(&spans, bbox, &lines, without_header_and_footer);

    let mut flow = Flow::new();
    flow::build(&mut flow, &spans, &root, bbox.min_x());

    Ok(flow)
}