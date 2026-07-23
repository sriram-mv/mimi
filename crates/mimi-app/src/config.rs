//! Minimalist config: `~/.config/mimi/config`, `key = value` lines,
//! `#` comments. No config file is required — the defaults are the product.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub font_size: f32,
    pub font_path: Option<PathBuf>,
    pub shell: Option<String>,
    pub scrollback: usize,
    pub padding: f32,
    /// Whether agents on the control socket may inject input (`run`,
    /// `send_text`). Read-only methods are always available.
    pub allow_agent_exec: bool,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    pub palette: [[u8; 3]; 16],
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_size: 14.0,
            font_path: None,
            shell: None,
            scrollback: 10_000,
            padding: 8.0,
            allow_agent_exec: true,
            fg: [0xd6, 0xdb, 0xe1],
            bg: [0x0e, 0x11, 0x16],
            cursor: [0xff, 0xb4, 0x54],
            palette: DEFAULT_PALETTE,
        }
    }
}

/// A restrained dark palette tuned for legibility on the default background.
pub const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0x1c, 0x21, 0x28],   // black
    [0xe8, 0x6c, 0x75],   // red
    [0x8fu8, 0xc8, 0x7f], // green
    [0xe5, 0xc0, 0x7b],   // yellow
    [0x6c, 0xb2, 0xf0],   // blue
    [0xc7, 0x92, 0xea],   // magenta
    [0x66, 0xc7, 0xc4],   // cyan
    [0xd6, 0xdb, 0xe1],   // white
    [0x49, 0x52, 0x5e],   // bright black
    [0xf2, 0x87, 0x79],   // bright red
    [0xa5, 0xd6, 0x96],   // bright green
    [0xf0, 0xd1, 0x97],   // bright yellow
    [0x8a, 0xc4, 0xf2],   // bright blue
    [0xd8, 0xaa, 0xf2],   // bright magenta
    [0x85, 0xd5, 0xd2],   // bright cyan
    [0xf0, 0xf2, 0xf5],   // bright white
];

fn parse_hex(v: &str) -> Option<[u8; 3]> {
    let v = v.trim().trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

impl Config {
    pub fn load() -> Config {
        let mut cfg = Config::default();
        let Some(home) = std::env::var_os("HOME") else {
            return cfg;
        };
        let path = PathBuf::from(home).join(".config/mimi/config");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return cfg;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "font_size" => {
                    if let Ok(v) = value.parse() {
                        cfg.font_size = v;
                    }
                }
                "font" => cfg.font_path = Some(PathBuf::from(value)),
                "shell" => cfg.shell = Some(value.to_string()),
                "scrollback" => {
                    if let Ok(v) = value.parse() {
                        cfg.scrollback = v;
                    }
                }
                "padding" => {
                    if let Ok(v) = value.parse() {
                        cfg.padding = v;
                    }
                }
                "allow_agent_exec" => cfg.allow_agent_exec = value != "false",
                "fg" => {
                    if let Some(c) = parse_hex(value) {
                        cfg.fg = c;
                    }
                }
                "bg" => {
                    if let Some(c) = parse_hex(value) {
                        cfg.bg = c;
                    }
                }
                "cursor" => {
                    if let Some(c) = parse_hex(value) {
                        cfg.cursor = c;
                    }
                }
                k if k.starts_with("palette") => {
                    if let (Ok(i), Some(c)) =
                        (k["palette".len()..].parse::<usize>(), parse_hex(value))
                    {
                        if i < 16 {
                            cfg.palette[i] = c;
                        }
                    }
                }
                _ => {}
            }
        }
        cfg
    }
}
