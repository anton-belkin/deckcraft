//! Voice questions (the same as GridCraft's): hold ⌥ (Alt) or click the microphone on the Agent
//! tab, speak, click the shapes or slides you mean while speaking; release, and the question goes
//! on in the background, without a preview or a confirmation.
//!
//! The question is put on the selection at once (a topic, with a place for everything pointed at,
//! "transcribing"); [`craft_voice`] transcribes it on a worker thread and the text, with a marker
//! `⟦n⟧` where each shape or slide was pointed at (placed by time), goes to the agent's queue
//! (`agent.spoken`). A failed transcription is a flag with Retry and Dismiss, never a dialog. Audio
//! stays in memory until its text is there (or the person dismisses it), and is never written
//! anywhere.
//!
//! The model is the person's choice on the Agent tab (off until they pick one; small or turbo), an
//! app setting (`voice.json` beside the remembered agent mode), never the deck's.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use craft_voice::{Audio, Event, Model, Pointer, Recorder, Transcriber};
use deckcraft_engine::Session;
use deckcraft_engine::model::ShapeId;
use deckcraft_ui_egui::SlideApp;
use deckcraft_ui_egui::ext::SlideGeometry;
use deckcraft_ui_egui::theme::{self, Tokens};
use egui::{Align2, Color32, Rect, Stroke, pos2, vec2};
use serde_json::{Value as Json, json};

/// How long ⌥ is held alone before recording starts (a tap or a shortcut is not a question).
const HOLD: f64 = 0.2;
/// Shorter recordings with nothing pointed at are taken for an accidental press.
const MIN_SECONDS: f32 = 0.4;
/// A loaded model is let go after this long without a question (it holds 0.6–1.3 GB).
const IDLE_UNLOAD: f64 = 15.0 * 60.0;

/// The key, as people see it (a word: the UI font has no ⌥).
pub(crate) const KEY: &str = if cfg!(target_os = "macos") { "Option" } else { "Alt" };

/// The recording colour.
pub(crate) const RED: Color32 = Color32::from_rgb(0xD9, 0x30, 0x25);

/// Where the model stands, for the ribbon.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Status {
    /// No model chosen.
    #[default]
    Off,
    /// Chosen, downloaded, not loaded (loads with the first question).
    Idle,
    Downloading(u64, u64),
    Loading,
    Ready,
    Failed(String),
}

impl Status {
    fn id(&self) -> &'static str {
        match self {
            Status::Off => "off",
            Status::Idle => "idle",
            Status::Downloading(..) => "downloading",
            Status::Loading => "loading",
            Status::Ready => "ready",
            Status::Failed(_) => "failed",
        }
    }
}

/// One thing pointed at: `2` (slide 2) or `2/17` (shape 17 on slide 2), slides 1-based.
type Anchor = String;

/// What is selected: the slide, and the shape (selected or being edited).
pub(crate) type Sel = (usize, Option<ShapeId>);

fn anchor_of(sel: Sel) -> Anchor {
    match sel.1 {
        Some(id) => format!("{}/{}", sel.0 + 1, id.0),
        None => format!("{}", sel.0 + 1),
    }
}

/// `{slide, shape?}` for a command.
fn place_json(a: &str) -> Json {
    let (slide, shape) = match a.split_once('/') {
        Some((s, id)) => (s, id.parse::<u32>().ok()),
        None => (a, None),
    };
    let slide: usize = slide.parse().unwrap_or(1);
    match shape {
        Some(id) => json!({"slide": slide, "shape": id}),
        None => json!({"slide": slide}),
    }
}

fn selection(d: &deckcraft_engine::DocState) -> Sel {
    let sel = &d.selection;
    let shape = sel.text.as_ref().filter(|t| !t.notes).map(|t| t.shape).or_else(|| sel.shapes.first().copied());
    (sel.slide, shape)
}

struct Recording {
    /// None in a preview (offscreen snapshots) or with a stand-in microphone.
    recorder: Option<Recorder>,
    fake: Option<Vec<f32>>,
    preview_secs: f32,
    doc: u64,
    /// The selection when it started: the question's main place.
    anchor: Anchor,
    pointing: Pointing,
    by_key: bool,
}

/// Pointing while recording, frame by frame: every click on the slide and every other selection
/// change (a thumbnail, the keyboard) becomes a pointer at the moment it began.
#[derive(Debug, Default)]
pub(crate) struct Pointing {
    pub pointers: Vec<Pointer<Anchor>>,
    /// The selection last seen.
    last: Option<Sel>,
    /// When a click on the slide began (seconds into the recording), and whether a frame has
    /// passed since the button came up.
    press: Option<f32>,
    settled: bool,
}

/// Pointing at the same place again within this long adds no marker (a double click, a click on
/// a shape that was just selected).
const SAME_PLACE: f32 = 1.0;

impl Pointing {
    pub(crate) fn new(selection: Sel) -> Pointing {
        Pointing { last: Some(selection), ..Pointing::default() }
    }

    /// One frame. Extension frames run before the canvas handles this frame's input, so a click's
    /// selection is read a frame after the button comes up, never the one before it.
    pub(crate) fn observe(&mut self, now: f32, pressed_on_slide: bool, down: bool, selection: Sel) {
        if pressed_on_slide {
            self.press = Some(now);
            self.settled = false;
        }
        if down {
            return;
        }
        if let Some(t) = self.press {
            if !self.settled {
                self.settled = true;
                return;
            }
            self.add(t, anchor_of(selection));
            self.press = None;
            self.last = Some(selection);
            return;
        }
        // The selection moved some other way (a thumbnail, the keys).
        if self.last != Some(selection) {
            self.add(now, anchor_of(selection));
            self.last = Some(selection);
        }
    }

    fn add(&mut self, at: f32, anchor: Anchor) {
        if self.pointers.iter().any(|p| p.anchor == anchor && (p.at - at).abs() < SAME_PLACE) {
            return;
        }
        self.pointers.push(Pointer { at, anchor });
    }
}

/// A question waiting for its text.
struct Job {
    doc: u64,
    question: String,
    pointers: Vec<Pointer<Anchor>>,
    /// Kept for a retry until the text is there.
    audio: Audio,
    /// The id of its transcription now.
    ticket: u64,
    failed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Key {
    #[default]
    Idle,
    Armed(f64),
    /// ⌥ went into a shortcut: wait for it to be released.
    Chord,
    Recording,
}

#[derive(Default)]
pub(crate) struct Voice {
    choice: Option<Model>,
    settings_read: bool,
    transcriber: Option<Transcriber>,
    status: Status,
    rec: Option<Recording>,
    jobs: Vec<Job>,
    next_ticket: u64,
    key: Key,
    last_used: f64,
    /// ⌥ was used for a question this frame: its release isn't a keytip.
    pub(crate) key_used: bool,
}

/// A model chosen through `agent.voiceModel` (control channel, MCP), taken by the UI next frame.
static REQUEST: Mutex<Option<Option<Model>>> = Mutex::new(None);
/// What the ribbon shows, for `agent.voiceStatus`.
static SNAPSHOT: Mutex<Option<Json>> = Mutex::new(None);
/// The settings file, when a test moves it.
static SETTINGS: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What an offscreen snapshot shows instead of a live microphone and download.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// Recording for this many seconds, these places pointed at (`2/17`, `3`).
    pub recording: Option<(f32, Vec<String>)>,
    /// A download this far along (bytes, total).
    pub downloading: Option<(u64, u64)>,
}

static PREVIEW: Mutex<Option<Preview>> = Mutex::new(None);

/// Shows a recording or a download without a microphone or a network (snapshots).
#[doc(hidden)]
pub fn preview(p: Preview) {
    if let Ok(mut g) = PREVIEW.lock() {
        *g = Some(p);
    }
}

/// A stand-in microphone (tests): recordings return these samples, and nothing is transcribed.
static FAKE_MIC: Mutex<Option<Vec<f32>>> = Mutex::new(None);

/// Records from a stand-in instead of the microphone, and transcribes nothing (tests).
#[doc(hidden)]
pub fn use_fake_microphone(samples: Option<Vec<f32>>) {
    if let Ok(mut g) = FAKE_MIC.lock() {
        *g = samples;
    }
}

fn fake_mic() -> Option<Vec<f32>> {
    FAKE_MIC.lock().ok().and_then(|g| g.clone())
}

/// Moves the settings file (tests).
pub fn set_settings_path(p: Option<PathBuf>) {
    if let Ok(mut g) = SETTINGS.lock() {
        *g = p;
    }
}

fn settings_path() -> Option<PathBuf> {
    if let Some(p) = SETTINGS.lock().ok().and_then(|g| g.clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("DECKCRAFT_VOICE_SETTINGS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::MODE_FILE.get().and_then(|f| f.parent().map(|d| d.join("voice.json")))
}

fn read_choice() -> Option<Model> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    let v: Json = serde_json::from_str(&text).ok()?;
    Model::from_id(v.get("model")?.as_str()?)
}

fn write_choice(m: Option<Model>) -> Result<(), String> {
    let Some(p) = settings_path() else { return Err("no settings folder".into()) };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, json!({"model": m.map_or("off", Model::id)}).to_string()).map_err(|e| e.to_string())
}

fn model_dir() -> Option<PathBuf> {
    craft_voice::default_dir()
}

// ---------------------------------------------------------------- commands

pub(crate) static SPECS: &[deckcraft_engine::cmd::CommandSpec] = &[
    deckcraft_engine::cmd::CommandSpec {
        id: "agent.voiceModel",
        label: "Voice Questions Model",
        menu: &[],
        shortcut: None,
        params: "{model: off|small|turbo} — the speech model for voice questions (an app setting; downloaded on first use)",
        enabled: always,
        run: voice_model,
        journal: false,
        undoable: false,
    },
    deckcraft_engine::cmd::CommandSpec {
        id: "agent.voiceStatus",
        label: "Voice Questions Status",
        menu: &[],
        shortcut: None,
        params: "{} → {model, status, done?, total?, error?, recording, transcribing}",
        enabled: always,
        run: voice_status,
        journal: false,
        undoable: false,
    },
];

fn always(_s: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn voice_model(_s: &mut Session, p: &Json) -> deckcraft_engine::Result<Json> {
    let bad = |m: &str| deckcraft_engine::EngineError::BadParams { cmd: "agent.voiceModel".into(), msg: m.into() };
    let id = p.get("model").and_then(Json::as_str).ok_or_else(|| bad("missing `model` (off, small or turbo)"))?;
    let m = match id {
        "off" => None,
        id => Some(Model::from_id(id).ok_or_else(|| bad("`model` is off, small or turbo"))?),
    };
    write_choice(m).map_err(|e| deckcraft_engine::EngineError::Other(format!("couldn't keep the setting: {e}")))?;
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(m);
    }
    Ok(json!({"model": m.map_or("off", Model::id)}))
}

fn voice_status(_s: &mut Session, _p: &Json) -> deckcraft_engine::Result<Json> {
    Ok(SNAPSHOT.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| json!({"model": "off", "status": "off"})))
}

// ---------------------------------------------------------------- the frame

impl Voice {
    pub(crate) fn choice(&self) -> Option<Model> {
        self.choice
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }

    pub(crate) fn recording(&self) -> bool {
        self.rec.is_some()
    }

    /// Seconds recorded and the input level, while recording.
    pub(crate) fn meter(&self) -> Option<(f32, f32)> {
        self.rec.as_ref().map(|r| r.recorder.as_ref().map_or((r.preview_secs, 0.35), |m| (m.position(), m.level())))
    }

    /// The places pointed at so far, numbered (while recording).
    pub(crate) fn pointed(&self) -> Vec<(u32, String)> {
        let Some(r) = &self.rec else { return vec![] };
        numbered(&r.pointing.pointers)
    }

    /// The person picked a model (or off) on the ribbon.
    pub(crate) fn choose(&mut self, app: &mut SlideApp, ctx: &egui::Context, m: Option<Model>) {
        if let Err(e) = write_choice(m) {
            app.set_status(format!("Voice: couldn't keep the setting ({e})"));
        }
        self.apply(app, ctx, m);
    }

    fn apply(&mut self, app: &mut SlideApp, ctx: &egui::Context, m: Option<Model>) {
        self.settings_read = true;
        if self.choice == m {
            return;
        }
        self.choice = m;
        self.transcriber = None;
        self.status = match m {
            None => Status::Off,
            Some(_) => Status::Idle,
        };
        // Downloaded on first use: picking a model that isn't here downloads it now, with its
        // progress on the ribbon. One that is here loads with the first question.
        if let Some(m) = m
            && !model_dir().is_some_and(|d| m.is_ready(&d))
        {
            self.start(ctx);
        }
        // Questions waiting for text go to the new model (or fail when voice is off).
        let waiting: Vec<usize> = (0..self.jobs.len()).filter(|&i| self.jobs.get(i).is_some_and(|j| !j.failed)).collect();
        for i in waiting {
            self.resend(app, ctx, i);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (Some(m), Some(dir)) = (self.choice, model_dir()) else { return };
        if self.transcriber.is_some() || fake_mic().is_some() {
            return;
        }
        let c = ctx.clone();
        let languages = std::env::var("DECKCRAFT_VOICE_LANGUAGES")
            .map(|v| v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default();
        let ready = m.is_ready(&dir);
        self.transcriber = Some(Transcriber::start(m, dir, craft_voice::Options { languages }, move || c.request_repaint()));
        self.status = if ready { Status::Loading } else { Status::Downloading(0, m.download_size()) };
    }

    /// Every frame: the key, the pointing, the transcriber's news.
    pub(crate) fn frame(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        self.key_used = false;
        if !self.settings_read {
            self.settings_read = true;
            let m = read_choice();
            self.apply(app, ctx, m);
        }
        if let Some(m) = REQUEST.lock().ok().and_then(|mut r| r.take()) {
            self.apply(app, ctx, m);
        }
        if let Some(p) = PREVIEW.lock().ok().and_then(|mut g| g.take()) {
            self.show_preview(app, p);
        }
        let now = ctx.input(|i| i.time);
        self.key(app, ctx, now);
        self.pointing(app, ctx);
        self.news(app, now);
        if self.jobs.is_empty() && self.rec.is_none() && self.transcriber.is_some() && now - self.last_used > IDLE_UNLOAD {
            // Let the model go; the next question loads it again.
            self.transcriber = None;
            self.status = if self.choice.is_some() { Status::Idle } else { Status::Off };
        }
        if let Ok(mut g) = SNAPSHOT.lock() {
            let mut v = json!({
                "model": self.choice.map_or("off", Model::id),
                "status": self.status.id(),
                "recording": self.rec.is_some(),
                "transcribing": self.jobs.iter().filter(|j| !j.failed).count(),
                "failed": self.jobs.iter().filter(|j| j.failed).count(),
            });
            match &self.status {
                Status::Downloading(done, total) => {
                    v["done"] = json!(done);
                    v["total"] = json!(total);
                }
                Status::Failed(e) => v["error"] = json!(e),
                _ => {}
            }
            *g = Some(v);
        }
        if self.rec.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    /// Push to talk: ⌥ held alone starts recording, its release sends; Esc cancels.
    fn key(&mut self, app: &mut SlideApp, ctx: &egui::Context, now: f64) {
        let (mods, keys, esc) = ctx.input(|i| {
            // Another key with ⌥ makes it a shortcut. Modifier keys themselves don't count: egui
            // reports ⌥ as a key too, sometimes a frame after the modifiers changed.
            let modifier = |k: &egui::Key| {
                use egui::Key::*;
                matches!(k, AltLeft | AltRight | ShiftLeft | ShiftRight | ControlLeft | ControlRight | SuperLeft | SuperRight)
            };
            let keys = i.events.iter().any(|e| match e {
                egui::Event::Key { key, pressed: true, .. } => !modifier(key),
                egui::Event::Text(_) => true,
                egui::Event::PointerButton { pressed: true, .. } => false,
                _ => false,
            });
            let esc = i.key_pressed(egui::Key::Escape);
            (i.modifiers, keys, esc)
        });
        let alone = mods.alt && !mods.shift && !mods.command && !mods.ctrl && !mods.mac_cmd;
        if esc && self.rec.is_some() {
            self.cancel();
            self.key = if mods.alt { Key::Chord } else { Key::Idle };
            self.key_used = true;
            return;
        }
        // Not while typing in a shape or a text box.
        let typing = app.session.active().is_some_and(|d| d.selection.text.is_some()) || ctx.text_edit_focused();
        self.key = match self.key {
            Key::Idle if alone && !typing && self.choice.is_some() && self.rec.is_none() && app.session.active().is_some() && app.show.is_none() => {
                ctx.request_repaint_after(Duration::from_secs_f64(HOLD + 0.02));
                Key::Armed(now)
            }
            Key::Idle => Key::Idle,
            Key::Armed(_) if !mods.alt => Key::Idle,
            Key::Armed(_) if keys || !alone => Key::Chord,
            Key::Armed(t) if now - t >= HOLD => {
                if self.begin(app, ctx, true) {
                    Key::Recording
                } else {
                    Key::Chord
                }
            }
            Key::Armed(t) => {
                ctx.request_repaint_after(Duration::from_secs_f64((HOLD - (now - t)).max(0.0) + 0.02));
                Key::Armed(t)
            }
            Key::Chord if mods.alt => Key::Chord,
            Key::Chord => Key::Idle,
            Key::Recording if self.rec.is_none() => {
                if mods.alt {
                    Key::Chord
                } else {
                    Key::Idle
                }
            }
            Key::Recording if !mods.alt => {
                if self.rec.as_ref().is_some_and(|r| r.by_key) {
                    self.finish(app, ctx);
                }
                // Releasing Alt alone arms the Agent tab's keytip on Windows and Linux: not after
                // a question.
                self.key_used = true;
                Key::Idle
            }
            Key::Recording => Key::Recording,
        };
        if matches!(self.key, Key::Recording | Key::Chord) && self.rec.is_some() {
            self.key_used = true;
        }
    }

    fn show_preview(&mut self, app: &SlideApp, p: Preview) {
        if let Some((done, total)) = p.downloading {
            self.status = Status::Downloading(done, total);
        }
        let (Some((secs, places)), Some(d)) = (p.recording, app.session.active()) else { return };
        let sel = selection(d);
        let n = places.len().max(1) as f32;
        self.rec = Some(Recording {
            recorder: None,
            fake: None,
            preview_secs: secs,
            doc: d.uid,
            anchor: anchor_of(sel),
            pointing: Pointing {
                pointers: places.into_iter().enumerate().map(|(i, a)| Pointer { at: secs * (i as f32 + 0.5) / n, anchor: a }).collect(),
                ..Pointing::new(sel)
            },
            by_key: true,
        });
    }

    /// The microphone button: start, or stop and send.
    pub(crate) fn toggle(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        if self.rec.is_some() {
            self.finish(app, ctx);
        } else {
            self.begin(app, ctx, false);
        }
    }

    fn begin(&mut self, app: &mut SlideApp, ctx: &egui::Context, by_key: bool) -> bool {
        let Some(d) = app.session.active() else { return false };
        if self.choice.is_none() {
            app.set_status("Voice questions: choose a speech model on the Agent tab first");
            return false;
        }
        let sel = selection(d);
        let doc = d.uid;
        let fake = fake_mic();
        let started = if fake.is_some() { Ok(None) } else { Recorder::start().map(Some) };
        match started {
            Ok(recorder) => {
                self.rec = Some(Recording { recorder, fake, preview_secs: 0.0, doc, anchor: anchor_of(sel), pointing: Pointing::new(sel), by_key });
                // Load the model while the person speaks.
                self.start(ctx);
                self.last_used = ctx.input(|i| i.time);
                true
            }
            Err(e) => {
                app.set_status(format!("Voice: {e}"));
                false
            }
        }
    }

    fn cancel(&mut self) {
        // Dropping the recorder drops the audio.
        self.rec = None;
    }

    /// While recording: every click on the slide and every selection change is a pointer, at the
    /// moment it began.
    fn pointing(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        let Some(r) = self.rec.as_mut() else { return };
        let Some(d) = app.session.active() else { return };
        if d.uid != r.doc {
            return;
        }
        let at = r.recorder.as_ref().map_or(r.preview_secs, Recorder::position);
        let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        let on_slide = pressed && matches!((app.canvas_rect, pos), (Some(c), Some(p)) if c.contains(p));
        r.pointing.observe(at, on_slide, down, selection(d));
    }

    /// Key up: the question goes on the deck now, its text follows.
    fn finish(&mut self, app: &mut SlideApp, ctx: &egui::Context) {
        let Some(r) = self.rec.take() else { return };
        let (doc, main, pointers) = (r.doc, r.anchor, r.pointing.pointers);
        let audio = match (r.recorder, r.fake) {
            (Some(recorder), _) => recorder.stop(),
            (None, Some(samples)) => Ok(Audio { samples }),
            // A preview: nothing was recorded.
            (None, None) => return,
        };
        let audio = match audio {
            Ok(a) => a,
            Err(e) => {
                app.set_status(format!("Voice: {e}"));
                return;
            }
        };
        if audio.seconds() < MIN_SECONDS && pointers.is_empty() {
            return;
        }
        self.submit(app, ctx, doc, main, pointers, audio);
    }

    /// A recording to ask about: on the deck now (transcribing), to the transcriber.
    fn submit(&mut self, app: &mut SlideApp, ctx: &egui::Context, doc: u64, main: Anchor, pointers: Vec<Pointer<Anchor>>, audio: Audio) {
        let refs: Vec<Json> = numbered(&pointers)
            .into_iter()
            .map(|(n, a)| {
                let mut r = place_json(&a);
                r["n"] = json!(n);
                r
            })
            .collect();
        let mut params = place_json(&main);
        params["transcribing"] = json!(true);
        params["spoken"] = json!(true);
        params["refs"] = json!(refs);
        let question = match on_doc(&mut app.session, doc, "agent.ask", params) {
            Ok(v) => v.get("id").and_then(Json::as_str).unwrap_or_default().to_string(),
            Err(e) => {
                app.set_status(format!("Voice: {e}"));
                return;
            }
        };
        self.jobs.push(Job { doc, question, pointers, audio, ticket: 0, failed: false });
        let i = self.jobs.len() - 1;
        self.resend(app, ctx, i);
    }

    fn resend(&mut self, app: &mut SlideApp, ctx: &egui::Context, i: usize) {
        self.start(ctx);
        self.resend_now(app, i);
    }

    /// (Re)sends a job's audio to the transcriber.
    fn resend_now(&mut self, app: &mut SlideApp, i: usize) {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let Some(job) = self.jobs.get_mut(i) else { return };
        let sent = self.transcriber.as_ref().is_some_and(|t| t.transcribe(ticket, job.audio.clone()));
        job.ticket = ticket;
        job.failed = !sent;
        let (doc, q) = (job.doc, job.question.clone());
        let params =
            if sent { json!({"id": q}) } else { json!({"id": q, "error": "Voice questions are off: choose a speech model on the Agent tab"}) };
        let _ = on_doc(&mut app.session, doc, "agent.spoken", params);
    }

    /// Retry on a failed spoken question.
    pub(crate) fn retry(&mut self, app: &mut SlideApp, ctx: &egui::Context, question: &str) {
        match self.jobs.iter().position(|j| j.question == question) {
            Some(i) => self.resend(app, ctx, i),
            // The audio is gone (the app was restarted): nothing to retry.
            None => app.set_status("This recording is no longer here: ask again"),
        }
    }

    /// Dismiss on a failed (or still transcribing) spoken question: it goes, with its audio.
    pub(crate) fn dismiss(&mut self, app: &mut SlideApp, question: &str) {
        let doc = self.jobs.iter().find(|j| j.question == question).map(|j| j.doc);
        self.jobs.retain(|j| j.question != question);
        let uid = doc.or_else(|| app.session.active().map(|d| d.uid)).unwrap_or_default();
        let _ = on_doc(&mut app.session, uid, "agent.removeQuestion", json!({"id": question}));
    }

    fn news(&mut self, app: &mut SlideApp, now: f64) {
        let Some(t) = &self.transcriber else { return };
        for e in t.poll() {
            match e {
                Event::Downloading { done, total, .. } => self.status = Status::Downloading(done, total),
                Event::Loading { .. } => self.status = Status::Loading,
                Event::Ready { .. } => self.status = Status::Ready,
                Event::Failed { id: None, error } => self.status = Status::Failed(error),
                Event::Failed { id: Some(ticket), error } => {
                    if let Some(j) = self.jobs.iter_mut().find(|j| j.ticket == ticket) {
                        j.failed = true;
                        let (doc, q) = (j.doc, j.question.clone());
                        let _ = on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "error": error}));
                    }
                }
                Event::Text { id: ticket, transcript, secs } => {
                    self.last_used = now;
                    let Some(i) = self.jobs.iter().position(|j| j.ticket == ticket) else { continue };
                    let Some(j) = self.jobs.get_mut(i) else { continue };
                    let (doc, q) = (j.doc, j.question.clone());
                    if transcript.is_empty() {
                        j.failed = true;
                        let _ = on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "error": "Nothing was heard"}));
                        continue;
                    }
                    let text = craft_voice::splice(&transcript, &j.pointers).text;
                    // When and where: enough to see why a marker landed where it did (text and
                    // times only, never audio).
                    let segs: Vec<String> = transcript.segments.iter().map(|g| format!("{:.2}–{:.2} {}", g.start, g.end, g.text)).collect();
                    let ptrs: Vec<String> = j.pointers.iter().map(|p| format!("{:.2} {}", p.at, p.anchor)).collect();
                    log::info!("voice {q}: {secs:.2} s, {} · segments [{}] · pointers [{}]", transcript.language, segs.join(" | "), ptrs.join(", "));
                    // With the agent now (or dismissed, or its deck closed); the audio goes.
                    let _ = on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "text": text}));
                    self.jobs.remove(i);
                }
            }
        }
        // Questions whose deck was closed: let their audio go.
        let open: Vec<u64> = app.session.documents().iter().map(|d| d.uid).collect();
        self.jobs.retain(|j| open.contains(&j.doc));
    }
}

/// The references in the order they were pointed at, the same place reusing its number: the
/// numbering `craft_voice::splice` gives the markers.
fn numbered(pointers: &[Pointer<Anchor>]) -> Vec<(u32, Anchor)> {
    let mut order: Vec<&Pointer<Anchor>> = pointers.iter().collect();
    order.sort_by(|a, b| a.at.total_cmp(&b.at));
    let mut out: Vec<(u32, Anchor)> = vec![];
    for p in order {
        if !out.iter().any(|(_, a)| *a == p.anchor) {
            let n = u32::try_from(out.len()).unwrap_or(u32::MAX).saturating_add(1);
            out.push((n, p.anchor.clone()));
        }
    }
    out
}

/// Runs a command on a given deck (the one the question was asked in), whichever is in front.
pub(crate) fn on_doc(s: &mut Session, uid: u64, id: &str, params: Json) -> Result<Json, String> {
    let Some(i) = s.documents().iter().position(|d| d.uid == uid) else { return Err("the deck was closed".into()) };
    let front = s.active_index();
    s.set_active(i);
    let r = s.execute(id, &params).map_err(|e| e.to_string());
    if let Some(f) = front {
        s.set_active(f);
    }
    r
}

// ---------------------------------------------------------------- drawing

/// While recording: a pill at the bottom of the slide (time, level, how to send or cancel).
pub(crate) fn indicator(v: &Voice, app: &SlideApp, ctx: &egui::Context) {
    let (Some((secs, level)), Some(canvas)) = (v.meter(), app.canvas_rect) else { return };
    let t = Tokens::get(ctx);
    let by_key = v.rec.as_ref().is_some_and(|r| r.by_key);
    let hint = if by_key { format!("release {KEY} to send · Esc cancels") } else { "click the microphone to send · Esc cancels".into() };
    let n = v.pointed().len();
    let text = if n > 0 {
        format!("Listening {secs:.0}s · {n} pointed · {hint}")
    } else {
        format!("Listening {secs:.0}s · click the shapes you mean · {hint}")
    };
    let pos = pos2(canvas.center().x, canvas.bottom() - 18.0);
    egui::Area::new(egui::Id::new("agent_voice_indicator"))
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE.fill(t.card).stroke(Stroke::new(1.0, RED)).corner_radius(14.0).inner_margin(egui::Margin::symmetric(10, 5)).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(64.0, 16.0), egui::Sense::hover());
                        let pulse = 0.6 + 0.4 * ((ui.input(|i| i.time) * 3.0).sin() * 0.5 + 0.5) as f32;
                        ui.painter().circle_filled(pos2(r.left() + 6.0, r.center().y), 5.0, RED.gamma_multiply(pulse));
                        // A level meter: five bars.
                        let lv = (level * 8.0).clamp(0.0, 1.0);
                        for k in 0..5 {
                            let h = 3.0 + 10.0 * (lv * (1.0 - (k as f32 - 2.0).abs() * 0.18)).max(0.05);
                            let x = r.left() + 18.0 + k as f32 * 8.0;
                            let bar = Rect::from_center_size(pos2(x, r.center().y), vec2(4.0, h));
                            ui.painter().rect_filled(bar, 2.0, if lv > 0.05 { RED } else { t.text_dim });
                        }
                        ui.label(egui::RichText::new(text).font(theme::font(12.0)).color(t.text));
                    });
                },
            );
        });
}

/// While recording: a numbered bubble on each shape (or slide) pointed at on this slide.
pub(crate) fn bubbles(v: &Voice, app: &SlideApp, painter: &egui::Painter, geo: &SlideGeometry) {
    let Some(d) = app.session.active() else { return };
    if v.rec.as_ref().is_none_or(|r| r.doc != d.uid) {
        return;
    }
    let slide = d.selection.slide;
    for (n, a) in v.pointed() {
        let p = place_json(&a);
        if p["slide"].as_u64() != Some(slide as u64 + 1) {
            continue;
        }
        let r = match p["shape"].as_u64().and_then(|id| u32::try_from(id).ok()).and_then(|id| crate::overlay::shape_rect(app, geo, ShapeId(id))) {
            Some(r) => r,
            None => geo.slide,
        };
        painter.rect_stroke(r.expand(2.0), 2.0, Stroke::new(1.5, RED), egui::StrokeKind::Outside);
        crate::marker(painter, pos2(r.left() + 20.0, r.top()), &n.to_string(), RED, false, 16.0);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use deckcraft_agent::layer::{self, QuestionStatus};

    fn sel(slide: usize, shape: Option<u32>) -> Sel {
        (slide, shape.map(ShapeId))
    }

    /// Frames of pointing: (time, pressed on the slide, button down, selection).
    fn run(frames: &[(f32, bool, bool, Sel)]) -> Vec<(f32, String)> {
        let mut p = Pointing::new(sel(0, None));
        for &(t, pressed, down, s) in frames {
            p.observe(t, pressed, down, s);
        }
        p.pointers.into_iter().map(|x| (x.at, x.anchor)).collect()
    }

    #[test]
    fn a_quick_click_points_at_the_shape_clicked_not_the_one_before() {
        // Press and release in one frame: the canvas selects shape 9 only after this frame's
        // extension hook ran, so the selection still says the slide here.
        let got = run(&[(1.0, true, false, sel(0, None)), (1.06, false, false, sel(0, Some(9))), (1.12, false, false, sel(0, Some(9)))]);
        assert_eq!(got, vec![(1.0, "1/9".to_string())]);
    }

    #[test]
    fn a_thumbnail_points_at_its_slide_and_the_same_place_within_a_second_counts_once() {
        let got = run(&[(1.0, false, false, sel(2, None)), (1.5, false, false, sel(3, None))]);
        assert_eq!(got, vec![(1.0, "3".to_string()), (1.5, "4".to_string())]);
        // A double click on shape 9: one pointer.
        let got = run(&[
            (1.0, true, false, sel(0, None)),
            (1.05, false, false, sel(0, Some(9))),
            (1.3, true, false, sel(0, Some(9))),
            (1.35, false, false, sel(0, Some(9))),
        ]);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn numbering_matches_the_splice() {
        let ps = vec![
            Pointer { at: 2.0, anchor: "2/7".to_string() },
            Pointer { at: 0.5, anchor: "2/3".to_string() },
            Pointer { at: 3.0, anchor: "2/3".to_string() },
        ];
        let t =
            craft_voice::Transcript::from_segments(vec![craft_voice::Segment { start: 0.0, end: 4.0, text: "this and that and this".into() }], "en");
        assert_eq!(numbered(&ps), craft_voice::splice(&t, &ps).refs);
    }

    #[test]
    fn with_voice_off_a_recording_becomes_a_failed_flag_not_a_dialog() {
        crate::install();
        set_settings_path(Some(std::env::temp_dir().join(format!("deckcraft-voice-test-{}.json", std::process::id()))));
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
        let id = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Revenue grew 6%"})).unwrap()["id"].as_u64().unwrap();
        let mut app = SlideApp::new(s, Default::default());
        let mut v = Voice { settings_read: true, ..Voice::default() };
        let doc = app.session.active().unwrap().uid;
        let ctx = egui::Context::default();
        v.submit(&mut app, &ctx, doc, "2".into(), vec![Pointer { at: 0.2, anchor: format!("2/{id}") }], Audio { samples: vec![0.1; 16_000] });
        let l = layer::get(app.session.active().unwrap());
        assert_eq!(l.questions[0].status, QuestionStatus::Failed);
        assert_eq!(l.questions[0].refs.len(), 1);
        // Dismissed: gone, and its audio with it.
        let q = l.questions[0].id.clone();
        v.dismiss(&mut app, &q);
        assert!(layer::get(app.session.active().unwrap()).questions.is_empty());
        assert!(v.jobs.is_empty());
    }

    /// Speech made by macOS `say` (none elsewhere).
    fn say(text: &str) -> Option<Audio> {
        let dir = std::env::temp_dir().join(format!("deckcraft-voice-say-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let wav = dir.join(format!("q-{}.wav", text.len()));
        let ok = std::process::Command::new("say")
            .args(["-v", "Samantha", "-o"])
            .arg(&wav)
            .args(["--data-format=LEI16@16000", text])
            .status()
            .ok()?
            .success();
        let audio = ok.then(|| std::fs::read(&wav).ok()).flatten().and_then(|b| Audio::from_wav(&b).ok());
        let _ = std::fs::remove_dir_all(&dir);
        audio
    }

    /// The whole path: speech → Whisper → text with markers → a question with refs → what the
    /// agent reads. Runs when the small model is downloaded and `say` exists.
    #[test]
    fn speech_with_pointing_reaches_the_agent() {
        let ready = craft_voice::default_dir().is_some_and(|d| Model::Small.is_ready(&d));
        let Some(audio) = ready.then(|| say("Why did this grow so much faster than that?")).flatten() else {
            eprintln!("skipped: needs the small model in the models folder and macOS `say`");
            return;
        };
        crate::install();
        set_settings_path(Some(std::env::temp_dir().join(format!("deckcraft-voice-test2-{}.json", std::process::id()))));
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
        let a = s.execute("insert.textBox", &json!({"rect": [60, 60, 400, 60], "text": "Revenue grew 6%"})).unwrap()["id"].as_u64().unwrap();
        let b = s.execute("insert.textBox", &json!({"rect": [60, 300, 400, 60], "text": "Costs fell 2%"})).unwrap()["id"].as_u64().unwrap();
        let mut app = SlideApp::new(s, Default::default());
        let ctx = egui::Context::default();
        let mut v = Voice { settings_read: true, ..Voice::default() };
        v.apply(&mut app, &ctx, Some(Model::Small));
        let doc = app.session.active().unwrap().uid;
        let secs = audio.seconds();
        let pointers = vec![Pointer { at: secs * 0.25, anchor: format!("2/{a}") }, Pointer { at: secs * 0.85, anchor: format!("2/{b}") }];
        v.submit(&mut app, &ctx, doc, "2".into(), pointers, audio);
        let status = |app: &SlideApp| layer::get(app.session.active().unwrap()).questions[0].status;
        assert_eq!(status(&app), QuestionStatus::Transcribing);
        let t0 = std::time::Instant::now();
        while status(&app) == QuestionStatus::Transcribing {
            assert!(t0.elapsed().as_secs() < 300, "timed out");
            v.frame(&mut app, &ctx);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(status(&app), QuestionStatus::Pending);
        assert!(v.jobs.is_empty(), "the audio is gone once the text is there");
        let list = app.session.execute("agent.questions", &json!({"status": "pending", "actor": "agent"})).unwrap();
        let q = &list[0];
        let (spoken, text) = (q["spokenText"].as_str().unwrap(), q["text"].as_str().unwrap());
        eprintln!("spoken: {spoken}\nagent reads: {text}");
        assert_eq!(craft_voice::markers_in(spoken), vec![1, 2], "{spoken}");
        assert!(text.contains("“Revenue grew 6%”⟧"), "{text}");
        assert!(text.contains("“Costs fell 2%”⟧"), "{text}");
    }
}
