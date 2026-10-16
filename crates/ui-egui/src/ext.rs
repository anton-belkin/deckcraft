//! UI extension points: panels, ribbon tabs, slide overlays, context-menu items and presenter-view sections
//! contributed by crates an app links in (the UI knows nothing about them). Register with
//! [`register`] before the first frame.
//!
//! Extensions act like the rest of the UI: they read engine state from `app.session` and act
//! through `app.run(id, params)`.

use std::cell::RefCell;

use deckcraft_engine::model::Presentation;
use deckcraft_geom::Point;
use egui::{Painter, Pos2, Rect, Ui};

use crate::SlideApp;
use crate::canvas::Xf;

/// Where the edited slide is on screen this frame.
pub struct SlideGeometry {
    /// Slide points to screen points.
    pub xf: Xf,
    /// The slide's rectangle on screen.
    pub slide: Rect,
    /// The visible canvas around it.
    pub canvas: Rect,
    /// The pointer, when it is over the canvas.
    pub pointer: Option<Pos2>,
}

impl SlideGeometry {
    pub fn to_screen(&self, p: Point) -> Pos2 {
        self.xf.to_screen(p)
    }
    pub fn to_slide(&self, p: Pos2) -> Point {
        self.xf.to_slide(p)
    }
}

/// A UI extension. Every method has a default that does nothing.
pub trait UiExtension {
    /// Once per frame, before anything is drawn (shortcuts, polling, opening panels).
    fn frame(&mut self, _app: &mut SlideApp, _ctx: &egui::Context) {}
    /// A right-hand side panel beside the slide; show one with `egui::Panel::right(..)` or nothing.
    fn side_panel(&mut self, _app: &mut SlideApp, _ui: &mut Ui) {}
    /// Paint over the edited slide (after the slide and the built-in overlays).
    fn paint_slide(&mut self, _app: &mut SlideApp, _ui: &mut Ui, _painter: &Painter, _geo: &SlideGeometry) {}
    /// Items appended to the slide's context menu. Return true to close the menu.
    fn context_menu(&mut self, _app: &mut SlideApp, _ui: &mut Ui) -> bool {
        false
    }
    /// A section under the speaker notes in Presenter View.
    fn presenter_notes(&mut self, _ui: &mut Ui, _doc: &Presentation, _slide: usize) {}
    /// Paint over the current slide's picture in Presenter View (`rect` is the slide on screen).
    fn presenter_slide(&mut self, _ui: &mut Ui, _rect: Rect, _doc: &Presentation, _slide: usize) {}
    /// Ribbon tabs this extension adds (after the built-in ones, before contextual tabs).
    fn ribbon_tabs(&self, _app: &SlideApp) -> Vec<String> {
        Vec::new()
    }
    /// Draws the content of one of its ribbon tabs (groups made with `widgets::group`, like the
    /// built-in tabs). Called only for names it returned from [`UiExtension::ribbon_tabs`].
    fn ribbon_tab(&mut self, _app: &mut SlideApp, _ui: &mut Ui, _name: &str) {}
}

thread_local! {
    static EXTENSIONS: RefCell<Vec<Box<dyn UiExtension>>> = RefCell::new(Vec::new());
}

/// Adds a UI extension (on the UI thread).
pub fn register(ext: Box<dyn UiExtension>) {
    EXTENSIONS.with(|e| e.borrow_mut().push(ext));
}

/// Runs `f` on every extension. The list is taken out while they run, so an extension may use
/// the whole app (and even register another extension).
fn each(mut f: impl FnMut(&mut dyn UiExtension)) {
    let mut list = EXTENSIONS.with(|e| std::mem::take(&mut *e.borrow_mut()));
    for ext in &mut list {
        f(ext.as_mut());
    }
    EXTENSIONS.with(|e| {
        let mut slot = e.borrow_mut();
        list.append(&mut slot);
        *slot = list;
    });
}

pub(crate) fn frame(app: &mut SlideApp, ctx: &egui::Context) {
    each(|x| x.frame(app, ctx));
}

pub(crate) fn side_panels(app: &mut SlideApp, ui: &mut Ui) {
    each(|x| x.side_panel(app, ui));
}

pub(crate) fn paint_slide(app: &mut SlideApp, ui: &mut Ui, painter: &Painter, xf: Xf, slide: Rect, canvas: Rect) {
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p));
    let geo = SlideGeometry { xf, slide, canvas, pointer };
    each(|x| x.paint_slide(app, ui, painter, &geo));
}

/// Appends the extensions' items to the slide's context menu (closing it when one asks).
pub(crate) fn context_menu(app: &mut SlideApp, ui: &mut Ui) {
    let mut close = false;
    each(|x| close |= x.context_menu(app, ui));
    if close {
        ui.close();
    }
}

pub(crate) fn presenter_notes(ui: &mut Ui, doc: &Presentation, slide: usize) {
    each(|x| x.presenter_notes(ui, doc, slide));
}

pub(crate) fn ribbon_tabs(app: &SlideApp) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    each(|x| {
        for t in x.ribbon_tabs(app) {
            if !v.contains(&t) {
                v.push(t);
            }
        }
    });
    v
}

pub(crate) fn ribbon_tab(app: &mut SlideApp, ui: &mut Ui, name: &str) {
    let mut done = false;
    each(|x| {
        if !done && x.ribbon_tabs(app).iter().any(|t| t == name) {
            x.ribbon_tab(app, ui, name);
            done = true;
        }
    });
}

pub(crate) fn presenter_slide(ui: &mut Ui, rect: Rect, doc: &Presentation, slide: usize) {
    each(|x| x.presenter_slide(ui, rect, doc, slide));
}
