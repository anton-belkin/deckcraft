//! The agent layer's MCP over stdio, for clients that start local servers as processes (Claude
//! Desktop and Cowork, `claude mcp add deckcraft -- deckcraft --agent-stdio`).
//!
//! `deckcraft --agent-stdio` serves newline-delimited JSON-RPC on stdin/stdout and forwards tool
//! calls to the running app's agent server (`http://127.0.0.1:7984/mcp`). `initialize` and
//! `tools/list` are answered here, so the client starts fine before DeckCraft does; a tool called
//! while DeckCraft isn't open says so plainly, and the next call works once it is. Nothing but
//! protocol goes to stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};

/// Longest a forwarded call may take (`wait_for_questions` waits up to 50 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(75);
/// Largest reply read from the app.
const MAX_REPLY: u64 = 32 * 1024 * 1024;

/// A backend for the answers given here (no tool reaches it).
struct Local;

impl deckcraft_mcp::Backend for Local {
    fn call(&mut self, _method: &str, _params: Value) -> Result<Value, String> {
        Err("not connected".into())
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "stdio bridge".into()
    }
}

/// POSTs one JSON-RPC message to the app's `/mcp`. `Ok(None)` for an accepted notification.
pub fn post(port: u16, body: &Value) -> Result<Option<Value>, String> {
    let mut s = TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500))
        .map_err(|_| format!("DeckCraft isn't open (no agent server on 127.0.0.1:{port}). Open DeckCraft, then call the tool again."))?;
    s.set_read_timeout(Some(CALL_TIMEOUT)).map_err(|e| e.to_string())?;
    let body = body.to_string();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s.take(MAX_REPLY));
    let mut status = String::new();
    r.read_line(&mut status).map_err(|e| format!("DeckCraft didn't answer: {e}"))?;
    let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("DeckCraft sent a malformed reply")?;
    let mut len: Option<usize> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).map_err(|e| e.to_string())? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let mut body = Vec::new();
    match len {
        Some(n) => {
            body.resize(n, 0);
            r.read_exact(&mut body).map_err(|e| e.to_string())?;
        }
        None => {
            r.read_to_end(&mut body).map_err(|e| e.to_string())?;
        }
    }
    if code == 202 || body.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&body).map(Some).map_err(|e| format!("DeckCraft sent a malformed reply ({code}): {e}"))
}

const STDIO_NOTE: &str = " Over this stdio connection there is no event stream: listen with wait_for_questions in a loop (it returns as soon as a question is pending, or empty after up to 50 s), answer what it returned, and call it again.";

/// The reply to one message.
pub fn handle(port: u16, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned();
    if method == "tools/call" {
        let id = id?;
        return Some(match post(port, msg) {
            Ok(Some(v)) => v,
            Ok(None) => json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "no reply"}], "isError": true}}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": e}], "isError": true}}),
        });
    }
    if method == "initialize" || method.starts_with("notifications/") {
        // Let the app know an agent is there (its Connection status), when it is open.
        let _ = post(port, msg);
    }
    let mut reply = crate::mcp::handle(&mut Local, msg)?;
    if method == "initialize"
        && let Some(text) = reply.pointer_mut("/result/instructions")
        && let Some(s) = text.as_str()
    {
        *text = Value::String(format!("{}{STDIO_NOTE}", s.replace("<port>", &port.to_string())));
    }
    Some(reply)
}

/// Serves newline-delimited JSON-RPC from `input` until it closes.
pub fn serve(port: u16, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| handle(port, m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => handle(port, &msg),
            Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
        };
        if let Some(v) = reply {
            writeln!(output, "{v}")?;
            output.flush()?;
        }
    }
    Ok(())
}
