use serde::{Serialize, Deserialize};
use table::Table;

pub use crate::util::{Rect, CellContent};

#[derive(Serialize, Deserialize, Debug)]
pub struct Word {
    pub text: String,
    pub rect: Rect,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct Line {
    pub words: Vec<Word>,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct Run {
    pub lines: Vec<Line>,
    pub kind: RunType,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Flow {
    pub lines: Vec<Line>,
    pub runs: Vec<Run>,
}
#[derive(Serialize, Deserialize, Debug)]
pub enum RunType {
    ParagraphContinuation,
    Paragraph,
    Header,
    /// A table, cells row by row. `rows[r][c]` is `None` when the cell is
    /// empty or covered by a cell that spans into it from above/left.
    Table(TableRun),
    Cell,
}

/// A table as extracted from a page: `columns` wide, cells in reading order.
#[derive(Serialize, Deserialize, Debug)]
pub struct TableRun {
    pub columns: usize,
    pub rows: Vec<Vec<Option<CellContent>>>,
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
        let (num_rows, num_cols) = table.size();
        if num_rows == 0 || num_cols == 0 {
            return;
        }
        let mut rows = vec![vec![None; num_cols as usize]; num_rows as usize];
        for cell in table.values() {
            rows[cell.row as usize][cell.col as usize] = Some(cell.value.clone());
        }
        self.runs.push(Run {
            lines: vec![],
            kind: RunType::Table(TableRun {
                columns: num_cols as usize,
                rows,
            }),
        });
    }
}
