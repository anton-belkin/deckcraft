//! Offscreen render of DeckCraft with the agent layer, for looking at the UI without a window
//! (and for the README's screenshots). Builds a demo deck in code: a client's name marked as client
//! data, topics in every status (one the agent is working on, one with an unread answer, one the
//! agent suggests agreeing, one whose shape was deleted), prepared answers, and a table linked to a
//! workbook whose numbers have since changed.
//!
//! `cargo run -p deckcraft-agent-ui --example agent_snapshot -- [--pane selection|overview|history|topic:N]
//!  [--slide N] [--hover-link] [--presenter] [--agent-tab] [--collapsed]
//!  [--context none|web|cowork|code|folder] [--status listening|checking] [--logged-in] [--menu steps|modes] [--size 1440x900] [--dark] out.png`
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
    // A date on slide 3 that gets a question and is then deleted: its place reads "removed — was …".
    let date = run(&mut s, "insert.textBox", json!({"rect": [520, 420, 360, 40], "text": "Legal sign-off: 28 November"}))["id"].as_u64().unwrap();
    // The layer: client data, topics in every state, prepared answers, decisions.
    let cover = title[1].0;
    run(&mut s, "agent.private", json!({"slide": 1, "shape": cover, "words": "Acme Holdings AG"}));
    // Slide 1: the title, agreed by the presenter.
    let t_cover = run(&mut s, "agent.ask", json!({"slide": 1, "shape": title[0].0, "text": "Is “Q3 Board Review” the title the board pack uses?"}));
    let q0 = t_cover["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.claim", json!({"id": q0, "actor": "agent"}));
    run(&mut s, "agent.answer", json!({"id": q0, "actor": "agent", "text": "Yes: the pack and the invitation both say “Q3 Board Review”."}));
    run(&mut s, "agent.seen", json!({"id": q0}));
    run(&mut s, "agent.setStatus", json!({"topic": t_cover["topicId"], "status": "agreed", "text": "matches the invitation"}));
    // Slide 2: the growth figure (answered, unread), with the table as a second place.
    let q1 =
        run(&mut s, "agent.ask", json!({"slide": 2, "shape": growth, "words": "6%", "text": "Is 6% on the same basis as last quarter's figure?"}));
    let q2 = run(&mut s, "agent.ask", json!({"slide": 2, "shape": table.0, "text": "Why is West growing faster than North?", "new": true}));
    let t1 = q1["topicId"].as_str().unwrap().to_string();
    let (q1, q2) = (q1["id"].as_str().unwrap().to_string(), q2["id"].as_str().unwrap().to_string());
    run(&mut s, "agent.claim", json!({"id": q1, "actor": "agent"}));
    run(
        &mut s,
        "agent.answer",
        json!({"id": q1, "actor": "agent", "text": format!("Yes: both quarters are management accounts at constant currency; Q2 was restated for the West reclassification (+12k). The 6% comes from [the table's totals](shape:2/{}), and slide 3 assumes it.", table.0), "links": [{"label": "Finance pack", "href": "https://example.com/finance/q3"}]}),
    );
    run(
        &mut s,
        "agent.addPlaces",
        json!({"topic": t1, "actor": "agent", "places": [{"slide": 2, "shape": table.0, "reason": "the 6% is the change in this table's Q3 total over Q2"}]}),
    );
    run(
        &mut s,
        "agent.note",
        json!({"topic": t1, "kind": "source", "actor": "agent", "title": "Q3 management accounts", "text": "Constant currency, restated Q2.", "url": "https://example.com/finance/q3", "summary": "Finance pack, 14 Oct: management accounts for Q2 and Q3."}),
    );
    // The table: the agent is working on it.
    run(&mut s, "agent.claim", json!({"id": q2, "actor": "agent"}));
    // Slide 2's prepared answers: for information.
    let faq = run(
        &mut s,
        "agent.note",
        json!({"slide": 2, "kind": "faq", "actor": "agent", "title": "Are these numbers audited?", "text": "No: management accounts. The audit of H2 closes in February."}),
    );
    run(
        &mut s,
        "agent.note",
        json!({"topic": faq["topicId"], "kind": "faq", "actor": "agent", "title": "Why did North jump?", "text": "Two renewals moved from Q2 into Q3 (about 50k). Without them North is flat."}),
    );
    // Slide 2's title: the agent suggests Agreed.
    let title2 = s.doc().unwrap().doc.slides[1].shapes[0].id.0;
    let q3 = run(&mut s, "agent.ask", json!({"slide": 2, "shape": title2, "text": "Revenue or net revenue?"}));
    let q3id = q3["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.claim", json!({"id": q3id, "actor": "agent"}));
    run(
        &mut s,
        "agent.answer",
        json!({"id": q3id, "actor": "agent", "text": "Revenue: the model has no deductions in Q2 or Q3, so the two are the same."}),
    );
    run(&mut s, "agent.seen", json!({"id": q3id}));
    run(
        &mut s,
        "agent.proposeAgreed",
        json!({"topic": q3["topicId"], "actor": "agent", "reasons": "Revenue equals net revenue here (no deductions in the model); the board asked for top line."}),
    );
    // Slide 3: a question waiting, a revise, and the date that is about to go.
    run(&mut s, "agent.ask", json!({"slide": 3, "text": "What do we need from legal before December?"}));
    let body = s.doc().unwrap().doc.slides[2].shapes.get(1).map(|x| x.id.0).unwrap();
    let q5 = run(&mut s, "agent.ask", json!({"slide": 3, "shape": body, "text": "Is January realistic for the kick-off?"}));
    run(
        &mut s,
        "agent.setStatus",
        json!({"topic": q5["topicId"], "status": "revise", "text": "the kick-off moves to February: see slide 2 for the renewals"}),
    );
    let q6 = run(&mut s, "agent.ask", json!({"slide": 3, "shape": date, "text": "Is 28 November confirmed with legal?"}));
    let q6id = q6["id"].as_str().unwrap().to_string();
    run(&mut s, "agent.claim", json!({"id": q6id, "actor": "agent"}));
    run(&mut s, "agent.answer", json!({"id": q6id, "actor": "agent", "text": "Not yet: legal confirms after the renewal is signed."}));
    run(&mut s, "agent.seen", json!({"id": q6id}));
    run(&mut s, "slide.go", json!({"index": 2}));
    run(&mut s, "selection.select", json!({"ids": [date]}));
    run(&mut s, "edit.delete", json!({}));
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
    let (mut status, mut context, mut menu, mut logged_in): (Option<String>, Option<String>, Option<String>, bool) = (None, None, None, false);
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
            "--context" => context = it.next(),
            "--menu" => menu = it.next(),
            "--logged-in" => logged_in = true,
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
    // What the setup looks like (never looked at for real from a screenshot run).
    deckcraft_agent::context::set_setup(deckcraft_agent::context::Setup {
        logged_in: Some(logged_in),
        code_has_hub: Some(true),
        desktop_has_hub: Some(true),
    });
    let ctx = match context.as_deref() {
        Some("none") => json!({"kind": "none"}),
        Some("web") => json!({"kind": "none", "web": true}),
        Some("code") => json!({"kind": "code", "code": "session"}),
        Some("folder") => json!({"kind": "code", "code": "folder", "folder": "/Users/anton/Clients/Acme board"}),
        _ => json!({"kind": "cowork"}),
    };
    run(&mut session, "agent.contextMode", ctx);
    deckcraft_agent::context::apply(&session);
    let uid = session.doc().unwrap().uid;
    match status.as_deref() {
        Some("checking") => {
            run(&mut session, "agent.ping", json!({}));
        }
        Some("listening") => {
            deckcraft_agent::listener::attach(uid, "Cowork · Acme board");
        }
        _ => {}
    }
    if slide != 2 {
        let _ = session.execute("slide.go", &json!({"index": slide.saturating_sub(1)}));
    }
    if presenter {
        // Presenter View as the hooks draw it: the slide with its topic markers (the presenter
        // turned them on), and the notes column with the toggle and the prepared answers.
        let doc = session.doc().unwrap().doc.clone();
        let uid = session.doc().unwrap().uid;
        let index = slide.saturating_sub(1);
        let notes = doc.slides.get(index).map(|s| s.notes_text()).unwrap_or_default();
        let w = 1280u32;
        let h = (w as f64 * doc.slide_size.height / doc.slide_size.width.max(1.0)) as u32;
        let img = deckcraft_render::render_slide(
            &doc,
            index,
            &deckcraft_render::RenderOpts { scale: w as f64 / doc.slide_size.width.max(1.0), size: Some((w, h)), ..Default::default() },
        );
        let picture = deckcraft_ui_egui::textures::to_color_image(&img, false);
        let mut tex: Option<egui::TextureHandle> = None;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1080.0, 440.0)).with_pixels_per_point(2.0).wgpu().build_ui(move |ui| {
            // The theme's fonts first (they take effect from the next frame).
            if tex.is_none() {
                deckcraft_ui_egui::theme::install_fonts(ui.ctx());
                tex = Some(ui.ctx().load_texture("slide", picture.clone(), Default::default()));
                return;
            }
            let Some(tex) = tex.clone() else { return };
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_gray(24)).inner_margin(16)).show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let size = egui::vec2(640.0, 640.0 * tex.size()[1] as f32 / tex.size()[0].max(1) as f32);
                    let r = ui.add(egui::Image::new((tex.id(), size))).rect;
                    deckcraft_agent_ui::presenter_slide_preview(ui, r, &doc, index, uid);
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(360.0);
                        ui.label(egui::RichText::new("Notes").size(13.0).color(egui::Color32::from_gray(170)));
                        ui.label(egui::RichText::new(&notes).size(18.0).color(egui::Color32::from_gray(230)));
                        deckcraft_agent_ui::presenter_preview(ui, &doc, index, uid);
                    });
                });
            });
        });
        h.run_steps(1);
        h.run_steps(4);
        h.render().expect("render").save(&out).expect("save");
        println!("{out}");
        return;
    }
    match &pane {
        Some(tab) => deckcraft_agent_ui::request_pane(true, tab),
        None => deckcraft_agent_ui::request_pane(false, "selection"),
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
    if let Some(m) = menu.as_deref() {
        use egui_kittest::kittest::Queryable;
        let label = match m {
            "modes" => {
                ["No context", "Claude Cowork", "Claude Code"].into_iter().find(|l| harness.query_by_label(l).is_some()).unwrap_or("Claude Cowork")
            }
            _ => "Get ready steps",
        };
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
