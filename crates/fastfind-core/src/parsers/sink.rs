//! Bounded text accumulator used by all parsers.

use std::time::Instant;

use super::{Loc, LocKind, ParseError, ParseResult};

/// Collects extracted text with a hard byte cap and a deadline.
///
/// * Control characters (except `\t` and `\n`) are replaced with spaces; `\r\n`/`\r` become `\n`.
/// * Runs of blank lines are collapsed to one blank line, which keeps stored text compact.
/// * Once full, all pushes are ignored and [`TextSink::is_full`] turns true, so parsers can
///   stop early instead of decoding the rest of a 1 GB file.
pub struct TextSink {
    buf: String,
    cap: usize,
    truncated: bool,
    locs: Vec<Loc>,
    deadline: Option<Instant>,
    ops: u32,
    timed_out: bool,
    pending_cr: bool,
}

impl TextSink {
    pub fn new(cap: usize, deadline: Option<Instant>) -> Self {
        Self {
            buf: String::with_capacity(cap.min(64 * 1024)),
            cap,
            truncated: false,
            locs: Vec::new(),
            deadline,
            ops: 0,
            timed_out: false,
            pending_cr: false,
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.truncated || self.timed_out
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.trim().is_empty()
    }

    pub fn text(&self) -> &str {
        &self.buf
    }

    /// Check the deadline (cheap: only every 256 calls hits the clock). Parsers call this in
    /// loops that may not push text (e.g. skipping a huge binary blob).
    pub fn check_deadline(&mut self) -> ParseResult<()> {
        self.ops = self.ops.wrapping_add(1);
        if self.ops.is_multiple_of(256) {
            if let Some(d) = self.deadline {
                if Instant::now() > d {
                    self.timed_out = true;
                }
            }
        }
        if self.timed_out {
            Err(ParseError::Timeout)
        } else {
            Ok(())
        }
    }

    fn room(&mut self, want: usize) -> usize {
        let room = self.cap.saturating_sub(self.buf.len());
        if want > room {
            self.truncated = true;
        }
        room.min(want)
    }

    pub fn push_str(&mut self, s: &str) {
        if self.is_full() || s.is_empty() {
            return;
        }
        let _ = self.check_deadline();
        let clean = s.bytes().all(|b| b >= 0x20 || b == b'\t' || b == b'\n') && !self.pending_cr;
        if clean && !s.contains("\n\n\n") {
            let n = self.room(s.len());
            let n = crate::util::floor_char_boundary(s, n);
            self.buf.push_str(&s[..n]);
            return;
        }
        for c in s.chars() {
            self.push_char(c);
            if self.is_full() {
                break;
            }
        }
    }

    pub fn push_char(&mut self, c: char) {
        if self.is_full() {
            return;
        }
        let c = match c {
            '\r' => {
                self.pending_cr = true;
                '\n'
            }
            '\n' if self.pending_cr => {
                self.pending_cr = false;
                return;
            }
            '\t' | '\n' => {
                self.pending_cr = false;
                c
            }
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{feff}' => {
                self.pending_cr = false;
                ' '
            }
            c => {
                self.pending_cr = false;
                c
            }
        };
        if c == '\n' && self.buf.ends_with("\n\n") {
            return;
        }
        if self.room(c.len_utf8()) < c.len_utf8() {
            return;
        }
        self.buf.push(c);
    }

    /// Line break unless the text already ends with one.
    pub fn newline(&mut self) {
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.push_char('\n');
        }
    }

    /// Word separator unless already separated.
    pub fn space(&mut self) {
        if !self.buf.is_empty() && !self.buf.ends_with(|c: char| c.is_whitespace()) {
            self.push_char(' ');
        }
    }

    pub fn tab(&mut self) {
        self.push_char('\t');
    }

    /// Record a location anchor at the current offset.
    pub fn anchor(&mut self, kind: LocKind) {
        if self.is_full() {
            return;
        }
        let offset = self.buf.len() as u32;
        // A later anchor at the same offset replaces a structural one of the same class.
        if let Some(last) = self.locs.last_mut() {
            if last.offset == offset && std::mem::discriminant(&last.kind) == std::mem::discriminant(&kind) {
                last.kind = kind;
                return;
            }
        }
        self.locs.push(Loc { offset, kind });
    }

    /// Re-create an anchor produced elsewhere (out-of-process extraction) at its offset.
    pub fn anchor_at(&mut self, loc: Loc) {
        if (loc.offset as usize) <= self.buf.len() {
            self.locs.push(loc);
        }
    }

    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Mutable access to the last anchor of the given kind (e.g. to attach a slide title that
    /// is only known after the slide's text has been read).
    pub fn last_anchor_mut(&mut self) -> Option<&mut Loc> {
        self.locs.last_mut()
    }

    pub fn truncated(&self) -> bool {
        self.truncated || self.timed_out
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out
    }

    pub fn into_parts(self) -> (String, Vec<Loc>, bool) {
        let truncated = self.truncated || self.timed_out;
        (self.buf, self.locs, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_on_char_boundary() {
        let mut s = TextSink::new(5, None);
        s.push_str("héllo world");
        assert!(s.is_full());
        assert_eq!(s.text(), "héll");
    }

    #[test]
    fn normalises_newlines_and_controls() {
        let mut s = TextSink::new(100, None);
        s.push_str("a\r\nb\rc\u{0}d\n\n\n\ne");
        assert_eq!(s.text(), "a\nb\nc d\n\ne");
    }

    #[test]
    fn anchors_record_offsets() {
        let mut s = TextSink::new(100, None);
        s.anchor(LocKind::Page(1));
        s.push_str("one");
        s.newline();
        s.anchor(LocKind::Page(2));
        s.push_str("two");
        let (_, locs, _) = s.into_parts();
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[1].offset, 4);
    }
}
