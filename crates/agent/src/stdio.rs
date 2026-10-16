//! The agent layer's MCP over stdio, for clients that start local servers as processes (Claude
//! Desktop and Cowork). `deckcraft --agent-stdio [--control 127.0.0.1:7985]` serves
//! newline-delimited JSON-RPC on stdin/stdout with the same tools as the in-app HTTP server, and
//! reaches the deck through the app's control channel, connected on the first tool call: Claude
//! can start before DeckCraft does and keeps working across app restarts. `initialize` and
//! `tools/list` work without the app; a tool called while it is closed says so. Nothing but
//! protocol goes to stdout. Same contract as GridCraft and WordCraft.

use std::io::{BufRead, Write};

use deckcraft_mcp::{Backend, Remote};
use serde_json::{Value, json};

/// A backend that connects to the app's control channel on first use, and again after the app
/// went away.
pub struct Lazy {
    addr: String,
    slot: Option<Remote>,
}

impl Lazy {
    pub fn new(addr: String) -> Self {
        Lazy { addr, slot: None }
    }
}

impl Backend for Lazy {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if self.slot.is_none() {
            let r = Remote::connect(&self.addr)
                .map_err(|e| format!("DeckCraft control channel at {} is not reachable ({e}). Is DeckCraft open?", self.addr))?;
            self.slot = Some(r);
        }
        let Some(b) = self.slot.as_mut() else { return Err("not connected".into()) };
        let r = b.call(method, params);
        // The connection reconnects by itself when the app comes back; say what to do meanwhile.
        if let Err(e) = &r
            && e.contains("not reachable")
        {
            return Err(format!("{e}. Is DeckCraft open?"));
        }
        r
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        format!("the DeckCraft app at {}", self.addr)
    }
}

const STDIO_NOTE: &str = " Over this stdio connection there is no event stream: listen with wait_for_questions in a loop (it returns as soon as a question is pending, or empty after up to 50 s), answer what it returned, and call it again.";

/// The reply to one message (`None` for notifications).
pub fn handle(b: &mut dyn Backend, msg: &Value) -> Option<Value> {
    let mut reply = crate::mcp::handle(b, msg)?;
    if msg.get("method").and_then(Value::as_str) == Some("initialize")
        && let Some(text) = reply.pointer_mut("/result/instructions")
        && let Some(s) = text.as_str()
    {
        *text = Value::String(format!("{s}{STDIO_NOTE}"));
    }
    Some(reply)
}

/// Serves newline-delimited JSON-RPC from `input` until it closes.
pub fn serve(b: &mut dyn Backend, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| handle(b, m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => handle(b, &msg),
            Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
        };
        if let Some(v) = reply {
            writeln!(output, "{v}")?;
            output.flush()?;
        }
    }
    Ok(())
}
