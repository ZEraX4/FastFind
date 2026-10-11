//! Legacy Excel (`.xls`, BIFF5/8) via calamine. The workbook lives inside an OLE2 container
//! without compression, so memory is bounded by file size (which is capped by settings).
//!
//! calamine's own container reader trusts the file's sector chains: a sector number past the
//! end of the file makes it allocate up to that offset, and a looping chain grows without end
//! (found by fuzzing). So the workbook stream is read with the `cfb` crate, which validates
//! the container, and calamine is given a freshly written container holding just that stream.
//! calamine also panics on some malformed records (unchecked slicing); those panics are caught
//! here and reported as a damaged file. And it reserves memory for every cell a sheet's
//! DIMENSIONS record declares (up to 2^32 rows x 256 columns), so a crafted record asks for
//! terabytes and the failed allocation aborts the process; those records are zeroed first.
//! Finally calamine stores each sheet as a dense grid spanning its outermost cells, so cells at
//! A1 and at the far corner of the 65,536 × 65,536 coordinate space ask for 137 GB; workbooks
//! whose grid would exceed `MAX_SHEET_CELLS` are refused before calamine reads them.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use calamine::{Data, Reader, Xls, XlsError};

use super::ooxml::RowWriter;
use super::sniff::{sniff, Sniffed};
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

pub struct XlsParser;

impl DocumentParser for XlsParser {
    fn name(&self) -> &'static str {
        "xls"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xls", "xlt", "xla"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Cfb
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let container = clean_container(path)?;
        let mut wb: Xls<_> = guarded(|| Xls::new(container))?.map_err(map_err)?;
        let names = wb.sheet_names().to_vec();
        let meta = DocMeta { pages: Some(names.len() as u32), ..Default::default() };
        for name in names {
            if sink.is_full() {
                break;
            }
            sink.check_deadline()?;
            let range = match guarded(|| wb.worksheet_range(&name))? {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(sheet = %name, error = %e, "xls sheet unreadable");
                    continue;
                }
            };
            sink.newline();
            sink.anchor(LocKind::Sheet(name.clone()));
            sink.push_str(&name);
            sink.newline();
            let (r0, c0) = range.start().unwrap_or((0, 0));
            let mut rows = RowWriter::new();
            for (ri, row) in range.rows().enumerate() {
                rows.new_row();
                for (ci, cell) in row.iter().enumerate() {
                    let text = match cell {
                        Data::Empty => continue,
                        Data::String(s) => s.clone(),
                        Data::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
                        Data::Error(e) => format!("{e}"),
                        other => other.to_string(),
                    };
                    rows.cell(sink, r0 + ri as u32 + 1, Some(c0 + ci as u32), &text);
                }
                if sink.is_full() {
                    break;
                }
            }
            rows.finish(sink);
        }
        Ok(meta)
    }
}

/// Zero every DIMENSIONS record (a size hint that only makes calamine pre-allocate), leaving
/// "one cell at A1". calamine reads records from the start of the stream and from each sheet
/// offset (BOUNDSHEET records), and a crafted offset may start mid-record, so every one of
/// those record sequences is walked, again until nothing changes.
/// Returns the offsets the record sequences were read from (the stream start and the sheets).
fn neutralize_dimensions(stream: &mut [u8]) -> Vec<usize> {
    const DIMENSIONS: u16 = 0x0200;
    const BOUNDSHEET: u16 = 0x0085;
    let u16_at = |s: &[u8], i: usize| u16::from_le_bytes([s[i], s[i + 1]]);
    let mut starts = vec![0usize];
    loop {
        let mut changed = false;
        let mut found = Vec::new();
        for &start in &starts {
            let mut i = start;
            while i + 4 <= stream.len() {
                let (typ, len) = (u16_at(stream, i), u16_at(stream, i + 2) as usize);
                let data = i + 4..(i + 4 + len).min(stream.len());
                if typ == DIMENSIONS && stream[data.clone()].iter().any(|&b| b != 0) {
                    stream[data.clone()].fill(0);
                    changed = true;
                } else if typ == BOUNDSHEET && data.len() >= 4 {
                    let s = &stream[data.clone()];
                    found.push(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
                }
                i = data.end;
            }
        }
        found.retain(|p| !starts.contains(p) && *p < stream.len());
        if !changed && found.is_empty() {
            break;
        }
        starts.extend(found);
        starts.sort_unstable();
        starts.dedup();
    }
    starts
}

/// A FILEPASS record in the workbook globals (before their EOF) means the file is encrypted.
fn is_encrypted(stream: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= stream.len() {
        match u16::from_le_bytes([stream[i], stream[i + 1]]) {
            0x002F => return true,
            0x000A => return false,
            _ => i += 4 + u16::from_le_bytes([stream[i + 2], stream[i + 3]]) as usize,
        }
    }
    false
}

/// Largest dense grid calamine builds for one sheet (about 32 bytes per cell): 2^22 cells,
/// for example 65,536 rows × 64 columns.
const MAX_SHEET_CELLS: u64 = 1 << 22;

/// Cells in the bounding box of the cell records calamine reads from `start` to the sheet's
/// EOF record. Every cell record begins with its row and column; MULRK covers a run of columns.
fn sheet_cells(stream: &[u8], start: usize) -> u64 {
    let u16_at = |i: usize| stream.get(i..i + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u64);
    let (mut rows, mut cols) = ((u64::MAX, 0), (u64::MAX, 0));
    let mut i = start;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16_at(i), u16_at(i + 2) as usize);
        let data = i + 4;
        let last_col = match typ {
            0x000A => break, // EOF
            0x0203 | 0x0204 | 0x00D6 | 0x0205 | 0x027E | 0x00FD | 0x0006 if len >= 4 => Some(u16_at(data + 2)),
            0x00BD if len >= 6 => Some(u16_at(data + 2) + ((len as u64 - 6) / 6).saturating_sub(1)),
            _ => None,
        };
        if let Some(last) = last_col {
            let (row, first) = (u16_at(data), u16_at(data + 2));
            rows = (rows.0.min(row), rows.1.max(row));
            cols = (cols.0.min(first), cols.1.max(last));
        }
        i = data + len;
    }
    if rows.0 == u64::MAX {
        return 0;
    }
    (rows.1 - rows.0 + 1) * (cols.1 - cols.0 + 1)
}

/// Run calamine, turning a panic on malformed input into a parse error.
fn guarded<T>(f: impl FnOnce() -> T) -> ParseResult<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|_| ParseError::Corrupt("damaged workbook".into()))
}

/// The workbook stream of `path`, re-packed in a well-formed compound file.
fn clean_container(path: &Path) -> ParseResult<Cursor<Vec<u8>>> {
    let mut src = cfb::CompoundFile::open(File::open(path)?).map_err(ParseError::corrupt)?;
    let name = ["/Workbook", "/Book"]
        .into_iter()
        .find(|n| src.is_stream(n))
        .ok_or_else(|| ParseError::Corrupt("no Workbook stream".into()))?;
    let mut data = Vec::new();
    src.open_stream(name)?.read_to_end(&mut data)?;
    if is_encrypted(&data) {
        // Record contents are encrypted from here on; calamine reports it too, but the cell
        // coordinates below would be noise.
        return Err(ParseError::Encrypted);
    }
    let sheets = neutralize_dimensions(&mut data);
    if let Some(cells) = sheets.iter().map(|&start| sheet_cells(&data, start)).max().filter(|&n| n > MAX_SHEET_CELLS) {
        return Err(ParseError::LimitExceeded(format!("a sheet spans {cells} cells")));
    }
    // Version 3 (512-byte sectors), as Excel writes it; calamine rejects a version 4 file
    // whose root has no mini stream.
    let mut out = cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new()))?;
    out.create_stream(name)?.write_all(&data)?;
    out.flush()?;
    let mut buf = out.into_inner();
    buf.set_position(0);
    Ok(buf)
}

fn map_err(e: XlsError) -> ParseError {
    match e {
        XlsError::Password => ParseError::Encrypted,
        XlsError::Io(io) => ParseError::Io(io),
        other => ParseError::corrupt(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(typ: u16, data: &[u8]) -> Vec<u8> {
        let mut v = typ.to_le_bytes().to_vec();
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn sheet_size_is_the_bounding_box_of_its_cells() {
        let cell = |typ: u16, row: u16, col: u16, extra: &[u8]| {
            let mut d = row.to_le_bytes().to_vec();
            d.extend_from_slice(&col.to_le_bytes());
            d.extend_from_slice(extra);
            rec(typ, &d)
        };
        let mut s = rec(0x0809, &[0; 4]);
        s.extend(cell(0x0203, 2, 1, &[0; 10])); // NUMBER at B3
        s.extend(cell(0x00BD, 9, 0, &[0; 6 * 4 + 2])); // MULRK A10:D10
        s.extend(rec(0x000A, &[]));
        s.extend(cell(0x0203, 60_000, 200, &[0; 10])); // after EOF: another sheet's
        assert_eq!(sheet_cells(&s, 0), 8 * 4, "rows 3-10, columns A-D");
        let mut far = rec(0x0809, &[0; 4]);
        far.extend(cell(0x00FD, 0, 0, &[0; 6]));
        far.extend(cell(0x0203, 65_535, 65_535, &[0; 10]));
        assert!(sheet_cells(&far, 0) > MAX_SHEET_CELLS);
    }

    #[test]
    fn dimensions_are_zeroed_even_behind_a_misaligned_sheet_offset() {
        let huge = [0xFFu8; 14];
        let hidden = rec(0x0200, &huge);
        // The BOUNDSHEET offset points inside the data of the record that follows it, where a
        // DIMENSIONS record is hidden; read from offset 0 it is just opaque data.
        let mut stream = rec(0x0809, &[0; 4]);
        stream.extend(rec(0x0200, &huge));
        let bound_at = stream.len();
        let hidden_at = bound_at + 4 + 6 + 4 + 3;
        let mut bound = (hidden_at as u32).to_le_bytes().to_vec();
        bound.extend_from_slice(&[0, 0]);
        stream.extend(rec(0x0085, &bound));
        let mut opaque = vec![0xAA; 3];
        opaque.extend(&hidden);
        stream.extend(rec(0x00FC, &opaque));
        assert_eq!(&stream[hidden_at..hidden_at + hidden.len()], &hidden[..]);

        neutralize_dimensions(&mut stream);
        assert_eq!(&stream[4 + 4 + 4..4 + 4 + 4 + 14], &[0u8; 14], "the regular record");
        assert_eq!(&stream[hidden_at + 4..hidden_at + 18], &[0u8; 14], "the one only a sheet offset reaches");
        assert_eq!(&stream[hidden_at - 3..hidden_at], &[0xAA; 3], "nothing else changes");
    }
}
