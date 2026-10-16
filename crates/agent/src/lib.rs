//! The agent layer (a DeckCraft fork feature): a layer of interaction with the presenter's own
//! agent that lives in the deck, not in a chat.
//!
//! - Questions asked on a slide, a shape, words in it or a table cell go into a queue in the
//!   deck; the agent picks them up through the in-app MCP server (watching `/events`), answers,
//!   and the shape shows "answer ready".
//! - Notes the agent leaves: prepared answers to questions the audience is likely to ask (per
//!   slide, shown to the presenter in Presenter View too), explanations, assumptions, sources and
//!   checks, with links to slides, shapes, other notes and the web.
//! - A privacy gate: text marked as client data is replaced by pseudonyms in everything the agent
//!   reads, and the agent can't change it.
//! - A decision history and an audit trail of who (person or agent) changed what, kept in the
//!   `.pptx` as a custom XML part.
//! - Excel → slide links: cells copied from GridCraft as a linked source paste as a table or a
//!   picture that remembers its source, notices when the numbers there change, and opens it.
//!
//! Wiring: [`install`] registers the `agent.*`/`link.*` commands and an observer with the engine
//! (`deckcraft_engine::ext`); the app starts the MCP server with [`server::start`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod anchor;
pub mod commands;
pub mod layer;
pub mod links;
#[cfg(not(target_arch = "wasm32"))]
pub mod mcp;
pub mod privacy;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod xlsx;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deckcraft_engine::ext::Observer;
use deckcraft_engine::{EngineError, Result, Session};
use serde_json::Value as Json;

pub use layer::{Actor, Layer};

/// Milliseconds since the Unix epoch (0 where the platform has no clock).
pub fn now_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// Registers the agent and link commands and the observer that keeps the layer in step with the
/// presentation. Safe to call more than once.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        deckcraft_engine::ext::register_commands(commands::SPECS);
        deckcraft_engine::ext::register_observer(&KEEPER);
    });
}

/// The layer of the active document.
pub fn active_layer(s: &Session) -> Option<Arc<Layer>> {
    s.active().map(layer::get)
}

static PASTE_PICTURE: AtomicBool = AtomicBool::new(false);

/// How a plain paste of linked cells lands: a picture (`true`) or a table (default).
pub fn set_paste_as_picture(on: bool) {
    PASTE_PICTURE.store(on, Ordering::Relaxed);
}

pub fn paste_as_picture() -> bool {
    PASTE_PICTURE.load(Ordering::Relaxed)
}

struct Keeper;
static KEEPER: Keeper = Keeper;

thread_local! {
    /// The commands running now (nested ones stack up): id and the document before.
    static RUNNING: std::cell::RefCell<Vec<(String, Option<(u64, usize)>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn doc_ptr(s: &Session) -> Option<(u64, usize)> {
    s.active().map(|d| (d.uid, Arc::as_ptr(&d.doc) as usize))
}

/// Commands that never change what the deck says (the history skips them).
fn is_view_only(id: &str) -> bool {
    id.starts_with("agent.")
        || id.starts_with("selection.")
        || id.starts_with("view.")
        || id.starts_with("file.")
        || id.starts_with("ui.")
        || id.starts_with("zoom.")
        || matches!(
            id,
            "slide.next"
                | "slide.previous"
                | "slide.go"
                | "slide.first"
                | "slide.last"
                | "slide.selectSlides"
                | "edit.copy"
                | "link.check"
                | "link.open"
                | "link.goto"
                | "text.edit"
        )
}

impl Observer for Keeper {
    fn before(&self, s: &mut Session, id: &str, _params: &Json) {
        RUNNING.with(|b| b.borrow_mut().push((id.to_string(), doc_ptr(s))));
        // Write the layer into the presentation just before it is saved.
        if matches!(id, "file.save" | "file.saveAs" | "file.saveBytes" | "file.saveTemplate")
            && let Some(d) = s.active_mut()
        {
            let l = layer::get(d);
            let has_part = d.doc.kept_parts.iter().any(layer::is_ours);
            if !l.is_empty() || has_part {
                let mut doc = (*d.doc).clone();
                layer::write_into(&mut doc, &l);
                // Saving makes this the saved state: the part isn't an unsaved change.
                d.doc = Arc::new(doc);
            }
        }
    }

    fn intercept(&self, s: &mut Session, id: &str, params: &Json) -> Option<Result<Json>> {
        if id != "edit.paste" {
            return None;
        }
        let payload = links::from_html(params.get("html").and_then(Json::as_str)?)?;
        let mode = if paste_as_picture() { links::Mode::Picture } else { links::Mode::Table };
        Some(links::paste(s, payload, mode, None))
    }

    fn after(&self, s: &mut Session, id: &str, params: &Json, result: &Result<Json>) {
        let (before, parent) = RUNNING.with(|b| {
            let mut b = b.borrow_mut();
            let me = b.pop().and_then(|(_, p)| p);
            (me, b.last().map(|(p, _)| p.clone()))
        });
        if id == "file.close"
            && let Some((uid, _)) = before
            && s.documents().iter().all(|d| d.uid != uid)
        {
            layer::forget(uid);
            return;
        }
        if result.is_ok() && matches!(id, "file.save" | "file.saveAs") {
            links::register_uses(s);
        }
        let Some(d) = s.active_mut() else { return };
        anchor::refresh(d);
        // Only what a person did, or what the agent ran through agent.exec: not the steps a
        // command takes inside (they are part of the outer one).
        let nested = parent.as_deref().is_some_and(|p| p != "agent.exec");
        let changed = result.is_ok() && before.is_some_and(|(uid, ptr)| uid == d.uid && ptr != Arc::as_ptr(&d.doc) as usize);
        if nested || !changed || is_view_only(id) || id == "edit.undo" || id == "edit.redo" {
            return;
        }
        log_change(s, id, params, parent.as_deref() == Some("agent.exec"));
    }
}

/// Records a change in the history. People's own edits are recorded once the deck has an agent
/// layer (a plain deck stays plain).
pub(crate) fn log_change(s: &mut Session, id: &str, params: &Json, agent: bool) {
    let Some(d) = s.active() else { return };
    if !agent && layer::get(d).is_empty() {
        return;
    }
    let (reason, source) = if agent { commands::PENDING_REASON.with(|r| r.borrow().clone()).unwrap_or_default() } else { Default::default() };
    let label = deckcraft_engine::find_command(id).map(|c| c.label.trim_end_matches('…').to_string()).unwrap_or_else(|| id.to_string());
    let slide = d.selection.slide;
    let shape = privacy::command_targets(s, params).first().and_then(|(_, id)| d.shape(*id)).map(anchor::shape_label);
    let place = match shape {
        Some(n) => format!("slide {} › {n}", slide + 1),
        None => format!("slide {}", slide + 1),
    };
    let text = if reason.is_empty() { format!("{label} on {place}") } else { format!("{label} on {place}: {reason}") };
    let actor = if agent { Actor::Agent } else { Actor::Human };
    layer::update(d, |l| l.log(actor, "change", text, id, None, source));
}

#[cfg(test)]
mod tests;
