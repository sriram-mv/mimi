//! The visible screen: a dense 2D array of cells.
//!
//! Scrollback lives in [`crate::term::Term`]; the grid is only ever
//! `rows * cols` — this keeps every cursor operation O(1) and cache-friendly.

use crate::cell::{Cell, Color};

#[derive(Clone)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    lines: Vec<Vec<Cell>>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        Grid {
            cols,
            rows,
            lines: vec![vec![Cell::default(); cols]; rows],
        }
    }

    #[inline]
    pub fn line(&self, row: usize) -> &[Cell] {
        &self.lines[row]
    }

    #[inline]
    pub fn cell(&self, col: usize, row: usize) -> &Cell {
        &self.lines[row][col]
    }

    #[inline]
    pub fn cell_mut(&mut self, col: usize, row: usize) -> &mut Cell {
        &mut self.lines[row][col]
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        for line in &mut self.lines {
            line.resize(cols, Cell::default());
        }
        self.lines.resize(rows, vec![Cell::default(); cols]);
        self.cols = cols;
        self.rows = rows;
    }

    /// Scroll `region` (top..=bot) up by n, returning the lines that fell off
    /// the top so the caller can push them into scrollback.
    pub fn scroll_up(&mut self, top: usize, bot: usize, n: usize) -> Vec<Vec<Cell>> {
        let n = n.min(bot - top + 1);
        let mut evicted = Vec::with_capacity(n);
        for _ in 0..n {
            let line = self.lines.remove(top);
            evicted.push(line);
            self.lines.insert(bot, vec![Cell::default(); self.cols]);
        }
        evicted
    }

    /// Scroll `region` (top..=bot) down by n. Lines falling off the bottom
    /// are discarded (per VT semantics).
    pub fn scroll_down(&mut self, top: usize, bot: usize, n: usize) {
        let n = n.min(bot - top + 1);
        for _ in 0..n {
            self.lines.remove(bot);
            self.lines.insert(top, vec![Cell::default(); self.cols]);
        }
    }

    pub fn clear_line_range(&mut self, row: usize, from: usize, to: usize, bg: Color) {
        let to = to.min(self.cols);
        for cell in &mut self.lines[row][from..to] {
            *cell = Cell::blank_with_bg(bg);
        }
    }

    pub fn clear_all(&mut self, bg: Color) {
        for row in 0..self.rows {
            self.clear_line_range(row, 0, self.cols, bg);
        }
    }

    /// Plain text of one row, trailing whitespace trimmed. Wide-char spacer
    /// cells are skipped so CJK/emoji round-trip as single characters.
    pub fn row_text(&self, row: usize) -> String {
        let mut s = String::with_capacity(self.cols);
        for cell in &self.lines[row] {
            if cell.flags & crate::cell::flags::WIDE_SPACER != 0 {
                continue;
            }
            s.push(cell.ch);
        }
        s.truncate(s.trim_end().len());
        s
    }

    pub fn take_lines(self) -> Vec<Vec<Cell>> {
        self.lines
    }
}
