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
    for o in observers() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.after(s, id, params, result)));
    }
}
