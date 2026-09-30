//! IPC commands. Every engine call runs on Tauri's blocking pool so the UI thread and the async
//! runtime never wait on disk or CPU work.

use fastfind_core::config::Settings;
use fastfind_core::model::{Diagnostics, IndexStatus, Page, Preview, RootInfo, SearchRequest, SearchResponse, SkippedFile, SnippetResult};
use fastfind_core::util::CancelToken;
use fastfind_core::Error;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

type R<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, req: SearchRequest) -> R<SearchResponse> {
    // Supersede any running search: type-ahead should never queue up stale work.
    let token = CancelToken::new();
    std::mem::replace(&mut *state.search_cancel.lock(), token.clone()).cancel();
    let engine = state.engine.clone();
    blocking(move || match engine.search(&req, &token) {
        Ok(r) => Ok(r),
        Err(Error::Cancelled) => Err("cancelled".into()),
        Err(e) => Err(err(e)),
    })
    .await
}

#[tauri::command]
pub async fn snippets(state: State<'_, AppState>, req: SearchRequest, paths: Vec<String>) -> R<Vec<SnippetResult>> {
    let engine = state.engine.clone();
    blocking(move || engine.snippets(&req, &paths).map_err(err)).await
}

#[tauri::command]
pub async fn preview(state: State<'_, AppState>, req: SearchRequest, path: String) -> R<Preview> {
    let engine = state.engine.clone();
    blocking(move || engine.preview(&req, &path).map_err(err)).await
}

#[tauri::command]
pub async fn status(state: State<'_, AppState>) -> R<IndexStatus> {
    let engine = state.engine.clone();
    blocking(move || engine.status().map_err(err)).await
}

#[tauri::command]
pub async fn roots(state: State<'_, AppState>) -> R<Vec<RootInfo>> {
    let engine = state.engine.clone();
    blocking(move || engine.roots().map_err(err)).await
}

#[tauri::command]
pub async fn add_root(state: State<'_, AppState>, path: String) -> R<RootInfo> {
    let engine = state.engine.clone();
    blocking(move || engine.add_root(&path).map_err(err)).await
}

#[tauri::command]
pub async fn remove_root(state: State<'_, AppState>, id: i64) -> R<()> {
    let engine = state.engine.clone();
    blocking(move || engine.remove_root(id).map_err(err)).await
}

#[tauri::command]
pub async fn rescan(state: State<'_, AppState>, id: Option<i64>) -> R<()> {
    state.engine.rescan(id);
    Ok(())
}

#[tauri::command]
pub async fn set_paused(state: State<'_, AppState>, paused: bool) -> R<()> {
    state.engine.set_paused(paused);
    Ok(())
}

#[tauri::command]
pub async fn rebuild_index(state: State<'_, AppState>) -> R<()> {
    let engine = state.engine.clone();
    blocking(move || {
        engine.rebuild();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> R<Settings> {
    Ok(state.engine.settings())
}

#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> R<Settings> {
    let engine = state.engine.clone();
    blocking(move || engine.update_settings(settings).map_err(err)).await
}

#[tauri::command]
pub async fn problems(state: State<'_, AppState>, status: Option<String>, query: String, offset: u64, limit: u64) -> R<Page<SkippedFile>> {
    let engine = state.engine.clone();
    blocking(move || engine.problems(status.as_deref(), &query, offset, limit).map_err(err)).await
}

#[tauri::command]
pub async fn diagnostics(state: State<'_, AppState>) -> R<Diagnostics> {
    let engine = state.engine.clone();
    blocking(move || Ok(engine.diagnostics())).await
}

/// Only files that are in the index may be opened: the web view cannot make the backend open
/// arbitrary paths even if its content were compromised.
fn check_indexed(state: &AppState, path: &str) -> R<()> {
    if state.engine.is_indexed(path) {
        Ok(())
    } else {
        Err("This file is not in the index.".into())
    }
}

#[tauri::command]
pub async fn open_file(app: AppHandle, state: State<'_, AppState>, path: String) -> R<()> {
    check_indexed(&state, &path)?;
    if !std::path::Path::new(&path).exists() {
        return Err("The file no longer exists. It will disappear from results after the next index update.".into());
    }
    app.opener().open_path(path.clone(), None::<&str>).map_err(err)?;
    state.engine.record_open(&path);
    Ok(())
}

#[tauri::command]
pub async fn reveal_file(app: AppHandle, state: State<'_, AppState>, path: String) -> R<()> {
    check_indexed(&state, &path)?;
    app.opener().reveal_item_in_dir(&path).map_err(err)?;
    state.engine.record_open(&path);
    Ok(())
}

#[tauri::command]
pub async fn open_logs(app: AppHandle, state: State<'_, AppState>) -> R<()> {
    let dir = state.engine.paths.log_dir.to_string_lossy().into_owned();
    app.opener().open_path(dir, None::<&str>).map_err(err)
}

#[tauri::command]
pub async fn supported_extensions(state: State<'_, AppState>) -> R<Vec<String>> {
    Ok(state.engine.supported_extensions().into_iter().map(String::from).collect())
}
