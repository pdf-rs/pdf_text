//! Words from glyphs.
//!
//! Where a word ends and the next one begins is a matter of *spacing*: the
//! gap between the ink of two glyphs, measured in em so that it does not
//! depend on the font size. A PDF may draw that gap with a space glyph, with a
//! `TJ` adjustment, with `Td`, or with nothing at all (justified text moves
//! glyphs apart until they line up). So the gap threshold has to be measured
//! from the text itself rather than assumed.
//!
//! [`Stats`] measures it at three levels — the block, the page, the document —
//! and [`concat_text`] uses the narrowest level that has enough evidence.

use pathfinder_geometry::vector::Vector2F;
use crate::Span as TextSpan;
use unicode_normalization::UnicodeNormalization;
use crate::{entry::Word, util::Rect};

/// Gaps are counted in a histogram of this many bins over `0..MAX_GAP_EM` em.
const BINS: usize = 96;
/// Gaps wider than this (in em) all land in the last bin.
const MAX_GAP_EM: f32 = 1.5;
/// Smallest number of glyph gaps before a histogram is trusted at all.
const MIN_SAMPLES: u32 = 12;
/// A word gap narrower or wider than this is not credible; if the measurement
/// lands outside, distrust it and fall back.
const MIN_SPACE_EM: f32 = 0.07;
const MAX_SPACE_EM: f32 = 1.0;
/// Lowest share of the gaps that may fall on either side of the split: a
/// threshold that only separates a handful of outliers from the rest says
/// nothing about words.
const MIN_LOWER_SHARE: f32 = 0.2;
const MIN_UPPER_SHARE: f32 = 0.04;
/// The gap above which two glyphs are in different words when there is no
/// text to measure (a lone heading, a page number, an empty document): about
/// one space glyph, wider than any kern and narrower than any word gap.
pub const FALLBACK_SPACE_EM: f32 = 0.25;

/// A histogram of the gaps between the ink of consecutive glyphs, in em, plus
/// the font sizes seen along the way.
///
/// Observations accumulate, so one `Stats` can cover a block, a page, or a
/// whole document; [`Stats::merge`] combines levels.
#[derive(Clone, Debug)]
pub struct Stats {
    gaps: [u32; BINS],
    count: u32,
    font_sizes: Vec<f32>,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            gaps: [0; BINS],
            count: 0,
            font_sizes: Vec::new(),
        }
    }
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the glyph gaps of one block of text: the spans of a line, of a
    /// paragraph, or of a page, in reading order.
    pub fn observe<'a>(&mut self, items: impl Iterator<Item = &'a TextSpan> + Clone) {
        let mut prev: Option<(f32, f32, f32)> = None; // (left, right, font size)

        for span in items {
            let tr_inv = span.transform.matrix.inverse();
            let x_off = (tr_inv * span.transform.vector).x();
            if span.font_size > 0.0 {
                self.font_sizes.push(span.font_size);
            }

            for c in span.chars.iter() {
                // A space glyph is a gap in itself; only the gaps *between*
                // ink count.
                if span.text[c.offset..].chars().next().map(char::is_whitespace).unwrap_or(false) {
                    continue;
                }
                let left = c.pos + x_off;
                let right = left + c.width;

                if let Some((prev_left, prev_right, prev_size)) = prev {
                    // Skip anything that goes backwards: the next line, a
                    // second column, a superscript.
                    if left > prev_left {
                        let size = 0.5 * (prev_size + span.font_size);
                        if size > 0.0 {
                            self.add_gap((left - prev_right) / size);
                        }
                    }
                }
                prev = Some((left, right, span.font_size));
            }
        }
    }

    fn add_gap(&mut self, em: f32) {
        if !em.is_finite() {
            return;
        }
        let bin = ((em.max(0.0) / MAX_GAP_EM) * BINS as f32) as usize;
        self.gaps[bin.min(BINS - 1)] += 1;
        self.count += 1;
    }

    /// Fold another sample into this one. Counts add, so the wider sample
    /// carries more weight.
    pub fn merge(&mut self, other: &Stats) {
        for (a, b) in self.gaps.iter_mut().zip(other.gaps.iter()) {
            *a += *b;
        }
        self.count += other.count;
        self.font_sizes.extend_from_slice(&other.font_sizes);
    }

    /// How many glyph gaps went into this histogram.
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Mean font size of the text that was observed, if any.
    pub fn font_size(&self) -> Option<f32> {
        if self.font_sizes.is_empty() {
            None
        } else {
            let sum: f32 = self.font_sizes.iter().sum();
            Some(sum / self.font_sizes.len() as f32)
        }
    }

    /// The gap, in em, above which two glyphs are in different words.
    ///
    /// The gap distribution of running text is two-humped: kerns and the
    /// occasional tight pair sit near zero, word gaps sit around a quarter to
    /// a third of an em. Otsu's method finds the cut that separates the two
    /// humps with the least overlap; when the text is not two-humped (one
    /// word, a run of digits, a table cell with no spaces) this returns `None`
    /// and the caller falls back to a wider sample.
    pub fn space_em(&self) -> Option<f32> {
        if self.count < MIN_SAMPLES {
            return None;
        }

        let total = self.count as f64;
        let sum_all: f64 = (0..BINS).map(|i| i as f64 * self.gaps[i] as f64).sum();

        // Between-class variance of every split, then the *middle* of the
        // splits that do equally well: with two tight humps separated by an
        // empty valley every cut in the valley is equally good, and the cut
        // belongs in the middle of it, not at its edge.
        let mut between = [0.0f64; BINS];
        let mut w0 = 0.0;
        let mut sum0 = 0.0;
        for i in 0..BINS - 1 {
            w0 += self.gaps[i] as f64;
            sum0 += i as f64 * self.gaps[i] as f64;
            let w1 = total - w0;
            if w0 < 1.0 || w1 < 1.0 {
                continue;
            }
            let m0 = sum0 / w0;
            let m1 = (sum_all - sum0) / w1;
            between[i] = w0 * w1 * (m0 - m1) * (m0 - m1);
        }
        let best = between.iter().cloned().fold(0.0, f64::max);
        if best <= 0.0 {
            return None;
        }
        let good = |b: f64| b >= best * (1.0 - 1e-6);
        let first = (0..BINS - 1).find(|&i| good(between[i]))?;
        let last = (0..BINS - 1).rev().find(|&i| good(between[i]))?;
        let bin = (first + last) / 2;

        // Where each class actually sits, to check that the cut means
        // something.
        let lower: f64 = (0..=bin).map(|i| self.gaps[i] as f64 * (i as f64 + 0.5)).sum::<f64>()
            / (0..=bin).map(|i| self.gaps[i] as f64).sum::<f64>().max(1.0);
        let upper: f64 = (bin + 1..BINS).map(|i| self.gaps[i] as f64 * (i as f64 + 0.5)).sum::<f64>()
            / (bin + 1..BINS).map(|i| self.gaps[i] as f64).sum::<f64>().max(1.0);
        let bin_em = (MAX_GAP_EM / BINS as f32) as f64;

        // Only a split with weight on both sides, and with an upper class that
        // sits where word gaps sit, is worth anything.
        let lower_share = (0..=bin).map(|i| self.gaps[i]).sum::<u32>() as f32 / self.count as f32;
        let upper_share = 1.0 - lower_share;
        if lower_share < MIN_LOWER_SHARE || upper_share < MIN_UPPER_SHARE {
            return None;
        }
        if upper * bin_em < 1.5 * MIN_SPACE_EM as f64 {
            return None;
        }

        // Cut between the last bin of the lower class and the first of the
        // upper one — the valley, not the middle of a hump.
        let threshold = (bin as f32 + 1.0) * MAX_GAP_EM / BINS as f32;
        if !(MIN_SPACE_EM..=MAX_SPACE_EM).contains(&threshold) {
            return None;
        }
        Some(threshold)
    }
}

/// Where [`concat_text`] takes its word-gap estimate from, besides the block
/// it is given: the page and the document that block belongs to.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context<'a> {
    pub page: Option<&'a Stats>,
    pub document: Option<&'a Stats>,
}

impl<'a> Context<'a> {
    pub fn new(page: Option<&'a Stats>, document: Option<&'a Stats>) -> Self {
        Context { page, document }
    }

    /// The narrowest level with enough evidence: the block itself, then its
    /// page, then the document, then a constant.
    fn space_em(self, block: &Stats) -> f32 {
        block.space_em()
            .or_else(|| self.page.and_then(Stats::space_em))
            .or_else(|| self.document.and_then(Stats::space_em))
            .unwrap_or(FALLBACK_SPACE_EM)
    }
}

pub fn concat_text<'a>(
    out: &mut String,
    items: impl Iterator<Item = &'a TextSpan> + Clone,
    context: Context,
) -> Vec<Word> {
    let mut words = vec![];

    // This block's own spacing first; the page and the document decide for
    // blocks that are too short to say anything (a heading, a table cell).
    let mut block = Stats::new();
    block.observe(items.clone());
    let space_em = context.space_em(&block);

    let mut end = 0.; // trailing edge of the last char
    let mut end_size = 0.; // font size of the last char
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
            let gap = c.pos + x_off - end;
            let size = if end_size > 0.0 { 0.5 * (end_size + span.font_size) } else { span.font_size };
            if !trailing_space && size > 0.0 && gap > space_em * size {
                // A span boundary can leave nothing but the space itself
                // behind; a whitespace-only "word" is not one.
                if !out[word_start_idx..].trim().is_empty() {
                    words.push(Word {
                        text: out[word_start_idx..].into(),
                        rect: Rect {
                            x: word_start_pos,
                            y: y_min,
                            h: y_max - y_min,
                            w: word_end - word_start_pos,
                        },
                    });
                }

                out.push(' ');
                trailing_space = true;
                word_start = true;
                word_start_idx = out.len();
            }
            pos = c.offset;
            end = c.pos + x_off + c.width;
            end_size = span.font_size;
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

        // The last character of the span is never appended inside the loop
        // above; append it now — but do not stack a second space when one text
        // run ends with a space glyph and the next one *is* a space glyph.
        if !(trailing_space && out.chars().last().map(|c| c.is_whitespace()).unwrap_or(false)) {
            out.extend(span.text[pos..].nfkc());
        }
    }
    if !out[word_start_idx..].trim().is_empty() {
        words.push(Word {
            text: out[word_start_idx..].into(),
            rect: Rect {
                x: word_start_pos,
                y: y_min,
                h: y_max - y_min,
                w: word_end - word_start_pos,
            },
        });
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;
    use pdf::content::TextMode;
    use pdf_render::{Fill, TextChar};
    use pathfinder_geometry::rect::RectF;
    use pathfinder_geometry::transform2d::Transform2F;
    use pathfinder_geometry::vector::vec2f;

    /// One text run on one baseline. The words are drawn with `gap` em of
    /// white space between them and *no space glyphs at all*, which is how
    /// justified text is usually drawn: the gap is all there is.
    fn run(words: &[&str], gap: f32, size: f32, x: f32) -> Span {
        let mut text = String::new();
        let mut chars = vec![];
        let mut pen = 0.0f32;
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                pen += gap * size;
            }
            for ch in word.chars() {
                let width = 0.55 * size;
                chars.push(TextChar {
                    offset: text.len(),
                    pos: pen,
                    width,
                    idx: chars.len(),
                    leading_space: 0.0,
                });
                text.push(ch);
                pen += width;
            }
        }
        Span {
            rect: RectF::from_points(vec2f(0.0, -size), vec2f(pen, 0.0)),
            width: pen,
            font_size: size,
            font: None,
            text,
            chars,
            color: Fill::black(),
            alpha: 1.0,
            transform: Transform2F::from_translation(vec2f(x, 600.0)),
            mode: TextMode::Fill,
            op_nr: 0,
            bytes: vec![],
        }
    }

    fn words(spans: &[Span], ctx: Context) -> Vec<String> {
        let mut text = String::new();
        concat_text(&mut text, spans.iter(), ctx)
            .into_iter()
            .map(|w| w.text)
            .collect()
    }

    #[test]
    fn words_without_space_glyphs_are_split_at_the_measured_gap() {
        let line = vec![run(&["alpha", "beta", "gamma", "delta", "epsilon"], 0.25, 10.0, 0.0)];
        assert_eq!(
            words(&line, Context::default()),
            ["alpha", "beta", "gamma", "delta", "epsilon"]
        );
    }

    /// Justified text moves the glyphs far enough apart that a fixed
    /// threshold either misses the gap or splits inside a word.
    #[test]
    fn wide_justified_gaps_are_still_word_gaps() {
        let line = vec![run(&["a", "b", "c", "d", "e", "f", "g"], 0.9, 10.0, 0.0)];
        assert_eq!(words(&line, Context::default()), ["a", "b", "c", "d", "e", "f", "g"]);
    }

    /// Two letters that kern apart are not two words.
    #[test]
    fn kerning_does_not_make_words() {
        let mut line = vec![run(&["internationalization"], 0.0, 10.0, 0.0)];
        // a little air between every glyph, as a loose font would have
        for (i, c) in line[0].chars.iter_mut().enumerate() {
            c.pos += 0.02 * 10.0 * i as f32;
        }
        assert_eq!(words(&line, Context::default()), ["internationalization"]);
    }

    /// A short block — a heading, a caption — has too few gaps to measure
    /// anything, so it takes the page's measure. Here the gap is one that the
    /// constant fallback (0.25 em) would miss.
    #[test]
    fn a_short_block_borrows_the_page_measure() {
        let body = run(
            &["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"],
            0.2,
            8.0,
            0.0,
        );
        let mut page = Stats::new();
        page.observe(std::iter::once(&body));

        let heading = vec![run(&["Chapter", "Seven"], 0.2, 24.0, 0.0)];
        assert_eq!(words(&heading, Context::new(Some(&page), None)), ["Chapter", "Seven"]);
        // without the page, two words and a gap below the fallback stay one
        assert_eq!(words(&heading, Context::default()), ["ChapterSeven"]);
    }

    /// The same for a page that holds hardly any text at all: the document
    /// knows how the text of this file is spaced.
    #[test]
    fn an_empty_page_borrows_the_document_measure() {
        let body = run(
            &["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"],
            0.2,
            8.0,
            0.0,
        );
        let mut document = Stats::new();
        document.observe(std::iter::once(&body));
        let page = Stats::new();

        let caption = vec![run(&["Figure", "7"], 0.2, 8.0, 0.0)];
        let ctx = Context::new(Some(&page), Some(&document));
        assert_eq!(words(&caption, ctx), ["Figure", "7"]);
        assert_eq!(words(&caption, Context::new(Some(&page), None)), ["Figure7"]);
    }

    /// A big heading on the page must not widen the spacing of the small text
    /// below it: the measure is taken per block first.
    #[test]
    fn a_big_heading_does_not_loosen_the_body_text() {
        let heading = run(&["Chapter", "Seven"], 0.3, 24.0, 0.0);
        let body = vec![run(&["alpha", "beta", "gamma", "delta", "epsilon"], 0.2, 6.0, 0.0)];
        let mut page = Stats::new();
        page.observe(std::iter::once(&heading));
        page.observe(body.iter());

        // 0.2 em of 6 pt is 1.2 pt; half the *heading's* size would be 12 pt.
        assert_eq!(
            words(&body, Context::new(Some(&page), None)),
            ["alpha", "beta", "gamma", "delta", "epsilon"]
        );
        assert_eq!(words(&body, Context::default()), ["alpha", "beta", "gamma", "delta", "epsilon"]);
    }
}
