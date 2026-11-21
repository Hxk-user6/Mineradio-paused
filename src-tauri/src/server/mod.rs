use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::analyzer::dj::analyze_podcast_dj_stream_with_options;
use crate::config::{
    compare_versions, extract_release_download_pages, extract_release_notes, normalize_version,
    safe_external_update_url, write_json_pretty_atomic, ConfigManager,
};
use crate::cuefield::{
    append_cuefield_feedback, plan_cuefield_transition_from_cache, read_cuefield_feedback_stats,
    TransitionRequest,
};
use crate::desktop::builtin_playlists::BuiltInPlaylistLibrary;
use crate::desktop::easter_egg_gate::LoginEasterEggGate;
use crate::desktop::local_library::{audio_mime_for_path, LocalMusicLibrary};
use crate::desktop::wallpaper_engine::{WallpaperEngineLibrary, WallpaperFileCapability};
use crate::providers::kugou::KugouClient;
use crate::providers::netease::{MatchHints, NeteaseClient};
use crate::providers::qishui::QishuiClient;
use crate::providers::qishui_decryptor::decrypt_qishui_track;
use crate::providers::qq::{QqClient, QqPlaybackHints};
use crate::providers::shared_http_client;

const PROTECTED_LOGIN_ROUTES: &[&str] = &[
    "/api/login/cookie",
    "/api/login/qr/key",
    "/api/login/qr/create",
    "/api/login/qr/check",
    "/api/qq/login/cookie",
    "/api/kugou/login/cookie",
    "/api/qishui/login/qrcode",
    "/api/qishui/login/check",
    "/api/qishui/login/cookie",
];

const QISHUI_DECRYPT_CACHE_MAX_BYTES: usize = 96 * 1024 * 1024;
const PROXY_COVER_MAX_BYTES: u64 = 24 * 1024 * 1024;
const LISTEN_SYNC_JOURNAL_LIMIT: usize = 512;

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct DecryptCache {
    entries: HashMap<String, (Vec<u8>, String)>,
    order: Vec<String>,
    total_bytes: usize,
}

impl DecryptCache {
    fn get(&self, key: &str) -> Option<(Vec<u8>, String)> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: String, buf: Vec<u8>, ct: String) {
        let sz = buf.len();
        if sz > QISHUI_DECRYPT_CACHE_MAX_BYTES {
            return;
        }
        if let Some((old, _)) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.len());
            self.order.retain(|k| k != &key);
        }
        while self.total_bytes + sz > QISHUI_DECRYPT_CACHE_MAX_BYTES && !self.order.is_empty() {
            let evict = self.order.remove(0);
            if let Some((old, _)) = self.entries.remove(&evict) {
                self.total_bytes = self.total_bytes.saturating_sub(old.len());
            }
        }
        self.total_bytes += sz;
        self.order.push(key.clone());
        self.entries.insert(key, (buf, ct));
    }
}

pub struct AppBackendState {
    pub config: Arc<ConfigManager>,
    pub gate: Arc<LoginEasterEggGate>,
    pub builtin_playlists: Arc<BuiltInPlaylistLibrary>,
    pub local_library: Arc<LocalMusicLibrary>,
    pub wallpaper_engine: Arc<WallpaperEngineLibrary>,
    pub netease: Arc<NeteaseClient>,
    pub qq: Arc<QqClient>,
    pub kugou: Arc<KugouClient>,
    pub qishui: Arc<QishuiClient>,
    decrypt_cache: Mutex<DecryptCache>,
    listen_journal: Mutex<Value>,
}

impl AppBackendState {
    pub fn new(config: Arc<ConfigManager>) -> Arc<Self> {
        let user_data = config.paths().user_data_dir.clone();
        let app_root = config.paths().app_root.clone();

        let netease_cookie = user_data.join(".cookie");
        let qq_cookie = user_data.join(".qq-cookie");
        let kugou_cookie = user_data.join(".kugou-cookie");
        let qishui_cookie = user_data.join(".qishui-cookie");
        let qishui_token = user_data.join(".qishui-token");
        let qishui_qr = user_data.join(".qishui-qr-login.json");

        let netease = Arc::new(NeteaseClient::new(Some(netease_cookie)));
        let qq = Arc::new(QqClient::new(Some(qq_cookie)));
        let kugou = Arc::new(KugouClient::new(Some(kugou_cookie)));
        let qishui = Arc::new(QishuiClient::new(
            Some(qishui_cookie),
            Some(qishui_token),
            Some(qishui_qr),
        ));

        let gate = Arc::new(LoginEasterEggGate::new(
            &user_data,
            vec![app_root.clone()],
        ));
        let n_ref = netease.clone();
        let qq_ref = qq.clone();
        let kg_ref = kugou.clone();
        let qs_ref = qishui.clone();
        gate.initialize(|| {
            let _ = n_ref.set_cookie("");
            let _ = qq_ref.set_cookie("");
            let _ = kg_ref.set_cookie("");
            let _ = qs_ref.set_cookie("");
            let _ = qs_ref.clear_access_token();
            Ok(())
        });

        let listen_journal = std::fs::read_to_string(&config.paths().listen_sync_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({ "version": 1, "entries": {} }));

        Arc::new(Self {
            builtin_playlists: Arc::new(BuiltInPlaylistLibrary::new(&user_data)),
            local_library: Arc::new(LocalMusicLibrary::new(&user_data)),
            wallpaper_engine: Arc::new(WallpaperEngineLibrary::new(&user_data)),
            config,
            gate,
            netease,
            qq,
            kugou,
            qishui,
            decrypt_cache: Mutex::new(DecryptCache::default()),
            listen_journal: Mutex::new(listen_journal),
        })
    }

    pub fn clear_all_credentials(&self) {
        let _ = self.netease.set_cookie("");
        let _ = self.qq.set_cookie("");
        let _ = self.kugou.set_cookie("");
        let _ = self.qishui.set_cookie("");
        let _ = self.qishui.clear_access_token();
        let _ = self.gate.clear_credential_files();
    }
}

fn json_status(status: StatusCode, val: Value) -> Response {
    (status, Json(val)).into_response()
}

fn ok_json(val: Value) -> Response {
    (StatusCode::OK, Json(val)).into_response()
}

fn err_json(status: StatusCode, err: impl Into<String>) -> Response {
    let msg = err.into();
    (
        status,
        Json(json!({ "ok": false, "error": msg, "message": msg })),
    )
        .into_response()
}

fn json_id_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    if f.fract().abs() < 1e-9 {
                        format!("{:.0}", f)
                    } else {
                        f.to_string()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        Some(Value::Object(obj)) => {
            for key in [
                "id",
                "songId",
                "trackId",
                "playlistId",
                "albumId",
                "cid",
                "commentId",
                "listId",
                "pid",
                "global_collection_id",
                "mid",
                "hash",
                "fileHash",
                "value",
            ] {
                if let Some(inner) = obj.get(key) {
                    let s = json_id_str(Some(inner));
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
            String::new()
        }
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|item| json_id_str(Some(item)))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(","),
        _ => String::new(),
    }
}

fn json_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| i != 0)
            .or_else(|| n.as_f64().map(|f| f.is_finite() && f != 0.0))
            .unwrap_or(default),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

fn json_f64(v: Option<&Value>, default: f64) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(default),
        Some(Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .unwrap_or(default),
        _ => default,
    }
}

fn parse_action_toggle(v: Option<&Value>, default: bool) -> bool {
    let Some(val) = v else {
        return default;
    };
    let s = json_id_str(Some(val)).to_ascii_lowercase();
    match s.as_str() {
        "1" | "true" => true,
        "0" | "2" | "false" => false,
        _ => json_bool(Some(val), default),
    }
}

fn merge_query_and_body(q: &HashMap<String, String>, body: &Bytes) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in q {
        map.insert(k.clone(), Value::String(v.clone()));
    }
    if !body.is_empty() {
        if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(body) {
            for (k, v) in obj {
                if !v.is_null() || !map.contains_key(&k) {
                    map.insert(k, v);
                }
            }
        } else {
            for (k, v) in url::form_urlencoded::parse(body) {
                map.insert(k.into_owned(), Value::String(v.into_owned()));
            }
        }
    }
    Value::Object(map)
}

async fn easter_egg_gate_middleware(
    State(state): State<Arc<AppBackendState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if PROTECTED_LOGIN_ROUTES.contains(&path) && !state.gate.is_unlocked() {
        return json_status(
            StatusCode::LOCKED,
            json!({
                "ok": false,
                "unlocked": false,
                "error": "LOGIN_EASTER_EGG_LOCKED",
                "message": "请先完成登录彩蛋解锁。",
            }),
        );
    }
    next.run(req).await
}

pub fn build_router(state: Arc<AppBackendState>) -> Router {
    Router::new()
        .route("/api/spotify", any(spotify_removed))
        .route("/api/spotify/{*rest}", any(spotify_removed))
        .route("/api/app/version", get(app_version))
        .route("/api/platform/capabilities", get(platform_capabilities))
        .route("/api/update/latest", get(update_latest))
        .route("/api/update/download", any(update_external_only))
        .route("/api/update/download/status", any(update_external_only))
        .route("/api/update/patch", any(update_external_only))
        .route("/api/update/patch/status", any(update_external_only))
        .route("/api/beatmap/cache/status", get(beatmap_cache_status))
        .route("/api/beatmap/cache", get(beatmap_cache_get).post(beatmap_cache_post))
        .route("/api/cuefield/transition", post(cuefield_transition))
        .route("/api/cuefield/feedback", get(cuefield_feedback_get).post(cuefield_feedback_post))
        .route("/api/podcast/dj-beatmap", any(podcast_dj_beatmap))
        .route("/api/listen/report", post(listen_report))
        .route("/api/listen/total", get(listen_total))
        .route("/api/discover/home", get(discover_home))
        .route("/api/discover", get(discover_home))
        .route("/api/weather/radio", get(weather_radio))
        .route("/api/weather-radio", get(weather_radio))
        .route("/api/weather/ip-location", get(weather_ip_location))
        // Netease
        .route("/api/search", get(netease_search))
        .route("/api/song/url", get(netease_song_url))
        .route("/api/lyric", get(netease_lyric))
        .route("/api/user/playlists", get(netease_user_playlists))
        .route("/api/playlist/tracks", get(netease_playlist_tracks))
        .route("/api/playlist/create", any(netease_playlist_create))
        .route("/api/playlist/add-song", any(netease_playlist_add_song))
        .route("/api/playlist/subscribe", any(netease_playlist_subscribe))
        .route("/api/album/detail", get(netease_album_detail))
        .route("/api/album/subscribe", any(netease_album_subscribe))
        .route("/api/album/subscribe/check", get(netease_album_subscribe_check))
        .route("/api/artist/detail", get(netease_artist_detail))
        .route("/api/song/comments", get(netease_song_comments).post(netease_post_comment))
        .route("/api/song/comments/like", any(netease_comment_like))
        .route("/api/song/like", any(netease_song_like))
        .route("/api/like", any(netease_song_like))
        .route("/api/song/like/check", get(netease_song_like_check))
        .route("/api/podcast/search", get(netease_podcast_search))
        .route("/api/podcast/hot", get(netease_podcast_hot))
        .route("/api/podcast/detail", get(netease_podcast_detail))
        .route("/api/podcast/programs", get(netease_podcast_programs))
        .route("/api/podcast/my", get(netease_podcast_my))
        .route("/api/podcast/my/items", get(netease_podcast_my_items))
        .route("/api/login/status", get(netease_login_status))
        .route("/api/login/cookie", post(netease_login_cookie))
        .route("/api/login/qr/key", get(netease_login_qr_key))
        .route("/api/login/qr/create", get(netease_login_qr_create))
        .route("/api/login/qr/check", get(netease_login_qr_check))
        .route("/api/logout", post(netease_logout).get(netease_logout))
        // QQ
        .route("/api/qq/search", get(qq_search))
        .route("/api/qq/song/url", get(qq_song_url))
        .route("/api/qq/lyric", get(qq_lyric))
        .route("/api/qq/login/status", get(qq_login_status))
        .route("/api/qq/login/cookie", post(qq_login_cookie))
        .route("/api/qq/logout", post(qq_logout).get(qq_logout))
        .route("/api/qq/user/playlists", get(qq_user_playlists))
        .route("/api/qq/playlist/tracks", get(qq_playlist_tracks))
        .route("/api/qq/artist/detail", get(qq_artist_detail))
        .route("/api/qq/album/detail", get(qq_album_detail))
        .route("/api/qq/song/comments", get(qq_song_comments))
        .route("/api/qq/like", post(qq_like_unsupported))
        // Kugou
        .route("/api/kugou/search", get(kugou_search))
        .route("/api/kugou/recommendations", get(kugou_recommendations))
        .route("/api/kugou/song/url", get(kugou_song_url))
        .route("/api/kugou/lyric", get(kugou_lyric))
        .route("/api/kugou/login/status", get(kugou_login_status))
        .route("/api/kugou/login/cookie", post(kugou_login_cookie))
        .route("/api/kugou/logout", post(kugou_logout).get(kugou_logout))
        .route("/api/kugou/user/playlists", get(kugou_user_playlists))
        .route("/api/kugou/playlist/tracks", get(kugou_playlist_tracks))
        .route("/api/kugou/song/like/check", get(kugou_like_check))
        .route("/api/kugou/song/like", post(kugou_like_toggle))
        .route("/api/kugou/playlist/add-song", any(kugou_playlist_add_song))
        // Qishui
        .route("/api/qishui/search", get(qishui_search))
        .route("/api/qishui/feed", get(qishui_feed))
        .route("/api/qishui/status", get(qishui_status))
        .route("/api/qishui/login/status", get(qishui_status))
        .route("/api/qishui/login/cookie", post(qishui_login_cookie))
        .route("/api/qishui/logout", post(qishui_logout).get(qishui_logout))
        .route("/api/qishui/login/qrcode", get(qishui_login_qrcode).post(qishui_login_qrcode))
        .route("/api/qishui/login/check", get(qishui_login_check))
        .route("/api/qishui/user/playlists", get(qishui_user_playlists))
        .route("/api/qishui/playlist/tracks", get(qishui_playlist_tracks))
        .route("/api/qishui/song/like/check", get(qishui_like_check))
        .route("/api/qishui/song/like", post(qishui_song_like))
        .route("/api/qishui/playlist/collect", post(qishui_playlist_collect))
        .route("/api/qishui/playlist/add-song", post(qishui_playlist_add_song))
        .route("/api/qishui/album/collect", post(qishui_album_collect))
        .route("/api/qishui/song/comments", get(qishui_comments).post(qishui_create_comment))
        .route("/api/qishui/song/url", get(qishui_song_url))
        .route("/api/qishui/lyric", get(qishui_lyric))
        // Proxy & Local media
        .route("/api/cover", get(proxy_cover))
        .route("/api/audio", get(proxy_audio))
        .route("/local-media/{kind}/{id}", get(serve_local_media))
        .route("/we-media/file/{id}", get(serve_we_media))
        .route("/we-media/file/{id}/{*rel}", get(serve_we_media_nested))
        .route("/qishui-auth-proxy/{name}", get(serve_qishui_auth_asset))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            easter_egg_gate_middleware,
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|origin, _| {
                    is_allowed_cors_origin(origin)
                }))
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    header::RANGE,
                    header::ACCEPT,
                ])
                .expose_headers([
                    header::CONTENT_LENGTH,
                    header::CONTENT_RANGE,
                    header::ACCEPT_RANGES,
                    header::CONTENT_TYPE,
                ]),
        )
        .with_state(state)
}

fn is_allowed_cors_origin(origin: &HeaderValue) -> bool {
    let Ok(raw) = origin.to_str() else {
        return false;
    };
    let Ok(parsed) = url::Url::parse(raw.trim()) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host_lower = host.to_ascii_lowercase();
    match parsed.scheme() {
        "tauri" | "asset" => host_lower == "localhost",
        "http" | "https" => {
            host_lower == "localhost"
                || host_lower == "127.0.0.1"
                || host_lower == "::1"
                || host_lower == "[::1]"
                || host_lower == "tauri.localhost"
                || host_lower.ends_with(".localhost")
        }
        _ => false,
    }
}

async fn qq_like_unsupported() -> Response {
    json_status(
        StatusCode::NOT_IMPLEMENTED,
        json!({
            "ok": false,
            "unsupported": true,
            "code": "QQ_LIKE_UNSUPPORTED",
            "error": "QQ音乐暂不支持接口红心收藏，请前往QQ音乐客户端操作",
            "message": "QQ音乐暂不支持接口红心收藏，请前往QQ音乐客户端操作",
        }),
    )
}

pub async fn start_local_server(state: Arc<AppBackendState>) -> Result<u16, String> {
    let preferred = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(3000);
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", preferred)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("Failed to bind local server: {e}"))?,
    };
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read bound port: {e}"))?
        .port();
    let app = build_router(state);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[Mineradio Axum Server] error: {e}");
        }
    });
    Ok(port)
}

// ---------- 核心路由 ----------

async fn spotify_removed() -> Response {
    json_status(
        StatusCode::NOT_FOUND,
        json!({
            "ok": false,
            "error": "PROVIDER_REMOVED",
            "message": "该平台接口已从 Mineradio 移除。",
        }),
    )
}

async fn update_external_only() -> Response {
    json_status(
        StatusCode::GONE,
        json!({
            "ok": false,
            "externalOnly": true,
            "error": "UPDATE_EXTERNAL_ONLY",
            "message": "Mineradio 已停用客户端本地下载与快速补丁，请使用外部下载页面。",
        }),
    )
}

async fn app_version(State(state): State<Arc<AppBackendState>>) -> Response {
    let paths = state.config.paths();
    ok_json(json!({
        "name": "mineradio",
        "productName": paths.runtime_name,
        "version": paths.app_version,
        "update": {
            "provider": "github",
            "configured": true,
            "owner": "XxHuberrr",
            "repo": "Mineradio",
            "preview": false,
            "manifestOverride": !paths.update_manifest.is_empty(),
        }
    }))
}

async fn platform_capabilities(State(state): State<Arc<AppBackendState>>) -> Response {
    let qs_logged = crate::providers::qishui::qishui_cookie_has_login(&state.qishui.get_cookie());
    ok_json(json!({
        "netease": {
            "playlists": true, "likeRead": true, "likeWrite": true, "albumRead": true,
            "albumCollect": true, "commentsRead": true, "commentsWrite": true,
            "listenReport": "experimental-unverified",
        },
        "qq": {
            "playlists": true, "likeRead": true, "likeWrite": false, "albumRead": true,
            "albumCollect": false, "commentsRead": true, "commentsWrite": false,
            "listenReport": false,
        },
        "kugou": {
            "playlists": true, "likeRead": true, "likeWrite": true, "albumRead": false,
            "albumCollect": false, "commentsRead": false, "commentsWrite": false,
            "listenReport": false,
        },
        "qishui": {
            "playlists": true, "likeRead": true, "likeWrite": qs_logged,
            "albumRead": false, "albumCollect": qs_logged,
            "commentsRead": qs_logged, "commentsWrite": qs_logged,
            "recentPlayReport": qs_logged, "listenReport": false,
        },
    }))
}

async fn update_latest(State(state): State<Arc<AppBackendState>>) -> Response {
    let current = state.config.paths().app_version.clone();
    let manifest_ref = state.config.paths().update_manifest.clone();

    if !manifest_ref.is_empty() {
        let raw_text = if manifest_ref.starts_with("http://") || manifest_ref.starts_with("https://") {
            shared_http_client()
                .get(&manifest_ref)
                .send()
                .await
                .ok()
                .and_then(|r| if r.status().is_success() { Some(r) } else { None })
        } else {
            None
        };
        let manifest_json: Option<Value> = if let Some(resp) = raw_text {
            resp.json().await.ok()
        } else {
            std::fs::read_to_string(&manifest_ref)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        };
        if let Some(data) = manifest_json {
            return ok_json(format_github_release(&current, &data));
        }
    }

    let api_url = "https://api.github.com/repos/XxHuberrr/Mineradio/releases/latest";
    if let Ok(resp) = shared_http_client()
        .get(api_url)
        .header("User-Agent", format!("Mineradio/{current}"))
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_millis(8500))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<Value>().await {
                return ok_json(format_github_release(&current, &data));
            }
        }
    }

    // Fallback to latest.yml
    let yml_candidates = [
        "https://gh.llkk.cc/https://github.com/XxHuberrr/Mineradio/releases/latest/download/latest.yml",
        "https://ghfast.top/https://github.com/XxHuberrr/Mineradio/releases/latest/download/latest.yml",
        "https://github.com/XxHuberrr/Mineradio/releases/latest/download/latest.yml",
    ];
    for candidate in yml_candidates {
        if let Ok(resp) = shared_http_client()
            .get(candidate)
            .timeout(std::time::Duration::from_millis(5500))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    if let Ok(yml) = serde_yaml::from_str::<Value>(&text) {
                        let ver = yml
                            .get("version")
                            .and_then(|v| v.as_str())
                            .map(normalize_version)
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| current.clone());
                        let html_url = format!("https://github.com/XxHuberrr/Mineradio/releases/tag/v{ver}");
                        return ok_json(json!({
                            "configured": true,
                            "preview": false,
                            "updateAvailable": compare_versions(&ver, &current) > 0,
                            "currentVersion": current,
                            "latestVersion": ver,
                            "source": "latest-yml",
                            "release": {
                                "tagName": format!("v{ver}"),
                                "name": format!("Mineradio v{ver}"),
                                "version": ver,
                                "publishedAt": yml.get("releaseDate").and_then(|v| v.as_str()).unwrap_or(""),
                                "htmlUrl": html_url,
                                "downloadPageUrl": html_url,
                                "downloadPages": [],
                                "summary": "发现新版本，请前往发布页面获取安装包。",
                                "notes": ["更新入口已改为浏览器外部下载", "Mineradio 不再在本地下载或应用补丁"],
                            }
                        }));
                    }
                }
            }
        }
    }

    ok_json(json!({
        "configured": true,
        "preview": false,
        "updateAvailable": false,
        "currentVersion": current,
        "latestVersion": current,
        "release": Value::Null,
    }))
}

fn format_github_release(current: &str, data: &Value) -> Value {
    let raw_tag = data
        .get("tag_name")
        .or_else(|| data.get("version"))
        .or_else(|| data.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(current);
    let latest = {
        let n = normalize_version(raw_tag);
        if n.is_empty() { current.to_string() } else { n }
    };
    let html_url = data
        .get("html_url")
        .and_then(|v| v.as_str())
        .and_then(safe_external_update_url)
        .unwrap_or_default();
    let body = data.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let pages = extract_release_download_pages(body);
    let notes = extract_release_notes(body);
    let first_ext = pages
        .first()
        .map(|p| p.url.clone())
        .unwrap_or_default();
    let dl_page = if !first_ext.is_empty() {
        first_ext.clone()
    } else {
        html_url.clone()
    };
    json!({
        "configured": true,
        "preview": false,
        "updateAvailable": compare_versions(&latest, current) > 0,
        "currentVersion": current,
        "latestVersion": latest,
        "release": {
            "tagName": data.get("tag_name").and_then(|v| v.as_str()).unwrap_or(&format!("v{latest}")),
            "name": data.get("name").and_then(|v| v.as_str()).unwrap_or(&format!("Mineradio v{latest}")),
            "version": latest,
            "publishedAt": data.get("published_at").and_then(|v| v.as_str()).unwrap_or(""),
            "htmlUrl": html_url,
            "externalUrl": first_ext,
            "downloadPageUrl": dl_page,
            "downloadPages": pages,
            "downloadUrl": first_ext,
            "asset": Value::Null,
            "patch": Value::Null,
            "patchAvailable": false,
            "summary": notes.first().cloned().unwrap_or_else(|| "发现新版本，建议更新。".to_string()),
            "notes": if notes.is_empty() {
                vec!["本次下载入口已更换，请使用公告中的新网盘链接。".to_string()]
            } else {
                notes
            },
        }
    })
}

async fn beatmap_cache_status(State(state): State<Arc<AppBackendState>>) -> Response {
    let info = state.config.beat_cache_root_info();
    let reason = if !info.allowed {
        Some("BEAT_CACHE_ON_C_DRIVE_DISABLED")
    } else if !info.available {
        Some("BEAT_CACHE_DRIVE_UNAVAILABLE")
    } else {
        None
    };
    let mode = if info.allowed && info.available {
        "disk"
    } else {
        "memory-only"
    };
    ok_json(json!({
        "enabled": info.allowed && info.available,
        "dir": info.dir,
        "drive": info.drive,
        "reason": reason,
        "mode": mode,
    }))
}

async fn beatmap_cache_get(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = q.get("key").cloned().unwrap_or_default();
    match state.config.read_beatmap_cache(&key) {
        Ok(Some(entry)) => ok_json(json!({
            "ok": true,
            "hit": true,
            "key": entry.key,
            "map": entry.map,
            "meta": entry.meta,
            "savedAt": entry.saved_at,
        })),
        Ok(None) => ok_json(json!({ "ok": true, "hit": false, "key": key })),
        Err(err) => ok_json(json!({
            "ok": false,
            "hit": false,
            "enabled": false,
            "mode": "memory-only",
            "key": key,
            "reason": err.code(),
        })),
    }
}

fn extract_song_artist_text(song: &serde_json::Map<String, Value>) -> String {
    if let Some(s) = song.get("artist").and_then(|v| v.as_str()) {
        return s.trim().to_string();
    }
    let arr = song
        .get("artists")
        .or_else(|| song.get("ar"))
        .or_else(|| song.get("artist"))
        .and_then(|v| v.as_array());
    if let Some(items) = arr {
        let names: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Object(obj) => obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect();
        return names.join(" / ");
    }
    String::new()
}

fn normalize_beatmap_cache_payload(body: &Value) -> Value {
    let key = body
        .get("key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let map_val = body
        .get("map")
        .or_else(|| body.get("beatMap"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(Value::Null);
    let meta_obj = body.get("meta").and_then(|v| v.as_object());
    let song_obj = body.get("song").and_then(|v| v.as_object());

    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .or_else(|| meta_obj.and_then(|m| m.get("provider")).and_then(|v| v.as_str()))
        .or_else(|| {
            song_obj
                .and_then(|s| s.get("provider").or_else(|| s.get("source")))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    let title = body
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| meta_obj.and_then(|m| m.get("title")).and_then(|v| v.as_str()))
        .or_else(|| {
            song_obj
                .and_then(|s| s.get("title").or_else(|| s.get("name")))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    let artist = body
        .get("artist")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            meta_obj
                .and_then(|m| m.get("artist"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .or_else(|| song_obj.map(extract_song_artist_text))
        .unwrap_or_default();
    let mode = body
        .get("mode")
        .and_then(|v| v.as_str())
        .or_else(|| meta_obj.and_then(|m| m.get("mode")).and_then(|v| v.as_str()))
        .unwrap_or("mr");

    json!({
        "key": key,
        "provider": provider,
        "title": title,
        "artist": artist,
        "mode": mode,
        "map": map_val,
    })
}

async fn beatmap_cache_post(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let normalized = normalize_beatmap_cache_payload(&body);
    match state.config.write_beatmap_cache(&normalized) {
        Ok(res) => ok_json(json!(res)),
        Err(err) => ok_json(json!({
            "ok": false,
            "enabled": false,
            "mode": "memory-only",
            "reason": err.code(),
        })),
    }
}

async fn cuefield_transition(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let from_key = body.get("fromKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let to_key = body.get("toKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from_lrc = body.get("fromLrc").and_then(|v| v.as_str()).map(str::to_string);
    let to_lrc = body.get("toLrc").and_then(|v| v.as_str()).map(str::to_string);
    let exit_bias = body.get("exitBias").and_then(|v| v.as_str()).unwrap_or("late");
    let max_entry_time = body
        .get("maxEntryTime")
        .and_then(|v| v.as_f64())
        .unwrap_or(32.0)
        .clamp(8.0, 32.0);
    let recent_recipes: Vec<String> = body
        .get("recentRecipes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        })
        .unwrap_or_default();
    let minimum_listen_until = body.get("minimumListenUntil").and_then(|v| v.as_f64());
    let enable_live = body
        .get("enableLiveEndCrossfadeFallback")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let enable_cadence = body
        .get("enableCadenceFallback")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tail_evidence = body.get("tailEvidence").cloned();
    let boundary_evidence: Vec<Value> = body
        .get("boundaryEvidence")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let req = TransitionRequest {
        from_key: &from_key,
        to_key: &to_key,
        from_lrc: from_lrc.as_deref(),
        to_lrc: to_lrc.as_deref(),
        exit_bias: Some(exit_bias),
        max_entry_time: Some(max_entry_time),
        recent_recipes,
        minimum_listen_until,
        enable_clean_boundary: body
            .get("enableCleanBoundary")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        enable_live_end_crossfade_fallback: enable_live,
        enable_cadence_fallback: enable_cadence,
        boundary_evidence,
        tail_evidence,
        synthetic_bridge_enabled: body
            .get("syntheticBridgeEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        version: body.get("version").cloned(),
    };

    match plan_cuefield_transition_from_cache(&req, |key| {
        state
            .config
            .read_beatmap_cache(key)
            .ok()
            .flatten()
            .and_then(|e| serde_json::to_value(e).ok())
    }) {
        Ok(plan) => ok_json(plan),
        Err(code) => err_json(StatusCode::BAD_REQUEST, code),
    }
}

async fn cuefield_feedback_get(State(state): State<Arc<AppBackendState>>) -> Response {
    let file = state
        .config
        .paths()
        .user_data_dir
        .join("cuefield-feedback.jsonl");
    match read_cuefield_feedback_stats(&file) {
        Ok(stats) => ok_json(json!({ "ok": true, "stats": stats })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn cuefield_feedback_post(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let file = state
        .config
        .paths()
        .user_data_dir
        .join("cuefield-feedback.jsonl");
    match append_cuefield_feedback(&file, &body) {
        Ok(record) => ok_json(json!({ "ok": true, "record": record })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, e),
    }
}

async fn podcast_dj_beatmap(
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let url = params
        .get("url")
        .or_else(|| params.get("audioUrl"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let duration_sec = json_f64(
        params.get("duration").or_else(|| params.get("durationSec")),
        0.0,
    )
    .max(0.0);
    let intro_only = match params.get("intro") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f > 0.0),
        Some(Value::String(s)) => {
            let trimmed = s.trim().to_ascii_lowercase();
            if let Ok(num) = trimmed.parse::<f64>() {
                num > 0.0
            } else {
                matches!(trimmed.as_str(), "true" | "yes" | "on")
            }
        }
        _ => false,
    };
    if url.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "MISSING_AUDIO_URL");
    }
    match analyze_podcast_dj_stream_with_options(url, duration_sec, intro_only).await {
        Ok(map) => ok_json(json!({ "ok": true, "map": map })),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

fn extract_listen_report_song_id(provider: &str, song: Option<&serde_json::Map<String, Value>>) -> String {
    let Some(s) = song else {
        return String::new();
    };
    match provider {
        "qq" => json_id_str(s.get("mid").or_else(|| s.get("songmid")).or_else(|| s.get("id"))),
        "kugou" => json_id_str(s.get("hash").or_else(|| s.get("fileHash")).or_else(|| s.get("id"))),
        _ => json_id_str(s.get("id").or_else(|| s.get("providerSongId"))),
    }
}

async fn listen_report(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let song_obj = body.get("song").and_then(|v| v.as_object());
    let provider_raw = body
        .get("provider")
        .and_then(|v| v.as_str())
        .or_else(|| {
            song_obj.and_then(|s| {
                s.get("provider")
                    .or_else(|| s.get("source"))
                    .or_else(|| s.get("sourceKey"))
                    .or_else(|| s.get("type"))
                    .or_else(|| s.get("resolvedPlaybackProvider"))
                    .and_then(|v| v.as_str())
            })
        })
        .unwrap_or("netease");
    let provider = provider_raw.trim().to_ascii_lowercase();

    let mut song_id = json_id_str(body.get("id"));
    if song_id.is_empty() {
        song_id = extract_listen_report_song_id(&provider, song_obj);
    }
    if song_id.is_empty() {
        return ok_json(json!({
            "accepted": false,
            "platformSubmitted": false,
            "eligible": false,
            "provider": provider,
            "reason": "MISSING_SONG_ID",
        }));
    }

    let ineligible_song = song_obj.is_some_and(|s| {
        let typ = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let src = s.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let trial = s.get("trial").and_then(|v| v.as_bool()).unwrap_or(false);
        typ == "local" || typ == "podcast" || src == "podcast" || trial
    });

    let has_listen_ms = body.get("listenMs").is_some();
    let listen_ms = if has_listen_ms {
        json_f64(body.get("listenMs"), 0.0)
            .clamp(0.0, 12.0 * 3600.0 * 1000.0)
            .round() as u64
    } else {
        (json_f64(body.get("time"), 0.0).clamp(0.0, 12.0 * 3600.0) * 1000.0).round() as u64
    };
    let duration_ms = json_f64(body.get("durationMs"), 0.0)
        .clamp(0.0, 12.0 * 3600.0 * 1000.0)
        .round() as u64;
    let capped_listen_ms = if duration_ms > 0 {
        listen_ms.min(duration_ms + 2500)
    } else {
        listen_ms
    };
    let required_ms = if duration_ms > 0 {
        let calc = if duration_ms <= 30_000 {
            (duration_ms as f64 * 0.8).ceil() as u64
        } else {
            ((duration_ms as f64 * 0.5).ceil() as u64).min(30_000)
        };
        calc.max(5_000)
    } else if has_listen_ms {
        30_000
    } else {
        5_000
    };

    if ineligible_song || capped_listen_ms < required_ms {
        return ok_json(json!({
            "accepted": false,
            "platformSubmitted": false,
            "eligible": false,
            "provider": provider,
            "songId": song_id,
            "listenMs": capped_listen_ms,
            "durationMs": duration_ms,
            "requiredMs": required_ms,
            "reason": "LISTEN_REPORT_NOT_ELIGIBLE",
        }));
    }

    if provider != "netease" && provider != "qishui" {
        return ok_json(json!({
            "accepted": false,
            "platformSubmitted": false,
            "eligible": true,
            "provider": provider,
            "songId": song_id,
            "reason": "UNSUPPORTED_PROVIDER",
        }));
    }

    let ctx_obj = body.get("context").and_then(|v| v.as_object());
    let mut source_id = json_id_str(body.get("sourceId"));
    if source_id.is_empty() {
        if let Some(ctx) = ctx_obj {
            source_id = json_id_str(
                ctx.get("playlistId")
                    .or_else(|| ctx.get("id"))
                    .or_else(|| ctx.get("sourceId")),
            );
        }
    }
    let time_sec = if body.get("time").is_some() {
        (json_f64(body.get("time"), 0.0).round() as u64).max(1)
    } else {
        (capped_listen_ms / 1000).max(1)
    };
    let raw_session = json_id_str(body.get("sessionId"));
    let session_id = if raw_session.is_empty() {
        song_id.clone()
    } else {
        raw_session
    };

    let journal_key = format!("{provider}:{session_id}");
    {
        let journal = lock_unpoisoned(&state.listen_journal);
        if let Some(prev) = journal
            .get("entries")
            .and_then(|e| e.get(&journal_key))
            .filter(|p| p.get("ok").and_then(|v| v.as_bool()) == Some(true))
        {
            return ok_json(json!({
                "accepted": true,
                "duplicate": true,
                "platformSubmitted": true,
                "eligible": true,
                "provider": provider,
                "songId": song_id,
                "sessionId": session_id,
                "previous": prev,
            }));
        }
    }

    let res = if provider == "netease" {
        state.netease.scrobble(&song_id, &source_id, time_sec).await
    } else {
        state.qishui.report_recently_played(&song_id).await
    };
    let submitted = res.is_ok();
    {
        let mut journal = lock_unpoisoned(&state.listen_journal);
        if let Some(entries) = journal.get_mut("entries").and_then(|e| e.as_object_mut()) {
            entries.insert(
                journal_key,
                json!({
                    "provider": provider,
                    "submittedAt": chrono::Utc::now().timestamp_millis(),
                    "songId": song_id,
                    "time": time_sec,
                    "listenMs": capped_listen_ms,
                    "ok": submitted,
                }),
            );
            if entries.len() > LISTEN_SYNC_JOURNAL_LIMIT {
                let first_key = entries.keys().next().cloned();
                if let Some(k) = first_key {
                    entries.remove(&k);
                }
            }
        }
        let _ = write_json_pretty_atomic(&state.config.paths().listen_sync_file, &*journal);
    }
    match res {
        Ok(val) => ok_json(json!({
            "accepted": true,
            "platformSubmitted": true,
            "eligible": true,
            "provider": provider,
            "songId": song_id,
            "sessionId": session_id,
            "listenMs": capped_listen_ms,
            "durationMs": duration_ms,
            "body": val,
        })),
        Err(e) => ok_json(json!({
            "accepted": false,
            "platformSubmitted": false,
            "eligible": true,
            "provider": provider,
            "songId": song_id,
            "error": e,
        })),
    }
}

async fn listen_total(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let provider = q.get("provider").map(String::as_str).unwrap_or("netease");
    if provider != "netease" {
        return ok_json(json!({ "provider": provider, "supported": false, "accountDurationSync": "unsupported" }));
    }
    match state.netease.listen_total().await {
        Ok(body) => ok_json(json!({ "provider": "netease", "supported": true, "readOnly": true, "body": body })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn discover_home(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.netease.discover_home().await {
        Ok(v) => ok_json(v),
        Err(e) => json_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e, "loggedIn": false, "dailySongs": [], "playlists": [], "podcasts": [] }),
        ),
    }
}

async fn weather_ip_location() -> Response {
    match shared_http_client()
        .get("http://ip-api.com/json/?lang=zh-CN")
        .timeout(std::time::Duration::from_millis(4500))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<Value>().await {
                let loc = json!({
                    "name": data.get("city").and_then(|v| v.as_str()).unwrap_or("上海"),
                    "country": data.get("country").and_then(|v| v.as_str()).unwrap_or("China"),
                    "latitude": data.get("lat").and_then(|v| v.as_f64()).unwrap_or(31.2304),
                    "longitude": data.get("lon").and_then(|v| v.as_f64()).unwrap_or(121.4737),
                    "timezone": data.get("timezone").and_then(|v| v.as_str()).unwrap_or("Asia/Shanghai"),
                });
                return ok_json(json!({ "ok": true, "location": loc }));
            }
        }
        _ => {}
    }
    ok_json(json!({
        "ok": true,
        "location": { "name": "上海", "country": "China", "latitude": 31.2304, "longitude": 121.4737, "timezone": "Asia/Shanghai" }
    }))
}

async fn weather_radio(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let lat = q.get("lat").and_then(|s| s.parse::<f64>().ok()).unwrap_or(31.2304);
    let lon = q.get("lon").and_then(|s| s.parse::<f64>().ok()).unwrap_or(121.4737);
    let city = q.get("city").or_else(|| q.get("q")).cloned().unwrap_or_else(|| "上海".to_string());

    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day&timezone=auto"
    );
    let mut temp = 22.0;
    let mut code = 0i64;
    if let Ok(resp) = shared_http_client()
        .get(&url)
        .timeout(std::time::Duration::from_millis(4500))
        .send()
        .await
    {
        if let Ok(val) = resp.json::<Value>().await {
            temp = val["current"]["temperature_2m"].as_f64().unwrap_or(22.0);
            code = val["current"]["weather_code"].as_i64().unwrap_or(0);
        }
    }

    let (title, tagline, queries): (&str, &str, &[&str]) = match code {
        0..=2 => ("晴空微风电台", "适合轻快与透亮旋律", &["晴天 轻快", "日系 清爽", "City Pop"]),
        3 | 45 | 48 => ("多云漫步电台", "松弛、留白与城市感", &["Lo-fi chill", "Indie pop", "夜色 氛围"]),
        51..=67 | 80..=82 => ("雨声玻璃电台", "温柔的雨夜与独立人声", &["雨天 治愈", "民谣 吉他", "Acoustic"]),
        71..=77 | 85..=86 => ("落雪壁炉电台", "低饱和度的温暖钢琴与弦乐", &["钢琴 纯音乐", "冬日 温暖"]),
        _ => ("夜间深空电台", "沉浸合成器与深夜节拍", &["Synthwave", "Electronic chill"]),
    };

    let mut songs = Vec::new();
    let mut seen = HashSet::new();
    for kw in queries.iter().take(2) {
        if let Ok(list) = state.netease.search(kw, 6, 0).await {
            for s in list {
                let id_key = s.get("id").map(|v| v.to_string()).unwrap_or_default();
                if !id_key.is_empty() && seen.insert(id_key) {
                    songs.push(s);
                }
            }
        }
    }

    ok_json(json!({
        "ok": true,
        "weather": {
            "city": city,
            "temperature": temp,
            "weatherCode": code,
        },
        "radio": {
            "title": title,
            "subtitle": tagline,
            "seedQueries": queries,
            "songs": songs.into_iter().take(18).collect::<Vec<_>>(),
            "updatedAt": chrono::Utc::now().timestamp_millis(),
        }
    }))
}

// ---------- 网易云路由 ----------

async fn netease_search(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let kw = q.get("keywords").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20usize).clamp(1, 50);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0usize);
    match state.netease.search(&kw, limit, offset).await {
        Ok(songs) => {
            let len = songs.len();
            ok_json(json!({
                "songs": songs,
                "offset": offset,
                "limit": limit,
                "nextOffset": offset + len,
                "hasMore": len >= limit,
            }))
        }
        Err(e) => json_status(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": e, "songs": [] })),
    }
}

async fn netease_song_url(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let quality = q.get("quality").or_else(|| q.get("level")).cloned().unwrap_or_else(|| "hires".to_string());
    let hints = MatchHints {
        id: id.clone(),
        name: q.get("name").cloned().unwrap_or_default(),
        title: q.get("title").cloned().unwrap_or_default(),
        artist: q.get("artist").cloned().unwrap_or_default(),
        artist_id: q.get("artistId").cloned().unwrap_or_default(),
        artist_ids: q.get("artistIds").cloned().unwrap_or_default(),
        artist_names: q.get("artistNames").cloned().unwrap_or_default(),
        album: q.get("album").cloned().unwrap_or_default(),
        duration: q.get("duration").and_then(|s| s.parse::<f64>().ok()).map(|n| json!(n)),
        exclude_ids: q.get("excludeIds").cloned().unwrap_or_default(),
        skip_direct: q.get("skipDirect").map(|s| s == "1" || s == "true").unwrap_or(false),
    };
    match state.netease.song_url(&id, &quality, Some(hints)).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_lyric(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    match state.netease.lyric(&id).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_user_playlists(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let uid = q.get("uid").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok());
    let offset = q.get("offset").and_then(|s| s.parse().ok());
    match state.netease.user_playlists(&uid, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn netease_playlist_tracks(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok());
    let offset = q.get("offset").and_then(|s| s.parse().ok());
    match state.netease.playlist_tracks(&id, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn netease_playlist_create(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let privacy_str = json_id_str(params.get("privacy"));
    let privacy = if privacy_str.is_empty() {
        "0".to_string()
    } else {
        privacy_str
    };
    match state.netease.playlist_create(name, &privacy).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let has_explicit_pid = params.get("pid").is_some() || params.get("playlistId").is_some();
    let pid = json_id_str(
        params
            .get("pid")
            .or_else(|| params.get("playlistId"))
            .or_else(|| params.get("id")),
    );
    let ids = json_id_str(
        params
            .get("tracks")
            .or_else(|| params.get("ids"))
            .or_else(|| if has_explicit_pid { params.get("id") } else { None }),
    );
    match state.netease.playlist_add_song(&pid, &ids).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_playlist_subscribe(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let id = json_id_str(params.get("id").or_else(|| params.get("playlistId")));
    let sub = if let Some(t) = params.get("t") {
        parse_action_toggle(Some(t), true)
    } else {
        json_bool(
            params
                .get("subscribe")
                .or_else(|| params.get("subscribed"))
                .or_else(|| params.get("collected")),
            true,
        )
    };
    match state.netease.playlist_subscribe(&id, sub).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_album_detail(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(80);
    match state.netease.album_detail(&id, limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_album_subscribe(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let id = json_id_str(params.get("id").or_else(|| params.get("albumId")));
    let sub = if let Some(t) = params.get("t") {
        parse_action_toggle(Some(t), true)
    } else {
        json_bool(
            params
                .get("subscribe")
                .or_else(|| params.get("subscribed"))
                .or_else(|| params.get("collected")),
            true,
        )
    };
    match state.netease.album_subscribe(&id, sub).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_album_subscribe_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = q
        .get("ids")
        .or_else(|| q.get("id"))
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    match state.netease.album_subscribe_check(&ids).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_artist_detail(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match state.netease.artist_detail(&id, limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_song_comments(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.netease.song_comments(&id, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_post_comment(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let id = json_id_str(params.get("id"));
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let reply_to = json_id_str(params.get("replyTo").or_else(|| params.get("commentId")));
    let thread_id = if reply_to.is_empty() {
        None
    } else {
        Some(reply_to.as_str())
    };
    match state.netease.post_comment(&id, content, thread_id).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_comment_like(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let id = json_id_str(params.get("id"));
    let cid = json_id_str(params.get("cid").or_else(|| params.get("commentId")));
    let liked = if let Some(t) = params.get("t") {
        parse_action_toggle(Some(t), true)
    } else {
        json_bool(params.get("like").or_else(|| params.get("liked")), true)
    };
    match state.netease.comment_like(&id, &cid, liked).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_song_like(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let id = json_id_str(params.get("id"));
    let like = json_bool(params.get("like").or_else(|| params.get("liked")), true);
    match state.netease.like_song(&id, like).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_song_like_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let uid = q.get("uid").cloned().unwrap_or_default();
    let ids: Vec<String> = q
        .get("ids")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    match state.netease.song_like_check(&uid, &ids).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_search(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let kw = q.get("keywords").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    match state.netease.podcast_search(&kw, limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_hot(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.netease.podcast_hot(limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_detail(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rid = q.get("id").or_else(|| q.get("rid")).cloned().unwrap_or_default();
    match state.netease.podcast_detail(&rid).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_programs(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rid = q.get("id").or_else(|| q.get("rid")).cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(40);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let asc = q
        .get("asc")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    match state.netease.podcast_programs(&rid, limit, offset, asc).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_my(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.netease.podcast_my().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_podcast_my_items(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = q.get("key").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(40);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.netease.podcast_my_items(&key, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_login_status(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.netease.login_status().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn netease_login_cookie(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body
        .get("cookie")
        .or_else(|| body.get("data"))
        .or_else(|| body.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let normalized = crate::providers::normalize_cookie_header(raw);
    let parsed = crate::providers::parse_cookie_string(&normalized);
    if normalized.is_empty() || parsed.get("MUSIC_U").is_none_or(|v| v.trim().is_empty()) {
        return json_status(
            StatusCode::BAD_REQUEST,
            json!({
                "loggedIn": false,
                "error": "INVALID_NETEASE_COOKIE",
                "message": "网易云 cookie 缺少 MUSIC_U",
            }),
        );
    }
    if let Err(e) = state.netease.set_cookie(&normalized) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.netease.login_status().await {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("saved".to_string(), json!(true));
                obj.insert("hasCookie".to_string(), json!(true));
            }
            ok_json(v)
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn netease_login_qr_key(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.netease.login_qr_key().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_login_qr_create(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = q.get("key").cloned().unwrap_or_default();
    match state.netease.login_qr_create(&key).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_login_qr_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let key = q.get("key").cloned().unwrap_or_default();
    match state.netease.login_qr_check(&key).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_logout(State(state): State<Arc<AppBackendState>>) -> Response {
    ok_json(state.netease.logout().await)
}

// ---------- QQ 路由 ----------

async fn qq_search(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let kw = q.get("keywords").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(12).clamp(4, 30);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.qq.search(&kw, limit, offset).await {
        Ok(songs) => {
            let next_offset = offset + songs.len();
            let has_more = songs.len() >= limit;
            ok_json(json!({
                "provider": "qq",
                "songs": songs,
                "offset": offset,
                "limit": limit,
                "nextOffset": next_offset,
                "hasMore": has_more,
            }))
        }
        Err(e) => json_status(StatusCode::BAD_GATEWAY, json!({ "error": e, "songs": [] })),
    }
}

async fn qq_song_url(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mid = q.get("mid").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let media_mid = q.get("mediaMid").cloned();
    let quality = q.get("quality").cloned().unwrap_or_else(|| "hires".to_string());
    let hints = QqPlaybackHints {
        vip_required: q.get("vipRequired").map(|s| json!(s)),
        need_vip: q.get("needVip").map(|s| json!(s)),
        only_vip_playable: q.get("onlyVipPlayable").map(|s| json!(s)),
        fee: q.get("fee").and_then(|s| s.parse().ok()),
        privilege: q.get("privilege").and_then(|s| s.parse().ok()),
    };
    match state
        .qq
        .song_url(&mid, media_mid.as_deref().unwrap_or(""), &quality, Some(&hints))
        .await
    {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_lyric(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mid = q.get("mid").cloned().unwrap_or_default();
    let id = q.get("id").cloned();
    match state.qq.lyric(&mid, id.as_deref()).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_login_status(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let force_vip = q.get("forceVip").map(|s| s == "1" || s == "true").unwrap_or(false);
    match state.qq.login_status(false, force_vip).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn qq_login_cookie(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body
        .get("cookie")
        .or_else(|| body.get("data"))
        .or_else(|| body.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let normalized = crate::providers::qq::normalize_qq_cookie_input(raw);
    let obj = crate::providers::parse_cookie_string(&normalized);
    let uin = crate::providers::qq::qq_cookie_uin(&obj);
    let playback_key = crate::providers::qq::qq_cookie_playback_key(&obj);
    if normalized.is_empty() || uin.is_empty() || playback_key.is_empty() {
        let has_web_session =
            !uin.is_empty() && !crate::providers::qq::qq_cookie_music_key(&obj).is_empty();
        let error = if has_web_session {
            "QQ_PLAYBACK_AUTH_INCOMPLETE"
        } else {
            "INVALID_QQ_COOKIE"
        };
        let message = if has_web_session {
            "QQ 账号验证已完成，但 QQ 音乐播放授权尚未生成，请重新打开官方登录窗口完成授权"
        } else {
            "QQ cookie 缺少 uin 或有效 QQ 音乐播放票据"
        };
        return json_status(
            StatusCode::BAD_REQUEST,
            json!({
                "provider": "qq",
                "loggedIn": false,
                "partial": has_web_session,
                "error": error,
                "message": message,
            }),
        );
    }
    if let Err(e) = state.qq.set_cookie(&normalized) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.qq.login_status(true, true).await {
        Ok(mut v) => {
            if let Some(map) = v.as_object_mut() {
                map.insert("saved".to_string(), json!(true));
            }
            ok_json(v)
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn qq_logout(State(state): State<Arc<AppBackendState>>) -> Response {
    let _ = state.qq.set_cookie("");
    ok_json(json!({ "ok": true }))
}

async fn qq_user_playlists(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.qq.user_playlists().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_playlist_tracks(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok());
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.qq.playlist_tracks(&id, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_artist_detail(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mid = q.get("mid").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match state.qq.artist_detail(&mid, limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_album_detail(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mid = q.get("mid").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(80);
    match state.qq.album_detail(&mid, limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qq_song_comments(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let mid = q.get("mid").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let id_opt = if id.is_empty() { None } else { Some(id.as_str()) };
    let mid_opt = if mid.is_empty() { None } else { Some(mid.as_str()) };
    match state.qq.song_comments(id_opt, mid_opt, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

// ---------- 酷狗路由 ----------

async fn kugou_search(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let kw = q.get("keywords").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(15);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.kugou.search(&kw, limit, offset).await {
        Ok(songs) => {
            let len = songs.len();
            ok_json(json!({
                "songs": songs,
                "offset": offset,
                "limit": limit,
                "nextOffset": offset + len,
                "hasMore": len >= limit,
            }))
        }
        Err(e) => json_status(StatusCode::BAD_GATEWAY, json!({ "error": e, "songs": [] })),
    }
}

async fn kugou_recommendations(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(18);
    match state.kugou.recommendations(limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_song_url(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let hash = q.get("hash").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let album_id = q.get("albumId").cloned();
    let album_audio_id = q.get("albumAudioId").cloned();
    let quality = q.get("quality").cloned().unwrap_or_else(|| "hires".to_string());
    let hints = json!({
        "hqHash": q.get("hqHash"),
        "sqHash": q.get("sqHash"),
        "resHash": q.get("resHash"),
        "mixSongId": q.get("mixSongId"),
        "name": q.get("name"),
        "artist": q.get("artist"),
    });
    match state
        .kugou
        .song_url_with_hints(
            &hash,
            album_id.as_deref().unwrap_or(""),
            album_audio_id.as_deref(),
            &quality,
            Some(&hints),
        )
        .await
    {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_lyric(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let hash = q.get("hash").or_else(|| q.get("id")).cloned().unwrap_or_default();
    let kw = q.get("keyword").or_else(|| q.get("name")).cloned().unwrap_or_default();
    let dur = q.get("duration").and_then(|s| s.parse().ok()).unwrap_or(0);
    let kw_opt = if kw.is_empty() { None } else { Some(kw.as_str()) };
    match state.kugou.lyric(&hash, kw_opt, dur).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_login_status(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.kugou.login_status().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn kugou_login_cookie(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body
        .get("cookie")
        .or_else(|| body.get("data"))
        .or_else(|| body.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let normalized = {
        let n = crate::providers::normalize_cookie_header(raw);
        if n.is_empty() {
            raw.trim().to_string()
        } else {
            n
        }
    };
    let auth = crate::providers::kugou::extract_kugou_auth(&normalized);
    let obj = crate::providers::parse_cookie_string(&normalized);
    let has_mid = obj.get("kg_mid").is_some_and(|v| !v.is_empty());
    if normalized.is_empty() || (!auth.logged_in && !has_mid) {
        return json_status(
            StatusCode::BAD_REQUEST,
            json!({
                "provider": "kugou",
                "loggedIn": false,
                "error": "INVALID_KUGOU_COOKIE",
                "message": "酷狗 cookie 无效或缺少登录标识",
            }),
        );
    }
    if let Err(e) = state.kugou.set_cookie(&normalized) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.kugou.login_status().await {
        Ok(mut v) => {
            if let Some(map) = v.as_object_mut() {
                map.insert("saved".to_string(), json!(true));
                map.insert(
                    "partial".to_string(),
                    json!(auth.logged_in && !auth.playback_ready),
                );
            }
            ok_json(v)
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn kugou_logout(State(state): State<Arc<AppBackendState>>) -> Response {
    let _ = state.kugou.set_cookie("");
    ok_json(json!({ "ok": true }))
}

async fn kugou_user_playlists(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.kugou.user_playlists().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_playlist_tracks(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q
        .get("id")
        .or_else(|| q.get("global_collection_id"))
        .cloned()
        .unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let paged = q
        .get("paged")
        .map(|s| s == "1" || s == "true")
        .unwrap_or_else(|| q.contains_key("limit") || q.contains_key("offset"));
    match state.kugou.playlist_tracks(&id, limit, offset, paged).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

fn is_kugou_auth_required_response(v: &Value) -> bool {
    v.get("error").and_then(|e| e.as_str()) == Some("KUGOU_AUTH_REQUIRED")
}

async fn kugou_like_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let hashes: Vec<String> = q
        .get("hashes")
        .or_else(|| q.get("ids"))
        .or_else(|| q.get("hash"))
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    match state.kugou.like_check(&hashes).await {
        Ok(v) if is_kugou_auth_required_response(&v) => json_status(StatusCode::UNAUTHORIZED, v),
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_like_toggle(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let like = json_bool(params.get("like"), true);
    match state.kugou.like_toggle(&params, like).await {
        Ok(v) if is_kugou_auth_required_response(&v) => json_status(StatusCode::UNAUTHORIZED, v),
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let params = merge_query_and_body(&q, &body);
    let list_id = json_id_str(
        params
            .get("listId")
            .or_else(|| params.get("pid"))
            .or_else(|| params.get("id")),
    );
    let song = params.get("song").unwrap_or(&params);
    match state.kugou.playlist_add_song(&list_id, song).await {
        Ok(v) if is_kugou_auth_required_response(&v) => json_status(StatusCode::UNAUTHORIZED, v),
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

// ---------- 汽水路由 ----------

async fn qishui_search(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let kw = q.get("keywords").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(15);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.qishui.search(&kw, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_feed(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(18);
    match state.qishui.feed(limit).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_status(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.qishui.status().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn qishui_login_cookie(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let cookie = body.get("cookie").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = state.qishui.set_cookie(cookie) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.qishui.status().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn qishui_logout(State(state): State<Arc<AppBackendState>>) -> Response {
    let _ = state.qishui.logout();
    ok_json(json!({ "ok": true }))
}

async fn qishui_login_qrcode(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.qishui.create_qr_code().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_login_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let token = q.get("token").cloned().unwrap_or_default();
    match state.qishui.check_qr_connect(&token).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_user_playlists(State(state): State<Arc<AppBackendState>>) -> Response {
    match state.qishui.user_playlists().await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_playlist_tracks(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    let offset = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    match state.qishui.playlist_tracks(&id, limit, offset).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_like_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = q
        .get("ids")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    match state.qishui.song_like_check(&ids).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_song_like(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = json_id_str(body.get("id"));
    let liked = json_bool(body.get("like").or_else(|| body.get("liked")), true);
    match state.qishui.song_like(&id, liked).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_playlist_collect(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = json_id_str(body.get("id").or_else(|| body.get("playlistId")));
    let collected = json_bool(body.get("collected").or_else(|| body.get("subscribe")), true);
    match state.qishui.playlist_collect(&id, collected).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let pid = json_id_str(body.get("playlistId").or_else(|| body.get("pid")).or_else(|| body.get("id")));
    let tid = json_id_str(body.get("trackId").or_else(|| body.get("songId")));
    match state.qishui.playlist_add_song(&pid, &tid).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_album_collect(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = json_id_str(body.get("id").or_else(|| body.get("albumId")));
    let collected = json_bool(body.get("collected").or_else(|| body.get("subscribe")), true);
    match state.qishui.album_collect(&id, collected).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_comments(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    let cursor = q
        .get("cursor")
        .or_else(|| q.get("offset"))
        .cloned()
        .unwrap_or_else(|| "0".to_string());
    match state.qishui.song_comments(&id, limit, &cursor).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_create_comment(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = json_id_str(body.get("id"));
    let text = body.get("content").or_else(|| body.get("text")).and_then(|v| v.as_str()).unwrap_or("");
    match state.qishui.create_comment(&id, text).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_song_url(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let quality = q.get("quality").cloned().unwrap_or_else(|| "hires".to_string());
    match state.qishui.song_url(&id, &quality).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_lyric(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    match state.qishui.lyric(&id).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

// ---------- 代理与本地媒体流 ----------

fn is_forbidden_ip_core(ip: IpAddr, allow_proxy_fake_ip: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let is_this_net = o[0] == 0;
            let is_cgnat = o[0] == 100 && (o[1] & 0b1100_0000) == 64;
            let is_bench_fake_ip = o[0] == 198 && (o[1] == 18 || o[1] == 19);
            let is_ietf = o[0] == 192 && o[1] == 0 && o[2] == 0;
            let is_reserved = o[0] >= 240;
            let blocked_proxy_range = if allow_proxy_fake_ip {
                false
            } else {
                is_cgnat || is_bench_fake_ip
            };
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                || is_this_net
                || is_ietf
                || is_reserved
                || blocked_proxy_range
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip_core(IpAddr::V4(mapped), allow_proxy_fake_ip);
            }
            let seg0 = v6.segments()[0];
            let is_ula = (seg0 & 0xfe00) == 0xfc00;
            let is_link_local = (seg0 & 0xffc0) == 0xfe80;
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || is_ula || is_link_local
        }
    }
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    is_forbidden_ip_core(ip, false)
}

fn is_forbidden_resolved_ip(ip: IpAddr) -> bool {
    is_forbidden_ip_core(ip, true)
}

fn validate_external_media_url_syntax(raw_url: &str) -> Result<url::Url, (StatusCode, &'static str)> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing media url"));
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| (StatusCode::BAD_REQUEST, "Invalid media url"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err((StatusCode::BAD_REQUEST, "Invalid url scheme"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err((StatusCode::BAD_REQUEST, "Credentials in url are not allowed"));
    }
    if let Some(port) = parsed.port() {
        if port != 80 && port != 443 {
            return Err((StatusCode::FORBIDDEN, "Non-standard port blocked"));
        }
    }
    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            if is_forbidden_ip(IpAddr::V4(v4)) {
                return Err((StatusCode::FORBIDDEN, "Private or loopback ip blocked"));
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if is_forbidden_ip(IpAddr::V6(v6)) {
                return Err((StatusCode::FORBIDDEN, "Private or loopback ip blocked"));
            }
        }
        Some(url::Host::Domain(domain)) => {
            let clean = domain.trim_end_matches('.').to_ascii_lowercase();
            if clean.is_empty()
                || !clean.contains('.')
                || clean == "localhost"
                || clean.ends_with(".localhost")
                || clean.ends_with(".local")
                || clean.ends_with(".internal")
                || clean.ends_with(".arpa")
            {
                return Err((StatusCode::FORBIDDEN, "Local or internal hostname blocked"));
            }
            if let Ok(ip) = clean.parse::<IpAddr>() {
                if is_forbidden_ip(ip) {
                    return Err((StatusCode::FORBIDDEN, "Private or loopback ip blocked"));
                }
            }
        }
        None => return Err((StatusCode::BAD_REQUEST, "Missing host")),
    }
    Ok(parsed)
}

async fn validate_and_resolve_external_url(raw_url: &str) -> Result<url::Url, (StatusCode, &'static str)> {
    let parsed = validate_external_media_url_syntax(raw_url)?;
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        let port = parsed.port_or_known_default().unwrap_or(443);
        if let Ok(addrs) = tokio::net::lookup_host((domain, port)).await {
            for addr in addrs {
                if is_forbidden_resolved_ip(addr.ip()) {
                    return Err((StatusCode::FORBIDDEN, "Resolved private or loopback ip blocked"));
                }
            }
        }
    }
    Ok(parsed)
}

async fn proxy_cover(Query(q): Query<HashMap<String, String>>) -> Response {
    let cover_url = q.get("url").map(String::as_str).unwrap_or("").trim();
    if let Err((status, msg)) = validate_and_resolve_external_url(cover_url).await {
        return (status, msg).into_response();
    }
    let resp = match shared_http_client()
        .get(cover_url)
        .header("Referer", "https://music.163.com/")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };

    if let Some(content_len) = resp.content_length() {
        if content_len > PROXY_COVER_MAX_BYTES {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Cover image exceeds size limit").into_response();
        }
    }

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let cl = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ct)
        .header("Cross-Origin-Resource-Policy", "cross-origin")
        .header(header::CACHE_CONTROL, "public, max-age=86400");
    if let Some(len) = cl {
        builder = builder.header(header::CONTENT_LENGTH, len);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn parse_byte_range(range_hdr: &str, total: u64) -> Option<(u64, u64)> {
    let trimmed = range_hdr.trim();
    if trimmed.len() < 6 || !trimmed[..6].eq_ignore_ascii_case("bytes=") {
        return None;
    }
    let spec = &trimmed[6..];
    let (s_raw, e_raw) = spec.split_once('-')?;
    if total == 0 {
        return None;
    }
    if s_raw.is_empty() {
        let suffix = e_raw.trim().parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = total.saturating_sub(suffix);
        Some((start, total - 1))
    } else {
        let start = s_raw.trim().parse::<u64>().ok()?;
        if start >= total {
            return None;
        }
        let end = if e_raw.trim().is_empty() {
            total - 1
        } else {
            e_raw.trim().parse::<u64>().ok()?.min(total - 1)
        };
        if end < start {
            None
        } else {
            Some((start, end))
        }
    }
}

fn send_memory_audio_buffer(buf: Vec<u8>, content_type: &str, range_hdr: Option<&str>) -> Response {
    let total = buf.len() as u64;
    if let Some(rh) = range_hdr {
        if let Some((start, end)) = parse_byte_range(rh, total) {
            let slice = buf[start as usize..=end as usize].to_vec();
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, slice.len())
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::from(slice))
                .unwrap();
        }
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(buf))
        .unwrap()
}

async fn proxy_audio(
    State(state): State<Arc<AppBackendState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let audio_url = q.get("url").map(String::as_str).unwrap_or("").trim();
    if audio_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing url").into_response();
    }
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    // Qishui encrypted audio stream handling (#auth=spade_a)
    if let Some((clean_url, fragment)) = audio_url.split_once("#auth=") {
        if let Err((status, msg)) = validate_and_resolve_external_url(clean_url).await {
            return (status, msg).into_response();
        }
        if let Some((cached, ct)) = lock_unpoisoned(&state.decrypt_cache).get(audio_url) {
            return send_memory_audio_buffer(cached, &ct, range_hdr);
        }
        let spade_a = percent_encoding::percent_decode_str(fragment)
            .decode_utf8_lossy()
            .to_string();
        let resp = match shared_http_client()
            .get(clean_url)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return (StatusCode::BAD_GATEWAY, format!("Encrypted audio fetch failed: {e}")).into_response(),
        };
        if !resp.status().is_success() {
            return (
                StatusCode::BAD_GATEWAY,
                format!("Encrypted audio upstream returned {}", resp.status().as_u16()),
            )
                .into_response();
        }
        if let Some(len) = resp.content_length() {
            if len > QISHUI_DECRYPT_CACHE_MAX_BYTES as u64 {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Encrypted audio exceeds max buffer size",
                )
                    .into_response();
            }
        }
        let mut bytes = Vec::with_capacity(
            resp.content_length()
                .map(|v| v.min(QISHUI_DECRYPT_CACHE_MAX_BYTES as u64) as usize)
                .unwrap_or(0),
        );
        let mut stream = resp.bytes_stream();
        while let Some(chunk_res) = futures_util::TryStreamExt::try_next(&mut stream)
            .await
            .transpose()
        {
            let Ok(chunk) = chunk_res else {
                return (StatusCode::BAD_GATEWAY, "Encrypted audio stream aborted").into_response();
            };
            if bytes.len().saturating_add(chunk.len()) > QISHUI_DECRYPT_CACHE_MAX_BYTES {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Encrypted audio exceeds max buffer size",
                )
                    .into_response();
            }
            bytes.extend_from_slice(&chunk);
        }
        let dec = match decrypt_qishui_track(&bytes, &spade_a) {
            Ok(d) => d,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("Encrypted audio decrypt failed: {e}"),
                )
                    .into_response();
            }
        };
        let ct = if dec.is_flac { "audio/flac" } else { "audio/mp4" }.to_string();
        lock_unpoisoned(&state.decrypt_cache).insert(
            audio_url.to_string(),
            dec.buffer.clone(),
            ct.clone(),
        );
        return send_memory_audio_buffer(dec.buffer, &ct, range_hdr);
    }

    if let Err((status, msg)) = validate_and_resolve_external_url(audio_url).await {
        return (status, msg).into_response();
    }

    let referer = if audio_url.contains("kugou.com") {
        "https://www.kugou.com/"
    } else if audio_url.contains("qq.com") || audio_url.contains("gtimg.cn") {
        "https://y.qq.com/"
    } else {
        "https://music.163.com/"
    };

    let mut req = shared_http_client()
        .get(audio_url)
        .header("Referer", referer)
        .timeout(std::time::Duration::from_secs(12));
    if let Some(rh) = range_hdr {
        req = req.header(header::RANGE, rh);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/mpeg")
        .to_string();
    let cl = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cr = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ct)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(v) = cl {
        builder = builder.header(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = cr {
        builder = builder.header(header::CONTENT_RANGE, v);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn serve_file_with_range(
    file_path: PathBuf,
    content_type: String,
    range_hdr: Option<&str>,
) -> Response {
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let meta = match file.metadata().await {
        Ok(m) if m.is_file() => m,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let size = meta.len();

    if let Some(rh) = range_hdr {
        let Some((start, end)) = parse_byte_range(rh, size) else {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .unwrap();
        };
        if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let len = end - start + 1;
        let stream = ReaderStream::new(file.take(len));
        return Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, len)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
            .header(header::ACCEPT_RANGES, "bytes")
            .header("Cross-Origin-Resource-Policy", "cross-origin")
            .body(Body::from_stream(stream))
            .unwrap();
    }

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, size)
        .header(header::ACCEPT_RANGES, "bytes")
        .header("Cross-Origin-Resource-Policy", "cross-origin")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn serve_local_media(
    State(state): State<Arc<AppBackendState>>,
    AxumPath((kind, id)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let cap = q.get("cap").map(String::as_str).unwrap_or("");
    let Some((path, mime)) = state.local_library.resolve_media_file(&kind, &id, cap) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    serve_file_with_range(path, mime, range_hdr).await
}

async fn serve_we_media_resolved(
    state: Arc<AppBackendState>,
    id: &str,
    rel: &str,
    cap: &str,
    headers: HeaderMap,
) -> Response {
    let expected = state.wallpaper_engine.media_token();
    if expected.is_empty()
        || !crate::desktop::local_library::constant_time_eq(cap.trim(), &expected)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let decoded_rel = percent_encoding::percent_decode_str(rel)
        .decode_utf8_lossy()
        .into_owned();
    let Ok(path) = state
        .wallpaper_engine
        .resolve_project_file(id, &decoded_rel, WallpaperFileCapability::AnySafe)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = audio_mime_for_path(&path)
        .or_else(|| match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => Some("video/mp4"),
            "webm" => Some("video/webm"),
            "mov" => Some("video/quicktime"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            "pkg" | "pak" => Some("application/octet-stream"),
            _ => None,
        })
        .unwrap_or("application/octet-stream")
        .to_string();
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    serve_file_with_range(path, mime, range_hdr).await
}

async fn serve_we_media(
    State(state): State<Arc<AppBackendState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rel = q.get("path").map(String::as_str).unwrap_or("");
    let cap = q
        .get("cap")
        .or_else(|| q.get("token"))
        .map(String::as_str)
        .unwrap_or("");
    serve_we_media_resolved(state, &id, rel, cap, headers).await
}

async fn serve_we_media_nested(
    State(state): State<Arc<AppBackendState>>,
    AxumPath((id, rel_path)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rel = q
        .get("path")
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&rel_path);
    let cap = q
        .get("cap")
        .or_else(|| q.get("token"))
        .map(String::as_str)
        .unwrap_or("");
    serve_we_media_resolved(state, &id, rel, cap, headers).await
}

async fn serve_qishui_auth_asset(
    State(state): State<Arc<AppBackendState>>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let allowed = match name.as_str() {
        "security_host.html" | "security_seed.html" => "text/html; charset=utf-8",
        "bdms.js" | "react.js" | "react-dom.js" | "sdk-glue.js" => "application/javascript; charset=utf-8",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let file = state
        .config
        .paths()
        .app_root
        .join("qishui-auth-v6")
        .join(&name);
    let Ok(bytes) = tokio::fs::read(&file).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(allowed))
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::local_library::LocalImportEntry;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> (tempfile::TempDir, Arc<AppBackendState>) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Arc::new(ConfigManager::with_user_data_dir(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
        ));
        let state = AppBackendState::new(cfg);
        (tmp, state)
    }

    #[tokio::test]
    async fn core_endpoints_version_spotify_update_and_gate_work() {
        let (_tmp, state) = test_state();
        let app = build_router(state.clone());

        // 1. Version
        let resp = app
            .clone()
            .oneshot(Request::get("/api/app/version").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let ver: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(ver["name"], "mineradio");

        // 2. Spotify removed -> 404
        let resp = app
            .clone()
            .oneshot(Request::get("/api/spotify/search?keywords=test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // 3. Update download/patch -> 410 UPDATE_EXTERNAL_ONLY
        let resp = app
            .clone()
            .oneshot(Request::post("/api/update/download").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::GONE);

        // 4. Protected login route locked -> 423
        let resp = app
            .clone()
            .oneshot(Request::get("/api/login/qr/key").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::LOCKED);

        // Unlock gate
        let unlocked = state.gate.unlock("世界和平");
        assert!(unlocked.unlocked);
        assert!(state.gate.is_unlocked());
    }

    #[tokio::test]
    async fn beatmap_cuefield_and_feedback_routes_work_end_to_end() {
        let (_tmp, state) = test_state();
        let app = build_router(state.clone());

        let make_beats = |count: usize| -> Vec<Value> {
            (0..count)
                .map(|i| {
                    json!({
                        "time": i as f64 * 0.5,
                        "strength": 0.85,
                        "confidence": 0.9,
                        "impact": if i % 4 == 0 { 0.9 } else { 0.5 },
                        "downbeat": i % 4 == 0,
                    })
                })
                .collect()
        };

        for (key, dur) in [("netease:1", 180.0), ("netease:2", 200.0)] {
            let body = serde_json::to_string(&json!({
                "key": key,
                "meta": { "title": key, "artist": "Tester" },
                "map": { "duration": dur, "gridStep": 0.5, "beats": make_beats(340) }
            }))
            .unwrap();
            let resp = app
                .clone()
                .oneshot(
                    Request::post("/api/beatmap/cache")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let trans_body = serde_json::to_string(&json!({
            "fromKey": "netease:1",
            "toKey": "netease:2",
            "exitBias": "late"
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/cuefield/transition")
                    .header("content-type", "application/json")
                    .body(Body::from(trans_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let plan_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let plan: Value = serde_json::from_slice(&plan_bytes).unwrap();
        assert_eq!(plan["ok"], true);
        assert!(plan["artifact"]["artifactId"].as_str().unwrap().len() == 64);

        let fb_body = serde_json::to_string(&json!({ "rating": 1 })).unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/cuefield/feedback")
                    .header("content-type", "application/json")
                    .body(Body::from(fb_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn local_media_serves_range_and_rejects_bad_token() {
        let (tmp, state) = test_state();
        let wav = tmp.path().join("demo.wav");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&88200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&wav, &bytes).unwrap();

        let imp = state.local_library.import_files(
            vec![LocalImportEntry {
                path: wav.to_string_lossy().to_string(),
                relative_path: Some("demo.wav".into()),
            }],
            false,
        );
        assert!(imp.ok);
        let id = imp.tracks[0].local_file_id.clone();
        let cap = state.local_library.media_token();

        let app = build_router(state);

        let bad = app
            .clone()
            .oneshot(
                Request::get(&format!("/local-media/audio/{id}?cap=bad"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::NOT_FOUND);

        let partial = app
            .oneshot(
                Request::get(&format!("/local-media/audio/{id}?cap={cap}"))
                    .header("range", "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            partial.headers().get("content-range").unwrap(),
            &format!("bytes 0-3/{}", bytes.len())
        );
        let slice = partial.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&slice[..], b"RIFF");
    }

    #[test]
    fn json_id_str_and_param_helpers_handle_all_variants() {
        assert_eq!(json_id_str(Some(&json!("  12345  "))), "12345");
        assert_eq!(json_id_str(Some(&json!(9876543210u64))), "9876543210");
        assert_eq!(json_id_str(Some(&json!(-42))), "-42");
        assert_eq!(json_id_str(Some(&json!(2048.0))), "2048");
        assert_eq!(json_id_str(Some(&json!({ "songId": 778899 }))), "778899");
        assert_eq!(json_id_str(Some(&json!([111, "222", { "id": 333 }]))), "111,222,333");
        assert_eq!(json_id_str(Some(&json!(true))), "");
        assert_eq!(json_id_str(None), "");

        assert!(json_bool(Some(&json!("1")), false));
        assert!(json_bool(Some(&json!("true")), false));
        assert!(!json_bool(Some(&json!("0")), true));
        assert!(!json_bool(Some(&json!("false")), true));
        assert!(!json_bool(Some(&json!(0)), true));
        assert!(parse_action_toggle(Some(&json!(1)), false));
        assert!(!parse_action_toggle(Some(&json!(2)), true));
        assert!(!parse_action_toggle(Some(&json!("0")), true));

        let mut q = HashMap::new();
        q.insert("id".to_string(), "100".to_string());
        q.insert("like".to_string(), "false".to_string());
        let merged = merge_query_and_body(
            &q,
            &Bytes::from_static(br#"{"id":200,"replyTo":999}"#),
        );
        assert_eq!(json_id_str(merged.get("id")), "200");
        assert_eq!(json_id_str(merged.get("replyTo")), "999");
        assert!(!json_bool(merged.get("like"), true));
    }

    #[tokio::test]
    async fn beatmap_cache_accepts_beatmap_and_song_alias() {
        let (_tmp, state) = test_state();
        let app = build_router(state);

        let payload = serde_json::to_string(&json!({
            "key": "qq:track_x",
            "beatMap": {
                "duration": 142.5,
                "gridStep": 0.5,
                "beats": []
            },
            "song": {
                "provider": "qq",
                "title": "Midnight Drive",
                "artists": [{ "name": "Alice" }, { "name": "Bob" }]
            }
        }))
        .unwrap();

        let post_resp = app
            .clone()
            .oneshot(
                Request::post("/api/beatmap/cache")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_resp.status(), StatusCode::OK);
        let post_json: Value = serde_json::from_slice(
            &post_resp.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();
        assert_eq!(post_json["ok"], true);

        let get_resp = app
            .oneshot(
                Request::get("/api/beatmap/cache?key=qq:track_x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let get_json: Value = serde_json::from_slice(
            &get_resp.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();
        assert_eq!(get_json["hit"], true);
        assert_eq!(get_json["meta"]["provider"], "qq");
        assert_eq!(get_json["meta"]["title"], "Midnight Drive");
        assert_eq!(get_json["meta"]["artist"], "Alice / Bob");
        assert_eq!(get_json["map"]["duration"], 142.5);
    }

    #[tokio::test]
    async fn listen_report_validates_duration_and_payload_variants() {
        let (_tmp, state) = test_state();
        let app = build_router(state);

        // 1. 原版 payload 有效播放时长不足
        let short_legacy = serde_json::to_string(&json!({
            "provider": "netease",
            "song": { "id": 123456 },
            "sessionId": "sess-0001",
            "listenMs": 2000,
            "durationMs": 200000
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/listen/report")
                    .header("content-type", "application/json")
                    .body(Body::from(short_legacy))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["accepted"], false);
        assert_eq!(val["eligible"], false);
        assert_eq!(val["reason"], "LISTEN_REPORT_NOT_ELIGIBLE");

        // 2. 简化 payload 时长不足 (< 5s)
        let short_new = serde_json::to_string(&json!({
            "provider": "netease",
            "id": 998877,
            "time": 2,
            "sessionId": "sess-0002"
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/listen/report")
                    .header("content-type", "application/json")
                    .body(Body::from(short_new))
                    .unwrap(),
            )
            .await
            .unwrap();
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["accepted"], false);
        assert_eq!(val["reason"], "LISTEN_REPORT_NOT_ELIGIBLE");

        // 3. 播客曲目不允许上报
        let podcast_body = serde_json::to_string(&json!({
            "provider": "netease",
            "song": { "id": 555, "type": "podcast" },
            "sessionId": "sess-0003",
            "listenMs": 60000,
            "durationMs": 120000
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/listen/report")
                    .header("content-type", "application/json")
                    .body(Body::from(podcast_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["accepted"], false);
        assert_eq!(val["reason"], "LISTEN_REPORT_NOT_ELIGIBLE");

        // 4. 不支持的平台
        let qq_body = serde_json::to_string(&json!({
            "provider": "qq",
            "id": "001abc",
            "time": 60,
            "sessionId": "sess-0004"
        }))
        .unwrap();
        let resp = app
            .oneshot(
                Request::post("/api/listen/report")
                    .header("content-type", "application/json")
                    .body(Body::from(qq_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["accepted"], false);
        assert_eq!(val["reason"], "UNSUPPORTED_PROVIDER");
    }

    #[tokio::test]
    async fn login_cookie_validation_and_kugou_unauthorized_work() {
        let (_tmp, state) = test_state();
        assert!(state.gate.unlock("世界和平").unlocked);
        let app = build_router(state);

        // Netease cookie 缺 MUSIC_U -> 400
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/login/cookie")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cookie":"__csrf=abc"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["error"], "INVALID_NETEASE_COOKIE");

        // QQ cookie 缺凭证 -> 400 INVALID_QQ_COOKIE
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/qq/login/cookie")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cookie":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["error"], "INVALID_QQ_COOKIE");

        // QQ cookie 有网页会话但缺 playback key -> 400 QQ_PLAYBACK_AUTH_INCOMPLETE
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/qq/login/cookie")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cookie":"uin=o123456789; skey=test_web_skey"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["error"], "QQ_PLAYBACK_AUTH_INCOMPLETE");
        assert_eq!(val["partial"], true);

        // Kugou cookie 为空 -> 400 INVALID_KUGOU_COOKIE
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/kugou/login/cookie")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cookie":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let val: Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(val["error"], "INVALID_KUGOU_COOKIE");

        // Kugou 未登录调用 like/check、song/like、playlist/add-song -> 401
        for req in [
            Request::get("/api/kugou/song/like/check?hashes=abc")
                .body(Body::empty())
                .unwrap(),
            Request::post("/api/kugou/song/like")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"hash":"abc","like":true}"#))
                .unwrap(),
            Request::get("/api/kugou/playlist/add-song?pid=100&hash=abc")
                .body(Body::empty())
                .unwrap(),
        ] {
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            let val: Value =
                serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(val["error"], "KUGOU_AUTH_REQUIRED");
        }

        // /api/podcast/dj-beatmap 支持 GET + POST 参数校验
        let resp_get = app
            .clone()
            .oneshot(
                Request::get("/api/podcast/dj-beatmap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_get.status(), StatusCode::BAD_REQUEST);

        let resp_post = app
            .oneshot(
                Request::post("/api/podcast/dj-beatmap")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"intro":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_post.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn we_media_serves_query_and_nested_paths_with_cap_check() {
        let (tmp, state) = test_state();
        let proj = tmp.path().join("wp_demo");
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::fs::write(proj.join("sub").join("loop.mp4"), b"mp4-payload").unwrap();
        std::fs::write(proj.join("preview.jpg"), b"jpg-payload").unwrap();
        std::fs::write(
            proj.join("project.json"),
            json!({
                "title": "Test WP",
                "type": "video",
                "file": "sub/loop.mp4",
                "preview": "preview.jpg"
            })
            .to_string(),
        )
        .unwrap();

        let snap = state.wallpaper_engine.add_directory(&proj).unwrap();
        let id = snap.projects[0].id.clone();
        let token = snap.media_token.clone();

        let app = build_router(state);

        // 错误 token -> 403
        let bad = app
            .clone()
            .oneshot(
                Request::get(&format!("/we-media/file/{id}?path=preview.jpg&cap=wrong"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::FORBIDDEN);

        // 查询参数 path + token
        let ok_preview = app
            .clone()
            .oneshot(
                Request::get(&format!(
                    "/we-media/file/{id}?path=preview.jpg&token={token}"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_preview.status(), StatusCode::OK);
        assert_eq!(
            ok_preview.into_body().collect().await.unwrap().to_bytes().as_ref(),
            b"jpg-payload"
        );

        // 多段路径 + cap
        let ok_nested = app
            .clone()
            .oneshot(
                Request::get(&format!("/we-media/file/{id}/sub/loop.mp4?cap={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_nested.status(), StatusCode::OK);
        assert_eq!(
            ok_nested.into_body().collect().await.unwrap().to_bytes().as_ref(),
            b"mp4-payload"
        );

        // 路径穿越 -> 404
        let traversal = app
            .clone()
            .oneshot(
                Request::get(&format!(
                    "/we-media/file/{id}?path=../secret.mp4&cap={token}"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(traversal.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn proxy_routes_block_ssrf_and_restrict_cors_and_qq_like() {
        let (_tmp, state) = test_state();
        let app = build_router(state);

        for blocked_url in [
            "http://127.0.0.1:8080/secret",
            "http://169.254.169.254/latest/meta-data/",
            "http://localhost/admin",
            "http://[::1]/local",
            "http://10.0.0.1/internal",
            "file:///etc/passwd#auth=abc",
            "http://127.0.0.1:9000/a.mp3#auth=abc",
            "https://music.163.com:8443/nonstd.mp3",
        ] {
            let encoded = urlencoding_for_test(blocked_url);
            let resp = app
                .clone()
                .oneshot(
                    Request::get(&format!("/api/audio?url={encoded}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::FORBIDDEN,
                "expected 400/403 for {blocked_url}, got {}",
                resp.status()
            );
        }

        let cover_ssrf = app
            .clone()
            .oneshot(
                Request::get("/api/cover?url=http%3A%2F%2F169.254.169.254%2Flatest")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cover_ssrf.status(), StatusCode::FORBIDDEN);

        // 合法远端 URL 语法允许，但拒绝未授权的第三方 CORS Origin
        let evil_cors = app
            .clone()
            .oneshot(
                Request::get("/api/app/version")
                    .header(header::ORIGIN, "https://evil.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(evil_cors
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());

        let tauri_cors = app
            .clone()
            .oneshot(
                Request::get("/api/app/version")
                    .header(header::ORIGIN, "http://tauri.localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            tauri_cors
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("http://tauri.localhost")
        );

        let qq_like = app
            .oneshot(
                Request::post("/api/qq/like")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(qq_like.status(), StatusCode::NOT_IMPLEMENTED);

        // 字面 Fake-IP 直连仍被拦截，而经过公网域名解析出的代理 Fake-IP (198.18.0.28) 允许通过
        let fake_ip: IpAddr = "198.18.0.28".parse().unwrap();
        assert!(is_forbidden_ip(fake_ip));
        assert!(!is_forbidden_resolved_ip(fake_ip));
        assert!(is_forbidden_resolved_ip("127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_resolved_ip("169.254.169.254".parse().unwrap()));
    }

    fn urlencoding_for_test(raw: &str) -> String {
        percent_encoding::utf8_percent_encode(raw, percent_encoding::NON_ALPHANUMERIC).to_string()
    }
}
