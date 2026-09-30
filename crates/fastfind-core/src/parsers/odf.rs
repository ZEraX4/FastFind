//! OpenDocument (`.odt`, `.ods`, `.odp` and templates): streams `content.xml`, reads title and
//! author from `meta.xml`. Spreadsheet tables become sheets with cell locations; presentation
//! pages become slides.

use std::io::BufReader;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::ooxml::RowWriter;
use super::sniff::{sniff, Sniffed};
use super::zipsafe::SafeZip;
use super::{push_xml_ref, DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct OdfParser;

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| super::attr_value(&a))
}

impl DocumentParser for OdfParser {
    fn name(&self) -> &'static str {
        "opendocument"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["odt", "ott", "ods", "ots", "odp", "otp", "odg", "fodt"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Zip
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        if zip.open_entry("META-INF/manifest.xml")?.is_none() && !zip.contains("content.xml") {
            return Err(ParseError::Corrupt("not an OpenDocument package".into()));
        }
        let mut meta = read_meta(&mut zip)?;
        let Some(entry) = zip.open_entry("content.xml")? else {
            return Err(ParseError::Corrupt("missing content.xml".into()));
        };
        let mut reader = Reader::from_reader(BufReader::with_capacity(64 * 1024, entry));
        reader.config_mut().check_end_names = false;
        let mut buf = Vec::with_capacity(8192);
        let mut slide = 0u32;
        let mut sheets = 0u32;
        // Spreadsheet state.
        let mut in_table = false;
        let mut is_sheet = false;
        let mut rows = RowWriter::new();
        let mut row: u32 = 0;
        let mut row_repeat: u32 = 0;
        let mut col: u32 = 0;
        let mut cell_repeat: u32 = 1;
        let mut cell_text = String::new();
        let mut in_cell = false;
        let doc_is_spreadsheet = path.extension().map(|e| e.eq_ignore_ascii_case("ods") || e.eq_ignore_ascii_case("ots")).unwrap_or(false);
        loop {
            sink.check_deadline()?;
            let ev = match reader.read_event_into(&mut buf) {
                Ok(ev) => ev,
                Err(e) if !sink.is_empty() => {
                    tracing::debug!(error = %e, "odf content truncated");
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            match &ev {
                Event::Start(e) | Event::Empty(e) => {
                    let empty = matches!(ev, Event::Empty(_));
                    match e.local_name().as_ref() {
                        "page" => {
                            slide += 1;
                            sink.newline();
                            sink.anchor(LocKind::Slide(slide, attr(e, "name").filter(|n| !n.starts_with("page"))));
                        }
                        "table" if !empty => {
                            in_table = true;
                            is_sheet = doc_is_spreadsheet;
                            if is_sheet {
                                sheets += 1;
                                let name = attr(e, "name").unwrap_or_else(|| format!("Sheet{sheets}"));
                                sink.newline();
                                sink.anchor(LocKind::Sheet(name.clone()));
                                sink.push_str(&name);
                                sink.newline();
                                rows = RowWriter::new();
                                row = 0;
                            }
                        }
                        "table-row" => {
                            let rep: u32 = attr(e, "number-rows-repeated").and_then(|v| v.parse().ok()).unwrap_or(1);
                            row += 1;
                            // A repeated row's content is written once; the counter skips the
                            // repeats when the row ends (empty repeated rows, often ~1M of them,
                            // cost nothing).
                            row_repeat = rep.saturating_sub(1);
                            if empty {
                                row = row.saturating_add(row_repeat);
                                row_repeat = 0;
                            }
                            col = 0;
                            rows.new_row();
                        }
                        "table-cell" | "covered-table-cell" => {
                            cell_repeat = attr(e, "number-columns-repeated").and_then(|v| v.parse().ok()).unwrap_or(1);
                            if empty {
                                col = col.saturating_add(cell_repeat);
                            } else {
                                in_cell = true;
                                cell_text.clear();
                            }
                        }
                        "tab" => {
                            if in_cell && is_sheet { cell_text.push(' ') } else { sink.tab() }
                        }
                        "s" => {
                            if in_cell && is_sheet { cell_text.push(' ') } else { sink.push_char(' ') }
                        }
                        "line-break" => {
                            if in_cell && is_sheet { cell_text.push(' ') } else { sink.newline() }
                        }
                        _ => {}
                    }
                }
                Event::End(e) => match e.local_name().as_ref() {
                    "p" | "h" => {
                        if in_cell && is_sheet {
                            cell_text.push(' ');
                        } else if in_cell {
                            sink.space();
                        } else {
                            sink.newline();
                        }
                    }
                    "table-cell" | "covered-table-cell" => {
                        if is_sheet {
                            let text = cell_text.trim().to_string();
                            if !text.is_empty() {
                                // A repeated non-empty cell is written once (enough for search).
                                rows.cell(sink, row, Some(col), &text);
                            }
                        } else {
                            sink.tab();
                        }
                        col = col.saturating_add(cell_repeat);
                        in_cell = false;
                    }
                    "table-row" => {
                        row = row.saturating_add(row_repeat);
                        row_repeat = 0;
                        if !is_sheet && in_table {
                            sink.newline();
                        }
                    }
                    "table" => {
                        if is_sheet {
                            rows.finish(sink);
                        }
                        in_table = false;
                        is_sheet = false;
                    }
                    _ => {}
                },
                Event::Text(t) => {
                    let s = t.html_content();
                    if in_cell && is_sheet {
                        if cell_text.len() < 64 * 1024 {
                            cell_text.push_str(&s);
                        }
                    } else {
                        sink.push_str(&s);
                    }
                }
                Event::GeneralRef(r) => {
                    if in_cell && is_sheet {
                        if let Some(c) = super::ref_char(r) {
                            cell_text.push(c);
                        }
                    } else {
                        push_xml_ref(sink, r);
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            if sink.is_full() {
                break;
            }
            buf.clear();
        }
        if slide > 0 {
            meta.pages = Some(slide);
        } else if sheets > 0 {
            meta.pages = Some(sheets);
        }
        Ok(meta)
    }
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
                        "title" => meta.title = Some(v),
                        "initial-creator" | "creator" if meta.author.is_none() => meta.author = Some(v),
                        "subject" => meta.subject = Some(v),
                        "keyword" => meta.keywords = Some(v),
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
