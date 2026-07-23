//! mimi-core: the terminal emulation engine.
//!
//! Built from first principles: a byte-at-a-time VT state machine (`parser`)
//! drives a screen model (`term` + `grid`). No async, no allocations on the
//! hot path beyond the grid itself. The embedder feeds PTY bytes into
//! [`Emulator::process`] and reads the resulting screen state and events.
//!
//! Semantic command blocks (OSC 133 / OSC 633 shell integration) are first
//! class: every command a shell runs becomes a [`blocks::Block`] with its
//! command line, cwd, output, and exit code — the substrate for agentic
//! workflows.

pub mod blocks;
pub mod cell;
pub mod grid;
pub mod parser;
pub mod term;

pub use cell::{flags, Cell, Color};
pub use grid::Grid;
pub use parser::Parser;
pub use term::{Term, TermEvent};

/// Parser + terminal bundled together so embedders hold a single object
/// behind a lock and feed it raw PTY bytes.
pub struct Emulator {
    pub parser: Parser,
    pub term: Term,
}

impl Emulator {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        Self {
            parser: Parser::new(),
            term: Term::new(cols, rows, scrollback),
        }
    }

    /// Feed raw bytes from the PTY. Returns true if anything visible may
    /// have changed (used to coalesce redraws).
    pub fn process(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        for &b in bytes {
            self.parser.advance(&mut self.term, b);
        }
        true
    }
}
