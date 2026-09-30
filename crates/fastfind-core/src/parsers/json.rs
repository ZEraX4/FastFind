//! JSON → text with a streaming lexer: keys, string values and numbers are emitted, structure
//! is dropped. No DOM is built, so multi-GB JSON and arbitrarily deep nesting are safe.
//! Invalid JSON degrades gracefully (strings are still extracted).

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use super::{DocMeta, DocumentParser, ParseContext, ParseResult, TextMode, TextSink};

pub struct JsonParser;

impl DocumentParser for JsonParser {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json", "geojson", "har", "webmanifest", "jsonc", "json5"]
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Reparse
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let r = BufReader::with_capacity(256 * 1024, File::open(path)?);
        json_to_text(r, sink)?;
        Ok(DocMeta::default())
    }
}

pub fn json_to_text<R: Read>(r: R, sink: &mut TextSink) -> ParseResult<()> {
    #[derive(PartialEq)]
    enum St {
        Out,
        Str,
        Esc,
        Uni(u8, u32),
        Num,
        Word,
    }
    let mut st = St::Out;
    let mut cur: Vec<u8> = Vec::with_capacity(256);
    let mut pending_high: Option<u32> = None;
    let mut buf = [0u8; 64 * 1024];
    let mut r = r;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sink.check_deadline()?;
        for &b in &buf[..n] {
            match st {
                St::Out => match b {
                    b'"' => {
                        st = St::Str;
                        cur.clear();
                    }
                    b'-' | b'0'..=b'9' => {
                        st = St::Num;
                        cur.clear();
                        cur.push(b);
                    }
                    b't' | b'f' | b'n' => st = St::Word,
                    b',' | b'}' | b']' => sink.newline(),
                    b':' => sink.push_str(": "),
                    _ => {}
                },
                St::Word => {
                    if !b.is_ascii_alphabetic() {
                        st = St::Out;
                        if matches!(b, b',' | b'}' | b']') {
                            sink.newline();
                        }
                    }
                }
                St::Num => {
                    if b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-') {
                        if cur.len() < 64 {
                            cur.push(b);
                        }
                    } else {
                        sink.push_str(std::str::from_utf8(&cur).unwrap_or(""));
                        st = St::Out;
                        if matches!(b, b',' | b'}' | b']') {
                            sink.newline();
                        } else if b == b':' {
                            sink.push_str(": ");
                        }
                    }
                }
                St::Str => match b {
                    b'"' => {
                        sink.push_str(&String::from_utf8_lossy(&cur));
                        cur.clear();
                        st = St::Out;
                    }
                    b'\\' => st = St::Esc,
                    _ => {
                        cur.push(b);
                        if cur.len() >= 16 * 1024 {
                            // Very long strings are flushed in pieces (on a UTF-8 boundary).
                            let cut = match std::str::from_utf8(&cur) {
                                Ok(_) => cur.len(),
                                Err(e) => e.valid_up_to(),
                            };
                            sink.push_str(std::str::from_utf8(&cur[..cut]).unwrap_or(""));
                            cur.drain(..cut);
                        }
                    }
                },
                St::Esc => {
                    st = St::Str;
                    match b {
                        b'n' => cur.push(b'\n'),
                        b't' => cur.push(b'\t'),
                        b'r' | b'b' | b'f' => cur.push(b' '),
                        b'u' => st = St::Uni(0, 0),
                        other => cur.push(other),
                    }
                }
                St::Uni(k, v) => {
                    let d = (b as char).to_digit(16);
                    match d {
                        Some(d) => {
                            let v = v * 16 + d;
                            if k == 3 {
                                st = St::Str;
                                if (0xD800..0xDC00).contains(&v) {
                                    pending_high = Some(v);
                                } else {
                                    let c = if (0xDC00..0xE000).contains(&v) {
                                        pending_high.take().and_then(|h| char::from_u32(0x10000 + ((h - 0xD800) << 10) + (v - 0xDC00)))
                                    } else {
                                        char::from_u32(v)
                                    };
                                    let mut tmp = [0u8; 4];
                                    cur.extend_from_slice(c.unwrap_or('\u{fffd}').encode_utf8(&mut tmp).as_bytes());
                                }
                            } else {
                                st = St::Uni(k + 1, v);
                            }
                        }
                        None => {
                            st = St::Str;
                            cur.push(b);
                        }
                    }
                }
            }
        }
        if sink.is_full() {
            return Ok(());
        }
    }
    if st == St::Num {
        sink.push_str(std::str::from_utf8(&cur).unwrap_or(""));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_keys_values_and_unicode_escapes() {
        let mut sink = TextSink::new(1 << 20, None);
        let src = r#"{"customer":"Acme \"Ltd\"","total":1234.5,"ok":true,"emoji":"😀","nested":[{"note":"line\nbreak"}]}"#;
        json_to_text(src.as_bytes(), &mut sink).unwrap();
        let t = sink.text();
        assert!(t.contains("customer: Acme \"Ltd\""));
        assert!(t.contains("total: 1234.5"));
        assert!(t.contains("😀"));
        assert!(t.contains("line\nbreak"));
        assert!(!t.contains("true"));
    }

    #[test]
    fn deep_nesting_is_fine() {
        let deep = "[".repeat(1_000_000) + "\"x\"" + &"]".repeat(1_000_000);
        let mut sink = TextSink::new(1 << 20, None);
        json_to_text(deep.as_bytes(), &mut sink).unwrap();
        assert!(sink.text().contains('x'));
    }
}
