mod gap;
mod line;
mod render;
mod table;

use gap::{dist_x, dist_y, gaps, left_right_gap, top_bottom_gap};
use line::{analyze_lines, overlapping_lines, Lines};
use pdf_render::TextSpan;
use pathfinder_geometry::rect::RectF;


use crate::classify::{classify, Class};
use crate::util::avg;

#[cfg(feature="ocr")]
use tesseract_plumbing::Text;

use std::mem::take;
use font::Encoder;

pub fn build<E: Encoder>(spans: &[TextSpan<E>], bbox: RectF, lines: &[[f32; 4]], without_header_and_footer: bool) -> Node {
    if spans.len() == 0 {
        return Node::singleton(&[]);
    }

    let mut boxes: Vec<(RectF, usize)> = spans.iter().enumerate().map(|(i, t)| (t.rect, i)).collect();
    let mut boxes = boxes.as_mut_slice();
    if without_header_and_footer {
        boxes = exclude_header_and_footer(boxes, bbox, spans);
    }

    let lines = analyze_lines(lines);
    
    split(&mut boxes, &spans, &lines)
}

pub fn exclude_header_and_footer<'a, E: Encoder>(boxes: &'a mut [(RectF, usize)], bbox: RectF, spans: &[TextSpan<E>]) -> &'a mut [(RectF, usize)]
{
    let avg_font_size: f32 = avg(spans.iter().map(|s| s.font_size)).unwrap();

    let probably_header = |boxes: &[(RectF, usize)]| {
        let class = classify(boxes.iter().filter_map(|&(_, i)| spans.get(i)));
        if matches!(class, Class::Header | Class::Number) {
            return true;
        }
        let f = avg(boxes.iter().filter_map(|&(_, i)| spans.get(i)).map(|s| s.font_size)).unwrap();
        f > avg_font_size
    };
    let probably_footer = |boxes: &mut [(RectF, usize)]| {
        sort_x(boxes);
        let x_gaps: Vec<f32> = gap::gaps(avg_font_size, boxes, |r| (r.min_x(), r.max_x()))
            .collect();
        
        let is_footer = split_by(boxes, x_gaps.as_slice(), |r| r.min_x())
            .all(|cell| probably_header(cell));

        is_footer
    };

    sort_y(boxes);

    let mut boxes = boxes;
    let (top, bottom) = top_bottom_gap(boxes, bbox);
    if let Some(bottom) = bottom {
        if probably_footer(&mut boxes[bottom..]) {
            boxes = &mut boxes[..bottom];
        }
    }
    if let Some(top) = top {
        if probably_header(&mut boxes[..top]) {
            boxes = &mut boxes[top..];
        }
    }
    sort_x(boxes);
    let (left, right) = left_right_gap(boxes, bbox);
    if let Some(right) = right {
        if probably_header(&boxes[right..]) {
            boxes = &mut boxes[..right];
        }
    }
    if let Some(left) = left {
        if probably_header(&boxes[..left]) {
            boxes = &mut boxes[left..];
        }
    }

    boxes
}


#[derive(Debug)]
pub enum Node {
    Final { indices: Vec<usize> },
    Grid { x: Vec<f32>, y: Vec<f32>, cells: Vec<Node>, tag: NodeTag },
    Table { table: table::Table<Vec<usize>> },
}
impl Node {
    pub fn tag(&self) -> NodeTag {
        match *self {
            Node::Grid { tag, .. } => tag,
            Node::Table { .. } => NodeTag::Complex,
            Node::Final { .. } => NodeTag::Singleton,
        }
    }
    pub fn indices(&self, out: &mut Vec<usize>) {
        match *self {
            Node::Final { ref indices } => out.extend_from_slice(&indices),
            Node::Grid { ref cells, .. } => {
                for n in cells {
                    n.indices(out);
                }
            }
            Node::Table { ref table } => {
                out.extend(
                    table.values()
                        .flat_map(|v| v.value.iter())
                        .cloned()
                );
            }
        }
    }
    pub fn singleton(nodes: &[(RectF, usize)]) -> Self {
        Node::Final { indices: nodes.iter().map(|t| t.1).collect() }
    }
}

#[derive(PartialOrd, Ord, Eq, PartialEq, Clone, Copy, Debug)]
pub enum NodeTag {
    Singleton,
    Line,
    Paragraph,
    Complex,
}

fn split<E: Encoder>(boxes: &mut [(RectF, usize)], spans: &[TextSpan<E>], lines: &Lines) -> Node {
    let num_boxes = boxes.len();
    if num_boxes < 2 {
        return Node::singleton(boxes);
    }

    sort_x(boxes);
    let max_x_gap = dist_x(boxes);

    sort_y(boxes);
    let max_y_gap = dist_y(boxes);

    let x_y_ratio = 1.0;

    let max_gap = match (max_x_gap, max_y_gap) {
        (Some((x, _)), Some((y, _))) => x.max(y * x_y_ratio),
        (Some((x, _)), None) => x,
        (None, Some((y, _))) => y * x_y_ratio,
        (None, None) => {
            sort_x(boxes);
            return Node::singleton(boxes);
        }
    };
    let x_threshold = (max_gap * 0.5).max(1.0);
    let y_threshold = (max_gap * 0.5 / x_y_ratio).max(0.1);
    let mut cells = vec![];

    let y_gaps: Vec<f32> = gaps(y_threshold, boxes, |r| (r.min_y(), r.max_y()))
        .collect();
    
    sort_x(boxes);
    let x_gaps: Vec<f32> = gaps(x_threshold, boxes, |r| (r.min_x(), r.max_x()))
        .collect();

    if x_gaps.len() == 0 && y_gaps.len() == 0 {
        return overlapping_lines(boxes);
    }

    //TODO: Disable the table::split for now,becuase it is not accurate 
    // if x_gaps.len() > 1 && y_gaps.len() > 1 {
    //     return table::split(boxes, spans, lines);
    // }

    assert!(
        x_gaps.len() > 0 || y_gaps.len() > 0, 
        "At least one of x_gaps and y_gaps must be non-empty, otherwise the memory will be exhausted"
    );
    sort_y(boxes);
    for row in split_by(boxes, &y_gaps, |r| r.min_y()) {
        if x_gaps.len() > 0 {
            sort_x(row);
            for cell in split_by(row, &x_gaps, |r| r.min_x()) {
                sort_y(cell);
                assert!(cell.len() < num_boxes);
                cells.push(split(cell, spans, lines));
            }
        } else {
            cells.push(split(row, spans, lines));
        }
    }

    let tag = if y_gaps.len() == 0 {
        if cells.iter().all(|n| n.tag() <= NodeTag::Line) {
            NodeTag::Line
        } else {
            NodeTag::Complex
        }
    } else if x_gaps.len() == 0 {
        if cells.iter().all(|n| n.tag() <= NodeTag::Line) {
            NodeTag::Paragraph
        } else {
            NodeTag::Complex
        }
    } else {
        NodeTag::Complex
    };

    Node::Grid {
        x: x_gaps,
        y: y_gaps,
        cells,
        tag,
    }
}

fn sort_x(boxes: &mut [(RectF, usize)]) {
    boxes.sort_unstable_by(|a, b| a.0.min_x().partial_cmp(&b.0.min_x()).unwrap());
}
fn sort_y(boxes: &mut [(RectF, usize)]) {
    boxes.sort_unstable_by(|a, b| a.0.min_y().partial_cmp(&b.0.min_y()).unwrap());
}

fn split_by<'a>(list: &'a mut [(RectF, usize)], at: &'a [f32], by: impl Fn(&RectF) -> f32) -> impl Iterator<Item=&'a mut [(RectF, usize)]> {
    SplitBy {
        data: list,
        points: at.iter().cloned(),
        by,
        end: false
    }
}

struct SplitBy<'a, I, F> {
    data: &'a mut [(RectF, usize)],
    points: I,
    by: F,
    end: bool,
}
impl<'a, I, F> Iterator for SplitBy<'a, I, F> where
    I: Iterator<Item=f32>,
    F: Fn(&RectF) -> f32
{
    type Item = &'a mut [(RectF, usize)];
    fn next(&mut self) -> Option<Self::Item> {
        if self.end {
            return None;
        }
        match self.points.next() {
            Some(p) => {
                let idx = self.data.iter().position(|(ref r, _)| (self.by)(r) > p).unwrap_or(self.data.len());
                let (head, tail) = take(&mut self.data).split_at_mut(idx);
                self.data = tail;
                Some(head)
            },
            None => {
                self.end = true;
                Some(take(&mut self.data))
            }
        }
    }
}
