//! The `agent.*` and `link.*` commands. The presenter's side (asking, marking client data) and the
//! agent's side (answers, notes, masked reads) are both commands, so the UI, the control channel
//! and the in-app MCP server reach them the same way.
//!
//! Agent-facing reads take `masked` (default true; always true for the agent): client data shows
//! as pseudonyms, so no real client text leaves the app.

use std::cell::Cell as StdCell;

use deckcraft_engine::cmd::{CommandSpec, has_doc, has_slide};
use deckcraft_engine::model::{Shape, ShapeId, ShapeKind, TextBody};
use deckcraft_engine::{EngineError, Result, Session, Target};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Index, Place, shape_label};
use crate::layer::{self, Actor, Answer, Layer, Link, Note, Question, QuestionStatus};
use crate::links;
use crate::privacy::{self, Masker};
use crate::{bad, now_ms};

macro_rules! spec {
    ($kind:ident $id:literal, $label:literal, $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $en, run: $run, journal: spec!(@journal $kind), undoable: spec!(@undo $kind) }
    };
    (@journal query) => { false };
    (@journal change) => { true };
    (@journal edit) => { true };
    (@undo query) => { false };
    (@undo change) => { false };
    (@undo edit) => { true };
}

pub static SPECS: &[CommandSpec] = &[
    // the presenter's side
    spec!(change "agent.ask", "Ask the Agent", "{text, slide?, shape?, words?, cell?} (default: the selection)", has_slide, ask),
    spec!(change "agent.seen", "Mark Answer Read", "{id}", has_doc, seen),
    spec!(change "agent.private", "Mark as Client Data", "{slide?, shape?|id?, words?, on?: bool (default true)} — the agent sees pseudonyms there and can't change it", has_slide, private),
    spec!(change "agent.removeNote", "Remove Agent Note", "{id}", has_doc, remove_note),
    // the agent's side
    spec!(query "agent.questions", "Agent Questions", "{status?: pending|working|answered|open|all (default all)} → questions with where they are", has_doc, questions),
    spec!(change "agent.claim", "Take Question", "{id} — marks it as being worked on", has_doc, claim),
    spec!(change "agent.answer", "Answer Question", "{id, text, links?: [{label, href, summary?}]}", has_doc, answer),
    spec!(change "agent.note", "Add Agent Note", "{text, kind?: faq|explanation|assumption|source|check, title? (faq: the question), links?, slide?, shape?, words?, cell?, actor?}", has_slide, note),
    spec!(change "agent.decision", "Log Decision", "{text, source?, slide?, shape?, actor?}", has_doc, decision),
    spec!(query "agent.layer", "Agent Layer", "{} → anchors (resolved), questions, notes, client-data marks, history", has_doc, layer_cmd),
    spec!(query "agent.context", "Agent Context", "{masked?} → the deck at a glance: slides with titles, what is on them, client data, open questions, recent decisions", has_doc, context),
    spec!(query "agent.read", "Agent Read", "{slide?: n|\"all\" (default: current), masked?} → slides: shapes with text, tables, links, notes", has_doc, read),
    spec!(edit "agent.exec", "Run Command as Agent", "{command, params?, reason?, source?} — runs a command that changes the deck, logged as the agent's; refused on client data", has_doc, exec),
    // Excel links
    spec!(edit "link.paste", "Paste Linked Cells", "{payload: object|json string | html: clipboard HTML, mode?: table|picture (default table), rect?: [x,y,w,h]}", has_slide, link_paste),
    spec!(query "link.list", "Linked Cells", "{masked?} → linked shapes with their source and last check", has_doc, link_list),
    spec!(change "link.check", "Check Linked Cells", "{id?: shape id (default: every link in the deck), gridcraft?: bool|\"host:port\" (ask a running GridCraft first; default true)} → state per link: current|stale|missing|error", has_doc, link_check),
    spec!(edit "link.update", "Update from Source", "{id?: shape id (default: the selected linked shape)} — brings in the source's current values", has_doc, link_update),
    spec!(query "link.open", "Open Source", "{id?, dryRun?: bool} — opens the workbook at the linked cells (in GridCraft when it runs)", has_doc, link_open),
    spec!(edit "link.unlink", "Break Link", "{id?} — keeps the table or picture, forgets the source", has_doc, link_unlink),
    spec!(query "link.uses", "Where Linked Cells Are Used", "{} → this deck's entries for the links registry (links.md)", has_doc, link_uses),
];

// ---------------------------------------------------------------- params

fn str_p<'a>(p: &'a Json, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Json::as_str)
}

fn masked_p(p: &Json) -> bool {
    actor_p(p, Actor::Human) == Actor::Agent || p.get("masked").and_then(Json::as_bool).unwrap_or(true)
}

thread_local! {
    /// > 0 while `agent.exec` runs a command: the history records it as the agent's.
    pub(crate) static AGENT_DEPTH: StdCell<u32> = const { StdCell::new(0) };
    pub(crate) static PENDING_REASON: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn actor_p(p: &Json, default: Actor) -> Actor {
    match str_p(p, "actor") {
        Some("agent") => Actor::Agent,
        Some("human") if AGENT_DEPTH.with(StdCell::get) == 0 => Actor::Human,
        _ if AGENT_DEPTH.with(StdCell::get) > 0 => Actor::Agent,
        _ => default,
    }
}

fn text_p(p: &Json, k: &str, cmd: &str) -> Result<String> {
    let t = str_p(p, k).map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(cmd, format!("missing `{k}`")))?;
    // Long enough for an essay, short enough to never be a problem in the file.
    Ok(t.chars().take(20_000).collect())
}

fn links_p(p: &Json) -> Vec<Link> {
    p.get("links")
        .and_then(Json::as_array)
        .map(|a| a.iter().take(50).filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default()
}

fn find_shape_by<'a>(shapes: &'a [Shape], v: &Json) -> Option<&'a Shape> {
    let mut hit = None;
    deckcraft_engine::model::walk(shapes, &mut |sh, _| {
        if hit.is_some() {
            return;
        }
        let ok = match v {
            Json::Number(n) => n.as_u64() == Some(u64::from(sh.id.0)),
            Json::String(s) => s.trim().parse::<u32>().ok() == Some(sh.id.0) || sh.name.eq_ignore_ascii_case(s.trim()),
            _ => false,
        };
        if ok {
            hit = Some(sh);
        }
    });
    hit
}

/// The text a selection covers in a body ((paragraph, char) positions).
fn selected_words(body: &TextBody, a: (usize, usize), b: (usize, usize)) -> String {
    let mut out = String::new();
    for (i, para) in body.paragraphs.iter().enumerate().take(b.0.saturating_add(1)).skip(a.0) {
        let chars: Vec<char> = para.text().chars().collect();
        let from = if i == a.0 { a.1.min(chars.len()) } else { 0 };
        let to = if i == b.0 { b.1.min(chars.len()) } else { chars.len() };
        if i > a.0 {
            out.push('\n');
        }
        out.extend(chars.get(from..to.max(from)).unwrap_or(&[]));
    }
    out
}

/// Words of a shape (its text, or a table's cells).
fn shape_text(sh: &Shape) -> String {
    match &sh.kind {
        ShapeKind::Table(t) => {
            t.rows.iter().map(|r| r.cells.iter().map(|c| c.text.text()).collect::<Vec<_>>().join("\t")).collect::<Vec<_>>().join("\n")
        }
        _ => sh.text.as_ref().map(TextBody::text).unwrap_or_default(),
    }
}

/// The place a command is about: its params, else the selection, else the current slide.
fn place_p(s: &Session, p: &Json, cmd: &str) -> Result<Place> {
    let d = s.doc()?;
    if d.selection.target != Target::Slides {
        return Err(bad(cmd, "the agent layer works on slides (leave Slide Master view)"));
    }
    let explicit = ["slide", "shape", "id", "words", "cell"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    if !explicit {
        let sel = &d.selection;
        let slide = sel.slide;
        if let Some(t) = sel.text.as_ref().filter(|t| !t.notes)
            && let Some(sh) = d.shape(t.shape)
        {
            let (a, b) = t.ordered();
            let words = match (t.is_range(), &sh.text, t.cell) {
                (true, Some(body), None) => selected_words(body, a, b).trim().to_string(),
                _ => String::new(),
            };
            return Ok(Place { slide, shape: Some(sh.id), text: words, cell: t.cell });
        }
        if let Some((table, a, _)) = sel.cells {
            return Ok(Place { slide, shape: Some(table), text: String::new(), cell: Some(a) });
        }
        if let Some(id) = sel.shapes.first() {
            return Ok(Place { slide, shape: Some(*id), ..Default::default() });
        }
        return Ok(Place::slide_only(slide));
    }
    let slide = match p.get("slide").and_then(Json::as_u64) {
        Some(n) if n >= 1 => usize::try_from(n - 1).map_err(|_| bad(cmd, "no such slide"))?,
        Some(_) => return Err(bad(cmd, "slides are numbered from 1")),
        None => d.selection.slide,
    };
    let sl = d.doc.slides.get(slide).ok_or_else(|| bad(cmd, format!("no slide {} (the deck has {})", slide + 1, d.doc.slides.len())))?;
    let shape_v = p.get("shape").or_else(|| p.get("id")).filter(|v| !v.is_null());
    let shape = match shape_v {
        Some(v) => Some(find_shape_by(&sl.shapes, v).ok_or_else(|| bad(cmd, format!("no shape {v} on slide {}", slide + 1)))?),
        None => None,
    };
    let words = str_p(p, "words").map(str::trim).unwrap_or("").chars().take(2_000).collect::<String>();
    let cell = p.get("cell").and_then(Json::as_array).and_then(|a| {
        let r = a.first()?.as_u64()?.checked_sub(1)?;
        let c = a.get(1)?.as_u64()?.checked_sub(1)?;
        Some((usize::try_from(r).ok()?, usize::try_from(c).ok()?))
    });
    if !words.is_empty() {
        let sh = shape.ok_or_else(|| bad(cmd, "`words` needs a `shape`"))?;
        if !shape_text(sh).contains(&words) {
            return Err(bad(cmd, format!("“{words}” isn't in {}", shape_label(sh))));
        }
    }
    if cell.is_some() && !shape.is_some_and(|sh| matches!(sh.kind, ShapeKind::Table(_))) {
        return Err(bad(cmd, "`cell` needs a table `shape`"));
    }
    Ok(Place { slide, shape: shape.map(|x| x.id), text: words, cell })
}

fn where_json(s: &Session, l: &Layer, m: &Masker, anchor_id: &str) -> Json {
    let Ok(d) = s.doc() else { return Json::Null };
    let Some(place) = anchor::resolve_id(d, l, anchor_id) else { return json!({"lost": true}) };
    let mut o = json!({"slide": place.slide + 1, "label": m.text(&place.label(&d.doc)), "href": place.href()});
    if let Some(id) = place.shape {
        o["shape"] = json!(id.0);
        if let Some(sh) = d.doc.slides.get(place.slide).and_then(|x| x.shape(id)) {
            o["shapeText"] = json!(m.in_shape(place.slide, id, &shape_text(sh)).chars().take(2_000).collect::<String>());
        }
    }
    if !place.text.is_empty() {
        o["words"] = json!(m.text(&place.text));
    }
    if let Some((r, c)) = place.cell {
        o["cell"] = json!([r + 1, c + 1]);
    }
    o
}

// ---------------------------------------------------------------- presenter's side

fn ask(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.ask")?;
    let place = place_p(s, p, "agent.ask")?;
    let a = anchor::anchor_at(s, &place)?;
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("q");
        l.questions.push(Question {
            id: id.clone(),
            anchor: a,
            text,
            status: QuestionStatus::Pending,
            asked_at: now_ms(),
            answer: None,
            seen: false,
        });
        id
    });
    let label = place.label(&s.doc()?.doc);
    Ok(json!({"id": id, "place": label}))
}

fn seen(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.seen")?;
    let ok = layer::update(s.doc()?, |l| l.questions.iter_mut().find(|q| q.id == id).map(|q| q.seen = true).is_some());
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.seen", format!("no question `{id}`"))) }
}

fn private(s: &mut Session, p: &Json) -> Result<Json> {
    let on = p.get("on").and_then(Json::as_bool).unwrap_or(true);
    if actor_p(p, Actor::Human) == Actor::Agent && !on {
        return Err(bad("agent.private", "only a person can lift the client-data mark"));
    }
    let place = place_p(s, p, "agent.private")?;
    if place.shape.is_none() {
        return Err(bad("agent.private", "mark a shape (or words in one), not a whole slide"));
    }
    let label = place.label(&s.doc()?.doc);
    if on {
        let a = anchor::anchor_at(s, &place)?;
        layer::update(s.doc()?, |l| {
            if !l.private.contains(&a) {
                l.private.push(a);
            }
        });
    } else {
        let d = s.doc()?;
        let ix = Index::of(&d.doc);
        let doc = d.doc.clone();
        layer::update(d, |l| {
            let hits: Vec<String> = l
                .anchors
                .iter()
                .filter(|a| {
                    anchor::resolve(&doc, &ix, a)
                        .is_some_and(|x| x.slide == place.slide && x.shape == place.shape && (place.text.is_empty() || x.text == place.text))
                })
                .map(|a| a.id.clone())
                .collect();
            l.private.retain(|id| !hits.contains(id));
        });
    }
    let actor = actor_p(p, Actor::Human);
    layer::update(s.doc()?, |l| {
        l.log(actor, "change", format!("{} client data: {label}", if on { "Marked" } else { "Unmarked" }), "agent.private", None, String::new())
    });
    Ok(json!({"place": label, "private": on}))
}

fn remove_note(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeNote")?;
    let ok = layer::update(s.doc()?, |l| {
        let n = l.notes.len();
        l.notes.retain(|x| x.id != id);
        n != l.notes.len()
    });
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.removeNote", format!("no note `{id}`"))) }
}

// ---------------------------------------------------------------- agent's side

fn status_matches(q: &Question, want: &str) -> bool {
    match want {
        "pending" => q.status == QuestionStatus::Pending,
        "working" => q.status == QuestionStatus::Working,
        "answered" => q.status == QuestionStatus::Answered,
        "open" => q.status != QuestionStatus::Answered,
        _ => true,
    }
}

pub fn question_json(s: &Session, l: &Layer, m: &Masker, q: &Question) -> Json {
    let mut o = serde_json::to_value(q).unwrap_or(Json::Null);
    o["text"] = json!(m.text(&q.text));
    o["where"] = where_json(s, l, m, &q.anchor);
    o
}

fn questions(s: &mut Session, p: &Json) -> Result<Json> {
    let l = layer::get(s.doc()?);
    let want = str_p(p, "status").unwrap_or("all");
    if !l.questions.iter().any(|q| status_matches(q, want)) {
        return Ok(json!([]));
    }
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let l = layer::get(s.doc()?);
    Ok(Json::Array(l.questions.iter().filter(|q| status_matches(q, want)).map(|q| question_json(s, &l, &m, q)).collect()))
}

fn claim(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.claim")?;
    let r = layer::update(s.doc()?, |l| match l.questions.iter_mut().find(|q| q.id == id) {
        Some(q) if q.status == QuestionStatus::Pending => {
            q.status = QuestionStatus::Working;
            Ok(())
        }
        Some(q) => Err(format!("question `{id}` is {:?}", q.status).to_lowercase()),
        None => Err(format!("no question `{id}`")),
    });
    r.map(|()| json!({"id": id})).map_err(|m| bad("agent.claim", m))
}

fn answer(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.answer")?;
    let text = text_p(p, "text", "agent.answer")?;
    let links = links_p(p);
    let ok = layer::update(s.doc()?, |l| {
        let Some(q) = l.questions.iter_mut().find(|q| q.id == id) else { return false };
        q.status = QuestionStatus::Answered;
        q.seen = false;
        q.answer = Some(Answer { text, links, at: now_ms() });
        let (anchor, qtext) = (q.anchor.clone(), q.text.clone());
        l.log(Actor::Agent, "decision", format!("Answered “{qtext}”"), "agent.answer", Some(anchor), id.clone());
        true
    });
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.answer", format!("no question `{id}`"))) }
}

fn note(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.note")?;
    let place = place_p(s, p, "agent.note")?;
    let a = anchor::anchor_at(s, &place)?;
    let kind = match str_p(p, "kind") {
        Some(k) if layer::NOTE_KINDS.contains(&k) => k.to_string(),
        _ => "explanation".into(),
    };
    let title: String = str_p(p, "title").unwrap_or("").chars().take(500).collect();
    let links = links_p(p);
    let author = actor_p(p, Actor::Human);
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("n");
        l.notes.push(Note { id: id.clone(), anchor: a, kind, title, text, links, author, at: now_ms() });
        id
    });
    Ok(json!({"id": id, "place": place.label(&s.doc()?.doc)}))
}

fn decision(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.decision")?;
    let anchor = if ["slide", "shape", "id", "words"].iter().any(|k| p.get(*k).is_some()) {
        let place = place_p(s, p, "agent.decision")?;
        Some(anchor::anchor_at(s, &place)?)
    } else {
        None
    };
    let source: String = str_p(p, "source").unwrap_or("").chars().take(2_000).collect();
    let actor = actor_p(p, Actor::Human);
    layer::update(s.doc()?, |l| l.log(actor, "decision", text, "", anchor, source));
    Ok(json!({}))
}

pub fn layer_json(s: &Session, masked: bool) -> Result<Json> {
    let m = if masked { Masker::new(s) } else { Masker::off() };
    let d = s.doc()?;
    let l = layer::get(d);
    let mut v = serde_json::to_value(&*l).unwrap_or(Json::Null);
    if let Some(o) = v.as_object_mut() {
        o.remove("pseudonyms");
    }
    if let Some(list) = v.get_mut("anchors").and_then(Json::as_array_mut) {
        for a in list.iter_mut() {
            let id = a.get("id").and_then(Json::as_str).unwrap_or("").to_string();
            a["resolved"] = where_json(s, &l, &m, &id);
            for k in ["text", "shapeName"] {
                if let Some(t) = a.get(k).and_then(Json::as_str).map(|t| m.text(t)) {
                    a[k] = json!(t);
                }
            }
        }
    }
    if let Some(list) = v.get_mut("questions").and_then(Json::as_array_mut) {
        for q in list.iter_mut() {
            if let Some(t) = q.get("text").and_then(Json::as_str).map(|t| m.text(t)) {
                q["text"] = json!(t);
            }
        }
    }
    if let Some(list) = v.get_mut("log").and_then(Json::as_array_mut) {
        for e in list.iter_mut() {
            if let Some(t) = e.get("text").and_then(Json::as_str).map(|t| m.text(t)) {
                e["text"] = json!(t);
            }
        }
    }
    Ok(v)
}

fn layer_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    layer_json(s, masked_p(p))
}

fn link_json(m: &Masker, slide: usize, id: ShapeId, l: &links::Link) -> Json {
    let check = links::last_check(&l.id);
    let private = l.private || m.is_private_shape(slide, id);
    let mut o = json!({
        "slide": slide + 1,
        "shape": id.0,
        "link": l.id,
        "mode": l.mode,
        "source": {"file": l.source.file_name(), "path": l.source.path, "sheet": l.source.sheet, "range": l.source.range, "anchor": l.source.anchor, "label": m.text(&l.source.label)},
        "copiedAt": l.copied_at,
        "pastedAt": l.pasted_at,
        "state": check.as_ref().map_or(links::State::Unchecked, |c| c.state),
        "message": check.as_ref().map(|c| c.message.clone()).unwrap_or_default(),
    });
    if private && m.on {
        o["private"] = json!(true);
    } else {
        o["text"] = json!(l.text);
    }
    o
}

/// One slide as the agent reads it.
fn slide_json(s: &Session, l: &Layer, m: &Masker, i: usize) -> Json {
    let Ok(d) = s.doc() else { return Json::Null };
    let Some(sl) = d.doc.slides.get(i) else { return Json::Null };
    let ix = Index::of(&d.doc);
    let here = |anchor: &str| l.anchor(anchor).and_then(|a| anchor::resolve(&d.doc, &ix, a));
    let mut shapes = vec![];
    deckcraft_engine::model::walk(&sl.shapes, &mut |sh, depth| {
        let x = deckcraft_engine::cmd::xfrm_of(&d.doc, &deckcraft_engine::Selection { slide: i, ..Default::default() }, sh);
        let private = m.is_private_shape(i, sh.id);
        let mut o = json!({
            "id": sh.id.0,
            "name": m.in_shape(i, sh.id, &shape_label(sh)),
            "kind": sh.kind_name(),
            "box": [x.x.round(), x.y.round(), x.w.round(), x.h.round()],
        });
        if depth > 0 {
            o["inGroup"] = json!(true);
        }
        if let Some(ph) = sh.ph_type() {
            o["placeholder"] = json!(format!("{ph:?}").to_lowercase());
        }
        if let Some(t) = sh.text.as_ref().filter(|t| !t.is_empty()) {
            o["text"] = json!(m.in_shape(i, sh.id, &t.text()));
        }
        if let ShapeKind::Table(t) = &sh.kind {
            let rows: Vec<Vec<String>> = t.rows.iter().map(|r| r.cells.iter().map(|c| m.in_shape(i, sh.id, &c.text.text())).collect()).collect();
            o["table"] = json!(rows);
        }
        if !sh.descr.trim().is_empty() {
            o["altText"] = json!(m.in_shape(i, sh.id, &sh.descr));
        }
        if let Some(link) = links::of(sh) {
            o["link"] = link_json(m, i, sh.id, &link);
        }
        if private {
            o["private"] = json!(true);
        }
        let qs: Vec<&str> =
            l.questions.iter().filter(|q| here(&q.anchor).is_some_and(|p| p.slide == i && p.shape == Some(sh.id))).map(|q| q.id.as_str()).collect();
        if !qs.is_empty() {
            o["questions"] = json!(qs);
        }
        shapes.push(o);
    });
    let notes: Vec<Json> = l
        .notes
        .iter()
        .filter(|n| here(&n.anchor).is_some_and(|p| p.slide == i))
        .map(|n| json!({"id": n.id, "kind": n.kind, "title": m.text(&n.title), "text": m.text(&n.text), "where": where_json(s, l, m, &n.anchor)}))
        .collect();
    let layout = d.doc.layout(sl.layout).map(|(_, x)| x.name.clone()).unwrap_or_default();
    json!({
        "slide": i + 1,
        "title": m.text(&sl.title()),
        "layout": layout,
        "hidden": sl.hidden,
        "hasClientData": m.has_private(i),
        "shapes": shapes,
        "speakerNotes": m.text(&sl.notes_text()),
        "agentNotes": notes,
    })
}

fn context(s: &mut Session, p: &Json) -> Result<Json> {
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let d = s.doc()?;
    let l = layer::get(d);
    let ix = Index::of(&d.doc);
    let slides: Vec<Json> = d
        .doc
        .slides
        .iter()
        .enumerate()
        .map(|(i, sl)| {
            let on = |anchor: &str| l.anchor(anchor).and_then(|a| anchor::resolve(&d.doc, &ix, a)).is_some_and(|p| p.slide == i);
            let faq = l.notes.iter().filter(|n| n.kind == "faq" && on(&n.anchor)).count();
            let mut words = 0usize;
            deckcraft_engine::model::walk(&sl.shapes, &mut |sh, _| words += shape_text(sh).split_whitespace().count());
            json!({
                "slide": i + 1,
                "title": m.text(&sl.title()),
                "shapes": sl.shapes.len(),
                "words": words,
                "hidden": sl.hidden,
                "hasClientData": m.has_private(i),
                "openQuestions": l.questions.iter().filter(|q| q.status != QuestionStatus::Answered && on(&q.anchor)).count(),
                "preparedAnswers": faq,
                "links": links::all(&d.doc).iter().filter(|(s, _, _)| *s == i).count(),
            })
        })
        .collect();
    let private: Vec<Json> = l.private.iter().map(|id| where_json(s, &l, &m, id)).collect();
    Ok(json!({
        "title": m.text(&d.title()),
        "path": d.path,
        "currentSlide": d.selection.slide + 1,
        "slideSize": [d.doc.slide_size.width, d.doc.slide_size.height],
        "slides": slides,
        "clientData": private,
        "privacyNote": "Text marked as client data shows as pseudonyms like [client-3] wherever it appears in the deck; the same text always gets the same pseudonym. Never present a pseudonym as a real name or number, never guess what it stands for, and don't change shapes marked private (your changes there are refused).",
        "openQuestions": l.questions.iter().filter(|q| q.status != QuestionStatus::Answered).count(),
        "notes": l.notes.len(),
        "log": l.log.iter().rev().take(20).map(|e| json!({"seq": e.seq, "actor": e.actor, "kind": e.kind, "text": m.text(&e.text), "source": e.source})).collect::<Vec<_>>(),
    }))
}

fn read(s: &mut Session, p: &Json) -> Result<Json> {
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let d = s.doc()?;
    let l = layer::get(d);
    let n = d.doc.slides.len();
    let which: Vec<usize> = match p.get("slide") {
        Some(Json::String(a)) if a == "all" => (0..n.min(500)).collect(),
        Some(v) => {
            let k =
                v.as_u64().filter(|k| *k >= 1 && (*k as usize) <= n).ok_or_else(|| bad("agent.read", format!("no slide {v} (the deck has {n})")))?;
            vec![k as usize - 1]
        }
        None => vec![d.selection.slide.min(n.saturating_sub(1))],
    };
    let slides: Vec<Json> = which.into_iter().map(|i| slide_json(s, &l, &m, i)).collect();
    Ok(json!({"masked": m.on, "slides": slides}))
}

// ---------------------------------------------------------------- acting as the agent

/// Commands the agent may not run through `agent.exec`: reads (they'd return client data
/// unmasked), files, the clipboard, the layer's own commands.
fn refused(id: &str, spec: &CommandSpec) -> Option<String> {
    if id.starts_with("agent.") {
        return Some("call agent.* commands directly".into());
    }
    if !spec.journal
        || id.starts_with("file.")
        || id.starts_with("edit.copy")
        || id.starts_with("edit.cut")
        || id.starts_with("edit.paste")
        || id.starts_with("link.")
    {
        return Some(format!("`{id}` reads or moves deck content outside the privacy gate: use agent.read"));
    }
    None
}

fn exec(s: &mut Session, p: &Json) -> Result<Json> {
    let command = text_p(p, "command", "agent.exec")?;
    let params = p.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(spec) = deckcraft_engine::find_command(&command) else { return Err(EngineError::UnknownCommand(command)) };
    if let Some(why) = refused(&command, spec) {
        return Err(bad("agent.exec", why));
    }
    let set = privacy::private_shapes(s);
    if !set.is_empty() {
        let d = s.doc()?;
        let slide = d.selection.slide;
        for t in privacy::command_targets(s, &params) {
            if set.contains(&t) {
                return Err(bad("agent.exec", format!("shape {} on slide {} holds client data: the agent can't change it", t.1, t.0 + 1)));
            }
        }
        let slide_wide = command.starts_with("slide.")
            && !matches!(
                command.as_str(),
                "slide.new" | "slide.next" | "slide.previous" | "slide.go" | "slide.first" | "slide.last" | "slide.selectSlides"
            );
        if slide_wide && privacy::slide_has_private(&set, slide) {
            return Err(bad("agent.exec", format!("slide {} holds client data: the agent can't delete, replace or rearrange it", slide + 1)));
        }
        if matches!(command.as_str(), "edit.selectAll" | "find.replaceAll" | "find.replace" | "design.apply") {
            return Err(bad("agent.exec", format!("`{command}` can reach client data: change shapes one by one")));
        }
    }
    let reason: String = str_p(p, "reason").unwrap_or("").chars().take(4_000).collect();
    let source: String = str_p(p, "source").unwrap_or("").chars().take(2_000).collect();
    PENDING_REASON.with(|r| *r.borrow_mut() = Some((reason, source)));
    AGENT_DEPTH.with(|x| x.set(x.get().saturating_add(1)));
    let r = s.execute(&command, &params);
    AGENT_DEPTH.with(|x| x.set(x.get().saturating_sub(1)));
    PENDING_REASON.with(|r| r.borrow_mut().take());
    // Only success is reported back: a command's result can carry deck content.
    // The history records the command as the agent's (the observer sees it run inside agent.exec).
    r.map(|v| json!({"ok": true, "command": command, "id": v.get("id")}))
}

// ---------------------------------------------------------------- links

fn link_paste(s: &mut Session, p: &Json) -> Result<Json> {
    let payload = match (p.get("payload"), str_p(p, "html")) {
        (Some(Json::String(t)), _) => links::parse(t).map_err(|e| bad("link.paste", e))?,
        (Some(v @ Json::Object(_)), _) => links::parse(&v.to_string()).map_err(|e| bad("link.paste", e))?,
        (_, Some(h)) => links::from_html(h).ok_or_else(|| bad("link.paste", "the clipboard HTML has no linked cells"))?,
        _ => return Err(bad("link.paste", "pass `payload` or `html`")),
    };
    let mode = match str_p(p, "mode") {
        Some("picture") => links::Mode::Picture,
        Some("table") | None => links::Mode::Table,
        Some(o) => return Err(bad("link.paste", format!("unknown mode `{o}` (table, picture)"))),
    };
    let rect = p.get("rect").and_then(Json::as_array).and_then(|a| {
        let v: Vec<f64> = a.iter().filter_map(Json::as_f64).filter(|f| f.is_finite()).collect();
        (v.len() == 4 && v[2] > 1.0 && v[3] > 1.0).then(|| deckcraft_geom::Xfrm::new(v[0], v[1], v[2], v[3]))
    });
    links::paste(s, payload, mode, rect)
}

fn link_list(s: &mut Session, p: &Json) -> Result<Json> {
    let m = if masked_p(p) { Masker::new(s) } else { Masker::off() };
    let d = s.doc()?;
    Ok(Json::Array(links::all(&d.doc).iter().map(|(slide, id, l)| link_json(&m, *slide, *id, l)).collect()))
}

/// The linked shape a link command is about: `id`, else the selected shape.
fn linked_target(s: &Session, p: &Json, cmd: &str) -> Result<(ShapeId, links::Link)> {
    let d = s.doc()?;
    let id = p
        .get("id")
        .and_then(Json::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .map(ShapeId)
        .or_else(|| d.selection.shapes.first().copied())
        .ok_or_else(|| bad(cmd, "select a linked table or picture"))?;
    // `id` may be on another slide.
    let sh = d.shape(id).or_else(|| d.doc.slides.iter().find_map(|sl| sl.shape(id))).ok_or_else(|| bad(cmd, format!("no shape {id}")))?;
    let link = links::of(sh).ok_or_else(|| bad(cmd, format!("{} isn't linked to a workbook", shape_label(sh))))?;
    Ok((id, link))
}

/// `gridcraft`: true (default) asks the running GridCraft at its usual address, an address asks
/// that one, false reads only the file.
fn gridcraft_p(p: &Json) -> Option<String> {
    match p.get("gridcraft") {
        Some(Json::Bool(false)) => None,
        Some(Json::String(a)) if !a.is_empty() => Some(a.clone()),
        _ => Some(links::gridcraft_addr()),
    }
}

fn link_check(s: &mut Session, p: &Json) -> Result<Json> {
    let ask = gridcraft_p(p);
    let d = s.doc()?;
    let deck = d.path.clone();
    let list: Vec<(usize, ShapeId, links::Link)> = match p.get("id") {
        Some(_) => {
            let (id, l) = linked_target(s, p, "link.check")?;
            vec![(d.selection.slide, id, l)]
        }
        None => links::all(&d.doc),
    };
    let mut out = vec![];
    for (slide, id, l) in list {
        let c = links::check(&l, deck.as_deref(), ask.as_deref());
        out.push(json!({"slide": slide + 1, "shape": id.0, "link": l.id, "state": c.state, "message": c.message, "via": c.via, "range": c.range, "anchored": c.anchored, "changed": c.changed.len()}));
        links::remember(&l.id, c);
    }
    Ok(Json::Array(out))
}

fn link_update(s: &mut Session, p: &Json) -> Result<Json> {
    let (id, l) = linked_target(s, p, "link.update")?;
    let deck = s.doc()?.path.clone();
    let c = links::check(&l, deck.as_deref(), gridcraft_p(p).as_deref());
    if matches!(c.state, links::State::Missing | links::State::Error) {
        links::remember(&l.id, c.clone());
        return Err(bad("link.update", c.message));
    }
    // The shape may be on another slide: go there, so the edit lands on it.
    let d = s.doc()?;
    if d.shape(id).is_none()
        && let Some(i) = d.doc.slides.iter().position(|sl| sl.shape(id).is_some())
    {
        s.select(|_, sel| sel.slide = i)?;
    }
    let r = links::update(s, id, &c)?;
    let actor = actor_p(p, Actor::Human);
    let text = format!("Updated linked cells from {} ({} changed)", l.source.place(), c.changed.len());
    layer::update(s.doc()?, |x| x.log(actor, "change", text, "link.update", None, l.source.file_name()));
    Ok(r)
}

fn link_open(s: &mut Session, p: &Json) -> Result<Json> {
    let (_, l) = linked_target(s, p, "link.open")?;
    let deck = s.doc()?.path.clone();
    links::open_source(&l, deck.as_deref(), p.get("dryRun").and_then(Json::as_bool).unwrap_or(false)).map_err(|e| bad("link.open", e))
}

fn link_unlink(s: &mut Session, p: &Json) -> Result<Json> {
    let (id, l) = linked_target(s, p, "link.unlink")?;
    s.edit(|doc, sel| {
        let sh = deckcraft_engine::shapes_mut(doc, sel)
            .and_then(|x| deckcraft_engine::model::find_shape_mut(x, id))
            .ok_or_else(|| bad("link.unlink", "the shape is on another slide"))?;
        sh.tags.retain(|(n, _)| n != links::TAG);
        Ok(())
    })?;
    Ok(json!({"id": id.0, "link": l.id}))
}

fn link_uses(s: &mut Session, _p: &Json) -> Result<Json> {
    Ok(Json::Array(links::uses(s)))
}
