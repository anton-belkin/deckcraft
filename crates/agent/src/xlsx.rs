//! Just enough of a `.xlsx` reader to check a linked range: the workbook's sheets and defined
//! names (a hidden name anchors the range, so it follows inserted rows and columns), shared
//! strings, and the cached values of the cells in a range. Every size is capped: the file is
//! input.

use std::io::Read;

use deckcraft_pptx::{XmlEl, parse_xml};
use serde_json::{Value as Json, json};

/// Largest workbook read (bytes on disk).
pub const MAX_FILE: u64 = 200 * 1024 * 1024;
/// Largest part inflated.
const MAX_PART: u64 = 256 * 1024 * 1024;
/// Most cells read from a range.
pub const MAX_CELLS: usize = 20_000;

/// A zero-based cell range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub r0: u32,
    pub c0: u32,
    pub r1: u32,
    pub c1: u32,
}

impl Range {
    pub fn rows(&self) -> usize {
        (self.r1 - self.r0) as usize + 1
    }
    pub fn cols(&self) -> usize {
        (self.c1 - self.c0) as usize + 1
    }
    pub fn a1(&self) -> String {
        let one = col_name(self.c0) + &(self.r0 + 1).to_string();
        if self.r0 == self.r1 && self.c0 == self.c1 { one } else { format!("{one}:{}{}", col_name(self.c1), self.r1 + 1) }
    }
}

pub fn col_name(mut c: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `B4` → (row 3, col 1).
pub fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || letters.len() > 3 || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut col: u32 = 0;
    for ch in letters.to_ascii_uppercase().bytes() {
        col = col.checked_mul(26)?.checked_add(u32::from(ch - b'A') + 1)?;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > 1_048_576 || col == 0 || col > 16_384 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// `B4:D9` or `B4`.
pub fn parse_range(s: &str) -> Option<Range> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = parse_cell(a)?;
    let (r1, c1) = parse_cell(b)?;
    Some(Range { r0: r0.min(r1), c0: c0.min(c1), r1: r0.max(r1), c1: c0.max(c1) })
}

/// `'My sheet'!$B$4:$D$9` → ("My sheet", range).
pub fn parse_ref(s: &str) -> Option<(String, Range)> {
    let s = s.trim().trim_start_matches('=');
    let i = s.rfind('!')?;
    let raw = s.get(..i)?;
    let sheet =
        if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') { raw.get(1..raw.len() - 1)?.replace("''", "'") } else { raw.to_string() };
    Some((sheet, parse_range(s.get(i + 1..)?)?))
}

pub struct Workbook {
    zip: zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    /// (name, part path).
    pub sheets: Vec<(String, String)>,
    /// (name, formula, sheet scope).
    pub names: Vec<(String, String, Option<usize>)>,
    shared: Option<Vec<String>>,
}

/// The cells of a range as read.
#[derive(Clone, Debug, PartialEq)]
pub struct Cells {
    pub sheet: String,
    pub range: Range,
    /// Rows of values: numbers, strings, booleans, null for empty; errors as `"#DIV/0!"` strings.
    pub values: Vec<Vec<Json>>,
}

fn part(zip: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Option<Vec<u8>> {
    let f = zip.by_name(name).ok()?;
    let mut out = Vec::new();
    f.take(MAX_PART).read_to_end(&mut out).ok()?;
    Some(out)
}

fn resolve(base: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

impl Workbook {
    pub fn read(bytes: Vec<u8>) -> Result<Workbook, String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a workbook: {e}"))?;
        let wb_xml = part(&mut zip, "xl/workbook.xml").ok_or("not a workbook: no xl/workbook.xml")?;
        let wb = parse_xml(&wb_xml).map_err(|e| format!("workbook.xml: {e}"))?;
        let rels_xml = part(&mut zip, "xl/_rels/workbook.xml.rels").unwrap_or_default();
        let rels = parse_xml(&rels_xml).ok();
        let target = |id: &str| -> Option<String> {
            let r = rels.as_ref()?;
            r.root.children_named("Relationship").find(|x| x.attr("Id") == Some(id)).and_then(|x| x.attr("Target")).map(|t| resolve("xl", t))
        };
        let mut sheets = vec![];
        if let Some(list) = wb.root.child("sheets") {
            for s in list.children_named("sheet").take(10_000) {
                let (Some(name), Some(rid)) = (s.attr("name"), s.attr("r:id")) else { continue };
                if let Some(t) = target(rid) {
                    sheets.push((name.to_string(), t));
                }
            }
        }
        let mut names = vec![];
        if let Some(list) = wb.root.child("definedNames") {
            for n in list.children_named("definedName").take(100_000) {
                let Some(name) = n.attr("name") else { continue };
                let scope = n.attr("localSheetId").and_then(|v| v.parse().ok());
                names.push((name.to_string(), n.text(), scope));
            }
        }
        Ok(Workbook { zip, sheets, names, shared: None })
    }

    /// Where a defined name points (`Sheet!B4:D9`), when it is a plain reference.
    pub fn name(&self, name: &str) -> Option<(String, Range)> {
        let (_, f, _) = self.names.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name))?;
        if f.contains("#REF!") {
            return None;
        }
        parse_ref(f)
    }

    fn shared_strings(&mut self) -> &[String] {
        if self.shared.is_none() {
            let mut list = vec![];
            if let Some(bytes) = part(&mut self.zip, "xl/sharedStrings.xml")
                && let Ok(doc) = parse_xml(&bytes)
            {
                for si in doc.root.children_named("si").take(5_000_000) {
                    // Rich text runs and plain `<t>`; phonetic runs (`rPh`) aren't the text.
                    let mut s = String::new();
                    for c in si.elements() {
                        match c.local() {
                            "t" => s.push_str(&c.text()),
                            "r" => {
                                if let Some(t) = c.child("t") {
                                    s.push_str(&t.text());
                                }
                            }
                            _ => {}
                        }
                    }
                    list.push(s);
                }
            }
            self.shared = Some(list);
        }
        self.shared.as_deref().unwrap_or(&[])
    }

    /// The cached values of a range on a sheet.
    pub fn cells(&mut self, sheet: &str, range: Range) -> Result<Cells, String> {
        if range.rows().saturating_mul(range.cols()) > MAX_CELLS {
            return Err(format!("range too large ({} cells max)", MAX_CELLS));
        }
        let path =
            self.sheets.iter().find(|(n, _)| n.eq_ignore_ascii_case(sheet)).map(|(_, p)| p.clone()).ok_or_else(|| format!("no sheet `{sheet}`"))?;
        let bytes = part(&mut self.zip, &path).ok_or_else(|| format!("sheet `{sheet}` can't be read"))?;
        let doc = parse_xml(&bytes).map_err(|e| format!("sheet `{sheet}`: {e}"))?;
        let mut values = vec![vec![Json::Null; range.cols()]; range.rows()];
        let mut raw: Vec<(usize, usize, &XmlEl)> = vec![];
        if let Some(data) = doc.root.child("sheetData") {
            for row in data.children_named("row") {
                let rn = row.attr("r").and_then(|v| v.parse::<u32>().ok());
                if let Some(rn) = rn
                    && (rn == 0 || rn - 1 < range.r0 || rn - 1 > range.r1)
                {
                    continue;
                }
                for c in row.children_named("c") {
                    let Some((r, col)) = c.attr("r").and_then(parse_cell) else { continue };
                    if r < range.r0 || r > range.r1 || col < range.c0 || col > range.c1 {
                        continue;
                    }
                    raw.push(((r - range.r0) as usize, (col - range.c0) as usize, c));
                }
            }
        }
        let needs_shared = raw.iter().any(|(_, _, c)| c.attr("t") == Some("s"));
        let shared: Vec<String> = if needs_shared { self.shared_strings().to_vec() } else { vec![] };
        for (r, c, el) in raw {
            let v = el.child("v").map(|v| v.text());
            let value = match el.attr("t").unwrap_or("n") {
                "s" => v.and_then(|i| i.trim().parse::<usize>().ok()).and_then(|i| shared.get(i).cloned()).map(Json::String).unwrap_or(Json::Null),
                "str" => v.map(Json::String).unwrap_or(Json::Null),
                "inlineStr" => el.child("is").map(|x| Json::String(x.text())).unwrap_or(Json::Null),
                "b" => v.map(|x| json!(x.trim() == "1")).unwrap_or(Json::Null),
                "e" => v.map(Json::String).unwrap_or(Json::Null),
                _ => v.and_then(|x| x.trim().parse::<f64>().ok()).filter(|f| f.is_finite()).map(|f| json!(f)).unwrap_or(Json::Null),
            };
            if let Some(slot) = values.get_mut(r).and_then(|row| row.get_mut(c)) {
                *slot = value;
            }
        }
        Ok(Cells { sheet: sheet.to_string(), range, values })
    }
}

/// Reads a range of a workbook file, through its anchor name when it still has it.
/// Returns the cells and whether the anchor was found.
pub fn read_range(bytes: Vec<u8>, anchor: &str, sheet: &str, range: &str) -> Result<(Cells, bool), String> {
    let mut wb = Workbook::read(bytes)?;
    if !anchor.is_empty() {
        if let Some((sh, r)) = wb.name(anchor) {
            return Ok((wb.cells(&sh, r)?, true));
        }
        if wb.names.iter().any(|(n, _, _)| n.eq_ignore_ascii_case(anchor)) {
            return Err("the linked cells were deleted in the workbook".into());
        }
    }
    let r = parse_range(range).ok_or_else(|| format!("not a range: `{range}`"))?;
    Ok((wb.cells(sheet, r)?, false))
}

/// A minimal workbook, for tests and examples: one or more sheets of values and defined names.
/// (`values`: rows of numbers, strings, booleans or null, starting at A1.)
pub fn build(sheets: &[(&str, Vec<Vec<Json>>)], names: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut out);
        let opt = zip::write::SimpleFileOptions::default();
        let mut file = |name: &str, body: String| {
            let _ = z.start_file(name, opt);
            let _ = z.write_all(body.as_bytes());
        };
        let mut ct = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>",
        );
        for i in 0..sheets.len() {
            ct.push_str(&format!(
                "<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
                i + 1
            ));
        }
        ct.push_str("</Types>");
        file("[Content_Types].xml", ct);
        file(
            "_rels/.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".into(),
        );
        let mut wb = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
        );
        let mut rels = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for (i, (name, _)) in sheets.iter().enumerate() {
            wb.push_str(&format!("<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>", esc(name), i + 1, i + 1));
            rels.push_str(&format!(
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
                i + 1,
                i + 1
            ));
        }
        wb.push_str("</sheets>");
        if !names.is_empty() {
            wb.push_str("<definedNames>");
            for (n, f) in names {
                wb.push_str(&format!("<definedName name=\"{}\" hidden=\"1\">{}</definedName>", esc(n), esc(f)));
            }
            wb.push_str("</definedNames>");
        }
        wb.push_str("</workbook>");
        rels.push_str("</Relationships>");
        file("xl/workbook.xml", wb);
        file("xl/_rels/workbook.xml.rels", rels);
        for (i, (_, rows)) in sheets.iter().enumerate() {
            let mut x = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
            );
            for (r, row) in rows.iter().enumerate() {
                x.push_str(&format!("<row r=\"{}\">", r + 1));
                for (c, v) in row.iter().enumerate() {
                    let at = format!("{}{}", col_name(c as u32), r + 1);
                    match v {
                        Json::Number(n) => x.push_str(&format!("<c r=\"{at}\"><v>{n}</v></c>")),
                        Json::String(s) => x.push_str(&format!("<c r=\"{at}\" t=\"inlineStr\"><is><t>{}</t></is></c>", esc(s))),
                        Json::Bool(b) => x.push_str(&format!("<c r=\"{at}\" t=\"b\"><v>{}</v></c>", u8::from(*b))),
                        _ => {}
                    }
                }
                x.push_str("</row>");
            }
            x.push_str("</sheetData></worksheet>");
            file(&format!("xl/worksheets/sheet{}.xml", i + 1), x);
        }
        let _ = z.finish();
    }
    out.into_inner()
}
