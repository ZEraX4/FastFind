//! Outlook archives (`.pst`, [MS-PST]) through Microsoft's `outlook-pst` crate.
//!
//! An archive holds many e-mails, and each becomes its own search result (a *member* of the
//! archive file, see `index::members`). `read_archive` walks every folder under the mailbox
//! root and hands each message, as indexable text, to a callback; `export_eml` writes one
//! message as an `.eml` file so it can be opened in the default mail app.
//!
//! Message text matches `.msg` and `.eml`: a From / To / Cc / Date / Subject block, the body
//! (plain text, else HTML, else compressed RTF), then each attachment after a `Part` anchor,
//! read by the regular parsers. Attached Outlook items contribute their headers and body.
//!
//! Both PST variants are read: Unicode (Outlook 2003 and later) and ANSI (older). Offline
//! mailbox caches (`.ost`) use a different header and are not supported by the crate.

use std::io::{self, Read, Seek};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use encoding_rs::{Encoding, WINDOWS_1252};
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::messaging::attachment::{AnsiAttachment, Attachment, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::message::{AnsiMessage, Message, UnicodeMessage};
use outlook_pst::messaging::store::{AnsiStore, EntryId, Store, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::{AnsiPstFile, UnicodePstFile};

use super::embedded::{begin_part, extract_embedded, MAX_NESTING};
use super::html::{decode_html_bytes, html_to_text};
use super::msg::decompress_rtf;
use super::rtf::{codepage, rtf_to_text};
use super::{Limits, Loc, ParseError, ParseResult, TextSink};

/// One e-mail of an archive, ready to index.
pub struct ArchivedMessage {
    /// Node ID: stable within the archive, used to find the message again.
    pub id: u32,
    /// Folder path, e.g. "Inbox › Projects".
    pub folder: String,
    pub subject: String,
    pub sender: Option<String>,
    /// Unix seconds (sent, else received, else created).
    pub time: Option<i64>,
    pub size: u64,
    pub text: String,
    pub locs: Vec<Loc>,
    pub truncated: bool,
}

pub struct ArchiveSummary {
    pub messages: u64,
    /// Messages that could not be read (damaged) and were skipped.
    pub skipped: u64,
}

/// Read every message of the archive at `path`. Each message is limited separately: its text
/// by `limits.max_text_bytes`, its time by `per_message` (`limits.deadline` is ignored, an
/// archive can take far longer than one file). `keep_going` is polled between messages.
pub fn read_archive(
    path: &Path,
    limits: &Limits,
    per_message: Option<Duration>,
    keep_going: &dyn Fn() -> bool,
    mut on_message: impl FnMut(ArchivedMessage),
) -> ParseResult<ArchiveSummary> {
    let pst = Pst::open(path)?;
    let store = pst.store();
    let root = store.properties().ipm_sub_tree_entry_id().map_err(corrupt)?;
    let root = store.open_folder(&root).map_err(corrupt)?;
    let mut summary = ArchiveSummary { messages: 0, skipped: 0 };
    let mut stack: Vec<(Rc<dyn Folder>, String)> = vec![(root, String::new())];
    while let Some((folder, path_name)) = stack.pop() {
        if !keep_going() {
            return Err(ParseError::Timeout);
        }
        if let Some(sub) = folder.hierarchy_table() {
            for row in sub.rows_matrix() {
                let Ok(entry) = store.properties().make_entry_id(NodeId::from(u32::from(row.id()))) else { continue };
                let Ok(child) = store.open_folder(&entry) else { continue };
                let name = child.properties().display_name().unwrap_or_default();
                let child_path = if path_name.is_empty() { name } else { format!("{path_name} › {name}") };
                stack.push((child, child_path));
            }
        }
        let Some(contents) = folder.contents_table() else { continue };
        for row in contents.rows_matrix() {
            if !keep_going() {
                return Err(ParseError::Timeout);
            }
            let id = u32::from(row.id());
            let limits = Limits { deadline: per_message.map(|d| Instant::now() + d), ..limits.clone() };
            match pst.message(id).map(|m| read_message(&m, id, &path_name, &limits)) {
                Ok(m) => {
                    summary.messages += 1;
                    on_message(m);
                }
                Err(e) => {
                    tracing::debug!(archive = %path.display(), id, error = %e, "archived message unreadable");
                    summary.skipped += 1;
                }
            }
        }
    }
    Ok(summary)
}

fn corrupt(e: io::Error) -> ParseError {
    ParseError::corrupt(e)
}

/// The store, as one of the crate's two concrete variants: opening attachments needs them.
enum Pst {
    Unicode(Rc<UnicodeStore>),
    Ansi(Rc<AnsiStore>),
}

/// A message of either variant.
enum Msg {
    Unicode(Rc<UnicodeMessage>),
    Ansi(Rc<AnsiMessage>),
}

impl Pst {
    fn open(path: &Path) -> ParseResult<Pst> {
        // Header: "!BDN" magic, CRC, "SM" client magic, then wVer (14/15 ANSI, 23 Unicode).
        // Read-only, so an archive Outlook has open can still be indexed.
        let mut file = std::fs::File::open(path)?;
        let mut head = [0u8; 12];
        if file.read_exact(&mut head).is_err() || &head[..4] != b"!BDN" {
            return Err(ParseError::Corrupt("not an Outlook archive".into()));
        }
        let version = u16::from_le_bytes([head[10], head[11]]);
        file.seek(io::SeekFrom::Start(0))?;
        let reader = Box::new(io::BufReader::new(file));
        // Outlook locks byte ranges of an archive it has open (ERROR_LOCK_VIOLATION on Windows):
        // that is an I/O error worth retrying later, not a damaged file.
        let bad = |e: io::Error| match (e.kind(), e.raw_os_error()) {
            (io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData, _) => {
                ParseError::Corrupt(format!("unreadable Outlook archive ({e})"))
            }
            (_, Some(_)) => ParseError::Io(e),
            _ => ParseError::Corrupt(format!("unreadable Outlook archive ({e})")),
        };
        // "SO" instead of "SM": an offline mailbox cache (.ost), whatever its extension.
        if &head[8..10] == b"SO" {
            return Err(ParseError::Unsupported("Outlook offline mailbox cache".into()));
        }
        match version {
            23 => {
                let f = UnicodePstFile::read_from(reader).map_err(bad)?;
                Ok(Pst::Unicode(UnicodeStore::read(Rc::new(f)).map_err(corrupt)?))
            }
            14 | 15 => {
                let f = AnsiPstFile::read_from(reader).map_err(bad)?;
                Ok(Pst::Ansi(AnsiStore::read(Rc::new(f)).map_err(corrupt)?))
            }
            // 4 KiB pages (Outlook 2013 and later offline caches), which the reader doesn't support.
            36 => Err(ParseError::Unsupported("Outlook data file with 4 KB pages".into())),
            v => Err(ParseError::Corrupt(format!("unknown Outlook archive version {v}"))),
        }
    }

    fn store(&self) -> Rc<dyn Store> {
        match self {
            Pst::Unicode(s) => s.clone(),
            Pst::Ansi(s) => s.clone(),
        }
    }

    fn entry(&self, id: u32) -> io::Result<EntryId> {
        self.store().properties().make_entry_id(NodeId::from(id))
    }

    fn message(&self, id: u32) -> io::Result<Msg> {
        let entry = self.entry(id)?;
        Ok(match self {
            Pst::Unicode(s) => Msg::Unicode(UnicodeMessage::read(s.clone(), &entry, None)?),
            Pst::Ansi(s) => Msg::Ansi(AnsiMessage::read(s.clone(), &entry, None)?),
        })
    }
}

impl Msg {
    fn get(&self) -> &dyn Message {
        match self {
            Msg::Unicode(m) => m.as_ref(),
            Msg::Ansi(m) => m.as_ref(),
        }
    }

    fn attachment(&self, row: u32) -> io::Result<Rc<dyn Attachment>> {
        Ok(match self {
            Msg::Unicode(m) => UnicodeAttachment::read(m.clone(), NodeId::from(row), None)?,
            Msg::Ansi(m) => AnsiAttachment::read(m.clone(), NodeId::from(row), None)?,
        })
    }
}

// Property IDs (same as in .msg files).
const SUBJECT: u16 = 0x0037;
const CLIENT_SUBMIT_TIME: u16 = 0x0039;
const SENT_REPRESENTING_NAME: u16 = 0x0042;
const SENT_REPRESENTING_EMAIL: u16 = 0x0065;
const SENDER_NAME: u16 = 0x0C1A;
const SENDER_EMAIL: u16 = 0x0C1F;
const RECIPIENT_TYPE: u16 = 0x0C15;
const MESSAGE_SIZE: u16 = 0x0E08;
const DISPLAY_CC: u16 = 0x0E03;
const DISPLAY_TO: u16 = 0x0E04;
const DELIVERY_TIME: u16 = 0x0E06;
const BODY: u16 = 0x1000;
const RTF_COMPRESSED: u16 = 0x1009;
const BODY_HTML: u16 = 0x1013;
const CREATION_TIME: u16 = 0x3007;
const DISPLAY_NAME: u16 = 0x3001;
const EMAIL_ADDRESS: u16 = 0x3003;
const ATTACH_FILENAME: u16 = 0x3704;
const ATTACH_LONG_FILENAME: u16 = 0x3707;
const ATTACH_MIME_TAG: u16 = 0x370E;
const SMTP_ADDRESS: u16 = 0x39FE;
const INTERNET_CPID: u16 = 0x3FDE;
const MESSAGE_CODEPAGE: u16 = 0x3FFD;
const SENDER_SMTP_ADDRESS: u16 = 0x5D01;

/// Property access over a message or attachment property bag.
struct Props<'a> {
    get: Box<dyn Fn(u16) -> Option<&'a PropertyValue> + 'a>,
    enc: &'static Encoding,
}

impl<'a> Props<'a> {
    fn of_message(m: &'a dyn Message) -> Props<'a> {
        let p = m.properties();
        let mut props = Props { get: Box::new(move |id| p.get(id)), enc: WINDOWS_1252 };
        let cp = [INTERNET_CPID, MESSAGE_CODEPAGE].into_iter().find_map(|id| match props.raw(id) {
            Some(PropertyValue::Integer32(c)) => codepage(*c as i64),
            _ => None,
        });
        props.enc = cp.unwrap_or(WINDOWS_1252);
        props
    }

    fn raw(&self, id: u16) -> Option<&'a PropertyValue> {
        (self.get)(id)
    }

    fn string(&self, id: u16) -> Option<String> {
        let s = value_string(self.raw(id)?, self.enc)?;
        let s = s.trim_end_matches('\0').to_string();
        (!s.trim().is_empty()).then_some(s)
    }

    /// The subject without the marker an archive may store before it: U+0001, then one
    /// character holding the length of a "RE: " style prefix.
    fn subject(&self) -> Option<String> {
        let s = self.string(SUBJECT)?;
        let s = match s.strip_prefix('\u{1}') {
            Some(rest) => rest.chars().skip(1).collect(),
            None => s,
        };
        (!s.trim().is_empty()).then_some(s)
    }

    fn binary(&self, id: u16) -> Option<&'a [u8]> {
        match self.raw(id)? {
            PropertyValue::Binary(b) => Some(b.buffer()),
            _ => None,
        }
    }

    fn time(&self, id: u16) -> Option<i64> {
        match self.raw(id)? {
            PropertyValue::Time(t) if *t > 0 => Some(filetime_to_unix(*t)),
            _ => None,
        }
    }

    fn int(&self, id: u16) -> Option<i64> {
        match self.raw(id)? {
            PropertyValue::Integer32(v) => Some(*v as i64),
            PropertyValue::Integer64(v) => Some(*v),
            _ => None,
        }
    }

    fn sender(&self) -> (Option<String>, Option<String>) {
        let name = self.string(SENDER_NAME).or_else(|| self.string(SENT_REPRESENTING_NAME));
        let addr = [SENDER_SMTP_ADDRESS, SENDER_EMAIL, SENT_REPRESENTING_EMAIL].into_iter().find_map(|id| self.string(id).filter(|a| a.contains('@')));
        (name, addr)
    }

    fn time_sent(&self) -> Option<i64> {
        [CLIENT_SUBMIT_TIME, DELIVERY_TIME, CREATION_TIME].into_iter().find_map(|id| self.time(id))
    }
}

fn value_string(v: &PropertyValue, enc: &'static Encoding) -> Option<String> {
    match v {
        PropertyValue::Unicode(u) => Some(u.to_string()),
        PropertyValue::String8(s) => Some(enc.decode_without_bom_handling(s.buffer()).0.into_owned()),
        _ => None,
    }
}

/// FILETIME (100 ns ticks since 1601) to Unix seconds.
fn filetime_to_unix(t: i64) -> i64 {
    t / 10_000_000 - 11_644_473_600
}

fn format_address(name: Option<String>, addr: Option<String>) -> Option<String> {
    match (name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()), addr) {
        (Some(n), Some(a)) if n != a => Some(format!("{n} <{a}>")),
        (Some(n), _) => Some(n),
        (None, a) => a,
    }
}

/// To, Cc and Bcc lists from the recipient table (or the display properties).
fn recipients(m: &dyn Message, props: &Props) -> [Option<String>; 3] {
    let mut lists: [Vec<String>; 3] = Default::default();
    if let Some(table) = m.recipient_table() {
        let ctx = table.context();
        for row in table.rows_matrix() {
            let Ok(values) = row.columns(ctx) else { continue };
            let col = |id: u16| -> Option<PropertyValue> {
                let i = ctx.columns().iter().position(|c| c.prop_id() == id)?;
                table.read_column(values[i].as_ref()?, ctx.columns()[i].prop_type()).ok()
            };
            let kind = match col(RECIPIENT_TYPE) {
                Some(PropertyValue::Integer32(k)) => (k & 0xF) as usize,
                _ => 1,
            };
            let text = |id| col(id).and_then(|v| value_string(&v, props.enc)).map(|s| s.trim_end_matches('\0').to_string());
            let addr = [SMTP_ADDRESS, EMAIL_ADDRESS].into_iter().find_map(|id| text(id).filter(|a| a.contains('@') && !a.starts_with('/')));
            if let (Some(list), Some(a)) = (lists.get_mut(kind.wrapping_sub(1)), format_address(text(DISPLAY_NAME), addr)) {
                list.push(a);
            }
        }
    }
    let [to, cc, bcc] = lists.map(|l| (!l.is_empty()).then(|| l.join(", ")));
    [to.or_else(|| props.string(DISPLAY_TO)), cc.or_else(|| props.string(DISPLAY_CC)), bcc]
}

fn read_message(m: &Msg, id: u32, folder: &str, limits: &Limits) -> ArchivedMessage {
    let msg = m.get();
    let props = Props::of_message(msg);
    let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
    let (name, addr) = props.sender();
    let subject = props.subject().unwrap_or_default();
    emit_message(msg, &props, Some(m), limits, &mut sink, 0);
    let (text, locs, truncated) = sink.into_parts();
    ArchivedMessage {
        id,
        folder: folder.to_string(),
        sender: name.clone().or(addr.clone()),
        time: props.time_sent(),
        size: props.int(MESSAGE_SIZE).map(|s| s.max(0) as u64).unwrap_or(text.len() as u64),
        subject,
        text,
        locs,
        truncated,
    }
}

/// Headers, body and (for top-level messages, whose attachments can be opened) attachments.
fn emit_message(msg: &dyn Message, props: &Props, opened: Option<&Msg>, limits: &Limits, sink: &mut TextSink, depth: usize) {
    let line = |sink: &mut TextSink, label: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            sink.push_str(label);
            sink.push_str(": ");
            sink.push_str(&v);
            sink.newline();
        }
    };
    let (name, addr) = props.sender();
    line(sink, "From", format_address(name, addr));
    let [to, cc, bcc] = recipients(msg, props);
    line(sink, "To", to);
    line(sink, "Cc", cc);
    line(sink, "Bcc", bcc);
    line(sink, "Date", props.time_sent().map(|t| mail_parser::DateTime::from_timestamp(t).to_rfc3339()));
    line(sink, "Subject", props.subject());
    sink.newline();
    if let Some(body) = props.string(BODY) {
        sink.push_str(&body);
        sink.newline();
    } else if let Some(html) = props.binary(BODY_HTML).and_then(decode_html_bytes).or_else(|| props.string(BODY_HTML)) {
        html_to_text(&html, sink, &mut super::DocMeta::default());
        sink.newline();
    } else if let Some(rtf) = props.binary(RTF_COMPRESSED).and_then(decompress_rtf) {
        let _ = rtf_to_text(&rtf, sink, limits.max_depth);
        sink.newline();
    }

    let Some(table) = msg.attachment_table() else { return };
    for row in table.rows_matrix() {
        if sink.is_full() || sink.check_deadline().is_err() {
            break;
        }
        let attachment = match opened {
            Some(m) => match m.attachment(u32::from(row.id())) {
                Ok(a) => a,
                Err(_) => continue,
            },
            // Attachments of attached messages: only their names are known here.
            None => continue,
        };
        let ap = attachment.properties();
        let aprops = Props { get: Box::new(|id| ap.get(id)), enc: props.enc };
        let file = [ATTACH_LONG_FILENAME, ATTACH_FILENAME, DISPLAY_NAME].into_iter().find_map(|id| aprops.string(id));
        match attachment.data() {
            Some(AttachmentData::Binary(data)) => {
                if let Some(file) = file {
                    let _ = extract_embedded(&file, data.buffer(), format!("Attachment: {file}"), limits, sink);
                }
            }
            Some(AttachmentData::Message(inner)) if limits.container_depth + depth < MAX_NESTING => {
                let iprops = Props::of_message(inner.as_ref());
                let label = iprops.subject().map(|s| format!("Attached message: {s}")).unwrap_or_else(|| "Attached message".into());
                begin_part(sink, label);
                emit_message(inner.as_ref(), &iprops, None, limits, sink, depth + 1);
            }
            _ => {}
        }
    }
}

// ----- export --------------------------------------------------------------------------------

/// Message `id` of the archive at `path` as an RFC 5322 (`.eml`) file, with its attachments.
pub fn export_eml(path: &Path, id: u32) -> ParseResult<Vec<u8>> {
    let pst = Pst::open(path)?;
    let m = pst.message(id).map_err(|e| ParseError::Corrupt(format!("message not found ({e})")))?;
    let msg = m.get();
    let props = Props::of_message(msg);
    let mut out = String::new();
    let header = |out: &mut String, name: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(&encode_header(&v));
            out.push_str("\r\n");
        }
    };
    let (name, addr) = props.sender();
    header(&mut out, "From", format_address(name, addr));
    let [to, cc, _] = recipients(msg, &props);
    header(&mut out, "To", to);
    header(&mut out, "Cc", cc);
    header(&mut out, "Subject", props.subject());
    if let Some(t) = props.time_sent().and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        out.push_str(&format!("Date: {}\r\n", t.to_rfc2822()));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("X-Unsent: 0\r\n");

    let text = props.string(BODY);
    let html = props.binary(BODY_HTML).and_then(decode_html_bytes).or_else(|| props.string(BODY_HTML));
    let mut parts: Vec<String> = Vec::new();
    match (&text, &html) {
        (Some(t), Some(h)) => {
            let b = "ff-alt";
            parts.push(format!(
                "Content-Type: multipart/alternative; boundary=\"{b}\"\r\n\r\n--{b}\r\n{}\r\n--{b}\r\n{}\r\n--{b}--\r\n",
                text_part("text/plain", t),
                text_part("text/html", h)
            ));
        }
        (Some(t), None) => parts.push(text_part("text/plain", t)),
        (None, Some(h)) => parts.push(text_part("text/html", h)),
        (None, None) => parts.push(text_part("text/plain", "")),
    }
    if let Some(table) = msg.attachment_table() {
        for row in table.rows_matrix() {
            let Ok(a) = m.attachment(u32::from(row.id())) else { continue };
            let ap = a.properties();
            let aprops = Props { get: Box::new(|id| ap.get(id)), enc: props.enc };
            let Some(AttachmentData::Binary(data)) = a.data() else { continue };
            let file = [ATTACH_LONG_FILENAME, ATTACH_FILENAME, DISPLAY_NAME].into_iter().find_map(|id| aprops.string(id)).unwrap_or_else(|| "attachment".into());
            let mime = aprops.string(ATTACH_MIME_TAG).unwrap_or_else(|| "application/octet-stream".into());
            parts.push(format!(
                "Content-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}",
                name = encode_header(&file).replace('"', ""),
                body = base64_lines(data.buffer())
            ));
        }
    }
    if parts.len() == 1 {
        out.push_str(&parts.remove(0));
    } else {
        let b = "ff-mixed";
        out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n"));
        for p in parts {
            out.push_str(&format!("--{b}\r\n{p}\r\n"));
        }
        out.push_str(&format!("--{b}--\r\n"));
    }
    Ok(out.into_bytes())
}

/// A UTF-8 text part, base64-encoded so any content and line length is safe.
fn text_part(mime: &str, body: &str) -> String {
    format!("Content-Type: {mime}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}", base64_lines(body.as_bytes()))
}

fn base64_lines(data: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 38);
    for chunk in encoded.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

/// RFC 2047 encoded-word for non-ASCII header text.
fn encode_header(v: &str) -> String {
    let v = v.replace(['\r', '\n'], " ");
    if v.is_ascii() {
        return v;
    }
    use base64::Engine;
    format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(v.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_and_headers() {
        // 2026-01-02T03:04:05Z
        assert_eq!(filetime_to_unix((1_767_323_045 + 11_644_473_600) * 10_000_000), 1_767_323_045);
        assert_eq!(encode_header("Budget"), "Budget");
        assert_eq!(encode_header("Geçiş"), "=?UTF-8?B?R2XDp2nFnw==?=");
        assert_eq!(base64_lines(&[0u8; 60]).lines().map(str::len).collect::<Vec<_>>(), [76, 4]);
    }

    #[test]
    fn non_archives_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.pst");
        std::fs::write(&p, b"not an archive at all").unwrap();
        assert!(read_archive(&p, &Limits::default(), None, &|| true, |_| {}).is_err());
    }
}
