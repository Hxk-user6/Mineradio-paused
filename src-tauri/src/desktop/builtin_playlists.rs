use crate::config::write_text_atomic;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const BUILT_IN_PLAYLIST_VERSION: u32 = 1;
pub const BUILT_IN_PLAYLIST_FILE: &str = "built-in-playlists.json";
pub const MAX_PLAYLISTS: usize = 100;
pub const MAX_TRACKS_PER_PLAYLIST: usize = 5000;
pub const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_TRACK_BYTES: usize = 48 * 1024;

const ALLOWED_PROVIDERS: &[&str] = &["netease", "qq", "kugou", "qishui", "local"];
const DANGEROUS_KEYS: &[&str] = &["__proto__", "prototype", "constructor"];

const TRACK_FIELDS: &[&str] = &[
    "provider",
    "source",
    "type",
    "id",
    "providerSongId",
    "provider_song_id",
    "trackId",
    "track_id",
    "mid",
    "songmid",
    "mediaMid",
    "media_mid",
    "qqId",
    "hash",
    "fileHash",
    "audioHash",
    "albumId",
    "album_id",
    "albumMid",
    "albummid",
    "albumAudioId",
    "album_audio_id",
    "mixSongId",
    "mix_song_id",
    "hqHash",
    "hq_hash",
    "sqHash",
    "sq_hash",
    "resHash",
    "res_hash",
    "name",
    "title",
    "artist",
    "album",
    "cover",
    "duration",
    "durationMs",
    "dt",
    "fee",
    "Fee",
    "playable",
    "playbackMode",
    "recommendationSource",
    "localKey",
    "localFileId",
    "localUrl",
    "localPath",
    "localMissing",
    "hasLyric",
    "lyricSource",
    "vipRequired",
    "needVip",
    "onlyVipPlayable",
    "only_vip_playable",
    "privilege",
    "Privilege",
    "mediaPrivilege",
    "media_privilege",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltInPlaylistError {
    NotFound,
    LimitReached,
    TrackInvalid,
    TrackLimitReached,
    TrackNotFound,
    IndexTooLarge,
    Io(String),
}

impl BuiltInPlaylistError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "BUILT_IN_PLAYLIST_NOT_FOUND",
            Self::LimitReached => "BUILT_IN_PLAYLIST_LIMIT_REACHED",
            Self::TrackInvalid => "BUILT_IN_PLAYLIST_TRACK_INVALID",
            Self::TrackLimitReached => "BUILT_IN_PLAYLIST_TRACK_LIMIT_REACHED",
            Self::TrackNotFound => "BUILT_IN_PLAYLIST_TRACK_NOT_FOUND",
            Self::IndexTooLarge => "BUILT_IN_PLAYLIST_INDEX_TOO_LARGE",
            Self::Io(_) => "BUILT_IN_PLAYLIST_IO_FAILED",
        }
    }
}

impl std::fmt::Display for BuiltInPlaylistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "BUILT_IN_PLAYLIST_IO_FAILED:{msg}"),
            other => write!(f, "{}", other.code()),
        }
    }
}

impl std::error::Error for BuiltInPlaylistError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistRecord {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub tracks: Vec<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: String,
    pub provider: String,
    pub source: String,
    pub builtin: bool,
    pub name: String,
    pub creator: String,
    pub track_count: usize,
    pub cover: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub shelf_pane: String,
    pub subscribed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltInPlaylistListResponse {
    pub ok: bool,
    pub version: u32,
    pub count: usize,
    pub playlists: Vec<PlaylistSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltInPlaylistPageResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub playlist: Option<PlaylistSummary>,
    pub tracks: Vec<Value>,
    pub total: usize,
    pub next_offset: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltInPlaylistMutationResponse {
    pub ok: bool,
    pub version: u32,
    pub count: usize,
    pub playlists: Vec<PlaylistSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist: Option<PlaylistSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedIndex {
    version: u32,
    updated_at: u64,
    playlists: Vec<PlaylistRecord>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn value_as_clean_string(val: &Value) -> String {
    match val {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn clean_text(raw: &str, fallback: &str, max_chars: usize) -> String {
    let cleaned = raw.replace('\0', "");
    let trimmed = cleaned.trim();
    let chosen = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    chosen.chars().take(max_chars).collect()
}

fn clean_opt_value(val: Option<&Value>, fallback: &str, max_chars: usize) -> String {
    match val {
        Some(v) => clean_text(&value_as_clean_string(v), fallback, max_chars),
        None => clean_text("", fallback, max_chars),
    }
}

fn is_truthy(val: Option<&Value>) -> bool {
    match val {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

fn is_valid_playlist_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn normalize_provider(song: &Map<String, Value>) -> &'static str {
    let source = song
        .get("provider")
        .or_else(|| song.get("source"))
        .or_else(|| song.get("type"))
        .map(|v| clean_text(&value_as_clean_string(v), "", 32).to_lowercase())
        .unwrap_or_default();

    if source == "spotify"
        || is_truthy(song.get("spotifyId"))
        || is_truthy(song.get("spotifyUri"))
    {
        return "unsupported";
    }
    if source == "local"
        || is_truthy(song.get("localFileId"))
        || is_truthy(song.get("localKey"))
        || is_truthy(song.get("localUrl"))
    {
        return "local";
    }
    if source == "qq" {
        return "qq";
    }
    if source == "kugou"
        || is_truthy(song.get("hash"))
        || is_truthy(song.get("fileHash"))
        || is_truthy(song.get("audioHash"))
    {
        return "kugou";
    }
    if source == "qishui" {
        return "qishui";
    }
    "netease"
}

fn clean_value(val: &Value, depth: usize) -> Option<Value> {
    match val {
        Value::Null | Value::Bool(_) => Some(val.clone()),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    Some(val.clone())
                } else {
                    Some(Value::from(0))
                }
            } else {
                Some(val.clone())
            }
        }
        Value::String(s) => Some(Value::String(clean_text(s, "", 4096))),
        Value::Array(arr) => {
            if depth >= 2 {
                return None;
            }
            let out: Vec<Value> = arr
                .iter()
                .take(20)
                .filter_map(|entry| clean_value(entry, depth + 1))
                .collect();
            Some(Value::Array(out))
        }
        Value::Object(obj) => {
            if depth >= 2 {
                return None;
            }
            let mut out = Map::new();
            for (k, v) in obj.iter().take(24) {
                if DANGEROUS_KEYS.contains(&k.as_str()) {
                    continue;
                }
                if let Some(cleaned) = clean_value(v, depth + 1) {
                    out.insert(k.clone(), cleaned);
                }
            }
            Some(Value::Object(out))
        }
    }
}

fn non_empty_field_str(map: &Map<String, Value>, keys: &[&str]) -> String {
    for &key in keys {
        if let Some(v) = map.get(key) {
            let s = value_as_clean_string(v);
            let cleaned = clean_text(&s, "", 512);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

/// 计算曲目跨平台去重身份标识（如 `netease:123`、`kugou:<lowercase_hash>`、`local:<id>`）。
pub fn track_identity(track: &Map<String, Value>) -> Option<String> {
    let provider = normalize_provider(track);
    if !ALLOWED_PROVIDERS.contains(&provider) {
        return None;
    }
    let raw = match provider {
        "local" => {
            let first = non_empty_field_str(track, &["localFileId", "localKey"]);
            if !first.is_empty() {
                first
            } else {
                let id_str = non_empty_field_str(track, &["id"]);
                id_str
                    .strip_prefix("local:")
                    .unwrap_or(&id_str)
                    .to_string()
            }
        }
        "qq" => non_empty_field_str(track, &["mid", "songmid", "id"]),
        "kugou" => non_empty_field_str(track, &["hash", "fileHash", "audioHash", "id"]),
        "qishui" => non_empty_field_str(track, &["id", "providerSongId", "trackId", "track_id"]),
        _ => non_empty_field_str(track, &["id"]),
    };
    let value = clean_text(&raw, "", 512);
    if value.is_empty() {
        return None;
    }
    let normalized = if provider == "kugou" {
        value.to_lowercase()
    } else {
        value
    };
    Some(format!("{provider}:{normalized}"))
}

/// 校验并清洗单条曲目 JSON 对象；拒绝 Spotify 与非法/超限曲目，并剥离原型污染等危险键。
pub fn sanitize_track(source: &Value) -> Option<Map<String, Value>> {
    let obj = source.as_object()?;
    let provider = normalize_provider(obj);
    if !ALLOWED_PROVIDERS.contains(&provider) {
        return None;
    }

    let mut track = Map::new();
    for &key in TRACK_FIELDS {
        let Some(raw_val) = obj.get(key) else {
            continue;
        };
        if raw_val.is_null() || raw_val.as_str() == Some("") {
            continue;
        }
        if let Some(cleaned) = clean_value(raw_val, 0) {
            if cleaned.as_str() == Some("") {
                continue;
            }
            track.insert(key.to_string(), cleaned);
        }
    }

    track.insert("provider".to_string(), Value::String(provider.to_string()));
    track.insert("source".to_string(), Value::String(provider.to_string()));
    if !track.contains_key("type") {
        let track_type = match provider {
            "local" => "local",
            "qq" => "qq",
            _ => "song",
        };
        track.insert("type".to_string(), Value::String(track_type.to_string()));
    }

    let name_val = track.get("name").or_else(|| track.get("title")).cloned();
    let title_val = track.get("title").or_else(|| track.get("name")).cloned();
    let name = clean_opt_value(name_val.as_ref(), "未知歌曲", 1000);
    let title = clean_opt_value(title_val.as_ref(), &name, 1000);
    let artist_fallback = if provider == "local" {
        "本地文件"
    } else {
        "未知歌手"
    };
    let artist = clean_opt_value(track.get("artist"), artist_fallback, 1000);

    track.insert("name".to_string(), Value::String(name));
    track.insert("title".to_string(), Value::String(title));
    track.insert("artist".to_string(), Value::String(artist));

    let identity = track_identity(&track)?;
    track.insert("builtInIdentity".to_string(), Value::String(identity));

    let serialized = serde_json::to_string(&track).ok()?;
    if serialized.len() > MAX_TRACK_BYTES {
        return None;
    }
    Some(track)
}

fn sanitize_playlist(source: &Value) -> Option<PlaylistRecord> {
    let obj = source.as_object()?;
    let id = clean_opt_value(obj.get("id"), "", 64).to_lowercase();
    if !is_valid_playlist_id(&id) {
        return None;
    }
    let mut tracks = Vec::new();
    let mut seen = HashSet::new();
    if let Some(arr) = obj.get("tracks").and_then(|v| v.as_array()) {
        for item in arr.iter().take(MAX_TRACKS_PER_PLAYLIST) {
            let Some(track) = sanitize_track(item) else {
                continue;
            };
            let identity = track
                .get("builtInIdentity")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if identity.is_empty() || !seen.insert(identity) {
                continue;
            }
            tracks.push(track);
        }
    }
    let now = now_millis();
    let created_at = obj
        .get("createdAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(now);
    let updated_at = obj
        .get("updatedAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(now);
    Some(PlaylistRecord {
        id,
        name: clean_opt_value(obj.get("name"), "未命名歌单", 80),
        created_at,
        updated_at,
        tracks,
    })
}

fn random_playlist_id() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn build_summary(playlist: &PlaylistRecord) -> PlaylistSummary {
    let cover = playlist
        .tracks
        .first()
        .and_then(|t| t.get("cover"))
        .map(|v| clean_text(&value_as_clean_string(v), "", 4096))
        .unwrap_or_default();
    PlaylistSummary {
        id: playlist.id.clone(),
        provider: "mineradio".to_string(),
        source: "mineradio".to_string(),
        builtin: true,
        name: playlist.name.clone(),
        creator: "MineradioX".to_string(),
        track_count: playlist.tracks.len(),
        cover,
        created_at: playlist.created_at,
        updated_at: playlist.updated_at,
        shelf_pane: "mine".to_string(),
        subscribed: false,
    }
}

pub struct BuiltInPlaylistLibrary {
    index_path: PathBuf,
    playlists: Mutex<Vec<PlaylistRecord>>,
}

impl BuiltInPlaylistLibrary {
    /// 创建内置歌单库并从 `built-in-playlists.json` 恢复已存歌单。
    pub fn new(user_data_dir: impl AsRef<Path>) -> Self {
        let index_path = user_data_dir.as_ref().join(BUILT_IN_PLAYLIST_FILE);
        let initial = load_index_from_disk(&index_path);
        Self {
            index_path,
            playlists: Mutex::new(initial),
        }
    }

    /// 返回当前全部内置歌单摘要列表。
    pub fn list(&self) -> BuiltInPlaylistListResponse {
        let guard = self.playlists.lock().unwrap();
        Self::build_list_response(&guard)
    }

    fn build_list_response(playlists: &[PlaylistRecord]) -> BuiltInPlaylistListResponse {
        BuiltInPlaylistListResponse {
            ok: true,
            version: BUILT_IN_PLAYLIST_VERSION,
            count: playlists.len(),
            playlists: playlists.iter().map(build_summary).collect(),
        }
    }

    /// 分页读取指定歌单的曲目列表。
    pub fn page(&self, id: &str, offset: usize, limit: usize) -> BuiltInPlaylistPageResponse {
        let normalized_id = clean_text(id, "", 64).to_lowercase();
        let guard = self.playlists.lock().unwrap();
        let Some(playlist) = guard.iter().find(|item| item.id == normalized_id) else {
            return BuiltInPlaylistPageResponse {
                ok: false,
                error: Some("BUILT_IN_PLAYLIST_NOT_FOUND".to_string()),
                playlist: None,
                tracks: Vec::new(),
                total: 0,
                next_offset: 0,
                has_more: false,
            };
        };
        let effective_limit = if limit == 0 { 96 } else { limit }.clamp(1, 500);
        let total = playlist.tracks.len();
        let start = offset.min(total);
        let end = start.saturating_add(effective_limit).min(total);
        let tracks: Vec<Value> = playlist.tracks[start..end]
            .iter()
            .cloned()
            .map(Value::Object)
            .collect();
        let next_offset = start + tracks.len();
        BuiltInPlaylistPageResponse {
            ok: true,
            error: None,
            playlist: Some(build_summary(playlist)),
            tracks,
            total,
            next_offset,
            has_more: next_offset < total,
        }
    }

    fn persist_playlists(&self, playlists: &[PlaylistRecord]) -> Result<(), BuiltInPlaylistError> {
        let payload = PersistedIndex {
            version: BUILT_IN_PLAYLIST_VERSION,
            updated_at: now_millis(),
            playlists: playlists.to_vec(),
        };
        let text = serde_json::to_string(&payload)
            .map_err(|e| BuiltInPlaylistError::Io(e.to_string()))?;
        if text.len() > MAX_INDEX_BYTES {
            return Err(BuiltInPlaylistError::IndexTooLarge);
        }
        write_text_atomic(&self.index_path, &text)
            .map_err(|e| BuiltInPlaylistError::Io(e.to_string()))
    }

    /// 创建新歌单（最多 100 个，新歌单置顶）。
    pub fn create(&self, name: &str) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let mut guard = self.playlists.lock().unwrap();
        if guard.len() >= MAX_PLAYLISTS {
            return Err(BuiltInPlaylistError::LimitReached);
        }
        let mut next = guard.clone();
        let now = now_millis();
        let playlist = PlaylistRecord {
            id: random_playlist_id(),
            name: clean_text(name, "我的歌单", 80),
            created_at: now,
            updated_at: now,
            tracks: Vec::new(),
        };
        let summary = build_summary(&playlist);
        next.insert(0, playlist);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: Some(summary),
            removed_id: None,
            added: None,
            duplicate: None,
            removed_index: None,
        })
    }

    /// 重命名指定歌单。
    pub fn rename(
        &self,
        id: &str,
        name: &str,
    ) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let normalized_id = clean_text(id, "", 64).to_lowercase();
        let mut guard = self.playlists.lock().unwrap();
        let mut next = guard.clone();
        let Some(playlist) = next.iter_mut().find(|item| item.id == normalized_id) else {
            return Err(BuiltInPlaylistError::NotFound);
        };
        playlist.name = clean_text(name, &playlist.name, 80);
        playlist.updated_at = now_millis();
        let summary = build_summary(playlist);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: Some(summary),
            removed_id: None,
            added: None,
            duplicate: None,
            removed_index: None,
        })
    }

    /// 删除指定歌单。
    pub fn delete(&self, id: &str) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let normalized_id = clean_text(id, "", 64).to_lowercase();
        let mut guard = self.playlists.lock().unwrap();
        let mut next = guard.clone();
        let Some(pos) = next.iter().position(|item| item.id == normalized_id) else {
            return Err(BuiltInPlaylistError::NotFound);
        };
        let removed = next.remove(pos);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: None,
            removed_id: Some(removed.id),
            added: None,
            duplicate: None,
            removed_index: None,
        })
    }

    /// 向歌单添加曲目；基于跨平台身份标识自动去重，上限 5000 首。
    pub fn add_track(
        &self,
        id: &str,
        source: &Value,
    ) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let track = sanitize_track(source).ok_or(BuiltInPlaylistError::TrackInvalid)?;
        let identity = track
            .get("builtInIdentity")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let normalized_id = clean_text(id, "", 64).to_lowercase();

        let mut guard = self.playlists.lock().unwrap();
        let mut next = guard.clone();
        let Some(playlist) = next.iter_mut().find(|item| item.id == normalized_id) else {
            return Err(BuiltInPlaylistError::NotFound);
        };
        let is_dup = playlist.tracks.iter().any(|existing| {
            existing
                .get("builtInIdentity")
                .and_then(|v| v.as_str())
                == Some(identity.as_str())
        });
        if is_dup {
            let summary = build_summary(playlist);
            let list = Self::build_list_response(&guard);
            return Ok(BuiltInPlaylistMutationResponse {
                ok: list.ok,
                version: list.version,
                count: list.count,
                playlists: list.playlists,
                playlist: Some(summary),
                removed_id: None,
                added: None,
                duplicate: Some(true),
                removed_index: None,
            });
        }
        if playlist.tracks.len() >= MAX_TRACKS_PER_PLAYLIST {
            return Err(BuiltInPlaylistError::TrackLimitReached);
        }
        playlist.tracks.push(track);
        playlist.updated_at = now_millis();
        let summary = build_summary(playlist);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: Some(summary),
            removed_id: None,
            added: Some(true),
            duplicate: None,
            removed_index: None,
        })
    }

    /// 按下标从歌单中移除曲目。
    pub fn remove_track(
        &self,
        id: &str,
        index: usize,
    ) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let normalized_id = clean_text(id, "", 64).to_lowercase();
        let mut guard = self.playlists.lock().unwrap();
        let mut next = guard.clone();
        let Some(playlist) = next.iter_mut().find(|item| item.id == normalized_id) else {
            return Err(BuiltInPlaylistError::NotFound);
        };
        if index >= playlist.tracks.len() {
            return Err(BuiltInPlaylistError::TrackNotFound);
        }
        playlist.tracks.remove(index);
        playlist.updated_at = now_millis();
        let summary = build_summary(playlist);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: Some(summary),
            removed_id: None,
            added: None,
            duplicate: None,
            removed_index: Some(index),
        })
    }

    /// 将歌单内曲目从 `from` 位置移动到 `to` 位置。
    pub fn reorder_track(
        &self,
        id: &str,
        from: usize,
        to: usize,
    ) -> Result<BuiltInPlaylistMutationResponse, BuiltInPlaylistError> {
        let normalized_id = clean_text(id, "", 64).to_lowercase();
        let mut guard = self.playlists.lock().unwrap();
        let mut next = guard.clone();
        let Some(playlist) = next.iter_mut().find(|item| item.id == normalized_id) else {
            return Err(BuiltInPlaylistError::NotFound);
        };
        let len = playlist.tracks.len();
        if from >= len || to >= len {
            return Err(BuiltInPlaylistError::TrackNotFound);
        }
        let track = playlist.tracks.remove(from);
        playlist.tracks.insert(to, track);
        playlist.updated_at = now_millis();
        let summary = build_summary(playlist);
        self.persist_playlists(&next)?;
        *guard = next;

        let list = Self::build_list_response(&guard);
        Ok(BuiltInPlaylistMutationResponse {
            ok: list.ok,
            version: list.version,
            count: list.count,
            playlists: list.playlists,
            playlist: Some(summary),
            removed_id: None,
            added: None,
            duplicate: None,
            removed_index: None,
        })
    }
}

fn load_index_from_disk(index_path: &Path) -> Vec<PlaylistRecord> {
    let Ok(meta) = fs::metadata(index_path) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() == 0 || meta.len() as usize > MAX_INDEX_BYTES {
        return Vec::new();
    }
    let Ok(raw) = fs::read_to_string(index_path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    if parsed.get("version").and_then(|v| v.as_u64()) != Some(BUILT_IN_PLAYLIST_VERSION as u64) {
        return Vec::new();
    }
    let Some(raw_playlists) = parsed.get("playlists").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in raw_playlists.iter().take(MAX_PLAYLISTS) {
        let Some(playlist) = sanitize_playlist(item) else {
            continue;
        };
        if seen.insert(playlist.id.clone()) {
            out.push(playlist);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn crud_pagination_reorder_and_persistence() {
        let tmp = TempDir::new().unwrap();
        let lib = BuiltInPlaylistLibrary::new(tmp.path());
        assert_eq!(lib.list().count, 0);

        let created = lib.create("夜听收藏").unwrap();
        assert_eq!(created.count, 1);
        let pid = created.playlist.unwrap().id;

        lib.rename(&pid, "深夜歌单").unwrap();
        assert_eq!(lib.list().playlists[0].name, "深夜歌单");

        let add1 = lib
            .add_track(
                &pid,
                &json!({ "provider": "netease", "id": 1001, "name": "晴天", "artist": "周杰伦", "cover": "https://img/1.jpg" }),
            )
            .unwrap();
        assert_eq!(add1.added, Some(true));

        let add2 = lib
            .add_track(
                &pid,
                &json!({ "provider": "qq", "mid": "001ABC", "title": "稻香", "artist": "周杰伦" }),
            )
            .unwrap();
        assert_eq!(add2.added, Some(true));

        let add3 = lib
            .add_track(
                &pid,
                &json!({ "provider": "qishui", "trackId": "qs-9", "name": "风" }),
            )
            .unwrap();
        assert_eq!(add3.added, Some(true));

        // 移动第 2 首到第 0 位
        lib.reorder_track(&pid, 2, 0).unwrap();
        let page1 = lib.page(&pid, 0, 2);
        assert!(page1.ok);
        assert_eq!(page1.total, 3);
        assert!(page1.has_more);
        assert_eq!(page1.tracks[0]["builtInIdentity"], "qishui:qs-9");
        assert_eq!(page1.tracks[1]["builtInIdentity"], "netease:1001");

        // 移除第 1 首
        lib.remove_track(&pid, 1).unwrap();
        let page_after = lib.page(&pid, 0, 10);
        assert_eq!(page_after.total, 2);

        // 重建实例验证持久化恢复
        let reloaded = BuiltInPlaylistLibrary::new(tmp.path());
        assert_eq!(reloaded.list().count, 1);
        assert_eq!(reloaded.list().playlists[0].name, "深夜歌单");
        assert_eq!(reloaded.page(&pid, 0, 10).total, 2);

        // 删除歌单
        let del = reloaded.delete(&pid).unwrap();
        assert_eq!(del.removed_id.as_deref(), Some(pid.as_str()));
        assert_eq!(reloaded.list().count, 0);
    }

    #[test]
    fn dedup_rejects_spotify_dangerous_keys_and_oversized_track() {
        let tmp = TempDir::new().unwrap();
        let lib = BuiltInPlaylistLibrary::new(tmp.path());
        let pid = lib.create("测试").unwrap().playlist.unwrap().id;

        // 酷狗哈希大小写归一化去重
        let first = lib
            .add_track(
                &pid,
                &json!({ "provider": "kugou", "hash": "ABCDEF1234", "name": "KG" }),
            )
            .unwrap();
        assert_eq!(first.added, Some(true));

        let dup = lib
            .add_track(
                &pid,
                &json!({ "provider": "kugou", "fileHash": "abcdef1234", "name": "KG2" }),
            )
            .unwrap();
        assert_eq!(dup.duplicate, Some(true));
        assert_eq!(lib.page(&pid, 0, 10).total, 1);

        // 本地曲目去重
        lib.add_track(
            &pid,
            &json!({ "provider": "local", "id": "local:0123456789abcdef01234567", "name": "L" }),
        )
        .unwrap();
        let local_dup = lib
            .add_track(
                &pid,
                &json!({ "localFileId": "0123456789abcdef01234567", "name": "L2" }),
            )
            .unwrap();
        assert_eq!(local_dup.duplicate, Some(true));

        // 拒绝 Spotify
        let sp1 = lib
            .add_track(&pid, &json!({ "provider": "spotify", "id": "sp1", "name": "S" }))
            .unwrap_err();
        assert_eq!(sp1, BuiltInPlaylistError::TrackInvalid);
        let sp2 = lib
            .add_track(&pid, &json!({ "spotifyId": "sp2", "id": "2", "name": "S2" }))
            .unwrap_err();
        assert_eq!(sp2, BuiltInPlaylistError::TrackInvalid);

        // 危险键在嵌套对象中应被剥离
        lib.add_track(
            &pid,
            &json!({
                "provider": "netease",
                "id": 888,
                "name": "Safe",
                "__proto__": { "polluted": true },
                "privilege": {
                    "__proto__": { "evil": 1 },
                    "constructor": "bad",
                    "prototype": "bad",
                    "st": 0
                }
            }),
        )
        .unwrap();
        let page = lib.page(&pid, 0, 10);
        let last = page.tracks.last().unwrap();
        assert!(last.get("__proto__").is_none());
        let priv_obj = last["privilege"].as_object().unwrap();
        assert!(!priv_obj.contains_key("__proto__"));
        assert!(!priv_obj.contains_key("constructor"));
        assert!(!priv_obj.contains_key("prototype"));
        assert_eq!(priv_obj["st"], 0);

        // 超过 48 KiB 的单曲目应拒绝
        let huge_parts: Vec<String> = (0..15).map(|i| format!("{i}_{}", "x".repeat(4000))).collect();
        let huge_err = lib
            .add_track(
                &pid,
                &json!({
                    "provider": "netease",
                    "id": 999,
                    "name": "Huge",
                    "privilege": huge_parts.into_iter().enumerate().map(|(i, v)| (format!("k{i}"), Value::String(v))).collect::<Map<String, Value>>()
                }),
            )
            .unwrap_err();
        assert_eq!(huge_err, BuiltInPlaylistError::TrackInvalid);
    }

    #[test]
    fn enforces_max_playlists_limit() {
        let tmp = TempDir::new().unwrap();
        let lib = BuiltInPlaylistLibrary::new(tmp.path());
        for i in 0..MAX_PLAYLISTS {
            lib.create(&format!("P{i}")).unwrap();
        }
        let err = lib.create("Overflow").unwrap_err();
        assert_eq!(err, BuiltInPlaylistError::LimitReached);
    }
}
