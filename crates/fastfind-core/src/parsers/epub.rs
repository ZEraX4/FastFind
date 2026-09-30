//! EPUB: reads the OPF spine and extracts each XHTML chapter in reading order.

use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::html::html_to_text;
use super::sniff::{sniff, Sniffed};
use super::zipsafe::{resolve_target, SafeZip};
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct EpubParser;

impl DocumentParser for EpubParser {
    fn name(&self) -> &'static str {
        "epub"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["epub"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Zip
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut zip = SafeZip::open(path, ctx.limits)?;
        let container = zip
            .read_to_string("META-INF/container.xml", 1 << 20)?
            .ok_or_else(|| ParseError::Corrupt("missing META-INF/container.xml".into()))?;
        let opf_path = find_attr(&container, "rootfile", "full-path")
            .ok_or_else(|| ParseError::Corrupt("no rootfile".into()))?;
        let opf = zip.read_to_string(&opf_path, 8 << 20)?.ok_or_else(|| ParseError::Corrupt("missing OPF".into()))?;
        let base = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let (manifest, spine, mut meta) = parse_opf(&opf);
        let mut chapters: Vec<String> = spine
            .iter()
            .filter_map(|id| manifest.iter().find(|(i, _)| i == id).map(|(_, h)| resolve_target(base, h)))
            .collect();
        if chapters.is_empty() {
            chapters = zip.names().into_iter().filter(|n| n.ends_with(".xhtml") || n.ends_with(".html") || n.ends_with(".htm")).collect();
            chapters.sort();
        }
        for (i, ch) in chapters.iter().enumerate() {
            if sink.is_full() {
                break;
            }
            let Some(html) = zip.read_to_string(ch, 32 << 20)? else { continue };
            sink.newline();
            sink.anchor(LocKind::Section(format!("Chapter {}", i + 1)));
            let mut chapter_meta = DocMeta::default();
            html_to_text(&html, sink, &mut chapter_meta);
        }
        meta.pages = Some(chapters.len() as u32);
        Ok(meta)
    }
}

fn find_attr(xml: &str, elem: &str, attr: &str) -> Option<String> {
    let mut r = Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == elem => {
                return e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.local_name().as_ref() == attr)
                    .and_then(|a| super::attr_value(&a));
            }
            Event::Eof => return None,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// (manifest id → href, spine idrefs, metadata)
fn parse_opf(opf: &str) -> (Vec<(String, String)>, Vec<String>, DocMeta) {
    let mut manifest = Vec::new();
    let mut spine = Vec::new();
    let mut meta = DocMeta::default();
    let mut r = Reader::from_reader(opf.as_bytes());
    let mut buf = Vec::new();
    let mut current = String::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let get = |n: &str| {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == n)
                        .and_then(|a| super::attr_value(&a))
                };
                match e.local_name().as_ref() {
                    "item" => {
                        if let (Some(id), Some(href)) = (get("id"), get("href")) {
                            manifest.push((id, href));
                        }
                    }
                    "itemref" => {
                        if let Some(id) = get("idref") {
                            spine.push(id);
                        }
                    }
                    n => current = n.to_string(),
                }
            }
            Event::End(_) => current.clear(),
            Event::Text(t) => {
                let v = t.html_content().trim().to_string();
                if !v.is_empty() {
                    match current.as_str() {
                        "title" if meta.title.is_none() => meta.title = Some(v),
                        "creator" if meta.author.is_none() => meta.author = Some(v),
                        "subject" => meta.subject = Some(v),
                        _ => {}
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    (manifest, spine, meta)
}
