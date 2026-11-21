import { create } from 'zustand';
import {
  apiJson,
  desktopWindow,
  getRuntimeEndpoints,
} from '../bridge/tauriBridge';
import { audioEngine, gestureEngine, songIdentityKey, visualEngine } from '../engines';
import {
  DEFAULT_FX_SETTINGS,
  type BackgroundMediaState,
  type DiscoverHomeData,
  type FxArchiveSlot,
  type FxSettings,
  type HotkeyBinding,
  type LoginStatusMap,
  type LyricLine,
  type PlayMode,
  type PlatformLoginEntry,
  type PlaylistSummary,
  type PlayerStoreState,
  type ProviderType,
  type QualityPreference,
  type SearchMode,
  type ShelfItem,
  type SongRecord,
  type TodayListeningStats,
  type UpdateCheckResult,
  type WeatherRadioData,
} from '../types/mineradio';

export type { PlayerStoreState } from '../types/mineradio';
export type PlayerState = PlayerStoreState;

const PLAY_MODE_ORDER: PlayMode[] = ['loop', 'single', 'shuffle'];

export function resolveNextPlayMode(current: PlayMode): PlayMode {
  const idx = PLAY_MODE_ORDER.indexOf(current);
  return PLAY_MODE_ORDER[(idx + 1) % PLAY_MODE_ORDER.length];
}

export function computeNextTrackIndex(
  queueLength: number,
  currentIdx: number,
  playMode: PlayMode,
  randomFn: () => number = Math.random
): number {
  if (queueLength <= 0) return -1;
  if (queueLength === 1) return 0;
  if (playMode === 'single') {
    return currentIdx >= 0 && currentIdx < queueLength ? currentIdx : 0;
  }
  if (playMode === 'shuffle') {
    let next = Math.floor(randomFn() * queueLength);
    if (next === currentIdx) {
      next = (currentIdx + 1) % queueLength;
    }
    return next;
  }
  if (currentIdx < 0) return 0;
  return (currentIdx + 1) % queueLength;
}

export function computePrevTrackIndex(
  queueLength: number,
  currentIdx: number,
  playMode: PlayMode,
  randomFn: () => number = Math.random
): number {
  if (queueLength <= 0) return -1;
  if (queueLength === 1) return 0;
  if (playMode === 'single') {
    return currentIdx >= 0 && currentIdx < queueLength ? currentIdx : 0;
  }
  if (playMode === 'shuffle') {
    let prev = Math.floor(randomFn() * queueLength);
    if (prev === currentIdx) {
      prev = (currentIdx - 1 + queueLength) % queueLength;
    }
    return prev;
  }
  if (currentIdx <= 0) return queueLength - 1;
  return currentIdx - 1;
}

export function parseLrcText(
  lrc?: string,
  tlyric?: string,
  romalrc?: string
): LyricLine[] {
  const parseRaw = (raw: string | undefined): Array<{ time: number; text: string }> => {
    if (!raw) return [];
    const out: Array<{ time: number; text: string }> = [];
    const tagRe = /\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]/g;
    for (const rawLine of raw.split(/\r?\n/)) {
      const times: number[] = [];
      let match: RegExpExecArray | null;
      tagRe.lastIndex = 0;
      while ((match = tagRe.exec(rawLine)) !== null) {
        const min = Number(match[1]) || 0;
        const sec = Number(match[2]) || 0;
        const fracRaw = match[3] || '0';
        const frac =
          fracRaw.length === 3
            ? Number(fracRaw) / 1000
            : fracRaw.length === 2
            ? Number(fracRaw) / 100
            : Number(fracRaw) / 10;
        times.push(min * 60 + sec + frac);
      }
      if (!times.length) continue;
      const text = rawLine.replace(tagRe, '').trim();
      if (!text) continue;
      for (const time of times) {
        out.push({ time, text });
      }
    }
    return out.sort((a, b) => a.time - b.time);
  };

  const main = parseRaw(lrc);
  if (!main.length) return [];
  const trans = parseRaw(tlyric);
  const roma = parseRaw(romalrc);

  const findNearest = (arr: Array<{ time: number; text: string }>, t: number): string | undefined => {
    for (const item of arr) {
      if (Math.abs(item.time - t) <= 0.35) return item.text;
    }
    return undefined;
  };

  return main.map((line) => ({
    time: line.time,
    text: line.text,
    translation: trans.length ? findNearest(trans, line.time) : undefined,
    roma: roma.length ? findNearest(roma, line.time) : undefined,
  }));
}

export function findActiveLyricIndex(
  lines: LyricLine[],
  currentTime: number,
  offsetSec = 0
): number {
  if (!lines.length) return -1;
  const target = currentTime + offsetSec;
  let active = -1;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].time <= target + 0.05) active = i;
    else break;
  }
  return active;
}

const SEARCH_HISTORY_LS_KEY = 'mineradio-search-history-v1';
const HOTKEY_BINDINGS_LS_KEY = 'mineradio-hotkey-bindings-v1';
const TODAY_STATS_LS_KEY = 'mineradio-today-listening-stats-v1';

const DEFAULT_HOTKEY_BINDINGS: HotkeyBinding[] = [
  { id: 'togglePlay', action: 'togglePlay', label: '播放 / 暂停', accelerator: 'Alt+Space', enabled: true },
  { id: 'prevTrack', action: 'prevTrack', label: '上一首', accelerator: 'Alt+Left', enabled: true },
  { id: 'nextTrack', action: 'nextTrack', label: '下一首', accelerator: 'Alt+Right', enabled: true },
  { id: 'toggleDesktopLyrics', action: 'toggleDesktopLyrics', label: '开关桌面歌词', accelerator: 'Alt+L', enabled: true },
];

function clampNum(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Number.isFinite(v) ? v : min));
}

function hexToRgbTuple(hex: string | undefined, fallback = '255, 255, 255'): string {
  const raw = String(hex || '').trim().replace(/^#/, '');
  const full = raw.length === 3 ? raw.split('').map((c) => c + c).join('') : raw;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) return fallback;
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

export function applyFxCssVariables(fx: FxSettings): void {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  const rootStyle = root.style;

  const homeAccent = fx.homeAccentColor || '#ffffff';
  const uiAccent = fx.uiAccentColor || '#ffffff';
  const homeIconColor = fx.homeIconColor || '#ffffff';
  const visualIconColor = fx.visualIconColor || '#ffffff';

  rootStyle.setProperty('--home-accent', homeAccent);
  rootStyle.setProperty('--home-accent-rgb', hexToRgbTuple(homeAccent, '255, 255, 255'));
  rootStyle.setProperty('--ui-accent', uiAccent);
  rootStyle.setProperty('--fc-accent', uiAccent);
  rootStyle.setProperty('--fc-accent-rgb', hexToRgbTuple(uiAccent, '255, 255, 255'));
  rootStyle.setProperty('--home-icon-color', homeIconColor);
  rootStyle.setProperty('--home-icon-rgb', hexToRgbTuple(homeIconColor, '255, 255, 255'));
  rootStyle.setProperty('--visual-icon-color', visualIconColor);
  rootStyle.setProperty('--visual-icon-rgb', hexToRgbTuple(visualIconColor, '255, 255, 255'));

  const windowOpacity = clampNum(Number(fx.windowBackgroundOpacity ?? 1), 0, 1);
  const glassOpacity = clampNum(Number(fx.backgroundGlassOpacity ?? 0), 0, 1);
  const glassBlur = glassOpacity > 0.001 ? glassOpacity * 32 : 0;
  const bgOpacity = clampNum(Number(fx.backgroundOpacity ?? 1), 0, 1);

  rootStyle.setProperty('--window-bg-opacity', windowOpacity.toFixed(3));
  rootStyle.setProperty('--custom-bg-base-opacity', windowOpacity.toFixed(3));
  rootStyle.setProperty('--bg-glass-opacity', glassOpacity.toFixed(3));
  rootStyle.setProperty('--custom-bg-glass-opacity', glassOpacity.toFixed(3));
  rootStyle.setProperty('--custom-bg-glass-blur', `${glassBlur.toFixed(1)}px`);
  rootStyle.setProperty('--custom-bg-album-opacity', fx.backgroundAlbumCover ? bgOpacity.toFixed(3) : '1');

  const panelBlur = Math.round(clampNum(Number(fx.playlistPanelGlassBlur ?? 14), 14, 60));
  const panelDensity = clampNum(Number(fx.playlistPanelGlassDensity ?? 0.55), 0.55, 1);
  const openMs = Math.round(clampNum(Number(fx.playlistPanelOpenDuration ?? 0.72), 0.08, 0.72) * 1000);
  const closeMs = Math.round(clampNum(Number(fx.playlistPanelCloseDuration ?? 0.48), 0.06, 0.48) * 1000);

  rootStyle.setProperty('--playlist-panel-glass-blur', `${panelBlur}px`);
  rootStyle.setProperty('--playlist-sticky-blur', `${panelBlur}px`);
  rootStyle.setProperty('--playlist-toolbar-blur', `${Math.round(clampNum(panelBlur * 0.74, 12, 46))}px`);
  rootStyle.setProperty('--playlist-sticky-a1', clampNum(0.52 + panelDensity * 0.46, 0.55, 0.98).toFixed(3));
  rootStyle.setProperty('--playlist-sticky-a2', clampNum(0.46 + panelDensity * 0.48, 0.5, 0.94).toFixed(3));
  rootStyle.setProperty('--playlist-sticky-a3', clampNum(0.28 + panelDensity * 0.56, 0.36, 0.84).toFixed(3));
  rootStyle.setProperty('--playlist-toolbar-a1', clampNum(0.48 + panelDensity * 0.46, 0.52, 0.94).toFixed(3));
  rootStyle.setProperty('--playlist-toolbar-a2', clampNum(0.42 + panelDensity * 0.46, 0.46, 0.88).toFixed(3));
  rootStyle.setProperty('--playlist-toolbar-a3', clampNum(0.24 + panelDensity * 0.48, 0.3, 0.74).toFixed(3));
  rootStyle.setProperty('--playlist-panel-open-ms', `${openMs}ms`);
  rootStyle.setProperty('--playlist-panel-close-ms', `${closeMs}ms`);
  rootStyle.setProperty('--mineradio-playlist-panel-open-ms', `${openMs}ms`);
  rootStyle.setProperty('--mineradio-playlist-panel-close-ms', `${closeMs}ms`);

  const customBg = document.getElementById('custom-bg');
  if (customBg) {
    customBg.style.setProperty('--custom-bg-base-opacity', windowOpacity.toFixed(3));
    customBg.style.setProperty('--custom-bg-glass-opacity', glassOpacity.toFixed(3));
    customBg.style.setProperty('--custom-bg-glass-blur', `${glassBlur.toFixed(1)}px`);
  }
  const panelEl = document.getElementById('playlist-panel');
  if (panelEl) {
    panelEl.style.setProperty('--playlist-sticky-blur', `${panelBlur}px`);
  }
  document.body?.classList.toggle('custom-window-transparent', windowOpacity < 0.999);
  document.body?.classList.toggle('custom-bg-glass-active', glassOpacity > 0.001);
}

function readLsSafe<T>(key: string, fallback: T): T {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return fallback;
    const raw = window.localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function writeLsSafe(key: string, value: unknown): void {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return;
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore
  }
}

function filterShelfPlaylists(state: {
  userPlaylists: PlaylistSummary[];
  builtInPlaylists: PlaylistSummary[];
  fx: FxSettings;
}): PlaylistSummary[] {
  const combined = [...state.builtInPlaylists, ...state.userPlaylists];
  const showPodcasts = Boolean(state.fx?.shelfShowPodcasts);
  const mergeCollections = state.fx?.shelfMergeCollections !== false;

  const seen = new Set<string>();
  const out: PlaylistSummary[] = [];
  for (const pl of combined) {
    if (!pl) continue;
    const rawType = String((pl as unknown as { type?: string; itemType?: string }).type || (pl as unknown as { itemType?: string }).itemType || '').toLowerCase();
    const isPodcast = rawType === 'podcast' || String(pl.id).startsWith('podcast:');
    if (!showPodcasts && isPodcast) continue;
    if (!mergeCollections && pl.subscribed) continue;

    const dedupeKey = `${pl.provider || 'netease'}:${pl.id}`;
    if (mergeCollections && seen.has(dedupeKey)) continue;
    seen.add(dedupeKey);
    out.push(pl);
  }
  return out;
}

function buildShelfItemsFromState(state: {
  userPlaylists: PlaylistSummary[];
  builtInPlaylists: PlaylistSummary[];
  playQueue: SongRecord[];
  currentIdx: number;
  fx: FxSettings;
}): ShelfItem[] {
  const combined = filterShelfPlaylists(state);
  if (combined.length > 0) {
    return combined.map((pl) => ({
      type: 'playlist',
      title: pl.name,
      sub: `${pl.trackCount || 0} 首`,
      cover: pl.cover || '',
      tag: pl.provider === 'mineradio' ? '内置歌单' : pl.subscribed ? '收藏歌单' : '我的歌单',
      playlistId: `${pl.provider}:${pl.id}`,
      provider: pl.provider,
    }));
  }
  return state.playQueue.map((song, idx) => ({
    type: 'queue',
    title: song.name || song.title || '未知曲目',
    sub: song.artist || '未知歌手',
    cover: song.cover || '',
    tag: idx === state.currentIdx ? '正在播放' : `#${idx + 1}`,
    queueIndex: idx,
  }));
}

function todayDateKey(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(
    d.getDate()
  ).padStart(2, '0')}`;
}

function loadInitialTodayStats(): TodayListeningStats {
  const today = todayDateKey();
  const saved = readLsSafe<TodayListeningStats | null>(TODAY_STATS_LS_KEY, null);
  if (saved && saved.date === today) {
    return {
      date: today,
      totalSeconds: Number(saved.totalSeconds) || 0,
      playCount: Number(saved.playCount) || 0,
      topArtists: Array.isArray(saved.topArtists) ? saved.topArtists : [],
    };
  }
  return {
    date: today,
    totalSeconds: 0,
    playCount: 0,
    topArtists: [],
  };
}

function normalizePlatformStatusEntry(raw: Record<string, unknown> | null | undefined): PlatformLoginEntry {
  if (!raw || typeof raw !== 'object') return { loggedIn: false };
  const profile = (raw.profile && typeof raw.profile === 'object' ? raw.profile : {}) as Record<string, unknown>;
  const account = (raw.account && typeof raw.account === 'object' ? raw.account : {}) as Record<string, unknown>;
  const avatar = String(
    raw.avatar || raw.avatarUrl || profile.avatarUrl || profile.avatar || ''
  );
  const loggedIn = Boolean(
    raw.loggedIn ?? (raw.code === 200 && (profile.userId || account.id))
  );
  return {
    loggedIn,
    userId: (raw.userId ?? profile.userId ?? account.id) as string | number | undefined,
    nickname: String(raw.nickname || profile.nickname || ''),
    avatar,
    avatarUrl: avatar,
    vipType: (raw.vipType ?? profile.vipType) as number | undefined,
    cookieValid: raw.cookieValid as boolean | undefined,
    updatedAt: (raw.updatedAt as number | undefined) ?? Date.now(),
  };
}

function formatLyricsToLrc(lines: LyricLine[]): string {
  return lines
    .map((l) => {
      const m = Math.floor(l.time / 60);
      const s = (l.time % 60).toFixed(2).padStart(5, '0');
      return `[${String(m).padStart(2, '0')}:${s}]${l.text}`;
    })
    .join('\n');
}

const rawLyricTextCache = new Map<string, { lyric?: string; tlyric?: string; romalrc?: string }>();

async function fetchSongLyricPayload(
  song: SongRecord
): Promise<{ lyric?: string; tlyric?: string; romalrc?: string } | null> {
  const key = songIdentityKey(song);
  if (key && rawLyricTextCache.has(key)) {
    return rawLyricTextCache.get(key)!;
  }
  let lrcPayload: { lyric?: string; tlyric?: string; romalrc?: string } | null = null;
  if (song.lyric) {
    lrcPayload = {
      lyric: song.lyric,
      tlyric: song.tlyric,
      romalrc: song.romalrc,
    };
  } else if (song.provider === 'local' && song.localFileId) {
    const res = await desktopWindow.readLocalMusicLyric(song.localFileId).catch(() => null);
    if (res?.ok && res.lyric) lrcPayload = res;
  } else if (key) {
    const cached = await desktopWindow.readLyricCache(key).catch(() => null);
    if (cached?.ok && cached.payload && typeof cached.payload === 'object') {
      lrcPayload = cached.payload as { lyric?: string; tlyric?: string; romalrc?: string };
    } else {
      const provider = String(song.provider || 'netease').toLowerCase();
      const lyricUrl =
        provider === 'qq'
          ? `/api/qq/lyric?mid=${encodeURIComponent(String(song.mid || song.id || ''))}`
          : provider === 'kugou'
          ? `/api/kugou/lyric?hash=${encodeURIComponent(String(song.hash || song.id || ''))}`
          : provider === 'qishui'
          ? `/api/qishui/lyric?id=${encodeURIComponent(String(song.id || ''))}`
          : `/api/lyric?id=${encodeURIComponent(String(song.id || ''))}`;
      lrcPayload = await apiJson(lyricUrl, { timeoutMs: 5000 }).catch(() => null);
      if (lrcPayload?.lyric) {
        desktopWindow.writeLyricCache(key, lrcPayload).catch(() => {});
      }
    }
  }
  if (key && lrcPayload?.lyric) {
    rawLyricTextCache.set(key, lrcPayload);
  }
  return lrcPayload;
}

const DEFAULT_LOGIN_STATUS: LoginStatusMap = {
  netease: { loggedIn: false },
  qq: { loggedIn: false },
  kugou: { loggedIn: false },
  qishui: { loggedIn: false },
};

let appInitialized = false;
let lastStatsTrackKey = '';
let lastStatsAudioTime = 0;
let statsSecondFraction = 0;
let lastStatsSaveAt = 0;
let searchRequestSeq = 0;
let playRequestSeq = 0;
let playlistTracksRequestSeq = 0;

function mapPodcastSearchEntries(rawPodcasts: unknown): SongRecord[] {
  if (!Array.isArray(rawPodcasts)) return [];
  const out: SongRecord[] = [];
  for (const raw of rawPodcasts) {
    if (!raw || typeof raw !== 'object') continue;
    const item = raw as Record<string, unknown>;
    const djObj =
      item.dj && typeof item.dj === 'object' ? (item.dj as Record<string, unknown>) : null;
    const rawId = item.id ?? item.rid ?? item.radioId ?? item.podcastId;
    if (rawId === undefined || rawId === null || String(rawId).trim() === '') continue;
    const idStr = String(rawId);
    const artist = String(
      item.djName ||
        djObj?.nickname ||
        item.artist ||
        item.nickname ||
        '播客电台'
    ).trim();
    const album = String(item.category || item.sub || item.desc || '播客电台').trim();
    const cover = String(
      item.cover || item.picUrl || item.coverUrl || item.avatarUrl || ''
    ).trim();
    out.push({
      id: idStr,
      rid: idStr,
      radioId: idStr,
      podcastId: idStr,
      name: String(item.name || item.title || '未知播客'),
      artist,
      album,
      cover,
      provider: 'netease',
      source: 'netease',
      type: 'podcast-radio',
    });
  }
  return out;
}

export const usePlayerStore = create<PlayerStoreState>()((set, get) => ({
  // 播放与队列状态
  playQueue: [],
  currentIdx: -1,
  currentSong: null,
  playing: false,
  currentTime: 0,
  duration: 0,
  volume: 0.72,
  muted: false,
  playMode: 'loop',
  quality: 'exhigh',
  audioError: null,
  automixEnabled: false,
  cuefieldStatus: 'disabled',

  // 歌词状态
  lyrics: [],
  activeLyricIdx: -1,
  lyricOffsetSec: 0,
  desktopLyricsEnabled: false,
  desktopLyricsLocked: false,

  // 搜索与首页
  searchMode: 'song',
  searchQuery: '',
  searchResults: [],
  searchLoading: false,
  searchHistory: readLsSafe<string[]>(SEARCH_HISTORY_LS_KEY, []),
  homeDiscover: null,
  weatherRadio: null,
  todayListeningStats: loadInitialTodayStats(),

  // 歌单与本地曲库
  userPlaylists: [],
  builtInPlaylists: [],
  localTracks: [],
  activePlaylistTracks: [],

  // 登录态与彩蛋门控
  easterEggUnlocked: false,
  platformLogins: DEFAULT_LOGIN_STATUS,

  // 视觉 FX 参数与预设
  fx: { ...DEFAULT_FX_SETTINGS },
  activePreset: DEFAULT_FX_SETTINGS.preset,
  diyMode: false,
  shelfOpen: false,
  fxPanelOpen: false,
  playlistPanelOpen: false,
  splashDone: false,
  updateInfo: null,

  // 扩展弹窗与交互状态
  activeBackgroundMedia: null,
  trackDetailOpen: false,
  trackDetailSong: null,
  customLyricModalOpen: false,
  cuefieldFeedbackOpen: false,
  cuefieldLastTransition: null,
  gestureEnabled: false,
  gestureStatus: 'off',
  gestureHint: '',
  hotkeyBindings: readLsSafe<HotkeyBinding[]>(HOTKEY_BINDINGS_LS_KEY, DEFAULT_HOTKEY_BINDINGS),
  fxArchiveSlots: [],
  controlsHidden: false,
  immersiveMode: false,
  shelfExpandedPlaylistId: null,
  shelfFocusedIndex: 0,

  // ==================== Actions ====================

  initApp: async () => {
    await getRuntimeEndpoints();

    if (!appInitialized) {
      appInitialized = true;
      audioEngine.setCallbacks({
        onTimeUpdate: (currentTime, duration) => {
          const st = get();
          const nextActiveIdx = findActiveLyricIndex(
            st.lyrics,
            currentTime,
            st.lyricOffsetSec
          );
          const idxChanged = nextActiveIdx !== st.activeLyricIdx;

          // 今日听歌时长统计与歌手时长累加
          let nextStats = st.todayListeningStats;
          const today = todayDateKey();
          if (nextStats.date !== today) {
            nextStats = { date: today, totalSeconds: 0, playCount: 0, topArtists: [] };
            statsSecondFraction = 0;
          }
          const trackKey = songIdentityKey(st.currentSong);
          if (st.playing && st.currentSong && trackKey) {
            if (trackKey !== lastStatsTrackKey) {
              lastStatsTrackKey = trackKey;
              lastStatsAudioTime = currentTime;
            } else {
              const delta = currentTime - lastStatsAudioTime;
              lastStatsAudioTime = currentTime;
              if (delta > 0 && delta < 5) {
                statsSecondFraction += delta;
                if (statsSecondFraction >= 1) {
                  const addSec = Math.floor(statsSecondFraction);
                  statsSecondFraction -= addSec;
                  const artistName = String(st.currentSong.artist || '').trim();
                  const nextTopArtists = [...(nextStats.topArtists || [])];
                  if (artistName) {
                    const idx = nextTopArtists.findIndex((a) => a.name === artistName);
                    if (idx >= 0) {
                      nextTopArtists[idx] = {
                        ...nextTopArtists[idx],
                        seconds: (nextTopArtists[idx].seconds || 0) + addSec,
                      };
                    } else {
                      nextTopArtists.push({ name: artistName, count: 1, seconds: addSec });
                    }
                    nextTopArtists.sort(
                      (a, b) => (b.seconds || 0) - (a.seconds || 0) || b.count - a.count
                    );
                  }
                  nextStats = {
                    ...nextStats,
                    totalSeconds: nextStats.totalSeconds + addSec,
                    topArtists: nextTopArtists.slice(0, 12),
                  };
                  const nowWall = Date.now();
                  if (nowWall - lastStatsSaveAt > 3000) {
                    lastStatsSaveAt = nowWall;
                    writeLsSafe(TODAY_STATS_LS_KEY, nextStats);
                  }
                }
              }
            }
          } else {
            lastStatsAudioTime = currentTime;
          }

          set({
            currentTime,
            duration: duration || st.duration,
            activeLyricIdx: nextActiveIdx,
            todayListeningStats: nextStats,
          });
          const effectiveLyricTime = Math.max(0, currentTime + st.lyricOffsetSec);
          visualEngine.setLyrics(st.lyrics, nextActiveIdx, effectiveLyricTime);
          if (idxChanged && st.desktopLyricsEnabled && st.currentSong) {
            const line = st.lyrics[nextActiveIdx];
            const nextLine = st.lyrics[nextActiveIdx + 1];
            desktopWindow
              .updateDesktopLyrics({
                title: st.currentSong.name,
                artist: st.currentSong.artist || '',
                line: line?.text || st.currentSong.name,
                translation: line?.translation || '',
                nextLine: nextLine?.text || '',
                playing: st.playing,
                currentTime: effectiveLyricTime,
                duration,
              })
              .catch(() => {});
          }
        },
        onPlayStateChange: (playing) => {
          set({ playing });
          visualEngine.setPlaying(playing);
        },
        onEnded: () => {
          get().nextTrack().catch(() => {});
        },
        onError: (message) => {
          set({ audioError: message, playing: false });
          visualEngine.setPlaying(false);
        },
        onBeatMapReady: (songKey, map) => {
          const cur = get().currentSong;
          if (cur && songIdentityKey(cur) === songKey) {
            visualEngine.setBeatMap(map);
          }
        },
        onCuefieldStatus: (cuefieldStatus) => {
          set({ cuefieldStatus });
        },
        onCuefieldHandoff: (nextSong, nextIndex, transitionMeta) => {
          const prevSong = get().currentSong;
          const prevStats = get().todayListeningStats;
          const artistName = String(nextSong.artist || '').trim();
          const nextTopArtists = [...(prevStats.topArtists || [])];
          if (artistName) {
            const idx = nextTopArtists.findIndex((a) => a.name === artistName);
            if (idx >= 0) {
              nextTopArtists[idx] = { ...nextTopArtists[idx], count: nextTopArtists[idx].count + 1 };
            } else {
              nextTopArtists.push({ name: artistName, count: 1, seconds: 0 });
            }
          }
          lastStatsTrackKey = songIdentityKey(nextSong);
          lastStatsAudioTime = 0;

          set({
            currentSong: nextSong,
            currentIdx: nextIndex,
            currentTime: 0,
            duration: Number(nextSong.duration) || 0,
            playing: true,
            lyrics: [],
            activeLyricIdx: -1,
            cuefieldFeedbackOpen: true,
            cuefieldLastTransition: {
              ...(transitionMeta || {}),
              fromSong: prevSong ? { id: prevSong.id, name: prevSong.name, artist: prevSong.artist } : null,
              toSong: { id: nextSong.id, name: nextSong.name, artist: nextSong.artist },
              timestamp: Date.now(),
            },
            todayListeningStats: {
              ...prevStats,
              playCount: prevStats.playCount + 1,
              topArtists: nextTopArtists.slice(0, 12),
            },
          });

          visualEngine.setCurrentSongContext(nextSong, get().playQueue, nextIndex);
          visualEngine.setCover(nextSong.cover || '');
          visualEngine.setLyrics([], -1, 0);
          visualEngine.setPlaying(true);

          const handoffKey = songIdentityKey(nextSong);
          fetchSongLyricPayload(nextSong)
            .then((lrcPayload) => {
              const cur = get().currentSong;
              if (!cur || songIdentityKey(cur) !== handoffKey || !lrcPayload?.lyric) return;
              const parsed = parseLrcText(
                lrcPayload.lyric,
                lrcPayload.tlyric,
                lrcPayload.romalrc
              );
              const activeIdx = findActiveLyricIndex(
                parsed,
                get().currentTime,
                get().lyricOffsetSec
              );
              set({ lyrics: parsed, activeLyricIdx: activeIdx });
              const effectiveLyricTime = Math.max(0, get().currentTime + get().lyricOffsetSec);
              visualEngine.setLyrics(parsed, activeIdx, effectiveLyricTime);
              if (get().desktopLyricsEnabled) {
                const line = parsed[activeIdx] || parsed[0];
                const nextLine = parsed[activeIdx + 1] || parsed[1];
                desktopWindow
                  .updateDesktopLyrics({
                    title: cur.name,
                    artist: cur.artist || '',
                    line: line?.text || cur.name,
                    translation: line?.translation || '',
                    nextLine: nextLine?.text || '',
                    playing: get().playing,
                    currentTime: effectiveLyricTime,
                    duration: get().duration,
                  })
                  .catch(() => {});
              }
            })
            .catch(() => {});
        },
        getNextTrackContext: () => {
          const st = get();
          if (st.playQueue.length < 2 || st.playMode === 'single') return null;
          const nextIdx = computeNextTrackIndex(
            st.playQueue.length,
            st.currentIdx,
            st.playMode
          );
          if (nextIdx < 0 || nextIdx === st.currentIdx) return null;
          const nextSong = st.playQueue[nextIdx] || null;
          const fromKey = songIdentityKey(st.currentSong);
          const toKey = songIdentityKey(nextSong);
          const cachedFrom = fromKey ? rawLyricTextCache.get(fromKey)?.lyric : undefined;
          const cachedTo = toKey ? rawLyricTextCache.get(toKey)?.lyric : undefined;
          const fromLrc =
            cachedFrom ||
            st.currentSong?.lyric ||
            (st.lyrics.length > 0 ? formatLyricsToLrc(st.lyrics) : '');
          const toLrc = cachedTo || nextSong?.lyric || '';
          if (nextSong && !toLrc) {
            fetchSongLyricPayload(nextSong).catch(() => {});
          }
          return {
            currentSong: st.currentSong,
            currentIndex: st.currentIdx,
            nextSong,
            nextIndex: nextIdx,
            quality: st.quality,
            fromLrc,
            toLrc,
          };
        },
      });

      desktopWindow.onDesktopLyricsLockState((payload) => {
        set({ desktopLyricsLocked: Boolean(payload?.locked) });
      });
      desktopWindow.onDesktopLyricsEnabledState((payload) => {
        set({ desktopLyricsEnabled: Boolean(payload?.enabled) });
      });
      desktopWindow.onPlatformLoginSuccess(() => {
        get().refreshLoginStatuses().catch(() => {});
        get().refreshAllUserPlaylists().catch(() => {});
      });
    }

    const [fxRes, gateRes, builtInRes, localRes] = await Promise.all([
      desktopWindow.readCurrentFxAutosave().catch(() => ({ ok: false, snapshot: null })),
      desktopWindow.getLoginEasterEggStatus().catch(() => ({
        ok: false,
        gateVersion: 'world-peace-v1',
        unlocked: false,
        resetComplete: false,
      })),
      desktopWindow.listBuiltInPlaylists().catch(() => ({ ok: false, playlists: [] })),
      desktopWindow.listLocalMusicLibrary().catch(() => ({ ok: false, tracks: [] })),
    ]);

    const mergedFx: FxSettings = {
      ...DEFAULT_FX_SETTINGS,
      ...(fxRes?.snapshot || {}),
    };
    const builtInPlaylists = builtInRes?.playlists || [];
    const localTracks = localRes?.tracks || [];

    set((state) => ({
      fx: mergedFx,
      activePreset: mergedFx.preset,
      desktopLyricsEnabled: Boolean(mergedFx.desktopLyrics),
      easterEggUnlocked: Boolean(gateRes?.unlocked),
      builtInPlaylists,
      localTracks,
      shelfOpen: state.shelfOpen,
    }));

    applyFxCssVariables(mergedFx);
    visualEngine.syncFx(mergedFx);
    const shelfItems = buildShelfItemsFromState(get());
    visualEngine.setShelfItems(shelfItems, get().shelfOpen);

    // 非阻塞初始化：加载默认 FX 存档、配置全局热键、首页推荐、更新检查与多平台歌单
    get().loadFxArchiveDefaults().catch(() => {});
    desktopWindow.configureGlobalHotkeys(get().hotkeyBindings).catch(() => {});
    get().refreshHomeDiscover().catch(() => {});
    get().checkUpdates().catch(() => {});
    if (gateRes?.unlocked) {
      get().refreshLoginStatuses().catch(() => {});
      get().refreshAllUserPlaylists().catch(() => {});
    }
  },

  unlockEasterEgg: async (password: string) => {
    const res = await desktopWindow.unlockLoginEasterEgg(password);
    set({ easterEggUnlocked: Boolean(res.unlocked) });
    if (res.unlocked) {
      get().refreshLoginStatuses().catch(() => {});
    }
    return res;
  },

  resetEasterEgg: async () => {
    const res = await desktopWindow.resetLoginEasterEgg();
    set({
      easterEggUnlocked: false,
      platformLogins: DEFAULT_LOGIN_STATUS,
    });
    return res;
  },

  searchTracks: async (query: string, mode?: SearchMode) => {
    const reqSeq = ++searchRequestSeq;
    const trimmed = String(query || '').trim();
    const activeMode = mode ?? get().searchMode;
    set({ searchQuery: query, searchMode: activeMode });
    if (!trimmed) {
      set({ searchResults: [], searchLoading: false });
      return [];
    }

    set({ searchLoading: true });
    try {
      let endpoint = `/api/search?keywords=${encodeURIComponent(trimmed)}&limit=30`;
      if (activeMode === 'qq') {
        endpoint = `/api/qq/search?keywords=${encodeURIComponent(trimmed)}&limit=30`;
      } else if (activeMode === 'kugou') {
        endpoint = `/api/kugou/search?keywords=${encodeURIComponent(trimmed)}&limit=30`;
      } else if (activeMode === 'qishui') {
        endpoint = `/api/qishui/search?keywords=${encodeURIComponent(trimmed)}&limit=30`;
      } else if (activeMode === 'podcast') {
        endpoint = `/api/podcast/search?keywords=${encodeURIComponent(trimmed)}&limit=24`;
      }

      const res = await apiJson<{
        songs?: SongRecord[];
        tracks?: SongRecord[];
        results?: SongRecord[];
        podcasts?: unknown[];
        data?: { songs?: SongRecord[] };
      }>(endpoint, { timeoutMs: 12000 });

      if (reqSeq !== searchRequestSeq) return [];
      const list =
        res.songs ||
        res.tracks ||
        res.results ||
        res.data?.songs ||
        mapPodcastSearchEntries(res.podcasts);
      set({ searchResults: list, searchLoading: false });
      return list;
    } catch {
      if (reqSeq !== searchRequestSeq) return [];
      set({ searchResults: [], searchLoading: false });
      return [];
    }
  },

  playSong: async (songOrIndex: SongRecord | number, queue?: SongRecord[]) => {
    const reqSeq = ++playRequestSeq;
    const state = get();
    const nextQueue = Array.isArray(queue) ? queue : state.playQueue;
    let targetSong: SongRecord | null = null;
    let targetIdx = -1;

    if (typeof songOrIndex === 'number') {
      targetIdx = songOrIndex;
      targetSong = nextQueue[targetIdx] || null;
    } else {
      targetSong = songOrIndex;
      const key = songIdentityKey(songOrIndex);
      targetIdx = nextQueue.findIndex((item) => songIdentityKey(item) === key);
      if (targetIdx < 0) {
        const appended = [...nextQueue, songOrIndex];
        targetIdx = appended.length - 1;
        set({ playQueue: appended });
      } else if (Array.isArray(queue)) {
        set({ playQueue: nextQueue });
      }
    }

    if (!targetSong) return;

    const currentQueue = get().playQueue;
    const targetArtist = String(targetSong.artist || '').trim();
    lastStatsTrackKey = songIdentityKey(targetSong);
    lastStatsAudioTime = 0;

    set((prev) => {
      const today = todayDateKey();
      const baseStats =
        prev.todayListeningStats.date === today
          ? prev.todayListeningStats
          : { date: today, totalSeconds: 0, playCount: 0, topArtists: [] };
      const nextTopArtists = [...(baseStats.topArtists || [])];
      if (targetArtist) {
        const idx = nextTopArtists.findIndex((a) => a.name === targetArtist);
        if (idx >= 0) {
          nextTopArtists[idx] = { ...nextTopArtists[idx], count: nextTopArtists[idx].count + 1 };
        } else {
          nextTopArtists.push({ name: targetArtist, count: 1, seconds: 0 });
        }
        nextTopArtists.sort(
          (a, b) => (b.seconds || 0) - (a.seconds || 0) || b.count - a.count
        );
      }
      const nextStats = {
        ...baseStats,
        playCount: baseStats.playCount + 1,
        topArtists: nextTopArtists.slice(0, 12),
      };
      writeLsSafe(TODAY_STATS_LS_KEY, nextStats);
      return {
        playQueue: Array.isArray(queue) ? queue : currentQueue,
        currentIdx: targetIdx,
        currentSong: targetSong,
        currentTime: 0,
        duration: Number(targetSong!.duration) || 0,
        audioError: null,
        lyrics: [],
        activeLyricIdx: -1,
        todayListeningStats: nextStats,
      };
    });

    visualEngine.setCurrentSongContext(targetSong, get().playQueue, targetIdx);
    visualEngine.setBeatMap(audioEngine.getCachedBeatMap(targetSong));
    visualEngine.setCover(targetSong.cover || '');
    visualEngine.setLyrics([], -1, 0);
    visualEngine.setShelfItems(buildShelfItemsFromState(get()), get().shelfOpen);

    try {
      const resolvedUrl = await audioEngine.playTrack(targetSong, get().quality, true);
      if (reqSeq !== playRequestSeq || !resolvedUrl) return;
      set({ playing: true, audioError: null });
      visualEngine.setPlaying(true);
    } catch (err) {
      if (reqSeq !== playRequestSeq) return;
      const msg = err instanceof Error ? err.message : String(err);
      set({ playing: false, audioError: msg });
      visualEngine.setPlaying(false);
    }

    // 异步拉取并解析歌词
    const key = songIdentityKey(targetSong);
    try {
      const lrcPayload = await fetchSongLyricPayload(targetSong);
      if (
        reqSeq === playRequestSeq &&
        get().currentSong &&
        songIdentityKey(get().currentSong) === key &&
        lrcPayload?.lyric
      ) {
        const parsed = parseLrcText(lrcPayload.lyric, lrcPayload.tlyric, lrcPayload.romalrc);
        const activeIdx = findActiveLyricIndex(parsed, get().currentTime, get().lyricOffsetSec);
        set({ lyrics: parsed, activeLyricIdx: activeIdx });
        visualEngine.setLyrics(
          parsed,
          activeIdx,
          Math.max(0, get().currentTime + get().lyricOffsetSec)
        );
      }
    } catch {
      // ignore lyric fetch failure
    }
  },

  togglePlay: async () => {
    const st = get();
    if (!st.currentSong) {
      if (st.playQueue.length > 0) {
        await st.playSong(0);
      }
      return;
    }
    if (st.playing) {
      audioEngine.pause();
      set({ playing: false });
      visualEngine.setPlaying(false);
    } else {
      await audioEngine.play();
      set({ playing: true });
      visualEngine.setPlaying(true);
    }
  },

  nextTrack: async () => {
    const st = get();
    const nextIdx = computeNextTrackIndex(
      st.playQueue.length,
      st.currentIdx,
      st.playMode
    );
    if (nextIdx < 0) return;
    await st.playSong(nextIdx);
  },

  prevTrack: async () => {
    const st = get();
    const prevIdx = computePrevTrackIndex(
      st.playQueue.length,
      st.currentIdx,
      st.playMode
    );
    if (prevIdx < 0) return;
    await st.playSong(prevIdx);
  },

  seekTo: (seconds: number) => {
    const nextTime = Math.max(0, Number(seconds) || 0);
    audioEngine.seek(nextTime);
    const st = get();
    const activeIdx = findActiveLyricIndex(st.lyrics, nextTime, st.lyricOffsetSec);
    set({ currentTime: nextTime, activeLyricIdx: activeIdx });
    visualEngine.setLyrics(st.lyrics, activeIdx, Math.max(0, nextTime + st.lyricOffsetSec));
  },

  setVolume: (volume: number) => {
    const clamped = Math.max(0, Math.min(1, Number(volume) || 0));
    audioEngine.setVolume(clamped);
    set({ volume: clamped, muted: clamped === 0 ? true : false });
    audioEngine.setMuted(clamped === 0);
  },

  toggleMute: () => {
    const nextMuted = !get().muted;
    audioEngine.setMuted(nextMuted);
    set({ muted: nextMuted });
  },

  setPlayMode: (mode: PlayMode) => {
    audioEngine.setLoop(mode === 'single');
    set({ playMode: mode });
  },

  cyclePlayMode: () => {
    const nextMode = resolveNextPlayMode(get().playMode);
    get().setPlayMode(nextMode);
  },

  setQuality: (quality: QualityPreference) => {
    const prev = get();
    set({ quality });
    if (prev.currentSong && prev.quality !== quality) {
      const song = prev.currentSong;
      const resumeAt = prev.currentTime || audioEngine.getCurrentTime();
      const shouldAutoplay = prev.playing || audioEngine.isPlaying();
      audioEngine
        .playTrack(song, quality, shouldAutoplay)
        .then(() => {
          const cur = get().currentSong;
          if (cur && songIdentityKey(cur) === songIdentityKey(song) && resumeAt > 0) {
            audioEngine.seek(resumeAt);
            set({ currentTime: resumeAt });
          }
        })
        .catch(() => {});
    }
  },

  setAutomixEnabled: (enabled: boolean) => {
    audioEngine.setAutomixEnabled(enabled);
    set({ automixEnabled: Boolean(enabled) });
  },

  setLyricOffsetSec: (offsetSec: number) => {
    const st = get();
    const clamped = Math.max(-10, Math.min(10, Number(offsetSec) || 0));
    const activeIdx = findActiveLyricIndex(st.lyrics, st.currentTime, clamped);
    set({ lyricOffsetSec: clamped, activeLyricIdx: activeIdx });
    visualEngine.setLyrics(st.lyrics, activeIdx, Math.max(0, st.currentTime + clamped));
  },

  toggleDesktopLyrics: async (enabled?: boolean) => {
    const next = enabled ?? !get().desktopLyricsEnabled;
    const st = get();
    const line = st.lyrics[st.activeLyricIdx];
    const res = await desktopWindow.setDesktopLyricsEnabled(next, {
      title: st.currentSong?.name || 'Mineradio',
      artist: st.currentSong?.artist || '',
      line: line?.text || st.currentSong?.name || '',
      translation: line?.translation || '',
      playing: st.playing,
    });
    set({ desktopLyricsEnabled: Boolean(res.enabled) });
    get().updateFx({ desktopLyrics: Boolean(res.enabled) });
  },

  importLocalFiles: async (files?: FileList | File[]) => {
    let importedTracks: SongRecord[] = [];
    if (files && files.length > 0) {
      const res = await desktopWindow.importLocalMusicFiles(files);
      importedTracks = res?.tracks || [];
    } else {
      const chosen = await desktopWindow.chooseLocalMusicFiles();
      if (chosen.ok && chosen.files?.length) {
        const res = await desktopWindow.authorizeAndImport(
          chosen.files.map((p) => ({ path: p }))
        );
        importedTracks = res?.tracks || [];
      }
    }

    if (importedTracks.length > 0) {
      const lib = await desktopWindow.listLocalMusicLibrary().catch(() => ({
        ok: false,
        tracks: [] as SongRecord[],
      }));
      const merged = lib.tracks?.length ? lib.tracks : [...importedTracks, ...get().localTracks];
      set({ localTracks: merged });
      return importedTracks;
    }
    return [];
  },

  loadLocalLibrary: async () => {
    const res = await desktopWindow.listLocalMusicLibrary().catch(() => ({
      ok: false,
      tracks: [] as SongRecord[],
    }));
    const tracks = res?.tracks || [];
    set({ localTracks: tracks });
    return tracks;
  },

  loadPlaylistTracks: async (playlist: PlaylistSummary) => {
    if (!playlist) return [];
    const reqSeq = ++playlistTracksRequestSeq;
    if (playlist.provider === 'mineradio') {
      const res = await desktopWindow.readBuiltInPlaylist(String(playlist.id), {
        offset: 0,
        limit: 500,
      });
      if (reqSeq !== playlistTracksRequestSeq) return [];
      const tracks = res.tracks || [];
      set({ activePlaylistTracks: tracks });
      return tracks;
    }
    const id = encodeURIComponent(String(playlist.id));
    const endpoint =
      playlist.provider === 'qq'
        ? `/api/qq/playlist/tracks?id=${id}`
        : playlist.provider === 'kugou'
        ? `/api/kugou/playlist/tracks?id=${id}`
        : playlist.provider === 'qishui'
        ? `/api/qishui/playlist/tracks?id=${id}`
        : `/api/playlist/tracks?id=${id}`;
    const res = await apiJson<{ tracks?: SongRecord[]; songs?: SongRecord[] }>(endpoint, {
      timeoutMs: 12000,
    }).catch((): { tracks?: SongRecord[]; songs?: SongRecord[] } => ({ tracks: [] }));
    if (reqSeq !== playlistTracksRequestSeq) return [];
    const tracks = res.tracks || res.songs || [];
    set({ activePlaylistTracks: tracks });
    return tracks;
  },

  updateFx: (patch: Partial<FxSettings>) => {
    const nextFx: FxSettings = {
      ...get().fx,
      ...patch,
    };
    set({
      fx: nextFx,
      activePreset: nextFx.preset,
    });
    applyFxCssVariables(nextFx);
    visualEngine.syncFx(nextFx);
    if ('shelfShowPodcasts' in patch || 'shelfMergeCollections' in patch || 'shelf' in patch) {
      visualEngine.setShelfItems(buildShelfItemsFromState(get()), get().shelfOpen);
    }
    if ('gestureSensitivity' in patch && patch.gestureSensitivity) {
      gestureEngine.setSensitivity(patch.gestureSensitivity);
    }
    if ('gestureHandOverlay' in patch) {
      gestureEngine.setOverlayEnabled(patch.gestureHandOverlay !== false);
    }
    desktopWindow.saveCurrentFxAutosave(nextFx).catch(() => {});
  },

  resetFxDefaults: () => {
    const nextFx: FxSettings = { ...DEFAULT_FX_SETTINGS };
    set({
      fx: nextFx,
      activePreset: nextFx.preset,
    });
    applyFxCssVariables(nextFx);
    visualEngine.syncFx(nextFx);
    visualEngine.setShelfItems(buildShelfItemsFromState(get()), get().shelfOpen);
    desktopWindow.saveCurrentFxAutosave(nextFx).catch(() => {});
  },

  applyVisualPreset: (presetId: number) => {
    const normalized = Math.max(0, Math.min(12, Math.round(Number(presetId) || 0)));
    get().updateFx({ preset: normalized });
  },

  setSearchMode: (searchMode) => set({ searchMode }),
  setSearchQuery: (searchQuery) => set({ searchQuery }),
  setDiyMode: (diyMode) => set({ diyMode }),
  setShelfOpen: (shelfOpen) => {
    set({ shelfOpen });
    visualEngine.setShelfItems(buildShelfItemsFromState(get()), shelfOpen);
  },
  setFxPanelOpen: (fxPanelOpen) => set({ fxPanelOpen }),
  setPlaylistPanelOpen: (playlistPanelOpen) => set({ playlistPanelOpen }),
  setSplashDone: (splashDone) => set({ splashDone }),

  checkUpdates: async () => {
    try {
      const raw = await apiJson<UpdateCheckResult>('/api/update/latest', { timeoutMs: 8000 });
      const rel = raw.release;
      const hasUpdate = Boolean(raw.hasUpdate ?? raw.updateAvailable);
      const notesRaw = raw.notes ?? rel?.notes ?? rel?.summary;
      const notes = Array.isArray(notesRaw) ? notesRaw.join('\n') : String(notesRaw || '');
      const normalized: UpdateCheckResult = {
        ...raw,
        hasUpdate,
        updateAvailable: hasUpdate,
        downloadPages: raw.downloadPages || rel?.downloadPages || [],
        releaseUrl: raw.releaseUrl || rel?.downloadPageUrl || rel?.htmlUrl || '',
        notes,
        checkedAt: raw.checkedAt ?? Date.now(),
      };
      set({ updateInfo: normalized });
      return normalized;
    } catch {
      return null;
    }
  },

  loadHomeDiscover: async () => {
    try {
      const raw = await apiJson<DiscoverHomeData>('/api/discover/home', { timeoutMs: 8000 });
      const normalized: DiscoverHomeData = {
        ...raw,
        songs: raw.songs?.length ? raw.songs : raw.dailySongs || [],
        dailySongs: raw.dailySongs?.length ? raw.dailySongs : raw.songs || [],
        playlists: raw.playlists || [],
        podcasts: raw.podcasts || [],
        updatedAt: raw.updatedAt ?? Date.now(),
      };
      set({ homeDiscover: normalized });
      return normalized;
    } catch {
      return null;
    }
  },

  loadWeatherRadio: async () => {
    try {
      const raw = await apiJson<WeatherRadioData>('/api/weather/radio', { timeoutMs: 6000 });
      const rawWeather =
        raw.weather && typeof raw.weather === 'object' ? raw.weather : undefined;
      const summaryText =
        typeof raw.weather === 'string'
          ? raw.weather
          : rawWeather?.summary || raw.radio?.title || '晴朗微风';
      const weatherObj = rawWeather
        ? Object.assign({}, rawWeather, {
            summary: summaryText,
            toString: () => summaryText,
          })
        : summaryText;

      const normalized: WeatherRadioData = {
        ...raw,
        weather: weatherObj,
        city: raw.city || rawWeather?.city || '',
        temperature: raw.temperature ?? rawWeather?.temperature,
        greeting: raw.greeting || raw.radio?.subtitle || raw.radio?.title || '',
        tracks: raw.tracks?.length ? raw.tracks : raw.radio?.songs || [],
      };
      set({ weatherRadio: normalized });
      return normalized;
    } catch {
      return null;
    }
  },

  refreshHomeDiscover: async () => {
    await Promise.all([
      get().loadHomeDiscover().catch(() => null),
      get().loadWeatherRadio().catch(() => null),
    ]);
  },

  refreshLoginStatuses: async () => {
    try {
      const [neteaseRes, qqRes, kugouRes, qishuiRes] = await Promise.all([
        apiJson<Record<string, unknown>>('/api/login/status', { timeoutMs: 6000 }).catch(() => null),
        apiJson<Record<string, unknown>>('/api/qq/login/status', { timeoutMs: 6000 }).catch(() => null),
        apiJson<Record<string, unknown>>('/api/kugou/login/status', { timeoutMs: 6000 }).catch(() => null),
        apiJson<Record<string, unknown>>('/api/qishui/status', { timeoutMs: 6000 }).catch(() => null),
      ]);

      // 兼容单接口同时返回多平台或各自平台独立返回
      const neteaseRaw =
        neteaseRes && 'netease' in neteaseRes
          ? (neteaseRes.netease as Record<string, unknown>)
          : neteaseRes;
      const qqRaw =
        qqRes || (neteaseRes && 'qq' in neteaseRes ? (neteaseRes.qq as Record<string, unknown>) : null);
      const kugouRaw =
        kugouRes ||
        (neteaseRes && 'kugou' in neteaseRes ? (neteaseRes.kugou as Record<string, unknown>) : null);
      const qishuiRaw =
        qishuiRes ||
        (neteaseRes && 'qishui' in neteaseRes ? (neteaseRes.qishui as Record<string, unknown>) : null);

      set({
        platformLogins: {
          netease: normalizePlatformStatusEntry(neteaseRaw),
          qq: normalizePlatformStatusEntry(qqRaw),
          kugou: normalizePlatformStatusEntry(kugouRaw),
          qishui: normalizePlatformStatusEntry(qishuiRaw),
        },
      });
    } catch {
      // ignore when locked or offline
    }
  },

  refreshAllUserPlaylists: async () => {
    const extractList = (
      res: { playlists?: PlaylistSummary[]; list?: PlaylistSummary[]; data?: PlaylistSummary[] } | null,
      provider: ProviderType
    ): PlaylistSummary[] => {
      if (!res) return [];
      const arr = res.playlists || res.list || res.data || [];
      if (!Array.isArray(arr)) return [];
      return arr.map((pl) => ({
        ...pl,
        provider: pl.provider || provider,
      }));
    };

    const [neteaseRes, qqRes, kugouRes, qishuiRes] = await Promise.all([
      apiJson<{ playlists?: PlaylistSummary[]; list?: PlaylistSummary[] }>('/api/user/playlists', {
        timeoutMs: 8000,
      }).catch(() => null),
      apiJson<{ playlists?: PlaylistSummary[]; list?: PlaylistSummary[] }>('/api/qq/user/playlists', {
        timeoutMs: 8000,
      }).catch(() => null),
      apiJson<{ playlists?: PlaylistSummary[]; list?: PlaylistSummary[] }>('/api/kugou/user/playlists', {
        timeoutMs: 8000,
      }).catch(() => null),
      apiJson<{ playlists?: PlaylistSummary[]; list?: PlaylistSummary[] }>('/api/qishui/user/playlists', {
        timeoutMs: 8000,
      }).catch(() => null),
    ]);

    const mergedRaw = [
      ...extractList(neteaseRes, 'netease'),
      ...extractList(qqRes, 'qq'),
      ...extractList(kugouRes, 'kugou'),
      ...extractList(qishuiRes, 'qishui'),
    ];
    const seen = new Set<string>();
    const deduped: PlaylistSummary[] = [];
    for (const pl of mergedRaw) {
      const key = `${pl.provider}:${pl.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      deduped.push(pl);
    }

    set({ userPlaylists: deduped });
    visualEngine.setShelfItems(buildShelfItemsFromState(get()), get().shelfOpen);
    return deduped;
  },

  getShelfPlaylists: () => {
    const st = get();
    return filterShelfPlaylists({
      userPlaylists: st.userPlaylists,
      builtInPlaylists: st.builtInPlaylists,
      fx: st.fx,
    });
  },

  // ==================== 扩展 Actions ====================

  setActiveBackgroundMedia: (media: BackgroundMediaState | null) => {
    set({ activeBackgroundMedia: media });
  },

  openTrackDetail: (song?: SongRecord) => {
    set({
      trackDetailOpen: true,
      trackDetailSong: song || get().currentSong,
    });
  },

  closeTrackDetail: () => {
    set({ trackDetailOpen: false });
  },

  setCustomLyricModalOpen: (open: boolean) => {
    set({ customLyricModalOpen: Boolean(open) });
  },

  saveCustomLyricForSong: async (songKey: string, lrcText: string) => {
    const key = String(songKey || '').trim();
    if (!key) return;
    const payload = { lyric: lrcText, custom: true, updatedAt: Date.now() };
    rawLyricTextCache.set(key, payload);
    await desktopWindow.writeLyricCache(key, payload).catch(() => {});

    const cur = get().currentSong;
    if (cur && songIdentityKey(cur) === key) {
      const parsed = parseLrcText(lrcText);
      const activeIdx = findActiveLyricIndex(parsed, get().currentTime, get().lyricOffsetSec);
      set({ lyrics: parsed, activeLyricIdx: activeIdx });
      visualEngine.setLyrics(
        parsed,
        activeIdx,
        Math.max(0, get().currentTime + get().lyricOffsetSec)
      );
    }
  },

  submitCuefieldFeedback: async (rating: number) => {
    const last = get().cuefieldLastTransition;
    set({ cuefieldFeedbackOpen: false });
    try {
      await apiJson('/api/cuefield/feedback', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          rating: Math.max(1, Math.min(5, Math.round(Number(rating) || 3))),
          transition: last,
          timestamp: Date.now(),
        }),
        timeoutMs: 5000,
      });
    } catch {
      // ignore offline
    }
  },

  dismissCuefieldFeedback: () => {
    set({ cuefieldFeedbackOpen: false });
  },

  setGestureEnabled: async (enabled: boolean) => {
    const target = Boolean(enabled);
    if (target) {
      gestureEngine.setSensitivity(get().fx.gestureSensitivity || 'balanced');
      gestureEngine.setOverlayEnabled(get().fx.gestureHandOverlay !== false);
      const ok = await gestureEngine.start();
      set({
        gestureEnabled: ok,
        gestureStatus: ok ? 'active' : get().gestureStatus || 'error',
      });
      get().updateFx({ cam: ok ? 'gesture' : 'off' });
    } else {
      gestureEngine.stop();
      set({ gestureEnabled: false, gestureStatus: 'off', gestureHint: '' });
      get().updateFx({ cam: 'off' });
    }
  },

  addSearchHistory: (kw: string) => {
    const trimmed = String(kw || '').trim();
    if (!trimmed) return;
    const next = [trimmed, ...get().searchHistory.filter((item) => item !== trimmed)].slice(0, 20);
    set({ searchHistory: next });
    writeLsSafe(SEARCH_HISTORY_LS_KEY, next);
  },

  clearSearchHistory: () => {
    set({ searchHistory: [] });
    writeLsSafe(SEARCH_HISTORY_LS_KEY, []);
  },

  updateHotkeyBindings: async (bindings: HotkeyBinding[]) => {
    const list = Array.isArray(bindings) ? bindings : [];
    set({ hotkeyBindings: list });
    writeLsSafe(HOTKEY_BINDINGS_LS_KEY, list);
    await desktopWindow.configureGlobalHotkeys(list).catch(() => {});
  },

  loadFxArchiveDefaults: async () => {
    try {
      if (typeof fetch !== 'function') return;
      const resp = await fetch('/default-user-fx-archive.json');
      if (!resp.ok) return;
      const json = (await resp.json()) as {
        name?: string;
        snapshot?: Partial<FxSettings>;
        slots?: FxArchiveSlot[];
      };
      const slots: FxArchiveSlot[] = Array.isArray(json?.slots) ? json.slots : [];
      if (json?.snapshot && typeof json.snapshot === 'object') {
        slots.unshift({
          name: json.name || 'default',
          label: json.name || '默认存档',
          fx: json.snapshot,
        });
      }
      set({ fxArchiveSlots: slots });
    } catch {
      // ignore in test/offline mode
    }
  },

  applyFxArchiveSlot: (name: string) => {
    const found = get().fxArchiveSlots.find((s) => s.name === name);
    if (found?.fx) {
      get().updateFx(found.fx);
    }
  },

  setControlsHidden: (controlsHidden: boolean) => {
    set({ controlsHidden: Boolean(controlsHidden) });
  },

  setImmersiveMode: (immersiveMode: boolean) => {
    const next = Boolean(immersiveMode);
    set({ immersiveMode: next, controlsHidden: next });
  },

  setShelfExpandedPlaylistId: (shelfExpandedPlaylistId: string | null) => {
    set({ shelfExpandedPlaylistId });
  },

  setShelfFocusedIndex: (shelfFocusedIndex: number) => {
    const idx = Math.max(0, Math.round(Number(shelfFocusedIndex) || 0));
    set({ shelfFocusedIndex: idx });
    visualEngine.setShelfCenter(idx);
  },
}));
