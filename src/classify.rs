use std::sync::Arc;

use font::Encoder;
use pdf_render::TextSpan;

use crate::util::is_number;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Class {
    Number,
    Header,
    Paragraph,
    Mixed,
}

pub fn classify<'a, E: Encoder + 'a>(spans: impl Iterator<Item=&'a TextSpan<E>>) -> Class {
    use pdf_render::FontEntry;

    let mut bold = TriCount::new();
    let mut numeric = TriCount::new();
    let mut uniform = TriCount::new();
    let mut first_font: *const FontEntry<E> = std::ptr::null();

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

pub enum Tri {
    False,
    True,
    Maybe(f32),
    Unknown,
}

#[derive(Debug)]
pub struct TriCount {
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