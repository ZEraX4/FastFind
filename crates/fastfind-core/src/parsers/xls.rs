//! Legacy Excel (`.xls`, BIFF5/8) via calamine. The workbook lives inside an OLE2 container
//! without compression, so memory is bounded by file size (which is capped by settings).

use std::fs::File;
use std::io::BufReader;
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
        let mut wb: Xls<_> = Xls::new(BufReader::new(File::open(path)?)).map_err(map_err)?;
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

fn map_err(e: XlsError) -> ParseError {
    match e {
        XlsError::Password => ParseError::Encrypted,
        XlsError::Io(io) => ParseError::Io(io),
        other => ParseError::corrupt(other),
    }
}
