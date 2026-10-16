//! Checks the deck's linked cells against their sources in the background: when a deck is opened
//! or a link pasted, then every 30 seconds. Results go to `links::remember`, which the badges, the
//! hover card and the pane read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deckcraft_agent::links;
use deckcraft_ui_egui::SlideApp;

/// Seconds between checks.
const EVERY: f64 = 30.0;

#[derive(Default)]
pub(crate) struct Checker {
    busy: Arc<AtomicBool>,
    last: Option<f64>,
    /// The document and its link ids at the last check (a new one checks at once).
    seen: (u64, Vec<String>),
    /// Asked for a check now (the pane's Check Now).
    pub now: bool,
}

impl Checker {
    pub fn tick(&mut self, app: &SlideApp, ctx: &egui::Context) {
        let Some(d) = app.session.active() else { return };
        let list: Vec<links::Link> = links::all(&d.doc).into_iter().map(|(_, _, l)| l).collect();
        if list.is_empty() {
            return;
        }
        let ids: Vec<String> = list.iter().map(|l| l.id.clone()).collect();
        let time = ctx.input(|i| i.time);
        let changed = self.seen.0 != d.uid || self.seen.1 != ids;
        let due = self.last.is_none_or(|t| time - t >= EVERY);
        if !(changed || due || self.now) || self.busy.load(Ordering::Acquire) {
            if !due {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(EVERY));
            }
            return;
        }
        self.now = false;
        self.last = Some(time);
        self.seen = (d.uid, ids);
        let deck = d.path.clone();
        let busy = self.busy.clone();
        busy.store(true, Ordering::Release);
        let ctx = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let spawned = std::thread::Builder::new().name("link-check".into()).spawn(move || {
                let gridcraft = links::gridcraft_addr();
                for l in &list {
                    let c = links::check(l, deck.as_deref(), Some(&gridcraft));
                    links::remember(&l.id, c);
                }
                busy.store(false, Ordering::Release);
                ctx.request_repaint();
            });
            if spawned.is_err() {
                self.busy.store(false, Ordering::Release);
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (list, deck, ctx);
            busy.store(false, Ordering::Release);
        }
    }
}
