//! Generic XML → text (element text, CDATA and attribute values). Streaming; DTDs are ignored
//! (quick-xml never expands external or custom entities), so entity-expansion attacks such as
//! "billion laughs" do not apply.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{push_xml_ref, DocMeta, DocumentParser, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct XmlParser;

impl DocumentParser for XmlParser {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xml", "xsd", "xsl", "xslt", "svg", "rss", "atom", "xaml", "csproj", "vbproj", "fsproj",
          "props", "targets", "resx", "pom", "wsdl", "kml", "gpx", "fb2", "opf", "ncx", "config.xml", "nuspec", "manifest"]
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Reparse
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut reader = Reader::from_reader(BufReader::with_capacity(64 * 1024, File::open(path)?));
        reader.config_mut().check_end_names = false;
        let mut buf = Vec::with_capacity(8192);
        let mut meta = DocMeta::default();
        let mut in_title = false;
        let mut need_space = false;
        let mut title = String::new();
        loop {
            sink.check_deadline()?;
            let ev = match reader.read_event_into(&mut buf) {
                Ok(ev) => ev,
                // Malformed XML: keep what was extracted so far if there is any.
                Err(e) if !sink.is_empty() => {
                    tracing::debug!(error = %e, "xml parse stopped early");
                    break;
                }
                Err(e) => return Err(ParseError::corrupt(e)),
            };
            let is_start = matches!(ev, Event::Start(_));
            match ev {
                Event::Start(e) | Event::Empty(e) => {
                    in_title = is_start
                        && e.local_name().as_ref().eq_ignore_ascii_case("title")
                        && meta.title.is_none();
                    for a in e.attributes().flatten() {
                        if let Some(v) = super::attr_value(&a) {
                            let v = v.trim();
                            // Attribute values are often the only content (config files);
                            // only very long blobs (embedded data) are skipped.
                            if !v.is_empty() && v.len() < 2048 {
                                sink.space();
                                sink.push_str(v);
                                need_space = true;
                            }
                        }
                    }
                }
                Event::End(_) => {
                    if in_title {
                        let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
                        meta.title = (!t.is_empty()).then_some(t);
                        in_title = false;
                    }
                    sink.newline();
                }
                Event::Text(t) => {
                    let s = t.html_content();
                    if in_title && title.len() < 1024 {
                        title.push_str(&s);
                    }
                    if !s.trim().is_empty() {
                        if need_space {
                            sink.space();
                            need_space = false;
                        }
                        sink.push_str(&s);
                    }
                }
                Event::GeneralRef(r) => {
                    if in_title {
                        title.extend(super::ref_char(&r));
                    }
                    push_xml_ref(sink, &r)
                }
                Event::CData(c) => {
                    sink.space();
                    sink.push_str(&c);
                }
                Event::Eof => break,
                _ => {}
            }
            if sink.is_full() {
                break;
            }
            buf.clear();
        }
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::Limits;

    #[test]
    fn extracts_text_attributes_and_cdata() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.xml");
        std::fs::write(
            &p,
            r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY boom "BOOM">]>
<root><title>Quarterly &amp; Annual</title><item name="alpha" id="7">Beta &#x263A; &boom;</item><![CDATA[raw <text>]]></root>"#,
        )
        .unwrap();
        let limits = Limits::default();
        let mut sink = TextSink::new(1 << 20, None);
        let meta = XmlParser.extract(&p, &ParseContext { limits: &limits }, &mut sink).unwrap();
        let t = sink.text().to_string();
        assert!(t.contains("Quarterly & Annual"));
        assert!(t.contains("alpha"));
        assert!(t.contains("Beta ☺"));
        assert!(t.contains("raw <text>"));
        assert!(!t.contains("BOOM"), "custom entities must not be expanded");
        assert_eq!(meta.title.as_deref(), Some("Quarterly & Annual"));
    }
}
