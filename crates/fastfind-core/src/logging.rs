//! Structured logging.
//!
//! JSON lines, rotated daily, at most 7 files kept. **Document contents are never logged**:
//! log events carry paths, sizes, durations and error kinds only. Query strings are logged
//! only as their length.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// Initialise global logging. Keep the returned guard alive for the process lifetime so buffered
/// log lines are flushed on exit. Calling twice is harmless (the second call is a no-op).
pub fn init(log_dir: &Path, also_stderr: bool) -> Option<WorkerGuard> {
    let _ = std::fs::create_dir_all(log_dir);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("fastfind")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    // Third-party crates (tantivy segment bookkeeping etc.) only log warnings and above.
    let filter = || EnvFilter::try_from_env("FASTFIND_LOG").unwrap_or_else(|_| EnvFilter::new("info,tantivy=warn,notify=warn,tao=warn,wry=warn"));
    let file_layer = fmt::layer().json().with_current_span(false).with_writer(writer).with_filter(filter());
    let stderr_layer = also_stderr.then(|| fmt::layer().with_writer(std::io::stderr).with_filter(filter()));
    tracing_subscriber::registry().with(file_layer).with(stderr_layer).try_init().ok()?;
    Some(guard)
}
