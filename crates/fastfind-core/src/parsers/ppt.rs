//! Legacy PowerPoint 97–2003 (`.ppt`) text extraction, following [MS-PPT].
//!
//! 1. `Current User` stream → offset of the newest `UserEditAtom`.
//! 2. Walk the edit chain (newest first) collecting `PersistDirectoryAtom` entries into a
//!    persist-id → stream-offset map (newer entries win).
//! 3. The `DocumentContainer` holds `SlideListWithTextContainer`s: instance 0 lists slides in
//!    presentation order (`SlidePersistAtom`), optionally followed by their text atoms;
//!    instance 2 lists notes.
//! 4. For each slide, text atoms inside the slide's own container (drawing text boxes, which is
//!    where PowerPoint 2000+ stores text) are merged with the list text, de-duplicated.
//! 5. Notes are attached to slides via `NotesAtom.slideIdRef` ↔ `SlidePersistAtom.slideId`.
//!
//! Damaged files fall back to a linear scan of all text atoms. Encrypted files are detected
//! from the Current User header token.
//!
//! [MS-PPT]: https://learn.microsoft.com/openspecs/office_file_formats/ms-ppt/

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;

use super::doc::{ole_summary, read_stream};
use super::sniff::{sniff, Sniffed};
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct PptParser;

const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_NOTES: u16 = 0x03F0;
const RT_NOTES_ATOM: u16 = 0x03F1;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_TEXT_HEADER_ATOM: u16 = 0x0F9F;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_USER_EDIT_ATOM: u16 = 0x0FF5;
const RT_PERSIST_DIRECTORY_ATOM: u16 = 0x1772;
const RT_CRYPT_SESSION: u16 = 0x2F14;

const TOKEN_ENCRYPTED: u32 = 0xF3D1_C4DF;

impl DocumentParser for PptParser {
    fn name(&self) -> &'static str {
        "ppt"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ppt", "pps", "pot"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Cfb
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut comp = cfb::CompoundFile::open(File::open(path)?).map_err(ParseError::corrupt)?;
        if comp.exists("/EncryptionInfo") {
            return Err(ParseError::Encrypted);
        }
        let mut meta = ole_summary(&mut comp);
        let doc = read_stream(&mut comp, "/PowerPoint Document")?
            .ok_or_else(|| ParseError::Corrupt("no PowerPoint Document stream".into()))?;
        let current_user = read_stream(&mut comp, "/Current User")?;
        let depth = ctx.limits.max_depth.min(64);
        let slides = match current_user.as_deref().map(|cu| structured(&doc, cu, depth)) {
            Some(Ok(s)) if !s.is_empty() => s,
            Some(Err(ParseError::Encrypted)) => return Err(ParseError::Encrypted),
            _ => {
                // Fallback: every text atom in stream order as one untitled "slide".
                let mut texts = Vec::new();
                collect_texts(&doc, 0, doc.len(), depth, &mut texts, &mut false);
                if texts.is_empty() {
                    return Err(ParseError::Corrupt("no slide text found".into()));
                }
                vec![SlideText { title: None, body: texts.into_iter().map(|t| t.1).collect(), notes: vec![] }]
            }
        };
        meta.pages = Some(slides.len() as u32);
        for (i, s) in slides.iter().enumerate() {
            if sink.is_full() {
                break;
            }
            sink.newline();
            sink.anchor(LocKind::Slide(i as u32 + 1, s.title.clone()));
            for t in s.body.iter().chain(s.notes.iter()) {
                sink.push_str(t);
                sink.newline();
            }
            sink.check_deadline()?;
        }
        Ok(meta)
    }
}

#[derive(Debug, Clone, Copy)]
struct Rec {
    inst: u16,
    ver: u8,
    typ: u16,
    len: u32,
    /// Offset of the record data (after the 8-byte header).
    data: usize,
}

impl Rec {
    fn end(&self) -> usize {
        self.data + self.len as usize
    }
    fn is_container(&self) -> bool {
        self.ver == 0x0F
    }
}

fn rec_at(b: &[u8], off: usize) -> Option<Rec> {
    let h = b.get(off..off + 8)?;
    let vi = u16::from_le_bytes([h[0], h[1]]);
    let rec = Rec {
        ver: (vi & 0x0F) as u8,
        inst: vi >> 4,
        typ: u16::from_le_bytes([h[2], h[3]]),
        len: u32::from_le_bytes([h[4], h[5], h[6], h[7]]),
        data: off + 8,
    };
    (rec.end() <= b.len()).then_some(rec)
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Direct children of a container.
fn children(b: &[u8], parent: &Rec) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut off = parent.data;
    while off + 8 <= parent.end() {
        let Some(r) = rec_at(b, off) else { break };
        if r.end() > parent.end() {
            break;
        }
        out.push(r);
        off = r.end();
    }
    out
}

fn atom_text(b: &[u8], r: &Rec) -> Option<String> {
    let data = &b[r.data..r.end()];
    let s = match r.typ {
        RT_TEXT_CHARS_ATOM => {
            let u: Vec<u16> = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        }
        // "Bytes" atoms hold the low bytes of UTF-16 code units (i.e. Latin-1).
        RT_TEXT_BYTES_ATOM => data.iter().map(|&c| c as char).collect(),
        _ => return None,
    };
    // PowerPoint uses \r as paragraph separator and \x0B as line break.
    let s: String = s.chars().map(|c| if c == '\r' || c == '\u{0b}' { '\n' } else { c }).collect();
    let s = s.trim().to_string();
    (!s.is_empty() && !is_placeholder(&s)).then_some(s)
}

/// Master/layout placeholder prompts ("Click to edit Master title style") are not content.
fn is_placeholder(s: &str) -> bool {
    s.starts_with("Click to edit") || s == "*"
}

/// Recursively collect (is_title, text) from all text atoms under a range.
fn collect_texts(b: &[u8], start: usize, end: usize, depth: usize, out: &mut Vec<(bool, String)>, next_is_title: &mut bool) {
    if depth == 0 {
        return;
    }
    let mut off = start;
    while off + 8 <= end {
        let Some(r) = rec_at(b, off) else { break };
        if r.end() > end {
            break;
        }
        match r.typ {
            RT_TEXT_HEADER_ATOM => {
                // textType 0 = Title, 6 = CenterTitle.
                *next_is_title = matches!(u32_at(b, r.data), Some(0) | Some(6));
            }
            RT_TEXT_CHARS_ATOM | RT_TEXT_BYTES_ATOM => {
                if let Some(t) = atom_text(b, &r) {
                    out.push((*next_is_title, t));
                }
                *next_is_title = false;
            }
            _ if r.is_container() => collect_texts(b, r.data, r.end(), depth - 1, out, next_is_title),
            _ => {}
        }
        off = r.end();
    }
}

/// (persist id, slide id, (is_title, text) atoms from the slide list).
type ListSlide = (u32, u32, Vec<(bool, String)>);

struct SlideText {
    title: Option<String>,
    body: Vec<String>,
    notes: Vec<String>,
}

fn structured(doc: &[u8], current_user: &[u8], depth: usize) -> ParseResult<Vec<SlideText>> {
    let bad = || ParseError::Corrupt("invalid PowerPoint structure".into());
    // CurrentUserAtom: header(8) size(4) headerToken(4) offsetToCurrentEdit(4)
    let token = u32_at(current_user, 12).ok_or_else(bad)?;
    if token == TOKEN_ENCRYPTED {
        return Err(ParseError::Encrypted);
    }
    let mut edit_off = u32_at(current_user, 16).ok_or_else(bad)? as usize;
    let mut persist: HashMap<u32, usize> = HashMap::new();
    let mut doc_ref: Option<u32> = None;
    let mut seen = HashSet::new();
    while edit_off != 0 && seen.insert(edit_off) && seen.len() < 4096 {
        let r = rec_at(doc, edit_off).ok_or_else(bad)?;
        if r.typ != RT_USER_EDIT_ATOM {
            return Err(bad());
        }
        let last_edit = u32_at(doc, r.data + 8).ok_or_else(bad)? as usize;
        let pdir_off = u32_at(doc, r.data + 12).ok_or_else(bad)? as usize;
        if doc_ref.is_none() {
            doc_ref = u32_at(doc, r.data + 16);
        }
        if let Some(pd) = rec_at(doc, pdir_off).filter(|p| p.typ == RT_PERSIST_DIRECTORY_ATOM) {
            let mut o = pd.data;
            while o + 4 <= pd.end() {
                let v = u32_at(doc, o).ok_or_else(bad)?;
                let first = v & 0x000F_FFFF;
                let count = (v >> 20) as usize;
                o += 4;
                for k in 0..count {
                    if let Some(off) = u32_at(doc, o + k * 4) {
                        persist.entry(first + k as u32).or_insert(off as usize);
                    }
                }
                o += count * 4;
            }
        }
        edit_off = last_edit;
    }
    let doc_off = *doc_ref.and_then(|r| persist.get(&r)).ok_or_else(bad)?;
    let doc_rec = rec_at(doc, doc_off).filter(|r| r.typ == RT_DOCUMENT).ok_or_else(bad)?;
    let kids = children(doc, &doc_rec);
    if kids.iter().any(|k| k.typ == RT_CRYPT_SESSION) {
        return Err(ParseError::Encrypted);
    }

    // (persist id, slide id, texts from the list)
    let mut slides: Vec<ListSlide> = Vec::new();
    let mut notes_refs: Vec<u32> = Vec::new();
    for list in kids.iter().filter(|k| k.typ == RT_SLIDE_LIST_WITH_TEXT) {
        let mut next_is_title = false;
        for c in children(doc, list) {
            match (list.inst, c.typ) {
                (0, RT_SLIDE_PERSIST_ATOM) => {
                    let pid = u32_at(doc, c.data).unwrap_or(0);
                    let sid = u32_at(doc, c.data + 12).unwrap_or(0);
                    slides.push((pid, sid, Vec::new()));
                }
                (0, RT_TEXT_HEADER_ATOM) => next_is_title = matches!(u32_at(doc, c.data), Some(0) | Some(6)),
                (0, RT_TEXT_CHARS_ATOM | RT_TEXT_BYTES_ATOM) => {
                    if let (Some(s), Some(t)) = (slides.last_mut(), atom_text(doc, &c)) {
                        s.2.push((next_is_title, t));
                    }
                    next_is_title = false;
                }
                (2, RT_SLIDE_PERSIST_ATOM) => notes_refs.push(u32_at(doc, c.data).unwrap_or(0)),
                _ => {}
            }
        }
    }

    // Notes text keyed by the slide id it belongs to.
    let mut notes_by_slide: HashMap<u32, Vec<String>> = HashMap::new();
    for pid in notes_refs {
        let Some(nrec) = persist.get(&pid).and_then(|&o| rec_at(doc, o)).filter(|r| r.typ == RT_NOTES) else { continue };
        let slide_id = children(doc, &nrec).iter().find(|c| c.typ == RT_NOTES_ATOM).and_then(|a| u32_at(doc, a.data));
        let mut texts = Vec::new();
        collect_texts(doc, nrec.data, nrec.end(), depth, &mut texts, &mut false);
        if let Some(sid) = slide_id {
            notes_by_slide.entry(sid).or_default().extend(texts.into_iter().map(|t| t.1));
        }
    }

    let mut out = Vec::with_capacity(slides.len());
    for (pid, sid, list_texts) in slides {
        let mut texts = list_texts;
        if let Some(srec) = persist.get(&pid).and_then(|&o| rec_at(doc, o)).filter(|r| r.typ == RT_SLIDE) {
            let mut drawn = Vec::new();
            collect_texts(doc, srec.data, srec.end(), depth, &mut drawn, &mut false);
            for d in drawn {
                if !texts.iter().any(|t| t.1 == d.1) {
                    texts.push(d);
                }
            }
        }
        // Title first, then body text in stream order.
        texts.sort_by_key(|t| !t.0);
        let title = texts.iter().find(|t| t.0).map(|t| t.1.split_whitespace().collect::<Vec<_>>().join(" "));
        out.push(SlideText {
            title,
            body: texts.into_iter().map(|t| t.1).collect(),
            notes: notes_by_slide.remove(&sid).unwrap_or_default(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ver: u8, inst: u16, typ: u16, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((inst << 4) | ver as u16).to_le_bytes());
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn chars(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    /// Hand-built stream: Document with a slide list, a slide container with a text box,
    /// a persist directory and a user edit atom. Verifies order, titles and de-duplication.
    #[test]
    fn walks_persist_directory() {
        let mut doc = Vec::new();
        // Slide container (persist id 2) with a title text box.
        let mut th = rec(0, 0, RT_TEXT_HEADER_ATOM, &0u32.to_le_bytes());
        th.extend(rec(0, 0, RT_TEXT_CHARS_ATOM, &chars("Quarterly Results")));
        th.extend(rec(0, 0, RT_TEXT_HEADER_ATOM, &1u32.to_le_bytes()));
        th.extend(rec(0, 0, RT_TEXT_BYTES_ATOM, b"Revenue grew 12%"));
        let drawing = rec(0x0F, 0, 0xF00D, &th);
        let slide = rec(0x0F, 0, RT_SLIDE, &drawing);
        let slide_off = doc.len();
        doc.extend(&slide);
        // Document container (persist id 1): slide list inst 0 with one SlidePersistAtom
        // (persistIdRef=2, slideId=256) and the same body text (to test dedupe).
        let mut spa = Vec::new();
        spa.extend_from_slice(&2u32.to_le_bytes());
        spa.extend_from_slice(&[0; 8]);
        spa.extend_from_slice(&256u32.to_le_bytes());
        spa.extend_from_slice(&[0; 4]);
        let mut list = rec(0, 0, RT_SLIDE_PERSIST_ATOM, &spa);
        list.extend(rec(0, 0, RT_TEXT_BYTES_ATOM, b"Revenue grew 12%"));
        let doc_container = rec(0x0F, 0, RT_DOCUMENT, &rec(0x0F, 0, RT_SLIDE_LIST_WITH_TEXT, &list));
        let doc_off = doc.len();
        doc.extend(&doc_container);
        // Persist directory: ids 1..=2.
        let mut pd = Vec::new();
        pd.extend_from_slice(&(1u32 | (2u32 << 20)).to_le_bytes());
        pd.extend_from_slice(&(doc_off as u32).to_le_bytes());
        pd.extend_from_slice(&(slide_off as u32).to_le_bytes());
        let pd_off = doc.len();
        doc.extend(rec(0, 0, RT_PERSIST_DIRECTORY_ATOM, &pd));
        // UserEditAtom: lastSlideIdRef, version(2) minor(1) major(1), offsetLastEdit, offsetPersistDirectory, docPersistIdRef
        let mut ue = Vec::new();
        ue.extend_from_slice(&256u32.to_le_bytes());
        ue.extend_from_slice(&[0; 4]);
        ue.extend_from_slice(&0u32.to_le_bytes());
        ue.extend_from_slice(&(pd_off as u32).to_le_bytes());
        ue.extend_from_slice(&1u32.to_le_bytes());
        let ue_off = doc.len();
        doc.extend(rec(0, 0, RT_USER_EDIT_ATOM, &ue));
        let mut cu = vec![0u8; 8];
        cu.extend_from_slice(&20u32.to_le_bytes());
        cu.extend_from_slice(&0xE391C05Fu32.to_le_bytes());
        cu.extend_from_slice(&(ue_off as u32).to_le_bytes());

        let slides = structured(&doc, &cu, 32).unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].title.as_deref(), Some("Quarterly Results"));
        assert_eq!(slides[0].body, vec!["Quarterly Results".to_string(), "Revenue grew 12%".to_string()]);

        let mut enc = cu.clone();
        enc[12..16].copy_from_slice(&TOKEN_ENCRYPTED.to_le_bytes());
        assert!(matches!(structured(&doc, &enc, 32), Err(ParseError::Encrypted)));
    }
}
