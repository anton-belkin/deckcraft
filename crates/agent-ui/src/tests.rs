#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use deckcraft_engine::Session;
use deckcraft_ui_egui::{Services, SlideApp};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn harness(s: Session) -> Harness<'static, SlideApp> {
    crate::install();
    // Never a real agent, never the user's real Claude Desktop config.
    deckcraft_agent::runner::set_config(deckcraft_agent::runner::Config {
        command: "deckcraft-test-no-agent".into(),
        data_dir: Some(std::env::temp_dir().join(format!("deckcraft-ui-tests-{}/agent", std::process::id()))),
        ..Default::default()
    });
    deckcraft_agent::cowork::set_config_path(Some(
        std::env::temp_dir().join(format!("deckcraft-ui-tests-{}/claude_desktop_config.json", std::process::id())),
    ));
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SlideApp::new(s, Services::default()),
    );
    h.run_steps(4);
    h
}

#[test]
fn asking_from_the_pane_puts_a_question_on_the_selected_shape() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Revenue grew 42%"})).unwrap()["id"].as_u64().unwrap();
    s.execute("selection.select", &json!({"ids": [id]})).unwrap();
    crate::request_pane(true, "slide");
    let mut h = harness(s);
    // The pane's question box (the notes pane has a text box too).
    let is_ask = |n: &egui_kittest::Node<'_>| n.accesskit_node().placeholder().is_some_and(|p| p.contains("number come from"));
    h.get_all_by_role(egui::accesskit::Role::MultilineTextInput).find(is_ask).expect("the question box").focus();
    h.run_steps(1);
    h.get_all_by_role(egui::accesskit::Role::MultilineTextInput).find(is_ask).expect("the question box").type_text("Where is 42% from?");
    h.run_steps(1);
    h.get_by_label("Ask the Agent").click();
    h.run_steps(3);
    let app = h.state();
    let l = deckcraft_agent::active_layer(&app.session).unwrap();
    assert_eq!(l.questions.len(), 1, "one question");
    assert_eq!(l.questions[0].text, "Where is 42% from?");
    let place = deckcraft_agent::anchor::resolve_id(app.session.active().unwrap(), &l, &l.questions[0].anchor).unwrap();
    assert_eq!(place.shape.map(|x| u64::from(x.0)), Some(id));
}

#[test]
fn presenter_view_lists_the_slide_s_prepared_answers() {
    crate::install();
    let mut s = Session::with_new();
    s.execute("agent.note", &json!({"slide": 1, "kind": "faq", "title": "Is it audited?", "text": "Not yet.", "actor": "agent"})).unwrap();
    let doc = s.doc().unwrap().doc.clone();
    let uid = s.doc().unwrap().uid;
    let mut h = Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui(move |ui| {
        crate::presenter_preview(ui, &doc, 0, uid);
    });
    h.run_steps(2);
    h.get_by_label("Prepared answers (1)");
    h.get_by_label("Is it audited?");
}

#[test]
fn the_agent_tab_marks_the_selection_as_client_data() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Acme Holdings AG"})).unwrap()["id"].as_u64().unwrap();
    s.execute("selection.select", &json!({"ids": [id]})).unwrap();
    let mut h = harness(s);
    h.state_mut().ui.tab = "Agent".into();
    h.run_steps(2);
    h.get_by_label("Mark as Client Data").click();
    h.run_steps(2);
    let app = h.state();
    let set = deckcraft_agent::privacy::private_shapes(&app.session);
    assert!(set.iter().any(|(_, x)| u64::from(x.0) == id), "marked from the ribbon");
    assert_eq!(app.ui.tab, "Agent", "the tab stays known");
    h.get_by_label("Check");
    h.get_by_label("Unmark Client Data");
}

#[test]
fn keytip_a_opens_the_agent_tab_without_typing_into_the_slide() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Hello"})).unwrap()["id"].as_u64().unwrap();
    s.execute("selection.select", &json!({"ids": [id]})).unwrap();
    let mut h = harness(s);
    assert_ne!(h.state().ui.tab, "Agent");
    h.key_press(egui::Key::F10);
    h.run_steps(1);
    h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::Text("a".into()));
    h.run_steps(2);
    assert_eq!(h.state().ui.tab, "Agent");
    let text = h.state().session.active().unwrap().doc.slides[1]
        .shapes
        .iter()
        .find(|x| u64::from(x.id.0) == id)
        .and_then(|x| x.text.as_ref())
        .map(|t| t.text())
        .unwrap();
    assert_eq!(text, "Hello", "the A finished the keytip, it wasn't typed");
}

#[test]
fn check_sends_a_ping_that_is_never_shown_as_a_question() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let mut h = harness(s);
    h.state_mut().ui.tab = "Agent".into();
    h.run_steps(2);
    h.get_by_label("Nobody is listening");
    h.get_by_label("Check").click();
    h.run_steps(2);
    let app = h.state();
    let l = deckcraft_agent::active_layer(&app.session).unwrap();
    assert_eq!(l.questions.iter().filter(|q| q.is_ping()).count(), 1);
    assert_eq!(l.asked().count(), 0, "not a question for the presenter");
    h.get_by_label_contains("Checking");
    // Copy holds the commands of the mode; Connect Cowork is in it.
    h.get_by_label("Copy").click();
    h.run_steps(2);
    h.get_by_label("Claude Code: add MCP server");
    h.get_by_label_contains("Connect Cowork");
}
