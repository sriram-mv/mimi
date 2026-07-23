//! wgpu renderer: two instanced draw calls per frame (backgrounds, glyphs).
//! On macOS this runs on Metal. Redraws are event-driven — mimi burns zero
//! GPU/CPU when nothing changes.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use mimi_core::{flags, Term};
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::atlas::Atlas;
use crate::palette::Palette;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BgInstance {
    pos: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    pos: [f32; 2],
    size: [f32; 2],
    uv_pos: [f32; 2],
    uv_size: [f32; 2],
    color: [f32; 4],
}

/// Selection in absolute line coordinates (scrollback index space),
/// normalized so start <= end.
#[derive(Clone, Copy, PartialEq)]
pub struct Selection {
    pub start: (usize, usize), // (abs_row, col)
    pub end: (usize, usize),
}

impl Selection {
    pub fn normalized(&self) -> Selection {
        if self.start <= self.end {
            *self
        } else {
            Selection {
                start: self.end,
                end: self.start,
            }
        }
    }

    fn contains(&self, abs_row: usize, col: usize) -> bool {
        let s = self.normalized();
        if abs_row < s.start.0 || abs_row > s.end.0 {
            return false;
        }
        let after_start = abs_row > s.start.0 || col >= s.start.1;
        let before_end = abs_row < s.end.0 || col <= s.end.1;
        after_start && before_end
    }
}

pub struct FrameInput<'a> {
    pub term: &'a Term,
    pub view_offset: usize,
    pub selection: Option<Selection>,
    pub focused: bool,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    bg_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    atlas_layout: wgpu::BindGroupLayout,
    atlas_bind: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    pub atlas: Atlas,
    bg_buf: wgpu::Buffer,
    bg_capacity: usize,
    glyph_buf: wgpu::Buffer,
    glyph_capacity: usize,
    pub padding: f32,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        font: fontdue::Font,
        font_px: f32,
        padding: f32,
    ) -> Result<Renderer, String> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .map_err(|e| format!("create surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or("no GPU adapter found")?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("mimi"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| format!("request device: {e}"))?;

        let mut surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface not supported by adapter")?;
        surface_config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals"),
            contents: bytemuck::bytes_of(&Globals {
                screen: [size.width as f32, size.height as f32],
                _pad: [0.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals-bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let atlas = Atlas::new(&device, font, font_px);
        let atlas_bind = Self::make_atlas_bind(&device, &atlas_layout, &atlas, &sampler);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mimi-pipelines"),
            bind_group_layouts: &[&globals_layout, &atlas_layout],
            push_constant_ranges: &[],
        });

        let bg_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BgInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
        };
        let glyph_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GlyphInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4
            ],
        };

        let make_pipeline = |vs: &str, fs: &str, buf: wgpu::VertexBufferLayout, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers: &[buf],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_config.format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let bg_pipeline = make_pipeline("vs_bg", "fs_bg", bg_layout, wgpu::BlendState::REPLACE);
        let glyph_pipeline = make_pipeline(
            "vs_glyph",
            "fs_glyph",
            glyph_layout,
            wgpu::BlendState::ALPHA_BLENDING,
        );

        let bg_buf = Self::make_instance_buf(&device, "bg-instances", 4096 * 32);
        let glyph_buf = Self::make_instance_buf(&device, "glyph-instances", 4096 * 48);

        Ok(Renderer {
            surface,
            device,
            queue,
            surface_config,
            bg_pipeline,
            glyph_pipeline,
            globals_buf,
            globals_bind,
            atlas_layout,
            atlas_bind,
            sampler,
            atlas,
            bg_buf,
            bg_capacity: 4096,
            glyph_buf,
            glyph_capacity: 4096,
            padding,
        })
    }

    fn make_instance_buf(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn make_atlas_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas: &Atlas,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bind"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width.max(1);
        self.surface_config.height = height.max(1);
        self.surface.configure(&self.device, &self.surface_config);
        self.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck::bytes_of(&Globals {
                screen: [width as f32, height as f32],
                _pad: [0.0; 2],
            }),
        );
    }

    /// Rebuild the atlas at a new pixel size (zoom / DPI change).
    pub fn set_font_px(&mut self, px: f32) {
        self.atlas.rebuild(&self.device, px);
        self.atlas_bind =
            Self::make_atlas_bind(&self.device, &self.atlas_layout, &self.atlas, &self.sampler);
    }

    /// Grid dimensions that fit the current surface.
    pub fn grid_size(&self) -> (usize, usize) {
        let w = self.surface_config.width as f32 - self.padding * 2.0;
        let h = self.surface_config.height as f32 - self.padding * 2.0;
        let cols = (w / self.atlas.cell_w).floor().max(2.0) as usize;
        let rows = (h / self.atlas.cell_h).floor().max(1.0) as usize;
        (cols, rows)
    }

    pub fn render(&mut self, frame: &FrameInput, palette: &Palette) {
        let (cell_w, cell_h) = (self.atlas.cell_w, self.atlas.cell_h);
        let term = frame.term;
        let (cols, rows) = (term.cols(), term.rows());
        let sb_len = term.scrollback.len();
        let view_offset = frame.view_offset.min(sb_len);
        let view_top_abs = sb_len - view_offset;

        let mut bg_instances: Vec<BgInstance> = Vec::with_capacity(cols * rows / 4);
        let mut glyph_instances: Vec<GlyphInstance> = Vec::with_capacity(cols * rows);

        let cursor_visible = term.show_cursor;
        let cursor_screen_row = term.cursor_y + view_offset;

        for row in 0..rows {
            let Some(line) = term.view_line(view_offset, row) else {
                continue;
            };
            let abs_row = view_top_abs + row;
            let y = self.padding + row as f32 * cell_h;
            for (col, cell) in line.iter().enumerate().take(cols) {
                if cell.flags & flags::WIDE_SPACER != 0 {
                    continue;
                }
                let selected = frame.selection.is_some_and(|s| s.contains(abs_row, col));
                let inverse = (cell.flags & flags::INVERSE != 0) != selected;
                let (mut fg, mut bg) = (
                    palette.resolve_fg(cell.fg, cell.flags),
                    palette.resolve_bg(cell.bg),
                );
                if inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }

                let is_cursor = cursor_visible
                    && view_offset == 0
                    && row == cursor_screen_row
                    && col == term.cursor_x;
                if is_cursor && frame.focused {
                    bg = palette.cursor;
                    fg = palette.resolve_bg(mimi_core::Color::Default);
                }

                let x = self.padding + col as f32 * cell_w;
                let width_cells = if cell.flags & flags::WIDE != 0 {
                    2.0
                } else {
                    1.0
                };

                if bg != palette.bg {
                    bg_instances.push(BgInstance {
                        pos: [x, y],
                        size: [cell_w * width_cells, cell_h],
                        color: bg,
                    });
                } else if is_cursor && !frame.focused {
                    // Unfocused: hollow-ish cursor via underline bar.
                    bg_instances.push(BgInstance {
                        pos: [x, y + cell_h - 2.0],
                        size: [cell_w, 2.0],
                        color: palette.cursor,
                    });
                }

                if cell.flags & flags::HIDDEN != 0 {
                    continue;
                }
                if cell.ch != ' ' {
                    if let Some(glyph) = self.atlas.get(&self.queue, cell.ch) {
                        glyph_instances.push(GlyphInstance {
                            pos: [x + glyph.offset[0], y + glyph.offset[1]],
                            size: glyph.size,
                            uv_pos: glyph.uv_pos,
                            uv_size: glyph.uv_size,
                            color: fg,
                        });
                    }
                }
                if cell.flags & (flags::UNDERLINE | flags::STRIKETHROUGH) != 0 {
                    let line_y = if cell.flags & flags::UNDERLINE != 0 {
                        y + cell_h - 2.0
                    } else {
                        y + cell_h * 0.5
                    };
                    bg_instances.push(BgInstance {
                        pos: [x, line_y],
                        size: [cell_w * width_cells, 1.0],
                        color: fg,
                    });
                }
            }
        }

        self.upload_and_draw(&bg_instances, &glyph_instances, palette);
    }

    fn upload_and_draw(
        &mut self,
        bg_instances: &[BgInstance],
        glyph_instances: &[GlyphInstance],
        palette: &Palette,
    ) {
        if bg_instances.len() > self.bg_capacity {
            self.bg_capacity = bg_instances.len().next_power_of_two();
            self.bg_buf = Self::make_instance_buf(
                &self.device,
                "bg-instances",
                (self.bg_capacity * std::mem::size_of::<BgInstance>()) as u64,
            );
        }
        if glyph_instances.len() > self.glyph_capacity {
            self.glyph_capacity = glyph_instances.len().next_power_of_two();
            self.glyph_buf = Self::make_instance_buf(
                &self.device,
                "glyph-instances",
                (self.glyph_capacity * std::mem::size_of::<GlyphInstance>()) as u64,
            );
        }
        if !bg_instances.is_empty() {
            self.queue
                .write_buffer(&self.bg_buf, 0, bytemuck::cast_slice(bg_instances));
        }
        if !glyph_instances.is_empty() {
            self.queue
                .write_buffer(&self.glyph_buf, 0, bytemuck::cast_slice(glyph_instances));
        }

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.surface_config);
                match self.surface.get_current_texture() {
                    Ok(f) => f,
                    Err(_) => return,
                }
            }
            Err(_) => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let bg = palette.bg;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);
            pass.set_bind_group(1, &self.atlas_bind, &[]);
            if !bg_instances.is_empty() {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_vertex_buffer(0, self.bg_buf.slice(..));
                pass.draw(0..6, 0..bg_instances.len() as u32);
            }
            if !glyph_instances.is_empty() {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_vertex_buffer(0, self.glyph_buf.slice(..));
                pass.draw(0..6, 0..glyph_instances.len() as u32);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }
}
