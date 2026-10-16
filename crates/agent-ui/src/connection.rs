//! The Connection group of the Agent tab: three things only, the same in GridCraft, WordCraft and
//! DeckCraft.
//!
//! - **Status**: is anyone listening? Green once the agent answered a check ("Is anyone there?"),
//!   amber on passive signs only (an `/events` stream, a channel, a `wait_for_questions` open, the
//!   background agent running), grey when nothing listens, red when a check got no answer. **Check**
//!   sends the check.
//! - **Mode**: how the agent comes in (I connect my agent, Send to my Claude Code session,
//!   Background agent, Off), one line of explanation each.
//! - **Copy**: what the mode needs, one thing per item; for the background agent, Open session in
//!   Claude Desktop and New session instead. URL, ports and errors are under Details.

use deckcraft_agent::cowork::{self, Configured};
use deckcraft_agent::listener::{self, Health, Mode};
use deckcraft_agent::{channel, runner, server};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::icons::Icon;
use deckcraft_ui_egui::theme::{self, Tokens};
use deckcraft_ui_egui::widgets::{dropdown, rows, small_button, small_button_ex};
use egui::{Color32, RichText, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use crate::ribbon::STDIO_ENTRY;
use crate::{AgentUi, ago, palette};

/// Seconds between looks at Claude Desktop's config.
const COWORK_EVERY: f64 = 4.0;

/// The status mark: a filled dot (answering), half (listener, checking), a ring (nobody), a cross
/// (no answer). Drawn, so it never depends on the font.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Full,
    Half,
    Ring,
    Cross,
}

fn mark(ui: &mut Ui, m: Mark, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
    let c = r.center();
    let p = ui.painter();
    match m {
        Mark::Full => {
            p.circle_filled(c, 5.0, color);
        }
        Mark::Ring => {
            p.circle_stroke(c, 4.5, Stroke::new(1.5, color));
        }
        Mark::Half => {
            p.circle_stroke(c, 4.5, Stroke::new(1.5, color));
            let pts: Vec<egui::Pos2> =
                (0..=16).map(|i| std::f32::consts::PI * (0.5 + i as f32 / 16.0)).map(|a| pos2(c.x + 5.0 * a.cos(), c.y - 5.0 * a.sin())).collect();
            p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Mark::Cross => {
            let s = Stroke::new(2.0, color);
            p.line_segment([pos2(c.x - 4.0, c.y - 4.0), pos2(c.x + 4.0, c.y + 4.0)], s);
            p.line_segment([pos2(c.x + 4.0, c.y - 4.0), pos2(c.x - 4.0, c.y + 4.0)], s);
        }
    }
}

/// Seconds as "12 s", "3 min".
fn secs(ms: u64) -> String {
    let now = deckcraft_agent::now_ms();
    let s = now.saturating_sub(ms) / 1000;
    if s < 60 { format!("{s} s") } else { format!("{} min", s / 60) }
}

/// What the status says for the active deck: mark, colour, line, sub-line.
fn status(uid: Option<u64>, t: &Tokens) -> (Mark, Color32, String, String) {
    let p = palette(t);
    let mode = listener::mode();
    if mode == Mode::Off {
        return (Mark::Ring, t.text_faint, "Agent off".into(), "Mode: Off: questions wait, the agent's calls are refused".into());
    }
    let sig = listener::signals(uid);
    let last = if sig.last_call > 0 { format!("last call {}", ago(sig.last_call)) } else { "no call yet".into() };
    let with = |s: &str| if s.is_empty() { last.clone() } else { format!("{s} · {last}") };
    match listener::health(uid) {
        Health::Answering { at, secs: took, by } => {
            let who = if by.is_empty() { "Agent answering".to_string() } else { format!("{by} answering") };
            (Mark::Full, p.answered, format!("{who} · checked {} ago", secs(at)), with(&format!("answered in {took} s")))
        }
        Health::Checking { since } => (Mark::Half, p.pending, format!("Checking… {}", secs(since)), with(&sig.describe())),
        Health::NoAnswer { since } => (
            Mark::Cross,
            p.broken,
            "No answer to check".into(),
            with(&format!("asked {} ago{}", secs(since), if sig.any() { format!(" · {}", sig.describe()) } else { String::new() })),
        ),
        Health::Listening { label, last_seen, more } => {
            let others = if more > 0 { format!(" (+{more})") } else { String::new() };
            (
                Mark::Full,
                p.answered,
                format!("Listening: {label}{others} · {} ago", secs(last_seen)),
                with("attached through AItoCraft · not checked"),
            )
        }
        Health::Listener(s) => (Mark::Half, p.pending, "Listener connected · not checked".into(), with(&s.describe())),
        Health::Nobody(_) => {
            let stopped = uid.and_then(runner::status).map(|s| s.last_error).filter(|e| !e.is_empty() && mode == Mode::Background);
            let hint = match mode {
                Mode::Desktop | Mode::Channel => "questions queue in the deck: Copy phrase for Claude".to_string(),
                Mode::Background => {
                    stopped.map(|e| format!("the agent stopped: {e}")).unwrap_or_else(|| "the agent starts with the next question".into())
                }
                Mode::Off => String::new(),
            };
            (Mark::Ring, t.text_faint, "Nobody is listening for this document".into(), with(&hint))
        }
    }
}

/// Where the AItoCraft hub is: `AITOCRAFT_BIN`, the PATH, `~/aitocraft/target/release`,
/// `~/.cargo/bin`.
pub(crate) fn find_aitocraft() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("AITOCRAFT_BIN").map(std::path::PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let name = if cfg!(windows) { "aitocraft.exe" } else { "aitocraft" };
    let in_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join(name));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let known = home.into_iter().flat_map(|h| [h.join("aitocraft/target/release").join(name), h.join(".cargo/bin").join(name)]);
    in_path.chain(known).find(|p| p.is_file())
}

/// What the person pastes into the conversation that should work on this deck.
pub(crate) fn phrase(title: &str) -> String {
    format!("Work with me on \"{title}\" in DeckCraft through AItoCraft: attach to it and answer my questions from the document as they come.")
}

/// The app's executable and control port, from the Cowork entry the app set.
fn exe_and_control() -> Option<(String, u16)> {
    let e = STDIO_ENTRY.get()?;
    let exe = e.get("command")?.as_str()?.to_string();
    let control =
        e.get("args")?.as_array()?.iter().filter_map(|a| a.as_str()).find_map(|a| a.strip_prefix("127.0.0.1:")).and_then(|p| p.parse().ok())?;
    Some((exe, control))
}

/// A menu item that copies one thing.
fn copy_item(ui: &mut Ui, app: &mut SlideApp, label: &str, text: Option<String>, what: &str) {
    let r = ui.add_enabled(text.is_some(), egui::Button::new(label).frame(false)).on_hover_text(text.clone().unwrap_or_default());
    if r.clicked()
        && let Some(t) = text
    {
        ui.ctx().copy_text(t);
        app.set_status(format!("Copied: {what}"));
        ui.close();
    }
}

pub(crate) fn group(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let uid = app.session.active().map(|d| d.uid);
    let mode = listener::mode();
    let now = ui.input(|i| i.time);
    let srv = server::status();
    let url = crate::server_url().map(str::to_string).or_else(|| srv.port.map(|p| format!("http://127.0.0.1:{p}/mcp")));
    let base = url.as_deref().map(|u| u.trim_end_matches("/mcp").to_string());
    let entry = STDIO_ENTRY.get().cloned();
    let cowork_state = cowork::state();
    if st.ribbon.looked_at.is_none_or(|at| now - at > COWORK_EVERY) {
        st.ribbon.looked_at = Some(now);
        st.ribbon.configured = match (&entry, cowork::config_path()) {
            (Some(e), Some(path)) => Some(cowork::configured(&path, e)),
            _ => None,
        };
        st.ribbon.claude_running = matches!(cowork_state, cowork::State::Done { .. }) && cowork::claude_running();
    }
    let (m, color, line, sub) = status(uid, &t);
    // The status changes by itself (seconds counting, the check timing out).
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    let details = {
        let mut d = vec![];
        if let Some(u) = &url {
            d.push(format!("MCP: {u}"));
        }
        if let Some((_, c)) = exe_and_control() {
            d.push(format!("Control channel: 127.0.0.1:{c}"));
        }
        if srv.monitors > 0 {
            d.push(format!("/events streams: {}", srv.monitors));
        }
        if !srv.client.is_empty() {
            d.push(format!("Last MCP client: {}", srv.client));
        }
        d.join("\n")
    };

    deckcraft_ui_egui::widgets::group(ui, |ui| {
        // Status, with Check.
        rows(ui, |ui| {
            // A fixed width: a long label or error must not push the mode list off the ribbon.
            ui.set_width(310.0);
            ui.horizontal(|ui| {
                ui.set_height(22.0);
                mark(ui, m, color);
                let tip = if details.is_empty() { line.clone() } else { format!("{line}\n\n{details}") };
                ui.add(egui::Label::new(RichText::new(&line).font(theme::font(13.0)).color(t.text)).selectable(false).truncate()).on_hover_text(tip);
            });
            ui.horizontal(|ui| {
                ui.set_height(18.0);
                ui.add_space(16.0);
                ui.add(egui::Label::new(RichText::new(&sub).size(11.0).color(t.text_dim)).selectable(false).truncate()).on_hover_text(&sub);
            });
            let can_check = mode != Mode::Off && uid.is_some();
            if small_button(ui, Icon::Redo, "Check", can_check, false)
                .on_hover_text("Ask the agent \"Is anyone there?\": it answers \"here\" within a minute when it listens")
                .clicked()
            {
                let _ = app.run("agent.ping", json!({}));
            }
        });
        // Mode and Copy.
        rows(ui, |ui| {
            ui.add_space(2.0);
            let r = dropdown(ui, "agent_mode", mode.label(), 270.0, true, |ui| {
                ui.set_min_width(300.0);
                for item in Mode::ALL {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(item.label(), 0.0, egui::TextFormat { font_id: theme::font(12.5), color: t.text, ..Default::default() });
                    job.append(
                        &format!("\n{}", item.explanation()),
                        0.0,
                        egui::TextFormat { font_id: theme::font(10.5), color: t.text_dim, ..Default::default() },
                    );
                    if ui.add(egui::Button::selectable(item == mode, job)).clicked() {
                        if app.run("agent.agentMode", json!({"mode": item.id()})).is_ok() {
                            crate::remember_mode(item);
                        }
                        ui.close();
                    }
                }
            });
            let _ = r.on_hover_text(mode.explanation());
            ui.add_space(4.0);
            let label = if mode == Mode::Background { "Session" } else { "Copy" };
            let (main, arrow) = small_button_ex(ui, if mode == Mode::Background { Icon::Recent } else { Icon::Copy }, label, true, false, true);
            let main = main.on_hover_text(match mode {
                Mode::Desktop => "AItoCraft: the phrase for your conversation, and the one-time setup",
                Mode::Channel => "AItoCraft as a channel of your Claude Code session",
                Mode::Background => "The background agent's session",
                Mode::Off => "Details",
            });
            let title = app.session.active().map(deckcraft_agent::documents::title).unwrap_or_default();
            let hub = find_aitocraft();
            let hub_cmd = hub.as_ref().map(|h| h.display().to_string()).unwrap_or_else(|| "aitocraft".into());
            let q = |s: &str| if s.contains(' ') { format!("'{}'", s.replace('\'', "'\\''")) } else { s.to_string() };
            let mut menu = |ui: &mut Ui, app: &mut SlideApp| {
                ui.set_min_width(280.0);
                match mode {
                    Mode::Desktop | Mode::Channel => {
                        copy_item(
                            ui,
                            app,
                            "Copy phrase for Claude",
                            Some(phrase(&title)),
                            "the phrase: paste it into the conversation that should work on this deck",
                        );
                        let tip = match &hub {
                            Some(h) => format!("Runs {} --install-desktop: adds AItoCraft to Claude Desktop once, for every Craft app", h.display()),
                            None => "AItoCraft isn't installed here (build ~/aitocraft, or set AITOCRAFT_BIN)".to_string(),
                        };
                        if ui
                            .add_enabled(hub.is_some(), egui::Button::new("Install AItoCraft in Claude Desktop").frame(false))
                            .on_hover_text(tip)
                            .clicked()
                            && let Some(h) = hub.clone()
                        {
                            match std::process::Command::new(&h).arg("--install-desktop").output() {
                                Ok(o) if o.status.success() => app.set_status(format!(
                                    "AItoCraft is in Claude Desktop. {}",
                                    String::from_utf8_lossy(&o.stdout)
                                        .lines()
                                        .last()
                                        .unwrap_or("Quit Claude (⌘Q) and open it again if it was running.")
                                )),
                                Ok(o) => app.set_status(format!(
                                    "AItoCraft couldn't install: {}",
                                    String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("error")
                                )),
                                Err(e) => app.set_status(format!("Couldn't run AItoCraft: {e}")),
                            }
                            ui.close();
                        }
                        copy_item(
                            ui,
                            app,
                            "Claude Code: add AItoCraft",
                            Some(format!("claude mcp add --scope user aitocraft -- {}", q(&hub_cmd))),
                            "the Claude Code command (once, for every Craft app)",
                        );
                        if mode == Mode::Channel {
                            copy_item(
                                ui,
                                app,
                                "Claude Code: start with the channel",
                                Some("claude --dangerously-load-development-channels server:aitocraft".into()),
                                "the command that starts Claude Code with AItoCraft as a channel",
                            );
                        }
                    }
                    Mode::Background => {
                        if ui
                            .button("Open session in Claude Desktop")
                            .on_hover_text("Claude Desktop ▸ Code: the session named “DeckCraft · <file>”")
                            .clicked()
                        {
                            if let Err(e) = runner::open_claude_desktop() {
                                app.set_status(format!("Couldn't open Claude Desktop: {e}"));
                            }
                            ui.close();
                        }
                        if ui.button("New session").on_hover_text("A fresh session for this deck, started from a summary of its history").clicked() {
                            let _ = app.run("agent.agentReset", json!({}));
                            ui.close();
                        }
                        let mut c = runner::config();
                        let mut deck = c.use_deck_folder;
                        if ui
                            .checkbox(&mut deck, "Use the deck's folder as its project")
                            .on_hover_text(
                                "Its CLAUDE.md, memory and settings become the agent's context. Off: a neutral folder, ArtCraft/agent-sessions",
                            )
                            .changed()
                        {
                            c.use_deck_folder = deck;
                            runner::set_config(c);
                        }
                    }
                    Mode::Off => {}
                }
                ui.separator();
                ui.menu_button("Details", |ui| {
                    ui.set_min_width(320.0);
                    ui.menu_button("Direct connection (without AItoCraft)", |ui| {
                        ui.set_min_width(300.0);
                        copy_item(ui, app, "Claude Code: add DeckCraft (HTTP)", url.as_ref().map(|u| format!("claude mcp add --transport http deckcraft {u}")), "the Claude Code command");
                        copy_item(ui, app, "Monitor command", base.as_ref().map(|b| format!("curl -sN {b}/events")), "the monitor command");
                        let cmds = exe_and_control().map(|(exe, c)| channel::commands(&exe, c, srv.port.unwrap_or(server::DEFAULT_PORT)));
                        copy_item(ui, app, "Channel: add deckcraft --agent-stdio", cmds.as_ref().map(|c| c.0.clone()), "the command that adds DeckCraft's own channel server");
                        copy_item(ui, app, "Channel: start Claude Code with it", cmds.as_ref().map(|c| c.1.clone()), "the command that starts Claude Code with DeckCraft's channel");
                        let (sub, can) = match (&cowork_state, &st.ribbon.configured) {
                            (cowork::State::Waiting, _) => ("waiting: quit Claude (⌘Q)", false),
                            (cowork::State::Done { .. }, _) if !st.ribbon.claude_running => ("added: open Claude", true),
                            (cowork::State::Failed(_), _) => ("failed: try again", true),
                            (_, Some(Configured::Yes)) => ("connected", true),
                            (_, Some(Configured::Different)) => ("another build: point it here", true),
                            _ => ("not set up", true),
                        };
                        let r = ui.add_enabled(can && entry.is_some(), egui::Button::new(format!("Connect Cowork…  ({sub})")).frame(false));
                        if r.on_hover_text("Adds DeckCraft to Claude Desktop's local MCP servers (other servers are kept, the file is backed up); while Claude runs, it waits until you quit it").clicked()
                            && let Some(e) = entry.clone()
                        {
                            cowork::connect(e);
                            st.ribbon.looked_at = None;
                            match cowork::state() {
                                cowork::State::Waiting => app.set_status("Quit Claude with ⌘Q: DeckCraft adds itself to Claude's config as soon as Claude has quit, then open Claude again"),
                                cowork::State::Done { .. } => app.set_status("Added DeckCraft to Claude Desktop: open Claude, it loads DeckCraft when it starts"),
                                cowork::State::Failed(e) => app.set_status(format!("Couldn't connect Cowork: {e}")),
                                cowork::State::Idle => {}
                            }
                            ui.close();
                        }
                    });
                    ui.separator();
                    for l in details.lines() {
                        ui.label(RichText::new(l).size(11.5));
                    }
                    if let Some(d) = app.session.active() {
                        let session = deckcraft_agent::layer::get(d).agent_session.clone();
                        let rs = runner::status_json(d.uid, &session);
                        if mode == Mode::Background || !session.is_empty() {
                            ui.separator();
                            ui.label(
                                RichText::new(format!("Background agent: {}", if rs["running"] == true { "running" } else { "not running" }))
                                    .size(11.5),
                            );
                            for (k, label) in [("sessionId", "Session"), ("cwd", "Folder"), ("lastError", "Last error")] {
                                if let Some(v) = rs[k].as_str().filter(|v| !v.is_empty()) {
                                    ui.label(RichText::new(format!("{label}: {v}")).size(11.5));
                                }
                            }
                            if let Some(c) = rs["costUsd"].as_f64().filter(|c| *c > 0.0) {
                                ui.label(RichText::new(format!("Cost so far: ${c:.2}")).size(11.5));
                            }
                            if let Some(cmd) = rs["resumeCommand"].as_str() {
                                copy_item(ui, app, "Copy resume command", Some(cmd.to_string()), "the resume command");
                            }
                        }
                    }
                    if let cowork::State::Failed(e) = &cowork_state {
                        ui.separator();
                        ui.label(RichText::new(format!("Cowork: {e}")).size(11.5));
                    }
                });
            };
            egui::Popup::menu(&main).show(|ui| menu(ui, app));
            if let Some(a) = arrow {
                egui::Popup::menu(&a).show(|ui| menu(ui, app));
            }
        });
    });
}
