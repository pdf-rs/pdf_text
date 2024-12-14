use std::mem::take;

use font::Encoder;
use pathfinder_geometry::vector::Vector2F;
use pdf_render::TextSpan;
use itertools::Itertools;
use unicode_normalization::UnicodeNormalization;
use crate::{flow::{Char, Rect, Word}, util::avg};

pub fn concat_text<'a, E: Encoder + 'a>(out: &mut String, items: impl Iterator<Item=&'a TextSpan<E>> + Clone) -> Vec<Word> {
    let word_gap = analyze_word_gap(items.clone());
    let mut words = Vec::new();
    let mut current_word = WordBuilder::new(out.len());
    
    // Whether the last processed TextChar is a whitespace
    // ' '        Space
    // '\t'       Tab
    // '\n'       Line feed
    // '\r'       Carriage return
    // '\u{00A0}' Non-breaking space
    let mut trailing_space = out.chars().last().map_or(true, |c| c.is_whitespace());

    for span in items {
        let mut offset = 0;
        let tr_inv = span.transform.matrix.inverse();
        let x_off = (tr_inv * span.transform.vector).x();
        
        let mut chars = span.chars.iter().peekable();
        while let Some(current) = chars.next() {
            // Get text for current char
            let text = if let Some(next) = chars.peek() {
                let s = &span.text[offset..next.offset];
                offset = next.offset;
                s
            } else {
                &span.text[offset..]
            };

            // Calculate char positions
            let char_start = (span.transform.matrix * Vector2F::new(current.pos + x_off, 0.0)).x();
            let char_end = (span.transform.matrix * Vector2F::new(current.pos + x_off + current.width, 0.0)).x();
            
            let is_whitespace = text.chars().all(|c| c.is_whitespace());

            // Handle word boundaries
            if trailing_space && !is_whitespace {
                // Start new word after space
                current_word.start_new(out.len(), char_start);
                current_word.add_char(0, char_start, char_end);
                out.extend(text.nfkc());
            } else if !trailing_space {
                if is_whitespace {
                    // End word at space
                    words.push(current_word.build(out, char_end));
                    current_word = WordBuilder::new(out.len());
                    out.push(' ');
                } else if current.pos + x_off > current_word.end_pos + word_gap {
                    // End word at large gap
                    words.push(current_word.build(out, char_end));

                    current_word = WordBuilder::new(out.len());
                    current_word.start_new(out.len(), char_start);
                    current_word.add_char(0, char_start, char_end);
                    out.extend(text.nfkc());
                } else {
                    // Continue current word
                    current_word.add_char(current_word.char_count, char_start, char_end);
                    out.extend(text.nfkc());
                }
            }

            trailing_space = is_whitespace;
            current_word.update_bounds(span.rect.min_y(), span.rect.max_y());
        }
    }

    // Add final word if any
    if !current_word.is_empty() {
        let end_pos = current_word.end_pos;
        words.push(current_word.build(out, end_pos));
    }

    words
}

// Helper struct to build up words
struct WordBuilder {
    word_start_idx: usize,

    // For calculating the layout(position, width , height) of a word
    start_pos: f32,
    end_pos: f32, // trailing edge of the last char
    y_min: f32,
    y_max: f32,

    chars: Vec<Char>,
    char_count: usize,
    started: bool,
}

impl WordBuilder {
    fn new(word_start_idx: usize) -> Self {
        Self {
            word_start_idx,
            start_pos: 0.0,
            end_pos: 0.0,
            y_min: f32::INFINITY,
            y_max: -f32::INFINITY,
            chars: Vec::new(),
            char_count: 0,
            started: false,
        }
    }

    fn start_new(&mut self, word_start_idx: usize, start_pos: f32) {
        self.word_start_idx = word_start_idx;
        self.start_pos = start_pos;
        self.started = true;
    }

    fn add_char(&mut self, offset: usize, start: f32, end: f32) {
        self.chars.push(Char {
            offset,
            pos: start,
            width: end - start,
        });
        self.end_pos = end;
        self.char_count += 1;
    }

    fn update_bounds(&mut self, min_y: f32, max_y: f32) {
        if !self.started {
            self.y_min = min_y;
            self.y_max = max_y;
            self.started = true;
        } else {
            self.y_min = self.y_min.min(min_y);
            self.y_max = self.y_max.max(max_y);
        }
    }

    fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn build(mut self, out: &str, end_pos: f32) -> Word {
        Word {
            text: out[self.word_start_idx..].into(),
            rect: Rect {
                x: self.start_pos,
                y: self.y_min,
                h: self.y_max - self.y_min,
                w: end_pos - self.start_pos
            },
            chars: take(&mut self.chars)
        }
    }
}

/// Calculate gaps between each char,
/// The most important thing here is to make sure the gap is bigger than char gap, and less than word gap.
/// 
/// for example: 
/// think of something like "ab____________c de"
/// 
/// a-b has a zero space (or 0.01)
/// b-c has a huge space of 10
/// c-d has 0.2
/// d-e has 0.01
/// if we just take the average = 10.2 and divide that by 4 we get 2.5
/// and now c-d is smaller than that and not classified as a space
/// but if b-c is capped by the threshold of 0.5, the sum is 0.7, and the avg is 0.7/4 ~ 0.18
/// and everything is fine.

/// 0 + min(0.5, 10) + 0.2 + 0
/// 10 capped at 0.5 is0.5
/// min(0, 0.5) + min(10, 0.5) + min(0.2, 0.5) + min(0, 0.5)
/// 0 + 0.5 + 0.2 + 0
/// every value is limited to be at least 0.01 and not more than 0.5.
/// the 0.5 is 0.25 * font size of the left char and 0.25 * font size of the right char
/// if they are the same font size it is 0.5
fn analyze_word_gap<'a, E: Encoder + 'a>(items: impl Iterator<Item=&'a TextSpan<E>> + Clone) -> f32 {
    let gaps = items.clone()
        .flat_map(|s| {
            // the transform matrix is from em space to device space
            // so we need to invert it
            let tr_inv = s.transform.matrix.inverse();
            let pos = (tr_inv * s.transform.vector).x();

            s.chars.iter()
                .filter(|c| !s.text[c.offset..].chars().next().unwrap().is_whitespace())
                .map(move |c| (c.pos + pos, c.pos + pos + c.width, s.font_size))
        })
        .tuple_windows()
        .filter(|(a, b)| b.0 > a.0)
        .map(|(a, b)| (b.0 - a.1).max(0.01).min(0.25 * (a.2 + b.2)));

    let avg_font_size = avg(items.clone().map(|s| s.font_size)).unwrap();
    //gaps.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

    (0.5 * avg_font_size).min(2.0 * avg(gaps).unwrap_or(0.0)) //2.0 * gaps[gaps.len()/2];
}

#[cfg(test)]
mod tests {
    use pathfinder_geometry::{rect::RectF, transform2d::Transform2F};
    use pdf_render::{font::OutlineBuilder, Fill, TextChar};

    use super::*;

    #[test]
    fn test_concat_text() {
        let text_span: TextSpan<OutlineBuilder> = TextSpan {
            rect: RectF::from_points(Vector2F::new(56.8, 55.85077), Vector2F::new(136.26399, 67.85077)),
            width: 79.464,
            bbox: None,
            font_size: 12.0,
            font: None,
            text: "hello world".to_string(),
            chars: vec![
                TextChar { offset: 0, pos: 0.0, width: 7.224001 },
                TextChar { offset: 1, pos: 7.224001, width: 7.224001 },
                TextChar { offset: 2, pos: 14.448002, width: 7.224001 },
                TextChar { offset: 3, pos: 21.672003, width: 7.224001 },
                TextChar { offset: 4, pos: 28.896004, width: 7.224001 },
                TextChar { offset: 5, pos: 36.120003, width: 7.224001 },
                TextChar { offset: 6, pos: 43.344, width: 7.224001 },
                TextChar { offset: 7, pos: 50.568, width: 7.224001 },
                TextChar { offset: 8, pos: 57.792, width: 7.224001 },
                TextChar { offset: 9, pos: 65.016, width: 7.224001 },
                TextChar { offset: 10, pos: 72.24, width: 7.224001 },
            ],
            color: Fill::Solid(0.0, 0.5019608, 0.0),
            alpha: 1.0,
            transform: Transform2F::row_major(1.0, 0.0, 56.8, 0.0, 1.0, 67.85077),
            mode: pdf::content::TextMode::Fill,
            op_nr: 18,
        };

        let mut output = String::new();
        let words = concat_text(&mut output, vec![&text_span].into_iter());

        // Assert the concatenated text
        assert_eq!(output, "hello world");

        // Assert the words
        assert_eq!(words.len(), 2); // Expect two words: "hello" and "world"
    }
}