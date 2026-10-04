//! Signed in-app updates through `tauri-plugin-updater`.
//!
//! All network access happens here, in the backend; the web view itself has none. Nothing is
//! contacted until the user allows it: automatic checks run only when
//! `settings.updates.checkAutomatically` is `true` (asked once, on first launch), at most once a
//! day. A manual "Check for updates" is always an explicit user action. The request is a plain
//! download of the release manifest from GitHub; no information about the user's files is sent.
//!
//! Every downloaded package is verified against the public key in `tauri.conf.json` before it
//! is installed, and `requireSignedVersion` rejects a validly signed but older release.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fastfind_core::parsers::pdfworker;
use fastfind_core::Engine;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

use crate::AppState;

/// Opened when the running copy cannot replace itself (Linux .deb/.rpm installs).
pub const RELEASES_URL: &str = "https://github.com/ZEraX4/FastFind/releases/latest";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Give the first launch time to open the index and show results before any network work.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct UpdateState {
    /// The update found by the last check, waiting for "Install and restart".
    pending: Mutex<Option<Update>>,
    installing: Mutex<bool>,
    /// Set once the engine has been shut down for installing (the Windows hook runs inside
    /// the plugin, just before it launches the installer).
    engine_stopped: Arc<AtomicBool>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    /// Release notes (Markdown as written on the GitHub release; shown as plain text).
    pub notes: Option<String>,
    pub date: Option<String>,
    /// False when this copy cannot update itself (a Linux package-manager install); the UI
    /// then offers to open the download page instead.
    pub can_install: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// Windows installers, the macOS app bundle and the Linux AppImage can replace themselves.
/// A .deb/.rpm install belongs to the system package manager.
fn can_self_update() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some()
}

fn info(u: &Update) -> UpdateInfo {
    UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone().filter(|b| !b.trim().is_empty()),
        date: u.date.map(|d| d.date().to_string()),
        can_install: can_self_update(),
    }
}

fn stamp_file(app: &AppHandle) -> PathBuf {
    app.state::<AppState>().engine.paths.data_dir.join("last-update-check")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn last_check(app: &AppHandle) -> Option<u64> {
    std::fs::read_to_string(stamp_file(app)).ok()?.trim().parse().ok()
}

fn due(last: Option<u64>, now: u64) -> bool {
    match last {
        Some(t) => now.saturating_sub(t) >= CHECK_INTERVAL.as_secs() || t > now,
        None => true,
    }
}

/// The updater, with a pre-install hook that closes FastFind's index instead of only Tauri's
/// default cleanup. An `Update` keeps the hook of the updater that found it.
fn updater(app: &AppHandle) -> Result<Updater, String> {
    let engine = app.state::<AppState>().engine.clone();
    let stopped = app.state::<UpdateState>().engine_stopped.clone();
    let handle = app.clone();
    app.updater_builder()
        .on_before_exit(move || {
            shutdown_engine(&engine);
            stopped.store(true, Ordering::SeqCst);
            handle.cleanup_before_exit();
        })
        .build()
        .map_err(|e| e.to_string())
}

/// One check against the release manifest. Remembers the result for `install_update`.
async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = updater(app)?;
    let found = updater.check().await.map_err(|e| {
        tracing::warn!(error = %e, "update check failed");
        format!("Could not check for updates: {e}")
    })?;
    let _ = std::fs::write(stamp_file(app), now_secs().to_string());
    let state = app.state::<UpdateState>();
    let result = found.as_ref().map(info);
    match &result {
        Some(i) => tracing::info!(current = %i.current_version, available = %i.version, "update available"),
        None => tracing::info!("no update available"),
    }
    *state.pending.lock() = found;
    Ok(result)
}

/// Background loop: checks once a day while automatic checks are allowed. Re-reads the setting
/// every round, so turning it off in Settings takes effect without a restart.
pub fn spawn_auto_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio_sleep(FIRST_CHECK_DELAY).await;
        loop {
            let allowed = app.state::<AppState>().engine.settings().updates.check_automatically == Some(true);
            if allowed && due(last_check(&app), now_secs()) {
                if let Ok(Some(info)) = check(&app).await {
                    let _ = app.emit("update-available", info);
                }
            }
            tokio_sleep(Duration::from_secs(60 * 60)).await;
        }
    });
}

async fn tokio_sleep(d: Duration) {
    // Tauri's async runtime is Tokio, but sleeping on a blocking thread keeps this crate free of
    // a direct Tokio dependency.
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

/// Close the index cleanly (final commit, watchers stopped) and stop the PDF helper processes
/// before an installer replaces the executable.
fn shutdown_engine(engine: &Arc<Engine>) {
    tracing::info!("shutting down for update");
    engine.shutdown();
    if let Some(p) = pdfworker::pool() {
        p.shutdown();
    }
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    check(&app).await
}

/// Downloads the pending update (progress as `update-progress` events), verifies its
/// signature, then installs it and restarts. On Windows the installer takes over and this
/// process exits; on macOS/Linux the bundle is replaced in place and the app relaunches.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, UpdateState>) -> Result<(), String> {
    if !can_self_update() {
        return Err("This copy of FastFind is managed by your package manager. Update it from there or download the new version.".into());
    }
    {
        let mut busy = state.installing.lock();
        if *busy {
            return Err("An update is already being installed.".into());
        }
        *busy = true;
    }
    let result = install(&app, &state).await;
    *state.installing.lock() = false;
    result
}

async fn install(app: &AppHandle, state: &UpdateState) -> Result<(), String> {
    let Some(update) = state.pending.lock().clone() else {
        return Err("No update is waiting to be installed. Check for updates first.".into());
    };
    let engine = app.state::<AppState>().engine.clone();
    tracing::info!(version = %update.version, "downloading update");

    let mut downloaded = 0u64;
    let mut last_emit = 0u64;
    let emitter = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                // About 100 progress events per download at most.
                let step = total.map(|t| t / 100).unwrap_or(256 * 1024).max(64 * 1024);
                if downloaded - last_emit >= step || Some(downloaded) == total {
                    last_emit = downloaded;
                    let _ = emitter.emit("update-progress", Progress { downloaded, total });
                }
            },
            || {},
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "update download failed");
            format!("The update could not be downloaded or its signature is invalid: {e}")
        })?;
    tracing::info!(bytes = bytes.len(), "update downloaded and verified; installing");

    let stopped = state.engine_stopped.clone();
    let installed = tauri::async_runtime::spawn_blocking(move || {
        if !cfg!(windows) {
            // macOS/Linux replace the bundle in place and return; close the index before the
            // relaunch. (On Windows the plugin runs the hook, then exits after starting the
            // installer.)
            shutdown_engine(&engine);
            stopped.store(true, Ordering::SeqCst);
        }
        update.install(bytes)
    })
    .await
    .map_err(|e| e.to_string())?;
    match installed {
        Ok(()) => {
            tracing::info!("update installed; restarting");
            app.restart();
        }
        Err(e) => {
            tracing::error!(error = %e, "update install failed");
            if state.engine_stopped.load(Ordering::SeqCst) {
                // The index is already closed: restart the current version rather than leave
                // a window with no engine behind it.
                let _ = app.emit("update-failed-restarting", e.to_string());
                tokio_sleep(Duration::from_secs(4)).await;
                app.restart();
            }
            Err(format!("The update could not be installed: {e}"))
        }
    }
}

#[tauri::command]
pub fn open_release_page(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(RELEASES_URL, None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_once_a_day() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(due(None, 1_000_000));
        assert!(!due(Some(1_000_000), 1_000_000 + day - 1));
        assert!(due(Some(1_000_000), 1_000_000 + day));
        // A clock moved backwards must not suppress checks forever.
        assert!(due(Some(2_000_000), 1_000_000));
    }
}
