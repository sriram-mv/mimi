//! A single character cell: glyph, colors, attribute flags.

/// Attribute flags packed into a u16. Hand-rolled to keep the core
/// dependency-free.
pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const INVERSE: u16 = 1 << 3;
    pub const DIM: u16 = 1 << 4;
    pub const STRIKETHROUGH: u16 = 1 << 5;
    /// Leader cell of a double-width glyph (CJK, emoji).
    pub const WIDE: u16 = 1 << 6;
    /// The invisible cell right of a WIDE leader.
    pub const WIDE_SPACER: u16 = 1 << 7;
    pub const HIDDEN: u16 = 1 << 8;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    /// Use the theme's default fg/bg.
    Default,
    /// One of the 256 indexed colors (0-15 themed, rest computed).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        }
    }
}

impl Cell {
    /// A blank cell that keeps the current background (used when clearing).
    pub fn blank_with_bg(bg: Color) -> Self {
        Cell {
            bg,
            ..Cell::default()
        }
    }
}
