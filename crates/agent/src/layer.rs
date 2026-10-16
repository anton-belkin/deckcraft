//! The agent layer of one presentation: what the agent and the presenter added on top of the
//! slides.
//!
//! It lives in a per-document store while the presentation is open (so undoing a slide edit
//! never takes a question or an answer away) and is written into the presentation as a custom
//! XML part just before every save. DeckCraft keeps custom XML parts on open and save, and so
//! does PowerPoint, so the layer travels with the `.pptx`. On open it is read back from that part.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use deckcraft_engine::DocState;
use deckcraft_model::{KeptPart, Presentation};
use serde::{Deserialize, Serialize};

/// Namespace of the custom XML part (and how we recognise it, whatever its part name).
pub const NS: &str = "urn:deckcraft:agent-layer:v1";
const REL_CUSTOM_XML: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
const PART_NAME: &str = "customXml/deckcraftAgent.xml";
/// Largest layer read back from a file.
const MAX_PART: usize = 32 * 1024 * 1024;

/// Who did something.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    #[default]
    Human,
    Agent,
    /// The app itself (a linked source changed, …).
    App,
}

/// A place the layer points at: a slide, a shape on it, words in the shape's text or a table
/// cell. Slides and shapes are found by a hidden key in their tags (`p:custDataLst`), which
/// survives renumbering on open, copy and paste and a round trip through PowerPoint; the ids and
/// the slide number are where it was last seen, used when the key is gone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Anchor {
    pub id: String,
    pub slide_key: String,
    /// Empty: the slide itself.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub shape_key: String,
    /// The words the anchor is on (empty: the whole shape).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A table cell (row, column).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<(usize, usize)>,
    /// Last seen: slide index (0-based), slide id, shape id and name.
    pub slide: usize,
    pub slide_id: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub shape_id: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub shape_name: String,
    /// The slide or shape was deleted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,
    /// Where it was, kept up to date while it exists: "on slide 4, after the chart". A removed
    /// place reads "removed — was …".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub was: String,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// A link in a note or an answer: a web page (`https://…`), a slide (`slide:3`), a shape
/// (`shape:3/12`, slide number / shape id) or another note (`note:n3`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub href: String,
    /// A short summary of what is behind the link (for sources).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionStatus {
    #[default]
    Pending,
    /// An agent has taken it.
    Working,
    Answered,
    /// The person took it back (undo) while the agent worked on it.
    Withdrawn,
    /// Spoken, being transcribed: not with the agent yet (kept in memory only, never saved).
    Transcribing,
    /// The transcription failed: the person can retry or dismiss it (memory only).
    Failed,
}

impl QuestionStatus {
    /// Not a question for the agent yet: a spoken one still turning into text.
    pub fn is_draft(self) -> bool {
        matches!(self, QuestionStatus::Transcribing | QuestionStatus::Failed)
    }
}

/// What the person pointed at while speaking: marker `⟦n⟧` in the question's text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionRef {
    pub n: u32,
    pub anchor: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub anchor: String,
    pub text: String,
    #[serde(default)]
    pub status: QuestionStatus,
    #[serde(default)]
    pub asked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// The presenter has opened the answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    /// Shapes or slides pointed at while the question was spoken (`anchor` stays the main one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<QuestionRef>,
    /// Asked by voice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spoken: bool,
    /// Why the transcription failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// `ping` for a connection check (never shown, never saved); empty for a person's question.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// The topic it belongs to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Undone by the person: kept (redo brings it back), not shown, not sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Question {
    /// A connection check, not a person's question.
    pub fn is_ping(&self) -> bool {
        self.kind == "ping"
    }
}

/// A note on a slide or a shape: a prepared answer to a question the audience is likely to ask
/// (`faq`), why it is done this way (`explanation`), an assumption, a source, or a check the
/// agent made (`check`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub anchor: String,
    #[serde(default = "explanation")]
    pub kind: String,
    /// For `faq`: the question; otherwise a short title.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub author: Actor,
    #[serde(default)]
    pub at: u64,
    /// The topic it belongs to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// A source's address and what is behind it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// The person pinned it (it stays open in the topic).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// The person removed it (undo brings it back).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

/// A topic's status (TOPICS.md §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A question or doubt, no decision yet.
    #[default]
    Open,
    /// Something here must change.
    Revise,
    /// Settled and accepted (only a person sets it).
    Agreed,
    /// Nothing to decide: an explanation, a source, a caveat.
    Info,
}

impl Status {
    pub const ALL: [Status; 4] = [Status::Open, Status::Revise, Status::Agreed, Status::Info];

    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Status::Open),
            "revise" => Some(Status::Revise),
            "agreed" => Some(Status::Agreed),
            "info" | "information" | "for information" => Some(Status::Info),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Revise => "revise",
            Status::Agreed => "agreed",
            Status::Info => "info",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Status::Open => "Open",
            Status::Revise => "Revise",
            Status::Agreed => "Agreed",
            Status::Info => "For information",
        }
    }
}

/// Where a topic applies: one or several anchors (shapes, words in a shape, the slide).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Place {
    pub id: String,
    pub anchors: Vec<String>,
    /// Why the agent added it ("depends on 3.1: same revenue figure").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub by: Actor,
    /// The person rejected the agent's place (undo brings it back).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub rejected: bool,
    /// Made by duplicating the shape of another place (a copy that goes away goes with it).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub copy_of: String,
}

/// One question or theme about the deck: places, a status, notes, a discussion (TOPICS.md).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Topic {
    pub id: String,
    pub places: Vec<Place>,
    pub status: Status,
    /// Free text with the status ("agreed with the CFO on 14.10, if capex is confirmed").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    pub status_by: Actor,
    pub status_at: u64,
    /// The agent proposes Agreed, with its reasons (the person confirms with one click).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proposal: String,
    /// Undone by the person (redo brings it back).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    pub at: u64,
}

fn explanation() -> String {
    "explanation".into()
}

/// The kinds of notes.
pub const NOTE_KINDS: &[&str] = &["faq", "explanation", "assumption", "source", "decision", "check"];

/// The layer's format version: 2 has topics (v1 decks are migrated when read).
pub const VERSION: u32 = 2;

/// One entry of the decision history / audit trail.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub at: u64,
    pub actor: Actor,
    /// `decision` (why the deck is the way it is) or `change` (a command that changed it).
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// What the change is based on (a source, a question id…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

/// Most log entries kept (oldest dropped first).
pub const MAX_LOG: usize = 5_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    pub version: u32,
    pub next_id: u64,
    pub anchors: Vec<Anchor>,
    pub topics: Vec<Topic>,
    pub questions: Vec<Question>,
    pub notes: Vec<Note>,
    /// Anchors of client data: the agent sees pseudonyms there and can't change it.
    pub private: Vec<String>,
    /// Pseudonym numbers, by a hash of the text they stand for (the text itself isn't stored).
    pub pseudonyms: Vec<(String, u32)>,
    pub log: Vec<LogEntry>,
    /// The background agent's session id (`claude --resume`), so reopening the deck continues it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent_session: String,
    /// What the agent knows for this document (the Agent tab's Context group); none yet: the
    /// last mode used in the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::context::Context>,
    /// Bumped on every change (not meaningful in the file; lets views cache).
    #[serde(skip)]
    pub revision: u64,
}

impl Layer {
    pub fn fresh_id(&mut self, prefix: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        format!("{prefix}{}", self.next_id)
    }

    pub fn anchor(&self, id: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|a| a.id == id)
    }

    pub fn log(&mut self, actor: Actor, kind: &str, text: String, command: &str, anchor: Option<String>, source: String) {
        let seq = self.log.last().map_or(1, |e| e.seq.saturating_add(1));
        self.log.push(LogEntry { seq, at: crate::now_ms(), actor, kind: kind.into(), text, command: command.into(), anchor, source });
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
            self.log.drain(..extra);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
            && self.log.is_empty()
            && self.questions.iter().all(Question::is_ping)
            && self.notes.is_empty()
            && self.topics.is_empty()
            && self.context.is_none()
    }

    /// The people's questions (connection checks left out).
    pub fn asked(&self) -> impl Iterator<Item = &Question> {
        self.questions.iter().filter(|q| !q.is_ping())
    }

    /// Anchors nothing refers to any more.
    pub fn unused_anchors(&self) -> Vec<String> {
        // Anchors that links inside texts point at (`anchor:<id>`).
        let mut in_text: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut scan = |t: &str| {
            for part in t.split("(anchor:").skip(1) {
                if let Some(id) = part.split(')').next() {
                    in_text.insert(id.to_string());
                }
            }
        };
        for q in &self.questions {
            scan(&q.text);
            if let Some(a) = &q.answer {
                scan(&a.text);
            }
        }
        for n in &self.notes {
            scan(&n.text);
            scan(&n.title);
        }
        for t in &self.topics {
            scan(&t.status_text);
            scan(&t.proposal);
        }
        let used = |id: &str| {
            self.questions.iter().any(|q| q.anchor == id || q.refs.iter().any(|r| r.anchor == id))
                || self.notes.iter().any(|n| n.anchor == id)
                || self.topics.iter().any(|t| t.places.iter().any(|p| p.anchors.iter().any(|a| a == id)))
                || self.private.iter().any(|p| p == id)
                || self.log.iter().any(|e| e.anchor.as_deref() == Some(id))
                || in_text.contains(id)
        };
        self.anchors.iter().filter(|a| !used(&a.id)).map(|a| a.id.clone()).collect()
    }

    pub fn topic(&self, id: &str) -> Option<&Topic> {
        self.topics.iter().find(|t| t.id == id)
    }

    /// Version 1 (questions and notes on anchors) to 2 (topics): one topic per question, its
    /// anchor as the place, its answer in the discussion, Open (For information when answered and
    /// read); notes on one anchor become the notes of a topic of their own, For information.
    pub fn migrate(&mut self) {
        if self.version >= VERSION {
            return;
        }
        let mut topics = std::mem::take(&mut self.topics);
        let questions = std::mem::take(&mut self.questions);
        let mut migrated = Vec::with_capacity(questions.len());
        for mut q in questions {
            if q.topic.is_empty() && !q.is_ping() {
                let tid = self.fresh_id("t");
                let pid = self.fresh_id("p");
                let read = q.status == QuestionStatus::Answered && q.seen;
                topics.push(Topic {
                    id: tid.clone(),
                    places: vec![Place { id: pid, anchors: vec![q.anchor.clone()], by: Actor::Human, ..Default::default() }],
                    status: if read { Status::Info } else { Status::Open },
                    status_by: Actor::Human,
                    status_at: q.asked_at,
                    at: q.asked_at,
                    ..Default::default()
                });
                q.topic = tid;
            }
            migrated.push(q);
        }
        self.questions = migrated;
        let mut notes = std::mem::take(&mut self.notes);
        for n in notes.iter_mut().filter(|n| n.topic.is_empty()) {
            let existing = topics
                .iter()
                .find(|t| {
                    t.status == Status::Info
                        && t.places.len() == 1
                        && t.places.first().is_some_and(|p| p.anchors == vec![n.anchor.clone()])
                        && t.at == 0
                })
                .map(|t| t.id.clone());
            let tid = match existing {
                Some(t) => t,
                None => {
                    let tid = self.fresh_id("t");
                    let pid = self.fresh_id("p");
                    topics.push(Topic {
                        id: tid.clone(),
                        places: vec![Place { id: pid, anchors: vec![n.anchor.clone()], by: n.author, ..Default::default() }],
                        status: Status::Info,
                        status_by: n.author,
                        ..Default::default()
                    });
                    tid
                }
            };
            n.topic = tid;
        }
        self.notes = notes;
        self.topics = topics;
        self.version = VERSION;
    }

    /// The pseudonym number for a text hash, given out once and kept.
    pub fn pseudonym(&mut self, hash: &str) -> u32 {
        if let Some((_, n)) = self.pseudonyms.iter().find(|(h, _)| h == hash) {
            return *n;
        }
        let n = self.pseudonyms.iter().map(|(_, n)| *n).max().unwrap_or(0).saturating_add(1);
        self.pseudonyms.push((hash.to_string(), n));
        n
    }
}

// ---------------------------------------------------------------- the store

static STORE: Mutex<Option<HashMap<u64, Arc<Layer>>>> = Mutex::new(None);

fn store() -> MutexGuard<'static, Option<HashMap<u64, Arc<Layer>>>> {
    match STORE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The layer of a document (read from its presentation the first time).
pub fn get(d: &DocState) -> Arc<Layer> {
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    map.entry(d.uid).or_insert_with(|| Arc::new(read_from(&d.doc))).clone()
}

/// Changes the layer of a document.
pub fn update<R>(d: &DocState, f: impl FnOnce(&mut Layer) -> R) -> R {
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    let entry = map.entry(d.uid).or_insert_with(|| Arc::new(read_from(&d.doc)));
    let layer = Arc::make_mut(entry);
    let r = f(layer);
    layer.revision = layer.revision.wrapping_add(1);
    r
}

/// Forgets a document's layer (it is read from the presentation again next time).
pub fn forget(uid: u64) {
    if let Some(map) = store().as_mut() {
        map.remove(&uid);
    }
}

/// Uids with a layer in the store.
pub fn known_docs() -> Vec<u64> {
    store().as_ref().map(|m| m.keys().copied().collect()).unwrap_or_default()
}

// ---------------------------------------------------------------- in the file

/// The layer kept in a presentation's custom XML part, or an empty one.
pub fn read_from(doc: &Presentation) -> Layer {
    for part in &doc.kept_parts {
        if part.xml.len() > MAX_PART || !part.xml.contains(NS) {
            continue;
        }
        if let Some(json) = between(&part.xml, "<![CDATA[", "]]>")
            && let Ok(mut layer) = serde_json::from_str::<Layer>(json)
        {
            layer.migrate();
            return layer;
        }
    }
    Layer { version: VERSION, ..Default::default() }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s.get(start..)?.rfind(close)? + start;
    s.get(start..end)
}

/// Is this kept part ours?
pub fn is_ours(k: &KeptPart) -> bool {
    k.xml.contains(NS) || k.name.eq_ignore_ascii_case(PART_NAME)
}

/// Writes the layer into the presentation's kept parts (replacing an earlier copy). An empty layer
/// removes the part.
pub fn write_into(doc: &mut Presentation, layer: &Layer) {
    doc.kept_parts.retain(|k| !is_ours(k));
    // Connection checks never go into the file.
    let mut layer = layer.clone();
    layer.version = VERSION;
    // Spoken questions still being transcribed live in memory only.
    layer.questions.retain(|q| !q.is_ping() && !q.status.is_draft());
    if layer.is_empty() && layer.agent_session.is_empty() {
        return;
    }
    let json = serde_json::to_string(&layer).unwrap_or_default();
    // CDATA can't hold `]]>`; JSON only has it inside strings, where `]` means the same.
    let json = json.replace("]]>", "\\u005d\\u005d>");
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<agentLayer xmlns=\"{NS}\"><![CDATA[{json}]]></agentLayer>");
    doc.kept_parts.push(KeptPart {
        name: PART_NAME.into(),
        content_type: "application/xml".into(),
        xml,
        rel_type: REL_CUSTOM_XML.into(),
        from_root: false,
    });
}

/// The layer of a document by its uid, if it has been read (for views that only have the
/// presentation, like Presenter View).
pub fn by_uid(uid: u64) -> Option<Arc<Layer>> {
    store().as_ref().and_then(|m| m.get(&uid).cloned())
}

/// The layer as it goes out with a deck that is sent (TOPICS.md §1): places, statuses and notes
/// stay; the discussion (questions, answers, the history's lines about them) is left out unless
/// the sender includes it.
pub fn for_sending(l: &Layer, discussion: bool) -> Layer {
    let mut out = l.clone();
    out.topics.retain(|t| !t.hidden);
    out.notes.retain(|n| !n.deleted);
    // The sender's context (a local folder, their way of working) stays with them: the reader's
    // app starts the deck in its own last mode.
    out.context = None;
    if !discussion {
        out.questions.clear();
        out.log.retain(|e| e.command != "agent.answer");
        out.agent_session.clear();
    } else {
        out.questions.retain(|q| !q.hidden && !q.is_ping());
    }
    out
}
