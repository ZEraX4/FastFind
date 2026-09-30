//! Legacy Word 97–2003 (`.doc`) text extraction, following [MS-DOC].
//!
//! The document text is described by the *piece table* (CLX → PlcPcd) in the table stream:
//! each piece maps a range of character positions (CPs) to either 8-bit "compressed" text
//! (Windows-1252) or UTF-16LE in the `WordDocument` stream. Walking the pieces in CP order
//! reproduces the logical text, including fast-saved (complex) documents. The CP space is
//! partitioned into main text, footnotes, headers/footers, comments, endnotes and text boxes,
//! which become `Section` anchors.
//!
//! Field codes (between 0x13 and 0x14) are hidden; field results (0x14..0x15) are kept.
//! Encrypted/obfuscated documents are reported as encrypted. Word 6/95 files (nFib < 193) are
//! reported as unsupported rather than guessed at.
//!
//! [MS-DOC]: https://learn.microsoft.com/openspecs/office_file_formats/ms-doc/

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use encoding_rs::{Encoding, WINDOWS_1252};

use super::sniff::{sniff, Sniffed};
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct DocParser;

impl DocumentParser for DocParser {
    fn name(&self) -> &'static str {
        "doc"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["doc", "dot"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Cfb
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut comp = cfb::CompoundFile::open(File::open(path)?).map_err(ParseError::corrupt)?;
        if comp.exists("/EncryptionInfo") {
            return Err(ParseError::Encrypted);
        }
        let meta = ole_summary(&mut comp);
        let wd = read_stream(&mut comp, "/WordDocument")?
            .ok_or_else(|| ParseError::Corrupt("no WordDocument stream".into()))?;
        let fib = Fib::parse(&wd)?;
        let table = read_stream(&mut comp, if fib.which_table_1 { "/1Table" } else { "/0Table" })?
            .ok_or_else(|| ParseError::Corrupt("missing table stream".into()))?;
        let pieces = parse_piece_table(&table, fib.fc_clx, fib.lcb_clx)?;
        emit_text(&wd, &pieces, &fib, sink)?;
        Ok(meta)
    }
}

pub(crate) fn read_stream<F: Read + Seek>(comp: &mut cfb::CompoundFile<F>, name: &str) -> ParseResult<Option<Vec<u8>>> {
    if !comp.exists(name) {
        return Ok(None);
    }
    let mut s = comp.open_stream(name)?;
    let mut v = Vec::new();
    s.read_to_end(&mut v)?;
    Ok(Some(v))
}

#[inline]
fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

#[inline]
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

struct Fib {
    which_table_1: bool,
    /// Character counts of the CP sub-documents, in CP order.
    ccp: [u32; 7],
    fc_clx: u32,
    lcb_clx: u32,
}

const SUBDOC_LABELS: [&str; 7] = ["", "Footnotes", "Headers & footers", "Comments", "Endnotes", "Text boxes", "Header text boxes"];

impl Fib {
    fn parse(wd: &[u8]) -> ParseResult<Fib> {
        let bad = || ParseError::Corrupt("truncated FIB".into());
        let ident = u16_at(wd, 0).ok_or_else(bad)?;
        if ident != 0xA5EC {
            return Err(ParseError::Unsupported("not a Word 97-2003 document".into()));
        }
        let nfib = u16_at(wd, 2).ok_or_else(bad)?;
        if nfib < 0x00C1 {
            return Err(ParseError::Unsupported(format!("Word 6/95 format (nFib {nfib:#x})")));
        }
        let flags = u16_at(wd, 0x0A).ok_or_else(bad)?;
        if flags & 0x0100 != 0 || flags & 0x8000 != 0 {
            return Err(ParseError::Encrypted);
        }
        // FibBase (32 bytes) | csw | fibRgW | cslw | fibRgLw | cbRgFcLcb | fibRgFcLcb
        let csw = u16_at(wd, 32).ok_or_else(bad)? as usize;
        let cslw_off = 34 + csw * 2;
        let cslw = u16_at(wd, cslw_off).ok_or_else(bad)? as usize;
        let rglw = cslw_off + 2;
        let lw = |i: usize| u32_at(wd, rglw + i * 4).unwrap_or(0);
        // FibRgLw97: 3 ccpText, 4 ccpFtn, 5 ccpHdd, 6 ccpMcr (unused), 7 ccpAtn, 8 ccpEdn,
        // 9 ccpTxbx, 10 ccpHdrTxbx.
        let ccp = [lw(3), lw(4), lw(5), lw(7), lw(8), lw(9), lw(10)];
        let fclcb = rglw + cslw * 4 + 2;
        let fc_clx = u32_at(wd, fclcb + 33 * 8).ok_or_else(bad)?;
        let lcb_clx = u32_at(wd, fclcb + 33 * 8 + 4).ok_or_else(bad)?;
        Ok(Fib { which_table_1: flags & 0x0200 != 0, ccp, fc_clx, lcb_clx })
    }
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    cp_start: u32,
    cp_end: u32,
    fc: u32,
    compressed: bool,
}

fn parse_piece_table(table: &[u8], fc_clx: u32, lcb_clx: u32) -> ParseResult<Vec<Piece>> {
    let start = fc_clx as usize;
    let end = start.checked_add(lcb_clx as usize).filter(|&e| e <= table.len())
        .ok_or_else(|| ParseError::Corrupt("CLX outside table stream".into()))?;
    let mut pos = start;
    // Skip Prc entries (property modifiers), then read the Pcdt.
    while pos < end {
        match table[pos] {
            0x01 => {
                let cb = u16_at(table, pos + 1).ok_or_else(|| ParseError::Corrupt("bad Prc".into()))? as i16;
                pos += 3 + cb.max(0) as usize;
            }
            0x02 => {
                let lcb = u32_at(table, pos + 1).ok_or_else(|| ParseError::Corrupt("bad Pcdt".into()))? as usize;
                let plc = pos + 5;
                let plc_end = plc.checked_add(lcb).filter(|&e| e <= table.len())
                    .ok_or_else(|| ParseError::Corrupt("PlcPcd outside table".into()))?;
                if lcb < 4 {
                    return Ok(Vec::new());
                }
                let n = (lcb - 4) / 12;
                let mut pieces = Vec::with_capacity(n);
                for i in 0..n {
                    let cp_start = u32_at(table, plc + i * 4).unwrap_or(0);
                    let cp_end = u32_at(table, plc + (i + 1) * 4).unwrap_or(0);
                    let pcd = plc + (n + 1) * 4 + i * 8;
                    if pcd + 8 > plc_end {
                        break;
                    }
                    let raw = u32_at(table, pcd + 2).unwrap_or(0);
                    let compressed = raw & 0x4000_0000 != 0;
                    let fc = raw & 0x3FFF_FFFF;
                    if cp_end > cp_start {
                        pieces.push(Piece { cp_start, cp_end, fc: if compressed { fc / 2 } else { fc }, compressed });
                    }
                }
                pieces.sort_by_key(|p| p.cp_start);
                return Ok(pieces);
            }
            _ => return Err(ParseError::Corrupt("unexpected CLX entry".into())),
        }
    }
    Err(ParseError::Corrupt("no piece table".into()))
}

fn emit_text(wd: &[u8], pieces: &[Piece], fib: &Fib, sink: &mut TextSink) -> ParseResult<()> {
    // Sub-document boundaries in CP space.
    let mut bounds = [0u32; 8];
    for i in 0..7 {
        bounds[i + 1] = bounds[i].saturating_add(fib.ccp[i]);
    }
    let total = bounds[7];
    let mut next_bound = 1usize;
    // Field state: one entry per open field; `true` while inside the field code.
    let mut fields: Vec<bool> = Vec::new();
    let mut cp = 0u32;
    for p in pieces {
        if cp >= total && total > 0 {
            break;
        }
        cp = p.cp_start;
        let count = (p.cp_end - p.cp_start) as usize;
        let mut units: Vec<u16> = Vec::with_capacity(count.min(1 << 20));
        let start = p.fc as usize;
        if p.compressed {
            let end = start.saturating_add(count).min(wd.len());
            if start < end {
                // Map compressed bytes through Windows-1252 (per MS-DOC 2.4.1).
                let (s, _) = WINDOWS_1252.decode_without_bom_handling(&wd[start..end]);
                units.extend(s.encode_utf16());
            }
        } else {
            let end = start.saturating_add(count * 2).min(wd.len());
            if start < end {
                units.extend(wd[start..end].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])));
            }
        }
        for ch in char::decode_utf16(units).map(|r| r.unwrap_or('\u{fffd}')) {
            if total > 0 && cp >= total {
                break;
            }
            while next_bound < 7 && cp >= bounds[next_bound] {
                if fib.ccp[next_bound] > 0 {
                    sink.newline();
                    sink.anchor(LocKind::Section(SUBDOC_LABELS[next_bound].into()));
                }
                next_bound += 1;
                fields.clear();
            }
            cp += 1;
            match ch as u32 {
                0x13 => {
                    fields.push(true);
                    continue;
                }
                0x14 => {
                    if let Some(f) = fields.last_mut() {
                        *f = false;
                    }
                    continue;
                }
                0x15 => {
                    fields.pop();
                    continue;
                }
                _ => {}
            }
            if fields.iter().any(|&in_code| in_code) {
                continue;
            }
            match ch as u32 {
                0x0B..=0x0E => sink.push_char('\n'),
                0x07 => sink.push_char('\t'),
                0x1E => sink.push_char('-'),
                0x1F => {}
                0xA0 => sink.push_char(' '),
                c if c < 0x20 => {}
                _ => sink.push_char(ch),
            }
            if sink.is_full() {
                return Ok(());
            }
        }
        sink.check_deadline()?;
    }
    Ok(())
}

/// Title/author/subject/keywords from the OLE `\u{5}SummaryInformation` property set
/// (shared by .doc, .xls and .ppt). Failures yield empty metadata, never an error.
pub(crate) fn ole_summary<F: Read + Seek>(comp: &mut cfb::CompoundFile<F>) -> DocMeta {
    let mut meta = DocMeta::default();
    let Ok(Some(b)) = read_stream(comp, "/\u{5}SummaryInformation") else { return meta };
    let Some(set_off) = u32_at(&b, 44).map(|v| v as usize) else { return meta };
    let Some(nprops) = u32_at(&b, set_off + 4) else { return meta };
    let mut codepage: &'static Encoding = WINDOWS_1252;
    let mut entries = Vec::new();
    for i in 0..nprops.min(256) as usize {
        let (Some(id), Some(off)) = (u32_at(&b, set_off + 8 + i * 8), u32_at(&b, set_off + 12 + i * 8)) else { break };
        entries.push((id, set_off + off as usize));
    }
    // Property 1 = code page (VT_I2).
    if let Some(&(_, off)) = entries.iter().find(|(id, _)| *id == 1) {
        if let Some(cp) = u16_at(&b, off + 4) {
            let label = match cp {
                65001 => "utf-8".to_string(),
                1200 => "utf-16le".into(),
                c => format!("windows-{c}"),
            };
            if let Some(e) = Encoding::for_label(label.as_bytes()) {
                codepage = e;
            }
        }
    }
    for (id, off) in entries {
        let Some(vt) = u32_at(&b, off) else { continue };
        let Some(len) = u32_at(&b, off + 4).map(|v| v as usize) else { continue };
        let value = match vt {
            30 => b.get(off + 8..off + 8 + len.min(4096)).map(|s| {
                let s = s.split(|&c| c == 0).next().unwrap_or(s);
                codepage.decode_without_bom_handling(s).0.into_owned()
            }),
            31 => b.get(off + 8..off + 8 + (len * 2).min(8192)).map(|s| {
                let u: Vec<u16> = s.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                String::from_utf16_lossy(&u)
            }),
            _ => None,
        };
        let Some(v) = value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) else { continue };
        match id {
            2 => meta.title = Some(v),
            3 => meta.subject = Some(v),
            4 => meta.author = Some(v),
            5 => meta.keywords = Some(v),
            _ => {}
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal Word 97 FIB + piece table in memory and check the extraction logic,
    /// including field-code hiding and sub-document anchors. (Real Word files are exercised
    /// by the fixture-based integration tests.)
    #[test]
    fn piece_table_and_fields() {
        let main = "Hello \u{13}HYPERLINK x\u{14}link\u{15} world\r";
        let foot = "Note\r";
        let text: Vec<u16> = format!("{main}{foot}").encode_utf16().collect();
        let mut wd = vec![0u8; 1024];
        wd[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        wd[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        // csw = 14, cslw = 22, cbRgFcLcb = 93
        wd[32..34].copy_from_slice(&14u16.to_le_bytes());
        let cslw_off = 34 + 28;
        wd[cslw_off..cslw_off + 2].copy_from_slice(&22u16.to_le_bytes());
        let rglw = cslw_off + 2;
        let main_len = main.encode_utf16().count() as u32;
        let foot_len = foot.encode_utf16().count() as u32;
        wd[rglw + 12..rglw + 16].copy_from_slice(&main_len.to_le_bytes());
        wd[rglw + 16..rglw + 20].copy_from_slice(&foot_len.to_le_bytes());
        let fclcb = rglw + 22 * 4 + 2;
        // Text at offset 512, uncompressed.
        let fc = 512u32;
        for (i, u) in text.iter().enumerate() {
            wd[512 + i * 2..514 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        // Table stream: CLX with one piece.
        let mut table = vec![0u8; 16];
        let clx_off = table.len() as u32;
        table.push(0x02);
        let lcb = 4 * 2 + 8;
        table.extend_from_slice(&(lcb as u32).to_le_bytes());
        table.extend_from_slice(&0u32.to_le_bytes());
        table.extend_from_slice(&(text.len() as u32).to_le_bytes());
        table.extend_from_slice(&[0, 0]);
        table.extend_from_slice(&fc.to_le_bytes());
        table.extend_from_slice(&[0, 0]);
        let lcb_clx = table.len() as u32 - clx_off;
        wd[fclcb + 33 * 8..fclcb + 33 * 8 + 4].copy_from_slice(&clx_off.to_le_bytes());
        wd[fclcb + 33 * 8 + 4..fclcb + 33 * 8 + 8].copy_from_slice(&lcb_clx.to_le_bytes());

        let fib = Fib::parse(&wd).unwrap();
        let pieces = parse_piece_table(&table, fib.fc_clx, fib.lcb_clx).unwrap();
        let mut sink = TextSink::new(1 << 16, None);
        emit_text(&wd, &pieces, &fib, &mut sink).unwrap();
        let (t, locs, _) = sink.into_parts();
        assert!(t.starts_with("Hello link world\n"), "{t:?}");
        assert!(!t.contains("HYPERLINK"));
        assert!(t.contains("Note"));
        assert!(locs.iter().any(|l| l.kind == LocKind::Section("Footnotes".into())));
    }

    #[test]
    fn rejects_encrypted_and_old_formats() {
        let mut wd = vec![0u8; 1024];
        wd[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        wd[2..4].copy_from_slice(&0x0065u16.to_le_bytes());
        assert!(matches!(Fib::parse(&wd), Err(ParseError::Unsupported(_))));
        wd[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        wd[0x0A..0x0C].copy_from_slice(&0x0100u16.to_le_bytes());
        assert!(matches!(Fib::parse(&wd), Err(ParseError::Encrypted)));
    }
}
