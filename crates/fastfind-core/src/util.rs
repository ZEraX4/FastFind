//! Small shared helpers: hashing, time, path normalisation, thread priority, cancellation.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use xxhash_rust::xxh3::{xxh3_64, Xxh3};

/// Stable 64-bit identity of a path string (used as the catalog/text-store key and as the
/// Tantivy tie-break fast field).
pub fn path_hash(path: &str) -> u64 {
    xxh3_64(path.as_bytes())
}

/// Streaming xxHash3 of a file's content. Reads in 256 KB chunks; never loads the whole file.
pub fn content_hash(path: &Path, cancel: &CancelToken) -> io::Result<u64> {
    let mut f = File::open(path)?;
    let mut h = Xxh3::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.digest())
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn system_time_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}

/// Path as a display/key string. Paths that are not valid Unicode are converted lossily; the
/// original `PathBuf` is still used for all I/O.
pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `\\?\C:\x` → `C:\x` (canonicalize on Windows returns verbatim paths).
pub fn simplify_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return PathBuf::from(rest);
        }
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
    }
    p
}

/// The form an existing absolute folder is indexed under, with symlinks and Windows 8.3 short
/// names resolved exactly as roots are when added. Anything else is returned unchanged.
pub fn resolve_dir(dir: &str) -> String {
    let p = Path::new(dir);
    if !p.is_absolute() {
        return dir.to_string();
    }
    match std::fs::canonicalize(p) {
        Ok(c) => path_str(&simplify_path(c)),
        Err(_) => dir.to_string(),
    }
}

/// Form used for case-insensitive prefix/substring filtering: forward slashes and, on
/// case-insensitive platforms, lowercase.
pub fn filter_form(path: &str) -> String {
    let s = path.replace('\\', "/");
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Lower-case extension without the dot (`"Report.PDF"` → `"pdf"`).
pub fn extension_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Cheap cooperative cancellation flag shared between threads.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Lower the priority of the calling (worker) thread so indexing never makes the machine feel
/// sluggish. `background` additionally lowers I/O priority where the OS supports it.
pub fn lower_thread_priority(background: bool) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            THREAD_PRIORITY_BELOW_NORMAL,
        };
        let prio = if background { THREAD_MODE_BACKGROUND_BEGIN } else { THREAD_PRIORITY_BELOW_NORMAL };
        SetThreadPriority(GetCurrentThread(), prio);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        // On Linux, nice values apply per thread (per TID).
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, if background { 15 } else { 10 });
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = background;
    }
}

/// Recursively sum file sizes below `dir` (used for "Index size").
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(e.path()),
                    Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => {}
                }
            }
        }
    }
    total
}

/// Truncate a `String` to at most `max` bytes on a char boundary.
pub fn truncate_on_char_boundary(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

/// Floor a byte index to the nearest char boundary.
pub fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Byte offset → UTF-16 code-unit offset (the unit JavaScript strings use).
pub fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}
