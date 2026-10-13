//! i-net Clear Reports (formerly i-net Crystal-Clear) report files (`.rpt`): a zip whose
//! `mimetype` entry is `application/crystalclear.report`, with the report in `content.xml` and
//! its title in `meta.xml`. (Classic Crystal Reports `.rpt` files are OLE compound files and are
//! not read: they fall through to name-only indexing.)
//!
//! The text is what someone would look for in a report: the labels and database fields of each
//! area in reading order (top to bottom, then left to right, so a label and the field beside it
//! stay on one line), then the SQL queries, formulas, parameter prompts and database columns.
//! Each block is a section (`Details`, `SQL query`, `Formula: total`…) that snippets show.

use std::io::BufReader;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::sniff::{sniff, Sniffed};
use super::zipsafe::SafeZip;
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct InetReportParser;

const MIMETYPE: &str = "application/crystalclear";
/// Elements whose tops are this close (in twips, 1/1440 inch) share a line of text.
const SAME_LINE: i64 = 120;
/// Cap on one text value (a runaway SQL or formula).
const MAX_VALUE: usize = 1 << 20;

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| super::attr_value(&a))
}

impl DocumentParser for InetReportParser {
    fn name(&self) -> &'static str {
        "inet-report"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rpt"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Zip
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        let mimetype = zip.read_to_string("mimetype", 1024)?.unwrap_or_default();
        if !mimetype.trim().starts_with(MIMETYPE) {
            return Err(ParseError::Unsupported("not an i-net Clear Reports file".into()));
        }
        let meta = read_meta(&mut zip)?;
        let Some(entry) = zip.open_entry("content.xml")? else {
            return Err(ParseError::Corrupt("missing content.xml".into()));
        };
        let mut reader = Reader::from_reader(BufReader::with_capacity(64 * 1024, entry));
        reader.config_mut().check_end_names = false;
        let blocks = read_report(&mut reader, sink)?;
        for (label, text) in blocks {
            if sink.is_full() {
                break;
            }
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            sink.newline();
            sink.anchor(LocKind::Section(label));
            sink.push_str(text);
            sink.newline();
        }
        Ok(meta)
    }
}

/// One element placed in an area: its position and text.
struct Placed {
    y: i64,
    x: i64,
    text: String,
}

/// An open XML element and what was learnt about it.
struct Frame {
    tag: String,
    kind: Option<String>,
    /// `<Name value="…"/>` child (formulas, prompts).
    name: Option<String>,
    x: i64,
    y: i64,
}

/// All text blocks of the report, in the order they are shown: areas first, then queries,
/// formulas, prompts and columns.
fn read_report<R: std::io::BufRead>(reader: &mut Reader<R>, sink: &mut TextSink) -> ParseResult<Vec<(String, String)>> {
    let mut buf = Vec::with_capacity(8192);
    let mut stack: Vec<Frame> = Vec::new();
    let mut layout: Vec<(String, String)> = Vec::new();
    let mut other: Vec<(String, String)> = Vec::new();
    // Open sections (subreports nest them) with the elements placed so far.
    let mut sections: Vec<(String, Vec<Placed>)> = Vec::new();
    // The Text element being read: its position and text.
    let mut text_el: Option<Placed> = None;
    let mut text_depth = 0usize;
    let mut in_text_data = false;
    let mut value = String::new();
    let mut columns: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        sink.check_deadline()?;
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) if !layout.is_empty() || !other.is_empty() => {
                tracing::debug!(error = %e, "report content truncated");
                break;
            }
            Err(e) => return Err(e.into()),
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                let tag = e.local_name().as_ref().to_string();
                let num = |n: &str| attr(e, n).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                match tag.as_str() {
                    "Section" => {
                        if !empty {
                            let label = area_label(&attr(e, "type").unwrap_or_default());
                            sections.push((label, Vec::new()));
                        }
                    }
                    "Element" => {
                        let kind = attr(e, "type").unwrap_or_default();
                        if kind == "Text" && text_el.is_none() && !empty {
                            text_el = Some(Placed { y: num("y_position"), x: num("x_position"), text: String::new() });
                            text_depth = stack.len();
                        }
                    }
                    "Reference" => {
                        // A database or formula field placed in the layout.
                        let in_field = stack.last().is_some_and(|f| f.kind.as_deref() == Some("FieldElement"));
                        if let (true, Some(v)) = (in_field, attr(e, "value")) {
                            let (x, y) = stack.last().map(|f| (f.x, f.y)).unwrap_or_default();
                            if let Some((_, placed)) = sections.last_mut() {
                                placed.push(Placed { y, x, text: format!("{{{v}}}") });
                            }
                        }
                    }
                    "Name" => {
                        if let (Some(top), Some(v)) = (stack.last_mut(), attr(e, "value")) {
                            top.name = Some(v);
                        }
                    }
                    "PromptText" => {
                        // Parameters: their name and the question asked.
                        let name = stack.last().and_then(|f| f.name.clone()).unwrap_or_default();
                        let prompt = attr(e, "value").unwrap_or_default();
                        let text = if prompt.is_empty() || prompt == name { name.clone() } else { format!("{name}\n{prompt}") };
                        other.push((format!("Prompt: {name}"), text));
                    }
                    "Column" => {
                        if let (Some(source), Some(n)) = (stack.iter().rev().find(|f| f.tag == "Tablesource"), attr(e, "name")) {
                            let alias = source.name.clone().unwrap_or_default();
                            match columns.iter_mut().find(|(a, _)| *a == alias) {
                                Some((_, list)) => list.push(n),
                                None => columns.push((alias, vec![n])),
                            }
                        }
                    }
                    "TextData" => {
                        in_text_data = !empty;
                        value.clear();
                    }
                    _ => {}
                }
                if !empty {
                    let mut frame = Frame { tag: tag.clone(), kind: attr(e, "type"), name: None, x: num("x_position"), y: num("y_position") };
                    if tag == "Tablesource" {
                        frame.name = attr(e, "alias");
                    }
                    stack.push(frame);
                }
            }
            Event::End(_) => {
                let Some(frame) = stack.pop() else { continue };
                match frame.tag.as_str() {
                    "TextData" => {
                        in_text_data = false;
                        let v = std::mem::take(&mut value);
                        if let Some(t) = text_el.as_mut() {
                            t.text.push_str(&v);
                        } else if stack.iter().any(|f| f.tag == "Sql") {
                            let alias = stack.iter().rev().find(|f| f.tag == "Tablesource").and_then(|f| f.name.clone());
                            other.push((label_with("SQL query", alias), v));
                        } else if let Some(owner) = stack.iter().rev().find(|f| f.name.is_some()) {
                            let name = owner.name.clone().unwrap_or_default();
                            other.push((format!("Formula: {name}"), v));
                        }
                    }
                    "Element" => {
                        if frame.kind.as_deref() == Some("Paragraph") {
                            if let Some(t) = text_el.as_mut() {
                                t.text.push('\n');
                            }
                        }
                        if text_el.is_some() && stack.len() == text_depth {
                            let t = text_el.take().unwrap();
                            if let Some((_, placed)) = sections.last_mut() {
                                placed.push(t);
                            }
                        }
                    }
                    "Section" => {
                        if let Some((label, placed)) = sections.pop() {
                            layout.push((label, reading_order(placed)));
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if in_text_data => {
                if value.len() < MAX_VALUE {
                    value.push_str(&t.html_content());
                }
            }
            Event::CData(c) if in_text_data => {
                if value.len() < MAX_VALUE {
                    value.push_str(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    for (alias, list) in columns {
        other.push((label_with("Database fields", Some(alias)), list.join(", ")));
    }
    // Sections of the same area type (several detail sections) stay separate but keep order.
    layout.extend(other);
    Ok(layout)
}

fn label_with(label: &str, name: Option<String>) -> String {
    match name.filter(|n| !n.is_empty()) {
        Some(n) => format!("{label}: {n}"),
        None => label.to_string(),
    }
}

/// "ReportHeader" → "Report header", "Detail" → "Details".
fn area_label(kind: &str) -> String {
    match kind {
        "Detail" => "Details".into(),
        "" => "Report".into(),
        k => {
            let mut out = String::new();
            for (i, c) in k.chars().enumerate() {
                if i > 0 && c.is_uppercase() {
                    out.push(' ');
                    out.extend(c.to_lowercase());
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

/// Elements top to bottom; those starting at about the same height form one line, read left
/// to right.
fn reading_order(mut placed: Vec<Placed>) -> String {
    placed.retain(|p| !p.text.trim().is_empty());
    placed.sort_by_key(|p| (p.y, p.x));
    let mut lines: Vec<Vec<Placed>> = Vec::new();
    for p in placed {
        match lines.last_mut() {
            Some(line) if p.y - line[0].y <= SAME_LINE => line.push(p),
            _ => lines.push(vec![p]),
        }
    }
    let mut out = String::new();
    for mut line in lines {
        line.sort_by_key(|p| p.x);
        let parts: Vec<String> = line.iter().map(|p| p.text.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
        out.push_str(&parts.join(" "));
        out.push('\n');
    }
    out
}

fn read_meta(zip: &mut SafeZip) -> ParseResult<DocMeta> {
    let mut meta = DocMeta::default();
    let Some(xml) = zip.read_to_string("meta.xml", 4 << 20)? else { return Ok(meta) };
    let mut reader = Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    let mut current = String::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => current = e.local_name().as_ref().to_string(),
            Event::End(_) => current.clear(),
            Event::Text(t) => {
                let v = t.html_content().trim().to_string();
                if !v.is_empty() {
                    match current.as_str() {
                        "Title" => meta.title = Some(v),
                        "Author" => meta.author = Some(v),
                        "Subject" => meta.subject = Some(v),
                        "Keywords" => meta.keywords = Some(v),
                        _ => {}
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::locmap::describe;
    use crate::parsers::Limits;
    use std::io::Write;

    /// A report shaped like those i-net Clear Reports Designer saves.
    pub(crate) fn sample_report(path: &Path) {
        let content = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?>
<inet:Report xmlns:inet="http://www.inetsoftware.de" major_version="23" id="0">
  <Datasource>
    <DataSourceConfiguration name="sales"></DataSourceConfiguration>
    <Tablesource alias="Orders" databaseIdentifier="Orders">
      <Column name="ORDERCODE" type="11" />
      <Column name="CUSTOMERNAME" type="11" />
      <Sql><TextData><![CDATA[SELECT o.CODE AS OrderCode, c.NAME AS CustomerName
FROM ORDERS o JOIN CUSTOMERS c ON c.ID = o.CUSTOMERID]]></TextData></Sql>
    </Tablesource>
  </Datasource>
  <FormulaFields>
    <Field type="FormulaField"><Name value="discount_rate"/><TextData><![CDATA[if {Orders.TOTAL} > 1000 then 0.05 else 0]]></TextData></Field>
  </FormulaFields>
  <PromptFields>
    <Field type="PromptVarField" valueType="11"><Name value="RegionCode"/><PromptText value="Which region?"/></Field>
  </PromptFields>
  <AreaPair type="ReportAreaPair">
    <Area type="ReportHeader">
      <Section type="ReportHeader" width="4535" height="900">
        <Element type="Text" count="1" x_position="100" y_position="100">
          <Element type="Paragraph" count="1"><Element type="TextPart"><TextData><![CDATA[Quarterly penguin summary]]></TextData></Element></Element>
        </Element>
      </Section>
    </Area>
  </AreaPair>
  <AreaPair type="DetailArea">
    <Area type="Detail">
      <Section type="Detail" width="4535" height="2000">
        <Element type="FieldElement" x_position="1600" y_position="856"><Reference name="field" type="DatabaseField" value="Orders.ORDERCODE"/></Element>
        <Element type="Text" count="1" x_position="100" y_position="1500">
          <Element type="Paragraph" count="1"><Element type="TextPart"><TextData><![CDATA[Customer:]]></TextData></Element></Element>
        </Element>
        <Element type="Text" count="1" x_position="100" y_position="860">
          <Element type="Paragraph" count="1"><Element type="TextPart"><TextData><![CDATA[Order code:]]></TextData></Element></Element>
        </Element>
        <Element type="FieldElement" x_position="1600" y_position="1495"><Reference name="field" type="DatabaseField" value="Orders.CUSTOMERNAME"/></Element>
      </Section>
    </Area>
  </AreaPair>
</inet:Report>"#;
        let meta = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?><Meta xmlns="http://www.inetsoftware.de:meta:1.0"><Author>Report Team</Author><Title>Penguin orders by region</Title></Meta>"#;
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default();
        w.start_file("mimetype", stored).unwrap();
        w.write_all(b"application/crystalclear.report").unwrap();
        for (name, data) in [("content.xml", content), ("meta.xml", meta)] {
            w.start_file(name, deflated).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    fn extract(path: &Path) -> ParseResult<(String, Vec<crate::parsers::Loc>, DocMeta)> {
        let limits = Limits::default();
        let mut sink = TextSink::new(1 << 20, None);
        let meta = InetReportParser.extract(path, &ParseContext { limits: &limits }, &mut sink)?;
        let (text, locs, _) = sink.into_parts();
        Ok((text, locs, meta))
    }

    #[test]
    fn reads_layout_queries_formulas_and_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("orders.rpt");
        sample_report(&p);
        let (text, locs, meta) = extract(&p).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Penguin orders by region"));
        assert_eq!(meta.author.as_deref(), Some("Report Team"));
        // Labels sit beside their fields, in reading order.
        assert!(text.contains("Order code: {Orders.ORDERCODE}\nCustomer: {Orders.CUSTOMERNAME}"), "{text}");
        let at = |needle: &str| describe(&locs, &text, text.find(needle).unwrap_or_else(|| panic!("{needle:?} in {text}")), false);
        assert_eq!(at("penguin summary").as_deref(), Some("Report header"));
        assert_eq!(at("Order code").as_deref(), Some("Details"));
        assert_eq!(at("JOIN CUSTOMERS").as_deref(), Some("SQL query: Orders"));
        assert_eq!(at("0.05").as_deref(), Some("Formula: discount_rate"));
        assert_eq!(at("Which region?").as_deref(), Some("Prompt: RegionCode"));
        assert_eq!(at("ORDERCODE, CUSTOMERNAME").as_deref(), Some("Database fields: Orders"));
    }

    #[test]
    fn other_zips_named_rpt_are_not_reports() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("other.rpt");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        w.start_file("readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();
        assert!(matches!(extract(&p), Err(ParseError::Unsupported(_))));
    }

    #[test]
    fn area_labels() {
        assert_eq!(area_label("ReportHeader"), "Report header");
        assert_eq!(area_label("GroupFooter"), "Group footer");
        assert_eq!(area_label("Detail"), "Details");
    }
}
