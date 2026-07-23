//! The mimi window: winit event loop + PTY + agent server wiring.
//!
//! Threading model (no async runtime):
//!   - reader thread: PTY -> parser -> shared `Emulator`, then a coalesced
//!     wake to the event loop
//!   - waiter thread: waitpid on the shell, exits the app when it dies
//!   - agent threads: one per control-socket connection (in mimi-agent)
//!   - main thread: winit events, input encoding, GPU frames

mod atlas;
mod renderer;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mimi_agent::AgentServer;
use mimi_core::Emulator;
use mimi_pty::{Pty, PtyWriter, ShellChoice};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::config::Config;
use crate::input::{encode_key, encode_paste};
use crate::palette::Palette;
use renderer::{FrameInput, Renderer, Selection};

const FISH_INTEGRATION: &str = include_str!("../../../../shell/vendor_conf.d/mimi.fish");

#[derive(Debug)]
enum UserEvent {
    Wake,
    ChildExited,
}

pub fn run(config: Config) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("event loop: {e}"))?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, proxy);
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("event loop: {e}"))?;
    if let Some(err) = app.fatal_error.take() {
        return Err(err);
    }
    Ok(())
}

struct App {
    config: Config,
    palette: Palette,
    proxy: EventLoopProxy<UserEvent>,
    fatal_error: Option<String>,

    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    emulator: Option<Arc<Mutex<Emulator>>>,
    pty: Option<Arc<Pty>>,
    writer: Option<PtyWriter>,
    agent: Option<Arc<AgentServer>>,

    mods: ModifiersState,
    focused: bool,
    font_size: f32,
    scale: f64,
    view_offset: usize,
    selection: Option<Selection>,
    selecting: bool,
    mouse_cell: (usize, usize),
    last_title: String,
    clipboard: Option<arboard::Clipboard>,
    wake_pending: Arc<AtomicBool>,
}

impl App {
    fn new(config: Config, proxy: EventLoopProxy<UserEvent>) -> App {
        let palette = Palette::new(&config);
        let font_size = config.font_size;
        App {
            config,
            palette,
            proxy,
            fatal_error: None,
            window: None,
            renderer: None,
            emulator: None,
            pty: None,
            writer: None,
            agent: None,
            mods: ModifiersState::empty(),
            focused: true,
            font_size,
            scale: 1.0,
            view_offset: 0,
            selection: None,
            selecting: false,
            mouse_cell: (0, 0),
            last_title: String::new(),
            clipboard: arboard::Clipboard::new().ok(),
            wake_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, msg: String) {
        eprintln!("mimi: {msg}");
        self.fatal_error = Some(msg);
        event_loop.exit();
    }

    fn setup(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("mimi")
                        .with_inner_size(winit::dpi::LogicalSize::new(920.0, 600.0)),
                )
                .map_err(|e| format!("create window: {e}"))?,
        );
        self.scale = window.scale_factor();

        let font = atlas::load_font(self.config.font_path.as_ref())?;
        let font_px = (self.font_size as f64 * self.scale) as f32;
        let padding = (self.config.padding as f64 * self.scale) as f32;
        let renderer = Renderer::new(window.clone(), font, font_px, padding)?;
        let (cols, rows) = renderer.grid_size();

        let emulator = Arc::new(Mutex::new(Emulator::new(
            cols,
            rows,
            self.config.scrollback,
        )));

        // Shell: config override, else fish-first discovery.
        let shell = match &self.config.shell {
            Some(s) => {
                let p = PathBuf::from(s);
                if p.file_name().is_some_and(|n| n == "fish") {
                    ShellChoice::Fish(p)
                } else {
                    ShellChoice::Fallback(p)
                }
            }
            None => mimi_pty::find_shell(),
        };

        let socket_path = mimi_agent::default_socket_path();
        let mut env = vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM_PROGRAM".to_string(), "mimi".to_string()),
            (
                "TERM_PROGRAM_VERSION".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
            ("MIMI_SOCKET".to_string(), socket_path.display().to_string()),
        ];
        if let Some(share) = ensure_share_dir() {
            let existing = std::env::var("XDG_DATA_DIRS")
                .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
            env.push((
                "XDG_DATA_DIRS".to_string(),
                format!("{}:{existing}", share.display()),
            ));
        }

        let is_fish = shell.is_fish();
        let pty = Arc::new(
            Pty::spawn(shell, cols as u16, rows as u16, &env, None)
                .map_err(|e| format!("spawn shell: {e}"))?,
        );
        let writer = pty.writer();

        // Agent control plane.
        let agent_input: mimi_agent::InputFn = {
            let w = pty.writer();
            Arc::new(move |bytes: &[u8]| {
                let _ = w.clone().write_all(bytes);
            })
        };
        let agent = Arc::new(
            AgentServer::start(
                emulator.clone(),
                agent_input,
                self.config.allow_agent_exec,
                Some(socket_path),
            )
            .map_err(|e| format!("agent socket: {e}"))?,
        );

        // Reader thread: PTY bytes -> emulator -> wake.
        {
            let emulator = emulator.clone();
            let agent = agent.clone();
            let proxy = self.proxy.clone();
            let wake_pending = self.wake_pending.clone();
            let mut resp_writer = pty.writer();
            let reader = pty.reader().map_err(|e| format!("pty reader: {e}"))?;
            std::thread::Builder::new()
                .name("mimi-pty-reader".into())
                .spawn(move || {
                    mimi_pty::read_loop(reader, |chunk| {
                        let (responses, events) = {
                            let mut emu = emulator.lock().unwrap();
                            emu.process(chunk);
                            (
                                std::mem::take(&mut emu.term.responses),
                                emu.term.events.drain(..).collect::<Vec<_>>(),
                            )
                        };
                        if !responses.is_empty() {
                            let _ = resp_writer.write_all(&responses);
                        }
                        for ev in &events {
                            agent.broadcast(&mimi_agent::event_json(ev));
                        }
                        // Coalesce wakes: at most one in flight.
                        if !wake_pending.swap(true, Ordering::AcqRel) {
                            let _ = proxy.send_event(UserEvent::Wake);
                        }
                    });
                })
                .map_err(|e| format!("spawn reader: {e}"))?;
        }

        // Waiter thread: shell exit closes the window.
        {
            let pty = pty.clone();
            let proxy = self.proxy.clone();
            std::thread::Builder::new()
                .name("mimi-child-waiter".into())
                .spawn(move || {
                    pty.wait();
                    let _ = proxy.send_event(UserEvent::ChildExited);
                })
                .map_err(|e| format!("spawn waiter: {e}"))?;
        }

        if !is_fish {
            eprintln!(
                "mimi: fish not found, falling back to {}. \
                 Install fish (`brew install fish`) for the full experience.",
                pty.shell.path().display()
            );
        }

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.emulator = Some(emulator);
        self.writer = Some(writer);
        self.pty = Some(pty);
        self.agent = Some(agent);
        Ok(())
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if let Some(w) = &mut self.writer {
            let _ = w.write_all(bytes);
        }
        // Typing snaps the view back to the live screen.
        if self.view_offset != 0 {
            self.view_offset = 0;
            self.request_redraw();
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn sync_grid_to_surface(&mut self) {
        let Some(renderer) = &self.renderer else {
            return;
        };
        let (cols, rows) = renderer.grid_size();
        if let Some(emu) = &self.emulator {
            emu.lock().unwrap().term.resize(cols, rows);
        }
        if let Some(pty) = &self.pty {
            let _ = pty.resize(cols as u16, rows as u16, 0, 0);
        }
    }

    fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(6.0, 72.0);
        let px = (self.font_size as f64 * self.scale) as f32;
        if let Some(r) = &mut self.renderer {
            r.set_font_px(px);
        }
        self.sync_grid_to_surface();
        self.request_redraw();
    }

    fn handle_cmd_key(&mut self, key: &Key) -> bool {
        let Key::Character(s) = key else {
            return false;
        };
        match s.as_str() {
            "c" => {
                if let Some(sel) = self.selection {
                    let text = self
                        .emulator
                        .as_ref()
                        .map(|emu| selection_text(&emu.lock().unwrap(), sel))
                        .unwrap_or_default();
                    if let Some(cb) = &mut self.clipboard {
                        let _ = cb.set_text(text);
                    }
                }
                true
            }
            "v" => {
                let text = self
                    .clipboard
                    .as_mut()
                    .and_then(|cb| cb.get_text().ok())
                    .unwrap_or_default();
                if !text.is_empty() {
                    let bracketed = self
                        .emulator
                        .as_ref()
                        .is_some_and(|e| e.lock().unwrap().term.bracketed_paste);
                    let bytes = encode_paste(&text, bracketed);
                    self.write_pty(&bytes);
                }
                true
            }
            "=" | "+" => {
                self.set_font_size(self.font_size + 1.0);
                true
            }
            "-" => {
                self.set_font_size(self.font_size - 1.0);
                true
            }
            "0" => {
                self.set_font_size(self.config.font_size);
                true
            }
            "k" => {
                if let Some(emu) = &self.emulator {
                    emu.lock().unwrap().term.scrollback.clear();
                }
                self.view_offset = 0;
                self.request_redraw();
                true
            }
            _ => false,
        }
    }

    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let Some(r) = &self.renderer else {
            return (0, 0);
        };
        let (cols, rows) = self
            .emulator
            .as_ref()
            .map(|e| {
                let t = &e.lock().unwrap().term;
                (t.cols(), t.rows())
            })
            .unwrap_or((80, 24));
        let col = ((x - r.padding as f64) / r.atlas.cell_w as f64)
            .floor()
            .clamp(0.0, cols as f64 - 1.0) as usize;
        let row = ((y - r.padding as f64) / r.atlas.cell_h as f64)
            .floor()
            .clamp(0.0, rows as f64 - 1.0) as usize;
        (row, col)
    }

    fn abs_row(&self, screen_row: usize) -> usize {
        let sb = self
            .emulator
            .as_ref()
            .map(|e| e.lock().unwrap().term.scrollback.len())
            .unwrap_or(0);
        sb - self.view_offset.min(sb) + screen_row
    }

    fn scroll_view(&mut self, lines: isize) {
        let (in_alt, mouse_active, sgr, sb_len) = self
            .emulator
            .as_ref()
            .map(|e| {
                let t = &e.lock().unwrap().term;
                (
                    t.alt_active,
                    t.mouse_mode != 0,
                    t.mouse_sgr,
                    t.scrollback.len(),
                )
            })
            .unwrap_or((false, false, false, 0));

        if mouse_active && sgr {
            // Report the wheel to the app (button 64 up / 65 down).
            let (row, col) = self.mouse_cell;
            let btn = if lines > 0 { 64 } else { 65 };
            let seq = format!("\x1b[<{btn};{};{}M", col + 1, row + 1);
            let n = lines.unsigned_abs();
            for _ in 0..n {
                self.write_pty(seq.as_bytes());
            }
            return;
        }
        if in_alt {
            // Wheel scrolls TUI content via arrow keys.
            let seq: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            for _ in 0..lines.unsigned_abs() * 3 {
                self.write_pty(seq);
            }
            return;
        }
        let new_offset = (self.view_offset as isize + lines).clamp(0, sb_len as isize) as usize;
        if new_offset != self.view_offset {
            self.view_offset = new_offset;
            self.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(e) = self.setup(event_loop) {
                self.fail(event_loop, e);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                self.wake_pending.store(false, Ordering::Release);
                // New output snaps a scrolled view only if we're at the live
                // edge already; otherwise keep the user's place.
                if let (Some(emu), Some(window)) = (&self.emulator, &self.window) {
                    let title = {
                        let t = &emu.lock().unwrap().term;
                        if t.title.is_empty() {
                            match &t.cwd {
                                Some(cwd) => format!("{cwd} — mimi"),
                                None => "mimi".to_string(),
                            }
                        } else {
                            format!("{} — mimi", t.title)
                        }
                    };
                    if title != self.last_title {
                        window.set_title(&title);
                        self.last_title = title;
                    }
                }
                self.request_redraw();
            }
            UserEvent::ChildExited => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(pty) = &self.pty {
                    pty.kill();
                }
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                let (Some(renderer), Some(emulator)) = (&mut self.renderer, &self.emulator) else {
                    return;
                };
                let emu = emulator.lock().unwrap();
                renderer.render(
                    &FrameInput {
                        term: &emu.term,
                        view_offset: self.view_offset,
                        selection: self.selection,
                        focused: self.focused,
                    },
                    &self.palette,
                );
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.sync_grid_to_surface();
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                let px = (self.font_size as f64 * self.scale) as f32;
                if let Some(r) = &mut self.renderer {
                    r.set_font_px(px);
                }
                self.sync_grid_to_surface();
            }
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                let send_focus_events = self
                    .emulator
                    .as_ref()
                    .is_some_and(|e| e.lock().unwrap().term.focus_events);
                if send_focus_events {
                    self.write_pty(if focused { b"\x1b[I" } else { b"\x1b[O" });
                }
                self.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.mods.super_key() {
                    if self.handle_cmd_key(&event.logical_key) {
                        return;
                    }
                    if matches!(&event.logical_key, Key::Character(c) if c.as_str() == "q") {
                        event_loop.exit();
                    }
                    return;
                }
                // PageUp/PageDown with shift scroll the view (classic).
                if self.mods.shift_key() {
                    if let Key::Named(NamedKey::PageUp) = event.logical_key {
                        let rows = self
                            .emulator
                            .as_ref()
                            .map(|e| e.lock().unwrap().term.rows())
                            .unwrap_or(24);
                        self.scroll_view(rows as isize);
                        return;
                    }
                    if let Key::Named(NamedKey::PageDown) = event.logical_key {
                        let rows = self
                            .emulator
                            .as_ref()
                            .map(|e| e.lock().unwrap().term.rows())
                            .unwrap_or(24);
                        self.scroll_view(-(rows as isize));
                        return;
                    }
                }
                let app_cursor = self
                    .emulator
                    .as_ref()
                    .is_some_and(|e| e.lock().unwrap().term.app_cursor);
                if let Some(bytes) = encode_key(&event, self.mods, app_cursor) {
                    self.selection = None;
                    self.write_pty(&bytes);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as isize,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let cell_h = self
                            .renderer
                            .as_ref()
                            .map(|r| r.atlas.cell_h as f64)
                            .unwrap_or(16.0);
                        (pos.y / cell_h).round() as isize
                    }
                };
                if lines != 0 {
                    self.scroll_view(lines);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (row, col) = self.cell_at(position.x, position.y);
                self.mouse_cell = (row, col);
                if self.selecting {
                    let end = (self.abs_row(row), col);
                    if let Some(sel) = &mut self.selection {
                        if sel.end != end {
                            sel.end = end;
                            self.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        let (row, col) = self.mouse_cell;
                        let start = (self.abs_row(row), col);
                        self.selection = Some(Selection { start, end: start });
                        self.selecting = true;
                        self.request_redraw();
                    }
                    ElementState::Released => {
                        self.selecting = false;
                        // A click without drag clears the selection.
                        if let Some(sel) = self.selection {
                            if sel.start == sel.end {
                                self.selection = None;
                                self.request_redraw();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract the selected text (used by cmd+C).
fn selection_text(emu: &Emulator, sel: Selection) -> String {
    let term = &emu.term;
    let s = sel.normalized();
    let sb = term.scrollback.len();
    let mut out = String::new();
    for abs in s.start.0..=s.end.0 {
        let line: Option<&[mimi_core::Cell]> = if abs < sb {
            term.scrollback.get(abs).map(|l| l.as_slice())
        } else {
            let y = abs - sb;
            (y < term.rows()).then(|| term.grid.line(y))
        };
        let Some(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let from = if abs == s.start.0 { s.start.1 } else { 0 };
        let to = if abs == s.end.0 {
            s.end.1.min(line.len() - 1)
        } else {
            line.len() - 1
        };
        let mut row = String::new();
        for cell in &line[from.min(to)..=to] {
            if cell.flags & mimi_core::flags::WIDE_SPACER != 0 {
                continue;
            }
            row.push(cell.ch);
        }
        out.push_str(row.trim_end());
        if abs != s.end.0 {
            out.push('\n');
        }
    }
    out
}

/// Locate (or materialize) the share directory holding the fish integration,
/// so it works from a .app bundle, a unix prefix, or a bare cargo build.
fn ensure_share_dir() -> Option<PathBuf> {
    let script_rel = "fish/vendor_conf.d/mimi.fish";
    if let Some(dir) = std::env::var_os("MIMI_SHARE") {
        let dir = PathBuf::from(dir);
        if dir.join(script_rel).exists() {
            return Some(dir);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for candidate in [
            exe.parent()?.parent()?.join("Resources/share"), // Mimi.app bundle
            exe.parent()?.parent()?.join("share/mimi"),      // unix prefix
        ] {
            if candidate.join(script_rel).exists() {
                return Some(candidate);
            }
        }
    }
    // Bare build: materialize the embedded script under ~/.cache.
    let home = std::env::var_os("HOME")?;
    let share = PathBuf::from(home).join(".cache/mimi/share");
    let script = share.join(script_rel);
    let fresh = std::fs::read_to_string(&script)
        .map(|cur| cur != FISH_INTEGRATION)
        .unwrap_or(true);
    if fresh {
        std::fs::create_dir_all(script.parent()?).ok()?;
        std::fs::write(&script, FISH_INTEGRATION).ok()?;
    }
    Some(share)
}
