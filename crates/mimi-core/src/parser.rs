//! Byte-at-a-time VT escape-sequence state machine, written from first
//! principles (no parser dependency). Modeled on the classic VT500 parser
//! diagram, trimmed to what a modern UTF-8 terminal needs.
//!
//! The parser owns no screen state; it decodes bytes into calls on
//! [`crate::term::Term`].

use crate::term::Term;

const MAX_PARAMS: usize = 32;
const MAX_OSC: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Ground,
    Escape,
    EscapeInt,
    Csi,
    CsiIgnore,
    Osc,
    OscEsc,
    /// DCS / SOS / PM / APC — consumed and discarded.
    Str,
    StrEsc,
}

pub struct Parser {
    state: State,
    params: Vec<u16>,
    intermediates: Vec<u8>,
    /// Private-marker byte ('?', '>', '<', '=') or 0.
    private: u8,
    osc: Vec<u8>,
    // Incremental UTF-8 decoding.
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            params: Vec::with_capacity(MAX_PARAMS),
            intermediates: Vec::with_capacity(2),
            private: 0,
            osc: Vec::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    pub fn advance(&mut self, term: &mut Term, byte: u8) {
        match self.state {
            State::Ground => self.ground(term, byte),
            State::Escape => self.escape(term, byte),
            State::EscapeInt => self.escape_int(term, byte),
            State::Csi => self.csi(term, byte),
            State::CsiIgnore => self.csi_ignore(term, byte),
            State::Osc => self.osc_put(term, byte),
            State::OscEsc => self.osc_esc(term, byte),
            State::Str => {
                if byte == 0x1b {
                    self.state = State::StrEsc;
                } else if byte == 0x07 {
                    self.state = State::Ground;
                }
            }
            State::StrEsc => {
                // Only ESC \ (ST) terminates; anything else stays in the string.
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::Str
                };
            }
        }
    }

    // ---- Ground: printable text + C0 controls -------------------------------

    fn ground(&mut self, term: &mut Term, byte: u8) {
        if self.utf8_need > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8_buf[self.utf8_len] = byte;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_need {
                    match std::str::from_utf8(&self.utf8_buf[..self.utf8_len]) {
                        Ok(s) => {
                            if let Some(c) = s.chars().next() {
                                term.print(c);
                            }
                        }
                        Err(_) => term.print('\u{fffd}'),
                    }
                    self.utf8_need = 0;
                    self.utf8_len = 0;
                }
                return;
            }
            // Broken sequence: emit replacement, reprocess this byte fresh.
            self.utf8_need = 0;
            self.utf8_len = 0;
            term.print('\u{fffd}');
        }
        match byte {
            0x1b => self.enter_escape(),
            0x00..=0x1f => term.control(byte),
            0x7f => {} // DEL is ignored
            0x20..=0x7e => term.print(byte as char),
            0xc2..=0xdf => self.start_utf8(byte, 2),
            0xe0..=0xef => self.start_utf8(byte, 3),
            0xf0..=0xf4 => self.start_utf8(byte, 4),
            _ => term.print('\u{fffd}'),
        }
    }

    fn start_utf8(&mut self, byte: u8, need: usize) {
        self.utf8_buf[0] = byte;
        self.utf8_len = 1;
        self.utf8_need = need;
    }

    // ---- Escape -------------------------------------------------------------

    fn enter_escape(&mut self) {
        self.state = State::Escape;
        self.intermediates.clear();
        self.private = 0;
    }

    fn escape(&mut self, term: &mut Term, byte: u8) {
        match byte {
            b'[' => {
                self.params.clear();
                self.params.push(0);
                self.state = State::Csi;
            }
            b']' => {
                self.osc.clear();
                self.state = State::Osc;
            }
            b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
            0x20..=0x2f => {
                self.intermediates.push(byte);
                self.state = State::EscapeInt;
            }
            0x1b => self.enter_escape(),
            0x00..=0x1f => term.control(byte), // controls execute mid-sequence
            _ => {
                term.esc_dispatch(&[], byte);
                self.state = State::Ground;
            }
        }
    }

    fn escape_int(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x20..=0x2f => {
                if self.intermediates.len() < 2 {
                    self.intermediates.push(byte);
                }
            }
            0x1b => self.enter_escape(),
            0x00..=0x1f => term.control(byte),
            _ => {
                let inter = std::mem::take(&mut self.intermediates);
                term.esc_dispatch(&inter, byte);
                self.state = State::Ground;
            }
        }
    }

    // ---- CSI ----------------------------------------------------------------

    fn csi(&mut self, term: &mut Term, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                let p = self.params.last_mut().unwrap();
                *p = p.saturating_mul(10).saturating_add((byte - b'0') as u16);
            }
            // Treat ':' like ';' — folds `38:2:r:g:b` into the `;` form.
            b';' | b':' => {
                if self.params.len() < MAX_PARAMS {
                    self.params.push(0);
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            0x3c..=0x3f => {
                // Private markers are only valid before any digits.
                if self.params == [0] && self.private == 0 {
                    self.private = byte;
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            0x20..=0x2f => {
                if self.intermediates.len() < 2 {
                    self.intermediates.push(byte);
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            0x40..=0x7e => {
                term.csi_dispatch(self.private, &self.params, &self.intermediates, byte);
                self.state = State::Ground;
            }
            0x18 | 0x1a => self.state = State::Ground, // CAN / SUB abort
            0x1b => self.enter_escape(),
            0x00..=0x1f => term.control(byte),
            _ => self.state = State::CsiIgnore,
        }
    }

    fn csi_ignore(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x40..=0x7e => self.state = State::Ground,
            0x18 | 0x1a => self.state = State::Ground,
            0x1b => self.enter_escape(),
            0x00..=0x1f => term.control(byte),
            _ => {}
        }
    }

    // ---- OSC ----------------------------------------------------------------

    fn osc_put(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x07 => {
                term.osc_dispatch(&std::mem::take(&mut self.osc));
                self.state = State::Ground;
            }
            0x1b => self.state = State::OscEsc,
            0x18 | 0x1a => {
                self.osc.clear();
                self.state = State::Ground;
            }
            _ => {
                if self.osc.len() < MAX_OSC {
                    self.osc.push(byte);
                }
            }
        }
    }

    fn osc_esc(&mut self, term: &mut Term, byte: u8) {
        if byte == b'\\' {
            term.osc_dispatch(&std::mem::take(&mut self.osc));
            self.state = State::Ground;
        } else {
            // Not ST — the OSC was aborted by a new escape sequence.
            self.osc.clear();
            self.enter_escape();
            self.escape(term, byte);
        }
    }
}
