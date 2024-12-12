use font::Encoder;
use pathfinder_geometry::rect::RectF;
use pdf_render::TextSpan;
use itertools::Itertools;
use ordered_float::NotNan;
use crate::{node::{sort_x, sort_y, NodeTag}, util::avg};

use super::{gap::{dist_y, gaps}, line::Lines, split_by, Node};

pub use table::Table;

pub fn split<E: Encoder>(boxes: &mut [(RectF, usize)], spans: &[TextSpan<E>], lines_info: &Lines) -> Node {
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