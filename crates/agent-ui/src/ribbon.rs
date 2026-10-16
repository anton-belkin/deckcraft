//! The Agent tab on the ribbon: what isn't about one piece of content. Groups named the same in
//! GridCraft, WordCraft and DeckCraft:
//!
//! - **Agent**: the pane, with readiness for sign-off under it (`Agreed 3 of 5 · 1 new`);
//! - **Client data**: mark or unmark the selection;
//! - **Context**: what the agent knows (No context, Claude Cowork, Claude Code), whether it is
//!   ready, and how to get it ready (`context.rs`);
//! - **Links**: the deck's links to spreadsheet cells: how many are out of date, Check Now,
//!   Update Stale.
//!
//! - **Voice**: Ask by Voice (hold ⌥ or click, speak, point at shapes and slides) and the speech
//!   model, off until the person picks Small or Turbo;
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
    /// The Context group's own state.
    pub context: crate::context::State,
}

impl State {
    /// A lone ⌥ that was a voice question doesn't open the keytips.
    pub(crate) fn disarm(&mut self) {
        self.armed_at = None;
        self.alt_used = true;
    }
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
    let working_now = !deckcraft_agent::context::working(d).is_empty();
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
        let b = status_button(ui, Icon::Sparkle, "Agent", Some((&sub.0, sub.1)), true, st.open);
        // The agent is working on this deck: the same small blue pulse as the Context lamp.
        if working_now {
            let time = ui.input(|i| i.time);
            let dot = pos2(b.rect.center().x + 12.0, b.rect.min.y + 6.0);
            ui.painter().circle_filled(dot, 4.5, crate::context::pulse(p.working, time));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        if b.on_hover_text(tip).clicked() {
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

    // Context: what the agent knows, whether it is ready, how to get it ready.
    crate::context::group(st, app, ui);

    // Voice: ask by speaking and pointing, and the speech model.
    #[cfg(not(target_arch = "wasm32"))]
    voice_group(st, app, ui);

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

/// The Voice group: the microphone (status under it) and the model, off until the person picks one.
#[cfg(not(target_arch = "wasm32"))]
fn voice_group(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    use crate::voice::{KEY, RED, Status};
    use craft_voice::Model;
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let ctx = ui.ctx().clone();
    let v = &st.voice;
    let (status, color, tip): (String, Color32, String) = match (v.meter(), v.status()) {
        (Some((secs, _)), _) => (format!("{secs:.0}s"), RED, "Listening. Click the shapes or slides you mean; click again to send".into()),
        (None, Status::Off) => ("Off".into(), t.text_dim, "Choose a speech model (right) to ask by voice".into()),
        (None, Status::Downloading(done, total)) => {
            let pct = if *total > 0 { *done as f64 * 100.0 / *total as f64 } else { 0.0 };
            (format!("Downloading {pct:.0}%"), p.working, format!("Downloading the speech model ({} of {} MB)", done / 1_000_000, total / 1_000_000))
        }
        (None, Status::Loading) => ("Loading…".into(), p.working, "Loading the speech model".into()),
        (None, Status::Failed(e)) => ("Model error".into(), RED, format!("{e}. Choose the model again to retry")),
        (None, Status::Idle | Status::Ready) => (
            // Words, not "⌥": the ribbon font has no such glyph.
            if cfg!(target_os = "macos") { "Hold Option".to_string() } else { "Hold Alt".to_string() },
            t.text_dim,
            format!("Hold {KEY}, speak, click the shapes or slides you mean; release to send. Or click here to start and again to send"),
        ),
    };
    let on = v.recording();
    let downloading = match v.status() {
        Status::Downloading(done, total) if *total > 0 => Some(*done as f32 / *total as f32),
        _ => None,
    };
    let choice = v.choice();
    group(ui, |ui| {
        let resp = mic_button(ui, "Ask by\nVoice", (&status, color), on, &tip);
        if let Some(k) = downloading {
            let r = resp.rect;
            let bar = egui::Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() + 0.5), vec2(r.width() - 12.0, 2.0));
            ui.painter().rect_filled(bar, 1.0, t.separator);
            ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * k.clamp(0.0, 1.0), bar.height())), 1.0, p.working);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        if resp.clicked() {
            if choice.is_some() || st.voice.recording() {
                st.voice.toggle(app, &ctx);
            } else {
                st.voice.choose(app, &ctx, Some(Model::Small));
            }
        }
        rows(ui, |ui| {
            let here = |m: Model| craft_voice::default_dir().is_some_and(|d| m.is_ready(&d));
            let size = |m: Model| if here(m) { String::new() } else { format!(" · {} MB", m.download_size() / 1_000_000) };
            let mut pick = None;
            if radio(ui, "Voice Off", choice.is_none(), "No voice questions (the model is unloaded)").clicked() {
                pick = Some(None);
            }
            if radio(
                ui,
                &format!("Small{}", size(Model::Small)),
                choice == Some(Model::Small),
                "Whisper small, 5-bit: fast (about 1–1.5 s for a 5-second question on an M1 Pro). Downloaded once, on this computer; audio never leaves it",
            )
            .clicked()
            {
                pick = Some(Some(Model::Small));
            }
            if radio(
                ui,
                &format!("Turbo, slower{}", size(Model::Turbo)),
                choice == Some(Model::Turbo),
                "Whisper large-v3-turbo, 5-bit: more accurate with numbers and names, slower (about 2 s for a 5-second question on an M1 Pro). Downloaded once, on this computer; audio never leaves it",
            )
            .clicked()
            {
                pick = Some(Some(Model::Turbo));
            }
            if let Some(m) = pick {
                st.voice.choose(app, &ctx, m);
            }
        });
    });
}

/// A radio row on the ribbon.
#[cfg(not(target_arch = "wasm32"))]
fn radio(ui: &mut Ui, label: &str, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::font(12.0);
    let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 26.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 21.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let c = pos2(rect.left() + 9.0, rect.center().y);
    ui.painter().circle_stroke(c, 5.0, egui::Stroke::new(1.0, t.text_dim));
    if on {
        ui.painter().circle_filled(c, 3.0, t.accent);
    }
    ui.painter().text(pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, label, font, if on { t.text } else { t.text_dim });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, label));
    resp.on_hover_text(tip)
}

/// The tall microphone button, with its status under the label.
#[cfg(not(target_arch = "wasm32"))]
fn mic_button(ui: &mut Ui, label: &str, sub: (&str, Color32), on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::font(11.0);
    let sub_font = theme::font(10.0);
    let lines: Vec<&str> = label.split('\n').collect();
    let width = |s: &str, f: &egui::FontId| ui.fonts_mut(|x| x.layout_no_wrap(s.to_string(), f.clone(), t.text).size().x);
    let w = lines.iter().map(|l| width(l, &font)).fold(width(sub.0, &sub_font), f32::max);
    let (rect, resp) = ui.allocate_exact_size(vec2((w + 12.0).max(44.0), RIBBON_H - 4.0), Sense::click());
    let p = ui.painter_at(rect);
    if on {
        p.rect_filled(rect, CornerRadius::same(4), t.selected);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(4), t.hover);
    }
    let icon = egui::Rect::from_center_size(pos2(rect.center().x, rect.min.y + 13.0), vec2(22.0, 22.0));
    mic(&p, icon, if on { crate::voice::RED } else { t.text }, on);
    let mut y = rect.min.y + 26.0;
    for l in &lines {
        p.text(pos2(rect.center().x, y), Align2::CENTER_TOP, *l, font.clone(), t.text);
        y += 12.0;
    }
    p.text(pos2(rect.center().x, y), Align2::CENTER_TOP, sub.0, sub_font, sub.1);
    let name = label.replace('\n', " ");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    resp.on_hover_text(tip)
}

/// A microphone, drawn (the icon set has none): capsule, cradle, stem and base; filled while
/// recording.
#[cfg(not(target_arch = "wasm32"))]
fn mic(p: &egui::Painter, r: egui::Rect, color: Color32, on: bool) {
    let k = r.width() / 26.0;
    let c = r.center();
    let stroke = egui::Stroke::new(1.6, color);
    let body = egui::Rect::from_center_size(c + vec2(0.0, -4.0 * k), vec2(8.0 * k, 13.0 * k));
    if on {
        p.rect_filled(body, 4.0 * k, color);
    } else {
        p.rect_stroke(body, 4.0 * k, stroke, egui::StrokeKind::Middle);
    }
    let pts: Vec<egui::Pos2> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 16.0);
            c + vec2(7.0 * k * a.cos(), k * (6.0 * a.sin()))
        })
        .collect();
    p.add(egui::Shape::line(pts, stroke));
    p.line_segment([c + vec2(0.0, 6.0 * k), c + vec2(0.0, 9.5 * k)], stroke);
    p.line_segment([c + vec2(-4.0 * k, 9.5 * k), c + vec2(4.0 * k, 9.5 * k)], stroke);
}
