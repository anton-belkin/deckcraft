//! What the layer draws over the slide: badges at the top right of shapes (questions, notes,
//! client data, linked cells), the slide's prepared answers in its corner, and the hover card of a
//! linked table or picture.

use std::collections::HashMap;

use deckcraft_agent::anchor::{self, Index};
use deckcraft_agent::layer::{self, QuestionStatus};
use deckcraft_agent::links;
use deckcraft_engine::Target;
use deckcraft_engine::model::{Shape, ShapeId};
use deckcraft_geom::{Point, Xfrm};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::ext::SlideGeometry;
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use crate::{AgentUi, Palette, Tab, ago, palette, state_color, state_label};

/// The box of a shape on the edited slide (a shape inside a group: the group's box).
fn shape_box(app: &SlideApp, id: ShapeId) -> Option<Xfrm> {
    let d = app.session.active()?;
    let top: &Shape = d.shapes().iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some())?;
    Some(deckcraft_engine::cmd::xfrm_of(&d.doc, &d.selection, top))
}

fn screen(geo: &SlideGeometry, x: &Xfrm) -> Rect {
    Rect::from_two_pos(geo.to_screen(Point::new(x.x, x.y)), geo.to_screen(Point::new(x.x + x.w, x.y + x.h)))
}

/// One badge: a coloured pill with a short label.
struct Badge {
    color: Color32,
    text: String,
    tip: String,
    tab: Tab,
}

#[derive(Default)]
struct OnShape {
    badges: Vec<Badge>,
    link: Option<links::Link>,
}

fn pill(ui: &mut Ui, painter: &egui::Painter, at: Pos2, b: &Badge, id: egui::Id) -> (Rect, bool) {
    let galley = painter.layout_no_wrap(b.text.clone(), theme::bold(11.0), Color32::WHITE);
    let size = vec2(galley.size().x + 12.0, 18.0);
    let r = Rect::from_min_size(pos2(at.x - size.x, at.y), size);
    painter.rect_filled(r, CornerRadius::same(9), b.color);
    painter.rect_stroke(r, CornerRadius::same(9), Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
    painter.galley(pos2(r.min.x + 6.0, r.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    let resp = ui.interact(r, id, Sense::click()).on_hover_text(&b.tip);
    (r, resp.clicked())
}

pub(crate) fn paint(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui, painter: &egui::Painter, geo: &SlideGeometry) {
    let Some(d) = app.session.active() else { return };
    if d.selection.target != Target::Slides {
        return;
    }
    let slide = d.selection.slide;
    let l = layer::get(d);
    let doc = d.doc.clone();
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let ix = Index::of(&doc);
    let mut on: HashMap<ShapeId, OnShape> = HashMap::new();
    let mut slide_badges: Vec<Badge> = vec![];
    // Questions.
    let mut per_shape: HashMap<Option<ShapeId>, Vec<&layer::Question>> = HashMap::new();
    for q in &l.questions {
        if let Some(pl) = l.anchor(&q.anchor).and_then(|a| anchor::resolve(&doc, &ix, a)).filter(|pl| pl.slide == slide) {
            per_shape.entry(pl.shape).or_default().push(q);
        }
    }
    for (shape, qs) in per_shape {
        let b = question_badge(&p, &qs);
        match shape {
            Some(id) => on.entry(id).or_default().badges.push(b),
            None => slide_badges.push(b),
        }
    }
    // Notes and prepared answers.
    let mut notes: HashMap<Option<ShapeId>, (usize, usize)> = HashMap::new();
    for n in &l.notes {
        if let Some(pl) = l.anchor(&n.anchor).and_then(|a| anchor::resolve(&doc, &ix, a)).filter(|pl| pl.slide == slide) {
            let e = notes.entry(pl.shape).or_default();
            if n.kind == "faq" {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
    }
    let faq_total: usize = notes.values().map(|e| e.0).sum();
    for (shape, (faq, other)) in notes {
        if let Some(id) = shape
            && other > 0
        {
            on.entry(id).or_default().badges.push(Badge {
                color: p.note,
                text: format!("i {other}"),
                tip: format!("{other} agent note{}", if other == 1 { "" } else { "s" }),
                tab: Tab::Slide,
            });
        }
        let _ = faq;
    }
    if faq_total > 0 {
        slide_badges.push(Badge {
            color: p.note,
            text: format!("Q&A {faq_total}"),
            tip: "Prepared answers to likely questions on this slide".into(),
            tab: Tab::Slide,
        });
    }
    // Client data.
    let private = deckcraft_agent::privacy::private_shapes(&app.session);
    for (s, id) in &private {
        if *s == slide && d.shapes().iter().any(|x| x.id == *id) {
            on.entry(*id).or_default().badges.push(Badge {
                color: p.private,
                text: "client".into(),
                tip: "Client data: the agent sees pseudonyms here and can't change it".into(),
                tab: Tab::Slide,
            });
        }
    }
    // Linked cells.
    for (s, id, link) in links::all(&doc) {
        if s != slide {
            continue;
        }
        let state = links::last_check(&link.id).map_or(links::State::Unchecked, |c| c.state);
        let text = match state {
            links::State::Stale => "XLSX changed".to_string(),
            links::State::Missing | links::State::Error => "XLSX missing".to_string(),
            _ => "XLSX".to_string(),
        };
        let e = on.entry(id).or_default();
        e.badges.push(Badge {
            color: state_color(&p, state),
            text,
            tip: format!("Linked to {} in {}: {}", link.source.place(), link.source.file_name(), state_label(state)),
            tab: Tab::Links,
        });
        e.link = Some(link);
    }
    // Draw: shapes first, then the slide's own badges in its top-right corner.
    let mut hover_link: Option<(ShapeId, links::Link, Rect)> = None;
    let mut clicked: Option<(Tab, Option<ShapeId>)> = None;
    let mut ids: Vec<ShapeId> = on.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Some(x) = shape_box(app, id) else { continue };
        let r = screen(geo, &x);
        let Some(o) = on.get(&id) else { continue };
        let mut at = pos2(r.right(), r.top() - 22.0);
        for (k, b) in o.badges.iter().enumerate() {
            let (pr, c) = pill(ui, painter, at, b, egui::Id::new(("agent_badge", id.0, k)));
            if c {
                clicked = Some((b.tab, Some(id)));
            }
            at.x = pr.left() - 4.0;
        }
        if let Some(link) = &o.link {
            let state = links::last_check(&link.id).map_or(links::State::Unchecked, |c| c.state);
            if matches!(state, links::State::Stale | links::State::Missing | links::State::Error) {
                painter.rect_stroke(r.expand(3.0), CornerRadius::same(2), Stroke::new(2.0, state_color(&p, state)), egui::StrokeKind::Outside);
            }
            if geo.pointer.is_some_and(|pt| r.contains(pt)) {
                hover_link = Some((id, link.clone(), r));
            }
        }
    }
    let mut at = pos2(geo.slide.right() - 6.0, geo.slide.top() + 6.0);
    for (k, b) in slide_badges.iter().enumerate() {
        let (pr, c) = pill(ui, painter, at, b, egui::Id::new(("agent_slide_badge", k)));
        if c {
            clicked = Some((b.tab, None));
        }
        at.x = pr.left() - 4.0;
    }
    if let Some((tab, shape)) = clicked {
        st.open = true;
        st.tab = tab;
        if let Some(id) = shape {
            let _ = app.run("selection.select", json!({"ids": [id.0]}));
        }
    }
    if let Some((_, link, r)) = hover_link
        && !egui::Popup::is_any_open(ui.ctx())
    {
        hover_card(ui, &t, &p, &link, r, geo);
    }
}

fn question_badge(p: &Palette, qs: &[&layer::Question]) -> Badge {
    let ready = qs.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
    let working = qs.iter().filter(|q| q.status == QuestionStatus::Working).count();
    let pending = qs.iter().filter(|q| q.status == QuestionStatus::Pending).count();
    let first = qs.first().map(|q| q.text.clone()).unwrap_or_default();
    if ready > 0 {
        Badge {
            color: p.answered,
            text: if ready == 1 { "Answer ready".into() } else { format!("{ready} answers ready") },
            tip: first,
            tab: Tab::Slide,
        }
    } else if working > 0 {
        Badge { color: p.working, text: "Agent working".into(), tip: first, tab: Tab::Slide }
    } else if pending > 0 {
        Badge { color: p.pending, text: if pending == 1 { "? waiting".into() } else { format!("? {pending} waiting") }, tip: first, tab: Tab::Slide }
    } else {
        Badge { color: p.seen, text: format!("? {}", qs.len()), tip: first, tab: Tab::Slide }
    }
}

/// Where a linked table's numbers come from, and what changed there.
fn hover_card(ui: &mut Ui, t: &Tokens, p: &Palette, link: &links::Link, r: Rect, geo: &SlideGeometry) {
    let check = links::last_check(&link.id);
    let state = check.as_ref().map_or(links::State::Unchecked, |c| c.state);
    let pos = pos2((r.right() + 10.0).min(geo.canvas.right() - 330.0), r.top().max(geo.canvas.top() + 8.0));
    egui::Area::new(egui::Id::new("agent_link_card")).fixed_pos(pos).order(egui::Order::Tooltip).interactable(false).show(ui.ctx(), |ui| {
        egui::Frame::NONE
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(8.0)
            .inner_margin(10.0)
            .shadow(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: t.shadow })
            .show(ui, |ui| {
                ui.set_max_width(320.0);
                ui.label(
                    egui::RichText::new(if link.mode == links::Mode::Picture { "Linked picture of cells" } else { "Linked cells" })
                        .font(theme::bold(13.0))
                        .color(t.text),
                );
                if !link.source.label.is_empty() {
                    ui.label(egui::RichText::new(&link.source.label).color(t.text));
                }
                let place = check
                    .as_ref()
                    .filter(|c| !c.range.is_empty())
                    .map(|c| format!("{}!{}", links::quote_sheet(&c.sheet), c.range))
                    .unwrap_or_else(|| link.source.place());
                ui.label(egui::RichText::new(format!("{place} in {}", link.source.file_name())).color(t.text_dim));
                ui.label(egui::RichText::new(&link.source.path).small().color(t.text_faint));
                let copied = ago(link.copied_at);
                let updated = ago(if link.updated_at > 0 { link.updated_at } else { link.pasted_at });
                if !copied.is_empty() || !updated.is_empty() {
                    ui.label(egui::RichText::new(format!("Copied {copied} · values from {updated}")).small().color(t.text_dim));
                }
                ui.add_space(4.0);
                let mut line = state_label(state).to_string();
                if let Some(c) = &check
                    && !c.message.is_empty()
                    && state != links::State::Current
                {
                    line = format!("{line}: {}", c.message);
                }
                if let Some(c) = &check
                    && c.via == "gridcraft"
                {
                    line.push_str(" (asked GridCraft)");
                }
                ui.label(egui::RichText::new(line).strong().color(state_color(p, state)));
                if let Some(c) = &check {
                    for (r, col, old, new) in c.changed.iter().take(5) {
                        let head = link.text.get(*r).and_then(|row| row.first()).cloned().unwrap_or_default();
                        let col_head = link.text.first().and_then(|row| row.get(*col)).cloned().unwrap_or_default();
                        let at = [head, col_head].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ");
                        ui.label(egui::RichText::new(format!("{at}: was {old}, now {new}")).color(t.text));
                    }
                    if c.changed.len() > 5 {
                        ui.label(egui::RichText::new(format!("and {} more", c.changed.len() - 5)).small().color(t.text_dim));
                    }
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Right-click: Update from Source, Open Source").small().color(t.text_faint));
            });
    });
}
