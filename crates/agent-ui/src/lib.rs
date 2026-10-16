//! The agent layer's UI (a DeckCraft fork feature), plugged in through `deckcraft_ui_egui::ext`:
//!
//! - badges on shapes: a question waiting / being worked on / answer ready, agent notes, client
//!   data, linked cells (current, out of date, source missing); on the slide, its prepared answers;
//! - hovering a linked table or picture shows where its numbers come from and what changed;
//! - the Agent pane: ask about the selection, read answers and prepared answers with their links,
//!   mark client data, linked cells with Update and Open Source, the history, connecting an agent;
//! - prepared answers under the speaker notes in Presenter View;
//! - context-menu items: Ask the Agent, Mark as Client Data, Paste Linked as Picture, Update from
//!   Source, Open Source.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod checker;
mod overlay;
mod pane;

use std::sync::OnceLock;

use deckcraft_agent::anchor::{self, Index, Place};
use deckcraft_agent::layer::{self, Layer, QuestionStatus};
use deckcraft_agent::links;
use deckcraft_engine::model::{Presentation, ShapeId};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::ext::{SlideGeometry, UiExtension};
use deckcraft_ui_egui::theme::Tokens;
use egui::{Color32, RichText, Ui, vec2};
use serde_json::json;

static SERVER: OnceLock<String> = OnceLock::new();

/// Registers the agent UI (call once, on the UI thread, before the first frame).
pub fn install() {
    deckcraft_agent::install();
    deckcraft_ui_egui::ext::register(Box::new(AgentUi::default()));
}

/// Tells the pane where the agent server listens (shown under Connect).
pub fn set_server_url(url: String) {
    let _ = SERVER.set(url);
}

pub(crate) fn server_url() -> Option<&'static str> {
    SERVER.get().map(String::as_str)
}

static REQUEST: std::sync::Mutex<Option<(bool, String)>> = std::sync::Mutex::new(None);

/// Opens (or closes) the Agent pane on a tab (`slide`, `questions`, `links`, `history`,
/// `connect`) from the next frame.
pub fn request_pane(open: bool, tab: &str) {
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some((open, tab.to_string()));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Slide,
    Questions,
    Links,
    History,
    Connect,
}

#[derive(Default)]
pub(crate) struct AgentUi {
    pub open: bool,
    pub tab: Tab,
    /// Draft question for the selection, and whether its box should take focus.
    pub draft: String,
    pub focus_draft: bool,
    /// A note to scroll to.
    pub highlight_note: Option<String>,
    /// The document Presenter View shows (it only gets the presentation).
    pub presenter_uid: Option<u64>,
    pub checker: checker::Checker,
}

/// The agent layer's colours (not in the theme: they mean the same in light and dark).
pub(crate) struct Palette {
    pub pending: Color32,
    pub working: Color32,
    pub answered: Color32,
    pub seen: Color32,
    pub note: Color32,
    pub private: Color32,
    pub agent: Color32,
    pub person: Color32,
    pub stale: Color32,
    pub broken: Color32,
    pub linked: Color32,
}

pub(crate) fn palette(t: &Tokens) -> Palette {
    Palette {
        pending: Color32::from_rgb(0xE0, 0x8A, 0x00),
        working: Color32::from_rgb(0x2D, 0x6C, 0xDF),
        answered: Color32::from_rgb(0x1E, 0x9E, 0x5A),
        seen: t.text_faint,
        note: Color32::from_rgb(0x7B, 0x4F, 0xD6),
        private: if t.dark { Color32::from_rgb(0x8F, 0xA6, 0xC8) } else { Color32::from_rgb(0x4A, 0x62, 0x8A) },
        agent: Color32::from_rgb(0x7B, 0x4F, 0xD6),
        person: t.accent,
        stale: Color32::from_rgb(0xD9, 0x7A, 0x00),
        broken: Color32::from_rgb(0xC6, 0x28, 0x28),
        linked: Color32::from_rgb(0x1E, 0x9E, 0x5A),
    }
}

pub(crate) fn state_color(p: &Palette, s: links::State) -> Color32 {
    match s {
        links::State::Current => p.linked,
        links::State::Stale => p.stale,
        links::State::Missing | links::State::Error => p.broken,
        links::State::Unchecked => p.seen,
    }
}

pub(crate) fn state_label(s: links::State) -> &'static str {
    match s {
        links::State::Current => "up to date",
        links::State::Stale => "out of date",
        links::State::Missing => "source missing",
        links::State::Error => "source unreadable",
        links::State::Unchecked => "not checked yet",
    }
}

/// "3 min ago", "2 h ago", "5 d ago".
pub(crate) fn ago(ms: u64) -> String {
    let now = deckcraft_agent::now_ms();
    if ms == 0 || now < ms {
        return String::new();
    }
    let s = (now - ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// Where an anchor of the active document is.
pub(crate) fn place_of(app: &SlideApp, l: &Layer, anchor_id: &str) -> Option<Place> {
    let d = app.session.active()?;
    anchor::resolve_id(d, l, anchor_id)
}

/// Selects a place: goes to its slide and selects its shape.
pub(crate) fn go_to(app: &mut SlideApp, place: &Place) {
    let _ = app.run("slide.go", json!({"index": place.slide}));
    if let Some(id) = place.shape {
        let _ = app.run("selection.select", json!({"ids": [id.0]}));
    }
}

/// Follows a link: `slide:3`, `shape:3/12`, `note:n3`, a web page.
pub(crate) fn follow(st: &mut AgentUi, app: &mut SlideApp, href: &str) {
    if let Some(n) = href.strip_prefix("slide:").and_then(|n| n.trim().parse::<usize>().ok()).filter(|n| *n >= 1) {
        go_to(app, &Place::slide_only(n - 1));
    } else if let Some((n, id)) = href.strip_prefix("shape:").and_then(|x| x.split_once('/')) {
        if let (Ok(n), Ok(id)) = (n.trim().parse::<usize>(), id.trim().parse::<u32>())
            && n >= 1
        {
            go_to(app, &Place { slide: n - 1, shape: Some(ShapeId(id)), ..Default::default() });
        }
    } else if let Some(id) = href.strip_prefix("note:") {
        st.open = true;
        st.tab = Tab::Slide;
        st.highlight_note = Some(id.to_string());
    } else if href.starts_with("https://") || href.starts_with("http://") {
        app.open_url(href);
    }
}

/// Is the selected shape (or the shape being edited) marked as client data?
pub(crate) fn selection_private(app: &SlideApp) -> Option<(usize, ShapeId, bool)> {
    let d = app.session.active()?;
    let id = d.selection.text.as_ref().filter(|t| !t.notes).map(|t| t.shape).or_else(|| d.selection.shapes.first().copied())?;
    let set = deckcraft_agent::privacy::private_shapes(&app.session);
    Some((d.selection.slide, id, set.contains(&(d.selection.slide, id))))
}

/// The linked shape that is selected, with its link.
pub(crate) fn selected_link(app: &SlideApp) -> Option<(ShapeId, links::Link)> {
    let d = app.session.active()?;
    let id = *d.selection.shapes.first()?;
    Some((id, links::of(d.shape(id)?)?))
}

/// Prepared answers (faq notes) on a slide of a presentation.
pub(crate) fn prepared_answers<'a>(doc: &Presentation, l: &'a Layer, slide: usize) -> Vec<&'a layer::Note> {
    let ix = Index::of(doc);
    l.notes
        .iter()
        .filter(|n| n.kind == "faq" && l.anchor(&n.anchor).and_then(|a| anchor::resolve(doc, &ix, a)).is_some_and(|p| p.slide == slide))
        .collect()
}

impl UiExtension for AgentUi {
    fn frame(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        if let Some((open, tab)) = REQUEST.lock().ok().and_then(|mut r| r.take()) {
            self.open = open;
            self.tab = match tab.as_str() {
                "questions" => Tab::Questions,
                "links" => Tab::Links,
                "history" => Tab::History,
                "connect" => Tab::Connect,
                _ => Tab::Slide,
            };
        }
        if let Some(d) = app.session.active() {
            self.presenter_uid = Some(d.uid);
            // Read the layer now, so Presenter View finds it.
            let _ = layer::get(d);
        }
        self.checker.tick(app, ctx);
        if app.show.is_none() && app.backstage.is_none() {
            self.launcher(app, ctx);
        }
    }

    fn side_panel(&mut self, app: &mut SlideApp, ui: &mut Ui) {
        if self.open {
            pane::show(self, app, ui);
        }
    }

    fn paint_slide(&mut self, app: &mut SlideApp, ui: &mut Ui, painter: &egui::Painter, geo: &SlideGeometry) {
        overlay::paint(self, app, ui, painter, geo);
    }

    fn context_menu(&mut self, app: &mut SlideApp, ui: &mut Ui) -> bool {
        ui.separator();
        if ui.button("Ask the Agent…").clicked() {
            self.open = true;
            self.tab = Tab::Slide;
            self.focus_draft = true;
            return true;
        }
        if let Some((slide, id, private)) = selection_private(app)
            && ui.button(if private { "Unmark Client Data" } else { "Mark as Client Data" }).clicked()
        {
            let _ = app.run("agent.private", json!({"slide": slide + 1, "shape": id.0, "on": !private}));
            return true;
        }
        if let Some((id, _)) = selected_link(app) {
            if ui.button("Update from Source").clicked() {
                let _ = app.run("link.update", json!({"id": id.0}));
                return true;
            }
            if ui.button("Open Source").clicked() {
                let _ = app.run("link.open", json!({"id": id.0}));
                return true;
            }
        }
        if ui.button("Paste Linked as Picture").clicked() {
            let html = app.services.clipboard_html.as_mut().and_then(|f| f()).unwrap_or_default();
            if links::from_html(&html).is_some() {
                let _ = app.run("link.paste", json!({"html": html, "mode": "picture"}));
            } else {
                app.set_status("No linked cells on the clipboard: in GridCraft, use Copy Link to Cells first");
            }
            return true;
        }
        if ui.button(if self.open { "Hide Agent Pane" } else { "Agent Pane" }).clicked() {
            self.open = !self.open;
            return true;
        }
        false
    }

    fn presenter_notes(&mut self, ui: &mut Ui, doc: &Presentation, slide: usize) {
        let Some(l) = self.presenter_uid.and_then(layer::by_uid) else { return };
        let faq = prepared_answers(doc, &l, slide);
        if faq.is_empty() {
            return;
        }
        ui.add_space(14.0);
        ui.label(RichText::new(format!("Prepared answers ({})", faq.len())).size(13.0).color(Color32::from_gray(170)));
        for n in faq {
            ui.add_space(6.0);
            egui::Frame::NONE.fill(Color32::from_gray(36)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if !n.title.is_empty() {
                    ui.label(RichText::new(&n.title).strong().size(15.0).color(Color32::WHITE));
                }
                ui.label(RichText::new(&n.text).size(14.0).color(Color32::from_gray(225)));
            });
        }
    }
}

/// Presenter View's prepared-answers section for a document (what the hook draws), for
/// offscreen renders and tests.
pub fn presenter_preview(ui: &mut Ui, doc: &Presentation, slide: usize, uid: u64) {
    let mut st = AgentUi { presenter_uid: Some(uid), ..Default::default() };
    st.presenter_notes(ui, doc, slide);
}

impl AgentUi {
    /// The always-there Agent button at the bottom right of the canvas, with what is waiting.
    fn launcher(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        let Some(canvas) = app.canvas_rect else { return };
        let Some(d) = app.session.active() else { return };
        let l = layer::get(d);
        let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
        let waiting = l.questions.iter().filter(|q| q.status != QuestionStatus::Answered).count();
        let stale = links::all(&d.doc)
            .iter()
            .filter(|(_, _, k)| {
                links::last_check(&k.id).is_some_and(|c| matches!(c.state, links::State::Stale | links::State::Missing | links::State::Error))
            })
            .count();
        let t = Tokens::get(ctx);
        let p = palette(&t);
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let (text, color, tab) = if ready > 0 {
            (format!("{ready} answer{} ready", plural(ready)), p.answered, Tab::Questions)
        } else if stale > 0 {
            (format!("{stale} linked table{} out of date", plural(stale)), p.stale, Tab::Links)
        } else if waiting > 0 {
            (format!("{waiting} question{} with the agent", plural(waiting)), p.pending, Tab::Questions)
        } else {
            ("Agent".to_string(), t.accent, Tab::Slide)
        };
        if self.open && ready == 0 && waiting == 0 && stale == 0 {
            return;
        }
        let pos = egui::pos2(canvas.right() - 14.0, canvas.bottom() - 14.0);
        egui::Area::new(egui::Id::new("agent_launcher")).pivot(egui::Align2::RIGHT_BOTTOM).fixed_pos(pos).order(egui::Order::Foreground).show(
            ctx,
            |ui| {
                let b =
                    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(color).corner_radius(12.0).min_size(vec2(0.0, 26.0));
                if ui.add(b).on_hover_text("Questions, answers, notes and linked cells").clicked() {
                    self.open = true;
                    self.tab = tab;
                }
            },
        );
    }
}

#[cfg(test)]
mod tests;
