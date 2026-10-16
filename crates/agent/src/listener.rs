//! Is anyone listening? How the app brings the agent in (the mode), the passive signs that an
//! agent follows the questions, and the active check: a ping question ("Is anyone there?") the
//! agent answers with "here". Same contract in GridCraft, WordCraft and DeckCraft.
//!
//! A ping is a system question: it goes out through every path a question goes (list_questions,
//! wait_for_questions, `/events`, the channel, the background agent) with `"kind": "ping"`, but is
//! never shown as a question, gets no badge and is never saved in the file.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// How the app brings the agent in. Set by the app (a setting, `--agent-mode`), never by a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The person connects their own agent (Claude Code, Cowork, any MCP client).
    #[default]
    Connect,
    /// New questions go to the person's interactive Claude Code session (Claude Code Channels).
    Channel,
    /// The app runs a background agent (`claude -p`) per deck.
    Background,
    /// No agent: the server refuses the agent's calls.
    Off,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Connect, Mode::Channel, Mode::Background, Mode::Off];

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "connect" | "mcp" | "manual" => Some(Mode::Connect),
            "channel" | "session" | "my-session" => Some(Mode::Channel),
            "background" | "bg" => Some(Mode::Background),
            "off" | "none" => Some(Mode::Off),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Mode::Connect => "connect",
            Mode::Channel => "channel",
            Mode::Background => "background",
            Mode::Off => "off",
        }
    }
    /// The item in the mode list.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Connect => "I connect my agent",
            Mode::Channel => "Send to my Claude Code session",
            Mode::Background => "Background agent",
            Mode::Off => "Off",
        }
    }
    /// Its one line of explanation.
    pub fn explanation(self) -> &'static str {
        match self {
            Mode::Connect => "MCP: Claude Code, Cowork, any client",
            Mode::Channel => "Questions arrive in your running session (Channels)",
            Mode::Background => "DeckCraft runs claude itself, one session per deck",
            Mode::Off => "No agent: questions wait, the agent's calls are refused",
        }
    }
}

static MODE: AtomicU8 = AtomicU8::new(0);

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        1 => Mode::Channel,
        2 => Mode::Background,
        3 => Mode::Off,
        _ => Mode::Connect,
    }
}

/// Switches the mode; leaving the background mode stops its agents.
pub fn set_mode(m: Mode) {
    let n = match m {
        Mode::Connect => 0,
        Mode::Channel => 1,
        Mode::Background => 2,
        Mode::Off => 3,
    };
    MODE.store(n, Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    if m != Mode::Background {
        crate::runner::stop_all();
    }
}

// ---------------------------------------------------------------- passive signs

/// How long a `wait_for_questions` counts as listening after its last poll.
pub const WAIT_WINDOW_MS: u64 = 60_000;

static LAST_WAIT: AtomicU64 = AtomicU64::new(0);

/// A `wait_for_questions` is polling (over HTTP or stdio: both reach the app as commands).
pub fn note_wait() {
    LAST_WAIT.store(crate::now_ms(), Ordering::Relaxed);
}

/// What says an agent follows the questions, without asking it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    /// `/events` streams connected now.
    pub events: usize,
    /// Channel streams (`GET /mcp`) connected now.
    pub channel: usize,
    /// A `wait_for_questions` open now or in the last minute.
    pub waiting: bool,
    /// The deck's background agent is running.
    pub background: bool,
    /// The agent's last call to the layer (ms; 0: never).
    pub last_call: u64,
}

impl Signals {
    pub fn any(&self) -> bool {
        self.events > 0 || self.channel > 0 || self.waiting || self.background
    }
    /// "/events stream", "channel", "wait_for_questions", "background agent".
    pub fn describe(&self) -> String {
        let mut v: Vec<&str> = vec![];
        if self.background {
            v.push("background agent running");
        }
        if self.channel > 0 {
            v.push("channel connected");
        }
        if self.events > 0 {
            v.push("/events stream connected");
        }
        if self.waiting {
            v.push("wait_for_questions open");
        }
        v.join(" · ")
    }
}

pub fn signals(uid: Option<u64>) -> Signals {
    let now = crate::now_ms();
    #[cfg(not(target_arch = "wasm32"))]
    let (events, channel, http_call) = {
        let st = crate::server::status();
        (st.monitors, st.channels, st.last_call)
    };
    #[cfg(target_arch = "wasm32")]
    let (events, channel, http_call) = (0, 0, 0);
    #[cfg(not(target_arch = "wasm32"))]
    let background = uid.and_then(crate::runner::status).is_some_and(|s| s.running);
    #[cfg(target_arch = "wasm32")]
    let background = {
        let _ = uid;
        false
    };
    let last_wait = LAST_WAIT.load(Ordering::Relaxed);
    Signals {
        events,
        channel,
        waiting: last_wait > 0 && now.saturating_sub(last_wait) < WAIT_WINDOW_MS,
        background,
        last_call: crate::last_agent_call().max(http_call),
    }
}

// ---------------------------------------------------------------- the check

/// How long a ping waits for its answer before it counts as unanswered.
pub const PING_TIMEOUT_MS: u64 = 60_000;
/// How long an answered ping keeps saying "answering".
pub const PING_FRESH_MS: u64 = 15 * 60_000;
/// The ping question's text.
pub const PING_TEXT: &str = "Is anyone there? (connection check; answer \"here\")";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ping {
    pub id: String,
    pub uid: u64,
    pub sent_at: u64,
    pub answered_at: Option<u64>,
}

static PING: Mutex<Option<Ping>> = Mutex::new(None);

pub fn last_ping() -> Option<Ping> {
    match PING.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

/// Sets the last check (the ping command; tests and demos).
pub fn set_ping(p: Option<Ping>) {
    match PING.lock() {
        Ok(mut g) => *g = p,
        Err(e) => *e.into_inner() = p,
    }
}

/// The ping `id` was answered now.
pub(crate) fn answered(id: &str) {
    let mut g = match PING.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(p) = g.as_mut().filter(|p| p.id == id) {
        p.answered_at = Some(crate::now_ms());
    }
}

/// What the Connection status says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// A ping was answered `secs` seconds after it was sent, `at` (ms).
    Answering {
        at: u64,
        secs: u64,
    },
    /// A ping is out, sent at `since` (ms).
    Checking {
        since: u64,
    },
    /// The last ping got no answer within a minute.
    NoAnswer {
        since: u64,
    },
    /// Only passive signs.
    Listener(Signals),
    Nobody(Signals),
}

pub fn health(uid: Option<u64>) -> Health {
    let now = crate::now_ms();
    let sig = signals(uid);
    if let Some(p) = last_ping().filter(|p| uid.is_none_or(|u| u == p.uid)) {
        match p.answered_at {
            Some(at) if now.saturating_sub(at) < PING_FRESH_MS => return Health::Answering { at, secs: at.saturating_sub(p.sent_at) / 1000 },
            Some(_) => {}
            None if now.saturating_sub(p.sent_at) < PING_TIMEOUT_MS => return Health::Checking { since: p.sent_at },
            None => return Health::NoAnswer { since: p.sent_at },
        }
    }
    if sig.any() { Health::Listener(sig) } else { Health::Nobody(sig) }
}
