use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::config::{safe_external_update_url, write_json_pretty_atomic};
use crate::desktop::local_library::LocalImportEntry;
use crate::desktop::memory::{
    get_memory_snapshot as mem_snapshot, purge_system_memory as mem_purge,
    trim_app_memory as mem_trim, PurgeSystemMemoryPayload,
};
use crate::desktop::win32;
use crate::server::AppBackendState;

const WINDOW_SETTINGS_FILE: &str = "window-settings.json";
const DESKTOP_LYRICS_LABEL: &str = "desktop-lyrics";
const PLATFORM_LOGIN_LABEL: &str = "platform-login";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedWindowSettings {
    close_behavior: String,
}

impl Default for PersistedWindowSettings {
    fn default() -> Self {
        Self {
            close_behavior: "tray".to_string(),
        }
    }
}

pub struct TauriAppState {
    pub backend: Arc<AppBackendState>,
    pub http_port: AtomicU16,
    pub desktop_lyrics_enabled: AtomicBool,
    pub desktop_lyrics_locked: AtomicBool,
    pub desktop_lyrics_payload: Mutex<Option<Value>>,
    pub close_behavior: Mutex<String>,
    pub desktop_software_locked: AtomicBool,
    pub desktop_icons_visible: AtomicBool,
    pub global_hotkey_bindings: Mutex<Vec<Value>>,
    pub pointer_route: Mutex<Value>,
}

impl TauriAppState {
    pub fn new(backend: Arc<AppBackendState>) -> Self {
        let settings_path = backend.config.paths().user_data_dir.join(WINDOW_SETTINGS_FILE);
        let close_behavior = std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str::<PersistedWindowSettings>(&s).ok())
            .map(|s| normalize_close_behavior(&s.close_behavior))
            .unwrap_or_else(|| "tray".to_string());

        Self {
            backend,
            http_port: AtomicU16::new(0),
            desktop_lyrics_enabled: AtomicBool::new(false),
            desktop_lyrics_locked: AtomicBool::new(false),
            desktop_lyrics_payload: Mutex::new(None),
            close_behavior: Mutex::new(close_behavior),
            desktop_software_locked: AtomicBool::new(false),
            desktop_icons_visible: AtomicBool::new(true),
            global_hotkey_bindings: Mutex::new(Vec::new()),
            pointer_route: Mutex::new(json!({
                "overSoftwareUi": true,
                "overDesktopControls": false
            })),
        }
    }

    pub fn http_base(&self) -> String {
        let port = self.http_port.load(Ordering::Acquire);
        if port > 0 {
            format!("http://127.0.0.1:{port}")
        } else {
            String::new()
        }
    }

    pub fn save_close_behavior(&self, behavior: &str) -> String {
        let norm = normalize_close_behavior(behavior);
        *self.close_behavior.lock().unwrap() = norm.clone();
        let settings_path = self
            .backend
            .config
            .paths()
            .user_data_dir
            .join(WINDOW_SETTINGS_FILE);
        let _ = write_json_pretty_atomic(
            &settings_path,
            &PersistedWindowSettings {
                close_behavior: norm.clone(),
            },
        );
        norm
    }
}

fn normalize_close_behavior(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "quit" | "exit" => "quit".to_string(),
        _ => "tray".to_string(),
    }
}

pub fn build_window_state_payload(app: &AppHandle, state: &TauriAppState) -> Value {
    let win = app.get_webview_window("main");
    let is_maximized = win
        .as_ref()
        .and_then(|w| w.is_maximized().ok())
        .unwrap_or(false);
    let is_fullscreen = win
        .as_ref()
        .and_then(|w| w.is_fullscreen().ok())
        .unwrap_or(false);
    let is_minimized = win
        .as_ref()
        .and_then(|w| w.is_minimized().ok())
        .unwrap_or(false);
    let wp_status = state.backend.wallpaper_engine.get_wallpaper_mode_status();
    let close_behavior = state.close_behavior.lock().unwrap().clone();

    json!({
        "isDesktop": true,
        "isMaximized": is_maximized,
        "isFullScreen": is_fullscreen,
        "isMinimized": is_minimized,
        "isDesktopEmbedded": wp_status.enabled,
        "isDesktopInteractive": !state.desktop_software_locked.load(Ordering::Relaxed),
        "isPrimaryDisplay": true,
        "hasDisplayOnLeft": false,
        "closeBehavior": close_behavior,
    })
}

pub fn emit_window_state(app: &AppHandle, state: &TauriAppState) {
    let payload = build_window_state_payload(app, state);
    let _ = app.emit("desktop-window-state", payload);
}

#[tauri::command]
pub fn get_runtime_endpoints(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!({
        "httpBase": state.http_base(),
        "mediaToken": state.backend.local_library.media_token(),
        "wallpaperMediaToken": state.backend.wallpaper_engine.list_projects(false).media_token,
    })
}

// ==================== 窗口控制 ====================

#[tauri::command]
pub fn desktop_window_minimize(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.minimize();
        emit_window_state(&app, &state);
        return json!({ "ok": true });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_restore(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        emit_window_state(&app, &state);
        return json!({ "ok": true });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_toggle_maximize(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let is_max = win.is_maximized().unwrap_or(false);
        if is_max {
            let _ = win.unmaximize();
        } else {
            let _ = win.maximize();
        }
        emit_window_state(&app, &state);
        return json!({ "ok": true, "isMaximized": !is_max });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_toggle_fullscreen(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let is_fs = win.is_fullscreen().unwrap_or(false);
        let _ = win.set_fullscreen(!is_fs);
        emit_window_state(&app, &state);
        return json!({ "ok": true, "isFullScreen": !is_fs });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_exit_fullscreen_windowed(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_fullscreen(false);
        let _ = win.unmaximize();
        emit_window_state(&app, &state);
        return json!({ "ok": true, "isFullScreen": false });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_close(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    behavior: Option<String>,
) -> Value {
    let resolved = behavior
        .as_deref()
        .map(normalize_close_behavior)
        .unwrap_or_else(|| state.close_behavior.lock().unwrap().clone());
    if resolved == "quit" {
        app.exit(0);
        return json!({ "ok": true, "behavior": "quit" });
    }
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
        emit_window_state(&app, &state);
        return json!({ "ok": true, "behavior": "tray" });
    }
    json!({ "ok": false })
}

#[tauri::command]
pub fn desktop_window_get_state(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    build_window_state_payload(&app, &state)
}

#[tauri::command]
pub fn desktop_window_get_close_behavior(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    let behavior = state.close_behavior.lock().unwrap().clone();
    json!({ "behavior": behavior })
}

#[tauri::command]
pub fn desktop_window_set_close_behavior(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    behavior: String,
) -> Value {
    let saved = state.save_close_behavior(&behavior);
    emit_window_state(&app, &state);
    json!({ "ok": true, "behavior": saved })
}

// ==================== 彩蛋门控 ====================

#[tauri::command]
pub fn get_login_easter_egg_status(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!(state.backend.gate.public_status())
}

#[tauri::command]
pub fn unlock_login_easter_egg(
    state: tauri::State<'_, Arc<TauriAppState>>,
    password: String,
) -> Value {
    json!(state.backend.gate.unlock(&password))
}

#[tauri::command]
pub fn reset_login_easter_egg(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    let b = state.backend.clone();
    let res = state.backend.gate.reset_for_replay(|| {
        b.clear_all_credentials();
        Ok(())
    });
    json!(res)
}

// ==================== 内置歌单 CRUD ====================

#[tauri::command]
pub fn list_built_in_playlists(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!(state.backend.builtin_playlists.list())
}

#[tauri::command]
pub fn read_built_in_playlist(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Value {
    json!(state
        .backend
        .builtin_playlists
        .page(&id, offset.unwrap_or(0), limit.unwrap_or(200)))
}

#[tauri::command]
pub fn create_built_in_playlist(
    state: tauri::State<'_, Arc<TauriAppState>>,
    name: String,
) -> Value {
    match state.backend.builtin_playlists.create(&name) {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

#[tauri::command]
pub fn rename_built_in_playlist(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    name: String,
) -> Value {
    match state.backend.builtin_playlists.rename(&id, &name) {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

#[tauri::command]
pub fn delete_built_in_playlist(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
) -> Value {
    match state.backend.builtin_playlists.delete(&id) {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

#[tauri::command]
pub fn add_built_in_playlist_track(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    track: Value,
) -> Value {
    match state.backend.builtin_playlists.add_track(&id, &track) {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

#[tauri::command]
pub fn remove_built_in_playlist_track(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    index: usize,
) -> Value {
    match state.backend.builtin_playlists.remove_track(&id, index) {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

#[tauri::command]
pub fn reorder_built_in_playlist_track(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    from_index: usize,
    to_index: usize,
) -> Value {
    match state
        .backend
        .builtin_playlists
        .reorder_track(&id, from_index, to_index)
    {
        Ok(res) => json!(res),
        Err(err) => json!({ "ok": false, "error": err.code() }),
    }
}

// ==================== 本地曲库 ====================

#[tauri::command]
pub async fn choose_local_music_files(app: AppHandle) -> Value {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择本地音频文件")
            .add_filter(
                "音频文件",
                &["mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "alac", "aiff"],
            )
            .blocking_pick_files()
    })
    .await
    .ok()
    .flatten();

    match picked {
        Some(paths) if !paths.is_empty() => {
            let files: Vec<String> = paths
                .into_iter()
                .filter_map(|p| p.into_path().ok())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            json!({ "ok": true, "canceled": false, "files": files })
        }
        _ => json!({ "ok": false, "canceled": true, "files": [] }),
    }
}

#[tauri::command]
pub fn list_local_music_library(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!(state.backend.local_library.list_tracks())
}

#[tauri::command]
pub fn read_local_music_lyric(
    state: tauri::State<'_, Arc<TauriAppState>>,
    local_file_id: String,
) -> Value {
    json!(state.backend.local_library.lyric_for_track(&local_file_id))
}

#[tauri::command]
pub fn authorize_local_music(
    state: tauri::State<'_, Arc<TauriAppState>>,
    files: Vec<LocalImportEntry>,
) -> Value {
    json!(state.backend.local_library.authorize_files(files, false))
}

#[tauri::command]
pub fn import_local_music(
    state: tauri::State<'_, Arc<TauriAppState>>,
    token: String,
) -> Value {
    json!(state.backend.local_library.import_authorized(&token))
}

// ==================== 缓存与存档 ====================

#[tauri::command]
pub fn get_cache_settings(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!(state.backend.config.get_cache_settings())
}

#[tauri::command]
pub fn set_cache_settings(
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Option<Value>,
) -> Value {
    let p = payload.unwrap_or(Value::Null);
    let root = p
        .get("rootPath")
        .or_else(|| p.get("root_path"))
        .or_else(|| p.get("path"))
        .or_else(|| p.get("dir"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            state
                .backend
                .config
                .get_cache_settings()
                .settings
                .root_path
        });
    match state.backend.config.set_cache_settings(&root) {
        Ok(snap) => json!(snap),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

#[tauri::command]
pub async fn choose_cache_directory(app: AppHandle) -> Value {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择缓存根目录")
            .blocking_pick_folder()
    })
    .await
    .ok()
    .flatten();

    match picked.and_then(|p| p.into_path().ok()) {
        Some(path) => json!({
            "ok": true,
            "canceled": false,
            "path": path.to_string_lossy().into_owned(),
        }),
        None => json!({ "ok": false, "canceled": true }),
    }
}

#[tauri::command]
pub fn read_current_fx_autosave(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    match state.backend.config.read_current_fx_autosave() {
        Some(snap) => json!({ "ok": true, "snapshot": snap }),
        None => json!({ "ok": true, "snapshot": Value::Null }),
    }
}

#[tauri::command]
pub fn save_current_fx_autosave(
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Value,
) -> Value {
    match state.backend.config.save_current_fx_autosave(&payload) {
        Ok(()) => json!({ "ok": true }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

#[tauri::command]
pub fn read_lyric_cache(
    state: tauri::State<'_, Arc<TauriAppState>>,
    key: String,
) -> Value {
    json!(state.backend.config.read_lyric_cache(&key))
}

#[tauri::command]
pub fn write_lyric_cache(
    state: tauri::State<'_, Arc<TauriAppState>>,
    key: String,
    payload: Value,
) -> Value {
    match state.backend.config.write_lyric_cache(&key, &payload) {
        Ok(()) => json!({ "ok": true }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

// ==================== 内存快照与清理 ====================

#[tauri::command]
pub fn get_memory_snapshot() -> Value {
    json!(mem_snapshot())
}

#[tauri::command]
pub fn trim_app_memory(payload: Option<Value>) -> Value {
    let reason = payload
        .as_ref()
        .and_then(|v| v.get("reason"))
        .and_then(|v| v.as_str());
    json!(mem_trim(reason))
}

#[tauri::command]
pub fn purge_system_memory(payload: Option<PurgeSystemMemoryPayload>) -> Value {
    json!(mem_purge(payload))
}

#[tauri::command]
pub fn configure_memory_reduct(payload: Option<Value>) -> Value {
    let supported = cfg!(target_os = "windows");
    json!({
        "ok": supported,
        "supported": supported,
        "config": payload.unwrap_or_else(|| json!({})),
    })
}

// ==================== 桌面歌词悬浮窗 ====================

fn ensure_desktop_lyrics_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(existing) = app.get_webview_window(DESKTOP_LYRICS_LABEL) {
        return Ok(existing);
    }

    let builder = WebviewWindowBuilder::new(
        app,
        DESKTOP_LYRICS_LABEL,
        WebviewUrl::App("desktop-lyrics.html".into()),
    )
    .title("Mineradio 桌面歌词")
    .inner_size(760.0, 180.0)
    .min_inner_size(360.0, 96.0)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false);

    builder.build().map_err(|e| format!("CREATE_LYRICS_WINDOW_FAILED:{e}"))
}

#[tauri::command]
pub fn set_desktop_lyrics_enabled(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    enabled: bool,
    payload: Option<Value>,
) -> Value {
    state
        .desktop_lyrics_enabled
        .store(enabled, Ordering::Release);
    if let Some(ref p) = payload {
        *state.desktop_lyrics_payload.lock().unwrap() = Some(p.clone());
    }

    if enabled {
        match ensure_desktop_lyrics_window(&app) {
            Ok(win) => {
                let locked = state.desktop_lyrics_locked.load(Ordering::Relaxed);
                let _ = win.set_ignore_cursor_events(locked);
                let _ = win.show();
                let current_payload = state.desktop_lyrics_payload.lock().unwrap().clone();
                if let Some(p) = current_payload {
                    let _ = app.emit("mineradio-desktop-lyrics-state", p);
                }
            }
            Err(err) => {
                return json!({ "ok": false, "enabled": false, "error": err });
            }
        }
    } else if let Some(win) = app.get_webview_window(DESKTOP_LYRICS_LABEL) {
        let _ = win.hide();
    }

    let _ = app.emit(
        "mineradio-desktop-lyrics-enabled-state",
        json!({ "enabled": enabled }),
    );
    json!({ "ok": true, "enabled": enabled })
}

#[tauri::command]
pub fn update_desktop_lyrics(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Value,
) -> Value {
    *state.desktop_lyrics_payload.lock().unwrap() = Some(payload.clone());
    let _ = app.emit("mineradio-desktop-lyrics-state", payload);
    json!({ "ok": true })
}

#[tauri::command]
pub fn set_desktop_lyrics_lock_state(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    locked: bool,
) -> Value {
    state
        .desktop_lyrics_locked
        .store(locked, Ordering::Release);
    if let Some(win) = app.get_webview_window(DESKTOP_LYRICS_LABEL) {
        let _ = win.set_ignore_cursor_events(locked);
    }
    let _ = app.emit(
        "mineradio-desktop-lyrics-lock-state",
        json!({ "locked": locked }),
    );
    json!({ "ok": true, "locked": locked })
}

#[tauri::command]
pub fn move_desktop_lyrics_by(app: AppHandle, dx: f64, dy: f64) -> Value {
    if let Some(win) = app.get_webview_window(DESKTOP_LYRICS_LABEL) {
        if let Ok(pos) = win.outer_position() {
            let next_x = pos.x + dx.round() as i32;
            let next_y = pos.y + dy.round() as i32;
            let _ = win.set_position(PhysicalPosition::new(next_x, next_y));
            return json!({ "ok": true, "x": next_x, "y": next_y });
        }
    }
    json!({ "ok": false })
}

// ==================== 外部打开 / 剪贴板 / JSON 导入导出 ====================

#[tauri::command]
pub fn open_update_page(url: String) -> Value {
    let Some(safe) = safe_external_update_url(&url) else {
        return json!({ "ok": false, "error": "INVALID_UPDATE_URL" });
    };
    match opener::open(&safe) {
        Ok(()) => json!({ "ok": true, "url": safe }),
        Err(err) => json!({ "ok": false, "error": format!("OPEN_URL_FAILED:{err}") }),
    }
}

#[tauri::command]
pub fn copy_text(text: String) -> Value {
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(()) => json!({ "ok": true }),
        Err(err) => json!({ "ok": false, "error": format!("CLIPBOARD_WRITE_FAILED:{err}") }),
    }
}

#[tauri::command]
pub fn read_text() -> Value {
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) => json!({ "ok": true, "text": text }),
        Err(err) => json!({ "ok": false, "text": "", "error": format!("CLIPBOARD_READ_FAILED:{err}") }),
    }
}

#[tauri::command]
pub async fn export_json_file(app: AppHandle, payload: Value) -> Value {
    let default_name = payload
        .get("filename")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("mineradio-export.json")
        .to_string();
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("导出 JSON 文件")
        .to_string();
    let data = payload.get("data").cloned().unwrap_or(Value::Null);

    let saved = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title(&title)
            .set_file_name(&default_name)
            .add_filter("JSON", &["json"])
            .blocking_save_file()
    })
    .await
    .ok()
    .flatten();

    let Some(path) = saved.and_then(|p| p.into_path().ok()) else {
        return json!({ "ok": false, "canceled": true });
    };

    match write_json_pretty_atomic(&path, &data) {
        Ok(()) => json!({
            "ok": true,
            "canceled": false,
            "path": path.to_string_lossy().into_owned(),
        }),
        Err(err) => json!({ "ok": false, "error": format!("EXPORT_WRITE_FAILED:{err}") }),
    }
}

#[tauri::command]
pub async fn import_json_file(app: AppHandle) -> Value {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("导入 JSON 文件")
            .add_filter("JSON", &["json"])
            .blocking_pick_file()
    })
    .await
    .ok()
    .flatten();

    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return json!({ "ok": false, "canceled": true });
    };

    let Ok(meta) = std::fs::metadata(&path) else {
        return json!({ "ok": false, "error": "IMPORT_READ_FAILED" });
    };
    if meta.len() > 24 * 1024 * 1024 {
        return json!({ "ok": false, "error": "IMPORT_FILE_TOO_LARGE" });
    }
    match std::fs::read_to_string(&path)
        .map_err(|e| format!("IMPORT_READ_FAILED:{e}"))
        .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| format!("INVALID_JSON:{e}")))
    {
        Ok(data) => json!({
            "ok": true,
            "canceled": false,
            "path": path.to_string_lossy().into_owned(),
            "data": data,
        }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

// ==================== Wallpaper Engine & 桌面模式 ====================

#[tauri::command]
pub fn list_wallpaper_engine_projects(
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Option<Value>,
) -> Value {
    let force_refresh = payload
        .as_ref()
        .and_then(|v| v.get("forceRefresh").or_else(|| v.get("force")))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    json!(state.backend.wallpaper_engine.list_projects(force_refresh))
}

#[tauri::command]
pub fn get_wallpaper_engine_project_details(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
) -> Value {
    match state.backend.wallpaper_engine.get_project_details(&id) {
        Ok(details) => json!({ "ok": true, "project": details }),
        Err(err) => json!({ "ok": false, "project": Value::Null, "error": err }),
    }
}

#[tauri::command]
pub fn open_wallpaper_engine_project_details(
    state: tauri::State<'_, Arc<TauriAppState>>,
    id: String,
    target: Option<String>,
) -> Value {
    let Ok(details) = state.backend.wallpaper_engine.get_project_details(&id) else {
        return json!({ "ok": false, "error": "WALLPAPER_PROJECT_NOT_FOUND" });
    };
    let mode = target.unwrap_or_else(|| "we".to_string());
    if mode == "workshop" {
        if !details.workshop_id.is_empty() && details.workshop_id.chars().all(|c| c.is_ascii_digit()) {
            let url = format!(
                "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
                details.workshop_id
            );
            let _ = opener::open(&url);
            return json!({ "ok": true, "url": url });
        }
        return json!({ "ok": false, "error": "MISSING_WORKSHOP_ID" });
    }
    if !details.workshop_id.is_empty() && details.workshop_id.chars().all(|c| c.is_ascii_digit()) {
        let steam_url = format!("steam://url/CommunityFilePage/{}", details.workshop_id);
        if opener::open(&steam_url).is_ok() {
            return json!({ "ok": true, "url": steam_url });
        }
    }
    json!({ "ok": false, "error": "OPEN_DETAILS_UNAVAILABLE" })
}

#[tauri::command]
pub async fn choose_wallpaper_engine_directory(app: AppHandle) -> Value {
    let app_clone = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app_clone
            .dialog()
            .file()
            .set_title("添加 Wallpaper Engine 壁纸目录")
            .blocking_pick_folder()
    })
    .await
    .ok()
    .flatten();

    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return json!({ "ok": false, "canceled": true });
    };

    let state = app.state::<Arc<TauriAppState>>();
    match state.backend.wallpaper_engine.add_directory(&path) {
        Ok(snapshot) => json!({
            "ok": true,
            "canceled": false,
            "path": path.to_string_lossy().into_owned(),
            "snapshot": snapshot,
        }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

#[tauri::command]
pub async fn choose_wallpaper_engine_project_file(app: AppHandle) -> Value {
    let app_clone = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app_clone
            .dialog()
            .file()
            .set_title("选择 Wallpaper Engine project.json")
            .add_filter("Wallpaper Project", &["json"])
            .blocking_pick_file()
    })
    .await
    .ok()
    .flatten();

    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return json!({ "ok": false, "canceled": true });
    };

    let state = app.state::<Arc<TauriAppState>>();
    match state.backend.wallpaper_engine.add_manual_project_file(&path) {
        Ok(snapshot) => json!({
            "ok": true,
            "canceled": false,
            "path": path.to_string_lossy().into_owned(),
            "snapshot": snapshot,
        }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

#[tauri::command]
pub fn remove_wallpaper_engine_directory(
    state: tauri::State<'_, Arc<TauriAppState>>,
    root_id: String,
) -> Value {
    match state.backend.wallpaper_engine.remove_directory(&root_id) {
        Ok(snapshot) => json!({ "ok": true, "snapshot": snapshot }),
        Err(err) => json!({ "ok": false, "error": err }),
    }
}

#[tauri::command]
pub fn get_wallpaper_engine_runtime_status(
    state: tauri::State<'_, Arc<TauriAppState>>,
    _payload: Option<Value>,
) -> Value {
    let status = state.backend.wallpaper_engine.get_wallpaper_mode_status();
    json!({
        "ok": status.ok,
        "supported": status.supported,
        "active": status.enabled,
        "platform": status.platform,
        "workerwWindowId": status.workerw_window_id,
        "message": status.message,
    })
}

#[tauri::command]
pub fn start_wallpaper_engine_scene(
    _state: tauri::State<'_, Arc<TauriAppState>>,
    _payload: Option<Value>,
) -> Value {
    json!({
        "ok": false,
        "supported": cfg!(target_os = "windows"),
        "active": false,
        "message": "Wallpaper Engine scene native host is deferred on Tauri; video/image layers play directly inside Mineradio.",
    })
}

#[tauri::command]
pub fn stop_wallpaper_engine_scene(
    _state: tauri::State<'_, Arc<TauriAppState>>,
    _payload: Option<Value>,
) -> Value {
    json!({ "ok": true, "supported": cfg!(target_os = "windows"), "active": false })
}

#[tauri::command]
pub fn set_wallpaper_mode(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    enabled: bool,
    _payload: Option<Value>,
) -> Value {
    #[cfg(target_os = "windows")]
    {
        if let Some(win) = app.get_webview_window("main") {
            let hwnd = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
            if hwnd != 0 {
                if let Err(err) = win32::set_full_desktop_attached(hwnd, enabled) {
                    return json!({
                        "ok": false,
                        "enabled": false,
                        "supported": true,
                        "error": err,
                    });
                }
            }
        }
    }

    match state.backend.wallpaper_engine.set_wallpaper_mode(enabled) {
        Ok(status) => {
            let payload = json!({
                "ok": status.ok,
                "enabled": status.enabled,
                "supported": status.supported,
                "workerwWindowId": status.workerw_window_id,
                "message": status.message,
            });
            let _ = app.emit("mineradio-wallpaper-runtime-state", payload.clone());
            emit_window_state(&app, &state);
            payload
        }
        Err(err) => json!({
            "ok": false,
            "enabled": false,
            "supported": cfg!(target_os = "windows"),
            "error": err,
        }),
    }
}

#[tauri::command]
pub fn update_wallpaper_mode(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Option<Value>,
) -> Value {
    let status = state.backend.wallpaper_engine.get_wallpaper_mode_status();
    let res = json!({
        "ok": status.ok,
        "enabled": status.enabled,
        "supported": status.supported,
        "options": payload.unwrap_or_else(|| json!({})),
    });
    let _ = app.emit("mineradio-wallpaper-runtime-state", res.clone());
    res
}

#[tauri::command]
pub fn get_wallpaper_mode_status(state: tauri::State<'_, Arc<TauriAppState>>) -> Value {
    json!(state.backend.wallpaper_engine.get_wallpaper_mode_status())
}

#[tauri::command]
pub fn set_desktop_software_locked(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    locked: bool,
) -> Value {
    state
        .desktop_software_locked
        .store(locked, Ordering::Release);
    emit_window_state(&app, &state);
    json!({ "ok": true, "locked": locked })
}

#[tauri::command]
pub fn set_desktop_icons_visible(
    state: tauri::State<'_, Arc<TauriAppState>>,
    visible: bool,
) -> Value {
    match win32::set_desktop_icons_visible(visible) {
        Ok(actual) => {
            state.desktop_icons_visible.store(actual, Ordering::Release);
            json!({ "ok": true, "visible": actual, "supported": cfg!(target_os = "windows") })
        }
        Err(err) => json!({
            "ok": false,
            "visible": state.desktop_icons_visible.load(Ordering::Relaxed),
            "supported": cfg!(target_os = "windows"),
            "error": err,
        }),
    }
}

#[tauri::command]
pub fn request_desktop_keyboard_focus(app: AppHandle, _reason: Option<String>) -> Value {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_focus();
    }
    json!({ "ok": true })
}

#[tauri::command]
pub fn update_desktop_pointer_route(
    state: tauri::State<'_, Arc<TauriAppState>>,
    payload: Value,
) -> Value {
    *state.pointer_route.lock().unwrap() = payload;
    json!({ "ok": true })
}

#[tauri::command]
pub fn update_desktop_icon_shields(_payload: Value) -> Value {
    json!({ "ok": true, "supported": cfg!(target_os = "windows") })
}

// ==================== 全局快捷键 / 诊断 / 重启 ====================

#[tauri::command]
pub fn configure_global_hotkeys(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
    bindings: Vec<Value>,
) -> Value {
    *state.global_hotkey_bindings.lock().unwrap() = bindings.clone();
    let manager = app.global_shortcut();
    let _ = manager.unregister_all();

    let mut registered = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for item in bindings {
        let enabled = item
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let accel = item
            .get("accelerator")
            .or_else(|| item.get("shortcut"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if accel.is_empty() {
            continue;
        }
        let action = item
            .get("action")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let app_handle = app.clone();
        let action_clone = action.clone();
        let accel_clone = accel.clone();
        if let Err(err) = manager.on_shortcut(accel.as_str(), move |_app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let _ = app_handle.emit(
                    "mineradio-global-hotkey",
                    json!({
                        "action": action_clone,
                        "accelerator": accel_clone,
                    }),
                );
            }
        }) {
            errors.push(format!("{accel}: {err}"));
        } else {
            registered += 1;
        }
    }

    json!({
        "ok": errors.is_empty(),
        "registered": registered,
        "errors": errors,
    })
}

#[tauri::command]
pub fn get_gpu_diagnostics() -> Value {
    json!({
        "ok": true,
        "runtime": "tauri-webview",
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

#[tauri::command]
pub fn request_gesture_camera_permission() -> Value {
    json!({
        "ok": true,
        "granted": true,
        "delegatedToWebview": true,
    })
}

#[tauri::command]
pub fn restart_app(app: AppHandle) -> Value {
    app.restart();
}

// ==================== 平台 Webview 登录弹窗（受彩蛋门控保护） ====================

fn open_platform_login_window_impl(
    app: &AppHandle,
    state: &TauriAppState,
    title: &str,
    url: &str,
) -> Value {
    if !state.backend.gate.is_unlocked() {
        return json!({
            "ok": false,
            "error": "LOGIN_EASTER_EGG_LOCKED",
            "message": "登录入口尚未解锁"
        });
    }
    if let Some(existing) = app.get_webview_window(PLATFORM_LOGIN_LABEL) {
        let _ = existing.close();
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return json!({ "ok": false, "error": "INVALID_LOGIN_URL" });
    };
    match WebviewWindowBuilder::new(app, PLATFORM_LOGIN_LABEL, WebviewUrl::External(parsed))
        .title(title)
        .inner_size(980.0, 680.0)
        .center()
        .build()
    {
        Ok(_) => json!({ "ok": true, "url": url }),
        Err(err) => json!({ "ok": false, "error": format!("LOGIN_WINDOW_FAILED:{err}") }),
    }
}

#[tauri::command]
pub fn open_netease_music_login(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    open_platform_login_window_impl(
        &app,
        &state,
        "网易云音乐登录",
        "https://music.163.com/#/login",
    )
}

#[tauri::command]
pub fn open_qq_music_login(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    open_platform_login_window_impl(&app, &state, "QQ音乐登录", "https://y.qq.com/")
}

#[tauri::command]
pub fn open_kugou_music_login(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    open_platform_login_window_impl(&app, &state, "酷狗音乐登录", "https://www.kugou.com/")
}

#[tauri::command]
pub fn open_qishui_auth_login(
    app: AppHandle,
    state: tauri::State<'_, Arc<TauriAppState>>,
) -> Value {
    open_platform_login_window_impl(&app, &state, "汽水音乐登录", "https://music.douyin.com/")
}

#[tauri::command]
pub fn close_platform_login_window(app: AppHandle) -> Value {
    if let Some(win) = app.get_webview_window(PLATFORM_LOGIN_LABEL) {
        let _ = win.close();
    }
    json!({ "ok": true })
}
