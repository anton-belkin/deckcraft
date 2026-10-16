#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use deckcraft_engine::Session;
use deckcraft_ui_egui::{Services, SlideApp};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn harness(s: Session) -> Harness<'static, SlideApp> {
    crate::install();
    // Never a real agent, never the user's real Claude Desktop config or AItoCraft registry.
    deckcraft_agent::registry::set_dir(Some(std::env::temp_dir().join(format!("deckcraft-ui-tests-{}/agents", std::process::id()))));
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
    h.get_by_label("Get ready");
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
fn the_context_group_switches_modes_and_gets_ready_without_ping_wording() {
    use deckcraft_agent::context::{Kind, Setup};
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let mut h = harness(s);
    // Installed already: the next step is asking the chat (a copy, nothing written anywhere).
    deckcraft_agent::context::set_setup(Setup { logged_in: Some(false), code_has_hub: Some(true), desktop_has_hub: Some(true) });
    let _ = h.state_mut().run("agent.contextMode", json!({"kind": "cowork"}));
    h.state_mut().ui.tab = "Agent".into();
    h.run_steps(2);
    h.get_by_label("Claude Cowork");
    h.get_by_label("Not ready");
    assert!(h.query_by_label_contains("anyone there").is_none(), "the check is never shown");
    assert!(h.query_by_label("Check").is_none() && h.query_by_label("Copy").is_none(), "the old pieces are gone");
    h.get_by_label("Get ready").click();
    h.run_steps(2);
    h.get_by_label("Waiting");
    // The mode menu: three rows with what the agent knows.
    h.get_by_label("Claude Cowork").click();
    h.run_steps(2);
    h.get_by_label_contains("Your Cowork chat, its project and memory.");
    h.get_by_label_contains("Your Claude Code session or a project folder.");
    h.get_by_label_contains("Only this document. Runs in the background.").click();
    h.run_steps(2);
    let kind = |h: &Harness<'static, SlideApp>| deckcraft_agent::active_layer(&h.state().session).unwrap().context.clone().unwrap();
    assert_eq!(kind(&h).kind, Kind::None);
    // Web, undoable like every layer action of the person's.
    h.get_by_label("Web").click();
    h.run_steps(2);
    assert!(kind(&h).web);
    let _ = h.state_mut().run("edit.undo", json!({}));
    h.run_steps(1);
    assert!(!kind(&h).web);
    // Not logged in: Get ready's only step is logging in.
    h.get_by_label("Get ready steps").click();
    h.run_steps(2);
    h.get_by_label("Log in to Claude");
    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    // Claude Code ▸ Project folder: Choose folder…, then the folder is shown.
    let _ = h.state_mut().run("agent.contextMode", json!({"kind": "code"}));
    h.run_steps(2);
    h.get_by_label("Claude Code");
    deckcraft_agent::context::set_folder_picker(Some(Box::new(|| Some("/tmp/Acme board".to_string()))));
    h.get_by_label("Project folder").click();
    h.run_steps(2);
    h.get_by_label("Get ready").click();
    h.run_steps(2);
    deckcraft_agent::context::set_folder_picker(None);
    assert_eq!(kind(&h).folder, "/tmp/Acme board");
    assert_eq!(deckcraft_agent::listener::mode(), deckcraft_agent::listener::Mode::Background);
    h.get_by_label_contains("Acme board");
    let _ = h.state_mut().run("agent.contextMode", json!({"kind": "cowork"}));
}

/// A deck with a text box on slide 2, selected.
fn deck_with_box(text: &str) -> (Session, u64) {
    crate::install();
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": text})).unwrap()["id"].as_u64().unwrap();
    s.execute("selection.select", &json!({"ids": [id]})).unwrap();
    (s, id)
}

#[test]
fn a_question_from_the_pane_starts_a_topic_and_new_topic_starts_another() {
    let (s, _) = deck_with_box("Revenue grew 42%");
    crate::request_pane(true, "selection");
    let mut h = harness(s);
    let is_ask = |n: &egui_kittest::Node<'_>| n.accesskit_node().placeholder().is_some_and(|p| p.contains("number come from"));
    for (text, button) in [("Where is 42% from?", "Ask the Agent"), ("And the tone?", "Ask the Agent"), ("Separate: the font", "New Topic")] {
        h.get_all_by_role(egui::accesskit::Role::MultilineTextInput).find(is_ask).expect("the question box").focus();
        h.run_steps(1);
        h.get_all_by_role(egui::accesskit::Role::MultilineTextInput).find(is_ask).expect("the question box").type_text(text);
        h.run_steps(1);
        h.get_by_label(button).click();
        h.run_steps(3);
    }
    let l = deckcraft_agent::active_layer(&h.state().session).unwrap();
    assert_eq!(l.questions.len(), 3);
    assert_eq!(l.questions[0].topic, l.questions[1].topic, "a follow-up joins the topic");
    assert_ne!(l.questions[0].topic, l.questions[2].topic, "New Topic starts another");
    assert_eq!(l.topics.len(), 2);
    // Both cards are on the Selection tab, with the box at the bottom.
    h.get_by_label_contains("adds to topic");
    assert!(h.get_all_by_label("Open").count() >= 2, "each card shows its status");
}

#[test]
fn a_marker_opens_its_topic_and_the_overview_shows_readiness() {
    let (mut s, id) = deck_with_box("Revenue grew 42%");
    let t = s.execute("agent.ask", &json!({"text": "Basis?", "slide": 2, "shape": id})).unwrap()["topicId"].as_str().unwrap().to_string();
    s.execute("agent.ask", &json!({"text": "On the slide", "slide": 2, "new": true})).unwrap();
    s.execute("agent.setStatus", &json!({"topic": t, "status": "agreed"})).unwrap();
    s.select(|_, sel| {
        sel.text = None;
        sel.shapes.clear();
    })
    .unwrap();
    let mut h = harness(s);
    // The markers: the slide's topic is 1, the shape's 2.
    h.get_by_label("Topic marker 2").click();
    h.run_steps(3);
    assert!(h.state().session.active().unwrap().selection.shapes.iter().any(|x| u64::from(x.0) == id), "the marker selected its place");
    // Agreed: the discussion is folded, one click opens it.
    let folded = h.query_all_by_label("Basis?").count();
    h.get_by_label("Discussion (1)").click();
    h.run_steps(3);
    assert_eq!(folded, 0, "folded once a status is set");
    assert!(h.query_all_by_label("Basis?").count() > 0, "one click opens it");
    // Overview: readiness for sign-off, and a topic opens from there.
    crate::request_pane(true, "overview");
    h.run_steps(3);
    h.get_by_label("Agreed 1 of 2 · 1 open");
    h.get_by_label("Open topic 1").click();
    h.run_steps(3);
    assert!(h.get_all_by_label("On the slide").count() >= 1, "the topic opened");
}

#[test]
fn the_agent_s_proposal_is_agreed_with_one_click() {
    let (mut s, id) = deck_with_box("Revenue grew 42%");
    let t = s.execute("agent.ask", &json!({"text": "Basis?", "slide": 2, "shape": id})).unwrap()["topicId"].as_str().unwrap().to_string();
    s.execute("agent.proposeAgreed", &json!({"topic": t, "reasons": "same basis both quarters", "actor": "agent"})).unwrap();
    crate::request_pane(true, "selection");
    let mut h = harness(s);
    h.get_by_label("The agent suggests: Agreed");
    h.get_by_label("Confirm Agreed").click();
    h.run_steps(3);
    let l = deckcraft_agent::active_layer(&h.state().session).unwrap();
    assert_eq!(l.topic(&t).unwrap().status, deckcraft_agent::layer::Status::Agreed);
    assert_eq!(l.topic(&t).unwrap().status_by, deckcraft_agent::Actor::Human);
}

#[test]
fn presenter_view_offers_the_markers_toggle() {
    crate::install();
    let mut s = Session::with_new();
    s.execute("agent.ask", &json!({"slide": 1, "text": "Title right?"})).unwrap();
    let doc = s.doc().unwrap().doc.clone();
    let uid = s.doc().unwrap().uid;
    let mut h = Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui(move |ui| {
        crate::presenter_preview(ui, &doc, 0, uid);
    });
    h.run_steps(2);
    h.get_by_label("Show topic markers on the slide");
}
