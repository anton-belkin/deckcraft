//! Proposals instead of direct edits (the brief shared by the three apps, part B).
//!
//! In a topic the agent doesn't change the deck: `run_command {…, topic, option?}` runs the
//! command on the topic's **draft** (a copy of the accepted deck with the option's commands
//! replayed on it), which validates it, and records it in the topic's proposal. A proposal has
//! options (A, B, …), each its own list of commands with a title and a why (`describe_option`).
//! The person applies one option in the document (**one** undo step, the person's action, with a
//! decision note "chose A: …") or rejects the proposal (a comment comes back to the agent as a
//! follow-up in the topic). Options are built against the accepted deck: when a step doesn't fit
//! any more nothing is applied and the proposal is out of date; applying one proposal makes the
//! pending ones that touch the same shapes out of date. Reads mark the shapes a pending proposal
//! touches (`pending`), questions carry the proposals near them (`pendingNearby`), and the agent's
//! event stream tells it `proposal_applied`, `proposal_rejected` and `proposal_outdated`.

use std::sync::Arc;

use deckcraft_engine::model::Presentation;
use deckcraft_engine::{DocState, Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Index, Place as Where};
use crate::commands::str_p;
use crate::layer::{self, Actor, Layer, Note, Topic};
use crate::undo::{self, Op};
use crate::{bad, now_ms, topics};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Pending,
    Applied,
    Rejected,
    Outdated,
}

impl State {
    pub fn id(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Applied => "applied",
            State::Rejected => "rejected",
            State::Outdated => "outdated",
        }
    }
}

/// One recorded command.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Change {
    pub command: String,
    pub params: Json,
    pub summary: String,
    /// The slide it acts on (its id; 0: the current slide): commands act on the edited slide.
    #[serde(skip_serializing_if = "is_zero")]
    pub slide: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// An option of a proposal: its commands, title and why, and the shapes and slides it touches
/// (anchors in the accepted deck).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Choice {
    pub option: String,
    pub title: String,
    pub why: String,
    pub changes: Vec<Change>,
    pub touched: Vec<String>,
}

impl Choice {
    /// One line: the title, or the first changes.
    pub fn summary(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        let mut s: Vec<String> = self.changes.iter().take(2).map(|c| c.summary.clone()).collect();
        if self.changes.len() > 2 {
            s.push(format!("{} more", self.changes.len() - 2));
        }
        s.join("; ")
    }
}

/// A topic's proposal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Proposal {
    pub options: Vec<Choice>,
    pub state: State,
    /// The option applied.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub chosen: String,
    /// The person's comment on a reject.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    pub at: u64,
}

impl Proposal {
    pub fn option(&self, o: &str) -> Option<&Choice> {
        self.options.iter().find(|c| c.option == o)
    }
}

/// What the agent's event stream tells it about proposals (memory only).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Event {
    pub seq: u64,
    pub event: String,
    pub topic: String,
    pub option: String,
    pub comment: String,
}

fn event(l: &mut Layer, kind: &str, topic: &str, option: &str, comment: &str) {
    let seq = l.events.last().map_or(1, |e| e.seq + 1);
    l.events.push(Event { seq, event: kind.into(), topic: topic.into(), option: option.into(), comment: comment.into() });
    let n = l.events.len();
    if n > 200 {
        l.events.drain(..n - 200);
    }
}

// ---------------------------------------------------------------- drafts

/// A scratch session holding a copy of the deck, with the same selection.
fn scratch(d: &DocState, doc: Presentation) -> Session {
    let mut s = Session::new();
    let mut st = DocState::new(doc, None, "draft".into());
    st.selection = d.selection.clone();
    let i = s.add_document(st);
    s.set_active(i);
    s
}

/// The accepted deck with an option's commands replayed on it, or the step that no longer fits.
pub fn replay(d: &DocState, changes: &[Change]) -> std::result::Result<Presentation, (usize, String)> {
    let mut s = scratch(d, (*d.doc).clone());
    let uid = s.doc().map(|x| x.uid).unwrap_or_default();
    crate::commands::AGENT_DEPTH.with(|x| x.set(x.get().saturating_add(1)));
    let mut out = Ok(());
    for (i, c) in changes.iter().enumerate() {
        // On its slide, as when it was recorded.
        if c.slide != 0
            && let Ok(st) = s.doc_mut()
            && let Some(k) = st.doc.slides.iter().position(|x| x.id.0 == c.slide)
            && st.selection.slide != k
        {
            st.selection = deckcraft_engine::Selection { slide: k, ..Default::default() };
        }
        if let Err(e) = s.execute(&c.command, &c.params) {
            out = Err((i, e.to_string()));
            break;
        }
    }
    crate::commands::AGENT_DEPTH.with(|x| x.set(x.get().saturating_sub(1)));
    let doc = s.doc().map(|x| (*x.doc).clone());
    layer::forget(uid);
    out?;
    doc.map_err(|e| (0, e.to_string()))
}

thread_local! {
    /// Drafts, by (deck, topic, option, changes, accepted deck): the preview asks every frame.
    static DRAFTS: std::cell::RefCell<Vec<((u64, String, String, usize, usize), Option<Arc<Presentation>>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// An option's draft (cached), for the preview and `read_slides {view: "proposal"}`.
pub fn draft(d: &DocState, topic: &str, c: &Choice) -> Option<Arc<Presentation>> {
    let key = (d.uid, topic.to_string(), c.option.clone(), c.changes.len(), Arc::as_ptr(&d.doc) as usize);
    if let Some(hit) = DRAFTS.with(|m| m.borrow().iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())) {
        return hit;
    }
    let v = replay(d, &c.changes).ok().map(Arc::new);
    DRAFTS.with(|m| {
        let mut m = m.borrow_mut();
        m.push((key, v.clone()));
        let n = m.len();
        if n > 32 {
            m.drain(..n - 32);
        }
    });
    v
}

/// What a draft changes compared with the accepted deck: shapes changed or removed, and slides
/// with shapes added or that were added or removed (as places in the accepted deck).
pub fn touched(real: &Presentation, draft: &Presentation) -> Vec<Where> {
    let mut out: Vec<Where> = vec![];
    let mut push = |w: Where| {
        if !out.contains(&w) {
            out.push(w);
        }
    };
    for (i, sl) in real.slides.iter().enumerate() {
        let Some(ds) = draft.slides.iter().find(|x| x.id == sl.id) else {
            push(Where::slide_only(i));
            continue;
        };
        if ds == sl {
            continue;
        }
        let mut any_shape = false;
        for sh in &sl.shapes {
            match ds.shapes.iter().find(|x| x.id == sh.id) {
                Some(x) if x == sh => {}
                _ => {
                    any_shape = true;
                    push(Where { slide: i, shape: Some(sh.id), ..Default::default() });
                }
            }
        }
        if !any_shape || ds.shapes.iter().any(|x| !sl.shapes.iter().any(|y| y.id == x.id)) {
            push(Where::slide_only(i));
        }
    }
    // Slides only in the draft: where they would go.
    for (i, ds) in draft.slides.iter().enumerate() {
        if !real.slides.iter().any(|x| x.id == ds.id) {
            push(Where::slide_only(i.min(real.slides.len().saturating_sub(1))));
        }
    }
    out
}

/// The places (slide, shape) an option touches now.
pub fn touched_now(doc: &Presentation, ix: &Index, l: &Layer, c: &Choice) -> Vec<Where> {
    c.touched.iter().filter_map(|a| l.anchor(a)).filter_map(|a| anchor::resolve(doc, ix, a)).collect()
}

// ---------------------------------------------------------------- the agent's side

fn topic_of(s: &Session, p: &Json, cmd: &str) -> Result<String> {
    let r = p.get("topic").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).ok_or_else(|| bad(cmd, "missing `topic`"))?;
    let d = s.doc()?;
    let l = layer::get(d);
    topics::find(&d.doc, &Index::of(&d.doc), &l, &r).ok_or_else(|| bad(cmd, format!("no topic `{r}`")))
}

fn option_p(p: &Json) -> String {
    let o = str_p(p, "option").map(str::trim).filter(|o| !o.is_empty()).unwrap_or("A");
    o.chars().take(40).collect()
}

/// `run_command {…, topic, option?}` (from `agent.exec`, after its privacy checks): runs the
/// command on the option's draft, which validates it, and records it.
pub fn record(s: &mut Session, p: &Json, command: &str, params: &Json, reason: &str) -> Result<Json> {
    let tid = topic_of(s, p, "agent.exec")?;
    let option = option_p(p);
    let label = deckcraft_engine::find_command(command).map(|c| c.label).unwrap_or(command);
    let summary = if reason.is_empty() { label.to_string() } else { format!("{label}: {reason}") };
    let d = s.doc()?;
    let l = layer::get(d);
    let t = l.topic(&tid).ok_or_else(|| bad("agent.exec", "no such topic"))?;
    // A proposal that was applied, rejected or is out of date: a new one starts.
    let mut prop = t.draft.clone().filter(|x| x.state == State::Pending).unwrap_or_default();
    let mut choice = prop.option(&option).cloned().unwrap_or_else(|| Choice { option: option.clone(), ..Default::default() });
    // The slide it acts on: named, the one holding the shape it names, or the current one.
    let named = p.get("slide").and_then(Json::as_u64).and_then(|n| usize::try_from(n).ok()).and_then(|n| n.checked_sub(1));
    let shape = params.get("id").or_else(|| params.get("ids").and_then(|v| v.get(0))).and_then(Json::as_u64).and_then(|v| u32::try_from(v).ok());
    let on = named
        .or_else(|| shape.and_then(|id| d.doc.slides.iter().position(|sl| sl.shape(deckcraft_engine::model::ShapeId(id)).is_some())))
        .unwrap_or(d.selection.slide);
    let slide = d.doc.slides.get(on).map_or(0, |sl| sl.id.0);
    choice.changes.push(Change { command: command.to_string(), params: params.clone(), summary: summary.chars().take(300).collect(), slide });
    let result = replay(d, &choice.changes).map_err(|(i, e)| bad("agent.exec", format!("step {} of option {option} doesn't fit: {e}", i + 1)))?;
    let wheres = touched(&d.doc, &result);
    let mut anchors = vec![];
    for w in &wheres {
        anchors.push(anchor::anchor_at(s, w)?);
    }
    choice.touched = anchors;
    let n = choice.changes.len();
    match prop.options.iter_mut().find(|c| c.option == option) {
        Some(c) => *c = choice,
        None => prop.options.push(choice),
    }
    prop.state = State::Pending;
    prop.at = now_ms();
    let id = tid.clone();
    layer::update(s.doc()?, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.draft = Some(prop);
        }
        l.drafting.retain(|(t, _)| *t != id);
        l.drafting.push((id.clone(), now_ms()));
    });
    Ok(
        json!({"ok": true, "topic": tid, "option": option, "changes": n, "proposed": true, "hint": "recorded in the topic's proposal: the person applies it in the document"}),
    )
}

/// `agent.describeOption {topic, option, title, why}`.
pub fn describe(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_of(s, p, "agent.describeOption")?;
    let option = option_p(p);
    let title: String = str_p(p, "title").unwrap_or("").chars().take(300).collect();
    let why: String = str_p(p, "why").unwrap_or("").chars().take(4_000).collect();
    let why = crate::textlinks::store(s, &why)?;
    let r = layer::update(s.doc()?, |l| {
        let t = l.topics.iter_mut().find(|t| t.id == tid).ok_or("no such topic")?;
        let prop = t.draft.as_mut().filter(|x| x.state == State::Pending).ok_or("the topic has no pending proposal")?;
        let c = prop.options.iter_mut().find(|c| c.option == option).ok_or("no such option: record a command in it first")?;
        c.title = title;
        c.why = why;
        Ok::<(), &str>(())
    });
    r.map_err(|e| bad("agent.describeOption", e))?;
    Ok(json!({"topic": tid, "option": option}))
}

/// `agent.discardProposal {topic, option?}`: the agent takes back an option, or the whole proposal.
pub fn discard(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_of(s, p, "agent.discardProposal")?;
    let option = str_p(p, "option").map(str::to_string);
    layer::update(s.doc()?, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == tid) {
            match &option {
                Some(o) => {
                    if let Some(prop) = t.draft.as_mut() {
                        prop.options.retain(|c| &c.option != o);
                        if prop.options.is_empty() {
                            t.draft = None;
                        }
                    }
                }
                None => t.draft = None,
            }
        }
    });
    Ok(json!({"topic": tid, "option": option}))
}

/// `agent.proposalEvents {after?}` → events after that sequence number.
pub fn events(s: &mut Session, p: &Json) -> Result<Json> {
    let after = p.get("after").and_then(Json::as_u64).unwrap_or(0);
    let d = s.doc()?;
    let l = layer::get(d);
    let nums = topics::numbers_of(d, &l);
    Ok(Json::Array(
        l.events
            .iter()
            .filter(|e| e.seq > after)
            .map(|e| json!({"seq": e.seq, "event": e.event, "topic": nums.topic(&e.topic), "topicId": e.topic, "option": e.option, "comment": e.comment}))
            .collect(),
    ))
}

// ---------------------------------------------------------------- the person's side

fn person(p: &Json, cmd: &str) -> Result<()> {
    if crate::commands::actor_p(p, Actor::Human) != Actor::Human {
        return Err(bad(cmd, "the person applies or rejects a proposal, in the document"));
    }
    Ok(())
}

/// Marks pending proposals of other topics that touch these places out of date.
fn outdate_overlapping(l: &mut Layer, doc: &Presentation, except: &str, wheres: &[Where]) {
    let ix = Index::of(doc);
    let snapshot = l.clone();
    let mut hit = vec![];
    for t in &snapshot.topics {
        if t.id == except {
            continue;
        }
        let Some(prop) = t.draft.as_ref().filter(|x| x.state == State::Pending) else { continue };
        let overlaps = prop.options.iter().any(|c| {
            touched_now(doc, &ix, &snapshot, c)
                .iter()
                .any(|w| wheres.iter().any(|x| x.slide == w.slide && (x.shape == w.shape || x.shape.is_none() || w.shape.is_none())))
        });
        if overlaps {
            hit.push(t.id.clone());
        }
    }
    for id in hit {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id)
            && let Some(prop) = t.draft.as_mut()
        {
            prop.state = State::Outdated;
        }
        event(l, "proposal_outdated", &id, "", "");
    }
}

/// `agent.applyProposal {topic, option?}`: the person applies an option: its commands replayed on
/// the deck as one undo step, a decision note "chose A: …".
pub fn apply(s: &mut Session, p: &Json) -> Result<Json> {
    person(p, "agent.applyProposal")?;
    let tid = topic_of(s, p, "agent.applyProposal")?;
    let d = s.doc()?;
    let l = layer::get(d);
    let t: Topic = l.topic(&tid).cloned().ok_or_else(|| bad("agent.applyProposal", "no such topic"))?;
    let prop =
        t.draft.clone().filter(|x| x.state == State::Pending).ok_or_else(|| bad("agent.applyProposal", "the topic has no pending proposal"))?;
    let option = str_p(p, "option").map(str::to_string).or_else(|| prop.options.first().map(|c| c.option.clone())).unwrap_or_else(|| "A".into());
    let choice = prop.option(&option).cloned().ok_or_else(|| bad("agent.applyProposal", format!("no option {option}")))?;
    let ix = Index::of(&d.doc);
    let wheres = touched_now(&d.doc, &ix, &l, &choice);
    let result = match replay(d, &choice.changes) {
        Ok(r) => r,
        Err((i, e)) => {
            let id = tid.clone();
            layer::update(s.doc()?, |l| {
                if let Some(t) = l.topics.iter_mut().find(|t| t.id == id)
                    && let Some(prop) = t.draft.as_mut()
                {
                    prop.state = State::Outdated;
                }
                event(l, "proposal_outdated", &id, &option, &format!("step {} no longer fits: {e}", i + 1));
            });
            return Err(bad("agent.applyProposal", "out of date, ask the agent to redo"));
        }
    };
    // The decision note, then the undo step (the deck's copy before), then the deck.
    let title = if choice.title.is_empty() { choice.summary() } else { choice.title.clone() };
    let note_text = format!("chose {option}: {title}");
    let note = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("n");
        l.notes.push(Note {
            id: id.clone(),
            kind: "decision".into(),
            title: note_text.clone(),
            text: choice.why.clone(),
            author: Actor::Human,
            at: now_ms(),
            topic: tid.clone(),
            ..Default::default()
        });
        id
    });
    let mut after = prop.clone();
    after.state = State::Applied;
    after.chosen = option.clone();
    undo::push(s, Op::Draft { topic: tid.clone(), before: Some(prop), after: Some(after.clone()), note: Some(note) });
    {
        let st = s.doc_mut()?;
        st.doc = Arc::new(result);
        st.revision += 1;
    }
    let doc = s.doc()?.doc.clone();
    let id = tid.clone();
    layer::update(s.doc()?, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.draft = Some(after);
        }
        l.log(Actor::Human, "decision", format!("Applied option {option}: {title}"), "agent.applyProposal", None, id.clone());
        event(l, "proposal_applied", &id, &option, "");
        outdate_overlapping(l, &doc, &id, &wheres);
    });
    Ok(json!({"topic": tid, "option": option, "applied": choice.changes.len()}))
}

/// `agent.rejectProposal {topic, comment?}`: the person rejects; a comment goes to the agent as a
/// follow-up in the topic.
pub fn reject(s: &mut Session, p: &Json) -> Result<Json> {
    person(p, "agent.rejectProposal")?;
    let tid = topic_of(s, p, "agent.rejectProposal")?;
    let comment: String = str_p(p, "comment").unwrap_or("").trim().chars().take(4_000).collect();
    let d = s.doc()?;
    let prop = layer::get(d)
        .topic(&tid)
        .and_then(|t| t.draft.clone())
        .filter(|x| matches!(x.state, State::Pending | State::Outdated))
        .ok_or_else(|| bad("agent.rejectProposal", "the topic has no proposal to reject"))?;
    let mut after = prop.clone();
    after.state = State::Rejected;
    after.comment = comment.clone();
    let id = tid.clone();
    let a = after.clone();
    let c = comment.clone();
    layer::update(d, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.draft = Some(a);
        }
        l.log(
            Actor::Human,
            "decision",
            if c.is_empty() { "Rejected the proposal".into() } else { format!("Rejected the proposal: {c}") },
            "agent.rejectProposal",
            None,
            id.clone(),
        );
        event(l, "proposal_rejected", &id, "", &c);
    });
    undo::push(s, Op::Draft { topic: tid.clone(), before: Some(prop), after: Some(after), note: None });
    if !comment.is_empty() {
        crate::topic_cmds::ask(
            s,
            &json!({"topic": tid, "text": format!("I rejected your proposal: {comment}"), "slide": s.doc()?.selection.slide + 1}),
        )?;
    }
    Ok(json!({"topic": tid, "rejected": true}))
}

// ---------------------------------------------------------------- what the agent reads

/// `proposal` in list_topics / get_topic.
pub fn json(prop: &Proposal, render: &dyn Fn(&str) -> String) -> Json {
    let options: Vec<Json> = prop
        .options
        .iter()
        .map(|c| {
            json!({
                "option": c.option,
                "title": render(&c.title),
                "why": render(&c.why),
                "changes": c.changes.iter().map(|x| render(&x.summary)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut o = json!({"options": options, "state": prop.state.id()});
    if !prop.chosen.is_empty() {
        o["chosen"] = json!(prop.chosen);
    }
    if !prop.comment.is_empty() {
        o["comment"] = json!(render(&prop.comment));
    }
    o
}

/// Pending options touching a place: `[{topic, option, summary}]`.
pub fn pending_at(d: &DocState, l: &Layer, w: &Where) -> Vec<Json> {
    let ix = Index::of(&d.doc);
    let nums = topics::numbers_of(d, l);
    let mut out = vec![];
    for t in topics::visible(l) {
        let Some(prop) = t.draft.as_ref().filter(|x| x.state == State::Pending) else { continue };
        for c in &prop.options {
            let hit =
                touched_now(&d.doc, &ix, l, c).iter().any(|x| x.slide == w.slide && (w.shape.is_none() || x.shape.is_none() || x.shape == w.shape));
            if hit {
                out.push(json!({"topic": nums.topic(&t.id), "option": c.option, "summary": c.summary()}));
            }
        }
    }
    out
}

/// `read_slides {view: "proposal", topic, option?}`: the slides as the option would leave them.
pub fn read_view(s: &Session, p: &Json) -> Result<Session> {
    let tid = topic_of(s, p, "agent.read")?;
    let d = s.doc()?;
    let l = layer::get(d);
    let t = l.topic(&tid).ok_or_else(|| bad("agent.read", "no such topic"))?;
    let prop = t.draft.as_ref().ok_or_else(|| bad("agent.read", "the topic has no proposal"))?;
    let option = option_p(p);
    let c = prop.option(&option).ok_or_else(|| bad("agent.read", format!("no option {option}")))?;
    let doc = replay(d, &c.changes).map_err(|(i, e)| bad("agent.read", format!("step {} no longer fits: {e}", i + 1)))?;
    Ok(scratch(d, doc))
}
