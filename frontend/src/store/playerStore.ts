import { create } from 'zustand';
import {
  apiJson,
  desktopWindow,
  getRuntimeEndpoints,
} from '../bridge/tauriBridge';
import { audioEngine, songIdentityKey, visualEngine } from '../engines';
import {
  DEFAULT_FX_SETTINGS,
  type CuefieldStatus,
  type EasterEggStatus,
  type FxSettings,
  type HomeDiscoverData,
  type LoginStatusMap,
  type LyricLine,
  type PlayMode,
  type PlaylistSummary,
  type QualityPreference,
  type SearchMode,
  type ShelfItem,
  type SongRecord,
  type TodayListeningStats,
  type UpdateCheckResult,
  type WeatherRadioData,
} from '../types/mineradio';

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

function buildShelfItemsFromState(state: {
  userPlaylists: PlaylistSummary[];
  builtInPlaylists: PlaylistSummary[];
  playQueue: SongRecord[];
  currentIdx: number;
}): ShelfItem[] {
  const combined = [...state.builtInPlaylists, ...state.userPlaylists];
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

const DEFAULT_LOGIN_STATUS: LoginStatusMap = {
  netease: { loggedIn: false },
  qq: { loggedIn: false },
  kugou: { loggedIn: false },
  qishui: { loggedIn: false },
};

export interface PlayerState {
  // 播放与队列状态
  playQueue: SongRecord[];
  currentIdx: number;
  currentSong: SongRecord | null;
  playing: boolean;
  currentTime: number;
  duration: number;
  volume: number;
  muted: boolean;
  playMode: PlayMode;
  quality: QualityPreference;
  audioError: string | null;
  automixEnabled: boolean;
  cuefieldStatus: CuefieldStatus;

  // 歌词状态
  lyrics: LyricLine[];
  activeLyricIdx: number;
  lyricOffsetSec: number;
  desktopLyricsEnabled: boolean;
  desktopLyricsLocked: boolean;

  // 搜索与首页
  searchMode: SearchMode;
  searchQuery: string;
  searchResults: SongRecord[];
  searchLoading: boolean;
  homeDiscover: HomeDiscoverData | null;
  weatherRadio: WeatherRadioData | null;
  todayListeningStats: TodayListeningStats;

  // 歌单与本地曲库
  userPlaylists: PlaylistSummary[];
  builtInPlaylists: PlaylistSummary[];
  localTracks: SongRecord[];
  activePlaylistTracks: SongRecord[];

  // 登录态与彩蛋门控
  easterEggUnlocked: boolean;
  platformLogins: LoginStatusMap;

  // 视觉 FX 参数与预设 / UI 面板
  fx: FxSettings;
  activePreset: number;
  diyMode: boolean;
  shelfOpen: boolean;
  fxPanelOpen: boolean;
  playlistPanelOpen: boolean;
  splashDone: boolean;
  updateInfo: UpdateCheckResult | null;

  // 核心 Actions
  initApp: () => Promise<void>;
  unlockEasterEgg: (password: string) => Promise<EasterEggStatus>;
  resetEasterEgg: () => Promise<EasterEggStatus>;
  searchTracks: (query: string, mode?: SearchMode) => Promise<SongRecord[]>;
  playSong: (songOrIndex: SongRecord | number, queue?: SongRecord[]) => Promise<void>;
  togglePlay: () => Promise<void>;
  nextTrack: () => Promise<void>;
  prevTrack: () => Promise<void>;
  seekTo: (seconds: number) => void;
  setVolume: (volume: number) => void;
  toggleMute: () => void;
  setPlayMode: (mode: PlayMode) => void;
  cyclePlayMode: () => void;
  setQuality: (quality: QualityPreference) => void;
  setAutomixEnabled: (enabled: boolean) => void;
  setLyricOffsetSec: (offsetSec: number) => void;
  toggleDesktopLyrics: (enabled?: boolean) => Promise<void>;
  importLocalFiles: (files?: FileList | File[]) => Promise<SongRecord[]>;
  loadLocalLibrary: () => Promise<SongRecord[]>;
  loadPlaylistTracks: (playlist: PlaylistSummary) => Promise<SongRecord[]>;
  updateFx: (patch: Partial<FxSettings>) => void;
  resetFxDefaults: () => void;
  applyVisualPreset: (presetId: number) => void;
  setSearchMode: (mode: SearchMode) => void;
  setSearchQuery: (query: string) => void;
  setDiyMode: (diy: boolean) => void;
  setShelfOpen: (open: boolean) => void;
  setFxPanelOpen: (open: boolean) => void;
  setPlaylistPanelOpen: (open: boolean) => void;
  setSplashDone: (done: boolean) => void;
  checkUpdates: () => Promise<UpdateCheckResult | null>;
  refreshHomeDiscover: () => Promise<void>;
  refreshLoginStatuses: () => Promise<void>;
}

let appInitialized = false;

export const usePlayerStore = create<PlayerState>()((set, get) => ({
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
  homeDiscover: null,
  weatherRadio: null,
  todayListeningStats: {
    date: todayDateKey(),
    totalSeconds: 0,
    playCount: 0,
  },

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
          set({
            currentTime,
            duration: duration || st.duration,
            activeLyricIdx: nextActiveIdx,
          });
          visualEngine.setLyrics(st.lyrics, nextActiveIdx, currentTime);
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
                currentTime,
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
        onCuefieldStatus: (cuefieldStatus) => {
          set({ cuefieldStatus });
        },
        onCuefieldHandoff: (nextSong, nextIndex) => {
          set({
            currentSong: nextSong,
            currentIdx: nextIndex,
            currentTime: 0,
            playing: true,
          });
          visualEngine.setCover(nextSong.cover || '');
          visualEngine.setPlaying(true);
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
          return {
            currentSong: st.currentSong,
            currentIndex: st.currentIdx,
            nextSong: st.playQueue[nextIdx] || null,
            nextIndex: nextIdx,
            quality: st.quality,
          };
        },
      });

      desktopWindow.onDesktopLyricsLockState((payload) => {
        set({ desktopLyricsLocked: Boolean(payload?.locked) });
      });
      desktopWindow.onDesktopLyricsEnabledState((payload) => {
        set({ desktopLyricsEnabled: Boolean(payload?.enabled) });
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

    visualEngine.syncFx(mergedFx);
    const shelfItems = buildShelfItemsFromState(get());
    visualEngine.setShelfItems(shelfItems, get().shelfOpen);

    // 非阻塞加载首页推荐与更新检查
    get().refreshHomeDiscover().catch(() => {});
    get().checkUpdates().catch(() => {});
    if (gateRes?.unlocked) {
      get().refreshLoginStatuses().catch(() => {});
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
        data?: { songs?: SongRecord[] };
      }>(endpoint, { timeoutMs: 12000 });

      const list =
        res.songs || res.tracks || res.results || res.data?.songs || [];
      set({ searchResults: list, searchLoading: false });
      return list;
    } catch {
      set({ searchResults: [], searchLoading: false });
      return [];
    }
  },

  playSong: async (songOrIndex: SongRecord | number, queue?: SongRecord[]) => {
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
    set((prev) => ({
      playQueue: Array.isArray(queue) ? queue : currentQueue,
      currentIdx: targetIdx,
      currentSong: targetSong,
      currentTime: 0,
      duration: Number(targetSong!.duration) || 0,
      audioError: null,
      lyrics: [],
      activeLyricIdx: -1,
      todayListeningStats: {
        ...prev.todayListeningStats,
        playCount: prev.todayListeningStats.playCount + 1,
      },
    }));

    visualEngine.setCover(targetSong.cover || '');
    visualEngine.setLyrics([], -1, 0);
    visualEngine.setShelfItems(buildShelfItemsFromState(get()), get().shelfOpen);

    try {
      await audioEngine.playTrack(targetSong, get().quality, true);
      set({ playing: true, audioError: null });
      visualEngine.setPlaying(true);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      set({ playing: false, audioError: msg });
      visualEngine.setPlaying(false);
    }

    // 异步拉取并解析歌词
    const key = songIdentityKey(targetSong);
    try {
      let lrcPayload: { lyric?: string; tlyric?: string; romalrc?: string } | null = null;
      if (targetSong.lyric) {
        lrcPayload = {
          lyric: targetSong.lyric,
          tlyric: targetSong.tlyric,
          romalrc: targetSong.romalrc,
        };
      } else if (targetSong.provider === 'local' && targetSong.localFileId) {
        const res = await desktopWindow.readLocalMusicLyric(targetSong.localFileId);
        if (res.ok && res.lyric) lrcPayload = res;
      } else if (key) {
        const cached = await desktopWindow.readLyricCache(key).catch(() => null);
        if (cached?.ok && cached.payload && typeof cached.payload === 'object') {
          lrcPayload = cached.payload as { lyric?: string; tlyric?: string; romalrc?: string };
        } else {
          const provider = String(targetSong.provider || 'netease').toLowerCase();
          const lyricUrl =
            provider === 'qq'
              ? `/api/qq/lyric?mid=${encodeURIComponent(String(targetSong.mid || targetSong.id || ''))}`
              : provider === 'kugou'
              ? `/api/kugou/lyric?hash=${encodeURIComponent(String(targetSong.hash || targetSong.id || ''))}`
              : provider === 'qishui'
              ? `/api/qishui/lyric?id=${encodeURIComponent(String(targetSong.id || ''))}`
              : `/api/lyric?id=${encodeURIComponent(String(targetSong.id || ''))}`;
          lrcPayload = await apiJson(lyricUrl, { timeoutMs: 5000 }).catch(() => null);
          if (lrcPayload?.lyric) {
            desktopWindow.writeLyricCache(key, lrcPayload).catch(() => {});
          }
        }
      }

      if (get().currentSong && songIdentityKey(get().currentSong) === key && lrcPayload?.lyric) {
        const parsed = parseLrcText(lrcPayload.lyric, lrcPayload.tlyric, lrcPayload.romalrc);
        const activeIdx = findActiveLyricIndex(parsed, get().currentTime, get().lyricOffsetSec);
        set({ lyrics: parsed, activeLyricIdx: activeIdx });
        visualEngine.setLyrics(parsed, activeIdx, get().currentTime);
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
    visualEngine.setLyrics(st.lyrics, activeIdx, nextTime);
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
    set({ quality });
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
    visualEngine.setLyrics(st.lyrics, activeIdx, st.currentTime);
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
    if (playlist.provider === 'mineradio') {
      const res = await desktopWindow.readBuiltInPlaylist(String(playlist.id), {
        offset: 0,
        limit: 500,
      });
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
    visualEngine.syncFx(nextFx);
    desktopWindow.saveCurrentFxAutosave(nextFx).catch(() => {});
  },

  resetFxDefaults: () => {
    const nextFx: FxSettings = { ...DEFAULT_FX_SETTINGS };
    set({
      fx: nextFx,
      activePreset: nextFx.preset,
    });
    visualEngine.syncFx(nextFx);
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
      const info = await apiJson<UpdateCheckResult>('/api/update/latest', { timeoutMs: 8000 });
      set({ updateInfo: info });
      return info;
    } catch {
      return null;
    }
  },

  refreshHomeDiscover: async () => {
    try {
      const [discover, weather] = await Promise.all([
        apiJson<HomeDiscoverData>('/api/discover?limit=12', { timeoutMs: 8000 }).catch(() => null),
        apiJson<WeatherRadioData>('/api/weather-radio', { timeoutMs: 6000 }).catch(() => null),
      ]);
      set({
        homeDiscover: discover,
        weatherRadio: weather,
      });
    } catch {
      // ignore offline
    }
  },

  refreshLoginStatuses: async () => {
    try {
      const status = await apiJson<Partial<LoginStatusMap>>('/api/login/status', {
        timeoutMs: 6000,
      });
      set({
        platformLogins: {
          netease: status.netease || { loggedIn: false },
          qq: status.qq || { loggedIn: false },
          kugou: status.kugou || { loggedIn: false },
          qishui: status.qishui || { loggedIn: false },
        },
      });
    } catch {
      // ignore when locked or offline
    }
  },
}));
