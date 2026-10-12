//! Outlook messages (`.msg`, [MS-OXMSG]): a compound file whose properties are streams named
//! `__substg1.0_<id><type>` plus fixed-size values in `__properties_version1.0`.
//!
//! Indexed text matches `.eml`: a From / To / Cc / Date / Subject block, the body, then each
//! attachment after a `Part` anchor. The body is the plain-text property when present, else
//! the HTML body, else the compressed RTF body ([MS-OXRTFCP]) run through the RTF parser.
//! Attached files go through the regular parsers (shared with `.eml`); attached Outlook items
//! are their own sub-storages and are read recursively.
//!
//! Strings come as UTF-16 (`001F`) or, in older ANSI files, 8-bit (`001E`) in the message's
//! code page.

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use encoding_rs::{Encoding, WINDOWS_1252};

use super::embedded::{begin_part, extract_embedded, MAX_EMBEDDED_BYTES, MAX_NESTING};
use super::html::{decode_html_bytes, html_to_text};
use super::rtf::{codepage, rtf_to_text};
use super::sniff::{sniff, Sniffed};
use super::{DocMeta, DocumentParser, Limits, ParseContext, ParseError, ParseResult, TextMode, TextSink};

/// Bodies and attachments larger than this are not read.
const MAX_PROPERTY_BYTES: u64 = MAX_EMBEDDED_BYTES as u64;

// Property IDs.
const SUBJECT: u16 = 0x0037;
const CLIENT_SUBMIT_TIME: u16 = 0x0039;
const SENT_REPRESENTING_NAME: u16 = 0x0042;
const SENT_REPRESENTING_EMAIL: u16 = 0x0065;
const SENDER_NAME: u16 = 0x0C1A;
const SENDER_EMAIL: u16 = 0x0C1F;
const RECIPIENT_TYPE: u16 = 0x0C15;
const DISPLAY_CC: u16 = 0x0E03;
const DISPLAY_TO: u16 = 0x0E04;
const DELIVERY_TIME: u16 = 0x0E06;
const BODY: u16 = 0x1000;
const RTF_COMPRESSED: u16 = 0x1009;
const BODY_HTML: u16 = 0x1013;
const DISPLAY_NAME: u16 = 0x3001;
const EMAIL_ADDRESS: u16 = 0x3003;
const ATTACH_DATA: u16 = 0x3701;
const ATTACH_FILENAME: u16 = 0x3704;
const ATTACH_METHOD: u16 = 0x3705;
const ATTACH_LONG_FILENAME: u16 = 0x3707;
const SMTP_ADDRESS: u16 = 0x39FE;
const INTERNET_CPID: u16 = 0x3FDE;
const MESSAGE_CODEPAGE: u16 = 0x3FFD;
const SENDER_SMTP_ADDRESS: u16 = 0x5D01;

const ATTACH_EMBEDDED_MSG: u32 = 5;

pub struct MsgParser;

impl DocumentParser for MsgParser {
    fn name(&self) -> &'static str {
        "msg"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["msg"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Cfb
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let comp = cfb::CompoundFile::open(File::open(path)?).map_err(ParseError::corrupt)?;
        let mut msg = Store { comp, path: String::new(), enc: WINDOWS_1252 };
        if !msg.comp.is_stream(PROPERTIES) {
            return Err(ParseError::Corrupt("not an Outlook message".into()));
        }
        msg.enc = msg.codepage(TOP_HEADER);
        let meta = DocMeta {
            title: msg.string(SUBJECT).filter(|s| !s.trim().is_empty()),
            author: msg.sender().map(|(name, addr)| name.or(addr).unwrap_or_default()).filter(|s| !s.is_empty()),
            ..DocMeta::default()
        };
        emit_message(&mut msg, TOP_HEADER, ctx.limits, sink, 0)?;
        Ok(meta)
    }
}

const PROPERTIES: &str = "/__properties_version1.0";
/// Bytes before the first entry of a properties stream: top-level message, attached message,
/// and recipient or attachment storages.
const TOP_HEADER: usize = 32;
const EMBEDDED_HEADER: usize = 24;
const CHILD_HEADER: usize = 8;

/// One storage (the message, a recipient, an attachment or an attached message) of the file.
struct Store<F> {
    comp: cfb::CompoundFile<F>,
    /// Storage path, "" for the root.
    path: String,
    /// Code page of 8-bit strings.
    enc: &'static Encoding,
}

impl<F: Read + Seek> Store<F> {
    fn stream(&mut self, name: &str) -> Option<Vec<u8>> {
        let path = format!("{}/{name}", self.path);
        if !self.comp.is_stream(&path) {
            return None;
        }
        let mut v = Vec::new();
        self.comp.open_stream(&path).ok()?.take(MAX_PROPERTY_BYTES).read_to_end(&mut v).ok()?;
        Some(v)
    }

    fn string(&mut self, id: u16) -> Option<String> {
        let s = if let Some(b) = self.stream(&format!("__substg1.0_{id:04X}001F")) {
            let u: Vec<u16> = b.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            String::from_utf16_lossy(&u)
        } else {
            let b = self.stream(&format!("__substg1.0_{id:04X}001E"))?;
            self.enc.decode_without_bom_handling(&b).0.into_owned()
        };
        let s = s.trim_end_matches('\0').to_string();
        (!s.is_empty()).then_some(s)
    }

    fn binary(&mut self, id: u16) -> Option<Vec<u8>> {
        self.stream(&format!("__substg1.0_{id:04X}0102"))
    }

    /// Fixed-size property values (`__properties_version1.0`), by ID: (type, 8 value bytes).
    fn fixed(&mut self, header: usize) -> Vec<(u16, u16, [u8; 8])> {
        let Some(b) = self.stream("__properties_version1.0") else { return Vec::new() };
        b.get(header..)
            .unwrap_or_default()
            .as_chunks::<16>()
            .0
            .iter()
            .map(|e| {
                let tag = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                ((tag >> 16) as u16, tag as u16, [e[8], e[9], e[10], e[11], e[12], e[13], e[14], e[15]])
            })
            .collect()
    }

    fn long(&mut self, header: usize, id: u16) -> Option<u32> {
        // PT_LONG (0x0003)
        self.fixed(header).into_iter().find(|&(i, t, _)| i == id && t == 0x0003).map(|(_, _, v)| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }

    fn codepage(&mut self, header: usize) -> &'static Encoding {
        let cp = self.long(header, INTERNET_CPID).or_else(|| self.long(header, MESSAGE_CODEPAGE));
        cp.and_then(|c| codepage(c as i64)).unwrap_or(self.enc)
    }

    /// Names of the child storages starting with `prefix`, in order.
    fn children(&self, prefix: &str) -> Vec<String> {
        let root = if self.path.is_empty() { "/" } else { &self.path };
        let Ok(entries) = self.comp.read_storage(root) else { return Vec::new() };
        let mut v: Vec<String> = entries.filter(|e| e.is_storage() && e.name().starts_with(prefix)).map(|e| e.name().to_string()).collect();
        v.sort();
        v
    }

    /// Run `f` with this store pointed at a child storage, then restore it.
    fn enter<T>(&mut self, child: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = (self.path.clone(), self.enc);
        self.path = format!("{}/{child}", self.path);
        let r = f(self);
        (self.path, self.enc) = saved;
        r
    }

    /// (display name, e-mail address) of the sender.
    fn sender(&mut self) -> Option<(Option<String>, Option<String>)> {
        let name = self.string(SENDER_NAME).or_else(|| self.string(SENT_REPRESENTING_NAME));
        let addr = [SENDER_SMTP_ADDRESS, SENDER_EMAIL, SENT_REPRESENTING_EMAIL].into_iter().find_map(|id| self.string(id).filter(|a| a.contains('@')));
        (name.is_some() || addr.is_some()).then_some((name, addr))
    }
}

fn format_address(name: Option<String>, addr: Option<String>) -> Option<String> {
    match (name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()), addr) {
        (Some(n), Some(a)) if n != a => Some(format!("{n} <{a}>")),
        (Some(n), _) => Some(n),
        (None, a) => a,
    }
}

fn emit_message<F: Read + Seek>(msg: &mut Store<F>, header: usize, limits: &Limits, sink: &mut TextSink, depth: usize) -> ParseResult<()> {
    let line = |sink: &mut TextSink, label: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            sink.push_str(label);
            sink.push_str(": ");
            sink.push_str(&v);
            sink.newline();
        }
    };
    line(sink, "From", msg.sender().and_then(|(n, a)| format_address(n, a)));

    // Recipients: To (1), Cc (2), Bcc (3).
    let mut lists: [Vec<String>; 3] = Default::default();
    for r in msg.children("__recip_version1.0_") {
        msg.enter(&r, |m| {
            let kind = m.long(CHILD_HEADER, RECIPIENT_TYPE).unwrap_or(1) & 0xF;
            let addr = [SMTP_ADDRESS, EMAIL_ADDRESS].into_iter().find_map(|id| m.string(id).filter(|a| a.contains('@') && !a.starts_with('/')));
            if let (Some(list), Some(a)) = (lists.get_mut(kind.wrapping_sub(1) as usize), format_address(m.string(DISPLAY_NAME), addr)) {
                list.push(a);
            }
        });
    }
    let [to, cc, bcc] = lists.map(|l| (!l.is_empty()).then(|| l.join(", ")));
    line(sink, "To", to.or_else(|| msg.string(DISPLAY_TO)));
    line(sink, "Cc", cc.or_else(|| msg.string(DISPLAY_CC)));
    line(sink, "Bcc", bcc);

    let fixed = msg.fixed(header);
    let time = [CLIENT_SUBMIT_TIME, DELIVERY_TIME].into_iter().find_map(|id| fixed.iter().find(|&&(i, t, _)| i == id && t == 0x0040).map(|&(_, _, v)| u64::from_le_bytes(v)));
    // FILETIME: 100 ns ticks since 1601.
    line(sink, "Date", time.filter(|&t| t > 0).map(|t| mail_parser::DateTime::from_timestamp((t / 10_000_000) as i64 - 11_644_473_600).to_rfc3339()));
    line(sink, "Subject", msg.string(SUBJECT));
    sink.newline();

    sink.check_deadline()?;
    if let Some(body) = msg.string(BODY) {
        sink.push_str(&body);
        sink.newline();
    } else if let Some(html) = msg.binary(BODY_HTML).and_then(|b| decode_html_bytes(&b)).or_else(|| msg.string(BODY_HTML)) {
        html_to_text(&html, sink, &mut DocMeta::default());
        sink.newline();
    } else if let Some(rtf) = msg.binary(RTF_COMPRESSED).and_then(|b| decompress_rtf(&b)) {
        // A damaged RTF body leaves the headers and attachments indexed.
        let _ = rtf_to_text(&rtf, sink, limits.max_depth);
        sink.newline();
    }

    for a in msg.children("__attach_version1.0_") {
        if sink.is_full() {
            break;
        }
        sink.check_deadline()?;
        msg.enter(&a, |m| -> ParseResult<()> {
            let inner = "__substg1.0_3701000D";
            if m.long(CHILD_HEADER, ATTACH_METHOD) == Some(ATTACH_EMBEDDED_MSG) || m.comp.is_storage(format!("{}/{inner}", m.path)) {
                if limits.container_depth + depth < MAX_NESTING {
                    m.enter(inner, |m| {
                        m.enc = m.codepage(EMBEDDED_HEADER);
                        let label = m.string(SUBJECT).map(|s| format!("Attached message: {s}")).unwrap_or_else(|| "Attached message".into());
                        begin_part(sink, label);
                        emit_message(m, EMBEDDED_HEADER, limits, sink, depth + 1)
                    })?;
                }
                return Ok(());
            }
            let name = [ATTACH_LONG_FILENAME, ATTACH_FILENAME, DISPLAY_NAME].into_iter().find_map(|id| m.string(id).filter(|n| !n.trim().is_empty()));
            if let (Some(name), Some(data)) = (name, m.binary(ATTACH_DATA)) {
                extract_embedded(&name, &data, format!("Attachment: {name}"), limits, sink)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// The dictionary's initial contents ([MS-OXRTFCP] 2.1.2.1).
const RTF_PREBUF: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

/// Decompress an RTF body ([MS-OXRTFCP]). `None` if the header is not recognised.
pub(super) fn decompress_rtf(data: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let raw_size = (u32_at(4)? as usize).min(MAX_EMBEDDED_BYTES);
    let body = data.get(16..)?;
    match data.get(8..12)? {
        b"MELA" => return Some(body[..raw_size.min(body.len())].to_vec()),
        b"LZFu" => {}
        _ => return None,
    }
    let mut dict = [0u8; 4096];
    dict[..RTF_PREBUF.len()].copy_from_slice(RTF_PREBUF);
    let mut w = RTF_PREBUF.len();
    let mut out = Vec::with_capacity(raw_size);
    let mut i = 0;
    'outer: while i < body.len() && out.len() < raw_size {
        let control = body[i];
        i += 1;
        for bit in 0..8 {
            if control & (1 << bit) == 0 {
                let Some(&b) = body.get(i) else { break 'outer };
                i += 1;
                out.push(b);
                dict[w] = b;
                w = (w + 1) % 4096;
            } else {
                let Some(t) = body.get(i..i + 2) else { break 'outer };
                i += 2;
                let token = u16::from_be_bytes([t[0], t[1]]) as usize;
                let (offset, len) = (token >> 4, (token & 0xF) + 2);
                if offset == w {
                    break 'outer;
                }
                for k in 0..len {
                    let b = dict[(offset + k) % 4096];
                    out.push(b);
                    dict[w] = b;
                    w = (w + 1) % 4096;
                }
            }
            if out.len() >= raw_size {
                break 'outer;
            }
        }
    }
    out.truncate(raw_size);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [MS-OXRTFCP] 3.1.1: compressed form of "{\rtf1\ansi\ansicpg1252\pard hello world}\r\n".
    const SPEC_SAMPLE: [u8; 49] = [
        0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03, 0x00, 0x0a, 0x00,
        0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20, 0x62, 0x77,
        0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0,
    ];

    #[test]
    fn rtf_dictionary_and_decompression_match_the_specification() {
        assert_eq!(RTF_PREBUF.len(), 207);
        let out = decompress_rtf(&SPEC_SAMPLE).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    }

    fn fixed(header: usize, props: &[(u16, u16, u64)]) -> Vec<u8> {
        let mut v = vec![0u8; header];
        for &(id, ty, val) in props {
            v.extend_from_slice(&(((id as u32) << 16) | ty as u32).to_le_bytes());
            v.extend_from_slice(&[0; 4]);
            v.extend_from_slice(&val.to_le_bytes());
        }
        v
    }

    fn parse(path: &Path) -> ParseResult<(String, Vec<super::super::Loc>, DocMeta)> {
        let limits = Limits::default();
        let mut sink = TextSink::new(1 << 20, None);
        let meta = MsgParser.extract(path, &ParseContext { limits: &limits }, &mut sink)?;
        let (text, locs, _) = sink.into_parts();
        Ok((text, locs, meta))
    }

    /// A minimal message: only a compressed RTF body, 8-bit strings in windows-1254, a Cc
    /// recipient and a text attachment.
    #[test]
    fn rtf_only_body_ansi_strings_and_attachments() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.msg");
        let mut c = cfb::create(&path).unwrap();
        // 2026-01-02T03:04:05Z as a FILETIME.
        let sent = (1_767_323_045u64 + 11_644_473_600) * 10_000_000;
        let streams: Vec<(&str, Vec<u8>)> = vec![
            ("/__properties_version1.0", fixed(TOP_HEADER, &[(MESSAGE_CODEPAGE, 0x0003, 1254), (CLIENT_SUBMIT_TIME, 0x0040, sent)])),
            ("/__substg1.0_0037001E", b"Ge\xe7i\xfe izni".to_vec()),
            ("/__substg1.0_0C1A001E", b"Ay\xfee".to_vec()),
            ("/__substg1.0_10090102", SPEC_SAMPLE.to_vec()),
            ("/__recip_version1.0_#00000000/__properties_version1.0", fixed(CHILD_HEADER, &[(RECIPIENT_TYPE, 0x0003, 2)])),
            ("/__recip_version1.0_#00000000/__substg1.0_39FE001E", b"cc@example.com".to_vec()),
            ("/__attach_version1.0_#00000000/__properties_version1.0", fixed(CHILD_HEADER, &[(ATTACH_METHOD, 0x0003, 1)])),
            ("/__attach_version1.0_#00000000/__substg1.0_3707001E", b"list.txt".to_vec()),
            ("/__attach_version1.0_#00000000/__substg1.0_37010102", b"puffin inventory\n".to_vec()),
        ];
        c.create_storage("/__recip_version1.0_#00000000").unwrap();
        c.create_storage("/__attach_version1.0_#00000000").unwrap();
        for (name, data) in streams {
            c.create_stream(name).unwrap().write_all(&data).unwrap();
        }
        c.flush().unwrap();
        drop(c);

        let (text, locs, meta) = parse(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Geçiş izni"));
        assert_eq!(meta.author.as_deref(), Some("Ayşe"));
        for w in ["From: Ayşe", "Cc: cc@example.com", "Date: 2026-01-02T03:04:05Z", "Subject: Geçiş izni", "hello world", "puffin inventory"] {
            assert!(text.contains(w), "{w}: {text}");
        }
        let at = text.find("puffin").unwrap();
        assert_eq!(super::super::locmap::describe(&locs, &text, at, false).as_deref(), Some("Attachment: list.txt"));
    }

    #[test]
    fn other_compound_files_are_rejected() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.msg");
        let mut c = cfb::create(&path).unwrap();
        c.create_stream("/WordDocument").unwrap().write_all(b"x").unwrap();
        c.flush().unwrap();
        drop(c);
        assert!(matches!(parse(&path), Err(ParseError::Corrupt(_))));
    }

    #[test]
    fn uncompressed_rtf_and_junk() {
        let mut d = vec![0u8; 16];
        d[4] = 5;
        d[8..12].copy_from_slice(b"MELA");
        d.extend_from_slice(b"{\\rtf1 hi}");
        assert_eq!(decompress_rtf(&d).unwrap(), b"{\\rtf");
        assert!(decompress_rtf(b"nonsense").is_none());
    }
}
