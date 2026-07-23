# mimi architecture

mimi is built from first principles as four small crates, each doing one
job well, wired together with plain threads — no async runtime anywhere in
the stack.

```
┌─────────────────────────────────────────────────────────────┐
│ mimi-app (winit + wgpu)                                     │
│   main thread: window events, input encoding, GPU frames    │
│   reader thread: PTY bytes -> mimi-core -> redraw wake       │
│   waiter thread: waitpid on the shell                        │
│                                                               │
│   ┌───────────┐   ┌───────────┐   ┌────────────────────┐    │
│   │ mimi-core │   │ mimi-pty  │   │ mimi-agent          │    │
│   │ VT parser │   │ PTY spawn │   │ Unix-socket control │    │
│   │ grid/cell │   │ fish disc.│   │ plane (NDJSON)      │    │
│   │ blocks    │   │ resize    │   │ one thread/conn      │    │
│   └───────────┘   └───────────┘   └────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## mimi-core

The emulation engine, with no I/O and no GUI dependency — it's a pure
function from bytes to screen state, which is what makes it fast and easy
to test exhaustively (see `crates/mimi-core/tests/emulator.rs`).

- **`parser.rs`** — a byte-at-a-time state machine modeled on the classic
  VT500 parser diagram (Ground / Escape / CSI / OSC / DCS-string states),
  written directly against the bytes rather than through a parsing
  framework. UTF-8 is decoded incrementally so a multi-byte sequence split
  across two PTY reads still round-trips correctly.
- **`grid.rs`** — the visible screen: a dense `rows x cols` array of
  `Cell`s. Scroll operations are `Vec::remove`/`insert` on the line
  vector — O(rows) worst case, which is irrelevant at terminal sizes and
  keeps the code simple and cache-friendly. Scrolled-off lines feed
  `Term::scrollback`, a `VecDeque` capped at the configured size.
- **`cell.rs`** — a `Cell` is a `char` + fg/bg `Color` + a `u16` flag
  bitset (bold/italic/underline/inverse/dim/strikethrough/wide/hidden).
  Deliberately not generic or extensible beyond what real terminal output
  needs.
- **`term.rs`** — cursor, SGR pen state, scroll regions, alt screen,
  DEC private modes, OSC dispatch (title, cwd, semantic prompts). This is
  where escape sequences turn into screen mutations.
- **`blocks.rs`** — the semantic layer. OSC 133/633 markers (emitted by
  `shell/vendor_conf.d/mimi.fish`) delimit each command's start/end; mimi
  captures the command line, cwd, output, and exit code into a `Block`.
  This is the data structure agents actually want, instead of scraping
  rendered text.

## mimi-pty

PTY spawning built directly on `posix_openpt`/`fork`/`execvp` via `libc`
— no PTY crate, no tokio. `find_shell()` implements mimi's fish-first
philosophy: it checks the Homebrew (Apple Silicon and Intel), MacPorts,
and system install paths before falling back to `$PATH` lookup and then
`$SHELL`, so a fresh mimi install finds fish immediately.

## mimi-agent

The control plane that makes mimi agent-native rather than agent-tolerant.
Every window binds a private Unix socket (`$MIMI_SOCKET` in the child's
environment, under a `0700` per-user runtime directory) and speaks
newline-delimited JSON: request in, response out, plus an opt-in event
stream via `subscribe`. See `docs/AGENT_PROTOCOL.md` for the method
reference. One thread per connection; the shared `Emulator` is behind a
single `Mutex` since contention is negligible at terminal I/O rates.

## mimi-app

- **`gui/renderer.rs`** — a wgpu renderer (Metal on macOS) doing exactly
  two instanced draw calls per frame: one for cell backgrounds/underlines,
  one for glyphs sampled from an R8 atlas. No shaping, no ligatures, no
  layout engine — a terminal grid is already laid out.
  - **`gui/atlas.rs`** — lazy glyph rasterization via `fontdue`, shelf-packed
  into a 2048x2048 texture. Glyphs are rasterized once and reused for the
  life of the atlas.
- **`gui/mod.rs`** — the winit `ApplicationHandler`. Redraws are
  event-driven: the PTY reader thread posts a single coalesced wake per
  batch of output (an atomic flag prevents wake pile-up), so an idle
  terminal costs nothing and a `yes` loop doesn't queue thousands of
  redundant paints.
- **`input.rs`** — xterm-compatible key encoding (arrow keys, function
  keys, modified keys as `CSI 1;<mod><letter>`, app-cursor mode, ctrl+letter
  to C0).
- **`ctl.rs`** — the `mimi ctl` subcommand: a thin CLI wrapper around the
  agent protocol, usable by any agent or shell script without linking
  against mimi at all.

## Why this shape is fast

- **No async runtime.** Reading the PTY, waiting on the child, and serving
  agent connections are each one blocking OS thread. Fewer moving parts,
  no executor scheduling overhead, no `Future` allocation churn.
- **No allocation on the hot path.** Printing a character writes directly
  into the `Grid`'s existing `Vec<Cell>`; no per-glyph heap traffic.
- **Event-driven rendering.** GPU work happens only when the screen or
  cursor actually changes, coalesced to one frame per PTY read batch.
- **Direct GPU access.** wgpu targets Metal on macOS directly — no
  intermediate compositing layer, no CPU glyph blitting.
