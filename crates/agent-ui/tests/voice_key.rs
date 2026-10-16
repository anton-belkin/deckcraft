//! Push to talk in the running UI (egui_kittest): hold ⌥ and a recording starts, click a shape
//! while speaking, release and the question is asked with that shape pointed at; ⌥ with another
//! key is a shortcut, not a question. A stand-in microphone, no transcription: this is about the
//! key and the pointing. Its own test binary, so the UI's globals are its own.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use deckcraft_agent::layer::{self, QuestionStatus};
use deckcraft_engine::Session;
use deckcraft_ui_egui::{Services, SlideApp};
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> (Harness<'static, SlideApp>, u64, u64) {
    deckcraft_agent_ui::install();
    let dir = std::env::temp_dir().join(format!("deckcraft-voice-key-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    deckcraft_agent_ui::voice::set_settings_path(Some(dir.join("voice.json")));
    // Never a real agent, never the real AItoCraft registry.
    deckcraft_agent::registry::set_dir(Some(dir.join("agents")));
    deckcraft_agent::runner::set_config(deckcraft_agent::runner::Config {
        command: "deckcraft-test-no-agent".into(),
        data_dir: Some(dir.join("agent")),
        ..Default::default()
    });
    deckcraft_agent::cowork::set_config_path(Some(dir.join("claude_desktop_config.json")));
    // A second and a half of something above silence.
    deckcraft_agent_ui::voice::use_fake_microphone(Some((0..24_000).map(|i| ((i as f32) * 0.05).sin() * 0.1).collect()));
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let a = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Revenue grew 6%"})).unwrap()["id"].as_u64().unwrap();
    let b = s.execute("insert.textBox", &json!({"rect": [60, 300, 400, 60], "text": "Costs fell 2%"})).unwrap()["id"].as_u64().unwrap();
    s.execute("selection.select", &json!({"ids": [a]})).unwrap();
    s.execute("agent.voiceModel", &json!({"model": "small"})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut SlideApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        SlideApp::new(s, Services::default()),
    );
    h.run_steps(4);
    (h, a, b)
}

/// A click in the middle of a shape on the edited slide.
fn click_shape(h: &mut Harness<'static, SlideApp>, id: u64) {
    let app = h.state();
    let p = app
        .session
        .active()
        .and_then(|d| {
            let sh = d.shape(deckcraft_engine::model::ShapeId(id as u32))?;
            let x = deckcraft_engine::cmd::xfrm_of(&d.doc, &d.selection, sh);
            let r = app.slide_rect?;
            Some(egui::pos2(r.min.x + (x.x + x.w * 0.5) as f32 * app.canvas_scale, r.min.y + (x.y + x.h * 0.5) as f32 * app.canvas_scale))
        })
        .unwrap();
    h.event(Event::PointerMoved(p));
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

/// macOS: the modifiers change first, ⌥'s own key event comes a frame later.
fn press_option(h: &mut Harness<'static, SlideApp>) {
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.run_steps(1);
    h.event(Event::Key { key: Key::AltLeft, pressed: true, modifiers: Modifiers::ALT, repeat: false, physical_key: None });
    h.run_steps(1);
}

fn release_option(h: &mut Harness<'static, SlideApp>) {
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.event(Event::Key { key: Key::AltLeft, pressed: false, modifiers: Modifiers::NONE, repeat: false, physical_key: None });
    h.run_steps(3);
}

fn recording() -> bool {
    let mut s = Session::with_new();
    s.execute("agent.voiceStatus", &json!({})).unwrap()["recording"] == json!(true)
}

fn questions(h: &Harness<'static, SlideApp>) -> Vec<layer::Question> {
    layer::get(h.state().session.active().unwrap()).questions.clone()
}

#[test]
fn holding_option_records_pointing_at_shapes_and_releasing_asks() {
    let (mut h, a, b) = harness();
    // ⌥ alone, held: nothing at first (a tap isn't a question), recording after a moment.
    press_option(&mut h);
    assert!(!recording());
    h.run_steps(20);
    assert!(recording(), "holding ⌥ should start recording");
    // Pointing while speaking: a click on the other shape.
    click_shape(&mut h, b);
    // Release: the question is asked at once, on the first shape with the second pointed at.
    release_option(&mut h);
    assert!(!recording(), "releasing ⌥ should stop recording");
    let qs = questions(&h);
    assert_eq!(qs.len(), 1, "{qs:?}");
    assert!(qs[0].spoken);
    assert_eq!(qs[0].refs.len(), 1);
    // The question's topic has both shapes as places.
    let d = h.state().session.active().unwrap();
    let l = layer::get(d);
    let t = l.topic(&qs[0].topic).unwrap();
    let shapes: Vec<u64> = t
        .places
        .iter()
        .flat_map(|p| p.anchors.iter())
        .filter_map(|x| l.anchor(x))
        .filter_map(|x| deckcraft_agent::anchor::resolve(&d.doc, &deckcraft_agent::anchor::Index::of(&d.doc), x))
        .filter_map(|w| w.shape.map(|s| u64::from(s.0)))
        .collect();
    assert!(shapes.contains(&a) && shapes.contains(&b), "{shapes:?}");
    // The stand-in microphone transcribes nothing: a failed flag, no dialog.
    assert_eq!(qs[0].status, QuestionStatus::Failed);

    // ⌥ with another key is a shortcut: no recording, no question.
    press_option(&mut h);
    h.event(Event::Key { key: Key::E, pressed: true, modifiers: Modifiers::ALT, repeat: false, physical_key: None });
    h.run_steps(25);
    assert!(!recording());
    release_option(&mut h);
    assert_eq!(questions(&h).len(), 1);

    // A tap: no recording, no question.
    press_option(&mut h);
    release_option(&mut h);
    h.run_steps(20);
    assert!(!recording());
    assert_eq!(questions(&h).len(), 1);
}
