//! The Context group of the Agent tab (the brief shared by GridCraft, WordCraft and DeckCraft):
//! what does the agent know.
//!
//! - **Left**: a large button with the document's mode and a status dot. Its menu has three rows,
//!   each with the name, one line and chips for what the agent knows: No context [Document],
//!   Claude Cowork [Document][Your chat], Claude Code [Document][Project folder].
//! - **Right**, by mode: No context has **Web** (the background client may also search and fetch
//!   the web); Claude Code has **My session | Project folder**. Then the **Ready** lamp (green
//!   Ready, amber Waiting after a Get ready step until a listener attaches, grey Not ready) and
//!   **Get ready ▾**, whose main click runs the next step not yet done and whose list ticks the
//!   done ones.
//!
//! The mode is the document's (saved in its layer, `agent.contextMode`, undoable); a new document
//! starts with the last mode used. The app checks readiness by itself, pinging quietly when a
//! listener shows signs of life but hasn't answered yet; the check is never shown.

use std::collections::{HashMap, HashSet};

use deckcraft_agent::context::{self, Code, Context, Kind, Setup};
use deckcraft_agent::cowork::{self, Configured};
use deckcraft_agent::listener::{self, Health, Mode};
use deckcraft_agent::{channel, runner, server};
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::icons::{self, Icon};
use deckcraft_ui_egui::theme::{self, Tokens};
use deckcraft_ui_egui::widgets::{rows, small_button_ex};
use egui::{Color32, RichText, Sense, Ui, pos2, vec2};
use serde_json::json;

use crate::ribbon::{STDIO_ENTRY, status_button};
use crate::{AgentUi, palette};

/// Seconds between looks at Claude Desktop's config (Details ▸ Connect Cowork).
const COWORK_EVERY: f64 = 4.0;
/// Amber after a Get ready step for at most this long (seconds).
const WAITING_FOR: f64 = 600.0;
/// A quiet check at most this often (seconds).
const PING_EVERY: f64 = 120.0;
/// While waiting, the setup is looked at again this often (seconds).
const LOOK_EVERY: f64 = 5.0;

/// The group's own state.
#[derive(Default)]
pub(crate) struct State {
    /// Documents waiting after a Get ready step, since when (UI time).
    waiting: HashMap<u64, f64>,
    /// Copy steps done, by document.
    copied: HashSet<(u64, Step)>,
    last_ping: Option<f64>,
    last_look: Option<f64>,
}

/// The Ready lamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lamp {
    Ready,
    Waiting,
    NotReady,
}

impl Lamp {
    pub fn label(self) -> &'static str {
        match self {
            Lamp::Ready => "Ready",
            Lamp::Waiting => "Waiting",
            Lamp::NotReady => "Not ready",
        }
    }
    fn color(self, t: &Tokens) -> Color32 {
        let p = palette(t);
        match self {
            Lamp::Ready => p.answered,
            Lamp::Waiting => p.pending,
            Lamp::NotReady => t.text_faint,
        }
    }
}

/// A Get ready step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Step {
    LogIn,
    InstallDesktop,
    AskCowork,
    InstallCode,
    AskSession,
    ChooseFolder,
}

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::LogIn => "Log in to Claude",
            Step::InstallDesktop | Step::InstallCode => "Install AItoCraft",
            Step::AskCowork => "Ask Cowork to collaborate",
            Step::AskSession => "Ask my session to collaborate",
            Step::ChooseFolder => "Choose folder…",
        }
    }
}

/// The steps of a context, with whether each is done.
fn steps(c: &Context, setup: &Setup, copied: impl Fn(Step) -> bool, ready: bool) -> Vec<(Step, bool)> {
    match (c.kind, c.code) {
        (Kind::None, _) => {
            if setup.logged_in == Some(true) {
                vec![]
            } else {
                vec![(Step::LogIn, false)]
            }
        }
        (Kind::Cowork, _) => {
            vec![(Step::InstallDesktop, setup.desktop_has_hub == Some(true)), (Step::AskCowork, copied(Step::AskCowork) || ready)]
        }
        (Kind::Code, Code::Session) => {
            vec![(Step::InstallCode, setup.code_has_hub == Some(true)), (Step::AskSession, copied(Step::AskSession) || ready)]
        }
        (Kind::Code, Code::Folder) => vec![(Step::ChooseFolder, !c.folder.is_empty())],
    }
}

/// The lamp for a document, and what it says on hover.
pub(crate) fn lamp(c: &Context, setup: &Setup, uid: Option<u64>, waiting: bool) -> (Lamp, String) {
    match c.mode() {
        Mode::Background => {
            let folder_ok = !(c.kind == Kind::Code && c.code == Code::Folder && c.folder.is_empty());
            let stopped = uid.and_then(runner::status).map(|s| s.last_error).filter(|e| !e.is_empty());
            if setup.logged_in == Some(true) && folder_ok {
                let tip = match stopped {
                    Some(e) => format!("Ready: the agent starts with the next question (it stopped last time: {e})"),
                    None => "Ready: the agent starts with the next question".into(),
                };
                (Lamp::Ready, tip)
            } else if waiting {
                (Lamp::Waiting, "Waiting for the Get ready step to finish".into())
            } else if !folder_ok {
                (Lamp::NotReady, "Not ready: choose the project folder".into())
            } else {
                (Lamp::NotReady, "Not ready: log in to Claude (Get ready)".into())
            }
        }
        Mode::Off => (Lamp::NotReady, "The agent is off in this app".into()),
        Mode::Desktop | Mode::Channel => match listener::health(uid) {
            Health::Answering { by, .. } => (Lamp::Ready, if by.is_empty() { "Ready: your agent answers".into() } else { format!("Ready: {by}") }),
            Health::Listening { label, .. } => (Lamp::Ready, format!("Ready: {label} works on this document")),
            Health::Listener(s) => (Lamp::Ready, format!("Ready: {}", s.describe())),
            Health::Checking { .. } => (Lamp::Waiting, "Waiting for your agent".into()),
            _ if waiting => (Lamp::Waiting, "Waiting for your agent to attach to this document".into()),
            _ => (Lamp::NotReady, "Not ready: nobody works on this document yet. Your questions wait in it (Get ready)".into()),
        },
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

/// The command that starts Claude Code with AItoCraft as a channel.
pub(crate) const CHANNEL_COMMAND: &str = "claude --dangerously-load-development-channels server:aitocraft";

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

/// Runs a Get ready step.
fn run_step(st: &mut AgentUi, app: &mut SlideApp, ui: &Ui, step: Step, uid: u64, now: f64) {
    let title = app.session.active().map(deckcraft_agent::documents::title).unwrap_or_default();
    match step {
        Step::LogIn => {
            let cmd = format!("{} auth login", runner::config().command);
            match context::open_terminal(&cmd) {
                Ok(()) => app.set_status("Log in to Claude in the terminal window; DeckCraft notices when you're done"),
                Err(e) => app.set_status(format!("Couldn't open a terminal ({e}): run `{cmd}` yourself")),
            }
        }
        Step::InstallDesktop => match find_aitocraft() {
            Some(h) => match std::process::Command::new(&h).arg("--install-desktop").output() {
                Ok(o) if o.status.success() => app.set_status(format!(
                    "AItoCraft is in Claude Desktop. {}",
                    String::from_utf8_lossy(&o.stdout).lines().last().unwrap_or("Quit Claude (⌘Q) and open it again if it was running.")
                )),
                Ok(o) => {
                    app.set_status(format!("AItoCraft couldn't install: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("error")))
                }
                Err(e) => app.set_status(format!("Couldn't run AItoCraft: {e}")),
            },
            None => app.set_status("AItoCraft isn't installed on this Mac (build ~/aitocraft, or set AITOCRAFT_BIN)"),
        },
        Step::InstallCode => match find_aitocraft() {
            Some(h) => match context::install_in_code(&h) {
                Ok(()) => app.set_status("AItoCraft is in Claude Code (for every Craft app)"),
                Err(e) => app.set_status(format!("Couldn't add AItoCraft to Claude Code: {e}")),
            },
            None => app.set_status("AItoCraft isn't installed on this Mac (build ~/aitocraft, or set AITOCRAFT_BIN)"),
        },
        Step::AskCowork => {
            ui.ctx().copy_text(phrase(&title));
            app.set_status("Copied: paste it into the Cowork chat that should work on this deck");
            st.ribbon.context.copied.insert((uid, step));
        }
        Step::AskSession => {
            ui.ctx().copy_text(CHANNEL_COMMAND.into());
            app.set_status("Copied: start Claude Code with it, then say which deck to work on");
            st.ribbon.context.copied.insert((uid, step));
        }
        Step::ChooseFolder => {
            if let Some(f) = context::choose_folder() {
                let _ = app.run("agent.contextMode", json!({"folder": f}));
            }
            return;
        }
    }
    st.ribbon.context.waiting.insert(uid, now);
    let _ = context::setup(true);
}

/// The ☐ Web checkbox, drawn so it shows on the ribbon in both themes.
fn web_box(ui: &mut Ui, t: &Tokens, on: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(60.0, 22.0), Sense::click());
    let b = egui::Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(13.0, 13.0));
    let p = ui.painter();
    if r.hovered() {
        p.rect_filled(rect, 4.0, t.hover);
    }
    p.rect_filled(b, 2.0, if on { t.accent } else { t.field_bg });
    p.rect_stroke(b, 2.0, egui::Stroke::new(1.0, if on { t.accent } else { t.border }), egui::StrokeKind::Inside);
    if on {
        let s = egui::Stroke::new(1.8, Color32::WHITE);
        p.line_segment([pos2(b.left() + 3.0, b.center().y), pos2(b.center().x - 0.5, b.bottom() - 3.5)], s);
        p.line_segment([pos2(b.center().x - 0.5, b.bottom() - 3.5), pos2(b.right() - 2.5, b.top() + 3.0)], s);
    }
    p.text(pos2(b.right() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, "Web", theme::font(12.0), t.text);
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, "Web"));
    r
}

/// A row of the mode menu: the name, its line and chips.
fn mode_row(ui: &mut Ui, t: &Tokens, k: Kind, on: bool) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    job.append(k.label(), 0.0, egui::TextFormat { font_id: theme::bold(12.5), color: t.text, ..Default::default() });
    job.append(&format!("\n{}\n", k.line()), 0.0, egui::TextFormat { font_id: theme::font(10.5), color: t.text_dim, ..Default::default() });
    for (i, chip) in k.chips().iter().enumerate() {
        if i > 0 {
            job.append(" ", 0.0, egui::TextFormat { font_id: theme::font(10.5), ..Default::default() });
        }
        job.append(
            &format!(" {chip} "),
            0.0,
            egui::TextFormat { font_id: theme::font(10.5), color: t.text, background: t.hover, ..Default::default() },
        );
    }
    let b = egui::Button::new(job).min_size(vec2(280.0, 0.0)).fill(if on { t.selected } else { Color32::TRANSPARENT });
    let r = ui.add(b);
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, format!("{}: {}", k.label(), k.line())));
    r
}

pub(crate) fn group(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(d) = app.session.active() else { return };
    let uid = d.uid;
    let ctx = context::of(d);
    let now = ui.input(|i| i.time);
    let setup = context::setup(false);
    // Amber ends when a listener attaches, or after a while.
    let waiting = st.ribbon.context.waiting.get(&uid).is_some_and(|since| now - since < WAITING_FOR);
    let (lamp, tip) = lamp(&ctx, &setup, Some(uid), waiting);
    if lamp == Lamp::Ready {
        st.ribbon.context.waiting.remove(&uid);
    }
    if waiting && st.ribbon.context.last_look.is_none_or(|at| now - at > LOOK_EVERY) {
        st.ribbon.context.last_look = Some(now);
        let _ = context::setup(true);
    }
    // A quiet check when a listener shows signs of life but hasn't answered (never shown).
    if matches!(ctx.mode(), Mode::Desktop | Mode::Channel)
        && matches!(listener::health(Some(uid)), Health::Listener(_))
        && st.ribbon.context.last_ping.is_none_or(|at| now - at > PING_EVERY)
    {
        st.ribbon.context.last_ping = Some(now);
        let _ = app.run("agent.ping", json!({}));
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));

    deckcraft_ui_egui::widgets::group(ui, |ui| {
        // Left: the mode, with the lamp's dot.
        let icon = match ctx.kind {
            Kind::None => Icon::Slide,
            Kind::Cowork => Icon::CommentsBubble,
            Kind::Code => Icon::Sparkle,
        };
        let label = match ctx.kind {
            Kind::None => "No\ncontext",
            Kind::Cowork => "Claude\nCowork",
            Kind::Code => "Claude\nCode",
        };
        let b = status_button(ui, icon, label, None, true, false);
        let dot = pos2(b.rect.center().x + 12.0, b.rect.min.y + 6.0);
        ui.painter().circle_filled(dot, 4.5, lamp.color(&t));
        ui.painter().circle_stroke(dot, 4.5, egui::Stroke::new(1.0, t.chrome));
        let b = b.on_hover_text(format!("{}: {}\n{tip}", ctx.kind.label(), ctx.kind.line()));
        egui::Popup::menu(&b).show(|ui| {
            ui.set_min_width(300.0);
            for k in Kind::ALL {
                if mode_row(ui, &t, k, k == ctx.kind).clicked() && k != ctx.kind {
                    if app.run("agent.contextMode", json!({"kind": k.id()})).is_ok()
                        && let Some(d) = app.session.active()
                    {
                        crate::remember_mode(context::of(d).mode());
                    }
                    ui.close();
                }
            }
            ui.separator();
            details(st, app, ui, &ctx);
        });

        // Right: by mode.
        rows(ui, |ui| {
            ui.set_width(180.0);
            match ctx.kind {
                Kind::None => {
                    if web_box(ui, &t, ctx.web).on_hover_text("The background agent may also search and fetch the web").clicked() {
                        let _ = app.run("agent.contextMode", json!({"web": !ctx.web}));
                    }
                }
                Kind::Cowork => {}
                Kind::Code => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for c in [Code::Session, Code::Folder] {
                            let on = ctx.code == c;
                            let r = ui.selectable_label(on, RichText::new(c.label()).size(11.5));
                            let r = match c {
                                Code::Session => r.on_hover_text("Questions arrive in your running Claude Code session (AItoCraft as a channel)"),
                                Code::Folder => r.on_hover_text(
                                    "DeckCraft runs Claude in a project folder you choose: it sees that folder's CLAUDE.md, memory and files",
                                ),
                            };
                            if r.clicked() && !on {
                                let _ = app.run("agent.contextMode", json!({"code": if c == Code::Session { "session" } else { "folder" }}));
                            }
                        }
                    });
                }
            }
            // The lamp.
            ui.horizontal(|ui| {
                ui.set_height(20.0);
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, lamp.color(&t));
                ui.add(egui::Label::new(RichText::new(lamp.label()).size(12.5).color(t.text)).selectable(false)).on_hover_text(&tip);
                if ctx.kind == Kind::Code && ctx.code == Code::Folder && !ctx.folder.is_empty() {
                    let name =
                        std::path::Path::new(&ctx.folder).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| ctx.folder.clone());
                    ui.add(egui::Label::new(RichText::new(format!("  · {name}")).size(11.0).color(t.text_dim)).selectable(false).truncate())
                        .on_hover_text(format!("{}\nThe agent runs in this folder: it sees its CLAUDE.md, memory and files", ctx.folder));
                }
            });
            // Get ready.
            let copied = |s: Step| st.ribbon.context.copied.contains(&(uid, s));
            let list = steps(&ctx, &setup, copied, lamp == Lamp::Ready);
            if !list.is_empty() {
                let next = list.iter().find(|(_, done)| !done).or(list.last()).map(|(s, _)| *s);
                let (main, arrow) = small_button_ex(ui, Icon::Redo, "Get ready", true, false, true);
                let main = main.on_hover_text(match next {
                    Some(s) if list.iter().any(|(_, done)| !done) => format!("Next: {}", s.label()),
                    _ => "Every step is done".to_string(),
                });
                if main.clicked()
                    && let Some(s) = next
                {
                    run_step(st, app, ui, s, uid, now);
                }
                if let Some(a) = arrow {
                    a.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Get ready steps"));
                    let mut chosen = None;
                    egui::Popup::menu(&a).show(|ui| {
                        ui.set_min_width(240.0);
                        for (s, done) in &list {
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                if *done {
                                    icons::paint(ui.painter(), r, Icon::Check, t.text, false);
                                }
                                if ui.add(egui::Button::new(s.label()).frame(false)).clicked() {
                                    chosen = Some(*s);
                                    ui.close();
                                }
                            });
                        }
                    });
                    if let Some(s) = chosen {
                        run_step(st, app, ui, s, uid, now);
                    }
                }
            } else if ctx.kind == Kind::None {
                // Logged in: nothing to get ready.
                ui.add_space(22.0);
            }
        });
    });
}

/// Under the mode menu: how it is wired (the direct connection, the background agent's session).
fn details(st: &mut AgentUi, app: &mut SlideApp, ui: &mut Ui, ctx: &Context) {
    let srv = server::status();
    let url = crate::server_url().map(str::to_string).or_else(|| srv.port.map(|p| format!("http://127.0.0.1:{p}/mcp")));
    let base = url.as_deref().map(|u| u.trim_end_matches("/mcp").to_string());
    let entry = STDIO_ENTRY.get().cloned();
    let cowork_state = cowork::state();
    let now = ui.input(|i| i.time);
    if st.ribbon.looked_at.is_none_or(|at| now - at > COWORK_EVERY) {
        st.ribbon.looked_at = Some(now);
        st.ribbon.configured = match (&entry, cowork::config_path()) {
            (Some(e), Some(path)) => Some(cowork::configured(&path, e)),
            _ => None,
        };
        st.ribbon.claude_running = matches!(cowork_state, cowork::State::Done { .. }) && cowork::claude_running();
    }
    ui.menu_button("Details", |ui| {
        ui.set_min_width(320.0);
        if ctx.mode() == Mode::Background {
            if ui.button("Open session in Claude Desktop").on_hover_text("Claude Desktop ▸ Code: the session named “DeckCraft · <file>”").clicked() {
                if let Err(e) = runner::open_claude_desktop() {
                    app.set_status(format!("Couldn't open Claude Desktop: {e}"));
                }
                ui.close();
            }
            if ui.button("New session").on_hover_text("A fresh session for this deck, started from a summary of its history").clicked() {
                let _ = app.run("agent.agentReset", json!({}));
                ui.close();
            }
            ui.separator();
        }
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
        if let Some(u) = &url {
            ui.label(RichText::new(format!("MCP: {u}")).size(11.5));
        }
        if let Some((_, c)) = exe_and_control() {
            ui.label(RichText::new(format!("Control channel: 127.0.0.1:{c}")).size(11.5));
        }
        if !srv.client.is_empty() {
            ui.label(RichText::new(format!("Last MCP client: {}", srv.client)).size(11.5));
        }
        if let Some(d) = app.session.active() {
            let session = deckcraft_agent::layer::get(d).agent_session.clone();
            let rs = runner::status_json(d.uid, &session);
            if ctx.mode() == Mode::Background || !session.is_empty() {
                ui.separator();
                ui.label(RichText::new(format!("Background agent: {}", if rs["running"] == true { "running" } else { "not running" })).size(11.5));
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
}
