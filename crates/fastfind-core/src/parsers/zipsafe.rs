//! ZIP container access with decompression-bomb protection.
//!
//! Entry names are only used to look up parts inside the archive; nothing is ever extracted to
//! disk, so path traversal (`../../evil`) in entry names is harmless by construction.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use super::{Limits, ParseError, ParseResult};

pub struct SafeZip {
    archive: ZipArchive<BufReader<File>>,
    budget: u64,
    max_ratio: u64,
}

impl SafeZip {
    pub fn open(path: &Path, limits: &Limits) -> ParseResult<Self> {
        let f = BufReader::new(File::open(path)?);
        let archive = ZipArchive::new(f)?;
        if archive.len() > limits.max_zip_entries {
            return Err(ParseError::LimitExceeded(format!("{} zip entries", archive.len())));
        }
        Ok(Self { archive, budget: limits.max_zip_bytes, max_ratio: limits.max_zip_ratio })
    }

    pub fn names(&self) -> Vec<String> {
        self.archive.file_names().map(|s| s.to_string()).collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.archive.index_for_name(name).is_some()
    }

    /// Open an entry as a bounded reader. Fails fast when declared sizes look like a bomb and
    /// enforces the budget on bytes actually produced (declared sizes can lie).
    pub fn open_entry(&mut self, name: &str) -> ParseResult<Option<BoundedRead<'_>>> {
        let Some(idx) = self.archive.index_for_name(name) else { return Ok(None) };
        let entry = self.archive.by_index(idx)?;
        if entry.encrypted() {
            return Err(ParseError::Encrypted);
        }
        let size = entry.size();
        let csize = entry.compressed_size().max(1);
        if size > (1 << 20) && size / csize > self.max_ratio {
            return Err(ParseError::LimitExceeded(format!("compression ratio {} in {name}", size / csize)));
        }
        if size > self.budget {
            return Err(ParseError::LimitExceeded(format!("{name} expands to {size} bytes")));
        }
        Ok(Some(BoundedRead { inner: entry, remaining: &mut self.budget }))
    }

    /// Read a (small) entry fully, bounded by `max` bytes.
    pub fn read_to_string(&mut self, name: &str, max: u64) -> ParseResult<Option<String>> {
        let Some(r) = self.open_entry(name)? else { return Ok(None) };
        let mut s = String::new();
        r.take(max).read_to_string(&mut s).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData { ParseError::corrupt(e) } else { ParseError::Io(e) }
        })?;
        Ok(Some(s))
    }
}

pub struct BoundedRead<'a> {
    inner: zip::read::ZipFile<'a, BufReader<File>>,
    remaining: &'a mut u64,
}

impl Read for BoundedRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > *self.remaining {
            return Err(io::Error::other("zip decompression budget exceeded"));
        }
        *self.remaining -= n as u64;
        Ok(n)
    }
}

/// Resolve a relationship target relative to the part that references it
/// (`word/_rels/document.xml.rels` + `media/x.png` → `word/media/x.png`, `../a.xml` handled).
pub fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relationship_targets() {
        assert_eq!(resolve_target("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_target("ppt/slides", "../notesSlides/n1.xml"), "ppt/notesSlides/n1.xml");
        assert_eq!(resolve_target("xl", "/xl/worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve_target("a", "../../../../etc/passwd"), "etc/passwd");
    }
}
