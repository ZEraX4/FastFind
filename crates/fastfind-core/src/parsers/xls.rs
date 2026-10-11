//! Legacy Excel (`.xls`, BIFF5/8) via calamine. The workbook lives inside an OLE2 container
//! without compression, so memory is bounded by file size (which is capped by settings).
//!
//! calamine's own container reader trusts the file's sector chains: a sector number past the
//! end of the file makes it allocate up to that offset, and a looping chain grows without end
//! (found by fuzzing). So the workbook stream is read with the `cfb` crate, which validates
//! the container, and calamine is given a freshly written container holding just that stream.

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
        let mut wb: Xls<_> = Xls::new(clean_container(path)?).map_err(map_err)?;
        let names = wb.sheet_names().to_vec();
        let meta = DocMeta { pages: Some(names.len() as u32), ..Default::default() };
        for name in names {
            if sink.is_full() {
                break;
            }
            sink.check_deadline()?;
            let range = match wb.worksheet_range(&name) {
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

/// The workbook stream of `path`, re-packed in a well-formed compound file.
fn clean_container(path: &Path) -> ParseResult<Cursor<Vec<u8>>> {
    let mut src = cfb::CompoundFile::open(File::open(path)?).map_err(ParseError::corrupt)?;
    let name = ["/Workbook", "/Book"]
        .into_iter()
        .find(|n| src.is_stream(n))
        .ok_or_else(|| ParseError::Corrupt("no Workbook stream".into()))?;
    let mut data = Vec::new();
    src.open_stream(name)?.read_to_end(&mut data)?;
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
