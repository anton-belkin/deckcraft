//! fork:agent — starts the agent layer: the `agent.*` and `link.*` commands, the Agent pane and the
//! in-app MCP server (`http://127.0.0.1:7984/mcp`), which reaches the presentation through the
//! control channel.
//!
//! `--agent-port <port>` (or `DECKCRAFT_AGENT_PORT`) picks the port, `--no-agent` turns it off.
//! With the agent on, the control channel starts too (on 7985 unless `--control` says otherwise:
//! GridCraft keeps 7979, which DeckCraft's links ask).

/// Default port of the agent server.
const AGENT_PORT: u16 = 7984;
/// Control port used for the agent when none was asked for.
const CONTROL_PORT: u16 = 7985;

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
    let addr = format!("127.0.0.1:{control}");
    deckcraft_agent::links::set_own_control(addr.clone());
    match deckcraft_agent::server::start(port, addr) {
        Ok(port) => deckcraft_agent_ui::set_server_url(format!("http://127.0.0.1:{port}/mcp")),
        Err(e) => log::error!("agent server on port {port}: {e}"),
    }
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
