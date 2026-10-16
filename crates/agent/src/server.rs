//! The in-app agent server: HTTP on 127.0.0.1, started by the app itself, so connecting an agent
//! is one line of MCP config (`http://127.0.0.1:7984/mcp`) and no extra process.
//!
//! - `POST /mcp` — MCP over Streamable HTTP (JSON responses; no server-initiated stream).
//! - `GET /events` — one JSON line per new question, kept open: what an agent's background
//!   monitor watches (`curl -sN http://127.0.0.1:7984/events`).
//! - `GET /health` — `ok`.
//!
//! It reaches the document through the app's loopback control channel, like any other client.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use deckcraft_mcp::{Backend, Remote};
use serde_json::{Value, json};

/// Default port of the agent server.
pub const DEFAULT_PORT: u16 = 7984;
/// Largest request body accepted.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Largest request head accepted.
const MAX_HEAD: usize = 64 * 1024;

/// Starts the server on `port` (0 = any) talking to the control channel at `control`.
/// Returns the port it listens on.
pub fn start(port: u16, control: String) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new().name("agent-server".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let control = control.clone();
            let _ = std::thread::Builder::new().name("agent-conn".into()).spawn(move || serve(stream, &control));
        }
    })?;
    log::info!("agent MCP server on http://127.0.0.1:{port}/mcp");
    LISTENING.store(u32::from(port), Ordering::Relaxed);
    Ok(port)
}

static LISTENING: AtomicU32 = AtomicU32::new(0);
static LAST_CALL: AtomicU64 = AtomicU64::new(0);
static MONITORS: AtomicUsize = AtomicUsize::new(0);
static CLIENT: Mutex<String> = Mutex::new(String::new());

/// What the server knows about its agent, for the app's Connection status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The port it listens on (`None`: not started).
    pub port: Option<u16>,
    /// When an agent last called a tool or initialised (ms since the epoch; 0: never).
    pub last_call: u64,
    /// Agents watching `/events` right now.
    pub monitors: usize,
    /// The client's name from `initialize` (`claude-code`, `claude-ai`…).
    pub client: String,
}

pub fn status() -> Status {
    let port = u16::try_from(LISTENING.load(Ordering::Relaxed)).ok().filter(|p| *p != 0);
    let client = CLIENT.lock().map(|c| c.clone()).unwrap_or_default();
    Status { port, last_call: LAST_CALL.load(Ordering::Relaxed), monitors: MONITORS.load(Ordering::Relaxed), client }
}

fn note_call(msg: &Value) {
    LAST_CALL.store(crate::now_ms(), Ordering::Relaxed);
    if msg.get("method").and_then(Value::as_str) == Some("initialize")
        && let Some(name) = msg.pointer("/params/clientInfo/name").and_then(Value::as_str)
        && let Ok(mut c) = CLIENT.lock()
    {
        *c = name.chars().take(80).collect();
    }
}

/// Counts an `/events` watcher while it is connected.
struct Watching;

impl Watching {
    fn new() -> Self {
        MONITORS.fetch_add(1, Ordering::Relaxed);
        Watching
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        MONITORS.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = r.by_ref().take(MAX_HEAD as u64).read_until(b'\n', &mut line).ok()?;
        if n == 0 {
            return None;
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return None;
        }
    }
    let head = String::from_utf8(head).ok()?;
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let path = first.next()?.split('?').next().unwrap_or("/").to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body })
}

fn respond(w: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    write!(w, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n", body.len())?;
    w.write_all(body)?;
    w.flush()
}

/// Browsers may only reach us from pages on this machine (DNS-rebinding guard, as MCP asks).
fn origin_ok(req: &Request) -> bool {
    match req.header("origin") {
        None => true,
        Some(o) => {
            let host = o.split("://").nth(1).unwrap_or(o).split(':').next().unwrap_or("");
            matches!(host, "localhost" | "127.0.0.1" | "[::1]") || o == "null"
        }
    }
}

fn serve(stream: TcpStream, control: &str) {
    let _ = stream.set_nodelay(true);
    let Ok(mut w) = stream.try_clone() else { return };
    let mut r = BufReader::new(stream);
    let mut backend: Option<Remote> = None;
    while let Some(req) = read_request(&mut r) {
        if !origin_ok(&req) {
            let _ = respond(&mut w, "403 Forbidden", "text/plain", b"origin not allowed");
            return;
        }
        let result = match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/mcp") => post_mcp(&mut w, &req, control, &mut backend),
            ("GET", "/mcp") => respond(&mut w, "405 Method Not Allowed", "text/plain", b"POST JSON-RPC messages here"),
            ("DELETE", "/mcp") => respond(&mut w, "200 OK", "text/plain", b""),
            ("GET", "/events") => {
                events(w, control);
                return;
            }
            ("GET", "/health") => respond(&mut w, "200 OK", "text/plain", b"ok"),
            ("GET", "/") => respond(&mut w, "200 OK", "text/plain", HELP.as_bytes()),
            _ => respond(&mut w, "404 Not Found", "text/plain", b"not found"),
        };
        if result.is_err() || req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close")) {
            return;
        }
    }
}

const HELP: &str = "DeckCraft agent layer\n\nPOST /mcp     MCP (Streamable HTTP)\nGET  /events  one JSON line per new question (for a background monitor)\nGET  /health  ok\n";

fn backend<'a>(slot: &'a mut Option<Remote>, control: &str) -> Result<&'a mut Remote, String> {
    if slot.is_none() {
        *slot = Some(Remote::connect(control).map_err(|e| format!("DeckCraft control channel at {control} is not reachable: {e}"))?);
    }
    slot.as_mut().ok_or_else(|| "not connected".to_string())
}

fn post_mcp(w: &mut TcpStream, req: &Request, control: &str, slot: &mut Option<Remote>) -> std::io::Result<()> {
    let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
        let e = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
        return respond(w, "400 Bad Request", "application/json", e.to_string().as_bytes());
    };
    note_call(&msg);
    let reply = match backend(slot, control) {
        Ok(b) => handle_any(b, &msg),
        Err(e) => msg.get("id").map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": e}})),
    };
    match reply {
        Some(v) => respond(w, "200 OK", "application/json", v.to_string().as_bytes()),
        None => respond(w, "202 Accepted", "text/plain", b""),
    }
}

pub(crate) fn handle_any(b: &mut dyn Backend, msg: &Value) -> Option<Value> {
    match msg {
        Value::Array(batch) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| crate::mcp::handle(b, m)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => crate::mcp::handle(b, m),
    }
}

/// Streams new pending questions, one JSON object per line, until the client goes away.
fn events(mut w: TcpStream, control: &str) {
    if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n").is_err() {
        return;
    }
    let _ = w.flush();
    let _watching = Watching::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut slot: Option<Remote> = None;
    loop {
        if let Ok(b) = backend(&mut slot, control) {
            match b.call("engine.execute", json!({"command": "agent.questions", "params": {"status": "pending", "actor": "agent", "masked": true}})) {
                Ok(Value::Array(list)) => {
                    for q in list {
                        let Some(id) = q.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
                        if !seen.insert(id.clone()) {
                            continue;
                        }
                        let line = json!({
                            "event": "question",
                            "id": id,
                            "slide": q.pointer("/where/slide"),
                            "shape": q.pointer("/where/shape"),
                            "place": q.pointer("/where/label"),
                            "question": q.get("text"),
                            "hint": "claim_question, investigate, answer_question",
                        });
                        if writeln!(w, "{line}").and_then(|()| w.flush()).is_err() {
                            return;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => slot = None,
            }
        }
        std::thread::sleep(Duration::from_millis(800));
        // Notice a client that went away even when nothing is being written.
        if w.peer_addr().is_err() {
            return;
        }
    }
}
