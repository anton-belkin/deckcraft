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
}

impl Index {
    pub fn of(doc: &Presentation) -> Index {
        let mut ix = Index::default();
        for (i, sl) in doc.slides.iter().enumerate() {
            if let Some(k) = key_of(&sl.tags) {
                ix.slides.entry(k.to_string()).or_insert(i);
            }
            walk(&sl.shapes, &mut |sh, _| {
                if let Some(k) = key_of(&sh.tags) {
                    ix.shapes.entry(k.to_string()).or_insert((i, sh.id));
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

/// After a command: remembers where every anchor is, puts back keys an undo took away, and drops
/// anchors nothing uses.
pub fn refresh(d: &mut DocState) {
    let l = layer::get(d);
    if l.anchors.is_empty() {
        return;
    }
    let doc = d.doc.clone();
    let ix = Index::of(&doc);
    let unused = l.unused_anchors();
    let mut missing: Vec<(usize, String, Option<(ShapeId, String)>)> = vec![];
    let mut seen: Vec<(String, Option<Place>)> = vec![];
    for a in &l.anchors {
        let place = resolve(&doc, &ix, a);
        if let Some(p) = &place {
            let slide_missing = !ix.slides.contains_key(&a.slide_key);
            let shape_missing = !a.shape_key.is_empty() && !ix.shapes.contains_key(&a.shape_key);
            if slide_missing || shape_missing {
                missing.push((p.slide, a.slide_key.clone(), p.shape.map(|id| (id, a.shape_key.clone()))));
            }
        }
        seen.push((a.id.clone(), place));
    }
    let changed = seen.iter().any(|(id, p)| match (l.anchor(id), p) {
        (Some(a), Some(p)) => {
            let sl = doc.slides.get(p.slide);
            a.slide != p.slide || a.lost || sl.is_some_and(|s| s.id.0 != a.slide_id) || p.shape.is_some_and(|s| s.0 != a.shape_id)
        }
        (Some(a), None) => !a.lost,
        _ => false,
    });
    if changed || !unused.is_empty() {
        layer::update(d, |l| {
            for (id, p) in &seen {
                let Some(a) = l.anchors.iter_mut().find(|a| &a.id == id) else { continue };
                match p {
                    Some(p) => {
                        a.slide = p.slide;
                        a.lost = false;
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
