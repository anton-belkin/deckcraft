//! The person's layer actions on the undo stack (TOPICS.md §4a): each pushes a custom step into
//! the deck's ordinary history (`deckcraft_engine::ext::push_step`), in order with their edits.
//! Steps are semantic, not snapshots of the layer: undoing a question never takes away what the
//! agent wrote meanwhile, and redoing it restores it as it was.
//!
//! - A question not yet taken: undo hides it (and the topic or places it made); redo re-sends it.
//! - Being worked on: undo withdraws it (the agent's answer is refused); redo re-sends it.
//! - Answered: undo hides the topic with its answer; redo restores it, without asking again.
//!
//! The agent's contributions are not on the person's stack.

use std::sync::Arc;

use deckcraft_engine::Session;
use deckcraft_engine::ext::CustomStep;

use crate::layer::{self, Actor, Layer, QuestionStatus, Status};

/// One undoable action of the person's.
#[derive(Clone, Debug)]
pub enum Op {
    /// A question, with the topic it made (if it made one) and the places it added.
    Ask {
        question: String,
        topic: Option<String>,
        places: Vec<String>,
    },
    /// A topic made without a question, or places added to a topic.
    Places {
        topic: Option<String>,
        places: Vec<String>,
    },
    /// A status change: before and after (status, text, by, at, the agent's proposal).
    Status {
        topic: String,
        before: (Status, String, Actor, u64, String),
        after: (Status, String, Actor, u64, String),
    },
    /// A place's anchors replaced (widened): before and after.
    Widen {
        place: String,
        before: Vec<String>,
        after: Vec<String>,
    },
    /// Client data marked (`on`) or unmarked on anchors.
    Private {
        anchors: Vec<String>,
        on: bool,
    },
    Pin {
        note: String,
        on: bool,
    },
    /// A note the person removed, or one they wrote.
    RemoveNote {
        note: String,
    },
    AddNote {
        note: String,
    },
    /// An agent's place rejected.
    Reject {
        place: String,
    },
    /// A proposal applied or rejected: its state before and after, and the decision note.
    Draft {
        topic: String,
        before: Option<crate::proposals::Proposal>,
        after: Option<crate::proposals::Proposal>,
        note: Option<String>,
    },
    /// What the agent knows for the document (mode, web, folder).
    Context {
        before: Option<crate::context::Context>,
        after: Option<crate::context::Context>,
    },
}

impl Op {
    pub fn label(&self) -> &'static str {
        match self {
            Op::Ask { .. } => "Ask the Agent",
            Op::Places { topic: Some(_), .. } => "New Topic",
            Op::Places { .. } => "Add Place",
            Op::Status { .. } => "Set Status",
            Op::Widen { .. } => "Widen Place",
            Op::Private { on: true, .. } => "Mark as Client Data",
            Op::Private { .. } => "Unmark Client Data",
            Op::Pin { .. } => "Pin Note",
            Op::RemoveNote { .. } => "Remove Note",
            Op::AddNote { .. } => "Add Note",
            Op::Reject { .. } => "Reject Place",
            Op::Context { .. } => "Agent Context",
            Op::Draft { note: Some(_), .. } => "Apply Proposal",
            Op::Draft { .. } => "Reject Proposal",
        }
    }

    fn places(l: &mut Layer, ids: &[String], rejected: bool) {
        for t in &mut l.topics {
            for p in t.places.iter_mut().filter(|p| ids.contains(&p.id)) {
                p.rejected = rejected;
            }
        }
    }

    fn topic(l: &mut Layer, id: &Option<String>, hidden: bool) {
        if let Some(id) = id
            && let Some(t) = l.topics.iter_mut().find(|t| &t.id == id)
        {
            t.hidden = hidden;
        }
    }

    pub fn undo(&self, l: &mut Layer) {
        match self {
            Op::Ask { question, topic, places } => {
                if let Some(q) = l.questions.iter_mut().find(|q| &q.id == question) {
                    if q.status == QuestionStatus::Working {
                        q.status = QuestionStatus::Withdrawn;
                    }
                    q.hidden = true;
                }
                Op::topic(l, topic, true);
                Op::places(l, places, true);
            }
            Op::Places { topic, places } => {
                Op::topic(l, topic, true);
                Op::places(l, places, true);
            }
            Op::Status { topic, before, .. } => set_status(l, topic, before),
            Op::Widen { place, before, .. } => set_anchors(l, place, before),
            Op::Private { anchors, on } => anchors.iter().for_each(|a| private(l, a, !on)),
            Op::Pin { note, on } => pin(l, note, !on),
            Op::RemoveNote { note } => removed(l, note, false),
            Op::AddNote { note } => removed(l, note, true),
            Op::Reject { place } => Op::places(l, std::slice::from_ref(place), false),
            Op::Context { before, .. } => l.context = before.clone(),
            Op::Draft { topic, before, note, .. } => {
                set_draft(l, topic, before);
                if let Some(n) = note {
                    removed(l, n, true);
                }
            }
        }
    }

    pub fn redo(&self, l: &mut Layer) {
        match self {
            Op::Ask { question, topic, places } => {
                if let Some(q) = l.questions.iter_mut().find(|q| &q.id == question) {
                    q.hidden = false;
                    // Not answered: sent again.
                    if q.answer.is_none() {
                        q.status = QuestionStatus::Pending;
                        q.asked_at = crate::now_ms();
                    }
                }
                Op::topic(l, topic, false);
                Op::places(l, places, false);
            }
            Op::Places { topic, places } => {
                Op::topic(l, topic, false);
                Op::places(l, places, false);
            }
            Op::Status { topic, after, .. } => set_status(l, topic, after),
            Op::Widen { place, after, .. } => set_anchors(l, place, after),
            Op::Private { anchors, on } => anchors.iter().for_each(|a| private(l, a, *on)),
            Op::Pin { note, on } => pin(l, note, *on),
            Op::RemoveNote { note } => removed(l, note, true),
            Op::AddNote { note } => removed(l, note, false),
            Op::Reject { place } => Op::places(l, std::slice::from_ref(place), true),
            Op::Context { after, .. } => l.context = after.clone(),
            Op::Draft { topic, after, note, .. } => {
                set_draft(l, topic, after);
                if let Some(n) = note {
                    removed(l, n, false);
                }
            }
        }
    }
}

fn set_status(l: &mut Layer, topic: &str, v: &(Status, String, Actor, u64, String)) {
    if let Some(t) = l.topics.iter_mut().find(|t| t.id == topic) {
        t.status = v.0;
        t.status_text = v.1.clone();
        t.status_by = v.2;
        t.status_at = v.3;
        t.proposal = v.4.clone();
    }
}

fn set_anchors(l: &mut Layer, place: &str, anchors: &[String]) {
    for t in &mut l.topics {
        if let Some(p) = t.places.iter_mut().find(|p| p.id == place) {
            p.anchors = anchors.to_vec();
        }
    }
}

fn private(l: &mut Layer, anchor: &str, on: bool) {
    l.private.retain(|a| a != anchor);
    if on {
        l.private.push(anchor.to_string());
    }
}

fn set_draft(l: &mut Layer, topic: &str, v: &Option<crate::proposals::Proposal>) {
    if let Some(t) = l.topics.iter_mut().find(|t| t.id == topic) {
        t.draft = v.clone();
    }
}

fn pin(l: &mut Layer, note: &str, on: bool) {
    if let Some(n) = l.notes.iter_mut().find(|n| n.id == note) {
        n.pinned = on;
    }
}

fn removed(l: &mut Layer, note: &str, deleted: bool) {
    if let Some(n) = l.notes.iter_mut().find(|n| n.id == note) {
        n.deleted = deleted;
    }
}

/// A step on one deck's history.
struct Step {
    uid: u64,
    op: Op,
}

impl Step {
    fn apply(&self, s: &Session, redo: bool) {
        if let Some(d) = s.documents().iter().find(|d| d.uid == self.uid) {
            layer::update(d, |l| if redo { self.op.redo(l) } else { self.op.undo(l) });
        }
    }
}

impl CustomStep for Step {
    fn undo(&self, s: &mut Session) {
        self.apply(s, false);
    }
    fn redo(&self, s: &mut Session) {
        self.apply(s, true);
    }
}

/// Puts the person's action (already done) on the active deck's undo stack.
pub fn push(s: &mut Session, op: Op) {
    let Ok(d) = s.doc() else { return };
    let uid = d.uid;
    let label = op.label();
    let _ = deckcraft_engine::ext::push_step(s, label, Arc::new(Step { uid, op }));
}
