
use std::collections::BTreeSet;
use ordered_float::NotNan;
use pathfinder_geometry::rect::RectF;

use crate::util::avg;

use super::{sort_x, sort_y, Node, NodeTag};

pub fn analyze_lines(lines: &[[f32; 4]]) -> Lines {
    let mut hlines = BTreeSet::new();
    let mut vlines = BTreeSet::new();

    for &[x1, y1, x2, y2] in lines {
        if x1 == x2 {
            vlines.insert(NotNan::new(x1).unwrap());
        } else if y1 == y2 {
            hlines.insert(NotNan::new(y1).unwrap());
        }
    }

    let hlines = dedup(hlines.iter().cloned());
    let vlines = dedup(vlines.iter().cloned());

    let mut line_grid = vec![false; vlines.len() * hlines.len()];
    for &[x1, y1, x2, y2] in lines {
        // vertical line
        if x1 == x2 {
            let v_idx = vlines.iter().position(|&(a, b)| a <= x1 && x1 <= b).unwrap_or(vlines.len());
            let h_start = hlines.iter().position(|&(a, b)| y1 >= a).unwrap_or(hlines.len());
            let h_end = hlines.iter().position(|&(a, b)| y2 <= b).unwrap_or(hlines.len());
            for h in h_start .. h_end {
                line_grid[v_idx * hlines.len() + h] = true;
            }
        } 
        // horizontal line
        else if y1 == y2 {
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

/// Group lines that are consecutive within a distance of 10.0.
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

#[derive(Debug)]
pub struct Lines {
    pub hlines: Vec<(f32, f32)>,
    pub vlines: Vec<(f32, f32)>,
    pub line_grid: Vec<bool>,
}

/// Deals with things like superscript and subscript, which fall outside the usual bounds 
/// but need to be assigned to the correct line.
/// 
/// example, two lines:
/// hello world
/// m³2 test a number℡
pub fn overlapping_lines(boxes: &mut [(RectF, usize)]) -> Node {
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


#[cfg(test)]
mod tests {
    use super::*;
    use ordered_float::NotNan;

    #[test]
    fn test_dedup() {
        // Input data: A series of sorted `NotNan<f32>` values
        let input = vec![
            NotNan::new(1.0).unwrap(),
            NotNan::new(5.0).unwrap(),
            NotNan::new(8.0).unwrap(),
            NotNan::new(12.0).unwrap(),

            NotNan::new(25.0).unwrap(),
            NotNan::new(28.0).unwrap(),
        ];

        // Call the dedup function
        let result = dedup(input.into_iter());

        // Expected output:
        // (1.0, 12.0): All values between 1.0 and 12.0 are within a threshold of 10.0.
        // (25.0, 28.0): 25.0 and 28.0 are within a threshold of 10.0.
        let expected = vec![(1.0, 12.0), (25.0, 28.0)];

        // Assert that the result matches the expected output
        assert_eq!(result, expected);
    }
}