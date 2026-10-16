//! Offscreen render of DeckCraft with the agent layer, for looking at the UI without a window
//! (and for the README's screenshots). Builds a demo deck in code: a client's name marked as client
//! data, questions in every state, prepared answers, and a table linked to a workbook whose numbers
//! have since changed.
//!
//! `cargo run -p deckcraft-agent-ui --example agent_snapshot -- [--pane slide|questions|links|history|connect]
//!  [--slide N] [--hover-link] [--presenter] [--agent-tab] [--collapsed]
//!  [--status answering|checking|noanswer|listener|listening|nobody] [--mode connect|channel|background|off] [--menu] [--size 1440x900] [--dark] out.png`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use deckcraft_agent::links;
use deckcraft_engine::Session;
use deckcraft_engine::model::ShapeId;
use deckcraft_ui_egui::{Services, SlideApp};
use serde_json::{Value, json};

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// The demo deck. Returns the session and the linked table's id.
fn demo(dir: &std::path::Path) -> (Session, ShapeId) {
    let mut s = Session::with_new();
    // Slide 1: the cover, with the client's name.
    let d = s.doc().unwrap();
    let title = d.doc.slides[0].shapes.iter().map(|x| x.id).collect::<Vec<_>>();
    run(&mut s, "text.edit", json!({"id": title[0].0}));
    run(&mut s, "text.insert", json!({"text": "Q3 Board Review"}));
    run(&mut s, "text.edit", json!({"id": title[1].0}));
    run(&mut s, "text.insert", json!({"text": "Prepared for Acme Holdings AG"}));
    s.select(|_, sel| {
        sel.text = None;
        sel.shapes.clear();
    })
    .unwrap();
    // Slide 2: the numbers, from the model.
    run(&mut s, "slide.new", json!({"layout": "titleOnly", "title": "Revenue by region"}));
    let book = dir.join("q3-model.xlsx");
    let rows = |north: f64| {
        vec![
            vec![json!("Region"), json!("Q2"), json!("Q3")],
            vec![json!("North"), json!(1_180_000.0), json!(north)],
            vec![json!("South"), json!(910_000.0), json!(980_000.0)],
            vec![json!("West"), json!(640_000.0), json!(705_000.0)],
        ]
    };
    std::fs::write(&book, deckcraft_agent::xlsx::build(&[("Model", rows(1_250_000.0))], &[("_GcLink_l1", "Model!$A$1:$C$4")])).unwrap();
    let payload = links::Payload {
        kind: links::KIND.into(),
        version: 1,
        source: links::Source {
            app: "GridCraft".into(),
            path: book.to_string_lossy().to_string(),
            workbook_id: "demo".into(),
            anchor: "_GcLink_l1".into(),
            sheet: "Model".into(),
            range: "A1:C4".into(),
            label: "Revenue by region, Q2–Q3".into(),
        },
        values: rows(1_250_000.0),
        text: vec![
            vec!["Region".into(), "Q2".into(), "Q3".into()],
            vec!["North".into(), "1,180,000".into(), "1,250,000".into()],
            vec!["South".into(), "910,000".into(), "980,000".into()],
            vec!["West".into(), "640,000".into(), "705,000".into()],
        ],
        copied_at: deckcraft_agent::now_ms().saturating_sub(3 * 3600 * 1000),
        private: false,
        image: None,
    };
    let html = links::to_html(&payload);
    let table = run(&mut s, "edit.paste", json!({"text": links::to_tsv(&payload), "html": html}));
    let table = ShapeId(table["id"].as_u64().unwrap() as u32);
    let growth = run(&mut s, "insert.textBox", json!({"rect": [80, 400, 520, 50], "text": "Revenue grew 6% quarter on quarter to 2.94M"}));
    let growth = growth["id"].as_u64().unwrap();
    // Someone changes North in the workbook after the paste.
    std::fs::write(&book, deckcraft_agent::xlsx::build(&[("Model", rows(1_310_000.0))], &[("_GcLink_l1", "Model!$A$1:$C$4")])).unwrap();
    // Slide 3: next steps, naming the client again.
    run(
        &mut s,
        "slide.new",
        json!({"layout": "titleAndContent", "title": "Next steps with Acme Holdings AG", "body": "Sign the renewal by December\nKick-off in January"}),
    );
    // The layer: client data, questions, prepared answers, decisions.
    let cover = title[1].0;
    run(&mut s, "agent.private", json!({"slide": 1, "shape": cover, "words": "Acme Holdings AG"}));
    let q1 =
        run(&mut s, "agent.ask", json!({"slide": 2, "shape": growth, "words": "6%", "text": "Is 6% on the same basis as last quarter's figure?"}));
    let q2 = run(&mut s, "agent.ask", json!({"slide": 2, "shape": table.0, "text": "Why is West growing faster than North?"}));
    run(&mut s, "agent.ask", json!({"slide": 3, "text": "What do we need from legal before December?"}));
    let (q1, q2) = (q1["id"].as_str().unwrap().to_string(), q2["id"].as_str().unwrap().to_string());
    run(&mut s, "agent.claim", json!({"id": q1, "actor": "agent"}));
    run(
        &mut s,
        "agent.answer",
        json!({"id": q1, "actor": "agent", "text": "Yes: both quarters are management accounts, constant currency. Q2 restated for the West reclassification (+12k).", "links": [{"label": "Slide 2 table", "href": format!("shape:2/{}", table.0)}, {"label": "Finance pack", "href": "https://example.com/finance/q3"}]}),
    );
    run(&mut s, "agent.claim", json!({"id": q2, "actor": "agent"}));
    run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "kind": "faq", "actor": "agent", "title": "Are these numbers audited?", "text": "No: management accounts. The audit of H2 closes in February."}),
    );
    run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "kind": "faq", "actor": "agent", "title": "Why did North jump?", "text": "Two renewals moved from Q2 into Q3 (about 50k). Without them North is flat."}),
    );
    run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "shape": table.0, "kind": "source", "actor": "agent", "title": "From the Q3 model", "text": "Model!A1:C4, linked: the slide warns when the model changes."}),
    );
    run(
        &mut s,
        "agent.decision",
        json!({"actor": "agent", "text": "Show revenue, not EBITDA, on slide 2: the board asked for top line this quarter", "source": "CFO email, 3 Oct", "slide": 2}),
    );
    run(
        &mut s,
        "agent.exec",
        json!({"command": "slide.notes", "params": {"text": "Stress the renewals; North is flat underneath.", "index": 1}, "reason": "speaker notes for slide 2", "source": "q2 answer"}),
    );
    run(&mut s, "link.check", json!({"gridcraft": false}));
    run(&mut s, "slide.go", json!({"index": 1}));
    run(&mut s, "selection.select", json!({"ids": [growth]}));
    (s, table)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut pane, mut slide, mut hover, mut dark, mut presenter, mut agent_tab, mut collapsed) = (None, 2usize, false, false, false, false, false);
    let (mut status, mut mode, mut menu): (Option<String>, Option<String>, bool) = (None, None, false);
    let mut size = (1440.0f32, 900.0f32);
    let mut out = "agent.png".to_string();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pane" => pane = it.next(),
            "--slide" => slide = it.next().and_then(|v| v.parse().ok()).unwrap_or(2),
            "--hover-link" => hover = true,
            "--presenter" => presenter = true,
            "--agent-tab" => agent_tab = true,
            "--collapsed" => collapsed = true,
            "--status" => status = it.next(),
            "--mode" => mode = it.next(),
            "--menu" => menu = true,
            "--dark" => dark = true,
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|s| s.split_once('x').map(|(w, h)| (w.to_string(), h.to_string()))) {
                    size = (w.parse().unwrap_or(1440.0), h.parse().unwrap_or(900.0));
                }
            }
            other => out = other.to_string(),
        }
    }
    deckcraft_agent_ui::install();
    // Never the real AItoCraft registry, never a real agent from a screenshot run.
    deckcraft_agent::registry::set_dir(Some(std::env::temp_dir().join("deckcraft-agent-demo/agents")));
    deckcraft_agent::runner::set_config(deckcraft_agent::runner::Config {
        command: "deckcraft-demo-no-agent".into(),
        data_dir: Some(std::env::temp_dir().join("deckcraft-agent-demo/agent")),
        ..Default::default()
    });
    deckcraft_agent_ui::set_server_url("http://127.0.0.1:7984/mcp".into());
    deckcraft_agent_ui::set_stdio_entry(deckcraft_agent::cowork::entry(
        std::path::Path::new("/Applications/DeckCraft.app/Contents/MacOS/deckcraft"),
        7985,
    ));
    deckcraft_agent::cowork::set_config_path(Some(std::env::temp_dir().join("deckcraft-agent-demo/claude/claude_desktop_config.json")));
    let dir = std::env::temp_dir().join("deckcraft-agent-demo");
    std::fs::create_dir_all(&dir).unwrap();
    let (mut session, table) = demo(&dir);
    if let Some(m) = mode.as_deref().and_then(deckcraft_agent::listener::Mode::parse) {
        deckcraft_agent::listener::set_mode(m);
    }
    // The Connection status to show: the demo's agent calls count as "last call".
    let uid = session.doc().unwrap().uid;
    match status.as_deref() {
        Some("answering") => {
            let id = run(&mut session, "agent.ping", json!({}))["id"].as_str().unwrap().to_string();
            std::thread::sleep(std::time::Duration::from_millis(1200));
            run(&mut session, "agent.answer", json!({"id": id, "text": "here", "actor": "agent"}));
        }
        Some("checking") => {
            run(&mut session, "agent.ping", json!({}));
        }
        Some("noanswer") => {
            run(&mut session, "agent.ping", json!({}));
            let p = deckcraft_agent::listener::last_ping().unwrap();
            deckcraft_agent::listener::set_ping(Some(deckcraft_agent::listener::Ping { sent_at: p.sent_at.saturating_sub(95_000), ..p }));
        }
        Some("listener") => deckcraft_agent::listener::note_wait(),
        Some("listening") => {
            deckcraft_agent::listener::attach(uid, "Cowork · Acme board");
        }
        _ => {}
    }
    if slide != 2 {
        let _ = session.execute("slide.go", &json!({"index": slide.saturating_sub(1)}));
    }
    if presenter {
        // Presenter View's notes column, as the hook draws it.
        let doc = session.doc().unwrap().doc.clone();
        let uid = session.doc().unwrap().uid;
        let notes = doc.slides.get(slide.saturating_sub(1)).map(|s| s.notes_text()).unwrap_or_default();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(420.0, 520.0)).with_pixels_per_point(2.0).wgpu().build_ui(move |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_gray(24)).inner_margin(16)).show(ui, |ui| {
                ui.label(egui::RichText::new("Notes").size(13.0).color(egui::Color32::from_gray(170)));
                ui.label(egui::RichText::new(&notes).size(18.0).color(egui::Color32::from_gray(230)));
                deckcraft_agent_ui::presenter_preview(ui, &doc, slide.saturating_sub(1), uid);
            });
        });
        h.run_steps(4);
        h.render().expect("render").save(&out).expect("save");
        println!("{out}");
        return;
    }
    match &pane {
        Some(tab) => deckcraft_agent_ui::request_pane(true, tab),
        None => deckcraft_agent_ui::request_pane(false, "slide"),
    }
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(size.0, size.1)).with_pixels_per_point(2.0).with_max_steps(12).wgpu().build_ui_state(
            move |ui, app: &mut SlideApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            {
                let mut app = SlideApp::new(session, Services::default());
                if agent_tab {
                    app.ui.tab = "Agent".into();
                }
                app.ui.ribbon_collapsed = collapsed;
                app.ui.brightness = if dark { deckcraft_ui_egui::theme::Brightness::Dark } else { deckcraft_ui_egui::theme::Brightness::Light };
                app
            },
        );
    harness.run_steps(8);
    if menu {
        use egui_kittest::kittest::Queryable;
        let label = if deckcraft_agent::listener::mode() == deckcraft_agent::listener::Mode::Background { "Session" } else { "Copy" };
        harness.get_by_label(label).click();
        harness.run_steps(4);
    }
    if hover {
        let app = harness.state();
        let pos = app.session.active().and_then(|d| {
            let sh = d.shape(table)?;
            let x = deckcraft_engine::cmd::xfrm_of(&d.doc, &d.selection, sh);
            let r = app.slide_rect?;
            Some(egui::pos2(r.min.x + (x.x + x.w * 0.7) as f32 * app.canvas_scale, r.min.y + (x.y + x.h * 0.5) as f32 * app.canvas_scale))
        });
        if let Some(p) = pos {
            harness.hover_at(p);
            harness.run_steps(6);
        }
    }
    let img = harness.render().expect("render");
    img.save(&out).expect("save");
    println!("{out}");
}
