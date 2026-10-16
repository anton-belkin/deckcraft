//! What the agent knows: the Agent tab's Context group (the brief shared by GridCraft, WordCraft and
//! DeckCraft). Three modes, chosen per document and saved in its layer:
//!
//! - **No context**: only this document; the background client runs in its neutral, empty folder
//!   (with Web: WebSearch and WebFetch too);
//! - **Claude Cowork**: the person's Cowork chat, its project and memory (AItoCraft, the chat waits
//!   for questions);
//! - **Claude Code**: the person's Claude Code session (AItoCraft as a channel), or a project folder
//!   the background client runs in (its CLAUDE.md, memory and files).
//!
//! A new document starts with the last mode used in the app. The app's listener mode follows the
//! active document's context ([`apply`]).

use std::sync::Mutex;

use deckcraft_engine::{DocState, Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::layer;
use crate::listener::{self, Mode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    None,
    Cowork,
    Code,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::None, Kind::Cowork, Kind::Code];

    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "no context" | "background" => Some(Kind::None),
            "cowork" | "claude cowork" | "desktop" => Some(Kind::Cowork),
            "code" | "claude code" => Some(Kind::Code),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Kind::None => "none",
            Kind::Cowork => "cowork",
            Kind::Code => "code",
        }
    }
    /// The mode's name.
    pub fn label(self) -> &'static str {
        match self {
            Kind::None => "No context",
            Kind::Cowork => "Claude Cowork",
            Kind::Code => "Claude Code",
        }
    }
    /// Its one line.
    pub fn line(self) -> &'static str {
        match self {
            Kind::None => "Only this document. Runs in the background.",
            Kind::Cowork => "Your Cowork chat, its project and memory.",
            Kind::Code => "Your Claude Code session or a project folder.",
        }
    }
    /// What the agent knows.
    pub fn chips(self) -> &'static [&'static str] {
        match self {
            Kind::None => &["Document"],
            Kind::Cowork => &["Document", "Your chat"],
            Kind::Code => &["Document", "Project folder"],
        }
    }
}

/// Claude Code: the person's session, or a project folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Code {
    #[default]
    Session,
    Folder,
}

impl Code {
    pub fn label(self) -> &'static str {
        match self {
            Code::Session => "My session",
            Code::Folder => "Project folder",
        }
    }
}

/// A document's context.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Context {
    pub kind: Kind,
    pub code: Code,
    /// Claude Code ▸ Project folder: the folder the background client runs in.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// No context ▸ Web: the background client may also search and fetch the web.
    pub web: bool,
}

impl Context {
    /// How questions reach the agent.
    pub fn mode(&self) -> Mode {
        match (self.kind, self.code) {
            (Kind::None, _) | (Kind::Code, Code::Folder) => Mode::Background,
            (Kind::Cowork, _) => Mode::Desktop,
            (Kind::Code, Code::Session) => Mode::Channel,
        }
    }

    pub fn from_mode(m: Mode) -> Context {
        let kind = match m {
            Mode::Desktop => Kind::Cowork,
            Mode::Channel => Kind::Code,
            Mode::Background | Mode::Off => Kind::None,
        };
        Context { kind, ..Default::default() }
    }

    /// The folder the background client runs in, when the person chose one.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        (self.kind == Kind::Code && self.code == Code::Folder && !self.folder.is_empty()).then(|| std::path::PathBuf::from(&self.folder))
    }

    /// The background client may use the web.
    pub fn web(&self) -> bool {
        self.kind == Kind::None && self.web
    }

    pub fn json(&self) -> Json {
        json!({"kind": self.kind.id(), "code": match self.code { Code::Session => "session", Code::Folder => "folder" }, "folder": self.folder, "web": self.web, "mode": self.mode().id()})
    }
}

/// The last context the person chose in the app (a new document starts with it).
static LAST: Mutex<Option<Context>> = Mutex::new(None);

pub fn last() -> Context {
    let g = LAST.lock().unwrap_or_else(|p| p.into_inner());
    g.clone().unwrap_or_else(|| Context::from_mode(listener::mode()))
}

fn set_last(c: &Context) {
    // The folder belongs to its document.
    let mut c = c.clone();
    c.folder.clear();
    *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(c);
}

/// A document's context: saved in its layer, or the last one used.
pub fn of(d: &DocState) -> Context {
    layer::get(d).context.clone().unwrap_or_else(last)
}

/// The app's listener mode follows the active document's context (call every frame). An app
/// started with the agent off stays off for documents that never chose a mode.
pub fn apply(s: &Session) {
    let Some(d) = s.active() else { return };
    let saved = layer::get(d).context.clone();
    if saved.is_none() && listener::mode() == Mode::Off {
        return;
    }
    let m = saved.unwrap_or_else(last).mode();
    if listener::mode() != m {
        listener::set_mode(m);
    }
}

/// `agent.contextMode {kind?, code?: session|folder, folder?, web?}`: the person sets what the agent
/// knows for this document (undoable) → the context.
pub fn command(s: &mut Session, p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let before = layer::get(d).context.clone();
    let mut c = before.clone().unwrap_or_else(last);
    let changes = ["kind", "code", "folder", "web"].iter().any(|k| p.get(*k).is_some());
    if !changes {
        return Ok(c.json());
    }
    if p.get("actor").and_then(Json::as_str) == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(crate::bad("agent.contextMode", "only a person decides what the agent knows"));
    }
    if let Some(k) = p.get("kind").and_then(Json::as_str) {
        c.kind = Kind::parse(k).ok_or_else(|| crate::bad("agent.contextMode", "`kind` is none, cowork or code"))?;
    }
    if let Some(k) = p.get("code").and_then(Json::as_str) {
        c.code = match k {
            "session" => Code::Session,
            "folder" => Code::Folder,
            _ => return Err(crate::bad("agent.contextMode", "`code` is session or folder")),
        };
    }
    if let Some(f) = p.get("folder").and_then(Json::as_str) {
        c.folder = f.chars().take(4_000).collect();
    }
    if let Some(w) = p.get("web").and_then(Json::as_bool) {
        c.web = w;
    }
    if before.as_ref() == Some(&c) {
        return Ok(c.json());
    }
    let after = c.clone();
    layer::update(d, |l| l.context = Some(after.clone()));
    set_last(&c);
    listener::set_mode(c.mode());
    crate::undo::push(s, crate::undo::Op::Context { before, after: Some(c.clone()) });
    Ok(c.json())
}

// ---------------------------------------------------------------- working

/// The work counts as over after this long without the agent doing anything.
pub const IDLE_MS: u64 = 90_000;
/// A proposal step keeps its topic "being worked on" this long.
pub const DRAFTING_MS: u64 = 10_000;

/// When the agent last worked on each document (its calls on it), ms.
static WORK: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// The agent called the layer on this document.
pub fn note_work(uid: u64) {
    let mut g = WORK.lock().unwrap_or_else(|p| p.into_inner());
    g.retain(|(u, _)| *u != uid);
    g.push((uid, crate::now_ms()));
    let n = g.len();
    if n > 64 {
        g.drain(..n - 64);
    }
}

/// When the agent last worked on this document (0: not in this run).
pub fn last_work(uid: u64) -> u64 {
    WORK.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(u, _)| *u == uid).map_or(0, |x| x.1)
}

/// The agent worked on this document within the listener window: it counts as present (the lamp
/// goes back to green Ready when the work ends, not grey), even without a listener's heartbeat.
pub fn recently_worked(uid: u64) -> bool {
    let at = last_work(uid);
    at > 0 && crate::now_ms().saturating_sub(at) < IDLE_MS
}

/// The topics the agent is working on in this document now: a question claimed and not answered,
/// or a proposal being built (a step within the last few seconds). Empty when nothing happened for
/// [`IDLE_MS`].
pub fn working(d: &DocState) -> Vec<String> {
    let l = layer::get(d);
    let now = crate::now_ms();
    let drafted = l.drafting.iter().map(|x| x.1).max().unwrap_or(0);
    let last = last_work(d.uid).max(drafted);
    if now.saturating_sub(last) >= IDLE_MS {
        return vec![];
    }
    let mut out: Vec<String> = vec![];
    for q in &l.questions {
        if q.status == layer::QuestionStatus::Working && !q.hidden && !q.is_ping() && !out.contains(&q.topic) {
            out.push(q.topic.clone());
        }
    }
    for (t, at) in &l.drafting {
        if now.saturating_sub(*at) < DRAFTING_MS && !out.contains(t) {
            out.push(t.clone());
        }
    }
    out.retain(|t| l.topic(t).is_some_and(|x| !x.hidden));
    out
}

// ---------------------------------------------------------------- readiness checks

/// What the app found out about the person's setup (each looked at in the background).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    /// The claude CLI is logged in (`claude auth status`).
    pub logged_in: Option<bool>,
    /// Claude Code has AItoCraft (`claude mcp get aitocraft`).
    pub code_has_hub: Option<bool>,
    /// Claude Desktop's config has AItoCraft.
    pub desktop_has_hub: Option<bool>,
}

static SETUP: Mutex<(Setup, Option<u64>, bool)> = Mutex::new((Setup { logged_in: None, code_has_hub: None, desktop_has_hub: None }, None, false));

/// Seconds between looks.
const SETUP_EVERY_MS: u64 = 30_000;

/// The last look at the setup; starts another in the background when it is old (or `now`).
pub fn setup(now: bool) -> Setup {
    let mut g = SETUP.lock().unwrap_or_else(|p| p.into_inner());
    let t = crate::now_ms();
    let due = now || g.1.is_none_or(|at| t.saturating_sub(at) >= SETUP_EVERY_MS);
    if due && !g.2 {
        g.1 = Some(t);
        g.2 = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let spawned = std::thread::Builder::new().name("agent-setup".into()).spawn(|| {
                let s = look();
                let mut g = SETUP.lock().unwrap_or_else(|p| p.into_inner());
                g.0 = s;
                g.2 = false;
            });
            if spawned.is_err() {
                g.2 = false;
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            g.2 = false;
        }
    }
    g.0.clone()
}

/// For tests and offscreen renders: what the setup looks like, without looking.
pub fn set_setup(s: Setup) {
    let mut g = SETUP.lock().unwrap_or_else(|p| p.into_inner());
    g.0 = s;
    g.1 = Some(u64::MAX / 2);
}

#[cfg(not(target_arch = "wasm32"))]
fn quiet(cmd: &str, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).output().ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn look() -> Setup {
    let cmd = crate::runner::config().command;
    let logged_in = quiet(&cmd, &["auth", "status", "--json"]).map(|o| {
        o.status.success() && serde_json::from_slice::<Json>(&o.stdout).ok().and_then(|v| v.get("loggedIn").and_then(Json::as_bool)).unwrap_or(false)
    });
    let code_has_hub = quiet(&cmd, &["mcp", "get", "aitocraft"]).map(|o| o.status.success());
    let desktop_has_hub = crate::cowork::config_path().map(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
            .is_some_and(|v| v.pointer("/mcpServers/aitocraft").is_some())
    });
    Setup { logged_in, code_has_hub, desktop_has_hub }
}

// ---------------------------------------------------------------- the Get ready steps' actions

/// Opens a terminal running `command` (Log in to Claude).
pub fn open_terminal(command: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script =
            format!("tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell", command.replace('\\', "\\\\").replace('"', "\\\""));
        std::process::Command::new("osascript").args(["-e", &script]).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd").args(["/C", "start", "cmd", "/K", command]).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("x-terminal-emulator").args(["-e", "sh", "-c", command]).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = command;
        Err("not in the browser".into())
    }
}

type Picker = Box<dyn Fn() -> Option<String> + Send + Sync>;
static PICKER: Mutex<Option<Picker>> = Mutex::new(None);

/// Replaces the folder dialog (tests, offscreen renders).
pub fn set_folder_picker(f: Option<Picker>) {
    *PICKER.lock().unwrap_or_else(|p| p.into_inner()) = f;
}

/// Asks the person for a folder (Choose folder…).
pub fn choose_folder() -> Option<String> {
    if let Some(f) = PICKER.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return f();
    }
    #[cfg(target_os = "macos")]
    {
        let o = std::process::Command::new("osascript")
            .args(["-e", "POSIX path of (choose folder with prompt \"The project folder the agent works in\")"])
            .output()
            .ok()?;
        let p = String::from_utf8_lossy(&o.stdout).trim().trim_end_matches('/').to_string();
        (o.status.success() && !p.is_empty()).then_some(p)
    }
    #[cfg(target_os = "windows")]
    {
        let ps = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
        let o = std::process::Command::new("powershell").args(["-NoProfile", "-Command", ps]).output().ok()?;
        let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!p.is_empty()).then_some(p)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let o = std::process::Command::new("zenity").args(["--file-selection", "--directory"]).output().ok()?;
        let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !p.is_empty()).then_some(p)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Runs `claude mcp add --scope user aitocraft -- <hub>` (Claude Code ▸ Install AItoCraft).
pub fn install_in_code(hub: &std::path::Path) -> std::result::Result<(), String> {
    let cmd = crate::runner::config().command;
    let o = std::process::Command::new(&cmd)
        .args(["mcp", "add", "--scope", "user", "aitocraft", "--"])
        .arg(hub)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if o.status.success() {
        let _ = setup(true);
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("error").to_string())
    }
}
