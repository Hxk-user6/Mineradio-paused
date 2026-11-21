use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

pub const DEFAULT_APP_NAME: &str = "MineradioX";
pub const DEFAULT_APP_VERSION: &str = "2.2.0";
pub const CACHE_SETTINGS_FILE: &str = "cache-settings.json";
pub const CURRENT_FX_AUTOSAVE_FILE: &str = "current-fx-autosave.json";
pub const CURRENT_FX_AUTOSAVE_MAX_BYTES: usize = 12 * 1024 * 1024;
pub const LYRIC_CACHE_VERSION: u32 = 1;
pub const LYRIC_CACHE_MAX_BYTES: u64 = 96 * 1024 * 1024;
pub const LYRIC_CACHE_ENTRY_MAX_BYTES: usize = 1024 * 1024;
pub const DEFAULT_BEATMAP_CACHE_DIR: &str = r"D:\MineradioCache\beatmaps";
pub const BEATMAP_CACHE_VERSION: u32 = 1;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// 应用程序核心路径与运行时配置元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub app_root: PathBuf,
    pub user_data_dir: PathBuf,
    pub runtime_name: String,
    pub app_version: String,
    pub update_manifest: String,
    pub beat_cache_dir_override: Option<PathBuf>,
    pub cache_settings_file: PathBuf,
    pub current_fx_autosave_file: PathBuf,
    pub listen_sync_file: PathBuf,
    pub login_easter_egg_gate_file: PathBuf,
    pub local_library_file: PathBuf,
    pub builtin_playlists_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettings {
    pub version: u32,
    pub root_path: String,
    pub lyrics_path: String,
    pub chromium_path: String,
    pub beatmaps_path: String,
    pub native_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettingsDetail {
    pub root_path: String,
    pub lyrics_path: String,
    pub chromium_path: String,
    pub active_chromium_path: String,
    pub beatmaps_path: String,
    pub active_beatmaps_path: String,
    pub native_path: String,
    pub active_native_path: String,
    pub wallpaper_engine_path: String,
    pub active_wallpaper_engine_path: String,
    pub user_data_path: String,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CacheUsage {
    pub lyrics_bytes: u64,
    pub chromium_bytes: u64,
    pub beatmaps_bytes: u64,
    pub wallpaper_engine_bytes: u64,
    pub user_data_bytes: u64,
    pub total_managed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettingsSnapshot {
    pub ok: bool,
    pub settings: CacheSettingsDetail,
    pub usage: CacheUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricCacheRecord {
    pub version: u32,
    pub cached_at: u64,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricCacheReadResult {
    pub ok: bool,
    pub hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatCacheRootInfo {
    pub dir: PathBuf,
    pub root: String,
    pub drive: String,
    pub allowed: bool,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapCacheMeta {
    pub provider: String,
    pub title: String,
    pub artist: String,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapCacheEntry {
    pub v: u32,
    pub key: String,
    pub saved_at: u64,
    pub meta: BeatmapCacheMeta,
    pub map: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapCacheWriteResult {
    pub ok: bool,
    pub key: String,
    pub saved_at: u64,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeatCacheError {
    CDriveDisabled(BeatCacheRootInfo),
    DriveUnavailable(BeatCacheRootInfo),
    InvalidPayload,
    InvalidKey,
    Io(String),
}

impl BeatCacheError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CDriveDisabled(_) => "BEAT_CACHE_ON_C_DRIVE_DISABLED",
            Self::DriveUnavailable(_) => "BEAT_CACHE_DRIVE_UNAVAILABLE",
            Self::InvalidPayload => "INVALID_BEATMAP_CACHE_PAYLOAD",
            Self::InvalidKey => "INVALID_BEATMAP_CACHE_KEY",
            Self::Io(_) => "BEAT_CACHE_IO_FAILED",
        }
    }

    pub fn info(&self) -> Option<&BeatCacheRootInfo> {
        match self {
            Self::CDriveDisabled(info) | Self::DriveUnavailable(info) => Some(info),
            _ => None,
        }
    }
}

impl std::fmt::Display for BeatCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CDriveDisabled(_) => write!(f, "BEAT_CACHE_ON_C_DRIVE_DISABLED"),
            Self::DriveUnavailable(_) => write!(f, "BEAT_CACHE_DRIVE_UNAVAILABLE"),
            Self::InvalidPayload => write!(f, "INVALID_BEATMAP_CACHE_PAYLOAD"),
            Self::InvalidKey => write!(f, "INVALID_BEATMAP_CACHE_KEY"),
            Self::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for BeatCacheError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPage {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct ConfigManager {
    paths: AppPaths,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_package_json(app_root: &Path) -> serde_json::Value {
    let pkg_path = app_root.join("package.json");
    fs::read_to_string(pkg_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
}

fn find_app_root() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.clone());
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(5) {
            candidates.push(ancestor.to_path_buf());
        }
    }
    for dir in &candidates {
        if dir.join("package.json").is_file() || dir.join("public").is_dir() {
            return dir.clone();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn is_absolute_path_str(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    if Path::new(trimmed).is_absolute() {
        return true;
    }
    // 支持在非 Windows 宿主环境下的 Windows 盘符路径测试（如 C:\...）
    let bytes = trimmed.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn resolve_user_data_dir(app_root: &Path, runtime_name: &str) -> PathBuf {
    // 1. 启动 QA 隐藏模式覆盖
    if non_empty_env("MINERADIO_STARTUP_QA_HIDDEN").as_deref() == Some("1") {
        if let Some(qa_dir) = non_empty_env("MINERADIO_STARTUP_QA_USER_DATA") {
            if is_absolute_path_str(&qa_dir) {
                return PathBuf::from(qa_dir);
            }
        }
    }
    // 2. 显式环境变量覆盖
    if let Some(user_data) = non_empty_env("MINERADIO_USER_DATA_DIR") {
        let p = PathBuf::from(&user_data);
        if p.is_absolute() {
            return p;
        }
        return app_root.join(p);
    }
    // 3. 系统 AppData 目录 + runtime_name
    if let Some(base) = dirs::data_dir() {
        return base.join(runtime_name);
    }
    app_root.join(".user-data")
}

fn build_app_paths(app_root: PathBuf, user_data_dir: PathBuf) -> AppPaths {
    let pkg = read_package_json(&app_root);
    let pkg_runtime_name = pkg
        .get("mineradio")
        .and_then(|m| m.get("runtimeName"))
        .and_then(|v| v.as_str())
        .or_else(|| pkg.get("productName").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_APP_NAME);
    let runtime_name =
        non_empty_env("MINERADIO_RUNTIME_NAME").unwrap_or_else(|| pkg_runtime_name.to_string());

    let pkg_version = pkg
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_APP_VERSION);
    let app_version =
        non_empty_env("MINERADIO_VERSION").unwrap_or_else(|| pkg_version.to_string());

    let update_manifest = non_empty_env("MINERADIO_UPDATE_MANIFEST")
        .or_else(|| non_empty_env("MINERADIO_UPDATE_MANIFEST_URL"))
        .or_else(|| non_empty_env("MINERADIO_UPDATE_MANIFEST_FILE"))
        .unwrap_or_default();

    let beat_cache_dir_override =
        non_empty_env("MINERADIO_BEAT_CACHE_DIR").map(PathBuf::from);

    let cache_settings_file = user_data_dir.join(CACHE_SETTINGS_FILE);
    let current_fx_autosave_file = user_data_dir.join(CURRENT_FX_AUTOSAVE_FILE);
    let listen_sync_file = non_empty_env("MINERADIO_LISTEN_SYNC_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_data_dir.join("listen-sync-journal.json"));
    let login_easter_egg_gate_file = non_empty_env("MINERADIO_LOGIN_EASTER_EGG_GATE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_data_dir.join("login-easter-egg.json"));
    let local_library_file = user_data_dir.join("local-music-library.json");
    let builtin_playlists_file = user_data_dir.join("built-in-playlists.json");

    AppPaths {
        app_root,
        user_data_dir,
        runtime_name,
        app_version,
        update_manifest,
        beat_cache_dir_override,
        cache_settings_file,
        current_fx_autosave_file,
        listen_sync_file,
        login_easter_egg_gate_file,
        local_library_file,
        builtin_playlists_file,
    }
}

impl ConfigManager {
    /// 自动探测应用根目录与用户数据目录并应用环境变量覆盖。
    pub fn discover() -> Self {
        let app_root = find_app_root();
        let pkg = read_package_json(&app_root);
        let pkg_runtime_name = pkg
            .get("mineradio")
            .and_then(|m| m.get("runtimeName"))
            .and_then(|v| v.as_str())
            .or_else(|| pkg.get("productName").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_APP_NAME);
        let runtime_name =
            non_empty_env("MINERADIO_RUNTIME_NAME").unwrap_or_else(|| pkg_runtime_name.to_string());
        let user_data_dir = resolve_user_data_dir(&app_root, &runtime_name);
        Self {
            paths: build_app_paths(app_root, user_data_dir),
        }
    }

    /// 使用指定的应用根目录与用户数据目录构建配置管理器（用于测试或嵌入场景）。
    pub fn with_user_data_dir(
        app_root: impl AsRef<Path>,
        user_data_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            paths: build_app_paths(
                app_root.as_ref().to_path_buf(),
                user_data_dir.as_ref().to_path_buf(),
            ),
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    fn default_cache_root_path(&self) -> PathBuf {
        #[cfg(windows)]
        {
            let d_drive = Path::new(r"D:\");
            if d_drive.exists() {
                return d_drive.join("MineradioCache");
            }
        }
        self.paths.user_data_dir.join("cache")
    }

    fn normalize_cache_root_path(&self, raw: Option<&str>) -> PathBuf {
        let candidate = raw.map(str::trim).unwrap_or("");
        if candidate.is_empty() {
            return self.default_cache_root_path();
        }
        let path = PathBuf::from(candidate);
        if path.is_absolute() || has_windows_drive(candidate) {
            path
        } else {
            self.paths.user_data_dir.join(path)
        }
    }

    fn normalize_cache_settings(&self, root_opt: Option<&str>) -> CacheSettings {
        let root = self.normalize_cache_root_path(root_opt);
        CacheSettings {
            version: 1,
            root_path: root.to_string_lossy().into_owned(),
            lyrics_path: root.join("lyrics").to_string_lossy().into_owned(),
            chromium_path: root.join("chromium").to_string_lossy().into_owned(),
            beatmaps_path: root.join("beatmaps").to_string_lossy().into_owned(),
            native_path: root.join("native-helper-temp").to_string_lossy().into_owned(),
        }
    }

    fn read_raw_cache_settings(&self) -> CacheSettings {
        if let Ok(raw) = fs::read_to_string(&self.paths.cache_settings_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                let root_opt = val.get("rootPath").and_then(|v| v.as_str());
                return self.normalize_cache_settings(root_opt);
            }
        }
        self.normalize_cache_settings(None)
    }

    fn ensure_cache_directories(&self, settings: &CacheSettings) -> CacheSettings {
        let try_create = |s: &CacheSettings| -> io::Result<()> {
            fs::create_dir_all(&s.lyrics_path)?;
            fs::create_dir_all(&s.chromium_path)?;
            fs::create_dir_all(Path::new(&s.chromium_path).join(&self.paths.runtime_name))?;
            fs::create_dir_all(&s.beatmaps_path)?;
            fs::create_dir_all(&s.native_path)?;
            Ok(())
        };
        if try_create(settings).is_ok() {
            return settings.clone();
        }
        let fallback_root = self.paths.user_data_dir.join("cache-fallback");
        let fallback = self.normalize_cache_settings(fallback_root.to_str());
        let _ = try_create(&fallback);
        fallback
    }

    fn build_cache_snapshot(
        &self,
        settings: &CacheSettings,
        restart_required_top: Option<bool>,
    ) -> CacheSettingsSnapshot {
        let desired_chromium = Path::new(&settings.chromium_path).join(&self.paths.runtime_name);
        let active_chromium = desired_chromium.clone();
        let active_beatmaps = self
            .paths
            .beat_cache_dir_override
            .clone()
            .unwrap_or_else(|| PathBuf::from(&settings.beatmaps_path));
        let active_native = PathBuf::from(&settings.native_path);
        let wallpaper_engine_path =
            Path::new(&settings.native_path).join("wallpaper-engine-muted-package-cache");
        let active_wallpaper_engine_path =
            active_native.join("wallpaper-engine-muted-package-cache");

        let lyrics_bytes = directory_usage_bytes(Path::new(&settings.lyrics_path));
        let chromium_bytes = directory_usage_bytes(&active_chromium);
        let beatmaps_bytes = directory_usage_bytes(&active_beatmaps);
        let wallpaper_engine_bytes = directory_usage_bytes(&active_wallpaper_engine_path);
        let user_data_bytes = directory_usage_bytes(&self.paths.user_data_dir);

        let beatmaps_restart =
            Path::new(&settings.beatmaps_path) != active_beatmaps.as_path();
        let restart_required = beatmaps_restart;

        CacheSettingsSnapshot {
            ok: true,
            settings: CacheSettingsDetail {
                root_path: settings.root_path.clone(),
                lyrics_path: settings.lyrics_path.clone(),
                chromium_path: settings.chromium_path.clone(),
                active_chromium_path: active_chromium.to_string_lossy().into_owned(),
                beatmaps_path: settings.beatmaps_path.clone(),
                active_beatmaps_path: active_beatmaps.to_string_lossy().into_owned(),
                native_path: settings.native_path.clone(),
                active_native_path: active_native.to_string_lossy().into_owned(),
                wallpaper_engine_path: wallpaper_engine_path.to_string_lossy().into_owned(),
                active_wallpaper_engine_path: active_wallpaper_engine_path
                    .to_string_lossy()
                    .into_owned(),
                user_data_path: self.paths.user_data_dir.to_string_lossy().into_owned(),
                restart_required,
            },
            usage: CacheUsage {
                lyrics_bytes,
                chromium_bytes,
                beatmaps_bytes,
                wallpaper_engine_bytes,
                user_data_bytes,
                total_managed_bytes: lyrics_bytes
                    + chromium_bytes
                    + beatmaps_bytes
                    + wallpaper_engine_bytes,
            },
            restart_required: restart_required_top,
        }
    }

    /// 读取当前缓存目录配置与磁盘用量快照。
    pub fn get_cache_settings(&self) -> CacheSettingsSnapshot {
        let raw = self.read_raw_cache_settings();
        let active = self.ensure_cache_directories(&raw);
        self.build_cache_snapshot(&active, None)
    }

    /// 更新缓存根目录，验证目录可写并原子持久化到 `cache-settings.json`。
    pub fn set_cache_settings(
        &self,
        root_dir: impl AsRef<Path>,
    ) -> Result<CacheSettingsSnapshot, String> {
        let requested = root_dir.as_ref().to_string_lossy();
        let normalized = self.normalize_cache_settings(Some(requested.as_ref()));
        let root_path = PathBuf::from(&normalized.root_path);
        fs::create_dir_all(&root_path)
            .map_err(|e| format!("CACHE_SETTINGS_WRITE_FAILED:{e}"))?;
        // 校验可写性：尝试创建并移除探测文件
        let probe = root_path.join(format!(
            ".mineradio-write-probe-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        File::create(&probe)
            .and_then(|mut f| f.flush())
            .map_err(|e| format!("CACHE_SETTINGS_NOT_WRITABLE:{e}"))?;
        let _ = fs::remove_file(&probe);

        write_json_pretty_atomic(&self.paths.cache_settings_file, &normalized)
            .map_err(|e| format!("CACHE_SETTINGS_WRITE_FAILED:{e}"))?;
        let active = self.ensure_cache_directories(&normalized);
        let mut snapshot = self.build_cache_snapshot(&active, None);
        snapshot.restart_required = Some(snapshot.settings.restart_required);
        Ok(snapshot)
    }

    /// 读取 FX 自动保存文件；不存在、超限或非 JSON 对象时返回 `None`。
    pub fn read_current_fx_autosave(&self) -> Option<serde_json::Value> {
        let meta = fs::metadata(&self.paths.current_fx_autosave_file).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() as usize > CURRENT_FX_AUTOSAVE_MAX_BYTES
        {
            return None;
        }
        let raw = fs::read_to_string(&self.paths.current_fx_autosave_file).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if parsed.is_object() {
            Some(parsed)
        } else {
            None
        }
    }

    /// 原子保存 FX 自动保存数据，强制校验为 JSON 对象且体积不超过 12 MiB。
    pub fn save_current_fx_autosave(&self, payload: &serde_json::Value) -> Result<(), String> {
        if !payload.is_object() {
            return Err("INVALID_AUTOSAVE_PAYLOAD".to_string());
        }
        let text = serde_json::to_string(payload)
            .map_err(|e| format!("AUTOSAVE_SERIALIZE_FAILED:{e}"))?;
        if text.len() > CURRENT_FX_AUTOSAVE_MAX_BYTES {
            return Err("AUTOSAVE_PAYLOAD_TOO_LARGE".to_string());
        }
        write_text_atomic(&self.paths.current_fx_autosave_file, &text)
            .map_err(|e| format!("AUTOSAVE_WRITE_FAILED:{e}"))
    }

    fn lyric_cache_file_path(&self, key: &str) -> PathBuf {
        let settings = self.read_raw_cache_settings();
        let digest = hex::encode(Sha256::digest(key.as_bytes()));
        PathBuf::from(settings.lyrics_path).join(format!("{digest}.json"))
    }

    /// 读取歌词缓存（版本 1，单条上限 1 MiB）。
    pub fn read_lyric_cache(&self, key: &str) -> LyricCacheReadResult {
        if key.is_empty() {
            return LyricCacheReadResult {
                ok: true,
                hit: false,
                payload: None,
                cached_at: None,
                error: None,
            };
        }
        let file = self.lyric_cache_file_path(key);
        if !file.exists() {
            return LyricCacheReadResult {
                ok: true,
                hit: false,
                payload: None,
                cached_at: None,
                error: None,
            };
        }
        let meta = match fs::metadata(&file) {
            Ok(m) => m,
            Err(e) => {
                return LyricCacheReadResult {
                    ok: false,
                    hit: false,
                    payload: None,
                    cached_at: None,
                    error: Some(e.to_string()),
                }
            }
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() as usize > LYRIC_CACHE_ENTRY_MAX_BYTES {
            return LyricCacheReadResult {
                ok: true,
                hit: false,
                payload: None,
                cached_at: None,
                error: None,
            };
        }
        let raw = match fs::read_to_string(&file) {
            Ok(s) => s,
            Err(e) => {
                return LyricCacheReadResult {
                    ok: false,
                    hit: false,
                    payload: None,
                    cached_at: None,
                    error: Some(e.to_string()),
                }
            }
        };
        let record: LyricCacheRecord = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(_) => {
                return LyricCacheReadResult {
                    ok: true,
                    hit: false,
                    payload: None,
                    cached_at: None,
                    error: None,
                }
            }
        };
        if record.version != LYRIC_CACHE_VERSION || !record.payload.is_object() {
            return LyricCacheReadResult {
                ok: true,
                hit: false,
                payload: None,
                cached_at: None,
                error: None,
            };
        }
        if let Ok(opened) = fs::OpenOptions::new().write(true).open(&file) {
            let _ = opened.set_modified(SystemTime::now());
        }
        LyricCacheReadResult {
            ok: true,
            hit: true,
            payload: Some(record.payload),
            cached_at: Some(record.cached_at),
            error: None,
        }
    }

    /// 写入歌词缓存（版本 1，单条上限 1 MiB），并在写入后裁剪超出 96 MiB 总量限制的旧条目。
    pub fn write_lyric_cache(
        &self,
        key: &str,
        payload: &serde_json::Value,
    ) -> Result<(), String> {
        if key.is_empty() || !payload.is_object() {
            return Err("INVALID_LYRIC_CACHE_PAYLOAD".to_string());
        }
        let record = LyricCacheRecord {
            version: LYRIC_CACHE_VERSION,
            cached_at: now_millis(),
            payload: payload.clone(),
        };
        let text = serde_json::to_string(&record)
            .map_err(|e| format!("LYRIC_CACHE_SERIALIZE_FAILED:{e}"))?;
        if text.len() > LYRIC_CACHE_ENTRY_MAX_BYTES {
            return Err("LYRIC_CACHE_ENTRY_TOO_LARGE".to_string());
        }
        let file = self.lyric_cache_file_path(key);
        write_text_atomic(&file, &text).map_err(|e| format!("LYRIC_CACHE_WRITE_FAILED:{e}"))?;
        if let Some(parent) = file.parent() {
            prune_lyric_cache(parent);
        }
        Ok(())
    }

    fn effective_beat_cache_dir(&self) -> PathBuf {
        if let Some(ref override_dir) = self.paths.beat_cache_dir_override {
            return override_dir.clone();
        }
        let settings = self.read_raw_cache_settings();
        PathBuf::from(settings.beatmaps_path)
    }

    /// 返回 Beatmap 缓存目录及 Windows C 盘禁用保护检测结果。
    pub fn beat_cache_root_info(&self) -> BeatCacheRootInfo {
        let raw_dir = self.effective_beat_cache_dir();
        let dir_str = raw_dir.to_string_lossy();
        if let Some((drive, root)) = extract_windows_drive(&dir_str) {
            let allowed = !drive.eq_ignore_ascii_case("C:");
            let available = allowed && Path::new(&root).exists();
            return BeatCacheRootInfo {
                dir: raw_dir,
                root,
                drive,
                allowed,
                available,
            };
        }
        let resolved = if raw_dir.is_absolute() {
            raw_dir
        } else {
            self.paths.user_data_dir.join(raw_dir)
        };
        let root = resolved
            .ancestors()
            .last()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let available = Path::new(&root).exists();
        BeatCacheRootInfo {
            dir: resolved,
            root,
            drive: String::new(),
            allowed: true,
            available,
        }
    }

    fn ensure_beatmap_cache_dir(&self) -> Result<PathBuf, BeatCacheError> {
        let info = self.beat_cache_root_info();
        if !info.allowed {
            return Err(BeatCacheError::CDriveDisabled(info));
        }
        if !info.available {
            return Err(BeatCacheError::DriveUnavailable(info));
        }
        fs::create_dir_all(&info.dir).map_err(|e| BeatCacheError::Io(e.to_string()))?;
        Ok(info.dir)
    }

    fn safe_beatmap_cache_file(&self, key: &str) -> Result<Option<PathBuf>, BeatCacheError> {
        let dir = self.ensure_beatmap_cache_dir()?;
        let Some(sanitized) = sanitize_cache_key(key) else {
            return Ok(None);
        };
        Ok(Some(dir.join(format!("{sanitized}.json"))))
    }

    /// 读取 Beatmap 缓存，执行 C 盘禁用检查与路径安全校验。
    pub fn read_beatmap_cache(
        &self,
        key: &str,
    ) -> Result<Option<BeatmapCacheEntry>, BeatCacheError> {
        let Some(file) = self.safe_beatmap_cache_file(key)? else {
            return Ok(None);
        };
        if !file.is_file() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&file).map_err(|e| BeatCacheError::Io(e.to_string()))?;
        let entry: BeatmapCacheEntry = match serde_json::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(_) => return Ok(None),
        };
        if entry.map.is_object() {
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    /// 写入 Beatmap 缓存（原子写入），执行 C 盘禁用检查与 payload 校验。
    pub fn write_beatmap_cache(
        &self,
        body: &serde_json::Value,
    ) -> Result<BeatmapCacheWriteResult, BeatCacheError> {
        let key = body
            .get("key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        let map = body.get("map").filter(|v| v.is_object());
        if key.is_empty() || map.is_none() {
            return Err(BeatCacheError::InvalidPayload);
        }
        let Some(file) = self.safe_beatmap_cache_file(key)? else {
            return Err(BeatCacheError::InvalidKey);
        };
        let entry = BeatmapCacheEntry {
            v: BEATMAP_CACHE_VERSION,
            key: key.to_string(),
            saved_at: now_millis(),
            meta: BeatmapCacheMeta {
                provider: truncate_chars(
                    body.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
                    32,
                ),
                title: truncate_chars(
                    body.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    160,
                ),
                artist: truncate_chars(
                    body.get("artist").and_then(|v| v.as_str()).unwrap_or(""),
                    160,
                ),
                mode: truncate_chars(
                    body.get("mode").and_then(|v| v.as_str()).unwrap_or("mr"),
                    32,
                ),
            },
            map: map.cloned().unwrap_or_default(),
        };
        write_json_atomic(&file, &entry).map_err(|e| BeatCacheError::Io(e.to_string()))?;
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        Ok(BeatmapCacheWriteResult {
            ok: true,
            key: entry.key,
            saved_at: entry.saved_at,
            dir,
        })
    }
}

fn has_windows_drive(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn extract_windows_drive(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = format!("{}:", (bytes[0] as char).to_ascii_uppercase());
        let root = format!("{drive}\\");
        return Some((drive, root));
    }
    None
}

fn directory_usage_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                total = total.saturating_add(directory_usage_bytes(&path));
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn prune_lyric_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let is_hex_json = name_str.len() == 69
            && name_str.ends_with(".json")
            && name_str[..64].bytes().all(|b| b.is_ascii_hexdigit());
        if !is_hex_json {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
                files.push((path, meta.len(), mtime));
            }
        }
    }
    let mut total: u64 = files.iter().map(|(_, s, _)| *s).sum();
    if total <= LYRIC_CACHE_MAX_BYTES {
        return;
    }
    files.sort_by_key(|(_, _, t)| *t);
    for (path, size, _) in files {
        if total <= LYRIC_CACHE_MAX_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// 原子写入纯文本文件：创建父目录 -> 写入同目录唯一临时文件 -> 重命名覆盖。
pub fn write_text_atomic(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let seq = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".{file_name}.{}.{}.tmp", std::process::id(), seq);
    let tmp_path = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    };

    let write_res = (|| -> io::Result<()> {
        let mut f = File::create(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// 原子写入紧凑格式 JSON 文件。
pub fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_text_atomic(path, &json)
}

/// 原子写入缩进格式 JSON 文件。
pub fn write_json_pretty_atomic<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_text_atomic(path, &json)
}

/// 规范化并清洗缓存 key，返回安全的 `{label}-{sha256}` 文件名基干；空 key 或超过 240 字节返回 `None`。
pub fn sanitize_cache_key(key: &str) -> Option<String> {
    let raw = key.trim();
    if raw.is_empty() || raw.chars().count() > 240 {
        return None;
    }
    let mut cleaned = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-' {
            cleaned.push(ch);
            last_underscore = false;
        } else if !last_underscore {
            cleaned.push('_');
            last_underscore = true;
        }
    }
    let trimmed = cleaned.trim_matches('_');
    let mut label: String = trimmed.chars().take(48).collect();
    if label.is_empty() || label == "." || label == ".." {
        label = "beatmap".to_string();
    }
    let digest = hex::encode(Sha256::digest(raw.as_bytes()));
    Some(format!("{label}-{digest}"))
}

/// 规范化版本号字符串，去除前导 `v`/`V` 以及 `+build`/`-prerelease` 后缀。
pub fn normalize_version(value: &str) -> String {
    let mut s = value.trim();
    if s.starts_with('v') || s.starts_with('V') {
        s = &s[1..];
    }
    if let Some(pos) = s.find('+') {
        s = &s[..pos];
    }
    if let Some(pos) = s.find('-') {
        if pos + 1 < s.len() {
            s = &s[..pos];
        }
    }
    s.to_string()
}

fn parse_version_parts(version: &str) -> Vec<u64> {
    let norm = normalize_version(version);
    norm.split('.')
        .map(|part| {
            let digits: String = part
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// 按语义化数字段比较两个版本号，返回 `1` (`a > b`)、`0` (`a == b`) 或 `-1` (`a < b`)。
pub fn compare_versions(a: &str, b: &str) -> i32 {
    let aa = parse_version_parts(a);
    let bb = parse_version_parts(b);
    let len = aa.len().max(bb.len()).max(3);
    for i in 0..len {
        let left = aa.get(i).copied().unwrap_or(0);
        let right = bb.get(i).copied().unwrap_or(0);
        if left > right {
            return 1;
        }
        if left < right {
            return -1;
        }
    }
    0
}

/// 验证外部更新链接，仅允许长度不超过 2048 的合法 `https://` URL。
pub fn safe_external_update_url(value: &str) -> Option<String> {
    let raw = value.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return None;
    }
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    Some(parsed.to_string())
}

fn clean_release_line(line: &str) -> String {
    static HEADING_RE: OnceLock<Regex> = OnceLock::new();
    static BULLET_RE: OnceLock<Regex> = OnceLock::new();
    static NUMBERED_RE: OnceLock<Regex> = OnceLock::new();

    let heading = HEADING_RE.get_or_init(|| Regex::new(r"^\s*#{1,6}\s*").unwrap());
    let bullet = BULLET_RE.get_or_init(|| Regex::new(r"^\s*[-*]\s+").unwrap());
    let numbered = NUMBERED_RE.get_or_init(|| Regex::new(r"^\s*\d+[.)]\s+").unwrap());

    let s = heading.replace(line, "");
    let s = bullet.replace(&s, "");
    let s = numbered.replace(&s, "");
    s.replace("**", "").replace('`', "").trim().to_string()
}

fn normalize_update_download_pages(raw_pages: Vec<(String, String)>) -> Vec<DownloadPage> {
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    for (index, (raw_label, raw_url)) in raw_pages.into_iter().enumerate() {
        let Some(url) = safe_external_update_url(&raw_url) else {
            continue;
        };
        if !seen.insert(url.clone()) {
            continue;
        }
        let cleaned = clean_release_line(&raw_label)
            .replace(['<', '>', '|'], "")
            .chars()
            .take(24)
            .collect::<String>()
            .trim()
            .to_string();
        let label = if cleaned.is_empty() {
            format!("下载线路 {}", index + 1)
        } else {
            cleaned
        };
        pages.push(DownloadPage { label, url });
        if pages.len() >= 6 {
            break;
        }
    }
    pages
}

/// 从 Release 正文中提取网盘/下载页链接（优先解析隐藏注释，其次解析可见行）。
pub fn extract_release_download_pages(body: &str) -> Vec<DownloadPage> {
    static HIDDEN_RE: OnceLock<Regex> = OnceLock::new();
    static VISIBLE_RE: OnceLock<Regex> = OnceLock::new();

    let hidden_re = HIDDEN_RE.get_or_init(|| {
        Regex::new(
            r"(?i)<!--\s*mineradio-download-page\s*:\s*(?:([^|<>\r\n]{1,32})\s*\|\s*)?(https://[^\s<>]+)\s*-->",
        )
        .unwrap()
    });
    let visible_re = VISIBLE_RE.get_or_init(|| {
        Regex::new(
            r"(?im)^\s*[-*]?\s*(夸克盘|百度(?:云|网盘)|蓝奏(?:云|网盘)|网盘|下载(?:地址|页面|链接)?)\s*[:：]\s*(?:\[[^\]]*\]\()?(https://[^\s<>)\]]+)",
        )
        .unwrap()
    });

    let mut hidden_pages = Vec::new();
    for caps in hidden_re.captures_iter(body) {
        let label = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        let url = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
        hidden_pages.push((label, url));
    }
    if !hidden_pages.is_empty() {
        return normalize_update_download_pages(hidden_pages);
    }

    let mut visible_pages = Vec::new();
    for caps in visible_re.captures_iter(body) {
        let label = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let url = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
        visible_pages.push((label, url));
    }
    normalize_update_download_pages(visible_pages)
}

/// 从 Release 正文中提取精简版更新日志要点（最多 4 条）。
pub fn extract_release_notes(body: &str) -> Vec<String> {
    static COMMENT_RE: OnceLock<Regex> = OnceLock::new();
    static HEADING_SKIP_RE: OnceLock<Regex> = OnceLock::new();
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    static DOWNLOAD_PREFIX_RE: OnceLock<Regex> = OnceLock::new();

    let comment_re = COMMENT_RE.get_or_init(|| Regex::new(r"<!--[\s\S]*?-->").unwrap());
    let heading_skip_re = HEADING_SKIP_RE.get_or_init(|| {
        Regex::new(r"(?i)^(what'?s changed|changes|changelog|full changelog|更新日志|更新内容)$")
            .unwrap()
    });
    let url_re = URL_RE.get_or_init(|| Regex::new(r"(?i)https?://").unwrap());
    let download_prefix_re = DOWNLOAD_PREFIX_RE.get_or_init(|| {
        Regex::new(r"^(下载|网盘|夸克盘|百度(?:云|网盘)|蓝奏(?:云|网盘)|安装包)").unwrap()
    });

    let mut notes = Vec::new();
    for line in body.split('\n').map(|l| l.trim_end_matches('\r')) {
        if comment_re.is_match(line) {
            continue;
        }
        let text = clean_release_line(line);
        if text.is_empty()
            || heading_skip_re.is_match(&text)
            || url_re.is_match(&text)
            || download_prefix_re.is_match(&text)
            || text.chars().count() > 72
        {
            continue;
        }
        notes.push(text);
        if notes.len() >= 4 {
            break;
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .map(|&k| (k, std::env::var(k).ok()))
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn env_overrides_and_discover_priority() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&[
            "MINERADIO_STARTUP_QA_HIDDEN",
            "MINERADIO_STARTUP_QA_USER_DATA",
            "MINERADIO_USER_DATA_DIR",
            "MINERADIO_BEAT_CACHE_DIR",
            "MINERADIO_UPDATE_MANIFEST",
            "MINERADIO_RUNTIME_NAME",
            "MINERADIO_VERSION",
        ]);

        let tmp = TempDir::new().unwrap();
        let qa_dir = tmp.path().join("qa-user-data");
        let normal_dir = tmp.path().join("normal-user-data");
        let beat_dir = tmp.path().join("beats");

        std::env::set_var("MINERADIO_STARTUP_QA_HIDDEN", "1");
        std::env::set_var("MINERADIO_STARTUP_QA_USER_DATA", &qa_dir);
        std::env::set_var("MINERADIO_USER_DATA_DIR", &normal_dir);
        std::env::set_var("MINERADIO_BEAT_CACHE_DIR", &beat_dir);
        std::env::set_var("MINERADIO_UPDATE_MANIFEST", "https://example.com/manifest.json");
        std::env::set_var("MINERADIO_RUNTIME_NAME", "MineradioTest");
        std::env::set_var("MINERADIO_VERSION", "9.9.1");

        let cfg = ConfigManager::discover();
        assert_eq!(cfg.paths().user_data_dir, qa_dir);
        assert_eq!(cfg.paths().runtime_name, "MineradioTest");
        assert_eq!(cfg.paths().app_version, "9.9.1");
        assert_eq!(cfg.paths().update_manifest, "https://example.com/manifest.json");
        assert_eq!(cfg.paths().beat_cache_dir_override, Some(beat_dir));

        // 关闭 QA_HIDDEN 后应回退到 MINERADIO_USER_DATA_DIR
        std::env::set_var("MINERADIO_STARTUP_QA_HIDDEN", "0");
        let cfg2 = ConfigManager::discover();
        assert_eq!(cfg2.paths().user_data_dir, normal_dir);
    }

    #[test]
    fn cache_settings_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cfg = ConfigManager::with_user_data_dir(tmp.path(), tmp.path().join("ud"));
        let initial = cfg.get_cache_settings();
        assert!(initial.ok);
        assert!(Path::new(&initial.settings.lyrics_path).is_dir());

        let custom_root = tmp.path().join("custom-cache");
        let updated = cfg.set_cache_settings(&custom_root).unwrap();
        assert!(updated.ok);
        assert_eq!(updated.settings.root_path, custom_root.to_string_lossy());
        assert!(cfg.paths().cache_settings_file.is_file());

        let reloaded = cfg.get_cache_settings();
        assert_eq!(reloaded.settings.root_path, custom_root.to_string_lossy());
    }

    #[test]
    fn fx_autosave_roundtrip_and_limit() {
        let tmp = TempDir::new().unwrap();
        let cfg = ConfigManager::with_user_data_dir(tmp.path(), tmp.path().join("ud"));
        assert!(cfg.read_current_fx_autosave().is_none());

        // 非 Object 应拒绝
        assert_eq!(
            cfg.save_current_fx_autosave(&json!([1, 2, 3])).unwrap_err(),
            "INVALID_AUTOSAVE_PAYLOAD"
        );

        // 正常保存与读取
        let payload = json!({ "preset": "warm", "gain": 0.75 });
        cfg.save_current_fx_autosave(&payload).unwrap();
        assert_eq!(cfg.read_current_fx_autosave(), Some(payload));

        // 超过 12 MiB 应拒绝
        let huge = "a".repeat(CURRENT_FX_AUTOSAVE_MAX_BYTES + 10);
        let err = cfg
            .save_current_fx_autosave(&json!({ "blob": huge }))
            .unwrap_err();
        assert_eq!(err, "AUTOSAVE_PAYLOAD_TOO_LARGE");
    }

    #[test]
    fn lyric_cache_version_and_size_limit() {
        let tmp = TempDir::new().unwrap();
        let cfg = ConfigManager::with_user_data_dir(tmp.path(), tmp.path().join("ud"));

        let miss = cfg.read_lyric_cache("netease:123");
        assert!(miss.ok && !miss.hit);

        cfg.write_lyric_cache("netease:123", &json!({ "lrc": "[00:01.00]hello" }))
            .unwrap();
        let hit = cfg.read_lyric_cache("netease:123");
        assert!(hit.ok && hit.hit);
        assert_eq!(hit.payload.unwrap()["lrc"], "[00:01.00]hello");

        let oversized = "x".repeat(LYRIC_CACHE_ENTRY_MAX_BYTES + 10);
        let err = cfg
            .write_lyric_cache("netease:big", &json!({ "lrc": oversized }))
            .unwrap_err();
        assert_eq!(err, "LYRIC_CACHE_ENTRY_TOO_LARGE");
    }

    #[test]
    fn beatmap_cache_c_drive_protection_and_roundtrip() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture(&["MINERADIO_BEAT_CACHE_DIR"]);

        let tmp = TempDir::new().unwrap();
        // C 盘禁用保护测试
        std::env::set_var("MINERADIO_BEAT_CACHE_DIR", r"C:\MineradioCache\beatmaps");
        let cfg_c = ConfigManager::with_user_data_dir(tmp.path(), tmp.path().join("ud-c"));
        let info = cfg_c.beat_cache_root_info();
        assert_eq!(info.drive, "C:");
        assert!(!info.allowed);
        let write_err = cfg_c
            .write_beatmap_cache(&json!({ "key": "netease:1", "map": { "bpm": 128 } }))
            .unwrap_err();
        assert_eq!(write_err.code(), "BEAT_CACHE_ON_C_DRIVE_DISABLED");

        // 正常目录读写测试
        let beat_dir = tmp.path().join("beatmaps");
        std::env::set_var("MINERADIO_BEAT_CACHE_DIR", &beat_dir);
        let cfg_ok = ConfigManager::with_user_data_dir(tmp.path(), tmp.path().join("ud-ok"));
        let ok_info = cfg_ok.beat_cache_root_info();
        assert!(ok_info.allowed);
        assert!(ok_info.available);

        let res = cfg_ok
            .write_beatmap_cache(&json!({
                "key": "netease:42",
                "provider": "netease",
                "title": "Song",
                "artist": "Artist",
                "map": { "bpm": 120, "beats": [0.5, 1.0] }
            }))
            .unwrap();
        assert!(res.ok);

        let read_back = cfg_ok.read_beatmap_cache("netease:42").unwrap().unwrap();
        assert_eq!(read_back.key, "netease:42");
        assert_eq!(read_back.map["bpm"], 120);
    }

    #[test]
    fn version_url_and_release_helpers() {
        assert_eq!(normalize_version("v2.3.1-beta.1+build7"), "2.3.1");
        assert_eq!(compare_versions("v2.10.0", "2.2.9"), 1);
        assert_eq!(compare_versions("2.2.0", "v2.2"), 0);
        assert_eq!(compare_versions("1.9.9", "2.0.0"), -1);

        assert!(safe_external_update_url("http://example.com/a").is_none());
        assert_eq!(
            safe_external_update_url("https://example.com/dl"),
            Some("https://example.com/dl".to_string())
        );

        assert!(sanitize_cache_key("").is_none());
        assert!(sanitize_cache_key(&"a".repeat(241)).is_none());
        let sanitized = sanitize_cache_key("../weird:key/1").unwrap();
        assert!(!sanitized.contains('/'));
        assert!(!sanitized.contains('\\'));

        let body = r#"
## 更新日志
- 修复播放稳定性与缓存问题
- **新增** 本地曲库导入
下载：https://example.com/old
<!-- mineradio-download-page: 夸克盘 | https://pan.quark.cn/s/abc123 -->
<!-- mineradio-download-page: 备用 | http://insecure.example.com -->
"#;
        let pages = extract_release_download_pages(body);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].label, "夸克盘");
        assert_eq!(pages[0].url, "https://pan.quark.cn/s/abc123");

        let notes = extract_release_notes(body);
        assert_eq!(
            notes,
            vec!["修复播放稳定性与缓存问题", "新增 本地曲库导入"]
        );
    }
}
