use font::Encoder;
use pathfinder_geometry::vector::Vector2F;
use pdf_render::TextSpan;
use itertools::Itertools;
use unicode_normalization::UnicodeNormalization;
use crate::{util::avg, flow::{Word, Rect}};

pub fn concat_text<'a, E: Encoder + 'a>(out: &mut String, items: impl Iterator<Item=&'a TextSpan<E>> + Clone) -> Vec<Word> {
    let word_gap = analyze_word_gap(items.clone());
    let mut words: Vec<Word> = vec![];

    let mut end = 0.; // trailing edge of the last char

    // Whether the last processed TextChar is a space
    let mut trailing_space = out.chars().last().map(|c| c.is_whitespace()).unwrap_or(true);

    let mut word_start_pos = 0.0;
    let mut word_end_pos = 0.0;

    let mut word_start_idx = out.len();
    let mut y_min = f32::INFINITY;
    let mut y_max = -f32::INFINITY;
    let mut word_start = true;

    for span in items {
        let mut offset = 0; // byte index of last char into span.text
        let tr_inv = span.transform.matrix.inverse();
        let x_off = (tr_inv * span.transform.vector).x();
        
        let chars = span.chars.as_slice();
        for (i, c) in chars.iter().enumerate() {
            let next_offset = chars.get(i + 1).map_or(span.text.len(), |next| next.offset);
            let s: &str = &span.text[offset..next_offset];

            out.extend(s.nfkc());

            let is_whitespace = s.chars().all(|c| c.is_whitespace());
            if trailing_space {
                if !is_whitespace {
                    word_start = true;
                    word_start_idx = out.len() - s.len();
                }
                trailing_space = is_whitespace;
            } else {
                trailing_space = is_whitespace;
                if is_whitespace {
                    words.push(Word {
                        text: out[word_start_idx..out.len()-s.len()].into(),
                        rect: Rect {
                            x: word_start_pos,
                            y: y_min,
                            h: y_max - y_min,
                            w: word_end_pos - word_start_pos
                        }
                    });
                } else if c.pos + x_off > end + word_gap {
                    words.push(Word {
                        text: out[word_start_idx..].into(),
                        rect: Rect {
                            x: word_start_pos,
                            y: y_min,
                            h: y_max - y_min,
                            w: word_end_pos - word_start_pos
                        }
                    });
                    
                    out.push(' ');
                    trailing_space = true;
                    word_start = true;
                    word_start_idx = out.len() - 1;
                }
            }

            end = c.pos + x_off + c.width;
            word_end_pos = (span.transform.matrix * Vector2F::new(end, 0.0)).x();

            if word_start {
                y_min = span.rect.min_y();
                y_max = span.rect.max_y();
                word_start_pos = (span.transform.matrix * Vector2F::new(c.pos + x_off, 0.0)).x();
                word_start = false;
            } else {
                y_min = y_min.min(span.rect.min_y());
                y_max = y_max.max(span.rect.max_y());
            }

            offset = next_offset;
        }
    }
    
    words.push(Word {
        text: out[word_start_idx..].into(),
        rect: Rect {
            x: word_start_pos,
            y: y_min,
            h: y_max - y_min,
            w: word_end_pos - word_start_pos
        }
    });
  
    words
}

/// Calculate gaps between each char,
/// The most important thing here is to make sure the gap is bigger than char gap, and less than word gap.
/// 
/// for example: 
/// think of something like "ab ____________c de"
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