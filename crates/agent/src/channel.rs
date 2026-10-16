//! "Send to my Claude Code session" ([`crate::listener::Mode::Channel`], Claude Code Channels,
//! research preview): the app pushes each new question on its `GET /mcp` stream, and the stdio
//! server ([`crate::stdio`]) passes them on to the Claude Code session that started it as
//! `notifications/claude/channel`:
//!
//! ```text
//! claude mcp add deckcraft-channel -- /path/to/deckcraft --agent-stdio --control 127.0.0.1:7985 --agent-port 7984
//! claude --dangerously-load-development-channels server:deckcraft-channel
//! ```
//!
//! Interactive sessions only (`claude -p` ignores channels).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};

/// The server's name for the channel registration.
pub const SERVER_NAME: &str = "deckcraft-channel";

/// The channel notification for a question (as the app's `GET /mcp` stream sends it).
pub fn notification(q: &Json) -> Json {
    let get = |k: &str| q.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let kind = get("kind");
    let content = if kind == "ping" {
        format!("Connection check from DeckCraft: answer question {} with \"here\" (answer_question) at once; don't investigate.", get("id"))
    } else {
        let place = q.pointer("/where/label").and_then(Json::as_str).unwrap_or("the deck");
        format!("Question in DeckCraft on {place}: {}", get("text"))
    };
    let slide = q.pointer("/where/slide").and_then(Json::as_u64).map(|n| n.to_string()).unwrap_or_default();
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": content,
            // Keys must be identifiers (letters, digits, underscores).
            "meta": {"question_id": get("id"), "kind": if kind.is_empty() { "question".to_string() } else { kind }, "slide": slide},
        },
    })
}

/// The command lines for "my session": add the channel server, then start Claude Code with it.
pub fn commands(exe: &str, control_port: u16, agent_port: u16) -> (String, String) {
    let q = |s: &str| if s.contains(' ') { format!("'{}'", s.replace('\'', "'\\''")) } else { s.to_string() };
    (
        format!("claude mcp add {SERVER_NAME} -- {} --agent-stdio --control 127.0.0.1:{control_port} --agent-port {agent_port}", q(exe)),
        format!("claude --dangerously-load-development-channels server:{SERVER_NAME}"),
    )
}

/// Follows the app's `GET /mcp` stream and passes each event on; reconnects while the app is away
/// (or not in the channel mode: the server refuses the stream then).
pub fn follow(port: u16, out: Arc<Mutex<dyn Write + Send>>) {
    loop {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) {
            let req = format!("GET /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n");
            if c.write_all(req.as_bytes()).is_ok() {
                for line in BufReader::new(c).lines().map_while(std::result::Result::ok) {
                    if let Some(data) = line.strip_prefix("data:")
                        && let Ok(v) = serde_json::from_str::<Json>(data.trim())
                    {
                        let mut w = match out.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        let _ = writeln!(w, "{v}");
                        let _ = w.flush();
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
