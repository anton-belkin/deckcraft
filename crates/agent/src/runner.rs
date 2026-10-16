//! The background agent ([`Mode::Background`]): one `claude -p` process per deck, fed the
//! questions as stream-json on its stdin. Same contract as GridCraft and WordCraft.
//!
//! It runs in a neutral folder (`ArtCraft/agent-sessions/`) unless the person chose the deck's
//! folder as its project (PROTOCOL §4: demoted, and explicit about what it implies), sees only the DeckCraft MCP server (`--strict-mcp-config`), may use only its tools
//! (`--allowedTools mcp__deckcraft__*`), gets the agent skill as its system prompt, and is bounded
//! by `--max-turns` and `--max-budget-usd`. It is named `DeckCraft · <file>` (`--name`); its
//! session id lives in the layer, so reopening the deck resumes the same conversation; a resume
//! that fails here, or New session, starts a fresh one from a summary of the history. Never
//! `--bare` (it needs an API key; a subscription login works without one). Stop is end of input,
//! then SIGINT, then kill.
//!
//! The mode is the app's, never the file's: opening a deck someone sent must not start an agent
//! that spends the reader's money.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

use deckcraft_engine::cmd::{CommandSpec, has_doc};
use deckcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::layer::{self, QuestionStatus};
use crate::listener::{self, Mode};

/// How the background agent is started.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The agent CLI (`claude` by default; `DECKCRAFT_AGENT_COMMAND` changes it).
    pub command: String,
    /// The in-app MCP server.
    pub mcp_url: String,
    pub max_turns: u32,
    pub max_budget_usd: f64,
    /// Where the MCP config and the skill are written.
    pub data_dir: Option<PathBuf>,
    /// Run in the deck's folder (its CLAUDE.md, memory and settings become the agent's context)
    /// instead of the neutral `ArtCraft/agent-sessions/` (PROTOCOL §4). Off by default.
    pub use_deck_folder: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            // Tests never start a real agent.
            command: if cfg!(test) {
                "deckcraft-test-no-agent".into()
            } else {
                std::env::var("DECKCRAFT_AGENT_COMMAND").unwrap_or_else(|_| "claude".into())
            },
            mcp_url: format!("http://127.0.0.1:{}/mcp", crate::server::DEFAULT_PORT),
            max_turns: 40,
            max_budget_usd: 3.0,
            data_dir: default_data_dir(),
            use_deck_folder: false,
        }
    }
}

fn default_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/DeckCraft/agent"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("DeckCraft").join("agent"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("deckcraft/agent"))
    }
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

pub fn config() -> Config {
    CONFIG.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn set_config(c: Config) {
    if let Ok(mut g) = CONFIG.lock() {
        *g = Some(c);
    }
}

/// The agent skill, given to the background agent as its system prompt.
pub const SKILL: &str = include_str!("../../../agent-layer/skill/deckcraft-agent/SKILL.md");

/// Where the agent runs: a neutral, empty folder (`ArtCraft/agent-sessions/`, so no project's
/// CLAUDE.md, memory or settings become its context, its sessions grouped there in Claude
/// Desktop), or the deck's folder when the person chose that.
pub fn session_folder(c: &Config, deck: Option<&str>) -> PathBuf {
    if c.use_deck_folder
        && let Some(dir) = deck.and_then(|p| std::path::absolute(p).ok()).and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    {
        return dir;
    }
    crate::registry::artcraft_dir().map(|d| d.join("agent-sessions")).or_else(|| c.data_dir.clone()).unwrap_or_else(std::env::temp_dir)
}

/// A random-enough UUID (v4 layout) for a session id.
pub fn new_uuid(salt: u64) -> String {
    let t = crate::now_ms();
    let mut x = t ^ salt.rotate_left(17) ^ u64::from(std::process::id()).rotate_left(41);
    let mut next = || {
        // splitmix64
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let (a, b) = (next(), next());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0x0fff) as u16,
        ((b >> 48) as u16 & 0x3fff) | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

// ---------------------------------------------------------------- one agent per deck

/// What a deck's background agent is doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub running: bool,
    pub session_id: String,
    /// The folder the agent runs in (its project in Claude Desktop).
    pub cwd: String,
    /// Messages sent and not yet finished.
    pub in_flight: usize,
    pub turns: usize,
    pub cost_usd: f64,
    pub last_error: String,
    /// The last few things it said or did, newest last.
    pub recent: Vec<String>,
}

struct Agent {
    child: Child,
    stdin: ChildStdin,
    sent: HashSet<String>,
    shared: Arc<Mutex<Status>>,
    /// Started with `--resume`: a quick failure means the session isn't on this machine.
    resumed: bool,
}

#[derive(Default)]
struct Agents {
    by_doc: HashMap<u64, Agent>,
    /// Statuses of agents that stopped, so their last error stays visible.
    ended: HashMap<u64, Status>,
    /// Decks whose resume failed: start a fresh session next time.
    fresh: HashSet<u64>,
    /// Questions already handed over (or that failed to start an agent), per deck.
    queue: HashMap<u64, VecDeque<String>>,
}

static AGENTS: Mutex<Option<Agents>> = Mutex::new(None);

fn agents() -> MutexGuard<'static, Option<Agents>> {
    match AGENTS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn lock(s: &Mutex<Status>) -> MutexGuard<'_, Status> {
    match s.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// The background agent's status for a deck.
pub fn status(uid: u64) -> Option<Status> {
    let g = agents();
    let a = g.as_ref()?;
    a.by_doc.get(&uid).map(|x| lock(&x.shared).clone()).or_else(|| a.ended.get(&uid).cloned())
}

/// The arguments the agent CLI is started with.
pub fn args(c: &Config, session: &str, resume: bool, mcp_config: &str, skill: Option<&str>, name: &str) -> Vec<String> {
    let mut a: Vec<String> =
        ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"].iter().map(|s| s.to_string()).collect();
    // Its name in the session lists (claude 2.1.301+).
    a.extend(["--name".to_string(), name.to_string()]);
    a.extend([if resume { "--resume" } else { "--session-id" }.to_string(), session.to_string()]);
    a.extend(["--mcp-config".to_string(), mcp_config.to_string(), "--strict-mcp-config".to_string()]);
    a.extend(["--allowedTools".to_string(), "mcp__deckcraft__*".to_string()]);
    if let Some(s) = skill {
        a.extend(["--append-system-prompt-file".to_string(), s.to_string()]);
    }
    a.extend(["--max-turns".to_string(), c.max_turns.to_string(), "--max-budget-usd".to_string(), format!("{:.2}", c.max_budget_usd)]);
    a
}

fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null}).to_string()
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// Reads the agent's stream-json: turns finished, cost, errors, what it said.
fn read_stdout(out: std::process::ChildStdout, shared: Arc<Mutex<Status>>) {
    for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
        let Ok(v) = serde_json::from_str::<Json>(&line) else { continue };
        let mut st = lock(&shared);
        match v.get("type").and_then(Json::as_str) {
            Some("system") if v.get("subtype").and_then(Json::as_str) == Some("init") => {
                if let Some(id) = v.get("session_id").and_then(Json::as_str) {
                    st.session_id = id.to_string();
                }
            }
            Some("assistant") => {
                let parts = v.pointer("/message/content").and_then(Json::as_array).cloned().unwrap_or_default();
                for p in parts {
                    let line = match p.get("type").and_then(Json::as_str) {
                        Some("text") => p.get("text").and_then(Json::as_str).map(|t| short(t.trim(), 160)),
                        Some("tool_use") => p.get("name").and_then(Json::as_str).map(|n| format!("→ {}", n.trim_start_matches("mcp__deckcraft__"))),
                        _ => None,
                    };
                    if let Some(l) = line.filter(|l| !l.is_empty()) {
                        st.recent.push(l);
                    }
                }
            }
            Some("result") => {
                st.in_flight = st.in_flight.saturating_sub(1);
                st.turns += 1;
                if let Some(c) = v.get("total_cost_usd").and_then(Json::as_f64) {
                    st.cost_usd = c;
                }
                if v.get("is_error").and_then(Json::as_bool) == Some(true) {
                    let sub = v.get("subtype").and_then(Json::as_str).unwrap_or("error");
                    let text = v.get("result").and_then(Json::as_str).unwrap_or("");
                    st.last_error = short(&format!("{sub}: {text}"), 300);
                }
            }
            _ => {}
        }
        let n = st.recent.len();
        if n > 12 {
            st.recent.drain(..n - 12);
        }
    }
    lock(&shared).running = false;
}

fn read_stderr(err: std::process::ChildStderr, shared: Arc<Mutex<Status>>) {
    for line in BufReader::new(err).lines().map_while(std::result::Result::ok) {
        let l = line.trim();
        if !l.is_empty() {
            lock(&shared).last_error = short(l, 300);
        }
    }
}

/// A summary of the deck's history, to start a fresh session from.
fn seed(l: &layer::Layer) -> String {
    let mut s = String::from("You are continuing work on this presentation in a fresh session. What happened so far, from the deck's history:\n");
    for e in l.log.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev() {
        let who = if e.actor == layer::Actor::Agent { "agent" } else { "presenter" };
        s.push_str(&format!("- {who} {}: {}\n", e.kind, short(&e.text, 200)));
    }
    let open = l.asked().filter(|q| q.status != QuestionStatus::Answered).count();
    s.push_str(&format!("{open} question(s) are open. Call get_context before anything else.\n"));
    s
}

fn write_support_files(c: &Config) -> std::result::Result<(String, Option<String>), String> {
    let dir = c.data_dir.clone().ok_or("no folder for the agent's files")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mcp = dir.join("mcp.json");
    let conf = json!({"mcpServers": {"deckcraft": {"type": "http", "url": c.mcp_url}}});
    std::fs::write(&mcp, conf.to_string()).map_err(|e| format!("{}: {e}", mcp.display()))?;
    let skill = dir.join("deckcraft-agent-skill.md");
    let skill = std::fs::write(&skill, SKILL).ok().map(|()| skill.display().to_string());
    Ok((mcp.display().to_string(), skill))
}

/// Starts the background agent of the active deck (resuming its session when it has one).
pub fn start(s: &Session) -> std::result::Result<Status, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    let uid = d.uid;
    if agents().as_ref().is_some_and(|a| a.by_doc.contains_key(&uid)) {
        return status(uid).ok_or_else(|| "starting".into());
    }
    let c = config();
    let (mcp, skill) = write_support_files(&c)?;
    let l = layer::get(d);
    let fresh = agents().as_ref().is_some_and(|a| a.fresh.contains(&uid));
    let (session, resume) = if l.agent_session.is_empty() || fresh { (new_uuid(uid), false) } else { (l.agent_session.clone(), true) };
    let cwd = session_folder(&c, d.path.as_deref());
    let _ = std::fs::create_dir_all(&cwd);
    let mut child = Command::new(&c.command)
        .args(args(&c, &session, resume, &mcp, skill.as_deref(), &format!("DeckCraft · {}", crate::documents::title(d))))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start `{}`: {e}", c.command))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("the agent's pipes didn't open".into());
    };
    let shared = Arc::new(Mutex::new(Status { running: true, session_id: session.clone(), cwd: cwd.display().to_string(), ..Default::default() }));
    {
        let sh = shared.clone();
        let _ = std::thread::Builder::new().name("agent-stdout".into()).spawn(move || read_stdout(stdout, sh));
        let sh = shared.clone();
        let _ = std::thread::Builder::new().name("agent-stderr".into()).spawn(move || read_stderr(stderr, sh));
    }
    let mut agent = Agent { child, stdin, sent: HashSet::new(), shared, resumed: resume };
    // A fresh session gets the history, when there is one.
    let mut first = vec![];
    if !resume && !l.log.is_empty() {
        first.push(seed(&l));
    }
    for m in first {
        if writeln!(agent.stdin, "{}", user_message(&m)).and_then(|()| agent.stdin.flush()).is_ok() {
            lock(&agent.shared).in_flight += 1;
        }
    }
    let st = lock(&agent.shared).clone();
    {
        let mut g = agents();
        let a = g.get_or_insert_with(Agents::default);
        a.fresh.remove(&uid);
        a.ended.remove(&uid);
        a.by_doc.insert(uid, agent);
    }
    if l.agent_session != session {
        layer::update(d, |l| l.agent_session = session.clone());
    }
    Ok(st)
}

/// Stops a deck's agent: end of input, then SIGINT, then (after a grace period) kill.
pub fn stop(uid: u64) {
    let agent = agents().as_mut().and_then(|a| a.by_doc.remove(&uid));
    let Some(mut agent) = agent else { return };
    drop(agent.stdin);
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-INT", &agent.child.id().to_string()]).status();
    }
    let mut done = false;
    for _ in 0..30 {
        if matches!(agent.child.try_wait(), Ok(Some(_))) {
            done = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !done {
        let _ = agent.child.kill();
        let _ = agent.child.wait();
    }
    let mut st = lock(&agent.shared).clone();
    st.running = false;
    st.in_flight = 0;
    if let Some(a) = agents().as_mut() {
        a.ended.insert(uid, st);
    }
}

pub fn stop_all() {
    let uids: Vec<u64> = agents().as_ref().map(|a| a.by_doc.keys().copied().collect()).unwrap_or_default();
    for u in uids {
        stop(u);
    }
}

/// A fresh session for the active deck: the old one stays in the agent's history; the new one
/// starts from a summary of the deck's log.
pub fn reset(s: &Session) -> std::result::Result<Status, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    stop(d.uid);
    agents().get_or_insert_with(Agents::default).fresh.insert(d.uid);
    start(s)
}

/// The text a question is sent as.
pub fn question_text(s: &Session, q: &Json) -> String {
    let get = |k: &str| q.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    if get("kind") == "ping" {
        return format!(
            "Connection check from DeckCraft (question id {}): answer it at once with answer_question, text \"here\". Don't investigate.",
            get("id")
        );
    }
    let title = s.doc().map(|d| d.title()).unwrap_or_default();
    let place = q.pointer("/where/label").and_then(Json::as_str).unwrap_or("the deck");
    let mut t = format!("A question was asked in the presentation “{title}”, on {place}:\n\n“{}”\n\nQuestion id: {}.", get("text"), get("id"));
    t.push_str(
        " Claim it (claim_question), investigate with the deckcraft tools, and answer with answer_question, linking the slides and sources you used.",
    );
    t
}

/// Called every frame (and by tests): in the background mode, starts the active deck's agent when
/// it has open questions and sends each new question once, in order.
pub fn tick(s: &mut Session) {
    if listener::mode() != Mode::Background {
        return;
    }
    let Some(d) = s.active() else { return };
    let uid = d.uid;
    // A finished process: keep its status; a resume that failed starts fresh next time.
    let exited = {
        let mut g = agents();
        let a = g.get_or_insert_with(Agents::default);
        let gone = a.by_doc.get_mut(&uid).is_some_and(|x| !matches!(x.child.try_wait(), Ok(None)));
        if gone && let Some(mut x) = a.by_doc.remove(&uid) {
            let mut st = lock(&x.shared).clone();
            st.running = false;
            st.in_flight = 0;
            if x.resumed && st.turns <= 1 {
                a.fresh.insert(uid);
                if st.last_error.is_empty() {
                    st.last_error = "the saved session couldn't be resumed here; the next question starts a fresh one".into();
                }
            }
            let _ = x.child.wait();
            a.ended.insert(uid, st);
        }
        gone
    };
    let l = layer::get(d);
    let now = crate::now_ms();
    let pending: Vec<String> = l
        .questions
        .iter()
        .filter(|q| q.status == QuestionStatus::Pending && !(q.is_ping() && now.saturating_sub(q.asked_at) >= listener::PING_TIMEOUT_MS))
        .map(|q| q.id.clone())
        .collect();
    if pending.is_empty() {
        return;
    }
    let running = agents().as_ref().is_some_and(|a| a.by_doc.contains_key(&uid));
    if !running {
        // Don't restart in a loop on the frame after a crash: once per new question.
        let unseen = {
            let g = agents();
            let done = g.as_ref().and_then(|a| a.queue.get(&uid)).cloned().unwrap_or_default();
            pending.iter().any(|p| !done.contains(p))
        };
        if exited || !unseen {
            return;
        }
        if let Err(e) = start(s) {
            let mut g = agents();
            let a = g.get_or_insert_with(Agents::default);
            a.ended.insert(uid, Status { last_error: e, ..Default::default() });
            a.queue.entry(uid).or_default().extend(pending);
            return;
        }
    }
    let new: Vec<String> = {
        let g = agents();
        g.as_ref().and_then(|a| a.by_doc.get(&uid)).map(|x| pending.iter().filter(|p| !x.sent.contains(*p)).cloned().collect()).unwrap_or_default()
    };
    if new.is_empty() {
        return;
    }
    let list = match s.execute("agent.questions", &json!({"status": "pending", "masked": true, "poll": true})) {
        Ok(Json::Array(v)) => v,
        _ => return,
    };
    let texts: Vec<(String, String)> = list
        .iter()
        .filter_map(|q| q.get("id").and_then(Json::as_str).filter(|id| new.iter().any(|n| n == id)).map(|id| (id.to_string(), question_text(s, q))))
        .collect();
    let mut g = agents();
    let Some(a) = g.as_mut() else { return };
    let Some(x) = a.by_doc.get_mut(&uid) else { return };
    for (id, text) in texts {
        if writeln!(x.stdin, "{}", user_message(&text)).and_then(|()| x.stdin.flush()).is_ok() {
            x.sent.insert(id.clone());
            let mut st = lock(&x.shared);
            st.in_flight += 1;
            st.recent.push(format!("← question {id}"));
        } else {
            lock(&x.shared).last_error = "the agent stopped reading its input".into();
        }
        a.queue.entry(uid).or_default().push_back(id);
    }
}

/// `claude --resume <id>` in the agent's folder: continue its conversation by hand (stop the
/// background agent first: one writer per session).
pub fn resume_command(st: &Status) -> String {
    let c = config();
    let cd = if st.cwd.is_empty() { String::new() } else { format!("cd '{}' && ", st.cwd.replace('\'', "'\\''")) };
    format!("{cd}{} --resume {}", c.command, st.session_id)
}

/// Brings Claude Desktop to the front, where the background agent's session sits in the Code tab
/// under the deck's folder (named `DeckCraft · <file>`).
pub fn open_claude_desktop() -> std::result::Result<(), String> {
    let r = if cfg!(target_os = "macos") {
        Command::new("open").args(["-a", "Claude"]).status()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", "claude://"]).status()
    } else {
        Command::new("xdg-open").arg("claude://").status()
    };
    match r {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("Claude Desktop didn't open: is it installed?".into()),
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------- commands (the person's, not the agent's)

macro_rules! spec {
    ($id:literal, $label:literal, $params:literal, $run:expr) => {
        CommandSpec {
            id: $id,
            label: $label,
            menu: &[],
            shortcut: None,
            params: $params,
            enabled: has_doc,
            run: $run,
            journal: false,
            undoable: false,
        }
    };
}

pub static SPECS: &[CommandSpec] = &[
    spec!(
        "agent.agentMode",
        "Agent Mode",
        "{mode?: connect|channel|background|off} → the mode (you connect your agent; questions go to your own Claude Code session; DeckCraft runs a background agent per deck; off) and the background agent's status",
        mode_cmd
    ),
    spec!("agent.agentStart", "Start Background Agent", "{} — starts (or resumes) the active deck's background agent", start_cmd),
    spec!("agent.agentStop", "Stop Background Agent", "{}", stop_cmd),
    spec!("agent.agentReset", "New Agent Session", "{} — a fresh session for the active deck, started from a summary of its history", reset_cmd),
    spec!("agent.agentStatus", "Background Agent Status", "{} → mode, running, session id, folder, cost, last error, what it did lately", status_cmd),
];

/// The status as JSON (with the mode and the command to continue the session by hand).
pub fn status_json(uid: u64, session: &str) -> Json {
    let st = status(uid).unwrap_or_else(|| Status { session_id: session.to_string(), ..Default::default() });
    json!({
        "mode": listener::mode().id(),
        "command": config().command,
        "running": st.running,
        "sessionId": st.session_id,
        "cwd": st.cwd,
        "inFlight": st.in_flight,
        "turns": st.turns,
        "costUsd": st.cost_usd,
        "lastError": st.last_error,
        "recent": st.recent,
        "resumeCommand": if st.session_id.is_empty() { Json::Null } else { json!(resume_command(&st)) },
    })
}

fn refuse_agent(p: &Json, cmd: &str) -> Result<()> {
    if p.get("actor").and_then(Json::as_str) == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(crate::bad(cmd, "only a person decides how the agent runs"));
    }
    Ok(())
}

fn here(s: &Session) -> Result<(u64, String)> {
    let d = s.doc()?;
    Ok((d.uid, layer::get(d).agent_session.clone()))
}

fn mode_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    if let Some(m) = p.get("mode").and_then(Json::as_str) {
        refuse_agent(p, "agent.agentMode")?;
        listener::set_mode(Mode::parse(m).ok_or_else(|| crate::bad("agent.agentMode", "`mode` is connect, channel, background or off"))?);
    }
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn start_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    refuse_agent(p, "agent.agentStart")?;
    start(s).map_err(|e| crate::bad("agent.agentStart", e))?;
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn stop_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    refuse_agent(p, "agent.agentStop")?;
    let (uid, session) = here(s)?;
    stop(uid);
    Ok(status_json(uid, &session))
}

fn reset_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    refuse_agent(p, "agent.agentReset")?;
    reset(s).map_err(|e| crate::bad("agent.agentReset", e))?;
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn status_cmd(s: &mut Session, _p: &Json) -> Result<Json> {
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}
