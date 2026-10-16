//! fork:agent — starts the agent layer: the `agent.*` and `link.*` commands, the Agent pane and the
//! in-app MCP server (`http://127.0.0.1:7984/mcp`), which reaches the presentation through the
//! control channel.
//!
//! `--agent-port <port>` (or `DECKCRAFT_AGENT_PORT`) picks the port, `--no-agent` turns it off,
//! `--agent-mode connect|channel|background|off` says how the agent comes in (else the mode last
//! chosen on the Agent tab).
//! `--agent-stdio [--control HOST:PORT]` doesn't open a window: it serves the agent layer's MCP
//! over stdin/stdout for Claude Desktop and Cowork, reaching the running app through its control
//! channel (see `deckcraft_agent::stdio`).
//! With the agent on, the control channel starts too (on 7985 unless `--control` says otherwise:
//! GridCraft keeps 7979, which DeckCraft's links ask).

/// Default port of the agent server.
const AGENT_PORT: u16 = 7984;
/// Control port used for the agent when none was asked for.
const CONTROL_PORT: u16 = 7985;

/// `--agent-mode` was given (it wins over the remembered mode).
static MODE_GIVEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Where the app remembers the agent mode (the app's setting, never the file's).
fn mode_file() -> Option<std::path::PathBuf> {
    crate::config_dir().map(|d| d.join("agent").join("mode"))
}

/// Takes the agent's own flags out of the files the command line named (the app's own parser
/// takes unknown arguments for files); returns the files, the agent port and the control port.
pub fn take_args(args: Vec<String>, control: Option<u16>) -> (Vec<String>, Option<u16>, Option<u16>) {
    let mut port = std::env::var("DECKCRAFT_AGENT_PORT").ok().and_then(|p| p.parse().ok()).or(Some(AGENT_PORT));
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent-port" => port = it.next().and_then(|p| p.parse().ok()),
            "--no-agent" => port = None,
            "--agent-mode" => {
                if let Some(m) = it.next().as_deref().and_then(deckcraft_agent::listener::Mode::parse) {
                    deckcraft_agent::listener::set_mode(m);
                    MODE_GIVEN.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            _ => rest.push(a),
        }
    }
    (rest, port, control_port(port, control))
}

/// The control port: the agent server needs one.
pub fn control_port(agent: Option<u16>, control: Option<u16>) -> Option<u16> {
    if agent.is_some() { control.or(Some(CONTROL_PORT)) } else { control }
}

/// Registers the agent layer and starts its server (on the UI thread, before the first frame).
pub fn start(agent: Option<u16>, control: Option<u16>) {
    let (Some(port), Some(control)) = (agent, control) else { return };
    deckcraft_agent_ui::install();
    if let Some(f) = mode_file() {
        if !MODE_GIVEN.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(m) = std::fs::read_to_string(&f).ok().as_deref().and_then(deckcraft_agent::listener::Mode::parse)
        {
            deckcraft_agent::listener::set_mode(m);
        }
        deckcraft_agent_ui::set_mode_file(f);
    }
    let addr = format!("127.0.0.1:{control}");
    deckcraft_agent::links::set_own_control(addr.clone());
    match deckcraft_agent::server::start(port, addr) {
        Ok(port) => {
            deckcraft_agent_ui::set_server_url(format!("http://127.0.0.1:{port}/mcp"));
            if let Ok(exe) = std::env::current_exe() {
                deckcraft_agent_ui::set_stdio_entry(deckcraft_agent::cowork::entry(&exe, control));
            }
        }
        Err(e) => log::error!("agent server on port {port}: {e}"),
    }
}

/// `deckcraft --agent-stdio [--control HOST:PORT] [--agent-port PORT]`: serve the agent layer's MCP over stdio (for
/// Claude Desktop and Cowork), reaching the running app through its control channel, and exit.
/// Runs first in `main`: nothing else of the app starts (no log, no window), and nothing but
/// protocol goes to stdout.
pub fn stdio_if_asked() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == deckcraft_agent::cowork::STDIO_FLAG) {
        return;
    }
    let port = std::env::var("DECKCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(CONTROL_PORT);
    let mut addr = format!("127.0.0.1:{port}");
    if let Some(v) = args.iter().position(|a| a == "--control").and_then(|i| args.get(i + 1)) {
        addr = if v.contains(':') { v.clone() } else { format!("127.0.0.1:{v}") };
    }
    let agent_port = args
        .iter()
        .position(|a| a == "--agent-port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .or_else(|| std::env::var("DECKCRAFT_AGENT_PORT").ok().and_then(|p| p.parse().ok()))
        .unwrap_or(AGENT_PORT);
    let code = deckcraft_agent::stdio::run(&addr, agent_port);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_flags_are_taken_out_and_the_control_port_follows() {
        let (rest, port, control) = take_args(vec!["--agent-port".into(), "9000".into(), "deck.pptx".into()], None);
        assert_eq!(rest, vec!["deck.pptx".to_string()]);
        assert_eq!(port, Some(9000));
        assert_eq!(control, Some(CONTROL_PORT));
        let (_, off, control) = take_args(vec!["--no-agent".into()], None);
        assert_eq!((off, control), (None, None));
        assert_eq!(control_port(Some(1), None), Some(CONTROL_PORT));
        assert_eq!(control_port(Some(1), Some(7990)), Some(7990));
        assert_eq!(control_port(None, None), None);
    }
}
