// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod desktop;
mod updates;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastfind_core::config::AppPaths;
use fastfind_core::index::IndexStore;
use fastfind_core::parsers::pdfworker;
use fastfind_core::{Engine, EngineOptions};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

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
            // A second launch shows the running window instead (the index has one writer); it may
            // be hidden in the tray.
            desktop::show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(desktop::shortcut_plugin())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![desktop::HIDDEN_ARG])))
        .manage(updates::UpdateState::default())
        .manage(desktop::DesktopState::default())
        .setup(move |app| {
            // Once after some updates, opening the engine upgrades the index format (seconds per
            // 100,000 files). The event loop only runs after `setup` returns, so the engine opens
            // on a thread, a small progress window is shown meanwhile, and the main window (whose
            // UI calls commands immediately) is created once the engine is ready.
            let splash = if IndexStore::upgrade_pending(&paths.index_dir) {
                tracing::info!("index format upgrade pending; showing the progress window");
                WebviewWindowBuilder::new(app, "splash", WebviewUrl::App("splash.html".into()))
                    .title("FastFind")
                    .inner_size(440.0, 170.0)
                    .resizable(false)
                    .decorations(false)
                    .closable(false)
                    .center()
                    .build()
                    .map_err(|e| tracing::warn!(error = %e, "cannot show the progress window"))
                    .ok()
            } else {
                None
            };
            let (handle, paths) = (app.handle().clone(), paths.clone());
            std::thread::Builder::new().name("ff-open".into()).spawn(move || open_engine(handle, paths, splash))?;
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
            commands::check_ocr,
            updates::check_for_update,
            updates::install_update,
            updates::open_release_page,
            desktop::shortcut_status,
            desktop::quick_focus,
            desktop::quick_hide,
            desktop::quick_resize,
            desktop::quick_show_all,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build FastFind");

    app.run(|handle, event| {
        // macOS: clicking the Dock icon while the window is hidden in the menu bar.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            desktop::show_main(handle);
        }
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

/// Open the engine, make it available to commands, then show the main window.
fn open_engine(handle: AppHandle, paths: AppPaths, splash: Option<WebviewWindow>) {
    let t0 = Instant::now();
    tracing::info!("window runtime ready; opening engine");
    let res = handle.path().resource_dir().ok();
    let mut pdfium_dirs = Vec::new();
    if let Some(r) = &res {
        pdfium_dirs.push(r.join("pdfium"));
        pdfium_dirs.push(r.clone());
    }
    let opts = EngineOptions { pdfium_dirs, watch: true, scan_on_start: true, pdf_worker_exe: std::env::current_exe().ok() };
    let engine = match Engine::open(paths.clone(), opts) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "cannot open index");
            use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
            handle
                .dialog()
                .message(format!("FastFind could not open its index:\n\n{e}\n\nData folder: {}", paths.data_dir.display()))
                .title("FastFind")
                .kind(MessageDialogKind::Error)
                .blocking_show();
            std::process::exit(1);
        }
    };
    // Tell the UI when newly indexed files become searchable (throttled).
    let emitter = handle.clone();
    let last = Mutex::new(Instant::now() - Duration::from_secs(10));
    engine.index.add_listener(move |generation| {
        let mut l = last.lock();
        if l.elapsed() >= Duration::from_millis(1500) {
            *l = Instant::now();
            let _ = emitter.emit("index-updated", generation);
        }
    });
    handle.manage(AppState { engine, search_cancel: Mutex::new(Default::default()) });
    updates::spawn_auto_check(handle.clone());
    tracing::info!(ms = t0.elapsed().as_millis() as u64, "engine ready");

    // The main window is declared in tauri.conf.json with "create": false. Started at login, it
    // stays hidden: FastFind waits in the tray for the shortcut.
    let hidden = std::env::args().any(|a| a == desktop::HIDDEN_ARG);
    let window = handle.config().app.windows.iter().find(|w| w.label == "main").cloned();
    match window.map(|cfg| WebviewWindowBuilder::from_config(&handle, &cfg).and_then(|b| b.visible(!hidden).build())) {
        Some(Ok(main)) => desktop::setup(&handle, &main),
        Some(Err(e)) => tracing::error!(error = %e, "cannot create the main window"),
        None => tracing::error!("no main window in the configuration"),
    }
    // After the main window exists, so closing it never leaves the app without windows.
    if let Some(s) = splash {
        let _ = s.destroy();
    }
}
