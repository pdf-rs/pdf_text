use crate::Span as TextSpan;
use pathfinder_geometry::{
    vector::Vector2F,
    rect::RectF
};
#[cfg(feature="ocr")]
use tesseract_plumbing::Text;

use std::collections::BTreeSet;
use std::iter::once;
use std::sync::Arc;
use itertools::{Itertools};
use ordered_float::NotNan;
use crate::entry::{Flow, Line, Run, RunType, Word};
use crate::util::{is_number, avg, CellContent};
use crate::text::{concat_text, Context};
use std::mem::take;
use table::Table;

pub fn build(spans: &[TextSpan], bbox: RectF, lines: &[[f32; 4]]) -> Node {
    if spans.len() == 0 {
        return Node::singleton(&[]);
    }

    let mut boxes: Vec<(RectF, usize)> = spans.iter().enumerate().map(|(i, t)| (t.rect, i)).collect();
    let mut boxes = boxes.as_mut_slice();

    // A page that was scanned a little askew: turn it straight before anything
    // is measured on it. Everything below compares `min_y` and rect centers,
    // and on a page that leans by half a degree those comparisons see the
    // drift, not the lines.
    let skew = page_skew(spans);
    // Below a twentieth of a degree there is nothing to straighten: the angle
    // is the noise of the measurement, and turning a page by noise only moves
    // every comparison in this file by a hair.
    if skew.abs() >= 0.001 {
        rotate(&mut boxes, bbox, skew);
    }

    let avg_font_size = avg(spans.iter().map(|s| s.font_size)).unwrap();
    let probaby_header = |boxes: &[(RectF, usize)]| {
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
        
        let count = split_by(boxes, &x_gaps, |r| r.min_x()).filter(|cell| probaby_header(cell)).count();
        count == x_gaps.len() + 1
    };

    sort_y(boxes);
    let (top, bottom) = top_bottom_gap(boxes, bbox);
    if let Some(bottom) = bottom {
        if bottom > 0 && bottom < boxes.len() && probably_footer(&mut boxes[bottom..]) {
            boxes = &mut boxes[..bottom];
        }
    }
    if let Some(top) = top {
        if top > 0 && top < boxes.len() && probaby_header(&mut boxes[..top]) {
            boxes = &mut boxes[top..];
        }
    }
    sort_x(boxes);
    let (left, right) = left_right_gap(boxes, bbox);
    if let Some(right) = right {
        if probaby_header(&boxes[right..]) {
            boxes = &mut boxes[..right];
        }
    }
    if let Some(left) = left {
        if probaby_header(&boxes[..left]) {
            boxes = &mut boxes[left..];
        }
    }
    let lines = analyze_lines(lines);
    // A table is a grid of cells, and `split` below reads a page cell by cell:
    // given a table it hands back its columns, one after the other, as if the
    // table were read down each column and not across each row. So tables are
    // found first, and the rest of the page is split around them.
    sort_y(boxes);
    let bands = find_tables(boxes, &spans);
    if !bands.is_empty() {
        // The widest gaps of the page's prose, which the pieces around the
        // table are too small to measure for themselves. The table is left
        // out: its columns are wider than any space in the text, and a piece
        // measured against them would read its own paragraphs as one.
        let mut prose: Vec<(RectF, usize)> = boxes.iter()
            .filter(|b| !bands.iter().any(|band| {
                let y = baseline(&b.0);
                y >= band.top && y <= band.bottom
            }))
            .cloned()
            .collect();
        sort_x(&mut prose);
        let x = dist_x(&mut prose).map(|g| g.0).unwrap_or(0.0);
        sort_y(&mut prose);
        let y = dist_y(&mut prose).map(|g| g.0).unwrap_or(0.0);
        return build_bands(boxes, &spans, &lines, bands, (x, y));
    }
    split(boxes, &spans, &lines)
}

/// The page built with its tables put back together: `split` for everything
/// between them, `Node::Table` for the tables themselves.
fn build_bands(
    boxes: &mut [(RectF, usize)],
    spans: &[TextSpan],
    lines: &Lines,
    bands: Vec<Band>,
    page: (f32, f32),
) -> Node {
    boxes.sort_unstable_by(|a, b| baseline(&a.0).partial_cmp(&baseline(&b.0)).unwrap_or(std::cmp::Ordering::Equal));

    let mut cells = vec![];
    let mut y = vec![];
    let mut i = 0;
    for band in bands {
        let start = i;
        while i < boxes.len() && baseline(&boxes[i].0) < band.top { i += 1; }
        if i > start {
            cells.push(split_within(&mut boxes[start..i], spans, lines, Some(page)));
            y.push(0.5 * (band.top + baseline(&boxes[i - 1].0)));
        }
        while i < boxes.len() && baseline(&boxes[i].0) <= band.bottom { i += 1; }
        cells.push(Node::Table { table: band.table });
    }
    if i < boxes.len() {
        cells.push(split_within(&mut boxes[i..], spans, lines, Some(page)));
    }
    if cells.len() == 1 {
        return cells.pop().unwrap();
    }
    Node::Grid { x: vec![], y, cells, tag: NodeTag::Complex }
}

// ---------------------------------------------------------------------------
// Tables

/// A run of consecutive lines that reads as a table, and the table.
struct Band {
    /// The baselines the band runs from and to.
    top: f32,
    bottom: f32,
    table: Table<Vec<Vec<usize>>>,
}

/// One visual line of a page, with its words grouped into cells.
struct TextLine {
    cells: Vec<Cell>,
    /// The line the words are set on.
    baseline: f32,
    /// The top and bottom of the line's boxes.
    y: Span,
    /// How tall the line is set: the yardstick for the spaces in it.
    em: f32,
}

/// The words of a line that stand together: one cell of a table, or a word or
/// two of prose.
struct Cell {
    span: Span,
    items: Vec<usize>,
}

/// The line a box of text is set on: the bottom of its box, one em below the
/// top. Superscripts sit a little above the line, the next line a whole line
/// below it.
///
/// The boxes of `build` are one em tall and stand on their baseline, so two
/// words share a line exactly when their boxes share a bottom — a tall glyph
/// in the cell next door does not move it.
fn baseline(rect: &RectF) -> f32 {
    rect.max_y()
}

/// The page as lines of words, top to bottom.
fn text_lines(boxes: &[(RectF, usize)], spans: &[TextSpan]) -> Vec<TextLine> {
    let mut order: Vec<usize> = (0..boxes.len())
        .filter(|&i| boxes[i].0.min_y().is_finite() && boxes[i].0.max_y().is_finite())
        .collect();
    order.sort_by(|&a, &b| baseline(&boxes[a].0)
        .partial_cmp(&baseline(&boxes[b].0))
        .unwrap_or(std::cmp::Ordering::Equal));

    let em = median(order.iter().map(|&i| boxes[i].0.height()).collect());
    // A superscript sits a third of an em off the line it belongs to, the next
    // line is a whole em or more away.
    let tol = (0.35 * em).max(0.1);

    let mut out = vec![];
    let mut run: Vec<(RectF, usize)> = vec![];
    let mut anchor = 0.0;
    for &i in &order {
        let b = baseline(&boxes[i].0);
        if run.is_empty() {
            anchor = b;
        } else if (b - anchor).abs() > tol {
            out.push(make_line(&run, spans, em));
            run.clear();
            anchor = b;
        }
        run.push(boxes[i]);
    }
    if !run.is_empty() {
        out.push(make_line(&run, spans, em));
    }
    out
}

fn make_line(boxes: &[(RectF, usize)], spans: &[TextSpan], body_em: f32) -> TextLine {
    let mut boxes = boxes.to_vec();
    sort_x(&mut boxes);

    // A line that runs into a formula is set in several sizes; its spaces are
    // still those of the body text around it.
    let em = median(boxes.iter().map(|b| b.0.height()).collect::<Vec<_>>()).min(1.3 * body_em);

    // Words are separated by a space; the cells of a table by a gap of an em
    // or more. An em is the yardstick, not the widest space on the line: on a
    // line of one-word cells every space is a column gap.
    let threshold = 0.8 * em;

    let mut cells: Vec<Cell> = vec![];
    let mut span = Span::horiz(&boxes[0].0);
    let mut items = vec![boxes[0].1];
    for w in boxes.windows(2) {
        let gap = w[1].0.min_x() - w[0].0.max_x();
        let x2 = Span::horiz(&w[1].0);
        if gap > threshold {
            if let Some(span) = span {
                cells.push(Cell { span, items: take(&mut items) });
            }
            span = x2;
        } else {
            span = match (span, x2) {
                (Some(a), Some(b)) => Some(Span { start: a.start.min(b.start), end: a.end.max(b.end) }),
                (a, _) => a,
            };
        }
        items.push(w[1].1);
    }
    if let Some(span) = span {
        cells.push(Cell { span, items });
    }

    let y = boxes.iter().filter_map(|b| Span::vert(&b.0)).reduce(|a, b| Span {
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }).unwrap();
    let baseline = median(boxes.iter().map(|b| baseline(&b.0)).collect());

    let _ = spans;
    TextLine { cells, baseline, y, em }
}

fn median(mut v: Vec<f32>) -> f32 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// The distance from one line of a block to the next, in page units.
fn line_gap(lines: &[TextLine]) -> f32 {
    let gaps: Vec<f32> = lines.windows(2)
        .map(|w| w[1].baseline - w[0].baseline)
        .filter(|g| *g > 0.0)
        .collect();
    median(gaps)
}

/// Every table on the page, in the order the lines come down it.
fn find_tables(boxes: &[(RectF, usize)], spans: &[TextSpan]) -> Vec<Band> {
    let lines = text_lines(boxes, spans);
    let mut bands = vec![];
    let mut i = 0;

    while i < lines.len() {
        // A line with one cell is prose — or a cell of a table that has no
        // neighbour on its row, or one that wrapped. A table is a run of lines
        // that fall into columns, with lines that do not in between.
        if lines[i].cells.len() < 2 {
            i += 1;
            continue;
        }
        let mut last = i;
        let mut end = i + 1;
        while end < lines.len() {
            let line = &lines[end];
            if line.cells.len() >= 2 {
                last = end;
                end += 1;
                continue;
            }
            // A single cell belongs to the table when it stands inside the
            // columns of the table so far and under the line before it. The
            // paragraph that follows a table starts at the page's margin, to
            // the left of every column.
            let (left, right) = lines[i..=last].iter()
                .flat_map(|l| l.cells.iter())
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), c| {
                    (lo.min(c.span.start.into_inner()), hi.max(c.span.end.into_inner()))
                });
            let inside = line.cells[0].span.start.into_inner() >= left - 0.5 * line.em
                && line.cells[0].span.end.into_inner() <= right + 0.5 * line.em;
            let close = line.baseline - lines[end - 1].baseline < 1.6 * line_gap(&lines[i..=last]).max(line.em);
            if !(inside && close) {
                break;
            }
            end += 1;
        }

        if let Some(table) = table_from_lines(&lines[i..=last]) {
            let em = median(lines[i..=last].iter().map(|l| l.em).collect());
            bands.push(Band {
                top: lines[i].baseline - 0.5 * em,
                bottom: lines[last].baseline + 0.5 * em,
                table,
            });
            i = last + 1;
        } else {
            i = last + 1;
        }
    }
    bands
}

/// Read a block of lines as a table, or say that it is not one.
fn table_from_lines(lines: &[TextLine]) -> Option<Table<Vec<Vec<usize>>>> {
    let em = median(lines.iter().map(|l| l.em).collect());
    if em <= 0.0 {
        return None;
    }

    // The columns: the cells, left to right, joined while they touch. A table
    // has at least two, and one with a dozen is a page laid out in columns.
    let mut cells: Vec<(Span, usize)> = lines.iter()
        .enumerate()
        .flat_map(|(i, l)| l.cells.iter().map(move |c| (c.span, i)))
        .collect();
    cells.sort_by_key(|c| c.0.start);

    let mut columns: Vec<Span> = vec![];
    for &(span, _) in &cells {
        if let Some(last) = columns.last_mut() {
            if span.start <= last.end {
                *last = Span { start: last.start.min(span.start), end: last.end.max(span.end) };
                continue;
            }
        }
        columns.push(span);
    }
    if !(2..=12).contains(&columns.len()) {
        return None;
    }

    // The cells of a column start at the same place. Text that merely happens
    // to have a wide space in it does not: its "columns" wander.
    let mut lefts: Vec<(f32, f32)> = columns.iter().map(|s| (s.start.into_inner(), s.start.into_inner())).collect();
    for &(span, _) in &cells {
        let i = columns.iter().position(|s| s.start <= span.start && span.start <= s.end)
            .unwrap_or(0);
        let x = span.start.into_inner();
        lefts[i] = (lefts[i].0.min(x), lefts[i].1.max(x));
    }
    for (i, (lo, hi)) in lefts.iter().enumerate() {
        let width = columns[i].end.into_inner() - columns[i].start.into_inner();
        if hi - lo > (2.0 * em).max(0.5 * width) {
            return None;
        }
    }
    let col = |c: &Cell| {
        let x = 0.5 * (c.span.start.into_inner() + c.span.end.into_inner());
        columns.iter().position(|s| s.start.into_inner() <= x && x <= s.end.into_inner())
            .unwrap_or_else(|| {
                columns.iter().enumerate()
                    .min_by(|a, b| {
                        let d = |s: &Span| (s.start.into_inner() - x).abs();
                        d(a.1).partial_cmp(&d(b.1)).unwrap()
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            })
    };

    // The rows: a row is started by each line that has something in the
    // leftmost column; the lines under it fill in the cells that wrapped, and
    // a line that has nothing there belongs to the row above it.
    let first_of = |line: &TextLine| line.cells.iter().map(|c| col(c)).min();
    let mut row_of_line = vec![0usize; lines.len()];
    let mut rows = 0;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && first_of(line) == Some(0) {
            rows += 1;
        }
        row_of_line[i] = rows;
    }
    let rows = rows + 1;

    let mut table: Table<Vec<Vec<usize>>> = Table::empty(rows as u32, columns.len() as u32);
    let mut per_row: Vec<Vec<u32>> = vec![vec![]; rows];
    for (i, line) in lines.iter().enumerate() {
        let row = row_of_line[i] as u32;
        for cell in &line.cells {
            let c = col(cell) as u32;
            if !per_row[row as usize].contains(&c) {
                per_row[row as usize].push(c);
            }
            // The line below a cell continues it: the same column of the same
            // row, one line further down.
            if let Some(v) = table.get_cell_value_mut(row, c) {
                v.push(cell.items.clone());
            } else {
                table.set_cell(vec![cell.items.clone()], row, c, 1, 1);
            }
        }
    }

    // A table is mostly there: every row has something in it, most rows have
    // more than one cell, and there are cells enough to be worth a table.
    // Formulae and loose lines fall into columns too, but leave the grid
    // mostly empty.
    let mut wide = 0;
    let mut filled = 0;
    for row in &per_row {
        if row.is_empty() {
            return None;
        }
        if row.len() >= 2 {
            wide += 1;
        }
        filled += row.len();
    }
    if wide * 2 < rows as usize || filled < 2 * rows as usize || rows < 3 {
        return None;
    }
    // Every column is used by most rows. Poetry and lists are not: a line is
    // either indented or it is not, so their right-hand "columns" come and go.
    for c in 0..columns.len() as u32 {
        let used = per_row.iter().filter(|row| row.contains(&c)).count();
        if 5 * used < 3 * rows as usize {
            return None;
        }
    }
    Some(table)
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

pub fn split2(boxes: &mut [(RectF, usize)], spans: &[TextSpan], lines_info: &Lines) -> Node {
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

        let mut table: Table<Vec<Vec<usize>>> = Table::empty(lines.len() as u32, columns.len() as u32);

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
                    cell.push(parts.clone());
                } else {
                    let colspan = (last_col - first_col) as u32 + 1;
                    let rowspan = 1;
                    table.set_cell(vec![parts.clone()], row, first_col as u32, rowspan, colspan);
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
    Table { table: Table<Vec<Vec<usize>>> },
}
impl Node {
    fn tag(&self) -> NodeTag {
        match *self {
            Node::Grid { tag, .. } => tag,
            Node::Table { .. } => NodeTag::Complex,
            Node::Final { .. } => NodeTag::Singleton,
        }
    }
    fn indices(&self, out: &mut Vec<usize>) {
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
                        .flat_map(|v| v.value.iter().flatten())
                        .cloned()
                );
            }
        }
    }
    fn singleton(nodes: &[(RectF, usize)]) -> Self {
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

pub fn items(mut flow: &mut Flow, spans: &[TextSpan], node: &Node, x_anchor: f32, ctx: Context) {
    match *node {
        Node::Final { ref indices } => {
            if indices.len() > 0 {
                let node_spans = indices.iter().flat_map(|&i| spans.get(i));
                let bbox = node_spans.clone().map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap();
                let class = classify(node_spans.clone());
                let mut text = String::new();
                let words = concat_text(&mut text, node_spans, ctx);
                
                let t = match class {
                    Class::Header => RunType::Header,
                    _ => RunType::Paragraph,
                };
                flow.add_line(words, t);
            }
        }
        Node::Grid { ref x, ref y, ref cells, tag } => {
            match tag {
                NodeTag::Singleton |
                NodeTag::Line => {
                    let mut indices = vec![];
                    node.indices(&mut indices);
                    let line_spans = indices.iter().flat_map(|&i| spans.get(i));
                    let bbox: RectF = line_spans.clone().map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap().into();

                    let mut text = String::new();
                    let words = concat_text(&mut text, line_spans.clone(), ctx);
                    let class = classify(line_spans.clone());

                    let t = match class {
                        Class::Header => RunType::Header,
                        _ => RunType::Paragraph,
                    };
                    flow.add_line(words, t);
                }
                NodeTag::Paragraph => {
                    assert_eq!(x.len(), 0);
                    let mut lines: Vec<(RectF, usize)> = vec![];
                    let mut indices = vec![];
                    for n in cells {
                        let start = indices.len();
                        n.indices(&mut indices);
                        if indices.len() > start {
                            let cell_spans = indices[start..].iter().flat_map(|&i| spans.get(i));
                            let bbox = cell_spans.map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap().into();
                            lines.push((bbox, indices.len()));
                        }
                    }

                    let para_spans = indices.iter().flat_map(|&i| spans.get(i));
                    let class = classify(para_spans.clone());
                    let bbox = lines.iter().map(|t| t.0).reduce(|a, b| a.union_rect(b)).unwrap();
                    let line_height = avg(para_spans.map(|s| s.rect.height())).unwrap();
                    // classify the lines by this vertical line
                    let left_margin = bbox.min_x() + 0.5 * line_height;

                    // count how many are right and left of the split.
                    let mut left = 0;
                    let mut right = 0;

                    for (line_bbox, _) in lines.iter() {
                        if line_bbox.min_x() >= left_margin {
                            right += 1;
                        } else {
                            left += 1;
                        }
                    }

                    // typically paragraphs are indented to the right and longer than 2 lines.
                    // then there will be a higher left count than right count.
                    let indent = left > right;

                    let mut para_start = 0;
                    let mut line_start = 0;
                    let mut text = String::new();
                    let mut para_bbox = RectF::default();
                    let mut flow_lines = vec![];
                    for &(line_bbox, end) in lines.iter() {
                        if line_start != 0 {
                            // if a line is indented (or outdented), it marks a new paragraph
                            if (line_bbox.min_x() >= left_margin) == indent {
                                flow.runs.push(Run {
                                    lines: take(&mut flow_lines),
                                    kind: match class {
                                        Class::Header => RunType::Header,
                                        _ => RunType::Paragraph
                                    }
                                });
                                para_start = line_start;
                            } else {
                                text.push('\n');
                            }
                        }
                        if end > line_start {
                            let words = concat_text(&mut text, indices[line_start..end].iter().flat_map(|&i| spans.get(i)), ctx);

                            if words.len() > 0 {
                                flow_lines.push(Line { words });
                            }
                        }
                        if para_start == line_start {
                            para_bbox = line_bbox;
                        } else {
                            para_bbox = para_bbox.union_rect(line_bbox);
                        }
                        line_start = end;
                    }

                    flow.runs.push(Run {
                        lines: flow_lines,
                        kind: match class {
                            Class::Header => RunType::Header,
                            _ => RunType::Paragraph
                        }
                    });
                }
                NodeTag::Complex => {
                    let x_anchors = once(x_anchor).chain(x.iter().cloned()).cycle();
                    for (node, x) in cells.iter().zip(x_anchors) {
                        items(flow, spans, node, x, ctx);
                    }
                }
            }
        }
        Node::Table { ref table } => {
            if let Some(bbox) = table.values()
                .flat_map(|v| v.value.iter().flatten().filter_map(|&i| spans.get(i).map(|s| s.rect)))
                .reduce(|a, b| a.union_rect(b)) {
                let table = table.flat_map(|lines| {
                    let used: Vec<RectF> = lines.iter().flatten()
                        .filter_map(|&i| spans.get(i).map(|s| s.rect))
                        .collect();
                    let bbox: RectF = used.into_iter().reduce(|a, b| a.union_rect(b))?;
                    // The lines of a cell are set apart: a word that starts a
                    // line never stands where the line above it ended.
                    let mut text = String::new();
                    for line in lines.iter().filter(|l| !l.is_empty()) {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        concat_text(&mut text, line.iter().filter_map(|&i| spans.get(i)), ctx);
                    }
                    Some(CellContent {
                        text,
                        rect: bbox.into(),
                    })
                });
                let _ = bbox;
                flow.add_table(table);
            }
        }
    }
}


pub fn render(w: &mut String, spans: &[TextSpan], node: &Node, bbox: RectF) {
    _render(w, spans, node, bbox, 0)
}
fn _render(w: &mut String, spans: &[TextSpan], node: &Node, bbox: RectF, level: usize) {
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

fn split(boxes: &mut [(RectF, usize)], spans: &[TextSpan], lines: &Lines) -> Node {
    split_within(boxes, spans, lines, None)
}

/// `split`, but with the widest gaps of the whole page in hand.
///
/// A page that holds a table is split above and below it, so that the table is
/// not read as prose. Those pieces are narrower than the page, and a piece has
/// smaller gaps in it than the page does; measuring the thresholds on the
/// piece alone would read a paragraph as a grid. Passing the page's widest
/// gaps in keeps the thresholds what they would have been on the whole page.
fn split_within(
    boxes: &mut [(RectF, usize)],
    spans: &[TextSpan],
    lines: &Lines,
    page: Option<(f32, f32)>,
) -> Node {
    let num_boxes = boxes.len();
    if num_boxes < 2 {
        return Node::singleton(boxes);
    }

    sort_x(boxes);
    let max_x_gap = dist_x(boxes);
    sort_y(boxes);
    let max_y_gap = dist_y(boxes);

    let x_y_ratio = 1.0;

    let mut max_gap = match (max_x_gap, max_y_gap) {
        (Some((x, _)), Some((y, _))) => x.max(y * x_y_ratio),
        (Some((x, _)), None) => x,
        (None, Some((y, _))) => y * x_y_ratio,
        (None, None) => {
            sort_x(boxes);
            return Node::singleton(boxes);
        }
    };
    if let Some((x, y)) = page {
        max_gap = max_gap.max(x).max(y * x_y_ratio);
    }
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

    let mut gaps = gap_list(boxes, |r| (r.min_y(), r.max_y()));
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

/// One line of text as it is found on a page: the two ends of the words seen
/// on it so far, and in which direction it runs once two words say so.
#[derive(Clone, Copy)]
struct Baseline {
    first: Vector2F,
    last: Vector2F,
    dir: Option<Vector2F>,
    words: usize,
    em: f32,
}

/// The angle, in radians, at which the text of a page runs.
///
/// A scanned page is almost never exactly straight, and half a degree is
/// enough to matter to everything in this file: it compares `min_y` and rect
/// centers, so a line that drifts by a fraction of an em from word to word
/// looks like a new line every few words, columns lean, and the boxes the gap
/// splitter finds do not line up.
///
/// The angle is measured by following the words in the order they come down
/// the page. Each word joins the line it sits on — the line whose direction it
/// is closest to — and the first two words of a line give that line its
/// direction, which every further word refines: the direction is always the
/// one from the line's first word to its latest, so a page of long lines says
/// far more than a page of short ones. The page's angle is the median of the
/// line angles, weighted by how far each line reaches, so that a caption
/// printed straight, or a few words along a slanted arrow, cannot outvote the
/// body text.
///
/// Only lines set in the page's dominant font size are counted (a chart's
/// labels are not the body), only pages with several full lines of them are
/// believed, and only when those lines agree; and an angle of more than twenty
/// degrees is not a skew at all but a page set the other way round, which is
/// the caller's business, not ours. Otherwise there is no answer: the page is
/// straight, and zero is the right angle for it.
fn page_skew(spans: &[TextSpan]) -> f32 {
    if spans.len() < 6 {
        return 0.0;
    }

    // Where each span starts, and how tall its em is, from its text matrix:
    // the rect of a span cannot be used for this, because its bottom edge is
    // pulled down by whatever descenders the text happens to have.
    let mut items: Vec<(Vector2F, f32)> = spans.iter()
        .filter_map(|s| {
            let t = s.transform;
            let p = t * Vector2F::zero();
            let unit = t * Vector2F::new(1.0, 0.0) - p;
            let scale = unit.length();
            let em = if s.font_size > 0.0 { s.font_size * scale } else { scale };
            if !(em > 0.0) || !em.is_finite() || !p.x().is_finite() || !p.y().is_finite() {
                return None;
            }
            Some((p, em))
        })
        .collect();
    items.sort_unstable_by(|a, b| a.0.y().partial_cmp(&b.0.y()).unwrap());

    let mut open: Vec<Baseline> = vec![];
    for &(p, em) in items.iter() {
        let tol = (0.25 * em).max(0.05);

        let mut best: Option<(usize, f32)> = None;
        for (i, b) in open.iter().enumerate() {
            let d = match b.dir {
                Some(d) => {
                    let v = p - b.first;
                    (v.x() * d.y() - v.y() * d.x()).abs()
                }
                // One word has no direction: compare the baselines.
                None => (p.y() - b.first.y()).abs(),
            };
            if d <= tol && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }

        match best {
            Some((i, _)) => {
                let b = &mut open[i];
                b.words += 1;
                b.em = b.em.max(em);
                // A superscript or a subscript sits a little above or below
                // the baseline of the words around it; letting it move the
                // ends of a line tilts the whole page.
                let body = em >= 0.8 * b.em;
                if !body {
                    continue;
                }
                match b.dir {
                    // The second word gives the line a direction. Words do not
                    // arrive left to right — a page sorted by `min_y` hands a
                    // leaning line its words from the top down, which on a line
                    // that rises to the right is right to left — so the two
                    // ends are put in order by x, and the direction is always
                    // the one text is read in.
                    None => {
                        if p.x() < b.first.x() {
                            b.last = b.first;
                            b.first = p;
                        } else {
                            b.last = p;
                        }
                        let v = b.last - b.first;
                        let len = v.length();
                        if len > 0.0 {
                            b.dir = Some(v * (1.0 / len));
                        }
                    }
                    Some(d) => {
                        // Extend whichever end the word is beyond; a word
                        // between the ends says nothing about the direction.
                        let v = p - b.first;
                        let along = v.x() * d.x() + v.y() * d.y();
                        let reach = (b.last - b.first).length();
                        let extended = if along < 0.0 {
                            b.first = p;
                            true
                        } else if along > reach {
                            b.last = p;
                            true
                        } else {
                            false
                        };
                        if extended {
                            let v = b.last - b.first;
                            let len = v.length();
                            if len > 0.0 {
                                b.dir = Some(v * (1.0 / len));
                            }
                        }
                    }
                }
            }
            None => open.push(Baseline { first: p, last: p, dir: None, words: 1, em }),
        }
    }

    // The body text is what says how the page is turned; a chart's tick
    // labels, set smaller and at whatever angle the chart happened to use,
    // only add noise. So the lines that count are the ones set in the page's
    // dominant font size.
    let mut ems: Vec<f32> = open.iter().filter(|b| b.words >= 2).map(|b| b.em).collect();
    if ems.is_empty() {
        return 0.0;
    }
    ems.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let body_em = ems[ems.len() / 2];

    let mut angles: Vec<(f32, f32)> = open.iter()
        .filter(|b| b.words >= 4 && b.em >= 0.85 * body_em)
        .filter_map(|b| {
            let d = b.dir?;
            let reach = (b.last - b.first).length();
            let a = d.y().atan2(d.x());
            if a.is_finite() && a.abs() < 0.35 { Some((a, reach)) } else { None }
        })
        .collect();

    // A handful of short lines cannot tell a page's angle: an index, a
    // caption, a chart's labels. Two or three full lines can.
    if angles.len() < 6 {
        return 0.0;
    }
    angles.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let total: f32 = angles.iter().map(|&(_, w)| w).sum();
    if !(total > 600.0) {
        return 0.0;
    }
    let mut acc = 0.0;
    let mut median = 0.0;
    for &(a, w) in angles.iter() {
        acc += w;
        if acc >= 0.5 * total {
            median = a;
            break;
        }
    }

    // Only a page whose lines agree is askew. A page of figures has labels at
    // every angle, and the middle of those angles is not a skew; it is a
    // coincidence.
    let agree: f32 = angles.iter()
        .filter(|&&(a, _)| (a - median).abs() <= 0.02)
        .map(|&(_, w)| w)
        .sum();
    if agree < 0.7 * total {
        return 0.0;
    }
    median
}

/// Turn a point by `-skew` around the centre of `bbox`, so that a line running
/// at `skew` comes out horizontal.
fn turn(p: Vector2F, bbox: RectF, skew: f32) -> (f32, f32) {
    let (s, c) = skew.sin_cos();
    let v = p - bbox.center();
    (bbox.center().x() + c * v.x() + s * v.y(),
     bbox.center().y() - s * v.x() + c * v.y())
}

/// Turn every box by `-skew`, keeping each one's bounding rectangle.
fn rotate(boxes: &mut [(RectF, usize)], bbox: RectF, skew: f32) {
    for &mut (ref mut r, _) in boxes.iter_mut() {
        let (x0, y0) = turn(Vector2F::new(r.min_x(), r.min_y()), bbox, skew);
        let (x1, y1) = turn(Vector2F::new(r.max_x(), r.min_y()), bbox, skew);
        let (x2, y2) = turn(Vector2F::new(r.min_x(), r.max_y()), bbox, skew);
        let (x3, y3) = turn(Vector2F::new(r.max_x(), r.max_y()), bbox, skew);
        *r = RectF::from_points(
            Vector2F::new(x0.min(x1).min(x2).min(x3), y0.min(y1).min(y2).min(y3)),
            Vector2F::new(x0.max(x1).max(x2).max(x3), y0.max(y1).max(y2).max(y3)),
        );
    }
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
    use itertools::Either;
    // A page with nothing on the part of it that is being measured has no
    // gaps; it must not be a panic.
    let (first, rest) = match boxes.split_first() {
        Some(pair) => pair,
        None => return Either::Left(std::iter::empty()),
    };
    let (_, mut last_max) = span(&first.0);
    Either::Right(rest.iter().enumerate().filter_map(move |(idx, &(ref r, _))| {
        let (min, max) = span(&r);
        let r = if min > last_max {
            Some((last_max, min, idx+1))
        } else {
            None
        };
        last_max = max.max(last_max);
        r
    }))
}

fn gaps<'a>(threshold: f32, boxes: &'a [(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32) + 'a) -> impl Iterator<Item=f32> + 'a {
    use itertools::Either;
    let (first, rest) = match boxes.split_first() {
        Some(pair) => pair,
        None => return Either::Left(std::iter::empty()),
    };
    let (_, mut last_max) = span(&first.0);
    Either::Right(rest.iter().filter_map(move |&(ref r, _)| {
        let (min, max) = span(&r);
        let r = if min - last_max >= threshold {
            Some(0.5 * (last_max + min))
        } else {
            None
        };
        last_max = max.max(last_max);
        r
    }))
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

use super::util::Tri;
#[derive(Copy, Clone, Debug, PartialEq)]
enum Class {
    Number,
    Header,
    Paragraph,
    Mixed,
}

#[derive(Debug)]
struct TriCount {
    tru: usize,
    fal: usize,
}
impl TriCount {
    fn new() -> Self {
        TriCount {
            tru: 0,
            fal: 0
        }
    }
    fn add(&mut self, b: bool) {
        match b {
            false => self.fal += 1,
            true => self.tru += 1,
        }
    }
    fn count(&self) -> Tri {
        match (self.fal, self.tru) {
            (0, 0) => Tri::Unknown,
            (0, _) => Tri::True,
            (_, 0) => Tri::False,
            (f, t) => Tri::Maybe(t as f32 / (t + f) as f32)
        }
    }
}
fn classify<'a>(spans: impl Iterator<Item=&'a TextSpan>) -> Class {
    use pdf_render::FontEntry;
    use crate::Encoder;

    let mut bold = TriCount::new();
    let mut numeric = TriCount::new();
    let mut uniform = TriCount::new();
    let mut first_font: *const FontEntry<Encoder> = std::ptr::null();

    for s in spans {
        numeric.add(is_number(&s.text));
        if let Some(ref font) = s.font {
            bold.add(font.name.contains("Bold"));
            let font_ptr = Arc::as_ptr(font);
            if first_font.is_null() {
                first_font = font_ptr;
            } else {
                uniform.add(font_ptr == first_font);
            }
        }
    }
    uniform.add(true);

    match (numeric.count(), bold.count(), uniform.count()) {
        (Tri::True, _, Tri::True) => Class::Number,
        (_, Tri::True, Tri::True) => Class::Header,
        (_, Tri::False, Tri::True) => Class::Paragraph,
        (_, Tri::False, _) => Class::Paragraph,
        (_, Tri::Maybe(_), _) => Class::Paragraph,
        _ => Class::Mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf::content::TextMode;
    use pathfinder_geometry::transform2d::Transform2F;
    use pathfinder_geometry::vector::vec2f;
    use pdf_render::{Fill, TextChar};
    use crate::text::Context;

    /// One word of four letters, `em` high, starting at `x, y` on the page.
    /// The text matrix only moves it there; the size is in `Tf`, as it is in
    /// most real files.
    fn word(text: &str, x: f32, y: f32, em: f32) -> TextSpan {
        let mut chars = vec![];
        let mut body = String::new();
        let mut pen = 0.0f32;
        for ch in text.chars() {
            chars.push(TextChar {
                offset: body.len(),
                pos: pen,
                width: 0.65 * em,
                idx: chars.len(),
                leading_space: 0.0,
            });
            body.push(ch);
            pen += 0.65 * em;
        }
        TextSpan {
            rect: RectF::from_points(vec2f(x, y - em), vec2f(x + pen, y)),
            width: pen,
            font_size: em,
            font: None,
            text: body,
            chars,
            color: Fill::black(),
            alpha: 1.0,
            transform: Transform2F::from_translation(vec2f(x, y)),
            mode: TextMode::Fill,
            op_nr: 0,
            bytes: vec![],
        }
    }

    /// A page of eight lines of six words, each line running at `skew`
    /// radians — 0.0 being a page that is perfectly straight.
    fn page(skew: f32) -> Vec<TextSpan> {
        let em = 10.0;
        let mut spans = vec![];
        for line in 0..8 {
            let y = 100.0 + line as f32 * 2.0 * em;
            for w in 0..6 {
                let x = 50.0 + w as f32 * 2.8 * em;
                spans.push(word("word", x, y + (x - 50.0) * skew.tan(), em));
            }
        }
        spans
    }

    /// The flow the tree builder makes of a page, as the printout sees it.
    fn flow(spans: &[TextSpan]) -> Flow {
        let bbox = spans.iter().map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap();
        let node = build(spans, bbox, &[]);
        let mut flow = Flow::new();
        items(&mut flow, spans, &node, bbox.min_x(), Context::default());
        flow
    }

    fn lines(flow: &Flow) -> Vec<usize> {
        flow.runs.iter().flat_map(|r| r.lines.iter()).map(|l| l.words.len()).collect()
    }

    #[test]
    fn a_leaning_page_is_measured_by_its_own_lines() {
        let skew = 1.0f32.to_radians();
        let measured = page_skew(&page(skew));
        assert!(
            (measured - skew).abs() < 0.002,
            "{:.4} rad measured for a page set at {:.4}",
            measured, skew
        );
    }

    #[test]
    fn a_straight_page_is_left_alone() {
        assert_eq!(page_skew(&page(0.0)), 0.0);
    }

    #[test]
    fn a_single_line_cannot_tell_the_angle_of_a_page() {
        let spans: Vec<_> = (0..6).map(|w| word("word", 50.0 + w as f32 * 28.0, 100.0, 10.0)).collect();
        assert_eq!(page_skew(&spans), 0.0);
    }

    /// The point of measuring the angle: a page that leans by a degree still
    /// comes out as eight lines of six words, not as a heap of short ones.
    #[test]
    fn a_leaning_page_still_gives_one_line_per_line_of_text() {
        assert_eq!(lines(&flow(&page(0.0))), vec![6; 8]);
        assert_eq!(lines(&flow(&page(1.0f32.to_radians()))), vec![6; 8]);
    }

    /// A table of four rows and three columns, one word per cell, the columns
    /// set far enough apart to be columns and not spaces. With `wrapped` the
    /// cell in the second row, right column, takes two lines.
    fn table_page(wrapped: bool) -> Vec<TextSpan> {
        let em = 10.0;
        let columns = [50.0f32, 130.0, 210.0];
        let mut spans = vec![];
        for row in 0..4 {
            let y = 100.0 + row as f32 * 2.0 * em;
            for (c, &x) in columns.iter().enumerate() {
                spans.push(word(&format!("r{}c{}", row, c), x, y, em));
                if wrapped && row == 1 && c == 2 {
                    spans.push(word("more", x, y + em, em));
                }
            }
        }
        spans
    }

    fn boxes_of(spans: &[TextSpan]) -> Vec<(RectF, usize)> {
        let mut boxes: Vec<(RectF, usize)> = spans.iter().enumerate().map(|(i, s)| (s.rect, i)).collect();
        sort_y(&mut boxes);
        boxes
    }

    #[test]
    fn a_grid_of_cells_is_found_as_a_table() {
        let spans = table_page(false);
        let bands = find_tables(&boxes_of(&spans), &spans);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].table.size(), (4, 3));
    }

    /// A cell that wraps onto a second line is one cell with two lines in it,
    /// not a new row with nothing in the other columns.
    #[test]
    fn a_wrapped_cell_stays_one_cell() {
        let spans = table_page(true);
        let bands = find_tables(&boxes_of(&spans), &spans);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].table.size(), (4, 3));
        let cell = bands[0].table.values().find(|c| c.row == 1 && c.col == 2).unwrap();
        assert_eq!(cell.value.len(), 2);
    }

    /// Prose is not a table, however wide its spaces.
    #[test]
    fn a_paragraph_is_not_a_table() {
        let spans = page(0.0);
        assert!(find_tables(&boxes_of(&spans), &spans).is_empty());
    }

    /// A table reaches the flow as a table, rows by rows.
    #[test]
    fn a_table_is_read_across_its_rows() {
        let marked = flow(&table_page(false));
        let table = marked.runs.iter().find_map(|r| match &r.kind {
            RunType::Table(t) => Some(t),
            _ => None,
        }).expect("no table");
        assert_eq!((table.rows.len(), table.columns), (4, 3));
        let text = |r: usize, c: usize| table.rows[r][c].as_ref().map(|c| c.text.clone()).unwrap_or_default();
        assert_eq!(text(0, 0), "r0c0");
        assert_eq!(text(0, 2), "r0c2");
        assert_eq!(text(3, 2), "r3c2");
    }
}