//! Zip archives: the names of the files inside, and the content of those FastFind can read.
//!
//! Each file gets a `Part("In archive: path")` anchor followed by its path, so a file inside an
//! archive is found by name, then by its text: plain-text files are decoded in memory, other
//! documents go through the regular parsers (`embedded`), keeping their page and sheet
//! locations ("Page 2 · In archive: docs/report.pdf"). Archives inside the archive are opened
//! the same way, up to `MAX_NESTING` levels.
//!
//! Decompression goes through `SafeZip`: the archive's total output is bounded, entries with
//! bomb-like compression ratios are refused, and nothing is written outside a private
//! temporary folder. Encrypted entries keep only their name.
//!
//! Names: zips made by Windows' "Compressed folder" (before Windows 11) and many older tools
//! store names in the PC's DOS code page (CP857 on Turkish Windows, CP720 on Arabic) without
//! saying so, and the zip crate then decodes them as CP437, garbling them. Like Explorer, names
//! that are not valid UTF-8 are decoded with the system's OEM code page on Windows.

use std::io::Read;
use std::path::Path;

use super::embedded::{begin_part, extract_embedded, registry, MAX_EMBEDDED_BYTES, MAX_NESTING};
use super::plain::{detect_encoding, PLAIN_EXTENSIONS};
use super::sniff::{sniff, Sniffed};
use super::zipsafe::SafeZip;
use super::{DocMeta, DocumentParser, ParseContext, ParseResult, TextMode, TextSink};
use crate::util::extension_of;

pub struct ZipParser;

impl DocumentParser for ZipParser {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["zip"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Zip
    }

    fn text_mode(&self) -> TextMode {
        // Decompressing again for every snippet would be slow.
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let limits = ctx.limits;
        let mut zip = SafeZip::open_any_size(path, limits)?;
        let count = zip.len().min(limits.max_zip_entries);
        for i in 0..count {
            if sink.is_full() {
                break;
            }
            sink.check_deadline()?;
            let Some(decoded) = zip.name_at(i).map(str::to_string) else { continue };
            let name = zip.raw_name_at(i).and_then(|raw| better_name(&raw)).unwrap_or(decoded);
            if name.ends_with('/') {
                continue;
            }
            let label = format!("In archive: {name}");
            begin_part(sink, label.clone());
            sink.push_str(&name);
            sink.newline();

            let ext = extension_of(Path::new(&name));
            let plain = PLAIN_EXTENSIONS.contains(&ext.as_str());
            if !plain && (limits.container_depth >= MAX_NESTING || !registry().has_parser_for(&ext)) {
                continue;
            }
            // Damaged, encrypted, oversized or bomb-like entries keep only their name.
            let Some(data) = read_entry(&mut zip, i) else { continue };
            if plain {
                if let Some((enc, bom)) = detect_encoding(&data[..data.len().min(64 << 10)]) {
                    sink.push_str(&enc.decode_without_bom_handling(&data[bom..]).0);
                    sink.newline();
                }
            } else {
                extract_embedded(&name, &data, label, limits, sink)?;
            }
        }
        if zip.len() > count {
            sink.mark_truncated();
        }
        Ok(DocMeta::default())
    }
}

/// A better reading of an entry name than the zip crate's CP437 fallback, if there is one: the
/// bytes as UTF-8 when they are valid UTF-8, otherwise (on Windows) the OEM code page.
fn better_name(raw: &[u8]) -> Option<String> {
    if raw.is_ascii() {
        return None;
    }
    if let Ok(s) = std::str::from_utf8(raw) {
        return Some(s.to_string());
    }
    decode_codepage(raw, oem_codepage()?)
}

#[cfg(windows)]
fn oem_codepage() -> Option<u32> {
    // SAFETY: no arguments, no side effects.
    Some(unsafe { windows_sys::Win32::Globalization::GetOEMCP() })
}

#[cfg(not(windows))]
fn oem_codepage() -> Option<u32> {
    None
}

#[cfg(windows)]
fn decode_codepage(bytes: &[u8], codepage: u32) -> Option<String> {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, MB_ERR_INVALID_CHARS};
    let len = i32::try_from(bytes.len()).ok()?;
    // SAFETY: the input pointer and length describe `bytes`; the output buffer is sized by
    // the first call, which only measures.
    unsafe {
        let n = MultiByteToWideChar(codepage, MB_ERR_INVALID_CHARS, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return None;
        }
        let mut wide = vec![0u16; n as usize];
        let written = MultiByteToWideChar(codepage, MB_ERR_INVALID_CHARS, bytes.as_ptr(), len, wide.as_mut_ptr(), n);
        (written == n).then(|| String::from_utf16_lossy(&wide))
    }
}

#[cfg(not(windows))]
fn decode_codepage(_: &[u8], _: u32) -> Option<String> {
    None
}

fn read_entry(zip: &mut SafeZip, index: usize) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    zip.open_index(index).ok()?.take(MAX_EMBEDDED_BYTES as u64 + 1).read_to_end(&mut data).ok()?;
    (data.len() <= MAX_EMBEDDED_BYTES).then_some(data)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::parsers::locmap::describe;
    use crate::parsers::{Limits, Loc};

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, zip::write::SimpleFileOptions::default()).unwrap();
            } else {
                w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
    }

    fn parse(path: &Path) -> (String, Vec<Loc>, bool) {
        let limits = Limits::default();
        let mut sink = TextSink::new(1 << 20, None);
        ZipParser.extract(path, &ParseContext { limits: &limits }, &mut sink).unwrap();
        sink.into_parts()
    }

    fn location(text: &str, locs: &[Loc], needle: &str) -> Option<String> {
        describe(locs, text, text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {text:?}")), false)
    }

    #[test]
    fn names_and_text_of_entries_are_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner.zip");
        make_zip(&inner, &[("deep/notes.md", b"the narwhal ledger")]);
        let outer = dir.path().join("a.zip");
        let windows_1254 = b"Ge\xe7i\xfe izni \xe7ok \xf6nemli";
        make_zip(&outer, &[
            ("docs/", b""),
            ("docs/plan.txt", b"budget for the heron survey\n"),
            ("docs/turkish.txt", windows_1254),
            ("photo.jpg", b"\xff\xd8\xff\xe0 not really"),
            ("nested.zip", &std::fs::read(&inner).unwrap()),
        ]);
        let (text, locs, truncated) = parse(&outer);
        assert!(!truncated);
        assert_eq!(location(&text, &locs, "heron survey").as_deref(), Some("In archive: docs/plan.txt"));
        assert!(text.contains("Geçiş izni çok önemli"), "legacy encodings are detected: {text}");
        assert_eq!(location(&text, &locs, "photo.jpg").as_deref(), Some("In archive: photo.jpg"), "names of unreadable files are indexed");
        assert!(!text.contains("not really"));
        assert_eq!(location(&text, &locs, "narwhal").as_deref(), Some("In archive: deep/notes.md · In archive: nested.zip"));
    }

    #[test]
    fn utf8_names_are_kept_even_without_the_utf8_flag() {
        assert_eq!(better_name("Geçiş izni.txt".as_bytes()).as_deref(), Some("Geçiş izni.txt"));
        assert_eq!(better_name("تقرير.pdf".as_bytes()).as_deref(), Some("تقرير.pdf"));
        assert_eq!(better_name(b"plain.txt"), None, "ASCII: the zip crate's reading is right");
    }

    #[cfg(windows)]
    #[test]
    fn dos_code_page_names_decode_like_explorer() {
        // "Geçiş izni.txt" as Turkish Windows' Compressed folder stores it (CP857).
        let cp857 = b"Ge\x87i\x9f izni.txt";
        assert_eq!(decode_codepage(cp857, 857).as_deref(), Some("Geçiş izni.txt"));
        // "Café" in CP850 (Western Europe).
        assert_eq!(decode_codepage(b"Caf\x82", 850).as_deref(), Some("Café"));
        assert!(better_name(cp857).is_some(), "not UTF-8, so the OEM code page is used");
    }

    #[test]
    fn an_archive_containing_itself_stops_at_the_nesting_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = Vec::new();
        for level in 0..8 {
            let p = dir.path().join(format!("l{level}.zip"));
            let name = format!("level{level}.txt");
            let text = format!("marker{level}");
            if data.is_empty() {
                make_zip(&p, &[(&name, text.as_bytes())]);
            } else {
                make_zip(&p, &[(&name, text.as_bytes()), ("next.zip", &data)]);
            }
            data = std::fs::read(&p).unwrap();
        }
        let (text, _, _) = parse(&dir.path().join("l7.zip"));
        // The top archive plus MAX_NESTING levels below it.
        for level in (7 - MAX_NESTING)..=7 {
            assert!(text.contains(&format!("marker{level}")), "level {level}: {text}");
        }
        assert!(!text.contains(&format!("marker{}", 6 - MAX_NESTING)), "{text}");
    }

    #[test]
    fn entry_count_beyond_the_limit_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.zip");
        let names: Vec<String> = (0..30).map(|i| format!("f{i:02}.txt")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        make_zip(&p, &entries);
        let limits = Limits { max_zip_entries: 10, ..Limits::default() };
        let mut sink = TextSink::new(1 << 20, None);
        ZipParser.extract(&p, &ParseContext { limits: &limits }, &mut sink).unwrap();
        let (text, _, truncated) = sink.into_parts();
        assert!(truncated && text.contains("f09.txt") && !text.contains("f10.txt"));
    }
}
