//! What the layer draws over the slide: topic markers at the top right of shapes (and of the slide
//! for the slide's own topics), small tags for client data and linked cells at the top left, the
//! outline of the open topic's places, and the hover card of a linked table or picture.

use std::collections::HashMap;

use deckcraft_agent::anchor::Index;
use deckcraft_agent::layer;
use deckcraft_agent::{links, textlinks, topics};
use deckcraft_engine::Target;
use deckcraft_engine::model::{Shape, ShapeId};
use deckcraft_geom::{Point, Xfrm};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::ext::SlideGeometry;
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::{AgentUi, Palette, Tab, ago, marker, markers, open_topic, palette, state_color, state_label, status_color};

/// Marker size on screen (points).
const MARKER: f32 = 16.0;

/// The box of a shape on the edited slide (a shape inside a group: the group's box).
fn shape_box(app: &SlideApp, id: ShapeId) -> Option<Xfrm> {
    let d = app.session.active()?;
    let top: &Shape = d.shapes().iter().find(|s| s.id == id || deckcraft_engine::model::find_shape(s.children(), id).is_some())?;
    Some(deckcraft_engine::cmd::xfrm_of(&d.doc, &d.selection, top))
}

fn screen(geo: &SlideGeometry, x: &Xfrm) -> Rect {
    Rect::from_two_pos(geo.to_screen(Point::new(x.x, x.y)), geo.to_screen(Point::new(x.x + x.w, x.y + x.h)))
}

/// A small tag at a shape's top left: client data, linked cells.
struct Tag {
    color: Color32,
    text: String,
    tip: String,
}

fn tag(ui: &mut Ui, painter: &egui::Painter, at: Pos2, b: &Tag, id: egui::Id) -> (Rect, bool) {
    let galley = painter.layout_no_wrap(b.text.clone(), theme::bold(9.5), Color32::WHITE);
    let size = vec2(galley.size().x + 10.0, 15.0);
    let r = Rect::from_min_size(pos2(at.x, at.y - size.y / 2.0), size);
    painter.rect_filled(r, CornerRadius::same(4), b.color);
    painter.galley(pos2(r.min.x + 5.0, r.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    let resp = ui.interact(r, id, Sense::click()).on_hover_text(&b.tip);
    (r, resp.clicked())
}

#[derive(Default)]
struct OnShape {
    tags: Vec<Tag>,
    link: Option<links::Link>,
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
    let time = ui.input(|i| i.time);
    let mut on: HashMap<ShapeId, OnShape> = HashMap::new();
    // Client data.
    let private = deckcraft_agent::privacy::private_shapes(&app.session);
    for (s, id) in &private {
        if *s == slide && d.shapes().iter().any(|x| x.id == *id) {
            on.entry(*id).or_default().tags.push(Tag {
                color: p.private,
                text: "client".into(),
                tip: "Client data: the agent sees pseudonyms here and can't change it".into(),
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
        e.tags.push(Tag {
            color: state_color(&p, state),
            text,
            tip: format!("Linked to {} in {}: {}", link.source.place(), link.source.file_name(), state_label(state)),
        });
        e.link = Some(link);
    }
    // The open topic's places: a thin outline in its status colour.
    if st.open
        && let Some(tp) = st.topic.as_deref().and_then(|id| l.topic(id))
    {
        let ix = Index::of(&doc);
        for pl in topics::live(tp) {
            for (_, w) in topics::resolve(&doc, &ix, &l, pl) {
                if w.slide != slide {
                    continue;
                }
                if let Some(r) = w.shape.and_then(|id| shape_box(app, id)).map(|x| screen(geo, &x)) {
                    let width = if st.highlight_place.as_deref() == Some(pl.id.as_str()) { 2.5 } else { 1.5 };
                    painter.rect_stroke(
                        r.expand(4.0),
                        CornerRadius::same(3),
                        Stroke::new(width, status_color(&p, tp.status)),
                        egui::StrokeKind::Outside,
                    );
                }
            }
        }
    }
    let mut hover_link: Option<(links::Link, Rect)> = None;
    let mut clicked_tag: Option<ShapeId> = None;
    let mut ids: Vec<ShapeId> = on.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Some(x) = shape_box(app, id) else { continue };
        let r = screen(geo, &x);
        let Some(o) = on.get(&id) else { continue };
        let mut at = pos2(r.left(), r.top() - 10.0);
        for (k, b) in o.tags.iter().enumerate() {
            let (pr, c) = tag(ui, painter, at, b, egui::Id::new(("agent_tag", id.0, k)));
            if c {
                clicked_tag = Some(id);
            }
            at.x = pr.right() + 3.0;
        }
        if let Some(link) = &o.link {
            let state = links::last_check(&link.id).map_or(links::State::Unchecked, |c| c.state);
            if matches!(state, links::State::Stale | links::State::Missing | links::State::Error) {
                painter.rect_stroke(r.expand(3.0), CornerRadius::same(2), Stroke::new(2.0, state_color(&p, state)), egui::StrokeKind::Outside);
            }
            if geo.pointer.is_some_and(|pt| r.contains(pt)) {
                hover_link = Some((link.clone(), r));
            }
        }
    }
    // Topic markers: at the shape's top-right corner, the slide's own at the slide's.
    let nums = topics::numbers_of(d, &l);
    let mut clicked: Option<(String, String)> = None;
    let mut pulsing = false;
    for (shape, list) in markers(&doc, &l, slide, &p, time) {
        let corner = match shape {
            Some(id) => match shape_box(app, id) {
                Some(x) => {
                    let r = screen(geo, &x);
                    pos2(r.right() + MARKER / 2.0, r.top())
                }
                None => continue,
            },
            None => pos2(geo.slide.right() - 6.0, geo.slide.top() + MARKER / 2.0 + 6.0),
        };
        let mut right = corner;
        for (k, (tid, pid, label, color, unread)) in list.iter().enumerate() {
            let Some(topic) = l.topic(tid) else { continue };
            pulsing |= topics::working(&l, topic);
            let r = marker(painter, right, label, *color, *unread, MARKER);
            let tip = marker_tip(d, &l, &nums, topic);
            let resp = ui.interact(r, egui::Id::new(("agent_marker", tid.as_str(), pid.as_str(), k)), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Topic marker {label}")));
            let resp = resp.on_hover_text(tip);
            if resp.clicked() {
                clicked = Some((tid.clone(), pid.clone()));
            }
            right.x = r.left() - 3.0;
        }
    }
    if pulsing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
    if let Some((tid, pid)) = clicked {
        open_topic(st, app, &tid, Some(&pid));
    } else if let Some(id) = clicked_tag {
        st.open = true;
        st.tab = Tab::Selection;
        let _ = app.run("selection.select", serde_json::json!({"ids": [id.0]}));
    }
    if let Some((link, r)) = hover_link
        && !egui::Popup::is_any_open(ui.ctx())
    {
        hover_card(ui, &t, &p, &link, r, geo);
    }
}

/// "Topic 3 · Revise: the totals are wrong" and the first question.
fn marker_tip(d: &deckcraft_engine::DocState, l: &layer::Layer, nums: &topics::Numbers, t: &layer::Topic) -> String {
    let mut s = format!("Topic {} · {}", nums.topic(&t.id), t.status.label());
    if !t.status_text.is_empty() {
        s.push_str(": ");
        s.push_str(&textlinks::plain(d, l, nums, &t.status_text));
    }
    if let Some(q) = l.questions.iter().find(|q| q.topic == t.id && !q.hidden && !q.is_ping()) {
        s.push('\n');
        s.push_str(&textlinks::plain(d, l, nums, &q.text));
    }
    if topics::working(l, t) {
        s.push_str("\nThe agent is working on it");
    } else if topics::unread(l, t) > 0 {
        s.push_str("\nAn answer is waiting");
    }
    s
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
