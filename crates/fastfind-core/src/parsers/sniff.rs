//! Magic-byte format detection. Used to route mislabelled files and to avoid opening binaries
//! that are clearly not text.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub const HEADER_LEN: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sniffed {
    Pdf,
    /// OLE2 / Compound File Binary (legacy Office, encrypted OOXML, MSG).
    Cfb,
    Zip,
    Rtf,
    Html,
    Xml,
    /// UTF-16 text (BOM or NUL pattern).
    Utf16,
    /// Plausibly text (UTF-8 or single-byte encoding).
    Text,
    /// Known binary signature or NUL bytes.
    Binary,
    Empty,
}

pub fn read_header(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; HEADER_LEN];
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    buf.truncate(n);
    Ok(buf)
}

const BINARY_MAGIC: &[&[u8]] = &[
    b"\x7fELF", b"MZ", b"\x89PNG", b"\xff\xd8\xff", b"GIF8", b"BM", b"II*\0", b"MM\0*",
    b"\x1f\x8b", b"7z\xbc\xaf", b"Rar!", b"\xfd7zXZ", b"BZh", b"\x00\x00\x01\x00", b"OggS",
    b"fLaC", b"ID3", b"RIFF", b"\x1aE\xdf\xa3", b"\xca\xfe\xba\xbe", b"\xcf\xfa\xed\xfe",
    b"\xce\xfa\xed\xfe", b"SQLite format 3", b"wOFF", b"wOF2", b"\0asm",
];

pub fn sniff(h: &[u8]) -> Sniffed {
    if h.is_empty() {
        return Sniffed::Empty;
    }
    if h.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return Sniffed::Cfb;
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") {
        return Sniffed::Zip;
    }
    if h[..h.len().min(1024)].windows(5).any(|w| w == b"%PDF-") {
        return Sniffed::Pdf;
    }
    if h.starts_with(b"\xFF\xFE") || h.starts_with(b"\xFE\xFF") {
        return Sniffed::Utf16;
    }
    for m in BINARY_MAGIC {
        if h.starts_with(m) {
            return Sniffed::Binary;
        }
    }
    let body = h.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(h);
    let trimmed: &[u8] = {
        let start = body.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(body.len());
        &body[start..]
    };
    if trimmed.starts_with(b"{\\rtf") {
        return Sniffed::Rtf;
    }
    if looks_utf16(body) {
        return Sniffed::Utf16;
    }
    if body.contains(&0) {
        return Sniffed::Binary;
    }
    let lower: Vec<u8> = trimmed.iter().take(256).map(|b| b.to_ascii_lowercase()).collect();
    if lower.starts_with(b"<!doctype html") || lower.starts_with(b"<html") || lower.starts_with(b"<head") {
        return Sniffed::Html;
    }
    if lower.starts_with(b"<?xml") {
        // Word 2003 XML / Excel XML spreadsheets are still XML; HTML with XML prolog is rare.
        if lower.windows(5).any(|w| w == b"<html") {
            return Sniffed::Html;
        }
        return Sniffed::Xml;
    }
    // Ratio of control bytes (other than whitespace) decides text vs binary.
    let ctrl = body.iter().filter(|&&b| b < 0x09 || (b > 0x0D && b < 0x20)).count();
    if ctrl * 100 > body.len().max(1) * 2 {
        return Sniffed::Binary;
    }
    Sniffed::Text
}

/// UTF-16 without BOM: most ASCII text has NUL in every other byte.
fn looks_utf16(h: &[u8]) -> bool {
    if h.len() < 16 {
        return false;
    }
    let n = h.len().min(4096) & !1;
    let (mut even0, mut odd0) = (0, 0);
    for i in (0..n).step_by(2) {
        if h[i] == 0 {
            even0 += 1;
        }
        if h[i + 1] == 0 {
            odd0 += 1;
        }
    }
    let pairs = n / 2;
    (odd0 * 10 > pairs * 7 && even0 * 10 < pairs) || (even0 * 10 > pairs * 7 && odd0 * 10 < pairs)
}

/// Streams inside a compound file decide which legacy Office format it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfbKind {
    Word,
    Excel,
    PowerPoint,
    EncryptedOoxml,
    Unknown,
}

pub fn cfb_kind(path: &Path) -> CfbKind {
    let Ok(f) = File::open(path) else { return CfbKind::Unknown };
    let Ok(c) = cfb::CompoundFile::open(f) else { return CfbKind::Unknown };
    if c.exists("/EncryptionInfo") && c.exists("/EncryptedPackage") {
        CfbKind::EncryptedOoxml
    } else if c.exists("/WordDocument") {
        CfbKind::Word
    } else if c.exists("/Workbook") || c.exists("/Book") {
        CfbKind::Excel
    } else if c.exists("/PowerPoint Document") {
        CfbKind::PowerPoint
    } else {
        CfbKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_signatures() {
        assert_eq!(sniff(b"%PDF-1.7\n"), Sniffed::Pdf);
        assert_eq!(sniff(b"PK\x03\x04rest"), Sniffed::Zip);
        assert_eq!(sniff(b"{\\rtf1\\ansi"), Sniffed::Rtf);
        assert_eq!(sniff(b"  <!DOCTYPE html><html>"), Sniffed::Html);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><a/>"), Sniffed::Xml);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Sniffed::Binary);
        assert_eq!(sniff(b"hello world\n"), Sniffed::Text);
        assert_eq!(sniff(b"abc\0def"), Sniffed::Binary);
        assert_eq!(sniff(b""), Sniffed::Empty);
        let u16: Vec<u8> = "hello world, this is utf16".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(sniff(&u16), Sniffed::Utf16);
    }
}
