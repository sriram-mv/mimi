//! `mimi ctl` — the agent-facing CLI.
//!
//! Runs *inside* a mimi window (or anywhere with `$MIMI_SOCKET` set) and
//! speaks the control protocol. This is the zero-dependency hook for agentic
//! tools: `mimi ctl run "cargo test"` returns JSON with the command's
//! output and exit code.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use serde_json::{json, Value};

const USAGE: &str = "\
mimi ctl — control a running mimi window over its agent socket

USAGE:
    mimi ctl [--socket PATH] <command> [args]

COMMANDS:
    screen                  Dump the visible screen as JSON
    blocks [N]              List the last N command blocks (default 20)
    last                    Show the most recent block, with output
    run <command>           Run a command in the terminal, wait, print result
    send <text>             Type raw text into the terminal (no newline added)
    watch                   Subscribe and stream events as NDJSON
    raw <method> [json]     Call any protocol method with raw JSON params

The socket is taken from --socket or $MIMI_SOCKET (set inside every mimi
window). Protocol reference: docs/AGENT_PROTOCOL.md";

pub fn main(args: &[String]) -> i32 {
    let mut args = args.to_vec();
    let mut socket = std::env::var("MIMI_SOCKET").ok();
    if args.first().map(String::as_str) == Some("--socket") {
        if args.len() < 2 {
            eprintln!("--socket requires a path");
            return 2;
        }
        socket = Some(args[1].clone());
        args.drain(..2);
    }
    let Some(cmd) = args.first().cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return 0;
    }
    let Some(socket) = socket else {
        eprintln!("mimi ctl: no socket. Run inside a mimi window or pass --socket.");
        return 1;
    };

    let (method, params) = match cmd.as_str() {
        "screen" => ("screen".to_string(), Value::Null),
        "blocks" => {
            let limit = args
                .get(1)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(20);
            ("list_blocks".into(), json!({ "limit": limit }))
        }
        "last" => ("last_block".into(), Value::Null),
        "run" => {
            let command = args[1..].join(" ");
            if command.is_empty() {
                eprintln!("usage: mimi ctl run <command>");
                return 2;
            }
            ("run".into(), json!({ "command": command }))
        }
        "send" => {
            let text = args[1..].join(" ");
            ("send_text".into(), json!({ "text": text }))
        }
        "watch" => ("subscribe".into(), Value::Null),
        "raw" => {
            let Some(method) = args.get(1).cloned() else {
                eprintln!("usage: mimi ctl raw <method> [json-params]");
                return 2;
            };
            let params = match args.get(2) {
                Some(p) => match serde_json::from_str(p) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("invalid params json: {e}");
                        return 2;
                    }
                },
                None => Value::Null,
            };
            (method, params)
        }
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            return 2;
        }
    };

    let stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mimi ctl: cannot connect to {socket}: {e}");
            return 1;
        }
    };
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("mimi ctl: {e}");
            return 1;
        }
    };
    let mut reader = BufReader::new(stream);

    let req = json!({ "id": 1, "method": method, "params": params });
    if let Err(e) = writer.write_all(format!("{req}\n").as_bytes()) {
        eprintln!("mimi ctl: write failed: {e}");
        return 1;
    }

    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        eprintln!("mimi ctl: connection closed");
        return 1;
    }
    let reply: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mimi ctl: bad reply: {e}");
            return 1;
        }
    };
    if !reply["error"].is_null() {
        eprintln!("error: {}", reply["error"]);
        return 1;
    }

    if cmd == "watch" {
        // Stream events until the terminal (or user) closes the connection.
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => print!("{line}"),
            }
        }
        return 0;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&reply["result"]).unwrap()
    );
    // For `run`, mirror the remote command's exit code.
    if method == "run" {
        return reply["result"]["exit"].as_i64().unwrap_or(0).clamp(0, 255) as i32;
    }
    0
}
