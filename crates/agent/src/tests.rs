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
    assert_eq!(q.answer.as_ref().unwrap().links.len(), 1);
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
