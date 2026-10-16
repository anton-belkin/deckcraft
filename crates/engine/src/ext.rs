//! Extension points: commands and command observers registered at startup by crates built on top
//! of the engine (an app links them in; the engine knows nothing about them).
//!
//! - [`register_commands`] adds commands that behave like built-in ones: [`crate::find_command`]
//!   finds them and [`crate::Session::commands`] lists them, so the UI, the control channel, the
//!   CLI and MCP reach them like every other command.
//! - [`register_observer`] is told before and after every command [`crate::Session::execute`]
//!   runs, so an extension can keep its own per-document state in step with the presentation
//!   (load it when a file opens, write it into the presentation before a save, record who changed
//!   what). An observer may also take over a command ([`Observer::intercept`]): it then runs in
//!   the command's place, inside its undo step and panic guard.
//!
//! - [`push_step`] adds a custom undo step to the active document's ordinary history: an
//!   extension's own change (not a change of the presentation) that ⌘Z undoes and redoes in order
//!   with the person's edits.
//!
//! Registration is process-wide and additive; registering the same id twice keeps the first.

use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

use crate::cmd::CommandSpec;
use crate::{Result, Session};

/// Told about every command [`Session::execute`] runs.
pub trait Observer: Send + Sync {
    /// Before the command runs. Changes made to the active presentation here are part of the
    /// state the command starts from and are not an undo step.
    fn before(&self, _s: &mut Session, _id: &str, _params: &Value) {}
    /// Runs instead of the command when it returns `Some` (after the command's `enabled` check;
    /// the result is the command's, its changes are the command's undo step).
    fn intercept(&self, _s: &mut Session, _id: &str, _params: &Value) -> Option<Result<Value>> {
        None
    }
    /// After the command ran (its undo step, if any, is already recorded).
    fn after(&self, _s: &mut Session, _id: &str, _params: &Value, _result: &Result<Value>) {}
}

static COMMANDS: Mutex<Vec<&'static CommandSpec>> = Mutex::new(Vec::new());
static OBSERVERS: Mutex<Vec<&'static dyn Observer>> = Mutex::new(Vec::new());

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Adds extension commands. Ids already known (built-in or registered before) are skipped.
pub fn register_commands(specs: &'static [CommandSpec]) {
    let mut v = lock(&COMMANDS);
    for spec in specs {
        let known = crate::cmd::command_specs().iter().any(|c| c.id == spec.id) || v.iter().any(|c| c.id == spec.id);
        if !known {
            v.push(spec);
        }
    }
}

/// Adds a command observer.
pub fn register_observer(o: &'static dyn Observer) {
    lock(&OBSERVERS).push(o);
}

/// The registered extension commands.
pub fn commands() -> Vec<&'static CommandSpec> {
    lock(&COMMANDS).clone()
}

pub(crate) fn find(id: &str) -> Option<&'static CommandSpec> {
    lock(&COMMANDS).iter().find(|c| c.id == id).copied()
}

fn observers() -> Vec<&'static dyn Observer> {
    lock(&OBSERVERS).clone()
}

pub(crate) fn before(s: &mut Session, id: &str, params: &Value) {
    for o in observers() {
        // An observer that panics is skipped; the command still runs.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.before(s, id, params)));
    }
}

/// The first observer's replacement for the command, if one takes it over. Runs inside the
/// command's panic guard, so a panicking replacement fails the command and keeps the presentation.
pub(crate) fn intercept(s: &mut Session, id: &str, params: &Value) -> Option<Result<Value>> {
    observers().into_iter().find_map(|o| o.intercept(s, id, params))
}

pub(crate) fn after(s: &mut Session, id: &str, params: &Value, result: &Result<Value>) {
    if result.is_ok() && (id == "edit.undo" || id == "edit.redo") {
        run_steps(s, id == "edit.redo");
    }
    for o in observers() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.after(s, id, params, result)));
    }
}

// ---------------------------------------------------------------- custom undo steps

/// An extension's change that ⌘Z undoes and redoes, in order with the document's edits.
pub trait CustomStep: Send + Sync {
    fn undo(&self, s: &mut Session);
    fn redo(&self, s: &mut Session);
}

/// A pushed step: the presentation copies that identify it in the history (kept alive, so their
/// addresses are never reused), and the step.
struct Pushed {
    /// The history entry's copy: the document is this one right after the step was undone.
    undone: std::sync::Arc<crate::model::Presentation>,
    /// The document right after the step was done (or redone): what a redo lands on.
    done: std::sync::Arc<crate::model::Presentation>,
    step: std::sync::Arc<dyn CustomStep>,
}

static STEPS: Mutex<Vec<Pushed>> = Mutex::new(Vec::new());

/// Adds a custom undo step to the active document's history, labelled `label` ("Undo Ask the
/// Agent"). The presentation itself doesn't change: the entry holds an identical copy, so undoing
/// a snapshot step never runs the extension's undo and undoing this step never changes the slides.
pub fn push_step(s: &mut Session, label: &str, step: std::sync::Arc<dyn CustomStep>) -> Result<()> {
    let st = s.doc_mut()?;
    let undone = std::sync::Arc::new((*st.doc).clone());
    crate::push_undo(st, crate::HistoryEntry { label: label.to_string(), doc: undone.clone(), selection: st.selection.clone() });
    // A fresh copy as the current document: the step is a change (the file holds the layer).
    let done = std::sync::Arc::new((*st.doc).clone());
    st.doc = done.clone();
    st.revision += 1;
    let mut g = lock(&STEPS);
    g.push(Pushed { undone, done, step });
    // Steps whose copies are in no history any more can go (keep a generous tail).
    if g.len() > 4_000 {
        g.drain(..1_000);
    }
    Ok(())
}

/// After an undo or redo: if it was a custom step, run its undo or redo.
fn run_steps(s: &mut Session, redo: bool) {
    let Some(st) = s.active() else { return };
    let now = std::sync::Arc::as_ptr(&st.doc);
    let step = {
        let mut g = lock(&STEPS);
        let hit = g.iter_mut().find(|p| std::sync::Arc::as_ptr(if redo { &p.done } else { &p.undone }) == now);
        match hit {
            Some(p) => {
                if !redo {
                    // What a redo of this step will land on: the document it was undone from.
                    if let Some(top) = st.history.redo.last() {
                        p.done = top.doc.clone();
                    }
                }
                Some(p.step.clone())
            }
            None => None,
        }
    };
    if let Some(step) = step {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| if redo { step.redo(s) } else { step.undo(s) }));
        if r.is_err() {
            log::error!("a custom undo step panicked");
        }
    }
}
