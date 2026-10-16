//! The Agent pane: this slide (ask about the selection, prepared answers, questions and notes,
//! client data, linked cells), all questions, all linked cells, the history, and how to connect an
//! agent.

use deckcraft_agent::anchor::{Index, Place};
use deckcraft_agent::layer::{self, Actor, Layer, Note, Question, QuestionStatus};
use deckcraft_agent::{anchor, links};
use deckcraft_engine::model::ShapeId;
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Color32, RichText, Ui};
use serde_json::{Value as Json, json};

use crate::{
    AgentUi, Palette, Tab, ago, follow, go_to, palette, place_of, prepared_answers, selected_link, selection_private, state_color, state_label,
};

/// Something the user clicked, acted on once drawing is done (drawing borrows the app).
enum Act {
    Follow(String),
    Go(Place),
    Run(&'static str, Json),
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let mut acts: Vec<Act> = vec![];
    let mut close = false;
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
            let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
            let open_q = l.questions.iter().filter(|q| q.status != QuestionStatus::Answered).count();
            let stale =
                links::all(&d.doc).iter().filter(|(_, _, k)| links::last_check(&k.id).is_some_and(|c| c.state != links::State::Current)).count();
            ui.horizontal_wrapped(|ui| {
                let q = match (ready, open_q) {
                    (0, 0) => "Questions".to_string(),
                    (0, n) => format!("Questions ({n})"),
                    (r, _) => format!("Questions ({r} ready)"),
                };
                for (tab, name) in [
                    (Tab::Slide, "This Slide".to_string()),
                    (Tab::Questions, q),
                    (Tab::Links, if stale > 0 { format!("Linked Cells ({stale})") } else { "Linked Cells".into() }),
                    (Tab::History, "History".into()),
                ] {
                    if ui.selectable_label(st.tab == tab, name).clicked() {
                        st.tab = tab;
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match st.tab {
                Tab::Slide => slide_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::Questions => questions_tab(app, ui, &l, &t, &p, &mut acts),
                Tab::Links => links_tab(app, ui, &t, &p, &mut acts),
                Tab::History => history_tab(app, ui, &l, &t, &p, &mut acts),
            });
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
        }
    }
}

fn links_row(ui: &mut Ui, list: &[layer::Link], acts: &mut Vec<Act>) {
    if list.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for k in list {
            let r = ui.link(&k.label).on_hover_text(if k.summary.is_empty() { &k.href } else { &k.summary });
            if r.clicked() {
                acts.push(Act::Follow(k.href.clone()));
            }
        }
    });
}

fn card(ui: &mut Ui, t: &Tokens, stripe: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::NONE
        .fill(t.chrome)
        .corner_radius(6.0)
        .inner_margin(egui::Margin { left: 12, right: 8, top: 8, bottom: 8 })
        .stroke(egui::Stroke::new(1.0, t.card_border))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let top = ui.cursor().min;
            add(ui);
            let bottom = ui.min_rect().bottom();
            ui.painter().line_segment([egui::pos2(top.x - 7.0, top.y), egui::pos2(top.x - 7.0, bottom)], egui::Stroke::new(3.0, stripe));
        });
    ui.add_space(6.0);
}

fn heading(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).font(theme::bold(13.0)).color(t.text));
    ui.add_space(2.0);
}

fn question_card(app: &SlideApp, ui: &mut Ui, l: &Layer, q: &Question, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, show_place: bool) {
    let (status, color) = match q.status {
        QuestionStatus::Pending => ("Waiting for the agent", p.pending),
        QuestionStatus::Working => ("The agent is working on it", p.working),
        QuestionStatus::Answered if !q.seen => ("Answer ready", p.answered),
        QuestionStatus::Answered => ("Answered", p.seen),
    };
    card(ui, t, color, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(status).color(color).small().strong());
            if show_place && let Some(pl) = place_of(app, l, &q.anchor) {
                let label = app.session.active().map(|d| pl.label(&d.doc)).unwrap_or_default();
                if ui.link(RichText::new(label).small()).clicked() {
                    acts.push(Act::Go(pl));
                }
            }
            ui.label(RichText::new(ago(q.asked_at)).small().color(t.text_faint));
        });
        ui.label(RichText::new(&q.text).strong().color(t.text));
        if let Some(a) = &q.answer {
            ui.add_space(2.0);
            ui.label(RichText::new(&a.text).color(t.text));
            links_row(ui, &a.links, acts);
            if !q.seen && ui.small_button("Mark as Read").clicked() {
                acts.push(Act::Run("agent.seen", json!({"id": q.id})));
            }
        }
    });
}

fn note_card(ui: &mut Ui, n: &Note, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, highlight: bool) {
    let color = if n.kind == "faq" { p.answered } else { p.note };
    card(ui, t, color, |ui| {
        let kind = match n.kind.as_str() {
            "faq" => "Prepared answer",
            "assumption" => "Assumption",
            "source" => "Source",
            "check" => "Check",
            _ => "Explanation",
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(kind).small().strong().color(color));
            ui.label(RichText::new(if n.author == Actor::Agent { "by the agent" } else { "by you" }).small().color(t.text_faint));
        });
        if !n.title.is_empty() {
            let title = RichText::new(&n.title).strong().color(t.text);
            ui.label(if highlight { title.background_color(t.selected) } else { title });
        }
        ui.label(RichText::new(&n.text).color(t.text));
        links_row(ui, &n.links, acts);
        if ui.small_button("Remove").clicked() {
            acts.push(Act::Run("agent.removeNote", json!({"id": n.id})));
        }
    });
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
            ui.label(RichText::new(if link.mode == links::Mode::Picture { "picture" } else { "table" }).small().color(t.text_faint));
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

fn slide_tab(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let slide = d.selection.slide;
    let doc = d.doc.clone();
    let ix = Index::of(&doc);
    let here = |anchor: &str| l.anchor(anchor).and_then(|a| anchor::resolve(&doc, &ix, a)).filter(|pl| pl.slide == slide);
    // Ask about the selection.
    let target = selection_label(app);
    ui.label(RichText::new(format!("Ask about {target}")).color(t.text_dim));
    let edit =
        ui.add(egui::TextEdit::multiline(&mut st.draft).desired_rows(2).desired_width(f32::INFINITY).hint_text("Where does this number come from?"));
    if st.focus_draft {
        edit.request_focus();
        st.focus_draft = false;
    }
    let send = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.horizontal(|ui| {
        if (ui.button("Ask the Agent").clicked() || send) && !st.draft.trim().is_empty() {
            acts.push(Act::Run("agent.ask", json!({"text": st.draft.trim()})));
            st.draft.clear();
        }
    });
    if let Some((_, true)) = selection_private(app).map(|(_, id, p)| (id, p)) {
        ui.label(RichText::new("Client data: the agent reads pseudonyms like [client-1] here.").small().color(p.private));
    }
    // Prepared answers.
    let faq = prepared_answers(&doc, l, slide);
    heading(ui, t, &format!("Prepared answers ({})", faq.len()));
    if faq.is_empty() {
        ui.label(
            RichText::new(
                "None yet. Your agent prepares answers to the questions the audience is likely to ask; you see them in Presenter View too.",
            )
            .small()
            .color(t.text_faint),
        );
    }
    for n in faq {
        note_card(ui, n, t, p, acts, st.highlight_note.as_deref() == Some(n.id.as_str()));
    }
    // Questions on this slide.
    let qs: Vec<&Question> = l.questions.iter().filter(|q| here(&q.anchor).is_some()).collect();
    if !qs.is_empty() {
        heading(ui, t, "Questions");
        for q in qs.iter().rev() {
            question_card(app, ui, l, q, t, p, acts, true);
        }
    }
    let notes: Vec<&Note> = l.notes.iter().filter(|n| n.kind != "faq" && here(&n.anchor).is_some()).collect();
    if !notes.is_empty() {
        heading(ui, t, "Agent notes");
        for n in notes {
            note_card(ui, n, t, p, acts, st.highlight_note.as_deref() == Some(n.id.as_str()));
        }
    }
    let on_slide: Vec<_> = links::all(&doc).into_iter().filter(|(s, _, _)| *s == slide).collect();
    if !on_slide.is_empty() {
        heading(ui, t, "Linked cells");
        for (s, id, link) in &on_slide {
            link_card(ui, *s, *id, link, t, p, acts, false);
        }
    }
    let _ = selected_link(app);
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

fn questions_tab(app: &SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if l.questions.is_empty() {
        ui.label(
            RichText::new(
                "No questions yet. Select a shape, words or a number and ask the agent about it; the answer comes back here and on the slide.",
            )
            .color(t.text_faint),
        );
    }
    for q in l.questions.iter().rev() {
        question_card(app, ui, l, q, t, p, acts, true);
    }
}

fn links_tab(app: &SlideApp, ui: &mut Ui, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let all = links::all(&d.doc);
    ui.label(RichText::new(format!("{} link{} to spreadsheet cells", all.len(), if all.len() == 1 { "" } else { "s" })).color(t.text_dim));
    if all.is_empty() {
        ui.label(RichText::new("In GridCraft, select cells and use Copy Link to Cells, then paste here: the slide remembers where the numbers come from and tells you when they change.").small().color(t.text_faint));
    }
    for (s, id, link) in &all {
        link_card(ui, *s, *id, link, t, p, acts, true);
    }
}

fn history_tab(app: &SlideApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if l.log.is_empty() {
        ui.label(RichText::new("Decisions and changes, by you and by the agent, are recorded here and saved in the file.").color(t.text_faint));
    }
    for e in l.log.iter().rev().take(300) {
        let color = if e.actor == Actor::Agent { p.agent } else { p.person };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(if e.actor == Actor::Agent { "Agent" } else { "You" }).small().strong().color(color));
            ui.label(RichText::new(if e.kind == "decision" { "decided" } else { "changed" }).small().color(t.text_faint));
            ui.label(RichText::new(ago(e.at)).small().color(t.text_faint));
            if let Some(pl) = e.anchor.as_deref().and_then(|a| place_of(app, l, a))
                && ui.link(RichText::new(format!("slide {}", pl.slide + 1)).small()).clicked()
            {
                acts.push(Act::Go(pl));
            }
        });
        ui.label(RichText::new(&e.text).color(t.text));
        if !e.source.is_empty() {
            ui.label(RichText::new(format!("Based on: {}", e.source)).small().color(t.text_dim));
        }
        ui.add_space(4.0);
    }
}
