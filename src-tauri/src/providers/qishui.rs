//! 汽水音乐纯 Rust 客户端。
//!
//! 精确移植自 `qishui-api.js` 与 `qishui-qr-login.js`，涵盖：
//! - 公开目录搜索（火山车辆接口 + 本地相关性评分）与详情拉取
//! - PC Web 会话接口（`/luna/pc/...`）与 OpenAPI 适配（`open.douyin.com`）
//! - 音频流 `video_model` / `play_info_list` 解析及 `#auth=<spade_a>` URL 生成
//! - 会员权益分级（free / vip / svip）与 20 秒正观察宽限期
//! - 逐字歌词转换（LRC + YRC）
//! - 官方 Passport Web QR 登录状态机与可插拔签名回调钩子（支持 Axum 反向代理签名）+ Cookie 兜底

use md5::{Digest as Md5Digest, Md5};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use super::netease::render_qr_png_data_url;
use super::{
    extract_cookies_from_set_cookie_headers, is_cookie_attribute_name, normalize_cookie_header,
    parse_cookie_string, playback_restriction, read_cookie_file, serialize_cookie_object,
    shared_http_client, unix_millis, write_cookie_file, TtlCache,
};

const QISHUI_API_BASE: &str = "https://open.douyin.com";
const QISHUI_RELATED_MEDIA_PATH: &str = "/api/luna/v1/platform/feed/related-media/";
const QISHUI_FEED_SONG_TAB_PATH: &str = "/api/luna/v1/platform/feed/song-tab/";
const QISHUI_SCOPE: &str = "luna.openapi.platform.play_core";
const QISHUI_PUBLIC_SEARCH_URL: &str = "https://api-vehicle.volcengine.com/v2/search/type";
const QISHUI_PUBLIC_CONTENTS_URL: &str = "https://api-vehicle.volcengine.com/v2/custom/contents";
const QISHUI_WEB_PC_API_BASE: &str = "https://api.qishui.com";
const QISHUI_WEB_API_BASES: &[&str] = &["https://api5-lq.qishui.com", "https://api.qishui.com"];

const QISHUI_VIRTUAL_FEED_PLAYLIST_ID: &str = "qishui-feed";
const QISHUI_WEB_LIKED_PLAYLIST_ID: &str = "qishui-liked";
const QISHUI_WEB_RECENT_PLAYLIST_ID: &str = "qishui-recent";

const QISHUI_UA: &str = "Mineradio/2.1.0 (Qishui official OpenAPI bridge)";
const QISHUI_PUBLIC_UA: &str = "Mineradio/2.1.0 (Qishui public catalog bridge)";
const QISHUI_WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) SodaMusic/3.2.1 Chrome/136.0.7103.59 Electron/36.4.0-rs.22.release.main.1 TTElectron/36.4.0-rs.22.release.main.1 Safari/537.36";
const QISHUI_PC_APP_UA: &str = "LunaPC/3.3.0(359450208)";

const QISHUI_PASSPORT_AID: &str = "386088";
const QISHUI_PASSPORT_APP_VERSION: &str = "3.5.2";
const QISHUI_PASSPORT_SDK_VERSION: &str = "2.4.13";
const QISHUI_PASSPORT_VERIFY_SDK_VERSION: &str = "1.0.29";
const QISHUI_PASSPORT_SECURE_SDK_VERSION: &str = "3.3.5";
const QISHUI_PASSPORT_BDMS_VERSION: &str = "1.0.0.41";

const QISHUI_MEMBERSHIP_POSITIVE_GRACE_MS: u64 = 20 * 1000;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Passport HTTP 响应结果。
#[derive(Debug, Clone)]
pub struct QishuiPassportResponse {
    pub status: u16,
    pub body: String,
    pub set_cookies: BTreeMap<String, String>,
}

/// 汽水官方 Passport 请求签名/代理钩子（可由 Axum 反向代理或 Webview `a_bogus` 注入实现）。
pub trait QishuiPassportSigner: Send + Sync {
    fn request_passport<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a BTreeMap<String, String>,
        form_body: Option<&'a str>,
    ) -> BoxFuture<'a, Result<QishuiPassportResponse, String>>;
}

/// 默认基于 `reqwest` 的直接 HTTP Passport 请求执行器。
pub struct DirectHttpPassportSigner {
    client: reqwest::Client,
}

impl DirectHttpPassportSigner {
    pub fn new() -> Self {
        Self {
            client: shared_http_client().clone(),
        }
    }
}

impl Default for DirectHttpPassportSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl QishuiPassportSigner for DirectHttpPassportSigner {
    fn request_passport<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a BTreeMap<String, String>,
        form_body: Option<&'a str>,
    ) -> BoxFuture<'a, Result<QishuiPassportResponse, String>> {
        Box::pin(async move {
            let mut req = if method.eq_ignore_ascii_case("POST") {
                self.client.post(url)
            } else {
                self.client.get(url)
            };
            req = req.timeout(Duration::from_secs(30));
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            if let Some(body) = form_body {
                req = req.body(body.to_string());
            }
            let resp = req
                .send()
                .await
                .map_err(|e| format!("Qishui Passport request failed: {e}"))?;
            let status = resp.status().as_u16();
            let set_cookies = extract_cookies_from_set_cookie_headers(resp.headers());
            let body = resp
                .text()
                .await
                .map_err(|e| format!("Qishui Passport read failed: {e}"))?;
            Ok(QishuiPassportResponse {
                status,
                body,
                set_cookies,
            })
        })
    }
}

/// `.qishui-qr-login.json` 持久化结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QishuiQrConfig {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub install_id: String,
    #[serde(default)]
    pub verify_portrait_id: String,
    #[serde(default)]
    pub computer_name: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default)]
    pub ms_token: String,
}

impl Default for QishuiQrConfig {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            install_id: String::new(),
            verify_portrait_id: String::new(),
            computer_name: "Windows-PC".to_string(),
            cookie: String::new(),
            ms_token: String::new(),
        }
    }
}

// ---------- Cookie & Token 辅助 ----------

pub fn normalize_qishui_token(value: &str) -> String {
    let mut token = value.trim().to_string();
    if let Some(rest) = token.strip_prefix("Bearer ").or_else(|| token.strip_prefix("bearer ")) {
        token = rest.trim().to_string();
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(?:access-token|access_token)\s*[:=]\s*([^;\s]+)").unwrap());
    if let Some(caps) = re.captures(&token) {
        if let Some(m) = caps.get(1) {
            return m.as_str().trim().to_string();
        }
    }
    token
}

pub fn normalize_qishui_cookie_input(input: &str) -> String {
    normalize_cookie_header(input)
}

pub fn qishui_cookie_has_login(cookie_text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|;\s*)(sessionid|sessionid_ss|sid_guard|sid_tt|uid_tt|uid_tt_ss)=[^;\s]+")
            .unwrap()
    });
    re.is_match(cookie_text)
}

fn sha1_hex_short(input: &str, len: usize) -> String {
    // 复用简单的 FNV/SHA256 摘要用于稳定内存缓存 key
    use sha2::{Digest, Sha256};
    let hex = hex::encode(Sha256::digest(input.as_bytes()));
    hex[..len.min(hex.len())].to_string()
}

fn qishui_cookie_fingerprint(cookie_text: &str) -> String {
    sha1_hex_short(&normalize_qishui_cookie_input(cookie_text), 16)
}

fn qishui_cookie_user_id(cookie_text: &str) -> String {
    let obj = parse_cookie_string(cookie_text);
    let raw = obj
        .get("uid_tt")
        .or_else(|| obj.get("uid_tt_ss"))
        .or_else(|| obj.get("sessionid"))
        .or_else(|| obj.get("sessionid_ss"))
        .or_else(|| obj.get("sid_guard"))
        .map(|s| s.trim())
        .unwrap_or("");
    if raw.is_empty() {
        String::new()
    } else {
        format!("web:{}", sha1_hex_short(raw, 12))
    }
}

fn qishui_session_cookie_header(cookie_text: &str) -> String {
    let normalized = normalize_qishui_cookie_input(cookie_text);
    if qishui_cookie_has_login(&normalized) {
        return normalized;
    }
    let obj = parse_cookie_string(&normalized);
    let sid = obj
        .get("sessionid")
        .or_else(|| obj.get("sessionid_ss"))
        .map(|s| s.trim())
        .unwrap_or("");
    if sid.is_empty() {
        normalized
    } else {
        format!("sessionid={sid};")
    }
}

// ---------- 通用对象与文本提取工具 ----------

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_lyric_body(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n").trim().to_string()
}

fn pick_object<'a>(candidates: &[Option<&'a Value>]) -> &'a Value {
    static EMPTY: OnceLock<Value> = OnceLock::new();
    for item in candidates.iter().flatten() {
        if item.is_object() {
            return item;
        }
    }
    EMPTY.get_or_init(|| json!({}))
}

fn pick_array<'a>(candidates: &[Option<&'a Value>]) -> Vec<Value> {
    for item in candidates.iter().flatten() {
        if let Some(arr) = item.as_array() {
            return arr.clone();
        }
    }
    Vec::new()
}

fn qishui_object_string(obj: &Value, keys: &[&str]) -> String {
    let Some(map) = obj.as_object() else {
        return String::new();
    };
    for key in keys {
        let Some(v) = map.get(*key) else { continue };
        match v {
            Value::String(s) => {
                let n = normalize_text(s);
                if !n.is_empty() {
                    return n;
                }
            }
            Value::Number(n) => return n.to_string(),
            Value::Bool(b) => return b.to_string(),
            Value::Array(arr) => {
                for elem in arr {
                    if let Some(s) = elem.as_str() {
                        let n = normalize_text(s);
                        if !n.is_empty() {
                            return n;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn qishui_object_number(obj: &Value, keys: &[&str]) -> f64 {
    qishui_object_string(obj, keys)
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .unwrap_or(0.0)
}

fn qishui_normalize_duration_seconds(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value > 1000.0 {
        (value / 1000.0).round() as u64
    } else {
        value.round() as u64
    }
}

fn qishui_normalize_bitrate_kbps(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value > 1000.0 {
        (value / 1000.0).round() as u64
    } else {
        value.round() as u64
    }
}

fn first_url(value: Option<&Value>) -> String {
    let Some(v) = value else { return String::new() };
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .map(|item| first_url(Some(item)))
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        Value::Object(map) => {
            for k in [
                "url_list",
                "urls",
                "url",
                "uri",
                "main_url",
                "cover_url",
                "download_url",
            ] {
                let u = first_url(map.get(k));
                if !u.is_empty() {
                    return u;
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn qishui_image_url(value: Option<&Value>, suffix: &str) -> String {
    let Some(v) = value else { return String::new() };
    match v {
        Value::String(s) => {
            let t = normalize_text(s);
            if !t.to_ascii_lowercase().starts_with("http://")
                && !t.to_ascii_lowercase().starts_with("https://")
            {
                return String::new();
            }
            if !suffix.is_empty() && !t.contains('~') {
                format!("{t}{suffix}")
            } else {
                t
            }
        }
        Value::Array(arr) => arr
            .iter()
            .map(|item| qishui_image_url(Some(item), suffix))
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        Value::Object(map) => {
            let cover = normalize_text(&first_url(
                map.get("urls")
                    .or_else(|| map.get("url_list"))
                    .or_else(|| map.get("urlList"))
                    .or_else(|| map.get("url"))
                    .or_else(|| map.get("main_url"))
                    .or_else(|| map.get("cover_url"))
                    .or_else(|| map.get("image_url")),
            ));
            let uri = normalize_text(
                map.get("uri")
                    .or_else(|| map.get("url_key"))
                    .or_else(|| map.get("image_uri"))
                    .or_else(|| map.get("cover_uri"))
                    .and_then(|x| x.as_str())
                    .unwrap_or(""),
            );
            let mut out = cover;
            if !out.is_empty() && !uri.is_empty() && !out.contains(&uri) {
                out.push_str(&uri);
            }
            if out.is_empty()
                && (uri.to_ascii_lowercase().starts_with("http://")
                    || uri.to_ascii_lowercase().starts_with("https://"))
            {
                out = uri;
            }
            if !out.to_ascii_lowercase().starts_with("http://")
                && !out.to_ascii_lowercase().starts_with("https://")
            {
                return String::new();
            }
            if !suffix.is_empty() && !out.contains('~') {
                format!("{out}{suffix}")
            } else {
                out
            }
        }
        _ => String::new(),
    }
}

fn qishui_first_image_url(suffix: &str, candidates: &[Option<&Value>]) -> String {
    for c in candidates {
        let u = qishui_image_url(*c, suffix);
        if !u.is_empty() {
            return u;
        }
    }
    String::new()
}

// ---------- 逐字歌词转换（LRC + YRC） ----------

fn qishui_lyric_timestamp(ms: u64) -> String {
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let centis = (ms % 1000) / 10;
    format!("[{minutes:02}:{seconds:02}.{centis:02}]")
}

/// 将汽水音乐逐字歌词格式转换为标准 LRC 与 YRC。
pub fn qishui_convert_lyric(value: &str) -> (String, String) {
    static TIMED_RE: OnceLock<Regex> = OnceLock::new();
    static WORD_RE: OnceLock<Regex> = OnceLock::new();
    static STRIP_RE: OnceLock<Regex> = OnceLock::new();

    let input = normalize_lyric_body(value);
    if input.is_empty() {
        return (String::new(), String::new());
    }
    let timed_re = TIMED_RE.get_or_init(|| Regex::new(r"^\[(\d+),(\d+)\](.*)$").unwrap());
    let word_re = WORD_RE.get_or_init(|| Regex::new(r"([<(])(\d+),(\d+),(\d+)[>)]([^<(]*)").unwrap());
    let strip_re = STRIP_RE.get_or_init(|| Regex::new(r"[<(]\d+,\d+,\d+[>)]").unwrap());

    let mut lrc_lines = Vec::new();
    let mut yrc_lines = Vec::new();
    let mut converted = false;

    for raw_line in input.lines() {
        let line = raw_line.trim();
        let Some(caps) = timed_re.captures(line) else {
            continue;
        };
        let line_start: u64 = caps[1].parse().unwrap_or(0);
        let line_dur: u64 = caps[2].parse().unwrap_or(0);
        let body = &caps[3];

        let mut text = String::new();
        let mut yrc_body = String::new();
        for wc in word_re.captures_iter(body) {
            let bracket = &wc[1];
            let raw_start: u64 = wc[2].parse().unwrap_or(0);
            let word_dur: u64 = wc[3].parse().unwrap_or(0);
            let reserved: u64 = wc[4].parse().unwrap_or(0);
            let word_text = &wc[5];
            if word_text.is_empty() {
                continue;
            }
            let abs_start = if bracket == "<" {
                line_start + raw_start
            } else if raw_start >= line_start.saturating_sub(500) {
                raw_start
            } else {
                line_start + raw_start
            };
            text.push_str(word_text);
            yrc_body.push_str(&format!("({abs_start},{word_dur},{reserved}){word_text}"));
        }

        if text.is_empty() {
            text = strip_re.replace_all(body, "").to_string();
        }
        let clean_text = normalize_text(&text);
        if clean_text.is_empty() {
            continue;
        }
        converted = true;
        lrc_lines.push(format!("{}{}", qishui_lyric_timestamp(line_start), clean_text));
        yrc_lines.push(format!(
            "[{line_start},{line_dur}]{}",
            if yrc_body.is_empty() { &clean_text } else { &yrc_body }
        ));
    }

    if !converted {
        (input, String::new())
    } else {
        (lrc_lines.join("\n"), yrc_lines.join("\n"))
    }
}

// ---------- 会员权益判定与流分级 ----------

fn qishui_normalized_field_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ('\u{4E00}'..='\u{9FFF}').contains(c))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn qishui_explicit_positive(value: &Value) -> bool {
    match value {
        Value::Bool(true) => true,
        Value::Number(n) => n.as_f64().is_some_and(|f| f.is_finite() && f > 0.0),
        Value::String(s) => {
            let text = normalize_text(s).to_ascii_lowercase();
            if text.is_empty() {
                return false;
            }
            if let Ok(num) = text.parse::<f64>() {
                return num > 0.0;
            }
            matches!(
                text.as_str(),
                "true" | "yes" | "active" | "valid" | "enabled" | "opened" | "vip" | "svip" | "premium" | "member" | "会员" | "已开通" | "有效"
            )
        }
        _ => false,
    }
}

fn qishui_explicit_negative(value: &Value) -> bool {
    match value {
        Value::Bool(false) | Value::Null => true,
        Value::Number(n) => n.as_f64().is_some_and(|f| f.is_finite() && f <= 0.0),
        Value::String(s) => {
            let text = normalize_text(s).to_ascii_lowercase();
            if text.is_empty() {
                return false;
            }
            if let Ok(num) = text.parse::<f64>() {
                return num <= 0.0;
            }
            matches!(
                text.as_str(),
                "false" | "no" | "inactive" | "invalid" | "disabled" | "closed" | "expired" | "none" | "free" | "normal" | "ordinary" | "非会员" | "普通用户" | "未开通" | "无vip" | "已过期" | "过期"
            )
        }
        _ => false,
    }
}

fn qishui_membership_level_value(value: &Value) -> &'static str {
    let raw = match value {
        Value::String(s) => s.as_str(),
        _ => return "",
    };
    let text: String = normalize_text(raw)
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '_' | '-'))
        .collect();
    match text.as_str() {
        "svip" | "supervip" | "超级会员" | "超级vip" | "豪华会员" => "svip",
        "vip" | "premium" | "member" | "会员" | "普通会员" => "vip",
        "none" | "free" | "normal" | "ordinary" | "novip" | "非会员" | "普通用户" | "未开通" | "无vip" | "已过期" | "过期" => "none",
        _ => "",
    }
}

fn qishui_membership_expiry_millis(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n
            .as_f64()
            .filter(|f| f.is_finite() && *f > 0.0)
            .map(|f| if f < 100_000_000_000.0 { (f * 1000.0) as u64 } else { f as u64 })
            .unwrap_or(0),
        Value::String(s) => {
            let trimmed = s.trim();
            if let Ok(num) = trimmed.parse::<f64>() {
                if num.is_finite() && num > 0.0 {
                    return if num < 100_000_000_000.0 {
                        (num * 1000.0) as u64
                    } else {
                        num as u64
                    };
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
                let ms = dt.timestamp_millis();
                if ms > 0 {
                    return ms as u64;
                }
            }
            0
        }
        _ => 0,
    }
}

fn qishui_membership_object_state(
    value: &Value,
    level: &str,
    now_ms: u64,
) -> (bool, Option<bool>, u64) {
    let Some(map) = value.as_object() else {
        return (false, None, 0);
    };
    let mut status_positive = false;
    let mut status_negative = false;
    let mut generic_expiry_known = false;
    let mut generic_expiry_expired = false;
    let mut generic_future_expiry = 0u64;
    let mut level_expiry_known = false;
    let mut level_expiry_expired = false;
    let mut level_future_expiry = 0u64;

    let is_vip_exp = |k: &str| {
        matches!(
            k,
            "vipexpiretime"
                | "vipexpiresat"
                | "vipexpiredat"
                | "vipendtime"
                | "memberexpiretime"
                | "memberexpiresat"
                | "memberexpiredat"
                | "memberendtime"
        )
    };
    let is_svip_exp = |k: &str| {
        matches!(
            k,
            "svipexpiretime" | "svipexpiresat" | "svipexpiredat" | "svipendtime"
        )
    };

    for (key, item) in map {
        let norm = qishui_normalized_field_key(key);
        let is_generic = matches!(
            norm.as_str(),
            "expiretime" | "expiresat" | "expirationtime" | "expiredat" | "endtime" | "validuntil"
        );
        let is_level = match level {
            "svip" => is_svip_exp(&norm),
            "vip" => is_vip_exp(&norm),
            _ => is_vip_exp(&norm) || is_svip_exp(&norm),
        };
        if is_generic || is_level {
            let expires_at = qishui_membership_expiry_millis(item);
            let is_known_exp = expires_at > 0
                || match item {
                    Value::Number(n) => n.as_f64().is_some_and(|f| f.is_finite() && f <= 0.0),
                    Value::String(s) if !s.trim().is_empty() => {
                        s.trim().parse::<f64>().is_ok_and(|f| f <= 0.0)
                    }
                    _ => false,
                };
            if !is_known_exp {
                continue;
            }
            if is_level {
                level_expiry_known = true;
                if expires_at > now_ms {
                    level_future_expiry = level_future_expiry.max(expires_at);
                } else {
                    level_expiry_expired = true;
                }
            } else {
                generic_expiry_known = true;
                if expires_at > now_ms {
                    generic_future_expiry = generic_future_expiry.max(expires_at);
                } else {
                    generic_expiry_expired = true;
                }
            }
            continue;
        }
        if matches!(
            norm.as_str(),
            "status" | "state" | "active" | "valid" | "enabled" | "isactive" | "isvalid" | "isenabled"
        ) {
            if qishui_explicit_negative(item) {
                status_negative = true;
            } else if qishui_explicit_positive(item) {
                status_positive = true;
            }
        }
    }

    let expiry_known = level_expiry_known || generic_expiry_known;
    let expiry_expired = if level_expiry_known {
        level_expiry_expired
    } else {
        generic_expiry_expired
    };
    let future_expiry = if level_expiry_known {
        level_future_expiry
    } else {
        generic_future_expiry
    };
    let active = if status_negative || expiry_expired {
        Some(false)
    } else if future_expiry > 0 || (!expiry_known && status_positive) {
        Some(true)
    } else if expiry_known {
        Some(false)
    } else {
        None
    };
    let known = expiry_known || status_positive || status_negative;
    (
        known,
        active,
        if active == Some(true) { future_expiry } else { 0 },
    )
}

/// 从 `/luna/pc/me` 或用户资料中解析汽水音乐 VIP / SVIP 状态。
pub fn qishui_membership_from_data(value: &Value, now_ms: u64) -> Value {
    let mut membership_known = false;
    let mut is_vip = false;
    let mut is_svip = false;
    let mut vip_type = 0u64;
    let mut svip_type = 0u64;
    let mut vip_expires_at = 0u64;
    let mut svip_expires_at = 0u64;
    let mut visited = 0usize;

    #[allow(clippy::too_many_arguments)]
    fn apply_level(
        level: &str,
        num: u64,
        active: Option<bool>,
        exp: u64,
        now_ms: u64,
        is_vip: &mut bool,
        is_svip: &mut bool,
        vip_type: &mut u64,
        svip_type: &mut u64,
        vip_expires_at: &mut u64,
        svip_expires_at: &mut u64,
    ) {
        if active == Some(false) || level.is_empty() {
            return;
        }
        if level == "svip" {
            *is_svip = true;
            *is_vip = true;
            *svip_type = (*svip_type).max(num.max(1));
            if exp > now_ms && (*svip_expires_at == 0 || exp < *svip_expires_at) {
                *svip_expires_at = exp;
            }
        } else if level == "vip" {
            *is_vip = true;
            *vip_type = (*vip_type).max(num.max(1));
            if exp > now_ms && (*vip_expires_at == 0 || exp < *vip_expires_at) {
                *vip_expires_at = exp;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        node: &Value,
        depth: usize,
        now_ms: u64,
        visited: &mut usize,
        membership_known: &mut bool,
        is_vip: &mut bool,
        is_svip: &mut bool,
        vip_type: &mut u64,
        svip_type: &mut u64,
        vip_expires_at: &mut u64,
        svip_expires_at: &mut u64,
    ) {
        if depth > 6 || *visited > 600 {
            return;
        }
        *visited += 1;
        if let Some(arr) = node.as_array() {
            for item in arr.iter().take(120) {
                visit(
                    item,
                    depth + 1,
                    now_ms,
                    visited,
                    membership_known,
                    is_vip,
                    is_svip,
                    vip_type,
                    svip_type,
                    vip_expires_at,
                    svip_expires_at,
                );
            }
            return;
        }
        let Some(map) = node.as_object() else { return };
        let (_, vip_active, vip_exp) = qishui_membership_object_state(node, "vip", now_ms);
        let (_, svip_active, svip_exp) = qishui_membership_object_state(node, "svip", now_ms);

        for (key, item) in map.iter().take(160) {
            let norm = qishui_normalized_field_key(key);
            if matches!(
                norm.as_str(),
                "sviptype" | "sviplevel" | "superviptype" | "superviplevel"
            ) {
                *membership_known = true;
                if let Some(n) = item.as_u64().filter(|&v| v > 0) {
                    apply_level(
                        "svip",
                        n,
                        svip_active,
                        svip_exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            } else if matches!(
                norm.as_str(),
                "viptype" | "viplevel" | "membertype" | "memberlevel" | "musicviptype" | "musicviplevel"
            ) {
                *membership_known = true;
                if let Some(n) = item.as_u64().filter(|&v| v > 0) {
                    apply_level(
                        "vip",
                        n,
                        vip_active,
                        vip_exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            } else if matches!(
                norm.as_str(),
                "issvip" | "issupervip" | "hassvip" | "hassupervip" | "svipactive" | "svipenabled"
            ) {
                *membership_known = true;
                if qishui_explicit_positive(item) {
                    apply_level(
                        "svip",
                        1,
                        svip_active,
                        svip_exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            } else if matches!(
                norm.as_str(),
                "isvip" | "ismember" | "hasvip" | "hasmembership" | "vipactive" | "vipenabled"
            ) {
                *membership_known = true;
                if qishui_explicit_positive(item) {
                    apply_level(
                        "vip",
                        1,
                        vip_active,
                        vip_exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            } else if matches!(
                norm.as_str(),
                "viplevelname" | "vipname" | "memberlevelname" | "membername" | "membershiplevel" | "membershiptype"
            ) {
                *membership_known = true;
                let lvl = qishui_membership_level_value(item);
                let (act, exp) = if lvl == "svip" {
                    (svip_active, svip_exp)
                } else {
                    (vip_active, vip_exp)
                };
                apply_level(
                    lvl,
                    1,
                    act,
                    exp,
                    now_ms,
                    is_vip,
                    is_svip,
                    vip_type,
                    svip_type,
                    vip_expires_at,
                    svip_expires_at,
                );
            } else if matches!(
                norm.as_str(),
                "svipinfo" | "svipdetail" | "svipbenefit" | "svippackage" | "supervipinfo" | "supervipdetail" | "supervipbenefit" | "supervippackage"
            ) {
                *membership_known = true;
                let (_, act, exp) = qishui_membership_object_state(item, "svip", now_ms);
                if act == Some(true) {
                    apply_level(
                        "svip",
                        1,
                        Some(true),
                        exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            } else if matches!(
                norm.as_str(),
                "vipinfo" | "vipdetail" | "vipbenefit" | "vippackage" | "memberinfo" | "memberdetail" | "memberbenefit" | "memberpackage" | "membershipinfo" | "membershipdetail"
            ) {
                *membership_known = true;
                let (_, act, exp) = qishui_membership_object_state(item, "vip", now_ms);
                if act == Some(true) {
                    apply_level(
                        "vip",
                        1,
                        Some(true),
                        exp,
                        now_ms,
                        is_vip,
                        is_svip,
                        vip_type,
                        svip_type,
                        vip_expires_at,
                        svip_expires_at,
                    );
                }
            }
            if item.is_object() || item.is_array() {
                visit(
                    item,
                    depth + 1,
                    now_ms,
                    visited,
                    membership_known,
                    is_vip,
                    is_svip,
                    vip_type,
                    svip_type,
                    vip_expires_at,
                    svip_expires_at,
                );
            }
        }
    }

    visit(
        value,
        0,
        now_ms,
        &mut visited,
        &mut membership_known,
        &mut is_vip,
        &mut is_svip,
        &mut vip_type,
        &mut svip_type,
        &mut vip_expires_at,
        &mut svip_expires_at,
    );
    if is_svip {
        is_vip = true;
    }
    let vip_level = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else {
        "none"
    };
    let expires_at = [vip_expires_at, svip_expires_at]
        .into_iter()
        .filter(|&x| x > now_ms)
        .min()
        .unwrap_or(0);

    json!({
        "membershipKnown": membership_known,
        "vipType": if is_svip { svip_type } else { vip_type },
        "vipLevel": vip_level,
        "isVip": is_vip,
        "isSvip": is_svip,
        "vipLabel": match vip_level { "svip" => "SVIP", "vip" => "VIP", _ => "无VIP" },
        "expiresAt": expires_at,
    })
}

fn qishui_unknown_membership(error: &str) -> Value {
    json!({
        "membershipKnown": false,
        "membershipStatus": "unknown",
        "reason": "membership_unknown",
        "vipType": 0,
        "vipLevel": "unknown",
        "isVip": false,
        "isSvip": false,
        "vipLabel": "未知会员状态",
        "expiresAt": 0,
        "sessionValidated": false,
        "error": if error.is_empty() { "QISHUI_MEMBERSHIP_UNKNOWN" } else { error },
    })
}

fn qishui_normalize_required_tier(value: &str) -> &'static str {
    let text: String = normalize_text(value)
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .collect();
    if matches!(
        text.as_str(),
        "svip" | "supervip" | "hires" | "highres" | "highresolution" | "master" | "atmos" | "dolby" | "spatial"
    ) {
        return "svip";
    }
    if matches!(
        text.as_str(),
        "vip" | "member" | "premium" | "lossless" | "sq" | "flac" | "exhigh" | "high" | "higher" | "highest" | "hq" | "320"
    ) {
        return "vip";
    }
    "free"
}

fn qishui_higher_required_tier(a: &str, b: &str) -> &'static str {
    let rank = |t: &str| match t {
        "svip" => 2,
        "vip" => 1,
        _ => 0,
    };
    let na = qishui_normalize_required_tier(a);
    let nb = qishui_normalize_required_tier(b);
    if rank(nb) > rank(na) { nb } else { na }
}

fn qishui_required_tier_allowed(required_tier: &str, membership: &Value) -> bool {
    let tier = qishui_normalize_required_tier(required_tier);
    if tier == "free" {
        return true;
    }
    if membership.get("membershipKnown").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    if tier == "svip" {
        membership.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
    } else {
        membership.get("isVip").and_then(|v| v.as_bool()) == Some(true)
    }
}

#[derive(Debug, Clone)]
pub struct QishuiStreamCandidate {
    pub url: String,
    pub auth: String,
    pub size: u64,
    pub format: String,
    pub bitrate: f64,
    pub quality: String,
    pub duration: u64,
    pub required_tier: &'static str,
}

impl QishuiStreamCandidate {
    pub fn quality_rank(&self) -> u32 {
        let q: String = normalize_text(&self.quality)
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
            .collect();
        let f = normalize_text(&self.format).to_ascii_lowercase();
        let br = qishui_normalize_bitrate_kbps(self.bitrate);
        let lossless_fmt = f.contains("flac") || f.contains("alac") || f.contains("wav");
        let lossless_lbl = q.contains("lossless") || q.contains("flac") || q.contains("sq") || q.contains("svip");
        let hires_lbl = q.contains("hires") || q.contains("master");
        if hires_lbl && (lossless_fmt || br >= 900) {
            110
        } else if lossless_lbl || lossless_fmt || br >= 900 {
            100
        } else if hires_lbl {
            90
        } else if q.contains("atmos") || q.contains("dolby") || q.contains("spatial") {
            88
        } else if q.contains("highest") || q.contains("excellent") || q.contains("superhigh") || q.contains("hq") {
            80
        } else if q.contains("higher") || q.contains("high") || q.contains("320") || br >= 320 {
            70
        } else if q.contains("standard") || q.contains("medium") || q.contains("normal") || q.contains("128") || br >= 128 {
            50
        } else if q.contains("low") || q.contains("preview") {
            10
        } else if br > 0 {
            20
        } else {
            0
        }
    }

    pub fn playback_level(&self) -> &'static str {
        let r = self.quality_rank();
        if r >= 100 {
            "lossless"
        } else if r >= 80 {
            "hires"
        } else if r >= 65 {
            "exhigh"
        } else {
            "standard"
        }
    }

    pub fn compute_required_tier(&self) -> &'static str {
        let mut tier = qishui_normalize_required_tier(self.required_tier);
        let q: String = normalize_text(&self.quality)
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
            .collect();
        let f = normalize_text(&self.format).to_ascii_lowercase();
        let br = qishui_normalize_bitrate_kbps(self.bitrate);
        if q.contains("svip")
            || q.contains("supervip")
            || q.contains("hires")
            || q.contains("highres")
            || q.contains("highresolution")
            || q.contains("master")
            || q.contains("atmos")
            || q.contains("dolby")
            || q.contains("spatial")
        {
            tier = qishui_higher_required_tier(tier, "svip");
        } else if q.contains("lossless")
            || q.contains("flac")
            || q.contains("sq")
            || f.contains("flac")
            || f.contains("alac")
            || f.contains("wav")
            || br >= 900
            || q.contains("highest")
            || q.contains("excellent")
            || q.contains("superhigh")
            || q.contains("higher")
            || q.contains("high")
            || q.contains("hq")
            || q.contains("exhigh")
            || q.contains("320")
            || br > 192
        {
            tier = qishui_higher_required_tier(tier, "vip");
        }
        tier
    }
}

fn qishui_better_stream(a: &QishuiStreamCandidate, b: &QishuiStreamCandidate) -> bool {
    if a.duration > 0 || b.duration > 0 {
        if a.duration > b.duration + 1 {
            return true;
        }
        if b.duration > a.duration + 1 {
            return false;
        }
    }
    let ar = a.quality_rank();
    let br = b.quality_rank();
    if ar != br {
        return ar > br;
    }
    let ab = qishui_normalize_bitrate_kbps(a.bitrate);
    let bb = qishui_normalize_bitrate_kbps(b.bitrate);
    if ab != bb {
        return ab > bb;
    }
    a.size > b.size
}

fn qishui_best_stream_candidate(candidates: &[QishuiStreamCandidate]) -> Option<QishuiStreamCandidate> {
    let mut best: Option<&QishuiStreamCandidate> = None;
    for item in candidates {
        if item.url.is_empty() {
            continue;
        }
        if best.is_none_or(|b| qishui_better_stream(item, b)) {
            best = Some(item);
        }
    }
    best.cloned()
}

pub fn qishui_best_stream_for_membership(
    candidates: &[QishuiStreamCandidate],
    membership: &Value,
) -> Option<QishuiStreamCandidate> {
    let allowed: Vec<QishuiStreamCandidate> = candidates
        .iter()
        .filter(|c| !c.url.is_empty() && qishui_required_tier_allowed(c.compute_required_tier(), membership))
        .cloned()
        .collect();
    qishui_best_stream_candidate(&allowed)
}

/// 生成带 `#auth=` 片段的 URL，供本地 `/api/audio` 代理调用 `qishui_decryptor` 实时解密。
pub fn qishui_url_with_auth(url: &str, auth: &str) -> String {
    let clean_url = normalize_text(url);
    let clean_auth = normalize_text(auth);
    if clean_url.is_empty() || clean_auth.is_empty() || clean_url.contains("#auth=") {
        return clean_url;
    }
    let encoded = percent_encoding::utf8_percent_encode(
        &clean_auth,
        percent_encoding::NON_ALPHANUMERIC,
    );
    format!("{clean_url}#auth={encoded}")
}

fn qishui_maybe_parse_json(value: &Value) -> Value {
    let Some(mut text) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return value.clone();
    };
    let mut owned = text.to_string();
    for _ in 0..3 {
        if !owned.starts_with('"') {
            break;
        }
        match serde_json::from_str::<Value>(&owned) {
            Ok(Value::String(s)) => {
                owned = s.trim().to_string();
            }
            Ok(other) => return other,
            Err(_) => break,
        }
    }
    text = owned.as_str();
    serde_json::from_str::<Value>(text).unwrap_or_else(|_| value.clone())
}

fn qishui_video_model_play_auth(value: &Value) -> String {
    let child = pick_object(&[
        value.get("encrypt_info"),
        value.get("EncryptInfo"),
        value.get("encryptInfo"),
    ]);
    qishui_object_string(child, &["spade_a", "SpadeA", "spadeA", "play_auth", "PlayAuth"])
}

fn qishui_video_model_quality_hint(key: &str) -> &'static str {
    let norm: String = normalize_text(key)
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .collect();
    if norm.is_empty() {
        return "";
    }
    for token in [
        "hires", "lossless", "sq", "flac", "highest", "higher", "standard", "normal",
    ] {
        if norm.contains(token) {
            return token;
        }
    }
    ""
}

fn qishui_stream_from_object(
    value: &Value,
    inherited_auth: &str,
    inherited_dur: u64,
    key_hint: &str,
) -> Option<QishuiStreamCandidate> {
    if !value.is_object() {
        return None;
    }
    let url = {
        let direct = qishui_object_string(
            value,
            &[
                "main_play_url",
                "MainPlayUrl",
                "main_url",
                "MainUrl",
                "url",
                "URL",
                "play_url",
                "PlayURL",
                "playable_url",
                "PlayableUrl",
                "playableUrl",
            ],
        );
        if !direct.is_empty() {
            direct
        } else {
            let backup = qishui_object_string(
                value,
                &[
                    "backup_play_url",
                    "BackupPlayUrl",
                    "backup_url",
                    "BackupUrl",
                    "backup_url_1",
                    "backup_url_2",
                    "backup_url_3",
                ],
            );
            if !backup.is_empty() {
                backup
            } else {
                normalize_text(&first_url(
                    value
                        .get("backup_urls")
                        .or_else(|| value.get("backupUrls"))
                        .or_else(|| value.get("url_list"))
                        .or_else(|| value.get("UrlList")),
                ))
            }
        }
    };
    if url.is_empty() {
        return None;
    }

    let meta = pick_object(&[
        value.get("video_meta"),
        value.get("VideoMeta"),
        value.get("meta"),
        value.get("Meta"),
    ]);
    let size = {
        let s1 = qishui_object_number(
            value,
            &["size", "Size", "file_size", "FileSize", "data_size", "DataSize"],
        );
        if s1 > 0.0 {
            s1 as u64
        } else {
            qishui_object_number(meta, &["size", "Size", "file_size", "FileSize"]) as u64
        }
    };
    let dur = {
        let d = qishui_normalize_duration_seconds(qishui_object_number(
            value,
            &["duration", "Duration"],
        ));
        if d > 0 { d } else { inherited_dur }
    };
    let bitrate = {
        let b1 = qishui_object_number(
            value,
            &[
                "bitrate",
                "Bitrate",
                "real_bitrate",
                "RealBitrate",
                "br",
                "BR",
                "bit_rate",
                "BitRate",
            ],
        );
        if b1 > 0.0 {
            b1
        } else {
            let b2 = qishui_object_number(
                meta,
                &[
                    "bitrate",
                    "Bitrate",
                    "real_bitrate",
                    "RealBitrate",
                    "bit_rate",
                    "BitRate",
                ],
            );
            if b2 > 0.0 {
                b2
            } else if size > 0 && dur > 0 {
                let est = ((size as f64) * 8.0 / (dur as f64)).round();
                if (32_000.0..=12_000_000.0).contains(&est) {
                    est
                } else {
                    0.0
                }
            } else {
                0.0
            }
        }
    };
    let auth = {
        let a1 = qishui_object_string(value, &["play_auth", "PlayAuth", "spade_a", "SpadeA"]);
        if !a1.is_empty() {
            a1
        } else {
            let a2 = qishui_video_model_play_auth(value);
            if !a2.is_empty() {
                a2
            } else {
                inherited_auth.to_string()
            }
        }
    };
    let format = {
        let f1 = qishui_object_string(
            value,
            &["format", "Format", "vtype", "VType", "file_format", "FileFormat"],
        );
        if !f1.is_empty() {
            f1
        } else {
            qishui_object_string(
                meta,
                &["format", "Format", "vtype", "VType", "codec_type", "CodecType"],
            )
        }
    };
    let quality = {
        let q1 = qishui_object_string(
            value,
            &[
                "quality",
                "Quality",
                "definition",
                "Definition",
                "quality_type",
                "QualityType",
            ],
        );
        if !q1.is_empty() {
            q1
        } else {
            let gd = qishui_object_string(value, &["gear_des_key", "GearDesKey"]);
            let hint_src = if !gd.is_empty() { gd } else { key_hint.to_string() };
            qishui_video_model_quality_hint(&hint_src).to_string()
        }
    };
    let mut cand = QishuiStreamCandidate {
        url,
        auth,
        size,
        format,
        bitrate,
        quality,
        duration: dur,
        required_tier: "free",
    };
    cand.required_tier = cand.compute_required_tier();
    Some(cand)
}

fn qishui_collect_video_model_streams(
    raw_value: &Value,
    key_hint: &str,
    inherited_auth: &str,
    inherited_dur: u64,
    out: &mut Vec<QishuiStreamCandidate>,
) {
    let val = qishui_maybe_parse_json(raw_value);
    if let Some(arr) = val.as_array() {
        for item in arr {
            qishui_collect_video_model_streams(
                item,
                key_hint,
                inherited_auth,
                inherited_dur,
                out,
            );
        }
        return;
    }
    let Some(map) = val.as_object() else { return };
    let own_auth = {
        let a = qishui_video_model_play_auth(&val);
        if !a.is_empty() {
            a
        } else {
            inherited_auth.to_string()
        }
    };
    let own_dur = {
        let d = qishui_normalize_duration_seconds(qishui_object_number(
            &val,
            &["video_duration", "duration", "Duration"],
        ));
        if d > 0 { d } else { inherited_dur }
    };
    if let Some(entry) = qishui_stream_from_object(&val, &own_auth, own_dur, key_hint) {
        out.push(entry);
    }
    for (k, v) in map {
        qishui_collect_video_model_streams(v, k, &own_auth, own_dur, out);
    }
}

pub fn collect_qishui_track_v2_streams(
    payload: &Value,
) -> (Value, Value, Vec<QishuiStreamCandidate>, Vec<QishuiStreamCandidate>) {
    let data = payload.get("data").unwrap_or(payload);
    let track = pick_object(&[
        data.get("track"),
        data.get("track_info"),
        data.get("trackInfo"),
        payload.get("track"),
        payload.get("track_info"),
        payload.get("trackInfo"),
    ])
    .clone();
    let player = pick_object(&[
        data.get("track_player"),
        data.get("trackPlayer"),
        payload.get("track_player"),
        payload.get("trackPlayer"),
        track.get("track_player"),
        track.get("trackPlayer"),
    ])
    .clone();
    let audio_info = pick_object(&[
        track.get("audio_info"),
        track.get("audioInfo"),
        payload.get("audio_info"),
        payload.get("audioInfo"),
    ]);
    let play_info_list = pick_array(&[
        audio_info.get("play_info_list"),
        audio_info.get("PlayInfoList"),
        audio_info.get("playInfoList"),
    ]);
    let mut streams: Vec<QishuiStreamCandidate> = play_info_list
        .iter()
        .filter_map(|item| qishui_stream_from_object(item, "", 0, ""))
        .collect();
    let video_model = player
        .get("video_model")
        .or_else(|| player.get("VideoModel"))
        .or_else(|| player.get("videoModel"))
        .or_else(|| track.get("video_model"))
        .or_else(|| track.get("VideoModel"))
        .cloned()
        .unwrap_or(Value::Null);
    qishui_collect_video_model_streams(&video_model, "", "", 0, &mut streams);

    let bit_rates = pick_array(&[
        track.get("bit_rates"),
        track.get("bitRates"),
        audio_info.get("bit_rates"),
        audio_info.get("bitRates"),
        payload.get("bit_rates"),
        payload.get("bitRates"),
    ]);
    let fallback_streams: Vec<QishuiStreamCandidate> = bit_rates
        .iter()
        .filter_map(|item| qishui_stream_from_object(item, "", 0, ""))
        .collect();
    (track, player, streams, fallback_streams)
}

// ---------- 曲目 / 公开搜索评分 / 歌词提取 ----------

fn qishui_search_comparable(value: &str) -> String {
    normalize_text(value)
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4E00}'..='\u{9FFF}').contains(c))
        .collect()
}

pub fn qishui_public_search_score(song: &Value, keywords: &str) -> i64 {
    let query = qishui_search_comparable(keywords);
    if query.is_empty() {
        return 0;
    }
    let name = qishui_search_comparable(song.get("name").and_then(|v| v.as_str()).unwrap_or(""));
    let artist =
        qishui_search_comparable(song.get("artist").and_then(|v| v.as_str()).unwrap_or(""));
    let album =
        qishui_search_comparable(song.get("album").and_then(|v| v.as_str()).unwrap_or(""));
    let mut score = 0i64;
    if name == query {
        score += 180;
    } else if name.contains(&query) {
        score += 120;
    } else if !name.is_empty() && query.contains(&name) && name.chars().count() >= 2 {
        score += 70;
    }
    if artist == query {
        score += 150;
    } else if artist.contains(&query) {
        score += 105;
    }
    if album == query {
        score += 80;
    } else if album.contains(&query) {
        score += 45;
    }
    for token in normalize_text(keywords).split_whitespace() {
        let tk = qishui_search_comparable(token);
        if tk.chars().count() < 2 {
            continue;
        }
        if name.contains(&tk) {
            score += 28;
        }
        if artist.contains(&tk) {
            score += 22;
        }
        if album.contains(&tk) {
            score += 10;
        }
    }
    score
}

pub fn rank_qishui_public_songs(songs: Vec<Value>, keywords: &str, limit: usize) -> Vec<Value> {
    let mut scored: Vec<(Value, usize, i64)> = songs
        .into_iter()
        .enumerate()
        .map(|(idx, s)| {
            let sc = qishui_public_search_score(&s, keywords);
            (s, idx, sc)
        })
        .collect();
    let has_matched = scored.iter().any(|(_, _, s)| *s > 0);
    if has_matched {
        scored.retain(|(_, _, s)| *s > 0);
    }
    scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit.max(1))
        .map(|(s, _, _)| s)
        .collect()
}

fn extract_qishui_media_list(payload: &Value) -> Vec<Value> {
    let data = payload.get("data").unwrap_or(payload);
    let direct = pick_array(&[
        data.get("media_resources"),
        data.get("media_list"),
        data.get("related_media"),
        data.get("medias"),
        data.get("media"),
        data.get("tracks"),
        data.get("track_list"),
        data.get("songs"),
        data.get("items"),
        data.get("list"),
        data.get("result"),
        data.get("song_list"),
        data.get("recommend_media_list"),
    ]);
    if !direct.is_empty() {
        return direct;
    }
    let mut best: Vec<Value> = Vec::new();
    fn walk(node: &Value, depth: usize, best: &mut Vec<Value>) {
        if depth > 4 {
            return;
        }
        match node {
            Value::Array(arr) => {
                let media_like: Vec<Value> = arr
                    .iter()
                    .filter(|item| {
                        item.is_object()
                            && (item.get("media").is_some()
                                || item.get("track_entity").is_some()
                                || item.get("entity").is_some()
                                || item.get("base_info").is_some()
                                || item.get("id").is_some()
                                || item.get("media_id").is_some())
                    })
                    .cloned()
                    .collect();
                if media_like.len() > best.len() {
                    *best = media_like;
                }
                for item in arr {
                    walk(item, depth + 1, best);
                }
            }
            Value::Object(map) => {
                for v in map.values().take(80) {
                    walk(v, depth + 1, best);
                }
            }
            _ => {}
        }
    }
    walk(data, 0, &mut best);
    best
}

fn map_qishui_media(
    raw: &Value,
    index: usize,
    query: &str,
    direct_playable: bool,
) -> Option<Value> {
    let entity = pick_object(&[raw.get("entity"), raw.get("data"), Some(raw)]);
    let media = pick_object(&[entity.get("media"), raw.get("media"), Some(entity)]);
    let wrapper = pick_object(&[
        entity.get("track_wrapper"),
        media.get("track_wrapper"),
        raw.get("track_wrapper"),
    ]);
    let track = pick_object(&[
        wrapper.get("track"),
        media.get("track_entity"),
        raw.get("track_entity"),
        media.get("track"),
        raw.get("track"),
        Some(media),
    ]);
    let base = pick_object(&[
        track.get("base_info"),
        media.get("base_info"),
        raw.get("base_info"),
        Some(track),
    ]);
    let display = pick_object(&[
        track.get("display_info"),
        media.get("display_info"),
        raw.get("display_info"),
    ]);
    let related = pick_object(&[
        track.get("related_info"),
        media.get("related_info"),
        raw.get("related_info"),
    ]);

    let id = {
        let mut found = String::new();
        for cand in [
            base.get("id"),
            track.get("id"),
            media.get("id"),
            raw.get("id"),
            raw.get("media_id"),
            raw.get("item_id"),
            raw.get("song_id"),
            raw.get("vid"),
        ]
        .into_iter()
        .flatten()
        {
            let s = match cand {
                Value::String(st) => normalize_text(st),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            if !s.is_empty() {
                found = s;
                break;
            }
        }
        if found.is_empty() {
            format!("qishui-{index}-{query}")
        } else {
            found
        }
    };
    let name = {
        let mut found = String::new();
        for cand in [
            base.get("name"),
            base.get("title"),
            track.get("name"),
            track.get("title"),
            media.get("name"),
            raw.get("name"),
            raw.get("title"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(s) = cand.as_str() {
                let n = normalize_text(s);
                if !n.is_empty() {
                    found = n;
                    break;
                }
            }
        }
        found
    };
    if id.is_empty() || name.is_empty() {
        return None;
    }

    let links = pick_array(&[
        related.get("artist_links"),
        related.get("artists"),
        base.get("artist_links"),
        base.get("artists"),
        display.get("artist_links"),
        display.get("artists"),
        track.get("artists"),
        media.get("artists"),
    ]);
    let mut artists: Vec<Value> = Vec::new();
    let mut seen_names = HashSet::new();
    for item in &links {
        let aname = normalize_text(
            item.get("name")
                .or_else(|| item.get("display_name"))
                .or_else(|| item.get("simple_display_name"))
                .or_else(|| item.get("title"))
                .or_else(|| item.get("artist_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        if !aname.is_empty() && seen_names.insert(aname.clone()) {
            let aid = item
                .get("id")
                .or_else(|| item.get("artist_id"))
                .or_else(|| item.get("open_id"))
                .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))
                .unwrap_or_default();
            artists.push(json!({ "id": aid, "name": aname, "mid": aid }));
        }
    }
    let artist = if !artists.is_empty() {
        artists
            .iter()
            .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" / ")
    } else {
        normalize_text(
            base.get("artist_name")
                .or_else(|| display.get("artist_name"))
                .or_else(|| related.get("artist_name"))
                .or_else(|| track.get("artist_name"))
                .or_else(|| media.get("artist_name"))
                .or_else(|| base.get("author"))
                .or_else(|| raw.get("author"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )
    };

    let album_link = pick_object(&[
        related.get("album_link"),
        related.get("album"),
        base.get("album"),
        display.get("album"),
        track.get("album"),
        media.get("album"),
    ]);
    let album = normalize_text(
        album_link
            .get("name")
            .or_else(|| album_link.get("title"))
            .or_else(|| base.get("album_name"))
            .or_else(|| display.get("album_name"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let cover = qishui_first_image_url(
        "~c5_375x375.jpg",
        &[
            display.get("cover_url"),
            display.get("url_cover"),
            base.get("cover_url"),
            base.get("url_cover"),
            album_link.get("cover_url"),
            album_link.get("url_cover"),
            track.get("url_cover"),
            track.get("cover_url"),
            media.get("cover_url"),
            media.get("url_cover"),
            raw.get("cover_url"),
            raw.get("cover"),
            raw.get("url_cover"),
        ],
    );
    let duration_ms = base
        .get("duration_ms")
        .or_else(|| base.get("duration"))
        .or_else(|| track.get("duration_ms"))
        .or_else(|| track.get("duration"))
        .or_else(|| media.get("duration_ms"))
        .or_else(|| media.get("duration"))
        .or_else(|| raw.get("duration"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let lyric_info = pick_object(&[
        display.get("lyric_info"),
        track.get("lyric_info"),
        base.get("lyric_info"),
    ]);
    let lyric_entity = pick_object(&[
        lyric_info.get("lyric_entity"),
        lyric_info.get("lyric"),
        lyric_info.get("original_lyric"),
    ]);
    let raw_lyric = normalize_lyric_body(
        lyric_entity
            .get("content")
            .or_else(|| lyric_info.get("content"))
            .or_else(|| lyric_info.get("lyric"))
            .or_else(|| lyric_info.get("lyric_text"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let (lyric, _) = qishui_convert_lyric(&raw_lyric);

    let fee = if track
        .get("label_info")
        .and_then(|l| l.get("only_vip_playable"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        1
    } else {
        0
    };

    Some(json!({
        "provider": "qishui",
        "source": "qishui",
        "type": "qishui",
        "id": id,
        "providerSongId": id,
        "name": name,
        "artist": artist,
        "artists": artists,
        "album": album,
        "cover": cover,
        "duration": qishui_normalize_duration_seconds(duration_ms),
        "popularity": raw.get("popularity").or_else(|| raw.get("hot")).or_else(|| raw.get("heat")).or_else(|| raw.get("play_count")).or_else(|| related.get("play_count")).and_then(|v| v.as_u64()).unwrap_or(0),
        "fee": fee,
        "playable": direct_playable,
        "playbackMode": if direct_playable { "direct-url" } else { "recommend-match" },
        "recommendationSource": if direct_playable { "qishui-pc-session" } else { "qishui-openapi" },
        "qishuiRank": index,
        "qishuiQuery": query,
        "lyric": lyric,
        "tlyric": "",
        "restriction": playback_restriction("qishui", "provider_limited", "汽水音乐当前作为推荐/匹配源接入，播放时会自动寻找其它可播版本。", "switch_source", None),
    }))
}

fn dedupe_qishui_songs(songs: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in songs {
        let key = s
            .get("providerSongId")
            .or_else(|| s.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !key.is_empty() && seen.insert(key) {
            out.push(s);
        }
    }
    out
}

fn extract_qishui_lyrics(payload: &Value) -> (String, String) {
    fn text_from_node(raw: &Value) -> String {
        let val = qishui_maybe_parse_json(raw);
        if let Some(s) = val.as_str() {
            let n = normalize_lyric_body(s);
            let lower = n.to_ascii_lowercase();
            return if lower.starts_with("http://") || lower.starts_with("https://") {
                String::new()
            } else {
                n
            };
        }
        if !val.is_object() {
            return String::new();
        }
        let entity = pick_object(&[
            val.get("lyric_entity"),
            val.get("lyricEntity"),
            val.get("original_lyric"),
            val.get("originalLyric"),
            Some(&val),
        ]);
        for k in [
            "content",
            "lyric_text",
            "lyricText",
            "text",
            "original_content",
            "originalContent",
        ] {
            if let Some(c) = entity.get(k) {
                let t = text_from_node(c);
                if !t.is_empty() {
                    return t;
                }
            }
        }
        String::new()
    }

    let mut lyric = String::new();
    let mut tlyric = String::new();
    let mut visited = 0usize;

    fn visit(
        node: &Value,
        path_text: &str,
        depth: usize,
        visited: &mut usize,
        lyric: &mut String,
        tlyric: &mut String,
    ) {
        if depth > 7 || *visited >= 600 || (!lyric.is_empty() && !tlyric.is_empty()) {
            return;
        }
        let val = qishui_maybe_parse_json(node);
        let Some(map) = val.as_object() else { return };
        *visited += 1;
        for (k, child) in map.iter().take(120) {
            let child_path = if path_text.is_empty() {
                k.clone()
            } else {
                format!("{path_text}.{k}")
            };
            let kl = k.to_ascii_lowercase();
            if kl.contains("lyric") || kl.contains("tlyric") || kl.contains("translation") {
                let text = text_from_node(child);
                if !text.is_empty() {
                    let cpl = child_path.to_ascii_lowercase();
                    if cpl.contains("translat") || cpl.contains("tlyric") || cpl.contains("lang_translation") {
                        if tlyric.is_empty() {
                            *tlyric = text;
                        }
                    } else if lyric.is_empty() {
                        *lyric = text;
                    }
                }
            }
            if matches!(
                kl.as_str(),
                "album_tracks" | "artist_tracks" | "chart_tracks" | "comments" | "prompts" | "recommend_media_list"
            ) {
                continue;
            }
            visit(child, &child_path, depth + 1, visited, lyric, tlyric);
        }
    }
    visit(payload, "", 0, &mut visited, &mut lyric, &mut tlyric);
    (lyric, tlyric)
}

#[derive(Debug, Clone)]
struct QrSessionState {
    generation: u64,
    active_token: String,
    confirmed_token: String,
    confirmed_result: Option<Value>,
    pending: bool,
    config: QishuiQrConfig,
}

#[derive(Debug, Clone)]
struct PositiveMembershipHistory {
    membership: Value,
    expires_at: u64,
    retained_until: u64,
}

#[derive(Debug, Clone)]
struct PlaylistCursorState {
    raw_items: Vec<Value>,
    cursor: String,
    has_more: bool,
    last_json: Option<Value>,
    updated_at: u64,
}

/// 汽水音乐纯 Rust 异步客户端。
pub struct QishuiClient {
    cookie_file: Option<PathBuf>,
    token_file: Option<PathBuf>,
    qr_config_file: Option<PathBuf>,
    cookie: RwLock<String>,
    access_token: RwLock<String>,
    client: reqwest::Client,
    passport_signer: Arc<dyn QishuiPassportSigner>,
    qr_state: Mutex<QrSessionState>,
    search_cache: TtlCache<String, Value>,
    lyric_cache: TtlCache<String, Value>,
    public_detail_cache: TtlCache<String, (Value, String, String)>,
    feed_cache: TtlCache<String, Value>,
    web_library_cache: TtlCache<String, Value>,
    web_playlist_cache: TtlCache<String, Value>,
    membership_cache: TtlCache<String, Value>,
    track_metadata_cache: TtlCache<String, Value>,
    playback_cache: TtlCache<String, Value>,
    membership_positive_history: Mutex<HashMap<String, PositiveMembershipHistory>>,
    playlist_cursor_cache: Mutex<HashMap<String, PlaylistCursorState>>,
}

impl QishuiClient {
    /// 创建汽水客户端并从可选磁盘文件加载 Cookie、OpenAPI Token 与 QR 配置。
    pub fn new(
        cookie_file: Option<PathBuf>,
        token_file: Option<PathBuf>,
        qr_config_file: Option<PathBuf>,
    ) -> Self {
        let qr_cfg = qr_config_file
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|txt| {
                serde_json::from_str::<QishuiQrConfig>(txt.trim_start_matches('\u{FEFF}')).ok()
            })
            .unwrap_or_default();

        let disk_cookie = cookie_file
            .as_deref()
            .map(read_cookie_file)
            .map(|s| normalize_qishui_cookie_input(&s))
            .unwrap_or_default();
        let initial_cookie = if !disk_cookie.is_empty() {
            disk_cookie
        } else {
            normalize_qishui_cookie_input(&qr_cfg.cookie)
        };

        let initial_token = token_file
            .as_deref()
            .map(read_cookie_file)
            .map(|s| normalize_qishui_token(&s))
            .unwrap_or_default();

        Self {
            cookie_file,
            token_file,
            qr_config_file,
            cookie: RwLock::new(initial_cookie),
            access_token: RwLock::new(initial_token),
            client: shared_http_client().clone(),
            passport_signer: Arc::new(DirectHttpPassportSigner::new()),
            qr_state: Mutex::new(QrSessionState {
                generation: 0,
                active_token: String::new(),
                confirmed_token: String::new(),
                confirmed_result: None,
                pending: false,
                config: qr_cfg,
            }),
            search_cache: TtlCache::new(80, Duration::from_secs(2 * 60)),
            lyric_cache: TtlCache::new(240, Duration::from_secs(30 * 60)),
            public_detail_cache: TtlCache::new(240, Duration::from_secs(30 * 60)),
            feed_cache: TtlCache::new(16, Duration::from_secs(90)),
            web_library_cache: TtlCache::new(24, Duration::from_secs(90)),
            web_playlist_cache: TtlCache::new(48, Duration::from_secs(90)),
            membership_cache: TtlCache::new(24, Duration::from_secs(60)),
            track_metadata_cache: TtlCache::new(120, Duration::from_secs(20)),
            playback_cache: TtlCache::new(120, Duration::from_secs(4 * 60)),
            membership_positive_history: Mutex::new(HashMap::new()),
            playlist_cursor_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 注入自定义 Passport 签名/代理钩子（如本地 Axum 反向代理签名通道）。
    pub fn with_passport_signer(mut self, signer: Arc<dyn QishuiPassportSigner>) -> Self {
        self.passport_signer = signer;
        self
    }

    pub fn get_cookie(&self) -> String {
        self.cookie.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn get_access_token(&self) -> String {
        self.access_token
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn clear_runtime_caches(&self) {
        self.search_cache.clear();
        self.feed_cache.clear();
        self.web_library_cache.clear();
        self.web_playlist_cache.clear();
        self.membership_cache.clear();
        self.track_metadata_cache.clear();
        self.playback_cache.clear();
        if let Ok(mut g) = self.membership_positive_history.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.playlist_cursor_cache.lock() {
            g.clear();
        }
    }

    fn invalidate_library_caches(&self) {
        self.web_library_cache.clear();
        self.web_playlist_cache.clear();
        if let Ok(mut g) = self.playlist_cursor_cache.lock() {
            g.clear();
        }
    }

    fn persist_qr_config(&self, cfg: &QishuiQrConfig) {
        if let Some(path) = &self.qr_config_file {
            if let Ok(text) = serde_json::to_string_pretty(cfg) {
                let _ = write_cookie_file(path, &text);
            }
        }
    }

    /// 手动导入或更新汽水 Cookie（支持扫码后自动同步及手动导入兜底）。
    pub fn set_cookie(&self, input: &str) -> Result<String, String> {
        let normalized = normalize_qishui_cookie_input(input);
        let value = if normalized.is_empty() {
            input.trim().to_string()
        } else {
            normalized
        };
        if let Ok(mut g) = self.cookie.write() {
            *g = value.clone();
        }
        if let Some(path) = &self.cookie_file {
            write_cookie_file(path, &value)?;
        }
        if let Ok(mut qr) = self.qr_state.lock() {
            qr.config.cookie = value.clone();
            self.persist_qr_config(&qr.config);
        }
        self.clear_runtime_caches();
        Ok(value)
    }

    pub fn save_access_token(&self, value: &str) -> Result<Value, String> {
        let token = normalize_qishui_token(value);
        if token.len() < 10 {
            return Err("INVALID_QISHUI_TOKEN".to_string());
        }
        if let Ok(mut g) = self.access_token.write() {
            *g = token.clone();
        }
        if let Some(path) = &self.token_file {
            write_cookie_file(path, &token)?;
        }
        self.clear_runtime_caches();
        let mut st = self
            .get_static_status()
            .as_object()
            .cloned()
            .unwrap_or_default();
        st.insert("saved".into(), json!(true));
        Ok(Value::Object(st))
    }

    pub fn clear_access_token(&self) -> Value {
        if let Ok(mut g) = self.access_token.write() {
            g.clear();
        }
        if let Some(path) = &self.token_file {
            let _ = std::fs::remove_file(path);
        }
        self.clear_runtime_caches();
        let mut st = self
            .get_static_status()
            .as_object()
            .cloned()
            .unwrap_or_default();
        st.insert("ok".into(), json!(true));
        Value::Object(st)
    }

    /// 退出汽水登录并清除全部 Cookie、OpenAPI Token 与 QR 状态。
    pub fn logout(&self) -> Value {
        let _ = self.set_cookie("");
        let _ = self.clear_access_token();
        if let Ok(mut qr) = self.qr_state.lock() {
            qr.generation += 1;
            qr.active_token.clear();
            qr.confirmed_token.clear();
            qr.confirmed_result = None;
            qr.pending = false;
            qr.config.cookie.clear();
            qr.config.ms_token.clear();
            self.persist_qr_config(&qr.config);
        }
        json!({ "ok": true, "provider": "qishui", "loggedIn": false })
    }

    /// 返回本地静态状态（不发网络请求）。
    pub fn get_static_status(&self) -> Value {
        let cookie = self.get_cookie();
        let token = self.get_access_token();
        let token_configured = !token.is_empty();
        let web_session = qishui_cookie_has_login(&cookie);
        let configured = token_configured || web_session;
        let user_id = if web_session {
            qishui_cookie_user_id(&cookie)
        } else {
            String::new()
        };
        json!({
            "provider": "qishui",
            "label": "汽水音乐",
            "short": "QS",
            "configured": configured,
            "tokenConfigured": token_configured,
            "webSession": web_session,
            "cookieReady": web_session,
            "loggedIn": web_session,
            "playbackMode": if web_session { "direct-url" } else { "recommend-match" },
            "scope": QISHUI_SCOPE,
            "userId": user_id,
            "nickname": if web_session { "汽水音乐账号" } else { "" },
            "vipType": 0,
            "vipLevel": "none",
            "isVip": false,
            "isSvip": false,
            "vipLabel": "无VIP",
            "membershipKnown": false,
            "capabilities": {
                "search": token_configured || web_session || true,
                "relatedMedia": token_configured,
                "feedSongTab": token_configured || web_session,
                "lyric": true,
                "playableUrl": web_session,
                "login": true,
                "userPlaylists": configured,
                "playlistTracks": configured,
                "webSession": web_session,
            },
            "message": if web_session {
                "汽水音乐官方扫码登录已连接，可同步歌单与我的喜欢，并按账号权益播放。"
            } else if token_configured {
                "已有旧版开放平台目录授权；账号功能仍需完成官方扫码登录。"
            } else {
                "请使用抖音 App 扫描 Mineradio 中的汽水官方二维码；未登录时仅保留公开搜索匹配。"
            }
        })
    }

    // ---------- QR 扫码登录状态机 ----------

    fn ensure_qr_identity(&self) -> QishuiQrConfig {
        let mut qr = self.qr_state.lock().unwrap();
        let mut changed = false;
        let mut rng = rand::thread_rng();
        let random_digits = |len: usize, rng: &mut rand::rngs::ThreadRng| -> String {
            let mut s = rng.gen_range(1..=8).to_string();
            while s.len() < len {
                s.push_str(&rng.gen_range(0..=9).to_string());
            }
            s
        };
        if qr.config.device_id.is_empty() {
            qr.config.device_id = random_digits(16, &mut rng);
            changed = true;
        }
        if qr.config.install_id.is_empty() {
            qr.config.install_id = random_digits(15, &mut rng);
            changed = true;
        }
        if qr.config.verify_portrait_id.is_empty() {
            let mut bytes = [0u8; 16];
            rng.fill(&mut bytes);
            qr.config.verify_portrait_id = format!("{}.login", hex::encode(bytes));
            changed = true;
        }
        if qr.config.computer_name.is_empty() {
            qr.config.computer_name = "Windows-PC".to_string();
            changed = true;
        }
        if qr.config.ms_token.is_empty() {
            let mut bytes = [0u8; 88];
            for b in bytes.iter_mut() {
                *b = rng.gen();
            }
            use base64::engine::general_purpose::URL_SAFE_NO_PAD;
            use base64::Engine;
            qr.config.ms_token = format!("{}==", URL_SAFE_NO_PAD.encode(bytes));
            changed = true;
        }
        if changed {
            self.persist_qr_config(&qr.config);
        }
        qr.config.clone()
    }

    async fn passport_request(
        &self,
        method: &str,
        pathname: &str,
        extra_params: &[(&str, &str)],
        form_data: Option<&BTreeMap<String, String>>,
    ) -> Result<QishuiPassportResponse, String> {
        let id = self.ensure_qr_identity();
        let mut rand_hex = [0u8; 4];
        let mut trace_bytes = [0u8; 16];
        {
            let mut rng = rand::thread_rng();
            rng.fill(&mut rand_hex);
            rng.fill(&mut trace_bytes);
        }
        let biz_trace_id = hex::encode(rand_hex);
        let trace_id = hex::encode(trace_bytes);

        let mut url = reqwest::Url::parse(&format!("https://api.qishui.com{pathname}"))
            .map_err(|e| format!("Invalid Passport URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("passport_jssdk_version", QISHUI_PASSPORT_SDK_VERSION);
            pairs.append_pair("passport_jssdk_type", "normal");
            pairs.append_pair("is_from_ttaccountsdk", "1");
            pairs.append_pair("aid", QISHUI_PASSPORT_AID);
            pairs.append_pair("language", "zh");
            pairs.append_pair("account_sdk_source", "web");
            pairs.append_pair("p_js_v", QISHUI_PASSPORT_SDK_VERSION);
            pairs.append_pair("p_js_t", "pro");
            pairs.append_pair("p_zt", QISHUI_PASSPORT_SECURE_SDK_VERSION);
            pairs.append_pair("p_ver", QISHUI_PASSPORT_VERIFY_SDK_VERSION);
            pairs.append_pair("request_host", "app%3A%2F%2Fresources");
            pairs.append_pair("p_bd", QISHUI_PASSPORT_BDMS_VERSION);
            pairs.append_pair("biz_trace_id", &biz_trace_id);
            pairs.append_pair("is_new_login", "1");
            pairs.append_pair("is_from_iesaccountsaas", "1");
            pairs.append_pair("device_id", &id.device_id);
            pairs.append_pair("install_id", &id.install_id);
            pairs.append_pair("did", &id.device_id);
            pairs.append_pair("iid", &id.install_id);
            pairs.append_pair("device_platform", "PC");
            pairs.append_pair("version_code", QISHUI_PASSPORT_APP_VERSION);
            pairs.append_pair("msToken", &id.ms_token);
            for (k, v) in extra_params {
                pairs.append_pair(k, v);
            }
        }

        let mut headers = BTreeMap::new();
        headers.insert("Accept".into(), "application/json, text/javascript".into());
        headers.insert("User-Agent".into(), QISHUI_WEB_UA.into());
        headers.insert("x-tt-passport-verify-portrait".into(), id.verify_portrait_id);
        headers.insert("x-tt-passport-trace-id".into(), biz_trace_id);
        headers.insert(
            "x-tt-trace-id".into(),
            format!("00-{trace_id}-{}-01", &trace_id[..16]),
        );

        let form_body = if let Some(data) = form_data {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in data {
                ser.append_pair(k, v);
            }
            let encoded = ser.finish();
            let mut hasher = Md5::new();
            hasher.update(encoded.as_bytes());
            headers.insert(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            );
            headers.insert("x-ss-stub".into(), hex::encode_upper(hasher.finalize()));
            Some(encoded)
        } else {
            None
        };

        self.passport_signer
            .request_passport(method, url.as_str(), &headers, form_body.as_deref())
            .await
    }

    /// 发起官方 Passport QR 码生成。
    pub async fn create_qr_code(&self) -> Result<Value, String> {
        let generation = {
            let mut qr = self.qr_state.lock().unwrap();
            qr.generation += 1;
            qr.active_token.clear();
            qr.confirmed_token.clear();
            qr.confirmed_result = None;
            qr.pending = true;
            qr.generation
        };
        let cfg = self.ensure_qr_identity();
        let resp = self
            .passport_request(
                "GET",
                "/passport/web/get_qrcode/",
                &[
                    ("next", "https://api.qishui.com"),
                    ("need_logo", "false"),
                    ("need_short_url", "false"),
                ],
                None,
            )
            .await?;

        if resp.status < 200 || resp.status >= 400 {
            return Err(format!("汽水登录接口 HTTP {}", resp.status));
        }
        let mut envelope: Value =
            serde_json::from_str(&resp.body).map_err(|_| "汽水登录接口返回了无效 JSON".to_string())?;
        let data = envelope.get("data").cloned().unwrap_or_else(|| json!({}));
        let err_code = data
            .get("error_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if envelope.get("message").and_then(|v| v.as_str()) != Some("success") || err_code != 0 {
            return Err(format!(
                "二维码生成失败：code={err_code} {}",
                data.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            ));
        }

        let token = data
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let index_url = data
            .get("qrcode_index_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parsed_index = reqwest::Url::parse(index_url)
            .map_err(|_| "qrcode_index_url 缺少有效 URL".to_string())?;
        let scan_token = parsed_index
            .query_pairs()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| token.clone());
        if scan_token.is_empty() {
            return Err("QISHUI_QR_PAYLOAD_INCOMPLETE".to_string());
        }

        let mut scan_u =
            reqwest::Url::parse("https://bff-pc.qishui.com/light/invoke/scan_login").unwrap();
        scan_u
            .query_pairs_mut()
            .append_pair("token", &scan_token)
            .append_pair("os", "Windows")
            .append_pair("computer_name", &cfg.computer_name);
        let scan_url = scan_u.to_string().replace('+', "%20");
        let qr_data_url = render_qr_png_data_url(&scan_url)?;

        {
            let mut qr = self.qr_state.lock().unwrap();
            if qr.generation != generation {
                return Err("QISHUI_QR_REPLACED".to_string());
            }
            qr.active_token = token.clone();
        }

        if let Some(d_obj) = envelope.get_mut("data").and_then(|d| d.as_object_mut()) {
            d_obj.insert("qrcode".into(), json!(qr_data_url));
            d_obj.insert("scan_url".into(), json!(scan_url));
        }
        Ok(envelope)
    }

    /// 轮询检查 Passport QR 扫码连接状态并在确认后自动持久化 Cookie。
    pub async fn check_qr_connect(&self, token: &str) -> Result<Value, String> {
        let tok = token.trim();
        if tok.is_empty() {
            return Err("QISHUI_QR_TOKEN_REQUIRED".to_string());
        }
        let generation = {
            let qr = self.qr_state.lock().unwrap();
            if tok != qr.active_token {
                return Ok(json!({
                    "message": "error",
                    "data": { "error_code": 2, "status": "expired", "confirmed": false },
                }));
            }
            if qr.confirmed_token == tok {
                if let Some(ref res) = qr.confirmed_result {
                    return Ok(res.clone());
                }
            }
            qr.generation
        };

        let mut form = BTreeMap::new();
        form.insert("need_logo".into(), "false".into());
        form.insert("need_short_url".into(), "false".into());
        form.insert("is_frontier".into(), "true".into());
        form.insert("token".into(), tok.to_string());
        form.insert("is_new_login".into(), "1".into());
        form.insert("next".into(), "https://api.qishui.com".into());

        let resp = self
            .passport_request("POST", "/passport/web/check_qrconnect/", &[], Some(&form))
            .await?;
        if resp.status < 200 || resp.status >= 400 {
            return Err(format!("汽水登录接口 HTTP {}", resp.status));
        }
        let mut envelope: Value =
            serde_json::from_str(&resp.body).map_err(|_| "汽水登录接口返回了无效 JSON".to_string())?;
        let data = envelope.get("data").cloned().unwrap_or_else(|| json!({}));
        let err_code = data
            .get("error_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        let status_str = data
            .get("status")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let session_cookie = data
            .get("session_cookie")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut merged_cookies = parse_cookie_string(&self.get_cookie());
        for (k, v) in &resp.set_cookies {
            if !is_cookie_attribute_name(k) && !v.is_empty() {
                merged_cookies.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in parse_cookie_string(session_cookie) {
            if !is_cookie_attribute_name(&k) && !v.is_empty() {
                merged_cookies.insert(k, v);
            }
        }
        let merged_cookie_str = serialize_cookie_object(&merged_cookies);

        let is_success_signal = err_code == 0
            && (status_str == "3" || status_str == "confirmed" || !session_cookie.is_empty());

        if is_success_signal && !merged_cookie_str.is_empty() {
            let still_current = self
                .qr_state
                .lock()
                .map(|qr| qr.generation == generation && qr.active_token == tok)
                .unwrap_or(false);
            if still_current {
                let _ = self.set_cookie(&merged_cookie_str);
            }
        }

        let has_login = qishui_cookie_has_login(&self.get_cookie());
        let confirmed = {
            let mut qr = self.qr_state.lock().unwrap();
            let ok = qr.generation == generation
                && qr.active_token == tok
                && is_success_signal
                && has_login;
            if ok {
                qr.confirmed_token = tok.to_string();
                qr.pending = false;
                if let Some(d_obj) = envelope.get_mut("data").and_then(|d| d.as_object_mut()) {
                    d_obj.insert("confirmed".into(), json!(true));
                }
                qr.confirmed_result = Some(envelope.clone());
            } else if let Some(d_obj) = envelope.get_mut("data").and_then(|d| d.as_object_mut()) {
                d_obj.insert("confirmed".into(), json!(false));
            }
            ok
        };
        let _ = confirmed;
        Ok(envelope)
    }

    pub fn qr_login_status(&self) -> Value {
        let cookie = self.get_cookie();
        let qr = self.qr_state.lock().unwrap();
        json!({
            "provider": "qishui",
            "loggedIn": !qr.pending && qishui_cookie_has_login(&cookie),
            "cookieReady": qishui_cookie_has_login(&cookie),
            "qrPending": qr.pending,
            "qrConfirmed": !qr.active_token.is_empty() && qr.confirmed_token == qr.active_token,
            "deviceId": qr.config.device_id,
            "msTokenReady": !qr.config.ms_token.is_empty(),
        })
    }

    // ---------- 底层 HTTP 封装（PC Web & OpenAPI） ----------

    fn pc_app_params(extra: &[(&str, String)]) -> Vec<(String, String)> {
        let now = unix_millis().to_string();
        let iid = (unix_millis() + 1).to_string();
        let mut params = vec![
            ("aid".into(), "386088".into()),
            ("app_name".into(), "luna_pc".into()),
            ("region".into(), "cn".into()),
            ("geo_region".into(), "cn".into()),
            ("os_region".into(), "cn".into()),
            ("device_id".into(), now.clone()),
            ("iid".into(), iid),
            ("version_name".into(), "3.3.0".into()),
            ("version_code".into(), "30030000".into()),
            ("channel".into(), "official".into()),
            ("build_mode".into(), "master".into()),
            ("ac".into(), "wifi".into()),
            ("tz_name".into(), "Asia/Shanghai".into()),
            ("device_platform".into(), "windows".into()),
            ("device_type".into(), "Windows".into()),
            ("os_version".into(), "Windows 11".into()),
            ("fp".into(), now),
        ];
        for (k, v) in extra {
            if !v.is_empty() {
                params.push(((*k).to_string(), v.clone()));
            }
        }
        params
    }

    async fn web_request_json(
        &self,
        api_path: &str,
        params: &[(String, String)],
        bases: &[&str],
        pc_app: bool,
        session_only: bool,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let cookie_hdr = if session_only {
            qishui_session_cookie_header(&cookie)
        } else {
            normalize_qishui_cookie_input(&cookie)
        };
        let mut last_err = String::from("QISHUI_WEB_REQUEST_FAILED");
        for base in bases {
            let target = if api_path.starts_with("http://") || api_path.starts_with("https://") {
                api_path.to_string()
            } else {
                format!("{}{}", base.trim_end_matches('/'), api_path)
            };
            let mut url = reqwest::Url::parse(&target).map_err(|e| format!("Invalid URL: {e}"))?;
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in params {
                    if !v.is_empty() {
                        pairs.append_pair(k, v);
                    }
                }
            }
            let mut req = self
                .client
                .get(url)
                .timeout(Duration::from_millis(timeout_ms))
                .header(reqwest::header::ACCEPT, "application/json,text/plain,*/*")
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/json; charset=utf-8",
                )
                .header(
                    reqwest::header::USER_AGENT,
                    if pc_app { QISHUI_PC_APP_UA } else { QISHUI_WEB_UA },
                );
            if pc_app {
                req = req
                    .header("x-luna-background-type", "foreground")
                    .header("x-luna-is-background-req", "0")
                    .header("x-luna-is-local-user", "1");
            }
            if !cookie_hdr.is_empty() {
                req = req.header(reqwest::header::COOKIE, &cookie_hdr);
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    if status >= 400 {
                        last_err = format!("HTTP {status}");
                        if status == 401 || status == 403 {
                            break;
                        }
                        continue;
                    }
                    if text.trim().is_empty() {
                        last_err = "QISHUI_EMPTY_RESPONSE".into();
                        continue;
                    }
                    let json: Value = serde_json::from_str(&text)
                        .map_err(|e| format!("Invalid JSON from Qishui API: {e}"))?;
                    if let Some(code) = json
                        .get("status_code")
                        .or_else(|| json.get("error_code"))
                        .and_then(|v| v.as_i64())
                    {
                        if code != 0 {
                            let msg = json
                                .get("status_info")
                                .and_then(|s| s.get("status_msg"))
                                .or_else(|| json.get("message"))
                                .or_else(|| json.get("status_msg"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("QISHUI_PC_API_ERROR");
                            return Err(format!("QISHUI_PC_API_{code}: {msg}"));
                        }
                    }
                    return Ok(json);
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }
        Err(last_err)
    }

    async fn pc_post_json(
        &self,
        api_path: &str,
        payload: &Value,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        if !qishui_cookie_has_login(&cookie) {
            return Err("QISHUI_COOKIE_REQUIRED".to_string());
        }
        let mut url = reqwest::Url::parse(&format!("{QISHUI_WEB_PC_API_BASE}{api_path}"))
            .map_err(|e| format!("Invalid URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in Self::pc_app_params(&[]) {
                pairs.append_pair(&k, &v);
            }
        }
        let body = serde_json::to_string(payload).map_err(|e| format!("JSON encode: {e}"))?;
        let resp = self
            .client
            .post(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::ACCEPT, "application/json,text/plain,*/*")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
            .header(reqwest::header::USER_AGENT, QISHUI_PC_APP_UA)
            .header(reqwest::header::REFERER, "https://www.qishui.com/")
            .header("x-luna-background-type", "foreground")
            .header("x-luna-is-background-req", "0")
            .header("x-luna-is-local-user", "1")
            .header(reqwest::header::COOKIE, qishui_session_cookie_header(&cookie))
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Qishui PC POST failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Qishui PC POST JSON parse: {e}"))?;
        if let Some(code) = json
            .get("status_code")
            .or_else(|| json.get("error_code"))
            .and_then(|v| v.as_i64())
        {
            if code != 0 {
                return Err(format!("QISHUI_PC_API_{code}"));
            }
        }
        Ok(json)
    }

    async fn openapi_post(&self, api_path: &str, payload: &Value) -> Result<Value, String> {
        let token = self.get_access_token();
        if token.is_empty() {
            return Err("QISHUI_TOKEN_REQUIRED".to_string());
        }
        let url = format!("{QISHUI_API_BASE}{api_path}");
        let body = serde_json::to_string(payload).map_err(|e| format!("JSON encode: {e}"))?;
        let resp = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(7))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, QISHUI_UA)
            .header("access-token", &token)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Qishui OpenAPI failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Qishui OpenAPI JSON parse: {e}"))?;
        let err_code = json
            .get("data")
            .and_then(|d| {
                d.get("error_code")
                    .or_else(|| d.get("err_code"))
                    .or_else(|| d.get("code"))
            })
            .or_else(|| {
                json.get("error_code")
                    .or_else(|| json.get("err_code"))
                    .or_else(|| json.get("code"))
            })
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if err_code != 0 {
            return Err(format!("QISHUI_API_ERROR_{err_code}"));
        }
        Ok(json)
    }

    // ---------- 会员核验 & 个人资料 ----------

    fn apply_membership_observation(&self, history_key: &str, membership: Value) -> Value {
        let now = unix_millis();
        let known = membership
            .get("membershipKnown")
            .and_then(|v| v.as_bool())
            == Some(true);
        let expires_at = membership
            .get("expiresAt")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let is_vip = membership.get("isVip").and_then(|v| v.as_bool()) == Some(true);
        let is_svip = membership.get("isSvip").and_then(|v| v.as_bool()) == Some(true);
        let positive = known && (is_vip || is_svip);

        if known {
            if let Ok(mut g) = self.membership_positive_history.lock() {
                g.remove(history_key);
                if positive && expires_at > now && !history_key.is_empty() {
                    let mut retained_val = membership.clone();
                    if let Some(obj) = retained_val.as_object_mut() {
                        obj.insert(
                            "membershipStatus".into(),
                            json!(if is_svip { "svip" } else { "vip" }),
                        );
                    }
                    g.insert(
                        history_key.to_string(),
                        PositiveMembershipHistory {
                            membership: retained_val,
                            expires_at,
                            retained_until: expires_at
                                .min(now + QISHUI_MEMBERSHIP_POSITIVE_GRACE_MS),
                        },
                    );
                }
            }
            let mut out = membership;
            if let Some(obj) = out.as_object_mut() {
                obj.insert(
                    "membershipStatus".into(),
                    json!(if positive {
                        if is_svip { "svip" } else { "vip" }
                    } else {
                        "free"
                    }),
                );
            }
            return out;
        }

        if let Ok(mut g) = self.membership_positive_history.lock() {
            if let Some(prev) = g.get(history_key).cloned() {
                if prev.expires_at > now && prev.retained_until > now {
                    let mut out = prev.membership;
                    if let Some(obj) = out.as_object_mut() {
                        obj.insert("retainedOfficialPositive".into(), json!(true));
                        obj.insert("retainedUntil".into(), json!(prev.retained_until));
                        obj.insert("entitlementSource".into(), json!("recent-official-positive"));
                    }
                    return out;
                }
                g.remove(history_key);
            }
        }
        qishui_unknown_membership("QISHUI_MEMBERSHIP_UNKNOWN")
    }

    async fn fetch_playback_membership(&self) -> Value {
        let cookie = self.get_cookie();
        if !qishui_cookie_has_login(&cookie) {
            return qishui_unknown_membership("QISHUI_COOKIE_REQUIRED");
        }
        let cache_key = format!("membership|{}", qishui_cookie_fingerprint(&cookie));
        if let Some(cached) = self.membership_cache.get(&cache_key) {
            return cached;
        }
        let result = match self
            .web_request_json(
                "/luna/pc/me",
                &Self::pc_app_params(&[]),
                &[QISHUI_WEB_PC_API_BASE],
                true,
                true,
                2500,
            )
            .await
        {
            Ok(json_resp) => {
                let data = json_resp.get("data").unwrap_or(&json_resp);
                let now = unix_millis();
                let mem = qishui_membership_from_data(data, now);
                let mut obs = mem.as_object().cloned().unwrap_or_default();
                obs.insert("sessionValidated".into(), json!(true));
                obs.insert("entitlementSource".into(), json!("official-pc-me"));
                self.apply_membership_observation(&cache_key, Value::Object(obs))
            }
            Err(err) => {
                if err.contains("401") || err.contains("1000016") {
                    if let Ok(mut g) = self.membership_positive_history.lock() {
                        g.remove(&cache_key);
                    }
                    let mut u = qishui_unknown_membership(&err);
                    if let Some(obj) = u.as_object_mut() {
                        obj.insert("reauthRequired".into(), json!(true));
                    }
                    u
                } else {
                    self.apply_membership_observation(&cache_key, qishui_unknown_membership(&err))
                }
            }
        };
        let ttl = if result.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true) {
            Duration::from_secs(10)
        } else {
            Duration::from_millis(1)
        };
        self.membership_cache.set(cache_key, result.clone(), Some(ttl));
        result
    }

    /// 查询并验证汽水登录状态与个人资料。
    pub async fn status(&self) -> Result<Value, String> {
        let mut status = self.get_static_status();
        if status.get("webSession").and_then(|v| v.as_bool()) != Some(true) {
            return Ok(status);
        }
        match self.fetch_web_library().await {
            Ok(library) => {
                if let Some(profile) = library.get("profile") {
                    if let Some(obj) = status.as_object_mut() {
                        for k in ["userId", "nickname", "avatar", "douyinId"] {
                            if let Some(v) = profile.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                                obj.insert(k.into(), json!(v));
                            }
                        }
                        let known = profile
                            .get("membershipKnown")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let vip_level = if known {
                            profile
                                .get("vipLevel")
                                .and_then(|v| v.as_str())
                                .unwrap_or("none")
                        } else {
                            "unknown"
                        };
                        obj.insert(
                            "vipType".into(),
                            profile.get("vipType").cloned().unwrap_or(json!(0)),
                        );
                        obj.insert("vipLevel".into(), json!(vip_level));
                        obj.insert(
                            "isVip".into(),
                            json!(profile.get("isVip").and_then(|v| v.as_bool()).unwrap_or(false)),
                        );
                        obj.insert(
                            "isSvip".into(),
                            json!(profile.get("isSvip").and_then(|v| v.as_bool()).unwrap_or(false)),
                        );
                        obj.insert(
                            "vipLabel".into(),
                            json!(if known {
                                match vip_level { "svip" => "SVIP", "vip" => "VIP", _ => "无VIP" }
                            } else {
                                "未知会员状态"
                            }),
                        );
                        obj.insert("membershipKnown".into(), json!(known));
                        obj.insert("profileReady".into(), json!(true));
                        obj.insert("sessionValidated".into(), json!(true));
                        obj.insert("stale".into(), json!(false));
                        obj.insert("reauthRequired".into(), json!(false));
                        obj.insert(
                            "libraryReady".into(),
                            json!(library.get("libraryReady").and_then(|v| v.as_bool()).unwrap_or(false)),
                        );
                    }
                }
                Ok(status)
            }
            Err(err) => {
                let expired = err.contains("401") || err.contains("1000016");
                if let Some(obj) = status.as_object_mut() {
                    obj.insert("loggedIn".into(), json!(false));
                    obj.insert("webSession".into(), json!(false));
                    obj.insert("sessionValidated".into(), json!(false));
                    obj.insert("reauthRequired".into(), json!(expired));
                    obj.insert("stale".into(), json!(!expired));
                    obj.insert("error".into(), json!(if expired { "QISHUI_SESSION_EXPIRED" } else { "QISHUI_SESSION_UNVERIFIED" }));
                }
                Ok(status)
            }
        }
    }

    // ---------- 搜索 & 推荐 Feed ----------

    async fn public_search(
        &self,
        keywords: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let req_limit = (offset + limit * 3).max(36).min(100);
        let mut url = reqwest::Url::parse(QISHUI_PUBLIC_SEARCH_URL).unwrap();
        url.query_pairs_mut()
            .append_pair("keyword", keywords)
            .append_pair("search_type", "music")
            .append_pair("limit", &req_limit.to_string())
            .append_pair("real_offset", "0")
            .append_pair("search_source", "qishui");

        let json: Value = self
            .client
            .get(url)
            .timeout(Duration::from_secs(8))
            .header(reqwest::header::ACCEPT, "application/json,text/plain,*/*")
            .header(reqwest::header::USER_AGENT, QISHUI_PUBLIC_UA)
            .send()
            .await
            .map_err(|e| format!("Qishui public search failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Qishui public search JSON: {e}"))?;

        let list = json
            .get("data")
            .and_then(|d| d.get("list"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mapped: Vec<Value> = list
            .iter()
            .enumerate()
            .filter_map(|(idx, raw)| {
                let author = pick_object(&[
                    raw.get("author_info"),
                    raw.get("author"),
                    raw.get("artist"),
                ]);
                let album = pick_object(&[raw.get("album_info"), raw.get("album")]);
                let id = qishui_object_string(raw, &["item_id", "id", "song_id", "music_id"]);
                let id = if id.is_empty() {
                    format!("qishui-public-{idx}-{keywords}")
                } else {
                    id
                };
                let name = qishui_object_string(raw, &["title", "name", "song_name"]);
                if name.is_empty() {
                    return None;
                }
                let artist_name = {
                    let a = qishui_object_string(author, &["name"]);
                    if !a.is_empty() {
                        a
                    } else {
                        qishui_object_string(raw, &["author_name", "artist_name", "singer"])
                    }
                };
                let aid = qishui_object_string(author, &["id", "author_id"]);
                let dur_raw = raw
                    .get("duration")
                    .or_else(|| raw.get("duration_ms"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let dur_sec = if dur_raw > 10_000.0 {
                    (dur_raw / 1000.0).round() as u64
                } else {
                    dur_raw.round() as u64
                };
                let cover = first_url(
                    raw.get("cover_url")
                        .or_else(|| raw.get("cover"))
                        .or_else(|| raw.get("artwork"))
                        .or_else(|| album.get("cover_url")),
                );
                let fee = if raw
                    .get("qishui_label_info")
                    .and_then(|q| q.get("only_vip_playable"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
                {
                    1
                } else {
                    0
                };
                Some(json!({
                    "provider": "qishui",
                    "source": "qishui",
                    "type": "qishui",
                    "id": id,
                    "providerSongId": id,
                    "name": name,
                    "artist": artist_name,
                    "artists": if artist_name.is_empty() { vec![] } else { vec![json!({ "id": aid, "name": artist_name, "mid": aid })] },
                    "album": qishui_object_string(album, &["name"]),
                    "cover": cover,
                    "duration": dur_sec,
                    "fee": fee,
                    "playable": false,
                    "playbackMode": "recommend-match",
                    "recommendationSource": "qishui-public-catalog",
                    "qishuiRank": idx,
                    "qishuiQuery": keywords,
                }))
            })
            .collect();

        let ranked = rank_qishui_public_songs(mapped, keywords, req_limit);
        let ranked_len = ranked.len();
        let songs: Vec<Value> = ranked.into_iter().skip(offset).take(limit).collect();
        let st = self.get_static_status();
        Ok(json!({
            "provider": "qishui",
            "configured": st["configured"],
            "loggedIn": st["loggedIn"],
            "webSession": st["webSession"],
            "publicCatalog": true,
            "songs": songs,
            "rawCount": list.len(),
            "offset": offset,
            "limit": limit,
            "nextOffset": offset + songs.len(),
            "hasMore": songs.len() >= limit && (offset + songs.len() < ranked_len || req_limit < 100),
        }))
    }

    pub async fn search(
        &self,
        keywords: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let kw = normalize_text(keywords);
        let lim = limit.clamp(1, 18);
        let st = self.get_static_status();
        if kw.is_empty() {
            return Ok(json!({ "provider": "qishui", "songs": [], "configured": st["configured"] }));
        }
        let web_session = st.get("webSession").and_then(|v| v.as_bool()) == Some(true);
        let token_cfg = st.get("tokenConfigured").and_then(|v| v.as_bool()) == Some(true);
        let cache_key = format!(
            "{}|{lim}|{offset}|{}",
            kw.to_ascii_lowercase(),
            if web_session {
                qishui_cookie_fingerprint(&self.get_cookie())
            } else if token_cfg {
                "token".into()
            } else {
                "public".into()
            }
        );
        if let Some(cached) = self.search_cache.get(&cache_key) {
            return Ok(cached);
        }

        if web_session {
            if let Ok(json_resp) = self
                .web_request_json(
                    "/luna/pc/search/track",
                    &Self::pc_app_params(&[
                        ("q", kw.clone()),
                        ("cursor", offset.to_string()),
                        ("count", lim.to_string()),
                        ("search_method", "input".into()),
                    ]),
                    &[QISHUI_WEB_PC_API_BASE],
                    true,
                    true,
                    8500,
                )
                .await
            {
                let data = json_resp.get("data").unwrap_or(&json_resp);
                let groups = pick_array(&[
                    data.get("result_groups"),
                    data.get("resultGroups"),
                    data.get("search_result").and_then(|s| s.get("result_groups")),
                ]);
                let mut raw_items = Vec::new();
                for grp in &groups {
                    if let Some(arr) = grp
                        .get("data")
                        .or_else(|| grp.get("items"))
                        .or_else(|| grp.get("list"))
                        .or_else(|| grp.get("result"))
                        .and_then(|v| v.as_array())
                    {
                        raw_items.extend(arr.clone());
                    }
                }
                if raw_items.is_empty() {
                    raw_items = extract_qishui_media_list(&json_resp);
                }
                let songs: Vec<Value> = dedupe_qishui_songs(
                    raw_items
                        .iter()
                        .enumerate()
                        .filter_map(|(i, item)| map_qishui_media(item, i, &kw, true))
                        .collect(),
                )
                .into_iter()
                .take(lim)
                .collect();
                let res = json!({
                    "provider": "qishui",
                    "configured": true,
                    "loggedIn": true,
                    "webSession": true,
                    "source": "qishui-pc-search",
                    "songs": songs,
                    "rawCount": raw_items.len(),
                    "offset": offset,
                    "limit": lim,
                    "nextOffset": offset + songs.len(),
                    "hasMore": songs.len() >= lim,
                });
                self.search_cache.set(cache_key, res.clone(), None);
                return Ok(res);
            }
        }

        if token_cfg && offset == 0 {
            if let Ok(json_resp) = self
                .openapi_post(
                    QISHUI_RELATED_MEDIA_PATH,
                    &json!({
                        "search_query": kw,
                        "played_media": [],
                        "count": lim,
                        "common_params": { "trigger_name": "mineradio_search", "scene": "search", "source": "mineradio" },
                    }),
                )
                .await
            {
                let raw_items = extract_qishui_media_list(&json_resp);
                let songs: Vec<Value> = raw_items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, item)| map_qishui_media(item, i, &kw, false))
                    .take(lim)
                    .collect();
                let res = json!({
                    "provider": "qishui",
                    "configured": true,
                    "songs": songs,
                    "rawCount": raw_items.len(),
                    "offset": offset,
                    "limit": lim,
                    "nextOffset": songs.len(),
                    "hasMore": raw_items.len() >= lim,
                });
                self.search_cache.set(cache_key, res.clone(), None);
                return Ok(res);
            }
        }

        let pub_res = self.public_search(&kw, lim, offset).await?;
        self.search_cache.set(cache_key, pub_res.clone(), None);
        Ok(pub_res)
    }

    pub async fn feed(&self, limit: usize) -> Result<Value, String> {
        let lim = limit.clamp(1, 50);
        let st = self.get_static_status();
        let token_cfg = st.get("tokenConfigured").and_then(|v| v.as_bool()) == Some(true);
        let web_session = st.get("webSession").and_then(|v| v.as_bool()) == Some(true);

        if token_cfg {
            let cache_key = format!("feed|{lim}");
            if let Some(cached) = self.feed_cache.get(&cache_key) {
                return Ok(cached);
            }
            let json_resp = self
                .openapi_post(
                    QISHUI_FEED_SONG_TAB_PATH,
                    &json!({
                        "count": lim,
                        "played_media": [],
                        "common_params": { "trigger_name": "mineradio_feed", "scene": "feed", "source": "mineradio" },
                    }),
                )
                .await?;
            let raw_items = extract_qishui_media_list(&json_resp);
            let songs: Vec<Value> = raw_items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| map_qishui_media(item, i, "feed", false))
                .take(lim)
                .collect();
            let res = json!({
                "provider": "qishui",
                "configured": true,
                "songs": songs,
                "rawCount": raw_items.len(),
            });
            self.feed_cache.set(cache_key, res.clone(), None);
            return Ok(res);
        }

        if web_session {
            let cache_key = format!("web-feed|{}|{lim}", qishui_cookie_fingerprint(&self.get_cookie()));
            if let Some(cached) = self.feed_cache.get(&cache_key) {
                return Ok(cached);
            }
            for path in ["/luna/feed/song-tab", "/luna/pc/feed/song-tab"] {
                if let Ok(json_resp) = self
                    .web_request_json(
                        path,
                        &[
                            ("cursor".into(), "0".into()),
                            ("cnt".into(), lim.to_string()),
                            ("count".into(), lim.to_string()),
                            ("aid".into(), "386088".into()),
                            ("app_name".into(), "luna_pc".into()),
                        ],
                        QISHUI_WEB_API_BASES,
                        false,
                        false,
                        8000,
                    )
                    .await
                {
                    let raw_items = extract_qishui_media_list(&json_resp);
                    let songs: Vec<Value> = dedupe_qishui_songs(
                        raw_items
                            .iter()
                            .enumerate()
                            .filter_map(|(i, item)| map_qishui_media(item, i, "web-feed", true))
                            .collect(),
                    )
                    .into_iter()
                    .take(lim)
                    .collect();
                    if !songs.is_empty() {
                        let res = json!({
                            "provider": "qishui",
                            "configured": true,
                            "webSession": true,
                            "songs": songs,
                            "rawCount": raw_items.len(),
                        });
                        self.feed_cache.set(cache_key, res.clone(), None);
                        return Ok(res);
                    }
                }
            }
        }

        Ok(json!({
            "provider": "qishui",
            "configured": false,
            "songs": [],
            "error": "QISHUI_TOKEN_REQUIRED",
        }))
    }

    // ---------- 歌单与曲目 ----------

    async fn fetch_web_library(&self) -> Result<Value, String> {
        let cookie = self.get_cookie();
        if !qishui_cookie_has_login(&cookie) {
            return Ok(
                json!({ "provider": "qishui", "loggedIn": false, "webSession": false, "playlists": [], "likedTracks": [], "recentTracks": [] }),
            );
        }
        let cache_key = format!("library|{}", qishui_cookie_fingerprint(&cookie));
        if let Some(cached) = self.web_library_cache.get(&cache_key) {
            return Ok(cached);
        }

        let me_json = self
            .web_request_json(
                "/luna/pc/me",
                &Self::pc_app_params(&[]),
                &[QISHUI_WEB_PC_API_BASE],
                true,
                true,
                6500,
            )
            .await?;
        let data = me_json.get("data").unwrap_or(&me_json);
        let user = pick_object(&[
            data.get("my_info"),
            data.get("myInfo"),
            data.get("user"),
            data.get("user_info"),
            data.get("userInfo"),
            data.get("account"),
            data.get("me"),
            Some(data),
        ]);
        let user_id = {
            let u1 = qishui_object_string(user, &["id", "user_id", "userId", "uid", "sec_uid", "open_id"]);
            if !u1.is_empty() {
                u1
            } else {
                qishui_object_string(data, &["user_id", "userId", "uid", "id"])
            }
        };
        if user_id.is_empty() {
            return Err("QISHUI_PROFILE_INCOMPLETE".to_string());
        }
        let nickname = {
            let n1 = qishui_object_string(user, &["nickname", "nick_name", "nickName", "display_name", "name"]);
            if !n1.is_empty() {
                n1
            } else {
                qishui_object_string(data, &["nickname", "nick_name", "name"])
            }
        };
        let avatar = qishui_first_image_url(
            "~c5_300x300.jpg",
            &[
                user.get("larger_avatar_url"),
                user.get("medium_avatar_url"),
                user.get("avatar_url"),
                user.get("avatar"),
                data.get("avatar_url"),
                data.get("avatar"),
            ],
        );
        let mem = qishui_membership_from_data(data, unix_millis());
        let mut profile = json!({
            "userId": user_id,
            "nickname": nickname,
            "avatar": avatar,
            "profileReady": true,
        });
        if let (Some(po), Some(mo)) = (profile.as_object_mut(), mem.as_object()) {
            for (k, v) in mo {
                po.insert(k.clone(), v.clone());
            }
        }

        let p1 = Self::pc_app_params(&[("user_id", user_id.clone()), ("count", "50".into())]);
        let p2 = Self::pc_app_params(&[("count", "50".into())]);
        let p3 = Self::pc_app_params(&[("count", "50".into())]);
        let (created_res, collect_res, recent_res) = tokio::join!(
            self.web_request_json("/luna/pc/user/playlist", &p1, &[QISHUI_WEB_PC_API_BASE], true, true, 6500),
            self.web_request_json("/luna/pc/me/collection/mixed", &p2, &[QISHUI_WEB_PC_API_BASE], true, true, 6500),
            self.web_request_json("/luna/pc/me/recently-played-media", &p3, &[QISHUI_WEB_PC_API_BASE], true, true, 6500),
        );

        let mut playlists = Vec::new();
        let mut liked_tracks = Vec::new();
        let mut recent_tracks = Vec::new();
        let mut errors = Vec::new();

        for (label, res) in [
            ("created", created_res),
            ("collection", collect_res),
            ("recent", recent_res),
        ] {
            match res {
                Ok(json) => {
                    if label == "created" || label == "collection" {
                        extract_playlist_cards(&json, label == "created", &mut playlists);
                    }
                    let songs: Vec<Value> = extract_qishui_media_list(&json)
                        .iter()
                        .enumerate()
                        .filter_map(|(i, m)| map_qishui_media(m, i, label, true))
                        .collect();
                    if label == "recent" {
                        recent_tracks.extend(songs);
                    } else {
                        liked_tracks.extend(songs);
                    }
                }
                Err(e) => {
                    errors.push(format!("{label}:{e}"));
                }
            }
        }

        let mut seen = HashSet::new();
        let unique_playlists: Vec<Value> = playlists
            .into_iter()
            .filter(|pl| {
                let id = pl.get("id").and_then(|v| v.as_str()).unwrap_or("");
                !id.is_empty() && seen.insert(id.to_string())
            })
            .collect();
        let liked_card = unique_playlists
            .iter()
            .find(|pl| {
                pl.get("isLiked").and_then(|v| v.as_bool()) == Some(true)
                    && pl
                        .get("name")
                        .and_then(|v| v.as_str())
                        .is_some_and(|n| n.contains("喜欢") || n.to_ascii_lowercase().contains("favorite"))
            })
            .or_else(|| {
                unique_playlists
                    .iter()
                    .find(|pl| pl.get("isLiked").and_then(|v| v.as_bool()) == Some(true))
            })
            .cloned();

        let res = json!({
            "provider": "qishui",
            "loggedIn": true,
            "configured": true,
            "webSession": true,
            "playlists": unique_playlists,
            "likedCard": liked_card,
            "likedTracks": dedupe_qishui_songs(liked_tracks),
            "recentTracks": dedupe_qishui_songs(recent_tracks),
            "profile": profile,
            "libraryReady": errors.is_empty(),
            "errors": errors,
        });
        if errors.is_empty() {
            self.web_library_cache.set(cache_key, res.clone(), None);
        }
        Ok(res)
    }

    pub async fn user_playlists(&self) -> Result<Value, String> {
        let st = self.get_static_status();
        if st.get("webSession").and_then(|v| v.as_bool()) == Some(true) {
            let lib = self.fetch_web_library().await?;
            let liked_tracks = lib
                .get("likedTracks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let recent_tracks = lib
                .get("recentTracks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let profile = lib.get("profile").cloned().unwrap_or_else(|| json!({}));
            let liked_card = lib.get("likedCard").cloned().unwrap_or_else(|| json!({}));

            let mut playlists = vec![build_virtual_playlist(
                QISHUI_WEB_LIKED_PLAYLIST_ID,
                "汽水我的喜欢",
                &liked_tracks,
                liked_card.get("cover").and_then(|v| v.as_str()).unwrap_or(""),
                liked_tracks.len().max(
                    liked_card
                        .get("trackCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize,
                ),
                profile
                    .get("nickname")
                    .and_then(|v| v.as_str())
                    .unwrap_or("汽水音乐"),
            )];
            if let Some(arr) = lib.get("playlists").and_then(|v| v.as_array()) {
                for pl in arr {
                    let pid = pl.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if !pid.is_empty()
                        && !playlists
                            .iter()
                            .any(|x| x.get("id").and_then(|v| v.as_str()) == Some(pid))
                    {
                        playlists.push(pl.clone());
                    }
                }
            }
            if !recent_tracks.is_empty() {
                playlists.push(build_virtual_playlist(
                    QISHUI_WEB_RECENT_PLAYLIST_ID,
                    "汽水最近播放",
                    &recent_tracks,
                    "",
                    recent_tracks.len(),
                    "汽水音乐",
                ));
            }
            playlists.push(json!({
                "provider": "qishui",
                "source": "qishui",
                "type": "playlist",
                "id": QISHUI_VIRTUAL_FEED_PLAYLIST_ID,
                "name": "汽水推荐",
                "cover": "",
                "trackCount": 0,
                "playCount": 0,
                "creator": "汽水音乐",
                "subscribed": false,
                "virtual": true,
                "playbackMode": "recommend-match",
            }));
            return Ok(json!({
                "provider": "qishui",
                "loggedIn": true,
                "configured": true,
                "webSession": true,
                "playlists": playlists,
                "tracksPreview": liked_tracks.into_iter().take(8).collect::<Vec<_>>(),
                "libraryErrors": lib["errors"],
                "libraryReady": lib["libraryReady"],
                "profile": profile,
                "userId": profile["userId"],
                "nickname": profile["nickname"],
                "avatar": profile["avatar"],
            }));
        }

        if st.get("configured").and_then(|v| v.as_bool()) != Some(true) {
            return Ok(json!({
                "provider": "qishui",
                "loggedIn": false,
                "configured": false,
                "playlists": [],
                "error": "QISHUI_TOKEN_REQUIRED",
                "message": st["message"],
            }));
        }

        let feed = self.feed(24).await?;
        let songs = feed
            .get("songs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let pl = build_virtual_playlist(
            QISHUI_VIRTUAL_FEED_PLAYLIST_ID,
            "汽水推荐",
            &songs,
            "",
            songs.len(),
            "汽水音乐",
        );
        Ok(json!({
            "provider": "qishui",
            "loggedIn": false,
            "configured": true,
            "playlists": [pl],
            "tracksPreview": songs.into_iter().take(8).collect::<Vec<_>>(),
        }))
    }

    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let id = normalize_text(playlist_id.trim_start_matches("qishui:"));
        let lim = limit.clamp(1, 50);
        let web_session = qishui_cookie_has_login(&cookie);

        if web_session {
            if id == QISHUI_WEB_LIKED_PLAYLIST_ID || id == "liked" || id == "favorite" {
                let lib = self.fetch_web_library().await?;
                let all_songs = lib
                    .get("likedTracks")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let tracks: Vec<Value> = all_songs.iter().skip(offset).take(lim).cloned().collect();
                let next_offset = offset + tracks.len();
                return Ok(json!({
                    "provider": "qishui",
                    "loggedIn": true,
                    "configured": true,
                    "webSession": true,
                    "playlist": build_virtual_playlist(QISHUI_WEB_LIKED_PLAYLIST_ID, "汽水我的喜欢", &all_songs, "", all_songs.len(), "汽水音乐"),
                    "tracks": tracks,
                    "total": all_songs.len(),
                    "offset": offset,
                    "limit": lim,
                    "nextOffset": next_offset,
                    "hasMore": next_offset < all_songs.len(),
                }));
            }
            if id == QISHUI_WEB_RECENT_PLAYLIST_ID || id == "recent" {
                let lib = self.fetch_web_library().await?;
                let all_songs = lib
                    .get("recentTracks")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let tracks: Vec<Value> = all_songs.iter().skip(offset).take(lim).cloned().collect();
                let next_offset = offset + tracks.len();
                return Ok(json!({
                    "provider": "qishui",
                    "loggedIn": true,
                    "configured": true,
                    "webSession": true,
                    "playlist": build_virtual_playlist(QISHUI_WEB_RECENT_PLAYLIST_ID, "汽水最近播放", &all_songs, "", all_songs.len(), "汽水音乐"),
                    "tracks": tracks,
                    "total": all_songs.len(),
                    "offset": offset,
                    "limit": lim,
                    "nextOffset": next_offset,
                    "hasMore": next_offset < all_songs.len(),
                }));
            }
            if !id.is_empty() && id != QISHUI_VIRTUAL_FEED_PLAYLIST_ID && id != "feed" {
                return self.fetch_web_playlist_tracks(&id, lim, offset).await;
            }
        }

        let feed = self.feed((offset + lim).clamp(lim, 50)).await?;
        let all_songs = feed
            .get("songs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let tracks: Vec<Value> = all_songs.iter().skip(offset).take(lim).cloned().collect();
        Ok(json!({
            "provider": "qishui",
            "loggedIn": true,
            "configured": true,
            "playlist": build_virtual_playlist(QISHUI_VIRTUAL_FEED_PLAYLIST_ID, "汽水推荐", &all_songs, "", all_songs.len(), "汽水音乐"),
            "tracks": tracks,
            "total": all_songs.len(),
            "offset": offset,
            "limit": lim,
            "nextOffset": offset + tracks.len(),
            "hasMore": false,
        }))
    }

    async fn fetch_web_playlist_tracks(
        &self,
        id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let fp = qishui_cookie_fingerprint(&cookie);
        let cache_key = format!("playlist|{fp}|{id}|{limit}|{offset}");
        if let Some(cached) = self.web_playlist_cache.get(&cache_key) {
            return Ok(cached);
        }

        let cursor_key = format!("{fp}|{id}");
        let mut cursor_state = {
            let g = self.playlist_cursor_cache.lock().unwrap();
            g.get(&cursor_key)
                .filter(|s| unix_millis().saturating_sub(s.updated_at) <= 10 * 60 * 1000)
                .cloned()
                .unwrap_or(PlaylistCursorState {
                    raw_items: Vec::new(),
                    cursor: String::new(),
                    has_more: true,
                    last_json: None,
                    updated_at: unix_millis(),
                })
        };

        let target_count = offset + limit;
        while cursor_state.raw_items.len() < target_count && cursor_state.has_more {
            let count = (target_count - cursor_state.raw_items.len()).clamp(1, 100);
            let json_resp = self
                .web_request_json(
                    "/luna/pc/playlist/detail",
                    &Self::pc_app_params(&[
                        ("playlist_id", id.to_string()),
                        ("cursor", cursor_state.cursor.clone()),
                        ("count", count.to_string()),
                    ]),
                    &[QISHUI_WEB_PC_API_BASE],
                    true,
                    true,
                    9000,
                )
                .await?;
            let page_raw = extract_qishui_media_list(&json_resp);
            let page_empty = page_raw.is_empty();
            cursor_state.raw_items.extend(page_raw);
            let page_data = json_resp.get("data").unwrap_or(&json_resp);
            let next_cursor = qishui_object_string(page_data, &["next_cursor", "nextCursor"]);
            let has_more_flag = page_data
                .get("has_more")
                .or_else(|| page_data.get("hasMore"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            cursor_state.cursor = next_cursor.clone();
            cursor_state.has_more = has_more_flag && !next_cursor.is_empty() && !page_empty;
            cursor_state.last_json = Some(json_resp);
            cursor_state.updated_at = unix_millis();
        }

        {
            let mut g = self.playlist_cursor_cache.lock().unwrap();
            g.insert(cursor_key, cursor_state.clone());
        }

        let empty = json!({});
        let last_json = cursor_state.last_json.as_ref().unwrap_or(&empty);
        let data = last_json.get("data").unwrap_or(last_json);
        let meta = pick_object(&[
            data.get("playlist"),
            last_json.get("playlist"),
            data.get("playlist_info"),
            last_json.get("playlist_info"),
        ]);
        let all_tracks = dedupe_qishui_songs(
            cursor_state
                .raw_items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| map_qishui_media(item, i, "web-playlist", true))
                .collect(),
        );
        let tracks: Vec<Value> = all_tracks.iter().skip(offset).take(limit).cloned().collect();
        let total = meta
            .get("count_tracks")
            .or_else(|| meta.get("track_count"))
            .or_else(|| data.get("total"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(all_tracks.len());
        let pl_name = qishui_object_string(meta, &["title", "name", "display_title"]);
        let mut playlist = build_virtual_playlist(
            id,
            if pl_name.is_empty() { "汽水歌单" } else { &pl_name },
            &tracks,
            &qishui_first_image_url(
                "~c5_300x300.jpg",
                &[meta.get("cover_url"), meta.get("cover")],
            ),
            total,
            "汽水音乐",
        );
        if let Some(obj) = playlist.as_object_mut() {
            obj.insert("virtual".into(), json!(false));
        }

        let res = json!({
            "provider": "qishui",
            "loggedIn": true,
            "configured": true,
            "webSession": true,
            "playlist": playlist,
            "tracks": tracks,
            "total": total,
            "offset": offset,
            "limit": limit,
            "nextOffset": offset + tracks.len(),
            "hasMore": cursor_state.has_more || offset + tracks.len() < total,
            "rawCount": cursor_state.raw_items.len(),
        });
        self.web_playlist_cache.set(cache_key, res.clone(), None);
        Ok(res)
    }

    // ---------- 播放地址（带 #auth= 解密参数） ----------

    pub async fn song_url(&self, track_id: &str, quality: &str) -> Result<Value, String> {
        let id = normalize_text(track_id);
        let cookie = self.get_cookie();
        if id.is_empty() {
            return Ok(json!({
                "provider": "qishui",
                "playable": false,
                "url": "",
                "reason": "missing_id",
            }));
        }
        if !qishui_cookie_has_login(&cookie) {
            return Ok(json!({
                "provider": "qishui",
                "playable": false,
                "url": "",
                "loggedIn": false,
                "playbackKeyReady": false,
                "reason": "login_required",
                "message": "Qishui playback requires the local SodaMusic PC login state.",
            }));
        }

        let cache_meta_key = format!("track-v2-meta|{}|{id}", qishui_cookie_fingerprint(&cookie));
        let payload = if let Some(cached) = self.track_metadata_cache.get(&cache_meta_key) {
            cached
        } else {
            let body = json!({
                "track_id": id,
                "media_type": "track",
                "queue_type": "favorite_track_playlist",
                "scene_name": "library",
            });
            let mut url = reqwest::Url::parse(&format!("{QISHUI_WEB_PC_API_BASE}/luna/pc/track_v2"))
                .unwrap();
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in Self::pc_app_params(&[]) {
                    pairs.append_pair(&k, &v);
                }
            }
            let res = self
                .client
                .post(url)
                .timeout(Duration::from_secs(3))
                .header(reqwest::header::USER_AGENT, QISHUI_PC_APP_UA)
                .header(reqwest::header::REFERER, "https://www.qishui.com/")
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/json; charset=utf-8",
                )
                .header(reqwest::header::COOKIE, qishui_session_cookie_header(&cookie))
                .json(&body)
                .send()
                .await;
            let json_payload = match res {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => None,
            };
            let payload = match json_payload {
                Some(p) => p,
                None => {
                    self.web_request_json(
                        "/luna/pc/track_v2",
                        &Self::pc_app_params(&[
                            ("track_id", id.clone()),
                            ("media_type", "track".into()),
                        ]),
                        &[QISHUI_WEB_PC_API_BASE],
                        true,
                        true,
                        3000,
                    )
                    .await?
                }
            };
            self.track_metadata_cache
                .set(cache_meta_key, payload.clone(), None);
            payload
        };

        let membership = self.fetch_playback_membership().await;
        if membership.get("reauthRequired").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(json!({
                "provider": "qishui",
                "playable": false,
                "url": "",
                "loggedIn": false,
                "reauthRequired": true,
                "reason": "login_required",
                "error": "QISHUI_SESSION_EXPIRED",
            }));
        }

        let (track, player, mut streams, fallback_streams) =
            collect_qishui_track_v2_streams(&payload);
        let player_info_url =
            qishui_object_string(&player, &["url_player_info", "URLPlayerInfo", "urlPlayerInfo"]);
        if !player_info_url.is_empty()
            && (player_info_url.starts_with("http://") || player_info_url.starts_with("https://"))
        {
            if let Ok(resp) = self
                .client
                .get(&player_info_url)
                .timeout(Duration::from_secs(3))
                .header(reqwest::header::USER_AGENT, QISHUI_WEB_UA)
                .header(reqwest::header::REFERER, "https://api.qishui.com/")
                .header(reqwest::header::COOKIE, &cookie)
                .send()
                .await
            {
                if let Ok(pi_json) = resp.json::<Value>().await {
                    let result = pick_object(&[pi_json.get("Result"), pi_json.get("result")]);
                    let pi_data = pick_object(&[
                        result.get("Data"),
                        result.get("data"),
                        pi_json.get("Data"),
                        pi_json.get("data"),
                    ]);
                    let list = pick_array(&[
                        pi_data.get("PlayInfoList"),
                        pi_data.get("playInfoList"),
                        pi_data.get("play_info_list"),
                        pi_json.get("PlayInfoList"),
                    ]);
                    for item in list {
                        if let Some(s) = qishui_stream_from_object(&item, "", 0, "") {
                            streams.push(s);
                        }
                    }
                }
            }
        }

        let best = qishui_best_stream_for_membership(&streams, &membership)
            .or_else(|| qishui_best_stream_for_membership(&fallback_streams, &membership));

        let Some(stream) = best else {
            let unrestricted = qishui_best_stream_candidate(&streams)
                .or_else(|| qishui_best_stream_candidate(&fallback_streams));
            let required_tier = unrestricted
                .as_ref()
                .map(|s| s.compute_required_tier())
                .unwrap_or("free");
            let reason = if membership.get("membershipKnown").and_then(|v| v.as_bool()) != Some(true)
                && required_tier != "free"
            {
                "membership_unknown"
            } else if required_tier == "svip" {
                "svip_required"
            } else if required_tier == "vip" {
                "vip_required"
            } else {
                "source_unavailable"
            };
            return Ok(json!({
                "provider": "qishui",
                "playable": false,
                "url": "",
                "loggedIn": true,
                "playbackKeyReady": true,
                "requiredTier": required_tier,
                "reason": reason,
                "membershipKnown": membership["membershipKnown"],
                "vipLevel": membership["vipLevel"],
                "isVip": membership["isVip"],
                "isSvip": membership["isSvip"],
            }));
        };

        let full_duration = qishui_normalize_duration_seconds(
            track
                .get("duration_ms")
                .or_else(|| track.get("duration"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        );
        let duration = if stream.duration > 0 {
            stream.duration
        } else {
            full_duration
        };
        let trial = duration > 0 && full_duration > 0 && duration + 5 < full_duration;
        let kbps = qishui_normalize_bitrate_kbps(stream.bitrate);

        Ok(json!({
            "provider": "qishui",
            "playbackMode": "direct-url",
            "url": qishui_url_with_auth(&stream.url, &stream.auth),
            "playable": true,
            "trial": trial,
            "loggedIn": true,
            "playbackKeyReady": true,
            "membershipKnown": membership["membershipKnown"],
            "vipType": membership["vipType"],
            "vipLevel": membership["vipLevel"],
            "isVip": membership["isVip"],
            "isSvip": membership["isSvip"],
            "vipLabel": membership["vipLabel"],
            "level": stream.playback_level(),
            "quality": if !stream.quality.is_empty() { &stream.quality } else if !stream.format.is_empty() { &stream.format } else { stream.playback_level() },
            "requiredTier": stream.compute_required_tier(),
            "br": if kbps > 0 { kbps * 1000 } else { 0 },
            "size": stream.size,
            "duration": duration,
            "requestedQuality": quality,
            "source": "qishui-pc-track-v2",
            "encrypted": !stream.auth.is_empty(),
        }))
    }

    // ---------- 歌词 ----------

    pub async fn lyric(&self, id: &str) -> Result<Value, String> {
        let track_id = normalize_text(id);
        if track_id.is_empty() {
            return Ok(
                json!({ "provider": "qishui", "lyric": "", "tlyric": "", "source": "none", "error": "Missing Qishui id" }),
            );
        }
        if let Some(cached) = self.lyric_cache.get(&track_id) {
            return Ok(cached);
        }

        let try_cache = |raw_lrc: &str, raw_tlrc: &str, src: &str| -> Option<Value> {
            let (lyric, yrc) = qishui_convert_lyric(raw_lrc);
            let (tlyric, ytlrc) = qishui_convert_lyric(raw_tlrc);
            if lyric.is_empty() && tlyric.is_empty() {
                return None;
            }
            let payload = json!({
                "provider": "qishui",
                "lyric": lyric,
                "tlyric": tlyric,
                "yrc": yrc,
                "ytlrc": ytlrc,
                "source": src,
                "cachedAt": unix_millis(),
            });
            self.lyric_cache
                .set(track_id.clone(), payload.clone(), None);
            Some(payload)
        };

        // 1. SEO H5 track
        let seo_url = format!(
            "https://beta-luna.douyin.com/luna/h5/seo_track?track_id={track_id}&device_platform=web"
        );
        if let Ok(resp) = self
            .client
            .get(&seo_url)
            .timeout(Duration::from_secs(8))
            .header(reqwest::header::USER_AGENT, QISHUI_WEB_UA)
            .header(reqwest::header::REFERER, "https://www.douyin.com/")
            .send()
            .await
        {
            if let Ok(json_val) = resp.json::<Value>().await {
                let (l, tl) = extract_qishui_lyrics(&json_val);
                if let Some(res) = try_cache(&l, &tl, "qishui-beta-seo-track") {
                    return Ok(res);
                }
            }
        }

        // 2. PC track_v2 GET (when logged in)
        if qishui_cookie_has_login(&self.get_cookie()) {
            if let Ok(json_val) = self
                .web_request_json(
                    "/luna/pc/track_v2",
                    &Self::pc_app_params(&[
                        ("track_id", track_id.clone()),
                        ("media_type", "track".into()),
                    ]),
                    &[QISHUI_WEB_PC_API_BASE],
                    true,
                    true,
                    3000,
                )
                .await
            {
                let (l, tl) = extract_qishui_lyrics(&json_val);
                if let Some(res) = try_cache(&l, &tl, "qishui-pc-track-v2") {
                    return Ok(res);
                }
            }
        }

        // 3. Public detail
        let pub_url = format!(
            "{QISHUI_PUBLIC_CONTENTS_URL}?sources=qishui&need_author=true&need_album=true&need_ugc=true&need_stat=true&item_ids={track_id}"
        );
        if let Ok(resp) = self
            .client
            .get(&pub_url)
            .timeout(Duration::from_secs(8))
            .header(reqwest::header::USER_AGENT, QISHUI_PUBLIC_UA)
            .send()
            .await
        {
            if let Ok(json_val) = resp.json::<Value>().await {
                if let Some(item) = json_val
                    .get("data")
                    .and_then(|d| d.get("list"))
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                {
                    let (l, tl) = extract_qishui_lyrics(item);
                    self.public_detail_cache.set(
                        track_id.clone(),
                        (item.clone(), l.clone(), tl.clone()),
                        None,
                    );
                    if let Some(res) = try_cache(&l, &tl, "qishui-public-detail") {
                        return Ok(res);
                    }
                }
            }
        }

        Ok(json!({
            "provider": "qishui",
            "lyric": "",
            "tlyric": "",
            "yrc": "",
            "ytlrc": "",
            "source": "none",
        }))
    }

    // ---------- 喜欢 / 收藏 / 评论 / 播放历史 ----------

    pub async fn song_like_check(&self, track_ids: &[String]) -> Result<Value, String> {
        let cookie = self.get_cookie();
        if track_ids.is_empty() {
            return Ok(
                json!({ "provider": "qishui", "loggedIn": qishui_cookie_has_login(&cookie), "ids": [], "liked": {}, "complete": true }),
            );
        }
        if !qishui_cookie_has_login(&cookie) {
            return Ok(
                json!({ "provider": "qishui", "loggedIn": false, "ids": track_ids, "liked": {}, "complete": false, "error": "QISHUI_COOKIE_REQUIRED" }),
            );
        }
        let lib = self.fetch_web_library().await?;
        let mut known_tracks = lib
            .get("likedTracks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut complete = false;
        if let Some(liked_id) = lib
            .get("likedCard")
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Ok(detail) = self.fetch_web_playlist_tracks(liked_id, 50, 0).await {
                if let Some(arr) = detail.get("tracks").and_then(|v| v.as_array()) {
                    known_tracks.extend(arr.clone());
                    complete = detail.get("hasMore").and_then(|v| v.as_bool()) != Some(true);
                }
            }
        }
        let known_set: HashSet<String> = known_tracks
            .iter()
            .filter_map(|s| {
                s.get("providerSongId")
                    .or_else(|| s.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        let mut liked = serde_json::Map::new();
        for id in track_ids {
            liked.insert(id.clone(), json!(known_set.contains(id)));
        }
        Ok(json!({
            "provider": "qishui",
            "loggedIn": true,
            "ids": track_ids,
            "liked": Value::Object(liked),
            "complete": complete,
            "checkedCount": known_set.len(),
        }))
    }

    pub async fn song_like(&self, track_id: &str, liked: bool) -> Result<Value, String> {
        let id = normalize_text(track_id);
        if id.is_empty() {
            return Err("Missing Qishui track id".to_string());
        }
        let api_path = if liked {
            "/luna/pc/me/collection/media"
        } else {
            "/luna/pc/me/collection/media/delete"
        };
        self.pc_post_json(
            api_path,
            &json!({ "media": [{ "type": "track", "id": id }], "scene": "" }),
            9000,
        )
        .await?;
        self.invalidate_library_caches();
        Ok(json!({ "provider": "qishui", "loggedIn": true, "id": id, "liked": liked, "ok": true }))
    }

    pub async fn playlist_collect(&self, playlist_id: &str, collected: bool) -> Result<Value, String> {
        let id = normalize_text(playlist_id.trim_start_matches("qishui:"));
        if id.is_empty() {
            return Err("Missing Qishui playlist id".to_string());
        }
        let api_path = if collected {
            "/luna/pc/me/collection/playlist"
        } else {
            "/luna/pc/me/collection/playlist/delete"
        };
        self.pc_post_json(api_path, &json!({ "playlist_ids": [id] }), 9000)
            .await?;
        self.invalidate_library_caches();
        Ok(
            json!({ "provider": "qishui", "loggedIn": true, "id": id, "collected": collected, "ok": true }),
        )
    }

    pub async fn playlist_add_song(&self, playlist_id: &str, track_id: &str) -> Result<Value, String> {
        let pid = normalize_text(playlist_id.trim_start_matches("qishui:"));
        let tid = normalize_text(track_id);
        if pid.is_empty() || tid.is_empty() {
            return Err("Missing Qishui playlist or track id".to_string());
        }
        self.pc_post_json(
            "/luna/pc/me/playlist/media/append",
            &json!({ "playlist_id": pid, "media": [{ "id": tid, "type": "track" }] }),
            9000,
        )
        .await?;
        self.invalidate_library_caches();
        Ok(json!({
            "provider": "qishui",
            "loggedIn": true,
            "pid": pid,
            "id": tid,
            "success": true,
            "ok": true,
        }))
    }

    pub async fn album_collect(&self, album_id: &str, collected: bool) -> Result<Value, String> {
        let id = normalize_text(album_id);
        if id.is_empty() {
            return Err("Missing Qishui album id".to_string());
        }
        let api_path = if collected {
            "/luna/pc/me/collection/album"
        } else {
            "/luna/pc/me/collection/album/delete"
        };
        self.pc_post_json(api_path, &json!({ "album_ids": [id] }), 9000)
            .await?;
        self.invalidate_library_caches();
        Ok(
            json!({ "provider": "qishui", "loggedIn": true, "id": id, "collected": collected, "ok": true }),
        )
    }

    pub async fn report_recently_played(&self, track_id: &str) -> Result<Value, String> {
        let id = normalize_text(track_id);
        if id.is_empty() {
            return Err("Missing Qishui track id".to_string());
        }
        self.pc_post_json(
            "/luna/pc/me/recently-played-media",
            &json!({ "media": [{ "type": "track", "id": id }] }),
            6500,
        )
        .await?;
        self.web_library_cache.clear();
        Ok(json!({ "provider": "qishui", "loggedIn": true, "id": id, "reported": true, "ok": true }))
    }

    pub async fn song_comments(
        &self,
        track_id: &str,
        count: usize,
        cursor: &str,
    ) -> Result<Value, String> {
        let id = normalize_text(track_id);
        if id.is_empty() {
            return Ok(
                json!({ "provider": "qishui", "id": "", "comments": [], "total": 0, "error": "Missing Qishui track id" }),
            );
        }
        if !qishui_cookie_has_login(&self.get_cookie()) {
            return Ok(
                json!({ "provider": "qishui", "id": id, "loggedIn": false, "comments": [], "total": 0, "error": "QISHUI_COOKIE_REQUIRED" }),
            );
        }
        let cnt = count.clamp(1, 50);
        let json_resp = self
            .web_request_json(
                "/luna/pc/comments",
                &Self::pc_app_params(&[
                    ("group_id", id.clone()),
                    ("cursor", cursor.to_string()),
                    ("count", cnt.to_string()),
                    ("group_type", "0".into()),
                ]),
                &[QISHUI_WEB_PC_API_BASE],
                true,
                true,
                8500,
            )
            .await?;
        let data = json_resp.get("data").unwrap_or(&json_resp);
        let raw_comments = pick_array(&[
            data.get("comments"),
            data.get("comment_list"),
            data.get("commentList"),
            data.get("items"),
            data.get("list"),
            json_resp.get("comments"),
        ]);
        let comments: Vec<Value> = raw_comments
            .iter()
            .filter_map(map_qishui_comment)
            .collect();
        let next_cursor = qishui_object_string(data, &["next_cursor", "nextCursor", "cursor"]);
        let total = data
            .get("total")
            .or_else(|| data.get("total_count"))
            .or_else(|| data.get("totalCount"))
            .or_else(|| data.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(comments.len() as u64);
        let has_more = data
            .get("has_more")
            .or_else(|| data.get("hasMore"))
            .and_then(|v| v.as_bool())
            .unwrap_or(!next_cursor.is_empty());
        Ok(json!({
            "provider": "qishui",
            "id": id,
            "loggedIn": true,
            "comments": comments,
            "total": total,
            "cursor": cursor,
            "nextCursor": next_cursor,
            "hasMore": has_more,
        }))
    }

    pub async fn create_comment(&self, track_id: &str, text: &str) -> Result<Value, String> {
        let id = normalize_text(track_id);
        let body_text = normalize_lyric_body(text);
        if id.is_empty() || body_text.is_empty() {
            return Err("Missing Qishui track id or comment text".to_string());
        }
        let json_resp = self
            .pc_post_json(
                "/luna/pc/comments/create",
                &json!({ "group_id": id, "text": body_text, "group_type": 0 }),
                9000,
            )
            .await?;
        let data = json_resp.get("data").unwrap_or(&json_resp);
        let comment = map_qishui_comment(data);
        Ok(json!({
            "provider": "qishui",
            "id": id,
            "loggedIn": true,
            "created": true,
            "ok": true,
            "comment": comment,
        }))
    }
}

fn map_qishui_comment(raw: &Value) -> Option<Value> {
    let comment = pick_object(&[
        raw.get("comment"),
        raw.get("comment_info"),
        raw.get("commentInfo"),
        Some(raw),
    ]);
    let content = normalize_lyric_body(&qishui_object_string(
        comment,
        &["text", "content", "comment_text", "commentText"],
    ));
    if content.is_empty() {
        return None;
    }
    let user = pick_object(&[
        comment.get("user"),
        comment.get("user_info"),
        comment.get("userInfo"),
        comment.get("author"),
        raw.get("user"),
        raw.get("user_info"),
        raw.get("author"),
    ]);
    let time_raw = qishui_object_number(
        comment,
        &["create_time", "createTime", "created_at", "createdAt", "time"],
    ) as u64;
    let time_ms = if time_raw > 0 && time_raw < 10_000_000_000 {
        time_raw * 1000
    } else {
        time_raw
    };
    let avatar = qishui_first_image_url(
        "~c5_100x100.jpg",
        &[
            user.get("avatar_url"),
            user.get("avatarUrl"),
            user.get("avatar"),
            user.get("medium_avatar_url"),
            user.get("larger_avatar_url"),
        ],
    );
    Some(json!({
        "id": qishui_object_string(comment, &["id", "comment_id", "commentId"]),
        "content": content,
        "likedCount": qishui_object_number(comment, &["like_count", "likeCount", "digg_count", "diggCount", "liked_count"]) as u64,
        "time": time_ms,
        "user": {
            "id": qishui_object_string(user, &["id", "user_id", "userId", "uid"]),
            "nickname": qishui_object_string(user, &["nickname", "nick_name", "nickName", "name"]),
            "avatar": avatar,
        }
    }))
}

fn extract_playlist_cards(payload: &Value, is_created: bool, out: &mut Vec<Value>) {
    let data = payload.get("data").unwrap_or(payload);
    let mut seen = HashSet::new();
    fn visit(
        node: &Value,
        depth: usize,
        is_created: bool,
        seen: &mut HashSet<String>,
        out: &mut Vec<Value>,
    ) {
        if depth > 6 {
            return;
        }
        if let Some(arr) = node.as_array() {
            for item in arr {
                visit(item, depth + 1, is_created, seen, out);
            }
            return;
        }
        let Some(map) = node.as_object() else { return };
        for cand in [
            map.get("playlist"),
            map.get("playlist_info"),
            map.get("collection"),
            map.get("collect_playlist"),
            map.get("fav_playlist"),
            map.get("resource"),
            Some(node),
        ]
        .into_iter()
        .flatten()
        {
            if !cand.is_object() {
                continue;
            }
            let id = qishui_object_string(
                cand,
                &[
                    "playlist_id",
                    "playlistId",
                    "collection_id",
                    "collectionId",
                    "id",
                    "item_id",
                    "resource_id",
                    "object_id",
                    "server_id",
                ],
            );
            let name = qishui_object_string(
                cand,
                &[
                    "title",
                    "public_title",
                    "publicTitle",
                    "name",
                    "display_title",
                    "display_name",
                    "playlist_name",
                    "collection_name",
                ],
            );
            if id.is_empty() || name.is_empty() {
                continue;
            }
            let count = qishui_object_number(
                cand,
                &["count_tracks", "track_count", "media_count", "count", "total", "song_count"],
            ) as u64;
            let is_like_name = name.contains("喜欢")
                || name.contains("收藏")
                || name.to_ascii_lowercase().contains("favorite")
                || name.to_ascii_lowercase().contains("liked");
            let type_str = qishui_object_string(cand, &["type", "card_type", "resource_type"]);
            let lower_type = format!("{type_str} {name}").to_ascii_lowercase();
            if count == 0
                && !is_like_name
                && !lower_type.contains("playlist")
                && !lower_type.contains("collection")
                && !lower_type.contains("fav")
                && !lower_type.contains("歌单")
            {
                continue;
            }
            let key = format!("{id}|{name}");
            if !seen.insert(key) {
                continue;
            }
            let primary_like = name.contains("喜欢")
                || name.to_ascii_lowercase().contains("favorite")
                || name.to_ascii_lowercase().contains("liked");
            let owned = is_created || primary_like;
            let creator = qishui_object_string(cand, &["creator_name", "author_name", "owner_name"]);
            out.push(json!({
                "provider": "qishui",
                "source": "qishui",
                "type": "playlist",
                "id": id,
                "name": name,
                "cover": qishui_first_image_url("~c5_300x300.jpg", &[cand.get("cover_url"), cand.get("cover"), cand.get("image"), cand.get("image_url")]),
                "trackCount": count,
                "playCount": qishui_object_number(cand, &["play_count", "playCount"]) as u64,
                "creator": if creator.is_empty() { "汽水音乐" } else { &creator },
                "subscribed": !owned,
                "shelfPane": if owned { "mine" } else { "fav" },
                "owned": owned,
                "virtual": false,
                "webSession": true,
                "isLiked": is_like_name,
                "playbackMode": "recommend-match",
            }));
        }
        for v in map.values().take(80) {
            visit(v, depth + 1, is_created, seen, out);
        }
    }
    visit(data, 0, is_created, &mut seen, out);
}

fn build_virtual_playlist(
    id: &str,
    name: &str,
    songs: &[Value],
    cover: &str,
    track_count: usize,
    creator: &str,
) -> Value {
    let resolved_cover = if !cover.is_empty() {
        cover.to_string()
    } else {
        songs
            .iter()
            .filter_map(|s| s.get("cover").and_then(|v| v.as_str()))
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string()
    };
    json!({
        "provider": "qishui",
        "source": "qishui",
        "type": "playlist",
        "id": id,
        "name": name,
        "cover": resolved_cover,
        "trackCount": track_count,
        "playCount": 0,
        "creator": creator,
        "subscribed": false,
        "shelfPane": "mine",
        "owned": true,
        "virtual": true,
        "webSession": true,
        "playbackMode": "recommend-match",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPassportSigner;

    impl QishuiPassportSigner for MockPassportSigner {
        fn request_passport<'a>(
            &'a self,
            _method: &'a str,
            url: &'a str,
            _headers: &'a BTreeMap<String, String>,
            _form_body: Option<&'a str>,
        ) -> BoxFuture<'a, Result<QishuiPassportResponse, String>> {
            Box::pin(async move {
                if url.contains("/passport/web/get_qrcode/") {
                    return Ok(QishuiPassportResponse {
                        status: 200,
                        body: json!({
                            "message": "success",
                            "data": {
                                "error_code": 0,
                                "token": "qr-tok-001",
                                "qrcode_index_url": "https://passport.qishui.com/scan?token=qr-tok-001"
                            }
                        })
                        .to_string(),
                        set_cookies: BTreeMap::new(),
                    });
                }
                if url.contains("/passport/web/check_qrconnect/") {
                    let mut sc = BTreeMap::new();
                    sc.insert("sessionid".into(), "sess-xyz-999".into());
                    return Ok(QishuiPassportResponse {
                        status: 200,
                        body: json!({
                            "message": "success",
                            "data": {
                                "error_code": 0,
                                "status": "3",
                                "session_cookie": "sid_tt=tt-abc-123"
                            }
                        })
                        .to_string(),
                        set_cookies: sc,
                    });
                }
                Err("unexpected passport endpoint".into())
            })
        }
    }

    #[test]
    fn word_by_word_lyric_converts_to_lrc_and_yrc() {
        let raw = "[1000,2500]<0,500,0>Hello <500,800,0>World";
        let (lrc, yrc) = qishui_convert_lyric(raw);
        assert_eq!(lrc, "[00:01.00]Hello World");
        assert_eq!(yrc, "[1000,2500](1000,500,0)Hello (1500,800,0)World");
    }

    #[test]
    fn membership_and_stream_tier_filtering_work() {
        let now = unix_millis();
        let future_sec = (now / 1000) + 3600;
        let vip_mem = qishui_membership_from_data(
            &json!({ "is_vip": true, "vip_expire_time": future_sec }),
            now,
        );
        assert_eq!(vip_mem["isVip"], true);
        assert_eq!(vip_mem["isSvip"], false);

        let payload = json!({
            "data": {
                "track": { "duration_ms": 200000 },
                "track_player": {
                    "video_model": json!({
                        "video_list": {
                            "hires": {
                                "main_url": "https://cdn.example.com/hires.flac",
                                "format": "flac",
                                "quality": "hires",
                                "bitrate": 1411000,
                                "encrypt_info": { "spade_a": "abcdef01" }
                            },
                            "lossless": {
                                "main_url": "https://cdn.example.com/sq.flac",
                                "format": "flac",
                                "quality": "lossless",
                                "bitrate": 950000,
                                "encrypt_info": { "spade_a": "spade+key==" }
                            },
                            "standard": {
                                "main_url": "https://cdn.example.com/std.m4a",
                                "format": "m4a",
                                "quality": "standard",
                                "bitrate": 128000
                            }
                        }
                    }).to_string()
                }
            }
        });
        let (_, _, streams, _) = collect_qishui_track_v2_streams(&payload);
        assert_eq!(streams.len(), 3);

        // VIP cannot select Hi-Res (SVIP), selects Lossless with #auth=
        let best_vip = qishui_best_stream_for_membership(&streams, &vip_mem).unwrap();
        assert_eq!(best_vip.quality, "lossless");
        let auth_url = qishui_url_with_auth(&best_vip.url, &best_vip.auth);
        assert!(auth_url.starts_with("https://cdn.example.com/sq.flac#auth="));
        assert!(auth_url.contains("spade"));

        // Free user gets standard only
        let free_mem = json!({ "membershipKnown": true, "isVip": false, "isSvip": false });
        let best_free = qishui_best_stream_for_membership(&streams, &free_mem).unwrap();
        assert_eq!(best_free.quality, "standard");
    }

    #[test]
    fn public_search_ranking_prefers_exact_title_and_artist_matches() {
        let songs = vec![
            json!({ "id": "1", "name": "Other Song", "artist": "Unknown", "album": "" }),
            json!({ "id": "2", "name": "晴天", "artist": "周杰伦", "album": "叶惠美" }),
            json!({ "id": "3", "name": "晴天娃娃", "artist": "江语晨", "album": "" }),
        ];
        let ranked = rank_qishui_public_songs(songs, "晴天", 10);
        assert_eq!(ranked[0]["id"], "2");
        assert_eq!(ranked[1]["id"], "3");
    }

    #[tokio::test]
    async fn qr_login_lifecycle_persists_cookies_and_handles_logout() {
        let tmp = tempfile::tempdir().unwrap();
        let cookie_file = tmp.path().join(".qishui-cookie");
        let token_file = tmp.path().join(".qishui-token");
        let qr_file = tmp.path().join(".qishui-qr-login.json");

        let client = QishuiClient::new(
            Some(cookie_file.clone()),
            Some(token_file),
            Some(qr_file.clone()),
        )
        .with_passport_signer(Arc::new(MockPassportSigner));

        let qr = client.create_qr_code().await.unwrap();
        let token = qr["data"]["token"].as_str().unwrap().to_string();
        assert_eq!(token, "qr-tok-001");
        assert!(qr["data"]["qrcode"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));

        // Expired/unknown token
        let wrong = client.check_qr_connect("wrong-token").await.unwrap();
        assert_eq!(wrong["data"]["status"], "expired");
        assert_eq!(wrong["data"]["confirmed"], false);

        // Confirm valid token
        let ok = client.check_qr_connect(&token).await.unwrap();
        assert_eq!(ok["data"]["confirmed"], true);
        assert!(qishui_cookie_has_login(&client.get_cookie()));
        assert!(cookie_file.exists());
        assert!(qr_file.exists());

        let status = client.qr_login_status();
        assert_eq!(status["loggedIn"], true);
        assert_eq!(status["qrConfirmed"], true);

        // Logout clears cookie and qr state
        client.logout();
        assert!(!qishui_cookie_has_login(&client.get_cookie()));
        assert_eq!(client.qr_login_status()["loggedIn"], false);
    }
}
