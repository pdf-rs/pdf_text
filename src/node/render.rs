use font::Encoder;
use itertools::Itertools;
use pathfinder_geometry::{rect::RectF, vector::Vector2F};
use pdf_render::TextSpan;

use crate::classify::classify;

use super::Node;

pub fn render<E: Encoder>(w: &mut String, spans: &[TextSpan<E>], node: &Node, bbox: RectF) {
    _render(w, spans, node, bbox, 0)
}

fn _render<E: Encoder>(w: &mut String, spans: &[TextSpan<E>], node: &Node, bbox: RectF, level: usize) {
    use std::fmt::Write;

    match *node {
        Node::Final { ref indices } => {
            /*
            for i in start..end {
                if let Span::Text(ref t) = spans[i] {
                    write!(w, r#"<text"#).unwrap();
                    write!(w, r#" font-size="{}""#, t.font_size).unwrap();
                    write!(w, r#" transform="{}""#, Transform::from(t.transform)).unwrap();
                    write_text_span(w, t);
                    write!(w, "</text>").unwrap();
                }
            }
            */
            
            if indices.len() > 0 {
                let class = classify(indices.iter().cloned().filter_map(|i| spans.get(i)));

                for &i in indices.iter() {
                    let r = spans[i].rect;
                    write!(w, r#"<line x1="{}" x2="{}" y1="{}" y2="{}" class="{:?}" />"#,
                        r.min_x(), r.max_x(), r.max_y(), r.max_y(),
                        class
                    );
                }
            }
        }
        Node::Grid { ref x, ref y, ref cells, tag } => {
            use std::iter::once;
            let columns = x.len() + 1;
            write!(w, r#"<rect x="{}" y="{}" width="{}" height="{}" class="{:?}" />"#,
                bbox.min_x(), bbox.min_y(), bbox.width(), bbox.height(), tag
            );

            for (j, ((min_y, max_y), row)) in once(bbox.min_y()).chain(y.iter().cloned()).chain(once(bbox.max_y())).tuple_windows().zip(cells.chunks_exact(columns)).enumerate() {
                if j > 0 {
                    writeln!(w, r#"<line x1="{}" x2="{}" y1="{}" y2="{}" level="{level}"></line>"#,
                        bbox.min_x(), bbox.max_x(), min_y, min_y);
                }

                for (i, ((min_x, max_x), cell)) in once(bbox.min_x()).chain(x.iter().cloned()).chain(once(bbox.max_x())).tuple_windows().zip(row).enumerate() {
                    if i > 0 {
                        writeln!(w, r#"<line x1="{}" x2="{}" y1="{}" y2="{}" level="{level}"></line>"#,
                            min_x, min_x, bbox.min_y(), bbox.max_y());
                    }

                    let bbox = RectF::from_points(Vector2F::new(min_x, min_y), Vector2F::new(max_x, max_y));
                    _render(w, spans, cell, bbox, level+1);
                }
            }
        }
        Node::Table { .. } => {
            
        }
    }
}
