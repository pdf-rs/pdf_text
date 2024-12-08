use crate::classify::{classify, Class};
use crate::tree::{Node, NodeTag};
use crate::util::{avg, CellContent, Rect};
use crate::text::concat_text;
use std::iter::once;
use pathfinder_geometry::rect::RectF;
use pdf_render::TextSpan;

use std::mem::take;
use font::Encoder;
use serde::{Serialize, Deserialize};
use table::Table;

#[derive(Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub rect: Rect,
}
#[derive(Serialize, Deserialize)]
pub struct Line {
    pub words: Vec<Word>,
}
#[derive(Serialize, Deserialize)]
pub struct Run {
    pub lines: Vec<Line>,
    pub kind: RunType,
}

#[derive(Serialize, Deserialize)]
pub enum RunType {
    ParagraphContinuation,
    Paragraph,
    Header,
    Cell,
}

#[derive(Serialize, Deserialize)]
pub struct Flow {
    pub lines: Vec<Line>,
    pub runs: Vec<Run>,
}

impl Flow {
    pub fn new() -> Self {
        Flow { 
            lines: vec![],
            runs: vec![]
        }
    }
    pub fn add_line(&mut self, words: Vec<Word>, kind: RunType) {
        if words.len() > 0 {
            self.runs.push(Run {
                lines: vec![Line { words }], 
                kind
            });
        }
    }
    pub fn add_table(&mut self, table: Table<CellContent>) {
        
    }
}

pub(crate) fn build<E: Encoder>(mut flow: &mut Flow, spans: &[TextSpan<E>], node: &Node, x_anchor: f32) {
    match *node {
        Node::Final { ref indices } => {
            if indices.len() > 0 {
                let node_spans = indices.iter().flat_map(|&i| spans.get(i));
                let bbox = node_spans.clone().map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap();
                let class = classify(node_spans.clone());
                let mut text = String::new();
                let words = concat_text(&mut text, node_spans);

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
                    let words = concat_text(&mut text, line_spans.clone());
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
                            let words = concat_text(&mut text, indices[line_start..end].iter().flat_map(|&i| spans.get(i)));

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
                        build(flow, spans, node, x);
                    }
                }
            }
        }
        Node::Table { ref table } => {
            if let Some(bbox) = table.values()
                .flat_map(|v| v.value.iter().flat_map(|&i| spans.get(i).map(|s| s.rect)))
                .reduce(|a, b| a.union_rect(b)) {
                let table = table.flat_map(|indices| {
                    if indices.len() == 0 {
                        None
                    } else {
                        let line_spans = indices.iter().flat_map(|&i| spans.get(i));
                        let bbox: RectF = line_spans.clone().map(|s| s.rect).reduce(|a, b| a.union_rect(b)).unwrap().into();

                        let mut text = String::new();
                        concat_text(&mut text, line_spans.clone());
                        Some(CellContent {
                            text,
                            rect: bbox.into(),
                        })
                    }
                });
                flow.add_table(table);
            }
        }
    }
}