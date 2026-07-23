//! Glyph atlas: rasterize with fontdue on demand, shelf-pack into a single
//! R8 texture the glyph shader samples from.

use std::collections::HashMap;
use std::path::PathBuf;

const ATLAS_SIZE: u32 = 2048;
const PAD: u32 = 1;

/// Monospace font candidates, macOS system fonts first.
const FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
    "/Library/Fonts/SF-Mono-Regular.otf",
    // Linux (dev environments).
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
];

pub fn load_font(explicit: Option<&PathBuf>) -> Result<fontdue::Font, String> {
    let mut tried = Vec::new();
    let candidates: Vec<PathBuf> = explicit
        .into_iter()
        .cloned()
        .chain(FONT_CANDIDATES.iter().map(PathBuf::from))
        .collect();
    for path in &candidates {
        let Ok(bytes) = std::fs::read(path) else {
            tried.push(format!("{} (unreadable)", path.display()));
            continue;
        };
        match fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                ..Default::default()
            },
        ) {
            Ok(font) => return Ok(font),
            Err(e) => tried.push(format!("{} ({e})", path.display())),
        }
    }
    Err(format!(
        "no usable monospace font found; set `font = /path/to/font.ttf` in \
         ~/.config/mimi/config. Tried:\n  {}",
        tried.join("\n  ")
    ))
}

#[derive(Clone, Copy)]
pub struct GlyphInfo {
    pub uv_pos: [f32; 2],
    pub uv_size: [f32; 2],
    /// Bitmap size in px.
    pub size: [f32; 2],
    /// Offset from the cell's top-left to the bitmap's top-left.
    pub offset: [f32; 2],
}

pub struct Atlas {
    font: fontdue::Font,
    px: f32,
    map: HashMap<char, Option<GlyphInfo>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub cell_w: f32,
    pub cell_h: f32,
    ascent: f32,
}

fn line_metrics(font: &fontdue::Font, px: f32) -> fontdue::LineMetrics {
    font.horizontal_line_metrics(px)
        .unwrap_or(fontdue::LineMetrics {
            ascent: px * 0.8,
            descent: -(px * 0.2),
            line_gap: 0.0,
            new_line_size: px,
        })
}

fn make_texture(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph-atlas"),
        size: wgpu::Extent3d {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

impl Atlas {
    pub fn new(device: &wgpu::Device, font: fontdue::Font, px: f32) -> Atlas {
        let line = line_metrics(&font, px);
        let cell_h = (line.ascent - line.descent + line.line_gap).ceil();
        let cell_w = font.metrics('M', px).advance_width.round().max(1.0);
        let (texture, view) = make_texture(device);
        Atlas {
            font,
            px,
            map: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            texture,
            view,
            cell_w,
            cell_h,
            ascent: line.ascent.ceil(),
        }
    }

    /// Rebuild in place for a new pixel size (font zoom / DPI change).
    pub fn rebuild(&mut self, device: &wgpu::Device, px: f32) {
        let line = line_metrics(&self.font, px);
        self.px = px;
        self.cell_h = (line.ascent - line.descent + line.line_gap).ceil();
        self.cell_w = self.font.metrics('M', px).advance_width.round().max(1.0);
        self.ascent = line.ascent.ceil();
        self.map.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
        let (texture, view) = make_texture(device);
        self.texture = texture;
        self.view = view;
    }

    pub fn get(&mut self, queue: &wgpu::Queue, ch: char) -> Option<GlyphInfo> {
        if let Some(cached) = self.map.get(&ch) {
            return *cached;
        }
        let info = self.rasterize(queue, ch);
        self.map.insert(ch, info);
        info
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, ch: char) -> Option<GlyphInfo> {
        let (metrics, bitmap) = self.font.rasterize(ch, self.px);
        if metrics.width == 0 || metrics.height == 0 {
            return None;
        }
        let (w, h) = (metrics.width as u32, metrics.height as u32);

        // Shelf packing; wrap to the atlas origin if full (rare in practice:
        // 2048^2 holds thousands of glyphs).
        if self.shelf_x + w + PAD > ATLAS_SIZE {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.shelf_y + h + PAD > ATLAS_SIZE {
            self.map.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_h = 0;
        }
        let (x, y) = (self.shelf_x, self.shelf_y);
        self.shelf_x += w + PAD;
        self.shelf_h = self.shelf_h.max(h);

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &bitmap,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        let s = ATLAS_SIZE as f32;
        Some(GlyphInfo {
            uv_pos: [x as f32 / s, y as f32 / s],
            uv_size: [w as f32 / s, h as f32 / s],
            size: [metrics.width as f32, metrics.height as f32],
            offset: [
                metrics.xmin as f32,
                self.ascent - metrics.height as f32 - metrics.ymin as f32,
            ],
        })
    }
}
