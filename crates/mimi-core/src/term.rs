//! The terminal screen model: cursor, modes, scroll regions, alt screen,
//! scrollback, and dispatch targets for the parser.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use crate::blocks::BlockStore;
use crate::cell::{flags, Cell, Color};
use crate::grid::Grid;

/// Events surfaced to the embedder (window title, bell, block lifecycle...).
#[derive(Clone, Debug, PartialEq)]
pub enum TermEvent {
    Title(String),
    Bell,
    CwdChanged(String),
    BlockStarted(u64),
    BlockFinished(u64),
}

#[derive(Clone, Copy)]
struct SavedCursor {
    x: usize,
    y: usize,
    pen: Cell,
    origin_mode: bool,
    pending_wrap: bool,
}

pub struct Term {
    pub grid: Grid,
    /// Primary screen stashed while the alt screen is active.
    saved_primary: Option<Grid>,
    pub alt_active: bool,

    pub scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,

    pub cursor_x: usize,
    pub cursor_y: usize,
    /// Current SGR attributes, applied to printed cells.
    pen: Cell,
    saved_cursor: Option<SavedCursor>,
    pending_wrap: bool,

    scroll_top: usize,
    scroll_bot: usize,

    // Modes.
    pub show_cursor: bool,
    pub bracketed_paste: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub focus_events: bool,
    pub mouse_mode: u16, // 0 | 1000 | 1002 | 1003
    pub mouse_sgr: bool,
    autowrap: bool,
    origin_mode: bool,
    insert_mode: bool,

    tabstops: Vec<bool>,

    // Charsets (DEC line drawing for G0/G1, selected via SI/SO).
    g0_linedraw: bool,
    g1_linedraw: bool,
    g1_active: bool,

    pub title: String,
    pub cwd: Option<String>,

    pub events: VecDeque<TermEvent>,
    /// Bytes the emulator wants written back to the PTY (DA/DSR replies).
    pub responses: Vec<u8>,

    pub blocks: BlockStore,
}

fn default_tabstops(cols: usize) -> Vec<bool> {
    (0..cols).map(|i| i % 8 == 0 && i != 0).collect()
}

impl Term {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        Term {
            grid: Grid::new(cols, rows),
            saved_primary: None,
            alt_active: false,
            scrollback: VecDeque::new(),
            max_scrollback,
            cursor_x: 0,
            cursor_y: 0,
            pen: Cell::default(),
            saved_cursor: None,
            pending_wrap: false,
            scroll_top: 0,
            scroll_bot: rows - 1,
            show_cursor: true,
            bracketed_paste: false,
            app_cursor: false,
            app_keypad: false,
            focus_events: false,
            mouse_mode: 0,
            mouse_sgr: false,
            autowrap: true,
            origin_mode: false,
            insert_mode: false,
            tabstops: default_tabstops(cols),
            g0_linedraw: false,
            g1_linedraw: false,
            g1_active: false,
            title: String::new(),
            cwd: None,
            events: VecDeque::new(),
            responses: Vec::new(),
            blocks: BlockStore::default(),
        }
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.grid.cols
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.grid.rows
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols() && rows == self.rows() {
            return;
        }
        self.grid.resize(cols, rows);
        if let Some(primary) = &mut self.saved_primary {
            primary.resize(cols, rows);
        }
        for line in &mut self.scrollback {
            line.resize(cols, Cell::default());
        }
        self.tabstops = default_tabstops(cols);
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.cursor_x = self.cursor_x.min(cols - 1);
        self.cursor_y = self.cursor_y.min(rows - 1);
        self.pending_wrap = false;
    }

    fn push_event(&mut self, ev: TermEvent) {
        // Bounded so a hostile stream can't balloon memory if the embedder
        // stalls.
        if self.events.len() < 1024 {
            self.events.push_back(ev);
        }
    }

    // ---- Printing -----------------------------------------------------------

    pub fn print(&mut self, ch: char) {
        let ch = self.map_charset(ch);
        let width = ch.width().unwrap_or(1);
        if width == 0 {
            return; // combining marks: not composed in v1
        }

        if !self.alt_active && self.blocks.is_capturing() {
            self.blocks.capture(ch);
        }

        if self.pending_wrap && self.autowrap {
            self.pending_wrap = false;
            self.cursor_x = 0;
            self.linefeed_no_capture();
        }

        // A wide char that doesn't fit wraps early (or clips without autowrap).
        if width == 2 && self.cursor_x + 2 > self.cols() {
            if self.autowrap {
                self.cursor_x = 0;
                self.linefeed_no_capture();
            } else {
                self.cursor_x = self.cols() - 2;
            }
        }

        if self.insert_mode {
            let y = self.cursor_y;
            let cols = self.cols();
            for x in (self.cursor_x + width..cols).rev() {
                *self.grid.cell_mut(x, y) = *self.grid.cell(x - width, y);
            }
        }

        let (x, y) = (self.cursor_x, self.cursor_y);
        let mut cell = self.pen;
        cell.ch = ch;
        if width == 2 {
            cell.flags |= flags::WIDE;
            *self.grid.cell_mut(x, y) = cell;
            let mut spacer = self.pen;
            spacer.ch = ' ';
            spacer.flags |= flags::WIDE_SPACER;
            *self.grid.cell_mut(x + 1, y) = spacer;
        } else {
            *self.grid.cell_mut(x, y) = cell;
        }

        let next = x + width;
        if next >= self.cols() {
            self.cursor_x = self.cols() - 1;
            self.pending_wrap = true;
        } else {
            self.cursor_x = next;
        }
    }

    fn map_charset(&self, ch: char) -> char {
        let linedraw = if self.g1_active {
            self.g1_linedraw
        } else {
            self.g0_linedraw
        };
        if !linedraw {
            return ch;
        }
        // DEC Special Graphics (what vim/tmux borders use).
        match ch {
            'j' => '┘',
            'k' => '┐',
            'l' => '┌',
            'm' => '└',
            'n' => '┼',
            'q' => '─',
            't' => '├',
            'u' => '┤',
            'v' => '┴',
            'w' => '┬',
            'x' => '│',
            'a' => '▒',
            'f' => '°',
            'g' => '±',
            'y' => '≤',
            'z' => '≥',
            '{' => 'π',
            '|' => '≠',
            '}' => '£',
            '~' => '·',
            '`' => '◆',
            '0' => '█',
            _ => ch,
        }
    }

    // ---- C0 controls --------------------------------------------------------

    pub fn control(&mut self, byte: u8) {
        match byte {
            0x07 => self.push_event(TermEvent::Bell),
            0x08 => {
                // BS
                self.cursor_x = self.cursor_x.saturating_sub(1);
                self.pending_wrap = false;
            }
            0x09 => self.horizontal_tab(),
            0x0a..=0x0c => {
                // LF / VT / FF
                if !self.alt_active && self.blocks.is_capturing() {
                    self.blocks.capture('\n');
                }
                self.linefeed_no_capture();
            }
            0x0d => {
                // CR
                self.cursor_x = 0;
                self.pending_wrap = false;
            }
            0x0e => self.g1_active = true,  // SO
            0x0f => self.g1_active = false, // SI
            _ => {}
        }
    }

    fn horizontal_tab(&mut self) {
        let mut x = self.cursor_x + 1;
        while x < self.cols() - 1 && !self.tabstops[x] {
            x += 1;
        }
        self.cursor_x = x.min(self.cols() - 1);
        self.pending_wrap = false;
    }

    fn linefeed_no_capture(&mut self) {
        if self.cursor_y == self.scroll_bot {
            self.scroll_region_up(1);
        } else if self.cursor_y + 1 < self.rows() {
            self.cursor_y += 1;
        }
        self.pending_wrap = false;
    }

    fn scroll_region_up(&mut self, n: usize) {
        let evicted = self.grid.scroll_up(self.scroll_top, self.scroll_bot, n);
        // Only a full-screen region feeds scrollback, and never on the alt
        // screen (matching conventional terminal behavior).
        if !self.alt_active
            && self.scroll_top == 0
            && self.scroll_bot == self.rows() - 1
            && self.max_scrollback > 0
        {
            for line in evicted {
                if self.scrollback.len() >= self.max_scrollback {
                    self.scrollback.pop_front();
                }
                self.scrollback.push_back(line);
            }
        }
    }

    // ---- ESC dispatch -------------------------------------------------------

    pub fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            (None, b'D') => self.linefeed_no_capture(), // IND
            (None, b'E') => {
                // NEL
                self.cursor_x = 0;
                self.linefeed_no_capture();
            }
            (None, b'H') => {
                if self.cursor_x < self.cols() {
                    self.tabstops[self.cursor_x] = true;
                }
            }
            (None, b'M') => self.reverse_index(),
            (None, b'c') => self.full_reset(),
            (None, b'=') => self.app_keypad = true,
            (None, b'>') => self.app_keypad = false,
            (Some(b'('), f) => self.g0_linedraw = f == b'0',
            (Some(b')'), f) => self.g1_linedraw = f == b'0',
            (Some(b'#'), b'8') => {
                // DECALN screen alignment test
                for y in 0..self.rows() {
                    for x in 0..self.cols() {
                        *self.grid.cell_mut(x, y) = Cell {
                            ch: 'E',
                            ..Cell::default()
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor_y == self.scroll_top {
            self.grid.scroll_down(self.scroll_top, self.scroll_bot, 1);
        } else {
            self.cursor_y = self.cursor_y.saturating_sub(1);
        }
        self.pending_wrap = false;
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            x: self.cursor_x,
            y: self.cursor_y,
            pen: self.pen,
            origin_mode: self.origin_mode,
            pending_wrap: self.pending_wrap,
        });
    }

    fn restore_cursor(&mut self) {
        if let Some(sc) = self.saved_cursor {
            self.cursor_x = sc.x.min(self.cols() - 1);
            self.cursor_y = sc.y.min(self.rows() - 1);
            self.pen = sc.pen;
            self.origin_mode = sc.origin_mode;
            self.pending_wrap = sc.pending_wrap;
        }
    }

    fn full_reset(&mut self) {
        let (cols, rows) = (self.cols(), self.rows());
        let max_scrollback = self.max_scrollback;
        let blocks = std::mem::take(&mut self.blocks);
        *self = Term::new(cols, rows, max_scrollback);
        self.blocks = blocks;
    }

    // ---- CSI dispatch -------------------------------------------------------

    pub fn csi_dispatch(&mut self, private: u8, params: &[u16], intermediates: &[u8], action: u8) {
        if !intermediates.is_empty() {
            // DECSCUSR (SP q) and friends: accepted, not yet styled.
            return;
        }
        let p = |i: usize| params.get(i).copied().unwrap_or(0) as usize;
        let p_or1 = |i: usize| p(i).max(1);

        if private == b'?' {
            match action {
                b'h' => self.dec_mode(params, true),
                b'l' => self.dec_mode(params, false),
                b'c' => self.responses.extend_from_slice(b"\x1b[?62;22c"),
                _ => {}
            }
            return;
        }
        if private != 0 {
            if private == b'>' && action == b'c' {
                self.responses.extend_from_slice(b"\x1b[>1;10;0c");
            }
            return;
        }

        match action {
            b'A' => self.move_cursor(0, -(p_or1(0) as isize)),
            b'B' => self.move_cursor(0, p_or1(0) as isize),
            b'C' => self.move_cursor(p_or1(0) as isize, 0),
            b'D' => self.move_cursor(-(p_or1(0) as isize), 0),
            b'E' => {
                self.cursor_x = 0;
                self.move_cursor(0, p_or1(0) as isize);
            }
            b'F' => {
                self.cursor_x = 0;
                self.move_cursor(0, -(p_or1(0) as isize));
            }
            b'G' => {
                self.cursor_x = (p_or1(0) - 1).min(self.cols() - 1);
                self.pending_wrap = false;
            }
            b'H' | b'f' => self.set_cursor_pos(p_or1(1) - 1, p_or1(0) - 1),
            b'd' => {
                let y = p_or1(0) - 1;
                let y = if self.origin_mode {
                    self.scroll_top + y
                } else {
                    y
                };
                self.cursor_y = y.min(self.rows() - 1);
                self.pending_wrap = false;
            }
            b'J' => self.erase_display(p(0)),
            b'K' => self.erase_line(p(0)),
            b'L' => self.insert_lines(p_or1(0)),
            b'M' => self.delete_lines(p_or1(0)),
            b'P' => self.delete_chars(p_or1(0)),
            b'@' => self.insert_chars(p_or1(0)),
            b'X' => self.erase_chars(p_or1(0)),
            b'S' => self.scroll_region_up(p_or1(0)),
            b'T' => self
                .grid
                .scroll_down(self.scroll_top, self.scroll_bot, p_or1(0)),
            b'm' => self.sgr(params),
            b'r' => {
                let top = p_or1(0) - 1;
                let bot = if p(1) == 0 { self.rows() - 1 } else { p(1) - 1 };
                if top < bot && bot < self.rows() {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                    self.set_cursor_pos(0, 0);
                }
            }
            b'g' => match p(0) {
                0 => {
                    if self.cursor_x < self.cols() {
                        self.tabstops[self.cursor_x] = false;
                    }
                }
                3 => self.tabstops.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            b'h' => {
                if params.contains(&4) {
                    self.insert_mode = true;
                }
            }
            b'l' => {
                if params.contains(&4) {
                    self.insert_mode = false;
                }
            }
            b'n' => match p(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let y = if self.origin_mode {
                        self.cursor_y - self.scroll_top
                    } else {
                        self.cursor_y
                    };
                    let reply = format!("\x1b[{};{}R", y + 1, self.cursor_x + 1);
                    self.responses.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            b'c' => self.responses.extend_from_slice(b"\x1b[?62;22c"),
            b's' => self.save_cursor(),
            b'u' => self.restore_cursor(),
            b'Z' => {
                // CBT — cursor backward tabulation
                for _ in 0..p_or1(0) {
                    let mut x = self.cursor_x;
                    while x > 0 {
                        x -= 1;
                        if self.tabstops[x] {
                            break;
                        }
                    }
                    self.cursor_x = x;
                }
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn move_cursor(&mut self, dx: isize, dy: isize) {
        let x = self.cursor_x as isize + dx;
        self.cursor_x = x.clamp(0, self.cols() as isize - 1) as usize;
        // Vertical movement respects the scroll region when starting inside it.
        let (min_y, max_y) = if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bot
        {
            (self.scroll_top as isize, self.scroll_bot as isize)
        } else {
            (0, self.rows() as isize - 1)
        };
        let y = self.cursor_y as isize + dy;
        self.cursor_y = y.clamp(min_y, max_y) as usize;
        self.pending_wrap = false;
    }

    fn set_cursor_pos(&mut self, x: usize, y: usize) {
        let y = if self.origin_mode {
            (self.scroll_top + y).min(self.scroll_bot)
        } else {
            y.min(self.rows() - 1)
        };
        self.cursor_x = x.min(self.cols() - 1);
        self.cursor_y = y;
        self.pending_wrap = false;
    }

    fn erase_display(&mut self, mode: usize) {
        let bg = self.pen.bg;
        match mode {
            0 => {
                self.erase_line(0);
                for y in self.cursor_y + 1..self.rows() {
                    self.grid.clear_line_range(y, 0, self.cols(), bg);
                }
            }
            1 => {
                self.erase_line(1);
                for y in 0..self.cursor_y {
                    self.grid.clear_line_range(y, 0, self.cols(), bg);
                }
            }
            2 => self.grid.clear_all(bg),
            3 => {
                self.grid.clear_all(bg);
                self.scrollback.clear();
            }
            _ => {}
        }
        self.pending_wrap = false;
    }

    fn erase_line(&mut self, mode: usize) {
        let bg = self.pen.bg;
        let (y, cols) = (self.cursor_y, self.cols());
        match mode {
            0 => self.grid.clear_line_range(y, self.cursor_x, cols, bg),
            1 => self
                .grid
                .clear_line_range(y, 0, (self.cursor_x + 1).min(cols), bg),
            2 => self.grid.clear_line_range(y, 0, cols, bg),
            _ => {}
        }
        self.pending_wrap = false;
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bot {
            self.grid.scroll_down(self.cursor_y, self.scroll_bot, n);
            self.cursor_x = 0;
            self.pending_wrap = false;
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bot {
            self.grid.scroll_up(self.cursor_y, self.scroll_bot, n);
            self.cursor_x = 0;
            self.pending_wrap = false;
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.cursor_x, self.cursor_y, self.cols());
        let n = n.min(cols - x);
        for i in x..cols {
            *self.grid.cell_mut(i, y) = if i + n < cols {
                *self.grid.cell(i + n, y)
            } else {
                Cell::blank_with_bg(self.pen.bg)
            };
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.cursor_x, self.cursor_y, self.cols());
        let n = n.min(cols - x);
        for i in (x + n..cols).rev() {
            *self.grid.cell_mut(i, y) = *self.grid.cell(i - n, y);
        }
        for i in x..x + n {
            *self.grid.cell_mut(i, y) = Cell::blank_with_bg(self.pen.bg);
        }
    }

    fn erase_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.cursor_x, self.cursor_y, self.cols());
        self.grid
            .clear_line_range(y, x, (x + n).min(cols), self.pen.bg);
    }

    fn dec_mode(&mut self, params: &[u16], set: bool) {
        for &p in params {
            match p {
                1 => self.app_cursor = set,
                6 => {
                    self.origin_mode = set;
                    self.set_cursor_pos(0, 0);
                }
                7 => self.autowrap = set,
                25 => self.show_cursor = set,
                47 | 1047 => self.set_alt_screen(set, false),
                1048 => {
                    if set {
                        self.save_cursor()
                    } else {
                        self.restore_cursor()
                    }
                }
                1049 => self.set_alt_screen(set, true),
                1000 | 1002 | 1003 => self.mouse_mode = if set { p } else { 0 },
                1004 => self.focus_events = set,
                1006 => self.mouse_sgr = set,
                2004 => self.bracketed_paste = set,
                _ => {}
            }
        }
    }

    fn set_alt_screen(&mut self, enter: bool, save_cursor: bool) {
        if enter && !self.alt_active {
            if save_cursor {
                self.save_cursor();
            }
            let alt = Grid::new(self.cols(), self.rows());
            self.saved_primary = Some(std::mem::replace(&mut self.grid, alt));
            self.alt_active = true;
            self.cursor_x = 0;
            self.cursor_y = 0;
            self.pending_wrap = false;
        } else if !enter && self.alt_active {
            if let Some(primary) = self.saved_primary.take() {
                self.grid = primary;
            }
            self.alt_active = false;
            if save_cursor {
                self.restore_cursor();
            }
            self.pending_wrap = false;
        }
    }

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        let params = if params.is_empty() { &[0][..] } else { params };
        while i < params.len() {
            let p = params[i];
            match p {
                0 => {
                    self.pen.fg = Color::Default;
                    self.pen.bg = Color::Default;
                    self.pen.flags = 0;
                }
                1 => self.pen.flags |= flags::BOLD,
                2 => self.pen.flags |= flags::DIM,
                3 => self.pen.flags |= flags::ITALIC,
                4 => self.pen.flags |= flags::UNDERLINE,
                7 => self.pen.flags |= flags::INVERSE,
                8 => self.pen.flags |= flags::HIDDEN,
                9 => self.pen.flags |= flags::STRIKETHROUGH,
                21 | 22 => self.pen.flags &= !(flags::BOLD | flags::DIM),
                23 => self.pen.flags &= !flags::ITALIC,
                24 => self.pen.flags &= !flags::UNDERLINE,
                27 => self.pen.flags &= !flags::INVERSE,
                28 => self.pen.flags &= !flags::HIDDEN,
                29 => self.pen.flags &= !flags::STRIKETHROUGH,
                30..=37 => self.pen.fg = Color::Indexed((p - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((p - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    let (color, used) = Self::parse_extended_color(&params[i + 1..]);
                    if let Some(c) = color {
                        if p == 38 {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn parse_extended_color(rest: &[u16]) -> (Option<Color>, usize) {
        match rest.first() {
            Some(5) => {
                if let Some(&n) = rest.get(1) {
                    (Some(Color::Indexed(n.min(255) as u8)), 2)
                } else {
                    (None, rest.len())
                }
            }
            Some(2) => {
                if rest.len() >= 4 {
                    (
                        Some(Color::Rgb(
                            rest[1].min(255) as u8,
                            rest[2].min(255) as u8,
                            rest[3].min(255) as u8,
                        )),
                        4,
                    )
                } else {
                    (None, rest.len())
                }
            }
            _ => (None, rest.len()),
        }
    }

    // ---- OSC dispatch -------------------------------------------------------

    pub fn osc_dispatch(&mut self, bytes: &[u8]) {
        let s = String::from_utf8_lossy(bytes);
        let (code, rest) = match s.split_once(';') {
            Some((c, r)) => (c, r),
            None => (s.as_ref(), ""),
        };
        match code {
            "0" | "2" => {
                self.title = rest.to_string();
                self.push_event(TermEvent::Title(rest.to_string()));
            }
            "7" => {
                if let Some(path) = parse_file_url(rest) {
                    self.cwd = Some(path.clone());
                    self.push_event(TermEvent::CwdChanged(path));
                }
            }
            "133" => self.osc_semantic(rest),
            "633" => self.osc_vscode(rest),
            "10" | "11" => {
                if rest == "?" {
                    // Default theme colors; real palette lives in the app.
                    let (osc, rgb) = if code == "10" {
                        ("10", "d6d6/dbdb/e1e1")
                    } else {
                        ("11", "0e0e/1111/1616")
                    };
                    let reply = format!("\x1b]{};rgb:{}\x1b\\", osc, rgb);
                    self.responses.extend_from_slice(reply.as_bytes());
                }
            }
            _ => {}
        }
    }

    /// OSC 133 (FinalTerm / semantic prompts): A prompt, B command-start,
    /// C pre-exec, D;exit finished.
    fn osc_semantic(&mut self, rest: &str) {
        let (mark, arg) = match rest.split_once(';') {
            Some((m, a)) => (m, Some(a)),
            None => (rest, None),
        };
        match mark {
            "A" | "B" => {}
            "C" => {
                let id = self.blocks.start(self.cwd.clone());
                self.push_event(TermEvent::BlockStarted(id));
            }
            "D" => {
                let exit = arg
                    .and_then(|a| a.split(';').next())
                    .and_then(|a| a.parse().ok());
                if let Some(id) = self.blocks.finish(exit) {
                    self.push_event(TermEvent::BlockFinished(id));
                }
            }
            _ => {}
        }
    }

    /// OSC 633 (VS Code shell integration) — we use E (command line) and
    /// P;Cwd=..., and accept A-D as aliases of 133.
    fn osc_vscode(&mut self, rest: &str) {
        let (mark, arg) = match rest.split_once(';') {
            Some((m, a)) => (m, Some(a)),
            None => (rest, None),
        };
        match mark {
            "E" => {
                if let Some(a) = arg {
                    // First field only; trailing ;nonce is discarded.
                    let cmd = a.split(';').next().unwrap_or("");
                    self.blocks.set_pending_command(decode_633(cmd));
                }
            }
            "P" => {
                if let Some(kv) = arg {
                    if let Some(cwd) = kv.strip_prefix("Cwd=") {
                        self.cwd = Some(cwd.to_string());
                        self.push_event(TermEvent::CwdChanged(cwd.to_string()));
                    }
                }
            }
            "C" => self.osc_semantic("C"),
            "D" => self.osc_semantic(rest),
            _ => {}
        }
    }

    // ---- Read-side helpers --------------------------------------------------

    /// Plain-text snapshot of the visible screen (one string per row).
    pub fn screen_text(&self) -> Vec<String> {
        (0..self.rows()).map(|y| self.grid.row_text(y)).collect()
    }

    /// Row `idx` of the composed view when scrolled back `offset` lines:
    /// negative indexes reach into scrollback.
    pub fn view_line(&self, offset: usize, idx: usize) -> Option<&[Cell]> {
        let sb = self.scrollback.len();
        let offset = offset.min(sb);
        let virt = idx as isize - offset as isize;
        if virt >= 0 {
            let y = virt as usize;
            (y < self.rows()).then(|| self.grid.line(y))
        } else {
            let sb_idx = (sb as isize + virt) as usize;
            self.scrollback.get(sb_idx).map(|l| l.as_slice())
        }
    }
}

/// `file://host/path` → decoded path.
fn parse_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path_start = rest.find('/')?;
    let path = &rest[path_start..];
    // Percent-decode.
    let mut out = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Undo VS Code 633;E escaping: `\\` and `\x3b` (`;`) and `\x0a` (newline).
fn decode_633(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if let Ok(b) = u8::from_str_radix(&format!("{hi}{lo}"), 16) {
                        out.push(b as char);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
