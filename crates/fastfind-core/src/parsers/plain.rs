//! Plain text: streaming decode with encoding detection. Never loads a whole file.

use std::fs::File;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::path::Path;

use encoding_rs::{CoderResult, Encoding, UTF_16BE, UTF_16LE, UTF_8};

use super::{DocMeta, DocumentParser, ParseContext, ParseError, ParseResult, TextMode, TextSink};

const READ_CHUNK: usize = 256 * 1024;
const DETECT_BYTES: usize = 64 * 1024;

/// Detect the encoding of a text file from its first bytes. `None` = binary (not text).
/// Returns the encoding and the BOM length to skip.
pub fn detect_encoding(head: &[u8]) -> Option<(&'static Encoding, usize)> {
    if let Some((enc, bom)) = Encoding::for_bom(head) {
        return Some((enc, bom));
    }
    // UTF-16 without BOM (NUL in every other byte for Latin text).
    if head.len() >= 16 {
        let n = head.len().min(4096) & !1;
        let pairs = (n / 2).max(1);
        let odd0 = (0..n).step_by(2).filter(|&i| head[i + 1] == 0).count();
        let even0 = (0..n).step_by(2).filter(|&i| head[i] == 0).count();
        if odd0 * 10 > pairs * 7 && even0 * 10 < pairs {
            return Some((UTF_16LE, 0));
        }
        if even0 * 10 > pairs * 7 && odd0 * 10 < pairs {
            return Some((UTF_16BE, 0));
        }
    }
    if head.contains(&0) {
        return None;
    }
    match std::str::from_utf8(head) {
        Ok(_) => return Some((UTF_8, 0)),
        // Incomplete multi-byte sequence cut at the end of the probe window is still UTF-8.
        Err(e) if e.error_len().is_none() => return Some((UTF_8, 0)),
        Err(_) => {}
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(head, false);
    Some((det.guess(None, chardetng::Utf8Detection::Allow), 0))
}

/// Stream a text file as decoded, line-aligned chunks. The callback may stop early by
/// returning `ControlFlow::Break`. Returns the detected encoding, or `Err(InvalidData)` for
/// binary files.
pub fn stream_text(
    path: &Path,
    mut f: impl FnMut(&str) -> ControlFlow<()>,
) -> io::Result<&'static Encoding> {
    let mut file = File::open(path)?;
    let mut head = vec![0u8; DETECT_BYTES];
    let mut n = 0;
    while n < head.len() {
        let r = file.read(&mut head[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    head.truncate(n);
    let (enc, bom) = detect_encoding(&head)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "binary content"))?;
    let mut decoder = enc.new_decoder_without_bom_handling();
    let mut out = String::with_capacity(READ_CHUNK + 1024);
    let mut carry = String::new();
    let mut input = head;
    input.drain(..bom);
    let mut buf = vec![0u8; READ_CHUNK];
    let mut last = false;
    loop {
        // Decode `input` completely into `out` (growing as needed).
        let mut src = &input[..];
        loop {
            out.reserve(decoder.max_utf8_buffer_length(src.len()).unwrap_or(src.len() * 3 + 16));
            let (res, read, _) = decoder.decode_to_string(src, &mut out, last);
            src = &src[read..];
            if res == CoderResult::InputEmpty {
                break;
            }
        }
        // Emit line-aligned text; keep the partial last line for the next round.
        let text = if carry.is_empty() { std::mem::take(&mut out) } else {
            let mut c = std::mem::take(&mut carry);
            c.push_str(&out);
            out.clear();
            c
        };
        let cut = if last { text.len() } else {
            match text.rfind('\n') {
                Some(p) => p + 1,
                None if text.len() > 4 * READ_CHUNK => text.len(),
                None => 0,
            }
        };
        if cut > 0 && f(&text[..cut]).is_break() {
            return Ok(enc);
        }
        carry.push_str(&text[cut..]);
        if last {
            break;
        }
        let r = file.read(&mut buf)?;
        if r == 0 {
            last = true;
            input.clear();
        } else {
            input.clear();
            input.extend_from_slice(&buf[..r]);
        }
    }
    Ok(enc)
}

pub struct PlainTextParser;

pub const PLAIN_EXTENSIONS: &[&str] = &[
    // prose & docs
    "txt", "text", "md", "markdown", "mdown", "rst", "adoc", "asciidoc", "org", "tex", "bib", "log",
    "nfo", "srt", "vtt", "eml", "mbox", "vcf", "ics", "diff", "patch",
    // tabular
    "csv", "tsv", "tab", "psv",
    // config
    "ini", "cfg", "conf", "config", "toml", "yaml", "yml", "properties", "env", "editorconfig",
    "gitignore", "gitattributes", "reg", "plist",
    // code
    "rs", "c", "h", "cpp", "hpp", "cc", "hh", "cxx", "hxx", "cs", "java", "kt", "kts", "go", "py",
    "pyw", "rb", "php", "pl", "pm", "lua", "r", "scala", "swift", "m", "mm", "js", "jsx", "ts", "tsx",
    "mjs", "cjs", "vue", "svelte", "css", "scss", "sass", "less", "sh", "bash", "zsh", "fish", "ps1",
    "psm1", "psd1", "bat", "cmd", "gradle", "cmake", "mk", "dockerfile", "tf", "hcl", "proto",
    "graphql", "gql", "dart", "ex", "exs", "erl", "hrl", "hs", "clj", "cljs", "fs", "fsx", "vb",
    "vbs", "asm", "s", "groovy", "jl", "nim", "zig", "sol", "sql", "ipynb", "rake", "gemspec",
    "cabal", "elm", "ml", "mli", "pas", "f90", "f", "for", "ada", "adb", "ads", "lisp", "el", "scm",
    "rkt", "v", "sv", "vhd", "vhdl", "tcl", "awk", "sed", "jsonl", "ndjson",
];

impl DocumentParser for PlainTextParser {
    fn name(&self) -> &'static str {
        "plain-text"
    }

    fn extensions(&self) -> &'static [&'static str] {
        PLAIN_EXTENSIONS
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        detect_encoding(header).is_some()
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Raw
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let res = stream_text(path, |chunk| {
            sink.push_str(chunk);
            if sink.is_full() { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        match res {
            Ok(_) => Ok(DocMeta::default()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(ParseError::Unsupported("binary content".into())),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_encodings() {
        assert_eq!(detect_encoding(b"hello").unwrap().0, UTF_8);
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFhi").unwrap(), (UTF_8, 3));
        assert_eq!(detect_encoding(b"\xFF\xFEh\0i\0").unwrap(), (UTF_16LE, 2));
        assert!(detect_encoding(b"ab\0cd").is_none());
        // Latin-1 "café résumé" is not valid UTF-8 → legacy single-byte guess.
        let (enc, _) = detect_encoding(b"caf\xe9 r\xe9sum\xe9 na\xefve fa\xe7ade").unwrap();
        assert_ne!(enc, UTF_8);
    }

    #[test]
    fn streams_line_aligned_chunks_and_decodes_utf16() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("u16.txt");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"\xFF\xFE").unwrap();
        let body: String = (0..50_000).map(|i| format!("line {i} ünïcode\n")).collect();
        for u in body.encode_utf16() {
            f.write_all(&u.to_le_bytes()).unwrap();
        }
        drop(f);
        let mut got = String::new();
        let mut chunks = 0;
        stream_text(&p, |c| {
            assert!(c.ends_with('\n'));
            got.push_str(c);
            chunks += 1;
            ControlFlow::Continue(())
        })
        .unwrap();
        assert!(chunks > 1);
        assert_eq!(got, body);
    }
}
