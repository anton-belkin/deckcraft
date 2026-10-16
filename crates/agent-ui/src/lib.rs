//! The agent layer's UI (a DeckCraft fork feature), plugged in through `deckcraft_ui_egui::ext`:
//!
//! - topic markers on shapes (`~/aitocraft/TOPICS.md`): a small numbered circle at the shape's top
//!   right, coloured by the topic's status (blue while the agent works on it, a dot for an unread
//!   answer); small tags for client data and linked cells;
//! - hovering a linked table or picture shows where its numbers come from and what changed;
//! - the Agent pane: Selection (the topics of the selection, the question box at the bottom),
//!   Overview (every topic in deck order, readiness for sign-off, filters, linked cells, a copy to
//!   send), History;
//! - in Presenter View: prepared answers under the speaker notes, and the markers on the slide when
//!   the presenter turns them on;
//! - context-menu items: Ask the Agent, Mark as Client Data, Paste Linked as Picture, Update from
//!   Source, Open Source.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod checker;
mod context;
mod overlay;
mod pane;
mod ribbon;
#[cfg(not(target_arch = "wasm32"))]
pub mod voice;

use std::collections::HashSet;
use std::sync::OnceLock;

use deckcraft_agent::anchor::{self, Index, Place};
use deckcraft_agent::layer::{self, Layer, Status};
use deckcraft_agent::{links, topics};
use deckcraft_engine::model::{Presentation, ShapeId};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::ext::{SlideGeometry, UiExtension};
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Color32, CornerRadius, Pos2, Rect, RichText, Stroke, Ui, pos2, vec2};
use serde_json::json;

static SERVER: OnceLock<String> = OnceLock::new();

/// Registers the agent UI (call once, on the UI thread, before the first frame).
pub fn install() {
    deckcraft_agent::install();
    #[cfg(not(target_arch = "wasm32"))]
    deckcraft_engine::ext::register_commands(voice::SPECS);
    // Once per UI thread: a second registration would run every hook twice.
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !DONE.with(|d| d.replace(true)) {
        deckcraft_ui_egui::ext::register(Box::new(AgentUi::default()));
    }
}

pub use ribbon::set_stdio_entry;

static MODE_FILE: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Where the Agent tab remembers the mode the person chose.
pub fn set_mode_file(p: std::path::PathBuf) {
    let _ = MODE_FILE.set(p);
}

pub(crate) fn remember_mode(m: deckcraft_agent::listener::Mode) {
    if let Some(f) = MODE_FILE.get() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, m.id());
    }
}

/// Tells the Agent tab where the agent server listens.
pub fn set_server_url(url: String) {
    let _ = SERVER.set(url);
}

pub(crate) fn server_url() -> Option<&'static str> {
    SERVER.get().map(String::as_str)
}

thread_local! {
    /// A pane request for the UI thread that made it (tests run several UIs at once).
    static REQUEST: std::cell::RefCell<Option<(bool, String)>> = const { std::cell::RefCell::new(None) };
}

/// Opens (or closes) the Agent pane on a tab (`selection`, `overview`, `history`; the old `slide`,
/// `questions` and `links` still work) or on a topic (`topic:3`, `topic:t5`) from the next frame
/// (on the UI thread).
pub fn request_pane(open: bool, tab: &str) {
    REQUEST.with(|r| *r.borrow_mut() = Some((open, tab.to_string())));
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Selection,
    Overview,
    History,
}

/// The Overview's filters (none on: every topic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Filter {
    Status(Status),
    Unread,
    WithAgent,
}

#[derive(Default)]
pub(crate) struct AgentUi {
    pub open: bool,
    pub tab: Tab,
    /// Draft question for the selection, and whether its box should take focus.
    pub draft: String,
    pub focus_draft: bool,
    /// A topic opened from a marker, the Overview or a link (shown first on the Selection tab while
    /// the selection stays what it was then), and the place or note to point at.
    pub topic: Option<String>,
    pub topic_for: Option<(usize, Vec<ShapeId>)>,
    pub highlight_place: Option<String>,
    pub highlight_note: Option<String>,
    /// Notes opened, discussions opened or folded by hand.
    pub open_notes: HashSet<String>,
    pub discussion: std::collections::HashMap<String, bool>,
    /// A status being set: topic, status, why.
    pub status_edit: Option<(String, Status, String)>,
    pub filters: HashSet<Filter>,
    /// A copy to send includes the discussion.
    pub send_discussion: bool,
    /// The option of each topic's proposal shown (and previewed on the slide).
    pub options: std::collections::HashMap<String, String>,
    /// A reject being written: topic, comment.
    pub reject_edit: Option<(String, String)>,
    /// The File context view is open (instead of a tab), and an entry being written: id (empty
    /// for a new one), title, kind, text.
    pub file_context: bool,
    pub fc_edit: Option<(String, String, String, String)>,
    /// The link under the pointer in the pane: its target is highlighted on the slide.
    pub hover_link: Option<String>,
    /// Presenter View shows the topic markers (the presenter turns them on).
    pub presenter_markers: bool,
    #[cfg(not(target_arch = "wasm32"))]
    pub voice: voice::Voice,
    /// The document Presenter View shows (it only gets the presentation).
    pub presenter_uid: Option<u64>,
    pub checker: checker::Checker,
    pub ribbon: ribbon::State,
    /// Window focus and the active deck, last frame.
    pub focus: (bool, Option<u64>),
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
    pub open: Color32,
    pub revise: Color32,
    pub agreed: Color32,
    pub info: Color32,
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
        open: Color32::from_rgb(0xE0, 0x8A, 0x00),
        revise: Color32::from_rgb(0xD9, 0x30, 0x25),
        agreed: Color32::from_rgb(0x1E, 0x9E, 0x5A),
        info: Color32::from_rgb(0x8A, 0x8F, 0x98),
    }
}

/// A topic status's colour (TOPICS.md §1).
pub(crate) fn status_color(p: &Palette, s: Status) -> Color32 {
    match s {
        Status::Open => p.open,
        Status::Revise => p.revise,
        Status::Agreed => p.agreed,
        Status::Info => p.info,
    }
}

/// A topic's marker colour: its status, or blue (pulsing) while the agent works on it.
pub(crate) fn marker_color(p: &Palette, l: &Layer, t: &layer::Topic, time: f64) -> Color32 {
    if topics::working(l, t) {
        let a = 0.65 + 0.35 * (time * 3.0).sin().abs() as f32;
        return p.working.gamma_multiply(a);
    }
    status_color(p, t.status)
}

/// A marker: a small circle (a pill for `3.2`) with the number, white-rimmed, its right edge at
/// `right` and its centre at `right.y`; a dot at its top right for an unread answer. Its rect.
pub(crate) fn marker(painter: &egui::Painter, right: Pos2, label: &str, color: Color32, unread: bool, size: f32) -> Rect {
    let galley = painter.layout_no_wrap(label.to_string(), theme::bold(size * 0.62), Color32::WHITE);
    let w = (galley.size().x + size * 0.5).max(size);
    let r = Rect::from_min_size(pos2(right.x - w, right.y - size / 2.0), vec2(w, size));
    let round = CornerRadius::same((size / 2.0).round() as u8);
    painter.rect_filled(r, round, color);
    painter.rect_stroke(r, round, Stroke::new(1.2, Color32::WHITE), egui::StrokeKind::Outside);
    painter.galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    if unread {
        let c = pos2(r.right() - 1.0, r.top() + 1.0);
        painter.circle_filled(c, size * 0.2 + 1.2, Color32::WHITE);
        painter.circle_filled(c, size * 0.2, Color32::from_rgb(0x2D, 0x6C, 0xDF));
    }
    r
}

/// The markers of a slide: for each top-level shape (or `None`, the slide), its places as
/// (topic id, place id, number, colour, unread), in topic order.
pub(crate) type Markers = Vec<(Option<ShapeId>, Vec<(String, String, String, Color32, bool)>)>;

pub(crate) fn markers(doc: &Presentation, l: &Layer, slide: usize, p: &Palette, time: f64) -> Markers {
    let ix = Index::of(doc);
    let nums = topics::numbers(doc, &ix, l);
    let top_of = |id: ShapeId| {
        doc.slides
            .get(slide)
            .and_then(|sl| sl.shapes.iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some()))
            .map(|s| s.id)
    };
    let mut out: Markers = vec![];
    let mut on = topics::places_on(doc, &ix, l, slide);
    on.sort_by_key(|(t, _, _)| nums.topic(t));
    for (tid, pid, ws) in on {
        let Some(t) = l.topic(&tid) else { continue };
        let color = marker_color(p, l, t, time);
        let unread = topics::unread(l, t) > 0;
        let mut seen: Vec<Option<ShapeId>> = vec![];
        for w in ws {
            let at = w.shape.and_then(top_of).or(w.shape);
            if seen.contains(&at) {
                continue;
            }
            seen.push(at);
            let entry = (tid.clone(), pid.clone(), nums.place(&pid), color, unread);
            match out.iter_mut().find(|(k, _)| *k == at) {
                Some((_, v)) => {
                    if !v.iter().any(|x| x.0 == tid && x.1 == pid) {
                        v.push(entry);
                    }
                }
                None => out.push((at, vec![entry])),
            }
        }
    }
    out
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

/// Opens a topic on the Selection tab: goes to its place (`place`, or its first one) and shows
/// the topic first while that stays selected.
pub(crate) fn open_topic(st: &mut AgentUi, app: &mut SlideApp, tid: &str, place: Option<&str>) {
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);
    let ix = Index::of(&d.doc);
    let Some(t) = l.topic(tid) else { return };
    let mut places: Vec<(topics::Key, Place)> = topics::live(t)
        .filter(|p| place.is_none_or(|x| x == p.id))
        .flat_map(|p| topics::resolve(&d.doc, &ix, &l, p))
        .map(|(_, w)| (topics::key(&d.doc, &w), w))
        .collect();
    places.sort_by_key(|(k, _)| *k);
    if let Some((_, w)) = places.into_iter().next() {
        go_to(app, &Place { text: String::new(), ..w });
    }
    st.open = true;
    st.tab = Tab::Selection;
    st.topic = Some(tid.to_string());
    st.highlight_place = place.map(str::to_string);
    st.topic_for = app.session.active().map(|d| (d.selection.slide, d.selection.shapes.clone()));
}

/// Follows a link: `slide:3`, `shape:3/12`, `topic:3`, `place:3.2`, `note:3/n2`, a web page.
pub(crate) fn follow(st: &mut AgentUi, app: &mut SlideApp, href: &str) {
    if let Some(r) = href.strip_prefix("topic:") {
        let tid = app.session.active().and_then(|d| topics::find(&d.doc, &Index::of(&d.doc), &layer::get(d), r));
        if let Some(tid) = tid {
            open_topic(st, app, &tid, None);
        }
    } else if let Some(r) = href.strip_prefix("place:") {
        let hit = app.session.active().and_then(|d| topics::find_place(&d.doc, &Index::of(&d.doc), &layer::get(d), r));
        if let Some((tid, pid)) = hit {
            open_topic(st, app, &tid, Some(&pid));
        }
    } else if let Some(n) = href.strip_prefix("slide:").and_then(|n| n.trim().parse::<usize>().ok()).filter(|n| *n >= 1) {
        go_to(app, &Place::slide_only(n - 1));
    } else if let Some((n, id)) = href.strip_prefix("shape:").and_then(|x| x.split_once('/')) {
        if let (Ok(n), Ok(id)) = (n.trim().parse::<usize>(), id.trim().parse::<u32>())
            && n >= 1
        {
            go_to(app, &Place { slide: n - 1, shape: Some(ShapeId(id)), ..Default::default() });
        }
    } else if let Some(r) = href.strip_prefix("note:") {
        let id = r.rsplit('/').next().unwrap_or(r).to_string();
        let tid = app.session.active().and_then(|d| layer::get(d).notes.iter().find(|n| n.id == id).map(|n| n.topic.clone()));
        if let Some(tid) = tid.filter(|t| !t.is_empty()) {
            open_topic(st, app, &tid, None);
        }
        st.open = true;
        st.tab = Tab::Selection;
        st.open_notes.insert(id.clone());
        st.highlight_note = Some(id);
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
        .filter(|n| n.kind == "faq" && !n.deleted && l.anchor(&n.anchor).and_then(|a| anchor::resolve(doc, &ix, a)).is_some_and(|p| p.slide == slide))
        .collect()
}

impl UiExtension for AgentUi {
    fn frame(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        if let Some((open, tab)) = REQUEST.with(|r| r.borrow_mut().take()) {
            self.open = open;
            self.tab = match tab.as_str() {
                "overview" | "questions" | "links" => Tab::Overview,
                "history" => Tab::History,
                _ => Tab::Selection,
            };
            if let Some(r) = tab.strip_prefix("topic:") {
                follow(self, app, &format!("topic:{r}"));
            }
        }
        if let Some(d) = app.session.active() {
            self.presenter_uid = Some(d.uid);
            // Read the layer now, so Presenter View finds it.
            let _ = layer::get(d);
        }
        ribbon::keytip(self, app, ctx);
        // Voice questions: hold ⌥, speak, point; the release isn't a keytip.
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.voice.frame(app, ctx);
            if self.voice.key_used {
                self.ribbon.disarm();
            }
            voice::indicator(&self.voice, app, ctx);
        }
        // The app's way of reaching the agent follows the active document's context.
        deckcraft_agent::context::apply(&app.session);
        // The background agent (in that mode): started with the first question, fed each new one.
        deckcraft_agent::runner::tick(&mut app.session);
        // AItoCraft's registry: which decks are open, who listens (debounced; a heartbeat keeps
        // it fresh while frames are rare). Gone when the window closes.
        // focusedAt (AItoCraft picks "the open file" by it): when the window gains focus or another
        // deck becomes active.
        let now_focus = (ctx.input(|i| i.focused), app.session.active().map(|d| d.uid));
        if now_focus != self.focus
            && now_focus.0
            && let Some(uid) = now_focus.1
        {
            deckcraft_agent::documents::focused(uid);
        }
        self.focus = now_focus;
        if let Some(url) = server_url() {
            if ctx.input(|i| i.viewport().close_requested()) {
                deckcraft_agent::registry::remove();
            } else {
                deckcraft_agent::registry::publish(url, deckcraft_agent::documents::list(&app.session));
            }
        }
        self.checker.tick(app, ctx);
        // The Agent tab shows the status; the floating button only while the ribbon is collapsed.
        if app.show.is_none() && app.backstage.is_none() && app.ui.ribbon_collapsed {
            self.launcher(app, ctx);
        }
    }

    fn ribbon_tabs(&self, app: &SlideApp) -> Vec<String> {
        if app.session.active().is_some() { vec![ribbon::TAB.to_string()] } else { vec![] }
    }

    fn ribbon_tab(&mut self, app: &mut SlideApp, ui: &mut Ui, _name: &str) {
        ribbon::show(self, app, ui);
    }

    fn side_panel(&mut self, app: &mut SlideApp, ui: &mut Ui) {
        if self.open {
            pane::show(self, app, ui);
        }
    }

    fn paint_slide(&mut self, app: &mut SlideApp, ui: &mut Ui, painter: &egui::Painter, geo: &SlideGeometry) {
        overlay::paint(self, app, ui, painter, geo);
        #[cfg(not(target_arch = "wasm32"))]
        voice::bubbles(&self.voice, app, painter, geo);
    }

    fn context_menu(&mut self, app: &mut SlideApp, ui: &mut Ui) -> bool {
        ui.separator();
        if ui.button("Ask the Agent…").clicked() {
            self.open = true;
            self.tab = Tab::Selection;
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
        if topics::visible(&l).next().is_some() {
            ui.add_space(10.0);
            let text = RichText::new("Show topic markers on the slide").size(12.0).color(Color32::from_gray(200));
            ui.checkbox(&mut self.presenter_markers, text).on_hover_text(topics::readiness_text(&l));
        }
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

    fn presenter_slide(&mut self, ui: &mut Ui, rect: Rect, doc: &Presentation, slide: usize) {
        if !self.presenter_markers {
            return;
        }
        let Some(l) = self.presenter_uid.and_then(layer::by_uid) else { return };
        let t = Tokens::get(ui.ctx());
        let p = palette(&t);
        let time = ui.input(|i| i.time);
        let scale = rect.width() / doc.slide_size.width.max(1.0) as f32;
        let painter = ui.painter_at(rect.expand(16.0));
        let sel = deckcraft_engine::Selection { slide, ..Default::default() };
        let mut pulsing = false;
        for (shape, list) in markers(doc, &l, slide, &p, time) {
            let corner = match shape.and_then(|id| doc.slides.get(slide).and_then(|s| s.shapes.iter().find(|x| x.id == id))) {
                Some(sh) => {
                    let x = deckcraft_engine::cmd::xfrm_of(doc, &sel, sh);
                    pos2(rect.left() + (x.x + x.w) as f32 * scale, rect.top() + x.y as f32 * scale)
                }
                None => pos2(rect.right() - 6.0, rect.top() + 14.0),
            };
            let mut right = corner;
            for (tid, _, label, color, unread) in &list {
                pulsing |= l.topic(tid).is_some_and(|t| topics::working(&l, t));
                let r = marker(&painter, pos2(right.x + 8.0, right.y), label, *color, *unread, 16.0);
                right.x = r.left() - 3.0 - 8.0;
            }
        }
        if pulsing {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}

/// Presenter View's notes section for a document (what the hook draws: the markers toggle, turned
/// on, and the prepared answers), for offscreen renders and tests.
pub fn presenter_preview(ui: &mut Ui, doc: &Presentation, slide: usize, uid: u64) {
    let mut st = AgentUi { presenter_uid: Some(uid), presenter_markers: true, ..Default::default() };
    st.presenter_notes(ui, doc, slide);
}

/// The markers Presenter View draws over the slide's picture in `rect` (when the presenter turned
/// them on), for offscreen renders and tests.
pub fn presenter_slide_preview(ui: &mut Ui, rect: Rect, doc: &Presentation, slide: usize, uid: u64) {
    let mut st = AgentUi { presenter_uid: Some(uid), presenter_markers: true, ..Default::default() };
    st.presenter_slide(ui, rect, doc, slide);
}

impl AgentUi {
    /// The always-there Agent button at the bottom right of the canvas (while the ribbon is
    /// collapsed), with readiness for sign-off or what is waiting.
    fn launcher(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        let Some(canvas) = app.canvas_rect else { return };
        let Some(d) = app.session.active() else { return };
        let l = layer::get(d);
        let ready: usize = topics::visible(&l).map(|t| topics::unread(&l, t)).sum();
        let stale = links::all(&d.doc)
            .iter()
            .filter(|(_, _, k)| {
                links::last_check(&k.id).is_some_and(|c| matches!(c.state, links::State::Stale | links::State::Missing | links::State::Error))
            })
            .count();
        let t = Tokens::get(ctx);
        let p = palette(&t);
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let (all, agreed, revise, open, info) = topics::readiness(&l);
        let (text, color) = if ready > 0 {
            (format!("{ready} answer{} ready", plural(ready)), p.answered)
        } else if stale > 0 {
            (format!("{stale} linked table{} out of date", plural(stale)), p.stale)
        } else if all > 0 {
            let color = if revise > 0 {
                p.revise
            } else if open > 0 {
                p.open
            } else if agreed + info == all {
                p.agreed
            } else {
                t.accent
            };
            (topics::readiness_text(&l), color)
        } else {
            ("Agent".to_string(), t.accent)
        };
        if self.open && ready == 0 && stale == 0 {
            return;
        }
        let pos = egui::pos2(canvas.right() - 14.0, canvas.bottom() - 14.0);
        egui::Area::new(egui::Id::new("agent_launcher")).pivot(egui::Align2::RIGHT_BOTTOM).fixed_pos(pos).order(egui::Order::Foreground).show(
            ctx,
            |ui| {
                let b = egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
                    .fill(color)
                    .corner_radius(12.0)
                    .min_size(vec2(0.0, 26.0))
                    .wrap_mode(egui::TextWrapMode::Extend);
                if ui.add(b).on_hover_text("Topics, answers, notes and linked cells").clicked() {
                    self.open = true;
                    self.tab = Tab::Overview;
                }
            },
        );
    }
}

#[cfg(test)]
mod tests;
