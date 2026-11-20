//! 第三方音乐平台客户端与汽水音频解密模块。
//!
//! 本模块统一提供：
//! - 通用 TTL 内存缓存 [`TtlCache`]
//! - 共享 HTTP 客户端 [`shared_http_client`]
//! - Cookie 解析、序列化与文件持久化工具
//! - 音频探针与播放受限提示构造工具
//! - 网易云、QQ 音乐、酷狗音乐、汽水音乐及汽水加密音频解密子模块

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod kugou;
pub mod netease;
pub mod qishui;
pub mod qishui_decryptor;
pub mod qq;

pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

static SHARED_HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// 获取全局共享的 `reqwest::Client` 实例。
pub fn shared_http_client() -> &'static reqwest::Client {
    SHARED_HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 当前 Unix 毫秒时间戳。
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 当前 Unix 秒级时间戳。
pub fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
struct CacheEntry<V> {
    value: V,
    inserted_at: Instant,
    expires_at: Instant,
}

/// 线程安全的带容量上限与 TTL 过期淘汰的内存缓存。
#[derive(Debug)]
pub struct TtlCache<K, V> {
    max_entries: usize,
    default_ttl: Duration,
    inner: Mutex<HashMap<K, CacheEntry<V>>>,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// 创建指定最大容量和默认 TTL 的缓存实例。
    pub fn new(max_entries: usize, default_ttl: Duration) -> Self {
        Self {
            max_entries: max_entries.max(1),
            default_ttl,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 读取未过期的缓存项。
    pub fn get(&self, key: &K) -> Option<V> {
        self.get_with_expiry(key).map(|(v, _)| v)
    }

    /// 读取未过期的缓存项及其过期时间点。
    pub fn get_with_expiry(&self, key: &K) -> Option<(V, Instant)> {
        let mut guard = self.inner.lock().ok()?;
        let now = Instant::now();
        if let Some(entry) = guard.get(key) {
            if now < entry.expires_at {
                return Some((entry.value.clone(), entry.expires_at));
            }
        }
        guard.remove(key);
        None
    }

    /// 写入缓存项；若 `ttl` 为 `None` 则使用默认 TTL。
    /// 若 `ttl` 为零则不写入（可用于跳过负缓存）。
    pub fn set(&self, key: K, value: V, ttl: Option<Duration>) {
        let ttl = ttl.unwrap_or(self.default_ttl);
        if ttl.is_zero() {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let now = Instant::now();
        guard.retain(|_, entry| now < entry.expires_at);
        guard.insert(
            key,
            CacheEntry {
                value,
                inserted_at: now,
                expires_at: now + ttl,
            },
        );
        while guard.len() > self.max_entries {
            if let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest_key);
            } else {
                break;
            }
        }
    }

    /// 删除指定 key。
    pub fn remove(&self, key: &K) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.remove(key);
        }
    }

    /// 清空全部缓存。
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear();
        }
    }

    /// 当前未清理的条目数量。
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// 缓存是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 判断名称是否为 HTTP Set-Cookie 的元数据属性而非业务 Cookie 键。
pub fn is_cookie_attribute_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "path" | "domain" | "expires" | "max-age" | "samesite" | "secure" | "httponly"
    )
}

/// 将 `key=value; key2=value2` 格式的 Cookie 字符串解析为有序键值映射。
pub fn parse_cookie_string(cookie_text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in cookie_text.lines() {
        for part in line.split(';') {
            let raw = part.trim();
            if raw.is_empty() {
                continue;
            }
            if let Some((k, v)) = raw.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                if !key.is_empty() {
                    out.insert(key.to_string(), val.to_string());
                }
            }
        }
    }
    out
}

/// 将任意键值对迭代器序列化为标准 HTTP `Cookie` 请求头字符串。
pub fn serialize_cookie_object<'a, I, K, V>(pairs: I) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    pairs
        .into_iter()
        .filter_map(|(k, v)| {
            let key = k.as_ref().trim();
            let val = v.as_ref().trim();
            if key.is_empty() || val.is_empty() {
                None
            } else {
                Some(format!("{key}={val}"))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 规范化原始 Cookie 输入文本（过滤 Set-Cookie 属性并按出现顺序去重）。
pub fn normalize_cookie_header(input: &str) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in input.lines() {
        for part in line.split(';') {
            let raw = part.trim();
            if raw.is_empty() {
                continue;
            }
            if let Some((k, v)) = raw.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                if key.is_empty() || is_cookie_attribute_name(key) || val.is_empty() {
                    continue;
                }
                if seen.insert(key.to_string()) {
                    order.push(key.to_string());
                }
                map.insert(key.to_string(), val.to_string());
            }
        }
    }

    order
        .into_iter()
        .filter_map(|k| map.get(&k).map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 从 HTTP 响应头的多个 `Set-Cookie` 字段提取业务 Cookie 键值对。
pub fn extract_cookies_from_set_cookie_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    for val in headers.get_all(reqwest::header::SET_COOKIE) {
        let Ok(raw) = val.to_str() else {
            continue;
        };
        if let Some(first_pair) = raw.split(';').next() {
            if let Some((k, v)) = first_pair.split_once('=') {
                let key = k.trim();
                let value = v.trim();
                if !key.is_empty() && !is_cookie_attribute_name(key) && !value.is_empty() {
                    cookies.insert(key.to_string(), value.to_string());
                }
            }
        }
    }
    cookies
}

/// 读取持久化 Cookie 文件（不存在或失败返回空串）。
pub fn read_cookie_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 写入持久化 Cookie 文件（自动创建父目录）。
pub fn write_cookie_file(path: &Path, value: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create cookie dir: {e}"))?;
        }
    }
    std::fs::write(path, value).map_err(|e| format!("Failed to write cookie file: {e}"))
}

/// 规范化前端传入的音质偏好枚举。
pub fn normalize_quality_preference(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "jymaster" | "master" | "studio" | "svip" => "jymaster",
        "hires" | "hi-res" | "highres" | "zhenyin" | "spatial" => "hires",
        "lossless" | "flac" | "sq" => "lossless",
        "exhigh" | "high" | "320" | "320k" | "hq" => "exhigh",
        "standard" | "normal" | "128" | "128k" | "std" => "standard",
        _ => "hires",
    }
}

/// 构造统一的播放限制说明对象。
pub fn playback_restriction(
    provider: &str,
    category: &str,
    message: &str,
    action: &str,
    extra: Option<Value>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("provider".into(), json!(provider));
    obj.insert("category".into(), json!(category));
    obj.insert("action".into(), json!(action));
    obj.insert("message".into(), json!(message));
    if let Some(Value::Object(extra_map)) = extra {
        for (k, v) in extra_map {
            obj.insert(k, v);
        }
    }
    Value::Object(obj)
}

/// 根据音频文件头部魔数识别容器/编码格式。
pub fn audio_probe_magic(buffer: &[u8]) -> &'static str {
    if buffer.is_empty() {
        return "";
    }
    if buffer.len() >= 3 && &buffer[..3] == b"ID3" {
        return "mp3-id3";
    }
    if buffer.len() >= 4 && &buffer[..4] == b"fLaC" {
        return "flac";
    }
    if buffer.len() >= 4 && &buffer[..4] == b"OggS" {
        return "ogg";
    }
    if buffer.len() >= 12 && &buffer[..4] == b"RIFF" && &buffer[8..12] == b"WAVE" {
        return "wave";
    }
    if buffer.len() >= 12 && &buffer[4..8] == b"ftyp" {
        return "mp4";
    }
    let scan = buffer.len().saturating_sub(1).min(2048);
    for i in 0..scan {
        if buffer[i] == 0xff && (buffer[i + 1] & 0xe0) == 0xe0 {
            return "mpeg-frame";
        }
    }
    ""
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioProbeResult {
    pub ok: bool,
    pub status: u16,
    pub bytes: usize,
    pub content_type: String,
    pub magic: String,
    pub reason: String,
}

const AUDIO_URL_PROBE_BYTES: usize = 8192;

/// 对上游音频流执行有限字节 Range 探针校验，确保返回的是有效音频而非 HTML/JSON 错误页。
pub async fn probe_playback_audio_url(
    client: &reqwest::Client,
    audio_url: &str,
    referer: &str,
    timeout: Duration,
) -> AudioProbeResult {
    let range_header = format!("bytes=0-{}", AUDIO_URL_PROBE_BYTES - 1);
    let mut req = client
        .get(audio_url)
        .timeout(timeout)
        .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
        .header(reqwest::header::RANGE, range_header);
    if !referer.is_empty() {
        req = req.header(reqwest::header::REFERER, referer);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            return AudioProbeResult {
                ok: false,
                status: 0,
                bytes: 0,
                content_type: String::new(),
                magic: String::new(),
                reason: if err.is_timeout() {
                    "timeout".into()
                } else {
                    "network".into()
                },
            };
        }
    };

    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if status != 200 && status != 206 {
        return AudioProbeResult {
            ok: false,
            status,
            bytes: 0,
            content_type,
            magic: String::new(),
            reason: format!("http-{status}"),
        };
    }

    let mut sample = Vec::with_capacity(AUDIO_URL_PROBE_BYTES);
    let mut stream = resp;
    while sample.len() < AUDIO_URL_PROBE_BYTES {
        match tokio::time::timeout(timeout, stream.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                let remaining = AUDIO_URL_PROBE_BYTES - sample.len();
                let take = remaining.min(chunk.len());
                sample.extend_from_slice(&chunk[..take]);
                if sample.len() >= AUDIO_URL_PROBE_BYTES {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(_)) | Err(_) => break,
        }
    }

    let magic = audio_probe_magic(&sample).to_string();
    let content_looks_text = content_type.contains("text/html")
        || content_type.contains("application/json")
        || content_type.contains("application/xml")
        || content_type.contains("text/plain");
    let ok = sample.len() >= 512 && !content_looks_text && !magic.is_empty();

    AudioProbeResult {
        ok,
        status,
        bytes: sample.len(),
        content_type,
        magic,
        reason: if ok {
            String::new()
        } else {
            "invalid-audio".into()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_cache_stores_expires_and_evicts_by_capacity() {
        let cache = TtlCache::new(2, Duration::from_secs(60));
        cache.set("k1".to_string(), 10, None);
        cache.set("k2".to_string(), 20, None);
        assert_eq!(cache.get(&"k1".to_string()), Some(10));
        assert_eq!(cache.get(&"k2".to_string()), Some(20));

        // Exceeding max_entries evicts one older item
        cache.set("k3".to_string(), 30, None);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&"k3".to_string()), Some(30));

        // Zero TTL does not store
        cache.set("k4".to_string(), 40, Some(Duration::ZERO));
        assert_eq!(cache.get(&"k4".to_string()), None);

        // Very short TTL expires
        cache.set("short".to_string(), 99, Some(Duration::from_millis(5)));
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cache.get(&"short".to_string()), None);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cookie_parse_serialize_and_normalize_work() {
        let raw = "MUSIC_U=abc123; Path=/; HttpOnly; __csrf=xyz; MUSIC_U=updated;\nNMTID=999";
        let normalized = normalize_cookie_header(raw);
        assert_eq!(normalized, "MUSIC_U=updated; __csrf=xyz; NMTID=999");

        let parsed = parse_cookie_string(&normalized);
        assert_eq!(parsed.get("MUSIC_U").map(String::as_str), Some("updated"));
        assert_eq!(parsed.get("__csrf").map(String::as_str), Some("xyz"));
        assert_eq!(parsed.get("NMTID").map(String::as_str), Some("999"));

        let serialized = serialize_cookie_object(&parsed);
        assert_eq!(serialized, "MUSIC_U=updated; NMTID=999; __csrf=xyz");
    }

    #[test]
    fn audio_probe_magic_identifies_formats() {
        assert_eq!(audio_probe_magic(b"ID3\x03\x00"), "mp3-id3");
        assert_eq!(audio_probe_magic(b"fLaC\x00\x00\x00\x22"), "flac");
        assert_eq!(audio_probe_magic(b"OggS\x00\x02"), "ogg");
        assert_eq!(audio_probe_magic(b"RIFF\x24\x00\x00\x00WAVE"), "wave");
        assert_eq!(audio_probe_magic(b"\x00\x00\x00\x18ftypM4A "), "mp4");
        assert_eq!(audio_probe_magic(&[0x00, 0xff, 0xfb, 0x90]), "mpeg-frame");
        assert_eq!(audio_probe_magic(b"<html>not audio</html>"), "");
    }
}
