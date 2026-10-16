//! The Agent tab on the ribbon: what isn't about one piece of content. Groups named the same in
//! GridCraft, WordCraft and DeckCraft:
//!
//! - **Agent**: the pane, with readiness for sign-off under it (`Agreed 3 of 5 · 1 new`);
//! - **Client data**: mark or unmark the selection;
//! - **Connection**: Connect Cowork (Claude Desktop's local servers) with where it stands, whether
//!   the server listens and an agent is active, Copy Claude Code command;
//! - **Links**: the deck's links to spreadsheet cells: how many are out of date, Check Now,
//!   Update Stale.
//!
//! Keytip **A** (Alt, A; F10, A on macOS) opens the tab.

use deckcraft_agent::cowork::Configured;
use deckcraft_agent::layer;
use deckcraft_agent::{links, topics};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::icons::{self, Icon};
use deckcraft_ui_egui::theme::{self, Tokens};
use deckcraft_ui_egui::widgets::{RIBBON_H, group, rows, small_button};
use egui::{Align2, Color32, CornerRadius, Response, RichText, Sense, Ui, pos2, vec2};
use serde_json::{Value as Json, json};

use crate::{AgentUi, Tab, palette, selection_private};

/// The tab's name.
pub const TAB: &str = "Agent";
/// Its keytip.
pub const KEYTIP: egui::Key = egui::Key::A;

#[derive(Default)]
pub(crate) struct State {
    /// What Claude Desktop's config says about us, when last looked.
    pub configured: Option<Configured>,
    /// Claude Desktop runs (only looked at after a connect, to say "open Claude").
    pub claude_running: bool,
    pub looked_at: Option<f64>,
    /// Keytips: the Alt key went down with nothing else pressed; armed after a lone Alt or F10.
    alt_down: bool,
    alt_used: bool,
    armed_at: Option<f64>,
    swallow_until: Option<f64>,
}

pub(crate) static STDIO_ENTRY: std::sync::OnceLock<Json> = std::sync::OnceLock::new();

/// The config entry Connect Cowork writes (this app in stdio mode, with its control port).
pub fn set_stdio_entry(e: Json) {
    let _ = STDIO_ENTRY.set(e);
}

/// A large ribbon button with a status line under its label, coloured.
pub(crate) fn status_button(ui: &mut Ui, icon: Icon, label: &str, sub: Option<(&str, Color32)>, enabled: bool, selected: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::font(11.0);
    let sub_font = theme::font(10.0);
    let lines: Vec<&str> = label.split('\n').collect();
    let width = |s: &str, f: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(s.to_string(), f.clone(), t.text).size().x);
    let mut w = lines.iter().map(|l| width(l, &font)).fold(0.0f32, f32::max);
    if let Some((s, _)) = sub {
        w = w.max(width(s, &sub_font));
    }
    let size = vec2((w + 12.0).max(44.0), RIBBON_H - 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter_at(rect);
    let bg = if selected {
        Some(t.selected)
    } else if resp.is_pointer_button_down_on() {
        Some(t.pressed)
    } else if resp.hovered() && enabled {
        Some(t.hover)
    } else {
        None
    };
    if let Some(c) = bg {
        p.rect_filled(rect, CornerRadius::same(4), c);
    }
    let icon_rect = egui::Rect::from_center_size(pos2(rect.center().x, rect.min.y + 13.0), vec2(22.0, 22.0));
    icons::paint(&p, icon_rect, icon, t.text, !enabled);
    let ink = if enabled { t.text } else { t.text_faint };
    let mut y = rect.min.y + 26.0;
    for l in &lines {
        p.text(pos2(rect.center().x, y), Align2::CENTER_TOP, *l, font.clone(), ink);
        y += 12.0;
    }
    if let Some((s, c)) = sub {
        p.text(pos2(rect.center().x, y), Align2::CENTER_TOP, s, sub_font, if enabled { c } else { t.text_faint });
    }
    let name = label.replace('\n', " ");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &name));
    resp
}

/// Alt, A (F10, A on macOS) opens the Agent tab. Runs before the app reads the keyboard, so the A
/// that finishes the keytip doesn't also type into the slide.
pub(crate) fn keytip(st: &mut AgentUi, app: &mut SlideApp, ctx: &egui::Context) {
    let s = &mut st.ribbon;
    let (now, alt, events) = ctx.input(|i| (i.time, i.modifiers.alt, i.events.clone()));
    // The letter that finished the keytip may arrive as text a frame later: it isn't typed.
    if let Some(until) = s.swallow_until {
        if now <= until && events.iter().any(|e| matches!(e, egui::Event::Text(t) if t.eq_ignore_ascii_case("a"))) {
            ctx.input_mut(|i| i.events.retain(|x| !matches!(x, egui::Event::Text(t) if t.eq_ignore_ascii_case("a"))));
            s.swallow_until = None;
            return;
        }
        if now > until {
            s.swallow_until = None;
        }
    }
    let key_down = |k: &egui::Event| matches!(k, egui::Event::Key { pressed: true, .. });
    if !cfg!(target_os = "macos") {
        if alt && !s.alt_down {
            s.alt_down = true;
            s.alt_used = false;
        }
        if alt && events.iter().any(key_down) {
            s.alt_used = true;
        }
        if !alt && s.alt_down {
            s.alt_down = false;
            if !s.alt_used {
                s.armed_at = Some(now);
                return;
            }
        }
    }
    for e in &events {
        if let egui::Event::Key { key: egui::Key::F10, pressed: true, modifiers, .. } = e
            && modifiers.is_none()
        {
            s.armed_at = Some(now);
            ctx.input_mut(|i| i.events.retain(|x| !matches!(x, egui::Event::Key { key: egui::Key::F10, .. })));
            return;
        }
    }
    let Some(at) = s.armed_at else { return };
    if now - at > 4.0 {
        s.armed_at = None;
        return;
    }
    let hit = events
        .iter()
        .any(|e| matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == KEYTIP && !modifiers.command && !modifiers.ctrl));
    if hit {
        s.armed_at = None;
        s.swallow_until = Some(now + 0.5);
        app.ui.tab = TAB.to_string();
        app.ui.ribbon_collapsed = false;
        ctx.input_mut(|i| {
            i.events.retain(|x| match x {
                egui::Event::Key { key, .. } => *key != KEYTIP,
                egui::Event::Text(t) => !t.eq_ignore_ascii_case("a"),
                _ => true,
            })
        });
    } else if events.iter().any(key_down) {
        s.armed_at = None;
    } else {
        // Show what the keytip does while it waits.
        let pos = app.canvas_rect.map(|r| r.left_top() + vec2(12.0, 8.0)).unwrap_or(pos2(12.0, 120.0));
        egui::Area::new(egui::Id::new("agent_keytip")).fixed_pos(pos).order(egui::Order::Tooltip).interactable(false).show(ctx, |ui| {
            let t = Tokens::get(ctx);
            egui::Frame::NONE.fill(t.card).stroke(egui::Stroke::new(1.0, t.card_border)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.label(RichText::new("A  Agent tab").size(12.0).color(t.text));
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);
    let ready: usize = topics::visible(&l).map(|x| topics::unread(&l, x)).sum();
    let working = topics::visible(&l).filter(|x| topics::working(&l, x)).count();
    let (all, agreed, revise, open, info) = topics::readiness(&l);
    let all_links = links::all(&d.doc);
    let checks: Vec<Option<links::Check>> = all_links.iter().map(|(_, _, k)| links::last_check(&k.id)).collect();
    let stale: Vec<u32> = all_links
        .iter()
        .zip(&checks)
        .filter(|(_, c)| c.as_ref().is_some_and(|c| c.state == links::State::Stale))
        .map(|((_, id, _), _)| id.0)
        .collect();
    let broken = checks.iter().filter(|c| c.as_ref().is_some_and(|c| matches!(c.state, links::State::Missing | links::State::Error))).count();
    let marks = l.private.len();

    // Agent: the pane, with readiness for sign-off under it (and what is new).
    group(ui, |ui| {
        let sub = if all == 0 {
            ("no topics".to_string(), t.text_faint)
        } else {
            let color = if ready > 0 {
                p.answered
            } else if working > 0 {
                p.working
            } else if revise > 0 {
                p.revise
            } else if open > 0 {
                p.open
            } else {
                p.agreed
            };
            let mut s = format!("Agreed {agreed} of {}", all - info);
            if ready > 0 {
                s.push_str(&format!(" · {ready} new"));
            } else if working > 0 {
                s.push_str(" · in work");
            }
            (s, color)
        };
        let tip = format!("The Agent pane: the selection's topics, the overview, the history (keytip A)\n{}", topics::readiness_text(&l));
        if status_button(ui, Icon::Sparkle, "Agent", Some((&sub.0, sub.1)), true, st.open).on_hover_text(tip).clicked() {
            st.open = !st.open;
            if st.open && ready > 0 {
                st.tab = Tab::Overview;
            }
        }
    });

    // Client data: mark or unmark the selection.
    group(ui, |ui| {
        let sel = selection_private(app);
        let private = sel.is_some_and(|(_, _, p)| p);
        let (icon, label) = if private { (Icon::Unlock, "Unmark\nClient Data") } else { (Icon::Lock, "Mark as\nClient Data") };
        let tip = if private {
            "Only a person can lift the mark"
        } else {
            "The agent sees pseudonyms like [client-1] for this text, wherever it appears in the deck, and can't change it"
        };
        if status_button(ui, icon, label, None, sel.is_some(), private).on_hover_text(tip).clicked()
            && let Some((s, id, on)) = sel
        {
            let _ = app.run("agent.private", json!({"slide": s + 1, "shape": id.0, "on": !on}));
        }
        rows(ui, |ui| {
            ui.add_space(2.0);
            ui.label(RichText::new(format!("Marks ({marks})")).size(11.5).color(t.text_dim)).on_hover_text("Client-data marks in this deck");
        });
    });

    // Connection: is anyone listening, the mode, what to copy.
    crate::connection::group(st, app, ui);

    // Links: the deck's links to spreadsheet cells.
    group(ui, |ui| {
        let n = all_links.len();
        let (sub, color) = if n == 0 {
            ("none".to_string(), t.text_faint)
        } else if broken > 0 {
            (format!("{} out of date", stale.len() + broken), p.broken)
        } else if !stale.is_empty() {
            (format!("{} out of date", stale.len()), p.stale)
        } else {
            ("up to date".to_string(), p.linked)
        };
        if status_button(ui, Icon::Table, "Linked\nCells", Some((&sub, color)), n > 0, false)
            .on_hover_text("Tables and pictures linked to spreadsheet cells: details in the pane's Overview")
            .clicked()
        {
            st.open = true;
            st.tab = Tab::Overview;
        }
        rows(ui, |ui| {
            if small_button(ui, Icon::Redo, "Check Now", n > 0, false)
                .on_hover_text("Checked every 30 s against the workbooks (or a running GridCraft)")
                .clicked()
            {
                st.checker.now = true;
            }
            if small_button(ui, Icon::Check, "Update Stale", !stale.is_empty(), false)
                .on_hover_text("Bring the new values into every out-of-date link")
                .clicked()
            {
                let at = app.session.active().map(|d| d.selection.slide);
                for id in &stale {
                    let _ = app.run("link.update", json!({"id": id}));
                }
                if let Some(i) = at {
                    let _ = app.run("slide.go", json!({"index": i}));
                }
            }
        });
    });
}
