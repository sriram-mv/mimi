//! Color resolution: terminal `Color` -> linear-ish sRGB floats for the GPU.

use mimi_core::{flags, Color};

use crate::config::Config;

pub struct Palette {
    pub fg: [f32; 4],
    pub bg: [f32; 4],
    pub cursor: [f32; 4],
    colors: [[f32; 4]; 256],
}

/// Convert an sRGB byte triplet to linear float for the GPU.
/// The surface is Bgra8UnormSrgb — the hardware applies the sRGB
/// transfer on output, so we must feed it linear values.
fn f(c: [u8; 3]) -> [f32; 4] {
    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        1.0,
    ]
}

#[inline]
fn srgb_to_linear(v: u8) -> f32 {
    let s = v as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

impl Palette {
    pub fn new(cfg: &Config) -> Palette {
        let mut colors = [[0.0; 4]; 256];
        for (i, entry) in cfg.palette.iter().enumerate() {
            colors[i] = f(*entry);
        }
        // 6x6x6 color cube (16..232).
        for i in 0..216 {
            let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
            let level = |v: usize| if v == 0 { 0u8 } else { (55 + v * 40) as u8 };
            colors[16 + i] = f([level(r), level(g), level(b)]);
        }
        // Grayscale ramp (232..256).
        for i in 0..24 {
            let v = (8 + i * 10) as u8;
            colors[232 + i] = f([v, v, v]);
        }
        Palette {
            fg: f(cfg.fg),
            bg: f(cfg.bg),
            cursor: f(cfg.cursor),
            colors,
        }
    }

    pub fn resolve_fg(&self, cell_fg: Color, cell_flags: u16) -> [f32; 4] {
        let bold = cell_flags & flags::BOLD != 0;
        let mut c = match cell_fg {
            Color::Default => self.fg,
            // Bold + base ANSI color brightens, the classic behavior.
            Color::Indexed(i) if bold && i < 8 => self.colors[i as usize + 8],
            Color::Indexed(i) => self.colors[i as usize],
            Color::Rgb(r, g, b) => f([r, g, b]),
        };
        if cell_flags & flags::DIM != 0 {
            for ch in &mut c[..3] {
                *ch *= 0.55;
            }
        }
        c
    }

    pub fn resolve_bg(&self, cell_bg: Color) -> [f32; 4] {
        match cell_bg {
            Color::Default => self.bg,
            Color::Indexed(i) => self.colors[i as usize],
            Color::Rgb(r, g, b) => f([r, g, b]),
        }
    }
}
