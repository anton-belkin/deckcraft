//! Excel → slide links (the contract is `agent-layer/links.md`).
//!
//! A spreadsheet (GridCraft) copies a range as a *linked source*: the HTML on the clipboard
//! carries a JSON payload (`data-artcraft-link`, base64) with the workbook's path and id, the
//! hidden name anchoring the range, the sheet and range, the values and their displayed text.
//! Pasting it here makes a table (values, DeckCraft's formatting) or a picture of the cells, and
//! keeps the link on the shape (tag `DECKCRAFT_LINK`, so it travels with the shape and survives
//! PowerPoint). The link is checked against the source: a running GridCraft is asked first, else
//! the `.xlsx` is read directly; a changed number marks the shape stale, and Update brings the new
//! values in. The workbook learns where its range is used from a registry of uses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use deckcraft_engine::cmd::{base64_decode, base64_encode};
use deckcraft_engine::model::table::Table;
use deckcraft_engine::model::text::{Align, TextBody};
use deckcraft_engine::model::{Shape, ShapeId, ShapeKind};
use deckcraft_engine::{Result, Session};
use deckcraft_geom::Xfrm;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::bad;

/// Tag on a linked shape holding its [`Link`] as JSON.
pub const TAG: &str = "DECKCRAFT_LINK";
/// HTML attribute carrying the payload (base64 of UTF-8 JSON).
pub const HTML_ATTR: &str = "data-artcraft-link";
/// Media type of the payload where a clipboard can carry custom types.
pub const MIME: &str = "application/x-artcraft-link+json";
/// `kind` of a payload.
pub const KIND: &str = "artcraft.link";
/// Most cells pasted as a linked range (a table on a slide is for people to read).
pub const MAX_CELLS: usize = 2_000;
/// Most rows and columns of a pasted table (DeckCraft's table limit).
pub const MAX_SIDE: usize = 75;
/// Default control port of a running GridCraft.
pub const GRIDCRAFT_CONTROL: &str = "127.0.0.1:7979";

/// Where the linked cells come from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Source {
    /// `GridCraft`.
    pub app: String,
    /// Absolute path of the workbook when it was copied.
    pub path: String,
    /// The workbook's own id (kept in the workbook), so a moved or renamed file is recognised.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub workbook_id: String,
    /// Hidden defined name anchoring the range (`_GcLink_l3`): follows inserted rows and columns.
    pub anchor: String,
    pub sheet: String,
    /// `B4:D9` when copied.
    pub range: String,
    /// What the range is about (`Revenue by region`), for people.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Source {
    pub fn place(&self) -> String {
        format!("{}!{}", quote_sheet(&self.sheet), self.range)
    }
    pub fn file_name(&self) -> String {
        std::path::Path::new(&self.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| self.path.clone())
    }
}

pub fn quote_sheet(name: &str) -> String {
    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && !name.starts_with(|c: char| c.is_ascii_digit()) {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

/// What the spreadsheet puts on the clipboard.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Payload {
    pub kind: String,
    pub version: u32,
    pub source: Source,
    /// Rows of values: numbers, strings, booleans; null for empty cells.
    pub values: Vec<Vec<Json>>,
    /// Rows of the cells' text as displayed (number formats applied).
    pub text: Vec<Vec<String>>,
    /// Milliseconds since the Unix epoch.
    pub copied_at: u64,
    /// The cells are marked as client data in the workbook.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Optional picture of the cells (base64 PNG) for picture mode; DeckCraft draws one otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// How a linked range sits on the slide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A table of the values, formatted by DeckCraft.
    #[default]
    Table,
    /// A picture of the cells (reads as a picture anywhere, PowerPoint included).
    Picture,
}

/// The link kept on a shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Link {
    pub v: u32,
    pub id: String,
    pub mode: Mode,
    pub source: Source,
    pub values: Vec<Vec<Json>>,
    pub text: Vec<Vec<String>>,
    pub copied_at: u64,
    pub pasted_at: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub updated_at: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

// ---------------------------------------------------------------- payload

/// The payload in clipboard HTML, if it carries one.
pub fn from_html(html: &str) -> Option<Payload> {
    let at = html.find(HTML_ATTR)?;
    let rest = html.get(at + HTML_ATTR.len()..)?.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = rest.get(1..)?;
    let end = body.find(quote)?;
    let raw: String = body.get(..end)?.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64_decode(&raw)?;
    parse(&String::from_utf8(bytes).ok()?).ok()
}

/// A payload from JSON text.
pub fn parse(json: &str) -> std::result::Result<Payload, String> {
    let p: Payload = serde_json::from_str(json).map_err(|e| format!("not a link payload: {e}"))?;
    validate(p)
}

fn validate(mut p: Payload) -> std::result::Result<Payload, String> {
    if p.kind != KIND {
        return Err(format!("not a link payload (kind `{}`)", p.kind));
    }
    if p.version != 1 {
        return Err(format!("link payload version {} is newer than this DeckCraft understands (1)", p.version));
    }
    if p.values.is_empty() && p.text.is_empty() {
        return Err("the link payload has no cells".into());
    }
    if p.values.is_empty() {
        p.values = p.text.iter().map(|r| r.iter().map(|t| Json::String(t.clone())).collect()).collect();
    }
    let rows = p.values.len();
    let cols = p.values.iter().map(Vec::len).max().unwrap_or(0).max(p.text.iter().map(Vec::len).max().unwrap_or(0));
    if rows > MAX_SIDE || cols > MAX_SIDE || rows.saturating_mul(cols) > MAX_CELLS {
        return Err(format!("{rows}×{cols} cells is too large to paste as a linked table ({MAX_SIDE} rows or columns, {MAX_CELLS} cells at most)"));
    }
    // Square the grids and fill missing text from the values.
    for r in &mut p.values {
        r.resize(cols, Json::Null);
    }
    p.text.resize(rows, vec![]);
    for (r, row) in p.text.iter_mut().enumerate() {
        row.resize(cols, String::new());
        for (c, t) in row.iter_mut().enumerate() {
            if t.is_empty()
                && let Some(v) = p.values.get(r).and_then(|x| x.get(c))
            {
                *t = value_text(v);
            }
        }
    }
    p.source.sheet = p.source.sheet.chars().take(200).collect();
    p.source.range = p.source.range.chars().take(64).collect();
    Ok(p)
}

/// The HTML a spreadsheet puts on the clipboard for a linked copy: a plain table (for any other
/// app) carrying the payload.
pub fn to_html(p: &Payload) -> String {
    let json = serde_json::to_string(p).unwrap_or_default();
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut h = format!("<table {HTML_ATTR}=\"{}\">", base64_encode(json.as_bytes()));
    for row in &p.text {
        h.push_str("<tr>");
        for t in row {
            h.push_str(&format!("<td>{}</td>", esc(t)));
        }
        h.push_str("</tr>");
    }
    h.push_str("</table>");
    h
}

/// The plain-text alternative: tab-separated displayed text.
pub fn to_tsv(p: &Payload) -> String {
    p.text.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n")
}

pub fn value_text(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        Json::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Json::Number(n) => n.as_f64().map(fmt_plain).unwrap_or_else(|| n.to_string()),
        other => other.to_string(),
    }
}

fn fmt_plain(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 { format!("{f:.0}") } else { format!("{}", (f * 1e10).round() / 1e10) }
}

// ---------------------------------------------------------------- on shapes

/// The link on a shape.
pub fn of(sh: &Shape) -> Option<Link> {
    let (_, v) = sh.tags.iter().find(|(n, _)| n == TAG)?;
    serde_json::from_str(v).ok()
}

fn set(sh: &mut Shape, link: &Link) {
    sh.tags.retain(|(n, _)| n != TAG);
    sh.tags.push((TAG.into(), serde_json::to_string(link).unwrap_or_default()));
}

/// Every linked shape of the presentation: (slide index, shape id, link).
pub fn all(doc: &deckcraft_engine::model::Presentation) -> Vec<(usize, ShapeId, Link)> {
    let mut out = vec![];
    for (i, sl) in doc.slides.iter().enumerate() {
        deckcraft_engine::model::walk(&sl.shapes, &mut |sh, _| {
            if let Some(l) = of(sh) {
                out.push((i, sh.id, l));
            }
        });
    }
    out
}

// ---------------------------------------------------------------- paste

fn is_number(v: &Json) -> bool {
    v.is_number()
}

/// Column widths (points) from the longest text in each column.
fn widths(text: &[Vec<String>], font: f64) -> Vec<f64> {
    let cols = text.iter().map(Vec::len).max().unwrap_or(0);
    (0..cols)
        .map(|c| {
            let n = text.iter().filter_map(|r| r.get(c)).map(|t| t.chars().count()).max().unwrap_or(1) as f64;
            (n * font * 0.56 + 18.0).clamp(48.0, 260.0)
        })
        .collect()
}

fn font_size(rows: usize) -> f64 {
    match rows {
        0..=6 => 16.0,
        7..=12 => 13.0,
        _ => 10.0,
    }
}

/// A table holding the cells: numbers right-aligned, the first row as a header.
fn build_table(text: &[Vec<String>], values: &[Vec<Json>], at: Option<Xfrm>, slide: (f64, f64)) -> (Table, Xfrm) {
    let rows = text.len().max(1);
    let font = font_size(rows);
    let mut ws = widths(text, font);
    let rh = font * 1.9;
    let mut w: f64 = ws.iter().sum();
    let max_w = slide.0 * 0.9;
    if w > max_w && w > 0.0 {
        let k = max_w / w;
        ws.iter_mut().for_each(|x| *x *= k);
        w = max_w;
    }
    let h = rh * rows as f64;
    let rect = at.unwrap_or_else(|| Xfrm::new(((slide.0 - w) / 2.0).max(0.0), ((slide.1 - h) / 2.0).max(0.0), w, h));
    let mut t = Table::new(rows, ws.len().max(1), rect.w, rect.h / rows as f64);
    let total: f64 = ws.iter().sum();
    if total > 0.0 {
        t.cols = ws.iter().map(|x| x * rect.w / total).collect();
    }
    for (r, row) in text.iter().enumerate() {
        for (c, s) in row.iter().enumerate() {
            let numeric = values.get(r).and_then(|x| x.get(c)).is_some_and(is_number);
            if let Some(cell) = t.cell_mut(r, c) {
                cell.text = cell_body(s, font, numeric);
            }
        }
    }
    (t, rect)
}

fn cell_body(s: &str, font: f64, right: bool) -> TextBody {
    let mut b = TextBody::from_text(s);
    for p in &mut b.paragraphs {
        if right {
            p.props.align = Some(Align::Right);
        }
        for r in &mut p.runs {
            r.props.size = Some(font);
        }
        p.end_props.size = Some(font);
    }
    b
}

/// Draws the cells as a PNG through DeckCraft's renderer (a table in the deck's style).
fn picture_of(s: &Session, t: &Table, rect: Xfrm) -> Result<Vec<u8>> {
    let st = s.doc()?;
    let mut slide = st.current_slide().cloned().ok_or_else(|| bad("link.paste", "no slide"))?;
    let gid = ShapeId(u32::MAX - 2);
    slide.shapes = vec![Shape { id: gid, xfrm: Some(rect), kind: ShapeKind::Table(t.clone()), ..Default::default() }];
    let img = deckcraft_engine::render::render_shape(&st.doc, &slide, gid, 2.0).ok_or_else(|| bad("link.paste", "the cells could not be drawn"))?;
    Ok(img.to_png())
}

/// Pastes a linked range on the current slide. Returns `{id, link}`.
pub fn paste(s: &mut Session, p: Payload, mode: Mode, rect: Option<Xfrm>) -> Result<Json> {
    let p = validate(p).map_err(|e| bad("link.paste", e))?;
    let size = s.doc()?.doc.slide_size;
    let (table, at) = build_table(&p.text, &p.values, rect, (size.width, size.height));
    let id = match mode {
        Mode::Table => {
            let r = s.execute("insert.table", &json!({"rows": table.rows.len(), "cols": table.cols.len(), "rect": [at.x, at.y, at.w, at.h]}))?;
            let id = shape_id(&r)?;
            s.edit(|doc, sel| {
                let sh = deckcraft_engine::shapes_mut(doc, sel)
                    .and_then(|l| deckcraft_engine::model::find_shape_mut(l, id))
                    .ok_or_else(|| bad("link.paste", "the table is gone"))?;
                if let ShapeKind::Table(t) = &mut sh.kind {
                    let style = std::mem::take(&mut t.style);
                    *t = table.clone();
                    t.style = style;
                }
                Ok(())
            })?;
            id
        }
        Mode::Picture => {
            let png = match p.image.as_deref().and_then(base64_decode) {
                Some(png) => png,
                None => picture_of(s, &table, at)?,
            };
            // The renderer draws an 8 pt margin around the shape.
            let r = s.execute(
                "insert.picture",
                &json!({"data": base64_encode(&png), "name": "Linked cells.png", "rect": [at.x - 8.0, at.y - 8.0, at.w + 16.0, at.h + 16.0]}),
            )?;
            shape_id(&r)?
        }
    };
    let now = crate::now_ms();
    let link = Link {
        v: 1,
        id: format!("l{:x}", now ^ u64::from(id.0) << 40),
        mode,
        source: p.source.clone(),
        values: p.values.clone(),
        text: p.text.clone(),
        copied_at: p.copied_at,
        pasted_at: now,
        updated_at: 0,
        private: p.private,
    };
    let name = format!("Linked {} {}", if mode == Mode::Table { "table" } else { "picture" }, p.source.place());
    s.edit(|doc, sel| {
        let sh = deckcraft_engine::shapes_mut(doc, sel)
            .and_then(|l| deckcraft_engine::model::find_shape_mut(l, id))
            .ok_or_else(|| bad("link.paste", "the shape is gone"))?;
        set(sh, &link);
        sh.name = name.chars().take(120).collect();
        if sh.descr.is_empty() {
            sh.descr = format!("Cells {} of {}", p.source.place(), p.source.file_name());
        }
        Ok(())
    })?;
    if p.private {
        let _ = s.execute("agent.private", &json!({"id": id.0, "on": true}));
    }
    register_uses(s);
    Ok(json!({"id": id.0, "link": link.id, "mode": mode, "source": p.source}))
}

fn shape_id(r: &Json) -> Result<ShapeId> {
    r.get("id").and_then(Json::as_u64).and_then(|n| u32::try_from(n).ok()).map(ShapeId).ok_or_else(|| bad("link.paste", "no shape was made"))
}

// ---------------------------------------------------------------- checking

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Unchecked,
    /// The source says the same.
    Current,
    /// A value changed in the source.
    Stale,
    /// The workbook isn't where it was (or anywhere we looked).
    Missing,
    /// It couldn't be read, or the linked cells are gone.
    Error,
}

/// What a check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Check {
    pub state: State,
    pub message: String,
    /// `gridcraft` (asked the running app) or `file` (read the .xlsx).
    pub via: String,
    /// The workbook that was read.
    pub path: String,
    /// Where the range is now (it moves with inserted rows and columns).
    pub sheet: String,
    pub range: String,
    /// The anchor name was found in the workbook.
    pub anchored: bool,
    /// Changed cells: (row, col, old text, new text) relative to the range.
    pub changed: Vec<(usize, usize, String, String)>,
    pub values: Vec<Vec<Json>>,
    /// Displayed text, when the running spreadsheet gave it (else formatted like the old text).
    pub text: Vec<Vec<String>>,
    pub at: u64,
}

/// Where to look for the workbook: its path, then next to the deck (moved together).
pub fn candidates(src: &Source, deck_path: Option<&str>) -> Vec<String> {
    let mut v = vec![src.path.clone()];
    if let Some(dir) = deck_path.and_then(|d| std::path::Path::new(d).parent()) {
        let name = src.file_name();
        if !name.is_empty() {
            v.push(dir.join(&name).to_string_lossy().to_string());
        }
    }
    v.retain(|p| !p.is_empty());
    v.dedup();
    v
}

pub fn same_value(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
            _ => x == y,
        },
        (Json::Null, Json::String(s)) | (Json::String(s), Json::Null) => s.trim().is_empty(),
        (Json::String(x), Json::String(y)) => x.trim() == y.trim(),
        _ => a == b,
    }
}

/// Compares the link's values with the source's and fills `changed`, `state` and `text`.
pub fn compare(link: &Link, c: &mut Check) {
    let rows = link.values.len().max(c.values.len());
    let cols = link.values.iter().chain(c.values.iter()).map(Vec::len).max().unwrap_or(0);
    let shape_changed = link.values.len() != c.values.len() || link.values.first().map(Vec::len) != c.values.first().map(Vec::len);
    let have_text = c.text.len() == c.values.len();
    let mut text = vec![];
    for r in 0..c.values.len() {
        let mut row = vec![];
        for col in 0..c.values.get(r).map_or(0, Vec::len) {
            let new = c.values.get(r).and_then(|x| x.get(col)).cloned().unwrap_or(Json::Null);
            let old_text = link.text.get(r).and_then(|x| x.get(col)).cloned().unwrap_or_default();
            let old = link.values.get(r).and_then(|x| x.get(col)).cloned().unwrap_or(Json::Null);
            let t = if have_text {
                c.text.get(r).and_then(|x| x.get(col)).cloned().unwrap_or_default()
            } else if same_value(&old, &new) {
                old_text.clone()
            } else {
                format_like(&old_text, &old, &new)
            };
            row.push(t);
        }
        text.push(row);
    }
    for r in 0..rows {
        for col in 0..cols {
            let old = link.values.get(r).and_then(|x| x.get(col)).cloned().unwrap_or(Json::Null);
            let new = c.values.get(r).and_then(|x| x.get(col)).cloned().unwrap_or(Json::Null);
            if !same_value(&old, &new) {
                let ot = link.text.get(r).and_then(|x| x.get(col)).cloned().unwrap_or_default();
                let nt = text.get(r).and_then(|x| x.get(col)).cloned().unwrap_or_default();
                c.changed.push((r, col, ot, nt));
            }
        }
    }
    c.text = text;
    if c.changed.is_empty() && !shape_changed {
        c.state = State::Current;
        c.message = "matches the source".into();
    } else {
        c.state = State::Stale;
        c.message = if shape_changed {
            format!("the source range is now {} rows × {} columns", c.values.len(), c.values.first().map_or(0, Vec::len))
        } else {
            let n = c.changed.len();
            format!("{n} value{} changed in the source", if n == 1 { "" } else { "s" })
        };
    }
}

/// `1,234.5` + 1234.5 → new value 2000 → `2,000.0`. Keeps the old text's prefix and suffix
/// (currency, %, units), its decimals and its thousands separators.
pub fn format_like(old_text: &str, old: &Json, new: &Json) -> String {
    let Some(nv) = new.as_f64() else { return value_text(new) };
    let t = old_text.trim();
    let start = t.find(|c: char| c.is_ascii_digit() || c == '-' || c == '(');
    let end = t.rfind(|c: char| c.is_ascii_digit() || c == ')');
    let (Some(a), Some(b)) = (start, end) else { return fmt_plain(nv) };
    if b < a {
        return fmt_plain(nv);
    }
    let (pre, num, post) = (t.get(..a).unwrap_or(""), t.get(a..=b).unwrap_or(""), t.get(b + 1..).unwrap_or(""));
    let percent = post.trim_start().starts_with('%');
    // A percentage shows the value × 100 when the old text did.
    let scale = match (old.as_f64(), num.replace([',', '(', ')', '-'], "").parse::<f64>()) {
        (Some(ov), Ok(shown)) if percent && ov != 0.0 && ((shown / ov) - 100.0).abs() < 1e-6 => 100.0,
        _ => 1.0,
    };
    let decimals = num.rsplit_once('.').map_or(0, |(_, d)| d.chars().take_while(char::is_ascii_digit).count());
    let grouped = num.contains(',');
    let paren = num.starts_with('(');
    let v = nv * scale;
    let mut digits = format!("{:.*}", decimals, v.abs());
    if grouped {
        let (int, frac) = digits.split_once('.').map(|(i, f)| (i.to_string(), format!(".{f}"))).unwrap_or((digits.clone(), String::new()));
        let mut g = String::new();
        for (i, ch) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                g.push(',');
            }
            g.push(ch);
        }
        digits = g + &frac;
    }
    let num = if v < 0.0 { if paren { format!("({digits})") } else { format!("-{digits}") } } else { digits };
    format!("{pre}{num}{post}")
}

/// Asks a running GridCraft for the linked cells (`link.read`, see links.md). `None` when no
/// GridCraft answers on `addr`, or it doesn't know the command.
#[cfg(not(target_arch = "wasm32"))]
pub fn ask_gridcraft(addr: &str, src: &Source) -> Option<Json> {
    control_call(addr, "engine.execute", json!({"command": "link.read", "params": {"path": src.path, "workbookId": src.workbook_id, "anchor": src.anchor, "sheet": src.sheet, "range": src.range}}))
        .ok()
}

/// One JSON-lines call to an app's control channel, with short timeouts (never blocks a frame for
/// long; a GridCraft that isn't running refuses at once).
#[cfg(not(target_arch = "wasm32"))]
pub fn control_call(addr: &str, method: &str, params: Json) -> std::result::Result<Json, String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    if own_control().is_some_and(|own| own == addr) {
        return Err("that is this app's own control channel".into());
    }
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_millis(250)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(1))).map_err(|e| e.to_string())?;
    let line = json!({"id": 1, "method": method, "params": params}).to_string();
    s.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    BufReader::new(s).take(16 * 1024 * 1024).read_until(b'\n', &mut reply).map_err(|e| e.to_string())?;
    let v: Json = serde_json::from_slice(&reply).map_err(|e| e.to_string())?;
    if v.get("ok").and_then(Json::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Json::Null))
    } else {
        Err(v.get("error").and_then(Json::as_str).unwrap_or("error").to_string())
    }
}

static OWN_CONTROL: Mutex<Option<String>> = Mutex::new(None);

/// Tells the links which control address is this app's own (never called as GridCraft).
pub fn set_own_control(addr: String) {
    if let Ok(mut g) = OWN_CONTROL.lock() {
        *g = Some(addr);
    }
}

fn own_control() -> Option<String> {
    OWN_CONTROL.lock().ok().and_then(|g| g.clone())
}

static GRIDCRAFT_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);
static REGISTRY_OVERRIDE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Points the links at another GridCraft control address (tests, a second instance).
pub fn set_gridcraft_addr(addr: Option<String>) {
    if let Ok(mut g) = GRIDCRAFT_OVERRIDE.lock() {
        *g = addr;
    }
}

/// Points the registry of uses at another file (tests).
pub fn set_registry_path(path: Option<std::path::PathBuf>) {
    if let Ok(mut g) = REGISTRY_OVERRIDE.lock() {
        *g = path;
    }
}

/// Where a running GridCraft listens: set by [`set_gridcraft_addr`], else `GRIDCRAFT_CONTROL`,
/// else 127.0.0.1:7979.
pub fn gridcraft_addr() -> String {
    if let Some(a) = GRIDCRAFT_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return a;
    }
    std::env::var("GRIDCRAFT_CONTROL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| GRIDCRAFT_CONTROL.into())
}

/// Checks a link against its source: a running GridCraft first (at `gridcraft`, when given),
/// else the file.
#[cfg(not(target_arch = "wasm32"))]
pub fn check(link: &Link, deck_path: Option<&str>, gridcraft: Option<&str>) -> Check {
    let mut c = Check { at: crate::now_ms(), sheet: link.source.sheet.clone(), range: link.source.range.clone(), ..Default::default() };
    if let Some(addr) = gridcraft
        && let Some(v) = ask_gridcraft(addr, &link.source)
    {
        let grid = |k: &str| v.get(k).and_then(Json::as_array).cloned().unwrap_or_default();
        c.values = grid("values").into_iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect();
        c.text = grid("text").into_iter().map(|r| r.as_array().map(|a| a.iter().map(value_text).collect()).unwrap_or_default()).collect();
        if !c.values.is_empty() {
            c.via = "gridcraft".into();
            c.path = v.get("path").and_then(Json::as_str).unwrap_or(&link.source.path).to_string();
            c.sheet = v.get("sheet").and_then(Json::as_str).unwrap_or(&link.source.sheet).to_string();
            c.range = v.get("range").and_then(Json::as_str).unwrap_or(&link.source.range).to_string();
            c.anchored = true;
            compare(link, &mut c);
            return c;
        }
    }
    c.via = "file".into();
    let Some(path) = candidates(&link.source, deck_path).into_iter().find(|p| std::path::Path::new(p).is_file()) else {
        c.state = State::Missing;
        c.message = format!("{} isn't where it was ({})", link.source.file_name(), link.source.path);
        return c;
    };
    c.path = path.clone();
    let bytes = match std::fs::metadata(&path) {
        Ok(m) if m.len() > crate::xlsx::MAX_FILE => Err("the workbook is too large to check".to_string()),
        _ => std::fs::read(&path).map_err(|e| e.to_string()),
    };
    let read = bytes.and_then(|b| crate::xlsx::read_range(b, &link.source.anchor, &link.source.sheet, &link.source.range));
    match read {
        Ok((cells, anchored)) => {
            c.values = cells.values;
            c.sheet = cells.sheet;
            c.range = cells.range.a1();
            c.anchored = anchored;
            compare(link, &mut c);
        }
        Err(e) => {
            c.state = State::Error;
            c.message = e;
        }
    }
    c
}

#[cfg(target_arch = "wasm32")]
pub fn check(_link: &Link, _deck_path: Option<&str>, _gridcraft: Option<&str>) -> Check {
    Check { state: State::Unchecked, message: "links are checked in the desktop app".into(), ..Default::default() }
}

// ---------------------------------------------------------------- results kept for the UI

static CHECKS: Mutex<Option<HashMap<String, Check>>> = Mutex::new(None);

/// The last check of a link (by link id).
pub fn last_check(id: &str) -> Option<Check> {
    CHECKS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(id).cloned()))
}

pub fn remember(id: &str, c: Check) {
    if let Ok(mut g) = CHECKS.lock() {
        g.get_or_insert_with(HashMap::new).insert(id.to_string(), c);
    }
}

// ---------------------------------------------------------------- update

/// Brings the source's current values into a linked shape (one undo step when run as a command).
pub fn update(s: &mut Session, id: ShapeId, c: &Check) -> Result<Json> {
    let d = s.doc()?;
    let sh = d.shape(id).ok_or_else(|| bad("link.update", format!("no shape {id} on this slide")))?;
    let mut link = of(sh).ok_or_else(|| bad("link.update", "that shape isn't linked"))?;
    if c.values.is_empty() {
        return Err(bad("link.update", if c.message.is_empty() { "the source couldn't be read".into() } else { c.message.clone() }));
    }
    let xfrm = deckcraft_engine::cmd::xfrm_of(&d.doc, &d.selection, sh);
    let same_size = link.values.len() == c.values.len() && link.values.first().map(Vec::len) == c.values.first().map(Vec::len);
    link.values = c.values.clone();
    link.text = c.text.clone();
    link.source.sheet = c.sheet.clone();
    link.source.range = c.range.clone();
    link.updated_at = crate::now_ms();
    let size = d.doc.slide_size;
    let (table, _) = build_table(&link.text, &link.values, Some(xfrm), (size.width, size.height));
    match link.mode {
        Mode::Table => {
            s.edit(|doc, sel| {
                let sh = deckcraft_engine::shapes_mut(doc, sel)
                    .and_then(|l| deckcraft_engine::model::find_shape_mut(l, id))
                    .ok_or_else(|| bad("link.update", "the shape is gone"))?;
                if let ShapeKind::Table(t) = &mut sh.kind {
                    if same_size {
                        // Keep the table as the presenter formatted it: only the words change.
                        for (r, row) in link.text.iter().enumerate() {
                            for (col, text) in row.iter().enumerate() {
                                if let Some(cell) = t.cell_mut(r, col) {
                                    replace_text(&mut cell.text, text);
                                }
                            }
                        }
                    } else {
                        let style = std::mem::take(&mut t.style);
                        *t = table.clone();
                        t.style = style;
                    }
                }
                set(sh, &link);
                Ok(())
            })?;
        }
        Mode::Picture => {
            let inner = Xfrm::new(xfrm.x + 8.0, xfrm.y + 8.0, (xfrm.w - 16.0).max(8.0), (xfrm.h - 16.0).max(8.0));
            let (table, _) = build_table(&link.text, &link.values, Some(inner), (size.width, size.height));
            let png = picture_of(s, &table, inner)?;
            s.edit(|doc, sel| {
                let mid = deckcraft_engine::model::MediaId(doc.alloc_id().ok_or_else(|| bad("link.update", "the presentation has no ids left"))?);
                doc.media.push(deckcraft_engine::model::MediaItem {
                    id: mid,
                    name: "Linked cells.png".into(),
                    content_type: "image/png".into(),
                    data: Arc::new(png),
                    link: None,
                });
                let sh = deckcraft_engine::shapes_mut(doc, sel)
                    .and_then(|l| deckcraft_engine::model::find_shape_mut(l, id))
                    .ok_or_else(|| bad("link.update", "the shape is gone"))?;
                if let ShapeKind::Picture { fill } = &mut sh.kind {
                    fill.media = mid;
                }
                set(sh, &link);
                Ok(())
            })?;
        }
    }
    let mut fresh = c.clone();
    fresh.state = State::Current;
    fresh.changed.clear();
    fresh.message = "updated from the source".into();
    remember(&link.id, fresh);
    Ok(json!({"id": id.0, "link": link.id, "changed": c.changed.len()}))
}

/// Replaces a text body's words, keeping the first run's and paragraph's formatting.
fn replace_text(b: &mut TextBody, text: &str) {
    let Some(p) = b.paragraphs.first().cloned() else {
        *b = TextBody::from_text(text);
        return;
    };
    let props = p.runs.first().map(|r| r.props.clone()).unwrap_or_else(|| p.end_props.clone());
    let mut fresh = TextBody::from_text(text);
    for np in &mut fresh.paragraphs {
        for r in &mut np.runs {
            r.props = props.clone();
        }
        np.props = p.props.clone();
        np.end_props = p.end_props.clone();
        np.level = p.level;
    }
    b.paragraphs = fresh.paragraphs;
}

// ---------------------------------------------------------------- back-references

/// The registry of where linked ranges are used: one JSON object per line (see links.md).
pub fn registry_path() -> Option<std::path::PathBuf> {
    if let Some(p) = REGISTRY_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("ARTCRAFT_LINKS_FILE") {
        return Some(std::path::PathBuf::from(p));
    }
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Application Support/ArtCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("ArtCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .map(|c| c.join("artcraft"))
    };
    base.map(|b| b.join("links.jsonl"))
}

/// The uses of the active deck's links, as registry lines.
pub fn uses(s: &Session) -> Vec<Json> {
    let Ok(d) = s.doc() else { return vec![] };
    all(&d.doc)
        .into_iter()
        .map(|(slide, id, l)| {
            let sl = d.doc.slides.get(slide);
            let shape_key = sl.and_then(|x| x.shape(id)).and_then(|sh| crate::anchor::key_of(&sh.tags).map(str::to_string));
            json!({
                "event": "use",
                "at": crate::now_ms(),
                "app": "DeckCraft",
                "link": l.id,
                "workbook": l.source.path,
                "workbookId": l.source.workbook_id,
                "anchor": l.source.anchor,
                "sheet": l.source.sheet,
                "range": l.source.range,
                "deck": d.path,
                "deckTitle": d.title(),
                "slide": slide + 1,
                "slideTitle": sl.map(|x| x.title()).unwrap_or_default(),
                "shape": id.0,
                "shapeKey": shape_key,
                "mode": l.mode,
            })
        })
        .collect()
}

/// Appends the deck's uses to the registry and tells a running GridCraft (best effort, off the
/// UI thread).
pub fn register_uses(s: &Session) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let lines = uses(s);
        if lines.is_empty() {
            return;
        }
        let _ = std::thread::Builder::new().name("link-uses".into()).spawn(move || {
            if let Some(path) = registry_path() {
                write_registry(&path, &lines);
            }
            let _ = control_call(&gridcraft_addr(), "engine.execute", json!({"command": "link.addUses", "params": {"uses": lines}}));
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = s;
}

/// Most lines kept in the registry (the oldest go).
const MAX_REGISTRY: usize = 20_000;

#[cfg(not(target_arch = "wasm32"))]
pub fn write_registry(path: &std::path::Path, lines: &[Json]) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let big = std::fs::metadata(path).is_ok_and(|m| m.len() > 8 * 1024 * 1024);
    if big && let Ok(text) = std::fs::read_to_string(path) {
        let keep: Vec<&str> = text.lines().rev().take(MAX_REGISTRY / 2).collect::<Vec<_>>().into_iter().rev().collect();
        let _ = std::fs::write(path, keep.join("\n") + "\n");
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        for l in lines {
            let _ = writeln!(f, "{l}");
        }
    }
}

// ---------------------------------------------------------------- open the source

/// How the source would be (or was) opened.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_source(link: &Link, deck_path: Option<&str>, dry_run: bool) -> std::result::Result<Json, String> {
    let path =
        candidates(&link.source, deck_path).into_iter().find(|p| std::path::Path::new(p).is_file()).unwrap_or_else(|| link.source.path.clone());
    let goto = if link.source.anchor.is_empty() { link.source.place() } else { link.source.anchor.clone() };
    let reveal = json!({"command": "link.reveal", "params": {"path": path, "workbookId": link.source.workbook_id, "anchor": link.source.anchor, "sheet": link.source.sheet, "range": link.source.range}});
    if dry_run {
        return Ok(json!({"path": path, "goto": goto, "control": gridcraft_addr(), "request": reveal}));
    }
    if control_call(&gridcraft_addr(), "engine.execute", reveal).is_ok() {
        return Ok(json!({"how": "gridcraft", "path": path, "goto": goto}));
    }
    // GridCraft isn't running: start it on the file (`--goto`), else the system's default app.
    let bin = std::env::var("GRIDCRAFT_BIN").ok().filter(|b| !b.is_empty());
    if let Some(bin) = bin
        && std::process::Command::new(&bin).arg(&path).arg("--goto").arg(&goto).spawn().is_ok()
    {
        return Ok(json!({"how": "gridcraft-launch", "path": path, "goto": goto}));
    }
    let opened = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&path).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", ""]).arg(&path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&path).spawn()
    };
    opened.map(|_| json!({"how": "system", "path": path, "goto": goto})).map_err(|e| format!("couldn't open {path}: {e}"))
}

#[cfg(target_arch = "wasm32")]
pub fn open_source(_link: &Link, _deck_path: Option<&str>, _dry_run: bool) -> std::result::Result<Json, String> {
    Err("opening the source needs the desktop app".into())
}
