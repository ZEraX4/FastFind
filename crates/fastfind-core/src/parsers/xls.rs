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
fn neutralize_dimensions(stream: &mut [u8]) {
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
    neutralize_dimensions(&mut data);
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
