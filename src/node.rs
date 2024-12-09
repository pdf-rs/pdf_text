mod gap;
mod line;
mod render;

use gap::{dist_x, dist_y, gap_list, gaps, left_right_gap, top_bottom_gap};
use line::{analyze_lines, overlapping_lines, Lines};
use pdf_render::TextSpan;
use pathfinder_geometry::rect::RectF;

use itertools::Itertools;
use ordered_float::NotNan;
use crate::classify::{classify, Class};
use crate::util::avg;

#[cfg(feature="ocr")]
use tesseract_plumbing::Text;

use std::boxed;
use std::mem::take;
use table::Table;
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

#[derive(Copy, Clone, Debug)]
struct Span {
    start: NotNan<f32>,
    end: NotNan<f32>,
}
impl Span {
    fn horiz(rect: &RectF) -> Option<Self> {
        Self::new(rect.min_x(), rect.max_x())
    }
    fn vert(rect: &RectF) -> Option<Self> {
        Self::new(rect.min_y(), rect.max_y())
    }
    fn new(mut start: f32, mut end: f32) -> Option<Self> {
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        Some(Span {
            start: NotNan::new(start).ok()?,
            end: NotNan::new(end).ok()?,
        })
    }
    fn intersect(self, other: Span) -> Option<Span> {
        if self.start <= other.end && other.start <= self.end {
            Some(Span {
                start: self.start.max(other.start),
                end: self.end.min(other.end),
            })
        } else {
            None
        }
    }
    fn union(self, other: Span) -> Option<Span> {
        if self.start <= other.end && other.start <= self.end {
            Some(Span {
                start: self.start.min(other.start),
                end: self.end.max(other.end)
            })
        } else {
            None
        }
    }
}

pub fn split2<E: Encoder>(boxes: &mut [(RectF, usize)], spans: &[TextSpan<E>], lines_info: &Lines) -> Node {
    use std::mem::replace;

    #[derive(Debug)]
    enum LineTag {
        Unknown,
        Text,
        Table,
    }

    sort_y(boxes);
    let mut lines = vec![];
    let mut y = Span::vert(&boxes[0].0).unwrap();
    let mut items = vec![boxes[0]];

    let build_line = |boxes: &[(RectF, usize)]| -> (LineTag, Span, Vec<(Span, Vec<usize>)>) {
        let mut line = vec![];
        let mut x = Span::horiz(&boxes[0].0).unwrap();
        let mut y = Span::vert(&boxes[0].0).unwrap();
        let mut items = vec![boxes[0].1];

        for &(rect, i) in &boxes[1..] {
            y = y.union(Span::vert(&rect).unwrap()).unwrap();
            let x2 = Span::horiz(&rect).unwrap();
            if let Some(u) = x.union(x2) {
                x = u;
                items.push(i);
            } else {
                line.push((x, replace(&mut items, vec![i])));
                x = x2;
            }
        }
        line.push((x, items));

        let f = avg(boxes.iter().filter_map(|&(_, i)| spans.get(i)).map(|s| s.font_size)).unwrap();

        let max_gap = line.iter().tuple_windows().map(|(l, r)| r.0.start - l.0.end).max();
        let tag = match max_gap {
            None => LineTag::Unknown,
            Some(x) if x.into_inner() < 0.3 * f => LineTag::Text,
            Some(_) => LineTag::Table,
        };

        (tag, y, line)
    };

    let mut line = vec![boxes[0]];
    for &(rect, i) in &boxes[1..] {
        let y2 = Span::vert(&rect).unwrap();
        if let Some(overlap) = y.intersect(y2) {
            y = overlap;
        } else {
            sort_x(&mut line);
            lines.push(build_line(&line));
            line.clear();
            y = y2
        }
        line.push((rect, i));
    }
    sort_x(&mut line);
    lines.push(build_line(&line));


    let mut vparts = vec![];
    let mut start = 0;
    while let Some(p) = lines[start..].iter().position(|(tag, _, line)| matches!(tag, LineTag::Unknown | LineTag::Table)) {
        let table_start = start + p;
        let table_end = lines[table_start+1..].iter().position(|(tag, _, _)| matches!(tag, LineTag::Text)).map(|e| table_start+1+e).unwrap_or(lines.len());
        
        for &(_, y, ref line) in &lines[start..table_start] {
            vparts.push((y, Node::Final { indices: line.iter().flat_map(|(_, indices)| indices.iter().cloned()).collect() }));
        }

        let lines = &lines[table_start..table_end];
        start = table_end;

        let mut columns: Vec<Span> = vec![];
        for (_, _, line) in lines.iter() {
            for &(x, ref parts) in line.iter() {
                // find any column that is contained in this
                let mut found = 0;
                for span in columns.iter_mut() {
                    if let Some(overlap) = span.intersect(x) {
                        *span = overlap;
                        found += 1;
                    }
                }
                if found == 0 {
                    columns.push(x);
                }
            }
        }
        let avg_vgap = avg(lines.iter().map(|(_, y, _)| y).tuple_windows().map(|(a, b)| *(b.start - a.end)));

        columns.sort_by_key(|s| s.start);

        let mut buf = String::new();

        let d_threshold = avg_vgap.unwrap_or(0.0);
        let mut prev_end = None;

        let mut table: Table<Vec<usize>> = Table::empty(lines.len() as u32, columns.len() as u32);

        let mut row = 0;
        for (_, span, line) in lines {
            let mut col = 0;
            
            let combine = prev_end.map(|y: NotNan<f32>| {
                if *(span.start - y) < d_threshold {
                    !lines_info.hlines.iter().map(|(a, b)| 0.5 * (a+b)).any(|l| *y < l && *span.start > l)
                } else {
                    false
                }
            }).unwrap_or(false);

            if !combine {
                row += 1;
            }

            for &(x, ref parts) in line {
                let mut cols = columns.iter().enumerate()
                    .filter(|&(_, &x2)| x.intersect(x2).is_some())
                    .map(|(i, _)| i);

                let first_col = cols.next().unwrap();
                let last_col = cols.last().unwrap_or(first_col);

                if let Some(cell) = combine.then(|| table.get_cell_value_mut(row, first_col as u32)).flatten() {
                    // append to previous line
                    cell.extend_from_slice(parts);
                } else {
                    let colspan = (last_col - first_col) as u32 + 1;
                    let rowspan = 1;
                    table.set_cell(parts.clone(), row, first_col as u32, rowspan, colspan);
                }
                col = last_col + 1;
            }
            prev_end = Some(span.end);
        }
        let y = Span { start: lines[0].1.start, end: lines.last().unwrap().1.end };
        vparts.push((y, Node::Table { table }));
    }
    for &(_, y, ref line) in &lines[start..] {
        vparts.push((y, Node::Final { indices: line.iter().flat_map(|(_, indices)| indices.iter().cloned()).collect() }));
    }

    if vparts.len() > 1 {
        let y = vparts.iter().tuple_windows().map(|(a, b)| 0.5 * (a.0.end + b.0.start).into_inner()).collect();
        Node::Grid {
            tag: NodeTag::Complex,
            x: vec![],
            y,
            cells: vparts.into_iter().map(|(_, n)| n).collect()
        }
    } else {
        vparts.pop().unwrap().1
    }
}

#[derive(Debug)]
pub enum Node {
    Final { indices: Vec<usize> },
    Grid { x: Vec<f32>, y: Vec<f32>, cells: Vec<Node>, tag: NodeTag },
    Table { table: Table<Vec<usize>> },
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

    if x_gaps.len() > 1 && y_gaps.len() > 1 {
        return split2(boxes, spans, lines);
    }

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

#[allow(dead_code)]
fn split_v(boxes: &mut [(RectF, usize)]) -> Node {
    let num_boxes = boxes.len();
    if num_boxes < 2 {
        return Node::singleton(boxes)
    }

    let max_y_gap = dist_y(boxes);

    let max_gap = match max_y_gap {
        Some((y, _)) => y,
        None => {
            sort_x(boxes);
            return Node::singleton(boxes);
        }
    };
    let threshold = max_gap * 0.8;
    let mut cells = vec![];

    let y_gaps: Vec<f32> = gaps(threshold, boxes, |r| (r.min_y(), r.max_y()))
        .collect();
    
    for row in split_by(boxes, &y_gaps, |r| r.min_y()) {
        assert!(row.len() < num_boxes);
        cells.push(split_v(row));
    }

    let tag = if cells.iter().all(|n| n.tag() <= NodeTag::Line) {
        NodeTag::Paragraph
    } else {
        NodeTag::Complex
    };

    Node::Grid {
        x: vec![],
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