// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastfind_core::config::AppPaths;
use fastfind_core::parsers::pdfworker;
use fastfind_core::{Engine, EngineOptions};
use parking_lot::Mutex;
use tauri::{Emitter, Manager};

pub struct AppState {
    pub engine: Arc<Engine>,
    /// Cancels the previous search when a new one starts (type-ahead).
    pub search_cancel: Mutex<fastfind_core::util::CancelToken>,
}

fn main() {
    // Helper-process mode: parse PDFs for the main process and exit (never opens a window).
    if std::env::args().any(|a| a == pdfworker::PDF_WORKER_ARG) {
        pdfworker::run_worker(&[]);
    }

    let paths = AppPaths::default_location();
    let _log_guard = fastfind_core::logging::init(&paths.log_dir, cfg!(debug_assertions));
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "FastFind starting");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch focuses the running window instead (the index has one writer).
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let t0 = Instant::now();
            tracing::info!("window runtime ready; opening engine");
            let res = app.path().resource_dir().ok();
            let mut pdfium_dirs = Vec::new();
            if let Some(r) = &res {
                pdfium_dirs.push(r.join("pdfium"));
                pdfium_dirs.push(r.clone());
            }
            let opts = EngineOptions {
                pdfium_dirs,
                watch: true,
                scan_on_start: true,
                pdf_worker_exe: std::env::current_exe().ok(),
            };
            let engine = match Engine::open(paths.clone(), opts) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!(error = %e, "cannot open index");
                    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                    app.dialog()
                        .message(format!("FastFind could not open its index:\n\n{e}\n\nData folder: {}", paths.data_dir.display()))
                        .title("FastFind")
                        .kind(MessageDialogKind::Error)
                        .blocking_show();
                    std::process::exit(1);
                }
            };
            // Tell the UI when newly indexed files become searchable (throttled).
            let handle = app.handle().clone();
            let last = Mutex::new(Instant::now() - Duration::from_secs(10));
            engine.index.add_listener(move |generation| {
                let mut l = last.lock();
                if l.elapsed() >= Duration::from_millis(1500) {
                    *l = Instant::now();
                    let _ = handle.emit("index-updated", generation);
                }
            });
            app.manage(AppState { engine, search_cancel: Mutex::new(Default::default()) });
            tracing::info!(ms = t0.elapsed().as_millis() as u64, "engine ready");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::snippets,
            commands::preview,
            commands::status,
            commands::roots,
            commands::add_root,
            commands::remove_root,
            commands::rescan,
            commands::set_paused,
            commands::rebuild_index,
            commands::get_settings,
            commands::save_settings,
            commands::problems,
            commands::diagnostics,
            commands::open_file,
            commands::reveal_file,
            commands::open_logs,
            commands::supported_extensions,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build FastFind");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                state.engine.shutdown();
            }
            if let Some(p) = pdfworker::pool() {
                p.shutdown();
            }
            tracing::info!("FastFind exited");
        }
    });
}
