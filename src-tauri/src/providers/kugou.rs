//! 酷狗音乐纯 Rust 客户端。
//!
//! 精确移植自 `kugou-api.js`，涵盖 Android / H5 MD5 签名、Gateway 请求、
//! `KuGoo=` 复合 Cookie 解析、Web RoleInfo 与多路 VIP 权益验证、
//! 5 路音频 URL 探测、KRC/LRC 歌词下载、歌单同步与喜欢操作。

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use md5::{Digest, Md5};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use super::{
    normalize_quality_preference, parse_cookie_string, read_cookie_file, shared_http_client,
    unix_millis, unix_secs, write_cookie_file, TtlCache, DEFAULT_USER_AGENT,
};

const KUGOU_SEARCH_URL: &str = "https://songsearch.kugou.com/song_search_v2";
const KUGOU_PLAY_MOBILE: &str = "https://m.kugou.com/app/i/getSongInfo.php";
const KUGOU_PLAY_WEB: &str = "https://wwwapi.kugou.com/play/songinfo";
const KUGOU_PLAY_WEB_RETRY: &str = "https://wwwapiretry.kugou.com/play/songinfo";
const KUGOU_LYRIC_SEARCH: &str = "https://krcs.kugou.com/search";
const KUGOU_LYRIC_DOWNLOAD: &str = "https://krcs.kugou.com/download";
const KUGOU_GATEWAY: &str = "https://gateway.kugou.com";
const KUGOU_VIP_ROLEINFO_URL: &str = "https://vip.kugou.com/recharge/roleinfo";
const KUGOU_REFERER: &str = "https://www.kugou.com/";

const KUGOU_APPID: u32 = 1005;
const KUGOU_WEB_APPID: u32 = 1014;
const KUGOU_CLIENTVER: u32 = 20489;
pub const KUGOU_ANDROID_SALT: &str = "OIlwieks28dk2k092lksi2UIkp";
pub const KUGOU_H5_SALT: &str = "NVPh5oo715z5DIWAeQlhMDsWXXQV4hwt";
const KUGOU_H5_SRC_APPID: &str = "2919";
const KUGOU_H5_CLIENTVER: &str = "20000";
pub const KUGOU_SIGN_KEY_SALT: &str = "57ae12eb6890223e355ccfcb74edf70d";
const KUGOU_GATEWAY_UA: &str = "Android15-1070-11083-46-0-DiscoveryDRADProtocol-wifi";

const KUGOU_PLAY_ATTEMPT_MS: u64 = 2500;
const KUGOU_PLAY_BUDGET_MS: u64 = 9000;
const KUGOU_VIP_STALE_POSITIVE_GRACE_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy)]
struct KugouQualityItem {
    key: &'static str,
    label: &'static str,
    field: &'static str,
}

const KUGOU_QUALITY_CHAIN: &[KugouQualityItem] = &[
    KugouQualityItem {
        key: "jymaster",
        label: "Hi-Res",
        field: "ResFileHash",
    },
    KugouQualityItem {
        key: "hires",
        label: "Hi-Res",
        field: "ResFileHash",
    },
    KugouQualityItem {
        key: "lossless",
        label: "无损",
        field: "SQFileHash",
    },
    KugouQualityItem {
        key: "exhigh",
        label: "极高",
        field: "HQFileHash",
    },
    KugouQualityItem {
        key: "standard",
        label: "标准",
        field: "FileHash",
    },
];

fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Android Gateway 参数 MD5 签名。
pub fn signature_android_params(params: &BTreeMap<String, String>, data: &str) -> String {
    let params_string: String = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    md5_hex(&format!(
        "{KUGOU_ANDROID_SALT}{params_string}{data}{KUGOU_ANDROID_SALT}"
    ))
}

/// H5 网关参数 MD5 签名。
pub fn signature_h5_params(params: &BTreeMap<String, String>, body_json: Option<&str>) -> String {
    let mut parts: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    if let Some(body) = body_json.filter(|s| !s.is_empty()) {
        parts.push(body.to_string());
    }
    md5_hex(&format!("{KUGOU_H5_SALT}{}{KUGOU_H5_SALT}", parts.join("")))
}

/// 音轨 `/v5/url` 的 `key` 参数签名。
pub fn sign_key(hash: &str, mid: &str, userid: &str, appid: u32) -> String {
    let app = if appid == 0 { KUGOU_APPID } else { appid };
    let uid = if userid.is_empty() { "0" } else { userid };
    md5_hex(&format!("{hash}{KUGOU_SIGN_KEY_SALT}{app}{mid}{uid}"))
}

/// 移动端 `getSongInfo.php` 的 `kgcloud` key。
pub fn kugou_cloud_key(hash: &str) -> String {
    md5_hex(&format!("{hash}kgcloud"))
}

/// FM 推荐接口 `key` 参数签名。
pub fn kugou_sign_params_key(clienttime: u64) -> String {
    md5_hex(&format!(
        "{KUGOU_APPID}{KUGOU_CLIENTVER}{clienttime}{KUGOU_ANDROID_SALT}"
    ))
}

// ---------- 文本清洗与 Cookie/Auth 提取 ----------

fn decode_kugou_display_text(text: &str) -> String {
    static PCT_U_RE: OnceLock<Regex> = OnceLock::new();
    let mut raw = text.trim().to_string();
    if raw.is_empty() {
        return String::new();
    }
    let re = PCT_U_RE.get_or_init(|| Regex::new(r"(?i)%u([0-9a-f]{4})").unwrap());
    raw = re
        .replace_all(&raw, |caps: &regex::Captures| {
            u32::from_str_radix(&caps[1], 16)
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .to_string();

    let has_cjk = raw.chars().any(|c| ('\u{3400}'..='\u{9FFF}').contains(&c));
    if !has_cjk && raw.contains('%') {
        let plus_space = raw.replace('+', " ");
        if let Ok(decoded) = percent_encoding::percent_decode_str(&plus_space).decode_utf8() {
            raw = decoded.to_string();
        }
    }
    raw.trim().to_string()
}

fn strip_kugou_html(text: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let clean = re.replace_all(text, "");
    decode_kugou_display_text(clean.trim())
}

fn strip_kugou_filename(raw: &str, fallback_artist: &str) -> String {
    static EXT_RE: OnceLock<Regex> = OnceLock::new();
    let re = EXT_RE.get_or_init(|| Regex::new(r"(?i)\.(mp3|flac|m4a|wav|ape|ogg)$").unwrap());
    let clean = strip_kugou_html(raw);
    let mut name = re.replace(&clean, "").trim().to_string();
    let artist = strip_kugou_html(fallback_artist);
    if !artist.is_empty() && name.starts_with(&artist) {
        name = name[artist.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—'))
            .trim()
            .to_string();
    }
    if name.is_empty() {
        clean
    } else {
        name
    }
}

fn kugou_cover_url(raw: &str, size: u32) -> String {
    let url = raw.trim();
    if url.is_empty() {
        return String::new();
    }
    let px = if size == 0 { 240 } else { size };
    url.replace("{size}", &px.to_string())
}

fn parse_kugou_compound(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let text = percent_encoding::percent_decode_str(raw.trim())
        .decode_utf8()
        .map(|c| c.to_string())
        .unwrap_or_else(|_| raw.trim().to_string());
    for part in text.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim();
            if !key.is_empty() {
                out.insert(key.to_string(), v.trim().to_string());
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KugouAuth {
    pub userid: String,
    pub token: String,
    pub mid: String,
    pub dfid: String,
    pub nickname: String,
    pub avatar: String,
    pub vip_type: u64,
    pub svip_type: u64,
    pub vip_level: String,
    pub is_vip: bool,
    pub is_svip: bool,
    pub membership_known: bool,
    pub logged_in: bool,
    pub playback_ready: bool,
}

fn map_first_positive(maps: &[&BTreeMap<String, String>], keys: &[&str]) -> u64 {
    for map in maps {
        for key in keys {
            if let Some(raw) = map.get(*key) {
                let trimmed = raw.trim();
                if let Ok(n) = trimmed.parse::<f64>() {
                    if n.is_finite() && n > 0.0 {
                        return n as u64;
                    }
                }
                if matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "true" | "yes" | "active" | "valid" | "enabled" | "vip" | "svip" | "premium" | "member"
                ) {
                    return 1;
                }
            }
        }
    }
    0
}

fn normalize_kugou_membership_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_kugou_vip_signal_key(norm: &str) -> bool {
    matches!(
        norm,
        "vip"
            | "viptype"
            | "isvip"
            | "viplevel"
            | "vipstatus"
            | "membertype"
            | "memberlevel"
            | "musicviplevel"
            | "mtype"
            | "ptype"
            | "vipytype"
            | "unionviptype"
            | "userviptype"
    )
}

fn is_kugou_svip_signal_key(norm: &str) -> bool {
    matches!(
        norm,
        "svip"
            | "sviptype"
            | "issvip"
            | "sviplevel"
            | "svipstatus"
            | "supervip"
            | "superviplevel"
            | "superviptype"
            | "luxuryviptype"
            | "vipluxurytype"
    )
}

fn is_kugou_vip_expiry_signal_key(norm: &str) -> bool {
    matches!(
        norm,
        "vipendtime"
            | "vipexpiretime"
            | "vipexpire"
            | "musicvipendtime"
            | "rawvipendtime"
            | "musicendtime"
            | "rawmusicendtime"
            | "svipendtime"
            | "svipexpiretime"
            | "supervipendtime"
            | "luxuryvipendtime"
    )
}

fn is_kugou_web_membership_signal_key(norm: &str) -> bool {
    matches!(
        norm,
        "role" | "producttype" | "usertype" | "userytype" | "ytype"
    )
}

fn map_has_membership_signal(map: &BTreeMap<String, String>) -> bool {
    map.keys().any(|k| {
        let norm = normalize_kugou_membership_key(k);
        is_kugou_vip_signal_key(&norm)
            || is_kugou_svip_signal_key(&norm)
            || is_kugou_vip_expiry_signal_key(&norm)
    })
}

/// 从酷狗 Cookie 提取身份、Token、设备 mid/dfid 及静态 VIP 提示。
pub fn extract_kugou_auth(cookie: &str) -> KugouAuth {
    let obj = parse_cookie_string(cookie.trim());
    let kugoo_raw = obj
        .get("KuGoo")
        .or_else(|| obj.get("kugou"))
        .or_else(|| obj.get("Kugou"))
        .map(String::as_str)
        .unwrap_or("");
    let kugoo = parse_kugou_compound(kugoo_raw);

    let userid_raw = obj
        .get("userid")
        .or_else(|| obj.get("UserId"))
        .or_else(|| obj.get("KugooID"))
        .or_else(|| obj.get("kugouID"))
        .or_else(|| kugoo.get("KugooID"))
        .or_else(|| kugoo.get("kugouID"))
        .or_else(|| kugoo.get("userid"))
        .or_else(|| kugoo.get("uid"))
        .map(String::as_str)
        .unwrap_or("");
    let userid: String = userid_raw.chars().filter(|c| c.is_ascii_digit()).collect();

    let token = obj
        .get("token")
        .or_else(|| obj.get("Token"))
        .or_else(|| obj.get("t"))
        .or_else(|| obj.get("T"))
        .or_else(|| kugoo.get("t"))
        .or_else(|| kugoo.get("token"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let seed = if !userid.is_empty() {
        userid.as_str()
    } else if !token.is_empty() {
        token.as_str()
    } else if !kugoo_raw.is_empty() {
        kugoo_raw
    } else {
        "guest"
    };
    let fallback_mid = md5_hex(&format!("mineradio-kugou:{seed}"));
    let mid = obj
        .get("kg_mid")
        .or_else(|| obj.get("KG_MID"))
        .or_else(|| obj.get("KUGOU_API_MID"))
        .or_else(|| obj.get("mid"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_mid);
    let dfid = obj
        .get("kg_dfid")
        .or_else(|| obj.get("KG_DFID"))
        .or_else(|| obj.get("dfid"))
        .or_else(|| obj.get("DFID"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-".to_string());

    let nickname_raw = kugoo
        .get("NickName")
        .or_else(|| kugoo.get("nickname"))
        .or_else(|| obj.get("NickName"))
        .or_else(|| obj.get("nickname"))
        .or_else(|| obj.get("UserName"))
        .or_else(|| obj.get("username"))
        .map(String::as_str)
        .unwrap_or("");
    let nickname = decode_kugou_display_text(nickname_raw);

    let avatar = kugoo
        .get("Pic")
        .or_else(|| kugoo.get("pic"))
        .or_else(|| obj.get("Pic"))
        .or_else(|| obj.get("avatar"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let vip_type = map_first_positive(
        &[&kugoo, &obj],
        &[
            "isVIP",
            "isVip",
            "is_vip",
            "vip_type",
            "VIPType",
            "vipLevel",
            "vip_level",
            "vip_status",
            "member_type",
            "member_level",
            "vip",
        ],
    );
    let svip_type = map_first_positive(
        &[&kugoo, &obj],
        &[
            "isSVIP",
            "isSvip",
            "is_svip",
            "svip_type",
            "SVIPType",
            "superVip",
            "super_vip",
            "svip_level",
            "svip_status",
            "svip",
        ],
    );
    let membership_known = map_has_membership_signal(&obj) || map_has_membership_signal(&kugoo);
    let is_svip = svip_type > 0;
    let is_vip = is_svip || vip_type > 0;
    let vip_level = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else {
        "none"
    };
    let logged_in = (!userid.is_empty() && userid != "0") || !kugoo_raw.is_empty();
    let playback_ready = !userid.is_empty() && userid != "0" && !token.is_empty();

    KugouAuth {
        userid,
        token,
        mid,
        dfid,
        nickname,
        avatar,
        vip_type,
        svip_type,
        vip_level: vip_level.to_string(),
        is_vip,
        is_svip,
        membership_known,
        logged_in,
        playback_ready,
    }
}

/// 校验酷狗 Cookie 是否具备完整播放授权（userid + token）。
pub fn kugou_cookie_has_playback(cookie: &str) -> bool {
    extract_kugou_auth(cookie).playback_ready
}

pub fn build_kugou_request_cookie(cookie: &str) -> String {
    let auth = extract_kugou_auth(cookie);
    let mut map = parse_cookie_string(cookie);
    if !map.contains_key("kg_mid") && !map.contains_key("KG_MID") {
        map.insert("kg_mid".into(), auth.mid);
    }
    if !map.contains_key("kg_dfid") && !map.contains_key("KG_DFID") {
        map.insert("kg_dfid".into(), auth.dfid);
    }
    map.into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

// ---------- VIP 解析与权益校验 ----------

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn first_positive_kugou_number(objects: &[&Value], keys: &[&str]) -> u64 {
    for obj in objects {
        let Some(map) = obj.as_object() else { continue };
        for key in keys {
            let Some(raw) = map.get(*key) else { continue };
            if let Some(n) = value_as_f64(raw) {
                if n > 0.0 {
                    return n as u64;
                }
            }
            if raw == &Value::Bool(true) {
                return 1;
            }
            if let Some(s) = raw.as_str() {
                if matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "active" | "valid" | "enabled" | "vip" | "svip" | "premium" | "member"
                ) {
                    return 1;
                }
            }
        }
    }
    0
}

fn kugou_object_has_membership_signal(value: &Value, allow_web_signals: bool) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.iter().any(|(k, item)| {
        if item.is_object() || item.is_array() {
            return false;
        }
        let norm = normalize_kugou_membership_key(k);
        is_kugou_vip_signal_key(&norm)
            || is_kugou_svip_signal_key(&norm)
            || is_kugou_vip_expiry_signal_key(&norm)
            || (allow_web_signals && is_kugou_web_membership_signal_key(&norm))
    })
}

fn kugou_membership_time(value: &Value) -> u64 {
    if let Some(n) = value_as_f64(value) {
        if n > 0.0 {
            return n as u64;
        }
    }
    if let Some(s) = value.as_str() {
        let trimmed = s.trim();
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
            let ms = dt.and_utc().timestamp_millis();
            if ms > 0 {
                return ms as u64;
            }
        }
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S") {
            let ms = dt.and_utc().timestamp_millis();
            if ms > 0 {
                return ms as u64;
            }
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                let ms = dt.and_utc().timestamp_millis();
                if ms > 0 {
                    return ms as u64;
                }
            }
        }
    }
    0
}

struct KugouTimeState {
    present: bool,
    valid: bool,
    future: bool,
    expires_at: u64,
}

fn kugou_time_state(obj: &Value, keys: &[&str], now_ms: u64) -> KugouTimeState {
    let now_sec = now_ms / 1000;
    let mut present = false;
    let mut valid = false;
    let mut future = false;
    let mut expires_at = 0u64;
    let Some(map) = obj.as_object() else {
        return KugouTimeState {
            present,
            valid,
            future,
            expires_at,
        };
    };
    for key in keys {
        let Some(raw) = map.get(*key) else { continue };
        if raw.is_null() {
            continue;
        }
        if let Some(s) = raw.as_str() {
            if s.trim().is_empty() || s.trim() == "0" {
                continue;
            }
        } else if value_as_f64(raw) == Some(0.0) {
            continue;
        }
        present = true;
        let val = kugou_membership_time(raw);
        if val == 0 {
            continue;
        }
        if val > 100_000_000_000 {
            valid = true;
            if val > now_ms {
                future = true;
                expires_at = expires_at.max(val);
            }
        } else if val > 1_000_000_000 {
            valid = true;
            if val > now_sec {
                future = true;
                expires_at = expires_at.max(val * 1000);
            }
        }
    }
    KugouTimeState {
        present,
        valid,
        future,
        expires_at,
    }
}

fn kugou_number_field_state(obj: &Value, keys: &[&str]) -> (bool, bool, u64) {
    let Some(map) = obj.as_object() else {
        return (false, false, 0);
    };
    for key in keys {
        let Some(raw) = map.get(*key) else { continue };
        if raw.is_null() {
            continue;
        }
        if let Some(s) = raw.as_str() {
            if s.trim().is_empty() {
                continue;
            }
        }
        if let Some(n) = value_as_f64(raw) {
            if n >= 0.0 {
                return (true, true, n as u64);
            }
        }
        return (true, false, 0);
    }
    (false, false, 0)
}

fn kugou_object_user_id(value: &Value) -> String {
    let Some(map) = value.as_object() else {
        return String::new();
    };
    for key in ["userid", "user_id", "userId", "uid", "KugooID"] {
        if let Some(v) = map.get(key) {
            let s = match v {
                Value::Number(n) => n.to_string(),
                Value::String(st) => st.clone(),
                _ => String::new(),
            };
            let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return digits;
            }
        }
    }
    String::new()
}

fn collect_kugou_vip_objects(
    value: &Value,
    out: &mut Vec<Value>,
    depth: usize,
    expected_user_id: &str,
    inherited_user_id: &str,
) {
    if depth > 6 || value.is_null() {
        return;
    }
    if let Some(arr) = value.as_array() {
        for item in arr {
            collect_kugou_vip_objects(item, out, depth + 1, expected_user_id, inherited_user_id);
        }
        return;
    }
    let Some(map) = value.as_object() else { return };
    let obj_uid = kugou_object_user_id(value);
    let scoped_uid = if !obj_uid.is_empty() {
        obj_uid.as_str()
    } else {
        inherited_user_id
    };
    if !expected_user_id.is_empty() && !scoped_uid.is_empty() && scoped_uid != expected_user_id {
        return;
    }
    out.push(value.clone());
    for (k, child) in map {
        if child.is_object() || child.is_array() {
            let map_uid = if k.len() >= 4 && k.bytes().all(|b| b.is_ascii_digit()) {
                k.as_str()
            } else {
                ""
            };
            if !expected_user_id.is_empty() && !map_uid.is_empty() && map_uid != expected_user_id {
                continue;
            }
            let next_inherited = if !map_uid.is_empty() {
                map_uid
            } else {
                scoped_uid
            };
            collect_kugou_vip_objects(child, out, depth + 1, expected_user_id, next_inherited);
        }
    }
}

const KUGOU_VIP_TYPE_KEYS: &[&str] = &[
    "vipType", "vip_type", "VIPType", "isVIP", "isVip", "is_vip", "vip_level", "vipLevel",
    "music_vip_level", "musicVipLevel", "m_type", "p_type", "vip_y_type", "union_vip_type",
    "user_vip_type", "vip_status", "member_type", "member_level", "vip",
];
const KUGOU_SVIP_TYPE_KEYS: &[&str] = &[
    "svipType", "svip_type", "SVIPType", "isSVIP", "isSvip", "is_svip", "superVip", "super_vip",
    "superVipLevel", "super_vip_level", "super_vip_type", "luxury_vip_type", "vip_luxury_type",
    "svip_level", "svip_status", "svip",
];
const KUGOU_VIP_EXPIRY_SOURCE_KEYS: &[&str] = &[
    "vip_end_time", "vipEndTime", "vip_expire_time", "vipExpireTime", "vip_expire", "vipExpire",
    "music_vip_end_time", "musicVipEndTime", "raw_vip_end_time", "rawVipEndTime", "rawvipendtime",
];
const KUGOU_SVIP_EXPIRY_SOURCE_KEYS: &[&str] = &[
    "svip_end_time", "svipEndTime", "svip_expire_time", "svipExpireTime",
    "super_vip_end_time", "superVipEndTime", "luxury_vip_end_time", "luxuryVipEndTime",
];
const KUGOU_MUSIC_PACKAGE_EXPIRY_SOURCE_KEYS: &[&str] = &[
    "music_end_time", "musicEndTime", "music_expire_time", "musicExpireTime",
    "raw_music_end_time", "rawMusicEndTime", "rawmusicendtime",
];
const KUGOU_WEB_ROLE_KEYS: &[&str] = &[
    "role", "user_type", "userType", "usertype", "user_y_type", "userYType", "userytype",
    "y_type", "yType", "ytype",
];

#[derive(Debug, Clone)]
struct KugouMembershipRecordState {
    known: bool,
    is_vip: bool,
    is_svip: bool,
    vip_type: u64,
    svip_type: u64,
    web_role: u64,
    web_role_known: bool,
    music_package_type: u64,
    music_package_expires_at: u64,
    expires_at: u64,
}

fn kugou_membership_record_state(
    value: &Value,
    allow_web_role: bool,
    now_ms: u64,
) -> KugouMembershipRecordState {
    let known = kugou_object_has_membership_signal(value, allow_web_role);
    let vip_type = first_positive_kugou_number(&[value], KUGOU_VIP_TYPE_KEYS);
    let svip_type = first_positive_kugou_number(&[value], KUGOU_SVIP_TYPE_KEYS);
    let vip_expiry = kugou_time_state(value, KUGOU_VIP_EXPIRY_SOURCE_KEYS, now_ms);
    let svip_expiry = kugou_time_state(value, KUGOU_SVIP_EXPIRY_SOURCE_KEYS, now_ms);
    let music_pkg_expiry = kugou_time_state(value, KUGOU_MUSIC_PACKAGE_EXPIRY_SOURCE_KEYS, now_ms);

    let (role_present, role_valid, web_role) = if allow_web_role {
        kugou_number_field_state(value, KUGOU_WEB_ROLE_KEYS)
    } else {
        (false, false, 0)
    };
    if role_present {
        let role_is_vip = role_valid && matches!(web_role, 1 | 2);
        let role_is_svip = role_valid && matches!(web_role, 6 | 11 | 13);
        let role_music_pkg = role_valid && matches!(web_role, 31 | 33);
        let role_expiry_ok = !vip_expiry.present || (vip_expiry.valid && vip_expiry.future);
        let pkg_expiry_ok =
            !music_pkg_expiry.present || (music_pkg_expiry.valid && music_pkg_expiry.future);
        let is_svip = role_is_svip && role_expiry_ok;
        let is_vip = (role_is_vip || role_is_svip) && role_expiry_ok;
        let has_pkg = role_music_pkg && pkg_expiry_ok;
        return KugouMembershipRecordState {
            known: true,
            is_vip,
            is_svip,
            vip_type: if is_vip { web_role } else { 0 },
            svip_type: if is_svip { web_role } else { 0 },
            web_role,
            web_role_known: true,
            music_package_type: if has_pkg { web_role } else { 0 },
            music_package_expires_at: if has_pkg {
                music_pkg_expiry.expires_at
            } else {
                0
            },
            expires_at: if is_vip { vip_expiry.expires_at } else { 0 },
        };
    }

    let is_svip = svip_expiry.future
        || (svip_type > 0 && !svip_expiry.present)
        || (value.get("isSvip") == Some(&Value::Bool(true)) && !svip_expiry.present);
    let is_vip = is_svip
        || vip_expiry.future
        || (vip_type > 0 && !vip_expiry.present)
        || (value.get("isVip") == Some(&Value::Bool(true)) && !vip_expiry.present);
    let exp = (if is_vip { vip_expiry.expires_at } else { 0 })
        .max(if is_svip { svip_expiry.expires_at } else { 0 });

    KugouMembershipRecordState {
        known,
        is_vip,
        is_svip,
        vip_type,
        svip_type,
        web_role: 0,
        web_role_known: false,
        music_package_type: 0,
        music_package_expires_at: 0,
        expires_at: exp,
    }
}

/// 规范化酷狗 VIP 探针响应（精确移植自 `normalizeKugouVipPayloadV2`）。
pub fn normalize_kugou_vip_payload_v2(
    payload: Option<&Value>,
    expected_user_id: &str,
    now_ms: u64,
) -> Value {
    let empty = json!({});
    let pl = payload.unwrap_or(&empty);
    let data = pl
        .get("data")
        .or_else(|| pl.get("result"))
        .or_else(|| pl.get("vip"))
        .unwrap_or(pl);
    let membership_stale = pl
        .get("__kugouMembershipStale")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let origin = pl
        .get("__kugouMembershipOrigin")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let is_web_role = origin == "kugou-web-roleinfo";
    let expected_uid: String = expected_user_id
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();

    let mut objs = Vec::new();
    collect_kugou_vip_objects(data, &mut objs, 0, &expected_uid, "");
    let mut api_states: Vec<KugouMembershipRecordState> = objs
        .iter()
        .map(|v| kugou_membership_record_state(v, is_web_role, now_ms))
        .filter(|s| s.known)
        .collect();
    if is_web_role {
        let auth_web: Vec<KugouMembershipRecordState> = api_states
            .iter()
            .filter(|s| s.web_role_known)
            .cloned()
            .collect();
        if !auth_web.is_empty() {
            api_states = auth_web;
        }
    }

    let api_known = !api_states.is_empty();
    let states = if api_known { api_states } else { Vec::new() };
    let active: Vec<&KugouMembershipRecordState> = states.iter().filter(|s| s.is_vip).collect();
    let is_svip = active.iter().any(|s| s.is_svip);
    let is_vip = is_svip || !active.is_empty();
    let vip_type = active.iter().map(|s| s.vip_type).max().unwrap_or(0);
    let svip_type = active.iter().map(|s| s.svip_type).max().unwrap_or(0);
    let music_package_type = states
        .iter()
        .map(|s| s.music_package_type)
        .max()
        .unwrap_or(0);
    let has_music_package = music_package_type > 0;
    let music_pkg_expires_at = states
        .iter()
        .map(|s| s.music_package_expires_at)
        .filter(|&v| v > 0)
        .min()
        .unwrap_or(0);
    let web_role = states.iter().map(|s| s.web_role).max().unwrap_or(0);
    let expires_at = active
        .iter()
        .map(|s| s.expires_at)
        .filter(|&v| v > 0)
        .min()
        .unwrap_or(0);
    let vip_level = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else {
        "none"
    };
    let tier = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else if has_music_package {
        "music-package"
    } else {
        "none"
    };
    let fresh_src = if is_web_role {
        "kugou-web-roleinfo"
    } else {
        "kugou-vip-api"
    };
    let stale_src = if is_web_role {
        "kugou-web-roleinfo-stale"
    } else {
        "kugou-vip-api-stale"
    };

    json!({
        "vipType": if is_svip { vip_type.max(svip_type) } else { vip_type },
        "svipType": svip_type,
        "vipLevel": vip_level,
        "isVip": is_vip,
        "isSvip": is_svip,
        "webRole": web_role,
        "hasMusicPackage": has_music_package,
        "musicPackageType": music_package_type,
        "musicPackageExpiresAt": music_pkg_expires_at,
        "membershipTier": tier,
        "membershipKnown": api_known,
        "membershipVerified": api_known && !membership_stale,
        "membershipStale": membership_stale,
        "vipSyncState": if membership_stale { "stale_positive" } else if api_known { "checked" } else { "unknown" },
        "membershipSource": if api_known { if membership_stale { stale_src } else { fresh_src } } else { "none" },
        "expiresAt": expires_at,
    })
}

pub fn normalize_kugou_web_role_info_payload(payload: Option<&Value>, expected_uid: &str) -> Value {
    let unknown = || {
        json!({
            "__kugouMembershipUnknown": true,
            "__kugouMembershipOrigin": "kugou-web-roleinfo",
        })
    };
    let Some(pl) = payload.filter(|v| v.is_object()) else {
        return unknown();
    };
    let data = pl
        .get("data")
        .or_else(|| pl.get("result"))
        .unwrap_or(pl);

    for target in [pl, data] {
        let Some(obj) = target.as_object() else { continue };
        for code_key in ["errno", "error_code", "errorCode", "errcode", "err_code"] {
            if let Some(n) = obj.get(code_key).and_then(value_as_f64) {
                if n > 0.0 {
                    return unknown();
                }
            }
        }
        if obj.get("success") == Some(&Value::Bool(false))
            || obj
                .get("status")
                .and_then(value_as_f64)
                .is_some_and(|n| n == 0.0)
            || obj
                .get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty())
        {
            return unknown();
        }
    }

    let expected: String = expected_uid
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let mut objs = Vec::new();
    collect_kugou_vip_objects(pl, &mut objs, 0, &expected, "");
    let record = objs
        .iter()
        .find(|v| kugou_number_field_state(v, KUGOU_WEB_ROLE_KEYS).0)
        .or_else(|| objs.iter().find(|v| kugou_object_has_membership_signal(v, true)));
    match record {
        Some(rec) => json!({
            "data": rec,
            "__kugouMembershipOrigin": "kugou-web-roleinfo",
        }),
        None => unknown(),
    }
}

pub fn kugou_membership_rights(status: &Value, playback_ready: bool, now_ms: u64) -> Value {
    let is_svip = status.get("isSvip").and_then(|v| v.as_bool()) == Some(true)
        || status.get("vipLevel").and_then(|v| v.as_str()) == Some("svip");
    let is_vip = is_svip
        || status.get("isVip").and_then(|v| v.as_bool()) == Some(true)
        || status.get("vipLevel").and_then(|v| v.as_str()) == Some("vip");
    let has_pkg = status.get("hasMusicPackage").and_then(|v| v.as_bool()) == Some(true)
        || status
            .get("musicPackageType")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0;
    let known = status
        .get("membershipKnown")
        .and_then(|v| v.as_bool())
        == Some(true);
    let verified = status
        .get("membershipVerified")
        .and_then(|v| v.as_bool())
        == Some(true);
    let sync_state = status
        .get("vipSyncState")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let source = status
        .get("membershipSource")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stale = status.get("membershipStale").and_then(|v| v.as_bool()) == Some(true)
        || sync_state == "stale_positive"
        || source.ends_with("-stale");
    let expires_at = status
        .get("expiresAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let pkg_expires_at = status
        .get("musicPackageExpiresAt")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let ready = playback_ready
        || status.get("playbackReady").and_then(|v| v.as_bool()) == Some(true)
        || status
            .get("playbackKeyReady")
            .and_then(|v| v.as_bool())
            == Some(true);

    let entitlement_current =
        verified && !stale && ready && (expires_at == 0 || expires_at > now_ms);
    let entitled_svip = entitlement_current && is_svip;
    let entitled_vip = entitlement_current && is_vip;
    let entitled_pkg =
        verified && !stale && ready && has_pkg && (pkg_expires_at == 0 || pkg_expires_at > now_ms);

    json!({
        "level": if is_svip { "svip" } else if is_vip { "vip" } else { "none" },
        "membershipTier": if is_svip { "svip" } else if is_vip { "vip" } else if has_pkg { "music-package" } else { "none" },
        "hasMusicPackage": has_pkg,
        "membershipKnown": known,
        "verified": verified,
        "stale": stale,
        "expiresAt": expires_at,
        "musicPackageExpiresAt": pkg_expires_at,
        "syncState": if !sync_state.is_empty() { sync_state } else if known { "checked" } else { "unknown" },
        "canPlayVipTracks": entitled_vip,
        "canPlaySvipTracks": entitled_svip,
        "canPlayMusicPackageTracks": entitled_pkg,
        "maxQuality": if entitled_svip { "hires" } else if entitled_vip { "lossless" } else { "standard" },
    })
}

pub fn kugou_effective_quality(requested_quality: &str, rights: &Value) -> &'static str {
    let requested = normalize_quality_preference(requested_quality);
    if rights.get("canPlaySvipTracks").and_then(|v| v.as_bool()) == Some(true) {
        return requested;
    }
    if rights.get("canPlayVipTracks").and_then(|v| v.as_bool()) != Some(true) {
        return "standard";
    }
    if requested == "jymaster" || requested == "hires" {
        "lossless"
    } else {
        requested
    }
}

// ---------- 曲目 / 歌单映射 ----------

fn map_kugou_artists(item: &Value) -> Vec<Value> {
    if let Some(singers) = item.get("Singers").and_then(|v| v.as_array()) {
        if !singers.is_empty() {
            return singers
                .iter()
                .filter_map(|s| {
                    let name = strip_kugou_html(
                        s.get("name")
                            .or_else(|| s.get("SingerName"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    );
                    if name.is_empty() {
                        return None;
                    }
                    let id = s
                        .get("id")
                        .or_else(|| s.get("SingerId"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    Some(json!({ "id": id, "name": name }))
                })
                .collect();
        }
    }
    static SEP_RE: OnceLock<Regex> = OnceLock::new();
    let re = SEP_RE.get_or_init(|| Regex::new(r"(?i)、|/|,| feat\.? ").unwrap());
    let singer_name = item
        .get("SingerName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ids = item
        .get("SingerId")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    re.split(singer_name)
        .map(strip_kugou_html)
        .filter(|n| !n.is_empty())
        .enumerate()
        .map(|(idx, name)| {
            let id = ids.get(idx).cloned().unwrap_or_else(|| json!(""));
            json!({ "id": id, "name": name })
        })
        .collect()
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub fn map_kugou_search_item(item: &Value) -> Value {
    let artists = map_kugou_artists(item);
    let hash = item
        .get("FileHash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let album_id = value_to_string(item.get("AlbumID"));
    let mix_song_id = value_to_string(item.get("MixSongID").or_else(|| item.get("mixsongid")));
    let album_audio_raw = value_to_string(
        item.get("EMixSongID")
            .or_else(|| item.get("AlbumAudioID"))
            .or_else(|| item.get("album_audio_id")),
    );
    let album_audio_id = if !mix_song_id.is_empty() && mix_song_id.bytes().all(|b| b.is_ascii_digit())
    {
        mix_song_id.clone()
    } else if !album_audio_raw.is_empty() {
        album_audio_raw
    } else {
        mix_song_id.clone()
    };
    let name = strip_kugou_html(
        item.get("SongName")
            .or_else(|| item.get("FileName"))
            .or_else(|| item.get("OriSongName"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let artist = {
        let joined = artists
            .iter()
            .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" / ");
        if !joined.is_empty() {
            joined
        } else {
            strip_kugou_html(
                item.get("SingerName")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
        }
    };
    let privilege = item
        .get("Privilege")
        .and_then(value_as_f64)
        .unwrap_or(0.0) as u64;
    let cover_raw = item
        .get("Image")
        .or_else(|| item.get("AlbumImage"))
        .or_else(|| item.get("cover"))
        .or_else(|| item.get("img"))
        .or_else(|| item.get("album_cover"))
        .or_else(|| item.get("album_img"))
        .or_else(|| item.get("pic"))
        .or_else(|| {
            item.get("albuminfo").and_then(|a| {
                a.get("img")
                    .or_else(|| a.get("cover"))
                    .or_else(|| a.get("sizable_cover"))
            })
        })
        .or_else(|| item.get("trans_param").and_then(|t| t.get("union_cover")))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let id = if !hash.is_empty() {
        hash.clone()
    } else if !mix_song_id.is_empty() {
        mix_song_id.clone()
    } else {
        album_audio_id.clone()
    };
    let popularity = item
        .get("Heat")
        .or_else(|| item.get("heat"))
        .or_else(|| item.get("Hot"))
        .or_else(|| item.get("hot"))
        .or_else(|| item.get("Score"))
        .or_else(|| item.get("score"))
        .and_then(value_as_f64)
        .unwrap_or(0.0);
    let rank_val = item
        .get("rank")
        .or_else(|| item.get("Rank"))
        .and_then(value_as_f64)
        .map(|n| json!(n))
        .unwrap_or(Value::Null);

    json!({
        "provider": "kugou",
        "source": "kugou",
        "type": "kugou",
        "id": id,
        "hash": hash,
        "fileHash": hash,
        "albumId": album_id,
        "album_id": album_id,
        "mixSongId": mix_song_id,
        "albumAudioId": album_audio_id,
        "album_audio_id": album_audio_id,
        "audioId": item.get("Audioid").or_else(|| item.get("Scid")).cloned().unwrap_or_else(|| json!("")),
        "name": name,
        "artist": artist,
        "artists": artists,
        "artistId": artists.first().and_then(|a| a.get("id")).cloned().unwrap_or(Value::Null),
        "album": strip_kugou_html(item.get("AlbumName").and_then(|v| v.as_str()).unwrap_or("")),
        "cover": kugou_cover_url(cover_raw, 240),
        "duration": (item.get("Duration").and_then(value_as_f64).unwrap_or(0.0) as u64) * 1000,
        "popularity": popularity,
        "kugouRank": rank_val,
        "fee": if privilege >= 10 { 1 } else { 0 },
        "privilege": privilege,
        "playable": privilege <= 8,
        "hqHash": item.get("HQFileHash").and_then(|v| v.as_str()).unwrap_or(""),
        "sqHash": item.get("SQFileHash").and_then(|v| v.as_str()).unwrap_or(""),
        "resHash": item.get("ResFileHash").and_then(|v| v.as_str()).unwrap_or(""),
    })
}

pub fn map_kugou_playlist_track(item: &Value) -> Value {
    let singers = item
        .get("singerinfo")
        .or_else(|| item.get("Singers"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let artist_label = singers
        .iter()
        .filter_map(|s| {
            s.get("name")
                .or_else(|| s.get("SingerName"))
                .and_then(|v| v.as_str())
        })
        .collect::<Vec<_>>()
        .join(" / ");
    let mix_song_id = value_to_string(
        item.get("mixsongid")
            .or_else(|| item.get("MixSongID"))
            .or_else(|| item.get("album_audio_id")),
    );
    let raw_name = item
        .get("name")
        .or_else(|| item.get("SongName"))
        .or_else(|| item.get("filename"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let duration_sec = item
        .get("duration")
        .or_else(|| item.get("Duration"))
        .and_then(value_as_f64)
        .unwrap_or_else(|| {
            item.get("timelen")
                .and_then(value_as_f64)
                .map(|ms| (ms / 1000.0).round())
                .unwrap_or(0.0)
        });
    let mut normalized = item.as_object().cloned().unwrap_or_default();
    normalized.insert(
        "FileHash".into(),
        item.get("hash")
            .or_else(|| item.get("FileHash"))
            .cloned()
            .unwrap_or(json!("")),
    );
    normalized.insert(
        "SongName".into(),
        json!(strip_kugou_filename(raw_name, &artist_label)),
    );
    if !normalized.contains_key("SingerName") {
        normalized.insert("SingerName".into(), json!(artist_label));
    }
    normalized.insert("Singers".into(), json!(singers));
    normalized.insert(
        "AlbumID".into(),
        item.get("albuminfo")
            .and_then(|a| a.get("id"))
            .or_else(|| item.get("album_id"))
            .or_else(|| item.get("AlbumID"))
            .cloned()
            .unwrap_or(json!("")),
    );
    normalized.insert("MixSongID".into(), json!(mix_song_id));
    normalized.insert(
        "EMixSongID".into(),
        json!(if !mix_song_id.is_empty() && mix_song_id.bytes().all(|b| b.is_ascii_digit()) {
            mix_song_id.clone()
        } else {
            value_to_string(item.get("album_audio_id").or_else(|| item.get("EMixSongID")))
        }),
    );
    normalized.insert(
        "AlbumName".into(),
        item.get("albuminfo")
            .and_then(|a| a.get("name"))
            .or_else(|| item.get("album_name"))
            .or_else(|| item.get("AlbumName"))
            .cloned()
            .unwrap_or(json!("")),
    );
    if let Some(cov) = item
        .get("cover")
        .or_else(|| item.get("img"))
        .or_else(|| item.get("Image"))
        .or_else(|| item.get("trans_param").and_then(|t| t.get("union_cover")))
    {
        normalized.insert("Image".into(), cov.clone());
    }
    normalized.insert("Duration".into(), json!(duration_sec));
    if let Some(priv_val) = item
        .get("media_privilege")
        .or_else(|| item.get("privilege"))
        .or_else(|| item.get("Privilege"))
    {
        normalized.insert("Privilege".into(), priv_val.clone());
    }
    let mut mapped = map_kugou_search_item(&Value::Object(normalized));
    if let Some(obj) = mapped.as_object_mut() {
        let fid = value_to_string(item.get("fileid").or_else(|| item.get("file_id")));
        if !fid.is_empty() {
            obj.insert("fileId".into(), json!(fid));
        }
    }
    mapped
}

fn parse_kugou_list_id(playlist_id: &str) -> String {
    let id = playlist_id.trim();
    if id.is_empty() {
        return String::new();
    }
    if id.bytes().all(|b| b.is_ascii_digit()) {
        return id.to_string();
    }
    if id.starts_with("collection_") {
        let parts: Vec<&str> = id.split('_').collect();
        if parts.len() >= 5 && !parts[3].is_empty() {
            return parts[3].to_string();
        }
    }
    id.to_string()
}

fn pick_kugou_play_url(json: &Value) -> String {
    fn pick(val: Option<&Value>) -> String {
        match val {
            Some(Value::Array(arr)) => arr.iter().map(|v| pick(Some(v))).find(|s| !s.is_empty()).unwrap_or_default(),
            Some(Value::String(s)) => {
                let c = s.replace("\\/", "/").trim().to_string();
                let lower = c.to_ascii_lowercase();
                if (lower.starts_with("http://") || lower.starts_with("https://"))
                    && !c.contains(',')
                    && !c.chars().any(|ch| ch.is_whitespace())
                {
                    c
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }
    let empty = json!({});
    let data = json.get("data").unwrap_or(&empty);
    for candidate in [
        pick(json.get("url")),
        pick(json.get("play_url")),
        pick(json.get("backupUrl")),
        pick(json.get("backup_url")),
        pick(json.get("play_backup_url")),
        pick(data.get("url")),
        pick(data.get("play_url")),
        pick(data.get("backupUrl")),
        pick(data.get("backup_url")),
        pick(data.get("play_backup_url")),
    ] {
        if !candidate.is_empty() {
            return candidate;
        }
    }
    String::new()
}

fn kugou_playback_trial(json: &Value) -> bool {
    let empty = json!({});
    let data = json.get("data").filter(|v| v.is_object()).unwrap_or(&empty);
    [json, data].iter().any(|obj| {
        ["is_free_part", "isFreePart", "trial", "is_trial", "isTrial"]
            .iter()
            .any(|k| {
                obj.get(*k).is_some_and(|v| match v {
                    Value::Bool(b) => *b,
                    Value::Number(n) => n.as_u64() == Some(1),
                    Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
                    _ => false,
                })
            })
    })
}

#[derive(Debug, Clone)]
struct KugouPlayAttemptResult {
    url: String,
    level: String,
    quality: String,
    trial: bool,
    source: String,
    restricted_category: String,
    restricted_message: String,
}

fn build_restriction_result(json: &Value, auth: &KugouAuth) -> KugouPlayAttemptResult {
    let code = json
        .get("err_code")
        .or_else(|| json.get("error_code"))
        .or_else(|| json.get("errcode"))
        .or_else(|| json.get("errno"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let raw_msg = json
        .get("error")
        .or_else(|| json.get("errmsg"))
        .or_else(|| json.get("msg"))
        .or_else(|| json.get("message"))
        .or_else(|| json.get("show_tips"))
        .or_else(|| json.get("data").and_then(|d| d.get("msg").or_else(|| d.get("show_tips"))))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let lower = raw_msg.to_ascii_lowercase();
    let (cat, msg) = if code == 30020 {
        ("verification_required", "酷狗需要官方安全验证，请打开酷狗官方登录窗口完成验证")
    } else if code == 30022 {
        ("client_only", "该酷狗歌曲仅支持官方客户端播放")
    } else if lower.contains("token") || lower.contains("login") || raw_msg.contains("登录") || raw_msg.contains("会话") || raw_msg.contains("凭证") {
        ("login_required", "酷狗播放登录态需要重新验证，请重新打开官方登录窗口")
    } else if lower.contains("vip") || raw_msg.contains("付费") || raw_msg.contains("会员") || raw_msg.contains("购买") {
        (if auth.playback_ready { "vip_required" } else { "login_required" }, "该酷狗歌曲需要有效会员或已购买权限")
    } else {
        ("url_unavailable", "酷狗暂未返回播放地址，请稍后重试")
    };
    KugouPlayAttemptResult {
        url: String::new(),
        level: String::new(),
        quality: String::new(),
        trial: false,
        source: String::new(),
        restricted_category: cat.to_string(),
        restricted_message: msg.to_string(),
    }
}

fn kugou_quality_param(requested: &str) -> &'static str {
    match normalize_quality_preference(requested) {
        "jymaster" => "viper_tape",
        "hires" => "hires",
        "lossless" => "flac",
        "exhigh" => "320",
        _ => "128",
    }
}

/// 酷狗音乐异步客户端。
pub struct KugouClient {
    cookie_file: Option<PathBuf>,
    cookie: RwLock<String>,
    client: reqwest::Client,
    search_cache: TtlCache<String, Vec<Value>>,
    song_url_cache: TtlCache<String, Value>,
    playlist_tracks_cache: TtlCache<String, Value>,
    profile_cache: TtlCache<String, Value>,
    vip_cache: TtlCache<String, Value>,
    verified_vip_history: Mutex<HashMap<String, (Value, u64)>>,
    favorite_list_cache: Mutex<(String, String, u64)>,
    like_file_id_by_hash: Mutex<HashMap<String, String>>,
}

impl KugouClient {
    /// 创建酷狗客户端；若指定持久化路径且文件存在，自动加载 Cookie。
    pub fn new(cookie_file: Option<PathBuf>) -> Self {
        let initial = cookie_file
            .as_deref()
            .map(read_cookie_file)
            .unwrap_or_default();
        Self {
            cookie_file,
            cookie: RwLock::new(initial),
            client: shared_http_client().clone(),
            search_cache: TtlCache::new(120, Duration::from_secs(2 * 60)),
            song_url_cache: TtlCache::new(240, Duration::from_secs(15 * 60)),
            playlist_tracks_cache: TtlCache::new(24, Duration::from_secs(5 * 60)),
            profile_cache: TtlCache::new(24, Duration::from_secs(5 * 60)),
            vip_cache: TtlCache::new(24, Duration::from_secs(5 * 60)),
            verified_vip_history: Mutex::new(HashMap::new()),
            favorite_list_cache: Mutex::new((String::new(), String::new(), 0)),
            like_file_id_by_hash: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_cookie(&self) -> String {
        self.cookie.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn has_cookie(&self) -> bool {
        !self.get_cookie().is_empty()
    }

    fn clear_session_caches(&self) {
        self.search_cache.clear();
        self.song_url_cache.clear();
        self.playlist_tracks_cache.clear();
        self.profile_cache.clear();
        self.vip_cache.clear();
        if let Ok(mut g) = self.verified_vip_history.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.favorite_list_cache.lock() {
            *g = (String::new(), String::new(), 0);
        }
        if let Ok(mut g) = self.like_file_id_by_hash.lock() {
            g.clear();
        }
    }

    pub fn set_cookie(&self, input: &str) -> Result<String, String> {
        let value = input.trim().to_string();
        if let Ok(mut g) = self.cookie.write() {
            *g = value.clone();
        }
        if let Some(path) = &self.cookie_file {
            write_cookie_file(path, &value)?;
        }
        self.clear_session_caches();
        Ok(value)
    }

    pub fn logout(&self) -> Value {
        let _ = self.set_cookie("");
        json!({ "ok": true })
    }

    fn vip_cache_key(auth: &KugouAuth) -> String {
        let material = format!(
            "{}\n{}\n{}\n{}",
            if auth.userid.is_empty() { "0" } else { &auth.userid },
            auth.token,
            auth.mid,
            auth.dfid
        );
        format!("vip|{}", hex::encode(Sha256::digest(material.as_bytes())))
    }

    fn build_h5_params(&self, auth: &KugouAuth, extra: &[(&str, String)]) -> BTreeMap<String, String> {
        let now = unix_millis().to_string();
        let mut map = BTreeMap::new();
        map.insert("srcappid".into(), KUGOU_H5_SRC_APPID.into());
        map.insert("clientver".into(), KUGOU_H5_CLIENTVER.into());
        map.insert("clienttime".into(), now.clone());
        map.insert("mid".into(), auth.mid.clone());
        map.insert("uuid".into(), now);
        map.insert("dfid".into(), auth.dfid.clone());
        map.insert("appid".into(), KUGOU_WEB_APPID.to_string());
        map.insert("token".into(), auth.token.clone());
        map.insert(
            "userid".into(),
            if auth.userid.is_empty() {
                "0".into()
            } else {
                auth.userid.clone()
            },
        );
        for (k, v) in extra {
            map.insert((*k).to_string(), v.clone());
        }
        map
    }

    async fn h5_gateway_request(
        &self,
        path: &str,
        method: &str,
        router: Option<&str>,
        extra_params: &[(&str, String)],
        body_obj: Option<&Value>,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let auth = extract_kugou_auth(&cookie);
        if !auth.playback_ready {
            return Err("KUGOU_AUTH_REQUIRED".to_string());
        }
        let body_text = body_obj
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("JSON encode: {e}"))?;
        let mut params = self.build_h5_params(&auth, extra_params);
        let sig = signature_h5_params(&params, body_text.as_deref());
        params.insert("signature".into(), sig);

        let mut url = reqwest::Url::parse(&format!("{KUGOU_GATEWAY}{path}"))
            .map_err(|e| format!("Invalid URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &params {
                pairs.append_pair(k, v);
            }
        }

        let is_post = method.eq_ignore_ascii_case("POST") || body_text.is_some();
        let mut req = if is_post {
            self.client.post(url)
        } else {
            self.client.get(url)
        };
        req = req
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie));
        if let Some(r) = router {
            req = req.header("x-router", r);
        }
        if let Some(bt) = body_text {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bt);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("Kugou H5 gateway failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Kugou H5 gateway JSON parse: {e}"))?;
        if json.get("status").and_then(|v| v.as_i64()) == Some(0) {
            let msg = json
                .get("error")
                .or_else(|| json.get("msg"))
                .or_else(|| json.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("KUGOU_GATEWAY_FAILED");
            return Err(msg.to_string());
        }
        Ok(json)
    }

    async fn android_gateway_request(
        &self,
        base_url: Option<&str>,
        path: &str,
        method: &str,
        router: Option<&str>,
        extra_params: &[(&str, String)],
        body_str: Option<&str>,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let auth = extract_kugou_auth(&cookie);
        if !auth.playback_ready {
            return Err("KUGOU_AUTH_REQUIRED".to_string());
        }
        let clienttime = unix_secs().to_string();
        let mut params = BTreeMap::new();
        params.insert("dfid".into(), auth.dfid.clone());
        params.insert("mid".into(), auth.mid.clone());
        params.insert("uuid".into(), "-".into());
        params.insert("appid".into(), KUGOU_APPID.to_string());
        params.insert("clientver".into(), KUGOU_CLIENTVER.to_string());
        params.insert("clienttime".into(), clienttime.clone());
        params.insert("token".into(), auth.token.clone());
        params.insert("userid".into(), auth.userid.clone());
        for (k, v) in extra_params {
            params.insert((*k).to_string(), v.clone());
        }
        let sig = signature_android_params(&params, body_str.unwrap_or(""));
        params.insert("signature".into(), sig);

        let base = base_url.unwrap_or(KUGOU_GATEWAY);
        let mut url = reqwest::Url::parse(&format!("{base}{path}"))
            .map_err(|e| format!("Invalid URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &params {
                pairs.append_pair(k, v);
            }
        }

        let mut req = if method.eq_ignore_ascii_case("POST") {
            self.client.post(url)
        } else {
            self.client.get(url)
        };
        req = req
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, KUGOU_GATEWAY_UA)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header("dfid", &auth.dfid)
            .header("mid", &auth.mid)
            .header("clienttime", &clienttime)
            .header("kg-rc", "1")
            .header("kg-thash", "5d816a0")
            .header("kg-rec", "1")
            .header("kg-rf", "B9EDA08A64250DEFFBCADDEE00F8F25F")
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie));
        if let Some(r) = router {
            req = req.header("x-router", r);
        }
        if let Some(b) = body_str.filter(|s| !s.is_empty()) {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(b.to_string());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("Kugou gateway failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Kugou gateway JSON parse: {e}"))?;
        if json.get("status").and_then(|v| v.as_i64()) == Some(0) {
            let msg = json
                .get("error")
                .or_else(|| json.get("msg"))
                .or_else(|| json.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("KUGOU_GATEWAY_FAILED");
            return Err(msg.to_string());
        }
        Ok(json)
    }

    // ---------- 搜索 ----------

    async fn search_page(
        &self,
        keywords: &str,
        page_size: usize,
        offset: usize,
    ) -> Result<Vec<Value>, String> {
        let cookie = self.get_cookie();
        let auth = extract_kugou_auth(&cookie);
        let page = offset / page_size + 1;
        let mut url = reqwest::Url::parse(KUGOU_SEARCH_URL).unwrap();
        url.query_pairs_mut()
            .append_pair("keyword", keywords)
            .append_pair("page", &page.to_string())
            .append_pair("pagesize", &page_size.to_string())
            .append_pair(
                "userid",
                if auth.userid.is_empty() { "-1" } else { &auth.userid },
            )
            .append_pair("clientver", "2000")
            .append_pair("platform", "WebFilter")
            .append_pair("tag", "em")
            .append_pair("filter", "2")
            .append_pair("iscorrection", "1")
            .append_pair("privilege_filter", "0")
            .append_pair("filter_ver", "2")
            .append_pair("appid", &KUGOU_WEB_APPID.to_string())
            .append_pair("token", &auth.token)
            .append_pair("mid", &auth.mid);

        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_secs(10))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await
            .map_err(|e| format!("Kugou search failed: {e}"))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Kugou search JSON parse: {e}"))?;

        let lists = json
            .get("data")
            .and_then(|d| d.get("lists"))
            .and_then(|v| v.as_array())
            .filter(|_| json.get("status").and_then(|v| v.as_i64()) == Some(1))
            .ok_or_else(|| "KUGOU_SEARCH_UNAVAILABLE".to_string())?;

        Ok(lists
            .iter()
            .map(map_kugou_search_item)
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("hash").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect())
    }

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
        let lim = limit.clamp(1, 20);
        let auth = extract_kugou_auth(&self.get_cookie());
        let cache_key = format!(
            "{}:{}:{lim}:{offset}",
            Self::vip_cache_key(&auth),
            kw.to_ascii_lowercase()
        );
        if let Some(cached) = self.search_cache.get(&cache_key) {
            return Ok(cached);
        }
        let skip = offset % lim;
        let first = self.search_page(kw, lim, offset - skip).await?;
        let result: Vec<Value> = if skip == 0 || first.len() < lim {
            first.into_iter().skip(skip).collect()
        } else {
            let next = self.search_page(kw, lim, offset - skip + lim).await?;
            first
                .into_iter()
                .chain(next)
                .skip(skip)
                .take(lim)
                .collect()
        };
        self.search_cache.set(cache_key, result.clone(), None);
        Ok(result)
    }

    // ---------- VIP 探测与登录状态 ----------

    fn stabilize_vip_probe(&self, cache_key: &str, payload: Value, auth: &KugouAuth) -> Value {
        let now = unix_millis();
        let parsed = normalize_kugou_vip_payload_v2(Some(&payload), &auth.userid, now);
        if parsed.get("membershipVerified").and_then(|v| v.as_bool()) == Some(true)
            && parsed.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true)
        {
            if parsed.get("isVip").and_then(|v| v.as_bool()) != Some(true) {
                if let Ok(mut g) = self.verified_vip_history.lock() {
                    g.remove(cache_key);
                }
                return payload;
            }
            let ent_exp = parsed
                .get("expiresAt")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if ent_exp <= now {
                if let Ok(mut g) = self.verified_vip_history.lock() {
                    g.remove(cache_key);
                }
                return payload;
            }
            let stale_until = (now + KUGOU_VIP_STALE_POSITIVE_GRACE_MS).min(ent_exp);
            if let Ok(mut g) = self.verified_vip_history.lock() {
                g.insert(cache_key.to_string(), (payload.clone(), stale_until));
            }
            return payload;
        }
        if payload
            .get("__kugouMembershipUnknown")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            if let Ok(mut g) = self.verified_vip_history.lock() {
                if let Some((prev_payload, stale_until)) = g.get(cache_key).cloned() {
                    if now < stale_until {
                        let mut out = prev_payload.as_object().cloned().unwrap_or_default();
                        out.insert("__kugouMembershipStale".into(), json!(true));
                        out.insert("__kugouMembershipStaleUntil".into(), json!(stale_until));
                        return Value::Object(out);
                    }
                    g.remove(cache_key);
                }
            }
        }
        payload
    }

    async fn fetch_web_vip_info(&self, auth: &KugouAuth) -> Value {
        let cookie = self.get_cookie();
        if !auth.logged_in || cookie.is_empty() {
            return normalize_kugou_web_role_info_payload(None, &auth.userid);
        }
        let url = format!("{KUGOU_VIP_ROLEINFO_URL}?n={}", unix_millis());
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_millis(2500))
            .header(reqwest::header::ACCEPT, "application/json, text/javascript, */*; q=0.01")
            .header(reqwest::header::REFERER, "https://vip.kugou.com/")
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header("X-Requested-With", "XMLHttpRequest")
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await;
        let payload = match resp {
            Ok(r) => r.json::<Value>().await.ok(),
            Err(_) => None,
        };
        normalize_kugou_web_role_info_payload(payload.as_ref(), &auth.userid)
    }

    async fn fetch_vip_info(&self, auth: &KugouAuth) -> Option<Value> {
        if !auth.logged_in {
            return None;
        }
        let cache_key = Self::vip_cache_key(auth);
        if let Some(cached) = self.vip_cache.get(&cache_key) {
            return Some(cached);
        }

        let web_role = self.fetch_web_vip_info(auth).await;
        let web_membership =
            normalize_kugou_vip_payload_v2(Some(&web_role), &auth.userid, unix_millis());
        let result = if web_membership.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true)
            || !auth.playback_ready
        {
            self.stabilize_vip_probe(&cache_key, web_role, auth)
        } else {
            let primary = tokio::time::timeout(
                Duration::from_millis(1500),
                self.android_gateway_request(
                    None,
                    "/v1/get_union_vip",
                    "GET",
                    None,
                    &[("busi_type", "concept".into())],
                    None,
                    2500,
                ),
            )
            .await
            .ok()
            .and_then(|r| r.ok());
            let primary_parsed = normalize_kugou_vip_payload_v2(
                primary.as_ref(),
                &auth.userid,
                unix_millis(),
            );
            let raw_probe = if primary_parsed.get("isVip").and_then(|v| v.as_bool()) == Some(true) {
                primary.unwrap()
            } else {
                let q1 = [("busi_type", "concept".to_string())];
                let q2 = [("busi_type", "concept".to_string()), ("userids", auth.userid.clone())];
                let q3 = [("busi_type", "concept".to_string()), ("userids", auth.userid.clone())];
                let q4 = [("plat", "0".to_string())];
                let q5 = [("busi_type", "concept".to_string())];
                let (f1, f2, f3, f4, f5) = tokio::join!(
                    self.android_gateway_request(None, "/v1/vipuser_sub", "GET", None, &q1, None, 2500),
                    self.android_gateway_request(None, "/kugouvip/v2/batch_union_vipinfo", "GET", None, &q2, None, 2500),
                    self.android_gateway_request(None, "/kugouvip/v1/batch_union_vipinfo", "GET", None, &q3, None, 2500),
                    self.android_gateway_request(None, "/mobile/vipinfo", "GET", None, &q4, None, 2500),
                    self.android_gateway_request(Some("https://kugouvip.kugou.com"), "/v1/get_union_vip", "GET", None, &q5, None, 2500),
                );
                let fallbacks = [f1.ok(), f2.ok(), f3.ok(), f4.ok(), f5.ok()];
                let mut vip_hit: Option<Value> = None;
                let mut known_non_member: Option<Value> = if primary_parsed
                    .get("membershipKnown")
                    .and_then(|v| v.as_bool())
                    == Some(true)
                {
                    primary
                } else {
                    None
                };
                let mut all_known = known_non_member.is_some();
                for item in fallbacks {
                    let Some(data) = item else {
                        all_known = false;
                        continue;
                    };
                    let parsed =
                        normalize_kugou_vip_payload_v2(Some(&data), &auth.userid, unix_millis());
                    if parsed.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true) {
                        if parsed.get("isVip").and_then(|v| v.as_bool()) == Some(true) {
                            vip_hit = Some(data);
                            break;
                        }
                        if known_non_member.is_none() {
                            known_non_member = Some(data);
                        }
                    } else {
                        all_known = false;
                    }
                }
                vip_hit
                    .or_else(|| if all_known { known_non_member } else { None })
                    .unwrap_or_else(|| json!({ "__kugouMembershipUnknown": true }))
            };
            self.stabilize_vip_probe(&cache_key, raw_probe, auth)
        };

        let parsed = normalize_kugou_vip_payload_v2(Some(&result), &auth.userid, unix_millis());
        let ttl = if result
            .get("__kugouMembershipUnknown")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            Duration::from_secs(10)
        } else if parsed.get("isVip").and_then(|v| v.as_bool()) == Some(true) {
            Duration::from_secs(5 * 60)
        } else {
            Duration::from_secs(60)
        };
        self.vip_cache.set(cache_key, result.clone(), Some(ttl));
        Some(result)
    }

    async fn fetch_profile_from_playlists(&self, auth: &KugouAuth) -> Value {
        if !auth.playback_ready {
            return json!({});
        }
        let cache_key = format!("profile|{}", Self::vip_cache_key(auth));
        if let Some(cached) = self.profile_cache.get(&cache_key) {
            return cached;
        }
        let Ok(json_resp) = self
            .h5_gateway_request(
                "/v7/get_all_list",
                "POST",
                Some("cloudlist.service.kugou.com"),
                &[("plat", "1".into())],
                Some(&json!({
                    "userid": auth.userid.parse::<u64>().unwrap_or(0),
                    "token": auth.token,
                    "total_ver": 979,
                    "type": 2,
                    "page": 1,
                    "pagesize": 20,
                })),
                2500,
            )
            .await
        else {
            return json!({});
        };
        let lists = extract_kugou_gateway_playlist_lists(&json_resp);
        let profile = pick_kugou_profile_from_lists(&lists, auth);
        self.profile_cache.set(cache_key, profile.clone(), None);
        profile
    }

    pub async fn login_status(&self) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let auth = extract_kugou_auth(&cookie);
        let profile_fut = async {
            if auth.nickname.is_empty() || auth.avatar.is_empty() {
                self.fetch_profile_from_playlists(&auth).await
            } else {
                json!({})
            }
        };
        let (profile, vip_probe) = tokio::join!(profile_fut, self.fetch_vip_info(&auth));
        let now = unix_millis();
        let vip = normalize_kugou_vip_payload_v2(vip_probe.as_ref(), &auth.userid, now);
        let nickname = if !auth.nickname.is_empty() {
            auth.nickname.clone()
        } else if let Some(pn) = profile
            .get("nickname")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            pn.to_string()
        } else if auth.logged_in {
            format!(
                "酷狗 {}",
                if auth.userid.is_empty() { "用户" } else { &auth.userid }
            )
        } else {
            "酷狗音乐".to_string()
        };
        let avatar = if !auth.avatar.is_empty() {
            auth.avatar.clone()
        } else {
            profile
                .get("avatar")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let rights = kugou_membership_rights(&vip, auth.playback_ready, now);
        let vip_level = vip
            .get("vipLevel")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        let vip_label = match vip_level {
            "svip" => "SVIP",
            "vip" => "VIP",
            _ => "无VIP",
        };

        Ok(json!({
            "provider": "kugou",
            "loggedIn": auth.logged_in,
            "playbackReady": auth.playback_ready,
            "playbackKeyReady": auth.playback_ready,
            "userId": auth.userid,
            "nickname": nickname,
            "avatar": avatar,
            "vipType": vip["vipType"],
            "svipType": vip["svipType"],
            "vipLevel": vip_level,
            "isVip": vip["isVip"],
            "isSvip": vip["isSvip"],
            "hasMusicPackage": vip["hasMusicPackage"],
            "musicPackageType": vip["musicPackageType"],
            "musicPackageExpiresAt": vip["musicPackageExpiresAt"],
            "membershipTier": vip["membershipTier"],
            "vipLabel": vip_label,
            "membershipKnown": vip["membershipKnown"],
            "membershipVerified": vip["membershipVerified"],
            "membershipStale": vip["membershipStale"],
            "vipSyncState": vip["vipSyncState"],
            "membershipSource": vip["membershipSource"],
            "membershipRights": rights,
            "expiresAt": vip["expiresAt"],
            "hasCookie": !cookie.is_empty(),
            "hasToken": !auth.token.is_empty(),
        }))
    }

    // ---------- 播放地址（5 路官方路由） ----------

    async fn play_via_h5(
        &self,
        hash: &str,
        album_id: &str,
        album_audio_id: u64,
        auth: &KugouAuth,
        requested_quality: &str,
        is_vip: bool,
        timeout_ms: u64,
    ) -> Result<KugouPlayAttemptResult, String> {
        if !auth.playback_ready {
            return Err("not ready".into());
        }
        let cookie = self.get_cookie();
        let quality = kugou_quality_param(requested_quality);
        let file_hash = hash.to_ascii_lowercase();
        let key = sign_key(&file_hash, &auth.mid, &auth.userid, KUGOU_WEB_APPID);
        let mut params = self.build_h5_params(
            auth,
            &[
                ("album_id", album_id.parse::<u64>().unwrap_or(0).to_string()),
                ("area_code", "1".into()),
                ("hash", file_hash),
                ("ssa_flag", "is_fromtrack".into()),
                ("version", "11430".into()),
                ("quality", quality.into()),
                ("album_audio_id", album_audio_id.to_string()),
                ("behavior", "play".into()),
                ("pid", "2".into()),
                ("cmd", "26".into()),
                ("pidversion", "3001".into()),
                ("IsFreePart", if is_vip { "0" } else { "1" }.into()),
                ("cdnBackup", "1".into()),
                ("module", "".into()),
                ("key", key),
            ],
        );
        let sig = signature_h5_params(&params, None);
        params.insert("signature".into(), sig);

        let mut url = reqwest::Url::parse(&format!("{KUGOU_GATEWAY}/v5/url")).unwrap();
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &params {
                pairs.append_pair(k, v);
            }
        }
        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header("x-router", "trackercdn.kugou.com")
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let play_url = pick_kugou_play_url(&json);
        if json.get("status").and_then(|v| v.as_i64()) == Some(1) && !play_url.is_empty() {
            let lvl = normalize_quality_preference(requested_quality);
            return Ok(KugouPlayAttemptResult {
                url: play_url,
                level: lvl.into(),
                quality: lvl.into(),
                trial: kugou_playback_trial(&json),
                source: "h5".into(),
                restricted_category: String::new(),
                restricted_message: String::new(),
            });
        }
        Ok(build_restriction_result(&json, auth))
    }

    async fn play_via_web(
        &self,
        hash: &str,
        album_id: &str,
        album_audio_id: u64,
        auth: &KugouAuth,
        timeout_ms: u64,
        retry: bool,
    ) -> Result<KugouPlayAttemptResult, String> {
        let cookie = self.get_cookie();
        let mut extra = vec![
            ("uuid", auth.mid.clone()),
            ("platid", "4".into()),
            ("hash", hash.to_string()),
            (
                "album_id",
                if album_id.is_empty() {
                    "0".into()
                } else {
                    album_id.to_string()
                },
            ),
        ];
        if album_audio_id > 0 {
            extra.push(("album_audio_id", album_audio_id.to_string()));
        }
        let mut params = self.build_h5_params(auth, &extra);
        let sig = signature_h5_params(&params, None);
        params.insert("signature".into(), sig);

        let base = if retry {
            KUGOU_PLAY_WEB_RETRY
        } else {
            KUGOU_PLAY_WEB
        };
        let mut url = reqwest::Url::parse(base).unwrap();
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &params {
                pairs.append_pair(k, v);
            }
        }
        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let play_url = pick_kugou_play_url(&json);
        if json.get("status").and_then(|v| v.as_i64()) == Some(1) && !play_url.is_empty() {
            let data = json.get("data");
            let bitrate = data
                .and_then(|d| d.get("bitrate"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let kbps = if bitrate >= 10_000 {
                bitrate / 1000
            } else {
                bitrate
            };
            let level = if kbps >= 900 {
                "lossless"
            } else if kbps >= 300 {
                "exhigh"
            } else {
                "standard"
            };
            let quality = data
                .and_then(|d| d.get("quality"))
                .and_then(|v| v.as_str())
                .unwrap_or(level);
            return Ok(KugouPlayAttemptResult {
                url: play_url,
                level: level.into(),
                quality: quality.into(),
                trial: kugou_playback_trial(&json),
                source: "web".into(),
                restricted_category: String::new(),
                restricted_message: String::new(),
            });
        }
        Ok(build_restriction_result(&json, auth))
    }

    async fn play_via_mobile(
        &self,
        hash: &str,
        album_id: &str,
        auth: &KugouAuth,
        is_vip: bool,
        timeout_ms: u64,
    ) -> Result<KugouPlayAttemptResult, String> {
        let cookie = self.get_cookie();
        let mut url = reqwest::Url::parse(KUGOU_PLAY_MOBILE).unwrap();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("cmd", "playInfo");
            pairs.append_pair("hash", hash);
            pairs.append_pair("key", &kugou_cloud_key(hash));
            pairs.append_pair(
                "album_id",
                if album_id.is_empty() { "0" } else { album_id },
            );
            pairs.append_pair("pid", "1");
            pairs.append_pair("forceDown", "0");
            pairs.append_pair("vip", if is_vip { "1" } else { "65530" });
            if !auth.userid.is_empty() {
                pairs.append_pair("userid", &auth.userid);
            }
            if !auth.token.is_empty() {
                pairs.append_pair("token", &auth.token);
            }
        }
        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, "https://m.kugou.com/")
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let play_url = pick_kugou_play_url(&json);
        if json.get("status").and_then(|v| v.as_i64()) == Some(1) && !play_url.is_empty() {
            return Ok(KugouPlayAttemptResult {
                url: play_url,
                level: "standard".into(),
                quality: "标准".into(),
                trial: kugou_playback_trial(&json),
                source: "mobile".into(),
                restricted_category: String::new(),
                restricted_message: String::new(),
            });
        }
        Ok(build_restriction_result(&json, auth))
    }

    async fn play_via_gateway(
        &self,
        hash: &str,
        album_id: &str,
        album_audio_id: u64,
        auth: &KugouAuth,
        requested_quality: &str,
        is_vip: bool,
        timeout_ms: u64,
    ) -> Result<KugouPlayAttemptResult, String> {
        if !auth.playback_ready {
            return Err("not ready".into());
        }
        let cookie = self.get_cookie();
        let quality = kugou_quality_param(requested_quality);
        let clienttime = unix_secs().to_string();
        let file_hash = hash.to_ascii_lowercase();
        let key = sign_key(&file_hash, &auth.mid, &auth.userid, KUGOU_APPID);

        let mut url = reqwest::Url::parse(&format!("{KUGOU_GATEWAY}/v5/url")).unwrap();
        url.query_pairs_mut()
            .append_pair("dfid", &auth.dfid)
            .append_pair("mid", &auth.mid)
            .append_pair("uuid", "-")
            .append_pair("appid", &KUGOU_APPID.to_string())
            .append_pair("clientver", &KUGOU_CLIENTVER.to_string())
            .append_pair("clienttime", &clienttime)
            .append_pair("token", &auth.token)
            .append_pair("userid", &auth.userid)
            .append_pair("album_id", &album_id.parse::<u64>().unwrap_or(0).to_string())
            .append_pair("area_code", "1")
            .append_pair("hash", &file_hash)
            .append_pair("ssa_flag", "is_fromtrack")
            .append_pair("version", "11430")
            .append_pair("quality", quality)
            .append_pair("album_audio_id", &album_audio_id.to_string())
            .append_pair("behavior", "play")
            .append_pair("pid", "2")
            .append_pair("cmd", "26")
            .append_pair("pidversion", "3001")
            .append_pair("IsFreePart", if is_vip { "0" } else { "1" })
            .append_pair("cdnBackup", "1")
            .append_pair("module", "")
            .append_pair("key", &key);

        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_millis(timeout_ms))
            .header(reqwest::header::USER_AGENT, KUGOU_GATEWAY_UA)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .header("dfid", &auth.dfid)
            .header("mid", &auth.mid)
            .header("clienttime", &clienttime)
            .header("x-router", "trackercdn.kugou.com")
            .header(reqwest::header::COOKIE, build_kugou_request_cookie(&cookie))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let play_url = pick_kugou_play_url(&json);
        if json.get("status").and_then(|v| v.as_i64()) == Some(1) && !play_url.is_empty() {
            let lvl = normalize_quality_preference(requested_quality);
            return Ok(KugouPlayAttemptResult {
                url: play_url,
                level: lvl.into(),
                quality: lvl.into(),
                trial: kugou_playback_trial(&json),
                source: "gateway".into(),
                restricted_category: String::new(),
                restricted_message: String::new(),
            });
        }
        Ok(build_restriction_result(&json, auth))
    }

    fn attach_playback_status(
        &self,
        mut payload: Value,
        auth: &KugouAuth,
        membership: &Value,
        rights: &Value,
    ) -> Value {
        if let Some(obj) = payload.as_object_mut() {
            let vip_level = membership
                .get("vipLevel")
                .and_then(|v| v.as_str())
                .unwrap_or("none");
            let is_svip = membership
                .get("isSvip")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_vip = membership
                .get("isVip")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            obj.insert("loggedIn".into(), json!(auth.logged_in));
            obj.insert("playbackReady".into(), json!(auth.playback_ready));
            obj.insert("playbackKeyReady".into(), json!(auth.playback_ready));
            obj.insert("vipType".into(), membership["vipType"].clone());
            obj.insert("svipType".into(), membership["svipType"].clone());
            obj.insert("vipLevel".into(), json!(vip_level));
            obj.insert("isVip".into(), json!(is_vip));
            obj.insert("isSvip".into(), json!(is_svip));
            obj.insert("hasMusicPackage".into(), membership["hasMusicPackage"].clone());
            obj.insert(
                "musicPackageType".into(),
                membership["musicPackageType"].clone(),
            );
            obj.insert(
                "musicPackageExpiresAt".into(),
                membership["musicPackageExpiresAt"].clone(),
            );
            obj.insert("membershipTier".into(), membership["membershipTier"].clone());
            obj.insert(
                "vipLabel".into(),
                json!(if is_svip {
                    "SVIP"
                } else if is_vip {
                    "VIP"
                } else {
                    "No VIP"
                }),
            );
            obj.insert("membershipKnown".into(), membership["membershipKnown"].clone());
            obj.insert(
                "membershipVerified".into(),
                membership["membershipVerified"].clone(),
            );
            obj.insert("membershipStale".into(), membership["membershipStale"].clone());
            obj.insert("vipSyncState".into(), membership["vipSyncState"].clone());
            obj.insert(
                "membershipSource".into(),
                membership["membershipSource"].clone(),
            );
            obj.insert("membershipRights".into(), rights.clone());
        }
        payload
    }

    /// 获取酷狗歌曲播放 URL（严格音质权限门控 + 5 路探测链 + 试听降级）。
    pub async fn song_url(
        &self,
        hash: &str,
        album_id: &str,
        album_audio_id: Option<&str>,
        quality: &str,
    ) -> Result<Value, String> {
        self.song_url_with_hints(hash, album_id, album_audio_id, quality, None)
            .await
    }

    pub async fn song_url_with_hints(
        &self,
        hash: &str,
        album_id: &str,
        album_audio_id: Option<&str>,
        quality: &str,
        hints: Option<&Value>,
    ) -> Result<Value, String> {
        let file_hash = hash.trim();
        if file_hash.is_empty() {
            return Ok(
                json!({ "provider": "kugou", "url": "", "playable": false, "error": "MISSING_HASH", "message": "缺少酷狗歌曲 hash" }),
            );
        }
        let cookie = self.get_cookie();
        let auth = extract_kugou_auth(&cookie);
        let requested_quality = normalize_quality_preference(quality);
        let album_audio_num: u64 = album_audio_id
            .unwrap_or("")
            .trim()
            .parse::<u64>()
            .unwrap_or(0);

        let now = unix_millis();
        let vip_probe = if auth.playback_ready {
            self.fetch_vip_info(&auth).await
        } else {
            None
        };
        let membership =
            normalize_kugou_vip_payload_v2(vip_probe.as_ref(), &auth.userid, now);
        let rights = kugou_membership_rights(&membership, auth.playback_ready, now);

        let can_vip = rights.get("canPlayVipTracks").and_then(|v| v.as_bool()) == Some(true);
        let can_svip = rights.get("canPlaySvipTracks").and_then(|v| v.as_bool()) == Some(true);
        let can_pkg =
            rights.get("canPlayMusicPackageTracks").and_then(|v| v.as_bool()) == Some(true);
        let playback_is_vip = can_vip || can_pkg;

        let member_track = hints.is_some_and(|h| {
            let fee = h.get("fee").and_then(value_as_f64).unwrap_or(0.0) as u64;
            let priv_num = h
                .get("privilege")
                .or_else(|| h.get("Privilege"))
                .and_then(value_as_f64)
                .unwrap_or(0.0) as u64;
            let truthy_k = |k: &str| {
                h.get(k).is_some_and(|v| {
                    v == &Value::Bool(true)
                        || v.as_u64() == Some(1)
                        || v.as_str().is_some_and(|s| {
                            matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "vip" | "svip")
                        })
                })
            };
            truthy_k("vipRequired")
                || truthy_k("needVip")
                || truthy_k("onlyVipPlayable")
                || fee > 0
                || priv_num >= 9
        });

        if member_track && !playback_is_vip {
            let known = membership.get("membershipKnown").and_then(|v| v.as_bool()) == Some(true);
            let verified =
                membership.get("membershipVerified").and_then(|v| v.as_bool()) == Some(true);
            let stale = membership.get("membershipStale").and_then(|v| v.as_bool()) == Some(true);
            let pending = auth.playback_ready && (!known || !verified || stale);
            let category = if pending {
                "membership_unknown"
            } else if auth.playback_ready {
                "vip_required"
            } else {
                "login_required"
            };
            let message = if pending {
                "酷狗会员权益暂未完成同步，请稍后重试或重新登录"
            } else if auth.playback_ready {
                "该酷狗歌曲需要有效会员或已购买权限"
            } else {
                "该酷狗歌曲需要先登录并验证播放权益"
            };
            return Ok(self.attach_playback_status(
                json!({
                    "provider": "kugou",
                    "url": "",
                    "playable": false,
                    "reason": category,
                    "message": message,
                    "restriction": { "category": category, "message": message },
                    "requestedQuality": requested_quality,
                    "hash": file_hash,
                }),
                &auth,
                &membership,
                &rights,
            ));
        }

        let effective_quality = kugou_effective_quality(requested_quality, &rights);
        let scope_tier = if can_svip {
            "svip"
        } else if can_vip {
            "vip"
        } else if can_pkg {
            "music-package"
        } else {
            "none"
        };
        let scope_hash = hex::encode(Sha256::digest(
            format!(
                "{}|{}|{}|{}",
                if auth.userid.is_empty() { "guest" } else { &auth.userid },
                auth.token,
                auth.mid,
                scope_tier
            )
            .as_bytes(),
        ));
        let cache_key = format!(
            "{}:{}:{}:{}:{}",
            &scope_hash[..20],
            file_hash.to_ascii_lowercase(),
            album_id,
            album_audio_num,
            effective_quality
        );
        if let Some(cached) = self.song_url_cache.get(&cache_key) {
            return Ok(self.attach_playback_status(cached, &auth, &membership, &rights));
        }

        let start_idx = KUGOU_QUALITY_CHAIN
            .iter()
            .position(|c| c.key == effective_quality)
            .unwrap_or(KUGOU_QUALITY_CHAIN.len() - 1);
        let mut candidates: Vec<(String, &'static str, &'static str)> = Vec::new();
        let mut seen_hashes = HashSet::new();
        for item in &KUGOU_QUALITY_CHAIN[start_idx..] {
            let cand_hash = match item.field {
                "ResFileHash" => hints
                    .and_then(|h| h.get("resHash").or_else(|| h.get("res_hash")))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "SQFileHash" => hints
                    .and_then(|h| h.get("sqHash").or_else(|| h.get("sq_hash")))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "HQFileHash" => hints
                    .and_then(|h| h.get("hqHash").or_else(|| h.get("hq_hash")))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                _ => file_hash,
            };
            if !cand_hash.is_empty() && seen_hashes.insert(cand_hash.to_string()) {
                candidates.push((cand_hash.to_string(), item.key, item.label));
            }
        }
        if seen_hashes.insert(file_hash.to_string()) {
            candidates.push((file_hash.to_string(), "standard", "标准"));
        }

        let deadline = unix_millis() + KUGOU_PLAY_BUDGET_MS;
        let mut last_restriction: Option<(String, String)> = None;
        let mut trial_candidate: Option<Value> = None;

        let accept_payload = |payload: &Value| -> bool {
            let resolved_level = normalize_quality_preference(
                payload
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("standard"),
            );
            if !can_vip && resolved_level != "standard" {
                return false;
            }
            if can_vip && !can_svip && (resolved_level == "jymaster" || resolved_level == "hires") {
                return false;
            }
            true
        };

        for (cand_hash, cand_level, cand_label) in &candidates {
            for attempt_idx in 0..5 {
                let rem = deadline.saturating_sub(unix_millis());
                if rem < 250 {
                    break;
                }
                let timeout = rem.min(KUGOU_PLAY_ATTEMPT_MS);
                let res = match attempt_idx {
                    0 => {
                        self.play_via_h5(
                            cand_hash,
                            album_id,
                            album_audio_num,
                            &auth,
                            cand_level,
                            playback_is_vip,
                            timeout,
                        )
                        .await
                    }
                    1 => {
                        self.play_via_web(
                            cand_hash,
                            album_id,
                            album_audio_num,
                            &auth,
                            timeout,
                            false,
                        )
                        .await
                    }
                    2 => {
                        self.play_via_mobile(cand_hash, album_id, &auth, playback_is_vip, timeout)
                            .await
                    }
                    3 => {
                        self.play_via_gateway(
                            cand_hash,
                            album_id,
                            album_audio_num,
                            &auth,
                            cand_level,
                            playback_is_vip,
                            timeout,
                        )
                        .await
                    }
                    _ => {
                        self.play_via_web(
                            cand_hash,
                            album_id,
                            album_audio_num,
                            &auth,
                            timeout,
                            true,
                        )
                        .await
                    }
                };

                let Ok(attempt) = res else {
                    if last_restriction.is_none() {
                        last_restriction = Some((
                            "url_unavailable".into(),
                            "酷狗播放接口暂时不可用，请稍后重试".into(),
                        ));
                    }
                    continue;
                };

                if !attempt.url.is_empty() {
                    let level = if attempt.level.is_empty() {
                        *cand_level
                    } else {
                        &attempt.level
                    };
                    let resolved_lvl = normalize_quality_preference(level);
                    let payload = json!({
                        "provider": "kugou",
                        "url": attempt.url,
                        "playable": true,
                        "trial": attempt.trial,
                        "level": level,
                        "quality": if attempt.quality.is_empty() { *cand_label } else { &attempt.quality },
                        "requestedQuality": requested_quality,
                        "effectiveQuality": effective_quality,
                        "qualityDowngraded": requested_quality != resolved_lvl,
                        "hash": cand_hash,
                        "playbackSource": attempt.source,
                    });
                    if attempt.trial {
                        if trial_candidate.is_none() {
                            trial_candidate = Some(payload);
                        }
                        continue;
                    }
                    if accept_payload(&payload) {
                        self.song_url_cache.set(cache_key, payload.clone(), None);
                        return Ok(self.attach_playback_status(
                            payload,
                            &auth,
                            &membership,
                            &rights,
                        ));
                    }
                    last_restriction = Some((
                        "vip_required".into(),
                        "当前酷狗账号没有该音质的播放权限".into(),
                    ));
                } else if !attempt.restricted_category.is_empty()
                    && (last_restriction.is_none()
                        || attempt.restricted_category != "url_unavailable")
                {
                    last_restriction = Some((
                        attempt.restricted_category,
                        attempt.restricted_message,
                    ));
                }
            }
            if unix_millis() >= deadline {
                break;
            }
        }

        if let Some(trial) = trial_candidate {
            if accept_payload(&trial) {
                self.song_url_cache.set(cache_key, trial.clone(), None);
                return Ok(self.attach_playback_status(trial, &auth, &membership, &rights));
            }
        }

        let (cat, msg) = last_restriction.unwrap_or_else(|| {
            (
                "url_unavailable".into(),
                "酷狗暂未返回播放地址，请稍后重试".into(),
            )
        });
        Ok(self.attach_playback_status(
            json!({
                "provider": "kugou",
                "url": "",
                "playable": false,
                "reason": cat,
                "message": msg,
                "restriction": { "category": cat, "message": msg },
                "requestedQuality": requested_quality,
                "effectiveQuality": effective_quality,
                "qualityDowngraded": requested_quality != effective_quality,
                "hash": file_hash,
            }),
            &auth,
            &membership,
            &rights,
        ))
    }

    // ---------- 歌词 ----------

    pub async fn lyric(
        &self,
        hash: &str,
        keyword: Option<&str>,
        duration_sec: u64,
    ) -> Result<Value, String> {
        let file_hash = hash.trim();
        if file_hash.is_empty() {
            return Ok(json!({ "provider": "kugou", "error": "Missing Kugou hash", "lyric": "" }));
        }
        let mut search_url = reqwest::Url::parse(KUGOU_LYRIC_SEARCH).unwrap();
        {
            let mut pairs = search_url.query_pairs_mut();
            pairs.append_pair("ver", "1");
            pairs.append_pair("man", "yes");
            pairs.append_pair("client", "pc");
            pairs.append_pair("keyword", keyword.unwrap_or(""));
            pairs.append_pair("duration", &duration_sec.to_string());
            pairs.append_pair("hash", file_hash);
        }
        let search_resp: Value = self
            .client
            .get(search_url)
            .timeout(Duration::from_secs(8))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .send()
            .await
            .map_err(|e| format!("Kugou lyric search failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Kugou lyric search JSON: {e}"))?;

        let candidate = search_resp
            .get("candidates")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());
        let Some(cand) = candidate else {
            return Ok(json!({ "provider": "kugou", "hash": file_hash, "lyric": "", "trans": "" }));
        };
        let cand_id = value_to_string(cand.get("id"));
        if cand_id.is_empty() {
            return Ok(json!({ "provider": "kugou", "hash": file_hash, "lyric": "", "trans": "" }));
        }
        let accesskey = cand.get("accesskey").and_then(|v| v.as_str()).unwrap_or("");

        let mut dl_url = reqwest::Url::parse(KUGOU_LYRIC_DOWNLOAD).unwrap();
        dl_url
            .query_pairs_mut()
            .append_pair("ver", "1")
            .append_pair("client", "pc")
            .append_pair("id", &cand_id)
            .append_pair("accesskey", accesskey)
            .append_pair("fmt", "lrc")
            .append_pair("charset", "utf8");

        let dl_resp: Value = self
            .client
            .get(dl_url)
            .timeout(Duration::from_secs(8))
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, KUGOU_REFERER)
            .send()
            .await
            .map_err(|e| format!("Kugou lyric download failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Kugou lyric download JSON: {e}"))?;

        let content_raw = dl_resp
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let lyric = if let Ok(bytes) = BASE64_STD.decode(content_raw) {
            if let Ok(decoded) = String::from_utf8(bytes) {
                let clean = decoded.trim_start_matches('\u{FEFF}');
                if clean.contains('[') || clean.chars().any(|c| ('\u{4E00}'..='\u{9FA5}').contains(&c)) {
                    clean.to_string()
                } else {
                    content_raw.to_string()
                }
            } else {
                content_raw.to_string()
            }
        } else {
            content_raw.to_string()
        };

        Ok(json!({
            "provider": "kugou",
            "hash": file_hash,
            "lyric": lyric,
            "trans": "",
        }))
    }

    // ---------- 猜你喜欢 / 歌单 / 红心 ----------

    pub async fn recommendations(&self, limit: usize) -> Result<Value, String> {
        let lim = limit.clamp(1, 20);
        let auth = extract_kugou_auth(&self.get_cookie());
        if !auth.playback_ready {
            return Ok(
                json!({ "provider": "kugou", "loggedIn": false, "songs": [], "error": "KUGOU_AUTH_REQUIRED" }),
            );
        }
        let clienttime = unix_millis();
        let payload = json!({
            "appid": KUGOU_APPID,
            "area_code": 1,
            "clienttime": clienttime,
            "clientver": KUGOU_CLIENTVER,
            "data": [{ "fmid": "0", "fmtype": 2, "offset": -1, "size": lim, "singername": "" }],
            "get_tracker": 1,
            "key": kugou_sign_params_key(clienttime),
            "mid": auth.mid,
            "uid": auth.userid.parse::<u64>().unwrap_or(0),
        });
        let body_str = serde_json::to_string(&payload).unwrap_or_default();
        match self
            .android_gateway_request(
                None,
                "/v1/app_song_list_offset",
                "POST",
                Some("fm.service.kugou.com"),
                &[],
                Some(&body_str),
                8000,
            )
            .await
        {
            Ok(json_resp) => {
                let data = json_resp.get("data");
                let raw_list = [
                    data.and_then(|d| d.get("info")),
                    data.and_then(|d| d.get("song_list")),
                    data.and_then(|d| d.get("songs")),
                    data.and_then(|d| d.get("list")),
                    data.and_then(|d| d.get("songlist")),
                    json_resp.get("info"),
                    json_resp.get("list"),
                ]
                .into_iter()
                .flatten()
                .find_map(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

                let songs: Vec<Value> = raw_list
                    .iter()
                    .map(map_kugou_playlist_track)
                    .filter(|s| {
                        !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                            && (!s.get("hash").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                                || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
                    })
                    .take(lim)
                    .collect();
                Ok(json!({
                    "provider": "kugou",
                    "loggedIn": true,
                    "songs": songs,
                    "updatedAt": unix_millis(),
                }))
            }
            Err(_) => Ok(json!({
                "provider": "kugou",
                "loggedIn": true,
                "songs": [],
                "error": "KUGOU_GUESS_EMPTY",
                "updatedAt": unix_millis(),
            })),
        }
    }

    pub async fn user_playlists(&self) -> Result<Value, String> {
        let auth = extract_kugou_auth(&self.get_cookie());
        if !auth.playback_ready {
            return Ok(json!({
                "provider": "kugou",
                "loggedIn": auth.logged_in,
                "playbackReady": false,
                "playlists": [],
                "error": "KUGOU_AUTH_REQUIRED",
                "message": "酷狗登录未完成，请重新网页登录",
            }));
        }
        let json_resp = self
            .h5_gateway_request(
                "/v7/get_all_list",
                "POST",
                Some("cloudlist.service.kugou.com"),
                &[("plat", "1".into())],
                Some(&json!({
                    "userid": auth.userid.parse::<u64>().unwrap_or(0),
                    "token": auth.token,
                    "total_ver": 979,
                    "type": 2,
                    "page": 1,
                    "pagesize": 50,
                })),
                8000,
            )
            .await;

        let Ok(resp) = json_resp else {
            return Ok(json!({
                "provider": "kugou",
                "loggedIn": true,
                "playbackReady": true,
                "libraryReady": false,
                "playlists": [],
                "error": "KUGOU_PLAYLIST_FAILED",
                "message": "酷狗歌单加载失败，请稍后重试",
            }));
        };
        let lists = extract_kugou_gateway_playlist_lists(&resp);
        let profile = pick_kugou_profile_from_lists(&lists, &auth);
        let playlists: Vec<Value> = lists
            .iter()
            .map(map_kugou_playlist_item)
            .filter(|pl| {
                !pl.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && !pl
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty()
            })
            .collect();
        Ok(json!({
            "provider": "kugou",
            "loggedIn": true,
            "playbackReady": true,
            "libraryReady": true,
            "userId": auth.userid,
            "nickname": if !auth.nickname.is_empty() { auth.nickname } else { profile.get("nickname").and_then(|v| v.as_str()).unwrap_or("").to_string() },
            "avatar": if !auth.avatar.is_empty() { auth.avatar } else { profile.get("avatar").and_then(|v| v.as_str()).unwrap_or("").to_string() },
            "playlists": playlists,
        }))
    }

    async fn fetch_playlist_tracks_page(
        &self,
        list_id: &str,
        auth: &KugouAuth,
        page_no: usize,
        pagesize: usize,
        base_offset: usize,
    ) -> Result<(Vec<Value>, usize), String> {
        let list_id_val: Value = list_id
            .parse::<u64>()
            .map(|n| json!(n))
            .unwrap_or_else(|_| json!(list_id));
        let json_resp = self
            .h5_gateway_request(
                "/v4/get_list_all_file",
                "POST",
                Some("cloudlist.service.kugou.com"),
                &[("plat", "1".into())],
                Some(&json!({
                    "listid": list_id_val,
                    "userid": auth.userid.parse::<u64>().unwrap_or(0),
                    "area_code": 1,
                    "show_relate_goods": 0,
                    "pagesize": pagesize,
                    "allplatform": 1,
                    "show_cover": 1,
                    "type": 0,
                    "token": auth.token,
                    "page": page_no,
                })),
                8000,
            )
            .await?;

        let empty = json!({});
        let data = json_resp.get("data").unwrap_or(&empty);
        let raw_list = data
            .get("info")
            .or_else(|| data.get("songs"))
            .or_else(|| data.get("lists"))
            .or_else(|| data.get("file"))
            .and_then(|v| v.as_array().or_else(|| v.get("file").and_then(|f| f.as_array())))
            .cloned()
            .unwrap_or_default();

        let tracks: Vec<Value> = raw_list
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let mut mapped = map_kugou_playlist_track(item);
                let added_at = item
                    .get("addtime")
                    .or_else(|| item.get("add_time"))
                    .or_else(|| item.get("collect_time"))
                    .or_else(|| item.get("ctime"))
                    .or_else(|| item.get("belong_cd_addtime"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(obj) = mapped.as_object_mut() {
                    obj.insert("addedAt".into(), json!(added_at));
                    obj.insert("playlistIndex".into(), json!(base_offset + idx));
                }
                mapped
            })
            .filter(|s| {
                !s.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && (!s.get("hash").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                        || !s.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            })
            .collect();
        let total = data
            .get("count")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .filter(|&n| n > 0)
            .unwrap_or(tracks.len());
        Ok((tracks, total))
    }

    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        limit: usize,
        offset: usize,
        paged: bool,
    ) -> Result<Value, String> {
        let auth = extract_kugou_auth(&self.get_cookie());
        if !auth.playback_ready {
            return Ok(
                json!({ "provider": "kugou", "tracks": [], "total": 0, "error": "KUGOU_AUTH_REQUIRED", "message": "酷狗登录未完成" }),
            );
        }
        let listid = parse_kugou_list_id(playlist_id);
        if listid.is_empty() {
            return Ok(
                json!({ "provider": "kugou", "tracks": [], "total": 0, "error": "MISSING_PLAYLIST_ID" }),
            );
        }
        let pagesize = limit.clamp(1, 50);

        if paged {
            let (probe_tracks, probe_total) = self
                .fetch_playlist_tracks_page(&listid, &auth, 1, pagesize, 0)
                .await?;
            let total = if probe_total > 0 {
                probe_total
            } else {
                probe_tracks.len()
            };
            if total == 0 {
                let mut rev = probe_tracks;
                rev.reverse();
                return Ok(json!({
                    "provider": "kugou",
                    "id": playlist_id,
                    "tracks": rev,
                    "total": 0,
                    "offset": offset,
                    "limit": pagesize,
                    "hasMore": false,
                }));
            }
            let orig_start = total.saturating_sub(offset + pagesize);
            let orig_end = total.saturating_sub(offset);
            let count = pagesize.min(orig_end.saturating_sub(orig_start));
            if count == 0 {
                return Ok(json!({
                    "provider": "kugou",
                    "id": playlist_id,
                    "tracks": [],
                    "total": total,
                    "offset": offset,
                    "limit": pagesize,
                    "hasMore": false,
                }));
            }
            let mut out = Vec::new();
            let mut remaining = count;
            let mut pos = orig_start;
            while remaining > 0 {
                let page_no = pos / pagesize + 1;
                let base_off = (page_no - 1) * pagesize;
                let (chunk, _) = self
                    .fetch_playlist_tracks_page(&listid, &auth, page_no, pagesize, base_off)
                    .await?;
                if chunk.is_empty() {
                    break;
                }
                let in_page = pos - base_off;
                let take = remaining.min(chunk.len().saturating_sub(in_page));
                if take == 0 {
                    break;
                }
                out.extend(chunk.into_iter().skip(in_page).take(take));
                remaining -= take;
                pos += take;
            }
            out.reverse();
            for (i, t) in out.iter_mut().enumerate() {
                if let Some(obj) = t.as_object_mut() {
                    obj.insert("playlistIndex".into(), json!(offset + i));
                }
            }
            let has_more = offset + out.len() < total;
            return Ok(json!({
                "provider": "kugou",
                "id": playlist_id,
                "tracks": out,
                "total": total,
                "offset": offset,
                "limit": pagesize,
                "hasMore": has_more,
            }));
        }

        let cache_key = format!("{listid}:{}", auth.userid);
        if let Some(cached) = self.playlist_tracks_cache.get(&cache_key) {
            return Ok(cached);
        }
        let mut tracks = Vec::new();
        let mut total = 0usize;
        for round in 0..500 {
            let (chunk, chunk_total) = self
                .fetch_playlist_tracks_page(&listid, &auth, round + 1, pagesize, tracks.len())
                .await?;
            if chunk_total > 0 {
                total = chunk_total;
            }
            if chunk.is_empty() {
                break;
            }
            let chunk_len = chunk.len();
            tracks.extend(chunk);
            if chunk_len < pagesize || (total > 0 && tracks.len() >= total) {
                break;
            }
        }
        tracks.reverse();
        for (i, t) in tracks.iter_mut().enumerate() {
            if let Some(obj) = t.as_object_mut() {
                obj.insert("playlistIndex".into(), json!(i));
            }
        }
        let final_total = if total > 0 { total } else { tracks.len() };
        let res = json!({
            "provider": "kugou",
            "id": playlist_id,
            "tracks": tracks,
            "total": final_total,
        });
        self.playlist_tracks_cache.set(cache_key, res.clone(), None);
        Ok(res)
    }

    async fn resolve_favorite_list_id(&self, auth: &KugouAuth) -> String {
        if !auth.playback_ready {
            return String::new();
        }
        let now = unix_millis();
        if let Ok(g) = self.favorite_list_cache.lock() {
            if !g.0.is_empty() && g.1 == auth.userid && now.saturating_sub(g.2) < 300_000 {
                return g.0.clone();
            }
        }
        let Ok(json_resp) = self
            .h5_gateway_request(
                "/v7/get_all_list",
                "POST",
                Some("cloudlist.service.kugou.com"),
                &[("plat", "1".into())],
                Some(&json!({
                    "userid": auth.userid.parse::<u64>().unwrap_or(0),
                    "token": auth.token,
                    "total_ver": 979,
                    "type": 2,
                    "page": 1,
                    "pagesize": 50,
                })),
                8000,
            )
            .await
        else {
            return String::new();
        };
        let lists = extract_kugou_gateway_playlist_lists(&json_resp);
        let fav = pick_kugou_favorite_playlist(&lists);
        let list_id = fav.map(resolve_kugou_list_id_from_item).unwrap_or_default();
        if !list_id.is_empty() {
            if let Ok(mut g) = self.favorite_list_cache.lock() {
                *g = (list_id.clone(), auth.userid.clone(), now);
            }
        }
        list_id
    }

    /// 批量检查歌曲是否在酷狗“我喜欢”歌单中。
    pub async fn like_check(&self, hashes: &[String]) -> Result<Value, String> {
        let auth = extract_kugou_auth(&self.get_cookie());
        if !auth.playback_ready {
            return Ok(json!({ "provider": "kugou", "liked": {}, "error": "KUGOU_AUTH_REQUIRED" }));
        }
        let hash_set: HashSet<String> = hashes
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if hash_set.is_empty() {
            return Ok(json!({ "provider": "kugou", "liked": {} }));
        }
        let list_id = self.resolve_favorite_list_id(&auth).await;
        if list_id.is_empty() {
            return Ok(json!({ "provider": "kugou", "liked": {}, "listId": "" }));
        }

        let mut liked = serde_json::Map::new();
        let first = self.playlist_tracks(&list_id, 50, 0, true).await?;
        let total = first
            .get("total")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let first_len = first
            .get("tracks")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let total_pages = (total.max(first_len).max(1) + 49) / 50;

        let mut page_order = Vec::new();
        for p in (1..=total_pages).rev() {
            if page_order.len() >= 6 {
                break;
            }
            page_order.push(p);
        }
        for p in 1..=total_pages {
            if page_order.len() >= 12 {
                break;
            }
            if !page_order.contains(&p) {
                page_order.push(p);
            }
        }

        for page in page_order {
            let chunk = if page == 1 && first_len > 0 {
                first.clone()
            } else {
                self.playlist_tracks(&list_id, 50, (page - 1) * 50, true)
                    .await
                    .unwrap_or_else(|_| json!({ "tracks": [] }))
            };
            let tracks = chunk
                .get("tracks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for t in &tracks {
                let h = t
                    .get("hash")
                    .or_else(|| t.get("fileHash"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if h.is_empty() || !hash_set.contains(&h) {
                    continue;
                }
                liked.insert(h.clone(), json!(true));
                if let Some(fid) = t.get("fileId").and_then(|v| v.as_str()) {
                    if let Ok(mut g) = self.like_file_id_by_hash.lock() {
                        g.insert(h, fid.to_string());
                    }
                }
            }
            if liked.len() >= hash_set.len() {
                break;
            }
        }

        Ok(json!({
            "provider": "kugou",
            "liked": Value::Object(liked),
            "listId": list_id,
        }))
    }

    /// 添加或取消喜欢酷狗歌曲。
    pub async fn like_toggle(&self, song: &Value, like: bool) -> Result<Value, String> {
        if like {
            self.playlist_add_song("", song).await
        } else {
            let auth = extract_kugou_auth(&self.get_cookie());
            if !auth.playback_ready {
                return Ok(
                    json!({ "provider": "kugou", "success": false, "error": "KUGOU_AUTH_REQUIRED" }),
                );
            }
            let target_list_id = self.resolve_favorite_list_id(&auth).await;
            if target_list_id.is_empty() {
                return Ok(
                    json!({ "provider": "kugou", "success": false, "error": "KUGOU_FAVORITE_LIST_NOT_FOUND" }),
                );
            }
            let hash = value_to_string(
                song.get("hash")
                    .or_else(|| song.get("fileHash"))
                    .or_else(|| song.get("id")),
            )
            .trim()
            .to_ascii_lowercase();
            let mut file_id = self
                .like_file_id_by_hash
                .lock()
                .ok()
                .and_then(|g| g.get(&hash).cloned())
                .unwrap_or_default();
            if file_id.is_empty() && !hash.is_empty() {
                for page in 1..=6 {
                    let chunk = self
                        .playlist_tracks(&target_list_id, 50, (page - 1) * 50, true)
                        .await?;
                    let tracks = chunk
                        .get("tracks")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for t in &tracks {
                        let th = t
                            .get("hash")
                            .or_else(|| t.get("fileHash"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if th == hash {
                            if let Some(fid) = t.get("fileId").and_then(|v| v.as_str()) {
                                file_id = fid.to_string();
                                if let Ok(mut g) = self.like_file_id_by_hash.lock() {
                                    g.insert(hash.clone(), file_id.clone());
                                }
                                break;
                            }
                        }
                    }
                    if !file_id.is_empty() || tracks.len() < 50 {
                        break;
                    }
                }
            }
            if file_id.is_empty() {
                return Ok(
                    json!({ "provider": "kugou", "success": false, "error": "KUGOU_SONG_NOT_IN_LIST" }),
                );
            }
            let listid_val: Value = target_list_id
                .parse::<u64>()
                .map(|n| json!(n))
                .unwrap_or_else(|_| json!(target_list_id));
            let fileid_val: Value = file_id
                .parse::<u64>()
                .map(|n| json!(n))
                .unwrap_or_else(|_| json!(file_id));
            let json_resp = self
                .h5_gateway_request(
                    "/v4/delete_songs",
                    "POST",
                    Some("cloudlist.service.kugou.com"),
                    &[],
                    Some(&json!({
                        "listid": listid_val,
                        "userid": auth.userid.parse::<u64>().unwrap_or(0),
                        "token": auth.token,
                        "type": 0,
                        "list_ver": 0,
                        "data": [{ "fileid": fileid_val }],
                    })),
                    8000,
                )
                .await?;
            if !hash.is_empty() {
                if let Ok(mut g) = self.like_file_id_by_hash.lock() {
                    g.remove(&hash);
                }
            }
            Ok(json!({
                "provider": "kugou",
                "success": true,
                "liked": false,
                "listId": target_list_id,
                "body": json_resp,
            }))
        }
    }

    /// 向指定歌单（空字符串则默认“我喜欢”歌单）添加歌曲。
    pub async fn playlist_add_song(&self, list_id: &str, song: &Value) -> Result<Value, String> {
        let auth = extract_kugou_auth(&self.get_cookie());
        if !auth.playback_ready {
            return Ok(
                json!({ "provider": "kugou", "success": false, "error": "KUGOU_AUTH_REQUIRED" }),
            );
        }
        let target_list_id = if !list_id.trim().is_empty() {
            list_id.trim().to_string()
        } else {
            self.resolve_favorite_list_id(&auth).await
        };
        if target_list_id.is_empty() {
            return Ok(
                json!({ "provider": "kugou", "success": false, "error": "KUGOU_FAVORITE_LIST_NOT_FOUND" }),
            );
        }
        let hash = value_to_string(
            song.get("hash")
                .or_else(|| song.get("fileHash"))
                .or_else(|| song.get("id")),
        )
        .trim()
        .to_ascii_lowercase();
        let name = song
            .get("name")
            .or_else(|| song.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let album_id = value_to_string(song.get("albumId").or_else(|| song.get("album_id")))
            .parse::<u64>()
            .unwrap_or(0);
        let mixsongid = value_to_string(
            song.get("mixSongId")
                .or_else(|| song.get("mixsongid"))
                .or_else(|| song.get("albumAudioId"))
                .or_else(|| song.get("album_audio_id")),
        )
        .parse::<u64>()
        .unwrap_or(0);
        let dur_ms = song
            .get("duration")
            .and_then(value_as_f64)
            .unwrap_or(0.0);
        let timelen = if dur_ms > 1000.0 {
            dur_ms.round() as u64
        } else {
            (dur_ms * 1000.0).round() as u64
        };
        let listid_val: Value = target_list_id
            .parse::<u64>()
            .map(|n| json!(n))
            .unwrap_or_else(|_| json!(target_list_id));

        let json_resp = self
            .h5_gateway_request(
                "/v6/add_song",
                "POST",
                Some("cloudlist.service.kugou.com"),
                &[
                    ("last_time", unix_secs().to_string()),
                    ("last_area", "gztx".into()),
                    ("userid", auth.userid.clone()),
                    ("token", auth.token.clone()),
                ],
                Some(&json!({
                    "userid": auth.userid.parse::<u64>().unwrap_or(0),
                    "token": auth.token,
                    "listid": listid_val,
                    "list_ver": 0,
                    "type": 0,
                    "slow_upload": 1,
                    "scene": "false;null",
                    "data": [{
                        "number": 1,
                        "name": name,
                        "hash": hash,
                        "size": 0,
                        "sort": 0,
                        "timelen": timelen,
                        "bitrate": 0,
                        "album_id": album_id,
                        "mixsongid": mixsongid,
                    }],
                })),
                8000,
            )
            .await?;

        if !hash.is_empty() {
            if let Ok(mut g) = self.like_file_id_by_hash.lock() {
                g.remove(&hash);
            }
        }
        Ok(json!({
            "provider": "kugou",
            "success": true,
            "liked": true,
            "listId": target_list_id,
            "body": json_resp,
        }))
    }
}

fn extract_kugou_gateway_playlist_lists(data: &Value) -> Vec<Value> {
    let root = data.get("data").unwrap_or(data);
    if let Some(arr) = root.get("info").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    let info = root.get("info").unwrap_or(root);
    let mut out = Vec::new();
    for src in [
        info.get("collect"),
        info.get("love"),
        info.get("self"),
        info.get("list"),
        root.get("list"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(arr) = src.as_array() {
            out.extend(arr.clone());
        }
    }
    out
}

fn pick_kugou_profile_from_lists(lists: &[Value], auth: &KugouAuth) -> Value {
    let mut selected: Option<&Value> = None;
    for item in lists {
        if !item.is_object() {
            continue;
        }
        let item_uid: String = value_to_string(
            item.get("list_create_userid")
                .or_else(|| item.get("userid"))
                .or_else(|| item.get("user_id"))
                .or_else(|| item.get("owner_id")),
        )
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
        if !auth.userid.is_empty() && item_uid == auth.userid {
            selected = Some(item);
            break;
        }
        if selected.is_none() {
            selected = Some(item);
        }
    }
    let Some(sel) = selected else {
        return json!({});
    };
    let nick = strip_kugou_html(
        sel.get("nickname")
            .or_else(|| sel.get("username"))
            .or_else(|| sel.get("user_name"))
            .or_else(|| sel.get("list_create_username"))
            .or_else(|| sel.get("owner_name"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let avatar = kugou_cover_url(
        sel.get("create_user_pic")
            .or_else(|| sel.get("user_pic"))
            .or_else(|| sel.get("avatar"))
            .or_else(|| sel.get("pic"))
            .or_else(|| sel.get("img"))
            .or_else(|| sel.get("imgurl"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        120,
    );
    json!({ "nickname": nick, "avatar": avatar })
}

fn map_kugou_playlist_item(item: &Value) -> Value {
    let id = value_to_string(
        item.get("global_collection_id")
            .or_else(|| item.get("specialid"))
            .or_else(|| item.get("listid"))
            .or_else(|| item.get("list_id"))
            .or_else(|| item.get("id")),
    );
    let list_id = {
        let direct = value_to_string(item.get("list_create_listid").or_else(|| item.get("listid")));
        if !direct.is_empty() {
            direct
        } else {
            parse_kugou_list_id(&id)
        }
    };
    let final_id = if !id.is_empty() { id } else { list_id.clone() };
    let name = strip_kugou_html(
        item.get("name")
            .or_else(|| item.get("listname"))
            .or_else(|| item.get("specialname"))
            .or_else(|| item.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("酷狗歌单"),
    );
    let cover_raw = item
        .get("pic")
        .or_else(|| item.get("img"))
        .or_else(|| item.get("imgurl"))
        .or_else(|| item.get("sizable_cover"))
        .or_else(|| item.get("create_user_pic"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let track_count = item
        .get("count")
        .or_else(|| item.get("m_count"))
        .or_else(|| item.get("song_count"))
        .or_else(|| item.get("total"))
        .or_else(|| item.get("list_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let creator = strip_kugou_html(
        item.get("nickname")
            .or_else(|| item.get("username"))
            .or_else(|| item.get("user_name"))
            .or_else(|| item.get("list_create_username"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    json!({
        "provider": "kugou",
        "source": "kugou",
        "id": final_id,
        "listId": list_id,
        "name": name,
        "cover": kugou_cover_url(cover_raw, 240),
        "trackCount": track_count,
        "creator": creator,
    })
}

fn pick_kugou_favorite_playlist(lists: &[Value]) -> Option<&Value> {
    let display_name = |item: &Value| {
        item.get("name")
            .or_else(|| item.get("listname"))
            .or_else(|| item.get("specialname"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let is_primary = |name: &str| {
        let lower = name.to_ascii_lowercase();
        name.contains("我喜欢")
            || lower.contains("liked music")
            || lower.contains("my favorite")
    };
    let is_fav = |name: &str| {
        let lower = name.to_ascii_lowercase();
        name.contains("我喜欢")
            || name.contains("我的收藏")
            || lower.contains("favorite")
            || lower.contains("liked")
    };
    lists
        .iter()
        .find(|i| is_primary(&display_name(i)))
        .or_else(|| lists.iter().find(|i| is_fav(&display_name(i))))
        .or_else(|| {
            lists.iter().find(|i| {
                i.get("is_default").and_then(|v| v.as_u64()) == Some(1)
                    || i.get("default").and_then(|v| v.as_u64()) == Some(1)
            })
        })
}

fn resolve_kugou_list_id_from_item(item: &Value) -> String {
    let direct = value_to_string(
        item.get("list_create_listid")
            .or_else(|| item.get("listid"))
            .or_else(|| item.get("list_id")),
    );
    if !direct.is_empty() {
        return direct;
    }
    let global = value_to_string(item.get("global_collection_id"));
    let parsed = parse_kugou_list_id(&global);
    if !parsed.is_empty() {
        parsed
    } else {
        value_to_string(item.get("id"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_h5_and_sign_key_signatures_match_js_contract() {
        let mut h5_params = BTreeMap::new();
        h5_params.insert("appid".into(), "1014".into());
        h5_params.insert("hash".into(), "abcdef".into());
        let h5_sig = signature_h5_params(&h5_params, Some(r#"{"a":1}"#));
        let expected_h5 = md5_hex(&format!(
            "{KUGOU_H5_SALT}appid=1014hash=abcdef{{\"a\":1}}{KUGOU_H5_SALT}"
        ));
        assert_eq!(h5_sig, expected_h5);

        let android_sig = signature_android_params(&h5_params, "body");
        let expected_android = md5_hex(&format!(
            "{KUGOU_ANDROID_SALT}appid=1014hash=abcdefbody{KUGOU_ANDROID_SALT}"
        ));
        assert_eq!(android_sig, expected_android);

        let key = sign_key("deadbeef", "mid123", "456", 1014);
        assert_eq!(
            key,
            md5_hex(&format!("deadbeef{KUGOU_SIGN_KEY_SALT}1014mid123456"))
        );
        assert_eq!(kugou_cloud_key("abc"), md5_hex("abckgcloud"));
    }

    #[test]
    fn extract_kugou_auth_parses_compound_cookie_and_playback_readiness() {
        let cookie = "KuGoo=KugooID%3D889900%26t%3Dtok12345%26NickName%3D%25E9%2585%25B7%25E7%258B%2597%26isVIP%3D1; kg_mid=mymid";
        assert!(kugou_cookie_has_playback(cookie));
        let auth = extract_kugou_auth(cookie);
        assert_eq!(auth.userid, "889900");
        assert_eq!(auth.token, "tok12345");
        assert_eq!(auth.mid, "mymid");
        assert_eq!(auth.nickname, "酷狗");
        assert!(auth.logged_in);
        assert!(auth.playback_ready);

        // Missing token cannot play
        assert!(!kugou_cookie_has_playback("userid=889900; kg_mid=abc"));
    }

    #[test]
    fn kugou_web_roleinfo_vip_and_quality_rights_enforce_tiers() {
        let now = 1_750_000_000_000u64;
        let future_sec = (now / 1000) + 86400;
        let svip_payload = normalize_kugou_web_role_info_payload(
            Some(&json!({ "status": 1, "data": { "userid": 1001, "role": 11, "vip_end_time": future_sec } })),
            "1001",
        );
        let svip_parsed = normalize_kugou_vip_payload_v2(Some(&svip_payload), "1001", now);
        assert_eq!(svip_parsed["isSvip"], true);
        assert_eq!(svip_parsed["vipLevel"], "svip");
        let svip_rights = kugou_membership_rights(&svip_parsed, true, now);
        assert_eq!(svip_rights["canPlaySvipTracks"], true);
        assert_eq!(kugou_effective_quality("jymaster", &svip_rights), "jymaster");

        let vip_payload = normalize_kugou_web_role_info_payload(
            Some(&json!({ "status": 1, "data": { "userid": 1001, "role": 1, "vip_end_time": future_sec } })),
            "1001",
        );
        let vip_parsed = normalize_kugou_vip_payload_v2(Some(&vip_payload), "1001", now);
        let vip_rights = kugou_membership_rights(&vip_parsed, true, now);
        assert_eq!(vip_rights["canPlayVipTracks"], true);
        assert_eq!(vip_rights["canPlaySvipTracks"], false);
        assert_eq!(kugou_effective_quality("hires", &vip_rights), "lossless");
    }

    #[test]
    fn map_kugou_search_and_playlist_track_strip_html_and_map_fields() {
        let item = json!({
            "FileHash": "AABBCCDD",
            "HQFileHash": "HQ11",
            "SQFileHash": "SQ22",
            "AlbumID": 777,
            "MixSongID": 888,
            "SongName": "<em>晴天</em>",
            "SingerName": "周杰伦",
            "AlbumName": "叶惠美",
            "Image": "https://img.kugou.com/{size}/cover.jpg",
            "Duration": 269,
            "Privilege": 10
        });
        let mapped = map_kugou_search_item(&item);
        assert_eq!(mapped["provider"], "kugou");
        assert_eq!(mapped["name"], "晴天");
        assert_eq!(mapped["cover"], "https://img.kugou.com/240/cover.jpg");
        assert_eq!(mapped["duration"], 269000);
        assert_eq!(mapped["fee"], 1);
        assert_eq!(mapped["playable"], false);
        assert_eq!(mapped["sqHash"], "SQ22");
    }
}
