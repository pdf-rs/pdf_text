use pathfinder_geometry::rect::RectF;
use serde::{Serialize, Deserialize};


pub fn is_number(s: &str) -> bool {
    s.len() > 0 && s.chars().all(|c| ('0' ..= '9').contains(&c))
}

pub fn avg(iter: impl Iterator<Item=f32>) -> Option<f32> {
    let mut count = 0;
    let mut sum = 0.;
    for i in iter {
        sum += i;
        count += 1;
    }
    if count > 0 {
        Some(sum / count as f32)
    } else {
        None
    }
}

pub enum Tri {
    False,
    True,
    Maybe(f32),
    Unknown,
}

#[derive(Copy, Clone, Debug)]
#[derive(Serialize, Deserialize)]
#[repr(C)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32
}
impl From<RectF> for Rect {
    fn from(r: RectF) -> Self {
        Rect {
            x: r.origin_x(),
            y: r.origin_y(),
            w: r.width(),
            h: r.height()
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CellContent {
    pub text: String,
    pub rect: Rect,
}