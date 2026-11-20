//! QQ 音乐纯 Rust 客户端与严格账号级 VIP 判定引擎。
//!
//! 对齐原 `qq-vip-api.js` 与 `server.js` 中 QQ 音乐全部接口，
//! 支持 `musicu.fcg`、`smartbox_new.fcg`、移动端搜索签名、Cookie 持久化、
//! 三路 `SRFVipQuery` 会员探测、陈旧正缓存宽限及音频流魔数探针。

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use chrono::DateTime;
use encoding_rs::GB18030;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use super::{
    normalize_quality_preference, parse_cookie_string, playback_restriction,
    probe_playback_audio_url, read_cookie_file, serialize_cookie_object, shared_http_client,
    unix_millis, write_cookie_file, DEFAULT_USER_AGENT,
};

const QQ_MUSICU_URL: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const QQ_SMARTBOX_URL: &str = "https://c.y.qq.com/splcloud/fcgi-bin/smartbox_new.fcg";
const QQ_REFERER: &str = "https://y.qq.com/";

const QQ_LIKED_PLAYLIST_ID: &str = "liked";
const QQ_LIKED_DIRID: u64 = 201;
const QQ_LIKED_PLAYLIST_NAME: &str = "QQ 音乐·我的喜欢";
const QQ_LIKED_PLAYLIST_COVER: &str = "https://y.gtimg.cn/mediastyle/global/img/cover_like.png";
const QQ_LIKED_AUTH_MESSAGE: &str = "QQ 音乐“我的喜欢”需要完整 QQ 音乐授权。请重新打开官方 QQ 音乐登录窗口，等待进入播放器页后再关闭。";

const QQ_VKEY_REQUEST_TIMEOUT_MS: u64 = 6000;
const QQ_AUDIO_PROBE_TOTAL_MS: u64 = 6200;
const QQ_AUDIO_PROBE_ATTEMPT_MS: u64 = 2000;
const QQ_VIP_INFO_CACHE_TTL_MS: u64 = 2 * 60 * 1000;
const QQ_VIP_STALE_POSITIVE_GRACE_MS: u64 = 10 * 60 * 1000;
const MAX_WALK_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy)]
struct QqQualityTemplate {
    prefix: &'static str,
    ext: &'static str,
    level: &'static str,
    label: &'static str,
}

const QQ_QUALITY_CANDIDATE_TEMPLATES: &[QqQualityTemplate] = &[
    QqQualityTemplate {
        prefix: "RS01",
        ext: ".flac",
        level: "hires",
        label: "Hi-Res FLAC",
    },
    QqQualityTemplate {
        prefix: "F000",
        ext: ".flac",
        level: "lossless",
        label: "无损 FLAC",
    },
    QqQualityTemplate {
        prefix: "M800",
        ext: ".mp3",
        level: "exhigh",
        label: "320k MP3",
    },
    QqQualityTemplate {
        prefix: "M500",
        ext: ".mp3",
        level: "standard",
        label: "128k MP3",
    },
    QqQualityTemplate {
        prefix: "C400",
        ext: ".m4a",
        level: "aac",
        label: "AAC/M4A",
    },
];

// ---------- 纯 Rust SHA-1 实现（RFC 3174，供 QQ 搜索签名使用） ----------

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, h) in [h0, h1, h2, h3, h4].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&h.to_be_bytes());
    }
    out
}

/// 生成 QQ 音乐移动搜索接口 `musics.fcg?sign=...` 签名。
pub fn qq_search_sign(text: &str) -> String {
    let hash_bytes = sha1_digest(text.as_bytes());
    let hash_hex = hex::encode(hash_bytes);
    let chars: Vec<char> = hash_hex.chars().collect();
    let pick = |indices: &[usize]| -> String {
        indices
            .iter()
            .filter_map(|&idx| chars.get(idx).copied())
            .collect()
    };
    let part1 = pick(&[23, 14, 6, 36, 16, 40, 7, 19]);
    let part2 = pick(&[16, 1, 32, 12, 19, 27, 8, 5]);
    const SCRAMBLE: [u8; 20] = [
        89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121,
        179,
    ];
    let xored: Vec<u8> = SCRAMBLE
        .iter()
        .zip(hash_bytes.iter())
        .map(|(s, h)| s ^ h)
        .collect();
    let middle: String = BASE64_STD
        .encode(xored)
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | '+' | '='))
        .collect();
    format!("zzc{part1}{middle}{part2}").to_ascii_lowercase()
}

// ---------- Cookie & UIN 解析工具 ----------

fn strip_non_digits_and_leading_zeros(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        digits
    } else {
        trimmed.to_string()
    }
}

/// 规范化 QQ UIN（去除非数字与前导零）。
pub fn normalize_qq_uin(raw: &str) -> String {
    strip_non_digits_and_leading_zeros(raw)
}

/// 从 QQ Cookie 对象中提取主 UIN（区分微信登录与 QQ 登录）。
pub fn qq_cookie_uin(obj: &BTreeMap<String, String>) -> String {
    let is_wechat = obj.get("wxopenid").is_some_and(|v| !v.is_empty())
        || obj
            .get("login_type")
            .and_then(|v| v.parse::<i64>().ok())
            == Some(2);
    let raw = if is_wechat {
        obj.get("wxuin")
            .filter(|v| !v.is_empty())
            .or_else(|| obj.get("uin"))
            .or_else(|| obj.get("p_uin"))
    } else {
        obj.get("uin")
            .filter(|v| !v.is_empty())
            .or_else(|| obj.get("qqmusic_uin"))
            .or_else(|| obj.get("wxuin"))
            .or_else(|| obj.get("p_uin"))
    };
    raw.map(|s| normalize_qq_uin(s)).unwrap_or_default()
}

/// 从 QQ Cookie 中提取会话密钥（包含网页与播放密钥）。
pub fn qq_cookie_music_key(obj: &BTreeMap<String, String>) -> String {
    for key in [
        "qm_keyst",
        "qqmusic_key",
        "music_key",
        "p_skey",
        "skey",
        "psrf_qqaccess_token",
        "psrf_qqrefresh_token",
        "wxrefresh_token",
        "wxskey",
    ] {
        if let Some(val) = obj.get(key) {
            if !val.is_empty() {
                return val.clone();
            }
        }
    }
    String::new()
}

/// 从 QQ Cookie 中提取具备播放授权的关键密钥。
pub fn qq_cookie_playback_key(obj: &BTreeMap<String, String>) -> String {
    for key in ["qm_keyst", "qqmusic_key", "music_key", "wxskey"] {
        if let Some(val) = obj.get(key) {
            if !val.is_empty() {
                return val.clone();
            }
        }
    }
    String::new()
}

/// 解码 QQ Cookie 中的昵称或展示文本（支持 hex、percent-encoding、GB18030、`\uXXXX` 与 Latin-1/UTF-8 错位）。
pub fn decode_qq_cookie_value(value: &str) -> String {
    let raw = value.trim();
    if raw.is_empty() {
        return String::new();
    }
    let mut candidates = vec![raw.to_string()];

    // Hex encoded UTF-8
    if raw.len() >= 4
        && raw.len() % 2 == 0
        && raw.bytes().all(|b| b.is_ascii_hexdigit())
    {
        if let Ok(bytes) = hex::decode(raw) {
            if let Ok(text) = String::from_utf8(bytes) {
                let trimmed = text.trim();
                if !trimmed.is_empty() && trimmed.bytes().any(|b| !(0x20..=0x7e).contains(&b)) {
                    candidates.push(trimmed.to_string());
                }
            }
        }
    }

    // Percent decoding (UTF-8 & GB18030)
    let plus_safe = raw.replace('+', " ");
    let mut percent_bytes = Vec::with_capacity(plus_safe.len());
    let mut has_percent_bytes = false;
    let bytes = plus_safe.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex_str, 16) {
                    percent_bytes.push(b);
                    has_percent_bytes = true;
                    i += 3;
                    continue;
                }
            }
        }
        percent_bytes.push(bytes[i]);
        i += 1;
    }
    if has_percent_bytes && !percent_bytes.is_empty() {
        let (gb_cow, _, _) = GB18030.decode(&percent_bytes);
        candidates.push(gb_cow.trim().to_string());
        candidates.push(String::from_utf8_lossy(&percent_bytes).trim().to_string());
    }

    // Unicode \uXXXX & Latin1-to-UTF8 repair
    let snapshot = candidates.clone();
    for item in snapshot {
        if item.contains("\\u") {
            let quoted = format!("\"{}\"", item.replace('"', "\\\""));
            if let Ok(decoded) = serde_json::from_str::<String>(&quoted) {
                candidates.push(decoded.trim().to_string());
            }
        }
        if item.contains('Ã') || item.contains('Â') {
            let latin1_bytes: Vec<u8> = item.chars().map(|c| c as u8).collect();
            if let Ok(repaired) = String::from_utf8(latin1_bytes) {
                candidates.push(repaired.trim().to_string());
            }
        }
    }

    fn score(text: &str) -> f64 {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return 1e9;
        }
        let mut s = 0.0f64;
        for ch in trimmed.chars() {
            match ch {
                '\u{FFFD}' => s += 80.0,
                'Ã' | 'Â' => s += 34.0,
                '\u{0080}'..='\u{009F}' => s += 42.0,
                '\x00'..='\x08' | '\x0E'..='\x1F' | '\x7F' => s += 50.0,
                '\u{4E00}'..='\u{9FFF}' => s -= 2.0,
                _ => {}
            }
        }
        let chars: Vec<char> = trimmed.chars().collect();
        for w in chars.windows(3) {
            if w[0] == '%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit() {
                s += 10.0;
            }
        }
        s + (trimmed.len().min(80) as f64) * 0.02
    }

    candidates
        .into_iter()
        .filter(|s| !s.is_empty())
        .min_by(|a, b| score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or_default()
}

pub fn qq_cookie_nickname(obj: &BTreeMap<String, String>, uin: &str) -> String {
    let norm_uin = normalize_qq_uin(uin);
    let mut keys = Vec::new();
    if !norm_uin.is_empty() {
        keys.push(format!("ptnick_{norm_uin}"));
        keys.push(format!("ptnick_0{norm_uin}"));
    }
    keys.extend([
        "ptnick".to_string(),
        "nick".to_string(),
        "nickname".to_string(),
        "qq_nickname".to_string(),
    ]);
    for key in &keys {
        if let Some(val) = obj.get(key) {
            let nick = decode_qq_cookie_value(val);
            if !nick.is_empty() {
                return nick;
            }
        }
    }
    for (k, v) in obj {
        if k.to_ascii_lowercase().starts_with("ptnick_") && !v.is_empty() {
            let nick = decode_qq_cookie_value(v);
            if !nick.is_empty() {
                return nick;
            }
        }
    }
    String::new()
}

pub fn qq_cookie_avatar(obj: &BTreeMap<String, String>, uin: &str) -> String {
    for key in ["qqmusic_avatar", "avatar", "avatarUrl", "headpic"] {
        if let Some(v) = obj.get(key) {
            if !v.is_empty() {
                return decode_qq_cookie_value(v);
            }
        }
    }
    let norm_uin = normalize_qq_uin(uin);
    if norm_uin.is_empty() {
        String::new()
    } else {
        format!("https://q1.qlogo.cn/g?b=qq&nk={norm_uin}&s=100")
    }
}

pub fn normalize_qq_cookie_input(cookie_text: &str) -> String {
    let mut obj = parse_cookie_string(cookie_text);
    let is_wechat = obj.get("wxopenid").is_some_and(|v| !v.is_empty())
        || obj
            .get("login_type")
            .and_then(|v| v.parse::<i64>().ok())
            == Some(2);
    if is_wechat {
        if let Some(wxuin) = obj.get("wxuin").cloned().filter(|s| !s.is_empty()) {
            obj.insert("uin".into(), wxuin);
        }
    }
    if obj.get("uin").is_none_or(|s| s.is_empty()) {
        if let Some(alt) = obj
            .get("qqmusic_uin")
            .or_else(|| obj.get("p_uin"))
            .cloned()
        {
            obj.insert("uin".into(), alt);
        }
    }
    if let Some(uin) = obj.get("uin").cloned() {
        let norm = normalize_qq_uin(&uin);
        if !norm.is_empty() {
            obj.insert("uin".into(), norm);
        }
    }
    serialize_cookie_object(&obj)
}

// ---------- 严格账号级 VIP 判定引擎（移植自 qq-vip-api.js） ----------

fn canonical_qq_vip_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_vip_type_key(k: &str) -> bool {
    matches!(
        k,
        "viptype"
            | "viplevel"
            | "musicviptype"
            | "musicviplevel"
            | "greenviptype"
            | "greenviplevel"
            | "greenlevel"
            | "associatortype"
            | "associatorlevel"
    )
}

fn is_svip_type_key(k: &str) -> bool {
    matches!(
        k,
        "sviptype"
            | "sviplevel"
            | "superviptype"
            | "superviplevel"
            | "luxuryviptype"
            | "luxuryviplevel"
            | "greensvip"
    )
}

fn is_vip_flag_key(k: &str) -> bool {
    matches!(
        k,
        "isvip"
            | "ivipflag"
            | "inewvip"
            | "vip"
            | "vipflag"
            | "isgreenvip"
            | "greenvip"
            | "ismember"
            | "member"
            | "isassociator"
            | "associator"
    )
}

fn is_svip_flag_key(k: &str) -> bool {
    matches!(
        k,
        "issvip"
            | "isupervip"
            | "inewsupervip"
            | "svip"
            | "issupervip"
            | "supervip"
            | "isluxuryvip"
            | "luxuryvip"
    )
}

fn is_membership_status_key(k: &str) -> bool {
    matches!(
        k,
        "status" | "state" | "active" | "opened" | "valid" | "vipstatus" | "memberstatus"
    )
}

fn is_membership_level_key(k: &str) -> bool {
    matches!(k, "level" | "viplevel" | "memberlevel")
}

fn is_vip_membership_status_key(k: &str) -> bool {
    matches!(
        k,
        "vipstatus" | "musicvipstatus" | "greenvipstatus" | "associatorstatus"
    )
}

fn is_svip_membership_status_key(k: &str) -> bool {
    matches!(
        k,
        "svipstatus" | "supervipstatus" | "luxuryvipstatus"
    )
}

fn is_vip_membership_expiry_key(k: &str) -> bool {
    matches!(
        k,
        "vipexpireat"
            | "vipexpiretime"
            | "vipexpirytime"
            | "vipendtime"
            | "musicvipexpiretime"
            | "musicvipendtime"
            | "greenvipexpiretime"
            | "greenvipendtime"
            | "associatorexpiretime"
            | "associatorendtime"
    )
}

fn is_svip_membership_expiry_key(k: &str) -> bool {
    matches!(
        k,
        "svipexpiretime"
            | "svipendtime"
            | "supervipexpiretime"
            | "supervipendtime"
            | "superendtime"
            | "luxuryvipexpiretime"
            | "luxuryvipendtime"
    )
}

fn is_qq_membership_expiry_key(k: &str) -> bool {
    let norm = canonical_qq_vip_key(k);
    matches!(
        norm.as_str(),
        "expire"
            | "expires"
            | "expireat"
            | "expiretime"
            | "expiry"
            | "expiryat"
            | "expirytime"
            | "endtime"
            | "validuntil"
            | "validtime"
            | "deadline"
            | "duetime"
    ) || is_vip_membership_expiry_key(&norm)
        || is_svip_membership_expiry_key(&norm)
}

fn primitive_membership_signal(value: &Value) -> Option<u8> {
    match value {
        Value::Bool(true) => Some(1),
        Value::Bool(false) => Some(0),
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).map(|f| if f > 0.0 { 1 } else { 0 }),
        Value::String(s) => {
            let text = s.trim().to_ascii_lowercase();
            if text.is_empty() {
                return None;
            }
            if let Ok(num) = text.parse::<f64>() {
                if num.is_finite() {
                    return Some(if num > 0.0 { 1 } else { 0 });
                }
            }
            if matches!(
                text.as_str(),
                "true"
                    | "yes"
                    | "active"
                    | "valid"
                    | "opened"
                    | "open"
                    | "vip"
                    | "svip"
                    | "premium"
                    | "member"
                    | "已开通"
                    | "有效"
                    | "会员"
                    | "绿钻"
                    | "豪华绿钻"
            ) {
                return Some(1);
            }
            if matches!(
                text.as_str(),
                "false"
                    | "no"
                    | "none"
                    | "normal"
                    | "ordinary"
                    | "expired"
                    | "inactive"
                    | "closed"
                    | "invalid"
                    | "未开通"
                    | "已过期"
                    | "过期"
                    | "普通用户"
                    | "普通账号"
                    | "非会员"
            ) {
                return Some(0);
            }
            None
        }
        _ => None,
    }
}

fn normalized_expiry_ms(value: &Value) -> u64 {
    const MIN_VALID_MS: u64 = 946_684_800_000; // 2000-01-01T00:00:00Z
    let try_num = |n: f64| -> u64 {
        if !n.is_finite() || n <= 0.0 {
            return 0;
        }
        let mut ms = n;
        if ms < 10_000_000_000.0 {
            ms *= 1000.0;
        }
        if ms >= MIN_VALID_MS as f64 {
            ms as u64
        } else {
            0
        }
    };
    match value {
        Value::Number(n) => n.as_f64().map(try_num).unwrap_or(0),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return 0;
            }
            if let Ok(num) = trimmed.parse::<f64>() {
                return try_num(num);
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
                let ms = dt.timestamp_millis();
                if ms > 0 && (ms as u64) >= MIN_VALID_MS {
                    return ms as u64;
                }
            }
            if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
                let ms = naive.and_utc().timestamp_millis();
                if ms > 0 && (ms as u64) >= MIN_VALID_MS {
                    return ms as u64;
                }
            }
            if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
                if let Some(dt) = date.and_hms_opt(0, 0, 0) {
                    let ms = dt.and_utc().timestamp_millis();
                    if ms > 0 && (ms as u64) >= MIN_VALID_MS {
                        return ms as u64;
                    }
                }
            }
            0
        }
        _ => 0,
    }
}

fn object_has_direct_membership_key(obj: &serde_json::Map<String, Value>) -> bool {
    obj.keys().any(|k| {
        let norm = canonical_qq_vip_key(k);
        is_vip_type_key(&norm)
            || is_svip_type_key(&norm)
            || is_vip_flag_key(&norm)
            || is_svip_flag_key(&norm)
            || is_vip_membership_status_key(&norm)
            || is_svip_membership_status_key(&norm)
    })
}

#[derive(Debug, Clone, Copy)]
struct PathContext {
    membership: bool,
    svip: bool,
}

fn path_membership_context(path_parts: &[String]) -> PathContext {
    let text = path_parts
        .iter()
        .map(|p| canonical_qq_vip_key(p))
        .collect::<Vec<_>>()
        .join(" ");
    let membership = text.contains("vip")
        || text.contains("member")
        || text.contains("membership")
        || text.contains("green")
        || text.contains("luxury")
        || text.contains("associator");
    let svip = text.contains("svip") || text.contains("supervip") || text.contains("luxuryvip") || text.contains("luxury");
    PathContext { membership, svip }
}

fn qq_membership_expiry_tier(
    normalized_key: &str,
    ctx: PathContext,
    has_vip_signal: bool,
    has_svip_signal: bool,
) -> &'static str {
    if is_svip_membership_expiry_key(normalized_key) {
        return "svip";
    }
    if is_vip_membership_expiry_key(normalized_key) {
        return "vip";
    }
    if ctx.svip {
        return "svip";
    }
    if has_vip_signal && has_svip_signal {
        return "both";
    }
    if has_svip_signal {
        return "svip";
    }
    "vip"
}

#[derive(Debug, Clone)]
struct QqVipAssessment {
    evidence: bool,
    vip_positive: bool,
    svip_positive: bool,
    vip_negative: bool,
    #[allow(dead_code)]
    svip_negative: bool,
    vip_type: u64,
    svip_type: u64,
    vip_expires_at: u64,
    svip_expires_at: u64,
}

fn assess_qq_vip_object(
    obj: &serde_json::Map<String, Value>,
    ctx: PathContext,
    now_ms: u64,
) -> QqVipAssessment {
    let mut vip_positive = false;
    let mut svip_positive = false;
    let mut vip_negative = false;
    let mut svip_negative = false;
    let mut vip_type = 0u64;
    let mut svip_type = 0u64;
    let mut evidence = false;
    let mut vip_expiry_values = Vec::new();
    let mut svip_expiry_values = Vec::new();

    let has_direct_membership_key = object_has_direct_membership_key(obj);
    let has_direct_vip_signal = obj.keys().any(|k| {
        let norm = canonical_qq_vip_key(k);
        is_vip_type_key(&norm) || is_vip_flag_key(&norm) || is_vip_membership_status_key(&norm)
    });
    let has_direct_svip_signal = obj.keys().any(|k| {
        let norm = canonical_qq_vip_key(k);
        is_svip_type_key(&norm) || is_svip_flag_key(&norm) || is_svip_membership_status_key(&norm)
    });
    let has_paired_membership_signal = has_direct_membership_key
        || (ctx.membership
            && obj.keys().any(|k| {
                let norm = canonical_qq_vip_key(k);
                is_membership_status_key(&norm)
                    || is_vip_membership_status_key(&norm)
                    || is_svip_membership_status_key(&norm)
                    || is_membership_level_key(&norm)
            }));

    for (key, value) in obj {
        let norm = canonical_qq_vip_key(key);
        let signal = primitive_membership_signal(value);
        let numeric = match value {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };

        if has_paired_membership_signal && is_qq_membership_expiry_key(&norm) {
            let expiry = normalized_expiry_ms(value);
            if expiry > 0 {
                let tier = qq_membership_expiry_tier(
                    &norm,
                    ctx,
                    has_direct_vip_signal,
                    has_direct_svip_signal,
                );
                if tier == "vip" || tier == "both" {
                    vip_expiry_values.push(expiry);
                }
                if tier == "svip" || tier == "both" {
                    svip_expiry_values.push(expiry);
                }
            }
            continue;
        }

        if is_vip_type_key(&norm) {
            evidence = true;
            if let Some(num) = numeric.filter(|n| n.is_finite() && *n > 0.0) {
                vip_type = vip_type.max(num as u64);
                vip_positive = true;
            } else if signal == Some(0) {
                vip_negative = true;
            }
            continue;
        }
        if is_svip_type_key(&norm) {
            evidence = true;
            if let Some(num) = numeric.filter(|n| n.is_finite() && *n > 0.0) {
                svip_type = svip_type.max(num as u64);
                svip_positive = true;
            } else if signal == Some(0) {
                svip_negative = true;
            }
            continue;
        }
        if is_vip_flag_key(&norm) {
            let Some(sig) = signal else { continue };
            evidence = true;
            if sig > 0 {
                vip_positive = true;
            } else {
                vip_negative = true;
            }
            continue;
        }
        if is_svip_flag_key(&norm) {
            let Some(sig) = signal else { continue };
            evidence = true;
            if sig > 0 {
                svip_positive = true;
            } else {
                svip_negative = true;
            }
            continue;
        }
        if is_vip_membership_status_key(&norm) || is_svip_membership_status_key(&norm) {
            let Some(sig) = signal else { continue };
            evidence = true;
            if is_svip_membership_status_key(&norm) {
                if sig > 0 {
                    svip_positive = true;
                } else {
                    svip_negative = true;
                }
            } else if sig > 0 {
                vip_positive = true;
            } else {
                vip_negative = true;
            }
            continue;
        }
        if ctx.membership && (is_membership_status_key(&norm) || is_membership_level_key(&norm)) {
            let Some(sig) = signal else { continue };
            evidence = true;
            if ctx.svip {
                if sig > 0 {
                    svip_positive = true;
                } else {
                    svip_negative = true;
                }
            } else if sig > 0 {
                vip_positive = true;
            } else {
                vip_negative = true;
            }
        }
    }

    let vip_expires_at = vip_expiry_values.iter().copied().max().unwrap_or(0);
    let svip_expires_at = svip_expiry_values.iter().copied().max().unwrap_or(0);
    if !vip_expiry_values.is_empty() && vip_expires_at <= now_ms {
        vip_negative = true;
        vip_positive = false;
        vip_type = 0;
    }
    if !svip_expiry_values.is_empty() && svip_expires_at <= now_ms {
        svip_negative = true;
        svip_positive = false;
        svip_type = 0;
    }

    QqVipAssessment {
        evidence,
        vip_positive,
        svip_positive,
        vip_negative,
        svip_negative,
        vip_type,
        svip_type,
        vip_expires_at,
        svip_expires_at,
    }
}

fn is_uin_identity_key(norm: &str) -> bool {
    matches!(norm, "uin" | "useruin" | "userid" | "qq" | "accountuin")
}

fn is_numeric_map_key_uin(key: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)^(?:o0*)?\d+$").unwrap());
    if re.is_match(key.trim()) {
        Some(strip_non_digits_and_leading_zeros(key))
    } else {
        None
    }
}

fn collect_qq_vip_assessments(
    value: &Value,
    out: &mut Vec<QqVipAssessment>,
    path_parts: &mut Vec<String>,
    depth: usize,
    now_ms: u64,
    expected_uin: &str,
) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    match value {
        Value::Array(arr) => {
            for (idx, item) in arr.iter().enumerate() {
                path_parts.push(format!("#{idx}"));
                collect_qq_vip_assessments(item, out, path_parts, depth + 1, now_ms, expected_uin);
                path_parts.pop();
            }
        }
        Value::Object(map) => {
            if !expected_uin.is_empty() {
                if let Some(last) = path_parts.last() {
                    if let Some(path_uin) = is_numeric_map_key_uin(last) {
                        if !path_uin.is_empty() && path_uin != expected_uin {
                            return;
                        }
                    }
                }
                let direct_uins: Vec<String> = map
                    .iter()
                    .filter(|(k, _)| is_uin_identity_key(&canonical_qq_vip_key(k)))
                    .filter_map(|(_, v)| {
                        let s = match v {
                            Value::String(st) => st.clone(),
                            Value::Number(num) => num.to_string(),
                            _ => String::new(),
                        };
                        let u = strip_non_digits_and_leading_zeros(&s);
                        (!u.is_empty()).then_some(u)
                    })
                    .collect();
                if !direct_uins.is_empty() && !direct_uins.iter().any(|u| u == expected_uin) {
                    return;
                }
            }

            let ctx = path_membership_context(path_parts);
            if ctx.membership || object_has_direct_membership_key(map) {
                let assessment = assess_qq_vip_object(map, ctx, now_ms);
                if assessment.evidence {
                    out.push(assessment);
                }
            }
            for (k, v) in map {
                path_parts.push(k.clone());
                collect_qq_vip_assessments(v, out, path_parts, depth + 1, now_ms, expected_uin);
                path_parts.pop();
            }
        }
        _ => {}
    }
}

pub fn empty_qq_vip_status() -> Value {
    json!({
        "vipType": 0,
        "svipType": 0,
        "vipLevel": "none",
        "isVip": false,
        "isSvip": false,
        "vipLabel": "无VIP",
        "membershipKnown": false,
        "resolved": false,
        "decision": "unknown",
        "vipExpiresAt": 0,
        "svipExpiresAt": 0,
        "expiresAt": 0,
    })
}

/// 解析单个或嵌套 QQ VIP 接口响应对象，返回规范化 VIP 状态。
pub fn normalize_qq_vip_payload(payload: &Value, expected_uin: &str, now_ms: u64) -> Value {
    let norm_uin = strip_non_digits_and_leading_zeros(expected_uin);
    let mut assessments = Vec::new();
    let mut path_parts = Vec::new();
    collect_qq_vip_assessments(
        payload,
        &mut assessments,
        &mut path_parts,
        0,
        now_ms,
        &norm_uin,
    );

    let positive: Vec<&QqVipAssessment> = assessments
        .iter()
        .filter(|a| a.vip_positive || a.svip_positive)
        .collect();
    let explicit_vip_negative = assessments.iter().any(|a| a.vip_negative);
    if positive.is_empty() {
        if explicit_vip_negative {
            let mut out = empty_qq_vip_status();
            if let Some(obj) = out.as_object_mut() {
                obj.insert("membershipKnown".into(), json!(true));
                obj.insert("resolved".into(), json!(true));
                obj.insert("decision".into(), json!("negative"));
            }
            return out;
        }
        return empty_qq_vip_status();
    }

    let vip_pos: Vec<&&QqVipAssessment> = positive.iter().filter(|a| a.vip_positive).collect();
    let svip_pos: Vec<&&QqVipAssessment> = positive.iter().filter(|a| a.svip_positive).collect();
    let is_svip = !svip_pos.is_empty();
    let vip_expires_at = vip_pos
        .iter()
        .map(|a| a.vip_expires_at)
        .filter(|&v| v > now_ms)
        .min()
        .unwrap_or(0);
    let svip_expires_at = svip_pos
        .iter()
        .map(|a| a.svip_expires_at)
        .filter(|&v| v > now_ms)
        .min()
        .unwrap_or(0);
    let expires_at = if is_svip {
        svip_expires_at
    } else {
        vip_expires_at
    };
    let vip_type = vip_pos
        .iter()
        .map(|a| a.vip_type)
        .max()
        .unwrap_or(0)
        .max(1);
    let svip_type = if is_svip {
        svip_pos
            .iter()
            .map(|a| a.svip_type)
            .max()
            .unwrap_or(0)
            .max(1)
    } else {
        0
    };

    json!({
        "vipType": vip_type,
        "svipType": svip_type,
        "vipLevel": if is_svip { "svip" } else { "vip" },
        "isVip": true,
        "isSvip": is_svip,
        "vipLabel": if is_svip { "SVIP" } else { "VIP" },
        "membershipKnown": true,
        "resolved": true,
        "decision": "positive",
        "vipExpiresAt": vip_expires_at,
        "svipExpiresAt": svip_expires_at,
        "expiresAt": expires_at,
    })
}

/// 合并多个 VIP 探针结果。
pub fn combine_qq_vip_results(results: &[Value], now_ms: u64) -> Value {
    let positives: Vec<&Value> = results
        .iter()
        .filter(|r| {
            r.get("decision").and_then(|v| v.as_str()) == Some("positive")
                || r.get("isVip").and_then(|v| v.as_bool()) == Some(true)
                || r.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
        })
        .collect();

    if !positives.is_empty() {
        let is_svip = positives.iter().any(|r| {
            r.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
                || r.get("vipLevel").and_then(|v| v.as_str()) == Some("svip")
        });
        let vip_expires_at = positives
            .iter()
            .filter_map(|r| {
                let v = r.get("vipExpiresAt").and_then(|x| x.as_u64()).unwrap_or(0);
                let fallback = if r.get("isSvip").and_then(|x| x.as_bool()) != Some(true)
                    && r.get("vipLevel").and_then(|x| x.as_str()) != Some("svip")
                {
                    r.get("expiresAt").and_then(|x| x.as_u64()).unwrap_or(0)
                } else {
                    0
                };
                let exp = if v > 0 { v } else { fallback };
                (exp > now_ms).then_some(exp)
            })
            .min()
            .unwrap_or(0);
        let svip_expires_at = positives
            .iter()
            .filter_map(|r| {
                let v = r.get("svipExpiresAt").and_then(|x| x.as_u64()).unwrap_or(0);
                let fallback = if r.get("isSvip").and_then(|x| x.as_bool()) == Some(true)
                    || r.get("vipLevel").and_then(|x| x.as_str()) == Some("svip")
                {
                    r.get("expiresAt").and_then(|x| x.as_u64()).unwrap_or(0)
                } else {
                    0
                };
                let exp = if v > 0 { v } else { fallback };
                (exp > now_ms).then_some(exp)
            })
            .min()
            .unwrap_or(0);
        let strongest = positives
            .iter()
            .find(|r| {
                is_svip
                    && (r.get("isSvip").and_then(|x| x.as_bool()) == Some(true)
                        || r.get("vipLevel").and_then(|x| x.as_str()) == Some("svip"))
            })
            .copied()
            .unwrap_or(positives[0]);
        let vip_type = positives
            .iter()
            .filter_map(|r| r.get("vipType").and_then(|x| x.as_u64()))
            .max()
            .unwrap_or(0)
            .max(1);
        let svip_type = if is_svip {
            positives
                .iter()
                .filter_map(|r| r.get("svipType").and_then(|x| x.as_u64()))
                .max()
                .unwrap_or(0)
                .max(1)
        } else {
            0
        };
        let expires_at = if is_svip {
            svip_expires_at
        } else {
            vip_expires_at
        };
        return json!({
            "vipType": vip_type,
            "svipType": svip_type,
            "vipLevel": if is_svip { "svip" } else { "vip" },
            "isVip": true,
            "isSvip": is_svip,
            "vipLabel": if is_svip { "SVIP" } else { "VIP" },
            "membershipKnown": true,
            "resolved": true,
            "decision": "positive",
            "vipExpiresAt": vip_expires_at,
            "svipExpiresAt": svip_expires_at,
            "expiresAt": expires_at,
            "vipSource": strongest.get("vipSource").and_then(|v| v.as_str()).unwrap_or(""),
            "rawCode": strongest.get("rawCode").and_then(|v| v.as_i64()).unwrap_or(0),
        });
    }

    if let Some(neg) = results.iter().find(|r| {
        r.get("decision").and_then(|v| v.as_str()) == Some("negative")
            && r.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true)
    }) {
        let mut out = empty_qq_vip_status();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("membershipKnown".into(), json!(true));
            obj.insert("resolved".into(), json!(true));
            obj.insert("decision".into(), json!("negative"));
            obj.insert(
                "vipSource".into(),
                json!(neg.get("vipSource").and_then(|v| v.as_str()).unwrap_or("")),
            );
            obj.insert(
                "rawCode".into(),
                json!(neg.get("rawCode").and_then(|v| v.as_i64()).unwrap_or(0)),
            );
        }
        return out;
    }

    empty_qq_vip_status()
}

fn qq_vip_probe_response_successful(body: &Value, response_key: &str) -> bool {
    let Some(obj) = body.as_object() else {
        return false;
    };
    for key in ["code", "result"] {
        if let Some(val) = obj.get(key) {
            if !val.is_object() {
                if let Some(num) = val.as_i64() {
                    if num != 0 {
                        return false;
                    }
                }
            }
        }
    }
    if let Some(block) = obj.get(response_key).and_then(|v| v.as_object()) {
        for key in ["code", "result"] {
            if let Some(val) = block.get(key) {
                if !val.is_object() {
                    if let Some(num) = val.as_i64() {
                        if num != 0 {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// 在响应体中定位与目标 `uin` 精确匹配的子树作用域。
pub fn qq_vip_payload_scopes_for_uin(payload: &Value, uin: &str) -> Vec<Value> {
    let expected = strip_non_digits_and_leading_zeros(uin);
    if expected.is_empty() || !payload.is_object() {
        return Vec::new();
    }
    let mut scopes = Vec::new();
    fn walk(
        value: &Value,
        depth: usize,
        expected: &str,
        scopes: &mut Vec<Value>,
    ) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        match value {
            Value::Array(arr) => {
                for item in arr {
                    walk(item, depth + 1, expected, scopes);
                }
            }
            Value::Object(map) => {
                let direct_match = map.iter().any(|(k, v)| {
                    if !is_uin_identity_key(&canonical_qq_vip_key(k)) {
                        return false;
                    }
                    let s = match v {
                        Value::String(st) => st.clone(),
                        Value::Number(num) => num.to_string(),
                        _ => String::new(),
                    };
                    strip_non_digits_and_leading_zeros(&s) == expected
                });
                if direct_match {
                    scopes.push(value.clone());
                }
                for (k, child) in map {
                    if let Some(map_uin) = is_numeric_map_key_uin(k) {
                        if map_uin == expected && child.is_object() {
                            scopes.push(child.clone());
                            continue;
                        }
                    }
                    walk(child, depth + 1, expected, scopes);
                }
            }
            _ => {}
        }
    }
    walk(payload, 0, &expected, &mut scopes);
    scopes
}

#[derive(Debug, Clone)]
pub struct QqVipProbe {
    pub source: String,
    pub response_key: String,
    pub uin: String,
    pub body: Value,
}

/// 并发执行 VIP 探针并根据账号匹配与法定人数（quorum）裁决当前账号 VIP 状态。
pub async fn resolve_qq_vip_from_probes<F, Fut>(
    probes: &[QqVipProbe],
    request_probe: F,
    now_ms: u64,
) -> Value
where
    F: Fn(QqVipProbe) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let req_ref = &request_probe;
    let futs = probes.iter().map(|p| {
        let owned = p.clone();
        async move {
            let res = req_ref(owned).await;
            (p, res)
        }
    });
    let settled = futures_util::future::join_all(futs).await;

    let mut values = Vec::new();
    let mut error_count = 0usize;
    for (probe, res) in settled {
        let Ok(body) = res else {
            error_count += 1;
            continue;
        };
        let scopes = qq_vip_payload_scopes_for_uin(&body, &probe.uin);
        let expected = strip_non_digits_and_leading_zeros(&probe.uin);
        let scoped_results: Vec<Value> = scopes
            .iter()
            .map(|sc| normalize_qq_vip_payload(sc, &expected, now_ms))
            .collect();
        let mut normalized = if scopes.is_empty() {
            empty_qq_vip_status()
        } else {
            combine_qq_vip_results(&scoped_results, now_ms)
        };
        let probe_ok = qq_vip_probe_response_successful(&body, &probe.response_key);
        let raw_code = body
            .get("code")
            .or_else(|| body.get("result"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if let Some(obj) = normalized.as_object_mut() {
            obj.insert("vipSource".into(), json!(probe.source));
            obj.insert("rawCode".into(), json!(raw_code));
            obj.insert("probeSuccessful".into(), json!(probe_ok));
            obj.insert("accountMatched".into(), json!(!scopes.is_empty()));
        }
        values.push(normalized);
    }

    let positive_values: Vec<Value> = values
        .iter()
        .filter(|v| {
            v.get("decision").and_then(|x| x.as_str()) == Some("positive")
                && v.get("probeSuccessful").and_then(|x| x.as_bool()) == Some(true)
        })
        .cloned()
        .collect();
    let negative_values: Vec<Value> = values
        .iter()
        .filter(|v| {
            v.get("decision").and_then(|x| x.as_str()) == Some("negative")
                && v.get("probeSuccessful").and_then(|x| x.as_bool()) == Some(true)
        })
        .cloned()
        .collect();
    let auth_negative_values: Vec<Value> = negative_values
        .iter()
        .filter(|v| v.get("accountMatched").and_then(|x| x.as_bool()) == Some(true))
        .cloned()
        .collect();

    let probe_count = probes.len();
    let completed_probe_count = values
        .iter()
        .filter(|v| v.get("probeSuccessful").and_then(|x| x.as_bool()) == Some(true))
        .count();
    let account_matched_probe_count = values
        .iter()
        .filter(|v| {
            v.get("accountMatched").and_then(|x| x.as_bool()) == Some(true)
                && v.get("probeSuccessful").and_then(|x| x.as_bool()) == Some(true)
        })
        .count();
    let negative_quorum = probe_count.max(1).min(2);
    let authoritative_negative =
        positive_values.is_empty() && auth_negative_values.len() >= negative_quorum;

    let mut combined = if !positive_values.is_empty() {
        combine_qq_vip_results(&positive_values, now_ms)
    } else if authoritative_negative {
        combine_qq_vip_results(&auth_negative_values, now_ms)
    } else {
        empty_qq_vip_status()
    };

    if let Some(obj) = combined.as_object_mut() {
        obj.insert("errorCount".into(), json!(error_count));
        obj.insert("probeCount".into(), json!(probe_count));
        obj.insert("completedProbeCount".into(), json!(completed_probe_count));
        obj.insert(
            "accountMatchedProbeCount".into(),
            json!(account_matched_probe_count),
        );
        obj.insert("positiveProbeCount".into(), json!(positive_values.len()));
        obj.insert(
            "negativeProbeCount".into(),
            json!(auth_negative_values.len()),
        );
        obj.insert("negativeQuorum".into(), json!(negative_quorum));
        obj.insert("authoritativeNegative".into(), json!(authoritative_negative));
        obj.insert(
            "authoritativePositive".into(),
            json!(!positive_values.is_empty()),
        );
        obj.insert(
            "probeIncomplete".into(),
            json!(positive_values.is_empty() && !authoritative_negative),
        );
    }
    combined
}

pub fn qq_vip_session_cache_key(
    uin: &str,
    music_key: &str,
    cookie_obj: &BTreeMap<String, String>,
) -> String {
    let user_id: String = uin.chars().filter(|c| c.is_ascii_digit()).collect();
    if user_id.is_empty() || music_key.is_empty() {
        return String::new();
    }
    let material = [
        user_id.as_str(),
        music_key,
        cookie_obj.get("login_type").map(String::as_str).unwrap_or(""),
        cookie_obj.get("qm_keyst").map(String::as_str).unwrap_or(""),
        cookie_obj
            .get("qqmusic_key")
            .map(String::as_str)
            .unwrap_or(""),
        cookie_obj.get("music_key").map(String::as_str).unwrap_or(""),
        cookie_obj.get("wxskey").map(String::as_str).unwrap_or(""),
        cookie_obj
            .get("psrf_qqaccess_token")
            .map(String::as_str)
            .unwrap_or(""),
        cookie_obj
            .get("psrf_qqrefresh_token")
            .map(String::as_str)
            .unwrap_or(""),
    ]
    .join("\n");
    let digest = hex::encode(Sha256::digest(material.as_bytes()));
    format!("{user_id}:{}", &digest[..32])
}

pub fn qq_vip_cache_ttl_ms(
    status: &Value,
    positive_ttl_ms: u64,
    negative_ttl_ms: u64,
    now_ms: u64,
) -> u64 {
    let resolved = status.get("resolved").and_then(|v| v.as_bool()) == Some(true);
    let known = status.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true);
    if !resolved || !known {
        return 0;
    }
    if status.get("isVip").and_then(|v| v.as_bool()) != Some(true) {
        return negative_ttl_ms;
    }
    let expires_at = status
        .get("expiresAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if expires_at == 0 {
        return positive_ttl_ms;
    }
    positive_ttl_ms.min(expires_at.saturating_sub(now_ms))
}

/// 生成 QQ 会员权益边界描述（`membershipRights`）。
pub fn qq_vip_entitlement_rights(status: &Value) -> Value {
    let is_svip = status.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
        || status.get("vipLevel").and_then(|v| v.as_str()) == Some("svip");
    let is_vip = is_svip
        || status.get("isVip").and_then(|v| v.as_bool()) == Some(true)
        || status.get("vipLevel").and_then(|v| v.as_str()) == Some("vip");
    let membership_known = status
        .get("membershipKnown")
        .and_then(|v| v.as_bool())
        == Some(true);
    let membership_stale = status
        .get("membershipStale")
        .and_then(|v| v.as_bool())
        == Some(true);
    let verified = membership_known && !membership_stale;
    let level = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else {
        "none"
    };
    let sync_state = if membership_stale {
        "stale_positive"
    } else if membership_known {
        "checked"
    } else {
        "unknown"
    };
    let max_quality = if verified {
        if is_svip {
            "hires"
        } else if is_vip {
            "lossless"
        } else {
            "standard"
        }
    } else {
        "standard"
    };
    json!({
        "level": level,
        "membershipKnown": membership_known,
        "verified": verified,
        "syncState": sync_state,
        "canPlayVipTracks": verified && is_vip,
        "canPlaySvipTracks": verified && is_svip,
        "maxQuality": max_quality,
    })
}

#[derive(Debug, Clone)]
pub struct CachedQqVipEntry {
    pub expires_at: u64,
    pub stale_until: u64,
    pub value: Value,
}

/// 当探针不完整时，在宽限期内保留已验证的正向 VIP 状态并标记为 `stale_positive`。
pub fn preserve_qq_vip_stale_positive(
    cached_entry: Option<&CachedQqVipEntry>,
    next_status: Value,
    now_ms: u64,
    grace_ms: u64,
) -> Value {
    let Some(entry) = cached_entry else {
        return next_status;
    };
    let cached = &entry.value;
    let cached_ok = cached.get("isVip").and_then(|v| v.as_bool()) == Some(true)
        && cached.get("resolved").and_then(|v| v.as_bool()) == Some(true)
        && cached.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true);
    if !cached_ok {
        return next_status;
    }
    if next_status.get("isVip").and_then(|v| v.as_bool()) == Some(true)
        && next_status.get("resolved").and_then(|v| v.as_bool()) == Some(true)
    {
        return next_status;
    }
    if next_status
        .get("authoritativeNegative")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return next_status;
    }
    let next_incomplete = next_status.get("decision").and_then(|v| v.as_str()) == Some("unknown")
        || next_status.get("resolved").and_then(|v| v.as_bool()) != Some(true)
        || next_status
            .get("probeIncomplete")
            .and_then(|v| v.as_bool())
            == Some(true);
    if !next_incomplete {
        return next_status;
    }

    let entitlement_expires_at = cached
        .get("expiresAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if entitlement_expires_at > 0 && entitlement_expires_at <= now_ms {
        return next_status;
    }
    let stale_until = if entry.stale_until > 0 {
        entry.stale_until
    } else {
        let cap = if entitlement_expires_at > 0 {
            entitlement_expires_at
        } else {
            u64::MAX
        };
        entry.expires_at.saturating_add(grace_ms).min(cap)
    };
    if now_ms >= stale_until {
        return next_status;
    }

    let probe_decision = next_status
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut out = cached.as_object().cloned().unwrap_or_default();
    out.insert("membershipKnown".into(), json!(true));
    out.insert("resolved".into(), json!(true));
    out.insert("decision".into(), json!("positive"));
    out.insert("membershipStale".into(), json!(true));
    out.insert("vipSyncState".into(), json!("stale_positive"));
    out.insert(
        "vipEvidenceConflict".into(),
        json!(probe_decision == "negative"),
    );
    out.insert("probeDecision".into(), json!(probe_decision));
    out.insert("vipSource".into(), json!("qq-vip-cache-stale-positive"));
    out.insert("staleUntil".into(), json!(stale_until));
    Value::Object(out)
}

fn with_qq_vip_sync_state(mut info: Value, probe_available: bool) -> Value {
    let logged_in = info
        .get("loggedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let playback_ready = info
        .get("playbackKeyReady")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auth_incomplete = logged_in && !playback_ready;
    let membership_unknown =
        logged_in && info.get("membershipKnown").and_then(|v| v.as_bool()) != Some(true);
    let stale_positive = logged_in
        && info.get("membershipStale").and_then(|v| v.as_bool()) == Some(true)
        && info.get("isVip").and_then(|v| v.as_bool()) == Some(true);
    let profile_unavailable = info
        .get("profileUnavailable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let membership_stale = logged_in
        && (auth_incomplete
            || membership_unknown
            || stale_positive
            || (profile_unavailable && !probe_available));
    let sync_state = if auth_incomplete {
        "authorization_incomplete"
    } else if stale_positive {
        "stale_positive"
    } else if membership_unknown {
        "unknown"
    } else if probe_available {
        "checked"
    } else if membership_stale {
        "stale"
    } else {
        "profile"
    };

    if let Some(obj) = info.as_object_mut() {
        obj.insert("membershipStale".into(), json!(membership_stale));
        obj.insert("authorizationIncomplete".into(), json!(auth_incomplete));
        obj.insert("vipSyncState".into(), json!(sync_state));
    }
    let rights = qq_vip_entitlement_rights(&info);
    if let Some(obj) = info.as_object_mut() {
        obj.insert("membershipRights".into(), rights);
    }
    info
}

/// 将主资料与三路 VIP 探针结果合并为最终登录状态。
pub fn merge_qq_vip_status(info: Value, vip: Option<&Value>, source: Option<&str>) -> Value {
    let profile_positive = info.get("isVip").and_then(|v| v.as_bool()) == Some(true)
        || info.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
        || matches!(
            info.get("vipLevel").and_then(|v| v.as_str()),
            Some("vip" | "svip")
        )
        || info.get("vipType").and_then(|v| v.as_u64()).unwrap_or(0) > 0
        || info.get("svipType").and_then(|v| v.as_u64()).unwrap_or(0) > 0;

    let probe_known = vip.is_some_and(|v| {
        v.get("resolved").and_then(|x| x.as_bool()) == Some(true)
            && v.get("membershipKnown").and_then(|x| x.as_bool()) != Some(false)
    });

    if !probe_known {
        let mut out = info.as_object().cloned().unwrap_or_default();
        out.insert("vipCheckedAt".into(), json!(unix_millis()));
        out.insert("vipProbeAvailable".into(), json!(false));
        let src = out
            .get("vipSource")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("profile")
            .to_string();
        out.insert("vipSource".into(), json!(src));
        return with_qq_vip_sync_state(Value::Object(out), false);
    }

    let vip = vip.unwrap();
    let vip_is_vip = vip.get("isVip").and_then(|v| v.as_bool()).unwrap_or(false);
    let auth_neg = vip
        .get("authoritativeNegative")
        .and_then(|v| v.as_bool())
        == Some(true);

    if profile_positive && !vip_is_vip && !auth_neg {
        let mut out = info.as_object().cloned().unwrap_or_default();
        out.insert("membershipKnown".into(), json!(true));
        out.insert("vipCheckedAt".into(), json!(unix_millis()));
        out.insert("vipProbeAvailable".into(), json!(true));
        out.insert("vipEvidenceConflict".into(), json!(true));
        let src = out
            .get("vipSource")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("qq-profile-vip")
            .to_string();
        out.insert("vipSource".into(), json!(src));
        return with_qq_vip_sync_state(Value::Object(out), true);
    }

    let logged_in = info
        .get("loggedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let playback_ready = info.get("playbackKeyReady").and_then(|v| v.as_bool()) == Some(true);
    if logged_in && !playback_ready && !vip_is_vip {
        let mut out = info.as_object().cloned().unwrap_or_default();
        out.insert("vipCheckedAt".into(), json!(unix_millis()));
        out.insert("vipProbeAvailable".into(), json!(false));
        let src = source
            .filter(|s| !s.is_empty())
            .or_else(|| vip.get("vipSource").and_then(|v| v.as_str()))
            .or_else(|| info.get("vipSource").and_then(|v| v.as_str()))
            .unwrap_or("qq-vip-probe-untrusted")
            .to_string();
        out.insert("vipSource".into(), json!(src));
        return with_qq_vip_sync_state(Value::Object(out), false);
    }

    let mut out = info.as_object().cloned().unwrap_or_default();
    out.insert(
        "vipType".into(),
        vip.get("vipType").cloned().unwrap_or(json!(0)),
    );
    out.insert(
        "svipType".into(),
        vip.get("svipType").cloned().unwrap_or(json!(0)),
    );
    out.insert(
        "vipLevel".into(),
        vip.get("vipLevel").cloned().unwrap_or(json!("none")),
    );
    out.insert("isVip".into(), json!(vip_is_vip));
    out.insert(
        "isSvip".into(),
        json!(vip.get("isSvip").and_then(|v| v.as_bool()).unwrap_or(false)),
    );
    out.insert(
        "vipLabel".into(),
        vip.get("vipLabel")
            .cloned()
            .unwrap_or_else(|| json!(if vip_is_vip { "VIP" } else { "无VIP" })),
    );
    out.insert("membershipKnown".into(), json!(true));
    out.insert(
        "membershipStale".into(),
        json!(vip
            .get("membershipStale")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
    );
    out.insert(
        "vipEvidenceConflict".into(),
        json!(vip
            .get("vipEvidenceConflict")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
    );
    out.insert(
        "probeDecision".into(),
        vip.get("probeDecision").cloned().unwrap_or(json!("")),
    );
    out.insert("authoritativeNegative".into(), json!(auth_neg));
    out.insert(
        "probeIncomplete".into(),
        json!(vip
            .get("probeIncomplete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
    );
    out.insert(
        "negativeProbeCount".into(),
        vip.get("negativeProbeCount").cloned().unwrap_or(json!(0)),
    );
    out.insert(
        "negativeQuorum".into(),
        vip.get("negativeQuorum").cloned().unwrap_or(json!(0)),
    );
    out.insert(
        "staleUntil".into(),
        vip.get("staleUntil").cloned().unwrap_or(json!(0)),
    );
    out.insert(
        "expiresAt".into(),
        vip.get("expiresAt").cloned().unwrap_or(json!(0)),
    );
    out.insert("vipCheckedAt".into(), json!(unix_millis()));
    out.insert("vipProbeAvailable".into(), json!(true));
    let src = source
        .filter(|s| !s.is_empty())
        .or_else(|| vip.get("vipSource").and_then(|v| v.as_str()))
        .unwrap_or("qq-vip-probe")
        .to_string();
    out.insert("vipSource".into(), json!(src));
    with_qq_vip_sync_state(Value::Object(out), true)
}

// ---------- QQ 曲目/歌单/歌词映射 ----------

fn parse_json_text(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let stripped = if let Some(rest) = trimmed.strip_prefix("callback(") {
        rest.strip_suffix(");")
            .or_else(|| rest.strip_suffix(')'))
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    serde_json::from_str(stripped).map_err(|e| format!("QQ JSON parse failed: {e}"))
}

pub fn qq_album_cover(album_mid: &str, size: u32) -> String {
    if album_mid.is_empty() {
        return String::new();
    }
    let px = if size == 0 { 300 } else { size };
    format!("https://y.qq.com/music/photo_new/T002R{px}x{px}M000{album_mid}.jpg?max_age=2592000")
}

pub fn qq_singer_avatar(singer_mid: &str, size: u32) -> String {
    if singer_mid.is_empty() {
        return String::new();
    }
    let px = if size == 0 { 300 } else { size };
    format!("https://y.qq.com/music/photo_new/T001R{px}x{px}M000{singer_mid}.jpg?max_age=2592000")
}

fn map_qq_artists(raw: Option<&Value>) -> Vec<Value> {
    let Some(arr) = raw.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|a| {
            let name = a
                .get("name")
                .or_else(|| a.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name.is_empty() {
                return None;
            }
            Some(json!({
                "id": a.get("id").cloned().unwrap_or(Value::Null),
                "mid": a.get("mid").cloned().unwrap_or(Value::Null),
                "name": name,
            }))
        })
        .collect()
}

pub fn map_qq_track(track: &Value, fallback: Option<&Value>) -> Value {
    let empty = json!({});
    let fb = fallback.unwrap_or(&empty);
    let album = track.get("album").unwrap_or(&empty);
    let mut artists = map_qq_artists(track.get("singer"));
    if artists.is_empty() {
        if let Some(arr) = fb.get("artists").and_then(|v| v.as_array()) {
            artists = arr.clone();
        }
    }
    let mid = track
        .get("mid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| fb.get("mid").and_then(|v| v.as_str()))
        .or_else(|| fb.get("songmid").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let album_mid = album
        .get("mid")
        .or_else(|| album.get("pmid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cover = {
        let c = qq_album_cover(&album_mid, 300);
        if !c.is_empty() {
            c
        } else {
            fb.get("cover")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
    };
    let artist_names = {
        let joined = artists
            .iter()
            .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" / ");
        if !joined.is_empty() {
            joined
        } else {
            fb.get("artist")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
    };
    let fee = if track
        .get("pay")
        .and_then(|p| p.get("pay_play"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        > 0
    {
        1
    } else {
        0
    };
    json!({
        "provider": "qq",
        "source": "qq",
        "type": "qq",
        "id": mid,
        "qqId": track.get("id").filter(|v| !v.is_null()).cloned().or_else(|| fb.get("qqId").cloned()).or_else(|| fb.get("id").cloned()).unwrap_or_else(|| json!("")),
        "mid": mid,
        "songmid": mid,
        "mediaMid": track.get("file").and_then(|f| f.get("media_mid")).cloned().unwrap_or(Value::Null),
        "name": track.get("name").or_else(|| track.get("title")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).or_else(|| fb.get("name").and_then(|v| v.as_str())).unwrap_or(""),
        "artist": artist_names,
        "artists": artists,
        "artistId": artists.first().and_then(|a| a.get("id").filter(|v| !v.is_null()).or_else(|| a.get("mid"))).cloned().unwrap_or(Value::Null),
        "artistMid": artists.first().and_then(|a| a.get("mid")).cloned().unwrap_or(Value::Null),
        "album": album.get("name").or_else(|| album.get("title")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).or_else(|| fb.get("album").and_then(|v| v.as_str())).unwrap_or(""),
        "albumMid": album_mid,
        "cover": cover,
        "duration": track.get("interval").and_then(|v| v.as_u64()).unwrap_or(0) * 1000,
        "fee": fee,
        "playable": false,
    })
}

fn map_qq_smart_song(item: &Value) -> Value {
    let mid = item
        .get("mid")
        .or_else(|| item.get("songmid"))
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let singer = item.get("singer").and_then(|v| v.as_str()).unwrap_or("");
    let artists = if singer.is_empty() {
        vec![]
    } else {
        vec![json!({ "name": singer })]
    };
    json!({
        "provider": "qq",
        "source": "qq",
        "type": "qq",
        "id": mid,
        "qqId": item.get("id").or_else(|| item.get("docid")).cloned().unwrap_or_else(|| json!("")),
        "mid": mid,
        "songmid": mid,
        "name": item.get("name").or_else(|| item.get("title")).and_then(|v| v.as_str()).unwrap_or(""),
        "artist": singer,
        "artists": artists,
        "album": "",
        "cover": "",
        "duration": 0,
        "fee": 0,
        "playable": false,
    })
}

pub fn map_qq_playlist_track(raw: &Value) -> Value {
    let empty = json!({});
    let has_direct = raw.get("songid").is_some()
        || raw.get("songmid").is_some()
        || raw.get("mid").is_some()
        || raw.get("name").is_some();
    let track = if has_direct {
        raw
    } else {
        raw.get("track_info")
            .or_else(|| raw.get("songInfo"))
            .or_else(|| raw.get("songinfo"))
            .or_else(|| raw.get("song"))
            .unwrap_or(&empty)
    };
    let album = track.get("album").unwrap_or(&empty);
    let artists = map_qq_artists(track.get("singer").or_else(|| track.get("singers")));
    let mid = track
        .get("mid")
        .or_else(|| track.get("songmid"))
        .or_else(|| raw.get("mid"))
        .or_else(|| raw.get("songmid"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let album_mid = album
        .get("mid")
        .or_else(|| track.get("albummid"))
        .or_else(|| raw.get("albummid"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let qq_id = track
        .get("id")
        .or_else(|| track.get("songid"))
        .or_else(|| raw.get("id"))
        .or_else(|| raw.get("songid"))
        .cloned()
        .unwrap_or_else(|| json!(""));
    let id_str = if !mid.is_empty() {
        mid.to_string()
    } else {
        match &qq_id {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    };
    let artist_str = {
        let joined = artists
            .iter()
            .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" / ");
        if !joined.is_empty() {
            joined
        } else {
            track
                .get("singername")
                .or_else(|| raw.get("singername"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
    };
    let interval = track
        .get("interval")
        .or_else(|| raw.get("interval"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let fee = if track
        .get("pay")
        .and_then(|p| p.get("pay_play"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        > 0
    {
        1
    } else {
        0
    };

    json!({
        "provider": "qq",
        "source": "qq",
        "type": "qq",
        "id": id_str,
        "qqId": qq_id,
        "mid": mid,
        "songmid": mid,
        "mediaMid": track.get("file").and_then(|f| f.get("media_mid")).or_else(|| track.get("strMediaMid")).or_else(|| track.get("media_mid")).or_else(|| raw.get("strMediaMid")).and_then(|v| v.as_str()).unwrap_or(""),
        "name": track.get("name").or_else(|| track.get("songname")).or_else(|| raw.get("songname")).and_then(|v| v.as_str()).unwrap_or(""),
        "artist": artist_str,
        "artists": artists,
        "artistId": artists.first().and_then(|a| a.get("id").filter(|v| !v.is_null()).or_else(|| a.get("mid"))).cloned().unwrap_or(Value::Null),
        "artistMid": artists.first().and_then(|a| a.get("mid")).cloned().unwrap_or(Value::Null),
        "album": album.get("name").or_else(|| album.get("title")).or_else(|| track.get("albumname")).or_else(|| raw.get("albumname")).and_then(|v| v.as_str()).unwrap_or(""),
        "albumMid": album_mid,
        "cover": qq_album_cover(album_mid, 300),
        "duration": interval * 1000,
        "fee": fee,
        "playable": false,
    })
}

fn is_qq_liked_playlist_id(id: &str) -> bool {
    let val = id.trim().to_ascii_lowercase();
    val == QQ_LIKED_PLAYLIST_ID || val == "qq-liked" || val == QQ_LIKED_DIRID.to_string()
}

fn is_qq_favorite_playlist(pl: &Value) -> bool {
    if let Some(id) = pl.get("id").and_then(|v| v.as_str()) {
        if is_qq_liked_playlist_id(id) {
            return true;
        }
    }
    if pl.get("dirid").and_then(|v| v.as_u64()) == Some(QQ_LIKED_DIRID)
        || pl.get("dirid").and_then(|v| v.as_str()) == Some("201")
    {
        return true;
    }
    let name = pl
        .get("name")
        .or_else(|| pl.get("diss_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let normalized: String = name
        .chars()
        .filter(|c| !matches!(c, '·' | '•' | '・' | '_' | '-') && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "我喜欢"
            | "我的喜欢"
            | "喜欢的音乐"
            | "qq音乐我喜欢"
            | "qq音乐我的喜欢"
            | "qq音乐喜欢的音乐"
    )
}

fn is_qzone_background_playlist(pl: &Value) -> bool {
    let name = pl.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let creator = pl.get("creator").and_then(|v| v.as_str()).unwrap_or("");
    let text = format!("{name} {creator}").to_ascii_lowercase();
    text.contains("qzone") || text.contains("空间") || text.contains("背景音乐")
}

fn map_qq_playlist(pl: &Value, kind: &str) -> Value {
    let dirid_str = pl
        .get("dirid")
        .or_else(|| pl.get("dir_id"))
        .map(|v| match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let liked = dirid_str == "201" || is_qq_favorite_playlist(pl);
    let id = if liked {
        QQ_LIKED_PLAYLIST_ID.to_string()
    } else {
        pl.get("dissid")
            .or_else(|| pl.get("tid"))
            .or_else(|| pl.get("dirid"))
            .or_else(|| pl.get("id"))
            .or_else(|| pl.get("diss_id"))
            .map(|v| match v {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    let raw_name = pl
        .get("diss_name")
        .or_else(|| pl.get("name"))
        .or_else(|| pl.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let decoded_name = decode_qq_cookie_value(raw_name);
    let final_name = if liked {
        QQ_LIKED_PLAYLIST_NAME
    } else if !decoded_name.is_empty() {
        decoded_name.as_str()
    } else {
        raw_name
    };
    let cover = if liked {
        QQ_LIKED_PLAYLIST_COVER
    } else {
        pl.get("diss_cover")
            .or_else(|| pl.get("logo"))
            .or_else(|| pl.get("picurl"))
            .or_else(|| pl.get("cover"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    json!({
        "provider": "qq",
        "source": "qq",
        "id": id,
        "dirid": dirid_str,
        "virtual": liked,
        "name": final_name,
        "cover": cover,
        "trackCount": pl.get("song_cnt").or_else(|| pl.get("songnum")).or_else(|| pl.get("total_song_num")).or_else(|| pl.get("song_count")).and_then(|v| v.as_u64()).unwrap_or(0),
        "playCount": pl.get("listen_num").or_else(|| pl.get("visitnum")).or_else(|| pl.get("play_count")).and_then(|v| v.as_u64()).unwrap_or(0),
        "creator": pl.get("hostname").or_else(|| pl.get("nick")).or_else(|| pl.get("creator")).and_then(|v| v.as_str()).unwrap_or("QQ 音乐"),
        "subscribed": kind == "collect",
        "specialType": if liked { 5 } else { 0 },
        "requiresPlaybackKey": false,
    })
}

fn decode_html_entities(text: &str) -> String {
    static HEX_RE: OnceLock<Regex> = OnceLock::new();
    static DEC_RE: OnceLock<Regex> = OnceLock::new();
    let hex_re = HEX_RE.get_or_init(|| Regex::new(r"(?i)&#x([0-9a-f]+);").unwrap());
    let dec_re = DEC_RE.get_or_init(|| Regex::new(r"&#(\d+);").unwrap());

    let step1 = hex_re.replace_all(text, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_else(|| caps[0].to_string())
    });
    let step2 = dec_re.replace_all(&step1, |caps: &regex::Captures| {
        caps[1]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_else(|| caps[0].to_string())
    });
    step2
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
}

/// 解码 QQ 音乐歌词（HTML 实体 + 可选 Base64）。
pub fn decode_qq_lyric_text(text: &str) -> String {
    let mut raw = decode_html_entities(text.trim());
    if raw.is_empty() {
        return String::new();
    }
    let compact: String = raw.split_whitespace().collect();
    let looks_base64 = compact.len() >= 8
        && compact.len() % 4 == 0
        && compact
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
    if looks_base64 && !raw.trim_start().starts_with('[') {
        if let Ok(bytes) = BASE64_STD.decode(&compact) {
            if let Ok(decoded) = String::from_utf8(bytes) {
                let clean = decoded.trim_start_matches('\u{FEFF}');
                if clean.contains('[')
                    || clean
                        .chars()
                        .any(|c| ('\u{4E00}'..='\u{9FA5}').contains(&c))
                {
                    raw = clean.to_string();
                }
            }
        }
    }
    decode_html_entities(&raw).replace("\r\n", "\n").trim().to_string()
}

fn classify_qq_playback_restriction(info: Option<&Value>, has_session: bool, has_playback_key: bool) -> Value {
    let raw_msg = info
        .and_then(|i| {
            i.get("msg")
                .or_else(|| i.get("tips"))
                .or_else(|| i.get("errmsg"))
                .or_else(|| i.get("message"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .trim();
    let code = info
        .and_then(|i| {
            i.get("result")
                .or_else(|| i.get("code"))
                .or_else(|| i.get("errtype"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    let lower = raw_msg.to_ascii_lowercase();

    if !has_session {
        return playback_restriction(
            "qq",
            "login_required",
            "QQ 音乐需要登录或授权后才能获取播放地址",
            "login",
            Some(json!({ "code": code, "rawMessage": raw_msg })),
        );
    }
    if !has_playback_key && code == 104003 {
        return playback_restriction(
            "qq",
            "login_required",
            "QQ 音乐当前只拿到了网页登录状态，还缺少播放授权，请重新打开官方 QQ 音乐登录窗口完成授权",
            "login",
            Some(json!({ "code": code, "rawMessage": raw_msg, "missingPlaybackKey": true })),
        );
    }
    if code == 104003 {
        return playback_restriction(
            "qq",
            "copyright_unavailable",
            "QQ 音乐没有给当前版本返回播放地址，通常是版权、会员或官方版本限制，可以换一个搜索结果或切到网易云源",
            "switch_source",
            Some(json!({ "code": code, "rawMessage": raw_msg })),
        );
    }
    if lower.contains("vip")
        || lower.contains("pay")
        || raw_msg.contains("会员")
        || raw_msg.contains("付费")
        || raw_msg.contains("购买")
        || raw_msg.contains("数字专辑")
        || raw_msg.contains("专辑")
    {
        return playback_restriction(
            "qq",
            "paid_required",
            "QQ 音乐歌曲需要会员、购买或数字专辑权限",
            "upgrade",
            Some(json!({ "code": code, "rawMessage": raw_msg })),
        );
    }
    if code != 0 {
        let msg = if raw_msg.is_empty() {
            "QQ 音乐版权暂不可播或仅官方客户端可播"
        } else {
            raw_msg
        };
        return playback_restriction(
            "qq",
            "copyright_unavailable",
            msg,
            "switch_source",
            Some(json!({ "code": code, "rawMessage": raw_msg })),
        );
    }
    playback_restriction(
        "qq",
        "url_unavailable",
        "QQ 音乐没有返回播放地址，可能受版权、会员或官方客户端限制",
        "switch_source",
        Some(json!({ "code": code, "rawMessage": raw_msg })),
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QqPlaybackHints {
    #[serde(default)]
    pub vip_required: Option<Value>,
    #[serde(default)]
    pub need_vip: Option<Value>,
    #[serde(default)]
    pub only_vip_playable: Option<Value>,
    #[serde(default)]
    pub fee: Option<u64>,
    #[serde(default)]
    pub privilege: Option<u64>,
}

fn truthy_qq_hint(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        Some(Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "vip"
        ),
        _ => false,
    }
}

fn qq_playback_member_hints(hints: Option<&QqPlaybackHints>) -> bool {
    let Some(h) = hints else { return false };
    truthy_qq_hint(h.vip_required.as_ref())
        || truthy_qq_hint(h.need_vip.as_ref())
        || truthy_qq_hint(h.only_vip_playable.as_ref())
        || h.fee.unwrap_or(0) > 0
        || h.privilege.unwrap_or(0) >= 9
}

/// QQ 音乐异步客户端。
pub struct QqClient {
    cookie_file: Option<PathBuf>,
    cookie: RwLock<String>,
    client: reqwest::Client,
    vip_info_cache: Mutex<HashMap<String, CachedQqVipEntry>>,
    liked_cover_cache: Mutex<HashMap<String, String>>,
}

impl QqClient {
    /// 创建 QQ 音乐客户端；若传入持久文件路径且存在则自动读取 Cookie。
    pub fn new(cookie_file: Option<PathBuf>) -> Self {
        let initial = cookie_file
            .as_deref()
            .map(read_cookie_file)
            .map(|s| normalize_qq_cookie_input(&s))
            .unwrap_or_default();
        Self {
            cookie_file,
            cookie: RwLock::new(initial),
            client: shared_http_client().clone(),
            vip_info_cache: Mutex::new(HashMap::new()),
            liked_cover_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_cookie(&self) -> String {
        self.cookie.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn has_cookie(&self) -> bool {
        !self.get_cookie().is_empty()
    }

    fn cookie_obj(&self) -> BTreeMap<String, String> {
        parse_cookie_string(&self.get_cookie())
    }

    fn clear_caches(&self) {
        if let Ok(mut g) = self.vip_info_cache.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.liked_cover_cache.lock() {
            g.clear();
        }
    }

    /// 设置并持久化 QQ 音乐 Cookie。
    pub fn set_cookie(&self, input: &str) -> Result<String, String> {
        let normalized = normalize_qq_cookie_input(input);
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
        self.clear_caches();
        Ok(value)
    }

    /// 退出 QQ 登录并清除本地 Cookie。
    pub fn logout(&self) -> Value {
        let _ = self.set_cookie("");
        json!({ "ok": true })
    }

    async fn musicu_request(
        &self,
        payload: &Value,
        with_cookie: bool,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let body = serde_json::to_string(payload).map_err(|e| format!("QQ JSON encode: {e}"))?;
        let mut req = self
            .client
            .post(QQ_MUSICU_URL)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, QQ_REFERER)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/json;charset=UTF-8",
            );
        let cookie = self.get_cookie();
        if with_cookie && !cookie.is_empty() {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .body(body)
            .send()
            .await
            .map_err(|e| format!("QQ musicu request failed: {e}"))?;
        let text = resp
            .text()
            .await
            .map_err(|e| format!("QQ musicu read failed: {e}"))?;
        parse_json_text(&text)
    }

    async fn get_json(
        &self,
        base_url: &str,
        params: &[(&str, String)],
        referer: Option<&str>,
        with_cookie: bool,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let mut url = reqwest::Url::parse(base_url).map_err(|e| format!("Invalid URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        let mut req = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, referer.unwrap_or(QQ_REFERER));
        let cookie = self.get_cookie();
        if with_cookie && !cookie.is_empty() {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("QQ GET request failed: {e}"))?;
        let text = resp
            .text()
            .await
            .map_err(|e| format!("QQ GET read failed: {e}"))?;
        parse_json_text(&text)
    }

    // ---------- 搜索与歌曲详情 ----------

    async fn full_song_search(
        &self,
        keywords: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, String> {
        let lim = limit.clamp(1, 30);
        let page_num = offset / lim + 1;
        let rand_suffix: u32 = rand::thread_rng().gen_range(100_000..999_999);
        let searchid = format!("{}{}", unix_millis(), rand_suffix);
        let payload = json!({
            "comm": {
                "ct": "11", "cv": "14090508", "v": "14090508", "tmeAppID": "qqmusic",
                "phonetype": "EBG-AN10", "os_ver": "12", "OpenUDID": "0", "QIMEI36": "0",
                "udid": "0", "chid": "0", "aid": "0", "oaid": "0", "taid": "0", "tid": "0",
                "wid": "0", "uid": "0", "sid": "0", "modeSwitch": "6", "teenMode": "0",
                "ui_mode": "2", "nettype": "1020"
            },
            "req": {
                "module": "music.search.SearchCgiService",
                "method": "DoSearchForQQMusicMobile",
                "param": {
                    "search_type": 0,
                    "searchid": searchid,
                    "query": keywords,
                    "page_num": page_num,
                    "num_per_page": lim,
                    "highlight": 0,
                    "nqc_flag": 0,
                    "multi_zhida": 0,
                    "cat": 2,
                    "grp": 1,
                    "sin": offset,
                    "sem": 0
                }
            }
        });
        let body_text = serde_json::to_string(&payload).map_err(|e| format!("JSON error: {e}"))?;
        let sign = qq_search_sign(&body_text);
        let url = format!("https://u.y.qq.com/cgi-bin/musics.fcg?sign={sign}");

        let resp = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(10))
            .header(reqwest::header::USER_AGENT, "QQMusic 14090508(android 12)")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body_text)
            .send()
            .await
            .map_err(|e| format!("QQ search failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("QQ search JSON parse failed: {e}"))?;

        let data = json.get("req").and_then(|r| r.get("data"));
        let body = data.and_then(|d| d.get("body")).or(data);
        let items = body
            .and_then(|b| {
                b.get("item_song")
                    .or_else(|| b.get("song").and_then(|s| s.get("list")))
                    .or_else(|| b.get("list"))
            })
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(items
            .iter()
            .map(|item| {
                let raw = item
                    .get("track_info")
                    .or_else(|| item.get("songInfo"))
                    .or_else(|| item.get("songinfo"))
                    .or_else(|| item.get("song"))
                    .unwrap_or(item);
                map_qq_track(raw, None)
            })
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("mid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect())
    }

    async fn smartbox_search(&self, keywords: &str, limit: usize) -> Result<Vec<Value>, String> {
        let json = self
            .get_json(
                QQ_SMARTBOX_URL,
                &[
                    ("format", "json".into()),
                    ("key", keywords.to_string()),
                    ("g_tk", "5381".into()),
                    ("loginUin", "0".into()),
                    ("hostUin", "0".into()),
                    ("inCharset", "utf8".into()),
                    ("outCharset", "utf-8".into()),
                    ("notice", "0".into()),
                    ("platform", "yqq.json".into()),
                    ("needNewCode", "0".into()),
                ],
                None,
                false,
                8000,
            )
            .await?;
        let items = json
            .get("data")
            .and_then(|d| d.get("song"))
            .and_then(|s| s.get("itemlist"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .take(limit.clamp(1, 10))
            .map(map_qq_smart_song)
            .collect())
    }

    async fn song_detail(&self, mid: &str, fallback: Value) -> Value {
        if mid.is_empty() {
            return fallback;
        }
        let payload = json!({
            "comm": { "ct": 24, "cv": 0 },
            "songinfo": {
                "module": "music.pf_song_detail_svr",
                "method": "get_song_detail_yqq",
                "param": { "song_mid": mid }
            }
        });
        match self.musicu_request(&payload, false, 6000).await {
            Ok(json) => {
                if let Some(track) = json
                    .get("songinfo")
                    .and_then(|s| s.get("data"))
                    .and_then(|d| d.get("track_info"))
                {
                    map_qq_track(track, Some(&fallback))
                } else {
                    fallback
                }
            }
            Err(_) => fallback,
        }
    }

    /// 搜索 QQ 音乐歌曲（`DoSearchForQQMusicMobile` 优先，空结果时回退 `smartbox_new`，并并发补全曲目详情）。
    pub async fn search(
        &self,
        keywords: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, String> {
        let kw = keywords.trim();
        if kw.is_empty() {
            return Ok(Vec::new());
        }
        let lim = limit.clamp(1, 30);
        let mut base = self.full_song_search(kw, lim, offset).await.unwrap_or_default();
        if base.is_empty() && offset == 0 {
            base = self.smartbox_search(kw, lim).await.unwrap_or_default();
        }
        let detail_futs = base.into_iter().map(|item| async move {
            let mid = item
                .get("mid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.song_detail(&mid, item).await
        });
        let detailed = futures_util::future::join_all(detail_futs).await;

        let mut seen = HashSet::new();
        Ok(detailed
            .into_iter()
            .filter(|song| {
                let name = song.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    return false;
                }
                let mid = song.get("mid").and_then(|v| v.as_str()).unwrap_or("");
                let id = song.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let key = if !mid.is_empty() {
                    mid.to_string()
                } else if !id.is_empty() {
                    id.to_string()
                } else {
                    format!(
                        "{name}|{}",
                        song.get("artist").and_then(|v| v.as_str()).unwrap_or("")
                    )
                };
                seen.insert(key)
            })
            .collect())
    }

    // ---------- 播放地址（CgiGetVkey + 音频探针） ----------

    pub async fn song_url(
        &self,
        mid: &str,
        media_mid: &str,
        quality: &str,
        playback_hints: Option<&QqPlaybackHints>,
    ) -> Result<Value, String> {
        let songmid = mid.trim();
        if songmid.is_empty() {
            return Ok(
                json!({ "provider": "qq", "url": "", "error": "MISSING_MID", "message": "Missing QQ song mid" }),
            );
        }
        let guid = rand::thread_rng()
            .gen_range(10_000_000..99_999_999)
            .to_string();
        let cookie_obj = self.cookie_obj();
        let uin = {
            let u = qq_cookie_uin(&cookie_obj);
            if u.is_empty() { "0".to_string() } else { u }
        };
        let playback_key = qq_cookie_playback_key(&cookie_obj);
        let has_playback_session = uin != "0" && !playback_key.is_empty();
        let requested_quality = normalize_quality_preference(quality);
        let member_track_hint = qq_playback_member_hints(playback_hints);

        let mut media_ids = Vec::new();
        let file_media_mid = media_mid.trim();
        if !file_media_mid.is_empty() {
            media_ids.push(file_media_mid.to_string());
        }
        if !media_ids.iter().any(|m| m == songmid) {
            media_ids.push(songmid.to_string());
        }

        let start_idx = QQ_QUALITY_CANDIDATE_TEMPLATES
            .iter()
            .position(|t| t.level == requested_quality)
            .unwrap_or(0);
        let mut file_candidates = Vec::new();
        for media_id in &media_ids {
            for tpl in &QQ_QUALITY_CANDIDATE_TEMPLATES[start_idx..] {
                file_candidates.push((
                    tpl.level,
                    tpl.label,
                    format!("{}{}{}", tpl.prefix, media_id, tpl.ext),
                ));
            }
        }
        let filenames: Vec<String> = file_candidates.iter().map(|c| c.2.clone()).collect();
        let songmids: Vec<&str> = if filenames.is_empty() {
            vec![songmid]
        } else {
            vec![songmid; filenames.len()]
        };
        let songtypes: Vec<u8> = vec![0; songmids.len()];

        let mut param = json!({
            "guid": guid,
            "songmid": songmids,
            "songtype": songtypes,
            "uin": uin,
            "loginflag": 1,
            "platform": "20",
        });
        if !filenames.is_empty() {
            param["filename"] = json!(filenames);
        }
        let mut comm = json!({
            "uin": uin,
            "format": "json",
            "ct": if playback_key.is_empty() { 24 } else { 19 },
            "cv": 0,
        });
        if !playback_key.is_empty() {
            comm["authst"] = json!(playback_key);
        }

        let json_resp = self
            .musicu_request(
                &json!({
                    "comm": comm,
                    "req_0": {
                        "module": "vkey.GetVkeyServer",
                        "method": "CgiGetVkey",
                        "param": param,
                    }
                }),
                true,
                QQ_VKEY_REQUEST_TIMEOUT_MS,
            )
            .await?;

        let data = json_resp.get("req_0").and_then(|r| r.get("data"));
        let infos = data
            .and_then(|d| d.get("midurlinfo"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let purl_infos: Vec<Value> = infos
            .iter()
            .filter(|item| {
                item.get("purl")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
            })
            .cloned()
            .collect();
        let sips: Vec<String> = data
            .and_then(|d| d.get("sip"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["https://ws.stream.qqmusic.qq.com/".to_string()]);

        let probe_deadline = unix_millis() + QQ_AUDIO_PROBE_TOTAL_MS;
        let mut probe_failures = Vec::new();
        let mut playable_info: Option<Value> = None;
        let mut playable_url = String::new();

        'outer: for candidate_info in &purl_infos {
            let purl = candidate_info
                .get("purl")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let fname = candidate_info
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            for sip in &sips {
                let rem = probe_deadline.saturating_sub(unix_millis());
                if rem < 300 {
                    break 'outer;
                }
                let candidate_url = format!("{sip}{purl}");
                let probe = probe_playback_audio_url(
                    &self.client,
                    &candidate_url,
                    QQ_REFERER,
                    Duration::from_millis(rem.min(QQ_AUDIO_PROBE_ATTEMPT_MS)),
                )
                .await;
                if probe.ok {
                    playable_info = Some(candidate_info.clone());
                    playable_url = candidate_url;
                    break 'outer;
                }
                let label = file_candidates
                    .iter()
                    .find(|c| c.2 == fname)
                    .map(|c| c.1)
                    .unwrap_or(if fname.is_empty() { "unknown" } else { fname });
                let reason = if probe.status > 0 {
                    probe.status.to_string()
                } else if !probe.reason.is_empty() {
                    probe.reason
                } else {
                    "failed".to_string()
                };
                probe_failures.push(format!("{label}:{reason}"));
            }
        }

        if let Some(info) = playable_info {
            let fname = info
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let meta = file_candidates.iter().find(|c| c.2 == fname);
            return Ok(json!({
                "provider": "qq",
                "url": playable_url,
                "trial": false,
                "playable": true,
                "playbackReady": true,
                "loggedIn": has_playback_session,
                "userId": if has_playback_session { &uin } else { "" },
                "playbackKeyReady": uin != "0" && !playback_key.is_empty(),
                "vipRequired": member_track_hint,
                "level": meta.map(|m| m.0).unwrap_or(fname),
                "quality": meta.map(|m| m.1).unwrap_or(fname),
                "filename": fname,
                "probeFailures": probe_failures.into_iter().take(12).collect::<Vec<_>>(),
                "requestedQuality": requested_quality,
            }));
        }

        let last_info = purl_infos.first().or_else(|| infos.first());
        let has_session = uin != "0" && !qq_cookie_music_key(&cookie_obj).is_empty();
        let restriction = classify_qq_playback_restriction(
            last_info,
            has_session,
            uin != "0" && !playback_key.is_empty(),
        );

        Ok(json!({
            "provider": "qq",
            "url": "",
            "playable": false,
            "error": "QQ_URL_UNAVAILABLE",
            "loggedIn": has_playback_session,
            "playbackKeyReady": uin != "0" && !playback_key.is_empty(),
            "userId": if has_playback_session { &uin } else { "" },
            "vipRequired": member_track_hint,
            "restriction": restriction,
            "reason": restriction["category"],
            "message": restriction["message"],
            "qqCode": last_info.and_then(|i| i.get("result").or_else(|| i.get("code")).or_else(|| i.get("errtype"))).cloned().unwrap_or(Value::Null),
            "rawMessage": last_info.and_then(|i| i.get("msg").or_else(|| i.get("tips")).or_else(|| i.get("errmsg"))).and_then(|v| v.as_str()).unwrap_or(""),
            "tried": file_candidates.iter().map(|c| format!("{} · {}", c.1, c.2)).collect::<Vec<_>>(),
            "probeFailures": probe_failures.into_iter().take(12).collect::<Vec<_>>(),
            "requestedQuality": requested_quality,
        }))
    }

    // ---------- 歌词 ----------

    pub async fn lyric(&self, mid: &str, id: Option<&str>) -> Result<Value, String> {
        let song_mid = mid.trim();
        let song_id: u64 = id
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        if song_mid.is_empty() && song_id == 0 {
            return Ok(
                json!({ "provider": "qq", "error": "Missing QQ song mid or id", "lyric": "" }),
            );
        }

        let mut param = serde_json::Map::new();
        if !song_mid.is_empty() {
            param.insert("songMID".into(), json!(song_mid));
        }
        if song_id > 0 {
            param.insert("songID".into(), json!(song_id));
        }

        let mut lyric_text = String::new();
        let mut trans_text = String::new();
        let mut qrc_text = String::new();
        let mut roma_text = String::new();
        let mut source = "qq-musicu";

        if let Ok(json_resp) = self
            .musicu_request(
                &json!({
                    "comm": { "ct": 24, "cv": 0 },
                    "lyric": {
                        "module": "music.musichallSong.PlayLyricInfo",
                        "method": "GetPlayLyricInfo",
                        "param": Value::Object(param),
                    }
                }),
                true,
                8000,
            )
            .await
        {
            if let Some(data) = json_resp.get("lyric").and_then(|l| l.get("data")) {
                lyric_text = decode_qq_lyric_text(
                    data.get("lyric").and_then(|v| v.as_str()).unwrap_or(""),
                );
                trans_text = decode_qq_lyric_text(
                    data.get("trans").and_then(|v| v.as_str()).unwrap_or(""),
                );
                qrc_text = decode_qq_lyric_text(
                    data.get("qrc").and_then(|v| v.as_str()).unwrap_or(""),
                );
                roma_text = decode_qq_lyric_text(
                    data.get("roma").and_then(|v| v.as_str()).unwrap_or(""),
                );
            }
        }

        if lyric_text.is_empty() && !song_mid.is_empty() {
            let uin = qq_cookie_uin(&self.cookie_obj());
            if let Ok(body) = self
                .get_json(
                    "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg",
                    &[
                        ("songmid", song_mid.to_string()),
                        ("songtype", "0".into()),
                        ("format", "json".into()),
                        ("nobase64", "1".into()),
                        ("g_tk", "5381".into()),
                        (
                            "loginUin",
                            if uin.is_empty() { "0".into() } else { uin },
                        ),
                        ("hostUin", "0".into()),
                        ("inCharset", "utf8".into()),
                        ("outCharset", "utf-8".into()),
                        ("notice", "0".into()),
                        ("platform", "yqq.json".into()),
                        ("needNewCode", "0".into()),
                    ],
                    Some("https://y.qq.com/portal/player.html"),
                    true,
                    8000,
                )
                .await
            {
                lyric_text = decode_qq_lyric_text(
                    body.get("lyric").and_then(|v| v.as_str()).unwrap_or(""),
                );
                if trans_text.is_empty() {
                    trans_text = decode_qq_lyric_text(
                        body.get("trans")
                            .or_else(|| body.get("tlyric"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    );
                }
                source = "qq-legacy";
            }
        }

        Ok(json!({
            "provider": "qq",
            "id": if song_id > 0 { json!(song_id) } else { json!("") },
            "mid": song_mid,
            "lyric": lyric_text,
            "tlyric": trans_text,
            "yrc": "",
            "qrc": qrc_text,
            "roma": roma_text,
            "source": if lyric_text.is_empty() { "qq-empty" } else { source },
        }))
    }

    // ---------- 登录状态与 VIP 探测 ----------

    fn normalize_profile(&self, body: Option<&Value>, cookie_obj: &BTreeMap<String, String>) -> Value {
        let uin = qq_cookie_uin(cookie_obj);
        let empty = json!({});
        let data = body
            .and_then(|b| {
                b.get("data")
                    .or_else(|| b.get("profile"))
                    .or_else(|| b.get("creator"))
                    .or_else(|| b.get("result"))
            })
            .unwrap_or(&empty);
        let creator = data
            .get("creator")
            .or_else(|| data.get("user"))
            .or_else(|| data.get("profile"))
            .unwrap_or(data);
        let vip_info = data
            .get("vipInfo")
            .or_else(|| data.get("vipinfo"))
            .or_else(|| data.get("vip"))
            .or_else(|| creator.get("vipInfo"))
            .or_else(|| creator.get("vipinfo"))
            .unwrap_or(&empty);

        let profile_nick = decode_qq_cookie_value(
            creator
                .get("nick")
                .or_else(|| creator.get("nickname"))
                .or_else(|| creator.get("name"))
                .or_else(|| creator.get("hostname"))
                .or_else(|| creator.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let profile_avatar = creator
            .get("headpic")
            .or_else(|| creator.get("avatar"))
            .or_else(|| creator.get("avatarUrl"))
            .or_else(|| creator.get("logo"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cookie_nick = qq_cookie_nickname(cookie_obj, &uin);
        let nick = if !profile_nick.is_empty() {
            profile_nick.clone()
        } else {
            cookie_nick.clone()
        };
        let avatar = if !profile_avatar.is_empty() {
            profile_avatar.clone()
        } else {
            qq_cookie_avatar(cookie_obj, &uin)
        };

        let profile_vip = normalize_qq_vip_payload(
            &json!({ "data": data, "creator": creator, "vipInfo": vip_info }),
            "",
            unix_millis(),
        );
        let music_key = qq_cookie_music_key(cookie_obj);
        let playback_key = qq_cookie_playback_key(cookie_obj);
        let nickname = if !nick.is_empty() {
            nick
        } else if !uin.is_empty() {
            format!("QQ {uin}")
        } else {
            "QQ 音乐".to_string()
        };
        let profile_source = if !profile_nick.is_empty() || !profile_avatar.is_empty() {
            "qq-profile"
        } else if !cookie_nick.is_empty() || !avatar.is_empty() {
            "cookie"
        } else {
            "fallback"
        };

        json!({
            "provider": "qq",
            "loggedIn": !uin.is_empty() && !music_key.is_empty(),
            "preview": false,
            "userId": uin,
            "nickname": nickname,
            "avatar": avatar,
            "vipType": profile_vip["vipType"],
            "svipType": profile_vip["svipType"],
            "vipLevel": profile_vip["vipLevel"],
            "isVip": profile_vip["isVip"],
            "isSvip": profile_vip["isSvip"],
            "vipLabel": profile_vip["vipLabel"],
            "membershipKnown": profile_vip["membershipKnown"],
            "expiresAt": profile_vip["expiresAt"],
            "hasCookie": self.has_cookie(),
            "playbackKeyReady": !playback_key.is_empty(),
            "profileSource": profile_source,
            "vipSource": if profile_vip["resolved"].as_bool() == Some(true) { "qq-profile-vip" } else { "profile" },
        })
    }

    async fn fetch_vip_status(
        &self,
        cookie_obj: &BTreeMap<String, String>,
        force: bool,
    ) -> Option<Value> {
        let uin = qq_cookie_uin(cookie_obj);
        let music_key = qq_cookie_playback_key(cookie_obj);
        if uin.is_empty() || music_key.is_empty() {
            return None;
        }
        let cache_key = qq_vip_session_cache_key(&uin, &music_key, cookie_obj);
        let cached = if !cache_key.is_empty() {
            self.vip_info_cache
                .lock()
                .ok()
                .and_then(|g| g.get(&cache_key).cloned())
        } else {
            None
        };
        let now = unix_millis();
        if !force {
            if let Some(ref entry) = cached {
                if now < entry.expires_at {
                    return Some(entry.value.clone());
                }
            }
        }

        let comm = json!({
            "uin": uin,
            "format": "json",
            "ct": 24,
            "cv": 0,
            "authst": music_key,
        });
        let probes = vec![
            QqVipProbe {
                source: "qq-vip-query-v2-list".into(),
                response_key: "req_1".into(),
                uin: uin.clone(),
                body: json!({
                    "comm": comm,
                    "req_1": {
                        "module": "userInfo.VipQueryServer",
                        "method": "SRFVipQuery_V2",
                        "param": { "uin_list": [uin] },
                    }
                }),
            },
            QqVipProbe {
                source: "qq-vip-query-v1-list".into(),
                response_key: "req_1".into(),
                uin: uin.clone(),
                body: json!({
                    "comm": comm,
                    "req_1": {
                        "module": "userInfo.VipQueryServer",
                        "method": "SRFVipQuery",
                        "param": { "uin_list": [uin] },
                    }
                }),
            },
            QqVipProbe {
                source: "qq-vip-query-v2-single".into(),
                response_key: "vip".into(),
                uin: uin.clone(),
                body: json!({
                    "comm": comm,
                    "vip": {
                        "module": "userInfo.VipQueryServer",
                        "method": "SRFVipQuery_V2",
                        "param": { "uin": uin, "uin_list": [uin] },
                    }
                }),
            },
        ];

        let value = resolve_qq_vip_from_probes(
            &probes,
            |probe: QqVipProbe| async move { self.musicu_request(&probe.body, true, 4200).await },
            now,
        )
        .await;

        let now2 = unix_millis();
        let stable_value = preserve_qq_vip_stale_positive(
            cached.as_ref(),
            value.clone(),
            now2,
            QQ_VIP_STALE_POSITIVE_GRACE_MS,
        );
        if stable_value
            .get("membershipStale")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            return Some(stable_value);
        }

        if value.get("resolved").and_then(|v| v.as_bool()) == Some(true) {
            let ttl_ms = qq_vip_cache_ttl_ms(&value, QQ_VIP_INFO_CACHE_TTL_MS, 30 * 1000, now2);
            if !cache_key.is_empty() && ttl_ms > 0 {
                let ent_exp = value
                    .get("expiresAt")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let stale_until = if value.get("isVip").and_then(|v| v.as_bool()) == Some(true) {
                    let cap = if ent_exp > 0 { ent_exp } else { u64::MAX };
                    now2.saturating_add(ttl_ms)
                        .saturating_add(QQ_VIP_STALE_POSITIVE_GRACE_MS)
                        .min(cap)
                } else {
                    0
                };
                if let Ok(mut g) = self.vip_info_cache.lock() {
                    g.insert(
                        cache_key,
                        CachedQqVipEntry {
                            expires_at: now2 + ttl_ms,
                            stale_until,
                            value: value.clone(),
                        },
                    );
                }
            }
            return Some(value);
        }
        None
    }

    /// 获取 QQ 音乐登录态与严格核验后的 VIP 状态。
    pub async fn login_status(&self, force_cookie: bool, force_vip: bool) -> Result<Value, String> {
        if force_cookie {
            if let Some(path) = &self.cookie_file {
                let loaded = normalize_qq_cookie_input(&read_cookie_file(path));
                if let Ok(mut g) = self.cookie.write() {
                    *g = loaded;
                }
            }
        }
        let cookie_obj = self.cookie_obj();
        let uin = qq_cookie_uin(&cookie_obj);
        let music_key = qq_cookie_music_key(&cookie_obj);
        if uin.is_empty() || music_key.is_empty() {
            return Ok(json!({
                "provider": "qq",
                "loggedIn": false,
                "hasCookie": self.has_cookie(),
            }));
        }

        let fallback = self.normalize_profile(None, &cookie_obj);
        let profile_params = [
            ("cid", "205360838".into()),
            ("userid", uin.clone()),
            ("reqfrom", "1".into()),
            ("g_tk", "5381".into()),
            ("loginUin", uin.clone()),
            ("hostUin", "0".into()),
            ("format", "json".into()),
            ("inCharset", "utf8".into()),
            ("outCharset", "utf-8".into()),
            ("notice", "0".into()),
            ("platform", "yqq.json".into()),
            ("needNewCode", "0".into()),
        ];
        let profile_fut = self.get_json(
            "https://c.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg",
            &profile_params,
            None,
            true,
            if force_vip { 6500 } else { 10000 },
        );
        let vip_fut = self.fetch_vip_status(&cookie_obj, force_vip);
        let (profile_res, vip_probe) = tokio::join!(profile_fut, vip_fut);

        match profile_res {
            Ok(body) => {
                let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                let result_code = body.get("result").and_then(|v| v.as_i64()).unwrap_or(0);
                if code == 1000 || result_code == 301 {
                    let mut fb = fallback.as_object().cloned().unwrap_or_default();
                    fb.insert("profileUnavailable".into(), json!(true));
                    return Ok(merge_qq_vip_status(
                        Value::Object(fb),
                        vip_probe.as_ref(),
                        vip_probe
                            .as_ref()
                            .and_then(|v| v.get("vipSource"))
                            .and_then(|v| v.as_str()),
                    ));
                }
                let info = self.normalize_profile(Some(&body), &cookie_obj);
                Ok(merge_qq_vip_status(
                    info,
                    vip_probe.as_ref(),
                    vip_probe
                        .as_ref()
                        .and_then(|v| v.get("vipSource"))
                        .and_then(|v| v.as_str()),
                ))
            }
            Err(_) => {
                let mut fb = fallback.as_object().cloned().unwrap_or_default();
                fb.insert("profileUnavailable".into(), json!(true));
                Ok(merge_qq_vip_status(
                    Value::Object(fb),
                    vip_probe.as_ref(),
                    vip_probe
                        .as_ref()
                        .and_then(|v| v.get("vipSource"))
                        .and_then(|v| v.as_str()),
                ))
            }
        }
    }

    // ---------- 用户歌单与曲目 ----------

    async fn fetch_created_playlists(&self, uin: &str) -> Result<Vec<Value>, String> {
        const PAGE_SIZE: usize = 200;
        let mut out = Vec::new();
        for page in 0..25 {
            let sin = page * PAGE_SIZE;
            let body = self
                .get_json(
                    "https://c.y.qq.com/rsc/fcgi-bin/fcg_user_created_diss",
                    &[
                        ("hostUin", "0".into()),
                        ("hostuin", uin.to_string()),
                        ("sin", sin.to_string()),
                        ("size", PAGE_SIZE.to_string()),
                        ("g_tk", "5381".into()),
                        ("loginUin", uin.to_string()),
                        ("format", "json".into()),
                        ("inCharset", "utf8".into()),
                        ("outCharset", "utf-8".into()),
                        ("notice", "0".into()),
                        ("platform", "yqq.json".into()),
                        ("needNewCode", "0".into()),
                    ],
                    Some("https://y.qq.com/portal/profile.html"),
                    true,
                    10000,
                )
                .await?;
            let rows = body
                .get("data")
                .and_then(|d| d.get("disslist"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let count = rows.len();
            out.extend(rows);
            if count < PAGE_SIZE {
                break;
            }
        }
        Ok(out)
    }

    async fn fetch_collected_playlists(&self, uin: &str) -> Result<Vec<Value>, String> {
        const PAGE_SIZE: usize = 200;
        let mut out = Vec::new();
        for page in 0..25 {
            let sin = page * PAGE_SIZE;
            let body = self
                .get_json(
                    "https://c.y.qq.com/fav/fcgi-bin/fcg_get_profile_order_asset.fcg",
                    &[
                        ("ct", "20".into()),
                        ("cid", "205360956".into()),
                        ("userid", uin.to_string()),
                        ("reqtype", "3".into()),
                        ("sin", sin.to_string()),
                        ("ein", (sin + PAGE_SIZE - 1).to_string()),
                    ],
                    Some("https://y.qq.com/portal/profile.html"),
                    true,
                    10000,
                )
                .await?;
            let rows = body
                .get("data")
                .and_then(|d| d.get("cdlist"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let count = rows.len();
            out.extend(rows);
            if count < PAGE_SIZE {
                break;
            }
        }
        Ok(out)
    }

    async fn fetch_liked_playlist_page(&self, limit: usize, offset: usize) -> Result<Value, String> {
        let lim = limit.clamp(1, 100);
        let body = self
            .musicu_request(
                &json!({
                    "comm": { "ct": 24, "cv": 0 },
                    "req_0": {
                        "module": "music.srfDissInfo.DissInfo",
                        "method": "CgiGetDiss",
                        "param": {
                            "disstid": 0,
                            "dirid": QQ_LIKED_DIRID,
                            "tag": 1,
                            "song_begin": offset,
                            "song_num": lim,
                            "userinfo": 1,
                            "orderlist": 1,
                        }
                    }
                }),
                true,
                10000,
            )
            .await?;

        let block = body.get("req_0").ok_or("QQ_LIKED_SYNC_FAILED")?;
        let data = block.get("data").cloned().unwrap_or_else(|| json!({}));
        let code = body
            .get("code")
            .or_else(|| block.get("code"))
            .or_else(|| data.get("code"))
            .or_else(|| data.get("subcode"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if code != 0 {
            let err_code = if matches!(code, 1000 | 10004 | 104003 | 301 | -100008) {
                "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN"
            } else {
                "QQ_LIKED_SYNC_FAILED"
            };
            return Err(err_code.to_string());
        }
        let raw_tracks = data
            .get("songlist")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let tracks: Vec<Value> = raw_tracks
            .iter()
            .map(map_qq_playlist_track)
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("mid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect();
        let page_span = data
            .get("songlist_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .max(raw_tracks.len() as u64) as usize;
        let upstream_total = data
            .get("total_song_num")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let total = if upstream_total > 0 {
            upstream_total
        } else {
            offset + page_span
        };
        let next_offset = offset + page_span;
        let has_more = data
            .get("hasmore")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0
            || next_offset < total;
        Ok(json!({
            "tracks": tracks,
            "total": total,
            "offset": offset,
            "limit": lim,
            "pageSpan": page_span,
            "nextOffset": next_offset,
            "hasMore": has_more,
        }))
    }

    fn build_liked_playlist_card(
        &self,
        info: &Value,
        liked_page: Option<&Value>,
        warning: &str,
    ) -> Value {
        let user_key = info
            .get("userId")
            .or_else(|| info.get("uin"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let tracks = liked_page
            .and_then(|p| p.get("tracks"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let page_offset = liked_page
            .and_then(|p| p.get("offset"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if liked_page.is_some() && page_offset == 0 && !user_key.is_empty() {
            if let Some(first_cover) = tracks
                .first()
                .and_then(|t| t.get("cover"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Ok(mut g) = self.liked_cover_cache.lock() {
                    g.insert(user_key.clone(), first_cover.to_string());
                }
            }
        }
        let cached_cover = if !user_key.is_empty() {
            self.liked_cover_cache
                .lock()
                .ok()
                .and_then(|g| g.get(&user_key).cloned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let cover = if !cached_cover.is_empty() {
            cached_cover
        } else if page_offset == 0 {
            tracks
                .first()
                .and_then(|t| t.get("cover"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(QQ_LIKED_PLAYLIST_COVER)
                .to_string()
        } else {
            QQ_LIKED_PLAYLIST_COVER.to_string()
        };
        let count = liked_page
            .and_then(|p| p.get("total"))
            .and_then(|v| v.as_u64())
            .unwrap_or(tracks.len() as u64);
        let creator = info
            .get("nickname")
            .or_else(|| info.get("userId"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("QQ 音乐");

        json!({
            "provider": "qq",
            "source": "qq",
            "id": QQ_LIKED_PLAYLIST_ID,
            "dirid": QQ_LIKED_DIRID.to_string(),
            "virtual": true,
            "name": QQ_LIKED_PLAYLIST_NAME,
            "cover": cover,
            "trackCount": count,
            "playCount": 0,
            "creator": creator,
            "subscribed": false,
            "specialType": 5,
            "requiresPlaybackKey": warning == "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN",
            "warning": warning,
        })
    }

    async fn get_liked_playlist_card(&self, info: &Value) -> Value {
        if info.get("playbackKeyReady").and_then(|v| v.as_bool()) != Some(true) {
            return self.build_liked_playlist_card(info, None, "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN");
        }
        match self.fetch_liked_playlist_page(1, 0).await {
            Ok(page) => self.build_liked_playlist_card(info, Some(&page), ""),
            Err(err) => self.build_liked_playlist_card(info, None, &err),
        }
    }

    /// 获取用户全部 QQ 音乐歌单（含我喜欢、自建与收藏）。
    pub async fn user_playlists(&self) -> Result<Value, String> {
        let info = self.login_status(false, false).await?;
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let uin = info
            .get("userId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !logged_in || uin.is_empty() {
            return Ok(json!({ "loggedIn": false, "provider": "qq", "playlists": [] }));
        }

        let (created_res, collect_res, liked_card) = tokio::join!(
            self.fetch_created_playlists(&uin),
            self.fetch_collected_playlists(&uin),
            self.get_liked_playlist_card(&info),
        );

        let mut base = Vec::new();
        base.push(liked_card);
        if let Ok(rows) = created_res {
            for pl in rows {
                let mapped = map_qq_playlist(&pl, "created");
                if !is_qq_favorite_playlist(&mapped) {
                    base.push(mapped);
                }
            }
        }
        if let Ok(rows) = collect_res {
            for pl in rows {
                let mapped = map_qq_playlist(&pl, "collect");
                if !is_qq_favorite_playlist(&mapped) {
                    base.push(mapped);
                }
            }
        }

        let mut seen = HashSet::new();
        let mut playlists: Vec<Value> = base
            .into_iter()
            .filter(|pl| {
                let id = pl.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = pl.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() || name.is_empty() || is_qzone_background_playlist(pl) {
                    return false;
                }
                seen.insert(id.to_string())
            })
            .collect();
        playlists.sort_by_key(|pl| std::cmp::Reverse(is_qq_favorite_playlist(pl)));

        Ok(json!({
            "loggedIn": true,
            "provider": "qq",
            "userId": uin,
            "playlists": playlists,
        }))
    }

    /// 获取 QQ 歌单曲目列表（支持虚拟 `liked` 与普通歌单 ID）。
    pub async fn playlist_tracks(
        &self,
        id: &str,
        limit: Option<usize>,
        offset: usize,
    ) -> Result<Value, String> {
        let info = self.login_status(false, false).await?;
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let uin = info
            .get("userId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !logged_in || uin.is_empty() {
            return Ok(json!({ "loggedIn": false, "provider": "qq", "tracks": [] }));
        }
        let pid = id.trim();
        if pid.is_empty() {
            return Ok(
                json!({ "loggedIn": true, "provider": "qq", "error": "Missing QQ playlist id", "tracks": [] }),
            );
        }

        let page_limit = limit.map(|l| l.min(500)).unwrap_or(0);
        if is_qq_liked_playlist_id(pid) {
            if info.get("playbackKeyReady").and_then(|v| v.as_bool()) != Some(true) {
                return Ok(json!({
                    "loggedIn": true,
                    "provider": "qq",
                    "playlist": self.build_liked_playlist_card(&info, None, "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN"),
                    "tracks": [],
                    "error": "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN",
                    "message": QQ_LIKED_AUTH_MESSAGE,
                    "requiresPlaybackKey": true,
                }));
            }
            let effective_lim = if page_limit > 0 { page_limit } else { 48 };
            return match self.fetch_liked_playlist_page(effective_lim, offset).await {
                Ok(page) => {
                    let card = self.build_liked_playlist_card(&info, Some(&page), "");
                    Ok(json!({
                        "loggedIn": true,
                        "provider": "qq",
                        "playlist": card,
                        "tracks": page["tracks"],
                        "total": page["total"],
                        "offset": offset,
                        "limit": page["limit"],
                        "nextOffset": page["nextOffset"],
                        "hasMore": page["hasMore"],
                        "partial": page_limit > 0,
                    }))
                }
                Err(code) => {
                    let req_key = code == "QQ_LIKED_REQUIRES_PLAYBACK_LOGIN";
                    Ok(json!({
                        "loggedIn": true,
                        "provider": "qq",
                        "playlist": self.build_liked_playlist_card(&info, None, &code),
                        "tracks": [],
                        "error": code,
                        "message": if req_key { QQ_LIKED_AUTH_MESSAGE } else { "QQ 音乐“我的喜欢”同步失败，请稍后刷新重试。" },
                        "requiresPlaybackKey": req_key,
                    }))
                }
            };
        }

        let mut params = vec![
            ("type", "1".into()),
            ("utf8", "1".into()),
            ("disstid", pid.to_string()),
            ("song_begin", offset.to_string()),
            ("loginUin", uin),
            ("format", "json".into()),
            ("inCharset", "utf8".into()),
            ("outCharset", "utf-8".into()),
            ("notice", "0".into()),
            ("platform", "yqq.json".into()),
            ("needNewCode", "0".into()),
        ];
        if page_limit > 0 {
            params.push(("song_num", page_limit.to_string()));
        }
        let result = self
            .get_json(
                "https://c.y.qq.com/qzone/fcg-bin/fcg_ucc_getcdinfo_byids_cp.fcg",
                &params,
                Some("https://y.qq.com/n/yqq/playlist"),
                true,
                10000,
            )
            .await?;

        let empty = json!({});
        let detail = result
            .get("cdlist")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .unwrap_or(&empty);
        let mut raw_tracks = detail
            .get("songlist")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let total_hint = detail
            .get("total_song_num")
            .or_else(|| detail.get("songnum"))
            .or_else(|| detail.get("song_cnt"))
            .or_else(|| detail.get("song_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        if page_limit > 0 && raw_tracks.len() > page_limit {
            if total_hint > 0 && raw_tracks.len() < total_hint {
                raw_tracks.truncate(page_limit);
            } else {
                raw_tracks = raw_tracks.into_iter().skip(offset).take(page_limit).collect();
            }
        }
        let tracks: Vec<Value> = raw_tracks
            .iter()
            .map(map_qq_playlist_track)
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("mid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect();
        let total = if total_hint > 0 {
            total_hint
        } else {
            tracks.len()
        };
        let next_offset = offset + tracks.len();
        let has_more = if page_limit > 0 {
            next_offset < total || tracks.len() >= page_limit
        } else {
            false
        };
        Ok(json!({
            "loggedIn": true,
            "provider": "qq",
            "playlist": {
                "provider": "qq",
                "id": pid,
                "name": detail.get("dissname").or_else(|| detail.get("diss_name")).or_else(|| detail.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                "cover": detail.get("logo").or_else(|| detail.get("diss_cover")).and_then(|v| v.as_str()).unwrap_or(""),
                "trackCount": total,
            },
            "tracks": tracks,
            "offset": offset,
            "limit": if page_limit > 0 { page_limit } else { tracks.len() },
            "nextOffset": next_offset,
            "hasMore": has_more,
            "partial": page_limit > 0,
            "total": total,
        }))
    }

    // ---------- 专辑、歌手与评论 ----------

    pub async fn album_detail(&self, mid: &str, limit: usize) -> Result<Value, String> {
        let album_mid = mid.trim();
        let num = limit.clamp(10, 120);
        if album_mid.is_empty() {
            return Ok(
                json!({ "provider": "qq", "error": "MISSING_ALBUM_MID", "album": Value::Null, "songs": [] }),
            );
        }
        let referer = format!("https://y.qq.com/n/ryqq/albumDetail/{album_mid}");
        let body = self
            .get_json(
                "https://c.y.qq.com/v8/fcg-bin/fcg_v8_album_info_cp.fcg",
                &[
                    ("albummid", album_mid.to_string()),
                    ("g_tk", "5381".into()),
                    ("loginUin", "0".into()),
                    ("hostUin", "0".into()),
                    ("format", "json".into()),
                    ("inCharset", "utf8".into()),
                    ("outCharset", "utf-8".into()),
                    ("notice", "0".into()),
                    ("platform", "yqq.json".into()),
                    ("needNewCode", "0".into()),
                ],
                Some(&referer),
                true,
                8000,
            )
            .await?;
        let empty = json!({});
        let data = body.get("data").unwrap_or(&empty);
        let raw_songs = data
            .get("list")
            .or_else(|| data.get("songlist"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let album_name_fallback = data
            .get("name")
            .or_else(|| data.get("title"))
            .or_else(|| data.get("albumname"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let songs: Vec<Value> = raw_songs
            .iter()
            .take(num)
            .map(|raw| {
                let mut cloned = raw.as_object().cloned().unwrap_or_default();
                cloned
                    .entry("albummid".to_string())
                    .or_insert_with(|| json!(album_mid));
                cloned
                    .entry("albumname".to_string())
                    .or_insert_with(|| json!(album_name_fallback));
                let mut song = map_qq_playlist_track(&Value::Object(cloned));
                if let Some(obj) = song.as_object_mut() {
                    if obj
                        .get("cover")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty()
                    {
                        obj.insert("cover".into(), json!(qq_album_cover(album_mid, 300)));
                    }
                    if obj
                        .get("albumMid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty()
                    {
                        obj.insert("albumMid".into(), json!(album_mid));
                    }
                }
                song
            })
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("mid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect();

        let singer_name = data
            .get("singername")
            .or_else(|| data.get("singerName"))
            .or_else(|| data.get("singer_name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                songs
                    .first()
                    .and_then(|s| s.get("artist"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        let total = data
            .get("total_song_num")
            .or_else(|| data.get("total"))
            .or_else(|| data.get("songnum"))
            .and_then(|v| v.as_u64())
            .unwrap_or(raw_songs.len() as u64)
            .max(songs.len() as u64);

        Ok(json!({
            "provider": "qq",
            "album": {
                "provider": "qq",
                "mid": album_mid,
                "albumMid": album_mid,
                "id": data.get("id").or_else(|| data.get("albumid")).cloned().unwrap_or_else(|| json!("")),
                "name": album_name_fallback,
                "artist": singer_name,
                "cover": qq_album_cover(album_mid, 300),
                "releaseDate": data.get("aDate").or_else(|| data.get("publictime")).or_else(|| data.get("pub_time")).and_then(|v| v.as_str()).unwrap_or(""),
                "trackCount": total,
            },
            "songs": songs,
            "total": total,
        }))
    }

    pub async fn artist_detail(&self, mid: &str, limit: usize) -> Result<Value, String> {
        let singer_mid = mid.trim();
        let num = limit.clamp(10, 80);
        if singer_mid.is_empty() {
            return Ok(
                json!({ "provider": "qq", "error": "MISSING_SINGER_MID", "artist": Value::Null, "songs": [] }),
            );
        }
        let json_resp = self
            .musicu_request(
                &json!({
                    "comm": { "ct": 24, "cv": 0 },
                    "singer": {
                        "module": "music.web_singer_info_svr",
                        "method": "get_singer_detail_info",
                        "param": { "sort": 5, "singermid": singer_mid, "sin": 0, "num": num }
                    }
                }),
                true,
                8000,
            )
            .await?;

        let block = json_resp.get("singer");
        let code = block
            .and_then(|b| b.get("code"))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if code != 0 {
            let err_msg = block
                .and_then(|b| b.get("message").or_else(|| b.get("msg")))
                .and_then(|v| v.as_str())
                .unwrap_or("QQ_ARTIST_DETAIL_FAILED");
            return Ok(
                json!({ "provider": "qq", "error": err_msg, "artist": Value::Null, "songs": [] }),
            );
        }
        let data = block.and_then(|b| b.get("data")).cloned().unwrap_or_else(|| json!({}));
        let info = data
            .get("singer_info")
            .or_else(|| data.get("singerInfo"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let raw_songs = data
            .get("songlist")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let songs: Vec<Value> = raw_songs
            .iter()
            .map(|raw| {
                let item = raw
                    .get("track_info")
                    .or_else(|| raw.get("songInfo"))
                    .or_else(|| raw.get("songinfo"))
                    .or_else(|| raw.get("song"))
                    .unwrap_or(raw);
                map_qq_track(item, None)
            })
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("mid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect();

        let artist_mid = info
            .get("mid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(singer_mid);
        let artist_name = info
            .get("name")
            .or_else(|| info.get("title"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                songs
                    .first()
                    .and_then(|s| s.get("artists"))
                    .and_then(|v| v.as_array())
                    .and_then(|arr| {
                        arr.iter()
                            .find(|a| a.get("mid").and_then(|v| v.as_str()) == Some(singer_mid))
                    })
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        let total_song = data
            .get("total_song")
            .or_else(|| data.get("song_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(songs.len() as u64);

        Ok(json!({
            "provider": "qq",
            "artist": {
                "provider": "qq",
                "id": info.get("id").cloned().unwrap_or_else(|| json!("")),
                "mid": artist_mid,
                "name": artist_name,
                "avatar": info.get("pic").or_else(|| info.get("avatar")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| qq_singer_avatar(artist_mid, 300)),
                "fans": info.get("fans").and_then(|v| v.as_u64()).unwrap_or(0),
                "musicSize": total_song,
                "albumSize": data.get("total_album").and_then(|v| v.as_u64()).unwrap_or(0),
                "mvSize": data.get("total_mv").and_then(|v| v.as_u64()).unwrap_or(0),
            },
            "total": total_song,
            "songs": songs,
        }))
    }

    pub async fn song_comments(
        &self,
        id: Option<&str>,
        mid: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let mut topid: String = id
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if topid.is_empty() {
            if let Some(m) = mid.filter(|s| !s.trim().is_empty()) {
                let detail = self.song_detail(m.trim(), json!({ "mid": m })).await;
                let fallback_id = detail
                    .get("qqId")
                    .or_else(|| detail.get("id"))
                    .map(|v| match v {
                        Value::Number(n) => n.to_string(),
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                topid = fallback_id
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect();
            }
        }
        if topid.is_empty() {
            return Ok(
                json!({ "provider": "qq", "error": "Missing QQ song id", "comments": [] }),
            );
        }
        let lim = limit.max(1);
        let page = offset / lim;
        let uin = qq_cookie_uin(&self.cookie_obj());
        let referer = format!(
            "https://y.qq.com/n/ryqq/songDetail/{}",
            mid.filter(|s| !s.is_empty()).unwrap_or(&topid)
        );
        let body = self
            .get_json(
                "https://c.y.qq.com/base/fcgi-bin/fcg_global_comment_h5.fcg",
                &[
                    ("g_tk", "5381".into()),
                    (
                        "loginUin",
                        if uin.is_empty() { "0".into() } else { uin },
                    ),
                    ("hostUin", "0".into()),
                    ("format", "json".into()),
                    ("inCharset", "utf8".into()),
                    ("outCharset", "utf-8".into()),
                    ("notice", "0".into()),
                    ("platform", "yqq.json".into()),
                    ("needNewCode", "0".into()),
                    ("cid", "205360772".into()),
                    ("reqtype", "2".into()),
                    ("biztype", "1".into()),
                    ("topid", topid.clone()),
                    ("cmd", "8".into()),
                    ("needmusiccrit", "0".into()),
                    ("pagenum", page.to_string()),
                    ("pagesize", lim.to_string()),
                ],
                Some(&referer),
                true,
                8000,
            )
            .await?;

        let hot_list = body
            .get("hot_comment")
            .and_then(|h| h.get("commentlist"))
            .and_then(|v| v.as_array());
        let normal_list = body
            .get("comment")
            .and_then(|c| c.get("commentlist"))
            .and_then(|v| v.as_array());
        let use_hot = offset == 0 && hot_list.is_some_and(|l| !l.is_empty());
        let raw = if use_hot {
            hot_list.cloned().unwrap_or_default()
        } else {
            normal_list.cloned().unwrap_or_default()
        };

        let comments: Vec<Value> = raw
            .iter()
            .filter_map(|c| {
                let content = c
                    .get("rootcommentcontent")
                    .or_else(|| c.get("content"))
                    .or_else(|| c.get("comment"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if content.is_empty() {
                    return None;
                }
                let time_raw = c
                    .get("time")
                    .or_else(|| c.get("commenttime"))
                    .or_else(|| c.get("createTime"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let time_ms = if time_raw > 0 && time_raw < 10_000_000_000 {
                    time_raw * 1000
                } else {
                    time_raw
                };
                let user = c.get("user").or_else(|| c.get("uin"));
                let nick = c
                    .get("nick")
                    .or_else(|| c.get("nickname"))
                    .or_else(|| c.get("encrypt_uin"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        user.and_then(|u| {
                            u.get("nick")
                                .or_else(|| u.get("nickname"))
                                .or_else(|| u.get("name"))
                                .and_then(|v| v.as_str())
                        })
                    })
                    .unwrap_or("QQ 音乐用户");
                let avatar = c
                    .get("avatarurl")
                    .or_else(|| c.get("avatar"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        user.and_then(|u| {
                            u.get("avatarurl")
                                .or_else(|| u.get("avatar"))
                                .and_then(|v| v.as_str())
                        })
                    })
                    .unwrap_or("");
                Some(json!({
                    "id": c.get("commentid").or_else(|| c.get("commentId")).or_else(|| c.get("id")).cloned().unwrap_or_else(|| json!("")),
                    "content": content,
                    "likedCount": c.get("praisenum").or_else(|| c.get("praise_num")).or_else(|| c.get("likedCount")).and_then(|v| v.as_u64()).unwrap_or(0),
                    "time": time_ms,
                    "user": {
                        "id": c.get("encrypt_uin").or_else(|| c.get("uin")).or_else(|| user.and_then(|u| u.get("uin"))).cloned().unwrap_or_else(|| json!("")),
                        "nickname": nick,
                        "avatar": avatar,
                    }
                }))
            })
            .collect();

        let total = body
            .get("comment")
            .and_then(|c| c.get("commenttotal").or_else(|| c.get("comment_total")))
            .and_then(|v| v.as_u64())
            .unwrap_or(comments.len() as u64);

        Ok(json!({
            "provider": "qq",
            "id": topid,
            "total": total,
            "comments": comments,
            "hot": use_hot,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_standard_vectors_and_qq_search_sign() {
        assert_eq!(
            hex::encode(sha1_digest(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex::encode(sha1_digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let sign = qq_search_sign(r#"{"comm":{"ct":"11"},"req":{}}"#);
        assert!(sign.starts_with("zzc"));
        assert!(sign.len() > 20);
        assert!(sign.chars().all(|c| !c.is_uppercase()));
    }

    #[test]
    fn qq_vip_normalization_scopes_to_expected_uin_and_checks_expiry() {
        let now = 1_750_000_000_000u64;
        let future_sec = (now / 1000) + 86400;
        let past_sec = (now / 1000) - 86400;

        let payload = json!({
            "req_1": {
                "code": 0,
                "data": {
                    "list": [
                        { "uin": "999999", "iVipFlag": 1, "iNewSuperVip": 1, "svipEndTime": future_sec },
                        { "uin": "123456", "iVipFlag": 1, "vipType": 2, "vipEndTime": future_sec }
                    ]
                }
            }
        });
        let scopes = qq_vip_payload_scopes_for_uin(&payload, "00123456");
        assert_eq!(scopes.len(), 1);
        let vip = normalize_qq_vip_payload(&scopes[0], "123456", now);
        assert_eq!(vip["isVip"], true);
        assert_eq!(vip["isSvip"], false);
        assert_eq!(vip["vipLevel"], "vip");

        // Expired VIP must downgrade to negative
        let expired_payload = json!({
            "uin": "123456",
            "iVipFlag": 1,
            "vipEndTime": past_sec
        });
        let expired = normalize_qq_vip_payload(&expired_payload, "123456", now);
        assert_eq!(expired["isVip"], false);
        assert_eq!(expired["decision"], "negative");
        assert_eq!(expired["membershipKnown"], true);
    }

    #[tokio::test]
    async fn qq_vip_probes_enforce_negative_quorum_and_stale_positive_grace() {
        let now = 1_750_000_000_000u64;
        let probes = vec![
            QqVipProbe {
                source: "p1".into(),
                response_key: "req_1".into(),
                uin: "10086".into(),
                body: json!({}),
            },
            QqVipProbe {
                source: "p2".into(),
                response_key: "req_1".into(),
                uin: "10086".into(),
                body: json!({}),
            },
        ];

        // Both probes confirm non-VIP -> authoritativeNegative = true
        let neg_result = resolve_qq_vip_from_probes(
            &probes,
            |_p: QqVipProbe| async move {
                Ok(json!({
                    "code": 0,
                    "req_1": { "code": 0, "data": { "uin": "10086", "isVip": 0 } }
                }))
            },
            now,
        )
        .await;
        assert_eq!(neg_result["authoritativeNegative"], true);
        assert_eq!(neg_result["isVip"], false);

        // Only one probe completes -> probeIncomplete = true, stale-positive grace preserves VIP
        let incomplete_result = resolve_qq_vip_from_probes(
            &probes,
            |p: QqVipProbe| {
                let src = p.source.clone();
                async move {
                    if src == "p1" {
                        Ok(json!({ "code": 0, "req_1": { "code": 0, "data": { "uin": "10086", "isVip": 0 } } }))
                    } else {
                        Err("timeout".into())
                    }
                }
            },
            now,
        )
        .await;
        assert_eq!(incomplete_result["probeIncomplete"], true);

        let cached_entry = CachedQqVipEntry {
            expires_at: now - 1000,
            stale_until: now + 60_000,
            value: json!({
                "vipType": 1,
                "svipType": 1,
                "vipLevel": "svip",
                "isVip": true,
                "isSvip": true,
                "vipLabel": "SVIP",
                "membershipKnown": true,
                "resolved": true,
                "decision": "positive",
                "expiresAt": now + 86_400_000
            }),
        };
        let preserved = preserve_qq_vip_stale_positive(
            Some(&cached_entry),
            incomplete_result,
            now,
            QQ_VIP_STALE_POSITIVE_GRACE_MS,
        );
        assert_eq!(preserved["isSvip"], true);
        assert_eq!(preserved["vipSyncState"], "stale_positive");

        let merged = merge_qq_vip_status(
            json!({ "loggedIn": true, "playbackKeyReady": true }),
            Some(&preserved),
            None,
        );
        assert_eq!(merged["vipSyncState"], "stale_positive");
        assert_eq!(merged["membershipRights"]["verified"], false);
        assert_eq!(merged["membershipRights"]["canPlayVipTracks"], false);
    }

    #[test]
    fn qq_cookie_decoding_and_track_mapping_work() {
        assert_eq!(normalize_qq_uin("o00123456"), "123456");
        let hex_utf8 = hex::encode("杰伦".as_bytes());
        assert_eq!(decode_qq_cookie_value(&hex_utf8), "杰伦");
        assert_eq!(decode_qq_lyric_text(&BASE64_STD.encode("[00:01.00]晴天")), "[00:01.00]晴天");

        let mapped = map_qq_track(
            &json!({
                "id": 97773,
                "mid": "0039MnYb0qxYhV",
                "name": "晴天",
                "interval": 269,
                "singer": [{ "id": 4558, "mid": "0025NhlN2yWrP4", "name": "周杰伦" }],
                "album": { "mid": "000MkMni19ClKG", "name": "叶惠美" },
                "file": { "media_mid": "0039MnYb0qxYhV" },
                "pay": { "pay_play": 1 }
            }),
            None,
        );
        assert_eq!(mapped["provider"], "qq");
        assert_eq!(mapped["mid"], "0039MnYb0qxYhV");
        assert_eq!(mapped["duration"], 269000);
        assert_eq!(mapped["fee"], 1);
    }
}
