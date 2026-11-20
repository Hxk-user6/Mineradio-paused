use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const WALLPAPER_ENGINE_SCHEME: &str = "mineradio-wallpaper";
pub const WALLPAPER_ENGINE_APP_ID: &str = "431960";
const CONFIG_FILE: &str = "wallpaper-engine-library.json";
const MAX_PROJECT_JSON_BYTES: u64 = 1024 * 1024;
const CACHE_TTL_MS: u64 = 30 * 1000;
const MAX_MANUAL_SCAN_DIRS: usize = 4000;
const MAX_SCENE_PROPERTIES: usize = 256;
const MAX_PROPERTY_OPTIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WallpaperFileCapability {
    Image,
    Video,
    Media,
    ScenePackage,
    AnySafe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualRootSummary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenePropertyOption {
    pub label: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenePropertyDescriptor {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub prop_type: String,
    pub value: Value,
    pub audio: bool,
    pub auto_muted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ScenePropertyOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenePropertyAnalysis {
    pub properties: Vec<ScenePropertyDescriptor>,
    pub mute_properties: BTreeMap<String, Value>,
    pub property_count: usize,
    pub audio_property_count: usize,
    pub muted_audio_property_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperProjectItem {
    pub id: String,
    pub title: String,
    pub project_type: String,
    pub media_type: String,
    pub media_animated: bool,
    pub playable: bool,
    pub engine_playable: bool,
    pub preview_only: bool,
    pub has_preview: bool,
    pub preview_animated: bool,
    pub source: String,
    pub source_label: String,
    pub workshop_id: String,
    pub property_count: usize,
    pub audio_property_count: usize,
    pub muted_audio_property_count: usize,
    pub updated_at: u64,
    pub safety_mode: String,
    pub media_url: String,
    pub preview_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperLibrarySnapshot {
    pub ok: bool,
    pub projects: Vec<WallpaperProjectItem>,
    pub count: usize,
    pub dynamic_count: usize,
    pub engine_playable_count: usize,
    pub preview_only_count: usize,
    pub source_count: usize,
    pub manual_roots: Vec<ManualRootSummary>,
    pub scanned_at: u64,
    pub elapsed_ms: u64,
    pub media_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperProjectDetails {
    pub ok: bool,
    pub id: String,
    pub title: String,
    pub project_type: String,
    pub workshop_id: String,
    pub property_count: usize,
    pub audio_property_count: usize,
    pub muted_audio_property_count: usize,
    pub properties: Vec<ScenePropertyDescriptor>,
    pub mute_properties: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperModeStatus {
    pub ok: bool,
    pub supported: bool,
    pub enabled: bool,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workerw_window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LibraryConfig {
    version: u32,
    #[serde(default)]
    manual_roots: Vec<String>,
    #[serde(default)]
    manual_project_files: Vec<String>,
}

#[derive(Debug, Clone)]
struct ProjectRecord {
    project_root: PathBuf,
    #[allow(dead_code)]
    project_file: PathBuf,
    #[allow(dead_code)]
    media: Option<PathBuf>,
    #[allow(dead_code)]
    preview: Option<PathBuf>,
    #[allow(dead_code)]
    scene_package: Option<PathBuf>,
    workshop_id: String,
}

#[derive(Debug, Clone)]
struct DiscoverySource {
    root: PathBuf,
    kind: &'static str,
    label: &'static str,
    direct: bool,
}

struct LibraryState {
    user_data_dir: PathBuf,
    config_path: PathBuf,
    auto_discover: bool,
    manual_roots: Vec<PathBuf>,
    manual_project_files: Vec<PathBuf>,
    index: HashMap<String, ProjectRecord>,
    snapshot: Option<WallpaperLibrarySnapshot>,
    media_token: String,
    wallpaper_mode_enabled: bool,
}

pub struct WallpaperEngineLibrary {
    state: Mutex<LibraryState>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn generate_media_token() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn path_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    raw.trim_end_matches(['/', '\\']).to_lowercase()
}

fn opaque_id(path: &Path) -> String {
    let key = path_key(path);
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest).chars().take(24).collect()
}

fn normalize_absolute_path(input: &Path) -> Option<PathBuf> {
    let raw = input.to_string_lossy().trim().trim_matches('"').to_string();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn sanitize_text(value: Option<&str>, fallback: &str) -> String {
    let cleaned: String = value
        .unwrap_or("")
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if code <= 0x1f || code == 0x7f {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(160).collect();
    if truncated.is_empty() {
        let fb = fallback.trim();
        if fb.is_empty() {
            "Wallpaper Engine".to_string()
        } else {
            fb.chars().take(160).collect()
        }
    } else {
        truncated
    }
}

fn strip_html_tags_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]{0,512}>").expect("valid html regex"))
}

fn sanitize_property_label(value: Option<&str>, fallback: &str) -> String {
    let raw = value.unwrap_or("");
    let no_tags = strip_html_tags_re().replace_all(raw, " ");
    let no_nbsp = no_tags
        .replace("&nbsp;", " ")
        .replace("&NBSP;", " ")
        .replace("&#160;", " ");
    let no_bidi: String = no_nbsp
        .chars()
        .map(|ch| match ch {
            '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}' | '\u{feff}' => ' ',
            _ => ch,
        })
        .collect();
    sanitize_text(Some(&no_bidi), fallback)
}

fn normalize_scene_property_value(value: Option<&Value>, max_len: usize) -> Value {
    match value {
        Some(Value::Bool(b)) => Value::Bool(*b),
        Some(Value::Number(n)) => Value::Number(n.clone()),
        Some(Value::String(s)) => {
            let cleaned: String = s
                .chars()
                .map(|ch| {
                    let code = ch as u32;
                    if code <= 0x1f || code == 0x7f {
                        ' '
                    } else {
                        ch
                    }
                })
                .collect();
            let trimmed: String = cleaned.trim().chars().take(max_len).collect();
            Value::String(trimmed)
        }
        _ => Value::Null,
    }
}

fn is_safe_property_key(key: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9_.-]{1,128}$").expect("valid key regex"));
    if !re.is_match(key) {
        return false;
    }
    !matches!(
        key.to_ascii_lowercase().as_str(),
        "__proto__" | "prototype" | "constructor"
    )
}

pub fn analyze_scene_properties(project: &Value) -> ScenePropertyAnalysis {
    static RE_HINT: OnceLock<Regex> = OnceLock::new();
    static RE_KEY: OnceLock<Regex> = OnceLock::new();
    static RE_STANDALONE: OnceLock<Regex> = OnceLock::new();
    static RE_VISUAL: OnceLock<Regex> = OnceLock::new();
    static RE_DECIBEL: OnceLock<Regex> = OnceLock::new();
    static RE_OFF_OPT: OnceLock<Regex> = OnceLock::new();
    static RE_MUTE: OnceLock<Regex> = OnceLock::new();

    let re_hint = RE_HINT.get_or_init(|| {
        Regex::new(r"(?i)(?:\bvolume\b|\bmute(?:d)?\b|\bsilent\b|\baudio\s*(?:volume|level|gain|enable|enabled|toggle)\b|\bmusic\s*(?:volume|level|gain|size|enable|enabled|toggle)\b|\bsound\s*(?:volume|level|gain|enable|enabled|toggle)\b|音量|静音|无声|音乐(?:大小|音量|开关)|声音(?:大小|音量|开关)|音效(?:大小|音量|开关))").expect("valid hint regex")
    });
    let re_key = RE_KEY.get_or_init(|| {
        Regex::new(r"(?i)^(?:volume|dbvolume|musicvolume|music_volume|audiovolume|audio_volume|soundvolume|sound_volume|bgmvolume|bgm_volume|muteaudio|audiomute|mutemusic|musicmute|music|audio|sound|bgm)$").expect("valid audio key regex")
    });
    let re_standalone = RE_STANDALONE.get_or_init(|| {
        Regex::new(r"(?i)^(?:(?:background\s*(?:audio|music|sound)|背景(?:音频|音乐|声音))|(?:audio|music|sound|bgm|音频|音乐|声音)(?:[\s,，、/_-]*(?:audio|music|sound|bgm|音频|音乐|声音|\d+))*)$").expect("valid standalone regex")
    });
    let re_visual = RE_VISUAL.get_or_init(|| {
        Regex::new(r"(?i)(?:visuali[sz](?:er|ation)|\bbars?\b|\bring\b|\bpulse\b|\bthreshold\b|\bsensitiv(?:e|ity)\b|\bintensity\b|\bcolou?r\b|\bopacity\b|\btransparen(?:cy|t)\b|\bbounce\b|\bflicker\b|\balbum\b|\binformation\b|\bresponse\b|\breactive\b|\bfrequency\b|\bspectrum\b|\bwave\b|\bnote\b|可视|频谱|跳动|闪烁|响应|颜色|透明|专辑|封面|信息)").expect("valid visual regex")
    });
    let re_decibel = RE_DECIBEL
        .get_or_init(|| Regex::new(r"(?i)(?:\bdb\b|decibel|分贝)").expect("valid db regex"));
    let re_off_opt = RE_OFF_OPT.get_or_init(|| {
        Regex::new(
            r"(?i)(?:\bnone\b|\boff\b|\bmute(?:d)?\b|\bsilent\b|\bdisable(?:d)?\b|关闭|静音|无声|不要音乐|无音乐)",
        )
        .expect("valid off option regex")
    });
    let re_mute = RE_MUTE.get_or_init(|| {
        Regex::new(r"(?i)(?:\bmute(?:d)?\b|\bsilent\b|静音|无声)").expect("valid mute regex")
    });

    let mut mute_properties = BTreeMap::new();
    mute_properties.insert("volume".to_string(), Value::from(0));
    let mut descriptors = Vec::new();

    let Some(properties) = project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
    else {
        return ScenePropertyAnalysis {
            properties: descriptors,
            mute_properties,
            property_count: 0,
            audio_property_count: 0,
            muted_audio_property_count: 0,
        };
    };

    let mut audio_property_count = 0usize;
    let mut muted_audio_property_count = 0usize;

    for (raw_key, raw_prop) in properties {
        if descriptors.len() >= MAX_SCENE_PROPERTIES {
            break;
        }
        let key = raw_key.trim();
        let Some(prop) = raw_prop.as_object() else {
            continue;
        };
        if !is_safe_property_key(key) {
            continue;
        }

        let prop_type_raw = prop
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let label = sanitize_property_label(prop.get("text").and_then(|v| v.as_str()), key);
        let hint = format!("{key} {label}");
        let normalized_key: String = key
            .chars()
            .filter(|c| *c != '_' && *c != '.' && *c != '-')
            .collect::<String>()
            .to_ascii_lowercase();

        let exact_audio_key = re_key.is_match(key);
        let is_mute_key = matches!(
            normalized_key.as_str(),
            "muteaudio" | "audiomute" | "mutemusic" | "musicmute"
        );
        let is_decibel_key = normalized_key == "dbvolume";
        let is_enable_key = matches!(
            normalized_key.as_str(),
            "audioenable"
                | "audioenabled"
                | "musicenable"
                | "musicenabled"
                | "soundenable"
                | "soundenabled"
                | "bgmenable"
                | "bgmenabled"
        );
        let explicit_audio_control =
            re_hint.is_match(&hint) || is_mute_key || is_decibel_key || is_enable_key;
        let visual_only = re_visual.is_match(&hint) && !explicit_audio_control;
        let audio_property = !visual_only
            && (exact_audio_key || explicit_audio_control || re_standalone.is_match(&label));

        let options: Vec<ScenePropertyOption> = prop
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .take(MAX_PROPERTY_OPTIONS)
                    .enumerate()
                    .filter_map(|(idx, opt)| {
                        let obj = opt.as_object()?;
                        let opt_val = normalize_scene_property_value(obj.get("value"), 256);
                        if opt_val.is_null() {
                            return None;
                        }
                        let fallback = format!("选项 {}", idx + 1);
                        let opt_label = sanitize_property_label(
                            obj.get("label").and_then(|v| v.as_str()),
                            &fallback,
                        );
                        Some(ScenePropertyOption {
                            label: opt_label,
                            value: opt_val,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let safe_type: String = prop_type_raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(32)
            .collect();
        let safe_type = if safe_type.is_empty() {
            "unknown".to_string()
        } else {
            safe_type
        };

        let minimum = prop
            .get("min")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite());
        let maximum = prop
            .get("max")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite());
        let step = prop
            .get("step")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n > 0.0);

        let prop_val = prop.get("value");
        let normalized_val = normalize_scene_property_value(prop_val, 512);

        let mut mute_value: Option<Value> = None;
        if audio_property {
            audio_property_count += 1;
            if safe_type == "bool" || matches!(prop_val, Some(Value::Bool(_))) {
                let target_bool = is_mute_key || re_mute.is_match(&hint);
                mute_value = Some(Value::Bool(target_bool));
            } else if safe_type == "slider" || matches!(prop_val, Some(Value::Number(_))) {
                if is_decibel_key
                    || re_decibel.is_match(&hint)
                    || (minimum.is_some_and(|m| m < 0.0) && maximum.is_some_and(|m| m <= 0.0))
                {
                    let v = minimum.unwrap_or(-60.0);
                    if let Some(num) = serde_json::Number::from_f64(v) {
                        mute_value = Some(Value::Number(num));
                    }
                } else if minimum.is_none_or(|m| m <= 0.0) && maximum.is_none_or(|m| m >= 0.0) {
                    mute_value = Some(Value::from(0));
                } else if let Some(min_v) = minimum {
                    if let Some(num) = serde_json::Number::from_f64(min_v) {
                        mute_value = Some(Value::Number(num));
                    }
                }
            } else if safe_type == "combo" && !options.is_empty() {
                let matched_opt = options
                    .iter()
                    .find(|opt| re_off_opt.is_match(&opt.label))
                    .or_else(|| {
                        if exact_audio_key {
                            options.iter().find(|opt| match &opt.value {
                                Value::String(s) => s == "0",
                                Value::Number(n) => n.as_i64() == Some(0) || n.as_f64() == Some(0.0),
                                _ => false,
                            })
                        } else {
                            None
                        }
                    });
                if let Some(opt) = matched_opt {
                    mute_value = Some(opt.value.clone());
                }
            }

            if let Some(ref mv) = mute_value {
                mute_properties.insert(key.to_string(), mv.clone());
                muted_audio_property_count += 1;
            }
        }

        descriptors.push(ScenePropertyDescriptor {
            key: key.to_string(),
            label,
            prop_type: safe_type,
            value: normalized_val,
            audio: audio_property,
            auto_muted: mute_value.is_some(),
            mute_value,
            min: minimum,
            max: maximum,
            step,
            options,
        });
    }

    ScenePropertyAnalysis {
        property_count: descriptors.len(),
        properties: descriptors,
        mute_properties,
        audio_property_count,
        muted_audio_property_count,
    }
}

fn extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| format!(".{}", s.to_ascii_lowercase()))
        .unwrap_or_default()
}

fn is_image_ext(ext: &str) -> bool {
    matches!(ext, ".jpg" | ".jpeg" | ".png" | ".webp" | ".gif")
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext, ".mp4" | ".webm" | ".m4v" | ".mov")
}

fn is_scene_package_ext(ext: &str) -> bool {
    matches!(ext, ".pkg" | ".pak")
}

fn capability_allows_ext(cap: WallpaperFileCapability, ext: &str) -> bool {
    match cap {
        WallpaperFileCapability::Image => is_image_ext(ext),
        WallpaperFileCapability::Video => is_video_ext(ext),
        WallpaperFileCapability::Media => is_image_ext(ext) || is_video_ext(ext),
        WallpaperFileCapability::ScenePackage => is_scene_package_ext(ext),
        WallpaperFileCapability::AnySafe => {
            is_image_ext(ext) || is_video_ext(ext) || is_scene_package_ext(ext)
        }
    }
}

fn resolve_safe_file_inside_root(
    project_root: &Path,
    rel_path: &str,
    cap: WallpaperFileCapability,
) -> Result<PathBuf, String> {
    let trimmed = rel_path.trim();
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains(':') {
        return Err("WALLPAPER_FILE_PATH_INVALID".to_string());
    }
    let normalized_rel = trimmed.replace('\\', "/");
    let candidate_rel = Path::new(&normalized_rel);
    if candidate_rel.is_absolute() {
        return Err("WALLPAPER_FILE_PATH_ABSOLUTE_FORBIDDEN".to_string());
    }
    for comp in candidate_rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("WALLPAPER_FILE_PATH_TRAVERSAL_FORBIDDEN".to_string());
            }
        }
    }

    let ext = extension_lower(candidate_rel);
    if !capability_allows_ext(cap, &ext) {
        return Err("WALLPAPER_FILE_EXTENSION_FORBIDDEN".to_string());
    }

    let real_root = fs::canonicalize(project_root)
        .map_err(|_| "WALLPAPER_PROJECT_ROOT_INVALID".to_string())?;
    let lexical_target = real_root.join(candidate_rel);
    let real_target =
        fs::canonicalize(&lexical_target).map_err(|_| "WALLPAPER_FILE_NOT_FOUND".to_string())?;

    if !real_target.starts_with(&real_root) {
        return Err("WALLPAPER_FILE_SYMLINK_ESCAPE_FORBIDDEN".to_string());
    }
    let meta = fs::metadata(&real_target).map_err(|_| "WALLPAPER_FILE_NOT_FOUND".to_string())?;
    if !meta.is_file() {
        return Err("WALLPAPER_FILE_NOT_A_FILE".to_string());
    }
    Ok(real_target)
}

fn validate_scene_package(path: &Path) -> bool {
    let ext = extension_lower(path);
    if !is_scene_package_ext(&ext) {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut header = [0u8; 12];
    if file.read_exact(&mut header).is_err() {
        return false;
    }
    let is_pkgv_header = |slice: &[u8]| {
        slice.len() == 8 && &slice[0..4] == b"PKGV" && slice[4..8].iter().all(|b| b.is_ascii_digit())
    };
    is_pkgv_header(&header[0..8]) || is_pkgv_header(&header[4..12])
}

fn read_project_manifest(project_root: &Path) -> Option<(Map<String, Value>, PathBuf, u64)> {
    let file_path = project_root.join("project.json");
    let meta = fs::metadata(&file_path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_PROJECT_JSON_BYTES {
        return None;
    }
    let real_root = fs::canonicalize(project_root).ok()?;
    let real_file = fs::canonicalize(&file_path).ok()?;
    if !real_file.starts_with(&real_root) {
        return None;
    }
    let raw_text = fs::read_to_string(&real_file).ok()?;
    let stripped = raw_text.strip_prefix('\u{feff}').unwrap_or(&raw_text);
    let parsed: Value = serde_json::from_str(stripped).ok()?;
    let obj = parsed.as_object()?.clone();
    let updated_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((obj, real_file, updated_at))
}

fn derive_workshop_id(project: &Map<String, Value>, project_root: &Path, source_kind: &str) -> String {
    static RE_DIGITS: OnceLock<Regex> = OnceLock::new();
    static RE_URL: OnceLock<Regex> = OnceLock::new();
    let re_digits =
        RE_DIGITS.get_or_init(|| Regex::new(r"^\d{5,32}$").expect("valid workshop id regex"));
    let re_url = RE_URL.get_or_init(|| {
        Regex::new(r"(?i)(?:[?&]id=|/filedetails/?)(\d{5,32})").expect("valid url id regex")
    });

    for key in ["workshopid", "workshopId", "publishedfileid", "publishedFileId"] {
        if let Some(val) = project.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }) {
            if re_digits.is_match(&val) {
                return val;
            }
        }
    }

    for key in ["workshopurl", "workshopUrl", "url"] {
        if let Some(s) = project.get(key).and_then(|v| v.as_str()) {
            if let Some(cap) = re_url.captures(s) {
                if let Some(m) = cap.get(1) {
                    return m.as_str().to_string();
                }
            }
        }
    }

    if source_kind == "workshop" {
        if let Some(name) = project_root.file_name().and_then(|n| n.to_str()) {
            if re_digits.is_match(name) {
                return name.to_string();
            }
        }
    }
    String::new()
}

fn first_project_file(
    project_root: &Path,
    candidates: &[String],
    cap: WallpaperFileCapability,
) -> Option<PathBuf> {
    for rel in candidates {
        if rel.trim().is_empty() {
            continue;
        }
        if let Ok(resolved) = resolve_safe_file_inside_root(project_root, rel, cap) {
            return Some(resolved);
        }
    }
    None
}

fn build_wallpaper_media_url(
    id: &str,
    real_root: &Path,
    target_file: Option<&Path>,
    media_token: &str,
) -> String {
    let Some(file) = target_file else {
        return String::new();
    };
    if media_token.is_empty() {
        return String::new();
    }
    let Ok(rel) = file.strip_prefix(real_root) else {
        return String::new();
    };
    let rel_str = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if rel_str.is_empty() {
        return String::new();
    }
    let encoded_rel =
        percent_encoding::utf8_percent_encode(&rel_str, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    format!("{WALLPAPER_ENGINE_SCHEME}://media/{id}?path={encoded_rel}&cap={media_token}")
}

fn index_project(
    project_root: &Path,
    source: &DiscoverySource,
    scene_package_override: Option<&Path>,
    media_token: &str,
) -> Option<(WallpaperProjectItem, ProjectRecord)> {
    let (project_obj, real_manifest_file, updated_at) = read_project_manifest(project_root)?;
    let project_val = Value::Object(project_obj.clone());

    let project_type = project_obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let file_field = project_obj
        .get("file")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let direct_ext = extension_lower(Path::new(&file_field));
    let inferred_media = if is_video_ext(&direct_ext) {
        "video"
    } else if is_image_ext(&direct_ext) {
        "image"
    } else {
        ""
    };

    // 按迁移契约过滤出可识别的 video / scene（并兼容无 type 但指向视频/图片的项目）
    let is_video_or_image =
        project_type == "video" || project_type == "image" || (project_type.is_empty() && !inferred_media.is_empty());
    let is_scene = project_type == "scene";
    if !is_video_or_image && !is_scene {
        return None;
    }

    let media = if is_video_or_image {
        first_project_file(
            project_root,
            std::slice::from_ref(&file_field),
            WallpaperFileCapability::Media,
        )
    } else {
        None
    };

    let scene_package = if is_scene {
        let override_rel = scene_package_override
            .and_then(|p| p.strip_prefix(project_root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let candidates = vec![
            override_rel,
            if is_scene_package_ext(&direct_ext) {
                file_field.clone()
            } else {
                String::new()
            },
            "scene.pkg".to_string(),
            "scene.pak".to_string(),
        ];
        first_project_file(project_root, &candidates, WallpaperFileCapability::ScenePackage)
            .filter(|p| validate_scene_package(p))
    } else {
        None
    };

    let mut preview_candidates = Vec::new();
    for key in ["preview", "cover", "poster"] {
        if let Some(s) = project_obj.get(key).and_then(|v| v.as_str()) {
            preview_candidates.push(s.to_string());
        }
    }
    for name in [
        "preview.jpg",
        "preview.jpeg",
        "preview.png",
        "preview.webp",
        "preview.gif",
        "cover.jpg",
        "cover.png",
        "cover.webp",
        "cover.gif",
    ] {
        preview_candidates.push(name.to_string());
    }
    let preview = first_project_file(
        project_root,
        &preview_candidates,
        WallpaperFileCapability::Image,
    );

    if media.is_none() && preview.is_none() && scene_package.is_none() {
        return None;
    }

    let real_root = fs::canonicalize(project_root).ok()?;
    let id = opaque_id(project_root);
    let media_ext = media
        .as_deref()
        .map(extension_lower)
        .unwrap_or_default();
    let preview_ext = preview
        .as_deref()
        .map(extension_lower)
        .unwrap_or_default();
    let media_type = if is_video_ext(&media_ext) {
        "video".to_string()
    } else if is_image_ext(&media_ext) {
        "image".to_string()
    } else {
        String::new()
    };
    let safe_project_type = if !project_type.is_empty() {
        project_type.clone()
    } else if !media_type.is_empty() {
        media_type.clone()
    } else {
        "unknown".to_string()
    };

    let engine_playable = scene_package.is_some();
    let playable = media.is_some();
    let preview_only = !playable && !engine_playable;
    let property_analysis = if is_scene {
        analyze_scene_properties(&project_val)
    } else {
        ScenePropertyAnalysis {
            properties: Vec::new(),
            mute_properties: BTreeMap::new(),
            property_count: 0,
            audio_property_count: 0,
            muted_audio_property_count: 0,
        }
    };

    let fallback_title = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Wallpaper Engine");
    let title = sanitize_text(
        project_obj.get("title").and_then(|v| v.as_str()),
        fallback_title,
    );
    let workshop_id = derive_workshop_id(&project_obj, project_root, source.kind);
    let safety_mode = if playable {
        "direct-media"
    } else if engine_playable {
        "native-engine"
    } else {
        "preview-only"
    }
    .to_string();

    let media_url = build_wallpaper_media_url(&id, &real_root, media.as_deref(), media_token);
    let preview_url = build_wallpaper_media_url(&id, &real_root, preview.as_deref(), media_token);

    let item = WallpaperProjectItem {
        id: id.clone(),
        title,
        project_type: safe_project_type,
        media_type,
        media_animated: media_ext == ".gif",
        playable,
        engine_playable,
        preview_only,
        has_preview: preview.is_some(),
        preview_animated: preview_ext == ".gif",
        source: source.kind.to_string(),
        source_label: source.label.to_string(),
        workshop_id: workshop_id.clone(),
        property_count: property_analysis.property_count,
        audio_property_count: property_analysis.audio_property_count,
        muted_audio_property_count: property_analysis.muted_audio_property_count,
        updated_at,
        safety_mode,
        media_url,
        preview_url,
    };

    let record = ProjectRecord {
        project_root: real_root,
        project_file: real_manifest_file,
        media,
        preview,
        scene_package,
        workshop_id,
    };

    Some((item, record))
}

fn known_wallpaper_containers(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("steamapps")
            .join("workshop")
            .join("content")
            .join(WALLPAPER_ENGINE_APP_ID),
        root.join("steamapps")
            .join("common")
            .join("wallpaper_engine")
            .join("projects")
            .join("myprojects"),
    ]
}

fn direct_project_directories(container: &Path) -> Vec<PathBuf> {
    let mut output = Vec::new();
    let Ok(entries) = fs::read_dir(container) else {
        return output;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("project.json").is_file() {
            output.push(path);
        }
    }
    output
}

fn manual_project_directories(root: &Path) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    if root.join("project.json").is_file() {
        return vec![root.to_path_buf()];
    }

    let mut known = Vec::new();
    for container in known_wallpaper_containers(root) {
        if container.is_dir() {
            known.extend(direct_project_directories(&container));
        }
    }
    if !known.is_empty() {
        return known;
    }

    let mut output = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0usize));
    let mut visited = 0usize;

    while let Some((dir, depth)) = queue.pop_front() {
        if visited >= MAX_MANUAL_SCAN_DIRS {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if name.starts_with('.')
                || matches!(lower.as_str(), "node_modules" | "cache" | "temp" | "tmp")
            {
                continue;
            }
            if path.join("project.json").is_file() {
                output.push(path);
            } else if depth < 2 {
                queue.push_back((path, depth + 1));
            }
            if visited >= MAX_MANUAL_SCAN_DIRS {
                break;
            }
        }
    }

    output
}

fn read_steam_library_folders(steam_root: &Path) -> Vec<PathBuf> {
    static RE_PATH: OnceLock<Regex> = OnceLock::new();
    static RE_NUM: OnceLock<Regex> = OnceLock::new();
    let re_path = RE_PATH.get_or_init(|| {
        Regex::new(r#"(?i)"path"\s+"([^"]+)""#).expect("valid vdf path regex")
    });
    let re_num = RE_NUM.get_or_init(|| {
        Regex::new(r#"(?i)"\d+"\s+"([a-z]:\\{1,2}[^"]+)""#).expect("valid vdf num regex")
    });

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    let mut push_unique = |p: PathBuf| {
        let key = path_key(&p);
        if !key.is_empty() && seen.insert(key) {
            roots.push(p);
        }
    };
    push_unique(steam_root.to_path_buf());

    for file in [
        steam_root.join("steamapps").join("libraryfolders.vdf"),
        steam_root.join("config").join("libraryfolders.vdf"),
    ] {
        let Ok(raw) = fs::read_to_string(&file) else {
            continue;
        };
        let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        for cap in re_path.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let normalized = m.as_str().replace("\\\\", "\\");
                if !normalized.is_empty() {
                    push_unique(PathBuf::from(normalized));
                }
            }
        }
        for cap in re_num.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let normalized = m.as_str().replace("\\\\", "\\");
                if !normalized.is_empty() {
                    push_unique(PathBuf::from(normalized));
                }
            }
        }
    }
    roots
}

fn discover_steam_libraries() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for env_key in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Ok(val) = std::env::var(env_key) {
            if !val.trim().is_empty() {
                candidates.push(PathBuf::from(val).join("Steam"));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        for raw in [
            r"C:\Program Files (x86)\Steam",
            r"C:\Program Files\Steam",
            r"D:\Steam",
            r"D:\SteamLibrary",
            r"E:\Steam",
            r"E:\SteamLibrary",
            r"F:\Steam",
            r"F:\SteamLibrary",
        ] {
            candidates.push(PathBuf::from(raw));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Library/Application Support/Steam"));
            candidates.push(home.join(".local/share/Steam"));
            candidates.push(home.join(".steam/steam"));
        }
    }

    let mut libraries = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !candidate.is_dir() {
            continue;
        }
        for lib in read_steam_library_folders(&candidate) {
            if lib.is_dir() {
                let key = path_key(&lib);
                if seen.insert(key) {
                    libraries.push(lib);
                }
            }
        }
    }
    libraries
}

fn read_config(config_path: &Path) -> LibraryConfig {
    let Ok(raw) = fs::read_to_string(config_path) else {
        return LibraryConfig {
            version: 2,
            ..Default::default()
        };
    };
    let Ok(parsed) = serde_json::from_str::<LibraryConfig>(&raw) else {
        return LibraryConfig {
            version: 2,
            ..Default::default()
        };
    };
    let mut seen_roots = HashSet::new();
    let manual_roots = parsed
        .manual_roots
        .into_iter()
        .filter_map(|p| normalize_absolute_path(Path::new(&p)))
        .filter(|p| seen_roots.insert(path_key(p)))
        .take(32)
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let mut seen_files = HashSet::new();
    let manual_project_files = parsed
        .manual_project_files
        .into_iter()
        .filter_map(|p| normalize_absolute_path(Path::new(&p)))
        .filter(|p| seen_files.insert(path_key(p)))
        .take(64)
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    LibraryConfig {
        version: 2,
        manual_roots,
        manual_project_files,
    }
}

fn save_config(state: &LibraryState) -> Result<(), String> {
    if let Some(parent) = state.config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let config = LibraryConfig {
        version: 2,
        manual_roots: state
            .manual_roots
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        manual_project_files: state
            .manual_project_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
    };
    let payload = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    let temp = state.config_path.with_extension("json.tmp");
    fs::write(&temp, payload).map_err(|e| e.to_string())?;
    if fs::rename(&temp, &state.config_path).is_err() {
        fs::copy(&temp, &state.config_path).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&temp);
    }
    Ok(())
}

impl WallpaperEngineLibrary {
    pub fn new(user_data_dir: impl AsRef<Path>) -> Self {
        Self::with_auto_discover(user_data_dir, true)
    }

    pub fn with_auto_discover(user_data_dir: impl AsRef<Path>, auto_discover: bool) -> Self {
        let dir = normalize_absolute_path(user_data_dir.as_ref())
            .unwrap_or_else(|| PathBuf::from("."));
        let config_path = dir.join(CONFIG_FILE);
        let config = read_config(&config_path);
        let manual_roots = config
            .manual_roots
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let manual_project_files = config
            .manual_project_files
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Self {
            state: Mutex::new(LibraryState {
                user_data_dir: dir,
                config_path,
                auto_discover,
                manual_roots,
                manual_project_files,
                index: HashMap::new(),
                snapshot: None,
                media_token: generate_media_token(),
                wallpaper_mode_enabled: false,
            }),
        }
    }

    pub fn user_data_dir(&self) -> PathBuf {
        self.state
            .lock()
            .expect("wallpaper library mutex poisoned")
            .user_data_dir
            .clone()
    }

    fn perform_scan_locked(state: &mut LibraryState) -> WallpaperLibrarySnapshot {
        let started_at = now_millis();
        let mut sources = Vec::new();
        let mut seen_sources = HashSet::new();

        if state.auto_discover {
            for lib in discover_steam_libraries() {
                for container in known_wallpaper_containers(&lib) {
                    if !container.is_dir() {
                        continue;
                    }
                    let key = path_key(&container);
                    if !seen_sources.insert(key) {
                        continue;
                    }
                    let is_workshop = container
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains("workshop");
                    sources.push(DiscoverySource {
                        root: container,
                        kind: if is_workshop { "workshop" } else { "local" },
                        label: if is_workshop {
                            "Steam 创意工坊"
                        } else {
                            "Wallpaper Engine 本地项目"
                        },
                        direct: true,
                    });
                }
            }
        }

        for root in &state.manual_roots {
            let key = path_key(root);
            if !root.is_dir() || !seen_sources.insert(key) {
                continue;
            }
            sources.push(DiscoverySource {
                root: root.clone(),
                kind: "imported",
                label: "手动导入",
                direct: false,
            });
        }

        let mut manual_package_by_root: HashMap<String, PathBuf> = HashMap::new();
        for file in &state.manual_project_files {
            if let Some(parent) = file.parent() {
                manual_package_by_root.insert(path_key(parent), file.clone());
            }
        }

        let mut discovered_projects: Vec<(PathBuf, DiscoverySource)> = Vec::new();
        let mut seen_projects = HashSet::new();
        for source in &sources {
            let dirs = if source.direct {
                direct_project_directories(&source.root)
            } else {
                manual_project_directories(&source.root)
            };
            for proj_root in dirs {
                let key = path_key(&proj_root);
                if seen_projects.insert(key) {
                    discovered_projects.push((proj_root, source.clone()));
                }
            }
        }

        let mut items = Vec::new();
        let mut next_index = HashMap::new();
        for (proj_root, source) in discovered_projects {
            let override_pkg = manual_package_by_root.get(&path_key(&proj_root)).map(|p| p.as_path());
            if let Some((item, record)) =
                index_project(&proj_root, &source, override_pkg, &state.media_token)
            {
                if !next_index.contains_key(&item.id) {
                    next_index.insert(item.id.clone(), record);
                    items.push(item);
                }
            }
        }

        items.sort_by(|a, b| {
            b.playable
                .cmp(&a.playable)
                .then_with(|| b.engine_playable.cmp(&a.engine_playable))
                .then_with(|| a.title.cmp(&b.title))
        });

        let dynamic_count = items
            .iter()
            .filter(|i| i.playable && i.media_type == "video")
            .count();
        let engine_playable_count = items.iter().filter(|i| i.engine_playable).count();
        let preview_only_count = items.iter().filter(|i| i.preview_only).count();
        let manual_roots = state
            .manual_roots
            .iter()
            .map(|root| ManualRootSummary {
                id: opaque_id(root),
                name: root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("导入目录")
                    .to_string(),
            })
            .collect();

        let scanned_at = now_millis();
        let snapshot = WallpaperLibrarySnapshot {
            ok: true,
            count: items.len(),
            projects: items,
            dynamic_count,
            engine_playable_count,
            preview_only_count,
            source_count: sources.len(),
            manual_roots,
            scanned_at,
            elapsed_ms: scanned_at.saturating_sub(started_at),
            media_token: state.media_token.clone(),
        };

        state.index = next_index;
        state.snapshot = Some(snapshot.clone());
        snapshot
    }

    pub fn list_projects(&self, force_refresh: bool) -> WallpaperLibrarySnapshot {
        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        if !force_refresh {
            if let Some(ref cached) = state.snapshot {
                if now_millis().saturating_sub(cached.scanned_at) < CACHE_TTL_MS {
                    return cached.clone();
                }
            }
        }
        Self::perform_scan_locked(&mut state)
    }

    pub fn add_directory(&self, dir: impl AsRef<Path>) -> Result<WallpaperLibrarySnapshot, String> {
        let normalized = normalize_absolute_path(dir.as_ref())
            .filter(|p| p.is_dir())
            .ok_or_else(|| "请选择存在的 Wallpaper Engine 项目目录".to_string())?;
        let found = manual_project_directories(&normalized);
        if found.is_empty() {
            return Err("所选目录中没有识别到 project.json".to_string());
        }

        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        let target_key = path_key(&normalized);
        if !state
            .manual_roots
            .iter()
            .any(|existing| path_key(existing) == target_key)
        {
            state.manual_roots.push(normalized);
            if state.manual_roots.len() > 32 {
                let excess = state.manual_roots.len() - 32;
                state.manual_roots.drain(0..excess);
            }
            save_config(&state)?;
        }
        Ok(Self::perform_scan_locked(&mut state))
    }

    pub fn add_manual_project_file(
        &self,
        file: impl AsRef<Path>,
    ) -> Result<WallpaperLibrarySnapshot, String> {
        let normalized = normalize_absolute_path(file.as_ref())
            .filter(|p| p.is_file())
            .ok_or_else(|| "请选择存在的 Wallpaper Engine 项目文件".to_string())?;

        if normalized
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("project.json"))
        {
            let parent = normalized
                .parent()
                .ok_or_else(|| "无效的 project.json 路径".to_string())?;
            return self.add_directory(parent);
        }

        let ext = extension_lower(&normalized);
        if !is_scene_package_ext(&ext) {
            return Err("请选择 project.json 或 Wallpaper Engine 场景包（.pkg/.pak）".to_string());
        }
        if !validate_scene_package(&normalized) {
            return Err(
                "所选文件不是有效的 Wallpaper Engine PKGV 场景包；普通游戏或 Chromium .pak 无法运行"
                    .to_string(),
            );
        }

        let project_root = normalized
            .parent()
            .ok_or_else(|| "场景包路径无效".to_string())?
            .to_path_buf();
        let (manifest, _, _) = read_project_manifest(&project_root)
            .ok_or_else(|| "场景包同目录缺少有效的 Scene project.json".to_string())?;
        let is_scene = manifest
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("scene"));
        if !is_scene {
            return Err("场景包同目录缺少有效的 Scene project.json".to_string());
        }

        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        let root_key = path_key(&project_root);
        if !state
            .manual_roots
            .iter()
            .any(|existing| path_key(existing) == root_key)
        {
            state.manual_roots.push(project_root);
            if state.manual_roots.len() > 32 {
                let excess = state.manual_roots.len() - 32;
                state.manual_roots.drain(0..excess);
            }
        }
        let file_key = path_key(&normalized);
        if !state
            .manual_project_files
            .iter()
            .any(|existing| path_key(existing) == file_key)
        {
            state.manual_project_files.push(normalized);
            if state.manual_project_files.len() > 64 {
                let excess = state.manual_project_files.len() - 64;
                state.manual_project_files.drain(0..excess);
            }
        }
        save_config(&state)?;
        Ok(Self::perform_scan_locked(&mut state))
    }

    pub fn remove_directory(&self, root_id: &str) -> Result<WallpaperLibrarySnapshot, String> {
        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        let target_id = root_id.trim();
        let removed_roots: Vec<PathBuf> = state
            .manual_roots
            .iter()
            .filter(|r| opaque_id(r) == target_id)
            .cloned()
            .collect();

        if !removed_roots.is_empty() {
            state.manual_roots.retain(|r| opaque_id(r) != target_id);
            state.manual_project_files.retain(|file| {
                !removed_roots
                    .iter()
                    .any(|root| file.starts_with(root))
            });
            save_config(&state)?;
        }
        Ok(Self::perform_scan_locked(&mut state))
    }

    pub fn get_project_details(&self, id: &str) -> Result<WallpaperProjectDetails, String> {
        let normalized_id = id.trim().to_ascii_lowercase();
        if normalized_id.len() != 24 || !normalized_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("WALLPAPER_PROJECT_ID_INVALID".to_string());
        }

        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        if state.snapshot.is_none() {
            Self::perform_scan_locked(&mut state);
        }
        let record = state
            .index
            .get(&normalized_id)
            .cloned()
            .ok_or_else(|| "WALLPAPER_PROJECT_NOT_FOUND".to_string())?;
        drop(state);

        let (project_obj, _, _) = read_project_manifest(&record.project_root)
            .ok_or_else(|| "WALLPAPER_PROJECT_MANIFEST_INVALID".to_string())?;
        let project_val = Value::Object(project_obj.clone());
        let analysis = analyze_scene_properties(&project_val);
        let fallback_title = record
            .project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Wallpaper Engine");
        let title = sanitize_text(
            project_obj.get("title").and_then(|v| v.as_str()),
            fallback_title,
        );
        let project_type: String = project_obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(32)
            .collect();
        let project_type = if project_type.is_empty() {
            "unknown".to_string()
        } else {
            project_type
        };
        let workshop_id = if record.workshop_id.is_empty() {
            derive_workshop_id(&project_obj, &record.project_root, "")
        } else {
            record.workshop_id
        };

        Ok(WallpaperProjectDetails {
            ok: true,
            id: normalized_id,
            title,
            project_type,
            workshop_id,
            property_count: analysis.property_count,
            audio_property_count: analysis.audio_property_count,
            muted_audio_property_count: analysis.muted_audio_property_count,
            properties: analysis.properties,
            mute_properties: analysis.mute_properties,
        })
    }

    pub fn resolve_project_file(
        &self,
        id: &str,
        rel_path: &str,
        cap: WallpaperFileCapability,
    ) -> Result<PathBuf, String> {
        let normalized_id = id.trim().to_ascii_lowercase();
        if normalized_id.len() != 24 || !normalized_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("WALLPAPER_PROJECT_ID_INVALID".to_string());
        }

        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        if state.snapshot.is_none() {
            Self::perform_scan_locked(&mut state);
        }
        let record = state
            .index
            .get(&normalized_id)
            .cloned()
            .ok_or_else(|| "WALLPAPER_PROJECT_NOT_FOUND".to_string())?;
        drop(state);

        resolve_safe_file_inside_root(&record.project_root, rel_path, cap)
    }

    pub fn get_wallpaper_mode_status(&self) -> WallpaperModeStatus {
        let _state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        #[cfg(target_os = "windows")]
        {
            let workerw = crate::desktop::win32::find_workerw()
                .ok()
                .map(|hwnd| hwnd.to_string());
            WallpaperModeStatus {
                ok: true,
                supported: true,
                enabled: state.wallpaper_mode_enabled,
                platform: "win32".to_string(),
                workerw_window_id: workerw,
                message: None,
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            WallpaperModeStatus {
                ok: true,
                supported: false,
                enabled: false,
                platform: std::env::consts::OS.to_string(),
                workerw_window_id: None,
                message: Some("Wallpaper desktop WorkerW mode is Windows-only.".to_string()),
            }
        }
    }

    pub fn set_wallpaper_mode(&self, enabled: bool) -> Result<WallpaperModeStatus, String> {
        let mut state = self
            .state
            .lock()
            .expect("wallpaper library mutex poisoned");
        #[cfg(target_os = "windows")]
        {
            let workerw = if enabled {
                Some(crate::desktop::win32::find_workerw()?.to_string())
            } else {
                None
            };
            state.wallpaper_mode_enabled = enabled;
            Ok(WallpaperModeStatus {
                ok: true,
                supported: true,
                enabled,
                platform: "win32".to_string(),
                workerw_window_id: workerw,
                message: None,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = enabled;
            state.wallpaper_mode_enabled = false;
            Ok(WallpaperModeStatus {
                ok: false,
                supported: false,
                enabled: false,
                platform: std::env::consts::OS.to_string(),
                workerw_window_id: None,
                message: Some("Wallpaper desktop WorkerW mode is Windows-only.".to_string()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn create_sample_video_project(base: &Path, folder: &str) -> PathBuf {
        let proj = base.join(folder);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("loop.mp4"), b"fake-mp4-data").unwrap();
        fs::write(proj.join("preview.jpg"), b"fake-jpg-data").unwrap();
        fs::write(
            proj.join("project.json"),
            json!({
                "title": "Cyber Rain",
                "type": "video",
                "file": "loop.mp4",
                "preview": "preview.jpg",
                "workshopid": "123456789"
            })
            .to_string(),
        )
        .unwrap();
        proj
    }

    fn create_sample_scene_project(base: &Path, folder: &str) -> PathBuf {
        let proj = base.join(folder);
        fs::create_dir_all(&proj).unwrap();
        let mut pkg_bytes = b"PKGV0001".to_vec();
        pkg_bytes.extend_from_slice(b"dummy-scene-body");
        fs::write(proj.join("scene.pkg"), pkg_bytes).unwrap();
        fs::write(proj.join("preview.png"), b"fake-png").unwrap();
        fs::write(
            proj.join("project.json"),
            json!({
                "title": "Neon City Scene",
                "type": "scene",
                "file": "scene.json",
                "preview": "preview.png",
                "general": {
                    "properties": {
                        "musicVolume": {
                            "text": "Music Volume",
                            "type": "slider",
                            "value": 80,
                            "min": 0,
                            "max": 100
                        },
                        "muteAudio": {
                            "text": "Mute Audio",
                            "type": "bool",
                            "value": false
                        },
                        "audioVisualizerColor": {
                            "text": "Audio Visualizer Color",
                            "type": "color",
                            "value": "1 1 1"
                        },
                        "dbVolume": {
                            "text": "Gain (dB)",
                            "type": "slider",
                            "value": -12,
                            "min": -60,
                            "max": 0
                        },
                        "bgmMode": {
                            "text": "背景音乐开关",
                            "type": "combo",
                            "value": "1",
                            "options": [
                                { "label": "开启", "value": "1" },
                                { "label": "静音", "value": "0" }
                            ]
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        proj
    }

    #[test]
    fn analyzes_scene_mute_properties_and_ignores_visual_only_properties() {
        let project = json!({
            "type": "scene",
            "general": {
                "properties": {
                    "musicVolume": { "text": "Music Volume", "type": "slider", "value": 80, "min": 0, "max": 100 },
                    "muteAudio": { "text": "Mute Audio", "type": "bool", "value": false },
                    "audioEnable": { "text": "Enable Audio", "type": "bool", "value": true },
                    "dbVolume": { "text": "Gain (dB)", "type": "slider", "value": -10, "min": -60, "max": 0 },
                    "visualizerBars": { "text": "Audio Visualizer Bars", "type": "slider", "value": 32, "min": 8, "max": 64 }
                }
            }
        });

        let analysis = analyze_scene_properties(&project);
        assert_eq!(analysis.property_count, 5);
        assert_eq!(analysis.audio_property_count, 4);
        assert_eq!(analysis.muted_audio_property_count, 4);

        assert_eq!(analysis.mute_properties.get("volume"), Some(&json!(0)));
        assert_eq!(analysis.mute_properties.get("musicVolume"), Some(&json!(0)));
        assert_eq!(analysis.mute_properties.get("muteAudio"), Some(&json!(true)));
        assert_eq!(analysis.mute_properties.get("audioEnable"), Some(&json!(false)));
        assert_eq!(analysis.mute_properties.get("dbVolume"), Some(&json!(-60.0)));
        assert!(!analysis.mute_properties.contains_key("visualizerBars"));
    }

    #[test]
    fn scans_video_and_scene_projects_and_filters_unsafe_types() {
        let user_dir = TempDir::new().unwrap();
        let library_root = TempDir::new().unwrap();

        create_sample_video_project(library_root.path(), "video_wp");
        create_sample_scene_project(library_root.path(), "scene_wp");

        let web_wp = library_root.path().join("web_wp");
        fs::create_dir_all(&web_wp).unwrap();
        fs::write(web_wp.join("index.html"), b"<html></html>").unwrap();
        fs::write(
            web_wp.join("project.json"),
            json!({ "title": "Web Wallpaper", "type": "web", "file": "index.html" }).to_string(),
        )
        .unwrap();

        let lib = WallpaperEngineLibrary::with_auto_discover(user_dir.path(), false);
        let snap = lib.add_directory(library_root.path()).unwrap();
        assert_eq!(snap.count, 2);
        assert_eq!(snap.dynamic_count, 1);
        assert_eq!(snap.engine_playable_count, 1);

        let video_item = snap
            .projects
            .iter()
            .find(|p| p.project_type == "video")
            .unwrap();
        assert_eq!(
            video_item.media_url,
            format!(
                "mineradio-wallpaper://media/{}?path=loop%2Emp4&cap={}",
                video_item.id, snap.media_token
            )
        );
        assert_eq!(
            video_item.preview_url,
            format!(
                "mineradio-wallpaper://media/{}?path=preview%2Ejpg&cap={}",
                video_item.id, snap.media_token
            )
        );

        let scene_item = snap
            .projects
            .iter()
            .find(|p| p.project_type == "scene")
            .unwrap();
        assert!(scene_item.media_url.is_empty());
        assert_eq!(
            scene_item.preview_url,
            format!(
                "mineradio-wallpaper://media/{}?path=preview%2Epng&cap={}",
                scene_item.id, snap.media_token
            )
        );
        let details = lib.get_project_details(&scene_item.id).unwrap();
        assert_eq!(details.audio_property_count, 4);
        assert_eq!(details.muted_audio_property_count, 4);
        assert_eq!(details.mute_properties.get("bgmMode"), Some(&json!("0")));
    }

    #[test]
    fn enforces_safe_path_resolution_against_traversal_and_bad_extensions() {
        let user_dir = TempDir::new().unwrap();
        let library_root = TempDir::new().unwrap();
        let proj = create_sample_video_project(library_root.path(), "video_wp");
        fs::write(library_root.path().join("secret.mp4"), b"outside").unwrap();

        let lib = WallpaperEngineLibrary::with_auto_discover(user_dir.path(), false);
        let snap = lib.add_directory(&proj).unwrap();
        let id = &snap.projects[0].id;

        let ok_media = lib
            .resolve_project_file(id, "loop.mp4", WallpaperFileCapability::Media)
            .unwrap();
        assert!(ok_media.ends_with("loop.mp4"));

        assert!(lib
            .resolve_project_file(id, "../secret.mp4", WallpaperFileCapability::Media)
            .is_err());
        assert!(lib
            .resolve_project_file(id, "/etc/passwd", WallpaperFileCapability::AnySafe)
            .is_err());
        assert!(lib
            .resolve_project_file(id, "project.json", WallpaperFileCapability::AnySafe)
            .is_err());
        assert!(lib
            .resolve_project_file(id, "loop.mp4", WallpaperFileCapability::Image)
            .is_err());
        assert!(lib
            .resolve_project_file(id, "loop.mp4\0.jpg", WallpaperFileCapability::Image)
            .is_err());
    }

    #[test]
    fn wallpaper_mode_status_degrades_gracefully_off_windows() {
        let user_dir = TempDir::new().unwrap();
        let lib = WallpaperEngineLibrary::with_auto_discover(user_dir.path(), false);
        let status = lib.get_wallpaper_mode_status();
        #[cfg(not(target_os = "windows"))]
        {
            assert!(!status.supported);
            assert!(!status.enabled);
            let set_res = lib.set_wallpaper_mode(true).unwrap();
            assert!(!set_res.supported);
            assert!(!set_res.enabled);
        }
    }
}
