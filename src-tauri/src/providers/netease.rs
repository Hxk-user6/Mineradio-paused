//! 网易云音乐纯 Rust 客户端（WeAPI / EAPI 协议与业务适配）。
//!
//! 对齐原 `server.js` 中基于 `NeteaseCloudMusicApi` 的全部业务接口，
//! 涵盖搜索、音质探测、站内同曲版本匹配、歌词、歌单、专辑、歌手、评论、
//! 红心、扫码登录、VIP 解析、首页发现、播客与听歌上报。

use aes::Aes128;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyInit, KeyIvInit};
use md5::{Digest, Md5};
use num_bigint::BigUint;
use qrcode::{EcLevel, QrCode};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use super::{
    extract_cookies_from_set_cookie_headers, normalize_cookie_header, normalize_quality_preference,
    parse_cookie_string, playback_restriction, probe_playback_audio_url, read_cookie_file,
    serialize_cookie_object, shared_http_client, unix_millis, unix_secs, write_cookie_file, TtlCache,
    DEFAULT_USER_AGENT,
};

type Aes128CbcEnc = cbc::Encryptor<Aes128>;
#[allow(dead_code)]
type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes128EcbEnc = ecb::Encryptor<Aes128>;
type Aes128EcbDec = ecb::Decryptor<Aes128>;

const WEAPI_PRESET_KEY: &[u8; 16] = b"0CoJUm6Qyw8W8jud";
const WEAPI_IV: &[u8; 16] = b"0102030405060708";
const WEAPI_BASE62: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const WEAPI_RSA_MODULUS_HEX: &str = "00e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b725152b3ab17a876aea8a5aa76d2e417629ec4ee341f56135fccf695280104e0312ecbda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932575cce10b424d813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db0a22b8e7";
const WEAPI_RSA_EXPONENT: u32 = 0x10001;
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";

const NETEASE_WEB_BASE: &str = "https://music.163.com";
const NETEASE_EAPI_BASE: &str = "https://interface3.music.163.com";

const NETEASE_DIRECT_RESOLVE_BUDGET_MS: u64 = 4800;
const NETEASE_SOURCE_MATCH_TOTAL_BUDGET_MS: u64 = 8000;
const NETEASE_SOURCE_MATCH_LOOKUP_BUDGET_MS: u64 = 4800;
const NETEASE_SONG_URL_TOTAL_BUDGET_MS: u64 = 12000;
const NETEASE_LOGIN_INFO_CACHE_TTL_MS: u64 = 30 * 1000;
const NETEASE_PLAYLIST_SYNC_PAGE_SIZE: usize = 200;
const NETEASE_PLAYLIST_SYNC_MAX_PAGES: usize = 80;
const NETEASE_TRACK_SYNC_PAGE_SIZE: usize = 500;
const NETEASE_TRACK_SYNC_MAX_PAGES: usize = 80;
const NETEASE_TRACK_STREAM_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone, Copy)]
struct QualityCandidate {
    level: &'static str,
    br: u64,
    label: &'static str,
    svip: bool,
}

const NETEASE_QUALITY_CANDIDATES: &[QualityCandidate] = &[
    QualityCandidate {
        level: "jymaster",
        br: 1_999_000,
        label: "超清母带",
        svip: true,
    },
    QualityCandidate {
        level: "hires",
        br: 1_999_000,
        label: "高清臻音",
        svip: false,
    },
    QualityCandidate {
        level: "lossless",
        br: 1_411_000,
        label: "无损",
        svip: false,
    },
    QualityCandidate {
        level: "exhigh",
        br: 999_000,
        label: "极高",
        svip: false,
    },
    QualityCandidate {
        level: "standard",
        br: 128_000,
        label: "标准",
        svip: false,
    },
];

/// WeAPI 加密结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeapiEncrypted {
    pub params: String,
    pub enc_sec_key: String,
}

/// 同曲匹配输入提示。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchHints {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub artist_id: String,
    #[serde(default)]
    pub artist_ids: String,
    #[serde(default)]
    pub artist_names: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration: Option<Value>,
    #[serde(default)]
    pub exclude_ids: String,
    #[serde(default)]
    pub skip_direct: bool,
}

#[derive(Debug, Clone)]
struct CachedLoginInfo {
    cookie: String,
    at_ms: u64,
    value: Value,
}

#[derive(Debug, Clone)]
struct PlaylistTrackIndex {
    playlist_meta: Value,
    track_ids: Vec<String>,
}

/// AES-128-CBC + PKCS7 加密。
fn aes_cbc_encrypt(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = Aes128CbcEnc::new(key.into(), iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|e| format!("AES-CBC encrypt failed: {e}"))?;
    Ok(ct.to_vec())
}

/// AES-128-CBC + PKCS7 解密（供单元测试验证）。
#[cfg(test)]
fn aes_cbc_decrypt(ciphertext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut buf = ciphertext.to_vec();
    let pt = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("AES-CBC decrypt failed: {e}"))?;
    Ok(pt.to_vec())
}

/// AES-128-ECB + PKCS7 加密。
fn aes_ecb_encrypt(plaintext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = Aes128EcbEnc::new(key.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|e| format!("AES-ECB encrypt failed: {e}"))?;
    Ok(ct.to_vec())
}

/// AES-128-ECB + PKCS7 解密。
pub fn aes_ecb_decrypt(ciphertext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut buf = ciphertext.to_vec();
    let pt = Aes128EcbDec::new(key.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("AES-ECB decrypt failed: {e}"))?;
    Ok(pt.to_vec())
}

fn md5_hex_lower(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// 网易云 WeAPI 加密（随机 16 字节秘钥 + 两轮 AES-128-CBC + RSA 无填充模幂）。
pub fn weapi_encrypt(text: &str) -> Result<WeapiEncrypted, String> {
    let mut rng = rand::thread_rng();
    let mut secret_key = [0u8; 16];
    for slot in &mut secret_key {
        let idx = rng.gen_range(0..WEAPI_BASE62.len());
        *slot = WEAPI_BASE62[idx];
    }
    weapi_encrypt_with_key(text, &secret_key)
}

/// 使用指定 16 字节随机密钥执行 WeAPI 加密（便于确定性测试）。
pub fn weapi_encrypt_with_key(text: &str, secret_key: &[u8; 16]) -> Result<WeapiEncrypted, String> {
    let first = aes_cbc_encrypt(text.as_bytes(), WEAPI_PRESET_KEY, WEAPI_IV)?;
    let first_b64 = BASE64_STD.encode(first);
    let second = aes_cbc_encrypt(first_b64.as_bytes(), secret_key, WEAPI_IV)?;
    let params = BASE64_STD.encode(second);

    let mut reversed_key = *secret_key;
    reversed_key.reverse();

    let modulus_bytes =
        hex::decode(WEAPI_RSA_MODULUS_HEX).map_err(|e| format!("Invalid RSA modulus: {e}"))?;
    let n = BigUint::from_bytes_be(&modulus_bytes);
    let e = BigUint::from(WEAPI_RSA_EXPONENT);
    let m = BigUint::from_bytes_be(&reversed_key);
    let encrypted_int = m.modpow(&e, &n);
    let enc_hex = format!("{encrypted_int:0256x}");

    Ok(WeapiEncrypted {
        params,
        enc_sec_key: enc_hex,
    })
}

/// 网易云 EAPI 加密（MD5 签名拼接 + AES-128-ECB + 大写十六进制）。
pub fn eapi_encrypt(url_path: &str, text: &str) -> Result<String, String> {
    let message = format!("nobody{url_path}use{text}md5forencrypt");
    let digest = md5_hex_lower(message.as_bytes());
    let data = format!("{url_path}-36cd479b6b5-{text}-36cd479b6b5-{digest}");
    let encrypted = aes_ecb_encrypt(data.as_bytes(), EAPI_KEY)?;
    Ok(hex::encode_upper(encrypted))
}

/// 解密 EAPI 响应或测试载荷。
pub fn eapi_decrypt_hex_or_raw(raw: &[u8]) -> Result<String, String> {
    let decrypted = if raw.iter().all(|b| b.is_ascii_hexdigit()) && raw.len() % 2 == 0 {
        let bin = hex::decode(raw).map_err(|e| format!("EAPI hex decode failed: {e}"))?;
        aes_ecb_decrypt(&bin, EAPI_KEY)?
    } else {
        aes_ecb_decrypt(raw, EAPI_KEY)?
    };
    String::from_utf8(decrypted).map_err(|e| format!("EAPI utf8 decode failed: {e}"))
}

/// 生成二维码 PNG Base64 DataURL。
pub fn render_qr_png_data_url(content: &str) -> Result<String, String> {
    let code = QrCode::with_error_correction_level(content.as_bytes(), EcLevel::M)
        .map_err(|e| format!("QR encode failed: {e}"))?;
    let img = code
        .render::<image::Luma<u8>>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .build();
    let mut png_bytes = Vec::new();
    let mut cursor = Cursor::new(&mut png_bytes);
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| format!("QR PNG render failed: {e}"))?;
    Ok(format!(
        "data:image/png;base64,{}",
        BASE64_STD.encode(png_bytes)
    ))
}

/// 映射网易云歌手列表。
pub fn map_artists(raw: Option<&Value>) -> Vec<Value> {
    let Some(arr) = raw.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return None;
            }
            let id = a.get("id").cloned().unwrap_or(Value::Null);
            Some(json!({ "id": id, "name": name }))
        })
        .collect()
}

/// 映射网易云歌曲记录（严格对齐 `server.js` `mapSongRecord` 输出格式）。
pub fn map_song_record(s: &Value) -> Value {
    let artists = map_artists(s.get("ar").or_else(|| s.get("artists")));
    let album = s
        .get("al")
        .or_else(|| s.get("album"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let artist_names = artists
        .iter()
        .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" / ");
    let artist_id = artists
        .first()
        .and_then(|a| a.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let album_name = album
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let album_id = album.get("id").cloned().unwrap_or_else(|| json!(""));
    let cover = album
        .get("picUrl")
        .or_else(|| album.get("coverUrl"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let duration = s
        .get("dt")
        .or_else(|| s.get("duration"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let popularity = s
        .get("pop")
        .or_else(|| s.get("popularity"))
        .or_else(|| s.get("score"))
        .or_else(|| s.get("hotScore"))
        .and_then(value_as_f64)
        .unwrap_or(0.0);
    let search_rank = match s.get("rank") {
        Some(Value::Null) | None => Value::Null,
        Some(Value::String(st)) if st.is_empty() => Value::Null,
        Some(v) => value_as_f64(v).map(|n| json!(n)).unwrap_or(Value::Null),
    };

    json!({
        "provider": "netease",
        "source": "netease",
        "type": "song",
        "id": s.get("id").cloned().unwrap_or(Value::Null),
        "name": s.get("name").cloned().unwrap_or_else(|| json!("")),
        "artist": artist_names,
        "artists": artists,
        "artistId": artist_id,
        "album": album_name,
        "albumId": album_id,
        "cover": cover,
        "duration": duration,
        "popularity": popularity,
        "searchRank": search_rank,
        "fee": s.get("fee").cloned().unwrap_or(Value::Null),
    })
}

fn map_discover_playlist(pl: &Value, tag: &str) -> Value {
    let creator = pl
        .get("creator")
        .or_else(|| pl.get("user"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let id = pl
        .get("id")
        .or_else(|| pl.get("resourceId"))
        .or_else(|| pl.get("creativeId"))
        .cloned()
        .unwrap_or(Value::Null);
    let name = pl
        .get("name")
        .or_else(|| pl.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cover = pl
        .get("picUrl")
        .or_else(|| pl.get("coverImgUrl"))
        .or_else(|| pl.get("coverUrl"))
        .or_else(|| {
            pl.get("uiElement")
                .and_then(|u| u.get("image"))
                .and_then(|i| i.get("imageUrl"))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let creator_name = creator
        .get("nickname")
        .or_else(|| creator.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let resolved_tag = if !tag.is_empty() {
        tag
    } else {
        pl.get("alg").and_then(|v| v.as_str()).unwrap_or("")
    };
    json!({
        "provider": "netease",
        "source": "netease",
        "type": "playlist",
        "id": id,
        "name": name,
        "cover": cover,
        "trackCount": pl.get("trackCount").or_else(|| pl.get("songCount")).or_else(|| pl.get("programCount")).and_then(|v| v.as_u64()).unwrap_or(0),
        "playCount": pl.get("playCount").or_else(|| pl.get("playcount")).and_then(|v| v.as_u64()).unwrap_or(0),
        "creator": creator_name,
        "tag": resolved_tag,
    })
}

fn map_netease_playlist_meta(pl: &Value, fallback_id: &str) -> Value {
    let id = pl
        .get("id")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!(fallback_id));
    let creator = pl
        .get("creator")
        .and_then(|c| c.get("nickname"))
        .or_else(|| pl.get("creatorNickname"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "id": id,
        "name": pl.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "cover": pl.get("coverImgUrl").or_else(|| pl.get("cover")).and_then(|v| v.as_str()).unwrap_or(""),
        "trackCount": pl.get("trackCount").or_else(|| pl.get("track_count")).and_then(|v| v.as_u64()).unwrap_or(0),
        "playCount": pl.get("playCount").or_else(|| pl.get("play_count")).and_then(|v| v.as_u64()).unwrap_or(0),
        "creator": creator,
        "subscribed": pl.get("subscribed").and_then(|v| v.as_bool()).unwrap_or(false),
        "specialType": pl.get("specialType").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

fn value_as_f64(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return n.is_finite().then_some(n);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    }
    None
}

fn value_as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    if let Some(n) = v.as_i64() {
        return (n >= 0).then_some(n as u64);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<u64>().ok();
    }
    None
}

fn value_id_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn normalize_api_code(payload: &Value) -> i64 {
    payload
        .get("code")
        .or_else(|| payload.get("body").and_then(|b| b.get("code")))
        .or_else(|| payload.get("status"))
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0)
}

fn normalize_api_message(payload: &Value) -> String {
    for key in ["message", "msg", "error"] {
        if let Some(s) = payload.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
        if let Some(s) = payload
            .get("body")
            .and_then(|b| b.get(key))
            .and_then(|v| v.as_str())
        {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

fn first_positive_number_from(objects: &[&Value], keys: &[&str]) -> u64 {
    for obj in objects {
        let Some(map) = obj.as_object() else {
            continue;
        };
        for key in keys {
            if let Some(val) = map.get(*key).and_then(value_as_f64) {
                if val > 0.0 {
                    return val as u64;
                }
            }
        }
    }
    0
}

fn collect_string_values(value: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 4 {
        return;
    }
    match value {
        Value::String(s) if !s.is_empty() => out.push(s.clone()),
        Value::Array(arr) => {
            for item in arr {
                collect_string_values(item, out, depth + 1);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_string_values(v, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn collect_vip_string_values(value: &Value, out: &mut Vec<String>, depth: usize) {
    static KEY_RE: OnceLock<Regex> = OnceLock::new();
    let re = KEY_RE.get_or_init(|| {
        Regex::new(r"(?i)vip|svip|member|associator|privilege|right|level|package|label|title|type")
            .unwrap()
    });
    if depth > 4 {
        return;
    }
    match value {
        Value::Array(arr) => {
            for item in arr {
                collect_vip_string_values(item, out, depth + 1);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                if re.is_match(k) {
                    collect_string_values(v, out, depth + 1);
                } else if v.is_object() || v.is_array() {
                    collect_vip_string_values(v, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn active_netease_vip_package(pkg: Option<&Value>, now_ms: u64) -> bool {
    let Some(pkg) = pkg else {
        return false;
    };
    if !pkg.is_object() {
        return false;
    }
    let expire = first_positive_number_from(
        &[pkg],
        &["expireTime", "expire_time", "expire", "endTime"],
    );
    if expire > 0 && expire < now_ms {
        return false;
    }
    first_positive_number_from(
        &[pkg],
        &[
            "vipLevel",
            "vip_level",
            "level",
            "vipType",
            "vip_type",
            "vipCode",
            "vip_code",
            "status",
        ],
    ) > 0
}

/// 解析网易云账号 VIP / SVIP 等级。
pub fn normalize_netease_vip(profile: &Value, account: &Value, extra: &Value) -> Value {
    let empty = json!({});
    let vip_info = profile
        .get("vipInfo")
        .or_else(|| profile.get("vipinfo"))
        .or_else(|| account.get("vipInfo"))
        .or_else(|| account.get("vipinfo"))
        .or_else(|| extra.get("vipInfo"))
        .or_else(|| extra.get("vipinfo"))
        .unwrap_or(&empty);
    let vip_extra = extra
        .get("vipExtra")
        .or_else(|| extra.get("vip_info"))
        .or_else(|| extra.get("vipInfoV2"))
        .unwrap_or(&empty);
    let vip_data = vip_extra.get("data").unwrap_or(vip_extra);
    let objects = [account, profile, vip_info, extra, vip_data];

    let vip_type = first_positive_number_from(
        &objects,
        &[
            "vipType",
            "vip_type",
            "viptype",
            "musicVipType",
            "music_vip_type",
            "musicVipLevel",
            "music_vip_level",
            "redVipLevel",
            "red_vip_level",
            "blackVipLevel",
            "black_vip_level",
            "luxuryVipLevel",
            "luxury_vip_level",
        ],
    );

    let mut vip_texts = Vec::new();
    let combined = json!({
        "account": account,
        "profile": profile,
        "vipInfo": vip_info,
        "extra": extra,
        "vipData": vip_data,
    });
    collect_vip_string_values(&combined, &mut vip_texts, 0);
    let text = vip_texts.join(" ").to_ascii_lowercase();

    let redplus = vip_data
        .get("redplus")
        .or_else(|| vip_data.get("redPlus"))
        .or_else(|| vip_info.get("redplus"))
        .or_else(|| vip_info.get("redPlus"))
        .or_else(|| extra.get("redplus"))
        .or_else(|| extra.get("redPlus"));
    let associator = vip_data
        .get("associator")
        .or_else(|| vip_info.get("associator"))
        .or_else(|| extra.get("associator"));
    let music_package = vip_data
        .get("musicPackage")
        .or_else(|| vip_data.get("music_package"))
        .or_else(|| vip_info.get("musicPackage"))
        .or_else(|| vip_info.get("music_package"))
        .or_else(|| extra.get("musicPackage"))
        .or_else(|| extra.get("music_package"));

    let svip_type = first_positive_number_from(
        &objects,
        &[
            "svipType",
            "svip_type",
            "superVipLevel",
            "super_vip_level",
            "superVipType",
            "super_vip_type",
        ],
    );

    let svip_obj_flag = objects.iter().any(|obj| {
        ["isSvip", "is_svip", "svip"]
            .iter()
            .any(|k| obj.get(*k).and_then(|v| v.as_bool()) == Some(true))
            || [
                "isSvip",
                "is_svip",
                "svip",
                "svipType",
                "svip_type",
                "superVipLevel",
                "super_vip_level",
            ]
            .iter()
            .any(|k| obj.get(*k).and_then(value_as_f64).unwrap_or(0.0) > 0.0)
    });
    let svip_text_flag = text.contains("svip")
        || text.contains("supervip")
        || text.contains("super_vip")
        || text.contains("黑胶svip")
        || text.contains("超级会员");

    let vip_obj_flag = objects.iter().any(|obj| {
        ["isVip", "is_vip", "vip"]
            .iter()
            .any(|k| obj.get(*k).and_then(|v| v.as_bool()) == Some(true))
            || ["isVip", "is_vip", "vip", "vipFlag", "vipflag"]
                .iter()
                .any(|k| obj.get(*k).and_then(value_as_f64).unwrap_or(0.0) > 0.0)
    });
    let vip_text_flag =
        text.contains("vip") || text.contains("黑胶") || text.contains("会员");

    let now_ms = unix_millis();
    let is_svip =
        svip_obj_flag || svip_text_flag || svip_type > 0 || active_netease_vip_package(redplus, now_ms);
    let is_vip = is_svip
        || vip_obj_flag
        || vip_text_flag
        || active_netease_vip_package(associator, now_ms)
        || active_netease_vip_package(music_package, now_ms)
        || vip_type > 0;
    let vip_level = if is_svip {
        "svip"
    } else if is_vip {
        "vip"
    } else {
        "none"
    };
    let vip_label = match vip_level {
        "svip" => "SVIP",
        "vip" => "VIP",
        _ => "无VIP",
    };

    json!({
        "vipType": vip_type,
        "vipLevel": vip_level,
        "isVip": is_vip,
        "isSvip": is_svip,
        "vipLabel": vip_label,
    })
}

fn normalize_login_info(profile: &Value, account: &Value, extra: &Value) -> Value {
    let user_id = profile
        .get("userId")
        .or_else(|| profile.get("user_id"))
        .or_else(|| profile.get("id"))
        .or_else(|| account.get("userId"))
        .or_else(|| account.get("id"))
        .filter(|v| !v.is_null() && v.as_str() != Some(""));

    let Some(user_id) = user_id else {
        return json!({ "loggedIn": false });
    };

    let vip = normalize_netease_vip(profile, account, extra);
    let nickname = profile
        .get("nickname")
        .or_else(|| profile.get("userName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("网易云用户");
    let avatar = profile
        .get("avatarUrl")
        .or_else(|| profile.get("avatar"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    json!({
        "loggedIn": true,
        "userId": user_id,
        "nickname": nickname,
        "avatar": avatar,
        "vipType": vip["vipType"],
        "vipLevel": vip["vipLevel"],
        "isVip": vip["isVip"],
        "isSvip": vip["isSvip"],
        "vipLabel": vip["vipLabel"],
    })
}

fn classify_netease_playback_restriction(last_data: Option<&Value>, login_info: &Value) -> Value {
    let logged_in = login_info
        .get("loggedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let vip_ready = login_info
        .get("isVip")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || login_info
            .get("isSvip")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        || matches!(
            login_info.get("vipLevel").and_then(|v| v.as_str()),
            Some("vip" | "svip")
        )
        || login_info
            .get("vipType")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0;

    let fee = last_data
        .and_then(|d| d.get("fee"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let code = last_data
        .and_then(|d| d.get("code"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let free_trial = last_data
        .and_then(|d| d.get("freeTrialInfo"))
        .is_some_and(|v| !v.is_null());

    if !logged_in {
        return playback_restriction(
            "netease",
            "login_required",
            "网易云需要登录后尝试获取完整播放地址",
            "login",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    if free_trial {
        return playback_restriction(
            "netease",
            "trial_only",
            "网易云仅返回试听片段，完整播放需要会员或购买",
            "upgrade",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    if fee == 1 {
        if vip_ready {
            return playback_restriction(
                "netease",
                "copyright_unavailable",
                "当前会员状态下仍未取得可播放地址，已尝试在网易云内匹配同一录音版本",
                "switch_source",
                Some(json!({ "code": code, "fee": fee })),
            );
        }
        return playback_restriction(
            "netease",
            "vip_required",
            "网易云歌曲需要 VIP 权限，当前无法获取完整播放地址",
            "upgrade",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    if fee == 4 {
        return playback_restriction(
            "netease",
            "paid_required",
            "网易云歌曲需要单曲、专辑购买或更高权限",
            "purchase",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    if fee == 8 {
        return playback_restriction(
            "netease",
            "copyright_unavailable",
            "当前网易云版本没有返回完整音源，已尝试匹配站内同一录音版本",
            "switch_source",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    if code == 404 || code == 403 {
        return playback_restriction(
            "netease",
            "copyright_unavailable",
            "网易云版权暂不可播，换源或稍后重试会更稳",
            "switch_source",
            Some(json!({ "code": code, "fee": fee })),
        );
    }
    playback_restriction(
        "netease",
        "url_unavailable",
        "网易云没有返回可播放地址，可能是版权、会员或地区限制",
        if logged_in { "switch_source" } else { "login" },
        Some(json!({ "code": code, "fee": fee })),
    )
}

// ---------- 站内同曲版本匹配工具 ----------

fn netease_source_match_text(value: &str) -> String {
    static BRACKET_RE: OnceLock<Regex> = OnceLock::new();
    static PUNCT_RE: OnceLock<Regex> = OnceLock::new();
    let bracket_re = BRACKET_RE.get_or_init(|| Regex::new(r"[（(【\[].*?[）)】\]]").unwrap());
    let punct_re =
        PUNCT_RE.get_or_init(|| Regex::new(r#"[\s·・\-—_.,，。:：'"“”‘’/\\|!?！？]+"#).unwrap());
    let lower = value.to_lowercase();
    let stripped = bracket_re.replace_all(&lower, "");
    punct_re.replace_all(&stripped, "").to_string()
}

fn netease_source_match_duration_ms(value: Option<&Value>) -> u64 {
    let mut d = value.and_then(value_as_f64).unwrap_or(0.0);
    if d > 0.0 && d < 10_000.0 {
        d *= 1000.0;
    }
    d.max(0.0) as u64
}

fn netease_source_match_artists(song: &Value) -> Vec<(String, String)> {
    let Some(list) = song
        .get("ar")
        .or_else(|| song.get("artists"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|a| {
            let id = value_id_string(a.get("id").unwrap_or(&Value::Null));
            let name = netease_source_match_text(
                a.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            );
            if id.is_empty() && name.is_empty() {
                None
            } else {
                Some((id, name))
            }
        })
        .collect()
}

fn netease_source_match_version_tokens(song: &Value) -> Vec<&'static str> {
    static RULES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        vec![
            ("live", Regex::new(r"\blive\b|现场|演唱会").unwrap()),
            ("cover", Regex::new(r"\bcover\b|翻唱").unwrap()),
            (
                "remix",
                Regex::new(r"\bremix\b|\b(?:pop |radio |club |digital dog )?mix\b|mix版").unwrap(),
            ),
            (
                "remaster",
                Regex::new(r"\bremaster(?:ed)?\b|重制").unwrap(),
            ),
            (
                "rerecord",
                Regex::new(r"\bre[ -]?record(?:ed|ing)?\b|重录").unwrap(),
            ),
            (
                "named-version",
                Regex::new(r"taylor['’]?s version|\bversion\b|\bver\.?\b|版本").unwrap(),
            ),
            (
                "edit",
                Regex::new(r"\bradio edit\b|\bedit\b|剪辑版").unwrap(),
            ),
            (
                "alternate-cut",
                Regex::new(r"\bstripped\b|\bmono\b|\bstereo\b|\bcommentary\b").unwrap(),
            ),
            (
                "instrumental",
                Regex::new(r"\binstrumental\b|伴奏|\bkaraoke\b").unwrap(),
            ),
            ("acoustic", Regex::new(r"\bacoustic\b|不插电").unwrap()),
            (
                "speed",
                Regex::new(r"\bnightcore\b|\bsped up\b|\bslowed(?: and reverb)?\b|加速|慢速|变速")
                    .unwrap(),
            ),
            ("dj", Regex::new(r"\bdj\b|dj版").unwrap()),
            ("demo", Regex::new(r"\bdemo\b|试听版").unwrap()),
        ]
    });

    let name = song.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let aliases = song
        .get("alia")
        .or_else(|| song.get("alias"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let text = format!("{name} {aliases}").to_lowercase();
    rules
        .iter()
        .filter(|(_, re)| re.is_match(&text))
        .map(|(tag, _)| *tag)
        .collect()
}

#[derive(Debug, Clone)]
struct MediaProfile {
    br: u64,
    size: u64,
    duration: u64,
    sr: u64,
}

fn netease_source_match_media_profiles(song: &Value) -> Vec<MediaProfile> {
    let mut profiles = Vec::new();
    for key in [
        "h", "m", "l", "sq", "hr", "hMusic", "mMusic", "lMusic", "sqMusic", "hrMusic",
    ] {
        let Some(item) = song.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        let br = item
            .get("br")
            .or_else(|| item.get("bitrate"))
            .and_then(value_as_u64)
            .unwrap_or(0);
        let size = item.get("size").and_then(value_as_u64).unwrap_or(0);
        let duration = item
            .get("playTime")
            .or_else(|| item.get("playtime"))
            .or_else(|| item.get("duration"))
            .and_then(value_as_u64)
            .unwrap_or(0);
        let sr = item
            .get("sr")
            .or_else(|| item.get("sampleRate"))
            .and_then(value_as_u64)
            .unwrap_or(0);
        if br > 0 || size > 0 {
            profiles.push(MediaProfile {
                br,
                size,
                duration,
                sr,
            });
        }
    }
    profiles
}

fn netease_source_match_fingerprint_count(source: &Value, candidate: &Value) -> usize {
    let sp = netease_source_match_media_profiles(source);
    let cp = netease_source_match_media_profiles(candidate);
    sp.iter()
        .filter(|left| {
            cp.iter().any(|right| {
                left.br > 0
                    && left.br == right.br
                    && left.size > 0
                    && left.size == right.size
                    && (left.duration == 0
                        || right.duration == 0
                        || left.duration.abs_diff(right.duration) <= 10)
                    && (left.sr == 0 || right.sr == 0 || left.sr == right.sr)
            })
        })
        .count()
}

fn netease_source_match_artist_set_equal(
    source_artists: &[(String, String)],
    candidate_artists: &[(String, String)],
) -> bool {
    let mut source_ids: Vec<&str> = source_artists
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    source_ids.sort();
    source_ids.dedup();
    let mut cand_ids: Vec<&str> = candidate_artists
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    cand_ids.sort();
    cand_ids.dedup();
    if !source_ids.is_empty() && !cand_ids.is_empty() {
        return source_ids == cand_ids;
    }

    let mut source_names: Vec<&str> = source_artists
        .iter()
        .map(|(_, name)| name.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    source_names.sort();
    source_names.dedup();
    let mut cand_names: Vec<&str> = candidate_artists
        .iter()
        .map(|(_, name)| name.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    cand_names.sort();
    cand_names.dedup();
    !source_names.is_empty() && source_names == cand_names
}

fn netease_source_match_candidate_score(source: &Value, candidate: &Value) -> f64 {
    let source_id = value_id_string(source.get("id").unwrap_or(&Value::Null));
    let cand_id = value_id_string(candidate.get("id").unwrap_or(&Value::Null));
    if source_id == cand_id {
        return -1.0;
    }
    let source_name = source.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let cand_name = candidate.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if netease_source_match_text(source_name) != netease_source_match_text(cand_name) {
        return -1.0;
    }
    if netease_source_match_version_tokens(source)
        != netease_source_match_version_tokens(candidate)
    {
        return -1.0;
    }

    let source_artists = netease_source_match_artists(source);
    let candidate_artists = netease_source_match_artists(candidate);
    let artist_id_match = source_artists
        .iter()
        .any(|(sid, _)| !sid.is_empty() && candidate_artists.iter().any(|(cid, _)| cid == sid));
    let artist_name_match = source_artists
        .iter()
        .any(|(_, sname)| !sname.is_empty() && candidate_artists.iter().any(|(_, cname)| cname == sname));
    let artist_set_match =
        netease_source_match_artist_set_equal(&source_artists, &candidate_artists);
    if !artist_id_match && !artist_name_match {
        return -1.0;
    }

    let source_duration =
        netease_source_match_duration_ms(source.get("dt").or_else(|| source.get("duration")));
    let cand_duration =
        netease_source_match_duration_ms(candidate.get("dt").or_else(|| candidate.get("duration")));
    let duration_diff = if source_duration > 0 && cand_duration > 0 {
        source_duration.abs_diff(cand_duration)
    } else {
        0
    };
    let duration_limit = if source_duration > 0 {
        ((source_duration as f64) * 0.012).max(1800.0) as u64
    } else {
        0
    };
    if duration_limit > 0 && duration_diff > duration_limit {
        return -1.0;
    }

    let fingerprint_matches = netease_source_match_fingerprint_count(source, candidate);
    let official_rec = candidate
        .get("__officialSourceMatch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let diff_limit = if official_rec { 1800 } else { 600 };
    if fingerprint_matches == 0
        && (!artist_set_match
            || source_duration == 0
            || cand_duration == 0
            || duration_diff > diff_limit)
    {
        return -1.0;
    }

    let privilege = candidate
        .get("__privilege")
        .or_else(|| candidate.get("privilege"))
        .and_then(|v| v.as_object());
    let mut score = (fingerprint_matches as f64) * 120.0;
    if artist_id_match {
        score += 70.0;
    } else if artist_name_match {
        score += 42.0;
    }
    if artist_set_match {
        score += 18.0;
    }
    if official_rec {
        score += 240.0;
    }
    if source_duration > 0 && cand_duration > 0 {
        score += (45.0 - (duration_diff as f64) / 100.0).max(0.0);
    }
    if let Some(priv_map) = privilege {
        let pl = priv_map.get("pl").and_then(value_as_u64).unwrap_or(0);
        let pl_level = priv_map
            .get("plLevel")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if pl > 0 && pl_level != "none" {
            score += 22.0;
        }
    }
    let pop = candidate
        .get("pop")
        .or_else(|| candidate.get("popularity"))
        .or_else(|| candidate.get("score"))
        .and_then(value_as_f64)
        .unwrap_or(0.0);
    score += (pop / 5.0).min(20.0);
    score
}

fn netease_source_match_hint_artists(hints: &MatchHints) -> Vec<Value> {
    let raw_ids = if !hints.artist_ids.is_empty() {
        &hints.artist_ids
    } else {
        &hints.artist_id
    };
    let ids: Vec<&str> = raw_ids
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut names: Vec<&str> = hints
        .artist_names
        .split('\u{001f}')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() && !hints.artist.trim().is_empty() {
        names.push(hints.artist.trim());
    }
    let count = ids.len().max(names.len());
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let id = ids.get(i).copied().unwrap_or("");
        let name = names.get(i).copied().unwrap_or(if count == 1 {
            hints.artist.trim()
        } else {
            ""
        });
        if !id.is_empty() || !name.is_empty() {
            out.push(json!({ "id": id, "name": name }));
        }
    }
    out
}

fn merge_netease_source_match_song(
    detail_song: Option<&Value>,
    search_song: Option<&Value>,
    hints: &MatchHints,
) -> Value {
    let mut merged = serde_json::Map::new();
    if let Some(Value::Object(sm)) = search_song {
        for (k, v) in sm {
            merged.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(dm)) = detail_song {
        for (k, v) in dm {
            merged.insert(k.clone(), v.clone());
        }
    }
    let filter_artists = |src: Option<&Value>| -> Vec<Value> {
        src.and_then(|s| s.get("ar").or_else(|| s.get("artists")))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|a| {
                        let has_id = !value_id_string(a.get("id").unwrap_or(&Value::Null)).is_empty();
                        let has_name = !a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .is_empty();
                        has_id || has_name
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let detail_artists = filter_artists(detail_song);
    let search_artists = filter_artists(search_song);
    let artists = if !detail_artists.is_empty() {
        detail_artists
    } else if !search_artists.is_empty() {
        search_artists
    } else {
        netease_source_match_hint_artists(hints)
    };

    let mut album = serde_json::Map::new();
    if let Some(sa) = search_song
        .and_then(|s| s.get("al").or_else(|| s.get("album")))
        .and_then(|v| v.as_object())
    {
        for (k, v) in sa {
            album.insert(k.clone(), v.clone());
        }
    }
    if let Some(da) = detail_song
        .and_then(|s| s.get("al").or_else(|| s.get("album")))
        .and_then(|v| v.as_object())
    {
        for (k, v) in da {
            album.insert(k.clone(), v.clone());
        }
    }
    let album_name = detail_song
        .and_then(|s| s.get("al").or_else(|| s.get("album")))
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            search_song
                .and_then(|s| s.get("al").or_else(|| s.get("album")))
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(hints.album.as_str())
        .to_string();
    album.insert("name".into(), json!(album_name));

    let id = detail_song
        .and_then(|s| s.get("id"))
        .filter(|v| !v.is_null())
        .cloned()
        .or_else(|| {
            search_song
                .and_then(|s| s.get("id"))
                .filter(|v| !v.is_null())
                .cloned()
        })
        .unwrap_or_else(|| json!(hints.id));
    let name = detail_song
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            search_song
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(if !hints.name.is_empty() {
            hints.name.as_str()
        } else {
            hints.title.as_str()
        })
        .to_string();
    let dt = detail_song
        .and_then(|s| s.get("dt").or_else(|| s.get("duration")))
        .and_then(value_as_u64)
        .filter(|&d| d > 0)
        .or_else(|| {
            search_song
                .and_then(|s| s.get("dt").or_else(|| s.get("duration")))
                .and_then(value_as_u64)
                .filter(|&d| d > 0)
        })
        .unwrap_or_else(|| netease_source_match_duration_ms(hints.duration.as_ref()));

    merged.insert("id".into(), id);
    merged.insert("name".into(), json!(name));
    merged.insert("ar".into(), json!(artists));
    merged.insert("artists".into(), json!(artists));
    merged.insert("al".into(), Value::Object(album.clone()));
    merged.insert("album".into(), Value::Object(album));
    merged.insert("dt".into(), json!(dt));

    if let Some(search_val) = search_song {
        for key in [
            "h", "m", "l", "sq", "hr", "hMusic", "mMusic", "lMusic", "sqMusic", "hrMusic",
        ] {
            if !merged.contains_key(key) {
                if let Some(v) = search_val.get(key) {
                    merged.insert(key.into(), v.clone());
                }
            }
        }
    }
    Value::Object(merged)
}

// ---------- 播客映射辅助 ----------

fn map_podcast_radio(r: &Value) -> Value {
    let dj = r
        .get("dj")
        .or_else(|| r.get("djSimple"))
        .or_else(|| r.get("djUser"))
        .or_else(|| r.get("creator"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let id = r
        .get("id")
        .or_else(|| r.get("rid"))
        .or_else(|| r.get("radioId"))
        .cloned()
        .unwrap_or(Value::Null);
    let dj_name = dj
        .get("nickname")
        .and_then(|v| v.as_str())
        .or_else(|| r.get("djName").and_then(|v| v.as_str()))
        .or_else(|| r.get("nickname").and_then(|v| v.as_str()))
        .unwrap_or("");
    json!({
        "id": id.clone(),
        "rid": id,
        "name": r.get("name").or_else(|| r.get("radioName")).and_then(|v| v.as_str()).unwrap_or(""),
        "cover": r.get("picUrl").or_else(|| r.get("picURL")).or_else(|| r.get("coverUrl")).or_else(|| r.get("coverImgUrl")).or_else(|| r.get("avatarUrl")).and_then(|v| v.as_str()).unwrap_or(""),
        "desc": r.get("desc").or_else(|| r.get("description")).or_else(|| r.get("rcmdText")).and_then(|v| v.as_str()).unwrap_or(""),
        "djName": dj_name,
        "category": r.get("category").or_else(|| r.get("categoryName")).and_then(|v| v.as_str()).unwrap_or(""),
        "programCount": r.get("programCount").or_else(|| r.get("programNum")).or_else(|| r.get("programCnt")).and_then(|v| v.as_u64()).unwrap_or(0),
        "subCount": r.get("subCount").or_else(|| r.get("subedCount")).or_else(|| r.get("subscriberCount")).and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

fn map_podcast_program(p: &Value, fallback_radio: &Value) -> Value {
    let empty = json!({});
    let main_song = p
        .get("mainSong")
        .or_else(|| p.get("song"))
        .or_else(|| p.get("mainTrack"))
        .unwrap_or(&empty);
    let radio = p.get("radio").unwrap_or(fallback_radio);
    let mapped_radio = map_podcast_radio(radio);
    let artists = map_artists(main_song.get("ar").or_else(|| main_song.get("artists")));
    let album = main_song
        .get("al")
        .or_else(|| main_song.get("album"))
        .unwrap_or(&empty);
    let dj = p
        .get("dj")
        .or_else(|| radio.get("dj"))
        .unwrap_or(&empty);
    let playable_id = main_song
        .get("id")
        .or_else(|| p.get("mainSongId"))
        .or_else(|| p.get("songId"))
        .cloned()
        .unwrap_or(Value::Null);
    let radio_name = mapped_radio
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let dj_nick = dj
        .get("nickname")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            mapped_radio
                .get("djName")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("");
    let artist_str = if !radio_name.is_empty() {
        radio_name.to_string()
    } else if !dj_nick.is_empty() {
        dj_nick.to_string()
    } else {
        artists
            .iter()
            .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" / ")
    };

    json!({
        "type": "podcast",
        "source": "podcast",
        "id": playable_id,
        "programId": p.get("id").or_else(|| p.get("programId")).cloned().unwrap_or(Value::Null),
        "radioId": mapped_radio.get("id").cloned().unwrap_or(Value::Null),
        "name": p.get("name").or_else(|| main_song.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
        "artist": artist_str,
        "artists": artists,
        "artistId": artists.first().and_then(|a| a.get("id")).cloned().unwrap_or(Value::Null),
        "album": if !radio_name.is_empty() { radio_name } else { album.get("name").and_then(|v| v.as_str()).unwrap_or("Podcast") },
        "cover": p.get("coverUrl").or_else(|| p.get("cover")).or_else(|| p.get("blurCoverUrl")).or_else(|| mapped_radio.get("cover")).or_else(|| album.get("picUrl")).and_then(|v| v.as_str()).unwrap_or(""),
        "duration": p.get("duration").or_else(|| main_song.get("dt")).or_else(|| main_song.get("duration")).and_then(|v| v.as_u64()).unwrap_or(0),
        "fee": main_song.get("fee").cloned().unwrap_or(Value::Null),
        "djName": dj_nick,
        "radioName": radio_name,
        "desc": p.get("description").or_else(|| p.get("desc")).and_then(|v| v.as_str()).unwrap_or(""),
        "createTime": p.get("createTime").and_then(|v| v.as_u64()).unwrap_or(0),
        "serialNum": p.get("serialNum").or_else(|| p.get("serial")).and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

fn first_array_from<'a>(obj: &'a Value, keys: &[&str]) -> Vec<Value> {
    for key in keys {
        let Some(val) = obj.get(*key) else {
            continue;
        };
        if let Some(arr) = val.as_array() {
            return arr.clone();
        }
        for sub in ["list", "data", "resources"] {
            if let Some(arr) = val.get(sub).and_then(|v| v.as_array()) {
                return arr.clone();
            }
        }
    }
    Vec::new()
}

fn map_podcast_voice(v: &Value) -> Value {
    let raw = v
        .get("resource")
        .or_else(|| v.get("voice"))
        .or_else(|| v.get("data"))
        .or_else(|| v.get("program"))
        .unwrap_or(v);
    let empty = json!({});
    let main_song = raw
        .get("mainSong")
        .or_else(|| raw.get("song"))
        .or_else(|| raw.get("track"))
        .unwrap_or(&empty);
    let radio = raw
        .get("radio")
        .or_else(|| raw.get("djRadio"))
        .or_else(|| raw.get("voiceList"))
        .or_else(|| raw.get("podcast"))
        .unwrap_or(&empty);
    let playable_id = raw
        .get("trackId")
        .or_else(|| raw.get("songId"))
        .or_else(|| raw.get("mainSongId"))
        .or_else(|| main_song.get("id"))
        .or_else(|| raw.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let radio_name = radio
        .get("name")
        .or_else(|| radio.get("radioName"))
        .or_else(|| radio.get("voiceListName"))
        .or_else(|| raw.get("podcastName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let artist = if !radio_name.is_empty() {
        radio_name
    } else {
        raw.get("djName")
            .and_then(|v| v.as_str())
            .unwrap_or("Voice")
    };
    json!({
        "type": "podcast",
        "source": "podcast",
        "sourceType": "podcast-voice",
        "id": playable_id,
        "programId": raw.get("programId").or_else(|| raw.get("voiceId")).or_else(|| raw.get("id")).cloned().unwrap_or(Value::Null),
        "radioId": radio.get("id").or_else(|| radio.get("radioId")).or_else(|| radio.get("voiceListId")).or_else(|| raw.get("radioId")).or_else(|| raw.get("voiceListId")).cloned().unwrap_or(Value::Null),
        "name": raw.get("name").or_else(|| raw.get("songName")).or_else(|| raw.get("title")).or_else(|| main_song.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
        "artist": artist,
        "album": if !radio_name.is_empty() { radio_name } else { "Podcast" },
        "cover": raw.get("coverUrl").or_else(|| raw.get("cover")).or_else(|| raw.get("picUrl")).or_else(|| raw.get("coverImgUrl")).or_else(|| radio.get("picUrl")).or_else(|| radio.get("coverUrl")).and_then(|v| v.as_str()).unwrap_or(""),
        "duration": raw.get("duration").or_else(|| raw.get("durationMs")).or_else(|| main_song.get("dt")).or_else(|| main_song.get("duration")).and_then(|v| v.as_u64()).unwrap_or(0),
        "djName": raw.get("djName").or_else(|| radio.get("dj").and_then(|d| d.get("nickname"))).and_then(|v| v.as_str()).unwrap_or(""),
        "radioName": radio_name,
        "desc": raw.get("desc").or_else(|| raw.get("description")).and_then(|v| v.as_str()).unwrap_or(""),
    })
}

fn map_podcast_collection_radio(r: &Value, key: &str) -> Value {
    let mut radio = map_podcast_radio(r);
    if let Some(obj) = radio.as_object_mut() {
        let dj = obj
            .get("djName")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                obj.get("category")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("Podcast")
            .to_string();
        let cat = obj
            .get("category")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Podcast")
            .to_string();
        let rid = obj.get("id").cloned().unwrap_or(Value::Null);
        obj.insert("type".into(), json!("podcast-radio"));
        obj.insert("sourceType".into(), json!("podcast-radio"));
        obj.insert("collectionKey".into(), json!(key));
        obj.insert("radioId".into(), rid);
        obj.insert("artist".into(), json!(dj));
        obj.insert("album".into(), json!(cat));
    }
    radio
}

fn podcast_collection_meta(key: &str, items: &[Value]) -> Value {
    let (title, sub, item_type) = match key {
        "collect" => ("收藏播客", "你收藏的播客", "radio"),
        "created" => ("创建播客", "你创建的播客", "radio"),
        "liked" => ("喜欢的声音", "收藏或最近喜欢的声音", "voice"),
        _ => (key, "", "radio"),
    };
    let cover = items
        .first()
        .and_then(|first| {
            first
                .get("cover")
                .or_else(|| first.get("picUrl"))
                .or_else(|| first.get("coverUrl"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    json!({
        "key": key,
        "title": title,
        "sub": sub,
        "itemType": item_type,
        "count": items.len(),
        "cover": cover,
    })
}

/// 网易云音乐纯 Rust 异步客户端。
pub struct NeteaseClient {
    cookie_file: Option<PathBuf>,
    cookie: RwLock<String>,
    client: reqwest::Client,
    web_base: String,
    eapi_base: String,
    login_info_cache: Mutex<Option<CachedLoginInfo>>,
    vip_info_cache: TtlCache<String, Value>,
    source_match_cache: TtlCache<String, Vec<Value>>,
    playlist_track_index_cache: TtlCache<String, PlaylistTrackIndex>,
}

impl NeteaseClient {
    /// 创建网易云客户端；如传入持久化路径且文件存在，则自动加载 Cookie。
    pub fn new(cookie_file: Option<PathBuf>) -> Self {
        let initial_cookie = cookie_file
            .as_deref()
            .map(read_cookie_file)
            .map(|s| normalize_cookie_header(&s))
            .unwrap_or_default();
        Self {
            cookie_file,
            cookie: RwLock::new(initial_cookie),
            client: shared_http_client().clone(),
            web_base: NETEASE_WEB_BASE.to_string(),
            eapi_base: NETEASE_EAPI_BASE.to_string(),
            login_info_cache: Mutex::new(None),
            vip_info_cache: TtlCache::new(32, Duration::from_secs(5 * 60)),
            source_match_cache: TtlCache::new(256, Duration::from_secs(12 * 60 * 60)),
            playlist_track_index_cache: TtlCache::new(8, Duration::from_secs(10 * 60)),
        }
    }

    /// 自定义后端基础 URL（用于测试或私有网关）。
    pub fn with_base_urls(mut self, web_base: impl Into<String>, eapi_base: impl Into<String>) -> Self {
        self.web_base = web_base.into().trim_end_matches('/').to_string();
        self.eapi_base = eapi_base.into().trim_end_matches('/').to_string();
        self
    }

    /// 读取当前内存中的 Cookie 字符串。
    pub fn get_cookie(&self) -> String {
        self.cookie.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// 判断是否已保存有效 Cookie（含 `MUSIC_U` 或非空）。
    pub fn has_cookie(&self) -> bool {
        !self.get_cookie().is_empty()
    }

    fn clear_session_caches(&self) {
        if let Ok(mut guard) = self.login_info_cache.lock() {
            *guard = None;
        }
        self.vip_info_cache.clear();
        self.playlist_track_index_cache.clear();
    }

    /// 设置并持久化网易云 Cookie（要求非空清空外校验并规范化）。
    pub fn set_cookie(&self, input: &str) -> Result<String, String> {
        let normalized = normalize_cookie_header(input);
        let value = if normalized.is_empty() {
            input.trim().to_string()
        } else {
            normalized
        };
        if let Ok(mut guard) = self.cookie.write() {
            *guard = value.clone();
        }
        if let Some(path) = &self.cookie_file {
            write_cookie_file(path, &value)?;
        }
        self.clear_session_caches();
        Ok(value)
    }

    /// 清空本地 Cookie 与缓存，并尝试通知网易云登出。
    pub async fn logout(&self) -> Value {
        let _ = self
            .weapi_request("/weapi/logout", json!({}), None, true)
            .await;
        let _ = self.set_cookie("");
        json!({ "ok": true })
    }

    fn csrf_from_cookie(cookie: &str) -> String {
        parse_cookie_string(cookie)
            .get("__csrf")
            .cloned()
            .unwrap_or_default()
    }

    async fn weapi_request(
        &self,
        path: &str,
        mut payload: Value,
        override_cookie: Option<&str>,
        with_cookie: bool,
    ) -> Result<(Value, BTreeMap<String, String>), String> {
        let cookie = if !with_cookie {
            String::new()
        } else {
            override_cookie
                .map(str::to_string)
                .unwrap_or_else(|| self.get_cookie())
        };
        let csrf = Self::csrf_from_cookie(&cookie);
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("csrf_token".to_string())
                .or_insert_with(|| json!(csrf));
        }

        let json_str =
            serde_json::to_string(&payload).map_err(|e| format!("JSON encode error: {e}"))?;
        let enc = weapi_encrypt(&json_str)?;
        let url = format!("{}{}", self.web_base, path);

        let mut req = self
            .client
            .post(&url)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, "https://music.163.com/")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            );
        if !cookie.is_empty() {
            let mut full_cookie = parse_cookie_string(&cookie);
            full_cookie
                .entry("os".to_string())
                .or_insert_with(|| "pc".to_string());
            full_cookie
                .entry("appver".to_string())
                .or_insert_with(|| "3.0.18.203152".to_string());
            req = req.header(
                reqwest::header::COOKIE,
                serialize_cookie_object(&full_cookie),
            );
        }

        let resp = req
            .form(&[
                ("params", enc.params.as_str()),
                ("encSecKey", enc.enc_sec_key.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("Netease WeAPI request failed: {e}"))?;

        let set_cookies = extract_cookies_from_set_cookie_headers(resp.headers());
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Netease WeAPI response read failed: {e}"))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| format!("Netease WeAPI JSON parse failed: {e}"))?;
        Ok((body, set_cookies))
    }

    async fn eapi_request(
        &self,
        eapi_url: &str,
        crypto_url: &str,
        mut payload: Value,
    ) -> Result<Value, String> {
        let cookie = self.get_cookie();
        let parsed = parse_cookie_string(&cookie);
        let csrf = parsed.get("__csrf").cloned().unwrap_or_default();
        let music_u = parsed.get("MUSIC_U").cloned().unwrap_or_default();
        let music_a = parsed.get("MUSIC_A").cloned().unwrap_or_default();
        let device_id = parsed
            .get("deviceId")
            .cloned()
            .unwrap_or_else(|| "mineradio_rust_device".to_string());
        let request_id = format!("{}_{:04}", unix_millis(), rand::thread_rng().gen_range(0..10000));

        let header = json!({
            "osver": "10,0,19045",
            "deviceId": device_id,
            "appver": "3.0.18.203152",
            "versioncode": "203152",
            "mobilename": "",
            "buildver": unix_secs().to_string(),
            "resolution": "1920x1080",
            "__csrf": csrf,
            "os": "pc",
            "channel": "netease",
            "requestId": request_id,
            "MUSIC_U": music_u,
            "MUSIC_A": music_a,
        });

        if let Some(obj) = payload.as_object_mut() {
            obj.insert("header".into(), header);
            obj.insert("e_r".into(), json!(false));
        }

        let text =
            serde_json::to_string(&payload).map_err(|e| format!("EAPI JSON encode error: {e}"))?;
        let params_hex = eapi_encrypt(crypto_url, &text)?;
        let url = format!("{}{}", self.eapi_base, eapi_url);

        let mut req = self
            .client
            .post(&url)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::REFERER, "https://music.163.com/")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            );
        if !cookie.is_empty() {
            req = req.header(reqwest::header::COOKIE, &cookie);
        }

        let resp = req
            .form(&[("params", params_hex.as_str())])
            .send()
            .await
            .map_err(|e| format!("Netease EAPI request failed: {e}"))?;

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Netease EAPI read failed: {e}"))?;
        if let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) {
            return Ok(parsed);
        }
        let decrypted = eapi_decrypt_hex_or_raw(&bytes)?;
        serde_json::from_str(&decrypted)
            .map_err(|e| format!("Netease EAPI decrypted JSON parse failed: {e}"))
    }

    // ---------- 搜索与歌曲详情 ----------

    /// 搜索歌曲（优先 `/weapi/cloudsearch/get/web`，缺失封面通过 `song_detail` 补齐）。
    pub async fn search(
        &self,
        keywords: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>, String> {
        let limit = limit.clamp(1, 50);
        let (body, _) = self
            .weapi_request(
                "/weapi/cloudsearch/get/web",
                json!({
                    "s": keywords,
                    "type": 1,
                    "limit": limit,
                    "offset": offset,
                    "total": true,
                }),
                None,
                true,
            )
            .await?;

        let songs = body
            .get("result")
            .and_then(|r| r.get("songs"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut mapped: Vec<Value> = songs.iter().map(map_song_record).collect();
        let missing: Vec<String> = mapped
            .iter()
            .filter(|s| {
                s.get("cover")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
            })
            .filter_map(|s| {
                let id = value_id_string(s.get("id").unwrap_or(&Value::Null));
                (!id.is_empty()).then_some(id)
            })
            .collect();

        if !missing.is_empty() {
            if let Ok(detail_body) = self.song_detail(&missing.join(",")).await {
                let mut id_to_pic: HashMap<String, String> = HashMap::new();
                if let Some(arr) = detail_body.get("songs").and_then(|v| v.as_array()) {
                    for s in arr {
                        let id = value_id_string(s.get("id").unwrap_or(&Value::Null));
                        let pic = s
                            .get("al")
                            .or_else(|| s.get("album"))
                            .and_then(|a| a.get("picUrl"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !id.is_empty() && !pic.is_empty() {
                            id_to_pic.insert(id, pic.to_string());
                        }
                    }
                }
                for item in &mut mapped {
                    let cover_empty = item
                        .get("cover")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty();
                    if cover_empty {
                        let id = value_id_string(item.get("id").unwrap_or(&Value::Null));
                        if let Some(pic) = id_to_pic.get(&id) {
                            if let Some(obj) = item.as_object_mut() {
                                obj.insert("cover".into(), json!(pic));
                            }
                        }
                    }
                }
            }
        }
        Ok(mapped)
    }

    /// 批量获取歌曲详情。
    pub async fn song_detail(&self, ids: &str) -> Result<Value, String> {
        let id_list: Vec<Value> = ids
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|id| {
                if let Ok(num) = id.parse::<u64>() {
                    json!({ "id": num })
                } else {
                    json!({ "id": id })
                }
            })
            .collect();
        let c_str =
            serde_json::to_string(&id_list).map_err(|e| format!("song_detail encode error: {e}"))?;
        let (body, _) = self
            .weapi_request("/weapi/v3/song/detail", json!({ "c": c_str }), None, true)
            .await?;
        Ok(body)
    }

    // ---------- 歌曲播放 URL 与同曲匹配 ----------

    async fn fetch_song_url_v1(&self, id: &str, level: &str) -> Result<Value, String> {
        let mut payload = json!({
            "ids": format!("[{id}]"),
            "level": level,
            "encodeType": "flac",
        });
        if level == "sky" {
            payload["immerseType"] = json!("c51");
        }
        self.eapi_request(
            "/eapi/song/enhance/player/url/v1",
            "/api/song/enhance/player/url/v1",
            payload,
        )
        .await
    }

    async fn fetch_song_url_legacy(&self, id: &str, br: u64) -> Result<Value, String> {
        self.eapi_request(
            "/eapi/song/enhance/player/url",
            "/api/song/enhance/player/url",
            json!({
                "ids": format!("[{id}]"),
                "br": br,
            }),
        )
        .await
    }

    async fn resolve_direct_song_url(
        &self,
        id: &str,
        login_info: &Value,
        quality_preference: &str,
    ) -> Value {
        let deadline = unix_millis() + NETEASE_DIRECT_RESOLVE_BUDGET_MS;
        let requested_quality = normalize_quality_preference(quality_preference);
        let svip_ready = login_info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && (login_info.get("vipLevel").and_then(|v| v.as_str()) == Some("svip")
                || login_info
                    .get("isSvip")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false));

        let start_idx = NETEASE_QUALITY_CANDIDATES
            .iter()
            .position(|c| c.level == requested_quality)
            .unwrap_or(0);
        let qualities: Vec<&QualityCandidate> = NETEASE_QUALITY_CANDIDATES[start_idx..]
            .iter()
            .filter(|q| !q.svip || svip_ready)
            .collect();

        let mut trial_fallback: Option<Value> = None;
        let mut last_data: Option<Value> = None;
        let mut last_error: Option<String> = None;
        let mut probe_failures: Vec<String> = Vec::new();
        let mut probe_cache = HashMap::new();

        for q in qualities {
            let now = unix_millis();
            if deadline.saturating_sub(now) < 500 {
                break;
            }
            let v1_budget = Duration::from_millis(deadline.saturating_sub(now).clamp(500, 2600));
            let result = match tokio::time::timeout(v1_budget, self.fetch_song_url_v1(id, q.level))
                .await
            {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("NETEASE_DIRECT_URL_TIMEOUT".to_string()),
            };

            let result = match result {
                Ok(v) => Ok(v),
                Err(e) => {
                    last_error = Some(e);
                    let now2 = unix_millis();
                    if deadline.saturating_sub(now2) < 500 {
                        continue;
                    }
                    let legacy_budget =
                        Duration::from_millis(deadline.saturating_sub(now2).clamp(500, 2200));
                    match tokio::time::timeout(legacy_budget, self.fetch_song_url_legacy(id, q.br))
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err("NETEASE_LEGACY_URL_TIMEOUT".to_string()),
                    }
                }
            };

            let body = match result {
                Ok(b) => b,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            let d = body
                .get("data")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .cloned();
            if let Some(ref data_item) = d {
                last_data = Some(data_item.clone());
                let url = data_item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let free_trial = data_item
                    .get("freeTrialInfo")
                    .filter(|v| !v.is_null())
                    .cloned();
                if !url.is_empty() {
                    let probe = if let Some(cached) = probe_cache.get(&url).cloned() {
                        cached
                    } else {
                        let rem = deadline.saturating_sub(unix_millis()).clamp(600, 2500);
                        let p = probe_playback_audio_url(
                            &self.client,
                            &url,
                            "https://music.163.com/",
                            Duration::from_millis(rem),
                        )
                        .await;
                        probe_cache.insert(url.clone(), p.clone());
                        p
                    };
                    if !probe.ok || probe.magic.is_empty() {
                        let reason = if probe.status > 0 {
                            probe.status.to_string()
                        } else if !probe.reason.is_empty() {
                            probe.reason
                        } else {
                            "invalid-audio".to_string()
                        };
                        probe_failures.push(format!("{}:{reason}", q.level));
                        continue;
                    }
                    if free_trial.is_none() {
                        return json!({
                            "provider": "netease",
                            "source": "netease",
                            "url": url,
                            "trial": false,
                            "playable": true,
                            "level": q.level,
                            "quality": q.label,
                            "br": data_item.get("br").cloned().unwrap_or(json!(q.br)),
                            "requestedQuality": requested_quality,
                            "probeStatus": probe.status,
                            "probeBytes": probe.bytes,
                            "probeMagic": probe.magic,
                        });
                    }
                    if trial_fallback.is_none() {
                        let restriction =
                            classify_netease_playback_restriction(Some(data_item), login_info);
                        trial_fallback = Some(json!({
                            "provider": "netease",
                            "source": "netease",
                            "url": url,
                            "trial": true,
                            "playable": true,
                            "level": q.level,
                            "quality": q.label,
                            "br": data_item.get("br").cloned().unwrap_or(json!(q.br)),
                            "requestedQuality": requested_quality,
                            "trialInfo": free_trial,
                            "restriction": restriction,
                            "probeStatus": probe.status,
                            "probeBytes": probe.bytes,
                            "probeMagic": probe.magic,
                        }));
                    }
                }
            }
        }

        if let Some(trial) = trial_fallback {
            return trial;
        }
        let restriction = classify_netease_playback_restriction(last_data.as_ref(), login_info);
        json!({
            "provider": "netease",
            "source": "netease",
            "url": Value::Null,
            "trial": false,
            "playable": false,
            "reason": restriction["category"],
            "message": restriction["message"],
            "restriction": restriction,
            "lastCode": last_data.as_ref().and_then(|d| d.get("code")).cloned().unwrap_or(Value::Null),
            "fee": last_data.as_ref().and_then(|d| d.get("fee")).cloned().unwrap_or(Value::Null),
            "error": last_error,
            "probeFailures": probe_failures.into_iter().take(12).collect::<Vec<_>>(),
            "requestedQuality": requested_quality,
        })
    }

    async fn find_same_track_candidates(
        &self,
        id: &str,
        hints: &MatchHints,
        lookup_deadline: u64,
    ) -> Vec<Value> {
        let source_id = id.trim();
        let title = if !hints.name.trim().is_empty() {
            hints.name.trim()
        } else {
            hints.title.trim()
        };
        let artist = hints.artist.trim();
        if source_id.is_empty() || title.is_empty() || artist.is_empty() {
            return Vec::new();
        }
        let dur_sec =
            (netease_source_match_duration_ms(hints.duration.as_ref()) as f64 / 1000.0).round()
                as u64;
        let cache_key = format!(
            "{source_id}|{}|{}|{dur_sec}",
            netease_source_match_text(title),
            netease_source_match_text(artist)
        );
        if let Some(cached) = self.source_match_cache.get(&cache_key) {
            return cached;
        }

        let query = format!("{title} {artist}");
        let search_budget = Duration::from_millis(
            lookup_deadline
                .saturating_sub(unix_millis())
                .clamp(500, 3000),
        );
        let search_res = tokio::time::timeout(
            search_budget,
            self.weapi_request(
                "/weapi/cloudsearch/get/web",
                json!({ "s": query, "type": 1, "limit": 16, "offset": 0 }),
                None,
                true,
            ),
        )
        .await;
        let search_songs = match search_res {
            Ok(Ok((body, _))) => body
                .get("result")
                .and_then(|r| r.get("songs"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => return Vec::new(),
        };

        let mut search_by_id = HashMap::new();
        let mut detail_ids = vec![source_id.to_string()];
        for s in &search_songs {
            let sid = value_id_string(s.get("id").unwrap_or(&Value::Null));
            if !sid.is_empty() {
                if detail_ids.len() < 13 && !detail_ids.contains(&sid) {
                    detail_ids.push(sid.clone());
                }
                search_by_id.insert(sid, s.clone());
            }
        }

        let mut detail_by_id = HashMap::new();
        let mut privilege_by_id = HashMap::new();
        let detail_budget = Duration::from_millis(
            lookup_deadline
                .saturating_sub(unix_millis())
                .clamp(500, 2000),
        );
        if let Ok(Ok(detail_body)) =
            tokio::time::timeout(detail_budget, self.song_detail(&detail_ids.join(","))).await
        {
            if let Some(arr) = detail_body.get("songs").and_then(|v| v.as_array()) {
                for s in arr {
                    let sid = value_id_string(s.get("id").unwrap_or(&Value::Null));
                    if !sid.is_empty() {
                        detail_by_id.insert(sid, s.clone());
                    }
                }
            }
            if let Some(arr) = detail_body.get("privileges").and_then(|v| v.as_array()) {
                for p in arr {
                    let pid = value_id_string(p.get("id").unwrap_or(&Value::Null));
                    if !pid.is_empty() {
                        privilege_by_id.insert(pid, p.clone());
                    }
                }
            }
        }

        let official_rec_id = detail_by_id
            .get(source_id)
            .or_else(|| search_by_id.get(source_id))
            .and_then(|s| s.get("noCopyrightRcmd"))
            .and_then(|r| r.get("songId"))
            .map(value_id_string)
            .unwrap_or_default();

        if !official_rec_id.is_empty()
            && !detail_by_id.contains_key(&official_rec_id)
            && !search_by_id.contains_key(&official_rec_id)
            && lookup_deadline.saturating_sub(unix_millis()) >= 500
        {
            let official_budget = Duration::from_millis(
                lookup_deadline
                    .saturating_sub(unix_millis())
                    .clamp(500, 1000),
            );
            if let Ok(Ok(off_body)) =
                tokio::time::timeout(official_budget, self.song_detail(&official_rec_id)).await
            {
                if let Some(arr) = off_body.get("songs").and_then(|v| v.as_array()) {
                    for s in arr {
                        let sid = value_id_string(s.get("id").unwrap_or(&Value::Null));
                        if !sid.is_empty() {
                            detail_by_id.insert(sid, s.clone());
                        }
                    }
                }
                if let Some(arr) = off_body.get("privileges").and_then(|v| v.as_array()) {
                    for p in arr {
                        let pid = value_id_string(p.get("id").unwrap_or(&Value::Null));
                        if !pid.is_empty() {
                            privilege_by_id.insert(pid, p.clone());
                        }
                    }
                }
            }
        }

        let mut source_hints = hints.clone();
        source_hints.id = source_id.to_string();
        source_hints.name = title.to_string();
        source_hints.artist = artist.to_string();
        let source = merge_netease_source_match_song(
            detail_by_id.get(source_id),
            search_by_id.get(source_id),
            &source_hints,
        );

        let mut candidate_seeds = search_songs;
        if !official_rec_id.is_empty()
            && !candidate_seeds
                .iter()
                .any(|s| value_id_string(s.get("id").unwrap_or(&Value::Null)) == official_rec_id)
        {
            let seed = detail_by_id.get(&official_rec_id).cloned().unwrap_or_else(|| {
                json!({
                    "id": official_rec_id,
                    "name": title,
                    "ar": netease_source_match_hint_artists(hints),
                    "dt": netease_source_match_duration_ms(hints.duration.as_ref()),
                })
            });
            candidate_seeds.insert(0, seed);
        }

        let mut ranked: Vec<Value> = Vec::new();
        for search_song in &candidate_seeds {
            let cid = value_id_string(search_song.get("id").unwrap_or(&Value::Null));
            if cid.is_empty() || cid == source_id {
                continue;
            }
            let mut candidate = merge_netease_source_match_song(
                detail_by_id.get(&cid),
                Some(search_song),
                &MatchHints::default(),
            );
            let priv_val = privilege_by_id
                .get(&cid)
                .cloned()
                .or_else(|| search_song.get("privilege").cloned())
                .unwrap_or_else(|| json!({}));
            let is_official = !official_rec_id.is_empty() && cid == official_rec_id;
            if let Some(obj) = candidate.as_object_mut() {
                obj.insert("__privilege".into(), priv_val);
                obj.insert("__officialSourceMatch".into(), json!(is_official));
            }
            let score = netease_source_match_candidate_score(&source, &candidate);
            if score < 0.0 {
                continue;
            }
            let src_dt = netease_source_match_duration_ms(
                source.get("dt").or_else(|| source.get("duration")),
            );
            let cand_dt = netease_source_match_duration_ms(
                candidate.get("dt").or_else(|| candidate.get("duration")),
            );
            let fp = netease_source_match_fingerprint_count(&source, &candidate);
            ranked.push(json!({
                "song": map_song_record(&candidate),
                "score": score,
                "fingerprintMatches": fp,
                "officialRecommendation": is_official,
                "durationDiff": src_dt.abs_diff(cand_dt),
            }));
        }

        ranked.sort_by(|a, b| {
            let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let da = a.get("durationDiff").and_then(|v| v.as_u64()).unwrap_or(0);
                    let db = b.get("durationDiff").and_then(|v| v.as_u64()).unwrap_or(0);
                    da.cmp(&db)
                })
        });
        ranked.truncate(4);
        let ttl = if ranked.is_empty() {
            Duration::from_secs(5 * 60)
        } else {
            Duration::from_secs(12 * 60 * 60)
        };
        self.source_match_cache
            .set(cache_key, ranked.clone(), Some(ttl));
        ranked
    }

    async fn resolve_same_track_playback(
        &self,
        id: &str,
        login_info: &Value,
        quality_preference: &str,
        match_hints: &MatchHints,
        request_deadline: u64,
    ) -> Option<(Value, Value, Vec<String>)> {
        let own_deadline = unix_millis() + NETEASE_SOURCE_MATCH_TOTAL_BUDGET_MS;
        let deadline = if request_deadline > 0 {
            own_deadline.min(request_deadline)
        } else {
            own_deadline
        };
        let lookup_budget = deadline
            .saturating_sub(unix_millis())
            .clamp(500, NETEASE_SOURCE_MATCH_LOOKUP_BUDGET_MS);
        let candidates = self
            .find_same_track_candidates(id, match_hints, unix_millis() + lookup_budget)
            .await;

        let excluded: HashSet<String> = match_hints
            .exclude_ids
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut tried_ids: Vec<String> = excluded.iter().cloned().collect();

        for candidate in candidates {
            let cid = value_id_string(
                candidate
                    .get("song")
                    .and_then(|s| s.get("id"))
                    .unwrap_or(&Value::Null),
            );
            if cid.is_empty() || excluded.contains(&cid) {
                continue;
            }
            tried_ids.push(cid.clone());
            let rem = deadline.saturating_sub(unix_millis());
            if rem < 800 {
                break;
            }
            let budget = Duration::from_millis(rem.min(NETEASE_DIRECT_RESOLVE_BUDGET_MS));
            let Ok(playback) = tokio::time::timeout(
                budget,
                self.resolve_direct_song_url(&cid, login_info, quality_preference),
            )
            .await
            else {
                continue;
            };
            let url_ok = playback
                .get("url")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());
            let is_trial = playback
                .get("trial")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if url_ok && !is_trial {
                return Some((candidate, playback, tried_ids));
            }
        }
        None
    }

    /// 获取歌曲播放地址（自动分级探针 + 试听检测 + 站内同曲版本匹配）。
    pub async fn song_url(
        &self,
        id: &str,
        quality: &str,
        match_hints: Option<MatchHints>,
    ) -> Result<Value, String> {
        let hints = match_hints.unwrap_or_default();
        let request_deadline = unix_millis() + NETEASE_SONG_URL_TOTAL_BUDGET_MS;
        let login_info = self.get_playback_login_info().await;

        let direct = if !hints.skip_direct {
            let budget = Duration::from_millis(
                request_deadline
                    .saturating_sub(unix_millis())
                    .clamp(500, NETEASE_DIRECT_RESOLVE_BUDGET_MS + 300),
            );
            match tokio::time::timeout(
                budget,
                self.resolve_direct_song_url(id, &login_info, quality),
            )
            .await
            {
                Ok(val) => val,
                Err(_) => {
                    let restriction = playback_restriction(
                        "netease",
                        "url_unavailable",
                        "网易云音源请求超时，已继续尝试站内同一录音版本",
                        "retry",
                        Some(json!({ "code": "NETEASE_DIRECT_RESOLVE_TIMEOUT" })),
                    );
                    json!({
                        "provider": "netease",
                        "source": "netease",
                        "url": Value::Null,
                        "trial": false,
                        "playable": false,
                        "reason": restriction["category"],
                        "message": restriction["message"],
                        "restriction": restriction,
                        "error": "NETEASE_DIRECT_RESOLVE_TIMEOUT",
                    })
                }
            }
        } else {
            let restriction = playback_restriction(
                "netease",
                "url_unavailable",
                "正在继续尝试网易云站内的其它同曲版本",
                "retry",
                Some(json!({ "code": "NETEASE_DIRECT_SKIPPED_AFTER_MATCH_FAILURE" })),
            );
            json!({
                "provider": "netease",
                "source": "netease",
                "url": Value::Null,
                "trial": false,
                "playable": false,
                "reason": restriction["category"],
                "message": restriction["message"],
                "restriction": restriction,
                "error": "NETEASE_DIRECT_SKIPPED_AFTER_MATCH_FAILURE",
            })
        };

        let direct_ok = direct
            .get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        let direct_trial = direct
            .get("trial")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if direct_ok && !direct_trial {
            return Ok(self.attach_login_vip_fields(direct, &login_info));
        }

        let source_match_attempted = !(hints.name.trim().is_empty() && hints.title.trim().is_empty())
            && !hints.artist.trim().is_empty();
        if let Some((candidate, playback, tried_ids)) = self
            .resolve_same_track_playback(id, &login_info, quality, &hints, request_deadline)
            .await
        {
            let fp = candidate
                .get("fingerprintMatches")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let official = candidate
                .get("officialRecommendation")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let match_kind = if fp > 0 {
                "netease_same_recording"
            } else if official {
                "netease_official_alternate"
            } else {
                "netease_same_track_metadata"
            };
            let resolved_id = value_id_string(
                candidate
                    .get("song")
                    .and_then(|s| s.get("id"))
                    .unwrap_or(&Value::Null),
            );
            let mut merged = playback.as_object().cloned().unwrap_or_default();
            merged.insert("provider".into(), json!("netease"));
            merged.insert("source".into(), json!("netease-same-track"));
            merged.insert("sourceMatch".into(), json!(true));
            merged.insert("matchKind".into(), json!(match_kind));
            merged.insert("matchedFromId".into(), json!(id));
            merged.insert("requestedSongId".into(), json!(id));
            merged.insert("resolvedNeteaseId".into(), json!(resolved_id));
            merged.insert("resolvedSongId".into(), json!(resolved_id));
            merged.insert("matchedSong".into(), candidate["song"].clone());
            merged.insert(
                "matchScore".into(),
                json!(candidate["score"].as_f64().unwrap_or(0.0).round() as i64),
            );
            merged.insert("fingerprintMatches".into(), json!(fp));
            merged.insert("sourceMatchTriedIds".into(), json!(tried_ids));
            merged.insert(
                "originalRestriction".into(),
                direct.get("restriction").cloned().unwrap_or(Value::Null),
            );
            return Ok(self.attach_login_vip_fields(Value::Object(merged), &login_info));
        }

        let mut out = direct.as_object().cloned().unwrap_or_default();
        out.insert("sourceMatchAttempted".into(), json!(source_match_attempted));
        Ok(self.attach_login_vip_fields(Value::Object(out), &login_info))
    }

    fn attach_login_vip_fields(&self, mut payload: Value, login_info: &Value) -> Value {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "loggedIn".into(),
                json!(login_info
                    .get("loggedIn")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)),
            );
            obj.insert(
                "vipType".into(),
                login_info.get("vipType").cloned().unwrap_or(json!(0)),
            );
            obj.insert(
                "vipLevel".into(),
                login_info
                    .get("vipLevel")
                    .cloned()
                    .unwrap_or(json!("none")),
            );
            obj.insert(
                "isVip".into(),
                json!(login_info
                    .get("isVip")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)),
            );
            obj.insert(
                "isSvip".into(),
                json!(login_info
                    .get("isSvip")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)),
            );
            obj.insert(
                "vipLabel".into(),
                login_info
                    .get("vipLabel")
                    .cloned()
                    .unwrap_or(json!("无VIP")),
            );
        }
        payload
    }

    // ---------- 歌词 ----------

    /// 获取歌曲歌词（优先新版逐字 `lyric/v1`，缺失部分回退 `/api/song/lyric`）。
    pub async fn lyric(&self, id: &str) -> Result<Value, String> {
        let mut body = json!({});
        let mut source = "lyric";
        if let Ok(nr) = self
            .eapi_request(
                "/eapi/song/lyric/v1",
                "/api/song/lyric/v1",
                json!({
                    "id": id,
                    "cp": false,
                    "tv": 0,
                    "lv": 0,
                    "rv": 0,
                    "kv": 0,
                    "yv": 0,
                    "ytv": 0,
                    "yrv": 0,
                }),
            )
            .await
        {
            body = nr;
            source = "lyric_new";
        }

        let node_text = |b: &Value, key: &str| -> String {
            b.get(key)
                .and_then(|n| n.get("lyric"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let has_primary = !node_text(&body, "lrc").is_empty() || !node_text(&body, "yrc").is_empty();
        let has_trans =
            !node_text(&body, "tlyric").is_empty() || !node_text(&body, "ytlrc").is_empty();

        if !has_primary || !has_trans {
            if let Ok((fb, _)) = self
                .weapi_request(
                    "/weapi/song/lyric",
                    json!({ "id": id, "tv": -1, "lv": -1, "rv": -1, "kv": -1 }),
                    None,
                    true,
                )
                .await
            {
                let mut merged = fb.as_object().cloned().unwrap_or_default();
                if let Some(p_map) = body.as_object() {
                    for (k, v) in p_map {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                for key in [
                    "lrc", "tlyric", "yrc", "ytlrc", "romalrc", "yromalrc", "klyric",
                ] {
                    let cur_empty = merged
                        .get(key)
                        .and_then(|n| n.get("lyric"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty();
                    if cur_empty {
                        if let Some(fb_node) = fb.get(key) {
                            merged.insert(key.to_string(), fb_node.clone());
                        }
                    }
                }
                body = Value::Object(merged);
                source = if source == "lyric_new" {
                    "lyric_new+lyric"
                } else {
                    "lyric"
                };
            }
        }

        Ok(json!({
            "lyric": node_text(&body, "lrc"),
            "tlyric": node_text(&body, "tlyric"),
            "yrc": node_text(&body, "yrc"),
            "ytlrc": node_text(&body, "ytlrc"),
            "romalrc": node_text(&body, "romalrc"),
            "yromalrc": node_text(&body, "yromalrc"),
            "source": source,
        }))
    }

    // ---------- 用户歌单与曲目 ----------

    /// 获取用户歌单列表（支持全量或分页）。
    pub async fn user_playlists(
        &self,
        uid: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Value, String> {
        if limit.is_some() || offset.is_some() {
            let lim = limit.unwrap_or(48).clamp(1, NETEASE_PLAYLIST_SYNC_PAGE_SIZE);
            let off = offset.unwrap_or(0);
            let (body, _) = self
                .weapi_request(
                    "/weapi/user/playlist",
                    json!({ "uid": uid, "limit": lim, "offset": off, "includeVideo": true }),
                    None,
                    true,
                )
                .await?;
            let raw = body
                .get("playlist")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let playlists: Vec<Value> = raw
                .iter()
                .map(|pl| {
                    let pid = value_id_string(pl.get("id").unwrap_or(&Value::Null));
                    map_netease_playlist_meta(pl, &pid)
                })
                .collect();
            let count = playlists.len();
            let total = body
                .get("total")
                .or_else(|| body.get("count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .max((off + count) as u64) as usize;
            let next_offset = off + count;
            return Ok(json!({
                "loggedIn": true,
                "userId": uid,
                "playlists": playlists,
                "total": total,
                "offset": off,
                "limit": lim,
                "nextOffset": next_offset,
                "hasMore": if total > 0 { next_offset < total } else { count >= lim },
                "partial": true,
            }));
        }

        let mut playlists = Vec::new();
        let mut seen = HashSet::new();
        let mut cur_offset = 0usize;
        let mut total = 0usize;
        for _ in 0..NETEASE_PLAYLIST_SYNC_MAX_PAGES {
            let (body, _) = self
                .weapi_request(
                    "/weapi/user/playlist",
                    json!({
                        "uid": uid,
                        "limit": NETEASE_PLAYLIST_SYNC_PAGE_SIZE,
                        "offset": cur_offset,
                        "includeVideo": true,
                    }),
                    None,
                    true,
                )
                .await?;
            let raw = body
                .get("playlist")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            total = body
                .get("total")
                .or_else(|| body.get("count"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(total);
            for pl in &raw {
                let id = value_id_string(pl.get("id").unwrap_or(&Value::Null));
                if !id.is_empty() && seen.insert(id.clone()) {
                    playlists.push(map_netease_playlist_meta(pl, &id));
                }
            }
            if raw.is_empty()
                || raw.len() < NETEASE_PLAYLIST_SYNC_PAGE_SIZE
                || (total > 0 && playlists.len() >= total)
            {
                break;
            }
            cur_offset += NETEASE_PLAYLIST_SYNC_PAGE_SIZE;
        }
        Ok(json!({
            "loggedIn": true,
            "userId": uid,
            "playlists": playlists,
        }))
    }

    async fn fetch_playlist_detail_raw(&self, id: &str) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request(
                "/weapi/v6/playlist/detail",
                json!({ "id": id, "n": 100000, "s": 0 }),
                None,
                true,
            )
            .await?;
        Ok(body)
    }

    async fn get_playlist_track_index(&self, id: &str) -> Result<PlaylistTrackIndex, String> {
        if let Some(cached) = self.playlist_track_index_cache.get(&id.to_string()) {
            return Ok(cached);
        }
        let body = self.fetch_playlist_detail_raw(id).await?;
        let pl = body.get("playlist").cloned().unwrap_or_else(|| json!({}));
        let mut meta = map_netease_playlist_meta(&pl, id);
        let raw_ids = pl
            .get("trackIds")
            .and_then(|v| v.as_array())
            .filter(|arr| !arr.is_empty())
            .or_else(|| pl.get("tracks").and_then(|v| v.as_array()))
            .cloned()
            .unwrap_or_default();
        let track_ids: Vec<String> = raw_ids
            .iter()
            .filter_map(|item| {
                let tid = item
                    .get("id")
                    .or_else(|| item.get("songId"))
                    .or_else(|| item.get("trackId"))
                    .map(value_id_string)
                    .unwrap_or_default();
                (!tid.is_empty()).then_some(tid)
            })
            .collect();
        if meta.get("trackCount").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("trackCount".into(), json!(track_ids.len()));
            }
        }
        let index = PlaylistTrackIndex {
            playlist_meta: meta,
            track_ids,
        };
        self.playlist_track_index_cache
            .set(id.to_string(), index.clone(), None);
        Ok(index)
    }

    /// 获取歌单曲目列表（分页时利用 `trackIds` 索引分片查询 `song_detail`，全量时分批拉取）。
    pub async fn playlist_tracks(
        &self,
        id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Value, String> {
        if limit.is_some() || offset.is_some() {
            let lim = limit.unwrap_or(48).clamp(1, NETEASE_TRACK_STREAM_PAGE_SIZE);
            let off = offset.unwrap_or(0);
            let index = self.get_playlist_track_index(id).await?;
            let total = index.track_ids.len();
            let page_ids: Vec<String> = index
                .track_ids
                .iter()
                .skip(off)
                .take(lim)
                .cloned()
                .collect();
            let requested_count = page_ids.len();
            let mut tracks = Vec::new();
            if !page_ids.is_empty() {
                let detail = self.song_detail(&page_ids.join(",")).await?;
                let rows = detail
                    .get("songs")
                    .or_else(|| detail.get("tracks"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let by_id: HashMap<String, Value> = rows
                    .into_iter()
                    .filter_map(|t| {
                        let tid = value_id_string(t.get("id").unwrap_or(&Value::Null));
                        (!tid.is_empty()).then_some((tid, t))
                    })
                    .collect();
                for tid in &page_ids {
                    if let Some(raw) = by_id.get(tid) {
                        tracks.push(map_song_record(raw));
                    }
                }
            }
            let next_offset = off + requested_count.max(tracks.len());
            return Ok(json!({
                "playlist": index.playlist_meta,
                "tracks": tracks,
                "offset": off,
                "limit": lim,
                "nextOffset": next_offset,
                "hasMore": if total > 0 { next_offset < total } else { tracks.len() >= lim },
                "total": total,
                "partial": true,
            }));
        }

        let index = self.get_playlist_track_index(id).await?;
        let mut all_tracks = Vec::new();
        for chunk in index.track_ids.chunks(NETEASE_TRACK_SYNC_PAGE_SIZE).take(NETEASE_TRACK_SYNC_MAX_PAGES) {
            if chunk.is_empty() {
                continue;
            }
            if let Ok(detail) = self.song_detail(&chunk.join(",")).await {
                let rows = detail
                    .get("songs")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let by_id: HashMap<String, Value> = rows
                    .into_iter()
                    .filter_map(|t| {
                        let tid = value_id_string(t.get("id").unwrap_or(&Value::Null));
                        (!tid.is_empty()).then_some((tid, t))
                    })
                    .collect();
                for tid in chunk {
                    if let Some(raw) = by_id.get(tid) {
                        all_tracks.push(map_song_record(raw));
                    }
                }
            }
        }
        let mut playlist_meta = index.playlist_meta;
        if playlist_meta
            .get("trackCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            == 0
        {
            if let Some(obj) = playlist_meta.as_object_mut() {
                obj.insert("trackCount".into(), json!(all_tracks.len()));
            }
        }
        Ok(json!({
            "playlist": playlist_meta,
            "tracks": all_tracks,
        }))
    }

    /// 创建歌单。
    pub async fn playlist_create(&self, name: &str, privacy: &str) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request(
                "/weapi/playlist/create",
                json!({
                    "name": name,
                    "privacy": privacy,
                    "type": "NORMAL",
                }),
                None,
                true,
            )
            .await?;
        let created = body
            .get("playlist")
            .or_else(|| body.get("data"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(json!({
            "loggedIn": true,
            "playlist": created,
            "body": body,
        }))
    }

    /// 向歌单添加歌曲。
    pub async fn playlist_add_song(&self, pid: &str, ids: &str) -> Result<Value, String> {
        let id_list: Vec<&str> = ids
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let track_ids_json =
            serde_json::to_string(&id_list).map_err(|e| format!("trackIds encode error: {e}"))?;
        let (body, _) = self
            .weapi_request(
                "/weapi/playlist/manipulate/tracks",
                json!({
                    "op": "add",
                    "pid": pid,
                    "trackIds": track_ids_json,
                    "imme": "true",
                }),
                None,
                true,
            )
            .await?;
        let code = normalize_api_code(&body);
        let msg = normalize_api_message(&body);
        let success = code == 200 && body.get("error").is_none();
        if success {
            self.playlist_track_index_cache.remove(&pid.to_string());
        }
        Ok(json!({
            "loggedIn": true,
            "pid": pid,
            "id": ids,
            "success": success,
            "code": code,
            "error": if success { Value::Null } else { json!(if msg.is_empty() { "PLAYLIST_ADD_FAILED" } else { &msg }) },
            "body": body,
        }))
    }

    /// 收藏或取消收藏歌单。
    pub async fn playlist_subscribe(&self, id: &str, subscribed: bool) -> Result<Value, String> {
        let path = if subscribed {
            "/weapi/playlist/subscribe"
        } else {
            "/weapi/playlist/unsubscribe"
        };
        let (body, _) = self
            .weapi_request(path, json!({ "id": id }), None, true)
            .await?;
        let code = normalize_api_code(&body);
        Ok(json!({
            "provider": "netease",
            "id": id,
            "subscribed": subscribed,
            "success": code == 200,
            "code": code,
            "body": body,
        }))
    }

    // ---------- 专辑与歌手 ----------

    /// 获取专辑详情与曲目列表。
    pub async fn album_detail(&self, id: &str, limit: usize) -> Result<Value, String> {
        let album_id = id.trim();
        let num = limit.clamp(10, 120);
        if album_id.is_empty() {
            return Ok(
                json!({ "provider": "netease", "error": "MISSING_ALBUM_ID", "album": Value::Null, "songs": [] }),
            );
        }
        let path = format!("/weapi/v1/album/{album_id}");
        let (body, _) = self.weapi_request(&path, json!({}), None, true).await?;
        let empty = json!({});
        let info = body
            .get("album")
            .or_else(|| body.get("data").and_then(|d| d.get("album").or(Some(d))))
            .unwrap_or(&empty);
        let raw_songs = body
            .get("songs")
            .and_then(|v| v.as_array())
            .or_else(|| info.get("songs").and_then(|v| v.as_array()))
            .cloned()
            .unwrap_or_default();
        let artists = map_artists(info.get("artists").or_else(|| info.get("ar")));
        let songs: Vec<Value> = raw_songs
            .iter()
            .take(num)
            .map(map_song_record)
            .filter(|s| !s.get("id").is_none_or(|v| v.is_null()))
            .collect();
        let artist_str = {
            let joined = artists
                .iter()
                .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(" / ");
            if !joined.is_empty() {
                joined
            } else {
                info.get("artist")
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        songs
                            .first()
                            .and_then(|s| s.get("artist"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_default()
            }
        };
        let total = info
            .get("size")
            .or_else(|| info.get("trackCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(raw_songs.len() as u64);

        Ok(json!({
            "provider": "netease",
            "album": {
                "provider": "netease",
                "id": info.get("id").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!(album_id)),
                "albumId": info.get("id").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!(album_id)),
                "name": info.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "artist": artist_str,
                "artists": artists,
                "cover": info.get("picUrl").or_else(|| info.get("coverUrl")).and_then(|v| v.as_str()).unwrap_or(""),
                "releaseDate": info.get("publishTime").cloned().unwrap_or(json!("")),
                "trackCount": total,
            },
            "songs": songs,
            "total": total,
        }))
    }

    /// 收藏或取消收藏专辑。
    pub async fn album_subscribe(&self, id: &str, subscribed: bool) -> Result<Value, String> {
        let path = if subscribed {
            "/weapi/album/sub"
        } else {
            "/weapi/album/unsub"
        };
        let (body, _) = self
            .weapi_request(path, json!({ "id": id }), None, true)
            .await?;
        let code = normalize_api_code(&body);
        Ok(json!({
            "provider": "netease",
            "id": id,
            "subscribed": subscribed,
            "success": code == 200,
            "code": code,
            "body": body,
        }))
    }

    /// 批量检查专辑是否已收藏。
    pub async fn album_subscribe_check(&self, ids: &[String]) -> Result<Value, String> {
        let wanted: HashSet<String> = ids.iter().cloned().collect();
        let mut found: HashSet<String> = HashSet::new();
        let mut offset = 0usize;
        for _ in 0..8 {
            if found.len() >= wanted.len() {
                break;
            }
            let (body, _) = self
                .weapi_request(
                    "/weapi/album/sublist",
                    json!({ "limit": 50, "offset": offset, "total": true }),
                    None,
                    true,
                )
                .await?;
            let rows = body
                .get("data")
                .and_then(|v| v.as_array())
                .or_else(|| body.get("albums").and_then(|v| v.as_array()))
                .cloned()
                .unwrap_or_default();
            for item in &rows {
                let id = value_id_string(item.get("id").unwrap_or(&Value::Null));
                if wanted.contains(&id) {
                    found.insert(id);
                }
            }
            let has_more = body
                .get("hasMore")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !has_more || rows.len() < 50 {
                break;
            }
            offset += rows.len();
        }
        let mut subscribed = serde_json::Map::new();
        for id in ids {
            subscribed.insert(id.clone(), json!(found.contains(id)));
        }
        Ok(json!({
            "provider": "netease",
            "ids": ids,
            "subscribed": Value::Object(subscribed),
        }))
    }

    /// 获取歌手详情及热门歌曲。
    pub async fn artist_detail(&self, id: &str, limit: usize) -> Result<Value, String> {
        let lim = limit.clamp(10, 80);
        let detail_body = self
            .weapi_request("/weapi/artist/head/info/get", json!({ "id": id }), None, true)
            .await
            .map(|(b, _)| b)
            .unwrap_or_else(|_| json!({}));

        let mut raw_songs = self
            .weapi_request(
                "/weapi/v1/artist/songs",
                json!({
                    "id": id,
                    "private_cloud": "true",
                    "work_type": 1,
                    "order": "hot",
                    "offset": 0,
                    "limit": lim,
                }),
                None,
                true,
            )
            .await
            .map(|(b, _)| {
                b.get("songs")
                    .or_else(|| b.get("data").and_then(|d| d.get("songs")))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        if raw_songs.is_empty() {
            if let Ok((top, _)) = self
                .weapi_request("/weapi/artist/top/song", json!({ "id": id }), None, true)
                .await
            {
                raw_songs = top
                    .get("songs")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
            }
        }

        let empty = json!({});
        let artist = detail_body
            .get("artist")
            .or_else(|| {
                detail_body
                    .get("data")
                    .and_then(|d| d.get("artist").or(Some(d)))
            })
            .unwrap_or(&empty);
        let songs: Vec<Value> = raw_songs
            .iter()
            .map(map_song_record)
            .filter(|s| !s.get("id").is_none_or(|v| v.is_null()))
            .take(lim)
            .collect();

        Ok(json!({
            "id": id,
            "artist": {
                "id": artist.get("id").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!(id)),
                "name": artist.get("name").or_else(|| artist.get("artistName")).and_then(|v| v.as_str()).unwrap_or(""),
                "avatar": artist.get("avatar").or_else(|| artist.get("cover")).or_else(|| artist.get("picUrl")).or_else(|| artist.get("img1v1Url")).and_then(|v| v.as_str()).unwrap_or(""),
                "brief": artist.get("briefDesc").or_else(|| artist.get("description")).or_else(|| artist.get("desc")).and_then(|v| v.as_str()).unwrap_or(""),
                "musicSize": artist.get("musicSize").or_else(|| artist.get("songSize")).and_then(|v| v.as_u64()).unwrap_or(0),
                "albumSize": artist.get("albumSize").and_then(|v| v.as_u64()).unwrap_or(0),
            },
            "songs": songs,
            "body": detail_body,
        }))
    }

    // ---------- 评论与红心 ----------

    /// 获取歌曲评论（offset == 0 时优先热评）。
    pub async fn song_comments(
        &self,
        id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let lim = limit.clamp(6, 50);
        let path = format!("/weapi/v1/resource/comments/R_SO_4_{id}");
        let (body, _) = self
            .weapi_request(
                &path,
                json!({
                    "rid": id,
                    "limit": lim,
                    "offset": offset,
                    "beforeTime": 0,
                }),
                None,
                true,
            )
            .await?;

        let hot_comments = body.get("hotComments").and_then(|v| v.as_array());
        let use_hot = offset == 0 && hot_comments.is_some();
        let raw = if use_hot {
            hot_comments.cloned().unwrap_or_default()
        } else {
            body.get("comments")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        };

        let comments: Vec<Value> = raw
            .iter()
            .filter_map(|c| {
                let content = c.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return None;
                }
                let user = c.get("user").and_then(|u| {
                    if u.is_object() {
                        Some(json!({
                            "id": u.get("userId").cloned().unwrap_or(Value::Null),
                            "nickname": u.get("nickname").and_then(|v| v.as_str()).unwrap_or(""),
                            "avatar": u.get("avatarUrl").and_then(|v| v.as_str()).unwrap_or(""),
                        }))
                    } else {
                        None
                    }
                });
                Some(json!({
                    "id": c.get("commentId").cloned().unwrap_or(Value::Null),
                    "content": content,
                    "likedCount": c.get("likedCount").and_then(|v| v.as_u64()).unwrap_or(0),
                    "time": c.get("time").and_then(|v| v.as_u64()).unwrap_or(0),
                    "user": user,
                }))
            })
            .collect();

        Ok(json!({
            "id": id,
            "total": body.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
            "comments": comments,
            "hot": use_hot,
            "body": body,
        }))
    }

    /// 发表或回复歌曲评论。
    pub async fn post_comment(
        &self,
        id: &str,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Value, String> {
        let thread_id = format!("R_SO_4_{id}");
        let (path, payload) = if let Some(cid) = reply_to.filter(|s| !s.is_empty()) {
            (
                "/weapi/resource/comments/reply",
                json!({
                    "threadId": thread_id,
                    "commentId": cid,
                    "content": content,
                }),
            )
        } else {
            (
                "/weapi/resource/comments/add",
                json!({
                    "threadId": thread_id,
                    "content": content,
                }),
            )
        };
        let (body, _) = self.weapi_request(path, payload, None, true).await?;
        let code = normalize_api_code(&body);
        Ok(json!({
            "provider": "netease",
            "id": id,
            "created": code == 200,
            "success": code == 200,
            "code": code,
            "body": body,
        }))
    }

    /// 点赞或取消点赞评论。
    pub async fn comment_like(&self, id: &str, cid: &str, liked: bool) -> Result<Value, String> {
        let path = if liked {
            "/weapi/v1/comment/like"
        } else {
            "/weapi/v1/comment/unlike"
        };
        let (body, _) = self
            .weapi_request(
                path,
                json!({
                    "threadId": format!("R_SO_4_{id}"),
                    "commentId": cid,
                }),
                None,
                true,
            )
            .await?;
        let code = normalize_api_code(&body);
        Ok(json!({
            "provider": "netease",
            "id": id,
            "commentId": cid,
            "liked": liked,
            "success": code == 200,
            "code": code,
            "body": body,
        }))
    }

    /// 红心或取消红心歌曲。
    pub async fn like_song(&self, id: &str, like: bool) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request(
                "/weapi/radio/like",
                json!({
                    "alg": "itembased",
                    "trackId": id,
                    "like": like,
                    "time": "3",
                }),
                None,
                true,
            )
            .await?;
        let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(200);
        Ok(json!({
            "loggedIn": true,
            "id": id,
            "liked": like,
            "code": code,
            "body": body,
        }))
    }

    /// 获取当前用户红心歌曲 ID 列表。
    pub async fn likelist(&self, uid: &str) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request("/weapi/song/like/get", json!({ "uid": uid }), None, true)
            .await?;
        Ok(body)
    }

    /// 批量检测歌曲红心状态。
    pub async fn song_like_check(&self, uid: &str, ids: &[String]) -> Result<Value, String> {
        let mut liked_ids: HashSet<String> = HashSet::new();
        let num_ids: Vec<u64> = ids.iter().filter_map(|s| s.parse::<u64>().ok()).collect();
        if !num_ids.is_empty() {
            if let Ok((body, _)) = self
                .weapi_request(
                    "/weapi/song/like/check",
                    json!({ "trackIds": serde_json::to_string(&num_ids).unwrap_or_default() }),
                    None,
                    true,
                )
                .await
            {
                let data = body
                    .get("data")
                    .or_else(|| body.get("ids"))
                    .unwrap_or(&body);
                if let Some(arr) = data.as_array() {
                    for item in arr {
                        let s = value_id_string(item);
                        if !s.is_empty() {
                            liked_ids.insert(s);
                        }
                    }
                } else if let Some(obj) = data.as_object() {
                    for id in ids {
                        if obj.get(id).and_then(|v| v.as_bool()) == Some(true) {
                            liked_ids.insert(id.clone());
                        }
                    }
                }
            }
        }

        if liked_ids.is_empty() {
            let list_body = self.likelist(uid).await?;
            if let Some(arr) = list_body.get("ids").and_then(|v| v.as_array()) {
                for item in arr {
                    let s = value_id_string(item);
                    if !s.is_empty() {
                        liked_ids.insert(s);
                    }
                }
            }
        }

        let mut liked = serde_json::Map::new();
        for id in ids {
            liked.insert(id.clone(), json!(liked_ids.contains(id)));
        }
        Ok(json!({
            "loggedIn": true,
            "ids": ids,
            "liked": Value::Object(liked),
        }))
    }

    // ---------- 扫码登录与用户信息 ----------

    /// 申请二维码登录 unikey。
    pub async fn login_qr_key(&self) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request(
                "/weapi/login/qrcode/unikey",
                json!({ "type": 1 }),
                None,
                false,
            )
            .await?;
        let key = body
            .get("unikey")
            .or_else(|| body.get("data").and_then(|d| d.get("unikey")))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(json!({ "key": key }))
    }

    /// 生成二维码图片（PNG DataURL）与扫码 URL。
    pub async fn login_qr_create(&self, key: &str) -> Result<Value, String> {
        let qr_url = format!("https://music.163.com/login?codekey={key}");
        let qr_img = render_qr_png_data_url(&qr_url)?;
        Ok(json!({
            "img": qr_img,
            "url": qr_url,
        }))
    }

    /// 轮询扫码状态（800=过期, 801=等待扫码, 802=待确认, 803=授权成功并自动持久化 Cookie）。
    pub async fn login_qr_check(&self, key: &str) -> Result<Value, String> {
        let (mut body, mut set_cookies) = self
            .weapi_request(
                "/weapi/login/qrcode/client/login",
                json!({ "key": key, "type": 1 }),
                None,
                false,
            )
            .await?;
        let mut code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let mut msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut cookie_str = serialize_cookie_object(&set_cookies);
        if code == 803 && cookie_str.is_empty() {
            if let Ok((retry_body, retry_cookies)) = self
                .weapi_request(
                    "/weapi/login/qrcode/client/login",
                    json!({ "key": key, "type": 1 }),
                    None,
                    true,
                )
                .await
            {
                let retry_str = serialize_cookie_object(&retry_cookies);
                if !retry_str.is_empty() {
                    body = retry_body;
                    set_cookies = retry_cookies;
                    code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(code);
                    msg = body
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&msg)
                        .to_string();
                    cookie_str = retry_str;
                }
            }
        }
        let _ = set_cookies;

        if code == 803 {
            if !cookie_str.is_empty() {
                let _ = self.set_cookie(&cookie_str);
            }
            let mut info = self.login_status().await.unwrap_or_else(|_| json!({ "loggedIn": false }));
            if !info
                .get("loggedIn")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                let empty = json!({});
                let profile = body
                    .get("profile")
                    .or_else(|| body.get("data").and_then(|d| d.get("profile")))
                    .unwrap_or(&empty);
                let account = body
                    .get("account")
                    .or_else(|| body.get("data").and_then(|d| d.get("account")))
                    .unwrap_or(&empty);
                let extra = body.get("data").unwrap_or(&body);
                let mut normalized = normalize_login_info(profile, account, extra);
                if normalized
                    .get("loggedIn")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    normalized = self
                        .enrich_login_info(normalized, profile, account, extra)
                        .await;
                }
                info = normalized;
            }
            if !info
                .get("loggedIn")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && !cookie_str.is_empty()
            {
                let nickname = body
                    .get("nickname")
                    .or_else(|| body.get("profile").and_then(|p| p.get("nickname")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("网易云用户");
                let avatar = body
                    .get("avatarUrl")
                    .or_else(|| body.get("profile").and_then(|p| p.get("avatarUrl")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                info = json!({
                    "loggedIn": true,
                    "pendingProfile": true,
                    "nickname": nickname,
                    "avatar": avatar,
                    "vipType": 0,
                    "vipLevel": "none",
                    "isVip": false,
                    "isSvip": false,
                    "vipLabel": "无VIP",
                });
            }
            let mut out = info.as_object().cloned().unwrap_or_default();
            out.insert("code".into(), json!(code));
            out.insert("message".into(), json!(msg));
            out.insert("hasCookie".into(), json!(!cookie_str.is_empty()));
            return Ok(Value::Object(out));
        }

        Ok(json!({
            "code": code,
            "message": msg,
            "nickname": body.get("nickname").cloned().unwrap_or(Value::Null),
            "avatar": body.get("avatarUrl").cloned().unwrap_or(Value::Null),
        }))
    }

    /// 获取用户 VIP 详情（`vip_info_v2` 优先，失败回退 `vip_info`）。
    pub async fn vip_info(&self, user_id: &str) -> Result<Option<Value>, String> {
        let uid = user_id.trim();
        let cookie = self.get_cookie();
        if uid.is_empty() || cookie.is_empty() {
            return Ok(None);
        }
        if let Some(cached) = self.vip_info_cache.get(&uid.to_string()) {
            return Ok(Some(cached));
        }
        let body = match self
            .weapi_request(
                "/weapi/music-vip-membership/client/vip/info",
                json!({ "userId": uid }),
                None,
                true,
            )
            .await
        {
            Ok((b, _)) => Some(b),
            Err(_) => self
                .weapi_request(
                    "/weapi/music-vip-membership/front/vip/info",
                    json!({ "userId": uid }),
                    None,
                    true,
                )
                .await
                .map(|(b, _)| b)
                .ok(),
        };
        if let Some(ref b) = body {
            self.vip_info_cache.set(uid.to_string(), b.clone(), None);
        }
        Ok(body)
    }

    async fn enrich_login_info(
        &self,
        info: Value,
        profile: &Value,
        account: &Value,
        extra: &Value,
    ) -> Value {
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let uid = value_id_string(info.get("userId").unwrap_or(&Value::Null));
        if !logged_in || uid.is_empty() {
            return info;
        }
        let vip_extra = tokio::time::timeout(Duration::from_millis(1800), self.vip_info(&uid))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        let Some(vip_extra_val) = vip_extra else {
            return info;
        };
        let mut merged_extra = extra.as_object().cloned().unwrap_or_default();
        merged_extra.insert("vipExtra".into(), vip_extra_val);
        let vip = normalize_netease_vip(profile, account, &Value::Object(merged_extra));
        let mut out = info.as_object().cloned().unwrap_or_default();
        if let Some(vmap) = vip.as_object() {
            for (k, v) in vmap {
                out.insert(k.clone(), v.clone());
            }
        }
        Value::Object(out)
    }

    async fn fetch_netease_login_info(&self) -> Value {
        let cookie = self.get_cookie();
        if cookie.is_empty() {
            return json!({
                "loggedIn": false,
                "vipType": 0,
                "vipLevel": "none",
                "isVip": false,
                "isSvip": false,
                "vipLabel": "无VIP",
            });
        }
        let empty = json!({});

        if let Ok(Ok((body, _))) = tokio::time::timeout(
            Duration::from_millis(2400),
            self.weapi_request("/weapi/w/nuser/account/get", json!({}), None, true),
        )
        .await
        {
            let data = body.get("data").unwrap_or(&body);
            let profile = data
                .get("profile")
                .or_else(|| body.get("profile"))
                .unwrap_or(&empty);
            let account = data
                .get("account")
                .or_else(|| body.get("account"))
                .unwrap_or(&empty);
            let info = normalize_login_info(profile, account, data);
            if info
                .get("loggedIn")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return self.enrich_login_info(info, profile, account, data).await;
            }
            let code = normalize_api_code(&body);
            let msg = normalize_api_message(&body).to_ascii_lowercase();
            if code == 301
                || code == 401
                || (code >= 300
                    && (msg.contains("登录") || msg.contains("login")))
            {
                let _ = self.set_cookie("");
            }
        }

        json!({
            "loggedIn": false,
            "hasCookie": !self.get_cookie().is_empty(),
            "vipType": 0,
            "vipLevel": "none",
            "isVip": false,
            "isSvip": false,
            "vipLabel": "无VIP",
        })
    }

    /// 获取登录状态（30 秒内缓存命中同一 Cookie 结果）。
    pub async fn login_status(&self) -> Result<Value, String> {
        let cookie_key = self.get_cookie();
        if cookie_key.is_empty() {
            return Ok(json!({
                "loggedIn": false,
                "vipType": 0,
                "vipLevel": "none",
                "isVip": false,
                "isSvip": false,
                "vipLabel": "无VIP",
            }));
        }
        if let Ok(guard) = self.login_info_cache.lock() {
            if let Some(ref cached) = *guard {
                if cached.cookie == cookie_key
                    && unix_millis().saturating_sub(cached.at_ms) < NETEASE_LOGIN_INFO_CACHE_TTL_MS
                {
                    return Ok(cached.value.clone());
                }
            }
        }
        let info = self.fetch_netease_login_info().await;
        if self.get_cookie() == cookie_key {
            if let Ok(mut guard) = self.login_info_cache.lock() {
                *guard = Some(CachedLoginInfo {
                    cookie: cookie_key,
                    at_ms: unix_millis(),
                    value: info.clone(),
                });
            }
        }
        Ok(info)
    }

    async fn get_playback_login_info(&self) -> Value {
        let cookie = self.get_cookie();
        if let Ok(Ok(info)) =
            tokio::time::timeout(Duration::from_millis(800), self.login_status()).await
        {
            return info;
        }
        if let Ok(guard) = self.login_info_cache.lock() {
            if let Some(ref cached) = *guard {
                if cached.cookie == cookie {
                    return cached.value.clone();
                }
            }
        }
        json!({
            "loggedIn": !cookie.is_empty(),
            "hasCookie": !cookie.is_empty(),
            "vipType": 0,
            "vipLevel": "none",
            "isVip": false,
            "isSvip": false,
            "vipLabel": "无VIP",
            "statusPending": true,
        })
    }

    // ---------- 首页发现 ----------

    /// 首页推荐（并发请求推荐歌单、私人推荐歌单与每日推荐歌曲）。
    pub async fn discover_home(&self) -> Result<Value, String> {
        let info = self.login_status().await?;
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !logged_in {
            return Ok(json!({
                "loggedIn": false,
                "user": Value::Null,
                "dailySongs": [],
                "dailySongTotal": 0,
                "dailySongsComplete": true,
                "playlists": [],
                "podcasts": [],
                "mode": "starter",
                "updatedAt": unix_millis(),
            }));
        }

        let (res_pers, res_rec, res_songs) = tokio::join!(
            self.weapi_request(
                "/weapi/personalized/playlist",
                json!({ "limit": 8, "total": true, "n": 1000 }),
                None,
                true
            ),
            self.weapi_request(
                "/weapi/v1/discovery/recommend/resource",
                json!({}),
                None,
                true
            ),
            self.weapi_request(
                "/weapi/v3/discovery/recommend/songs",
                json!({}),
                None,
                true
            ),
        );

        let public_playlists: Vec<Value> = res_pers
            .map(|(b, _)| {
                b.get("result")
                    .or_else(|| b.get("data"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|pl| map_discover_playlist(pl, "推荐歌单"))
                            .filter(|pl| {
                                !pl.get("id").is_none_or(|v| v.is_null())
                                    && !pl
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                            })
                            .take(8)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let private_playlists: Vec<Value> = res_rec
            .map(|(b, _)| {
                b.get("recommend")
                    .or_else(|| b.get("data"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|pl| map_discover_playlist(pl, "私人推荐"))
                            .filter(|pl| {
                                !pl.get("id").is_none_or(|v| v.is_null())
                                    && !pl
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .is_empty()
                            })
                            .take(6)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let daily_songs: Vec<Value> = res_songs
            .map(|(b, _)| {
                let raw = b
                    .get("data")
                    .and_then(|d| d.get("dailySongs").or_else(|| d.get("recommend")))
                    .or_else(|| b.get("recommend"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                raw.iter()
                    .map(map_song_record)
                    .filter(|s| {
                        !s.get("id").is_none_or(|v| v.is_null())
                            && !s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .is_empty()
                    })
                    .collect()
            })
            .unwrap_or_default();

        let playlists: Vec<Value> = private_playlists
            .into_iter()
            .chain(public_playlists)
            .take(10)
            .collect();

        Ok(json!({
            "loggedIn": true,
            "user": {
                "userId": info["userId"],
                "nickname": info.get("nickname").and_then(|v| v.as_str()).unwrap_or(""),
                "avatar": info.get("avatar").and_then(|v| v.as_str()).unwrap_or(""),
            },
            "dailySongs": daily_songs,
            "dailySongTotal": daily_songs.len(),
            "dailySongsComplete": true,
            "playlists": playlists,
            "podcasts": [],
            "updatedAt": unix_millis(),
        }))
    }

    // ---------- 播客 ----------

    /// 搜索播客电台 (`type=1009`)。
    pub async fn podcast_search(&self, keywords: &str, limit: usize) -> Result<Value, String> {
        let kw = keywords.trim();
        let lim = limit.clamp(6, 30);
        if kw.is_empty() {
            return Ok(json!({ "podcasts": [] }));
        }
        let (body, _) = self
            .weapi_request(
                "/weapi/cloudsearch/get/web",
                json!({ "s": kw, "type": 1009, "limit": lim, "offset": 0 }),
                None,
                true,
            )
            .await?;
        let empty = json!({});
        let result = body.get("result").unwrap_or(&empty);
        let raw = result
            .get("djRadios")
            .or_else(|| result.get("djradios"))
            .or_else(|| result.get("radios"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let podcasts: Vec<Value> = raw
            .iter()
            .map(map_podcast_radio)
            .filter(|p| !p.get("id").is_none_or(|v| v.is_null()))
            .collect();
        let total = result
            .get("djRadiosCount")
            .or_else(|| result.get("djradiosCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(podcasts.len() as u64);
        Ok(json!({ "podcasts": podcasts, "total": total }))
    }

    /// 热门播客电台。
    pub async fn podcast_hot(&self, limit: usize, offset: usize) -> Result<Value, String> {
        let lim = limit.clamp(6, 30);
        let (body, _) = self
            .weapi_request(
                "/weapi/djradio/hot/v1",
                json!({ "limit": lim, "offset": offset }),
                None,
                true,
            )
            .await?;
        let raw = body
            .get("djRadios")
            .or_else(|| body.get("djradios"))
            .or_else(|| body.get("radios"))
            .or_else(|| body.get("data"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let podcasts: Vec<Value> = raw
            .iter()
            .map(map_podcast_radio)
            .filter(|p| !p.get("id").is_none_or(|v| v.is_null()))
            .collect();
        let more = body
            .get("hasMore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(json!({ "podcasts": podcasts, "more": more }))
    }

    /// 播客电台详情。
    pub async fn podcast_detail(&self, rid: &str) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request("/weapi/djradio/v2/get", json!({ "id": rid }), None, true)
            .await?;
        let src = body
            .get("data")
            .or_else(|| body.get("djRadio"))
            .or_else(|| body.get("radio"))
            .unwrap_or(&body);
        Ok(json!({ "podcast": map_podcast_radio(src) }))
    }

    /// 播客节目列表。
    pub async fn podcast_programs(
        &self,
        rid: &str,
        limit: usize,
        offset: usize,
        asc: bool,
    ) -> Result<Value, String> {
        let lim = limit.clamp(10, 60);
        let (body, _) = self
            .weapi_request(
                "/weapi/dj/program/byradio",
                json!({
                    "radioId": rid,
                    "limit": lim,
                    "offset": offset,
                    "asc": asc,
                }),
                None,
                true,
            )
            .await?;
        let raw = body
            .get("programs")
            .and_then(|v| v.as_array())
            .or_else(|| {
                body.get("data")
                    .and_then(|d| d.get("list").or_else(|| d.get("programs")))
                    .and_then(|v| v.as_array())
            })
            .cloned()
            .unwrap_or_default();
        let radio = raw
            .first()
            .and_then(|p| p.get("radio"))
            .map(map_podcast_radio)
            .unwrap_or_else(|| json!({ "id": rid, "rid": rid }));
        let programs: Vec<Value> = raw
            .iter()
            .map(|p| map_podcast_program(p, &radio))
            .filter(|p| {
                !p.get("id").is_none_or(|v| v.is_null())
                    && !p
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty()
            })
            .collect();
        let more = body.get("more").and_then(|v| v.as_bool()).unwrap_or(false);
        let total = body
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(programs.len() as u64);
        Ok(json!({
            "radio": radio,
            "programs": programs,
            "more": more,
            "total": total,
        }))
    }

    /// 我的播客集合摘要（collect / created / liked）。
    pub async fn podcast_my(&self) -> Result<Value, String> {
        let info = self.login_status().await?;
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let uid = value_id_string(info.get("userId").unwrap_or(&Value::Null));
        if !logged_in || uid.is_empty() {
            let collections: Vec<Value> = ["collect", "created", "liked"]
                .iter()
                .map(|k| podcast_collection_meta(k, &[]))
                .collect();
            return Ok(json!({ "loggedIn": false, "collections": collections }));
        }
        let (c, cr, l) = tokio::join!(
            self.fetch_my_podcast_items("collect", &uid, 12, 0),
            self.fetch_my_podcast_items("created", &uid, 12, 0),
            self.fetch_my_podcast_items("liked", &uid, 12, 0),
        );
        let collections = vec![
            podcast_collection_meta(
                "collect",
                &c.map(|v| v.1).unwrap_or_default(),
            ),
            podcast_collection_meta(
                "created",
                &cr.map(|v| v.1).unwrap_or_default(),
            ),
            podcast_collection_meta(
                "liked",
                &l.map(|v| v.1).unwrap_or_default(),
            ),
        ];
        Ok(json!({ "loggedIn": true, "collections": collections }))
    }

    /// 获取指定分类的我的播客条目。
    pub async fn podcast_my_items(
        &self,
        key: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value, String> {
        let info = self.login_status().await?;
        let logged_in = info
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let uid = value_id_string(info.get("userId").unwrap_or(&Value::Null));
        if !logged_in || uid.is_empty() {
            return Ok(json!({ "loggedIn": false, "items": [] }));
        }
        let (item_type, items) = self.fetch_my_podcast_items(key, &uid, limit, offset).await?;
        let mut meta = podcast_collection_meta(key, &items)
            .as_object()
            .cloned()
            .unwrap_or_default();
        meta.insert("loggedIn".into(), json!(true));
        meta.insert("itemType".into(), json!(item_type));
        meta.insert("items".into(), json!(items));
        Ok(Value::Object(meta))
    }

    async fn fetch_my_podcast_items(
        &self,
        key: &str,
        uid: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(&'static str, Vec<Value>), String> {
        let lim = limit.clamp(8, 60);
        match key {
            "collect" => {
                let (body, _) = self
                    .weapi_request(
                        "/weapi/djradio/get/subed",
                        json!({ "limit": lim, "offset": offset, "total": true }),
                        None,
                        true,
                    )
                    .await?;
                let raw = first_array_from(&body, &["djRadios", "djradios", "radios", "data"]);
                let items = raw
                    .iter()
                    .map(|x| map_podcast_collection_radio(x, key))
                    .filter(|x| !x.get("id").is_none_or(|v| v.is_null()))
                    .collect();
                Ok(("radio", items))
            }
            "created" => {
                let (body, _) = self
                    .weapi_request(
                        "/weapi/djradio/get/byuser",
                        json!({ "uid": uid }),
                        None,
                        true,
                    )
                    .await?;
                let raw = first_array_from(&body, &["data", "djRadios", "djradios", "radios"]);
                let items = raw
                    .iter()
                    .map(|x| map_podcast_collection_radio(x, key))
                    .filter(|x| !x.get("id").is_none_or(|v| v.is_null()))
                    .collect();
                Ok(("radio", items))
            }
            "paid" => {
                let (body, _) = self
                    .weapi_request(
                        "/weapi/djradio/home/paygift/list",
                        json!({ "limit": lim, "offset": offset }),
                        None,
                        true,
                    )
                    .await?;
                let raw = first_array_from(&body, &["data", "djRadios", "djradios", "radios"]);
                let items = raw
                    .iter()
                    .map(|x| map_podcast_collection_radio(x, key))
                    .filter(|x| !x.get("id").is_none_or(|v| v.is_null()))
                    .collect();
                Ok(("radio", items))
            }
            "liked" => {
                let mut raw = self
                    .weapi_request("/weapi/sati/resource/sub/list", json!({}), None, true)
                    .await
                    .map(|(b, _)| first_array_from(&b, &["data", "resources", "list"]))
                    .unwrap_or_default();
                if raw.is_empty() {
                    raw = self
                        .weapi_request(
                            "/weapi/voice/workbench/recent/list",
                            json!({ "limit": lim }),
                            None,
                            true,
                        )
                        .await
                        .map(|(b, _)| first_array_from(&b, &["data", "list", "resources"]))
                        .unwrap_or_default();
                }
                let items = raw
                    .iter()
                    .map(map_podcast_voice)
                    .filter(|x| {
                        !x.get("id").is_none_or(|v| v.is_null())
                            && !x
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .is_empty()
                    })
                    .collect();
                Ok(("voice", items))
            }
            _ => Ok(("radio", Vec::new())),
        }
    }

    // ---------- 听歌上报与统计 ----------

    /// 上报听歌打卡记录（`/weapi/feedback/weblog`）。
    pub async fn scrobble(
        &self,
        id: &str,
        source_id: &str,
        time_sec: u64,
    ) -> Result<Value, String> {
        let logs = serde_json::to_string(&json!([{
            "action": "play",
            "json": {
                "download": 0,
                "end": "playend",
                "id": id,
                "sourceId": source_id,
                "time": time_sec.max(1),
                "type": "song",
                "wifi": 0,
                "source": "list",
            }
        }]))
        .map_err(|e| format!("scrobble encode error: {e}"))?;

        let (body, _) = self
            .weapi_request("/weapi/feedback/weblog", json!({ "logs": logs }), None, true)
            .await?;
        Ok(body)
    }

    /// 查询累计听歌统计数据。
    pub async fn listen_total(&self) -> Result<Value, String> {
        let (body, _) = self
            .weapi_request("/weapi/listen/data/total", json!({}), None, true)
            .await?;
        Ok(json!({
            "provider": "netease",
            "supported": true,
            "readOnly": true,
            "body": body,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapi_encrypt_produces_valid_params_and_256_hex_enc_sec_key() {
        let payload = r#"{"ids":"[12345]","br":320000,"csrf_token":""}"#;
        let fixed_key = *b"0123456789abcdef";
        let enc = weapi_encrypt_with_key(payload, &fixed_key).unwrap();

        assert_eq!(enc.enc_sec_key.len(), 256);
        assert!(enc.enc_sec_key.bytes().all(|b| b.is_ascii_hexdigit()));

        // Reverse-decrypt params with fixed_key then PRESET_KEY to verify CBC double-encryption
        let second_ct = BASE64_STD.decode(&enc.params).unwrap();
        let first_b64_bytes = aes_cbc_decrypt(&second_ct, &fixed_key, WEAPI_IV).unwrap();
        let first_b64_str = String::from_utf8(first_b64_bytes).unwrap();
        let first_ct = BASE64_STD.decode(first_b64_str.trim()).unwrap();
        let plain_bytes = aes_cbc_decrypt(&first_ct, WEAPI_PRESET_KEY, WEAPI_IV).unwrap();
        assert_eq!(String::from_utf8(plain_bytes).unwrap(), payload);

        // Verify randomized weapi_encrypt produces distinct params
        let rand_enc = weapi_encrypt(payload).unwrap();
        assert_eq!(rand_enc.enc_sec_key.len(), 256);
        assert!(!rand_enc.params.is_empty());
    }

    #[test]
    fn eapi_encrypt_produces_uppercase_hex_and_valid_md5_envelope() {
        let url_path = "/api/song/enhance/player/url/v1";
        let text = r#"{"ids":"[1001]","level":"lossless"}"#;
        let encrypted_hex = eapi_encrypt(url_path, text).unwrap();

        assert!(encrypted_hex
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F')));

        let decrypted = eapi_decrypt_hex_or_raw(encrypted_hex.as_bytes()).unwrap();
        let parts: Vec<&str> = decrypted.split("-36cd479b6b5-").collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], url_path);
        assert_eq!(parts[1], text);
        let expected_md5 = md5_hex_lower(format!("nobody{url_path}use{text}md5forencrypt").as_bytes());
        assert_eq!(parts[2], expected_md5);
    }

    #[test]
    fn map_song_record_and_vip_normalization_match_js_contract() {
        let raw = json!({
            "id": 186016,
            "name": "晴天",
            "ar": [{ "id": 6452, "name": "周杰伦" }],
            "al": { "id": 18905, "name": "叶惠美", "picUrl": "https://p1.music.126.net/cover.jpg" },
            "dt": 269000,
            "pop": 100,
            "rank": 1,
            "fee": 1
        });
        let mapped = map_song_record(&raw);
        assert_eq!(mapped["provider"], "netease");
        assert_eq!(mapped["id"], 186016);
        assert_eq!(mapped["name"], "晴天");
        assert_eq!(mapped["artist"], "周杰伦");
        assert_eq!(mapped["album"], "叶惠美");
        assert_eq!(mapped["cover"], "https://p1.music.126.net/cover.jpg");
        assert_eq!(mapped["duration"], 269000);
        assert_eq!(mapped["fee"], 1);

        let vip = normalize_netease_vip(
            &json!({ "vipType": 11 }),
            &json!({}),
            &json!({ "vipExtra": { "data": { "redplus": { "vipLevel": 5, "expireTime": 4_102_444_800_000u64 } } } }),
        );
        assert_eq!(vip["isVip"], true);
        assert_eq!(vip["isSvip"], true);
        assert_eq!(vip["vipLevel"], "svip");
        assert_eq!(vip["vipLabel"], "SVIP");
    }

    #[test]
    fn same_track_candidate_scoring_prefers_same_recording_and_rejects_live_remix() {
        let source = json!({
            "id": 100,
            "name": "七里香",
            "ar": [{ "id": 6452, "name": "周杰伦" }],
            "dt": 299000,
            "h": { "br": 320000, "size": 11960000 }
        });
        let same = json!({
            "id": 200,
            "name": "七里香",
            "ar": [{ "id": 6452, "name": "周杰伦" }],
            "dt": 299100,
            "h": { "br": 320000, "size": 11960000 },
            "privilege": { "pl": 320000, "plLevel": "exhigh" }
        });
        let live = json!({
            "id": 300,
            "name": "七里香 (Live)",
            "ar": [{ "id": 6452, "name": "周杰伦" }],
            "dt": 299000
        });
        assert!(netease_source_match_candidate_score(&source, &same) > 150.0);
        assert!(netease_source_match_candidate_score(&source, &live) < 0.0);
    }

    #[test]
    fn qr_png_data_url_and_cookie_persistence_work() {
        let data_url = render_qr_png_data_url("https://music.163.com/login?codekey=test").unwrap();
        assert!(data_url.starts_with("data:image/png;base64,"));

        let tmp = tempfile::tempdir().unwrap();
        let cookie_path = tmp.path().join(".cookie");
        let client = NeteaseClient::new(Some(cookie_path.clone()));
        client
            .set_cookie("MUSIC_U=secret_token; Path=/; __csrf=abc")
            .unwrap();
        assert!(client.has_cookie());

        let reloaded = NeteaseClient::new(Some(cookie_path));
        assert_eq!(reloaded.get_cookie(), "MUSIC_U=secret_token; __csrf=abc");
    }
}
