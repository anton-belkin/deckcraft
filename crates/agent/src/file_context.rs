//! The file's own context (next-brief E): what the deck is, for whom, house rules, background,
//! facts the agent gathered. Not tied to a place; kept in the file and when the deck is sent. The
//! agent reads a short summary with `get_context` and with every question.
//!
//! MCP: `get_file_context {document}`, `add_file_context {document, title, kind, text}` → id,
//! `update_file_context {document, id, title?, text?}`, `remove_file_context {document, id}`. The
//! person edits it from the pane's **File context** button; those edits are undoable.

use deckcraft_engine::{Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::commands::{actor_p, str_p, text_p};
use crate::layer::{self, Actor, Layer};
use crate::undo::{self, Op};
use crate::{bad, now_ms, textlinks};

/// The kinds of entries.
pub const KINDS: &[&str] = &["about", "audience", "rules", "background", "fact"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub text: String,
    pub by: Actor,
    pub at: u64,
}

fn kind_p(p: &Json) -> String {
    let k = str_p(p, "kind").unwrap_or("background").trim().to_lowercase();
    if KINDS.contains(&k.as_str()) { k } else { "background".into() }
}

/// The entries as the agent reads them.
pub fn list(s: &mut Session, p: &Json) -> Result<Json> {
    let m = if crate::commands::masked_p(p) { crate::privacy::Masker::new(s) } else { crate::privacy::Masker::off() };
    let d = s.doc()?;
    let l = layer::get(d);
    let nums = crate::topics::numbers_of(d, &l);
    Ok(Json::Array(
        l.file_context
            .iter()
            .map(|e| {
                json!({"id": e.id, "title": m.text(&e.title), "kind": e.kind, "text": m.text(&textlinks::render(d, &l, &nums, &e.text)), "by": e.by, "at": e.at})
            })
            .collect(),
    ))
}

/// A short summary for `get_context` and each question: the titles, and the start of each text.
pub fn summary(l: &Layer) -> String {
    let mut out: Vec<String> = vec![];
    for e in &l.file_context {
        let head: String = e.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
        let more = if e.text.chars().count() > 120 { "…" } else { "" };
        out.push(format!("{} ({}): {head}{more}", e.title, e.kind));
    }
    out.join("\n")
}

fn change(s: &mut Session, p: &Json, f: impl FnOnce(&mut Vec<Entry>, Actor) -> std::result::Result<Json, String>, cmd: &str) -> Result<Json> {
    let by = actor_p(p, Actor::Human);
    let d = s.doc()?;
    let before = layer::get(d).file_context.clone();
    let r = layer::update(d, |l| {
        let mut list = l.file_context.clone();
        let r = f(&mut list, by)?;
        l.file_context = list;
        Ok(r)
    });
    let r = r.map_err(|e: String| bad(cmd, e))?;
    if by == Actor::Human {
        let after = layer::get(s.doc()?).file_context.clone();
        undo::push(s, Op::FileContext { before, after });
    }
    Ok(r)
}

/// `agent.addFileContext {title, kind, text}` → id.
pub fn add(s: &mut Session, p: &Json) -> Result<Json> {
    let title: String = text_p(p, "title", "agent.addFileContext")?.chars().take(300).collect();
    let text = textlinks::store(s, &text_p(p, "text", "agent.addFileContext")?)?;
    let kind = kind_p(p);
    let id = layer::update(s.doc()?, |l| l.fresh_id("c"));
    let i = id.clone();
    change(
        s,
        p,
        move |list, by| {
            list.push(Entry { id: i.clone(), title, kind, text, by, at: now_ms() });
            Ok(json!({"id": i}))
        },
        "agent.addFileContext",
    )
}

/// `agent.updateFileContext {id, title?, text?, kind?}`.
pub fn update(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.updateFileContext")?;
    let title: Option<String> = str_p(p, "title").map(|t| t.trim().chars().take(300).collect()).filter(|t: &String| !t.is_empty());
    let text = match str_p(p, "text").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(textlinks::store(s, t)?),
        None => None,
    };
    let kind = str_p(p, "kind").map(|_| kind_p(p));
    change(
        s,
        p,
        move |list, by| {
            let e = list.iter_mut().find(|e| e.id == id).ok_or_else(|| format!("no entry `{id}`"))?;
            if let Some(t) = title {
                e.title = t;
            }
            if let Some(t) = text {
                e.text = t;
            }
            if let Some(k) = kind {
                e.kind = k;
            }
            e.by = by;
            e.at = now_ms();
            Ok(json!({"id": id}))
        },
        "agent.updateFileContext",
    )
}

/// `agent.removeFileContext {id}`.
pub fn remove(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeFileContext")?;
    change(
        s,
        p,
        move |list, _| {
            let n = list.len();
            list.retain(|e| e.id != id);
            if list.len() == n {
                return Err(format!("no entry `{id}`"));
            }
            Ok(json!({"id": id}))
        },
        "agent.removeFileContext",
    )
}
