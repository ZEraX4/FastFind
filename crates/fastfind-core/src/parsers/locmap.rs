//! Location anchors: map a byte offset in extracted text to "Page 3", "Slide 4 — Title",
//! "Sheet Q1!B7", "Line 120", and a compact binary encoding for storage.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LocKind {
    Page(u32),
    /// Approximate page (DOCX rendered page breaks).
    PageApprox(u32),
    Slide(u32, Option<String>),
    Sheet(String),
    /// Spreadsheet row number (1-based) at the start of a line; following lines are
    /// consecutive rows until the next `Row` anchor.
    Row(u32),
    Section(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loc {
    pub offset: u32,
    pub kind: LocKind,
}

/// Column index (0-based) → spreadsheet letters (`0` → `A`, `27` → `AB`).
pub fn column_letters(mut col: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// Human-readable location of byte `offset` in `text`. `line_based` → "Line N" when there are
/// no structural anchors (plain text files).
pub fn describe(locs: &[Loc], text: &str, offset: usize, line_based: bool) -> Option<String> {
    let offset = offset.min(text.len());
    let idx = locs.partition_point(|l| (l.offset as usize) <= offset);
    let before = &locs[..idx];
    if before.is_empty() {
        if line_based {
            let line = text.as_bytes()[..offset].iter().filter(|&&b| b == b'\n').count() + 1;
            return Some(format!("Line {line}"));
        }
        return None;
    }
    // Most recent anchor of each class.
    let mut page = None;
    let mut slide = None;
    let mut sheet: Option<(&str, u32)> = None;
    let mut row: Option<(u32, u32)> = None;
    let mut section = None;
    for l in before.iter().rev() {
        match &l.kind {
            LocKind::Page(p) if page.is_none() => page = Some(format!("Page {p}")),
            LocKind::PageApprox(p) if page.is_none() => page = Some(format!("≈ Page {p}")),
            LocKind::Slide(n, t) if slide.is_none() => {
                slide = Some(match t {
                    Some(t) if !t.trim().is_empty() => format!("Slide {n} — {}", t.trim()),
                    _ => format!("Slide {n}"),
                })
            }
            LocKind::Sheet(name) if sheet.is_none() => sheet = Some((name.as_str(), l.offset)),
            LocKind::Row(r) if row.is_none() && sheet.is_none() => row = Some((*r, l.offset)),
            LocKind::Section(s) if section.is_none() => section = Some(s.clone()),
            _ => {}
        }
        if slide.is_some() || sheet.is_some() {
            break;
        }
    }
    if let Some((name, _)) = sheet {
        if let Some((r, row_off)) = row {
            let seg = &text.as_bytes()[row_off as usize..offset];
            let lines = seg.iter().filter(|&&b| b == b'\n').count() as u32;
            let line_start = seg.iter().rposition(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
            let col = seg[line_start..].iter().filter(|&&b| b == b'\t').count() as u32;
            return Some(format!("{name}!{}{}", column_letters(col), r + lines));
        }
        return Some(format!("Sheet {name}"));
    }
    if let Some(s) = slide {
        return Some(s);
    }
    match (page, section) {
        (Some(p), Some(s)) => Some(format!("{p} · {s}")),
        (Some(p), None) => Some(p),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

/// Compact encoding for the text store: JSON is fine here (anchors are few per document and
/// the blob is zstd-compressed together with the text).
pub fn encode(locs: &[Loc]) -> Vec<u8> {
    serde_json::to_vec(locs).unwrap_or_default()
}

pub fn decode(bytes: &[u8]) -> Vec<Loc> {
    if bytes.is_empty() {
        return Vec::new();
    }
    serde_json::from_slice(bytes).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters() {
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(27), "AB");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(column_letters(702), "AAA");
    }

    #[test]
    fn sheet_cell_location() {
        let text = "Budget\nName\tQ1\tQ2\nRent\t100\t120\n";
        let locs = vec![
            Loc { offset: 0, kind: LocKind::Sheet("Budget".into()) },
            Loc { offset: 7, kind: LocKind::Row(1) },
        ];
        let off = text.find("120").unwrap();
        assert_eq!(describe(&locs, text, off, false).as_deref(), Some("Budget!C2"));
    }

    #[test]
    fn page_and_line() {
        let text = "a\nb\nc";
        assert_eq!(describe(&[], text, 4, true).as_deref(), Some("Line 3"));
        let locs = vec![Loc { offset: 2, kind: LocKind::Page(2) }];
        assert_eq!(describe(&locs, text, 4, false).as_deref(), Some("Page 2"));
        assert_eq!(describe(&locs, text, 0, false), None);
    }
}
