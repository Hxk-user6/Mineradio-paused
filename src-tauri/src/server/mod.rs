use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tower_http::cors::CorsLayer;

use crate::analyzer::dj::analyze_podcast_dj_stream;
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
const LISTEN_SYNC_JOURNAL_LIMIT: usize = 512;

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
        .route("/api/podcast/dj-beatmap", post(podcast_dj_beatmap))
        .route("/api/listen/report", post(listen_report))
        .route("/api/listen/total", get(listen_total))
        .route("/api/discover/home", get(discover_home))
        .route("/api/weather/radio", get(weather_radio))
        .route("/api/weather/ip-location", get(weather_ip_location))
        // Netease
        .route("/api/search", get(netease_search))
        .route("/api/song/url", get(netease_song_url))
        .route("/api/lyric", get(netease_lyric))
        .route("/api/user/playlists", get(netease_user_playlists))
        .route("/api/playlist/tracks", get(netease_playlist_tracks))
        .route("/api/playlist/create", post(netease_playlist_create))
        .route("/api/playlist/add-song", post(netease_playlist_add_song))
        .route("/api/playlist/subscribe", post(netease_playlist_subscribe))
        .route("/api/album/detail", get(netease_album_detail))
        .route("/api/album/subscribe", post(netease_album_subscribe))
        .route("/api/album/subscribe/check", get(netease_album_subscribe_check))
        .route("/api/artist/detail", get(netease_artist_detail))
        .route("/api/song/comments", get(netease_song_comments).post(netease_post_comment))
        .route("/api/song/comments/like", post(netease_comment_like))
        .route("/api/song/like", post(netease_song_like))
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
        .route("/api/kugou/playlist/add-song", post(kugou_playlist_add_song))
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
        .route("/qishui-auth-proxy/{name}", get(serve_qishui_auth_asset))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            easter_egg_gate_middleware,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state)
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

async fn beatmap_cache_post(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    match state.config.write_beatmap_cache(&body) {
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

async fn podcast_dj_beatmap(Json(body): Json<Value>) -> Response {
    let url = body.get("audioUrl").or_else(|| body.get("url")).and_then(|v| v.as_str()).unwrap_or("");
    let duration_sec = body.get("durationSec").or_else(|| body.get("duration")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    if url.trim().is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "MISSING_AUDIO_URL");
    }
    match analyze_podcast_dj_stream(url, duration_sec).await {
        Ok(map) => ok_json(json!({ "ok": true, "map": map })),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn listen_report(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("netease");
    if provider != "netease" {
        return ok_json(json!({
            "accepted": false,
            "platformSubmitted": false,
            "provider": provider,
            "reason": "UNSUPPORTED_PROVIDER",
        }));
    }
    let id = body.get("id").and_then(|v| v.as_str()).or_else(|| body.get("id").and_then(|v| v.as_i64().map(|_| ""))).unwrap_or("");
    let song_id = if id.is_empty() {
        body.get("id").map(|v| v.to_string()).unwrap_or_default().trim_matches('"').to_string()
    } else {
        id.to_string()
    };
    let source_id = body.get("sourceId").and_then(|v| v.as_str()).unwrap_or("");
    let time_sec = body.get("time").and_then(|v| v.as_u64()).unwrap_or(0);
    if song_id.is_empty() {
        return ok_json(json!({ "accepted": false, "platformSubmitted": false, "reason": "MISSING_SONG_ID" }));
    }
    let session_id = body
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or(&song_id)
        .to_string();

    let res = state.netease.scrobble(&song_id, source_id, time_sec).await;
    let submitted = res.is_ok();
    {
        let mut journal = state.listen_journal.lock().unwrap();
        if let Some(entries) = journal.get_mut("entries").and_then(|e| e.as_object_mut()) {
            entries.insert(
                format!("netease:{session_id}"),
                json!({
                    "provider": "netease",
                    "submittedAt": chrono::Utc::now().timestamp_millis(),
                    "songId": song_id,
                    "time": time_sec,
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
        Ok(val) => ok_json(json!({ "accepted": true, "platformSubmitted": true, "body": val })),
        Err(e) => ok_json(json!({ "accepted": false, "platformSubmitted": false, "error": e })),
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
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let privacy = body.get("privacy").and_then(|v| v.as_str()).unwrap_or("0");
    match state.netease.playlist_create(name, privacy).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let pid = body.get("pid").and_then(|v| v.as_str()).unwrap_or("");
    let ids = body.get("tracks").or_else(|| body.get("ids")).and_then(|v| v.as_str()).unwrap_or("");
    match state.netease.playlist_add_song(pid, ids).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_playlist_subscribe(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let sub = body.get("t").and_then(|v| v.as_i64()).map(|n| n == 1)
        .or_else(|| body.get("subscribed").and_then(|v| v.as_bool()))
        .unwrap_or(true);
    match state.netease.playlist_subscribe(id, sub).await {
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
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let sub = body.get("subscribed").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.netease.album_subscribe(id, sub).await {
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
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let thread_id = body.get("commentId").and_then(|v| v.as_str());
    match state.netease.post_comment(id, content, thread_id).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_comment_like(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let cid = body.get("cid").and_then(|v| v.as_str()).unwrap_or("");
    let liked = body.get("like").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.netease.comment_like(id, cid, liked).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn netease_song_like(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let like = body.get("like").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.netease.like_song(id, like).await {
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
    let _asc = q.get("asc").map(|s| s == "1" || s == "true").unwrap_or(false);
    match state.netease.podcast_programs(&rid, limit, offset).await {
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
    let cookie = body.get("cookie").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = state.netease.set_cookie(cookie) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.netease.login_status().await {
        Ok(v) => ok_json(v),
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
    let cookie = body.get("cookie").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = state.qq.set_cookie(cookie) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.qq.login_status(true, true).await {
        Ok(v) => ok_json(v),
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
    let cookie = body.get("cookie").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = state.kugou.set_cookie(cookie) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }
    match state.kugou.login_status().await {
        Ok(v) => ok_json(v),
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
    let id = q.get("id").cloned().unwrap_or_default();
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

async fn kugou_like_check(
    State(state): State<Arc<AppBackendState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let hashes: Vec<String> = q
        .get("hashes")
        .or_else(|| q.get("ids"))
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    match state.kugou.like_check(&hashes).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_like_toggle(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let like = body.get("like").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.kugou.like_toggle(&body, like).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn kugou_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let list_id = body.get("listId").or_else(|| body.get("id")).and_then(|v| v.as_str()).unwrap_or("");
    let song = body.get("song").unwrap_or(&body);
    match state.kugou.playlist_add_song(list_id, song).await {
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
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let liked = body.get("like").or_else(|| body.get("liked")).and_then(|v| v.as_bool()).unwrap_or(true);
    match state.qishui.song_like(id, liked).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_playlist_collect(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let collected = body.get("collected").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.qishui.playlist_collect(id, collected).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_playlist_add_song(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let pid = body.get("playlistId").or_else(|| body.get("id")).and_then(|v| v.as_str()).unwrap_or("");
    let tid = body.get("trackId").or_else(|| body.get("songId")).and_then(|v| v.as_str()).unwrap_or("");
    match state.qishui.playlist_add_song(pid, tid).await {
        Ok(v) => ok_json(v),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

async fn qishui_album_collect(
    State(state): State<Arc<AppBackendState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let collected = body.get("collected").and_then(|v| v.as_bool()).unwrap_or(true);
    match state.qishui.album_collect(id, collected).await {
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
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let text = body.get("content").or_else(|| body.get("text")).and_then(|v| v.as_str()).unwrap_or("");
    match state.qishui.create_comment(id, text).await {
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

async fn proxy_cover(Query(q): Query<HashMap<String, String>>) -> Response {
    let cover_url = q.get("url").map(String::as_str).unwrap_or("").trim();
    if !(cover_url.starts_with("http://") || cover_url.starts_with("https://")) {
        return (StatusCode::BAD_REQUEST, "Invalid cover url").into_response();
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
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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
    let spec = range_hdr.trim().strip_prefix("bytes=")?;
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
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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
        if let Some((cached, ct)) = state.decrypt_cache.lock().unwrap().get(audio_url) {
            return send_memory_audio_buffer(cached, &ct, range_hdr);
        }
        let spade_a = percent_encoding::percent_decode_str(fragment)
            .decode_utf8_lossy()
            .to_string();
        if let Ok(resp) = shared_http_client()
            .get(clean_url)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
        {
            if let Ok(bytes) = resp.bytes().await {
                if let Ok(dec) = decrypt_qishui_track(&bytes, &spade_a) {
                    let ct = if dec.is_flac { "audio/flac" } else { "audio/mp4" }.to_string();
                    state
                        .decrypt_cache
                        .lock()
                        .unwrap()
                        .insert(audio_url.to_string(), dec.buffer.clone(), ct.clone());
                    return send_memory_audio_buffer(dec.buffer, &ct, range_hdr);
                }
            }
        }
    }

    if !(audio_url.starts_with("http://") || audio_url.starts_with("https://")) {
        return (StatusCode::BAD_REQUEST, "Invalid audio url").into_response();
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
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
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

async fn serve_we_media(
    State(state): State<Arc<AppBackendState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rel = q.get("path").map(String::as_str).unwrap_or("");
    let Ok(path) = state
        .wallpaper_engine
        .resolve_project_file(&id, rel, WallpaperFileCapability::AnySafe)
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
            _ => None,
        })
        .unwrap_or("application/octet-stream")
        .to_string();
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    serve_file_with_range(path, mime, range_hdr).await
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
}
