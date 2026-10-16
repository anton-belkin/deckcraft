//! Links inside text (TOPICS.md §2, §2a). Answers, notes, status text and questions carry
//! Markdown links, `[slide 4](slide:4)`, `[the chart](shape:4/k4f2a9)`, `[3.2](place:3.2)`,
//! `[topic 3](topic:3)`, `[the source](note:3/n2)`, web addresses; a plain `slide 4` is linked by
//! itself.
//!
//! When a text is stored, every link to the deck becomes an anchor (`anchor:<id>`, the slide's or
//! shape's hidden key), topics and places become their ids, so links survive moving and renumbering.
//! A label that was the address is stored empty and shown as the current address: an answer that
//! said "slide 4" says "slide 5" once a slide is inserted before it, with nothing marking the change.

use deckcraft_engine::model::ShapeId;
use deckcraft_engine::{DocState, Result, Session};

use crate::anchor::{self, Index, Place};
use crate::layer::Layer;
use crate::topics::{self, Numbers};

/// A piece of rendered text, with where it leads (a canonical href: `slide:5`, `shape:5/12`,
/// `place:3.2`, `topic:3`, `note:3/n2`, `https://…`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub href: Option<String>,
}

enum Piece<'a> {
    Text(&'a str),
    Link(&'a str, &'a str),
}

/// Splits Markdown links out of a text.
fn pieces(t: &str) -> Vec<Piece<'_>> {
    let mut out = vec![];
    let mut rest = t;
    while let Some(open) = rest.find('[') {
        let after = rest.get(open + 1..).unwrap_or("");
        let Some(close) = after.find("](") else { break };
        let label = after.get(..close).unwrap_or("");
        let tail = after.get(close + 2..).unwrap_or("");
        let Some(end) = tail.find(')') else { break };
        if label.contains('[') || label.contains('\n') {
            out.push(Piece::Text(rest.get(..open + 1).unwrap_or("")));
            rest = after;
            continue;
        }
        out.push(Piece::Text(rest.get(..open).unwrap_or("")));
        out.push(Piece::Link(label, tail.get(..end).unwrap_or("")));
        rest = tail.get(end + 1..).unwrap_or("");
    }
    out.push(Piece::Text(rest));
    out
}

/// `slide 4` (any case) in plain text: its byte range and the slide number.
fn plain_slides(t: &str) -> Vec<(usize, usize, usize)> {
    let lower = t.to_ascii_lowercase();
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = lower.get(from..).and_then(|r| r.find("slide ")).map(|i| i + from) {
        let before_ok = i == 0 || !lower.as_bytes().get(i - 1).is_some_and(|c| c.is_ascii_alphanumeric());
        let digits: String = lower.get(i + 6..).unwrap_or("").chars().take_while(char::is_ascii_digit).collect();
        let end = i + 6 + digits.len();
        let after_ok = !lower.as_bytes().get(end).is_some_and(|c| c.is_ascii_alphanumeric());
        if before_ok
            && after_ok
            && let Ok(n) = digits.parse::<usize>()
        {
            out.push((i, end, n));
        }
        from = i + 6;
    }
    out
}

fn shape_of(s: &Session, slide: usize, x: &str) -> Option<ShapeId> {
    let d = s.doc().ok()?;
    let sl = d.doc.slides.get(slide)?;
    if let Ok(id) = x.parse::<u32>()
        && sl.shape(ShapeId(id)).is_some()
    {
        return Some(ShapeId(id));
    }
    let mut hit = None;
    deckcraft_engine::model::walk(&sl.shapes, &mut |sh, _| {
        if hit.is_none() && anchor::key_of(&sh.tags) == Some(x) {
            hit = Some(sh.id);
        }
    });
    hit
}

/// The stored form of a link target, and whether the label should follow the target.
fn store_target(s: &mut Session, label: &str, target: &str) -> Result<Option<(String, bool)>> {
    let t = target.trim();
    let doc_slides = s.doc()?.doc.slides.len();
    if let Some(n) = t.strip_prefix("slide:").and_then(|n| n.trim().parse::<usize>().ok()) {
        if n == 0 || n > doc_slides {
            return Ok(None);
        }
        let a = anchor::anchor_at(s, &Place::slide_only(n - 1))?;
        let auto = label.trim().eq_ignore_ascii_case(&format!("slide {n}")) || label.trim().is_empty();
        return Ok(Some((format!("anchor:{a}"), auto)));
    }
    if let Some((n, x)) = t.strip_prefix("shape:").and_then(|r| r.split_once('/')) {
        let Some(n) = n.trim().parse::<usize>().ok().filter(|n| *n >= 1 && *n <= doc_slides) else { return Ok(None) };
        let Some(id) = shape_of(s, n - 1, x.trim()) else { return Ok(None) };
        let a = anchor::anchor_at(s, &Place { slide: n - 1, shape: Some(id), ..Default::default() })?;
        return Ok(Some((format!("anchor:{a}"), label.trim().is_empty())));
    }
    let d = s.doc()?;
    let l = crate::layer::get(d);
    let ix = Index::of(&d.doc);
    if let Some(r) = t.strip_prefix("topic:") {
        let Some(id) = topics::find(&d.doc, &ix, &l, r) else { return Ok(None) };
        let auto = label.trim().is_empty() || label.trim().eq_ignore_ascii_case(&format!("topic {}", r.trim()));
        return Ok(Some((format!("topic:{id}"), auto)));
    }
    if let Some(r) = t.strip_prefix("place:") {
        let Some((_, pid)) = topics::find_place(&d.doc, &ix, &l, r) else { return Ok(None) };
        let auto = label.trim().is_empty() || label.trim() == r.trim();
        return Ok(Some((format!("place:{pid}"), auto)));
    }
    if let Some(r) = t.strip_prefix("note:") {
        let nid = r.rsplit('/').next().unwrap_or(r).trim();
        if l.notes.iter().any(|n| n.id == nid) {
            return Ok(Some((format!("note:{nid}"), false)));
        }
        return Ok(None);
    }
    Ok(Some((t.to_string(), false)))
}

/// The stored form of a text: links to the deck become anchors; a plain `slide 4` is linked.
pub fn store(s: &mut Session, text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    for p in pieces(text) {
        match p {
            Piece::Link(label, target) => match store_target(s, label, target)? {
                Some((t, auto)) => out.push_str(&format!("[{}]({t})", if auto { "" } else { label })),
                None => out.push_str(&format!("[{label}]({target})")),
            },
            Piece::Text(t) => {
                let n = s.doc()?.doc.slides.len();
                let mut last = 0;
                for (a, b, k) in plain_slides(t) {
                    if k == 0 || k > n {
                        continue;
                    }
                    let id = anchor::anchor_at(s, &Place::slide_only(k - 1))?;
                    out.push_str(t.get(last..a).unwrap_or(""));
                    out.push_str(&format!("[](anchor:{id})"));
                    last = b;
                }
                out.push_str(t.get(last..).unwrap_or(""));
            }
        }
    }
    Ok(out)
}

/// The text as it reads now: links with their current addresses and numbers.
pub fn spans(d: &DocState, l: &Layer, nums: &Numbers, text: &str) -> Vec<Span> {
    let ix = Index::of(&d.doc);
    let mut out = vec![];
    for p in pieces(text) {
        match p {
            Piece::Text(t) => {
                if !t.is_empty() {
                    out.push(Span { text: t.to_string(), href: None });
                }
            }
            Piece::Link(label, target) => {
                let (shown, href) = if let Some(id) = target.strip_prefix("anchor:") {
                    match l.anchor(id).and_then(|a| anchor::resolve(&d.doc, &ix, a)) {
                        Some(w) => {
                            let now = match w.shape {
                                Some(_) => topics::where_label(&d.doc, &w),
                                None => format!("slide {}", w.slide + 1),
                            };
                            (if label.is_empty() { now } else { label.to_string() }, Some(w.href()))
                        }
                        None => {
                            let was = l.anchor(id).map(|a| a.was.clone()).filter(|w| !w.is_empty()).unwrap_or_else(|| "deleted".into());
                            let shown = if label.is_empty() { format!("removed (was {was})") } else { format!("{label} (removed, was {was})") };
                            (shown, None)
                        }
                    }
                } else if let Some(id) = target.strip_prefix("topic:") {
                    let n = nums.topic(id);
                    if n == 0 {
                        (if label.is_empty() { "a removed topic".into() } else { label.to_string() }, None)
                    } else {
                        (if label.is_empty() { format!("topic {n}") } else { label.to_string() }, Some(format!("topic:{n}")))
                    }
                } else if let Some(id) = target.strip_prefix("place:") {
                    let n = nums.place(id);
                    if n.is_empty() {
                        (if label.is_empty() { "a removed place".into() } else { label.to_string() }, None)
                    } else {
                        (if label.is_empty() { n.clone() } else { label.to_string() }, Some(format!("place:{n}")))
                    }
                } else if let Some(id) = target.strip_prefix("note:") {
                    let topic = l.notes.iter().find(|n| n.id == id).map(|n| nums.topic(&n.topic)).unwrap_or(0);
                    (if label.is_empty() { id.to_string() } else { label.to_string() }, Some(format!("note:{topic}/{id}")))
                } else {
                    (if label.is_empty() { target.to_string() } else { label.to_string() }, Some(target.to_string()))
                };
                out.push(Span { text: shown, href });
            }
        }
    }
    out
}

/// The text for the agent: Markdown links with current addresses (`[slide 5](slide:5)`).
pub fn render(d: &DocState, l: &Layer, nums: &Numbers, text: &str) -> String {
    spans(d, l, nums, text)
        .into_iter()
        .map(|s| match s.href {
            Some(h) => format!("[{}]({h})", s.text),
            None => s.text,
        })
        .collect()
}

/// The text as plain words (labels, previews).
pub fn plain(d: &DocState, l: &Layer, nums: &Numbers, text: &str) -> String {
    spans(d, l, nums, text).into_iter().map(|s| s.text).collect()
}
