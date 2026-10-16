//! Topics (`~/aitocraft/TOPICS.md`): one question or theme about the deck, with places, a status,
//! notes and a discussion. This module numbers them in deck order, resolves their places and says
//! where a removed place was.
//!
//! Deck order: slides in order; on a slide, the slide's own places first, then shapes top to
//! bottom and left to right by their box (a shape's z-order isn't a reading order), then words by
//! where they are in the shape's text. Topics are numbered by their first place, like footnotes;
//! the places of a topic with several are numbered 3.1, 3.2 in the same order.

use std::collections::HashMap;

use deckcraft_engine::DocState;
use deckcraft_engine::model::{Presentation, ShapeId};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Index, Place as Where};
use crate::layer::{Layer, Place, QuestionStatus, Status, Topic};

/// Rows closer than this (points) read as one row.
const ROW: f64 = 24.0;

/// A position in deck order.
pub type Key = (usize, u8, i64, i64, usize);

/// Where a resolved anchor sits in deck order.
pub fn key(doc: &Presentation, w: &Where) -> Key {
    let Some(id) = w.shape else { return (w.slide, 0, 0, 0, 0) };
    let Some(sl) = doc.slides.get(w.slide) else { return (w.slide, 1, 0, 0, 0) };
    let sel = deckcraft_engine::Selection { slide: w.slide, ..Default::default() };
    // A shape inside a group sits where its top-level group does.
    let top = sl.shapes.iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some());
    let (x, y) = top.map(|s| deckcraft_engine::cmd::xfrm_of(doc, &sel, s)).map(|x| (x.x, x.y)).unwrap_or((0.0, 0.0));
    let at = if w.text.is_empty() { 0 } else { sl.shape(id).and_then(|s| s.text.as_ref()).and_then(|t| t.text().find(&w.text)).unwrap_or(0) };
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    (w.slide, 1, (finite(y) / ROW).round() as i64, finite(x).round() as i64, at)
}

/// The topics that count (not undone).
pub fn visible(l: &Layer) -> impl Iterator<Item = &Topic> {
    l.topics.iter().filter(|t| !t.hidden)
}

/// A topic's places that count (not rejected).
pub fn live(t: &Topic) -> impl Iterator<Item = &Place> {
    t.places.iter().filter(|p| !p.rejected)
}

/// The resolved anchors of a place (removed ones left out).
pub fn resolve(doc: &Presentation, ix: &Index, l: &Layer, p: &Place) -> Vec<(String, Where)> {
    p.anchors.iter().filter_map(|a| l.anchor(a).and_then(|x| anchor::resolve(doc, ix, x)).map(|w| (a.clone(), w))).collect()
}

/// Numbers of topics ("3") and places ("3" or "3.2").
#[derive(Clone, Debug, Default)]
pub struct Numbers {
    pub topics: HashMap<String, usize>,
    pub places: HashMap<String, String>,
    /// Topic ids in deck order.
    pub order: Vec<String>,
}

impl Numbers {
    pub fn topic(&self, id: &str) -> usize {
        self.topics.get(id).copied().unwrap_or(0)
    }
    pub fn place(&self, id: &str) -> String {
        self.places.get(id).cloned().unwrap_or_default()
    }
}

pub fn numbers(doc: &Presentation, ix: &Index, l: &Layer) -> Numbers {
    let far: Key = (usize::MAX, u8::MAX, i64::MAX, i64::MAX, usize::MAX);
    let mut topics: Vec<(Key, u64, &Topic, Vec<(Key, &Place)>)> = visible(l)
        .map(|t| {
            let mut ps: Vec<(Key, &Place)> =
                live(t).map(|p| (resolve(doc, ix, l, p).iter().map(|(_, w)| key(doc, w)).min().unwrap_or(far), p)).collect();
            ps.sort_by_key(|a| a.0);
            let k = ps.first().map_or(far, |p| p.0);
            (k, t.at, t, ps)
        })
        .collect();
    topics.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut n = Numbers::default();
    for (i, (_, _, t, ps)) in topics.iter().enumerate() {
        let no = i + 1;
        n.topics.insert(t.id.clone(), no);
        n.order.push(t.id.clone());
        for (k, (_, p)) in ps.iter().enumerate() {
            let label = if ps.len() == 1 { no.to_string() } else { format!("{no}.{}", k + 1) };
            n.places.insert(p.id.clone(), label);
        }
    }
    n
}

/// The numbers of a document.
pub fn numbers_of(d: &DocState, l: &Layer) -> Numbers {
    numbers(&d.doc, &Index::of(&d.doc), l)
}

/// "slide 4", "slide 4 › Revenue table", "slide 4 › TextBox 3 › “42%”".
pub fn where_label(doc: &Presentation, w: &Where) -> String {
    let mut s = format!("slide {}", w.slide + 1);
    if let Some(id) = w.shape
        && let Some(sh) = doc.slides.get(w.slide).and_then(|sl| sl.shape(id))
    {
        s.push_str(" › ");
        s.push_str(&anchor::shape_label(sh));
    }
    if let Some((r, c)) = w.cell {
        s.push_str(&format!(" › cell {},{}", r + 1, c + 1));
    }
    if !w.text.is_empty() {
        let t: String = w.text.chars().take(40).collect();
        s.push_str(&format!(" › “{t}”"));
    }
    s
}

/// A place in words: where it is now, or where it was.
pub fn place_label(doc: &Presentation, ix: &Index, l: &Layer, p: &Place) -> String {
    let found = resolve(doc, ix, l, p);
    if found.is_empty() {
        let was = p.anchors.iter().filter_map(|a| l.anchor(a)).map(|a| a.was.clone()).find(|w| !w.is_empty()).unwrap_or_else(|| "deleted".into());
        return format!("removed — was {was}");
    }
    let mut labels: Vec<String> = found.iter().map(|(_, w)| where_label(doc, w)).collect();
    labels.dedup();
    if labels.len() > 3 {
        let n = labels.len();
        labels.truncate(2);
        labels.push(format!("{} more", n - 2));
    }
    labels.join(" + ")
}

/// Where an existing shape (or the slide) is, to say later where it was: "on slide 4, after the
/// chart", "on slide 4, first on the slide", "slide 4, after slide 3".
pub fn whereabouts(doc: &Presentation, w: &Where) -> String {
    let Some(sl) = doc.slides.get(w.slide) else { return format!("on slide {}", w.slide + 1) };
    let Some(id) = w.shape else {
        return match w.slide.checked_sub(1).and_then(|i| doc.slides.get(i)) {
            Some(prev) => {
                let t = prev.title();
                if t.is_empty() {
                    format!("slide {}, after slide {}", w.slide + 1, w.slide)
                } else {
                    format!("slide {}, after slide {} “{t}”", w.slide + 1, w.slide)
                }
            }
            None => "the first slide".into(),
        };
    };
    let sel = deckcraft_engine::Selection { slide: w.slide, ..Default::default() };
    let mut order: Vec<(i64, i64, ShapeId, String)> = sl
        .shapes
        .iter()
        .map(|s| {
            let x = deckcraft_engine::cmd::xfrm_of(doc, &sel, s);
            let f = |v: f64| if v.is_finite() { v } else { 0.0 };
            ((f(x.y) / ROW).round() as i64, f(x.x).round() as i64, s.id, anchor::shape_label(s))
        })
        .collect();
    order.sort();
    let top = sl.shapes.iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some()).map(|s| s.id);
    match order.iter().position(|o| Some(o.2) == top) {
        Some(0) | None => format!("on slide {}, first on the slide", w.slide + 1),
        Some(i) => format!("on slide {}, after {}", w.slide + 1, order.get(i - 1).map(|o| o.3.clone()).unwrap_or_default()),
    }
}

/// The topics a place of the deck touches (the shape, or the slide when no shape), for the
/// Selection tab and for asking on a selection that already has a topic.
pub fn touching(doc: &Presentation, ix: &Index, l: &Layer, at: &Where) -> Vec<String> {
    visible(l)
        .filter(|t| {
            live(t).any(|p| {
                resolve(doc, ix, l, p)
                    .iter()
                    .any(|(_, w)| w.slide == at.slide && (w.shape == at.shape || at.shape.is_some() && w.shape.is_some() && same_top(doc, w, at)))
            })
        })
        .map(|t| t.id.clone())
        .collect()
}

/// Two shapes in one top-level group touch each other's topics.
fn same_top(doc: &Presentation, a: &Where, b: &Where) -> bool {
    let Some(sl) = doc.slides.get(a.slide) else { return false };
    let top = |id: ShapeId| sl.shapes.iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some()).map(|s| s.id);
    match (a.shape, b.shape) {
        (Some(x), Some(y)) => top(x).is_some() && top(x) == top(y),
        _ => false,
    }
}

/// Unread answers in a topic.
pub fn unread(l: &Layer, t: &Topic) -> usize {
    l.questions.iter().filter(|q| q.topic == t.id && !q.hidden && q.status == QuestionStatus::Answered && !q.seen).count()
}

/// The agent is working on a question of the topic.
pub fn working(l: &Layer, t: &Topic) -> bool {
    l.questions.iter().any(|q| q.topic == t.id && !q.hidden && matches!(q.status, QuestionStatus::Working | QuestionStatus::Transcribing))
}

/// "Agreed 10 of 15 · 2 revise · 3 open".
pub fn readiness(l: &Layer) -> (usize, usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0, 0);
    for t in visible(l) {
        c.0 += 1;
        match t.status {
            Status::Agreed => c.1 += 1,
            Status::Revise => c.2 += 1,
            Status::Open => c.3 += 1,
            Status::Info => c.4 += 1,
        }
    }
    c
}

pub fn readiness_text(l: &Layer) -> String {
    let (all, agreed, revise, open, info) = readiness(l);
    if all == 0 {
        return "No topics yet".into();
    }
    // For-information topics need no decision: sign-off counts the rest.
    let decide = all - info;
    let mut s = format!("Agreed {agreed} of {decide}");
    if revise > 0 {
        s.push_str(&format!(" · {revise} revise"));
    }
    if open > 0 {
        s.push_str(&format!(" · {open} open"));
    }
    if info > 0 {
        s.push_str(&format!(" · {info} for information"));
    }
    s
}

/// A topic id from `t5`, a number (`3`) or `topic:3`.
pub fn find(doc: &Presentation, ix: &Index, l: &Layer, r: &str) -> Option<String> {
    let r = r.trim().trim_start_matches("topic:");
    if l.topic(r).is_some() {
        return Some(r.to_string());
    }
    let n: usize = r.parse().ok()?;
    let nums = numbers(doc, ix, l);
    nums.order.get(n.checked_sub(1)?).cloned()
}

/// A place id from `p7`, a number (`3.2`, or `3` for a one-place topic) or `place:3.2`.
pub fn find_place(doc: &Presentation, ix: &Index, l: &Layer, r: &str) -> Option<(String, String)> {
    let r = r.trim().trim_start_matches("place:");
    for t in &l.topics {
        if let Some(p) = t.places.iter().find(|p| p.id == r) {
            return Some((t.id.clone(), p.id.clone()));
        }
    }
    let nums = numbers(doc, ix, l);
    let pid = nums.places.iter().find(|(_, label)| label.as_str() == r).map(|(id, _)| id.clone())?;
    let tid = l.topics.iter().find(|t| t.places.iter().any(|p| p.id == pid))?.id.clone();
    Some((tid, pid))
}

/// A topic for the agent (`list_topics`, `get_topic`): places resolved, status, notes, the
/// discussion when `full`.
pub fn json(d: &DocState, l: &Layer, nums: &Numbers, t: &Topic, full: bool, render: &dyn Fn(&str) -> String) -> Json {
    let ix = Index::of(&d.doc);
    let places: Vec<Json> = live(t)
        .map(|p| {
            let found = resolve(&d.doc, &ix, l, p);
            let mut o = json!({
                "place": nums.place(&p.id),
                "id": p.id,
                "where": render(&place_label(&d.doc, &ix, l, p)),
                "by": p.by,
            });
            if found.is_empty() {
                o["removed"] = json!(true);
            }
            if let Some((_, w)) = found.first() {
                o["slide"] = json!(w.slide + 1);
                o["href"] = json!(w.href());
                if let Some(id) = w.shape {
                    o["shape"] = json!(id.0);
                }
                let shapes: Vec<u32> = found.iter().filter_map(|(_, w)| w.shape.map(|s| s.0)).collect();
                if shapes.len() > 1 {
                    o["shapes"] = json!(shapes);
                }
            }
            if !p.reason.is_empty() {
                o["reason"] = json!(render(&p.reason));
            }
            o
        })
        .collect();
    let notes: Vec<Json> = l
        .notes
        .iter()
        .filter(|n| n.topic == t.id && !n.deleted)
        .map(|n| {
            let mut o = json!({"id": n.id, "kind": n.kind, "title": render(&n.title), "by": n.author});
            if full {
                o["text"] = json!(render(&n.text));
                if !n.url.is_empty() {
                    o["url"] = json!(n.url);
                    o["summary"] = json!(render(&n.summary));
                }
            }
            o
        })
        .collect();
    let mut o = json!({
        "topic": nums.topic(&t.id),
        "id": t.id,
        "status": t.status.id(),
        "statusText": render(&t.status_text),
        "statusBy": t.status_by,
        "places": places,
        "notes": notes,
        "unread": unread(l, t),
        "withAgent": working(l, t),
    });
    if !t.proposal.is_empty() {
        o["proposedAgreed"] = json!(render(&t.proposal));
    }
    if let Some(prop) = &t.draft {
        o["proposal"] = crate::proposals::json(prop, render);
    }
    if full {
        let disc: Vec<Json> = l
            .questions
            .iter()
            .filter(|q| q.topic == t.id && !q.hidden && !q.is_ping())
            .map(|q| {
                let mut e = json!({"id": q.id, "question": render(&q.text), "status": q.status, "askedAt": q.asked_at});
                if let Some(a) = &q.answer {
                    e["answer"] = json!(render(&a.text));
                }
                e
            })
            .collect();
        o["discussion"] = json!(disc);
    }
    o
}

/// The topic's id of a slide-level or shape-level anchor place, for tests and the UI.
pub fn places_on(doc: &Presentation, ix: &Index, l: &Layer, slide: usize) -> Vec<(String, String, Vec<Where>)> {
    let mut out = vec![];
    for t in visible(l) {
        for p in live(t) {
            let ws: Vec<Where> = resolve(doc, ix, l, p).into_iter().map(|(_, w)| w).filter(|w| w.slide == slide).collect();
            if !ws.is_empty() {
                out.push((t.id.clone(), p.id.clone(), ws));
            }
        }
    }
    out
}
