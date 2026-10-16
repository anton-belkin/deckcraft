//! The privacy gate. The presenter marks client data: a whole shape (its text, table cells, alt
//! text) or words in a shape's text. Everything the agent reads goes through a [`Masker`], which
//! puts a pseudonym (`[client-3]`) in place of each marked text, in the marked shape and wherever
//! else the same words appear in the deck (names in titles, notes, other slides). Pseudonyms are
//! stable: the same text always gets the same one, so the agent can still reason about "the client
//! in the title is the one in the table". The agent can't change a marked shape.

use std::collections::HashSet;

use deckcraft_engine::Session;
use deckcraft_model::{Presentation, Shape, ShapeId, ShapeKind, walk};

use crate::anchor::{self, Index};
use crate::layer;

/// Shortest marked text masked outside its own shape (shorter ones, like a lone "5", would hide
/// half the deck).
const MIN_ELSEWHERE: usize = 3;

/// FNV-1a: a stable hash, so pseudonyms survive saving and reopening without storing the text.
pub fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Replaces marked text with pseudonyms.
#[derive(Clone, Debug, Default)]
pub struct Masker {
    /// (text, token), longest first.
    secrets: Vec<(String, String)>,
    /// Shapes with client data (slide index, shape id), groups' children included.
    shapes: HashSet<(usize, ShapeId)>,
    /// Whole shapes marked (everything in them is masked).
    whole: HashSet<(usize, ShapeId)>,
    /// Slides with client data on them.
    slides: HashSet<usize>,
    pub on: bool,
}

/// Every non-empty text a shape holds: its paragraphs, table cells, alt text and title.
pub fn shape_texts(sh: &Shape) -> Vec<String> {
    let mut out = vec![];
    walk(std::slice::from_ref(sh), &mut |x, _| {
        if let Some(t) = &x.text {
            out.extend(t.paragraphs.iter().map(|p| p.text().trim().to_string()));
        }
        if let ShapeKind::Table(t) = &x.kind {
            for row in &t.rows {
                for c in &row.cells {
                    out.push(c.text.text().trim().to_string());
                    out.extend(c.text.paragraphs.iter().map(|p| p.text().trim().to_string()));
                }
            }
        }
        out.push(x.descr.trim().to_string());
        out.push(x.title.trim().to_string());
    });
    out.retain(|t| !t.is_empty());
    out.sort();
    out.dedup();
    out
}

/// The names and figures in a text: runs of capitalised words (not the first word of a sentence)
/// and numbers with three or more digits. `Prepared for Acme Holdings AG, fee 1,250,000` gives
/// `Acme Holdings AG` and `1,250,000`.
pub fn names_and_figures(t: &str) -> Vec<String> {
    let mut out = vec![];
    let mut run: Vec<&str> = vec![];
    let mut sentence_start = true;
    let trim = |w: &str| {
        w.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '"' | '“' | '”' | '\'' | '«' | '»')).to_string()
    };
    let flush = |run: &mut Vec<&str>, out: &mut Vec<String>| {
        if !run.is_empty() {
            let name = trim(&run.join(" "));
            if name.chars().count() >= MIN_ELSEWHERE {
                out.push(name);
            }
            run.clear();
        }
    };
    for w in t.split_whitespace() {
        let core = trim(w);
        let digits = core.chars().filter(char::is_ascii_digit).count();
        if digits >= 3 {
            flush(&mut run, &mut out);
            out.push(core.clone());
        } else if !sentence_start && core.chars().next().is_some_and(char::is_uppercase) {
            run.push(w);
            if w.ends_with([',', ';', ':', ')']) {
                flush(&mut run, &mut out);
            }
        } else {
            flush(&mut run, &mut out);
        }
        sentence_start = w.ends_with(['.', '!', '?', ':']) || w == "-" || w == "–" || w == "—";
    }
    flush(&mut run, &mut out);
    out
}

impl Masker {
    /// No masking (the presenter's own view).
    pub fn off() -> Self {
        Masker::default()
    }

    /// The masker for the active document (gives out pseudonyms for new marked text).
    pub fn new(s: &Session) -> Self {
        let Ok(d) = s.doc() else { return Masker { on: true, ..Default::default() } };
        let l = layer::get(d);
        let ix = Index::of(&d.doc);
        let mut m = Masker { on: true, ..Default::default() };
        let mut texts: Vec<String> = vec![];
        for id in &l.private {
            let Some(place) = l.anchor(id).and_then(|a| anchor::resolve(&d.doc, &ix, a)) else { continue };
            m.slides.insert(place.slide);
            let Some(sl) = d.doc.slides.get(place.slide) else { continue };
            let Some(sh) = place.shape.and_then(|x| sl.shape(x)) else { continue };
            walk(std::slice::from_ref(sh), &mut |x, _| {
                m.shapes.insert((place.slide, x.id));
            });
            if place.text.trim().is_empty() {
                walk(std::slice::from_ref(sh), &mut |x, _| {
                    m.whole.insert((place.slide, x.id));
                });
                let all = shape_texts(sh);
                // Names and figures in it are masked wherever else they appear.
                texts.extend(all.iter().flat_map(|t| names_and_figures(t)));
                texts.extend(all);
            } else {
                texts.push(place.text.trim().to_string());
            }
        }
        texts.sort();
        texts.dedup();
        let tokens: Vec<(String, String)> =
            layer::update(d, |l| texts.iter().map(|t| (t.clone(), format!("[client-{}]", l.pseudonym(&hash(t))))).collect());
        m.secrets = tokens;
        m.secrets.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        m
    }

    pub fn is_private_shape(&self, slide: usize, id: ShapeId) -> bool {
        self.shapes.contains(&(slide, id))
    }

    pub fn has_private(&self, slide: usize) -> bool {
        self.slides.contains(&slide)
    }

    pub fn any(&self) -> bool {
        !self.secrets.is_empty() || !self.shapes.is_empty()
    }

    /// Text from anywhere in the deck (titles, notes, questions): marked words become pseudonyms.
    pub fn text(&self, t: &str) -> String {
        self.replace(t, MIN_ELSEWHERE)
    }

    /// Text inside a shape: every marked text is replaced inside a marked shape.
    pub fn in_shape(&self, slide: usize, id: ShapeId, t: &str) -> String {
        if !self.on {
            return t.to_string();
        }
        if self.whole.contains(&(slide, id)) {
            // Everything in a wholly marked shape: whatever isn't a known text still goes.
            let r = self.replace(t, 1);
            return if self.secrets.iter().any(|(_, tok)| r.contains(tok.as_str())) || r.trim().is_empty() { r } else { "[client data]".into() };
        }
        if self.shapes.contains(&(slide, id)) { self.replace(t, 1) } else { self.text(t) }
    }

    fn replace(&self, t: &str, min: usize) -> String {
        if !self.on || self.secrets.is_empty() || t.is_empty() {
            return t.to_string();
        }
        let mut out = String::with_capacity(t.len());
        let mut i = 0;
        'outer: while i < t.len() {
            let rest = t.get(i..).unwrap_or("");
            for (secret, token) in &self.secrets {
                if secret.chars().count() >= min && rest.starts_with(secret.as_str()) {
                    out.push_str(token);
                    i += secret.len();
                    continue 'outer;
                }
            }
            let Some(c) = rest.chars().next() else { break };
            out.push(c);
            i += c.len_utf8();
        }
        out
    }
}

/// The marked shapes of a presentation as (slide, shape) pairs, for checks without masking.
pub fn private_shapes(s: &Session) -> HashSet<(usize, ShapeId)> {
    let mut m = Masker::new(s);
    std::mem::take(&mut m.shapes)
}

/// Shapes a command would touch: `id`/`ids` params, else the selection (and the text being edited).
pub fn command_targets(s: &Session, params: &serde_json::Value) -> Vec<(usize, ShapeId)> {
    let Ok(d) = s.doc() else { return vec![] };
    let slide = d.selection.slide;
    let mut out: Vec<(usize, ShapeId)> = vec![];
    let id = |v: &serde_json::Value| v.as_u64().and_then(|n| u32::try_from(n).ok()).map(ShapeId);
    if let Some(v) = params.get("id").and_then(id) {
        out.push((slide, v));
    }
    if let Some(a) = params.get("ids").and_then(serde_json::Value::as_array) {
        out.extend(a.iter().filter_map(id).map(|v| (slide, v)));
    }
    if out.is_empty() {
        out.extend(d.selection.shapes.iter().map(|v| (slide, *v)));
        if let Some(t) = &d.selection.text
            && !t.notes
        {
            out.push((slide, t.shape));
        }
        if let Some((t, _, _)) = d.selection.cells {
            out.push((slide, t));
        }
    }
    out
}

/// Does a presentation slide hold a marked shape? (Deleting or replacing it would touch client data.)
pub fn slide_has_private(set: &HashSet<(usize, ShapeId)>, slide: usize) -> bool {
    set.iter().any(|(s, _)| *s == slide)
}

/// Text of a presentation for tests and checks: is `needle` anywhere in what the agent can read?
pub fn leaks(json: &serde_json::Value, needle: &str) -> bool {
    json.to_string().contains(needle)
}

/// All slide texts (for a quick "does the deck mention it" check).
pub fn deck_text(doc: &Presentation) -> String {
    let mut out = String::new();
    for sl in &doc.slides {
        for sh in &sl.shapes {
            for t in shape_texts(sh) {
                out.push_str(&t);
                out.push('\n');
            }
        }
        out.push_str(&sl.notes_text());
    }
    out
}
