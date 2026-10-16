//! The Agent tab on the ribbon: what isn't about one piece of content. Four groups, named the same
//! in GridCraft, WordCraft and DeckCraft:
//!
//! - **Agent**: the pane, with what is waiting (answers ready, questions with the agent);
//! - **Client data**: mark or unmark the selection;
//! - **Connection**: the agent server, whether an agent is connected, Copy MCP Command, Connect
//!   Cowork (Claude Desktop's local servers);
//! - **Links**: the deck's links to spreadsheet cells: how many are out of date, Check Now,
//!   Update Stale.

use deckcraft_agent::layer::{self, QuestionStatus};
use deckcraft_agent::{cowork, links};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::icons::Icon;
use deckcraft_ui_egui::theme::Tokens;
use deckcraft_ui_egui::widgets::{big_button_ex, group, rows, small_button};
use egui::{Color32, RichText, Ui};
use serde_json::json;

use crate::{AgentUi, Tab, ago, palette, selection_private};

/// The tab's name.
pub const TAB: &str = "Agent";

/// Seconds between looks at Claude Desktop's config.
const COWORK_EVERY: f64 = 5.0;

#[derive(Default)]
pub(crate) struct State {
    /// Claude Desktop's config has DeckCraft (`Some(false)`: another binary), when last looked.
    cowork: Option<Option<bool>>,
    cowork_at: f64,
    /// Connected in this session: Claude must be restarted to see it.
    pub cowork_restart: bool,
}

fn status_line(ui: &mut Ui, color: Color32, text: &str, tip: &str) -> egui::Response {
    ui.horizontal(|ui| {
        ui.set_height(20.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 20.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 4.0, color);
        ui.add(egui::Label::new(RichText::new(text).size(11.5)).selectable(false).sense(egui::Sense::click()))
    })
    .inner
    .on_hover_text(tip)
}

fn exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "deckcraft".into())
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);
    let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
    let waiting = l.questions.iter().filter(|q| q.status == QuestionStatus::Pending).count();
    let working = l.questions.iter().filter(|q| q.status == QuestionStatus::Working).count();
    let all_links = links::all(&d.doc);
    let checks: Vec<Option<links::Check>> = all_links.iter().map(|(_, _, k)| links::last_check(&k.id)).collect();
    let stale: Vec<u32> = all_links
        .iter()
        .zip(&checks)
        .filter(|(_, c)| c.as_ref().is_some_and(|c| c.state == links::State::Stale))
        .map(|((_, id, _), _)| id.0)
        .collect();
    let broken = checks.iter().filter(|c| c.as_ref().is_some_and(|c| matches!(c.state, links::State::Missing | links::State::Error))).count();
    let marks = l.private.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };

    // Agent: the pane and what is waiting.
    group(ui, |ui| {
        if big_button_ex(ui, Icon::Sparkle, "Agent\nPane", true, false, st.open)
            .0
            .on_hover_text("Questions, answers and notes on this slide")
            .clicked()
        {
            st.open = !st.open;
        }
        rows(ui, |ui| {
            let (color, text) = if ready > 0 {
                (p.answered, format!("{ready} answer{} ready", plural(ready)))
            } else if working > 0 {
                (p.working, format!("Agent working on {working} question{}", plural(working)))
            } else if waiting > 0 {
                (p.pending, format!("{waiting} question{} waiting", plural(waiting)))
            } else {
                (t.text_faint, "No open questions".to_string())
            };
            if status_line(ui, color, &text, "Open the questions").clicked() {
                st.open = true;
                st.tab = Tab::Questions;
            }
            if small_button(ui, Icon::CommentsBubble, "Questions", true, st.open && st.tab == Tab::Questions).clicked() {
                st.open = true;
                st.tab = Tab::Questions;
            }
            if small_button(ui, Icon::Recent, "History", true, st.open && st.tab == Tab::History).clicked() {
                st.open = true;
                st.tab = Tab::History;
            }
        });
    });

    // Client data: mark or unmark the selection.
    group(ui, |ui| {
        let sel = selection_private(app);
        let can_mark = sel.is_some_and(|(_, _, p)| !p);
        let can_unmark = sel.is_some_and(|(_, _, p)| p);
        let tip = "The agent sees pseudonyms like [client-1] for client data, wherever it appears in the deck, and can't change it";
        if big_button_ex(ui, Icon::Lock, "Mark Client\nData", can_mark, false, can_unmark).0.on_hover_text(tip).clicked()
            && let Some((s, id, _)) = sel
        {
            let _ = app.run("agent.private", json!({"slide": s + 1, "shape": id.0, "on": true}));
        }
        if big_button_ex(ui, Icon::Unlock, "Unmark", can_unmark, false, false).0.on_hover_text("Only a person can lift the mark").clicked()
            && let Some((s, id, _)) = sel
        {
            let _ = app.run("agent.private", json!({"slide": s + 1, "shape": id.0, "on": false}));
        }
        rows(ui, |ui| {
            let text = if marks == 0 { "Nothing marked".to_string() } else { format!("{marks} mark{} in this deck", plural(marks)) };
            ui.add_space(4.0);
            ui.label(RichText::new(text).size(11.5).color(t.text_dim));
        });
    });

    // Connection: the server, the agent, Claude Code and Cowork.
    let now = ui.input(|i| i.time);
    if st.ribbon.cowork.is_none() || now - st.ribbon.cowork_at > COWORK_EVERY {
        st.ribbon.cowork = Some(cowork::config_path().and_then(|path| cowork::configured(&path, &exe())));
        st.ribbon.cowork_at = now;
    }
    group(ui, |ui| {
        let status = deckcraft_agent::server::status();
        let url = crate::server_url().map(str::to_string).or_else(|| status.port.map(|p| format!("http://127.0.0.1:{p}/mcp")));
        rows(ui, |ui| {
            match &url {
                Some(u) => {
                    status_line(ui, p.answered, &format!("Listening on {}", u.trim_start_matches("http://")), "The agent server inside DeckCraft")
                }
                None => status_line(ui, p.broken, "Agent server off", "Start DeckCraft without --no-agent"),
            };
            let fresh = status.last_call > 0 && deckcraft_agent::now_ms().saturating_sub(status.last_call) < 5 * 60 * 1000;
            let who = if status.client.is_empty() { "An agent".to_string() } else { status.client.clone() };
            if status.monitors > 0 {
                status_line(ui, p.answered, &format!("{who} is watching for questions"), "An agent follows /events");
            } else if fresh {
                status_line(ui, p.answered, &format!("{who} connected, {}", ago(status.last_call)), "Last call from an agent");
            } else if status.last_call > 0 {
                status_line(ui, t.text_faint, &format!("{who} last seen {}", ago(status.last_call)), "No call from an agent for a while");
            } else {
                status_line(ui, t.text_faint, "No agent connected yet", "Connect Claude Code (Copy MCP Command) or Cowork");
            }
            let (color, text, tip) = match (st.ribbon.cowork_restart, st.ribbon.cowork.flatten()) {
                (true, _) => (p.pending, "Cowork: quit Claude (⌘Q), reopen", "Claude Desktop reads its config only when it starts"),
                (false, Some(true)) => (p.answered, "Cowork: connected", "DeckCraft is in Claude Desktop's local MCP servers"),
                (false, Some(false)) => {
                    (p.pending, "Cowork: another DeckCraft", "Claude Desktop starts a different DeckCraft binary: Connect Cowork points it here")
                }
                (false, None) => (t.text_faint, "Cowork: not set up", "Connect Cowork adds DeckCraft to Claude Desktop"),
            };
            status_line(ui, color, text, tip);
        });
        let cmd = url.as_ref().map(|u| format!("claude mcp add --transport http deckcraft {u}"));
        if big_button_ex(ui, Icon::Copy, "Copy MCP\nCommand", cmd.is_some(), false, false).0.on_hover_text(cmd.clone().unwrap_or_default()).clicked()
            && let Some(c) = cmd
        {
            ui.ctx().copy_text(c);
            app.set_status("Copied: paste it in a terminal to connect Claude Code");
        }
        let port = status.port.unwrap_or(deckcraft_agent::server::DEFAULT_PORT);
        if big_button_ex(ui, Icon::Link, "Connect\nCowork", true, false, st.ribbon.cowork.flatten() == Some(true))
            .0
            .on_hover_text("Add DeckCraft to Claude Desktop's local MCP servers (other servers are kept; the file is backed up)")
            .clicked()
        {
            match cowork::config_path().ok_or_else(|| "no home folder".to_string()).and_then(|path| cowork::connect(&path, &exe(), port)) {
                Ok(c) if c.unchanged => {
                    app.set_status("DeckCraft is already in Claude Desktop. If Cowork doesn't list it, quit Claude (⌘Q) and reopen it")
                }
                Ok(_) => {
                    st.ribbon.cowork_restart = true;
                    st.ribbon.cowork = None;
                    app.set_status("Added DeckCraft to Claude Desktop. Quit Claude with ⌘Q and reopen it: it reads the config only when it starts");
                }
                Err(e) => app.set_status(format!("Couldn't connect Cowork: {e}")),
            }
        }
    });

    // Links: the deck's links to spreadsheet cells.
    group(ui, |ui| {
        let n = all_links.len();
        if big_button_ex(ui, Icon::Table, "Linked\nCells", n > 0, false, st.open && st.tab == Tab::Links)
            .0
            .on_hover_text("Tables and pictures linked to spreadsheet cells")
            .clicked()
        {
            st.open = true;
            st.tab = Tab::Links;
        }
        rows(ui, |ui| {
            let (color, text) = if n == 0 {
                (t.text_faint, "No linked cells".to_string())
            } else if !stale.is_empty() || broken > 0 {
                let bad = stale.len() + broken;
                (if broken > 0 { p.broken } else { p.stale }, format!("{bad} of {n} out of date"))
            } else {
                (p.linked, format!("{n} link{}, up to date", plural(n)))
            };
            status_line(ui, color, &text, "Checked every 30 s against the workbooks (or a running GridCraft)");
            if small_button(ui, Icon::Redo, "Check Now", n > 0, false).clicked() {
                st.checker.now = true;
            }
            if small_button(ui, Icon::Check, "Update Stale", !stale.is_empty(), false)
                .on_hover_text("Bring the new values into every out-of-date link")
                .clicked()
            {
                let at = app.session.active().map(|d| d.selection.slide);
                for id in &stale {
                    let _ = app.run("link.update", json!({"id": id}));
                }
                if let Some(i) = at {
                    let _ = app.run("slide.go", json!({"index": i}));
                }
            }
        });
    });
}
