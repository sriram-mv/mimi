//! The agent control plane: mimi's native interface for agentic workflows.
//!
//! Every mimi window listens on a private Unix socket (path exported to the
//! child shell as `$MIMI_SOCKET`). The protocol is newline-delimited JSON —
//! trivially speakable from any language, shell script, or MCP server, with
//! no async runtime on either side.
//!
//! Requests:  `{"id": 1, "method": "screen"}`
//! Responses: `{"id": 1, "result": {...}}` or `{"id": 1, "error": "..."}`
//! Events (after `subscribe`): `{"event": "block_finished", ...}`
//!
//! See `docs/AGENT_PROTOCOL.md` for the full method reference.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mimi_core::Emulator;
use serde::Deserialize;
use serde_json::{json, Value};

/// Callback used to feed agent-originated input into the PTY.
pub type InputFn = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub struct AgentServer {
    pub socket_path: PathBuf,
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
}

/// Private per-user runtime dir for sockets (0700).
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let uid = unsafe { libc::getuid() };
    let dir = base.join(format!("mimi-{uid}"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir.join(format!("{}.sock", std::process::id()))
}

impl AgentServer {
    /// Bind the socket and start the accept loop. `input` receives bytes the
    /// agent asks to type into the terminal; gated by `allow_input`.
    pub fn start(
        emulator: Arc<Mutex<Emulator>>,
        input: InputFn,
        allow_input: bool,
        socket_path: Option<PathBuf>,
    ) -> std::io::Result<AgentServer> {
        let path = socket_path.unwrap_or_else(default_socket_path);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let subscribers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        {
            let subscribers = subscribers.clone();
            std::thread::Builder::new()
                .name("mimi-agent-accept".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        let Ok(stream) = stream else { continue };
                        let emulator = emulator.clone();
                        let input = input.clone();
                        let subscribers = subscribers.clone();
                        std::thread::Builder::new()
                            .name("mimi-agent-conn".into())
                            .spawn(move || {
                                handle_conn(stream, emulator, input, allow_input, subscribers)
                            })
                            .ok();
                    }
                })?;
        }

        Ok(AgentServer {
            socket_path: path,
            subscribers,
        })
    }

    /// Push an event line to all subscribers (dead ones are pruned).
    pub fn broadcast(&self, event: &Value) {
        let line = format!("{event}\n");
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain_mut(|s| s.write_all(line.as_bytes()).is_ok());
    }

    pub fn shutdown(&self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_conn(
    stream: UnixStream,
    emulator: Arc<Mutex<Emulator>>,
    input: InputFn,
    allow_input: bool,
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let id = req.id.clone().unwrap_or(Value::Null);
                match dispatch(&req, &emulator, &input, allow_input, &writer, &subscribers) {
                    Ok(result) => json!({ "id": id, "result": result }),
                    Err(msg) => json!({ "id": id, "error": msg }),
                }
            }
            Err(e) => json!({ "id": null, "error": format!("bad request: {e}") }),
        };
        if writer.write_all(format!("{reply}\n").as_bytes()).is_err() {
            break;
        }
    }
}

fn dispatch(
    req: &Request,
    emulator: &Arc<Mutex<Emulator>>,
    input: &InputFn,
    allow_input: bool,
    stream: &UnixStream,
    subscribers: &Arc<Mutex<Vec<UnixStream>>>,
) -> Result<Value, String> {
    match req.method.as_str() {
        "hello" => Ok(json!({
            "name": "mimi",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        })),

        "screen" => {
            let emu = emulator.lock().unwrap();
            let t = &emu.term;
            Ok(json!({
                "cols": t.cols(),
                "rows": t.rows(),
                "cursor": { "x": t.cursor_x, "y": t.cursor_y },
                "alt_screen": t.alt_active,
                "title": t.title,
                "cwd": t.cwd,
                "lines": t.screen_text(),
            }))
        }

        "list_blocks" => {
            let limit = req.params["limit"].as_u64().unwrap_or(50) as usize;
            let emu = emulator.lock().unwrap();
            let blocks: Vec<Value> = emu
                .term
                .blocks
                .iter()
                .rev()
                .take(limit)
                .map(|b| block_json(b, false))
                .collect();
            Ok(json!(blocks))
        }

        "get_block" => {
            let id = req.params["id"]
                .as_u64()
                .ok_or_else(|| "missing param: id".to_string())?;
            let with_output = req.params["output"].as_bool().unwrap_or(true);
            let emu = emulator.lock().unwrap();
            emu.term
                .blocks
                .get(id)
                .map(|b| block_json(b, with_output))
                .ok_or_else(|| format!("no block with id {id}"))
        }

        "last_block" => {
            let with_output = req.params["output"].as_bool().unwrap_or(true);
            let emu = emulator.lock().unwrap();
            emu.term
                .blocks
                .last()
                .map(|b| block_json(b, with_output))
                .ok_or_else(|| "no blocks yet".to_string())
        }

        "send_text" => {
            if !allow_input {
                return Err("agent input disabled (allow_agent_exec=false)".into());
            }
            let text = req.params["text"]
                .as_str()
                .ok_or_else(|| "missing param: text".to_string())?;
            input(text.as_bytes());
            Ok(json!({ "sent": text.len() }))
        }

        "run" => {
            if !allow_input {
                return Err("agent input disabled (allow_agent_exec=false)".into());
            }
            let command = req.params["command"]
                .as_str()
                .ok_or_else(|| "missing param: command".to_string())?;
            if command.contains('\n') {
                return Err("command must be a single line".into());
            }
            let wait = req.params["wait"].as_bool().unwrap_or(true);
            let timeout_ms = req.params["timeout_ms"].as_u64().unwrap_or(30_000);

            let watermark = emulator.lock().unwrap().term.blocks.next_id();
            input(format!("{command}\n").as_bytes());
            if !wait {
                return Ok(json!({ "accepted": true }));
            }

            // Wait for a block at/after the watermark to finish. Requires
            // shell integration; without it we time out with a hint.
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            loop {
                let done = {
                    let emu = emulator.lock().unwrap();
                    let found = emu
                        .term
                        .blocks
                        .iter()
                        .rev()
                        .find(|b| b.id >= watermark && !b.running)
                        .map(|b| block_json(b, true));
                    found
                };
                if let Some(block) = done {
                    return Ok(block);
                }
                if Instant::now() >= deadline {
                    return Err(
                        "timed out waiting for command to finish (is shell integration \
                         active? try wait:false)"
                            .into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        "subscribe" => {
            let clone = stream.try_clone().map_err(|e| e.to_string())?;
            subscribers.lock().unwrap().push(clone);
            Ok(json!({ "subscribed": true }))
        }

        other => Err(format!("unknown method: {other}")),
    }
}

fn block_json(b: &mimi_core::blocks::Block, with_output: bool) -> Value {
    let mut v = json!({
        "id": b.id,
        "command": b.command,
        "cwd": b.cwd,
        "exit": b.exit,
        "running": b.running,
        "started_ms": b.started_ms,
        "ended_ms": b.ended_ms,
        "output_truncated": b.output_truncated,
        "output_bytes": b.output.len(),
    });
    if with_output {
        v["output"] = json!(b.output);
    }
    v
}

/// Serialize a TermEvent for broadcast.
pub fn event_json(ev: &mimi_core::TermEvent) -> Value {
    use mimi_core::TermEvent::*;
    match ev {
        Title(t) => json!({ "event": "title", "title": t }),
        Bell => json!({ "event": "bell" }),
        CwdChanged(p) => json!({ "event": "cwd", "cwd": p }),
        BlockStarted(id) => json!({ "event": "block_started", "id": id }),
        BlockFinished(id) => json!({ "event": "block_finished", "id": id }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    fn test_setup() -> (
        Arc<Mutex<Emulator>>,
        Arc<Mutex<Vec<u8>>>,
        AgentServer,
        PathBuf,
    ) {
        let emu = Arc::new(Mutex::new(Emulator::new(40, 10, 100)));
        let typed: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let typed2 = typed.clone();
        let input: InputFn = Arc::new(move |b: &[u8]| typed2.lock().unwrap().extend_from_slice(b));
        let path = std::env::temp_dir().join(format!(
            "mimi-test-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let server =
            AgentServer::start(emu.clone(), input, true, Some(path.clone())).expect("start server");
        (emu, typed, server, path)
    }

    fn call(path: &PathBuf, req: &str) -> Value {
        let mut conn = UnixStream::connect(path).expect("connect");
        conn.write_all(format!("{req}\n").as_bytes()).unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn hello_and_screen() {
        let (emu, _typed, _server, path) = test_setup();
        emu.lock().unwrap().process(b"hi agent");

        let v = call(&path, r#"{"id":1,"method":"hello"}"#);
        assert_eq!(v["result"]["name"], "mimi");

        let v = call(&path, r#"{"id":2,"method":"screen"}"#);
        assert_eq!(v["result"]["lines"][0], "hi agent");
        assert_eq!(v["result"]["cols"], 40);
    }

    #[test]
    fn blocks_over_socket() {
        let (emu, _typed, _server, path) = test_setup();
        emu.lock()
            .unwrap()
            .process(b"\x1b]633;E;make test\x07\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07");

        let v = call(&path, r#"{"id":1,"method":"list_blocks"}"#);
        assert_eq!(v["result"][0]["command"], "make test");
        assert_eq!(v["result"][0]["exit"], 0);

        let v = call(&path, r#"{"id":2,"method":"get_block","params":{"id":0}}"#);
        assert_eq!(v["result"]["output"], "ok\n");
    }

    #[test]
    fn send_text_reaches_input() {
        let (_emu, typed, _server, path) = test_setup();
        let v = call(
            &path,
            r#"{"id":1,"method":"send_text","params":{"text":"ls\n"}}"#,
        );
        assert_eq!(v["result"]["sent"], 3);
        assert_eq!(typed.lock().unwrap().as_slice(), b"ls\n");
    }

    #[test]
    fn run_waits_for_block() {
        let (emu, typed, _server, path) = test_setup();

        // Simulate the shell: once the command is typed, produce a block.
        {
            let emu = emu.clone();
            let typed = typed.clone();
            std::thread::spawn(move || loop {
                if !typed.lock().unwrap().is_empty() {
                    emu.lock()
                        .unwrap()
                        .process(b"\x1b]633;E;echo hey\x07\x1b]133;C\x07hey\r\n\x1b]133;D;0\x07");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            });
        }

        let v = call(
            &path,
            r#"{"id":1,"method":"run","params":{"command":"echo hey","timeout_ms":5000}}"#,
        );
        assert_eq!(v["result"]["exit"], 0, "response: {v}");
        assert_eq!(v["result"]["output"], "hey\n");
    }

    #[test]
    fn input_gating() {
        let emu = Arc::new(Mutex::new(Emulator::new(10, 5, 10)));
        let input: InputFn = Arc::new(|_| panic!("input must not be called"));
        let path = std::env::temp_dir().join(format!("mimi-gate-{}.sock", std::process::id()));
        let _server = AgentServer::start(emu, input, false, Some(path.clone())).unwrap();
        let v = call(
            &path,
            r#"{"id":1,"method":"run","params":{"command":"rm -rf /"}}"#,
        );
        assert!(v["error"].as_str().unwrap().contains("disabled"));
    }

    #[test]
    fn subscribe_receives_broadcast() {
        let (_emu, _typed, server, path) = test_setup();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"id\":1,\"method\":\"subscribe\"}\n")
            .unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // ack

        server.broadcast(&event_json(&mimi_core::TermEvent::BlockFinished(7)));
        line.clear();
        reader.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "block_finished");
        assert_eq!(v["id"], 7);
    }
}
