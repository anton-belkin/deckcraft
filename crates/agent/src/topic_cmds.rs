//! The topic commands (TOPICS.md): asking (a question starts or joins a topic), answering with
//! links in the text, notes, places, statuses. The person's actions go on the undo stack
//! ([`crate::undo`]); the agent's don't, and only a person sets Agreed.

use deckcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Index, Place as Where};
use crate::commands::{actor_p, links_p, masked_p, place_p, str_p, text_p};
use crate::layer::{self, Actor, Answer, Note, Place, Question, QuestionStatus, Status, Topic};
use crate::privacy::Masker;
use crate::undo::{self, Op};
use crate::{bad, now_ms, textlinks, topics};

/// A topic param (`t5`, a number or `topic:3`) to its id.
fn topic_p(s: &Session, p: &Json, cmd: &str) -> Result<String> {
    let r = p.get("topic").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).ok_or_else(|| bad(cmd, "missing `topic`"))?;
    let d = s.doc()?;
    let l = layer::get(d);
    topics::find(&d.doc, &Index::of(&d.doc), &l, &r).ok_or_else(|| bad(cmd, format!("no topic `{r}`")))
}

/// `links` (the old way): appended to the text as a sentence of links.
fn with_links(text: String, p: &Json) -> String {
    let links = links_p(p);
    if links.is_empty() {
        return text;
    }
    let list: Vec<String> = links.iter().map(|k| format!("[{}]({})", k.label, k.href)).collect();
    format!("{text}\n\nSources: {}.", list.join(", "))
}

/// A place param for `add_places` / `widen_place`: `{slide, shape?|shapes?, words?}` → anchors.
fn anchors_of(s: &mut Session, v: &Json, cmd: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    if let Some(shapes) = v.get("shapes").and_then(Json::as_array) {
        for sh in shapes.iter().take(50) {
            let mut one = v.clone();
            if let Some(o) = one.as_object_mut() {
                o.remove("shapes");
                o.insert("shape".into(), sh.clone());
            }
            let w = place_p(s, &one, cmd)?;
            out.push(anchor::anchor_at(s, &w)?);
        }
    } else {
        let w = place_p(s, v, cmd)?;
        out.push(anchor::anchor_at(s, &w)?);
    }
    Ok(out)
}

/// Starts a topic on `anchors`, or returns the topic `p` names / the selection already has.
fn topic_for(s: &mut Session, p: &Json, place: &Where, cmd: &str) -> Result<(String, bool, Vec<String>)> {
    let actor = actor_p(p, Actor::Human);
    let a = anchor::anchor_at(s, place)?;
    let d = s.doc()?;
    let l = layer::get(d);
    let ix = Index::of(&d.doc);
    let named = if p.get("topic").is_some() { Some(topic_p(s, p, cmd)?) } else { None };
    let existing = match named {
        Some(t) => Some(t),
        None if p.get("new").and_then(Json::as_bool) == Some(true) => None,
        None => topics::touching(&d.doc, &ix, &l, place).into_iter().next(),
    };
    if let Some(t) = existing {
        // A named topic gains the place when it isn't there yet.
        let has = l.topic(&t).is_some_and(|t| topics::live(t).any(|pl| pl.anchors.contains(&a)));
        let touched = topics::touching(&d.doc, &ix, &l, place).contains(&t);
        if has || touched {
            return Ok((t, false, vec![]));
        }
        let pid = layer::update(d, |l| {
            let pid = l.fresh_id("p");
            if let Some(tt) = l.topics.iter_mut().find(|x| x.id == t) {
                tt.places.push(Place { id: pid.clone(), anchors: vec![a.clone()], by: actor, ..Default::default() });
            }
            pid
        });
        return Ok((t, false, vec![pid]));
    }
    let (tid, pid) = layer::update(d, |l| {
        let tid = l.fresh_id("t");
        let pid = l.fresh_id("p");
        l.topics.push(Topic {
            id: tid.clone(),
            places: vec![Place { id: pid.clone(), anchors: vec![a.clone()], by: actor, ..Default::default() }],
            status: Status::Open,
            status_by: actor,
            status_at: now_ms(),
            at: now_ms(),
            ..Default::default()
        });
        (tid, pid)
    });
    Ok((tid, true, vec![pid]))
}

// ---------------------------------------------------------------- the person's side

/// `agent.ask`: a question on the selection (or the place named). It joins the topic the place
/// already has, or starts one (`new: true` always starts one; `topic` names one).
pub fn ask(s: &mut Session, p: &Json) -> Result<Json> {
    let raw = text_p(p, "text", "agent.ask")?;
    let place = place_p(s, p, "agent.ask")?;
    let (tid, created, places) = topic_for(s, p, &place, "agent.ask")?;
    let text = textlinks::store(s, &raw)?;
    let a = anchor::anchor_at(s, &place)?;
    let actor = actor_p(p, Actor::Human);
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("q");
        l.questions.push(Question {
            id: id.clone(),
            anchor: a,
            text,
            status: QuestionStatus::Pending,
            asked_at: now_ms(),
            topic: tid.clone(),
            ..Default::default()
        });
        id
    });
    if actor == Actor::Human {
        undo::push(s, Op::Ask { question: id.clone(), topic: created.then(|| tid.clone()), places });
    }
    let d = s.doc()?;
    let l = layer::get(d);
    let nums = topics::numbers_of(d, &l);
    Ok(json!({"id": id, "topic": nums.topic(&tid), "topicId": tid, "place": place.label(&d.doc)}))
}

/// `agent.newTopic`: a topic on the selection without a question (a note or a status to come).
pub fn new_topic(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.newTopic")?;
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("new".into(), json!(true));
    }
    let (tid, _, places) = topic_for(s, &q, &place, "agent.newTopic")?;
    if actor_p(p, Actor::Human) == Actor::Human {
        undo::push(s, Op::Places { topic: Some(tid.clone()), places });
    }
    Ok(json!({"topicId": tid}))
}

/// `agent.setStatus {topic, status, text}`: only a person sets Agreed.
pub fn set_status(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_p(s, p, "agent.setStatus")?;
    let status = str_p(p, "status").and_then(Status::parse).ok_or_else(|| bad("agent.setStatus", "`status` is open, revise, agreed or info"))?;
    let actor = actor_p(p, Actor::Human);
    if status == Status::Agreed && actor != Actor::Human {
        return Err(bad("agent.setStatus", "only a person sets Agreed: propose it with propose_agreed"));
    }
    let raw: String = str_p(p, "text").unwrap_or("").chars().take(4_000).collect();
    let text = textlinks::store(s, &raw)?;
    change_status(s, &tid, status, text, actor, "agent.setStatus")
}

fn change_status(s: &mut Session, tid: &str, status: Status, text: String, actor: Actor, cmd: &str) -> Result<Json> {
    let d = s.doc()?;
    let before = layer::get(d)
        .topic(tid)
        .map(|t| (t.status, t.status_text.clone(), t.status_by, t.status_at, t.proposal.clone()))
        .ok_or_else(|| bad(cmd, "no such topic"))?;
    let after = (status, text, actor, now_ms(), if status == Status::Agreed { String::new() } else { before.4.clone() });
    let a = after.clone();
    let id = tid.to_string();
    layer::update(d, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.status = a.0;
            t.status_text = a.1.clone();
            t.status_by = a.2;
            t.status_at = a.3;
            t.proposal = a.4.clone();
        }
        l.log(actor, "decision", format!("Status {}: {}", status.label(), a.1), cmd, None, id.clone());
    });
    if actor == Actor::Human {
        undo::push(s, Op::Status { topic: tid.to_string(), before, after });
    }
    Ok(json!({"topicId": tid, "status": status.id()}))
}

/// `agent.proposeAgreed {topic, reasons}`: the agent's suggestion, confirmed by a click.
pub fn propose_agreed(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_p(s, p, "agent.proposeAgreed")?;
    let raw = text_p(p, "reasons", "agent.proposeAgreed")?;
    let reasons = textlinks::store(s, &raw)?;
    let actor = actor_p(p, Actor::Agent);
    let id = tid.clone();
    layer::update(s.doc()?, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.proposal = reasons.clone();
        }
        l.log(actor, "decision", format!("Suggests Agreed: {reasons}"), "agent.proposeAgreed", None, id.clone());
    });
    Ok(json!({"topicId": tid, "proposed": "agreed"}))
}

/// `agent.confirmAgreed {topic}`: the person accepts the agent's proposal (one click).
pub fn confirm_agreed(s: &mut Session, p: &Json) -> Result<Json> {
    if actor_p(p, Actor::Human) != Actor::Human {
        return Err(bad("agent.confirmAgreed", "only a person sets Agreed"));
    }
    let tid = topic_p(s, p, "agent.confirmAgreed")?;
    let text = layer::get(s.doc()?).topic(&tid).map(|t| t.proposal.clone()).unwrap_or_default();
    change_status(s, &tid, Status::Agreed, text, Actor::Human, "agent.confirmAgreed")
}

/// `agent.addPlaces {topic, places: [{slide, shape?|shapes?, words?, reason}]}`.
pub fn add_places(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_p(s, p, "agent.addPlaces")?;
    let list = p.get("places").and_then(Json::as_array).cloned().ok_or_else(|| bad("agent.addPlaces", "missing `places`"))?;
    let actor = actor_p(p, Actor::Human);
    let mut added = vec![];
    for v in list.iter().take(50) {
        let anchors = anchors_of(s, v, "agent.addPlaces")?;
        let reason = textlinks::store(s, v.get("reason").and_then(Json::as_str).unwrap_or(""))?;
        let id = tid.clone();
        let pid = layer::update(s.doc()?, |l| {
            let pid = l.fresh_id("p");
            if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
                t.places.push(Place { id: pid.clone(), anchors, reason, by: actor, ..Default::default() });
            }
            pid
        });
        added.push(pid);
    }
    if actor == Actor::Human {
        undo::push(s, Op::Places { topic: None, places: added.clone() });
    }
    let d = s.doc()?;
    let nums = topics::numbers_of(d, &layer::get(d));
    Ok(json!({"topicId": tid, "places": added.iter().map(|p| json!({"id": p, "place": nums.place(p)})).collect::<Vec<_>>()}))
}

/// `agent.widenPlace {place, to: {slide, shape?|shapes?, words?}, reason}`.
pub fn widen_place(s: &mut Session, p: &Json) -> Result<Json> {
    let r = p
        .get("place")
        .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
        .ok_or_else(|| bad("agent.widenPlace", "missing `place`"))?;
    let (tid, pid) = {
        let d = s.doc()?;
        let l = layer::get(d);
        topics::find_place(&d.doc, &Index::of(&d.doc), &l, &r).ok_or_else(|| bad("agent.widenPlace", format!("no place `{r}`")))?
    };
    let to = p.get("to").cloned().ok_or_else(|| bad("agent.widenPlace", "missing `to`"))?;
    let after = anchors_of(s, &to, "agent.widenPlace")?;
    let reason = textlinks::store(s, str_p(p, "reason").unwrap_or(""))?;
    let actor = actor_p(p, Actor::Human);
    let id = pid.clone();
    let before = layer::update(s.doc()?, |l| {
        let mut before = vec![];
        for t in &mut l.topics {
            if let Some(pl) = t.places.iter_mut().find(|x| x.id == id) {
                before = std::mem::replace(&mut pl.anchors, after.clone());
                if !reason.is_empty() {
                    pl.reason = reason.clone();
                }
            }
        }
        before
    });
    if actor == Actor::Human {
        undo::push(s, Op::Widen { place: pid.clone(), before, after });
    }
    Ok(json!({"topicId": tid, "place": pid}))
}

/// `agent.rejectPlace {place}`: the person rejects a place the agent added.
pub fn reject_place(s: &mut Session, p: &Json) -> Result<Json> {
    let r = text_p(p, "place", "agent.rejectPlace")?;
    let (_, pid) = {
        let d = s.doc()?;
        let l = layer::get(d);
        topics::find_place(&d.doc, &Index::of(&d.doc), &l, &r).ok_or_else(|| bad("agent.rejectPlace", format!("no place `{r}`")))?
    };
    let id = pid.clone();
    layer::update(s.doc()?, |l| {
        for t in &mut l.topics {
            for pl in t.places.iter_mut().filter(|x| x.id == id) {
                pl.rejected = true;
            }
        }
    });
    undo::push(s, Op::Reject { place: pid.clone() });
    Ok(json!({"place": pid}))
}

/// `agent.pinNote {id, on?}`.
pub fn pin_note(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.pinNote")?;
    let on = p.get("on").and_then(Json::as_bool).unwrap_or(true);
    let ok = layer::update(s.doc()?, |l| l.notes.iter_mut().find(|n| n.id == id).map(|n| n.pinned = on).is_some());
    if !ok {
        return Err(bad("agent.pinNote", format!("no note `{id}`")));
    }
    undo::push(s, Op::Pin { note: id.clone(), on });
    Ok(json!({"id": id, "pinned": on}))
}

/// `agent.removeNote {id}`: the person's removal is undoable.
pub fn remove_note(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeNote")?;
    let ok = layer::update(s.doc()?, |l| l.notes.iter_mut().find(|n| n.id == id).map(|n| n.deleted = true).is_some());
    if !ok {
        return Err(bad("agent.removeNote", format!("no note `{id}`")));
    }
    if actor_p(p, Actor::Human) == Actor::Human {
        undo::push(s, Op::RemoveNote { note: id.clone() });
    }
    Ok(json!({"id": id}))
}

// ---------------------------------------------------------------- the agent's side

/// `agent.answer {id, text, links?}`: the text may carry links (§2); `links` are appended.
pub fn answer(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.answer")?;
    let raw = with_links(text_p(p, "text", "agent.answer")?, p);
    let l = layer::get(s.doc()?);
    let Some(q) = l.questions.iter().find(|q| q.id == id) else { return Err(bad("agent.answer", format!("no question `{id}`"))) };
    if q.is_ping() {
        // A connection check: noted, and gone (it was never a question for the presenter).
        crate::listener::answered(&id, str_p(p, "listener"));
        layer::update(s.doc()?, |l| l.questions.retain(|q| q.id != id));
        return Ok(json!({"id": id, "ping": true}));
    }
    if q.status == QuestionStatus::Withdrawn || q.hidden {
        return Err(bad("agent.answer", "this question was withdrawn by the user; drop it"));
    }
    let text = textlinks::store(s, &raw)?;
    layer::update(s.doc()?, |l| {
        let Some(q) = l.questions.iter_mut().find(|q| q.id == id) else { return };
        q.status = QuestionStatus::Answered;
        q.seen = false;
        q.answer = Some(Answer { text, links: vec![], at: now_ms() });
        let (anchor, qtext, topic) = (q.anchor.clone(), q.text.clone(), q.topic.clone());
        l.log(Actor::Agent, "decision", format!("Answered “{qtext}”"), "agent.answer", Some(anchor), topic);
    });
    Ok(json!({"id": id}))
}

/// `agent.note {topic? | slide/shape…, title, kind, text, url?, summary?, links?}`. Without a
/// topic, the note starts one on its place (For information: a prepared answer, a source).
pub fn note(s: &mut Session, p: &Json) -> Result<Json> {
    let raw = with_links(text_p(p, "text", "agent.note")?, p);
    let kind = match str_p(p, "kind") {
        Some(k) if layer::NOTE_KINDS.contains(&k) => k.to_string(),
        _ => "explanation".into(),
    };
    let author = actor_p(p, Actor::Human);
    let (tid, anchor_id, made) = if p.get("topic").is_some() {
        let t = topic_p(s, p, "agent.note")?;
        let d = s.doc()?;
        let a = layer::get(d).topic(&t).and_then(|t| t.places.first()).and_then(|pl| pl.anchors.first().cloned()).unwrap_or_default();
        (t, a, None)
    } else {
        let place = place_p(s, p, "agent.note")?;
        let a = anchor::anchor_at(s, &place)?;
        let (tid, pid) = layer::update(s.doc()?, |l| {
            let tid = l.fresh_id("t");
            let pid = l.fresh_id("p");
            l.topics.push(Topic {
                id: tid.clone(),
                places: vec![Place { id: pid.clone(), anchors: vec![a.clone()], by: author, ..Default::default() }],
                status: Status::Info,
                status_by: author,
                status_at: now_ms(),
                at: now_ms(),
                ..Default::default()
            });
            (tid, pid)
        });
        (tid, a, Some(pid))
    };
    let text = textlinks::store(s, &raw)?;
    let title = textlinks::store(s, &str_p(p, "title").unwrap_or("").chars().take(500).collect::<String>())?;
    let url: String = str_p(p, "url").unwrap_or("").chars().take(2_000).collect();
    let summary: String = str_p(p, "summary").unwrap_or("").chars().take(2_000).collect();
    let t = tid.clone();
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("n");
        l.notes.push(Note {
            id: id.clone(),
            anchor: anchor_id,
            kind,
            title,
            text,
            author,
            at: now_ms(),
            topic: t,
            url,
            summary,
            ..Default::default()
        });
        id
    });
    if author == Actor::Human {
        match made {
            Some(pid) => undo::push(s, Op::Places { topic: Some(tid.clone()), places: vec![pid] }),
            None => undo::push(s, Op::AddNote { note: id.clone() }),
        }
    }
    let d = s.doc()?;
    let nums = topics::numbers_of(d, &layer::get(d));
    Ok(json!({"id": id, "topic": nums.topic(&tid), "topicId": tid}))
}

/// `agent.topics {status?}` (list_topics).
pub fn list(s: &mut Session, p: &Json) -> Result<Json> {
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let want = str_p(p, "status").and_then(Status::parse);
    let d = s.doc()?;
    let l = layer::get(d);
    let nums = topics::numbers_of(d, &l);
    let render = |t: &str| m.text(&textlinks::render(d, &l, &nums, t));
    let mut out: Vec<(usize, Json)> = topics::visible(&l)
        .filter(|t| want.is_none_or(|w| t.status == w))
        .map(|t| (nums.topic(&t.id), topics::json(d, &l, &nums, t, false, &render)))
        .collect();
    out.sort_by_key(|(n, _)| *n);
    Ok(json!({"readiness": topics::readiness_text(&l), "topics": out.into_iter().map(|(_, j)| j).collect::<Vec<_>>()}))
}

/// `agent.topic {topic}` (get_topic): everything, discussion included.
pub fn get(s: &mut Session, p: &Json) -> Result<Json> {
    let tid = topic_p(s, p, "agent.topic")?;
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let d = s.doc()?;
    let l = layer::get(d);
    let nums = topics::numbers_of(d, &l);
    let render = |t: &str| m.text(&textlinks::render(d, &l, &nums, t));
    let t = l.topic(&tid).ok_or_else(|| bad("agent.topic", "no such topic"))?;
    Ok(topics::json(d, &l, &nums, t, true, &render))
}

/// Source changes that already raised their topics (document, link, what changed): a change raises
/// once, so a person who looked and set Agreed isn't overruled by the next check.
static RAISED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A linked table or picture whose source changed raises the topics on it to Revise (by the app,
/// in the log), once per change in the source.
pub fn raise_stale(s: &Session) {
    let Ok(d) = s.doc() else { return };
    let stale: Vec<(usize, deckcraft_engine::model::ShapeId)> = crate::links::all(&d.doc)
        .into_iter()
        .filter_map(|(slide, id, k)| {
            let c = crate::links::last_check(&k.id).filter(|c| c.state == crate::links::State::Stale)?;
            let key = format!("{}:{}:{:?}", d.uid, k.id, c.changed);
            let mut seen = RAISED.lock().unwrap_or_else(|p| p.into_inner());
            if seen.contains(&key) {
                return None;
            }
            if seen.len() > 2_000 {
                seen.drain(..500);
            }
            seen.push(key);
            Some((slide, id))
        })
        .collect();
    if stale.is_empty() {
        return;
    }
    let l = layer::get(d);
    let ix = Index::of(&d.doc);
    let raise: Vec<String> = topics::visible(&l)
        .filter(|t| t.status != Status::Revise)
        .filter(|t| {
            topics::live(t).any(|p| topics::resolve(&d.doc, &ix, &l, p).iter().any(|(_, w)| w.shape.is_some_and(|id| stale.contains(&(w.slide, id)))))
        })
        .map(|t| t.id.clone())
        .collect();
    if raise.is_empty() {
        return;
    }
    layer::update(d, |l| {
        for id in &raise {
            if let Some(t) = l.topics.iter_mut().find(|t| &t.id == id) {
                t.status = Status::Revise;
                t.status_text = "the linked cells changed in the workbook".into();
                t.status_by = Actor::App;
                t.status_at = now_ms();
            }
            l.log(Actor::App, "decision", "Status Revise: the linked cells changed in the workbook".into(), "link.check", None, id.clone());
        }
    });
}

/// `agent.copyToSend {path?, format?: pptx|deckcraft, discussion?: bool}`: a copy of the deck to
/// send, without the discussion unless `discussion` (→ `{bytes}` or `{data}` without a path).
pub fn copy_to_send(s: &mut Session, p: &Json) -> Result<Json> {
    if actor_p(p, Actor::Human) != Actor::Human {
        return Err(bad("agent.copyToSend", "the person sends the deck"));
    }
    let d = s.doc()?;
    let discussion = p.get("discussion").and_then(Json::as_bool).unwrap_or(false);
    let format = str_p(p, "format").unwrap_or("pptx");
    let mut doc = (*d.doc).clone();
    let l = layer::for_sending(&layer::get(d), discussion);
    layer::write_into(&mut doc, &l);
    let bytes = deckcraft_engine::cmd::file::save_bytes(&doc, format)?;
    match str_p(p, "path") {
        #[cfg(not(target_arch = "wasm32"))]
        Some(path) => {
            std::fs::write(path, &bytes).map_err(|e| bad("agent.copyToSend", format!("{path}: {e}")))?;
            Ok(json!({"path": path, "bytes": bytes.len(), "discussion": discussion}))
        }
        _ => Ok(json!({"data": deckcraft_engine::cmd::base64_encode(&bytes), "bytes": bytes.len(), "discussion": discussion})),
    }
}
