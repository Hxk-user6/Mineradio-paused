use crate::config::write_text_atomic;
use encoding_rs::GB18030;
use image::{ImageFormat, ImageReader};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::Picture;
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOCAL_MUSIC_SCHEME: &str = "mineradio-local";
pub const LOCAL_LIBRARY_VERSION: u32 = 1;
pub const LOCAL_LIBRARY_FILE: &str = "local-music-library.json";
pub const LOCAL_LIBRARY_DIRECTORY: &str = "local-music-library";
pub const LOCAL_COVER_DIRECTORY: &str = "covers";
pub const MAX_LIBRARY_INDEX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LYRIC_BYTES: usize = 512 * 1024;
pub const MAX_COVER_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_UNKNOWN_DIMENSION_COVER_BYTES: usize = 1024 * 1024;
pub const MAX_COVER_DIMENSION: u32 = 4096;
pub const MAX_COVER_PIXELS: u64 = 12 * 1024 * 1024;
pub const MAX_IMPORT_FILES: usize = 50_000;
pub const AUTHORIZE_TTL_MS: u64 = 3 * 60 * 1000;
pub const MAX_AUTHORIZE_CAPABILITIES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportEntry {
    pub path: String,
    #[serde(default)]
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTrackRecord {
    pub id: String,
    pub audio_path: String,
    pub relative_path: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub size: u64,
    pub mtime_ms: u64,
    pub revision: String,
    pub cover_path: String,
    pub cover_mime: String,
    pub lyric: String,
    pub lyric_source: String,
    pub imported_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTrackItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    pub provider: String,
    pub id: String,
    pub local_file_id: String,
    pub local_key: String,
    pub local_url: String,
    pub local_path: String,
    pub local_missing: bool,
    pub name: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub cover: String,
    pub has_lyric: bool,
    pub lyric_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportIssue {
    pub name: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTracksResult {
    pub ok: bool,
    pub version: u32,
    pub count: usize,
    pub tracks: Vec<LocalTrackItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failures: Option<Vec<LocalImportIssue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_warnings: Option<Vec<LocalImportIssue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLyricResult {
    pub ok: bool,
    pub local_file_id: String,
    pub lyric: String,
    pub lyric_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeFilesResult {
    pub ok: bool,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLocalLibrary {
    version: u32,
    updated_at: u64,
    media_token: String,
    records: Vec<LocalTrackRecord>,
}

#[derive(Debug, Clone)]
struct AuthorizeCapability {
    files: Vec<NormalizedEntry>,
    replace: bool,
    expires_at: u64,
}

#[derive(Debug, Clone)]
struct NormalizedEntry {
    path: PathBuf,
    relative_path: String,
}

struct LibraryState {
    records: HashMap<String, LocalTrackRecord>,
    order: Vec<String>,
    media_token: String,
}

pub struct LocalMusicLibrary {
    library_directory: PathBuf,
    cover_directory: PathBuf,
    index_path: PathBuf,
    state: Mutex<LibraryState>,
    capabilities: Mutex<CapabilityStore>,
}

struct CapabilityStore {
    map: HashMap<String, AuthorizeCapability>,
    fifo: VecDeque<String>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_hex_bytes(len_bytes: usize) -> String {
    let mut buf = vec![0u8; len_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
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

fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let aa = a.as_bytes();
    let bb = b.as_bytes();
    let max_len = aa.len().max(bb.len());
    let mut diff: u8 = if aa.len() == bb.len() { 0 } else { 1 };
    for i in 0..max_len {
        let x = *aa.get(i).unwrap_or(&0);
        let y = *bb.get(i).unwrap_or(&0);
        diff |= black_box(x ^ y);
    }
    black_box(diff) == 0
}

pub fn audio_mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => Some("audio/mpeg"),
        "flac" => Some("audio/flac"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "opus" => Some("audio/ogg"),
        _ => None,
    }
}

fn cover_extension_by_mime(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/webp" => Some(".webp"),
        "image/gif" => Some(".gif"),
        "image/bmp" => Some(".bmp"),
        _ => None,
    }
}

fn cover_mime_by_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn strip_windows_verbatim(path: PathBuf) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy();
        if raw.starts_with(r"\\?\UNC\") || raw.starts_with(r"\\?\unc\") {
            return None;
        }
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return Some(PathBuf::from(stripped));
        }
    }
    Some(path)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 校验并规范化绝对文件路径；拒绝空路径、UNC 路径（`\\` / `//`）及非绝对路径。
pub fn normalized_absolute_file_path(value: &str) -> Option<PathBuf> {
    let input = value.trim();
    if input.is_empty()
        || input.starts_with("\\\\")
        || input.starts_with("//")
        || input.starts_with("\\/")
        || input.starts_with("/\\")
    {
        return None;
    }
    let path = Path::new(input);
    if !path.is_absolute() {
        return None;
    }
    Some(lexical_normalize(path))
}

pub fn normalized_path_identity(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

pub fn supported_audio_path(value: &str) -> Option<PathBuf> {
    let resolved = normalized_absolute_file_path(value)?;
    audio_mime_for_path(&resolved)?;
    Some(resolved)
}

pub fn local_file_id(file_path: &Path) -> String {
    let identity = normalized_path_identity(file_path);
    let digest = hex::encode(Sha256::digest(identity.as_bytes()));
    digest[..24].to_string()
}

fn is_valid_id24(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_valid_token48(token: &str) -> bool {
    token.len() == 48 && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_path_inside(root: &Path, candidate: &Path) -> bool {
    let root_norm = fs::canonicalize(root).unwrap_or_else(|_| lexical_normalize(root));
    let cand_norm = fs::canonicalize(candidate).unwrap_or_else(|_| lexical_normalize(candidate));
    cand_norm.starts_with(&root_norm) && cand_norm != root_norm
}

fn encode_base36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while value > 0 {
        buf.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_else(|_| "0".to_string())
}

fn audio_revision(meta: &fs::Metadata) -> String {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{}-{}", encode_base36(mtime_ms), encode_base36(meta.len()))
}

fn local_media_url(kind: &str, id: &str, revision: &str, capability: &str) -> String {
    let mut query = Vec::new();
    if !revision.is_empty() {
        query.push(format!("v={revision}"));
    }
    if !capability.is_empty() {
        query.push(format!("cap={capability}"));
    }
    if query.is_empty() {
        format!("{LOCAL_MUSIC_SCHEME}://{kind}/{id}")
    } else {
        format!("{LOCAL_MUSIC_SCHEME}://{kind}/{id}?{}", query.join("&"))
    }
}

/// 解码 LRC 歌词字节缓冲区，依次支持 UTF-8 BOM、UTF-16 LE/BE BOM、UTF-8 与 GB18030。
pub fn decode_lyric_buffer(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let decoded = if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        String::from_utf8_lossy(&bytes[3..]).into_owned()
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Ok(utf8_str) = std::str::from_utf8(bytes) {
        utf8_str.to_string()
    } else {
        let (cow, _, _) = GB18030.decode(bytes);
        cow.into_owned()
    };
    decoded.replace('\0', "").trim().to_string()
}

/// 检查内嵌封面是否符合内存与像素预算（<= 6 MiB, <= 4096px, <= 12MP；未知尺寸 <= 1 MiB）。
pub fn cover_within_budget(data: &[u8], mime: &str) -> bool {
    if data.is_empty() || data.len() > MAX_COVER_BYTES {
        return false;
    }
    let dimensions = if let Some(fmt) = ImageFormat::from_mime_type(mime.trim()) {
        ImageReader::with_format(Cursor::new(data), fmt)
            .into_dimensions()
            .ok()
    } else {
        ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
    };
    let Some((width, height)) = dimensions else {
        return data.len() <= MAX_UNKNOWN_DIMENSION_COVER_BYTES;
    };
    width > 0
        && height > 0
        && width <= MAX_COVER_DIMENSION
        && height <= MAX_COVER_DIMENSION
        && (width as u64) * (height as u64) <= MAX_COVER_PIXELS
}

fn extract_tag_artist(tag: &Tag) -> String {
    if let Some(artist) = tag.artist() {
        let cleaned = clean_text(artist.as_ref(), "", 1000);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    let joined: Vec<String> = tag
        .items()
        .filter(|item| item.key() == &ItemKey::TrackArtist)
        .filter_map(|item| match item.value() {
            ItemValue::Text(s) | ItemValue::Locator(s) => {
                let c = clean_text(s, "", 500);
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            }
            _ => None,
        })
        .collect();
    clean_text(&joined.join(" / "), "", 1000)
}

fn extract_tag_lyric(tag: &Tag) -> String {
    if let Some(s) = tag.get_string(&ItemKey::Lyrics) {
        let cleaned = clean_text(s, "", MAX_LYRIC_BYTES);
        if !cleaned.is_empty() {
            return truncate_utf8_bytes(&cleaned, MAX_LYRIC_BYTES);
        }
    }
    for item in tag.items() {
        if item.key() == &ItemKey::Lyrics {
            if let Some(s) = item.value().text() {
                let cleaned = clean_text(s, "", MAX_LYRIC_BYTES);
                if !cleaned.is_empty() {
                    return truncate_utf8_bytes(&cleaned, MAX_LYRIC_BYTES);
                }
            }
        }
    }
    String::new()
}

struct ParsedAudioMetadata {
    title: String,
    artist: String,
    album: String,
    duration: f64,
    embedded_lyric: String,
    picture_mime: String,
    picture_data: Vec<u8>,
}

fn parse_audio_metadata(path: &Path) -> Result<ParsedAudioMetadata, String> {
    let tagged = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    let duration = tagged.properties().duration().as_secs_f64();
    let ordered_tags: Vec<&Tag> = tagged
        .primary_tag()
        .into_iter()
        .chain(tagged.tags().iter())
        .collect();

    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut embedded_lyric = String::new();
    let mut first_picture: Option<&Picture> = None;

    for tag in ordered_tags {
        if title.is_empty() {
            if let Some(t) = tag.title() {
                title = clean_text(t.as_ref(), "", 1000);
            }
        }
        if artist.is_empty() {
            artist = extract_tag_artist(tag);
        }
        if album.is_empty() {
            if let Some(a) = tag.album() {
                album = clean_text(a.as_ref(), "", 1000);
            }
        }
        if embedded_lyric.is_empty() {
            embedded_lyric = extract_tag_lyric(tag);
        }
        if first_picture.is_none() {
            first_picture = tag.pictures().first();
        }
    }

    let (picture_mime, picture_data) = if let Some(pic) = first_picture {
        let data = pic.data().to_vec();
        let raw_mime = pic
            .mime_type()
            .map(|m| m.as_str().trim().to_ascii_lowercase())
            .unwrap_or_default();
        let resolved_mime = if cover_extension_by_mime(&raw_mime).is_some() {
            raw_mime
        } else {
            image::guess_format(&data)
                .ok()
                .map(|fmt| fmt.to_mime_type().to_string())
                .unwrap_or(raw_mime)
        };
        (resolved_mime, data)
    } else {
        (String::new(), Vec::new())
    };

    Ok(ParsedAudioMetadata {
        title,
        artist,
        album,
        duration: if duration.is_finite() && duration > 0.0 {
            duration
        } else {
            0.0
        },
        embedded_lyric,
        picture_mime,
        picture_data,
    })
}

fn build_lrc_sidecar_index(entries: &[NormalizedEntry]) -> HashMap<String, HashMap<String, PathBuf>> {
    let mut dir_set = HashSet::new();
    let mut dirs = Vec::new();
    for entry in entries {
        if let Some(parent) = entry.path.parent() {
            let id = normalized_path_identity(parent);
            if dir_set.insert(id) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    let mut maps = HashMap::new();
    for dir in dirs {
        let mut lookup = HashMap::new();
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for item in read_dir.flatten() {
                let p = item.path();
                let is_lrc = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("lrc"))
                    .unwrap_or(false);
                if !is_lrc {
                    continue;
                }
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    lookup.insert(stem.to_lowercase(), p);
                }
            }
        }
        maps.insert(normalized_path_identity(&dir), lookup);
    }
    maps
}

fn normalize_import_entries(input: Vec<LocalImportEntry>) -> Vec<NormalizedEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for item in input.into_iter().take(MAX_IMPORT_FILES) {
        let Some(file_path) = supported_audio_path(&item.path) else {
            continue;
        };
        let identity = normalized_path_identity(&file_path);
        if identity.is_empty() || !seen.insert(identity) {
            continue;
        }
        let fallback_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative_path = clean_text(
            item.relative_path.as_deref().unwrap_or(""),
            &fallback_name,
            2000,
        );
        entries.push(NormalizedEntry {
            path: file_path,
            relative_path,
        });
    }
    entries
}

fn serialize_record(record: &LocalTrackRecord, media_token: &str) -> LocalTrackItem {
    let cover = if record.cover_path.is_empty() {
        String::new()
    } else {
        local_media_url("cover", &record.id, &record.revision, media_token)
    };
    let local_path = if record.relative_path.is_empty() {
        Path::new(&record.audio_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        record.relative_path.clone()
    };
    LocalTrackItem {
        kind: "local".to_string(),
        source: "local".to_string(),
        provider: "local".to_string(),
        id: format!("local:{}", record.id),
        local_file_id: record.id.clone(),
        local_key: record.id.clone(),
        local_url: local_media_url("audio", &record.id, &record.revision, media_token),
        local_path,
        local_missing: false,
        name: record.name.clone(),
        title: record.name.clone(),
        artist: if record.artist.is_empty() {
            "本地文件".to_string()
        } else {
            record.artist.clone()
        },
        album: record.album.clone(),
        duration: if record.duration.is_finite() && record.duration > 0.0 {
            record.duration
        } else {
            0.0
        },
        cover,
        has_lyric: !record.lyric.is_empty(),
        lyric_source: record.lyric_source.clone(),
    }
}

struct StagedCover {
    target_path: String,
    mime: String,
    staged_path: Option<PathBuf>,
    rejected: bool,
}

struct ParsedEntryResult {
    record: LocalTrackRecord,
    metadata_error: String,
    cover_warning: String,
    staged_cover_path: Option<PathBuf>,
    previous_cover_path: String,
}

impl LocalMusicLibrary {
    /// 创建本地曲库实例并从 `local-music-library.json` 恢复已存曲目索引。
    pub fn new(user_data_dir: impl AsRef<Path>) -> Self {
        let root = user_data_dir.as_ref().to_path_buf();
        let library_directory = root.join(LOCAL_LIBRARY_DIRECTORY);
        let cover_directory = library_directory.join(LOCAL_COVER_DIRECTORY);
        let index_path = root.join(LOCAL_LIBRARY_FILE);
        let state = load_index_from_disk(&index_path, &cover_directory);
        Self {
            library_directory,
            cover_directory,
            index_path,
            state: Mutex::new(state),
            capabilities: Mutex::new(CapabilityStore {
                map: HashMap::new(),
                fifo: VecDeque::new(),
            }),
        }
    }

    /// 返回用于鉴权 `mineradio-local://` 媒体请求的会话能力令牌。
    pub fn media_token(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .media_token
            .clone()
    }

    pub fn library_directory(&self) -> &Path {
        &self.library_directory
    }

    /// 返回当前已导入的全部本地曲目列表。
    pub fn list_tracks(&self) -> LocalTracksResult {
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        Self::build_tracks_result(&guard, None, None, None)
    }

    fn build_tracks_result(
        state: &LibraryState,
        failures: Option<Vec<LocalImportIssue>>,
        metadata_warnings: Option<Vec<LocalImportIssue>>,
        error: Option<String>,
    ) -> LocalTracksResult {
        let mut tracks = Vec::with_capacity(state.order.len());
        for id in &state.order {
            if let Some(rec) = state.records.get(id) {
                tracks.push(serialize_record(rec, &state.media_token));
            }
        }
        LocalTracksResult {
            ok: error.is_none(),
            version: LOCAL_LIBRARY_VERSION,
            count: tracks.len(),
            tracks,
            failures,
            metadata_warnings,
            error,
        }
    }

    /// 按曲目 ID（支持 `local:<id>` 或 24 位 hex）读取已索引歌词。
    pub fn lyric_for_track(&self, id: &str) -> LocalLyricResult {
        let clean_id = clean_text(id, "", 64)
            .strip_prefix("local:")
            .unwrap_or_else(|| id.trim())
            .to_lowercase();
        if !is_valid_id24(&clean_id) {
            return LocalLyricResult {
                ok: false,
                local_file_id: String::new(),
                lyric: String::new(),
                lyric_source: String::new(),
                missing: None,
                error: Some("LOCAL_TRACK_INVALID".to_string()),
            };
        }
        let guard = self.state.lock().unwrap();
        let Some(rec) = guard.records.get(&clean_id) else {
            return LocalLyricResult {
                ok: false,
                local_file_id: clean_id,
                lyric: String::new(),
                lyric_source: String::new(),
                missing: Some(true),
                error: Some("LOCAL_TRACK_MISSING".to_string()),
            };
        };
        LocalLyricResult {
            ok: true,
            local_file_id: clean_id,
            lyric: rec.lyric.clone(),
            lyric_source: rec.lyric_source.clone(),
            missing: None,
            error: None,
        }
    }

    /// 校验并授权待导入文件列表，签发有效期 3 分钟的一次性导入令牌。
    pub fn authorize_files(
        &self,
        files: Vec<LocalImportEntry>,
        replace: bool,
    ) -> AuthorizeFilesResult {
        let mut authorized = Vec::new();
        let mut seen = HashSet::new();

        for item in files.into_iter().take(MAX_IMPORT_FILES) {
            let Some(abs) = supported_audio_path(&item.path) else {
                continue;
            };
            let Ok(canonical) = fs::canonicalize(&abs) else {
                continue;
            };
            let Some(clean_canonical) = strip_windows_verbatim(canonical) else {
                continue;
            };
            let canonical_str = clean_canonical.to_string_lossy();
            if canonical_str.starts_with("\\\\") || canonical_str.starts_with("//") {
                continue;
            }
            let Ok(meta) = fs::metadata(&clean_canonical) else {
                continue;
            };
            if !meta.is_file() || audio_mime_for_path(&clean_canonical).is_none() {
                continue;
            }
            let identity = normalized_path_identity(&clean_canonical);
            if !seen.insert(identity) {
                continue;
            }
            let fallback_name = clean_canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let relative_path = clean_text(
                item.relative_path.as_deref().unwrap_or(""),
                &fallback_name,
                2000,
            );
            authorized.push(NormalizedEntry {
                path: clean_canonical,
                relative_path,
            });
        }

        if authorized.is_empty() {
            return AuthorizeFilesResult {
                ok: false,
                count: 0,
                token: None,
                error: Some("NO_AUTHORIZED_LOCAL_AUDIO".to_string()),
            };
        }

        let now = now_millis();
        let token = random_hex_bytes(24);
        let count = authorized.len();

        let mut store = self.capabilities.lock().unwrap();
        prune_capabilities(&mut store, now);
        store.map.insert(
            token.clone(),
            AuthorizeCapability {
                files: authorized,
                replace,
                expires_at: now.saturating_add(AUTHORIZE_TTL_MS),
            },
        );
        store.fifo.push_back(token.clone());
        while store.map.len() > MAX_AUTHORIZE_CAPABILITIES {
            if let Some(oldest) = store.fifo.pop_front() {
                store.map.remove(&oldest);
            } else {
                break;
            }
        }

        AuthorizeFilesResult {
            ok: true,
            count,
            token: Some(token),
            error: None,
        }
    }

    /// 消费 `authorize_files` 签发的一次性令牌并执行导入。
    pub fn import_authorized(&self, token: &str) -> LocalTracksResult {
        let normalized_token = token.trim().to_lowercase();
        let now = now_millis();
        let capability = {
            let mut store = self.capabilities.lock().unwrap();
            prune_capabilities(&mut store, now);
            if !is_valid_token48(&normalized_token) {
                None
            } else {
                let cap = store.map.remove(&normalized_token);
                store.fifo.retain(|t| t != &normalized_token);
                cap
            }
        };

        let Some(cap) = capability else {
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: None,
                metadata_warnings: None,
                error: Some("LOCAL_IMPORT_CAPABILITY_INVALID".to_string()),
            };
        };
        if cap.expires_at <= now {
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: None,
                metadata_warnings: None,
                error: Some("LOCAL_IMPORT_CAPABILITY_INVALID".to_string()),
            };
        }
        self.import_normalized_entries(cap.files, cap.replace)
    }

    /// 直接校验并导入本地音频文件列表（支持追加或替换）。
    pub fn import_files(&self, files: Vec<LocalImportEntry>, replace: bool) -> LocalTracksResult {
        let entries = normalize_import_entries(files);
        if entries.is_empty() {
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: Some(Vec::new()),
                metadata_warnings: None,
                error: Some("NO_SUPPORTED_LOCAL_AUDIO".to_string()),
            };
        }
        self.import_normalized_entries(entries, replace)
    }

    fn stage_cover(
        &self,
        id: &str,
        mime: &str,
        data: &[u8],
        previous: Option<&LocalTrackRecord>,
    ) -> Result<StagedCover, String> {
        if data.is_empty() && mime.is_empty() {
            return Ok(StagedCover {
                target_path: String::new(),
                mime: String::new(),
                staged_path: None,
                rejected: false,
            });
        }
        let Some(ext) = cover_extension_by_mime(mime) else {
            return Ok(StagedCover {
                target_path: previous.map(|p| p.cover_path.clone()).unwrap_or_default(),
                mime: previous.map(|p| p.cover_mime.clone()).unwrap_or_default(),
                staged_path: None,
                rejected: false,
            });
        };
        if !cover_within_budget(data, mime) {
            return Ok(StagedCover {
                target_path: previous.map(|p| p.cover_path.clone()).unwrap_or_default(),
                mime: previous.map(|p| p.cover_mime.clone()).unwrap_or_default(),
                staged_path: None,
                rejected: !data.is_empty(),
            });
        }
        fs::create_dir_all(&self.cover_directory).map_err(|e| e.to_string())?;
        let digest = &hex::encode(Sha256::digest(data))[..16];
        let target = self.cover_directory.join(format!("{id}-{digest}{ext}"));
        if target.exists() {
            return Ok(StagedCover {
                target_path: target.to_string_lossy().into_owned(),
                mime: mime.to_string(),
                staged_path: None,
                rejected: false,
            });
        }
        let staged = self.cover_directory.join(format!(
            ".{id}-{digest}.{}.{}.stage",
            std::process::id(),
            now_millis()
        ));
        fs::write(&staged, data).map_err(|e| e.to_string())?;
        Ok(StagedCover {
            target_path: target.to_string_lossy().into_owned(),
            mime: mime.to_string(),
            staged_path: Some(staged),
            rejected: false,
        })
    }

    fn parse_entry(
        &self,
        entry: &NormalizedEntry,
        sidecar_dirs: &HashMap<String, HashMap<String, PathBuf>>,
        previous_records: &HashMap<String, LocalTrackRecord>,
    ) -> Result<ParsedEntryResult, String> {
        let meta = fs::metadata(&entry.path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("LOCAL_AUDIO_NOT_FILE".to_string());
        }
        let id = local_file_id(&entry.path);
        let previous = previous_records.get(&id);

        let mut metadata_error = String::new();
        let parsed = match parse_audio_metadata(&entry.path) {
            Ok(p) => Some(p),
            Err(err) => {
                metadata_error = err.chars().take(500).collect();
                None
            }
        };

        let fallback_title = entry
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未知歌曲".to_string());

        let cover = match &parsed {
            Some(meta_ok) => {
                self.stage_cover(&id, &meta_ok.picture_mime, &meta_ok.picture_data, previous)?
            }
            None => StagedCover {
                target_path: previous.map(|p| p.cover_path.clone()).unwrap_or_default(),
                mime: previous.map(|p| p.cover_mime.clone()).unwrap_or_default(),
                staged_path: None,
                rejected: false,
            },
        };

        let mut lyric = String::new();
        let mut lyric_source = String::new();
        if let Some(parent) = entry.path.parent() {
            if let Some(lookup) = sidecar_dirs.get(&normalized_path_identity(parent)) {
                if let Some(sidecar_path) = lookup.get(&fallback_title.to_lowercase()) {
                    if let Ok(lrc_meta) = fs::metadata(sidecar_path) {
                        if lrc_meta.is_file()
                            && lrc_meta.len() > 0
                            && lrc_meta.len() as usize <= MAX_LYRIC_BYTES
                        {
                            if let Ok(bytes) = fs::read(sidecar_path) {
                                let decoded = decode_lyric_buffer(&bytes);
                                if !decoded.is_empty() {
                                    lyric = truncate_utf8_bytes(&decoded, MAX_LYRIC_BYTES);
                                    lyric_source = "sidecar".to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
        if lyric.is_empty() {
            if let Some(meta_ok) = &parsed {
                if !meta_ok.embedded_lyric.is_empty() {
                    lyric = meta_ok.embedded_lyric.clone();
                    lyric_source = "embedded".to_string();
                }
            }
        }
        if lyric.is_empty() && !metadata_error.is_empty() {
            if let Some(prev) = previous {
                if !prev.lyric.is_empty() {
                    lyric = prev.lyric.clone();
                    lyric_source = prev.lyric_source.clone();
                }
            }
        }

        let fallback_album = Path::new(&entry.relative_path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty() && s != ".")
            .map(|s| s.replace(['/', '\\'], " / "))
            .unwrap_or_default();

        let title_fallback = if !metadata_error.is_empty() {
            previous
                .map(|p| p.name.as_str())
                .unwrap_or(&fallback_title)
        } else {
            &fallback_title
        };
        let artist_fallback = if !metadata_error.is_empty() {
            previous.map(|p| p.artist.as_str()).unwrap_or("本地文件")
        } else {
            "本地文件"
        };
        let album_fallback = if !metadata_error.is_empty() {
            previous
                .map(|p| p.album.as_str())
                .unwrap_or(&fallback_album)
        } else {
            &fallback_album
        };

        let name = clean_text(
            parsed.as_ref().map(|p| p.title.as_str()).unwrap_or(""),
            title_fallback,
            1000,
        );
        let artist = clean_text(
            parsed.as_ref().map(|p| p.artist.as_str()).unwrap_or(""),
            artist_fallback,
            1000,
        );
        let album = clean_text(
            parsed.as_ref().map(|p| p.album.as_str()).unwrap_or(""),
            album_fallback,
            1000,
        );
        let duration = parsed
            .as_ref()
            .map(|p| p.duration)
            .filter(|&d| d > 0.0)
            .or_else(|| {
                if !metadata_error.is_empty() {
                    previous.map(|p| p.duration)
                } else {
                    None
                }
            })
            .unwrap_or(0.0);

        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let record = LocalTrackRecord {
            id,
            audio_path: entry.path.to_string_lossy().into_owned(),
            relative_path: if entry.relative_path.is_empty() {
                entry
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                entry.relative_path.clone()
            },
            name,
            artist,
            album,
            duration,
            size: meta.len(),
            mtime_ms,
            revision: audio_revision(&meta),
            cover_path: cover.target_path,
            cover_mime: cover.mime,
            lyric,
            lyric_source,
            imported_at: now_millis(),
        };

        Ok(ParsedEntryResult {
            record,
            metadata_error,
            cover_warning: if cover.rejected {
                "LOCAL_COVER_REJECTED_BY_BUDGET".to_string()
            } else {
                String::new()
            },
            staged_cover_path: cover.staged_path,
            previous_cover_path: previous.map(|p| p.cover_path.clone()).unwrap_or_default(),
        })
    }

    fn import_normalized_entries(
        &self,
        entries: Vec<NormalizedEntry>,
        replace: bool,
    ) -> LocalTracksResult {
        if entries.is_empty() {
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: Some(Vec::new()),
                metadata_warnings: None,
                error: Some("NO_SUPPORTED_LOCAL_AUDIO".to_string()),
            };
        }

        let sidecar_dirs = build_lrc_sidecar_index(&entries);
        let mut guard = self.state.lock().unwrap();

        let mut next_records = if replace {
            HashMap::new()
        } else {
            guard.records.clone()
        };
        let mut next_order = if replace {
            Vec::new()
        } else {
            guard.order.clone()
        };

        let mut failures = Vec::new();
        let mut metadata_warnings = Vec::new();
        let mut staged_covers: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut cleanup_after_commit: HashSet<String> = HashSet::new();

        for entry in &entries {
            match self.parse_entry(entry, &sidecar_dirs, &guard.records) {
                Ok(parsed) => {
                    let record = parsed.record;
                    let file_name = Path::new(&record.audio_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !parsed.metadata_error.is_empty() {
                        metadata_warnings.push(LocalImportIssue {
                            name: file_name.clone(),
                            error: parsed.metadata_error,
                        });
                    }
                    if !parsed.cover_warning.is_empty() {
                        metadata_warnings.push(LocalImportIssue {
                            name: file_name,
                            error: parsed.cover_warning,
                        });
                    }
                    if let Some(staged) = parsed.staged_cover_path {
                        staged_covers.push((staged, PathBuf::from(&record.cover_path)));
                    }
                    if !parsed.previous_cover_path.is_empty()
                        && (record.cover_path.is_empty()
                            || Path::new(&parsed.previous_cover_path)
                                != Path::new(&record.cover_path))
                    {
                        cleanup_after_commit.insert(parsed.previous_cover_path);
                    }
                    next_order.retain(|existing| existing != &record.id);
                    next_order.push(record.id.clone());
                    next_records.insert(record.id.clone(), record);
                }
                Err(err) => {
                    let name = entry
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    failures.push(LocalImportIssue {
                        name,
                        error: err.chars().take(500).collect(),
                    });
                }
            }
        }

        if next_order.is_empty() {
            for (staged, _) in staged_covers {
                let _ = fs::remove_file(staged);
            }
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: Some(failures),
                metadata_warnings: Some(metadata_warnings),
                error: Some("LOCAL_IMPORT_FAILED".to_string()),
            };
        }

        if replace {
            for id in &guard.order {
                if !next_records.contains_key(id) {
                    if let Some(old_rec) = guard.records.get(id) {
                        if !old_rec.cover_path.is_empty() {
                            cleanup_after_commit.insert(old_rec.cover_path.clone());
                        }
                    }
                }
            }
        }

        let mut created_covers = Vec::new();
        for (staged, target) in &staged_covers {
            if target.exists() {
                let _ = fs::remove_file(staged);
                continue;
            }
            if let Err(err) = fs::rename(staged, target) {
                let _ = fs::remove_file(staged);
                for created in &created_covers {
                    let _ = fs::remove_file(created);
                }
                return LocalTracksResult {
                    ok: false,
                    version: LOCAL_LIBRARY_VERSION,
                    count: 0,
                    tracks: Vec::new(),
                    failures: Some(failures),
                    metadata_warnings: Some(metadata_warnings),
                    error: Some(format!("LOCAL_COVER_COMMIT_FAILED:{err}")),
                };
            }
            created_covers.push(target.clone());
        }

        if let Err(err) = persist_index(
            &self.index_path,
            &guard.media_token,
            &next_order,
            &next_records,
        ) {
            for created in &created_covers {
                let _ = fs::remove_file(created);
            }
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: 0,
                tracks: Vec::new(),
                failures: Some(failures),
                metadata_warnings: Some(metadata_warnings),
                error: Some(err),
            };
        }

        guard.records = next_records;
        guard.order = next_order;
        for old_cover in cleanup_after_commit {
            let _ = fs::remove_file(old_cover);
        }

        Self::build_tracks_result(&guard, Some(failures), Some(metadata_warnings), None)
    }

    /// 从曲库中移除指定曲目 ID，并清理关联的封面缓存文件。
    pub fn remove_tracks(&self, ids: Vec<String>) -> LocalTracksResult {
        let requested: HashSet<String> = ids
            .into_iter()
            .map(|raw| {
                let cleaned = clean_text(&raw, "", 64);
                cleaned
                    .strip_prefix("local:")
                    .unwrap_or(&cleaned)
                    .to_lowercase()
            })
            .filter(|id| is_valid_id24(id))
            .collect();

        let mut guard = self.state.lock().unwrap();
        if requested.is_empty() {
            return Self::build_tracks_result(&guard, None, None, None);
        }

        let mut next_records = guard.records.clone();
        let mut removed = Vec::new();
        for id in &requested {
            if let Some(rec) = next_records.remove(id) {
                removed.push(rec);
            }
        }
        let next_order: Vec<String> = guard
            .order
            .iter()
            .filter(|id| next_records.contains_key(*id))
            .cloned()
            .collect();

        if let Err(err) = persist_index(
            &self.index_path,
            &guard.media_token,
            &next_order,
            &next_records,
        ) {
            return LocalTracksResult {
                ok: false,
                version: LOCAL_LIBRARY_VERSION,
                count: guard.order.len(),
                tracks: Vec::new(),
                failures: None,
                metadata_warnings: None,
                error: Some(err),
            };
        }

        guard.records = next_records;
        guard.order = next_order;
        for rec in removed {
            if !rec.cover_path.is_empty() {
                let _ = fs::remove_file(rec.cover_path);
            }
        }
        Self::build_tracks_result(&guard, None, None, None)
    }

    /// 校验媒体请求类别、曲目 ID 与 capability token，返回本地文件路径与 Content-Type。
    pub fn resolve_media_file(&self, kind: &str, id: &str, cap: &str) -> Option<(PathBuf, String)> {
        if kind != "audio" && kind != "cover" {
            return None;
        }
        let normalized_id = id.trim().to_ascii_lowercase();
        if !is_valid_id24(&normalized_id) {
            return None;
        }
        let guard = self.state.lock().unwrap();
        if !constant_time_eq(cap.trim(), &guard.media_token) {
            return None;
        }
        let record = guard.records.get(&normalized_id)?;
        if kind == "audio" {
            let path = supported_audio_path(&record.audio_path)?;
            if !path.is_file() {
                return None;
            }
            let mime = audio_mime_for_path(&path)?;
            Some((path, mime.to_string()))
        } else {
            if record.cover_path.is_empty() {
                return None;
            }
            let path = normalized_absolute_file_path(&record.cover_path)?;
            if !is_path_inside(&self.cover_directory, &path) || !path.is_file() {
                return None;
            }
            let ext_mime = cover_mime_by_extension(&path)?;
            let mime = if record.cover_mime.is_empty() {
                ext_mime.to_string()
            } else {
                record.cover_mime.clone()
            };
            Some((path, mime))
        }
    }
}

fn prune_capabilities(store: &mut CapabilityStore, now: u64) {
    let expired: Vec<String> = store
        .map
        .iter()
        .filter(|(_, cap)| cap.expires_at <= now)
        .map(|(k, _)| k.clone())
        .collect();
    for k in expired {
        store.map.remove(&k);
    }
    store.fifo.retain(|k| store.map.contains_key(k));
}

fn persist_index(
    index_path: &Path,
    media_token: &str,
    order: &[String],
    records: &HashMap<String, LocalTrackRecord>,
) -> Result<(), String> {
    let ordered_records: Vec<LocalTrackRecord> = order
        .iter()
        .filter_map(|id| records.get(id).cloned())
        .collect();
    let payload = PersistedLocalLibrary {
        version: LOCAL_LIBRARY_VERSION,
        updated_at: now_millis(),
        media_token: media_token.to_string(),
        records: ordered_records,
    };
    let text = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    if text.len() > MAX_LIBRARY_INDEX_BYTES {
        return Err("LOCAL_LIBRARY_INDEX_TOO_LARGE".to_string());
    }
    write_text_atomic(index_path, &text).map_err(|e| e.to_string())
}

fn load_index_from_disk(index_path: &Path, cover_directory: &Path) -> LibraryState {
    let default_token = random_hex_bytes(24);
    let mut state = LibraryState {
        records: HashMap::new(),
        order: Vec::new(),
        media_token: default_token,
    };

    let Ok(meta) = fs::metadata(index_path) else {
        return state;
    };
    if !meta.is_file() || meta.len() == 0 || meta.len() as usize > MAX_LIBRARY_INDEX_BYTES {
        return state;
    }
    let Ok(raw) = fs::read_to_string(index_path) else {
        return state;
    };
    let Ok(parsed) = serde_json::from_str::<PersistedLocalLibrary>(&raw) else {
        return state;
    };
    if parsed.version != LOCAL_LIBRARY_VERSION {
        return state;
    }
    let token_lower = parsed.media_token.to_ascii_lowercase();
    if is_valid_token48(&token_lower) {
        state.media_token = token_lower;
    }

    for source in parsed.records.into_iter().take(MAX_IMPORT_FILES) {
        let Some(audio_path) = supported_audio_path(&source.audio_path) else {
            continue;
        };
        let id = clean_text(&source.id, "", 64).to_lowercase();
        if !is_valid_id24(&id) || id != local_file_id(&audio_path) || state.records.contains_key(&id)
        {
            continue;
        }
        let cover_path = normalized_absolute_file_path(&source.cover_path)
            .filter(|p| is_path_inside(cover_directory, p))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let fallback_name = audio_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fallback_rel = audio_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let lyric_source = match source.lyric_source.as_str() {
            "sidecar" => "sidecar".to_string(),
            "embedded" => "embedded".to_string(),
            _ => String::new(),
        };

        let record = LocalTrackRecord {
            id: id.clone(),
            audio_path: audio_path.to_string_lossy().into_owned(),
            relative_path: clean_text(&source.relative_path, &fallback_rel, 2000),
            name: clean_text(&source.name, &fallback_name, 1000),
            artist: clean_text(&source.artist, "本地文件", 1000),
            album: clean_text(&source.album, "", 1000),
            duration: if source.duration.is_finite() && source.duration > 0.0 {
                source.duration
            } else {
                0.0
            },
            size: source.size,
            mtime_ms: source.mtime_ms,
            revision: clean_text(&source.revision, "", 100),
            cover_path,
            cover_mime: clean_text(&source.cover_mime, "", 100),
            lyric: truncate_utf8_bytes(&clean_text(&source.lyric, "", MAX_LYRIC_BYTES), MAX_LYRIC_BYTES),
            lyric_source,
            imported_at: source.imported_at,
        };
        state.records.insert(id.clone(), record);
        state.order.push(id);
    }

    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, codecs::png::PngEncoder, ColorType};
    use tempfile::TempDir;

    fn build_minimal_wav_1sec() -> Vec<u8> {
        let data_len: u32 = 16000;
        let riff_size: u32 = 36 + data_len;
        let mut buf = Vec::with_capacity(44 + data_len as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        buf.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf.resize(44 + data_len as usize, 0);
        buf
    }

    #[test]
    fn rejects_unc_and_relative_paths() {
        assert!(normalized_absolute_file_path("relative/song.mp3").is_none());
        assert!(normalized_absolute_file_path("./song.wav").is_none());
        assert!(normalized_absolute_file_path(r"\\server\share\song.mp3").is_none());
        assert!(normalized_absolute_file_path("//server/share/song.mp3").is_none());
        assert!(supported_audio_path("/tmp/demo/track.txt").is_none());
        assert!(supported_audio_path("/tmp/demo/track.OPUS").is_some());
    }

    #[test]
    fn cover_budget_checks_dimensions_and_unknown_size() {
        // 合法 2x2 PNG
        let mut png_bytes = Vec::new();
        let pixels = [255u8; 2 * 2 * 4];
        PngEncoder::new(&mut png_bytes)
            .write_image(&pixels, 2, 2, ColorType::Rgba8.into())
            .unwrap();
        assert!(cover_within_budget(&png_bytes, "image/png"));

        // 构造一个真实编码的尺寸超限 PNG（4097 x 100）
        let mut huge_dim_png = Vec::new();
        let huge_pixels = vec![0u8; 4097 * 100 * 4];
        PngEncoder::new(&mut huge_dim_png)
            .write_image(&huge_pixels, 4097, 100, ColorType::Rgba8.into())
            .unwrap();
        assert!(!cover_within_budget(&huge_dim_png, "image/png"));

        // 构造未知格式小图（<= 1 MiB 允许，> 1 MiB 拒绝）
        let small_unknown = vec![0xAB; 512];
        assert!(cover_within_budget(&small_unknown, "image/png"));
        let large_unknown = vec![0xAB; MAX_UNKNOWN_DIMENSION_COVER_BYTES + 1];
        assert!(!cover_within_budget(&large_unknown, "image/png"));
    }

    #[test]
    fn import_wav_with_gb18030_lrc_persist_and_resolve_media() {
        let tmp = TempDir::new().unwrap();
        let user_data = tmp.path().join("userData");
        let music_dir = tmp.path().join("Albums").join("Night");
        fs::create_dir_all(&music_dir).unwrap();

        let wav_path = music_dir.join("测试音轨.wav");
        fs::write(&wav_path, build_minimal_wav_1sec()).unwrap();

        let lrc_path = music_dir.join("测试音轨.lrc");
        let (gb_bytes, _, _) = GB18030.encode("[00:01.00]世界和平测试歌词\n");
        fs::write(&lrc_path, gb_bytes.as_ref()).unwrap();

        let lib = LocalMusicLibrary::new(&user_data);
        let token = lib.media_token();
        assert_eq!(token.len(), 48);

        // 先通过 authorize_files -> import_authorized 导入
        let auth = lib.authorize_files(
            vec![
                LocalImportEntry {
                    path: wav_path.to_string_lossy().into_owned(),
                    relative_path: Some("Night/测试音轨.wav".to_string()),
                },
                LocalImportEntry {
                    path: r"\\evil\share\bad.mp3".to_string(),
                    relative_path: None,
                },
            ],
            false,
        );
        assert!(auth.ok);
        assert_eq!(auth.count, 1);
        let auth_token = auth.token.unwrap();

        let imported = lib.import_authorized(&auth_token);
        assert!(imported.ok);
        assert_eq!(imported.count, 1);
        let track = &imported.tracks[0];
        assert_eq!(track.name, "测试音轨");
        assert_eq!(track.album, "Night");
        assert!(track.duration >= 0.9 && track.duration <= 1.1);
        assert!(track.has_lyric);
        assert_eq!(track.lyric_source, "sidecar");

        // 一次性令牌不能重复使用
        let reused = lib.import_authorized(&auth_token);
        assert!(!reused.ok);
        assert_eq!(
            reused.error.as_deref(),
            Some("LOCAL_IMPORT_CAPABILITY_INVALID")
        );

        // 校验解码后的 GB18030 歌词
        let lyric_res = lib.lyric_for_track(&track.id);
        assert!(lyric_res.ok);
        assert_eq!(lyric_res.lyric, "[00:01.00]世界和平测试歌词");
        assert_eq!(lyric_res.lyric_source, "sidecar");

        // 校验媒体解析与 capability token 鉴权
        assert!(lib
            .resolve_media_file("audio", &track.local_file_id, "wrong-cap")
            .is_none());
        let (resolved_path, mime) = lib
            .resolve_media_file("audio", &track.local_file_id, &token)
            .unwrap();
        assert_eq!(mime, "audio/wav");
        assert!(resolved_path.is_file());

        // 持久化恢复测试
        let reloaded = LocalMusicLibrary::new(&user_data);
        assert_eq!(reloaded.media_token(), token);
        let listed = reloaded.list_tracks();
        assert_eq!(listed.count, 1);
        assert_eq!(listed.tracks[0].id, track.id);
        let reloaded_lyric = reloaded.lyric_for_track(&track.local_file_id);
        assert!(reloaded_lyric.ok);
        assert_eq!(reloaded_lyric.lyric, "[00:01.00]世界和平测试歌词");

        // 移除曲目并验证持久化
        let removed = reloaded.remove_tracks(vec![track.id.clone()]);
        assert!(removed.ok);
        assert_eq!(removed.count, 0);
        let reloaded_after_remove = LocalMusicLibrary::new(&user_data);
        assert_eq!(reloaded_after_remove.list_tracks().count, 0);
    }
}
