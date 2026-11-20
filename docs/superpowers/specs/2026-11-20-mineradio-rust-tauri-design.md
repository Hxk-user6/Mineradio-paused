# Mineradio Rust + Tauri 2 + React 重构设计规格（全局总览 & M1 规格）

> 分支：`refactor/rust-tauri`（基于 `main` 创建，主分支保持只读）
> 日期：2026-11-20
> 许可证：GPL-3.0-only（遵守 `LICENSE`、`NOTICE.md` 与 `docs/THIRD_PARTY_PORTS.md`）

---

## 1. 背景与已对齐的设计决策

Mineradio 当前为基于 Electron (`^42.4.1`) + Node.js 原生 `http` 服务（约 4.2 万行）与原生拼接前端 JS/Three.js（约 7.8 万行）的桌面音乐播放器。本次重构在保持功能、交互流程与视觉呈现与 `v2.2.0` 一致的前提下，将桌面壳与后端全量重构为 **Rust + Tauri 2**，并将前端重构为 **React + Vite + TypeScript UI 面板 + 独立命令式 3D/Audio 引擎**。

通过 `/grill-me` 决策树，已确认以下 7 项核心架构决策：

1. **前端分层架构**：采用 `React + Vite + TypeScript` 负责 UI 面板壳层（首页 Dashboard、搜索、播放控制栏、歌单/队列面板、FX 控制台、设置/登录弹窗、桌面歌词悬浮窗）；`Three.js r128` 粒子场景、3D 歌单架、基于帧预算的舞台歌词渲染、以及 Web Audio / Cuefield 时间轴执行器封装为**独立命令式 TS/JS 引擎**，严禁将逐帧渲染塞入 React Virtual DOM。
2. **承载与前后端通信**：采用标准 Tauri 2 + Vite 模式（前端构建产物托管于 `tauri://`，开发期由 Vite 提供 HMR）；UI 业务与桌面控制通过 Tauri `invoke` / Event 通道调用 Rust；Rust 内嵌 `axum` 本地 HTTP 回环服务（`127.0.0.1:<open_port>`，配置 `CORS *`）专责音频 Range 流代理、汽水音频实时解密流、封面 CORS 代理与可选 HTTP 媒体接口。
3. **第三方音乐平台与签名策略**：全量纯 Rust 重写网易云（WeAPI/EAPI）、QQ 音乐、酷狗音乐、汽水目录与 AES-CTR 音频解密；汽水扫码签名使用 Tauri 独立 Webview + 本地 `axum` 反向代理（并保留手动 Cookie 导入兜底）。
4. **Windows 原生能力与跨平台边界**：核心应用与透明桌面歌词窗口全平台可用；Windows 下通过 `#[cfg(target_os = "windows")]` 实现 WorkerW 桌面嵌入、内存工作集裁剪与 Wallpaper Engine 本地项目索引/视频层，DWM 缩略图与高级图标遮罩预留接口并标明当前限制；非 Windows 平台（macOS/Linux）安全降级并明确返回不支持状态。
5. **目录结构与旧代码处置**：并行新建 `frontend/`（React + Vite + TS）与 `src-tauri/`（Rust + Tauri 2），复用 `public/css/`、`public/assets/`、`public/vendor/` 及前端引擎核心逻辑；迁移期保留原 `desktop/`、`server.js`、`*-api.js`、`public/` 作为行为/视觉/契约对照，待主流程验收后再统一归档清理。
6. **里程碑拆解与交付节奏**：拆分为 **M1 / M2 / M3** 三个独立可验证的子里程碑，按阶段独立完成 Spec → Plan → 实现 → 验证。
7. **工作区与验证基线**：在干净的 `refactor/rust-tauri` 分支上严格按 TDD 从零实施；建立 `cargo test`（单元 + Axum 路由契约测试）+ `cargo check --target x86_64-pc-windows-msvc` + Vite/Vitest 验证体系，原 Node `tests/` 仅作行为对照。

---

## 2. 全局目录结构与组件划分

```text
Mineradio-paused/
├── src-tauri/                     # [新] Rust + Tauri 2 桌面与后端工程
│   ├── Cargo.toml
│   ├── build.rs
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   ├── icons/
│   └── src/
│       ├── main.rs
│       ├── lib.rs
│       ├── config.rs              # 路径、缓存设置、FX 自动保存、更新配置
│       ├── server/                # Axum 本地 HTTP 媒体与 API 代理服务
│       ├── providers/             # 网易云/QQ/酷狗/汽水客户端与音频解密
│       ├── cuefield/              # AutoMix 分段、规划、Artifact 与反馈日志
│       ├── analyzer/              # DJ/播客音频流节拍分析 (Symphonia)
│       └── desktop/               # Tauri 窗口/托盘/快捷键、本地曲库、内置歌单、彩蛋门控、Win32
├── frontend/                      # [新] React + Vite + TypeScript 前端工程 (M2)
│   ├── package.json
│   ├── vite.config.ts
│   ├── index.html
│   ├── desktop-lyrics.html
│   └── src/
│       ├── app/                   # React UI 面板、弹窗与状态 Store (Zustand)
│       ├── bridge/                # Tauri invoke/event 与 Axum 媒体 Base URL 适配层
│       ├── engines/               # 命令式 Three.js 场景/歌词/歌单架 & Web Audio/Cuefield 引擎
│       └── styles/                # 复用并模块化 public/css/index.css
├── docs/superpowers/
│   ├── specs/                     # 设计规格文档
│   └── plans/                     # 实施计划文档
├── public/ desktop/ server.js ... # [保留对照] 旧 Electron/Node 代码，验收后清理
```

---

## 3. 三个里程碑分解总览

| 里程碑 | 目标范围 | 独立验收门槛 |
| --- | --- | --- |
| **M1：Rust + Tauri 核心后端与桌面基础** | `src-tauri/` 工程初始化、配置与持久化、登录彩蛋门控、内置跨平台歌单、本地音乐曲库（`lofty` + GB18030 LRC + 封面尺寸预算）、汽水 MP4/AES-CTR 音频解密、Cuefield AutoMix 引擎、Symphonia DJ 节拍分析、Axum 本地回环媒体/核心 API 服务（`/api/audio` Range、`/api/cover`、`/local-media/*`、`/api/cuefield/*`、`/api/beatmap/*`、`/api/update/latest`）、Tauri 主窗口 + 桌面歌词窗口 + 托盘 + 全局热键基础壳。 | `cargo test` 全部通过（含单元与 Axum HTTP 集成测试）；`cargo check`（macOS）与 `cargo check --target x86_64-pc-windows-msvc`（Windows 交叉检查）通过。 |
| **M2：React + Vite 前端 UI 壳与 3D/Audio 命令式引擎闭环** | 搭建 `frontend/`（React 19 + Vite + TS + Zustand + Vitest），封装并接入命令式 `Three.js` 场景/粒子/舞台歌词/3D 歌单架与 `Web Audio/Cuefield` 执行器；1:1 复刻启动 Splash、搜索框、首页 Dashboard、底部播控栏、歌单/队列侧栏、FX 控制台、本地导入与桌面歌词悬浮窗；接入 Tauri 主窗口与开发/构建流。 | `tsc --noEmit`、`vitest run`、`vite build` 与 `cargo tauri build --no-bundle` 通过；Tauri 应用可本地启动，支持本地音乐导入、播放可视化、3D 歌单架、歌词舞台与桌面歌词悬浮窗联动。 |
| **M3：第三方音乐平台全量协议 & Windows 原生桌面适配** | 纯 Rust 实现网易云（WeAPI/EAPI 全部 40 项接口、扫码/Cookie 登录、同音轨匹配）、QQ 音乐（`musicu.fcg`、搜索、歌单、VIP 严格判定）、酷狗音乐（Android/H5 MD5 签名、KRC 歌词、VIP）、汽水音乐（公开/Web/OpenAPI、Passport QR 反向代理签名与 Cookie 兜底）；Windows `#[cfg(target_os = "windows")]` WorkerW 桌面嵌入、图标可见控制、Wallpaper Engine 索引/视频背景与内存裁剪。 | 各平台接口与协议解析单测/Mock 集成测试全绿；Windows 交叉编译通过；完整汇总交付与已知限制清单。 |

---

## 4. M1 详细设计规格

### 4.1 `config.rs`（路径、缓存、FX 自动保存、Beatmap 缓存、更新解析）
- **目录规则**：
  - `userData`：优先级为测试隔离 env（`MINERADIO_STARTUP_QA_HIDDEN=1` + 绝对路径 `MINERADIO_STARTUP_QA_USER_DATA`）> `MINERADIO_USER_DATA_DIR` > 系统标准应用数据目录（`dirs::data_dir()/Mineradio`）。
  - 缓存根目录：Windows 上默认优先 `D:\MineradioCache`（C 盘受保护禁用 beatmap 磁盘写入），非 Windows 使用 `dirs::cache_dir()/MineradioCache`；支持通过 `cache-settings.json` 修改并持久化。
- **核心能力**：
  - `current-fx-autosave.json`（上限 12 MiB）原子读写（临时文件 + `fs::rename`）。
  - 歌词磁盘缓存（`LYRIC_CACHE_VERSION = 1`，单条上限 1 MiB，key 规范化防路径穿越）。
  - Beatmap 磁盘缓存读写与状态查询（`C_DRIVE_DISABLED` / `TARGET_DRIVE_UNAVAILABLE` / `disk` / `memory-only`）。
  - 版本号 SemVer 规范化与比较、GitHub Release 正文 `<!-- mineradio-download-page: 标签|URL -->` 提取与安全域名白名单校验（`github.com`, `pan.quark.cn`, `pan.baidu.com`, `123pan`, `lanzou*`, `mineradio.cn`）。

### 4.2 `desktop/easter_egg_gate.rs`（登录彩蛋门控）
- 对齐原 `desktop/login-easter-egg-gate.js`：
  - 版本标识 `world-peace-v1`，状态文件 `login-easter-egg.json`。
  - 首次初始化自动清理全部历史平台凭据文件（`.cookie`、`.qq-cookie`、`.kugou-cookie`、`.kugou-vip-evidence.json`、`.qishui-cookie`、`.qishui-token`、`.spotify-token.json`）并触发运行时会话重置回调。
  - 密码 `"世界和平"` 采用常量时间字节比对（防时序侧信道）。
  - 提供 `initialize`、`public_status`、`is_unlocked`、`unlock`、`reset_for_replay` 方法；未解锁时，后端受保护的平台登录路由统一返回 HTTP `423 LOGIN_EASTER_EGG_LOCKED`。

### 4.3 `desktop/builtin_playlists.rs`（内置跨平台歌单）
- 对齐原 `desktop/built-in-playlist-library.js`：
  - 持久化至 `built-in-playlists.json`（上限 32 MiB），最多 100 个歌单、单歌单最多 5000 首。
  - 跨平台曲目身份去重：`netease:<id>`、`qq:<mid|id>`、`kugou:<lowercase_hash|id>`、`qishui:<id>`、`local:<localFileId>`；拒绝 `spotify` 与 `__proto__`/`constructor` 等危险键。
  - 支持 `list`、`page(offset, limit)`、`create`、`rename`、`delete`、`add_track`（返回 `duplicate` 标志）、`remove_track`、`reorder_track`，全部持久化采用原子写入。

### 4.4 `desktop/local_library.rs`（本地音乐曲库）
- 对齐原 `desktop/local-music-library.js`：
  - 支持扩展名：`.mp3`、`.flac`、`.wav`、`.ogg`、`.m4a`、`.aac`、`.opus`（绝对路径且禁止 UNC `\\` 路径）。
  - `lofty` 提取标题、歌手、专辑、时长、内嵌歌词（`ItemKey::Lyrics`）及内嵌封面；结合 `image::ImageReader::into_dimensions()` 执行封面前置尺寸预算检查（最大 `6 MiB`、宽高 ≤ `4096`、总像素 ≤ `12MP`；无法解析尺寸的图片上限 `1 MiB`）。
  - 同目录同名 `.lrc` 侧边歌词扫描与解码：优先 UTF-8（剥离 BOM），无效 UTF-8 回退 `encoding_rs::GB18030`。
  - 授权两阶段导入（`authorize_files` 生成限时 token + `import_authorized` 执行解析持久化）与索引文件 `local-music-library.json`（上限 16 MiB）。
  - 媒体访问令牌 `media_token` 校验（防任意文件读取），供 Axum `/local-media/{kind}/{id}` 与 Tauri 协议 `mineradio-local://` 复用，支持 HTTP `Range` (`206 Partial Content`)。

### 4.5 `providers/qishui_decryptor.rs`（汽水加密音频实时解密）
- 对齐原 `qishui-audio-decryptor/`：
  - `spade_a` Base64 + 位计数异或变换密钥解析，支持直接 16 字节 Hex 密钥或 `spade_a` 密文。
  - MP4 Atom 解析：`moov -> trak -> mdia -> minf -> stbl -> (stsd, stsz, senc)` 与 `mdat`。
  - 使用 `aes` + `ctr` (`Aes128Ctr` 128-bit BE counter) 逐样本解密；若 `stsd` 包含 `dfLa` 则封装输出 `.flac`（`fLaC` + 元数据 + 明文帧），否则原位替换样本并在 `stsd` 将 `enca` 改为 `mp4a` 输出 `.m4a`。

### 4.6 `cuefield/`（AutoMix 过渡规划引擎）
- 对齐原 `cuefield/` 核心契约：
  - `lrc.rs`：LRC 时间戳解析（`[mm:ss.xx]`）、文本归一化、重复副歌组识别、歌词连结评分。
  - `structure.rs`：兼容数组紧凑格式与对象格式的节拍表归一化、BPM/拍号稳定性评分、小节与 8 小节乐句切分、保护期（`protectedUntil`）与出/入口候选点生成。
  - `planner.rs`：结合 `exitBias`、`maxEntryTime`、`recentRecipes`（去重避免连续同配方）、`minimumListenUntil` 与 `tailEvidence` 兜底交叉淡化，生成完整时间轴动作序列（`play`/`volume`/`filter`/`spectrum`/`stop`/`handoff`）及 SHA-256 `artifactId`。
  - `feedback.rs`：校验 `rating ∈ {1, 2, 3}` 并追加写入本地 `cuefield-feedback.jsonl`，统计评分分布。

### 4.7 `analyzer/dj.rs`（DJ / 播客节奏分析）
- 对齐原 `dj-analyzer.js` 输出字段：
  - 接收 HTTP 音频流或本地文件字节，使用 `symphonia` 解码 PCM（支持 MP3/AAC/M4A/FLAC/OGG/WAV），经高通 (`32 Hz`) + 低通 (`178 Hz`) 二阶 Biquad 滤波计算 10ms 帧低频 RMS 能量包络与峰值瞬态。
  - 检测局部峰值为 onset/beat，估计全局 `gridStep` 与 BPM，生成 `beats`、`pulseBeats`、`cameraBeats`、`kicks`、`duration` 与 `tempoSource`。

### 4.8 `server/`（Axum 本地 HTTP 媒体与核心 API 服务）
- 监听 `127.0.0.1:0`（或优先 `3000` 失败回退随机端口，测试可用 `:0`），配置全开放 `CORS`（支持 `tauri://localhost`、`http://tauri.localhost`、`http://localhost:*`）。
- M1 阶段挂载以下真实可测路由：
  - `GET /api/app/version`、`GET /api/platform/capabilities`、`GET /api/update/latest`、`ALL /api/update/download|patch*`（返回 `410 UPDATE_EXTERNAL_ONLY`）
  - `ALL /api/spotify*`（返回 `404 PROVIDER_REMOVED`）
  - `GET /api/beatmap/cache/status`、`GET/POST /api/beatmap/cache`
  - `POST /api/cuefield/transition`、`GET/POST /api/cuefield/feedback`
  - `POST /api/podcast/dj-beatmap`
  - `GET /api/cover`（带安全协议校验与上游透传 + CORS）
  - `GET /api/audio`（支持 `Range`、`#auth=` 汽水实时解密并缓存、流式转发）
  - `GET /local-media/{kind}/{id}`（本地音频 Range 流与内嵌封面读取，校验 `cap` token）
  - `GET /we-media/{id}`（Wallpaper Engine 资产安全读取，校验 `cap` token）
  - Gate 拦截中间件：登录相关路由在门控未解锁时返回 `423 LOGIN_EASTER_EGG_LOCKED`。

### 4.9 `desktop/`（Tauri 2 基础壳、多窗口与命令接口）
- Tauri 2 配置与启动装配：
  - 插件：`tauri-plugin-single-instance`、`tauri-plugin-global-shortcut`、`tauri-plugin-dialog`。
  - 系统托盘（显示/隐藏主窗口、退出）。
  - 主窗口（无边框透明 `decorations: false, transparent: true`）与桌面歌词悬浮窗（独立透明置顶无边框窗口，支持 `set_ignore_cursor_events` 鼠标穿透与事件状态广播）。
  - `commands.rs` 导出全套 M1 `#[tauri::command]`（窗口控制、关闭行为、彩蛋门控、内置歌单 CRUD、本地曲库授权/导入/查询/歌词、缓存设置、FX 自动保存、歌词缓存、外部 URL 打开、JSON 导入导出、剪贴板读写、内存快照、桌面歌词开关/状态同步/拖拽/锁定）。
  - 自定义 URI Scheme `mineradio-local` 与 `mineradio-wallpaper`（均带 `Range` 与 CORS 响应头），同时保留 Axum HTTP 媒体端点作为跨 WebView 兜底。

---

## 5. 错误处理与风险缓释

1. **Tauri 与 Axum 端口动态绑定**：前端通过 Tauri Command `get_runtime_endpoints()` 获取实际分配的 `http://127.0.0.1:<port>`，避免硬编码 `3000` 端口冲突导致启动失败。
2. **文件写入原子性**：所有 JSON 索引、配置、存档均采用“同目录临时文件 + `fs::rename`”，防止中途断电/崩溃损坏主索引。
3. **路径与原型污染防护**：本地文件与 Wallpaper Engine 资产读取严格执行规范化路径前缀校验 + 随机 `mediaToken` 校验，拒绝 UNC 路径与 `..` 逃逸。
4. **跨平台验证差距**：M1 在 macOS 环境可跑通全部 `cargo test` 及 macOS 本地 Tauri 编译运行；Windows 原生边界通过 `cargo check --target x86_64-pc-windows-msvc` 静态验证，具体 Windows 运行时表现将在已知限制中明确标注。
