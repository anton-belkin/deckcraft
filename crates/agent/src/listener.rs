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
    /// The person's own conversation (Claude Desktop, Cowork, any MCP client) through AItoCraft:
    /// they attach it to the deck and it waits for questions.
    #[default]
    Desktop,
    /// New questions go to the person's interactive Claude Code session (AItoCraft as a channel).
    Channel,
    /// The app runs a background agent (`claude -p`) per deck.
    Background,
    /// No agent: the server refuses the agent's calls.
    Off,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Desktop, Mode::Channel, Mode::Background, Mode::Off];

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "desktop" | "cowork" | "connect" | "mcp" | "manual" => Some(Mode::Desktop),
            "channel" | "session" | "my-session" => Some(Mode::Channel),
            "background" | "bg" => Some(Mode::Background),
            "off" | "none" => Some(Mode::Off),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Mode::Desktop => "desktop",
            Mode::Channel => "channel",
            Mode::Background => "background",
            Mode::Off => "off",
        }
    }
    /// The item in the mode list.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Desktop => "Claude Desktop / Cowork (AItoCraft)",
            Mode::Channel => "My Claude Code session (AItoCraft channel)",
            Mode::Background => "Background agent",
            Mode::Off => "Off",
        }
    }
    /// Its one line of explanation.
    pub fn explanation(self) -> &'static str {
        match self {
            Mode::Desktop => "Start a chat where the work belongs and say which deck: it waits for questions",
            Mode::Channel => "Questions arrive in your running Claude Code session",
            Mode::Background => "The app runs Claude on your subscription. It sees only this document. Uses your limits.",
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
        _ => Mode::Desktop,
    }
}

/// Switches the mode; leaving the background mode stops its agents.
pub fn set_mode(m: Mode) {
    let n = match m {
        Mode::Desktop => 0,
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
    /// The listener that answered (its label), when it said who it is.
    pub answered_by: String,
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

/// The ping `id` was answered now (by `listener`, when given).
pub(crate) fn answered(id: &str, listener: Option<&str>) {
    let mut g = match PING.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(p) = g.as_mut().filter(|p| p.id == id) {
        p.answered_at = Some(crate::now_ms());
        p.answered_by = listener.and_then(label_of).unwrap_or_default();
    }
}

/// What the Connection status says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// A ping was answered `secs` seconds after it was sent, `at` (ms), by a listener (its label).
    Answering {
        at: u64,
        secs: u64,
        by: String,
    },
    /// A ping is out, sent at `since` (ms).
    Checking {
        since: u64,
    },
    /// The last ping got no answer within a minute.
    NoAnswer {
        since: u64,
    },
    /// A conversation attached to the deck (AItoCraft): its label, when it was last seen.
    Listening {
        label: String,
        last_seen: u64,
        more: usize,
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
            Some(at) if now.saturating_sub(at) < PING_FRESH_MS => {
                return Health::Answering { at, secs: at.saturating_sub(p.sent_at) / 1000, by: p.answered_by };
            }
            Some(_) => {}
            None if now.saturating_sub(p.sent_at) < PING_TIMEOUT_MS => return Health::Checking { since: p.sent_at },
            None => return Health::NoAnswer { since: p.sent_at },
        }
    }
    if let Some(u) = uid {
        let ls = listeners(u);
        if let Some(l) = ls.iter().max_by_key(|l| l.last_seen) {
            return Health::Listening { label: l.label.clone(), last_seen: l.last_seen, more: ls.len() - 1 };
        }
    }
    if sig.any() { Health::Listener(sig) } else { Health::Nobody(sig) }
}

// ---------------------------------------------------------------- listeners (AItoCraft)

/// A listener not seen for this long is dropped.
pub const LISTENER_TTL_MS: u64 = 90_000;

/// A conversation attached to a deck (`attach`): it says who it is and keeps coming back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub id: String,
    pub label: String,
    /// The deck (document uid).
    pub uid: u64,
    pub last_seen: u64,
}

static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

fn all_listeners() -> std::sync::MutexGuard<'static, Vec<Listener>> {
    let mut g = match LISTENERS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = crate::now_ms();
    g.retain(|l| now.saturating_sub(l.last_seen) < LISTENER_TTL_MS);
    g
}

/// A new listener for a deck.
pub fn attach(uid: u64, label: &str) -> Listener {
    let now = crate::now_ms();
    let id = format!("L{:x}", (now ^ uid.rotate_left(13)) & 0xff_ffff_ffff);
    let label: String = label.trim().chars().take(80).collect();
    let l = Listener { id, label: if label.is_empty() { "An agent".into() } else { label }, uid, last_seen: now };
    let mut g = all_listeners();
    g.retain(|x| x.id != l.id);
    g.push(l.clone());
    l
}

pub fn detach(id: &str) -> bool {
    let mut g = all_listeners();
    let n = g.len();
    g.retain(|l| l.id != id);
    n != g.len()
}

/// A call carrying the listener: it is still there. Returns the deck it listens to.
pub fn touch(id: &str) -> Option<u64> {
    let mut g = all_listeners();
    let l = g.iter_mut().find(|l| l.id == id)?;
    l.last_seen = crate::now_ms();
    Some(l.uid)
}

/// The live listeners of a deck.
pub fn listeners(uid: u64) -> Vec<Listener> {
    all_listeners().iter().filter(|l| l.uid == uid).cloned().collect()
}

fn label_of(id: &str) -> Option<String> {
    all_listeners().iter().find(|l| l.id == id).map(|l| l.label.clone())
}
