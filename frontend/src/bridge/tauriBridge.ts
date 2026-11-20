import type {
  CacheSettings,
  DesktopLyricsPayload,
  DesktopWindowState,
  EasterEggStatus,
  FxSettings,
  MemorySnapshot,
  PlaylistSummary,
  RuntimeEndpoints,
  SongRecord,
  WallpaperEngineProject,
} from '../types/mineradio';

export interface TauriInternals {
  invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  transformCallback?: (callback: (response: unknown) => void, once?: boolean) => number;
  unregisterCallback?: (id: number) => void;
}

export interface TauriGlobalApi {
  core?: {
    invoke?: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  };
  event?: {
    listen?: <T = unknown>(
      event: string,
      handler: (event: { event: string; payload: T }) => void
    ) => Promise<() => void>;
  };
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
    __TAURI__?: TauriGlobalApi;
  }
}

export type UnlistenFn = () => void;

const DEFAULT_HTTP_BASE = 'http://127.0.0.1:3000';
const FX_AUTOSAVE_LS_KEY = 'mineradio-current-fx-autosave-v1';
const LYRIC_CACHE_LS_PREFIX = 'mineradio-lyric-cache:';
const BUILTIN_PLAYLISTS_LS_KEY = 'mineradio-builtin-playlists-v1';
const EASTER_EGG_LS_KEY = 'mineradio-login-easter-egg-v1';
const CLOSE_BEHAVIOR_LS_KEY = 'mineradio-close-behavior-v1';

const localEventBus = new EventTarget();

let cachedEndpoints: RuntimeEndpoints | null = null;
let cachedForTauri: boolean | null = null;
let pendingEndpointsPromise: Promise<RuntimeEndpoints> | null = null;

function normalizeHttpBase(base: string | undefined | null): string {
  const raw = String(base || '').trim().replace(/\/+$/, '');
  return raw || DEFAULT_HTTP_BASE;
}

export function isTauriRuntime(): boolean {
  if (typeof window === 'undefined') return false;
  return Boolean(
    (window.__TAURI_INTERNALS__ && typeof window.__TAURI_INTERNALS__.invoke === 'function') ||
      (window.__TAURI__ && window.__TAURI__.core && typeof window.__TAURI__.core.invoke === 'function')
  );
}

export function resetRuntimeEndpointsCache(): void {
  cachedEndpoints = null;
  cachedForTauri = null;
  pendingEndpointsPromise = null;
}

export async function invokeCommand<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>
): Promise<T> {
  if (typeof window === 'undefined') {
    throw new Error(`TAURI_UNAVAILABLE:${cmd}`);
  }
  if (window.__TAURI_INTERNALS__ && typeof window.__TAURI_INTERNALS__.invoke === 'function') {
    return window.__TAURI_INTERNALS__.invoke<T>(cmd, args);
  }
  if (window.__TAURI__?.core?.invoke) {
    return window.__TAURI__.core.invoke<T>(cmd, args);
  }
  throw new Error(`TAURI_UNAVAILABLE:${cmd}`);
}

async function safeInvoke<T>(
  cmd: string,
  args: Record<string, unknown> | undefined,
  fallback: () => T | Promise<T>
): Promise<T> {
  if (isTauriRuntime()) {
    try {
      return await invokeCommand<T>(cmd, args);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (!msg.startsWith('TAURI_UNAVAILABLE:')) {
        throw err;
      }
    }
  }
  return await fallback();
}

async function safeInvokeGraceful<T>(
  cmd: string,
  args: Record<string, unknown> | undefined,
  fallback: (err?: unknown) => T | Promise<T>
): Promise<T> {
  if (isTauriRuntime()) {
    try {
      return await invokeCommand<T>(cmd, args);
    } catch (err) {
      return await fallback(err);
    }
  }
  return await fallback();
}

export async function getRuntimeEndpoints(forceRefresh = false): Promise<RuntimeEndpoints> {
  const tauri = isTauriRuntime();
  if (!forceRefresh && cachedEndpoints && cachedForTauri === tauri) {
    return cachedEndpoints;
  }
  if (!forceRefresh && pendingEndpointsPromise && cachedForTauri === tauri) {
    return pendingEndpointsPromise;
  }

  cachedForTauri = tauri;
  pendingEndpointsPromise = (async () => {
    if (tauri) {
      try {
        const res = await invokeCommand<Partial<RuntimeEndpoints>>('get_runtime_endpoints');
        const normalized: RuntimeEndpoints = {
          httpBase: normalizeHttpBase(res?.httpBase),
          mediaToken: String(res?.mediaToken || ''),
        };
        cachedEndpoints = normalized;
        return normalized;
      } catch (err) {
        console.warn('[tauriBridge] get_runtime_endpoints failed, falling back to default:', err);
      }
    }
    const fallback: RuntimeEndpoints = {
      httpBase: DEFAULT_HTTP_BASE,
      mediaToken: '',
    };
    cachedEndpoints = fallback;
    return fallback;
  })();

  try {
    return await pendingEndpointsPromise;
  } finally {
    pendingEndpointsPromise = null;
  }
}

export function getCachedHttpBase(): string {
  return cachedEndpoints?.httpBase || DEFAULT_HTTP_BASE;
}

export function getCachedMediaToken(): string {
  return cachedEndpoints?.mediaToken || '';
}

function appendQueryParam(url: string, key: string, value: string): string {
  if (!value) return url;
  const hashIdx = url.indexOf('#');
  const base = hashIdx >= 0 ? url.slice(0, hashIdx) : url;
  const hash = hashIdx >= 0 ? url.slice(hashIdx) : '';
  const sep = base.includes('?') ? '&' : '?';
  return `${base}${sep}${encodeURIComponent(key)}=${encodeURIComponent(value)}${hash}`;
}

/**
 * 解析本地协议 URL (mineradio-local:// / mineradio-wallpaper://) 与相对 API 路径
 * 在浏览器降级模式或显式开启 preferHttpFallback 时转换为 Axum HTTP 兜底 URL
 */
export function resolveMediaUrl(
  url: string | undefined | null,
  opts?: { preferHttpFallback?: boolean }
): string {
  const raw = String(url || '').trim();
  if (!raw) return '';
  if (/^(data:|blob:)/i.test(raw)) return raw;
  if (raw.startsWith('/assets/')) return raw;

  const httpBase = getCachedHttpBase();
  const mediaToken = getCachedMediaToken();
  const preferHttp = opts?.preferHttpFallback ?? !isTauriRuntime();

  if (raw.startsWith('mineradio-local://')) {
    if (!preferHttp) return raw;
    const stripped = raw
      .replace(/^mineradio-local:\/\/(?:localhost\/)?/i, '')
      .replace(/^\/+/, '');
    const httpUrl = `${httpBase}/local-media/${stripped}`;
    return mediaToken && !/[?&]cap=/.test(httpUrl)
      ? appendQueryParam(httpUrl, 'cap', mediaToken)
      : httpUrl;
  }

  if (raw.startsWith('mineradio-wallpaper://')) {
    if (!preferHttp) return raw;
    const stripped = raw
      .replace(/^mineradio-wallpaper:\/\/(?:localhost\/)?/i, '')
      .replace(/^\/+/, '');
    const normalizedPath = stripped.startsWith('media/')
      ? `file/${stripped.slice('media/'.length)}`
      : stripped;
    const httpUrl = `${httpBase}/we-media/${normalizedPath}`;
    return mediaToken && !/[?&]cap=/.test(httpUrl)
      ? appendQueryParam(httpUrl, 'cap', mediaToken)
      : httpUrl;
  }

  if (raw.startsWith('/')) {
    return `${httpBase}${raw}`;
  }

  return raw;
}

export function audioProxyUrl(url: string | undefined | null): string {
  const raw = String(url || '').trim();
  if (!raw) return '';
  if (/^(data:|blob:)/i.test(raw)) return raw;
  if (/^(mineradio-local|mineradio-wallpaper):\/\//i.test(raw)) {
    return resolveMediaUrl(raw, { preferHttpFallback: true });
  }
  const httpBase = getCachedHttpBase();
  if (raw.startsWith(`${httpBase}/api/audio`) || raw.startsWith('/api/audio')) {
    return raw.startsWith('/') ? `${httpBase}${raw}` : raw;
  }
  if (raw.startsWith('/local-media/') || raw.startsWith('/we-media/')) {
    return resolveMediaUrl(raw, { preferHttpFallback: true });
  }
  return `${httpBase}/api/audio?url=${encodeURIComponent(raw)}`;
}

export function coverProxyUrl(url: string | undefined | null, cacheBust = false): string {
  const raw = String(url || '').trim();
  if (!raw) return '';
  if (/^(data:|blob:)/i.test(raw)) return raw;
  if (/^(mineradio-local|mineradio-wallpaper):\/\//i.test(raw)) {
    return resolveMediaUrl(raw, { preferHttpFallback: true });
  }
  const httpBase = getCachedHttpBase();
  if (raw.startsWith(`${httpBase}/api/cover`) || raw.startsWith('/api/cover')) {
    const full = raw.startsWith('/') ? `${httpBase}${raw}` : raw;
    return cacheBust ? appendQueryParam(full, 'v', String(Date.now())) : full;
  }
  if (raw.startsWith('/local-media/') || raw.startsWith('/we-media/') || raw.startsWith('/assets/')) {
    return resolveMediaUrl(raw, { preferHttpFallback: true });
  }
  return `${httpBase}/api/cover?url=${encodeURIComponent(raw)}${cacheBust ? `&v=${Date.now()}` : ''}`;
}

export interface ApiFetchInit extends RequestInit {
  timeoutMs?: number;
}

export async function apiFetch(path: string, init?: ApiFetchInit): Promise<Response> {
  const endpoints = await getRuntimeEndpoints();
  const rawPath = String(path || '').trim();
  const targetUrl = /^https?:\/\//i.test(rawPath)
    ? rawPath
    : `${endpoints.httpBase}${rawPath.startsWith('/') ? rawPath : `/${rawPath}`}`;

  const { timeoutMs, signal: externalSignal, headers: rawHeaders, ...rest } = init || {};
  const headers = new Headers(rawHeaders);
  if (!headers.has('Accept')) {
    headers.set('Accept', 'application/json');
  }

  let controller: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let signal = externalSignal;

  if (timeoutMs && timeoutMs > 0 && typeof AbortController !== 'undefined') {
    controller = new AbortController();
    if (externalSignal) {
      if (externalSignal.aborted) controller.abort();
      else externalSignal.addEventListener('abort', () => controller?.abort(), { once: true });
    }
    signal = controller.signal;
    timer = setTimeout(() => controller?.abort(), timeoutMs);
  }

  try {
    return await fetch(targetUrl, {
      ...rest,
      headers,
      signal,
    });
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export async function apiJson<T = Record<string, unknown>>(
  path: string,
  init?: ApiFetchInit
): Promise<T> {
  const resp = await apiFetch(path, init);
  const text = await resp.text();
  let parsed: unknown = {};
  if (text) {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = { ok: resp.ok, raw: text };
    }
  }
  if (!resp.ok) {
    const errObj = (parsed && typeof parsed === 'object' ? parsed : {}) as Record<string, unknown>;
    const errorMsg = String(errObj.error || errObj.message || `HTTP_${resp.status}`);
    const error = new Error(errorMsg) as Error & { status?: number; payload?: unknown };
    error.status = resp.status;
    error.payload = parsed;
    throw error;
  }
  return parsed as T;
}

/**
 * 统一事件订阅：优先使用 Tauri 2 事件系统，浏览器环境自动回退到局部 EventBus
 */
export async function listenEvent<T = unknown>(
  event: string,
  handler: (payload: T) => void
): Promise<UnlistenFn> {
  if (typeof handler !== 'function') return () => {};

  if (isTauriRuntime()) {
    if (window.__TAURI__?.event?.listen) {
      try {
        return await window.__TAURI__.event.listen<T>(event, (ev) => {
          handler((ev?.payload ?? {}) as T);
        });
      } catch (err) {
        console.warn(`[tauriBridge] __TAURI__.event.listen(${event}) failed:`, err);
      }
    }

    const internals = window.__TAURI_INTERNALS__;
    if (internals?.transformCallback && typeof internals.invoke === 'function') {
      try {
        const callbackId = internals.transformCallback((rawEvent: unknown) => {
          const evObj = (rawEvent && typeof rawEvent === 'object' ? rawEvent : {}) as {
            payload?: T;
          };
          handler((evObj.payload ?? rawEvent ?? {}) as T);
        });
        const eventId = await internals.invoke<number>('plugin:event|listen', {
          event,
          target: { kind: 'Any' },
          handler: callbackId,
        });
        return () => {
          internals.invoke('plugin:event|unlisten', { event, eventId }).catch(() => {});
          internals.unregisterCallback?.(callbackId);
        };
      } catch (err) {
        console.warn(`[tauriBridge] plugin:event|listen(${event}) failed:`, err);
      }
    }
  }

  const listener = (ev: Event) => {
    const custom = ev as CustomEvent<T>;
    handler((custom.detail ?? {}) as T);
  };
  localEventBus.addEventListener(event, listener);
  return () => localEventBus.removeEventListener(event, listener);
}

export function emitLocalEvent<T = unknown>(event: string, payload: T): void {
  localEventBus.dispatchEvent(new CustomEvent<T>(event, { detail: payload }));
}

function getSafeStorage(): Storage | null {
  try {
    const s =
      (typeof window !== 'undefined' && window.localStorage) ||
      (typeof globalThis !== 'undefined' && (globalThis as { localStorage?: Storage }).localStorage);
    if (s && typeof s.getItem === 'function' && typeof s.setItem === 'function') {
      return s;
    }
  } catch {
    // ignore
  }
  return null;
}

function readLsJson<T>(key: string, fallback: T): T {
  try {
    const storage = getSafeStorage();
    if (!storage) return fallback;
    const raw = storage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function writeLsJson(key: string, value: unknown): boolean {
  try {
    const storage = getSafeStorage();
    if (!storage) return false;
    storage.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

export interface LocalFileAuthorizeEntry {
  path: string;
  relativePath?: string;
}

export interface LocalImportResult {
  ok: boolean;
  count: number;
  tracks: SongRecord[];
  error?: string;
}

export interface LocalLyricReadResult {
  ok: boolean;
  lyric?: string;
  tlyric?: string;
  romalrc?: string;
  source?: string;
  error?: string;
}

export interface BuiltInPlaylistPageResult {
  ok: boolean;
  playlist?: PlaylistSummary;
  tracks?: SongRecord[];
  total?: number;
  offset?: number;
  limit?: number;
  error?: string;
}

export interface DesktopWindowBridge {
  readonly isDesktop: boolean;
  // 窗口控制
  minimize: () => Promise<unknown>;
  restore: () => Promise<unknown>;
  toggleMaximize: () => Promise<unknown>;
  toggleFullscreen: () => Promise<unknown>;
  exitFullscreenWindowed: () => Promise<unknown>;
  close: (behavior?: string) => Promise<unknown>;
  getState: () => Promise<DesktopWindowState>;
  getCloseBehavior: () => Promise<{ behavior: string }>;
  setCloseBehavior: (behavior: string) => Promise<{ ok: boolean; behavior: string }>;
  onStateChange: (callback: (state: DesktopWindowState) => void) => UnlistenFn;

  // 彩蛋门控
  getLoginEasterEggStatus: () => Promise<EasterEggStatus>;
  unlockLoginEasterEgg: (password: string) => Promise<EasterEggStatus>;
  resetLoginEasterEgg: () => Promise<EasterEggStatus>;

  // 内置歌单 CRUD
  listBuiltInPlaylists: () => Promise<{ ok: boolean; playlists: PlaylistSummary[] }>;
  readBuiltInPlaylist: (
    id: string,
    options?: { offset?: number; limit?: number }
  ) => Promise<BuiltInPlaylistPageResult>;
  createBuiltInPlaylist: (
    name: string
  ) => Promise<{ ok: boolean; playlist?: PlaylistSummary; error?: string }>;
  renameBuiltInPlaylist: (
    id: string,
    name: string
  ) => Promise<{ ok: boolean; playlist?: PlaylistSummary; error?: string }>;
  deleteBuiltInPlaylist: (id: string) => Promise<{ ok: boolean; error?: string }>;
  addBuiltInPlaylistTrack: (
    id: string,
    track: SongRecord
  ) => Promise<{ ok: boolean; duplicate?: boolean; error?: string }>;
  removeBuiltInPlaylistTrack: (
    id: string,
    index: number
  ) => Promise<{ ok: boolean; error?: string }>;
  reorderBuiltInPlaylistTrack: (
    id: string,
    fromIndex: number,
    toIndex: number
  ) => Promise<{ ok: boolean; error?: string }>;

  // 本地曲库
  chooseLocalMusicFiles: () => Promise<{ ok: boolean; files?: string[]; canceled?: boolean }>;
  listLocalMusicLibrary: () => Promise<{ ok: boolean; tracks: SongRecord[] }>;
  readLocalMusicLyric: (localFileId: string) => Promise<LocalLyricReadResult>;
  authorizeAndImport: (entries: LocalFileAuthorizeEntry[]) => Promise<LocalImportResult>;
  importLocalMusicFiles: (files: FileList | File[]) => Promise<LocalImportResult>;

  // 缓存 / FX 自动保存 / 歌词缓存
  getCacheSettings: () => Promise<CacheSettings>;
  setCacheSettings: (payload: Partial<CacheSettings>) => Promise<CacheSettings>;
  chooseCacheDirectory: () => Promise<{ ok: boolean; path?: string; canceled?: boolean }>;
  readCurrentFxAutosave: () => Promise<{ ok: boolean; snapshot?: Partial<FxSettings> | null }>;
  saveCurrentFxAutosave: (payload: Partial<FxSettings>) => Promise<{ ok: boolean }>;
  readLyricCache: (key: string) => Promise<{ ok: boolean; payload?: unknown }>;
  writeLyricCache: (key: string, payload: unknown) => Promise<{ ok: boolean }>;

  // 内存快照 / 清理
  getMemorySnapshot: () => Promise<MemorySnapshot>;
  trimAppMemory: (payload?: { reason?: string }) => Promise<{ ok: boolean; supported?: boolean }>;
  purgeSystemMemory: (payload?: Record<string, unknown>) => Promise<{ ok: boolean; supported?: boolean }>;
  configureMemoryReduct: (payload?: Record<string, unknown>) => Promise<{ ok: boolean; supported?: boolean }>;

  // 桌面歌词
  desktopLyricsGetState: () => Promise<{
    ok: boolean;
    enabled?: boolean;
    locked?: boolean;
    payload?: DesktopLyricsPayload | null;
  }>;
  setDesktopLyricsEnabled: (
    enabled: boolean,
    payload?: DesktopLyricsPayload
  ) => Promise<{ ok: boolean; enabled: boolean }>;
  updateDesktopLyrics: (payload: DesktopLyricsPayload) => Promise<{ ok: boolean }>;
  setDesktopLyricsLockState: (locked: boolean) => Promise<{ ok: boolean; locked: boolean }>;
  moveDesktopLyricsBy: (dx: number, dy: number) => Promise<{ ok: boolean }>;
  onDesktopLyricsLockState: (callback: (payload: { locked: boolean }) => void) => UnlistenFn;
  onDesktopLyricsEnabledState: (callback: (payload: { enabled: boolean }) => void) => UnlistenFn;

  // 平台授权登录弹窗控制
  openNeteaseMusicLogin: (
    forceReauth?: boolean
  ) => Promise<{ ok: boolean; url?: string; error?: string }>;
  openQQMusicLogin: (
    forceReauth?: boolean
  ) => Promise<{ ok: boolean; url?: string; error?: string }>;
  openKugouMusicLogin: (
    forceReauth?: boolean
  ) => Promise<{ ok: boolean; url?: string; error?: string }>;
  openQishuiAuthLogin: (
    forceReauth?: boolean
  ) => Promise<{ ok: boolean; url?: string; error?: string }>;
  clearPlatformLoginSession: (provider: string) => Promise<{ ok: boolean }>;
  closePlatformLoginWindow: () => Promise<{ ok: boolean }>;
  onPlatformLoginSuccess: (
    callback: (payload: { provider: string; loggedIn: boolean; status?: unknown }) => void
  ) => UnlistenFn;

  // 外部打开 / 剪贴板 / 导入导出 JSON
  openUpdatePage: (url: string) => Promise<{ ok: boolean; error?: string }>;
  copyText: (text: string) => Promise<{ ok: boolean }>;
  readText: () => Promise<{ ok: boolean; text: string }>;
  exportJsonFile: (payload: {
    filename?: string;
    data: unknown;
    title?: string;
  }) => Promise<{ ok: boolean; path?: string; canceled?: boolean }>;
  importJsonFile: () => Promise<{ ok: boolean; data?: unknown; canceled?: boolean; error?: string }>;

  // Wallpaper Engine & 桌面模式控制
  listWallpaperEngineProjects: (
    payload?: Record<string, unknown>
  ) => Promise<{ ok: boolean; projects: WallpaperEngineProject[]; roots?: unknown[] }>;
  getWallpaperEngineProjectDetails: (
    id: string
  ) => Promise<{ ok: boolean; project?: WallpaperEngineProject | null }>;
  openWallpaperEngineProjectDetails: (
    id: string,
    target?: 'we' | 'workshop'
  ) => Promise<{ ok: boolean }>;
  chooseWallpaperEngineDirectory: () => Promise<{ ok: boolean; path?: string; canceled?: boolean }>;
  chooseWallpaperEngineProjectFile: () => Promise<{ ok: boolean; path?: string; canceled?: boolean }>;
  removeWallpaperEngineDirectory: (rootId: string) => Promise<{ ok: boolean }>;
  getWallpaperEngineRuntimeStatus: (
    payload?: Record<string, unknown>
  ) => Promise<Record<string, unknown>>;
  startWallpaperEngineScene: (payload?: Record<string, unknown>) => Promise<Record<string, unknown>>;
  stopWallpaperEngineScene: (payload?: Record<string, unknown>) => Promise<Record<string, unknown>>;
  setWallpaperMode: (
    enabled: boolean,
    payload?: Record<string, unknown>
  ) => Promise<{ ok: boolean; enabled: boolean; supported?: boolean }>;
  updateWallpaperMode: (payload?: Record<string, unknown>) => Promise<{ ok: boolean }>;
  getWallpaperModeStatus: () => Promise<{ ok: boolean; enabled: boolean; supported?: boolean }>;
  setDesktopSoftwareLocked: (locked: boolean) => Promise<{ ok: boolean; locked: boolean }>;
  setDesktopIconsVisible: (visible: boolean) => Promise<{ ok: boolean; visible: boolean }>;
  requestDesktopKeyboardFocus: (reason?: string) => Promise<{ ok: boolean }>;
  updateDesktopPointerRoute: (payload: {
    overSoftwareUi?: boolean;
    overDesktopControls?: boolean;
  }) => Promise<void>;
  updateDesktopIconShields: (payload: Record<string, unknown>) => Promise<void>;
  onWallpaperModeState: (callback: (payload: Record<string, unknown>) => void) => UnlistenFn;
  onWallpaperEngineHostBoundsChanged: (
    callback: (payload: Record<string, unknown>) => void
  ) => UnlistenFn;

  // 全局热键 / GPU / 相机权限 / 重启
  configureGlobalHotkeys: (bindings: unknown[]) => Promise<{ ok: boolean }>;
  onGlobalHotkey: (callback: (payload: Record<string, unknown>) => void) => UnlistenFn;
  getGpuDiagnostics: () => Promise<Record<string, unknown>>;
  requestGestureCameraPermission: () => Promise<{ ok: boolean; granted?: boolean }>;
  restartApp: () => Promise<{ ok: boolean }>;
}

function subscribeWrapper<T>(event: string, callback: (payload: T) => void): UnlistenFn {
  if (typeof callback !== 'function') return () => {};
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;
  listenEvent<T>(event, callback)
    .then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    })
    .catch(() => {});
  return () => {
    cancelled = true;
    if (unlisten) unlisten();
  };
}

function getLocalBuiltInPlaylists(): PlaylistSummary[] {
  return readLsJson<PlaylistSummary[]>(BUILTIN_PLAYLISTS_LS_KEY, []);
}

function saveLocalBuiltInPlaylists(playlists: PlaylistSummary[]): void {
  writeLsJson(BUILTIN_PLAYLISTS_LS_KEY, playlists);
}

export const desktopWindow: DesktopWindowBridge = {
  get isDesktop(): boolean {
    return isTauriRuntime();
  },

  // ==================== 窗口控制 ====================
  minimize: () => safeInvoke('desktop_window_minimize', undefined, () => ({ ok: false })),
  restore: () => safeInvoke('desktop_window_restore', undefined, () => ({ ok: false })),
  toggleMaximize: () =>
    safeInvoke('desktop_window_toggle_maximize', undefined, () => ({ ok: false })),
  toggleFullscreen: () =>
    safeInvoke('desktop_window_toggle_fullscreen', undefined, async () => {
      if (typeof document !== 'undefined' && document.documentElement.requestFullscreen) {
        if (document.fullscreenElement) await document.exitFullscreen();
        else await document.documentElement.requestFullscreen();
        return { ok: true, isFullScreen: Boolean(document.fullscreenElement) };
      }
      return { ok: false };
    }),
  exitFullscreenWindowed: () =>
    safeInvoke('desktop_window_exit_fullscreen_windowed', undefined, async () => {
      if (typeof document !== 'undefined' && document.fullscreenElement && document.exitFullscreen) {
        await document.exitFullscreen();
      }
      return { ok: true };
    }),
  close: (behavior?: string) =>
    safeInvoke('desktop_window_close', behavior ? { behavior } : undefined, () => ({ ok: false })),
  getState: () =>
    safeInvoke<DesktopWindowState>('desktop_window_get_state', undefined, () => ({
      isDesktop: false,
      isMaximized: false,
      isFullScreen: typeof document !== 'undefined' ? Boolean(document.fullscreenElement) : false,
      isMinimized: false,
      isDesktopEmbedded: false,
      isDesktopInteractive: false,
      isPrimaryDisplay: true,
      hasDisplayOnLeft: false,
      closeBehavior: readLsJson<string>(CLOSE_BEHAVIOR_LS_KEY, 'tray'),
    })),
  getCloseBehavior: () =>
    safeInvoke('desktop_window_get_close_behavior', undefined, () => ({
      behavior: readLsJson<string>(CLOSE_BEHAVIOR_LS_KEY, 'tray'),
    })),
  setCloseBehavior: (behavior: string) =>
    safeInvoke('desktop_window_set_close_behavior', { behavior }, () => {
      writeLsJson(CLOSE_BEHAVIOR_LS_KEY, behavior);
      return { ok: true, behavior };
    }),
  onStateChange: (callback) => subscribeWrapper<DesktopWindowState>('desktop-window-state', callback),

  // ==================== 彩蛋门控 ====================
  getLoginEasterEggStatus: () =>
    safeInvoke<EasterEggStatus>('get_login_easter_egg_status', undefined, () => {
      const unlocked = readLsJson<boolean>(EASTER_EGG_LS_KEY, false);
      return {
        ok: true,
        gateVersion: 'world-peace-v1',
        unlocked,
        resetComplete: true,
      };
    }),
  unlockLoginEasterEgg: (password: string) =>
    safeInvoke<EasterEggStatus>(
      'unlock_login_easter_egg',
      { password: String(password || '') },
      () => {
        const matched = String(password || '').trim() === '世界和平';
        if (matched) writeLsJson(EASTER_EGG_LS_KEY, true);
        return {
          ok: matched,
          gateVersion: 'world-peace-v1',
          unlocked: matched || readLsJson<boolean>(EASTER_EGG_LS_KEY, false),
          resetComplete: true,
          error: matched ? undefined : 'INVALID_EASTER_EGG_PASSWORD',
          message: matched ? '已解锁全平台登录入口' : '暗号错误',
        };
      }
    ),
  resetLoginEasterEgg: () =>
    safeInvoke<EasterEggStatus>('reset_login_easter_egg', undefined, () => {
      writeLsJson(EASTER_EGG_LS_KEY, false);
      return {
        ok: true,
        gateVersion: 'world-peace-v1',
        unlocked: false,
        resetComplete: true,
      };
    }),

  // ==================== 内置歌单 CRUD ====================
  listBuiltInPlaylists: () =>
    safeInvoke('list_built_in_playlists', undefined, () => ({
      ok: true,
      playlists: getLocalBuiltInPlaylists(),
    })),
  readBuiltInPlaylist: (id: string, options?: { offset?: number; limit?: number }) =>
    safeInvoke(
      'read_built_in_playlist',
      { id: String(id || ''), offset: options?.offset ?? 0, limit: options?.limit ?? 200 },
      () => {
        const found = getLocalBuiltInPlaylists().find((p) => String(p.id) === String(id));
        if (!found) return { ok: false, error: 'BUILT_IN_PLAYLIST_NOT_FOUND' };
        const tracks = found.tracks || [];
        const offset = Math.max(0, options?.offset ?? 0);
        const limit = Math.max(1, options?.limit ?? 200);
        return {
          ok: true,
          playlist: found,
          tracks: tracks.slice(offset, offset + limit),
          total: tracks.length,
          offset,
          limit,
        };
      }
    ),
  createBuiltInPlaylist: (name: string) =>
    safeInvoke('create_built_in_playlist', { name: String(name || '') }, () => {
      const list = getLocalBuiltInPlaylists();
      const playlist: PlaylistSummary = {
        id: `mr-${Date.now().toString(36)}`,
        name: String(name || '新建歌单').trim() || '新建歌单',
        trackCount: 0,
        provider: 'mineradio',
        shelfPane: 'mine',
        createdAt: Date.now(),
        updatedAt: Date.now(),
        tracks: [],
      };
      list.unshift(playlist);
      saveLocalBuiltInPlaylists(list);
      return { ok: true, playlist };
    }),
  renameBuiltInPlaylist: (id: string, name: string) =>
    safeInvoke('rename_built_in_playlist', { id: String(id || ''), name: String(name || '') }, () => {
      const list = getLocalBuiltInPlaylists();
      const target = list.find((p) => String(p.id) === String(id));
      if (!target) return { ok: false, error: 'BUILT_IN_PLAYLIST_NOT_FOUND' };
      target.name = String(name || target.name).trim() || target.name;
      target.updatedAt = Date.now();
      saveLocalBuiltInPlaylists(list);
      return { ok: true, playlist: target };
    }),
  deleteBuiltInPlaylist: (id: string) =>
    safeInvoke('delete_built_in_playlist', { id: String(id || '') }, () => {
      const list = getLocalBuiltInPlaylists();
      const next = list.filter((p) => String(p.id) !== String(id));
      if (next.length === list.length) return { ok: false, error: 'BUILT_IN_PLAYLIST_NOT_FOUND' };
      saveLocalBuiltInPlaylists(next);
      return { ok: true };
    }),
  addBuiltInPlaylistTrack: (id: string, track: SongRecord) =>
    safeInvoke('add_built_in_playlist_track', { id: String(id || ''), track }, () => {
      const list = getLocalBuiltInPlaylists();
      const target = list.find((p) => String(p.id) === String(id));
      if (!target) return { ok: false, error: 'BUILT_IN_PLAYLIST_NOT_FOUND' };
      target.tracks = target.tracks || [];
      target.tracks.push(track);
      target.trackCount = target.tracks.length;
      target.updatedAt = Date.now();
      saveLocalBuiltInPlaylists(list);
      return { ok: true, duplicate: false };
    }),
  removeBuiltInPlaylistTrack: (id: string, index: number) =>
    safeInvoke('remove_built_in_playlist_track', { id: String(id || ''), index }, () => {
      const list = getLocalBuiltInPlaylists();
      const target = list.find((p) => String(p.id) === String(id));
      if (!target || !target.tracks || index < 0 || index >= target.tracks.length) {
        return { ok: false, error: 'BUILT_IN_PLAYLIST_TRACK_NOT_FOUND' };
      }
      target.tracks.splice(index, 1);
      target.trackCount = target.tracks.length;
      target.updatedAt = Date.now();
      saveLocalBuiltInPlaylists(list);
      return { ok: true };
    }),
  reorderBuiltInPlaylistTrack: (id: string, fromIndex: number, toIndex: number) =>
    safeInvoke(
      'reorder_built_in_playlist_track',
      { id: String(id || ''), fromIndex, toIndex },
      () => {
        const list = getLocalBuiltInPlaylists();
        const target = list.find((p) => String(p.id) === String(id));
        if (!target || !target.tracks) return { ok: false, error: 'BUILT_IN_PLAYLIST_NOT_FOUND' };
        const [moved] = target.tracks.splice(fromIndex, 1);
        if (!moved) return { ok: false, error: 'BUILT_IN_PLAYLIST_TRACK_NOT_FOUND' };
        target.tracks.splice(toIndex, 0, moved);
        target.updatedAt = Date.now();
        saveLocalBuiltInPlaylists(list);
        return { ok: true };
      }
    ),

  // ==================== 本地曲库 ====================
  chooseLocalMusicFiles: () =>
    safeInvoke('choose_local_music_files', undefined, () => ({
      ok: false,
      canceled: true,
      files: [],
    })),
  listLocalMusicLibrary: () =>
    safeInvoke('list_local_music_library', undefined, () => ({
      ok: true,
      tracks: [],
    })),
  readLocalMusicLyric: (localFileId: string) =>
    safeInvoke('read_local_music_lyric', { localFileId: String(localFileId || '') }, () => ({
      ok: false,
      error: 'LOCAL_LYRIC_UNAVAILABLE_IN_BROWSER',
    })),
  authorizeAndImport: async (entries: LocalFileAuthorizeEntry[]) => {
    if (!entries.length) {
      return { ok: false, count: 0, tracks: [], error: 'NO_AUTHORIZED_LOCAL_AUDIO' };
    }
    if (!isTauriRuntime()) {
      return { ok: false, count: 0, tracks: [], error: 'TAURI_REQUIRED_FOR_PATH_IMPORT' };
    }
    const auth = await invokeCommand<{ ok: boolean; token?: string; error?: string }>(
      'authorize_local_music',
      { files: entries }
    );
    if (!auth?.ok || !auth.token) {
      return { ok: false, count: 0, tracks: [], error: auth?.error || 'AUTHORIZE_FAILED' };
    }
    return invokeCommand<LocalImportResult>('import_local_music', { token: auth.token });
  },
  importLocalMusicFiles: async (files: FileList | File[]) => {
    const fileArray = Array.from(files || []);
    if (!fileArray.length) {
      return { ok: false, count: 0, tracks: [], error: 'NO_AUTHORIZED_LOCAL_AUDIO' };
    }
    if (isTauriRuntime()) {
      const entries: LocalFileAuthorizeEntry[] = [];
      for (const file of fileArray) {
        const fileWithPath = file as File & { path?: string; webkitRelativePath?: string };
        const filePath = fileWithPath.path || '';
        if (!filePath) continue;
        entries.push({
          path: filePath,
          relativePath: String(fileWithPath.webkitRelativePath || file.name || ''),
        });
      }
      if (entries.length) {
        return desktopWindow.authorizeAndImport(entries);
      }
    }
    // 纯浏览器兜底：使用 ObjectURL 生成可播本地记录
    const tracks: SongRecord[] = fileArray.map((file, idx) => {
      const blobUrl =
        typeof URL !== 'undefined' && typeof URL.createObjectURL === 'function'
          ? URL.createObjectURL(file)
          : '';
      const baseName = file.name.replace(/\.[^.]+$/, '');
      const localFileId = `browser-${Date.now().toString(36)}-${idx}`;
      return {
        id: localFileId,
        localFileId,
        localUrl: blobUrl,
        name: baseName || file.name,
        title: baseName || file.name,
        artist: '本地音频',
        album: '本地文件',
        provider: 'local',
        source: 'local',
      };
    });
    return { ok: true, count: tracks.length, tracks };
  },

  // ==================== 缓存 / FX 自动保存 / 歌词缓存 ====================
  getCacheSettings: async () => {
    const raw = await safeInvoke<CacheSettings>('get_cache_settings', undefined, () => ({
      beatmapDiskEnabled: false,
      lyricCacheEnabled: true,
    }));
    const rootPath = raw.settings?.rootPath || raw.cacheRoot || '';
    return {
      ...raw,
      cacheRoot: raw.cacheRoot || rootPath,
      effectiveRoot: raw.effectiveRoot || rootPath,
    };
  },
  setCacheSettings: (payload: Partial<CacheSettings>) =>
    safeInvoke<CacheSettings>('set_cache_settings', { payload }, () => payload),
  chooseCacheDirectory: () =>
    safeInvoke('choose_cache_directory', undefined, () => ({ ok: false, canceled: true })),
  readCurrentFxAutosave: () =>
    safeInvoke('read_current_fx_autosave', undefined, () => ({
      ok: true,
      snapshot: readLsJson<Partial<FxSettings> | null>(FX_AUTOSAVE_LS_KEY, null),
    })),
  saveCurrentFxAutosave: (payload: Partial<FxSettings>) =>
    safeInvoke('save_current_fx_autosave', { payload }, () => ({
      ok: writeLsJson(FX_AUTOSAVE_LS_KEY, payload),
    })),
  readLyricCache: (key: string) =>
    safeInvoke('read_lyric_cache', { key: String(key || '') }, () => {
      const data = readLsJson<unknown>(`${LYRIC_CACHE_LS_PREFIX}${key}`, null);
      return { ok: Boolean(data), payload: data };
    }),
  writeLyricCache: (key: string, payload: unknown) =>
    safeInvoke('write_lyric_cache', { key: String(key || ''), payload }, () => ({
      ok: writeLsJson(`${LYRIC_CACHE_LS_PREFIX}${key}`, payload),
    })),

  // ==================== 内存快照与清理 ====================
  getMemorySnapshot: async () => {
    const raw = await safeInvoke<MemorySnapshot>('get_memory_snapshot', undefined, () => ({
      supported: false,
      platform: typeof navigator !== 'undefined' ? navigator.platform : 'browser',
    }));
    return {
      ...raw,
      workingSetBytes:
        raw.workingSetBytes ??
        raw.process?.rssBytes ??
        (raw.process?.rssMB != null ? Math.round(raw.process.rssMB * 1024 * 1024) : undefined),
      privateBytes:
        raw.privateBytes ??
        (raw.process?.privateMB != null
          ? Math.round(raw.process.privateMB * 1024 * 1024)
          : undefined),
      systemTotalBytes: raw.systemTotalBytes ?? raw.totalBytes,
      systemUsedPercent: raw.systemUsedPercent ?? raw.usedPercent,
    };
  },
  trimAppMemory: (payload) =>
    safeInvoke('trim_app_memory', { payload: payload || {} }, () => ({
      ok: true,
      supported: false,
    })),
  purgeSystemMemory: (payload) =>
    safeInvoke('purge_system_memory', { payload: payload || {} }, () => ({
      ok: false,
      supported: false,
    })),
  configureMemoryReduct: (payload) =>
    safeInvoke('configure_memory_reduct', { payload: payload || {} }, () => ({
      ok: false,
      supported: false,
    })),

  // ==================== 桌面歌词 ====================
  desktopLyricsGetState: () =>
    safeInvokeGraceful<{
      ok: boolean;
      enabled?: boolean;
      locked?: boolean;
      payload?: DesktopLyricsPayload | null;
    }>('desktop_lyrics_get_state', undefined, () => ({
      ok: false,
      enabled: false,
      locked: false,
      payload: null,
    })),
  setDesktopLyricsEnabled: (enabled: boolean, payload?: DesktopLyricsPayload) =>
    safeInvoke('set_desktop_lyrics_enabled', { enabled: Boolean(enabled), payload: payload || {} }, () => {
      emitLocalEvent('mineradio-desktop-lyrics-enabled-state', { enabled: Boolean(enabled) });
      return { ok: true, enabled: Boolean(enabled) };
    }),
  updateDesktopLyrics: (payload: DesktopLyricsPayload) =>
    safeInvoke('update_desktop_lyrics', { payload: payload || {} }, () => {
      emitLocalEvent('mineradio-desktop-lyrics-update', payload);
      return { ok: true };
    }),
  setDesktopLyricsLockState: (locked: boolean) =>
    safeInvoke('set_desktop_lyrics_lock_state', { locked: Boolean(locked) }, () => {
      emitLocalEvent('mineradio-desktop-lyrics-lock-state', { locked: Boolean(locked) });
      return { ok: true, locked: Boolean(locked) };
    }),
  moveDesktopLyricsBy: (dx: number, dy: number) =>
    safeInvoke('move_desktop_lyrics_by', { dx: Number(dx) || 0, dy: Number(dy) || 0 }, () => ({
      ok: false,
    })),
  onDesktopLyricsLockState: (callback) =>
    subscribeWrapper<{ locked: boolean }>('mineradio-desktop-lyrics-lock-state', callback),
  onDesktopLyricsEnabledState: (callback) =>
    subscribeWrapper<{ enabled: boolean }>('mineradio-desktop-lyrics-enabled-state', callback),

  // ==================== 平台授权登录弹窗控制 ====================
  openNeteaseMusicLogin: (forceReauth = false) =>
    safeInvokeGraceful<{ ok: boolean; url?: string; error?: string }>(
      'open_netease_music_login',
      { forceReauth: Boolean(forceReauth) },
      (err) => ({
        ok: false,
        error: err instanceof Error ? err.message : 'TAURI_REQUIRED',
      })
    ),
  openQQMusicLogin: (forceReauth = false) =>
    safeInvokeGraceful<{ ok: boolean; url?: string; error?: string }>(
      'open_qq_music_login',
      { forceReauth: Boolean(forceReauth) },
      (err) => ({
        ok: false,
        error: err instanceof Error ? err.message : 'TAURI_REQUIRED',
      })
    ),
  openKugouMusicLogin: (forceReauth = false) =>
    safeInvokeGraceful<{ ok: boolean; url?: string; error?: string }>(
      'open_kugou_music_login',
      { forceReauth: Boolean(forceReauth) },
      (err) => ({
        ok: false,
        error: err instanceof Error ? err.message : 'TAURI_REQUIRED',
      })
    ),
  openQishuiAuthLogin: (forceReauth = false) =>
    safeInvokeGraceful<{ ok: boolean; url?: string; error?: string }>(
      'open_qishui_auth_login',
      { forceReauth: Boolean(forceReauth) },
      (err) => ({
        ok: false,
        error: err instanceof Error ? err.message : 'TAURI_REQUIRED',
      })
    ),
  clearPlatformLoginSession: (provider: string) =>
    safeInvokeGraceful<{ ok: boolean }>(
      'clear_platform_login_session',
      { provider: String(provider || '') },
      () => ({ ok: false })
    ),
  closePlatformLoginWindow: () =>
    safeInvokeGraceful<{ ok: boolean }>('close_platform_login_window', undefined, () => ({
      ok: true,
    })),
  onPlatformLoginSuccess: (callback) =>
    subscribeWrapper<{ provider: string; loggedIn: boolean; status?: unknown }>(
      'mineradio-platform-login-success',
      callback
    ),

  // ==================== 外部链接 / 剪贴板 / JSON 导入导出 ====================
  openUpdatePage: (url: string) =>
    safeInvoke('open_update_page', { url: String(url || '') }, () => {
      if (typeof window !== 'undefined' && /^https?:\/\//i.test(url)) {
        window.open(url, '_blank', 'noopener,noreferrer');
        return { ok: true };
      }
      return { ok: false, error: 'INVALID_UPDATE_URL' };
    }),
  copyText: (text: string) =>
    safeInvoke('copy_text', { text: String(text || '') }, async () => {
      if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(String(text || ''));
        return { ok: true };
      }
      return { ok: false };
    }),
  readText: () =>
    safeInvoke('read_text', undefined, async () => {
      if (typeof navigator !== 'undefined' && navigator.clipboard?.readText) {
        const text = await navigator.clipboard.readText();
        return { ok: true, text };
      }
      return { ok: false, text: '' };
    }),
  exportJsonFile: (payload) =>
    safeInvoke('export_json_file', { payload }, () => {
      if (typeof document === 'undefined' || typeof Blob === 'undefined') {
        return { ok: false, canceled: true };
      }
      const filename = payload.filename || 'mineradio-export.json';
      const blob = new Blob([JSON.stringify(payload.data, null, 2)], {
        type: 'application/json',
      });
      const href = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = href;
      a.download = filename;
      a.click();
      setTimeout(() => URL.revokeObjectURL(href), 1000);
      return { ok: true, path: filename };
    }),
  importJsonFile: () =>
    safeInvoke('import_json_file', undefined, () => {
      if (typeof document === 'undefined') return Promise.resolve({ ok: false, canceled: true });
      return new Promise((resolve) => {
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = '.json,application/json';
        input.onchange = async () => {
          const file = input.files?.[0];
          if (!file) {
            resolve({ ok: false, canceled: true });
            return;
          }
          try {
            const text = await file.text();
            resolve({ ok: true, data: JSON.parse(text) });
          } catch (err) {
            resolve({
              ok: false,
              error: err instanceof Error ? err.message : 'INVALID_JSON',
            });
          }
        };
        input.click();
      });
    }),

  // ==================== Wallpaper Engine & 桌面模式 ====================
  listWallpaperEngineProjects: (payload) =>
    safeInvoke('list_wallpaper_engine_projects', { payload: payload || {} }, () => ({
      ok: true,
      projects: [],
    })),
  getWallpaperEngineProjectDetails: (id: string) =>
    safeInvoke('get_wallpaper_engine_project_details', { id: String(id || '') }, () => ({
      ok: false,
      project: null,
    })),
  openWallpaperEngineProjectDetails: (id: string, target: 'we' | 'workshop' = 'we') =>
    safeInvoke('open_wallpaper_engine_project_details', { id: String(id || ''), target }, () => ({
      ok: false,
    })),
  chooseWallpaperEngineDirectory: () =>
    safeInvoke('choose_wallpaper_engine_directory', undefined, () => ({
      ok: false,
      canceled: true,
    })),
  chooseWallpaperEngineProjectFile: () =>
    safeInvoke('choose_wallpaper_engine_project_file', undefined, () => ({
      ok: false,
      canceled: true,
    })),
  removeWallpaperEngineDirectory: (rootId: string) =>
    safeInvoke('remove_wallpaper_engine_directory', { rootId: String(rootId || '') }, () => ({
      ok: false,
    })),
  getWallpaperEngineRuntimeStatus: (payload) =>
    safeInvoke('get_wallpaper_engine_runtime_status', { payload: payload || {} }, () => ({
      ok: true,
      supported: false,
      active: false,
    })),
  startWallpaperEngineScene: (payload) =>
    safeInvoke('start_wallpaper_engine_scene', { payload: payload || {} }, () => ({
      ok: false,
      supported: false,
    })),
  stopWallpaperEngineScene: (payload) =>
    safeInvoke('stop_wallpaper_engine_scene', { payload: payload || {} }, () => ({
      ok: true,
      supported: false,
    })),
  setWallpaperMode: (enabled: boolean, payload) =>
    safeInvoke(
      'set_wallpaper_mode',
      { enabled: Boolean(enabled), payload: payload || {} },
      () => ({ ok: false, enabled: false, supported: false })
    ),
  updateWallpaperMode: (payload) =>
    safeInvoke('update_wallpaper_mode', { payload: payload || {} }, () => ({ ok: false })),
  getWallpaperModeStatus: () =>
    safeInvoke('get_wallpaper_mode_status', undefined, () => ({
      ok: true,
      enabled: false,
      supported: false,
    })),
  setDesktopSoftwareLocked: (locked: boolean) =>
    safeInvoke('set_desktop_software_locked', { locked: Boolean(locked) }, () => ({
      ok: false,
      locked: Boolean(locked),
    })),
  setDesktopIconsVisible: (visible: boolean) =>
    safeInvoke('set_desktop_icons_visible', { visible: Boolean(visible) }, () => ({
      ok: false,
      visible: Boolean(visible),
    })),
  requestDesktopKeyboardFocus: (reason?: string) =>
    safeInvoke(
      'request_desktop_keyboard_focus',
      { reason: String(reason || 'renderer-pointerdown').slice(0, 80) },
      () => ({ ok: true })
    ),
  updateDesktopPointerRoute: async (payload) => {
    await safeInvoke('update_desktop_pointer_route', { payload }, () => undefined);
  },
  updateDesktopIconShields: async (payload) => {
    await safeInvoke('update_desktop_icon_shields', { payload }, () => undefined);
  },
  onWallpaperModeState: (callback) =>
    subscribeWrapper<Record<string, unknown>>('mineradio-wallpaper-runtime-state', callback),
  onWallpaperEngineHostBoundsChanged: (callback) =>
    subscribeWrapper<Record<string, unknown>>(
      'mineradio-wallpaper-engine-host-bounds-changed',
      callback
    ),

  // ==================== 全局热键 / GPU / 相机权限 / 重启 ====================
  configureGlobalHotkeys: (bindings: unknown[]) =>
    safeInvoke('configure_global_hotkeys', { bindings: bindings || [] }, () => ({ ok: false })),
  onGlobalHotkey: (callback) =>
    subscribeWrapper<Record<string, unknown>>('mineradio-global-hotkey', callback),
  getGpuDiagnostics: () =>
    safeInvoke('get_gpu_diagnostics', undefined, () => ({ ok: true, mode: 'browser' })),
  requestGestureCameraPermission: () =>
    safeInvoke('request_gesture_camera_permission', undefined, async () => {
      if (typeof navigator !== 'undefined' && navigator.mediaDevices?.getUserMedia) {
        try {
          const stream = await navigator.mediaDevices.getUserMedia({ video: true });
          stream.getTracks().forEach((t) => t.stop());
          return { ok: true, granted: true };
        } catch {
          return { ok: false, granted: false };
        }
      }
      return { ok: false, granted: false };
    }),
  restartApp: () =>
    safeInvoke('restart_app', undefined, () => {
      if (typeof window !== 'undefined') window.location.reload();
      return { ok: true };
    }),
};
