#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use deckcraft_engine::Session;
use deckcraft_engine::model::{ShapeId, ShapeKind};
use serde_json::{Value as Json, json};

use crate::layer::{self, QuestionStatus};
use crate::{anchor, links, xlsx};

fn setup() {
    crate::install();
    let reg = std::env::temp_dir().join(format!("deckcraft-agent-tests-{}.jsonl", std::process::id()));
    links::set_registry_path(Some(reg));
    // Nothing listens on the discard port: no test reaches a real GridCraft.
    links::set_gridcraft_addr(Some("127.0.0.1:9".into()));
}

fn run(s: &mut Session, id: &str, p: Json) -> Json {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// A three-slide deck: a title slide, a slide with a client's name in a text box and a numbers
/// box, and a slide that mentions the client again.
fn deck() -> (Session, ShapeId, ShapeId) {
    setup();
    // A new presentation starts with its title slide.
    let mut s = Session::with_new();
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    let client = run(&mut s, "insert.textBox", json!({"rect": [60, 60, 400, 60], "text": "Prepared for Acme Holdings AG"}));
    let numbers = run(&mut s, "insert.textBox", json!({"rect": [60, 200, 400, 60], "text": "Revenue grew 42% to 18.4M"}));
    run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": "Next steps with Acme Holdings AG", "body": "Sign the renewal"}));
    run(&mut s, "slide.go", json!({"index": 1}));
    let id = |v: &Json| ShapeId(v["id"].as_u64().unwrap() as u32);
    (s, id(&client), id(&numbers))
}

fn slide_of(s: &Session, id: ShapeId) -> usize {
    s.doc().unwrap().doc.slides.iter().position(|sl| sl.shape(id).is_some()).unwrap()
}

#[test]
fn extension_commands_are_listed_and_found() {
    setup();
    let s = Session::with_new();
    assert!(deckcraft_engine::find_command("agent.ask").is_some());
    assert!(deckcraft_engine::find_command("link.paste").is_some());
    assert!(s.commands().iter().any(|c| c.id == "agent.questions"));
}

#[test]
fn a_question_goes_from_waiting_to_answered() {
    let (mut s, _, numbers) = deck();
    let q = run(&mut s, "agent.ask", json!({"text": "Where does 42% come from?", "slide": 2, "shape": numbers.0, "words": "42%"}));
    let qid = q["id"].as_str().unwrap().to_string();
    assert!(q["place"].as_str().unwrap().contains("42%"), "{q}");
    let pending = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["where"]["slide"], 2);
    assert_eq!(pending[0]["where"]["shape"], numbers.0);
    assert_eq!(pending[0]["where"]["words"], "42%");
    run(&mut s, "agent.claim", json!({"id": qid}));
    assert!(s.execute("agent.claim", &json!({"id": qid})).is_err(), "taken twice");
    run(
        &mut s,
        "agent.answer",
        json!({"id": qid, "text": "Q3 over Q2 revenue, from the finance pack.", "links": [{"label": "Slide 2", "href": "slide:2"}]}),
    );
    let l = crate::active_layer(&s).unwrap();
    let q = l.questions.iter().find(|q| q.id == qid).unwrap();
    assert_eq!(q.status, QuestionStatus::Answered);
    assert!(!q.seen);
    // `links` became a sentence of links inside the text, stored as an anchor.
    let text = &q.answer.as_ref().unwrap().text;
    // "Slide 2" was the address: stored empty, shown as the current address.
    assert!(text.contains("Sources: [](anchor:"), "{text}");
    run(&mut s, "agent.seen", json!({"id": qid}));
    assert!(crate::active_layer(&s).unwrap().questions[0].seen);
}

#[test]
fn a_question_on_the_selection_anchors_to_the_selected_shape() {
    let (mut s, client, _) = deck();
    run(&mut s, "selection.select", json!({"ids": [client.0]}));
    let q = run(&mut s, "agent.ask", json!({"text": "Is this the legal name?"}));
    assert!(q["place"].as_str().unwrap().starts_with("Slide 2 › "), "{q}");
    let d = s.doc().unwrap();
    let sh = d.shape(client).unwrap();
    assert!(anchor::key_of(&sh.tags).is_some(), "the shape carries a hidden key");
}

#[test]
fn undo_never_takes_a_question_away_and_anchors_follow_moves() {
    let (mut s, _, numbers) = deck();
    run(&mut s, "agent.ask", json!({"text": "Source?", "slide": 2, "shape": numbers.0}));
    run(&mut s, "slide.move", json!({"from": 1, "to": 2}));
    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "edit.redo", json!({}));
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(l.questions.len(), 1);
    let d = s.doc().unwrap();
    let place = anchor::resolve_id(d, &l, &l.questions[0].anchor).unwrap();
    assert_eq!(place.slide, 2, "the slide moved to third place");
    assert_eq!(place.shape, Some(numbers));
}

#[test]
fn the_layer_survives_a_pptx_round_trip() {
    let (mut s, _, numbers) = deck();
    run(&mut s, "agent.ask", json!({"text": "Source?", "slide": 2, "shape": numbers.0, "words": "18.4M"}));
    run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "kind": "faq", "title": "Is 18.4M audited?", "text": "Not yet: management accounts.", "actor": "agent"}),
    );
    run(&mut s, "agent.decision", json!({"text": "Show revenue, not EBITDA, on slide 2", "source": "CFO call", "actor": "agent"}));
    let bytes = run(&mut s, "file.saveBytes", json!({"format": "pptx"}));
    let mut t = Session::new();
    run(&mut t, "file.openBytes", json!({"name": "deck.pptx", "data": bytes["data"]}));
    let l = crate::active_layer(&t).unwrap();
    assert_eq!(l.questions.len(), 1);
    assert_eq!(l.notes.len(), 1);
    assert!(l.log.iter().any(|e| e.text.contains("EBITDA")));
    let d = t.doc().unwrap();
    let place = anchor::resolve_id(d, &l, &l.questions[0].anchor).unwrap();
    assert_eq!(place.slide, 1);
    let sh = d.doc.slides[1].shape(place.shape.unwrap()).unwrap();
    assert!(sh.text.as_ref().unwrap().text().contains("18.4M"), "the key found the same shape after renumbering");
}

#[test]
fn client_data_is_masked_in_every_agent_read_and_the_agent_cant_write_there() {
    let (mut s, client, numbers) = deck();
    run(&mut s, "agent.private", json!({"slide": 2, "shape": client.0, "words": "Acme Holdings AG"}));
    run(&mut s, "agent.ask", json!({"text": "Is Acme Holdings AG the right entity?", "slide": 2, "shape": client.0}));
    let reads = [
        run(&mut s, "agent.read", json!({"slide": "all", "actor": "agent"})),
        run(&mut s, "agent.context", json!({"actor": "agent"})),
        run(&mut s, "agent.questions", json!({"actor": "agent"})),
        run(&mut s, "agent.layer", json!({"actor": "agent"})),
        // Asking for an unmasked read as the agent still masks.
        run(&mut s, "agent.read", json!({"slide": 3, "actor": "agent", "masked": false})),
    ];
    for r in &reads {
        assert!(!r.to_string().contains("Acme"), "leaked: {r}");
    }
    let all = reads[0].to_string();
    assert!(all.contains("[client-1]"), "{all}");
    assert!(reads[4].to_string().contains("Next steps with [client-1]"), "same name elsewhere gets the same pseudonym: {}", reads[4]);
    // The presenter's own view is not masked.
    assert!(run(&mut s, "agent.read", json!({"slide": 2, "masked": false})).to_string().contains("Acme Holdings AG"));
    // Writes to the marked shape are refused; elsewhere they work and are logged as the agent's.
    let e = s.execute("agent.exec", &json!({"command": "format.bold", "params": {"ids": [client.0]}, "reason": "emphasis"})).unwrap_err();
    assert!(e.to_string().contains("client data"), "{e}");
    run(&mut s, "selection.select", json!({"ids": [client.0]}));
    assert!(s.execute("agent.exec", &json!({"command": "edit.delete", "params": {}, "reason": "tidy"})).is_err(), "selection counts");
    assert!(s.execute("agent.exec", &json!({"command": "slide.delete", "params": {}, "reason": "tidy"})).is_err(), "the slide holds client data");
    run(&mut s, "selection.select", json!({"ids": [numbers.0]}));
    run(&mut s, "agent.exec", json!({"command": "format.bold", "params": {}, "reason": "the key number", "source": "q1"}));
    let l = crate::active_layer(&s).unwrap();
    let e = l.log.iter().rev().find(|e| e.kind == "change" && e.command == "format.bold").unwrap();
    assert_eq!(e.actor, crate::Actor::Agent);
    assert!(e.text.contains("the key number"), "{}", e.text);
    // Reads through exec are refused (they'd bypass the gate), and the agent can't lift a mark.
    assert!(s.execute("agent.exec", &json!({"command": "document.inspect", "params": {}})).is_err());
    assert!(s.execute("agent.exec", &json!({"command": "file.saveBytes", "params": {}})).is_err());
    assert!(s.execute("agent.private", &json!({"slide": 2, "shape": client.0, "on": false, "actor": "agent"})).is_err());
}

#[test]
fn names_and_figures_of_a_marked_shape_are_masked_everywhere() {
    use crate::privacy::names_and_figures;
    assert_eq!(names_and_figures("Prepared for Acme Holdings AG, fee 1,250,000."), vec!["Acme Holdings AG", "1,250,000"]);
    assert_eq!(names_and_figures("Revenue grew 42% in Q3"), Vec::<String>::new());
    assert_eq!(names_and_figures("Signed by Jane Doe. Next: Globex"), vec!["Jane Doe"]);
}

#[test]
fn a_wholly_private_table_shows_only_pseudonyms() {
    let (mut s, _, _) = deck();
    let t = run(&mut s, "insert.table", json!({"rows": 2, "cols": 2, "data": [["Client", "Fee"], ["Globex Ltd", "1,250,000"]]}));
    run(&mut s, "agent.private", json!({"id": t["id"]}));
    let r = run(&mut s, "agent.read", json!({"actor": "agent"})).to_string();
    for secret in ["Globex", "1,250,000", "Fee"] {
        assert!(!r.contains(secret), "{secret} leaked: {r}");
    }
}

#[test]
fn prepared_answers_belong_to_their_slide() {
    let (mut s, _, _) = deck();
    run(
        &mut s,
        "agent.note",
        json!({"slide": 3, "kind": "faq", "title": "When does the renewal start?", "text": "1 January, signed in December.", "actor": "agent"}),
    );
    run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "kind": "source", "title": "Finance pack", "text": "Q3 management accounts", "links": [{"label": "pack", "href": "https://example.com/q3"}], "actor": "agent"}),
    );
    let r = run(&mut s, "agent.read", json!({"slide": 3}));
    let notes = r["slides"][0]["agentNotes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["kind"], "faq");
    let ctx = run(&mut s, "agent.context", json!({}));
    assert_eq!(ctx["slides"][2]["preparedAnswers"], 1);
    assert_eq!(ctx["slides"][1]["preparedAnswers"], 0);
}

#[test]
fn people_s_edits_are_in_the_history_once_the_deck_has_a_layer() {
    let (mut s, _, numbers) = deck();
    run(&mut s, "selection.select", json!({"ids": [numbers.0]}));
    run(&mut s, "format.bold", json!({}));
    assert!(crate::active_layer(&s).unwrap().log.is_empty(), "a plain deck stays plain");
    run(&mut s, "agent.ask", json!({"text": "Why bold?", "slide": 2, "shape": numbers.0}));
    run(&mut s, "format.italic", json!({}));
    let l = crate::active_layer(&s).unwrap();
    let e = l.log.last().unwrap();
    assert_eq!(e.actor, crate::Actor::Human);
    assert_eq!(e.command, "format.italic");
    assert!(e.text.contains("slide 2"), "{}", e.text);
}

// ---------------------------------------------------------------- links

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("deckcraft-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The model: a header row and three regions, anchored by a hidden name.
fn workbook(revenue_north: f64, extra_row_above: bool) -> Vec<u8> {
    let mut rows = vec![];
    if extra_row_above {
        rows.push(vec![json!("inserted row")]);
    }
    rows.push(vec![json!("Region"), json!("Revenue")]);
    rows.push(vec![json!("North"), json!(revenue_north)]);
    rows.push(vec![json!("South"), json!(980000.0)]);
    let at = if extra_row_above { "Model!$A$2:$B$4" } else { "Model!$A$1:$B$3" };
    xlsx::build(&[("Model", rows)], &[("_GcLink_l1", at)])
}

fn payload(path: &str) -> links::Payload {
    links::Payload {
        kind: links::KIND.into(),
        version: 1,
        source: links::Source {
            app: "GridCraft".into(),
            path: path.into(),
            workbook_id: "wb-1".into(),
            anchor: "_GcLink_l1".into(),
            sheet: "Model".into(),
            range: "A1:B3".into(),
            label: "Revenue by region".into(),
        },
        values: vec![vec![json!("Region"), json!("Revenue")], vec![json!("North"), json!(1250000.0)], vec![json!("South"), json!(980000.0)]],
        text: vec![vec!["Region".into(), "Revenue".into()], vec!["North".into(), "1,250,000".into()], vec!["South".into(), "980,000".into()]],
        copied_at: 1,
        private: false,
        image: None,
    }
}

fn table_text(s: &Session, id: ShapeId) -> Vec<Vec<String>> {
    let d = s.doc().unwrap();
    let sl = &d.doc.slides[slide_of(s, id)];
    match &sl.shape(id).unwrap().kind {
        ShapeKind::Table(t) => t.rows.iter().map(|r| r.cells.iter().map(|c| c.text.text()).collect()).collect(),
        _ => panic!("not a table"),
    }
}

#[test]
fn the_payload_round_trips_through_clipboard_html() {
    let p = payload("/tmp/book.xlsx");
    let html = links::to_html(&p);
    assert!(html.contains(links::HTML_ATTR));
    assert!(html.contains("<td>1,250,000</td>"), "other apps get a plain table");
    let back = links::from_html(&html).unwrap();
    assert_eq!(back.source, p.source);
    assert_eq!(back.values, p.values);
    assert_eq!(links::to_tsv(&p).lines().nth(1), Some("North\t1,250,000"));
    assert!(links::from_html("<table><tr><td>1</td></tr></table>").is_none());
}

#[test]
fn a_linked_paste_notices_a_changed_number_even_after_rows_are_inserted() {
    let (mut s, _, _) = deck();
    let dir = temp_dir("stale");
    let book = dir.join("model.xlsx");
    std::fs::write(&book, workbook(1_250_000.0, false)).unwrap();
    let html = links::to_html(&payload(book.to_str().unwrap()));
    // A plain paste of the clipboard (as the UI does it) makes a linked table.
    let r = run(&mut s, "edit.paste", json!({"text": "Region\tRevenue", "html": html}));
    let id = ShapeId(r["id"].as_u64().unwrap() as u32);
    assert_eq!(table_text(&s, id)[1], vec!["North", "1,250,000"]);
    let c = run(&mut s, "link.check", json!({"gridcraft": false}));
    assert_eq!(c[0]["state"], "current", "{c}");
    assert_eq!(c[0]["anchored"], true);
    // In the workbook someone inserts a row above the range and changes North.
    std::fs::write(&book, workbook(1_310_000.0, true)).unwrap();
    let c = run(&mut s, "link.check", json!({"gridcraft": false}));
    assert_eq!(c[0]["state"], "stale", "{c}");
    assert_eq!(c[0]["changed"], 1);
    assert_eq!(c[0]["range"], "A2:B4", "the anchor followed the inserted row");
    let list = run(&mut s, "link.list", json!({}));
    assert_eq!(list[0]["state"], "stale");
    // Update brings the new number in, formatted like the old one; one undo step takes it back.
    run(&mut s, "link.update", json!({"id": id.0, "gridcraft": false}));
    assert_eq!(table_text(&s, id)[1], vec!["North", "1,310,000"]);
    let c = run(&mut s, "link.check", json!({"gridcraft": false}));
    assert_eq!(c[0]["state"], "current", "{c}");
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(table_text(&s, id)[1], vec!["North", "1,250,000"]);
}

#[test]
fn a_linked_picture_keeps_its_source_and_is_redrawn_on_update() {
    let (mut s, _, _) = deck();
    let dir = temp_dir("picture");
    let book = dir.join("model.xlsx");
    std::fs::write(&book, workbook(1_250_000.0, false)).unwrap();
    let r = run(&mut s, "link.paste", json!({"payload": payload(book.to_str().unwrap()), "mode": "picture"}));
    let id = ShapeId(r["id"].as_u64().unwrap() as u32);
    let media = |s: &Session| match &s.doc().unwrap().shape(id).unwrap().kind {
        ShapeKind::Picture { fill } => fill.media,
        _ => panic!("not a picture"),
    };
    let before = media(&s);
    let link = links::of(s.doc().unwrap().shape(id).unwrap()).unwrap();
    assert_eq!(link.mode, links::Mode::Picture);
    std::fs::write(&book, workbook(2_000_000.0, false)).unwrap();
    assert_eq!(run(&mut s, "link.check", json!({"gridcraft": false}))[0]["state"], "stale");
    run(&mut s, "link.update", json!({"id": id.0, "gridcraft": false}));
    assert_ne!(media(&s), before, "a new picture of the cells");
    let link = links::of(s.doc().unwrap().shape(id).unwrap()).unwrap();
    assert_eq!(link.text[1][1], "2,000,000");
    // The link travels with the deck.
    let bytes = run(&mut s, "file.saveBytes", json!({"format": "pptx"}));
    let mut t = Session::new();
    run(&mut t, "file.openBytes", json!({"name": "deck.pptx", "data": bytes["data"]}));
    assert_eq!(links::all(&t.doc().unwrap().doc).len(), 1);
}

#[test]
fn a_moved_workbook_is_found_next_to_the_deck_and_a_missing_one_is_flagged() {
    let (mut s, _, _) = deck();
    let dir = temp_dir("moved");
    run(&mut s, "link.paste", json!({"payload": payload("/nowhere/old place/model.xlsx")}));
    assert_eq!(run(&mut s, "link.check", json!({"gridcraft": false}))[0]["state"], "missing");
    // The deck and the workbook now sit together in one folder.
    std::fs::write(dir.join("model.xlsx"), workbook(1_250_000.0, false)).unwrap();
    run(&mut s, "file.saveAs", json!({"path": dir.join("deck.pptx").to_str().unwrap()}));
    let c = run(&mut s, "link.check", json!({"gridcraft": false}));
    assert_eq!(c[0]["state"], "current", "{c}");
}

#[test]
fn a_running_gridcraft_is_asked_first() {
    let (mut s, _, _) = deck();
    // A stand-in for GridCraft's control channel answering `link.read` (links.md).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Vec::<Json>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut w = stream.try_clone().unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let req: Json = serde_json::from_str(&line).unwrap();
            seen2.lock().unwrap().push(req.clone());
            let result = json!({"path": "/books/model.xlsx", "sheet": "Model", "range": "A1:B3",
                "values": [["Region", "Revenue"], ["North", 1400000], ["South", 980000]],
                "text": [["Region", "Revenue"], ["North", "$1.4M"], ["South", "$980k"]]});
            writeln!(w, "{}", json!({"id": req["id"], "ok": true, "result": result})).unwrap();
        }
    });
    let r = run(&mut s, "link.paste", json!({"payload": payload("/books/model.xlsx")}));
    let id = ShapeId(r["id"].as_u64().unwrap() as u32);
    let c = run(&mut s, "link.check", json!({"gridcraft": addr}));
    assert_eq!(c[0]["via"], "gridcraft");
    assert_eq!(c[0]["state"], "stale");
    let req = seen.lock().unwrap().iter().find(|r| r["params"]["command"] == "link.read").cloned().unwrap();
    assert_eq!(req["params"]["params"]["anchor"], "_GcLink_l1");
    run(&mut s, "link.update", json!({"id": id.0, "gridcraft": addr}));
    assert_eq!(table_text(&s, id)[1][1], "$1.4M", "GridCraft's own formatting");
}

#[test]
fn open_source_and_the_registry_of_uses() {
    let (mut s, _, _) = deck();
    let r = run(&mut s, "link.paste", json!({"payload": payload("/books/model.xlsx")}));
    let plan = run(&mut s, "link.open", json!({"id": r["id"], "dryRun": true}));
    assert_eq!(plan["goto"], "_GcLink_l1");
    assert_eq!(plan["request"]["command"], "link.reveal");
    run(&mut s, "slide.go", json!({"index": 0}));
    let link = links::all(&s.doc().unwrap().doc)[0].2.id.clone();
    let went = run(&mut s, "link.goto", json!({"link": link}));
    assert_eq!(went["slide"], 2);
    assert_eq!(s.doc().unwrap().selection.shapes, vec![ShapeId(r["id"].as_u64().unwrap() as u32)]);
    assert_eq!(run(&mut s, "link.goto", json!({"anchor": "_GcLink_l1"}))["slide"], 2);
    let uses = run(&mut s, "link.uses", json!({}));
    assert_eq!(uses[0]["anchor"], "_GcLink_l1");
    assert_eq!(uses[0]["slide"], 2);
    assert!(uses[0]["shapeKey"].is_null() || uses[0]["shapeKey"].is_string());
    let path = std::env::temp_dir().join(format!("registry-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    links::write_registry(&path, uses.as_array().unwrap());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.lines().next().unwrap().contains("\"event\":\"use\""));
}

#[test]
fn format_like_keeps_the_look_of_the_old_number() {
    use links::format_like;
    assert_eq!(format_like("1,250,000", &json!(1250000.0), &json!(1310000.0)), "1,310,000");
    assert_eq!(format_like("$1,234.50", &json!(1234.5), &json!(-99.0)), "$-99.00");
    assert_eq!(format_like("42%", &json!(0.42), &json!(0.5)), "50%");
    assert_eq!(format_like("42%", &json!(42.0), &json!(50.0)), "50%");
    assert_eq!(format_like("(1,000)", &json!(-1000.0), &json!(-2500.0)), "(2,500)");
    assert_eq!(format_like("18.4M", &json!(18.4), &json!(19.0)), "19.0M");
    assert_eq!(format_like("", &json!(null), &json!(7.0)), "7");
}

#[test]
fn hostile_payloads_are_refused() {
    setup();
    assert!(links::parse("{}").is_err());
    let mut p = payload("/x.xlsx");
    p.version = 9;
    assert!(links::parse(&serde_json::to_string(&p).unwrap()).is_err());
    let mut p = payload("/x.xlsx");
    p.values = vec![vec![json!(1); 100]; 100];
    assert!(links::parse(&serde_json::to_string(&p).unwrap()).is_err(), "too many cells");
    assert!(xlsx::Workbook::read(vec![1, 2, 3]).is_err());
    assert!(xlsx::parse_range("ZZZZ1").is_none());
    assert!(xlsx::parse_range("A0").is_none());
    assert_eq!(xlsx::parse_ref("'My ''x'' sheet'!$B$4:$D$9").unwrap().0, "My 'x' sheet");
}

// ---------------------------------------------------------------- MCP and the server

/// The app's control channel, in process: `engine.execute` and `engine.commands` on a session.
struct Local(Session);

impl deckcraft_mcp::Backend for Local {
    fn call(&mut self, method: &str, params: Json) -> Result<Json, String> {
        match method {
            "engine.execute" => {
                let cmd = params["command"].as_str().unwrap_or("");
                self.0.execute(cmd, &params["params"]).map_err(|e| e.to_string())
            }
            "engine.commands" => Ok(serde_json::to_value(self.0.commands()).unwrap()),
            other => Err(format!("unknown method {other}")),
        }
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "test".into()
    }
}

fn tool(b: &mut Local, name: &str, args: Json) -> Json {
    let r = crate::mcp::handle(b, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
    assert_eq!(r["result"]["isError"], false, "{name}: {r}");
    serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn the_mcp_tools_drive_the_layer_with_privacy() {
    let (mut s, client, _) = deck();
    run(&mut s, "agent.private", json!({"slide": 2, "shape": client.0}));
    let qid = run(&mut s, "agent.ask", json!({"text": "Check the client name", "slide": 2, "shape": client.0}))["id"].as_str().unwrap().to_string();
    let mut b = Local(s);
    let init =
        crate::mcp::handle(&mut b, &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "deckcraft-agent");
    let list = crate::mcp::handle(&mut b, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["get_context", "list_questions", "claim_question", "answer_question", "add_note", "log_decision", "run_command", "wait_for_questions"] {
        assert!(names.contains(&n), "{n}");
    }
    let ctx = tool(&mut b, "get_context", json!({}));
    let slides = tool(&mut b, "read_slides", json!({"slide": "all"}));
    let qs = tool(&mut b, "wait_for_questions", json!({"timeout_seconds": 0}));
    for v in [&ctx, &slides, &qs] {
        assert!(!v.to_string().contains("Acme"), "leaked through MCP: {v}");
    }
    assert_eq!(qs[0]["id"], qid.as_str());
    tool(&mut b, "claim_question", json!({"id": qid}));
    tool(&mut b, "answer_question", json!({"id": qid, "text": "It matches the contract's party name ([client-1])."}));
    tool(&mut b, "add_note", json!({"slide": 2, "kind": "faq", "title": "Who is the client?", "text": "See the cover: [client-1]."}));
    let l = crate::active_layer(&b.0).unwrap();
    assert_eq!(l.questions[0].status, QuestionStatus::Answered);
    assert_eq!(l.notes[0].author, crate::Actor::Agent);
    let r = crate::mcp::handle(
        &mut b,
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "edit.delete", "params": {"ids": [client.0]}, "reason": "x"}}}),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], true, "refused on client data");
}

/// A stand-in for the app's control channel on a socket, serving a session from a thread.
fn control_server(s: Session) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let session = Arc::new(Mutex::new(Local(s)));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let session = session.clone();
            std::thread::spawn(move || {
                crate::install();
                let mut w = stream.try_clone().unwrap();
                let mut r = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let req: Json = serde_json::from_str(&line).unwrap();
                    let res = deckcraft_mcp::Backend::call(&mut *session.lock().unwrap(), req["method"].as_str().unwrap(), req["params"].clone());
                    let reply = match res {
                        Ok(v) => json!({"id": req["id"], "ok": true, "result": v}),
                        Err(e) => json!({"id": req["id"], "ok": false, "error": e}),
                    };
                    if writeln!(w, "{reply}").is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

fn http(port: u16, method: &str, path: &str, body: &str) -> String {
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        c,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn a_question_reaches_the_agent_through_events_and_comes_back_answered() {
    let (s, _, numbers) = deck();
    let control = control_server(s);
    let port = crate::server::start(0, control.clone()).unwrap();
    assert!(http(port, "GET", "/health", "").ends_with("ok"));
    // The agent's background monitor.
    let mut events = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(events, "GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut ev = BufReader::new(events);
    // The presenter asks, through the app's control channel.
    let mut app = deckcraft_mcp::Remote::connect(&control).unwrap();
    let asked = deckcraft_mcp::Backend::call(
        &mut app,
        "engine.execute",
        json!({"command": "agent.ask", "params": {"text": "Where is 18.4M from?", "slide": 2, "shape": numbers.0}}),
    )
    .unwrap();
    let qid = asked["id"].as_str().unwrap().to_string();
    let line = loop {
        let mut l = String::new();
        ev.read_line(&mut l).unwrap();
        if l.trim_start().starts_with('{') {
            break l;
        }
    };
    let e: Json = serde_json::from_str(&line).unwrap();
    assert_eq!(e["event"], "question");
    assert_eq!(e["id"], qid.as_str());
    assert_eq!(e["slide"], 2);
    // The agent answers over MCP.
    let call =
        |name: &str, args: Json| json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    assert!(http(port, "POST", "/mcp", &call("claim_question", json!({"id": qid}))).contains("\"isError\":false"));
    assert!(http(port, "POST", "/mcp", &call("answer_question", json!({"id": qid, "text": "Finance pack, p. 4"}))).contains("\"isError\":false"));
    let qs =
        deckcraft_mcp::Backend::call(&mut app, "engine.execute", json!({"command": "agent.questions", "params": {"status": "answered"}})).unwrap();
    assert_eq!(qs[0]["id"], qid.as_str());
    assert_eq!(qs[0]["status"], "answered");
}

#[test]
fn layer_part_is_ours_and_cdata_safe() {
    let mut doc = deckcraft_engine::model::Presentation::default();
    let mut l = layer::Layer::default();
    l.log(crate::Actor::Human, "decision", "keeps ]]> safe".into(), "", None, String::new());
    layer::write_into(&mut doc, &l);
    assert_eq!(doc.kept_parts.len(), 1);
    assert_eq!(layer::read_from(&doc).log[0].text, "keeps ]]> safe");
    layer::write_into(&mut doc, &layer::Layer::default());
    assert!(doc.kept_parts.is_empty(), "an empty layer leaves no part");
}

#[test]
fn the_stdio_bridge_starts_without_the_app_and_reaches_it_once_it_is_open() {
    // Nothing listens on this port.
    let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let mut b = crate::stdio::Lazy::new(free);
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"cowork"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "not json",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_context","arguments":{}}}"#,
    ]
    .join("\n");
    let lines: Vec<Json> = stdio(&mut b, &input).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 4, "the notification gets no reply");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "deckcraft-agent");
    assert!(lines[0]["result"]["instructions"].as_str().unwrap().contains("wait_for_questions in a loop"));
    assert!(lines[1]["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "wait_for_questions"));
    assert_eq!(lines[2]["error"]["code"], -32700);
    assert_eq!(lines[3]["result"]["isError"], true);
    let msg = lines[3]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(msg.contains("not reachable") && msg.contains("Is DeckCraft open?"), "{msg}");

    // Now the app is open: a bridge reaches the deck through its control channel.
    let (mut s, _, numbers) = deck();
    run(&mut s, "agent.ask", json!({"text": "Where is 18.4M from?", "slide": 2, "shape": numbers.0}));
    let mut b = crate::stdio::Lazy::new(control_server(s));
    let before = crate::last_agent_call();
    let input = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wait_for_questions","arguments":{"timeout_seconds":0}}}"#;
    let text = stdio(&mut b, input);
    assert!(text.contains("Where is 18.4M from?"), "{text}");
    assert!(crate::last_agent_call() >= before && crate::last_agent_call() > 0, "the app sees an active agent");
}

/// Runs the stdio server over `input`, returning what it wrote.
fn stdio(b: &mut dyn deckcraft_mcp::Backend, input: &str) -> String {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let out: Arc<Mutex<dyn Write + Send>> = buf.clone();
    crate::stdio::serve(b, input.as_bytes(), out).unwrap();
    String::from_utf8(buf.lock().unwrap().clone()).unwrap()
}

// ---------------------------------------------------------------- is anyone there?

/// Tests that switch the app-wide mode run one at a time.
static MODE_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn a_ping_goes_out_like_a_question_but_is_never_shown_or_saved() {
    // The last check is app-wide: tests that make one run one at a time.
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut s, _, _) = deck();
    let uid = s.doc().unwrap().uid;
    assert!(s.execute("agent.ping", &json!({"actor": "agent"})).is_err(), "the agent doesn't ping itself");
    let id = run(&mut s, "agent.ping", json!({}))["id"].as_str().unwrap().to_string();
    assert!(matches!(crate::listener::health(Some(uid)), crate::listener::Health::Checking { .. }));
    let qs = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    let q = qs.as_array().unwrap().iter().find(|q| q["id"] == id.as_str()).unwrap().clone();
    assert_eq!(q["kind"], "ping");
    assert!(q["where"].is_null());
    assert_eq!(run(&mut s, "agent.context", json!({"actor": "agent"}))["openQuestions"], 0, "not a person's question");
    // Not in the file.
    let bytes = run(&mut s, "file.saveBytes", json!({"format": "pptx"}));
    let mut t = Session::new();
    run(&mut t, "file.openBytes", json!({"name": "d.pptx", "data": bytes["data"]}));
    assert!(crate::active_layer(&t).unwrap().questions.iter().all(|q| !q.is_ping()));
    // Answered: gone from the queue, and the status says so.
    run(&mut s, "agent.answer", json!({"id": id, "text": "here", "actor": "agent"}));
    assert!(crate::active_layer(&s).unwrap().questions.iter().all(|q| !q.is_ping()));
    assert!(matches!(crate::listener::health(Some(uid)), crate::listener::Health::Answering { .. }));
    assert_eq!(run(&mut s, "agent.connection", json!({}))["status"], "answering");
    assert!(crate::active_layer(&s).unwrap().log.iter().all(|e| !e.text.contains("Is anyone there")), "no history entry");
    // A ping nobody answers within a minute.
    crate::listener::set_ping(Some(crate::listener::Ping { id: "pingX".into(), uid, sent_at: 1, answered_at: None, answered_by: String::new() }));
    assert!(matches!(crate::listener::health(Some(uid)), crate::listener::Health::NoAnswer { .. }));
}

#[test]
fn wait_for_questions_counts_as_listening() {
    let (s, _, _) = deck();
    let mut b = Local(s);
    tool(&mut b, "wait_for_questions", json!({"timeout_seconds": 0}));
    assert!(crate::listener::signals(None).waiting);
    assert!(crate::listener::signals(None).describe().contains("wait_for_questions"));
}

#[test]
fn mode_off_refuses_the_agent_only() {
    use crate::listener::Mode;
    assert!(crate::refused_when_off(Mode::Off, &json!({"actor": "agent"})));
    assert!(!crate::refused_when_off(Mode::Off, &json!({})));
    assert!(!crate::refused_when_off(Mode::Desktop, &json!({"actor": "agent"})));
    for m in Mode::ALL {
        assert_eq!(Mode::parse(m.id()), Some(m));
        assert!(!m.label().is_empty() && !m.explanation().is_empty());
    }
}

#[test]
fn the_channel_stream_carries_questions_and_pings_in_channel_mode() {
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut s, _, numbers) = deck();
    run(&mut s, "agent.ask", json!({"text": "Source of 18.4M?", "slide": 2, "shape": numbers.0}));
    run(&mut s, "agent.ping", json!({}));
    let port = crate::server::start(0, control_server(s)).unwrap();
    // Not in the channel mode: no stream.
    crate::listener::set_mode(crate::listener::Mode::Desktop);
    assert!(http(port, "GET", "/mcp", "").starts_with("HTTP/1.1 405"));
    crate::listener::set_mode(crate::listener::Mode::Channel);
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(c, "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n").unwrap();
    let mut r = BufReader::new(c);
    let mut got: Vec<Json> = vec![];
    while got.len() < 2 {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap() == 0 {
            break;
        }
        if let Some(d) = l.strip_prefix("data:") {
            got.push(serde_json::from_str(d.trim()).unwrap());
        }
    }
    assert!(crate::server::status().channels >= 1, "the stream counts as a listener");
    crate::listener::set_mode(crate::listener::Mode::Desktop);
    assert!(got.iter().all(|e| e["method"] == "notifications/claude/channel"));
    let kinds: Vec<&str> = got.iter().map(|e| e["params"]["meta"]["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"question") && kinds.contains(&"ping"), "{got:?}");
    assert!(got.iter().any(|e| e["params"]["content"].as_str().unwrap().contains("Source of 18.4M?")));
    let init =
        crate::mcp::handle(&mut Local(Session::with_new()), &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})).unwrap();
    assert!(init["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
    let (add, start) = crate::channel::commands("/Applications/Deck Craft/deckcraft", 7985, 7984);
    assert!(add.contains("'/Applications/Deck Craft/deckcraft' --agent-stdio --control 127.0.0.1:7985 --agent-port 7984"), "{add}");
    assert!(start.contains("--dangerously-load-development-channels server:deckcraft-channel"));
}

#[test]
fn the_background_agent_is_started_as_specified_and_a_failure_is_shown() {
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let c =
        crate::runner::Config { data_dir: Some(std::env::temp_dir().join(format!("deckcraft-runner-{}", std::process::id()))), ..Default::default() };
    let a = crate::runner::args(&c, "S1", false, "/m.json", Some("/skill.md"), "DeckCraft · board.pptx", false);
    let joined = a.join(" ");
    for want in [
        "--name DeckCraft · board.pptx",
        "-p",
        "--input-format stream-json",
        "--output-format stream-json",
        "--verbose",
        "--session-id S1",
        "--strict-mcp-config",
        "--allowedTools mcp__deckcraft__*",
        "--append-system-prompt-file /skill.md",
    ] {
        assert!(joined.contains(want), "{want} in {joined}");
    }
    assert!(!joined.contains("--bare"));
    assert!(crate::runner::args(&c, "S1", true, "/m.json", None, "x", false).join(" ").contains("--resume S1"));
    let neutral = crate::runner::session_folder(&c, Some("/Users/x/Clients/Acme/board.pptx"));
    assert!(neutral.ends_with("agent-sessions"), "{neutral:?}");
    let opted = crate::runner::Config { use_deck_folder: true, ..c.clone() };
    assert_eq!(crate::runner::session_folder(&opted, Some("/Users/x/Clients/Acme/board.pptx")), std::path::PathBuf::from("/Users/x/Clients/Acme"));
    let u = crate::runner::new_uuid(7);
    assert_eq!(u.len(), 36);
    assert_eq!(u.chars().nth(14), Some('4'));
    crate::runner::set_config(c);
    let (mut s, _, numbers) = deck();
    run(&mut s, "agent.ask", json!({"text": "Why?", "slide": 2, "shape": numbers.0}));
    // The person decides the mode, never the agent.
    assert!(s.execute("agent.agentMode", &json!({"mode": "background", "actor": "agent"})).is_err());
    run(&mut s, "agent.agentMode", json!({"mode": "background"}));
    crate::runner::tick(&mut s);
    crate::listener::set_mode(crate::listener::Mode::Desktop);
    let st = run(&mut s, "agent.agentStatus", json!({}));
    assert_eq!(st["running"], false);
    assert!(st["lastError"].as_str().unwrap().contains("deckcraft-test-no-agent"), "{st}");
    let ping = json!({"id": "ping9", "kind": "ping"});
    assert!(crate::runner::question_text(&s, &ping).contains("\"here\""));
}

// ---------------------------------------------------------------- AItoCraft

/// Two decks: A (with a question, not active) and B (active).
fn two_decks() -> (Session, String, String) {
    setup();
    let mut s = Session::with_new();
    run(&mut s, "slide.new", json!({"layout": "titleOnly", "title": "Deck A"}));
    let a = crate::documents::id(s.doc().unwrap().uid);
    run(&mut s, "agent.ask", json!({"text": "Question in A", "slide": 2}));
    run(&mut s, "file.new", json!({}));
    run(&mut s, "agent.ask", json!({"text": "Question in B", "slide": 1}));
    let b = crate::documents::id(s.doc().unwrap().uid);
    (s, a, b)
}

#[test]
fn every_tool_works_on_a_deck_that_isnt_active() {
    let (s, a, b) = two_decks();
    let mut m = Local(s);
    let docs = tool(&mut m, "list_documents", json!({}));
    assert_eq!(docs.as_array().unwrap().len(), 2);
    assert!(docs.as_array().unwrap().iter().any(|d| d["id"] == b.as_str() && d["frontmost"] == true));
    let att = tool(&mut m, "attach", json!({"document": a, "label": "Cowork · Acme"}));
    assert_eq!(att["document"], a.as_str());
    assert_eq!(att["app"], "deckcraft");
    let listener = att["listener"].as_str().unwrap().to_string();
    let qs = tool(&mut m, "wait_for_questions", json!({"document": a, "listener": listener, "timeout_seconds": 0}));
    let qs = qs.as_array().unwrap();
    assert_eq!(qs.len(), 1, "only A's questions");
    assert_eq!(qs[0]["text"], "Question in A");
    assert_eq!(qs[0]["document"], a.as_str());
    assert!(qs[0]["documentTitle"].is_string());
    // The active deck stays B.
    assert_eq!(crate::documents::id(m.0.doc().unwrap().uid), b);
    // The listener shows on A, and A's status says who listens.
    let docs = tool(&mut m, "list_documents", json!({}));
    let da = docs.as_array().unwrap().iter().find(|d| d["id"] == a.as_str()).unwrap();
    assert_eq!(da["listeners"][0]["label"], "Cowork · Acme");
    let uid_a = crate::documents::parse(&a).unwrap();
    assert!(matches!(crate::listener::health(Some(uid_a)), crate::listener::Health::Listening { .. }));
    // A change on A through run_command lands on A, with its undo step there.
    let undo_a = m.0.documents().iter().find(|d| d.uid == uid_a).unwrap().history.undo.len();
    tool(&mut m, "run_command", json!({"document": a, "command": "slide.new", "params": {"layout": "blank"}, "reason": "test"}));
    let da = m.0.documents().iter().find(|d| d.uid == uid_a).unwrap();
    assert_eq!(da.doc.slides.len(), 3);
    assert_eq!(da.history.undo.len(), undo_a + 1);
    assert_eq!(crate::documents::id(m.0.doc().unwrap().uid), b);
    // An unknown deck is an error that says so.
    let r = crate::mcp::handle(
        &mut m,
        &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "get_context", "arguments": {"document": "deckcraft:1:2"}}}),
    )
    .unwrap();
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("document deckcraft:1:2 is not open in DeckCraft"), "{r}");
    tool(&mut m, "detach", json!({"listener": listener}));
    assert!(crate::listener::listeners(uid_a).is_empty());
}

#[test]
fn a_ping_answer_says_which_listener_answered() {
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut s, _, b) = two_decks();
    let uid = crate::documents::parse(&b).unwrap();
    let l = crate::listener::attach(uid, "Claude Code · deckcraft");
    let id = run(&mut s, "agent.ping", json!({}))["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.answer", json!({"id": id, "text": "here", "actor": "agent", "listener": l.id}));
    match crate::listener::health(Some(uid)) {
        crate::listener::Health::Answering { by, .. } => assert_eq!(by, "Claude Code · deckcraft"),
        other => panic!("{other:?}"),
    }
    crate::listener::set_ping(None);
}

#[test]
fn events_can_follow_one_deck() {
    let (s, a, _) = two_decks();
    let port = crate::server::start(0, control_server(s)).unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(c, "GET /events?document={} HTTP/1.1\r\nHost: localhost\r\n\r\n", a.replace(':', "%3A")).unwrap();
    let mut r = BufReader::new(c);
    let line = loop {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        if l.trim_start().starts_with('{') {
            break l;
        }
    };
    let e: Json = serde_json::from_str(&line).unwrap();
    assert_eq!(e["question"], "Question in A");
    assert_eq!(e["document"], a.as_str());
}

#[test]
fn the_registry_file_lists_the_decks_and_goes_away() {
    let (s, a, b) = two_decks();
    let dir = temp_dir("registry");
    crate::registry::set_dir(Some(dir.clone()));
    crate::registry::publish("http://127.0.0.1:7984/mcp", crate::documents::list(&s));
    crate::registry::flush(true);
    let f = crate::registry::file().unwrap();
    assert_eq!(f.file_name().unwrap().to_string_lossy(), format!("deckcraft-{}.json", std::process::id()));
    let v: Json = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
    assert_eq!(v["protocol"], "aitocraft/1");
    assert_eq!(v["app"], "deckcraft");
    assert_eq!(v["mcp"], "http://127.0.0.1:7984/mcp");
    let ids: Vec<&str> = v["documents"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&a.as_str()) && ids.contains(&b.as_str()));
    assert!(
        v["documents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d.get("openQuestions").is_some() && d.get("listeners").is_some() && d.get("focusedAt").is_some())
    );
    assert!(!v.to_string().contains("Question in A"), "no content in the registry");
    crate::registry::remove();
    assert!(!f.exists());
    crate::registry::set_dir(None);
}

#[test]
fn a_stale_check_drops_out_and_polls_are_not_tool_calls() {
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut s, _, _) = deck();
    let id = run(&mut s, "agent.ping", json!({}))["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("ping") && id[4..].parse::<u64>().is_ok(), "ping<ms>: {id}");
    // Make it a minute old: nobody gets it any more.
    layer::update(s.doc().unwrap(), |l| {
        if let Some(q) = l.questions.iter_mut().find(|q| q.id == id) {
            q.asked_at = q.asked_at.saturating_sub(61_000);
        }
    });
    let qs = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    assert!(qs.as_array().unwrap().iter().all(|q| q["id"] != id.as_str()));
    // Polls (`/events`, the channel, the runner) and waits don't count as the agent's calls.
    assert!(!crate::counts_as_call(&json!({"poll": true})));
    assert!(!crate::counts_as_call(&json!({"waiting": true})));
    assert!(crate::counts_as_call(&json!({"id": "q1"})));
    crate::listener::set_ping(None);
}

// ---------------------------------------------------------------- topics (TOPICS.md)

fn number(s: &Session, tid: &str) -> usize {
    let d = s.doc().unwrap();
    crate::topics::numbers_of(d, &crate::active_layer(s).unwrap()).topic(tid)
}

#[test]
fn a_question_starts_a_topic_and_a_second_one_on_the_shape_joins_it() {
    let (mut s, client, numbers) = deck();
    let q1 = run(&mut s, "agent.ask", json!({"text": "Source of 18.4M?", "slide": 2, "shape": numbers.0}));
    let t = q1["topicId"].as_str().unwrap().to_string();
    let l = crate::active_layer(&s).unwrap();
    let topic = l.topic(&t).unwrap();
    assert_eq!(topic.status, layer::Status::Open, "the first question opens it");
    assert_eq!(topic.places.len(), 1);
    let q2 = run(&mut s, "agent.ask", json!({"text": "And the 42%?", "slide": 2, "shape": numbers.0}));
    assert_eq!(q2["topicId"], t.as_str(), "a follow-up on the same place joins the topic");
    let q3 = run(&mut s, "agent.ask", json!({"text": "Separate thing", "slide": 2, "shape": numbers.0, "new": true}));
    assert_ne!(q3["topicId"], t.as_str(), "New topic starts another");
    // Numbering follows deck order: a topic on the client line (above the numbers) is number 1.
    let q4 = run(&mut s, "agent.ask", json!({"text": "Legal name?", "slide": 2, "shape": client.0}));
    let t4 = q4["topicId"].as_str().unwrap();
    assert_eq!(number(&s, t4), 1);
    assert_eq!(number(&s, &t), 2);
    // Questions carry their topic, so a follow-up is answered in context.
    let qs = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    let one = qs.as_array().unwrap().iter().find(|q| q["id"] == q2["id"]).unwrap().clone();
    assert_eq!(one["topic"]["id"], t.as_str());
    assert_eq!(one["topic"]["number"], 2);
    assert_eq!(one["topic"]["status"], "open");
}

#[test]
fn links_inside_text_become_anchors_and_follow_the_slides() {
    let (mut s, _, numbers) = deck();
    let qid = run(&mut s, "agent.ask", json!({"text": "Why?", "slide": 2, "shape": numbers.0}))["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.claim", json!({"id": qid, "actor": "agent"}));
    run(
        &mut s,
        "agent.answer",
        json!({"id": qid, "actor": "agent", "text": "See [slide 3](slide:3) and slide 2; the number is [here](shape:2/" .to_string() + &numbers.0.to_string() + ")."}),
    );
    let stored = crate::active_layer(&s).unwrap().questions.iter().find(|q| q.id == qid).unwrap().answer.clone().unwrap().text;
    assert!(stored.contains("[](anchor:") && !stored.contains("slide:3"), "stored as anchors: {stored}");
    // A slide inserted before them: the answer now says slide 4 and slide 3, and still leads there.
    run(&mut s, "slide.go", json!({"index": 0}));
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    let qs = run(&mut s, "agent.questions", json!({"actor": "agent"}));
    let text = qs.as_array().unwrap().iter().find(|q| q["id"] == qid.as_str()).unwrap()["answer"]["text"].as_str().unwrap().to_string();
    assert!(text.contains("[slide 4](slide:4)"), "{text}");
    assert!(text.contains("[slide 3](slide:3)"), "{text}");
    assert!(text.contains(&format!("[here](shape:3/{})", numbers.0)), "{text}");
}

#[test]
fn only_a_person_sets_agreed_and_the_agent_proposes_it() {
    let (mut s, _, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "OK?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    let e = s.execute("agent.setStatus", &json!({"topic": t, "status": "agreed", "actor": "agent"})).unwrap_err();
    assert!(e.to_string().contains("only a person sets Agreed"), "{e}");
    run(&mut s, "agent.setStatus", json!({"topic": t, "status": "revise", "text": "the base year is wrong", "actor": "agent"}));
    run(&mut s, "agent.proposeAgreed", json!({"topic": t, "reasons": "Checked against slide 3", "actor": "agent"}));
    assert!(s.execute("agent.confirmAgreed", &json!({"topic": t, "actor": "agent"})).is_err());
    run(&mut s, "agent.confirmAgreed", json!({"topic": t}));
    let l = crate::active_layer(&s).unwrap();
    let topic = l.topic(&t).unwrap();
    assert_eq!(topic.status, layer::Status::Agreed);
    assert_eq!(topic.status_by, crate::Actor::Human);
    assert!(topic.proposal.is_empty());
    // Every status change is in the log with who made it.
    let who: Vec<(crate::Actor, &str)> = l.log.iter().filter(|e| e.text.starts_with("Status")).map(|e| (e.actor, e.text.as_str())).collect();
    assert!(who.iter().any(|(a, t)| *a == crate::Actor::Agent && t.contains("Revise")));
    assert!(who.iter().any(|(a, t)| *a == crate::Actor::Human && t.contains("Agreed")));
}

#[test]
fn the_agent_adds_and_widens_places_and_the_person_can_reject_them() {
    let (mut s, client, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "Margin?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    let r = run(
        &mut s,
        "agent.addPlaces",
        json!({"topic": t, "actor": "agent", "places": [{"slide": 2, "shape": client.0, "reason": "the client named on slide 2 sets the fee"}]}),
    );
    let pid = r["places"][0]["id"].as_str().unwrap().to_string();
    let topic = s.execute("agent.topic", &json!({"topic": t})).unwrap();
    assert_eq!(topic["places"].as_array().unwrap().len(), 2);
    let labels: Vec<&str> = topic["places"].as_array().unwrap().iter().map(|p| p["place"].as_str().unwrap()).collect();
    assert!(labels.contains(&"1.1") && labels.contains(&"1.2"), "places numbered in deck order: {labels:?}");
    // Widen the first place to both shapes.
    let first = topic["places"][0]["id"].as_str().unwrap().to_string();
    run(
        &mut s,
        "agent.widenPlace",
        json!({"place": first, "to": {"slide": 2, "shapes": [client.0, numbers.0]}, "reason": "the whole cover", "actor": "agent"}),
    );
    let topic = s.execute("agent.topic", &json!({"topic": t})).unwrap();
    assert!(topic["places"].as_array().unwrap().iter().any(|p| p["shapes"].as_array().is_some_and(|a| a.len() == 2)));
    // The person rejects the agent's place; undo brings it back.
    run(&mut s, "agent.rejectPlace", json!({"place": pid}));
    assert_eq!(s.execute("agent.topic", &json!({"topic": t})).unwrap()["places"].as_array().unwrap().len(), 1);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.execute("agent.topic", &json!({"topic": t})).unwrap()["places"].as_array().unwrap().len(), 2);
}

#[test]
fn a_deleted_shape_keeps_its_whereabouts_and_undo_brings_it_back() {
    let (mut s, _, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "Why?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    run(&mut s, "selection.select", json!({"ids": [numbers.0]}));
    run(&mut s, "edit.delete", json!({}));
    let topic = s.execute("agent.topic", &json!({"topic": t})).unwrap();
    let w = topic["places"][0]["where"].as_str().unwrap();
    assert!(topic["places"][0]["removed"] == true, "{topic}");
    assert!(w.starts_with("removed — was on slide 2, after TextBox"), "{w}");
    run(&mut s, "edit.undo", json!({}));
    let topic = s.execute("agent.topic", &json!({"topic": t})).unwrap();
    assert!(topic["places"][0].get("removed").is_none(), "back as it was: {topic}");
    assert_eq!(topic["places"][0]["shape"], numbers.0);
}

#[test]
fn duplicating_a_shape_or_a_slide_duplicates_its_places() {
    let (mut s, _, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "Why?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    run(&mut s, "selection.select", json!({"ids": [numbers.0]}));
    run(&mut s, "edit.duplicate", json!({}));
    let places = |s: &mut Session| s.execute("agent.topic", &json!({"topic": t})).unwrap()["places"].as_array().unwrap().clone();
    let ps = places(&mut s);
    assert_eq!(ps.len(), 2, "the copy is a place of the same topic: {ps:?}");
    assert_ne!(ps[0]["shape"], ps[1]["shape"]);
    // Undo of the duplicate takes the copy's place away again.
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(places(&mut s).len(), 1);
    // A duplicated slide brings its places along.
    run(&mut s, "slide.duplicate", json!({"index": 1}));
    let ps = places(&mut s);
    assert_eq!(ps.len(), 2, "{ps:?}");
    let slides: Vec<i64> = ps.iter().map(|p| p["slide"].as_i64().unwrap()).collect();
    assert!(slides.contains(&2) && slides.contains(&3), "{slides:?}");
}

#[test]
fn undo_of_a_question_follows_where_the_agent_is() {
    let (mut s, _, numbers) = deck();
    // Not yet taken: undo removes it and its topic; redo sends it again.
    let q = run(&mut s, "agent.ask", json!({"text": "One?", "slide": 2, "shape": numbers.0}));
    let (qid, t) = (q["id"].as_str().unwrap().to_string(), q["topicId"].as_str().unwrap().to_string());
    run(&mut s, "edit.undo", json!({}));
    assert!(run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"})).as_array().unwrap().is_empty());
    assert!(run(&mut s, "agent.topics", json!({}))["topics"].as_array().unwrap().is_empty());
    run(&mut s, "edit.redo", json!({}));
    assert_eq!(run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"})).as_array().unwrap().len(), 1);
    // Being worked on: undo withdraws it, and the agent's answer is refused.
    run(&mut s, "agent.claim", json!({"id": qid, "actor": "agent"}));
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(run(&mut s, "agent.questions", json!({"status": "withdrawn", "actor": "agent"})).as_array().unwrap().len(), 1);
    let e = s.execute("agent.answer", &json!({"id": qid, "text": "x", "actor": "agent"})).unwrap_err();
    assert!(e.to_string().contains("withdrawn by the user"), "{e}");
    // Redo sends it again.
    run(&mut s, "edit.redo", json!({}));
    run(&mut s, "agent.claim", json!({"id": qid, "actor": "agent"}));
    run(&mut s, "agent.answer", json!({"id": qid, "text": "Because.", "actor": "agent"}));
    // Answered: undo hides the topic with its answer; redo restores it as it was, without asking again.
    run(&mut s, "edit.undo", json!({}));
    assert!(run(&mut s, "agent.topics", json!({}))["topics"].as_array().unwrap().is_empty());
    run(&mut s, "edit.redo", json!({}));
    let topic = s.execute("agent.topic", &json!({"topic": t})).unwrap();
    assert_eq!(topic["discussion"][0]["answer"], "Because.");
    assert_eq!(topic["discussion"][0]["status"], "answered");
    // Undoing a slide edit never touches the layer, and a layer step never touches the slides.
    let slides = s.doc().unwrap().doc.slides.len();
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    run(&mut s, "agent.setStatus", json!({"topic": t, "status": "info", "text": "noted"}));
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.doc().unwrap().doc.slides.len(), slides + 1, "the status step left the slides alone");
    assert_eq!(crate::active_layer(&s).unwrap().topic(&t).unwrap().status, layer::Status::Open);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.doc().unwrap().doc.slides.len(), slides);
    assert!(!crate::active_layer(&s).unwrap().topic(&t).unwrap().hidden, "the slide undo left the topic alone");
}

#[test]
fn the_persons_layer_actions_are_undoable_but_the_agents_are_not() {
    let (mut s, client, numbers) = deck();
    run(&mut s, "agent.private", json!({"slide": 2, "shape": client.0}));
    assert_eq!(crate::active_layer(&s).unwrap().private.len(), 1);
    run(&mut s, "edit.undo", json!({}));
    assert!(crate::active_layer(&s).unwrap().private.is_empty(), "marking client data is undoable");
    let undo_len = s.doc().unwrap().history.undo.len();
    let n = run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "shape": numbers.0, "kind": "source", "title": "Pack", "text": "p. 4", "url": "https://example.com", "summary": "Q3 pack", "actor": "agent"}),
    );
    assert_eq!(s.doc().unwrap().history.undo.len(), undo_len, "the agent's note isn't on the person's stack");
    let nid = n["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.pinNote", json!({"id": nid}));
    run(&mut s, "agent.removeNote", json!({"id": nid}));
    assert!(crate::active_layer(&s).unwrap().notes.iter().find(|x| x.id == nid).unwrap().deleted);
    run(&mut s, "edit.undo", json!({}));
    let note = crate::active_layer(&s).unwrap().notes.iter().find(|x| x.id == nid).unwrap().clone();
    assert!(!note.deleted && note.pinned);
    run(&mut s, "edit.undo", json!({}));
    assert!(!crate::active_layer(&s).unwrap().notes.iter().find(|x| x.id == nid).unwrap().pinned);
}

#[test]
fn version_1_decks_become_topics() {
    let mut l = layer::Layer {
        version: 1,
        anchors: vec![
            layer::Anchor { id: "a1".into(), slide_key: "k1".into(), ..Default::default() },
            layer::Anchor { id: "a2".into(), slide_key: "k2".into(), ..Default::default() },
        ],
        questions: vec![
            layer::Question { id: "q1".into(), anchor: "a1".into(), text: "Open one".into(), ..Default::default() },
            layer::Question {
                id: "q2".into(),
                anchor: "a1".into(),
                text: "Read one".into(),
                status: QuestionStatus::Answered,
                seen: true,
                answer: Some(layer::Answer { text: "A".into(), ..Default::default() }),
                ..Default::default()
            },
        ],
        notes: vec![
            layer::Note { id: "n1".into(), anchor: "a2".into(), text: "x".into(), ..Default::default() },
            layer::Note { id: "n2".into(), anchor: "a2".into(), text: "y".into(), ..Default::default() },
        ],
        ..Default::default()
    };
    l.migrate();
    assert_eq!(l.version, layer::VERSION);
    assert_eq!(l.topics.len(), 3, "one per question, one for the notes on an anchor");
    let t1 = l.topic(&l.questions[0].topic).unwrap();
    assert_eq!(t1.status, layer::Status::Open);
    assert_eq!(t1.places[0].anchors, vec!["a1".to_string()]);
    assert_eq!(l.topic(&l.questions[1].topic).unwrap().status, layer::Status::Info, "answered and read");
    assert_eq!(l.notes[0].topic, l.notes[1].topic);
    assert_eq!(l.topic(&l.notes[0].topic).unwrap().status, layer::Status::Info);
    // And a v1 part in a file loads as v2.
    let mut doc = deckcraft_engine::model::Presentation::default();
    let v1 = layer::Layer {
        version: 1,
        questions: vec![layer::Question { id: "q1".into(), anchor: "a1".into(), text: "Q".into(), ..Default::default() }],
        anchors: l.anchors.clone(),
        ..Default::default()
    };
    layer::write_into(&mut doc, &v1);
    // write_into stamps the current version: write the old one by hand.
    let xml = doc.kept_parts[0].xml.replace("\"version\":2", "\"version\":1");
    doc.kept_parts[0].xml = xml;
    let back = layer::read_from(&doc);
    assert_eq!(back.version, layer::VERSION);
    assert_eq!(back.topics.len(), 1);
}

#[test]
fn readiness_counts_for_sign_off() {
    let (mut s, client, numbers) = deck();
    let a = run(&mut s, "agent.ask", json!({"text": "1", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    let b = run(&mut s, "agent.ask", json!({"text": "2", "slide": 2, "shape": client.0}))["topicId"].as_str().unwrap().to_string();
    run(&mut s, "agent.setStatus", json!({"topic": a, "status": "agreed", "text": "fine"}));
    run(&mut s, "agent.setStatus", json!({"topic": b, "status": "revise"}));
    run(&mut s, "agent.note", json!({"slide": 3, "kind": "faq", "title": "Q?", "text": "A.", "actor": "agent"}));
    let r = run(&mut s, "agent.topics", json!({}));
    assert_eq!(r["readiness"], "Agreed 1 of 2 · 1 revise · 1 for information");
}

#[test]
fn a_stale_link_raises_its_topics_to_revise() {
    let (mut s, _, _) = deck();
    let dir = temp_dir("stale-topic");
    let book = dir.join("model.xlsx");
    std::fs::write(&book, workbook(1_250_000.0, false)).unwrap();
    let id = run(&mut s, "link.paste", json!({"payload": payload(book.to_str().unwrap())}))["id"].as_u64().unwrap();
    let t = run(&mut s, "agent.ask", json!({"text": "Right numbers?", "slide": 2, "shape": id}))["topicId"].as_str().unwrap().to_string();
    run(&mut s, "agent.setStatus", json!({"topic": t, "status": "info"}));
    std::fs::write(&book, workbook(1_310_000.0, false)).unwrap();
    run(&mut s, "link.check", json!({"gridcraft": false}));
    let l = crate::active_layer(&s).unwrap();
    let topic = l.topic(&t).unwrap();
    assert_eq!(topic.status, layer::Status::Revise);
    assert_eq!(topic.status_by, crate::Actor::App);
    // The person looked and agreed: the next check of the same change doesn't overrule them.
    run(&mut s, "agent.setStatus", json!({"topic": t, "status": "agreed"}));
    run(&mut s, "link.check", json!({"gridcraft": false}));
    assert_eq!(crate::active_layer(&s).unwrap().topic(&t).unwrap().status, layer::Status::Agreed);
}

#[test]
fn a_sent_copy_keeps_statuses_and_notes_but_not_the_discussion() {
    let (mut s, _, numbers) = deck();
    let q = run(&mut s, "agent.ask", json!({"text": "Is 18.4M audited?", "slide": 2, "shape": numbers.0}));
    let (qid, t) = (q["id"].as_str().unwrap().to_string(), q["topicId"].as_str().unwrap().to_string());
    run(&mut s, "agent.claim", json!({"id": qid, "actor": "agent"}));
    run(&mut s, "agent.answer", json!({"id": qid, "text": "No, management accounts.", "actor": "agent"}));
    run(&mut s, "agent.note", json!({"topic": t, "kind": "source", "title": "Q3 pack", "text": "p. 4", "actor": "agent"}));
    run(&mut s, "agent.setStatus", json!({"topic": t, "status": "info", "text": "unaudited, said so"}));
    let read = |data: &Json| {
        let mut t = Session::new();
        run(&mut t, "file.openBytes", json!({"name": "sent.pptx", "data": data["data"]}));
        crate::active_layer(&t).unwrap()
    };
    let l = read(&run(&mut s, "agent.copyToSend", json!({})));
    assert_eq!(l.topics.len(), 1);
    assert_eq!(l.topics[0].status_text, "unaudited, said so");
    assert_eq!(l.notes.len(), 1);
    assert!(l.questions.is_empty(), "no discussion");
    assert!(l.log.iter().all(|e| !e.text.contains("audited?")));
    let l = read(&run(&mut s, "agent.copyToSend", json!({"discussion": true})));
    assert_eq!(l.questions.len(), 1, "included when asked");
}

// ---------------------------------------------------------------- the Context group

#[test]
fn the_context_is_per_document_saved_in_the_file_and_undoable() {
    use crate::context::{self, Kind};
    let _g = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    crate::listener::set_mode(crate::listener::Mode::Desktop);
    let (mut s, _, _) = deck();
    // Nothing chosen yet: the last mode used in the app.
    let c = run(&mut s, "agent.contextMode", json!({}));
    assert_eq!(c["kind"], "cowork");
    // A person's choice; the agent can't make it.
    assert!(s.execute("agent.contextMode", &json!({"kind": "none", "actor": "agent"})).is_err());
    run(&mut s, "agent.contextMode", json!({"kind": "code", "code": "folder", "folder": "/tmp/project"}));
    assert_eq!(crate::listener::mode(), crate::listener::Mode::Background, "a project folder runs the background client");
    let l = crate::active_layer(&s).unwrap();
    let saved = l.context.clone().unwrap();
    assert_eq!((saved.kind, saved.folder.as_str()), (Kind::Code, "/tmp/project"));
    assert_eq!(saved.cwd(), Some(std::path::PathBuf::from("/tmp/project")));
    // In the file.
    let mut doc = (*s.doc().unwrap().doc).clone();
    layer::write_into(&mut doc, &l);
    assert_eq!(layer::read_from(&doc).context, Some(saved.clone()));
    // Not in a copy that is sent: the folder is the sender's.
    assert!(layer::for_sending(&l, true).context.is_none());
    // Undo takes the choice back.
    run(&mut s, "agent.contextMode", json!({"kind": "none", "web": true}));
    assert!(crate::active_layer(&s).unwrap().context.clone().unwrap().web());
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(crate::active_layer(&s).unwrap().context, Some(saved));
    run(&mut s, "edit.redo", json!({}));
    assert!(crate::active_layer(&s).unwrap().context.clone().unwrap().web());
    // A new document starts with the last mode used (not its folder); the app follows the active one.
    run(&mut s, "file.new", json!({}));
    let fresh = context::of(s.active().unwrap());
    assert_eq!((fresh.kind, fresh.folder.as_str()), (Kind::None, ""));
    crate::listener::set_mode(crate::listener::Mode::Channel);
    context::apply(&s);
    assert_eq!(crate::listener::mode(), crate::listener::Mode::Background);
    crate::listener::set_mode(crate::listener::Mode::Desktop);
}

#[test]
fn web_lets_the_background_client_search_and_fetch() {
    let c = crate::runner::Config::default();
    let with = crate::runner::args(&c, "S1", false, "/m.json", None, "x", true).join(" ");
    assert!(with.contains("--allowedTools WebSearch") && with.contains("--allowedTools WebFetch"), "{with}");
    let without = crate::runner::args(&c, "S1", false, "/m.json", None, "x", false).join(" ");
    assert!(!without.contains("WebSearch"));
    // Web belongs to No context only.
    let ctx = crate::context::Context { kind: crate::context::Kind::Cowork, web: true, ..Default::default() };
    assert!(!ctx.web());
}

// ---------------------------------------------------------------- voice questions

#[test]
fn a_spoken_question_waits_for_its_text_and_its_pointing_becomes_places() {
    let (mut s, client, numbers) = deck();
    // Asked on the numbers, the client line pointed at while speaking.
    let q = run(
        &mut s,
        "agent.ask",
        json!({"slide": 2, "shape": numbers.0, "transcribing": true, "refs": [{"n": 1, "slide": 2, "shape": client.0}, {"n": 2, "slide": 3}]}),
    );
    let id = q["id"].as_str().unwrap().to_string();
    let tid = q["topicId"].as_str().unwrap().to_string();
    // Not the agent's yet, and never in the file.
    assert!(run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"})).as_array().unwrap().is_empty());
    let l = crate::active_layer(&s).unwrap();
    let mut doc = (*s.doc().unwrap().doc).clone();
    layer::write_into(&mut doc, &l);
    assert!(layer::read_from(&doc).questions.is_empty());
    // One place per thing pointed at, besides the main one.
    let t = l.topic(&tid).unwrap();
    assert_eq!(t.places.len(), 3, "{:?}", t.places);
    assert_eq!(t.places.iter().filter(|p| p.reason == "pointed at while asking").count(), 2);
    // The text arrives: the agent reads where each marker points (the client line is client data).
    run(&mut s, "agent.private", json!({"slide": 2, "shape": client.0}));
    run(&mut s, "agent.spoken", json!({"id": id, "text": "Is this ⟦1⟧ client the one on ⟦2⟧?"}));
    let list = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    let q = &list[0];
    let text = q["text"].as_str().unwrap();
    assert!(text.contains("⟦1: slide 2 › "), "{text}");
    assert!(text.contains("⟦2: slide 3⟧"), "{text}");
    // Client data stays masked in what the agent reads.
    assert!(!text.contains("Acme"), "{text}");
    assert_eq!(q["refs"][1]["slide"], 3);
    assert_eq!(q["spoken"], true);
    // A failed one can be dismissed; its topic goes with it.
    let q2 = run(&mut s, "agent.ask", json!({"slide": 3, "transcribing": true, "new": true}));
    let id2 = q2["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.spoken", json!({"id": id2, "error": "Nothing was heard"}));
    assert!(s.execute("agent.claim", &json!({"id": id2, "actor": "agent"})).is_err());
    run(&mut s, "agent.removeQuestion", json!({"id": id2}));
    let l = crate::active_layer(&s).unwrap();
    assert!(l.questions.iter().all(|q| q.id != id2));
    assert!(l.topic(q2["topicId"].as_str().unwrap()).unwrap().hidden);
    // A question with the agent can't be dismissed that way.
    assert!(s.execute("agent.removeQuestion", &json!({"id": id})).is_err());
}

// ---------------------------------------------------------------- proposals (next-brief B)

fn text_of(s: &Session, id: ShapeId) -> String {
    let d = s.doc().unwrap();
    d.doc.slides.iter().find_map(|sl| sl.shape(id)).and_then(|sh| sh.text.as_ref()).map(|t| t.text()).unwrap_or_default()
}

#[test]
fn in_a_topic_run_command_proposes_and_the_person_applies_one_option_as_one_undo_step() {
    let (mut s, _, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "Shorter?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    let before = text_of(&s, numbers);
    // Option A and option B: recorded, the deck untouched.
    let r = run(
        &mut s,
        "agent.exec",
        json!({"actor": "agent", "topic": t, "command": "text.set", "params": {"id": numbers.0, "text": "Revenue +42% (18.4M)"}, "reason": "shorter"}),
    );
    assert_eq!(r["proposed"], true);
    run(
        &mut s,
        "agent.exec",
        json!({"actor": "agent", "topic": t, "option": "B", "command": "text.set", "params": {"id": numbers.0, "text": "Revenue grew to 18.4M"}, "reason": "without the percentage"}),
    );
    run(
        &mut s,
        "agent.describeOption",
        json!({"actor": "agent", "topic": t, "option": "B", "title": "Drop the percentage", "why": "the base changed on slide 2"}),
    );
    assert_eq!(text_of(&s, numbers), before, "a proposal doesn't change the deck");
    // A command that doesn't fit is refused and not recorded.
    assert!(s.execute("agent.exec", &json!({"actor": "agent", "topic": t, "command": "text.set", "params": {"id": 99_999, "text": "x"}})).is_err());
    // What the agent reads: the proposal, the shape it touches, the draft.
    let topic = run(&mut s, "agent.topic", json!({"topic": t, "actor": "agent"}));
    assert_eq!(topic["proposal"]["state"], "pending");
    assert_eq!(topic["proposal"]["options"].as_array().unwrap().len(), 2);
    assert_eq!(topic["proposal"]["options"][1]["title"], "Drop the percentage");
    let read = run(&mut s, "agent.read", json!({"slide": 2, "actor": "agent"}));
    let sh = read["slides"][0]["shapes"].as_array().unwrap().iter().find(|x| x["id"] == numbers.0).unwrap().clone();
    assert_eq!(sh["pending"].as_array().unwrap().len(), 2, "{sh}");
    let view = run(&mut s, "agent.read", json!({"slide": 2, "actor": "agent", "view": "proposal", "topic": t, "option": "B"}));
    assert!(view.to_string().contains("Revenue grew to 18.4M"));
    // The agent can't apply its own proposal.
    assert!(s.execute("agent.applyProposal", &json!({"topic": t, "actor": "agent"})).is_err());
    // The person applies B: the deck changes in one undo step, with a decision note.
    run(&mut s, "agent.applyProposal", json!({"topic": t, "option": "B"}));
    assert_eq!(text_of(&s, numbers), "Revenue grew to 18.4M");
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(l.topic(&t).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Applied);
    assert!(l.notes.iter().any(|n| n.topic == t && n.title == "chose B: Drop the percentage" && n.author == crate::Actor::Human));
    assert!(l.events.iter().any(|e| e.event == "proposal_applied" && e.option == "B"));
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(text_of(&s, numbers), before, "one undo takes the whole option back");
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(l.topic(&t).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Pending);
    assert!(l.notes.iter().filter(|n| n.topic == t).all(|n| n.deleted));
    run(&mut s, "edit.redo", json!({}));
    assert_eq!(text_of(&s, numbers), "Revenue grew to 18.4M");
}

#[test]
fn applying_one_proposal_outdates_the_overlapping_ones_and_a_step_that_no_longer_fits_applies_nothing() {
    let (mut s, client, numbers) = deck();
    let t1 = run(&mut s, "agent.ask", json!({"text": "Wording?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    let t2 = run(&mut s, "agent.ask", json!({"text": "Other wording?", "slide": 2, "shape": numbers.0, "new": true}))["topicId"]
        .as_str()
        .unwrap()
        .to_string();
    let t3 = run(&mut s, "agent.ask", json!({"text": "Client line?", "slide": 2, "shape": client.0}))["topicId"].as_str().unwrap().to_string();
    for (t, id, text) in [(&t1, numbers, "One"), (&t2, numbers, "Two"), (&t3, client, "Prepared for the client")] {
        run(&mut s, "agent.exec", json!({"actor": "agent", "topic": t, "command": "text.set", "params": {"id": id.0, "text": text}}));
    }
    run(&mut s, "agent.applyProposal", json!({"topic": t1}));
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(l.topic(&t2).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Outdated, "same shape");
    assert_eq!(l.topic(&t3).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Pending, "another shape");
    assert!(l.events.iter().any(|e| e.event == "proposal_outdated" && e.topic == t2));
    // The person deletes the client line: t3's step no longer fits, nothing is applied.
    run(&mut s, "selection.select", json!({"ids": [client.0]}));
    run(&mut s, "edit.delete", json!({}));
    let e = s.execute("agent.applyProposal", &json!({"topic": t3})).unwrap_err().to_string();
    assert!(e.contains("out of date, ask the agent to redo"), "{e}");
    assert_eq!(crate::active_layer(&s).unwrap().topic(&t3).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Outdated);
}

#[test]
fn a_reject_with_a_comment_comes_back_as_a_follow_up_and_is_undoable() {
    let (mut s, _, numbers) = deck();
    let t = run(&mut s, "agent.ask", json!({"text": "Shorter?", "slide": 2, "shape": numbers.0}))["topicId"].as_str().unwrap().to_string();
    run(&mut s, "agent.exec", json!({"actor": "agent", "topic": t, "command": "text.set", "params": {"id": numbers.0, "text": "Short"}}));
    run(&mut s, "agent.rejectProposal", json!({"topic": t, "comment": "keep the 42%"}));
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(l.topic(&t).unwrap().draft.as_ref().unwrap().state, crate::proposals::State::Rejected);
    assert!(l.events.iter().any(|e| e.event == "proposal_rejected" && e.comment == "keep the 42%"));
    let pending = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    assert!(pending.as_array().unwrap().iter().any(|q| q["text"].as_str().unwrap().contains("keep the 42%") && q["topic"]["id"] == t.as_str()));
    // A new proposal near it shows up on the next question about that place.
    run(&mut s, "agent.exec", json!({"actor": "agent", "topic": t, "command": "text.set", "params": {"id": numbers.0, "text": "Revenue grew 42%"}}));
    let q = run(&mut s, "agent.ask", json!({"text": "And this?", "slide": 2, "shape": numbers.0}));
    let list = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    let mine = list.as_array().unwrap().iter().find(|x| x["id"] == q["id"]).unwrap().clone();
    assert_eq!(mine["pendingNearby"].as_array().unwrap().len(), 1, "{mine}");
}

// ---------------------------------------------------------------- the file's context (next-brief E)

#[test]
fn the_file_context_reaches_the_agent_is_kept_when_sent_and_the_person_s_edits_undo() {
    let (mut s, _, numbers) = deck();
    let id = run(
        &mut s,
        "agent.addFileContext",
        json!({"actor": "agent", "title": "Audience", "kind": "audience", "text": "The board of Acme, quarterly review"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The agent reads a summary with get_context and with each question.
    let ctx = run(&mut s, "agent.context", json!({"actor": "agent"}));
    assert!(ctx["fileContext"].as_str().unwrap().contains("Audience (audience): The board"), "{ctx}");
    run(&mut s, "agent.ask", json!({"text": "Tone?", "slide": 2, "shape": numbers.0}));
    let q = run(&mut s, "agent.questions", json!({"status": "pending", "actor": "agent"}));
    assert!(q[0]["fileContext"].as_str().unwrap().contains("Audience"));
    assert_eq!(run(&mut s, "agent.fileContext", json!({"actor": "agent"})).as_array().unwrap().len(), 1);
    // Kept in a copy that is sent.
    let l = crate::active_layer(&s).unwrap();
    assert_eq!(layer::for_sending(&l, false).file_context.len(), 1);
    // The person's edits are undoable.
    run(&mut s, "agent.updateFileContext", json!({"id": id, "text": "The audit committee"}));
    run(&mut s, "agent.addFileContext", json!({"title": "House rules", "kind": "rules", "text": "No forecasts on slides"}));
    assert_eq!(crate::active_layer(&s).unwrap().file_context.len(), 2);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(crate::active_layer(&s).unwrap().file_context.len(), 1);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(crate::active_layer(&s).unwrap().file_context[0].text, "The board of Acme, quarterly review");
    run(&mut s, "agent.removeFileContext", json!({"id": id}));
    assert!(crate::active_layer(&s).unwrap().file_context.is_empty());
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(crate::active_layer(&s).unwrap().file_context.len(), 1);
}
