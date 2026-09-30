//! Out-of-process PDF extraction.
//!
//! PDFium is not thread-safe, so in-process use serialises every PDF through one lock. A small
//! pool of helper processes (the application's own executable started with
//! [`PDF_WORKER_ARG`]) gives real parallelism and **crash isolation**: PDFium is C++ and parses
//! untrusted input, so a malformed PDF that crashes or hangs it only takes down a helper, which
//! is killed/respawned while the file is recorded as failed.
//!
//! Protocol: one JSON request line on stdin, one JSON response line on stdout.
//! Idle helpers exit after [`IDLE_EXIT`] to give memory back.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

use super::{DocMeta, Limits, Loc, ParseContext, ParseError, ParseResult, TextSink};

/// Command-line flag that turns the executable into a PDF helper.
pub const PDF_WORKER_ARG: &str = "--fastfind-pdf-worker";
const IDLE_EXIT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
struct Request {
    path: PathBuf,
    max_text_bytes: usize,
    max_pages: u32,
    timeout_ms: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Response {
    ok: bool,
    /// "encrypted" | "corrupt" | "unsupported" | "limit" | "timeout" | "io"
    error: Option<String>,
    message: Option<String>,
    text: String,
    locs: Vec<Loc>,
    truncated: bool,
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    keywords: Option<String>,
    pages: Option<u32>,
    flags: u64,
}

/// Entry point of a helper process: serve requests until stdin closes.
pub fn run_worker(pdfium_dirs: &[PathBuf]) -> ! {
    crate::util::lower_thread_priority(false);
    super::pdf::init(pdfium_dirs);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => serve(&req),
            Err(e) => Response { error: Some("corrupt".into()), message: Some(e.to_string()), ..Default::default() },
        };
        if serde_json::to_writer(&mut out, &resp).is_err() || out.write_all(b"\n").is_err() || out.flush().is_err() {
            break;
        }
    }
    std::process::exit(0)
}

fn serve(req: &Request) -> Response {
    // Test hook: simulate PDFium crashing on a specific file to exercise crash isolation.
    if let Ok(pat) = std::env::var("FASTFIND_TEST_PDF_CRASH_ON") {
        if !pat.is_empty() && req.path.to_string_lossy().contains(&pat) {
            std::process::abort();
        }
    }
    let limits = Limits {
        max_text_bytes: req.max_text_bytes,
        max_pdf_pages: req.max_pages,
        deadline: Some(Instant::now() + Duration::from_millis(req.timeout_ms)),
        ..Limits::default()
    };
    let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
    let res = super::pdf::extract_in_process(&req.path, &ParseContext { limits: &limits }, &mut sink);
    let (text, locs, truncated) = sink.into_parts();
    match res {
        Ok(m) => Response {
            ok: true,
            text,
            locs,
            truncated,
            title: m.title,
            author: m.author,
            subject: m.subject,
            keywords: m.keywords,
            pages: m.pages,
            flags: m.flags,
            ..Default::default()
        },
        Err(e) => {
            let kind = match &e {
                ParseError::Encrypted => "encrypted",
                ParseError::Corrupt(_) => "corrupt",
                ParseError::Unsupported(_) => "unsupported",
                ParseError::LimitExceeded(_) => "limit",
                ParseError::Timeout => "timeout",
                ParseError::Io(_) => "io",
            };
            // A timeout still returns the partial text.
            Response { error: Some(kind.into()), message: Some(e.to_string()), text, locs, truncated: true, ..Default::default() }
        }
    }
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    last_used: Instant,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Busy {
    started: Instant,
    limit: Duration,
    pid: u32,
}

/// Idle helpers and the number of live helpers, guarded by one mutex so waiters can never
/// miss a wake-up (a helper returned between "no idle helper" and "wait" would otherwise
/// leave the waiter sleeping until its timeout).
struct PoolState {
    idle: Vec<Helper>,
    running: usize,
}

pub struct PdfWorkerPool {
    exe: PathBuf,
    pdfium_dirs: Vec<PathBuf>,
    max: usize,
    state: Mutex<PoolState>,
    cv: Condvar,
    busy: Mutex<Vec<Busy>>,
    stop: AtomicBool,
}

static POOL: OnceLock<Arc<PdfWorkerPool>> = OnceLock::new();

/// Configure helper processes (called once at startup by the app/CLI). Without this, PDFs are
/// parsed in-process.
pub fn configure(exe: PathBuf, pdfium_dirs: Vec<PathBuf>, max: usize) -> Option<Arc<PdfWorkerPool>> {
    if !exe.is_file() {
        return None;
    }
    let pool = POOL.get_or_init(|| {
        let p = Arc::new(PdfWorkerPool {
            exe,
            pdfium_dirs,
            max: max.clamp(1, 16),
            state: Mutex::new(PoolState { idle: Vec::new(), running: 0 }),
            cv: Condvar::new(),
            busy: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let w = Arc::downgrade(&p);
        std::thread::Builder::new()
            .name("ff-pdf-watchdog".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(p) = w.upgrade() else { return };
                if p.stop.load(Ordering::Relaxed) {
                    return;
                }
                p.watchdog();
            })
            .ok();
        p
    });
    Some(pool.clone())
}

pub fn pool() -> Option<&'static Arc<PdfWorkerPool>> {
    POOL.get()
}

impl PdfWorkerPool {
    pub fn max_workers(&self) -> usize {
        self.max
    }

    fn spawn(&self) -> std::io::Result<Helper> {
        let mut cmd = Command::new(&self.exe);
        cmd.arg(PDF_WORKER_ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(d) = self.pdfium_dirs.first() {
            cmd.env("FASTFIND_PDFIUM_DIR", d);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stdout = BufReader::with_capacity(1 << 20, child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?);
        Ok(Helper { child, stdin, stdout, last_used: Instant::now() })
    }

    fn acquire(&self) -> std::io::Result<Helper> {
        let mut st = self.state.lock();
        loop {
            if let Some(h) = st.idle.pop() {
                return Ok(h);
            }
            if st.running < self.max {
                st.running += 1;
                drop(st);
                return self.spawn().inspect_err(|_| {
                    self.state.lock().running -= 1;
                    self.cv.notify_one();
                });
            }
            self.cv.wait(&mut st);
        }
    }

    fn release(&self, h: Option<Helper>) {
        let mut st = self.state.lock();
        match h {
            Some(mut h) => {
                h.last_used = Instant::now();
                st.idle.push(h);
            }
            None => st.running -= 1,
        }
        drop(st);
        self.cv.notify_one();
    }

    /// Kill helpers stuck past their deadline and helpers idle for too long.
    fn watchdog(&self) {
        let now = Instant::now();
        for b in self.busy.lock().iter() {
            if now.duration_since(b.started) > b.limit + Duration::from_secs(10) {
                tracing::warn!(pid = b.pid, "PDF helper exceeded its time limit; killing it");
                kill_pid(b.pid);
            }
        }
        let mut st = self.state.lock();
        let before = st.idle.len();
        st.idle.retain(|h| now.duration_since(h.last_used) < IDLE_EXIT);
        let reaped = before - st.idle.len();
        st.running -= reaped;
        drop(st);
        if reaped > 0 {
            self.cv.notify_all();
        }
    }

    pub fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let timeout = ctx.limits.deadline.map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(Duration::from_secs(120));
        let req = Request {
            path: path.to_path_buf(),
            max_text_bytes: ctx.limits.max_text_bytes,
            max_pages: ctx.limits.max_pdf_pages,
            timeout_ms: timeout.as_millis() as u64,
        };
        let mut h = self.acquire()?;
        let pid = h.child.id();
        self.busy.lock().push(Busy { started: Instant::now(), limit: timeout, pid });
        let result = (|| -> std::io::Result<Response> {
            serde_json::to_writer(&mut h.stdin, &req)?;
            h.stdin.write_all(b"\n")?;
            h.stdin.flush()?;
            let mut line = String::new();
            if h.stdout.read_line(&mut line)? == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "helper exited"));
            }
            serde_json::from_str(&line).map_err(std::io::Error::other)
        })();
        self.busy.lock().retain(|b| b.pid != pid);
        match result {
            Ok(r) => {
                self.release(Some(h));
                sink.push_str(&r.text);
                for l in r.locs {
                    // Re-create anchors at their original offsets.
                    sink.anchor_at(l);
                }
                if r.truncated {
                    sink.mark_truncated();
                }
                if r.ok {
                    Ok(DocMeta { title: r.title, author: r.author, subject: r.subject, keywords: r.keywords, pages: r.pages, flags: r.flags })
                } else {
                    let msg = r.message.unwrap_or_default();
                    Err(match r.error.as_deref() {
                        Some("encrypted") => ParseError::Encrypted,
                        Some("timeout") => ParseError::Timeout,
                        Some("unsupported") => ParseError::Unsupported(msg),
                        Some("limit") => ParseError::LimitExceeded(msg),
                        _ => ParseError::Corrupt(msg),
                    })
                }
            }
            Err(e) => {
                // Crashed, killed by the watchdog, or protocol error: drop this helper.
                drop(h);
                self.release(None);
                tracing::warn!(path = %path.display(), error = %e, "PDF helper failed");
                Err(ParseError::Corrupt("PDF engine crashed or timed out on this file".into()))
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut st = self.state.lock();
        let n = st.idle.len();
        st.idle.clear();
        st.running -= n;
    }
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 1);
            CloseHandle(h);
        }
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}
