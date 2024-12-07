use pathfinder_geometry::vector::Vector2F;
use pdf_render::TextSpan;
use itertools::{Itertools};
use unicode_normalization::UnicodeNormalization;
use crate::{util::avg, entry::Word, util::Rect};

pub fn concat_text<'a>(out: &mut String, items: impl Iterator<Item=&'a TextSpan> + Clone) -> Vec<Word> {
    let mut words = vec![];

    let gaps = items.clone()
        .flat_map(|s| {
            let tr_inv = s.transform.matrix.inverse();
            let pos = (tr_inv * s.transform.vector).x();
            s.chars.iter()
                .filter(|c| !s.text[c.offset..].chars().next().unwrap().is_whitespace())
                // (left edge, right edge, font size)
                .map(move |c| (c.pos + pos, c.pos + pos + c.width, s.font_size))
        })
        .tuple_windows()
        // skip things that go in reverse
        .filter(|(a, b)| b.0 > a.0)
        // compute the distance between the right edge of the left char and the left edge of the right char
        // and clamp it to a minimum of 0.01 and maximum of half the mean font size
        .map(|(a, b)| (b.0 - a.1).max(0.01).min(0.25 * (a.2 + b.2)));

    // compute the average font size of all chars
    let font_size = avg(items.clone().map(|s| s.font_size)).unwrap();
    //gaps.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

    // set the threshold at twice the average gap, clamped to half the font size
    let space_gap = (0.5 * font_size).min(2.0 * avg(gaps).unwrap_or(0.0)); //2.0 * gaps[gaps.len()/2];
    let mut end = 0.; // trailing edge of the last char
    let mut trailing_space = out.chars().last().map(|c| c.is_whitespace()).unwrap_or(true);
    let mut word_start_pos = 0.0;
    let mut word_start_idx = out.len();
    let mut y_min = f32::INFINITY;
    let mut y_max = -f32::INFINITY;
    let mut word_start = true;
    let mut word_end = 0.0;

    for span in items {
        let mut pos = 0; // byte index of last char into span.text
        let tr_inv = span.transform.matrix.inverse();
        let x_off = (tr_inv * span.transform.vector).x();
        for c in span.chars.iter() {

            let s = &span.text[pos..c.offset];
            if c.offset > 0 {
                let is_whitespace = s.chars().all(|c| c.is_whitespace());
                if !trailing_space || !is_whitespace {
                    out.extend(s.nfkc());
                }
                trailing_space = is_whitespace;
            }
            if !trailing_space && c.pos + x_off > end + space_gap {
                words.push(Word {
                    text: out[word_start_idx..].into(),
                    rect: Rect {
                        x: word_start_pos,
                        y: y_min,
                        h: y_max - y_min,
                        w: word_end - word_start_pos
                    }
                });
                
                out.push(' ');
                trailing_space = true;
                word_start = true;
                word_start_idx = out.len();
            }
            pos = c.offset;
            end = c.pos + x_off + c.width;
            if c.offset == 0 || !trailing_space {
                word_end = (span.transform.matrix * Vector2F::new(end, 0.0)).x();
            }

            if word_start {
                y_min = span.rect.min_y();
                y_max = span.rect.max_y();
                word_start_pos = (span.transform.matrix * Vector2F::new(c.pos + x_off, 0.0)).x();
                word_start = false;
            } else {
                y_min = y_min.min(span.rect.min_y());
                y_max = y_max.max(span.rect.max_y());
            }
        }
        trailing_space = span.text[pos..].chars().all(|c| c.is_whitespace());

        out.extend(span.text[pos..].nfkc());
    }
    words.push(Word {
        text: out[word_start_idx..].into(),
        rect: Rect {
            x: word_start_pos,
            y: y_min,
            h: y_max - y_min,
            w: word_end - word_start_pos
        }
    });
    
    words
}
