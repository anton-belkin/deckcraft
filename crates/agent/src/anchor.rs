//! Anchors: hidden keys in the tags (`p:custDataLst`) of slides and shapes, which DeckCraft and
//! PowerPoint keep with the slide or shape through renumbering on open, reordering, copy and paste
//! and a round trip. The last known slide and shape ids are kept too: an undo that takes a key
//! away gets it back.

use std::collections::HashMap;
use std::sync::Arc;

use deckcraft_engine::{DocState, Result, Session};
use deckcraft_model::{Presentation, Shape, ShapeId, ShapeKind, walk};

use crate::layer::{self, Anchor, Layer};

/// Tag name of the hidden key on slides and shapes.
pub const KEY_TAG: &str = "DECKCRAFT_AGENT_KEY";

/// A resolved place in the presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// Slide index (0-based).
    pub slide: usize,
    /// A shape on it (`None`: the slide itself).
    pub shape: Option<ShapeId>,
    /// Words in the shape's text (empty: the whole shape).
    pub text: String,
    /// A table cell (row, column).
    pub cell: Option<(usize, usize)>,
}

impl Place {
    pub fn slide_only(slide: usize) -> Self {
        Place { slide, ..Default::default() }
    }

    /// `Slide 3`, `Slide 3 › Title 1`, `Slide 3 › Revenue table › cell 2,3`, `… › “42%”`.
    pub fn label(&self, doc: &Presentation) -> String {
        let mut s = format!("Slide {}", self.slide + 1);
        if let Some(id) = self.shape
            && let Some(sh) = doc.slides.get(self.slide).and_then(|sl| sl.shape(id))
        {
            s.push_str(" › ");
            s.push_str(&shape_label(sh));
        }
        if let Some((r, c)) = self.cell {
            s.push_str(&format!(" › cell {},{}", r + 1, c + 1));
        }
        if !self.text.is_empty() {
            let t: String = self.text.chars().take(40).collect();
            s.push_str(&format!(" › “{t}”"));
        }
        s
    }

    /// The href of a link to this place: `slide:3` or `shape:3/12`.
    pub fn href(&self) -> String {
        match self.shape {
            Some(id) => format!("shape:{}/{}", self.slide + 1, id.0),
            None => format!("slide:{}", self.slide + 1),
        }
    }
}

/// A shape's name, or its kind when it has none.
pub fn shape_label(sh: &Shape) -> String {
    if sh.name.trim().is_empty() { format!("{} {}", sh.kind_name(), sh.id.0) } else { sh.name.clone() }
}

pub fn key_of(tags: &[(String, String)]) -> Option<&str> {
    tags.iter().find(|(n, _)| n == KEY_TAG).map(|(_, v)| v.as_str())
}

fn set_key(tags: &mut Vec<(String, String)>, key: &str) {
    tags.retain(|(n, _)| n != KEY_TAG);
    tags.push((KEY_TAG.to_string(), key.to_string()));
}

/// Where every key is in a presentation.
#[derive(Default)]
pub struct Index {
    slides: HashMap<String, usize>,
    shapes: HashMap<String, (usize, ShapeId)>,
    /// Every place a key is found (duplicated slides and shapes carry it twice).
    all_slides: HashMap<String, Vec<usize>>,
    all_shapes: HashMap<String, Vec<(usize, ShapeId)>>,
}

impl Index {
    pub fn of(doc: &Presentation) -> Index {
        let mut ix = Index::default();
        for (i, sl) in doc.slides.iter().enumerate() {
            if let Some(k) = key_of(&sl.tags) {
                ix.slides.entry(k.to_string()).or_insert(i);
                ix.all_slides.entry(k.to_string()).or_default().push(i);
            }
            walk(&sl.shapes, &mut |sh, _| {
                if let Some(k) = key_of(&sh.tags) {
                    ix.shapes.entry(k.to_string()).or_insert((i, sh.id));
                    ix.all_shapes.entry(k.to_string()).or_default().push((i, sh.id));
                }
            });
        }
        ix
    }
}

/// Where an anchor is now: by its keys, else where it was last seen.
pub fn resolve(doc: &Presentation, ix: &Index, a: &Anchor) -> Option<Place> {
    let by_id = || doc.slides.iter().position(|s| s.id.0 == a.slide_id);
    let slide = ix.slides.get(&a.slide_key).copied().or_else(|| if a.lost { None } else { by_id() });
    let place = |slide: usize, shape: Option<ShapeId>| Place { slide, shape, text: a.text.clone(), cell: a.cell };
    if a.shape_key.is_empty() {
        return slide.map(|s| place(s, None));
    }
    if let Some((s, id)) = ix.shapes.get(&a.shape_key) {
        return Some(place(*s, Some(*id)));
    }
    if a.lost {
        return None;
    }
    let s = slide?;
    let sl = doc.slides.get(s)?;
    let sh = sl.shape(ShapeId(a.shape_id))?;
    // A shape that took the id over carries its own key: it isn't ours.
    if key_of(&sh.tags).is_some_and(|k| k != a.shape_key) {
        return None;
    }
    Some(place(s, Some(sh.id)))
}

/// Resolves an anchor of a document (by its id in the layer).
pub fn resolve_id(d: &DocState, l: &Layer, id: &str) -> Option<Place> {
    let a = l.anchor(id)?;
    resolve(&d.doc, &Index::of(&d.doc), a)
}

fn fresh_key(l: &mut Layer) -> String {
    let n = l.fresh_id("");
    format!("k{:x}{n}", crate::now_ms() & 0xff_ffff)
}

/// Anchors `place`, reusing an anchor already on exactly that place. Puts the hidden keys on the
/// slide and the shape (no undo step).
pub fn anchor_at(s: &mut Session, place: &Place) -> Result<String> {
    let d = s.doc()?;
    let doc = d.doc.clone();
    let sl = doc.slides.get(place.slide).ok_or_else(|| crate::bad("agent", format!("no slide {}", place.slide + 1)))?;
    let shape = match place.shape {
        Some(id) => Some(sl.shape(id).ok_or_else(|| crate::bad("agent", format!("no shape {id} on slide {}", place.slide + 1)))?),
        None => None,
    };
    let l = layer::get(d);
    let ix = Index::of(&doc);
    if let Some(a) = l.anchors.iter().find(|a| resolve(&doc, &ix, a).as_ref() == Some(place)) {
        return Ok(a.id.clone());
    }
    let (slide_key, shape_key, id) = layer::update(d, |l| {
        let sk = key_of(&sl.tags).map(str::to_string).unwrap_or_else(|| fresh_key(l));
        let hk = shape.map(|sh| key_of(&sh.tags).map(str::to_string).unwrap_or_else(|| fresh_key(l))).unwrap_or_default();
        (sk, hk, l.fresh_id("a"))
    });
    let anchor = Anchor {
        id: id.clone(),
        slide_key: slide_key.clone(),
        shape_key: shape_key.clone(),
        text: place.text.clone(),
        cell: place.cell,
        slide: place.slide,
        slide_id: sl.id.0,
        shape_id: shape.map_or(0, |sh| sh.id.0),
        shape_name: shape.map(shape_label).unwrap_or_default(),
        lost: false,
        was: String::new(),
    };
    layer::update(s.doc()?, |l| l.anchors.push(anchor));
    let needs_slide = key_of(&sl.tags) != Some(slide_key.as_str());
    let needs_shape = shape.is_some_and(|sh| key_of(&sh.tags) != Some(shape_key.as_str()));
    if needs_slide || needs_shape {
        let st = s.doc_mut()?;
        put_keys(Arc::make_mut(&mut st.doc), place.slide, &slide_key, place.shape.map(|id| (id, shape_key.as_str())));
        st.revision += 1;
    }
    Ok(id)
}

fn put_keys(doc: &mut Presentation, slide: usize, slide_key: &str, shape: Option<(ShapeId, &str)>) {
    let Some(sl) = doc.slides.get_mut(slide) else { return };
    let sl = Arc::make_mut(sl);
    if key_of(&sl.tags) != Some(slide_key) {
        set_key(&mut sl.tags, slide_key);
    }
    if let Some((id, key)) = shape
        && let Some(sh) = sl.shape_mut(id)
        && key_of(&sh.tags) != Some(key)
    {
        set_key(&mut sh.tags, key);
    }
}

thread_local! {
    /// The document and layer each document's anchors were last refreshed against.
    static REFRESHED: std::cell::RefCell<HashMap<u64, (usize, u64)>> = std::cell::RefCell::new(HashMap::new());
}

/// After a command: remembers where every anchor is (and, while it exists, where it sits, to say
/// later where it was), puts back keys an undo took away, gives duplicated slides and shapes keys
/// of their own with places in the same topics, drops places of copies that went away, and drops
/// anchors nothing uses.
pub fn refresh(d: &mut DocState) {
    let l = layer::get(d);
    if l.anchors.is_empty() {
        return;
    }
    let stamp = (Arc::as_ptr(&d.doc) as usize, l.revision);
    if REFRESHED.with(|r| r.borrow().get(&d.uid) == Some(&stamp)) {
        return;
    }
    duplicates(d);
    let l = layer::get(d);
    let doc = d.doc.clone();
    let ix = Index::of(&doc);
    let unused = l.unused_anchors();
    let mut missing: Vec<(usize, String, Option<(ShapeId, String)>)> = vec![];
    let mut seen: Vec<(String, Option<Place>, String)> = vec![];
    for a in &l.anchors {
        let place = resolve(&doc, &ix, a);
        let mut was = a.was.clone();
        if let Some(p) = &place {
            let slide_missing = !ix.slides.contains_key(&a.slide_key);
            let shape_missing = !a.shape_key.is_empty() && !ix.shapes.contains_key(&a.shape_key);
            if slide_missing || shape_missing {
                missing.push((p.slide, a.slide_key.clone(), p.shape.map(|id| (id, a.shape_key.clone()))));
            }
            was = crate::topics::whereabouts(&doc, p);
        }
        seen.push((a.id.clone(), place, was));
    }
    let changed = seen.iter().any(|(id, p, was)| match (l.anchor(id), p) {
        (Some(a), Some(p)) => {
            let sl = doc.slides.get(p.slide);
            a.slide != p.slide || a.lost || &a.was != was || sl.is_some_and(|s| s.id.0 != a.slide_id) || p.shape.is_some_and(|s| s.0 != a.shape_id)
        }
        (Some(a), None) => !a.lost,
        _ => false,
    });
    // Copies whose shape went away (an undone duplicate) go with it.
    let gone_copies: Vec<String> = l
        .topics
        .iter()
        .flat_map(|t| t.places.iter())
        .filter(|p| !p.copy_of.is_empty() && p.anchors.iter().all(|a| seen.iter().any(|(id, w, _)| id == a && w.is_none())))
        .map(|p| p.id.clone())
        .collect();
    if changed || !unused.is_empty() || !gone_copies.is_empty() {
        layer::update(d, |l| {
            for (id, p, was) in &seen {
                let Some(a) = l.anchors.iter_mut().find(|a| &a.id == id) else { continue };
                match p {
                    Some(p) => {
                        a.slide = p.slide;
                        a.lost = false;
                        a.was = was.clone();
                        if let Some(sl) = doc.slides.get(p.slide) {
                            a.slide_id = sl.id.0;
                            if let Some(sh) = p.shape.and_then(|id| sl.shape(id)) {
                                a.shape_id = sh.id.0;
                                a.shape_name = shape_label(sh);
                            }
                        }
                    }
                    None => a.lost = true,
                }
            }
            for t in &mut l.topics {
                t.places.retain(|p| !gone_copies.contains(&p.id));
            }
            l.anchors.retain(|a| !unused.contains(&a.id));
        });
    }
    if !missing.is_empty() {
        let doc = Arc::make_mut(&mut d.doc);
        for (slide, sk, shape) in &missing {
            put_keys(doc, *slide, sk, shape.as_ref().map(|(id, k)| (*id, k.as_str())));
        }
        d.revision += 1;
    }
    let stamp = (Arc::as_ptr(&d.doc) as usize, layer::get(d).revision);
    REFRESHED.with(|r| r.borrow_mut().insert(d.uid, stamp));
}

/// A slide or shape that carries a key another one already has was duplicated (Duplicate, copy
/// and paste, a duplicated slide): it gets a key of its own, and every place on the original gets
/// a place on the copy in the same topic (TOPICS.md §2a). The occurrence the anchor last saw keeps
/// the key.
fn duplicates(d: &mut DocState) {
    let doc = d.doc.clone();
    let ix = Index::of(&doc);
    let l = layer::get(d);
    let mut slide_dups: Vec<(String, usize)> = vec![];
    for (k, at) in &ix.all_slides {
        if at.len() < 2 || !l.anchors.iter().any(|a| &a.slide_key == k) {
            continue;
        }
        let keeper = l
            .anchors
            .iter()
            .find(|a| &a.slide_key == k)
            .and_then(|a| at.iter().copied().find(|i| doc.slides.get(*i).is_some_and(|s| s.id.0 == a.slide_id)))
            .unwrap_or(at[0]);
        slide_dups.extend(at.iter().copied().filter(|i| *i != keeper).map(|i| (k.clone(), i)));
    }
    let mut shape_dups: Vec<(String, usize, ShapeId)> = vec![];
    for (k, at) in &ix.all_shapes {
        if at.len() < 2 || !l.anchors.iter().any(|a| &a.shape_key == k) {
            continue;
        }
        let keeper =
            l.anchors.iter().find(|a| &a.shape_key == k).and_then(|a| at.iter().copied().find(|(_, id)| id.0 == a.shape_id)).unwrap_or(at[0]);
        shape_dups.extend(at.iter().copied().filter(|x| *x != keeper).map(|(i, id)| (k.clone(), i, id)));
    }
    if slide_dups.is_empty() && shape_dups.is_empty() {
        return;
    }
    // New keys for the copies (in the presentation), new anchors and places (in the layer).
    let mut new_slide_keys: Vec<(usize, String, String)> = vec![];
    let mut new_shape_keys: Vec<(usize, ShapeId, String, String)> = vec![];
    layer::update(d, |l| {
        for (old, i) in &slide_dups {
            new_slide_keys.push((*i, old.clone(), fresh_key(l)));
        }
        for (old, i, id) in &shape_dups {
            new_shape_keys.push((*i, *id, old.clone(), fresh_key(l)));
        }
        let mut copies: Vec<(String, String)> = vec![];
        for (i, old, new) in &new_slide_keys {
            let olds: Vec<Anchor> = l.anchors.iter().filter(|a| &a.slide_key == old && a.shape_key.is_empty()).cloned().collect();
            for a in olds {
                let id = l.fresh_id("a");
                l.anchors.push(Anchor { id: id.clone(), slide_key: new.clone(), slide: *i, lost: false, ..a.clone() });
                copies.push((a.id, id));
            }
        }
        for (i, sid, old, new) in &new_shape_keys {
            let slide_key = doc.slides.get(*i).and_then(|s| key_of(&s.tags)).map(str::to_string);
            let slide_key = new_slide_keys.iter().find(|(j, _, _)| j == i).map(|(_, _, k)| k.clone()).or(slide_key).unwrap_or_default();
            let olds: Vec<Anchor> = l.anchors.iter().filter(|a| &a.shape_key == old).cloned().collect();
            for a in olds {
                let id = l.fresh_id("a");
                l.anchors.push(Anchor {
                    id: id.clone(),
                    slide_key: slide_key.clone(),
                    shape_key: new.clone(),
                    slide: *i,
                    shape_id: sid.0,
                    lost: false,
                    ..a.clone()
                });
                copies.push((a.id, id));
            }
        }
        // Each place on an original gains a place on the copy, in the same topic.
        let mut added: Vec<(usize, layer::Place)> = vec![];
        for (ti, t) in l.topics.iter().enumerate() {
            for p in t.places.iter().filter(|p| !p.rejected) {
                let on_copy: Vec<String> = p.anchors.iter().filter_map(|a| copies.iter().find(|(o, _)| o == a).map(|(_, c)| c.clone())).collect();
                if !on_copy.is_empty() {
                    added.push((
                        ti,
                        layer::Place {
                            id: String::new(),
                            anchors: on_copy,
                            reason: p.reason.clone(),
                            by: p.by,
                            rejected: false,
                            copy_of: p.id.clone(),
                        },
                    ));
                }
            }
        }
        for (ti, mut p) in added {
            p.id = l.fresh_id("p");
            if let Some(t) = l.topics.get_mut(ti) {
                t.places.push(p);
            }
        }
    });
    let doc = Arc::make_mut(&mut d.doc);
    for (i, _, new) in &new_slide_keys {
        if let Some(sl) = doc.slides.get_mut(*i) {
            set_key(&mut Arc::make_mut(sl).tags, new);
        }
    }
    for (i, id, _, new) in &new_shape_keys {
        if let Some(sh) = doc.slides.get_mut(*i).and_then(|sl| Arc::make_mut(sl).shape_mut(*id)) {
            set_key(&mut sh.tags, new);
        }
    }
    d.revision += 1;
}

/// The shapes of a slide that hold text, in reading order of the tree (groups included).
pub fn text_shapes(shapes: &[Shape]) -> Vec<&Shape> {
    let mut out = vec![];
    walk(shapes, &mut |sh, _| {
        if sh.text.as_ref().is_some_and(|t| !t.is_empty()) || matches!(sh.kind, ShapeKind::Table(_)) {
            out.push(sh);
        }
    });
    out
}
