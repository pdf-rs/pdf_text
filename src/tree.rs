use pdf_render::TextSpan;
use pathfinder_geometry::{
    vector::Vector2F,
    rect::RectF
};

use std::collections::BTreeSet;

use itertools::Itertools;
use ordered_float::NotNan;
use crate::classify::{classify, Class};
use crate::util::avg;

#[cfg(feature="ocr")]
use tesseract_plumbing::Text;

use std::mem::take;
use table::Table;
use font::Encoder;

pub fn build<E: Encoder>(spans: &[TextSpan<E>], bbox: RectF, lines: &[[f32; 4]]) -> Node {
    if spans.len() == 0 {
        return Node::singleton(&[]);
    }
    let mut boxes: Vec<(RectF, usize)> = spans.iter().enumerate().map(|(i, t)| (t.rect, i)).collect();
    let mut boxes = boxes.as_mut_slice();
    
    let avg_font_size = avg(spans.iter().map(|s| s.font_size)).unwrap();

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
        let x_gaps: Vec<f32> = gaps(avg_font_size, boxes, |r| (r.min_x(), r.max_x()))
            .collect();
        
        let count = split_by(boxes, &x_gaps, |r| r.min_x()).filter(|cell| probably_header(cell)).count();
        count == x_gaps.len() + 1
    };

    sort_y(boxes);
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
    let lines = analyze_lines(lines);
    split(boxes, &spans, &lines)
}

fn analyze_lines(lines: &[[f32; 4]]) -> Lines {
    let mut hlines = BTreeSet::new();
    let mut vlines = BTreeSet::new();

    for &[x1, y1, x2, y2] in lines {
        if x1 == x2 {
            vlines.insert(NotNan::new(x1).unwrap());
        } else if y1 == y2 {
            hlines.insert(NotNan::new(y1).unwrap());
        }
    }

    fn dedup(lines: impl Iterator<Item=NotNan<f32>>) -> Vec<(f32, f32)> {
        let threshold = 10.0;
        let mut out = vec![];
        let mut lines = lines.map(|f| *f).peekable();
        while let Some(start) = lines.next() {
            let mut last = start;
            while let Some(&p) = lines.peek() {
                if last + threshold > p {
                    last = p;
                    lines.next();
                } else {
                    break;
                }
            }
            out.push((start, last));
        }
        out
    }

    let hlines = dedup(hlines.iter().cloned());
    let vlines = dedup(vlines.iter().cloned());

    let mut line_grid = vec![false; vlines.len() * hlines.len()];
    for &[x1, y1, x2, y2] in lines {
        if x1 == x2 {
            let v_idx = vlines.iter().position(|&(a, b)| a <= x1 && x1 <= b).unwrap_or(vlines.len());
            let h_start = hlines.iter().position(|&(a, b)| y1 >= a).unwrap_or(hlines.len());
            let h_end = hlines.iter().position(|&(a, b)| y2 <= b).unwrap_or(hlines.len());
            for h in h_start .. h_end {
                line_grid[v_idx * hlines.len() + h] = true;
            }
        } else if y1 == y2 {
            let h_idx = hlines.iter().position(|&(a, b)| a <= y1 && y1 <= b).unwrap_or(hlines.len());
            let v_start = vlines.iter().position(|&(a, b)| x1 >= a).unwrap_or(vlines.len());
            let v_end = vlines.iter().position(|&(a, b)| x2 <= b).unwrap_or(vlines.len());
            for v in v_start .. v_end {
                line_grid[v * hlines.len() + h_idx] = true;
            }
        }
    }


    //println!("hlines: {:?}", hlines);
    //println!("vlines: {:?}", vlines);

    Lines { hlines, vlines, line_grid }
}

pub struct Lines {
    hlines: Vec<(f32, f32)>,
    vlines: Vec<(f32, f32)>,
    line_grid: Vec<bool>,
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

    assert!(x_gaps.len() > 0 || y_gaps.len() > 0);
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

fn top_bottom_gap(boxes: &mut [(RectF, usize)], bbox: RectF) -> (Option<usize>, Option<usize>) {
    let num_boxes = boxes.len();
    if num_boxes < 2 {
        return (None, None);
    }

    let mut gaps = gap_list(boxes, |r| (
        // top left y
        r.min_y(), 
        // bottom right y
        r.max_y()
    ));
    let top_limit = bbox.min_y() + bbox.height() * 0.2;
    let bottom_limit = bbox.min_y() + bbox.height() * 0.8;

    match gaps.next() {
        Some((y, _, top)) if y < top_limit => {
            match gaps.last() {
                Some((y, _, bottom)) if y > bottom_limit => (Some(top), Some(bottom)),
                _ => (Some(top), None)
            }
        }
        Some((y, _, bottom)) if y > bottom_limit => (None, Some(bottom)),
        _ => (None, None)
    }
}

fn left_right_gap(boxes: &mut [(RectF, usize)], bbox: RectF) -> (Option<usize>, Option<usize>) {
    let num_boxes = boxes.len();
    if num_boxes < 2 {
        return (None, None);
    }

    let mut gaps = gap_list(boxes, |r| (r.min_x(), r.max_x()));
    let left_limit = bbox.min_x() + bbox.width() * 0.2;
    let right_limit = bbox.min_x() + bbox.width() * 0.8;
    match gaps.next() {
        Some((x, _, left)) if x < left_limit  => {
            match gaps.last() {
                Some((x, _, right)) if x > right_limit => (Some(left), Some(right)),
                _ => (Some(left), None)
            }
        }
        Some((x, _, right)) if x > right_limit => (None, Some(right)),
        _ => (None, None)
    }
}

fn sort_x(boxes: &mut [(RectF, usize)]) {
    boxes.sort_unstable_by(|a, b| a.0.min_x().partial_cmp(&b.0.min_x()).unwrap());
}
fn sort_y(boxes: &mut [(RectF, usize)]) {
    boxes.sort_unstable_by(|a, b| a.0.min_y().partial_cmp(&b.0.min_y()).unwrap());
}
fn overlapping_lines(boxes: &mut [(RectF, usize)]) -> Node {
    sort_y(boxes);
    let avg_height = avg(boxes.iter().map(|(r, _)| r.height())).unwrap();
    
    let mut y_center = boxes[0].0.center().y();
    let mut lines = vec![];
    let mut y_splits = vec![];

    let mut start = 0;
    'a: loop {
        for (i, &(r, _)) in boxes[start..].iter().enumerate() {
            if r.center().y() > 0.5 * avg_height + y_center {
                let end = start + i;
                sort_x(&mut boxes[start..end]);
                let bbox = boxes[start..end].iter().map(|&(r, _)| r).reduce(|a, b| a.union_rect(b)).unwrap();

                y_splits.push(bbox.max_y());
                lines.push(Node::singleton(&boxes[start..end]));
                y_center = r.center().y();

                start = end;
                continue 'a;
            }
        }

        sort_x(&mut boxes[start..]);
        lines.push(Node::singleton(&boxes[start..]));

        break;
    }
    match lines.len() {
        0 => Node::singleton(&[]),
        1 => lines.pop().unwrap(),
        _ => Node::Grid {
            x: vec![],
            y: y_splits,
            cells: lines,
            tag: NodeTag::Paragraph
        }
    }
}

fn gap_list<'a>(boxes: &'a [(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32) + 'a) -> impl Iterator<Item=(f32, f32, usize)> + 'a {
    let mut boxes = boxes.iter();
    let &(ref r, _) = boxes.next().unwrap();
    let (_, mut last_max) = span(r);
    boxes.enumerate().filter_map(move |(idx, &(ref r, _))| {
        // top left y, bottom right y
        let (min, max) = span(&r);
        let r = if min > last_max {
            Some((last_max, min, idx+1))
        } else {
            None
        };
        last_max = max.max(last_max);
        r
    })
}

fn gaps<'a>(threshold: f32, boxes: &'a [(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32) + 'a) -> impl Iterator<Item=f32> + 'a {
    let mut boxes = boxes.iter();
    let &(ref r, _) = boxes.next().unwrap();
    let (_, mut last_max) = span(r);
    boxes.filter_map(move |&(ref r, _)| {
        let (min, max) = span(&r);
        let r = if min - last_max >= threshold {
            Some(0.5 * (last_max + min))
        } else {
            None
        };
        last_max = max.max(last_max);
        r
    })
}

fn max_gap(boxes: &[(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32)) -> Option<(f32, f32)> {
    gap_list(boxes, span)
    .max_by_key(|&(a, b, _)| NotNan::new(b - a).unwrap())
    .map(|(a, b, _)| (b - a, 0.5 * (a + b)))
}

fn dist_x(boxes: &[(RectF, usize)]) -> Option<(f32, f32)> {
    max_gap(boxes, |r| (r.min_x(), r.max_x()))
}
fn dist_y(boxes: &[(RectF, usize)]) -> Option<(f32, f32)> {
    max_gap(boxes, |r| (r.min_y(), r.max_y()))
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