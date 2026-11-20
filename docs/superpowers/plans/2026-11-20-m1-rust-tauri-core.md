# Mineradio M1: Rust + Tauri 核心后端与桌面基础 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在独立分支 `refactor/rust-tauri` 上构建完整可测试的 Rust + Tauri 2 核心后端与桌面基础（配置与缓存、登录彩蛋门控、内置歌单、本地曲库、汽水音频解密、Cuefield AutoMix 引擎、Symphonia DJ 节奏分析、Axum 媒体与核心 API 服务、Tauri 多窗口/托盘/命令壳），并全量通过 `cargo test` 与 Windows 交叉编译静态检查。

**Architecture:** Tauri 2 管理桌面生命周期、无边框主窗口、透明穿透桌面歌词窗口、系统托盘与全局快捷键，并通过 `#[tauri::command]` 暴露本地曲库/歌单/门控/设置能力；同时在进程内启动基于 `axum` 的本地回环 HTTP 服务（`127.0.0.1:<port>`，`CORS *`），专责流式 Range 音频代理、汽水 AES-CTR 实时解密、封面代理、Cuefield/Beatmap 接口及本地媒体 HTTP 兜底路由。

**Tech Stack:** Rust 1.96, Tauri 2 (`tray-icon`, `single-instance`, `global-shortcut`, `dialog`), Axum 0.8, Tokio 1, Reqwest 0.12 (`rustls-tls`), Lofty 0.22, Symphonia 0.5, `aes`/`ctr`/`sha2`, `encoding_rs`, `image`, `windows` 0.58 (`#[cfg(windows)]`).

---

## 文件结构映射

- Create: `src-tauri/Cargo.toml`
- Create: `src-tauri/build.rs`
- Create: `src-tauri/tauri.conf.json`
- Create: `src-tauri/capabilities/default.json`
- Create: `src-tauri/icons/icon.png` (copy from `build/icon.png`), `src-tauri/icons/icon.ico` (copy from `build/icon.ico`)
- Create: `src-tauri/src/main.rs`
- Create: `src-tauri/src/lib.rs`
- Create: `src-tauri/src/config.rs`
- Create: `src-tauri/src/desktop/mod.rs`
- Create: `src-tauri/src/desktop/easter_egg_gate.rs`
- Create: `src-tauri/src/desktop/builtin_playlists.rs`
- Create: `src-tauri/src/desktop/local_library.rs`
- Create: `src-tauri/src/desktop/memory.rs`
- Create: `src-tauri/src/desktop/win32.rs`
- Create: `src-tauri/src/desktop/commands.rs`
- Create: `src-tauri/src/providers/mod.rs`
- Create: `src-tauri/src/providers/qishui_decryptor.rs`
- Create: `src-tauri/src/cuefield/mod.rs`
- Create: `src-tauri/src/cuefield/lrc.rs`
- Create: `src-tauri/src/cuefield/structure.rs`
- Create: `src-tauri/src/cuefield/planner.rs`
- Create: `src-tauri/src/cuefield/feedback.rs`
- Create: `src-tauri/src/analyzer/mod.rs`
- Create: `src-tauri/src/analyzer/dj.rs`
- Create: `src-tauri/src/server/mod.rs`

> 说明：因当前工作环境镜像配置中 `ustc` 索引易超时，所有 `cargo` 命令统一附加环境变量 `CARGO_SOURCE_USTC_REGISTRY="sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"` 以保证稳定拉取。

---

### Task 1: 初始化 Tauri 2 工程骨架与构建配置

**Files:**
- Create: `src-tauri/Cargo.toml`
- Create: `src-tauri/build.rs`
- Create: `src-tauri/tauri.conf.json`
- Create: `src-tauri/capabilities/default.json`
- Create: `src-tauri/icons/icon.png`
- Create: `src-tauri/icons/icon.ico`
- Create: `src-tauri/src/main.rs`
- Create: `src-tauri/src/lib.rs`

- [ ] **Step 1: 准备目录与图标资源**

```bash
mkdir -p src-tauri/icons src-tauri/capabilities src-tauri/src/server src-tauri/src/providers src-tauri/src/cuefield src-tauri/src/analyzer src-tauri/src/desktop
cp build/icon.png src-tauri/icons/icon.png
cp build/icon.ico src-tauri/icons/icon.ico
```

- [ ] **Step 2: 写入 `src-tauri/Cargo.toml`、`build.rs`、`tauri.conf.json` 与 `capabilities/default.json`**

`src-tauri/Cargo.toml`:
```toml
[package]
name = "mineradio"
version = "2.2.0"
description = "Windows 沉浸式音乐播放器，融合桌面模式、歌词舞台、粒子视觉和 3D 歌单架。"
authors = ["Mineradio"]
license = "GPL-3.0-only"
edition = "2021"

[lib]
name = "mineradio_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon", "image-png", "image-ico", "devtools"] }
tauri-plugin-single-instance = "2"
tauri-plugin-global-shortcut = "2"
tauri-plugin-dialog = "2"

axum = "0.8"
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["io"] }
tower-http = { version = "0.6", features = ["cors", "fs"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream", "cookies", "gzip", "deflate"] }

serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"

aes = "0.8"
cbc = "0.1"
ecb = "0.1"
ctr = "0.9"
cipher = "0.4"
md-5 = "0.10"
sha2 = "0.10"
hmac = "0.12"
base64 = "0.22"
hex = "0.4"
num-bigint = "0.4"
rand = "0.8"

lofty = "0.22"
symphonia = { version = "0.5", features = ["mp3", "aac", "isomp4", "flac", "ogg", "pcm", "wav"] }
encoding_rs = "0.8"
qrcode = "0.14"
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "webp", "gif", "bmp", "ico"] }

url = "2"
percent-encoding = "2"
bytes = "1"
futures-util = "0.3"
regex = "1"
dirs = "6"
walkdir = "2"
opener = "0.7"
arboard = "3"
chrono = "0.4"

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
  "Win32_Foundation",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Gdi",
  "Win32_System_ProcessStatus",
  "Win32_System_Threading",
  "Win32_System_Memory",
  "Win32_UI_WindowsAndMessaging",
  "Win32_Security",
] }

[dev-dependencies]
tempfile = "3"
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
```

`src-tauri/build.rs`:
```rust
fn main() {
    tauri_build::build()
}
```

`src-tauri/tauri.conf.json`:
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Mineradio",
  "version": "2.2.0",
  "identifier": "com.mineradio.desktop",
  "build": {
    "frontendDist": "../public"
  },
  "app": {
    "withGlobalTauri": true,
    "macOSPrivateApi": true,
    "windows": [],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": [
      "icons/icon.png",
      "icons/icon.ico"
    ],
    "resources": [
      "../docs/update",
      "../package.json"
    ]
  }
}
```
> 注：M1 暂先用 `../public` 占位以满足 `tauri-build` 编译检查；M2 创建 `frontend/` 后切换为 `beforeDevCommand`、`beforeBuildCommand` 与 `frontendDist: "../frontend/dist"`。

`src-tauri/capabilities/default.json`:
```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Default Mineradio capabilities for main and overlay windows",
  "windows": ["main", "desktop-lyrics"],
  "permissions": [
    "core:default",
    "core:window:default",
    "core:event:default",
    "global-shortcut:default",
    "dialog:default"
  ]
}
```

`src-tauri/src/main.rs` 与 `src-tauri/src/lib.rs`:
```rust
// src-tauri/src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mineradio_lib::run();
}
```

```rust
// src-tauri/src/lib.rs
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Mineradio");
}
```

- [ ] **Step 3: 验证基础骨架编译通过**

Run:
```bash
cd src-tauri && CARGO_SOURCE_USTC_REGISTRY="sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/" cargo check
```
Expected: `Finished dev profile` 无错误。

- [ ] **Step 4: 提交 Task 1**

```bash
git add src-tauri/Cargo.toml src-tauri/build.rs src-tauri/tauri.conf.json src-tauri/capabilities/default.json src-tauri/icons src-tauri/src/main.rs src-tauri/src/lib.rs
git commit -m "chore(tauri): initialize Tauri 2 crate skeleton for Mineradio"
```

---

### Task 2: 实现 `config.rs`（路径规范、缓存、FX 自动保存、Beatmap 缓存与更新解析）

**Files:**
- Create: `src-tauri/src/config.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod config;`)

- [ ] **Step 1: 先编写失败单元测试（写入 `src-tauri/src/config.rs` 仅含测试占位失败或先建测试并确认编译失败）**

在 `src-tauri/src/lib.rs` 加入 `pub mod config;`，并在 `src-tauri/src/config.rs` 中先写测试模块：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_comparison_matches_semver() {
        assert_eq!(compare_versions("v2.3.0", "2.2.0"), 1);
        assert_eq!(compare_versions("2.2.0", "v2.2.0"), 0);
        assert_eq!(compare_versions("2.1.9", "2.2.0"), -1);
    }

    #[test]
    fn release_download_page_extraction_filters_urls() {
        let body = r#"
- 修复播放稳定性问题
<!-- mineradio-download-page: 夸克盘|https://pan.quark.cn/s/4b124d3e81d3 -->
<!-- mineradio-download-page: 恶意站|https://evil.example.com/setup.exe -->
<!-- mineradio-download-page: 百度云|https://pan.baidu.com/s/17CwpHUza67w_Grgc3s5nOw?pwd=SJHP -->
"#;
        let pages = extract_release_download_pages(body);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["label"], "夸克盘");
        assert_eq!(pages[1]["label"], "百度云");
        assert_eq!(extract_release_notes(body), vec!["修复播放稳定性问题"]);
    }

    #[test]
    fn cache_read_write_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ConfigManager::with_user_data_dir(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let saved = cfg.save_current_fx_autosave(&json!({ "depth": 0.2, "lyricFont": "sans" }));
        assert_eq!(saved["ok"], true);
        let read = cfg.read_current_fx_autosave();
        assert_eq!(read["lyricFont"], "sans");

        let lc = cfg.write_lyric_cache("netease:12345", &json!({ "lrc": "[00:01.00]hi" }));
        assert_eq!(lc["ok"], true);
        let hit = cfg.read_lyric_cache("netease:12345");
        assert_eq!(hit["hit"], true);
        assert_eq!(hit["data"]["lrc"], "[00:01.00]hi");

        let bm_write = cfg.write_beatmap_cache(&json!({
            "key": "netease:12345",
            "map": { "duration": 180.0, "gridStep": 0.5, "beats": [] },
            "meta": { "title": "Track" }
        })).unwrap();
        assert_eq!(bm_write["ok"], true);
        let bm_read = cfg.read_beatmap_cache("netease:12345").unwrap().unwrap();
        assert_eq!(bm_read["meta"]["title"], "Track");
    }
}
```

- [ ] **Step 2: 运行测试确认因未实现而失败**

Run: `cd src-tauri && cargo test config::tests`
Expected: FAIL (`cannot find function compare_versions` 等编译失败)。

- [ ] **Step 3: 编写 `config.rs` 完整实现**

实现以下公开 API 与结构：
- `UpdateConfig`、`CacheSettings`、`AppPaths`、`ConfigManager`（含 `discover()`、`with_user_data_dir()`、`get_cache_settings()`、`set_cache_settings()`、`read_current_fx_autosave()`、`save_current_fx_autosave()`、`read_lyric_cache()`、`write_lyric_cache()`、`beat_cache_root_info()`、`read_beatmap_cache()`、`write_beatmap_cache()`）。
- 原子文件工具 `write_text_atomic`、`write_json_atomic`、`sanitize_cache_key`。
- 版本与 Release 解析工具 `normalize_version`、`compare_versions`、`safe_external_update_url`、`extract_release_download_pages`、`extract_release_notes`。

- [ ] **Step 4: 运行测试验证全部通过**

Run: `cd src-tauri && cargo test config::tests`
Expected: `3 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 2**

```bash
git add src-tauri/src/config.rs src-tauri/src/lib.rs
git commit -m "feat(tauri): implement ConfigManager, atomic persistence, and update metadata helpers"
```

---

### Task 3: 实现 `desktop/easter_egg_gate.rs`（登录彩蛋门控与凭据清理）

**Files:**
- Create: `src-tauri/src/desktop/mod.rs`
- Create: `src-tauri/src/desktop/easter_egg_gate.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod desktop;`)

- [ ] **Step 1: 编写失败单元测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_clears_credentials_and_unlocks_with_password() {
        let dir = tempfile::tempdir().unwrap();
        let cookie = dir.path().join(".cookie");
        fs::write(&cookie, "MUSIC_U=secret").unwrap();

        let gate = LoginEasterEggGate::new(dir.path().to_path_buf(), vec![]);
        let init = gate.initialize(|| {});
        assert_eq!(init["resetPerformed"], true);
        assert_eq!(init["unlocked"], false);
        assert!(!cookie.exists());

        let bad = gate.unlock("wrong");
        assert_eq!(bad["ok"], false);
        assert_eq!(bad["error"], "LOGIN_EASTER_EGG_INVALID");

        let ok = gate.unlock("世界和平");
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["unlocked"], true);
        assert!(gate.is_unlocked());

        let reset = gate.reset_for_replay(|| {});
        assert_eq!(reset["replayReset"], true);
        assert!(!gate.is_unlocked());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test desktop::easter_egg_gate::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `LoginEasterEggGate`**

实现 `LOGIN_EASTER_EGG_GATE_VERSION` (`"world-peace-v1"`)、`LOGIN_EASTER_EGG_PASSWORD` (`"世界和平"`)、`secure_password_match`（恒定时间异或比较）、`initialize`、`public_status`、`is_unlocked`、`unlock`、`reset_for_replay`、`clear_credential_files`。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test desktop::easter_egg_gate::tests`
Expected: `1 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 3**

```bash
git add src-tauri/src/desktop/mod.rs src-tauri/src/desktop/easter_egg_gate.rs src-tauri/src/lib.rs
git commit -m "feat(desktop): port LoginEasterEggGate with constant-time check and credential wipe"
```

---

### Task 4: 实现 `desktop/builtin_playlists.rs`（内置跨平台歌单 CRUD 与持久化）

**Files:**
- Create: `src-tauri/src/desktop/builtin_playlists.rs`
- Modify: `src-tauri/src/desktop/mod.rs`

- [ ] **Step 1: 编写失败单元测试**

测试创建歌单、添加 Netease/QQ 曲目、同 ID 去重 (`duplicate: true`)、拒绝 `spotify` 与非法 `__proto__` 键、曲目重排、以及跨实例磁盘重载验证。
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_playlist_crud_dedup_reorder_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = BuiltInPlaylistLibrary::new(tmp.path().to_path_buf());
        let created = lib.create("深夜电台").unwrap();
        let pid = created["playlist"]["id"].as_str().unwrap().to_string();

        let add1 = lib
            .add_track(&pid, &json!({ "provider": "netease", "id": 1001, "name": "Track A", "artist": "Artist A" }))
            .unwrap();
        assert_eq!(add1["duplicate"], false);
        let dup = lib
            .add_track(&pid, &json!({ "provider": "netease", "id": 1001, "name": "Track A", "artist": "Artist A" }))
            .unwrap();
        assert_eq!(dup["duplicate"], true);

        assert!(lib.add_track(&pid, &json!({ "provider": "spotify", "id": "sp1", "name": "X" })).is_err());

        lib.add_track(&pid, &json!({ "provider": "qq", "mid": "00123", "name": "Track B", "artist": "Artist B" }))
            .unwrap();
        lib.reorder_track(&pid, 0, 1).unwrap();

        let reloaded = BuiltInPlaylistLibrary::new(tmp.path().to_path_buf());
        let page = reloaded.page(&pid, 0, 10);
        assert_eq!(page["total"], 2);
        assert_eq!(page["tracks"][0]["name"], "Track B");
        assert_eq!(page["tracks"][1]["name"], "Track A");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test desktop::builtin_playlists::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `BuiltInPlaylistLibrary`**

实现 100 歌单 / 5000 曲目 / 32 MiB 限制、`sanitize_track`、`list`、`page`、`create`、`rename`、`delete`、`add_track`、`remove_track`、`reorder_track` 及原子持久化。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test desktop::builtin_playlists::tests`
Expected: `1 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 4**

```bash
git add src-tauri/src/desktop/builtin_playlists.rs src-tauri/src/desktop/mod.rs
git commit -m "feat(desktop): port BuiltInPlaylistLibrary with deduplication and atomic storage"
```

---

### Task 5: 实现 `desktop/local_library.rs`（本地曲库导入、Lofty 元数据、GB18030 LRC 与封面预算）

**Files:**
- Create: `src-tauri/src/desktop/local_library.rs`
- Modify: `src-tauri/src/desktop/mod.rs`

- [ ] **Step 1: 编写失败单元测试**

测试：`decode_lyric_buffer` 解码 GB18030 `.lrc`、`cover_within_budget` 拒绝超 6 MiB 或超高像素图片、两阶段 `authorize_files` + `import_authorized` 导入真实 WAV 文件并关联同目录 `.lrc`、重新实例化 `LocalMusicLibrary` 持久恢复且 `resolve_media_file` 验证 `cap` token。
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write_minimal_wav(path: &Path) {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&88200u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn gb18030_sidecar_lyric_decodes_cleanly() {
        let lrc = "[00:01.00]中文歌词测试";
        let (encoded, _, _) = GB18030.encode(lrc);
        assert_eq!(decode_lyric_buffer(&encoded), lrc);
    }

    #[test]
    fn oversized_cover_is_rejected_by_budget() {
        let fake = vec![0xFFu8; MAX_COVER_BYTES + 10];
        assert!(!cover_within_budget(&fake));
    }

    #[test]
    fn local_library_authorize_import_sidecar_lrc_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let music_dir = tmp.path().join("music");
        fs::create_dir_all(&music_dir).unwrap();
        let wav_path = music_dir.join("Song.wav");
        let lrc_path = music_dir.join("Song.lrc");
        write_minimal_wav(&wav_path);
        let (gb, _, _) = GB18030.encode("[00:02.50]测试歌词");
        fs::write(&lrc_path, gb).unwrap();

        let lib = LocalMusicLibrary::new(tmp.path().to_path_buf());
        let auth = lib.authorize_files(
            vec![ImportFileInput { path: wav_path.to_string_lossy().to_string(), relative_path: "Song.wav".into() }],
            false,
        );
        assert_eq!(auth["ok"], true);
        let imported = lib.import_authorized(auth["token"].as_str().unwrap());
        assert_eq!(imported["ok"], true);
        assert_eq!(imported["count"], 1);
        let id = imported["tracks"][0]["localFileId"].as_str().unwrap().to_string();

        let reloaded = LocalMusicLibrary::new(tmp.path().to_path_buf());
        let lyric = reloaded.lyric_for_track(&id);
        assert_eq!(lyric["ok"], true);
        assert_eq!(lyric["lyricSource"], "sidecar");
        assert_eq!(lyric["lyric"], "[00:02.50]测试歌词");

        assert!(reloaded.resolve_media_file("audio", &id, "wrong-cap").is_none());
        assert!(reloaded.resolve_media_file("audio", &id, reloaded.media_token()).is_some());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test desktop::local_library::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `LocalMusicLibrary`**

实现支持音频校验、`lofty` 元数据与封面提取、`image` 尺寸检查、GB18030/UTF-8 侧边 `.lrc` 扫描、原子索引持久化与媒体 Token 安全校验。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test desktop::local_library::tests`
Expected: `3 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 5**

```bash
git add src-tauri/src/desktop/local_library.rs src-tauri/src/desktop/mod.rs
git commit -m "feat(desktop): implement LocalMusicLibrary with Lofty tags, cover budget, and GB18030 LRC"
```

---

### Task 6: 实现 `providers/qishui_decryptor.rs`（汽水音频 MP4 Box 与 AES-128-CTR 解密）

**Files:**
- Create: `src-tauri/src/providers/mod.rs`
- Create: `src-tauri/src/providers/qishui_decryptor.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod providers;`)

- [ ] **Step 1: 编写失败单元测试**

构造加密 M4A 容器（含 `moov/trak/mdia/minf/stbl/stsd(enca)/stsz/senc` + `mdat` AES-128-CTR 密文），以及带 `dfLa` 元数据的 FLAC 封装用例，测试解密还原及扩展名识别。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test providers::qishui_decryptor::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `decrypt_spade_a` 与 `decrypt_qishui_track`**

实现 `spade_a` 解密算法、MP4 Box 递归查找、`stsz`/`senc`/`dfLa` 解析、AES-128-CTR 解密及 M4A (`enca -> mp4a`) / FLAC (`fLaC`) 输出封装。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test providers::qishui_decryptor::tests`
Expected: `2 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 6**

```bash
git add src-tauri/src/providers/mod.rs src-tauri/src/providers/qishui_decryptor.rs src-tauri/src/lib.rs
git commit -m "feat(providers): port Qishui MP4/FLAC AES-128-CTR track decryptor"
```

---

### Task 7: 实现 `cuefield/` 模块（LRC 锚点、结构图谱、过渡规划与反馈日志）

**Files:**
- Create: `src-tauri/src/cuefield/mod.rs`
- Create: `src-tauri/src/cuefield/lrc.rs`
- Create: `src-tauri/src/cuefield/structure.rs`
- Create: `src-tauri/src/cuefield/planner.rs`
- Create: `src-tauri/src/cuefield/feedback.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod cuefield;`)

- [ ] **Step 1: 编写失败单元测试**

在 `cuefield/mod.rs` 编写 3 组测试：
1. `cuefield_plans_transition_between_cached_tracks`：验证双曲目缓存规划出合理 `mixStart`/`handoffAt`、时间轴动作序列与 64 位 SHA-256 `artifactId`；
2. `cuefield_end_of_track_fallback_with_tail_evidence`：验证开启 `enable_cadence_fallback` + `tailEvidence` 时生成 `end-of-track-crossfade` 方案；
3. `cuefield_feedback_validates_rating_and_records_jsonl`：验证仅接受评分 `1/2/3` 并统计 JSONL 记录。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test cuefield::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `lrc.rs`、`structure.rs`、`planner.rs`、`feedback.rs` 与 `mod.rs`**

完整实现 LRC 解析、节拍网格归一化（兼容紧凑数组与对象格式）、小节/乐句切分、保护期计算、配方去重路由、时间轴构造与反馈日志读写。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test cuefield::tests`
Expected: `3 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 7**

```bash
git add src-tauri/src/cuefield src-tauri/src/lib.rs
git commit -m "feat(cuefield): port AutoMix transition planner, structure analyzer, and feedback log"
```

---

### Task 8: 实现 `analyzer/dj.rs`（Symphonia PCM 解码与 DJ/播客节奏分析）

**Files:**
- Create: `src-tauri/src/analyzer/mod.rs`
- Create: `src-tauri/src/analyzer/dj.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod analyzer;`)

- [ ] **Step 1: 编写失败单元测试**

构造一段带周期性 120 BPM（每 `0.5s` 出现一个低频脉冲）的合成单声道 44100 Hz WAV 字节流（持续 16 秒），断言 `analyze_audio_bytes` 能解码并检测出接近 `0.5s`（允许 `±0.05s`）的 `gridStep`、非空 `beats`/`pulseBeats`/`cameraBeats`/`kicks`。
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn synthesize_pulse_wav(sample_rate: u32, duration_sec: f64, beat_period_sec: f64) -> Vec<u8> {
        let num_samples = (sample_rate as f64 * duration_sec) as usize;
        let mut pcm = Vec::with_capacity(num_samples * 2);
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let phase_in_beat = t % beat_period_sec;
            let val = if phase_in_beat < 0.04 {
                (2.0 * std::f64::consts::PI * 70.0 * phase_in_beat).sin() * 0.9
            } else {
                0.0
            };
            let s16 = (val * 32767.0) as i16;
            pcm.extend_from_slice(&s16.to_le_bytes());
        }
        let data_len = pcm.len() as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&pcm);
        wav
    }

    #[test]
    fn dj_analyzer_detects_periodic_low_end_beats() {
        let wav = synthesize_pulse_wav(44100, 16.0, 0.5);
        let map = analyze_audio_bytes(&wav, Some("wav"), 16.0).unwrap();
        let grid_step = map["gridStep"].as_f64().unwrap();
        assert!((grid_step - 0.5).abs() < 0.06, "unexpected gridStep: {grid_step}");
        assert!(map["beats"].as_array().unwrap().len() >= 20);
        assert!(map["cameraBeats"].as_array().unwrap().len() >= 10);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test analyzer::dj::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `analyzer/dj.rs`**

使用 `symphonia` 解码输入字节为单声道样本流，经过 32 Hz 高通 + 178 Hz 低通二阶 Biquad 滤波器计算 10ms RMS/峰值帧，再执行局部自适应阈值峰值挑选与全局节拍网格估算，输出与 `dj-analyzer.js` 兼容的 Beatmap JSON；并提供 `analyze_audio_url` 通过 `reqwest` 下载分析（带总字节上限与超时防护）。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test analyzer::dj::tests`
Expected: `1 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 8**

```bash
git add src-tauri/src/analyzer src-tauri/src/lib.rs
git commit -m "feat(analyzer): port podcast/DJ audio beatmap analyzer using Symphonia"
```

---

### Task 9: 实现 `server/mod.rs`（Axum 本地回环媒体与核心 API 服务 + 集成契约测试）

**Files:**
- Create: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/lib.rs` (添加 `pub mod server;`)

- [ ] **Step 1: 编写失败的 Axum HTTP 集成测试（`tower::ServiceExt::oneshot`）**

测试覆盖：
1. `GET /api/app/version` 返回 `200` 及 `version`；
2. `GET /api/spotify/search` 返回 `404 PROVIDER_REMOVED`；
3. `POST /api/login/qr/key` 在彩蛋门控未解锁时返回 `423 LOGIN_EASTER_EGG_LOCKED`；
4. `POST /api/beatmap/cache` + `GET /api/beatmap/cache` + `POST /api/cuefield/transition` + `POST/GET /api/cuefield/feedback` 端到端闭环；
5. `GET /api/update/download` 返回 `410 UPDATE_EXTERNAL_ONLY`；
6. `GET /local-media/audio/{id}?cap={token}` 带 `Range: bytes=0-9` 返回 `206 Partial Content` 与 `Content-Range` 头，无效 `cap` 返回 `404`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test server::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `server/mod.rs`**

实现 `AppBackendState`、`build_router(state)`、`start_local_server(state)`（绑定 `127.0.0.1:0` 或优先 `3000` 并返回真实端口）：
- `CorsLayer::permissive()` 支持 `tauri://` 与 `http://localhost:*` 跨域；
- 挂载全部 M1 核心路由：`/api/app/version`、`/api/platform/capabilities`、`/api/update/latest`（支持 `MINERADIO_UPDATE_MANIFEST` 本地/远程覆写 + GitHub API + `latest.yml` 回退）、`/api/update/download|patch*` (`410`)、`/api/spotify*` (`404`)、`/api/beatmap/cache/status`、`/api/beatmap/cache`、`/api/cuefield/transition`、`/api/cuefield/feedback`、`/api/podcast/dj-beatmap`、`/api/cover`、`/api/audio`（含 `#auth=` 汽水解密与 Range 流转发）、`/local-media/{kind}/{id}`（带 `Range` 解析）、及彩蛋门控中间件。

- [ ] **Step 4: 运行测试验证通过**

Run: `cd src-tauri && cargo test server::tests`
Expected: `6 passed; 0 failed`。

- [ ] **Step 5: 提交 Task 9**

```bash
git add src-tauri/src/server/mod.rs src-tauri/src/lib.rs
git commit -m "feat(server): implement Axum local media proxy, Range streaming, and core API routes"
```

---

### Task 10: 实现 Tauri 2 桌面层（`memory.rs`, `win32.rs`, `commands.rs`, `desktop/mod.rs`, `lib.rs`）与跨平台编译验收

**Files:**
- Create: `src-tauri/src/desktop/memory.rs`
- Create: `src-tauri/src/desktop/win32.rs`
- Create: `src-tauri/src/desktop/commands.rs`
- Modify: `src-tauri/src/desktop/mod.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: 为跨平台桌面状态与命令辅助逻辑编写失败单元测试**

测试：内存快照/清理在非 Windows 返回受控不支持状态、彩蛋与歌单/本地曲库通过共享状态结构一致响应、以及 Windows 条件编译模块的类型签名一致性。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test desktop::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `memory.rs`、`win32.rs`、`commands.rs` 与 `lib.rs` Tauri 启动装配**

1. `memory.rs` & `win32.rs`：在 `#[cfg(target_os = "windows")]` 使用 `windows` crate 实现 `EmptyWorkingSet` / `GetProcessMemoryInfo` 与 `Progman`/`WorkerW` 基础桌面附加/脱离与图标显隐控制；非 Windows 安全返回不支持状态。
2. `commands.rs`：导出全部 M1 `#[tauri::command]`：
   - `get_runtime_endpoints`（返回 `{ httpBase, mediaToken }`）
   - `desktop_window_minimize`、`desktop_window_restore`、`desktop_window_toggle_maximize`、`desktop_window_toggle_fullscreen`、`desktop_window_exit_fullscreen_windowed`、`desktop_window_get_state`、`desktop_window_close`、`desktop_window_get_close_behavior`、`desktop_window_set_close_behavior`
   - `get_memory_snapshot`、`trim_app_memory`、`purge_system_memory`
   - `get_cache_settings`、`set_cache_settings`、`choose_cache_directory`
   - `read_current_fx_autosave`、`save_current_fx_autosave`、`read_lyric_cache`、`write_lyric_cache`
   - `get_login_easter_egg_status`、`unlock_login_easter_egg`、`reset_login_easter_egg`
   - `list_built_in_playlists`、`read_built_in_playlist`、`create_built_in_playlist`、`rename_built_in_playlist`、`delete_built_in_playlist`、`add_built_in_playlist_track`、`remove_built_in_playlist_track`、`reorder_built_in_playlist_track`
   - `list_local_music_library`、`read_local_music_lyric`、`authorize_local_music`、`import_local_music`、`choose_local_music_files`
   - `open_update_page`（仅放行白名单外部 URL 用 `opener::open` 打开）
   - `copy_text`、`read_text`、`export_json_file`、`import_json_file`
   - `set_desktop_lyrics_enabled`、`update_desktop_lyrics`、`set_desktop_lyrics_lock_state`、`move_desktop_lyrics_by`
   - `configure_global_hotkeys`
3. `lib.rs`：装配 `single-instance`、`global-shortcut`、`dialog`、托盘图标与菜单、自定义协议 `mineradio-local`、拉起 Axum 本地服务并创建主窗口 `main`。

- [ ] **Step 4: 运行全量测试与跨平台检查**

Run:
```bash
cd src-tauri && CARGO_SOURCE_USTC_REGISTRY="sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/" cargo test
cd src-tauri && CARGO_SOURCE_USTC_REGISTRY="sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/" cargo check --target x86_64-pc-windows-msvc
```
Expected: 全部单元测试 & Axum 集成测试 `PASS`；macOS 原生构建与 `x86_64-pc-windows-msvc` 目标均 `0 errors`。

- [ ] **Step 5: 提交 Task 10**

```bash
git add src-tauri/src
git commit -m "feat(desktop): complete Tauri 2 window/tray/commands shell and Windows conditional layer"
```
