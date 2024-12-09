use ordered_float::NotNan;
use pathfinder_geometry::rect::RectF;

pub fn gap_list<'a>(boxes: &'a [(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32) + 'a) -> impl Iterator<Item=(f32, f32, usize)> + 'a {
    let mut boxes = boxes.iter();
    let &(ref r, _) = boxes.next().unwrap();
    let (_, mut last_max) = span(r);

    boxes.enumerate().filter_map(move |(idx, &(ref r, _))| {
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

pub fn gaps<'a>(threshold: f32, boxes: &'a [(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32) + 'a) -> impl Iterator<Item=f32> + 'a {
    let mut boxes = boxes.iter();
    let &(ref r, _) = boxes.next().unwrap();
    let (_, mut last_max) = span(r);
    boxes.filter_map(move |&(ref r, _)| {
        let (min, max) = span(&r);
        let r = if min - last_max >= threshold {
            // The middle position of the gap
            Some(0.5 * (last_max + min))
        } else {
            None
        };
        last_max = max.max(last_max);
        r
    })
}

/// Return the size of the gap and the middle position of the gap.
pub fn max_gap(boxes: &[(RectF, usize)], span: impl Fn(&RectF) -> (f32, f32)) -> Option<(f32, f32)> {
    gap_list(boxes, span)
    .max_by_key(|&(a, b, _)| NotNan::new(b - a).unwrap())
    .map(|(a, b, _)| (b - a, 0.5 * (a + b)))
}

pub fn dist_x(boxes: &[(RectF, usize)]) -> Option<(f32, f32)> {
    max_gap(boxes, |r| (r.min_x(), r.max_x()))
}
pub fn dist_y(boxes: &[(RectF, usize)]) -> Option<(f32, f32)> {
    max_gap(boxes, |r| (r.min_y(), r.max_y()))
}

pub fn top_bottom_gap(boxes: &mut [(RectF, usize)], bbox: RectF) -> (Option<usize>, Option<usize>) {
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

pub fn left_right_gap(boxes: &mut [(RectF, usize)], bbox: RectF) -> (Option<usize>, Option<usize>) {
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
