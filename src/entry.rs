use serde::{Serialize, Deserialize};
use table::Table;

use crate::util::{Rect, CellContent};

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
pub struct Flow {
    pub lines: Vec<Line>,
    pub runs: Vec<Run>,
}
#[derive(Serialize, Deserialize)]
pub enum RunType {
    ParagraphContinuation,
    Paragraph,
    Header,
    Cell,
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
