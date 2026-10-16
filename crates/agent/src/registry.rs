//! The AItoCraft registry file (`~/aitocraft/PROTOCOL.md` §1): `deckcraft-<pid>.json` in the
//! shared `ArtCraft/agents/` folder, saying which decks are open and where the MCP server is.
//! Written atomically (a temp file, then a rename), at most once a second when something changed,
//! and every 10 s as a heartbeat; deleted when the app quits.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::documents;

pub const PROTOCOL: &str = "aitocraft/1";
/// Most often a change is written.
const DEBOUNCE_MS: u64 = 1_000;
/// The heartbeat.
const HEARTBEAT_MS: u64 = 10_000;

struct Reg {
    mcp: String,
    documents: Vec<Json>,
    written: Option<Vec<Json>>,
    last_write: u64,
    started: bool,
}

static REG: Mutex<Option<Reg>> = Mutex::new(None);
static DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn reg() -> std::sync::MutexGuard<'static, Option<Reg>> {
    match REG.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Points the registry at another folder (tests, demos).
pub fn set_dir(d: Option<PathBuf>) {
    if let Ok(mut g) = DIR_OVERRIDE.lock() {
        *g = d;
    }
}

/// The shared folder. In tests: only an override (never the user's real registry).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = DIR_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return Some(d);
    }
    if cfg!(test) {
        return None;
    }
    artcraft_dir().map(|d| d.join("agents"))
}

/// `ArtCraft` in the user's data folder.
pub fn artcraft_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/ArtCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ArtCraft"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("ArtCraft"))
    }
}

/// This instance's file.
pub fn file() -> Option<PathBuf> {
    dir().map(|d| d.join(format!("{}-{}.json", documents::APP, documents::pid())))
}

/// The file's content.
pub fn content(mcp: &str, docs: &[Json]) -> Json {
    json!({
        "protocol": PROTOCOL,
        "app": documents::APP,
        "appTitle": documents::APP_TITLE,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": documents::pid(),
        "mcp": mcp,
        "heartbeat": crate::now_ms(),
        "documents": docs,
    })
}

fn write(mcp: &str, docs: &[Json]) {
    let (Some(dir), Some(path)) = (dir(), file()) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!(".{}-{}.json.tmp", documents::APP, documents::pid()));
    let text = serde_json::to_string_pretty(&content(mcp, docs)).unwrap_or_default();
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The documents now (call every frame; writes are debounced, the heartbeat runs on its own).
pub fn publish(mcp: &str, docs: Vec<Json>) {
    let start = {
        let mut g = reg();
        let r = g.get_or_insert_with(|| Reg { mcp: mcp.to_string(), documents: vec![], written: None, last_write: 0, started: false });
        r.mcp = mcp.to_string();
        r.documents = docs;
        let first = !r.started;
        r.started = true;
        first
    };
    flush(false);
    if start {
        let _ = std::thread::Builder::new().name("aitocraft-registry".into()).spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if !flush(false) {
                    return;
                }
            }
        });
    }
}

/// Writes when something changed (debounced) or the heartbeat is due; `force` writes now.
/// Returns false once the registry was removed (the heartbeat stops).
pub fn flush(force: bool) -> bool {
    let job = {
        let mut g = reg();
        let Some(r) = g.as_mut() else { return false };
        let now = crate::now_ms();
        let since = now.saturating_sub(r.last_write);
        let changed = r.written.as_ref() != Some(&r.documents);
        if force || (changed && since >= DEBOUNCE_MS) || since >= HEARTBEAT_MS {
            r.written = Some(r.documents.clone());
            r.last_write = now;
            Some((r.mcp.clone(), r.documents.clone()))
        } else {
            None
        }
    };
    if let Some((mcp, docs)) = job {
        write(&mcp, &docs);
    }
    true
}

/// Deletes the file (the app quits) and stops the heartbeat.
pub fn remove() {
    reg().take();
    if let Some(p) = file() {
        let _ = std::fs::remove_file(p);
    }
}
