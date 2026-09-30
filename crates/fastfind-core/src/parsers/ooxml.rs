//! Office Open XML: `.docx`, `.pptx`, `.xlsx` (and macro/template variants).
//!
//! Parts are streamed through quick-xml from a [`SafeZip`] (bounded decompression), so memory
//! stays constant regardless of document size. Structure is preserved where it helps snippets:
//! Word paragraphs/tables/headers/footers/notes, slide order + titles + notes, sheet names and
//! exact cell references.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::sniff::{sniff, Sniffed};
use super::zipsafe::{resolve_target, SafeZip};
use super::{
    push_xml_ref, DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode,
    TextSink,
};
use crate::model::flags;

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Stream the XML events of a zip part. Returns `Ok(false)` if the part does not exist.
fn for_each_event<F>(zip: &mut SafeZip, part: &str, sink: &mut TextSink, mut f: F) -> ParseResult<bool>
where
    F: FnMut(&Event, &mut TextSink) -> ParseResult<()>,
{
    let Some(entry) = zip.open_entry(part)? else { return Ok(false) };
    let mut reader = Reader::from_reader(BufReader::with_capacity(64 * 1024, entry));
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::with_capacity(8192);
    loop {
        sink.check_deadline()?;
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) if !sink.is_empty() => {
                tracing::debug!(part, error = %e, "ooxml part truncated/malformed");
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if matches!(ev, Event::Eof) {
            break;
        }
        f(&ev, sink)?;
        if sink.is_full() {
            break;
        }
        buf.clear();
    }
    Ok(true)
}

/// Generic event loop over a small XML string (metadata, relationships, workbook).
fn small_xml_events(xml: &str, mut f: impl FnMut(&Event)) {
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        if matches!(ev, Event::Eof) {
            break;
        }
        f(&ev);
        buf.clear();
    }
}

fn local(e: &BytesStart) -> String {
    e.local_name().as_ref().to_string()
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| super::attr_value(&a))
}

/// Value of the namespaced `r:id` attribute.
fn rel_id_attr(e: &BytesStart) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(":id"))
        .and_then(|a| super::attr_value(&a))
}

/// Relationship id → (type, target) for a `.rels` part.
fn read_rels(zip: &mut SafeZip, rels_part: &str) -> ParseResult<HashMap<String, (String, String)>> {
    let mut map = HashMap::new();
    if let Some(xml) = zip.read_to_string(rels_part, 8 << 20)? {
        small_xml_events(&xml, |ev| {
            if let Event::Start(e) | Event::Empty(e) = ev {
                if e.local_name().as_ref() == "Relationship" {
                    if let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target")) {
                        map.insert(id, (attr(e, "Type").unwrap_or_default(), target));
                    }
                }
            }
        });
    }
    Ok(map)
}

/// `docProps/core.xml` Dublin Core metadata.
fn read_core_props(zip: &mut SafeZip) -> ParseResult<DocMeta> {
    let mut meta = DocMeta::default();
    if let Some(xml) = zip.read_to_string("docProps/core.xml", 4 << 20)? {
        let mut current: Option<String> = None;
        small_xml_events(&xml, |ev| match ev {
            Event::Start(e) => current = Some(local(e)),
            Event::End(_) => current = None,
            Event::Text(t) => {
                let v = t.html_content().trim().to_string();
                if v.is_empty() {
                    return;
                }
                match current.as_deref() {
                    Some("title") => meta.title = Some(v),
                    Some("creator") => meta.author = Some(v),
                    Some("subject") => meta.subject = Some(v),
                    Some("keywords") => meta.keywords = Some(v),
                    Some("description") if meta.subject.is_none() => meta.subject = Some(v),
                    _ => {}
                }
            }
            _ => {}
        });
    }
    Ok(meta)
}

fn zip_can_handle(header: &[u8]) -> bool {
    sniff(header) == Sniffed::Zip
}

/// Numeric suffix of a part name (`word/header12.xml` → 12) for stable ordering.
fn part_number(name: &str) -> u32 {
    let stem = name.rsplit('/').next().unwrap_or(name);
    stem.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)
}

fn parts_matching(zip: &SafeZip, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = zip
        .names()
        .into_iter()
        .filter(|n| n.starts_with(prefix) && n.ends_with(".xml") && !n[prefix.len()..].contains('/'))
        .collect();
    v.sort_by_key(|n| part_number(n));
    v
}

// ---------------------------------------------------------------------------------------------
// Word (.docx)
// ---------------------------------------------------------------------------------------------

pub struct DocxParser;

impl DocumentParser for DocxParser {
    fn name(&self) -> &'static str {
        "docx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["docx", "docm", "dotx", "dotm"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        zip_can_handle(header)
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        let mut meta = read_core_props(&mut zip)?;
        meta.flags |= flags::APPROX_PAGES;
        if !zip.contains("word/document.xml") {
            return Err(ParseError::Corrupt("missing word/document.xml".into()));
        }
        let mut pages = WordPages { page: 1, just_broke: false, track: true };
        sink.anchor(LocKind::PageApprox(1));
        word_part(&mut zip, "word/document.xml", sink, &mut pages)?;
        meta.pages = Some(pages.page);
        pages.track = false;
        let sections: [(&str, &str); 5] = [
            ("word/footnotes.xml", "Footnotes"),
            ("word/endnotes.xml", "Endnotes"),
            ("word/comments.xml", "Comments"),
            ("word/header", "Header"),
            ("word/footer", "Footer"),
        ];
        for (part, label) in sections {
            let names = if part.ends_with(".xml") { vec![part.to_string()] } else { parts_matching(&zip, part) };
            for name in names {
                if sink.is_full() {
                    break;
                }
                if zip.contains(&name) {
                    sink.newline();
                    sink.anchor(LocKind::Section(label.into()));
                    word_part(&mut zip, &name, sink, &mut pages)?;
                }
            }
        }
        Ok(meta)
    }
}

struct WordPages {
    page: u32,
    just_broke: bool,
    track: bool,
}

fn word_part(zip: &mut SafeZip, part: &str, sink: &mut TextSink, pages: &mut WordPages) -> ParseResult<()> {
    let mut in_t = false;
    let mut in_tabs = false;
    for_each_event(zip, part, sink, |ev, sink| {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "t" if !empty => in_t = true,
                    "tabs" if !empty => in_tabs = true,
                    "tab" if !in_tabs => sink.tab(),
                    "br" => {
                        if attr(e, "type").as_deref() == Some("page") && pages.track {
                            pages.page += 1;
                            pages.just_broke = true;
                            sink.newline();
                            sink.anchor(LocKind::PageApprox(pages.page));
                        } else {
                            sink.newline();
                        }
                    }
                    "cr" => sink.newline(),
                    // Word records where it last broke pages when rendering; a hard page break
                    // is followed by one of these for the same break, so don't double count.
                    "lastRenderedPageBreak" if pages.track => {
                        if pages.just_broke {
                            pages.just_broke = false;
                        } else {
                            pages.page += 1;
                            sink.anchor(LocKind::PageApprox(pages.page));
                        }
                    }
                    "noBreakHyphen" => sink.push_char('-'),
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "tabs" => in_tabs = false,
                "p" => sink.newline(),
                "tc" => sink.tab(),
                "tr" => sink.newline(),
                _ => {}
            },
            Event::Text(t) if in_t => {
                let s = t.html_content();
                if !s.trim().is_empty() {
                    pages.just_broke = false;
                }
                sink.push_str(&s);
            }
            Event::GeneralRef(r) if in_t => push_xml_ref(sink, r),
            _ => {}
        }
        Ok(())
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// PowerPoint (.pptx)
// ---------------------------------------------------------------------------------------------

pub struct PptxParser;

impl DocumentParser for PptxParser {
    fn name(&self) -> &'static str {
        "pptx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        zip_can_handle(header)
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        let mut meta = read_core_props(&mut zip)?;
        let slides = pptx_slide_order(&mut zip)?;
        if slides.is_empty() && !zip.contains("ppt/presentation.xml") {
            return Err(ParseError::Corrupt("missing ppt/presentation.xml".into()));
        }
        for (i, slide) in slides.iter().enumerate() {
            if sink.is_full() {
                break;
            }
            sink.newline();
            sink.anchor(LocKind::Slide(i as u32 + 1, None));
            let title = pptx_text(&mut zip, slide, sink, true)?;
            if let Some(loc) = sink.last_anchor_mut() {
                if let LocKind::Slide(n, _) = loc.kind {
                    loc.kind = LocKind::Slide(n, title.filter(|t| !t.trim().is_empty()).map(|t| truncate_title(&t)));
                }
            }
            // Speaker notes.
            let dir = slide.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let file = slide.rsplit('/').next().unwrap_or(slide);
            let rels = read_rels(&mut zip, &format!("{dir}/_rels/{file}.rels"))?;
            if let Some((_, target)) = rels.values().find(|(t, _)| t.ends_with("/notesSlide")) {
                let notes = resolve_target(dir, target);
                sink.newline();
                pptx_text(&mut zip, &notes, sink, false)?;
            }
        }
        meta.pages = Some(slides.len() as u32);
        Ok(meta)
    }
}

fn truncate_title(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 80 {
        format!("{}…", t.chars().take(79).collect::<String>())
    } else {
        t
    }
}

/// Slide parts in presentation order (from `sldIdLst`), falling back to numeric order.
fn pptx_slide_order(zip: &mut SafeZip) -> ParseResult<Vec<String>> {
    let rels = read_rels(zip, "ppt/_rels/presentation.xml.rels")?;
    let mut ids = Vec::new();
    if let Some(xml) = zip.read_to_string("ppt/presentation.xml", 16 << 20)? {
        small_xml_events(&xml, |ev| {
            if let Event::Start(e) | Event::Empty(e) = ev {
                if e.local_name().as_ref() == "sldId" {
                    // `id` (numeric) and `r:id` share a local name; the relationship id is the
                    // namespaced one.
                    if let Some(rid) = rel_id_attr(e) {
                        ids.push(rid);
                    }
                }
            }
        });
    }
    let mut slides: Vec<String> = ids
        .iter()
        .filter_map(|id| rels.get(id))
        .filter(|(t, _)| t.ends_with("/slide"))
        .map(|(_, target)| resolve_target("ppt", target))
        .collect();
    if slides.is_empty() {
        slides = parts_matching(zip, "ppt/slides/slide");
    }
    Ok(slides)
}

/// Extract a slide/notes part; returns the title placeholder text when `want_title`.
fn pptx_text(zip: &mut SafeZip, part: &str, sink: &mut TextSink, want_title: bool) -> ParseResult<Option<String>> {
    let mut in_t = false;
    let mut shape_is_title = false;
    let mut title = String::new();
    let mut in_shape = 0usize;
    for_each_event(zip, part, sink, |ev, sink| {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "sp" if !empty => {
                        in_shape += 1;
                        shape_is_title = false;
                    }
                    "ph" => {
                        if let Some(t) = attr(e, "type") {
                            if t == "title" || t == "ctrTitle" {
                                shape_is_title = true;
                            }
                        }
                    }
                    "t" if !empty => in_t = true,
                    "br" => sink.newline(),
                    "tab" => sink.tab(),
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "p" => {
                    sink.newline();
                    if shape_is_title && !title.is_empty() {
                        title.push(' ');
                    }
                }
                "sp" => {
                    in_shape = in_shape.saturating_sub(1);
                    shape_is_title = false;
                }
                "tc" => sink.tab(),
                _ => {}
            },
            Event::Text(t) if in_t => {
                let s = t.html_content();
                if want_title && shape_is_title && title.len() < 512 {
                    title.push_str(&s);
                }
                sink.push_str(&s);
            }
            Event::GeneralRef(r) if in_t => push_xml_ref(sink, r),
            _ => {}
        }
        Ok(())
    })?;
    Ok(want_title.then_some(title))
}

// ---------------------------------------------------------------------------------------------
// Excel (.xlsx)
// ---------------------------------------------------------------------------------------------

pub struct XlsxParser;

impl DocumentParser for XlsxParser {
    fn name(&self) -> &'static str {
        "xlsx"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xlsx", "xlsm", "xltx", "xltm", "xlam"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        zip_can_handle(header)
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        let mut meta = read_core_props(&mut zip)?;
        let sheets = xlsx_sheets(&mut zip)?;
        if sheets.is_empty() {
            return Err(ParseError::Corrupt("workbook has no sheets".into()));
        }
        // Shared strings may be large; cap their total size at twice the text budget.
        let shared = xlsx_shared_strings(&mut zip, sink, (ctx.limits.max_text_bytes as u64).saturating_mul(2).max(1 << 20))?;
        for (name, part) in &sheets {
            if sink.is_full() {
                break;
            }
            sink.newline();
            sink.anchor(LocKind::Sheet(name.clone()));
            sink.push_str(name);
            sink.newline();
            xlsx_sheet(&mut zip, part, &shared, sink)?;
        }
        meta.pages = Some(sheets.len() as u32);
        Ok(meta)
    }
}

/// (sheet name, part path) in workbook order.
fn xlsx_sheets(zip: &mut SafeZip) -> ParseResult<Vec<(String, String)>> {
    let rels = read_rels(zip, "xl/_rels/workbook.xml.rels")?;
    let mut out = Vec::new();
    if let Some(xml) = zip.read_to_string("xl/workbook.xml", 16 << 20)? {
        small_xml_events(&xml, |ev| {
            if let Event::Start(e) | Event::Empty(e) = ev {
                if e.local_name().as_ref() == "sheet" {
                    let name = attr(e, "name").unwrap_or_else(|| format!("Sheet{}", out.len() + 1));
                    if let Some((_, target)) = rel_id_attr(e).and_then(|r| rels.get(&r)) {
                        out.push((name, resolve_target("xl", target)));
                    }
                }
            }
        });
    }
    if out.is_empty() {
        for (i, p) in parts_matching(zip, "xl/worksheets/sheet").into_iter().enumerate() {
            out.push((format!("Sheet{}", i + 1), p));
        }
    }
    Ok(out)
}

fn xlsx_shared_strings(zip: &mut SafeZip, sink: &mut TextSink, budget: u64) -> ParseResult<Vec<String>> {
    let mut strings = Vec::new();
    let mut used = 0u64;
    let mut cur = String::new();
    let mut in_t = false;
    let mut in_phonetic = false;
    let Some(entry) = zip.open_entry("xl/sharedStrings.xml")? else { return Ok(strings) };
    let mut reader = Reader::from_reader(BufReader::with_capacity(64 * 1024, entry));
    let mut buf = Vec::with_capacity(8192);
    loop {
        sink.check_deadline()?;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "si" => cur.clear(),
                "t" => in_t = true,
                "rPh" => in_phonetic = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "si" => {
                    used += cur.len() as u64;
                    strings.push(if used <= budget { std::mem::take(&mut cur) } else { String::new() });
                }
                "t" => in_t = false,
                "rPh" => in_phonetic = false,
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == "si" => strings.push(String::new()),
            Ok(Event::Text(t)) if in_t && !in_phonetic => cur.push_str(&t.html_content()),
            Ok(Event::GeneralRef(r)) if in_t && !in_phonetic => {
                if let Some(c) = super::ref_char(&r) {
                    cur.push(c);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "sharedStrings malformed; using partial table");
                break;
            }
        }
        buf.clear();
    }
    Ok(strings)
}

/// Parse a cell reference like `AB12` into zero-based column and one-based row.
pub fn parse_cell_ref(r: &str) -> Option<(u32, u32)> {
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &r[letters.len()..];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut col = 0u32;
    for c in letters.bytes() {
        col = col * 26 + (c.to_ascii_uppercase() - b'A' + 1) as u32;
    }
    Some((col - 1, digits.parse().ok()?))
}

/// Emits one text line per non-empty row; cells tab-separated at their true column (gaps up to
/// 64 columns padded) and `Row` anchors whenever row numbers jump, so a match offset maps back
/// to an exact cell reference.
pub(crate) struct RowWriter {
    cur_row: Option<u32>,
    line_col: u32,
    next_col: u32,
}

impl RowWriter {
    pub fn new() -> Self {
        Self { cur_row: None, line_col: 0, next_col: 0 }
    }

    pub fn cell(&mut self, sink: &mut TextSink, row: u32, col: Option<u32>, value: &str) {
        let col = col.unwrap_or(self.next_col);
        self.next_col = col + 1;
        if value.trim().is_empty() {
            return;
        }
        if self.cur_row != Some(row) {
            if self.cur_row.is_some() {
                sink.push_char('\n');
            }
            let consecutive = self.cur_row.map(|r| r + 1 == row).unwrap_or(false);
            if !consecutive {
                sink.anchor(LocKind::Row(row));
            }
            self.cur_row = Some(row);
            self.line_col = 0;
        }
        if col > self.line_col {
            for _ in 0..(col - self.line_col).min(64) {
                sink.push_char('\t');
            }
            self.line_col = col;
        }
        // Cell text must stay on one line to keep row arithmetic exact.
        for ch in value.chars() {
            sink.push_char(if ch == '\n' || ch == '\r' || ch == '\t' { ' ' } else { ch });
        }
    }

    pub fn new_row(&mut self) {
        self.next_col = 0;
    }

    pub fn finish(&mut self, sink: &mut TextSink) {
        if self.cur_row.is_some() {
            sink.push_char('\n');
        }
        self.cur_row = None;
    }
}

fn xlsx_sheet(zip: &mut SafeZip, part: &str, shared: &[String], sink: &mut TextSink) -> ParseResult<()> {
    let mut rows = RowWriter::new();
    let mut row_attr: u32 = 0;
    let mut cell: Option<(Option<(u32, u32)>, String)> = None; // (ref, type)
    let mut value = String::new();
    let mut in_value = false;
    let mut in_formula = false;
    for_each_event(zip, part, sink, |ev, sink| {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "row" => {
                        row_attr = attr(e, "r").and_then(|r| r.parse().ok()).unwrap_or(row_attr + 1);
                        rows.new_row();
                    }
                    "c" if !empty => {
                        let r = attr(e, "r").and_then(|r| parse_cell_ref(&r));
                        cell = Some((r, attr(e, "t").unwrap_or_default()));
                        value.clear();
                    }
                    "v" | "t" if !empty && cell.is_some() && !in_formula => in_value = true,
                    "f" if !empty => in_formula = true,
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "v" | "t" => in_value = false,
                "f" => in_formula = false,
                "c" => {
                    if let Some((r, t)) = cell.take() {
                        let text: std::borrow::Cow<str> = match t.as_str() {
                            "s" => value.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).map(|s| s.as_str().into()).unwrap_or_default(),
                            "b" => (if value.trim() == "1" { "TRUE" } else { "FALSE" }).into(),
                            _ => value.as_str().into(),
                        };
                        let (col, row) = match r {
                            Some((c, r)) => (Some(c), r),
                            None => (None, row_attr),
                        };
                        rows.cell(sink, row, col, &text);
                    }
                }
                "sheetData" => rows.finish(sink),
                _ => {}
            },
            Event::Text(t) if in_value => value.push_str(&t.html_content()),
            Event::GeneralRef(r) if in_value => {
                if let Some(c) = super::ref_char(r) {
                    value.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    rows.finish(sink);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_refs() {
        assert_eq!(parse_cell_ref("A1"), Some((0, 1)));
        assert_eq!(parse_cell_ref("AB12"), Some((27, 12)));
        assert_eq!(parse_cell_ref("12"), None);
    }
}
