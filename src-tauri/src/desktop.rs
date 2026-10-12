//! Running in the background: the tray (menu-bar) icon, the global shortcut that opens the
//! pop-up search, hiding instead of quitting when the main window is closed, and starting at
//! login. Everything here follows `Settings.desktop` and is re-applied when settings are saved.

use std::str::FromStr;

use fastfind_core::config::DesktopSettings;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::AppState;

/// Started at login: open to the tray without showing the main window.
pub const HIDDEN_ARG: &str = "--hidden";

const QUICK: &str = "quick";
const QUICK_WIDTH: f64 = 680.0;
const QUICK_HEIGHT: f64 = 64.0;

/// What the UI needs to know about the pop-up search shortcut.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutStatus {
    pub enabled: bool,
    pub shortcut: String,
    /// The shortcut works right now.
    pub registered: bool,
    /// Why it doesn't: invalid, or taken by another application.
    pub problem: Option<String>,
}

#[derive(Default)]
pub struct DesktopState {
    status: Mutex<ShortcutStatus>,
    quick_item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

/// The global-shortcut plugin, whose handler toggles the pop-up search.
pub fn shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            // In FastFind's own window the keys keep their meaning there (Ctrl+Shift+F opens
            // the filters): a global shortcut never reaches the window, so hand it over.
            // A hidden window can still report focus (hiding sends no focus-lost event).
            let in_front = |w: &WebviewWindow| {
                w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false)
            };
            if let Some(main) = app.get_webview_window("main").filter(in_front) {
                let shortcut = app.state::<DesktopState>().status.lock().shortcut.clone();
                let _ = main.emit_to("main", "app-shortcut", shortcut);
            } else {
                toggle_quick(app);
            }
        })
        .build()
}

/// Once the engine is ready: tray icon, pop-up window, close handling, and the settings.
pub fn setup(app: &AppHandle, main: &WebviewWindow) {
    if let Err(e) = build_tray(app) {
        tracing::warn!(error = %e, "cannot create the tray icon");
    }
    if let Err(e) = build_quick(app) {
        tracing::warn!(error = %e, "cannot create the quick search window");
    }
    let handle = app.clone();
    let window = main.clone();
    main.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            let Some(state) = handle.try_state::<AppState>() else { return };
            let settings = state.engine.settings();
            if !settings.desktop.keep_running {
                return;
            }
            api.prevent_close();
            let _ = window.hide();
            if !settings.desktop.close_notice_shown {
                let mut s = settings.clone();
                s.desktop.close_notice_shown = true;
                let shortcut = s.desktop.quick_search.then(|| display_shortcut(&s.desktop.quick_search_shortcut));
                if let Err(e) = state.engine.update_settings(s) {
                    tracing::warn!(error = %e, "cannot save settings");
                }
                close_notice(&handle, shortcut);
            }
        }
    });
    let desktop = app.state::<AppState>().engine.settings().desktop;
    apply(app, &desktop, None);
}

/// (Re-)register the shortcut and the login item. `previous` avoids touching the login item
/// when that setting didn't change.
pub fn apply(app: &AppHandle, d: &DesktopSettings, previous: Option<&DesktopSettings>) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let mut status = ShortcutStatus { enabled: d.quick_search, shortcut: d.quick_search_shortcut.clone(), ..Default::default() };
    if d.quick_search {
        match Shortcut::from_str(&d.quick_search_shortcut) {
            Err(_) => status.problem = Some(format!("“{}” is not a valid shortcut.", display_shortcut(&d.quick_search_shortcut))),
            Ok(sc) => match gs.register(sc) {
                Ok(()) => status.registered = true,
                Err(e) => {
                    tracing::warn!(shortcut = %d.quick_search_shortcut, error = %e, "cannot register the quick search shortcut");
                    status.problem = Some(format!("{} is already used by another application.", display_shortcut(&d.quick_search_shortcut)));
                }
            },
        }
    }
    let state = app.state::<DesktopState>();
    if let Some(item) = state.quick_item.lock().as_ref() {
        let label = if status.registered { format!("Quick search\t{}", display_shortcut(&d.quick_search_shortcut)) } else { "Quick search".into() };
        let _ = item.set_text(label);
    }
    *state.status.lock() = status;

    if previous.is_none_or(|p| p.start_at_login != d.start_at_login) {
        let auto = app.autolaunch();
        let result = if d.start_at_login { auto.enable() } else if auto.is_enabled().unwrap_or(false) { auto.disable() } else { Ok(()) };
        if let Err(e) = result {
            tracing::warn!(error = %e, enable = d.start_at_login, "cannot change the login item");
        }
    }
}

/// "CommandOrControl+Shift+F" as users read it on this system.
pub fn display_shortcut(s: &str) -> String {
    let mac = cfg!(target_os = "macos");
    s.split('+')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "commandorcontrol" | "commandorctrl" | "cmdorctrl" | "cmdorcontrol" => if mac { "⌘" } else { "Ctrl" }.to_string(),
            "control" | "ctrl" => if mac { "⌃" } else { "Ctrl" }.to_string(),
            "alt" | "option" => if mac { "⌥" } else { "Alt" }.to_string(),
            "shift" => if mac { "⇧" } else { "Shift" }.to_string(),
            "super" | "command" | "cmd" => if mac { "⌘" } else { "Win" }.to_string(),
            other => {
                let k = other.strip_prefix("key").or_else(|| other.strip_prefix("digit")).unwrap_or(other);
                let mut c = k.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(if mac { "" } else { "+" })
}

fn close_notice(app: &AppHandle, shortcut: Option<String>) {
    use tauri_plugin_dialog::DialogExt;
    let how = match shortcut {
        Some(s) => format!("Press {s} anywhere to search, or click the FastFind icon in the "),
        None => "Click the FastFind icon in the ".into(),
    };
    let place = if cfg!(target_os = "macos") { "menu bar" } else { "system tray" };
    app.dialog()
        .message(format!(
            "FastFind keeps running in the background so your files stay indexed.\n\n{how}{place} to open it. To quit, choose Quit FastFind from that icon's menu.\n\nYou can change this in Settings → General."
        ))
        .title("FastFind is still running")
        .show(|_| {});
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let quick = MenuItem::with_id(app, "quick", "Quick search", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open FastFind", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit FastFind", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quick, &open, &PredefinedMenuItem::separator(app)?, &quit])?;
    *app.state::<DesktopState>().quick_item.lock() = Some(quick);
    let mut tray = TrayIconBuilder::with_id("fastfind")
        .tooltip("FastFind")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "quick" => show_quick(app),
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// The pop-up search window: created hidden once, then shown and hidden by the shortcut.
fn build_quick(app: &AppHandle) -> tauri::Result<()> {
    let w = WebviewWindowBuilder::new(app, QUICK, WebviewUrl::App("quick.html".into()))
        .title("FastFind quick search")
        .inner_size(QUICK_WIDTH, QUICK_HEIGHT)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;
    let window = w.clone();
    w.on_window_event(move |event| {
        // Like Spotlight: clicking anywhere else dismisses it. Focused(false) also fires when
        // focus merely moves into the window's own web view (Windows), so check a moment later
        // whether another window really took over.
        if let WindowEvent::Focused(false) = event {
            let window = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if !is_foreground(&window) {
                    let _ = window.hide();
                }
            });
        }
    });
    Ok(())
}

#[cfg(windows)]
fn is_foreground(w: &WebviewWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: no arguments; returns the current foreground window handle (or null).
    w.hwnd().map(|h| unsafe { GetForegroundWindow() } == h.0).unwrap_or(false)
}

#[cfg(not(windows))]
fn is_foreground(w: &WebviewWindow) -> bool {
    w.is_focused().unwrap_or(false)
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_quick(app: &AppHandle) {
    match app.get_webview_window(QUICK) {
        Some(w) if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) => {
            let _ = w.hide();
        }
        _ => show_quick(app),
    }
}

pub fn show_quick(app: &AppHandle) {
    let Some(w) = app.get_webview_window(QUICK) else { return };
    // Centred near the top of the screen the mouse is on.
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    if let (Some(m), Ok(size)) = (monitor, w.outer_size()) {
        let (mp, ms) = (m.position(), m.size());
        let x = mp.x + (ms.width as i32 - size.width as i32) / 2;
        let y = mp.y + ms.height as i32 / 5;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
    // The page answers with `quick_focus` once the window is active.
    let _ = w.emit_to(QUICK, "quick-show", ());
}

pub fn hide_quick(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(QUICK) {
        let _ = w.hide();
    }
}

// ----- commands ---------------------------------------------------------------------------

#[tauri::command]
pub fn shortcut_status(state: tauri::State<'_, DesktopState>) -> ShortcutStatus {
    state.status.lock().clone()
}

/// Give the page keyboard focus. The window's focus is not the web view's (Windows/WebView2),
/// and focusing the web view in the same breath as the window loses to the window's own
/// activation, so the page asks for this after it has been shown.
#[tauri::command]
pub fn quick_focus(app: AppHandle) {
    if let Some(w) = app.get_webview_window(QUICK) {
        let webview: &tauri::Webview = w.as_ref();
        let _ = webview.set_focus();
    }
}

#[tauri::command]
pub fn quick_hide(app: AppHandle) {
    hide_quick(&app);
}

/// Resize the pop-up to fit its results (height in logical pixels).
#[tauri::command]
pub fn quick_resize(app: AppHandle, height: f64) {
    if let Some(w) = app.get_webview_window(QUICK) {
        let _ = w.set_size(tauri::LogicalSize::new(QUICK_WIDTH, height.clamp(QUICK_HEIGHT, 560.0)));
    }
}

/// "Show all results": hand the query to the main window.
#[tauri::command]
pub fn quick_show_all(app: AppHandle, query: String) {
    hide_quick(&app);
    show_main(&app);
    let _ = app.emit_to("main", "show-query", query);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_read_like_the_system_writes_them() {
        if cfg!(target_os = "macos") {
            assert_eq!(display_shortcut("CommandOrControl+Shift+F"), "⌘⇧F");
        } else {
            assert_eq!(display_shortcut("CommandOrControl+Shift+F"), "Ctrl+Shift+F");
            assert_eq!(display_shortcut("Alt+Space"), "Alt+Space");
            assert_eq!(display_shortcut("Ctrl+Alt+KeyK"), "Ctrl+Alt+K");
            assert_eq!(display_shortcut("Super+Digit1"), "Win+1");
        }
    }

    /// The main window must exist only after the engine is ready (its UI calls commands at
    /// once) and `desktop::setup` must get to run on it. A platform config replaces the whole
    /// `windows` list, so each one has to say `"create": false` again (1.4.0 on macOS didn't).
    #[test]
    fn every_platform_config_defers_the_main_window() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.starts_with("tauri.") && name.ends_with(".json")) {
                continue;
            }
            let conf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for w in conf["app"]["windows"].as_array().into_iter().flatten() {
                if w["label"] == "main" {
                    assert_eq!(w["create"], serde_json::Value::Bool(false), "{name}: the main window needs \"create\": false");
                    checked += 1;
                }
            }
        }
        assert!(checked >= 2, "tauri.conf.json and tauri.macos.conf.json both define the main window");
    }

    #[test]
    fn the_default_shortcut_parses() {
        assert!(Shortcut::from_str(fastfind_core::config::DEFAULT_QUICK_SEARCH_SHORTCUT).is_ok());
        assert!(Shortcut::from_str("Ctrl+Alt+Space").is_ok());
        assert!(Shortcut::from_str("Ctrl+Nonsense").is_err());
    }
}
