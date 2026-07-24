<p align="center">
  <img src="assets/logo.svg" width="140" alt="mimi — a plush yellow bunny mascot" />
</p>

<h1 align="center">mimi</h1>

<p align="center">
  <a href="https://github.com/sriram-mv/mimi/actions/workflows/ci.yml"><img src="https://github.com/sriram-mv/mimi/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

A minimalist, GPU-accelerated terminal for macOS with [fish](https://fishshell.com/)
as its default shell and a native control plane for agentic workflows.

Built from first principles: a from-scratch VT/ANSI parser, direct
Metal rendering via wgpu, and no async runtime anywhere in the stack — PTY
I/O, the shell-exit watcher, and agent connections are each one plain OS
thread. Redraws are event-driven, so an idle window costs nothing.

## Why

Most terminals are agent-*tolerant* at best — an agent can pipe text
through them, but has to scrape ANSI-rendered output to know what a
command did. mimi is agent-*native*: every window exposes a private Unix
socket (`$MIMI_SOCKET`) that speaks structured JSON — list the commands
that ran, get one's exact output and exit code, run a new command and wait
for it, subscribe to events — no screen-scraping required. See
[`docs/AGENT_PROTOCOL.md`](docs/AGENT_PROTOCOL.md).

```console
$ mimi ctl run "cargo test"
$ mimi ctl last
$ mimi ctl watch
```

## Install (macOS)

```console
$ git clone <this repo> && cd mimi
$ make dist          # builds dist/Mimi.app for your Mac's architecture
$ make install        # copies it to /Applications
```

fish is not bundled — install it first (`brew install fish`) and mimi will
find it automatically across Homebrew (Apple Silicon or Intel), MacPorts,
and system install locations. Without fish, mimi falls back to `$SHELL`
and prints a notice; shell-integration features (semantic command blocks)
need the fish integration script in `shell/vendor_conf.d/mimi.fish`, which
mimi wires up automatically when it spawns fish.

## Building from source

Requires Rust (stable) and, for packaging, macOS with Xcode command line
tools.

```console
$ make build     # cargo build --workspace
$ make test      # cargo test --workspace
$ make run        # cargo run -p mimi-app
$ make check      # fmt + clippy + test
```

The workspace is four crates:

| Crate | Purpose |
|---|---|
| `mimi-core` | VT/ANSI parser, screen grid, semantic command blocks — no I/O |
| `mimi-pty` | PTY spawning on `libc`, fish-first shell discovery |
| `mimi-agent` | The Unix-socket agent control plane |
| `mimi-app` | winit + wgpu window, input, `mimi` and `mimi ctl` binaries |

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how it fits
together and why it's fast.

## Configuration

Optional, at `~/.config/mimi/config` (`key = value`, `#` comments):

```
font_size = 14
font = /path/to/a/monospace.ttf
shell = /opt/homebrew/bin/fish
scrollback = 10000
padding = 8
allow_agent_exec = true   # false = agents can read the screen but not type
fg = #d6dbe1
bg = #0e1116
cursor = #ffb454
palette0 = #1c2128         # ANSI colors 0-15
```

No config file is required — the defaults are the intended experience.

## Keybindings

| Key | Action |
|---|---|
| `Cmd+C` / `Cmd+V` | Copy selection / paste |
| `Cmd+=` / `Cmd+-` / `Cmd+0` | Zoom in / out / reset |
| `Cmd+K` | Clear scrollback |
| `Cmd+Q` | Quit |
| Mouse drag | Select text |
| Scroll wheel | Scroll view / send wheel events to full-screen apps |
| `Shift+PageUp/Down` | Scroll a full page |

## License

MIT — see [LICENSE](LICENSE).
