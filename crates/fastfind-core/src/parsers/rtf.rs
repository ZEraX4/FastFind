//! RTF → text. Handles groups, destinations to skip (font/colour tables, pictures, objects,
//! field instructions), `\'hh` code-page bytes, `\uN` Unicode with `\ucN` fallback skipping,
//! and `\binN` binary runs. Group depth is bounded.
//!
//! RTF matters beyond `.rtf`: many `.doc` files in the wild are RTF saved with a .doc name.

use std::io::Read;
use std::path::Path;

use encoding_rs::{Encoding, WINDOWS_1252};

use super::{DocMeta, DocumentParser, ParseContext, ParseError, ParseResult, TextMode, TextSink};

const MAX_RTF_BYTES: u64 = 256 << 20;

pub struct RtfParser;

impl DocumentParser for RtfParser {
    fn name(&self) -> &'static str {
        "rtf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rtf"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        let h = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header);
        h.trim_ascii_start().starts_with(b"{\\rtf")
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut data = Vec::new();
        std::fs::File::open(path)?.take(MAX_RTF_BYTES).read_to_end(&mut data)?;
        rtf_to_text(&data, sink, ctx.limits.max_depth)
    }
}

const SKIP_DESTINATIONS: &[&str] = &[
    "fonttbl", "colortbl", "stylesheet", "pict", "object", "objdata", "fldinst", "themedata",
    "colorschememapping", "datastore", "latentstyles", "rsidtbl", "listtable", "listoverridetable",
    "generator", "xmlnstbl", "mmathPr", "filetbl", "revtbl", "pgdsctbl", "bkmkstart", "bkmkend",
    "wgrffmtfilter", "passwordhash", "protusertbl", "shppict", "nonshppict", "blipuid", "sp",
    "header", "footer", "headerl", "headerr", "headerf", "footerl", "footerr", "footerf",
];

#[derive(Clone)]
struct Group {
    skip: bool,
    uc: usize,
    meta_field: Option<u8>,
}

pub fn rtf_to_text(data: &[u8], sink: &mut TextSink, max_depth: usize) -> ParseResult<DocMeta> {
    let mut meta = DocMeta::default();
    let mut enc: &'static Encoding = WINDOWS_1252;
    let mut stack: Vec<Group> = vec![Group { skip: false, uc: 1, meta_field: None }];
    let mut i = 0;
    let mut skip_chars = 0usize; // fallback chars to skip after \uN
    let mut bytes_run: Vec<u8> = Vec::new(); // consecutive \'hh bytes (multi-byte code pages)
    let mut meta_buf = String::new();
    let mut destination_star = false;

    macro_rules! flush_bytes {
        () => {
            if !bytes_run.is_empty() {
                let (s, _) = enc.decode_without_bom_handling(&bytes_run);
                emit(sink, &mut meta_buf, stack.last().unwrap(), &s);
                bytes_run.clear();
            }
        };
    }

    while i < data.len() {
        if sink.is_full() {
            break;
        }
        sink.check_deadline()?;
        let c = data[i];
        match c {
            b'{' => {
                flush_bytes!();
                if stack.len() >= max_depth {
                    return Err(ParseError::LimitExceeded("RTF nesting too deep".into()));
                }
                let top = stack.last().unwrap().clone();
                stack.push(Group { meta_field: None, ..top });
                i += 1;
            }
            b'}' => {
                flush_bytes!();
                if let Some(g) = stack.pop() {
                    if let Some(f) = g.meta_field {
                        let v = meta_buf.trim().to_string();
                        if !v.is_empty() {
                            match f {
                                b't' => meta.title = Some(v),
                                b'a' => meta.author = Some(v),
                                b's' => meta.subject = Some(v),
                                _ => meta.keywords = Some(v),
                            }
                        }
                        meta_buf.clear();
                    }
                }
                if stack.is_empty() {
                    break;
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= data.len() {
                    break;
                }
                let n = data[i];
                if n == b'\'' {
                    // \'hh
                    if i + 2 < data.len() {
                        if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&data[i + 1..i + 3]).unwrap_or("x"), 16) {
                            if skip_chars > 0 {
                                skip_chars -= 1;
                            } else {
                                bytes_run.push(v);
                            }
                        }
                    }
                    i += 3;
                    continue;
                }
                flush_bytes!();
                if !n.is_ascii_alphabetic() {
                    // Control symbol.
                    match n {
                        b'*' => destination_star = true,
                        b'~' => emit(sink, &mut meta_buf, stack.last().unwrap(), " "),
                        b'_' => emit(sink, &mut meta_buf, stack.last().unwrap(), "-"),
                        b'\\' | b'{' | b'}' => {
                            let s = (n as char).to_string();
                            emit(sink, &mut meta_buf, stack.last().unwrap(), &s);
                        }
                        b'\n' | b'\r' => emit(sink, &mut meta_buf, stack.last().unwrap(), "\n"),
                        _ => {}
                    }
                    i += 1;
                    continue;
                }
                let ws = i;
                while i < data.len() && data[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = std::str::from_utf8(&data[ws..i]).unwrap_or("");
                let ns = i;
                if i < data.len() && (data[i] == b'-' || data[i].is_ascii_digit()) {
                    i += 1;
                    while i < data.len() && data[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let param: Option<i64> = std::str::from_utf8(&data[ns..i]).ok().and_then(|s| s.parse().ok());
                if i < data.len() && data[i] == b' ' {
                    i += 1;
                }
                let g = stack.last_mut().unwrap();
                if destination_star {
                    destination_star = false;
                    g.skip = true;
                    continue;
                }
                match word {
                    w if SKIP_DESTINATIONS.contains(&w) => g.skip = true,
                    "info" => {}
                    "title" => g.meta_field = Some(b't'),
                    "author" => g.meta_field = Some(b'a'),
                    "subject" => g.meta_field = Some(b's'),
                    "keywords" => g.meta_field = Some(b'k'),
                    "operator" | "company" | "doccomm" | "creatim" | "revtim" | "printim" | "buptim" => g.skip = true,
                    "ansicpg" => {
                        if let Some(cp) = param {
                            if let Some(e) = codepage(cp) {
                                enc = e;
                            }
                        }
                    }
                    "uc" => g.uc = param.unwrap_or(1).clamp(0, 16) as usize,
                    "u" => {
                        if let Some(mut v) = param {
                            if v < 0 {
                                v += 65536;
                            }
                            if let Some(ch) = char::from_u32(v as u32) {
                                let s = ch.to_string();
                                emit(sink, &mut meta_buf, stack.last().unwrap(), &s);
                            }
                            skip_chars = stack.last().unwrap().uc;
                        }
                    }
                    "par" | "line" | "sect" | "page" | "row" => emit(sink, &mut meta_buf, stack.last().unwrap(), "\n"),
                    "tab" | "cell" => emit(sink, &mut meta_buf, stack.last().unwrap(), "\t"),
                    "emdash" => emit(sink, &mut meta_buf, stack.last().unwrap(), "—"),
                    "endash" => emit(sink, &mut meta_buf, stack.last().unwrap(), "–"),
                    "bullet" => emit(sink, &mut meta_buf, stack.last().unwrap(), "•"),
                    "lquote" | "rquote" => emit(sink, &mut meta_buf, stack.last().unwrap(), "'"),
                    "ldblquote" | "rdblquote" => emit(sink, &mut meta_buf, stack.last().unwrap(), "\""),
                    "bin" => {
                        let len = param.unwrap_or(0).max(0) as usize;
                        i = i.saturating_add(len).min(data.len());
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                // Plain text run.
                let start = i;
                while i < data.len() && !matches!(data[i], b'{' | b'}' | b'\\' | b'\r' | b'\n') {
                    i += 1;
                }
                let mut run = &data[start..i];
                if skip_chars > 0 {
                    let k = skip_chars.min(run.len());
                    run = &run[k..];
                    skip_chars -= k;
                }
                if !run.is_empty() {
                    flush_bytes!();
                    let (s, _) = enc.decode_without_bom_handling(run);
                    emit(sink, &mut meta_buf, stack.last().unwrap(), &s);
                }
            }
        }
    }
    flush_bytes!();
    Ok(meta)
}

fn emit(sink: &mut TextSink, meta_buf: &mut String, g: &Group, s: &str) {
    if g.meta_field.is_some() {
        if meta_buf.len() < 4096 {
            meta_buf.push_str(s);
        }
    } else if !g.skip {
        sink.push_str(s);
    }
}

fn codepage(cp: i64) -> Option<&'static Encoding> {
    let label = match cp {
        65001 => "utf-8".to_string(),
        932 => "shift_jis".into(),
        936 => "gbk".into(),
        949 => "euc-kr".into(),
        950 => "big5".into(),
        874 => "windows-874".into(),
        1250..=1258 => format!("windows-{cp}"),
        10000 => "macintosh".into(),
        _ => return None,
    };
    Encoding::for_label(label.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> (String, DocMeta) {
        let mut sink = TextSink::new(1 << 20, None);
        let meta = rtf_to_text(s.as_bytes(), &mut sink, 64).unwrap();
        (sink.into_parts().0, meta)
    }

    #[test]
    fn extracts_text_unicode_and_metadata() {
        // `@` stands for a backslash in front of `u8364` (a Unicode escape for the euro sign).
        let src = r"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\info{\title Budget 2026}{\author Jane}}{\*\generator Word;}\f0 Hello \b world\b0 !\par caf\'e9 @u8364?  price\tab 10\par {\field{\*\fldinst HYPERLINK x}{\fldrslt link text}}}"
            .replace('@', &char::from(92u8).to_string());
        let (t, m) = run(&src);
        // RTF consumes the single space that delimits a control word: "\b0 !" → "!".
        assert!(t.contains("Hello world!"), "{t}");
        assert!(t.contains("café €"), "{t}");
        assert!(t.contains("price\t10"));
        assert!(t.contains("link text"));
        assert!(!t.contains("Arial"));
        assert!(!t.contains("HYPERLINK"));
        assert!(!t.contains("Budget"));
        assert_eq!(m.title.as_deref(), Some("Budget 2026"));
        assert_eq!(m.author.as_deref(), Some("Jane"));
    }

    #[test]
    fn depth_limit() {
        let deep = format!("{{\\rtf1 {}x{}}}", "{".repeat(1000), "}".repeat(1000));
        let mut sink = TextSink::new(1 << 20, None);
        assert!(matches!(rtf_to_text(deep.as_bytes(), &mut sink, 64), Err(ParseError::LimitExceeded(_))));
    }
}
