//! The Agent pane (TOPICS.md §3): three tabs.
//!
//! - **Selection**: what is selected, then the topics touching it as cards (number and status with
//!   its text, places with the agent's reasons, notes that open, the discussion folded once a
//!   status is set), and the question box at the very bottom with "New Topic" beside it.
//! - **Overview**: readiness for sign-off on top, filters, every topic in deck order; the deck's
//!   linked cells; a copy to send, without the discussion unless the person includes it.
//! - **History**: the log of decisions and changes.

use deckcraft_agent::anchor::{Index, Place};
use deckcraft_agent::layer::{self, Actor, Layer, Note, QuestionStatus, Status, Topic};
use deckcraft_agent::topics::{self, Numbers};
use deckcraft_agent::{anchor, links, textlinks};
use deckcraft_engine::DocState;
use deckcraft_engine::model::ShapeId;
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Color32, RichText, Sense, Ui, pos2, vec2};
use serde_json::{Value as Json, json};

use crate::{
    AgentUi, Filter, Palette, Tab, ago, follow, go_to, marker, marker_color, open_topic, palette, place_of, selected_link, selection_private,
    state_color, state_label, status_color,
};

/// Something the user clicked, acted on once drawing is done (drawing borrows the app).
enum Act {
    Follow(String),
    Go(Place),
    Run(&'static str, Json),
    /// A question or a new topic: the topic it lands in is opened.
    Ask(&'static str, Json),
    Open(String),
    Send,
    /// Retry (true) or Dismiss a spoken question.
    Voice(bool, String),
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let mut acts: Vec<Act> = vec![];
    let mut close = false;
    // A topic opened from a marker or a link stays first while the selection is what it was.
    if let Some(d) = app.session.active() {
        let now = (d.selection.slide, d.selection.shapes.clone());
        if st.topic_for.as_ref().is_some_and(|f| *f != now) {
            st.topic = None;
            st.topic_for = None;
            st.highlight_place = None;
        }
    }
    egui::Panel::right("agent_pane")
        .resizable(true)
        .default_size(340.0)
        .size_range(280.0..=560.0)
        .frame(egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin::same(10)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Agent").font(theme::bold(16.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        close = true;
                    }
                });
            });
            let Some(d) = app.session.active() else { return };
            let l = layer::get(d);
            let unread: usize = topics::visible(&l).map(|x| topics::unread(&l, x)).sum();
            ui.horizontal_wrapped(|ui| {
                let overview = if unread > 0 { format!("Overview ({unread} new)") } else { "Overview".to_string() };
                for (tab, name) in [(Tab::Selection, "Selection".to_string()), (Tab::Overview, overview), (Tab::History, "History".into())] {
                    if ui.selectable_label(st.tab == tab, name).clicked() {
                        st.tab = tab;
                    }
                }
            });
            ui.separator();
            match st.tab {
                Tab::Selection => {
                    egui::Panel::bottom("agent_ask")
                        .resizable(false)
                        .frame(egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin { left: 0, right: 0, top: 8, bottom: 0 }))
                        .show(ui, |ui| ask_box(st, app, ui, &l, &t, &mut acts));
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| selection_tab(st, app, ui, &l, &t, &p, &mut acts));
                }
                Tab::Overview => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| overview_tab(st, app, ui, &l, &t, &p, &mut acts));
                }
                Tab::History => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| history_tab(app, ui, &l, &t, &p, &mut acts));
                }
            }
        });
    if close {
        st.open = false;
    }
    for a in acts {
        match a {
            Act::Follow(href) => follow(st, app, &href),
            Act::Go(place) => go_to(app, &place),
            Act::Run(id, params) => {
                let _ = app.run(id, params);
            }
            Act::Ask(id, params) => {
                if let Ok(r) = app.run(id, params)
                    && let Some(tid) = r.get("topicId").and_then(Json::as_str)
                {
                    st.topic = Some(tid.to_string());
                    st.topic_for = app.session.active().map(|d| (d.selection.slide, d.selection.shapes.clone()));
                }
            }
            Act::Open(tid) => open_topic(st, app, &tid, None),
            Act::Send => send_copy(st, app),
            #[cfg(not(target_arch = "wasm32"))]
            Act::Voice(retry, id) => {
                if retry {
                    let ctx = ui.ctx().clone();
                    st.voice.retry(app, &ctx, &id);
                } else {
                    st.voice.dismiss(app, &id);
                }
            }
            #[cfg(target_arch = "wasm32")]
            Act::Voice(..) => {}
        }
    }
}

/// Saves a copy of the deck to send: statuses, places and notes go with it, the discussion only
/// when the person ticked it.
fn send_copy(st: &AgentUi, app: &mut SlideApp) {
    let Some(d) = app.session.active() else { return };
    let stem = d
        .path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Presentation".into());
    let suggested = format!("{stem} (to send).pptx");
    let Some(pick) = app.services.pick_save.as_mut() else {
        app.set_status("Saving a copy to send needs the desktop app");
        return;
    };
    let Some(path) = pick(&suggested) else { return };
    match app.run("agent.copyToSend", json!({"path": path, "discussion": st.send_discussion})) {
        Ok(_) => app.set_status(format!("Saved a copy to send{}: {path}", if st.send_discussion { ", with the discussion" } else { "" })),
        Err(e) => app.set_status(format!("Couldn't save the copy: {e}")),
    }
}

// ---------------------------------------------------------------- pieces

/// Text with links inside it, as it reads now: words, and links that go where they say.
fn rich(ui: &mut Ui, d: &DocState, l: &Layer, nums: &Numbers, text: &str, color: Color32, small: bool, acts: &mut Vec<Act>) {
    for line in text.split('\n') {
        if line.trim().is_empty() {
            ui.add_space(3.0);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for s in textlinks::spans(d, l, nums, line) {
                let mut rt = RichText::new(&s.text);
                if small {
                    rt = rt.small();
                }
                match s.href {
                    Some(h) => {
                        let rt = rt.color(link_color(ui));
                        let tip = l
                            .notes
                            .iter()
                            .find(|n| !n.url.is_empty() && n.url == h)
                            .map(|n| textlinks::plain(d, l, nums, &n.summary))
                            .filter(|x| !x.is_empty())
                            .unwrap_or_else(|| h.clone());
                        if ui.link(rt).on_hover_text(tip).clicked() {
                            acts.push(Act::Follow(h));
                        }
                    }
                    None => {
                        ui.label(rt.color(color));
                    }
                }
            }
        });
    }
}

fn card(ui: &mut Ui, t: &Tokens, stripe: Color32, add: impl FnOnce(&mut Ui)) -> egui::Rect {
    let r = egui::Frame::NONE
        .fill(t.chrome)
        .corner_radius(6.0)
        .inner_margin(egui::Margin { left: 12, right: 8, top: 8, bottom: 8 })
        .stroke(egui::Stroke::new(1.0, t.card_border))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let top = ui.cursor().min;
            add(ui);
            let bottom = ui.min_rect().bottom();
            ui.painter().line_segment([pos2(top.x - 7.0, top.y), pos2(top.x - 7.0, bottom)], egui::Stroke::new(3.0, stripe));
        })
        .response
        .rect;
    ui.add_space(6.0);
    r
}

fn heading(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).font(theme::bold(13.0)).color(t.text));
    ui.add_space(2.0);
}

fn sub(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).small().strong().color(t.text_dim));
}

/// Links inside text: blue, apart from the text and from the status colours.
fn link_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_rgb(0x8A, 0xB4, 0xF8) } else { Color32::from_rgb(0x1F, 0x5F, 0xBF) }
}

/// A marker drawn inline (in a card's head, before a place).
fn inline_marker(ui: &mut Ui, label: &str, color: Color32, unread: bool, size: f32) {
    let w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), theme::bold(size * 0.62), Color32::WHITE).size().x);
    let (rect, _) = ui.allocate_exact_size(vec2((w + size * 0.5).max(size) + 4.0, size + 2.0), Sense::hover());
    marker(ui.painter(), pos2(rect.right() - 2.0, rect.center().y), label, color, unread, size);
}

fn note_kind(kind: &str) -> &'static str {
    match kind {
        "faq" => "Prepared answer",
        "assumption" => "Assumption",
        "source" => "Source",
        "check" => "Check",
        "decision" => "Decision",
        _ => "Explanation",
    }
}

fn by(a: Actor) -> &'static str {
    match a {
        Actor::Agent => "by the agent",
        Actor::App => "by DeckCraft",
        _ => "by you",
    }
}

/// A question's text with what was pointed at while speaking (`⟦n⟧`) as links: `(1)`.
fn with_refs(q: &layer::Question) -> String {
    let mut t = q.text.clone();
    for r in &q.refs {
        t = t.replace(&format!("⟦{}⟧", r.n), &format!("[({})](anchor:{})", r.n, r.anchor));
    }
    t
}

// ---------------------------------------------------------------- a topic

#[allow(clippy::too_many_arguments)]
fn topic_card(st: &mut AgentUi, ui: &mut Ui, d: &DocState, l: &Layer, nums: &Numbers, tp: &Topic, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let ix = Index::of(&d.doc);
    let color = status_color(p, tp.status);
    let time = ui.input(|i| i.time);
    let working = topics::working(l, tp);
    card(ui, t, color, |ui| {
        // Number and status.
        ui.horizontal_wrapped(|ui| {
            inline_marker(ui, &nums.topic(&tp.id).to_string(), marker_color(p, l, tp, time), topics::unread(l, tp) > 0, 18.0);
            ui.label(RichText::new(tp.status.label()).strong().color(color));
            let when = ago(tp.status_at);
            let who = if tp.status_by == Actor::Human { "" } else { by(tp.status_by) };
            let line = [who, when.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            if !line.is_empty() {
                ui.label(RichText::new(line).small().color(t.text_faint));
            }
            if working {
                ui.label(RichText::new("the agent is working on it").small().color(p.working));
            }
        });
        if !tp.status_text.is_empty() {
            rich(ui, d, l, nums, &tp.status_text, t.text, false, acts);
        }
        // Setting the status: the person's, with an optional why.
        ui.horizontal_wrapped(|ui| {
            for s in Status::ALL {
                let on = tp.status == s;
                let text = if on {
                    RichText::new(s.label()).small().strong().color(Color32::WHITE)
                } else {
                    RichText::new(s.label()).small().color(t.text_dim)
                };
                let b = if on { egui::Button::new(text).fill(status_color(p, s)).corner_radius(9.0) } else { egui::Button::new(text).frame(false) };
                if ui.add(b).clicked() && !on {
                    st.status_edit = Some((tp.id.clone(), s, String::new()));
                }
            }
        });
        let mut done = false;
        if let Some((id, s, why)) = st.status_edit.as_mut()
            && *id == tp.id
        {
            ui.add(egui::TextEdit::singleline(why).desired_width(f32::INFINITY).hint_text("Why (optional; slide 4 becomes a link)"));
            ui.horizontal(|ui| {
                if ui.button(format!("Set {}", s.label())).clicked() {
                    acts.push(Act::Run("agent.setStatus", json!({"topic": tp.id, "status": s.id(), "text": why.trim()})));
                    done = true;
                }
                if ui.small_button("Cancel").clicked() {
                    done = true;
                }
            });
        }
        if done {
            st.status_edit = None;
        }
        // The agent's proposal: Agreed, one click away.
        if !tp.proposal.is_empty() && tp.status != Status::Agreed {
            egui::Frame::NONE.fill(p.agreed.gamma_multiply(0.12)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("The agent suggests: Agreed").small().strong().color(p.agreed));
                rich(ui, d, l, nums, &tp.proposal, t.text, true, acts);
                if ui.button("Confirm Agreed").clicked() {
                    acts.push(Act::Run("agent.confirmAgreed", json!({"topic": tp.id})));
                }
            });
        }
        // Places.
        let mut places: Vec<&layer::Place> = topics::live(tp).collect();
        places.sort_by_key(|pl| nums.place(&pl.id));
        if places.len() > 1 || places.iter().any(|pl| !pl.reason.is_empty() || topics::resolve(&d.doc, &ix, l, pl).is_empty()) {
            sub(ui, t, "Places");
        }
        for pl in &places {
            let found = topics::resolve(&d.doc, &ix, l, pl);
            let label = topics::place_label(&d.doc, &ix, l, pl);
            let lit = st.highlight_place.as_deref() == Some(pl.id.as_str()) && st.topic.as_deref() == Some(tp.id.as_str());
            ui.horizontal_wrapped(|ui| {
                inline_marker(ui, &nums.place(&pl.id), if found.is_empty() { p.info } else { color }, false, 14.0);
                if found.is_empty() {
                    ui.label(RichText::new(label).small().strikethrough().color(t.text_dim))
                        .on_hover_text("The content was deleted: undo the delete to bring it back");
                } else {
                    let mut rt = RichText::new(label).small().color(link_color(ui));
                    if lit {
                        rt = rt.background_color(t.selected);
                    }
                    if ui.link(rt).clicked()
                        && let Some((_, w)) = found.first()
                    {
                        acts.push(Act::Go(w.clone()));
                    }
                }
                if pl.by == Actor::Agent {
                    ui.label(RichText::new("added by the agent").small().color(t.text_faint));
                    if ui.small_button("Reject").on_hover_text("Not a place of this topic (undoable)").clicked() {
                        acts.push(Act::Run("agent.rejectPlace", json!({"place": pl.id})));
                    }
                }
            });
            if !pl.reason.is_empty() {
                ui.indent(("reason", pl.id.as_str()), |ui| rich(ui, d, l, nums, &pl.reason, t.text_dim, true, acts));
            }
        }
        // Notes: titles that open.
        let notes: Vec<&Note> = l.notes.iter().filter(|n| n.topic == tp.id && !n.deleted).collect();
        if !notes.is_empty() {
            sub(ui, t, "Notes");
        }
        for n in notes {
            note_row(st, ui, d, l, nums, n, t, p, acts);
        }
        // The discussion: folded once a status is set.
        let qs: Vec<&layer::Question> = l.questions.iter().filter(|q| q.topic == tp.id && !q.hidden && !q.is_ping()).collect();
        if !qs.is_empty() {
            let default_open = tp.status == Status::Open || working || topics::unread(l, tp) > 0;
            let open = st.discussion.get(&tp.id).copied().unwrap_or(default_open);
            let head = egui::CollapsingHeader::new(RichText::new(format!("Discussion ({})", qs.len())).small().strong().color(t.text_dim))
                .id_salt(("discussion", tp.id.as_str()))
                .open(Some(open))
                .show(ui, |ui| {
                    for q in &qs {
                        ui.add_space(2.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(if q.spoken { "You said" } else { "You asked" }).small().strong().color(p.person));
                            ui.label(RichText::new(ago(q.asked_at)).small().color(t.text_faint));
                            let (s, c) = match q.status {
                                QuestionStatus::Pending => ("waiting for the agent", p.pending),
                                QuestionStatus::Working => ("the agent is working on it", p.working),
                                QuestionStatus::Withdrawn => ("withdrawn", t.text_faint),
                                QuestionStatus::Answered => ("", t.text_faint),
                                QuestionStatus::Transcribing => ("transcribing what you said…", p.working),
                                QuestionStatus::Failed => ("not transcribed", p.broken),
                            };
                            if !s.is_empty() {
                                ui.label(RichText::new(s).small().color(c));
                            }
                        });
                        rich(ui, d, l, nums, &with_refs(q), t.text, false, acts);
                        if q.status == QuestionStatus::Failed {
                            if !q.error.is_empty() {
                                ui.label(RichText::new(&q.error).small().color(t.text_dim));
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("Retry").on_hover_text("Transcribe the recording again").clicked() {
                                    acts.push(Act::Voice(true, q.id.clone()));
                                }
                                if ui.small_button("Dismiss").on_hover_text("Drop this question and its recording").clicked() {
                                    acts.push(Act::Voice(false, q.id.clone()));
                                }
                            });
                        }
                        if let Some(a) = &q.answer {
                            ui.add_space(2.0);
                            ui.label(RichText::new("Agent").small().strong().color(p.agent));
                            rich(ui, d, l, nums, &a.text, t.text, false, acts);
                            if !q.seen && ui.small_button("Mark as Read").clicked() {
                                acts.push(Act::Run("agent.seen", json!({"id": q.id})));
                            }
                        }
                    }
                });
            if head.header_response.clicked() {
                st.discussion.insert(tp.id.clone(), !open);
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn note_row(st: &mut AgentUi, ui: &mut Ui, d: &DocState, l: &Layer, nums: &Numbers, n: &Note, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let open = st.open_notes.contains(&n.id);
    let lit = st.highlight_note.as_deref() == Some(n.id.as_str());
    let title =
        if n.title.is_empty() { textlinks::plain(d, l, nums, &n.text).chars().take(60).collect() } else { textlinks::plain(d, l, nums, &n.title) };
    let color = if n.kind == "faq" { p.answered } else { p.note };
    let mut head = RichText::new(format!("{}{}: {title}", if n.pinned { "Pinned · " } else { "" }, note_kind(&n.kind))).small().color(t.text);
    if lit {
        head = head.background_color(t.selected);
    }
    let r = egui::CollapsingHeader::new(head).id_salt(("note", n.id.as_str())).open(Some(open)).show(ui, |ui| {
        ui.label(RichText::new(by(n.author)).small().color(color));
        rich(ui, d, l, nums, &n.text, t.text, false, acts);
        if !n.url.is_empty() {
            let tip = textlinks::plain(d, l, nums, &n.summary);
            if ui.link(RichText::new(&n.url).small().color(link_color(ui))).on_hover_text(if tip.is_empty() { n.url.clone() } else { tip }).clicked()
            {
                acts.push(Act::Follow(n.url.clone()));
            }
        }
        ui.horizontal(|ui| {
            if ui.small_button(if n.pinned { "Unpin" } else { "Pin" }).on_hover_text("Pinned notes stay on top").clicked() {
                acts.push(Act::Run("agent.pinNote", json!({"id": n.id, "on": !n.pinned})));
            }
            if ui.small_button("Remove").clicked() {
                acts.push(Act::Run("agent.removeNote", json!({"id": n.id})));
            }
        });
    });
    if r.header_response.clicked() {
        if open {
            st.open_notes.remove(&n.id);
        } else {
            st.open_notes.insert(n.id.clone());
        }
    }
}

fn link_card(ui: &mut Ui, slide: usize, id: ShapeId, link: &links::Link, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, show_slide: bool) {
    let check = links::last_check(&link.id);
    let state = check.as_ref().map_or(links::State::Unchecked, |c| c.state);
    card(ui, t, state_color(p, state), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(state_label(state)).small().strong().color(state_color(p, state)));
            if show_slide && ui.link(RichText::new(format!("Slide {}", slide + 1)).small()).clicked() {
                acts.push(Act::Go(Place { slide, shape: Some(id), ..Default::default() }));
            }
            ui.label(RichText::new(if link.mode == links::Mode::Picture { "linked picture" } else { "linked table" }).small().color(t.text_faint));
        });
        if !link.source.label.is_empty() {
            ui.label(RichText::new(&link.source.label).strong().color(t.text));
        }
        let place = check
            .as_ref()
            .filter(|c| !c.range.is_empty())
            .map(|c| format!("{}!{}", links::quote_sheet(&c.sheet), c.range))
            .unwrap_or_else(|| link.source.place());
        ui.label(RichText::new(format!("{place} in {}", link.source.file_name())).color(t.text_dim));
        if let Some(c) = &check
            && state != links::State::Current
            && !c.message.is_empty()
        {
            ui.label(RichText::new(&c.message).small().color(t.text));
        }
        ui.horizontal(|ui| {
            if state == links::State::Stale && ui.button("Update from Source").clicked() {
                acts.push(Act::Go(Place { slide, shape: Some(id), ..Default::default() }));
                acts.push(Act::Run("link.update", json!({"id": id.0})));
            }
            if ui.button("Open Source").clicked() {
                acts.push(Act::Run("link.open", json!({"id": id.0})));
            }
        });
    });
}

// ---------------------------------------------------------------- Selection

/// Where the selection is: the shape selected or edited, or the slide.
fn selection_place(d: &DocState) -> Place {
    let sel = &d.selection;
    let shape = sel.text.as_ref().filter(|x| !x.notes).map(|x| x.shape).or_else(|| sel.shapes.first().copied());
    Place { slide: sel.slide, shape, ..Default::default() }
}

/// The topics the Selection tab shows: the one opened first, then those touching the selected
/// shape (or every topic with a place on the slide when no shape is selected), in number order.
fn selection_topics(st: &AgentUi, d: &DocState, l: &Layer, nums: &Numbers) -> Vec<String> {
    let ix = Index::of(&d.doc);
    let at = selection_place(d);
    let mut ids: Vec<String> = if at.shape.is_some() {
        topics::touching(&d.doc, &ix, l, &at)
    } else {
        let mut v: Vec<String> = topics::places_on(&d.doc, &ix, l, at.slide).into_iter().map(|(t, _, _)| t).collect();
        v.dedup();
        v
    };
    ids.sort_by_key(|t| nums.topic(t));
    ids.dedup();
    if let Some(t) = st.topic.as_ref().filter(|t| l.topic(t).is_some_and(|x| !x.hidden)) {
        ids.retain(|x| x != t);
        ids.insert(0, t.clone());
    }
    ids
}

fn selection_tab(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let nums = topics::numbers_of(d, l);
    // What is selected.
    ui.label(RichText::new(selection_label(app)).strong().color(t.text));
    if let Some((_, _, true)) = selection_private(app) {
        ui.label(RichText::new("Client data: the agent reads pseudonyms like [client-1] here.").small().color(p.private));
    }
    if let Some((id, link)) = selected_link(app) {
        ui.add_space(4.0);
        link_card(ui, d.selection.slide, id, &link, t, p, acts, false);
    }
    let ids = selection_topics(st, d, l, &nums);
    if ids.is_empty() {
        ui.add_space(6.0);
        ui.label(
            RichText::new("No topics here yet. Ask about the selection below: the question starts a topic, numbered on the slide.")
                .small()
                .color(t.text_faint),
        );
        return;
    }
    ui.add_space(6.0);
    for id in ids {
        if let Some(tp) = l.topic(&id) {
            topic_card(st, ui, d, l, &nums, tp, t, p, acts);
        }
    }
}

/// The question box, at the very bottom of the Selection tab.
fn ask_box(st: &mut AgentUi, app: &SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let nums = topics::numbers_of(d, l);
    // The topic a question joins: the one opened, or the first touching the selection itself.
    let joins = st.topic.clone().filter(|t| l.topic(t).is_some_and(|x| !x.hidden)).or_else(|| {
        let mut v = topics::touching(&d.doc, &Index::of(&d.doc), l, &selection_place(d));
        v.sort_by_key(|t| nums.topic(t));
        v.into_iter().next()
    });
    let target = selection_label(app);
    let line = match &joins {
        Some(tid) => format!("Ask about {target} (adds to topic {})", nums.topic(tid)),
        None => format!("Ask about {target}"),
    };
    ui.label(RichText::new(line).small().color(t.text_dim));
    let edit =
        ui.add(egui::TextEdit::multiline(&mut st.draft).desired_rows(2).desired_width(f32::INFINITY).hint_text("Where does this number come from?"));
    if st.focus_draft {
        edit.request_focus();
        st.focus_draft = false;
    }
    let send = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.horizontal(|ui| {
        let text = st.draft.trim().to_string();
        if (ui.button("Ask the Agent").clicked() || send) && !text.is_empty() {
            let mut q = json!({"text": text});
            if let Some(tid) = &joins {
                q["topic"] = json!(tid);
            }
            acts.push(Act::Ask("agent.ask", q));
            st.draft.clear();
        }
        if ui.button("New Topic").on_hover_text("A separate topic on the selection (with the question, if you wrote one)").clicked() {
            if text.is_empty() {
                acts.push(Act::Ask("agent.newTopic", json!({})));
            } else {
                acts.push(Act::Ask("agent.ask", json!({"text": text, "new": true})));
                st.draft.clear();
            }
        }
    });
}

/// "the selected shape (Title 1)", "“42%” in TextBox 3", "slide 3".
fn selection_label(app: &SlideApp) -> String {
    let Some(d) = app.session.active() else { return "this slide".into() };
    let sel = &d.selection;
    if let Some(tsel) = sel.text.as_ref().filter(|x| !x.notes)
        && let Some(sh) = d.shape(tsel.shape)
    {
        if tsel.is_range()
            && let Some(body) = &sh.text
        {
            let (a, b) = tsel.ordered();
            let words: String = body
                .paragraphs
                .get(a.0)
                .map(|p| p.text().chars().skip(a.1).take(if a.0 == b.0 { b.1.saturating_sub(a.1) } else { 60 }).collect())
                .unwrap_or_default();
            if !words.trim().is_empty() {
                return format!("“{}” in {}", words.trim(), anchor::shape_label(sh));
            }
        }
        return anchor::shape_label(sh);
    }
    if let Some(sh) = sel.shapes.first().and_then(|id| d.shape(*id)) {
        return anchor::shape_label(sh);
    }
    format!("slide {}", sel.slide + 1)
}

// ---------------------------------------------------------------- Overview

fn passes(f: &std::collections::HashSet<Filter>, l: &Layer, tp: &Topic) -> bool {
    if f.is_empty() {
        return true;
    }
    f.iter().any(|x| match x {
        Filter::Status(s) => tp.status == *s,
        Filter::Unread => topics::unread(l, tp) > 0,
        Filter::WithAgent => {
            topics::working(l, tp) || l.questions.iter().any(|q| q.topic == tp.id && !q.hidden && q.status == QuestionStatus::Pending)
        }
    })
}

fn overview_tab(st: &mut AgentUi, app: &SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let ix = Index::of(&d.doc);
    let nums = topics::numbers_of(d, l);
    // Readiness for sign-off.
    let (all, agreed, revise, open, info) = topics::readiness(l);
    ui.label(RichText::new(topics::readiness_text(l)).font(theme::bold(14.0)).color(t.text));
    if all > 0 {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
        let mut x = rect.left();
        for (n, c) in [(agreed, p.agreed), (revise, p.revise), (open, p.open), (info, p.info)] {
            let w = rect.width() * n as f32 / all as f32;
            if w > 0.0 {
                ui.painter().rect_filled(egui::Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())), 0.0, c);
            }
            x += w;
        }
    }
    // Filters.
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        let mut chips: Vec<(Filter, String)> = Status::ALL.iter().map(|s| (Filter::Status(*s), s.label().to_string())).collect();
        chips.push((Filter::Unread, "Unread".into()));
        chips.push((Filter::WithAgent, "With the agent".into()));
        for (f, name) in chips {
            let on = st.filters.contains(&f);
            if ui.selectable_label(on, RichText::new(name).small()).clicked() {
                if on {
                    st.filters.remove(&f);
                } else {
                    st.filters.insert(f);
                }
            }
        }
    });
    ui.add_space(4.0);
    if all == 0 {
        ui.label(
            RichText::new("No topics yet. Select a shape, words or a number and ask the agent about it: the question starts a topic.")
                .small()
                .color(t.text_faint),
        );
    }
    let time = ui.input(|i| i.time);
    for tid in &nums.order {
        let Some(tp) = l.topic(tid) else { continue };
        if !passes(&st.filters, l, tp) {
            continue;
        }
        let color = status_color(p, tp.status);
        let places: Vec<&layer::Place> = topics::live(tp).collect();
        let first = places.iter().min_by_key(|pl| nums.place(&pl.id)).map(|pl| topics::place_label(&d.doc, &ix, l, pl)).unwrap_or_default();
        let notes = l.notes.iter().filter(|n| n.topic == tp.id && !n.deleted).count();
        let unread = topics::unread(l, tp);
        let r = card(ui, t, color, |ui| {
            ui.horizontal_wrapped(|ui| {
                inline_marker(ui, &nums.topic(tid).to_string(), marker_color(p, l, tp, time), unread > 0, 18.0);
                ui.label(RichText::new(tp.status.label()).strong().color(color));
                ui.label(RichText::new(first).small().color(t.text_dim));
            });
            let what = if tp.status_text.is_empty() {
                l.questions.iter().find(|q| q.topic == tp.id && !q.hidden && !q.is_ping()).map(|q| q.text.clone()).unwrap_or_default()
            } else {
                tp.status_text.clone()
            };
            if !what.is_empty() {
                let plain: String = textlinks::plain(d, l, &nums, &what);
                let short: String = plain.chars().take(140).collect();
                ui.label(RichText::new(if plain.chars().count() > 140 { format!("{short}…") } else { short }).color(t.text));
            }
            let mut counts = vec![format!("{} place{}", places.len(), if places.len() == 1 { "" } else { "s" })];
            if notes > 0 {
                counts.push(format!("{notes} note{}", if notes == 1 { "" } else { "s" }));
            }
            if unread > 0 {
                counts.push(format!("{unread} unread"));
            }
            if topics::working(l, tp) {
                counts.push("with the agent".into());
            }
            if !tp.proposal.is_empty() && tp.status != Status::Agreed {
                counts.push("the agent suggests Agreed".into());
            }
            ui.label(RichText::new(counts.join(" · ")).small().color(t.text_faint));
        });
        let resp = ui.interact(r, egui::Id::new(("agent_overview", tid.as_str())), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Open topic {}", nums.topic(tid))));
        if resp.on_hover_text("Open the topic").clicked() {
            acts.push(Act::Open(tid.clone()));
        }
    }
    // Linked cells.
    let all_links = links::all(&d.doc);
    if !all_links.is_empty() {
        let stale = all_links.iter().filter(|(_, _, k)| links::last_check(&k.id).is_some_and(|c| c.state != links::State::Current)).count();
        ui.add_space(6.0);
        let head = if stale > 0 {
            format!("Linked cells ({}, {stale} out of date)", all_links.len())
        } else {
            format!("Linked cells ({})", all_links.len())
        };
        egui::CollapsingHeader::new(RichText::new(head).strong()).id_salt("agent_links").default_open(stale > 0).show(ui, |ui| {
            for (s, id, link) in &all_links {
                link_card(ui, *s, *id, link, t, p, acts, true);
            }
        });
    }
    // A copy to send.
    heading(ui, t, "Send");
    ui.label(
        RichText::new("Statuses, places and notes go with the deck. The discussion stays here unless you include it.").small().color(t.text_faint),
    );
    ui.checkbox(&mut st.send_discussion, "Include the discussion");
    if ui.button("Save a Copy to Send…").clicked() {
        acts.push(Act::Send);
    }
}

// ---------------------------------------------------------------- History

fn history_tab(app: &SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let nums = topics::numbers_of(d, l);
    if l.log.is_empty() {
        ui.label(RichText::new("Decisions and changes, by you and by the agent, are recorded here and saved in the file.").color(t.text_faint));
    }
    for e in l.log.iter().rev().take(300) {
        let color = match e.actor {
            Actor::Agent => p.agent,
            Actor::App => t.text_dim,
            _ => p.person,
        };
        ui.horizontal_wrapped(|ui| {
            let who = match e.actor {
                Actor::Agent => "Agent",
                Actor::App => "DeckCraft",
                _ => "You",
            };
            ui.label(RichText::new(who).small().strong().color(color));
            ui.label(RichText::new(if e.kind == "decision" { "decided" } else { "changed" }).small().color(t.text_faint));
            ui.label(RichText::new(ago(e.at)).small().color(t.text_faint));
            if let Some(pl) = e.anchor.as_deref().and_then(|a| place_of(app, l, a))
                && ui.link(RichText::new(format!("slide {}", pl.slide + 1)).small()).clicked()
            {
                acts.push(Act::Go(pl));
            }
            if l.topic(&e.source).is_some() {
                let n = nums.topic(&e.source);
                if n > 0 && ui.link(RichText::new(format!("topic {n}")).small()).clicked() {
                    acts.push(Act::Open(e.source.clone()));
                }
            }
        });
        rich(ui, d, l, &nums, &e.text, t.text, false, acts);
        if !e.source.is_empty() && l.topic(&e.source).is_none() {
            ui.label(RichText::new(format!("Based on: {}", e.source)).small().color(t.text_dim));
        }
        ui.add_space(4.0);
    }
}
