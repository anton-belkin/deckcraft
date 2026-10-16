//! The decks this app has open, as AItoCraft sees them (`~/aitocraft/PROTOCOL.md` §1, §2): ids
//! `deckcraft:<pid>:<doc uid>`, titles, paths, the frontmost deck, open questions and listeners.
//! No content and no client data: titles and paths only.

use deckcraft_engine::{DocState, Session};
use serde_json::{Value as Json, json};

use crate::layer::{self, QuestionStatus};
use crate::listener;

/// The app's name in the protocol.
pub const APP: &str = "deckcraft";
pub const APP_TITLE: &str = "DeckCraft";

pub fn pid() -> u32 {
    std::process::id()
}

/// A deck's id: unique across apps, stable while it stays open.
pub fn id(uid: u64) -> String {
    format!("{APP}:{}:{uid}", pid())
}

/// The deck uid in an id of this app instance.
pub fn parse(id: &str) -> Option<u64> {
    let mut it = id.trim().split(':');
    let (app, p, uid) = (it.next()?, it.next()?, it.next()?);
    if app != APP || it.next().is_some() || p.parse::<u32>().ok()? != pid() {
        return None;
    }
    uid.parse().ok()
}

/// The index of an open deck by its id.
pub fn index(s: &Session, id: &str) -> Option<usize> {
    let uid = parse(id)?;
    s.documents().iter().position(|d| d.uid == uid)
}

/// What people call the deck: its file name, or its title while unsaved.
pub fn title(d: &DocState) -> String {
    d.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| d.title())
}

static FOCUSED: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());

/// The deck `uid` has focus now (its window came to the front, or it became the active deck).
pub fn focused(uid: u64) {
    let mut g = match FOCUSED.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    g.retain(|(u, _)| *u != uid);
    g.push((uid, crate::now_ms()));
    if g.len() > 256 {
        g.remove(0);
    }
}

/// When the deck last had focus (ms; 0: never in this run).
pub fn focused_at(uid: u64) -> u64 {
    FOCUSED.lock().ok().and_then(|g| g.iter().find(|(u, _)| *u == uid).map(|(_, t)| *t)).unwrap_or(0)
}

/// The registry object of a deck.
pub fn entry(d: &DocState, frontmost: bool) -> Json {
    let l = layer::get(d);
    let open = l.asked().filter(|q| q.status != QuestionStatus::Answered).count();
    let listeners: Vec<Json> =
        listener::listeners(d.uid).iter().map(|x| json!({"listener": x.id, "label": x.label, "lastSeen": x.last_seen})).collect();
    json!({
        "id": id(d.uid),
        "title": title(d),
        "path": d.path.as_deref().and_then(|p| std::path::absolute(p).ok()).map(|p| p.to_string_lossy().to_string()),
        "frontmost": frontmost,
        "focusedAt": focused_at(d.uid),
        "openQuestions": open,
        "listeners": listeners,
    })
}

/// Every open deck.
pub fn list(s: &Session) -> Vec<Json> {
    let active = s.active_index();
    s.documents().iter().enumerate().map(|(i, d)| entry(d, Some(i) == active)).collect()
}
