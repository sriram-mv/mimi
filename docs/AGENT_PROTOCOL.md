# mimi agent protocol

Every mimi window opens a private Unix domain socket for programmatic
control. This is the native interface for agentic workflows: instead of
scraping rendered text, an agent gets structured command blocks (command
line, cwd, output, exit code) and can drive the terminal directly.

## Connecting

The socket path is exported to every child process as `$MIMI_SOCKET`. It
lives under `$XDG_RUNTIME_DIR/mimi-<uid>/<pid>.sock` (falling back to
`$TMPDIR` or `/tmp`), in a directory created `0700` — only your user can
connect.

```console
$ echo $MIMI_SOCKET
/run/user/501/mimi-501/4213.sock
```

From any language, just connect and speak newline-delimited JSON. The
bundled CLI wraps this for shell scripts and simple agents:

```console
$ mimi ctl screen
$ mimi ctl run "cargo test"
$ mimi ctl blocks 10
$ mimi ctl watch
```

## Wire format

One JSON object per line, both directions.

**Request:**
```json
{"id": 1, "method": "screen", "params": {}}
```

**Response:**
```json
{"id": 1, "result": {...}}
```
or
```json
{"id": 1, "error": "message"}
```

`params` is optional and method-specific. `id` is echoed back verbatim
(any JSON value); use it to match responses to requests if you pipeline
multiple calls on one connection.

## Methods

### `hello`
No params. Returns `{"name": "mimi", "version": "...", "pid": ...}`.
Useful as a liveness/identity check.

### `screen`
No params. Returns the current visible screen:
```json
{
  "cols": 80, "rows": 24,
  "cursor": {"x": 12, "y": 3},
  "alt_screen": false,
  "title": "~/dev/mimi",
  "cwd": "/Users/you/dev/mimi",
  "lines": ["...", "..."]
}
```
`lines` is plain text, one string per row, trailing whitespace trimmed.

### `list_blocks`
Params: `{"limit": 50}` (optional, default 50). Returns an array of
blocks, most recent first, **without** their `output` field (fetch that
via `get_block` to avoid shipping megabytes for a listing):
```json
[{"id": 41, "command": "cargo test", "cwd": "/Users/you/dev/mimi",
  "exit": 0, "running": false, "started_ms": 1234, "ended_ms": 1240,
  "output_truncated": false, "output_bytes": 812}]
```

### `get_block`
Params: `{"id": 41, "output": true}`. `output` defaults to `true` and
controls whether the (potentially large, capped at 1 MiB) `output` field
is included. Errors if the id doesn't exist.

### `last_block`
Params: `{"output": true}`. Same shape as `get_block`, for the most
recently started command. Errors if no command has run yet.

### `run`
Execute a command and (by default) wait for it to finish.

Params:
```json
{"command": "cargo test", "wait": true, "timeout_ms": 30000}
```
- `command` — a single-line shell command (typed into the terminal
  followed by Enter; newlines are rejected — chain with `&&` instead).
- `wait` (default `true`) — if `false`, returns immediately with
  `{"accepted": true}` and you poll `last_block`/`get_block` yourself.
- `timeout_ms` (default 30000) — how long to wait for the shell-integration
  "command finished" marker before giving up.

On success (waited), returns the finished block, output included. Requires
shell integration (see below) to know when the command finished; without
it, `run` with `wait: true` will time out — use `wait: false` and poll, or
use `send_text` and read `screen` yourself.

Blocked with an error if the window was started with
`allow_agent_exec = false` in the config.

### `send_text`
Params: `{"text": "ls\n"}`. Types raw text into the terminal — no command
line parsing, no waiting, no newline added automatically. Use this for
interactive input (answering an `[y/N]` prompt, sending a signal via
`\x03`, etc). Same `allow_agent_exec` gating as `run`.

### `subscribe`
No params. Acknowledges with `{"subscribed": true}`, then that connection
starts receiving one JSON object per line for each event, interleaved with
any further request/response traffic on the same connection:
```json
{"event": "block_finished", "id": 41}
{"event": "title", "title": "~/dev/mimi"}
{"event": "cwd", "cwd": "/Users/you/dev/mimi"}
{"event": "bell"}
{"event": "block_started", "id": 42}
```
Open a dedicated connection for `subscribe` if you also want to make
blocking `run` calls concurrently — one socket, one in-flight request/response
pair at a time, though events can interleave with sync responses on that
same connection.

## Shell integration

Command blocks depend on the shell reporting boundaries via OSC 133/633,
implemented for fish in `shell/vendor_conf.d/mimi.fish` (auto-loaded when
mimi spawns fish — see `TERM_PROGRAM=mimi` guard in that file). Without
it, `screen` and `send_text` still work; `run`, `list_blocks`, and
`get_block` will see no blocks or will time out waiting for one.

## Security notes

- The socket directory is `0700` — only your user account can open it.
- `run` and `send_text` inject keystrokes into a real shell; treat access
  to the socket as equivalent to shell access. Set `allow_agent_exec =
  false` in `~/.config/mimi/config` to make a window read-only to agents
  (screen/blocks/subscribe still work, input methods return an error).
- There is no network exposure: the socket is Unix-domain, local-only, by
  construction.
