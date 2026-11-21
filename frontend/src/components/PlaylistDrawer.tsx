import React, { useRef, useState } from 'react';
import { apiJson, coverProxyUrl, desktopWindow } from '../bridge/tauriBridge';
import { songIdentityKey } from '../engines';
import { usePlayerStore } from '../store/playerStore';
import type { PlayMode, PlaylistSummary, SongRecord } from '../types/mineradio';

export interface PlaylistDrawerProps {
  onToast?: (message: string) => void;
  collectTargetSong?: SongRecord | null;
  onCloseCollect?: () => void;
  onSongSelected?: (song: SongRecord) => void;
}

type DrawerTab = 'queue' | 'playlists' | 'podcast' | 'local';

interface MyPodcast {
  id: string | number;
  key?: string;
  name: string;
  cover?: string;
  sub?: string;
  itemType?: string;
  count?: number;
}

function pickList(res: Record<string, unknown>, keys: string[]): Array<Record<string, unknown>> {
  for (const k of keys) {
    const v = res[k];
    if (Array.isArray(v)) return v as Array<Record<string, unknown>>;
  }
  const data = res.data;
  if (Array.isArray(data)) return data as Array<Record<string, unknown>>;
  if (data && typeof data === 'object') return pickList(data as Record<string, unknown>, keys);
  return [];
}

const PLAY_MODE_LABELS: Record<PlayMode, string> = {
  loop: '顺序循环',
  single: '单曲循环',
  shuffle: '随机播放',
};

export function PlaylistDrawer({
  onToast,
  collectTargetSong,
  onCloseCollect,
  onSongSelected,
}: PlaylistDrawerProps): React.ReactElement {
  const playlistPanelOpen = usePlayerStore((s) => s.playlistPanelOpen);
  const setPlaylistPanelOpen = usePlayerStore((s) => s.setPlaylistPanelOpen);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const currentIdx = usePlayerStore((s) => s.currentIdx);
  const currentSong = usePlayerStore((s) => s.currentSong);
  const playMode = usePlayerStore((s) => s.playMode);
  const cyclePlayMode = usePlayerStore((s) => s.cyclePlayMode);
  const playSong = usePlayerStore((s) => s.playSong);
  const builtInPlaylists = usePlayerStore((s) => s.builtInPlaylists);
  const userPlaylists = usePlayerStore((s) => s.userPlaylists);
  const localTracks = usePlayerStore((s) => s.localTracks);
  const activePlaylistTracks = usePlayerStore((s) => s.activePlaylistTracks);
  const loadPlaylistTracks = usePlayerStore((s) => s.loadPlaylistTracks);
  const importLocalFiles = usePlayerStore((s) => s.importLocalFiles);
  const loadLocalLibrary = usePlayerStore((s) => s.loadLocalLibrary);
  const fx = usePlayerStore((s) => s.fx);
  const updateFx = usePlayerStore((s) => s.updateFx);
  const homeDiscover = usePlayerStore((s) => s.homeDiscover);
  const refreshAllUserPlaylists = usePlayerStore((s) => s.refreshAllUserPlaylists);
  const [myPodcasts, setMyPodcasts] = useState<MyPodcast[]>([]);
  const [podcastLoading, setPodcastLoading] = useState(false);
  const [openPodcastId, setOpenPodcastId] = useState<string | number | null>(null);
  const [podcastItems, setPodcastItems] = useState<SongRecord[]>([]);
  const [podcastItemsType, setPodcastItemsType] = useState<'radio' | 'program'>('program');
  const [podcastItemsLoading, setPodcastItemsLoading] = useState(false);
  const [openRadioId, setOpenRadioId] = useState<string | number | null>(null);
  const [radioPrograms, setRadioPrograms] = useState<SongRecord[]>([]);
  const [radioProgramsLoading, setRadioProgramsLoading] = useState(false);

  const [activeTab, setActiveTab] = useState<DrawerTab>('queue');
  const [newPlaylistName, setNewPlaylistName] = useState('');
  const [creatingPlaylist, setCreatingPlaylist] = useState(false);
  const [expandedKey, setExpandedKey] = useState<string>('');
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingName, setEditingName] = useState('');
  const [loadingPlKey, setLoadingPlKey] = useState<string>('');
  const [collectNewName, setCollectNewName] = useState('');

  const localInputRef = useRef<HTMLInputElement | null>(null);

  const refreshBuiltInPlaylists = async () => {
    const res = await desktopWindow.listBuiltInPlaylists().catch(() => ({
      ok: false,
      playlists: [] as PlaylistSummary[],
    }));
    if (res.ok) {
      usePlayerStore.setState({ builtInPlaylists: res.playlists || [] });
    }
  };

  const refreshCrossPlatformPlaylists = async () => {
    await refreshBuiltInPlaylists();
    try {
      await refreshAllUserPlaylists();
    } catch {
      // ignore if unauthenticated or offline
    }
    onToast?.('歌单列表已刷新');
  };

  const loadMyPodcasts = async () => {
    setPodcastLoading(true);
    try {
      const res = await apiJson<Record<string, unknown>>('/api/podcast/my', { timeoutMs: 8000 });
      const list = pickList(res || {}, [
        'collections',
        'podcasts',
        'radios',
        'djRadios',
        'list',
        'items',
      ]);
      setMyPodcasts(
        list
          .map((raw): MyPodcast | null => {
            const keyStr = typeof raw.key === 'string' ? raw.key.trim() : '';
            const id = (keyStr || raw.id || raw.radioId || raw.podcastId) as
              | string
              | number
              | undefined;
            if (id === undefined || id === null || id === '') return null;
            const countNum =
              typeof raw.count === 'number' && Number.isFinite(raw.count) ? raw.count : undefined;
            return {
              id,
              key: keyStr || undefined,
              name: String(raw.title || raw.name || '未命名播客分类'),
              cover: String(raw.cover || raw.picUrl || ''),
              sub: String(raw.sub || raw.dj || raw.artist || raw.desc || raw.description || ''),
              itemType: typeof raw.itemType === 'string' ? raw.itemType : undefined,
              count: countNum,
            };
          })
          .filter((x): x is MyPodcast => Boolean(x))
      );
    } catch {
      setMyPodcasts([]);
      onToast?.('读取我的播客失败，请先登录网易云');
    } finally {
      setPodcastLoading(false);
    }
  };

  const toggleMyPodcast = async (pod: MyPodcast) => {
    if (openPodcastId === pod.id) {
      setOpenPodcastId(null);
      setPodcastItems([]);
      setOpenRadioId(null);
      setRadioPrograms([]);
      return;
    }
    setOpenPodcastId(pod.id);
    setPodcastItems([]);
    setOpenRadioId(null);
    setRadioPrograms([]);
    setPodcastItemsLoading(true);
    try {
      const queryUrl = pod.key
        ? `/api/podcast/my/items?key=${encodeURIComponent(pod.key)}&limit=40`
        : `/api/podcast/my/items?id=${encodeURIComponent(String(pod.id))}&limit=40`;
      const res = await apiJson<Record<string, unknown>>(queryUrl, { timeoutMs: 10000 });
      const itemTypeRaw = String(res?.itemType || pod.itemType || '').toLowerCase();
      setPodcastItemsType(itemTypeRaw === 'radio' ? 'radio' : 'program');
      const list = pickList(res || {}, [
        'items',
        'programs',
        'songs',
        'tracks',
        'list',
      ]) as unknown as Array<Record<string, unknown>>;
      setPodcastItems(
        list.map((raw) => {
          const itemId = (raw.id ?? raw.programId ?? raw.radioId ?? raw.rid) as
            | string
            | number
            | undefined;
          const isRadio = itemTypeRaw === 'radio';
          return {
            ...(raw as Partial<SongRecord>),
            id: itemId,
            rid: isRadio ? itemId : undefined,
            radioId: isRadio ? itemId : undefined,
            name: String(raw.name || raw.title || '未知节目'),
            artist: String(raw.artist || raw.djName || pod.name),
            provider: (raw.provider as SongRecord['provider']) || 'netease',
            type: isRadio ? 'podcast-radio' : 'podcast',
            podcastId: (raw.podcastId as string | number | undefined) ?? itemId,
            cover: String(raw.cover || raw.picUrl || pod.cover || ''),
          };
        })
      );
    } catch {
      onToast?.('读取节目失败');
    } finally {
      setPodcastItemsLoading(false);
    }
  };

  const toggleCollectionRadio = async (radio: SongRecord) => {
    const rid = radio.id ?? radio.rid ?? radio.radioId;
    if (rid === undefined || rid === null) return;
    if (openRadioId === rid) {
      setOpenRadioId(null);
      setRadioPrograms([]);
      return;
    }
    setOpenRadioId(rid);
    setRadioPrograms([]);
    setRadioProgramsLoading(true);
    try {
      const res = await apiJson<Record<string, unknown>>(
        `/api/podcast/radio/programs?id=${encodeURIComponent(String(rid))}&limit=30`,
        { timeoutMs: 10000 }
      );
      const list = pickList(res || {}, ['programs', 'songs', 'tracks', 'items', 'list']) as unknown as SongRecord[];
      setRadioPrograms(
        list.map((p) => ({
          ...p,
          provider: p.provider || 'netease',
          type: 'podcast',
          podcastId: p.podcastId ?? rid,
          cover: p.cover || radio.cover,
        }))
      );
    } catch {
      onToast?.('读取电台节目失败');
    } finally {
      setRadioProgramsLoading(false);
    }
  };

  const handleReorderTrack = async (
    playlistId: string,
    from: number,
    to: number,
    e: React.MouseEvent
  ) => {
    e.stopPropagation();
    if (to < 0 || to >= activePlaylistTracks.length) return;
    const res = await desktopWindow.reorderBuiltInPlaylistTrack(playlistId, from, to);
    if (res.ok) {
      const target = usePlayerStore
        .getState()
        .builtInPlaylists.find((p) => String(p.id) === playlistId);
      if (target) await loadPlaylistTracks(target);
    } else {
      onToast?.(res.error || '调整顺序失败');
    }
  };

  const handleSetNext = (idx: number, e: React.MouseEvent) => {
    e.stopPropagation();
    const st = usePlayerStore.getState();
    if (idx === st.currentIdx || idx < 0 || idx >= st.playQueue.length) return;
    const queue = [...st.playQueue];
    const [moved] = queue.splice(idx, 1);
    const curKey = st.currentSong ? songIdentityKey(st.currentSong) : '';
    let curPos = curKey ? queue.findIndex((q) => songIdentityKey(q) === curKey) : st.currentIdx;
    if (curPos < 0) curPos = Math.min(st.currentIdx, queue.length - 1);
    queue.splice(curPos + 1, 0, moved);
    usePlayerStore.setState({ playQueue: queue, currentIdx: curPos });
    onToast?.(`已设为下一首：${moved.name}`);
  };

  const handleCreateBuiltIn = async (nameRaw: string) => {
    const name = nameRaw.trim() || '新建歌单';
    const res = await desktopWindow.createBuiltInPlaylist(name);
    if (res.ok) {
      setNewPlaylistName('');
      setCreatingPlaylist(false);
      await refreshBuiltInPlaylists();
      onToast?.(`已创建内置歌单「${res.playlist?.name || name}」`);
      return res.playlist;
    }
    onToast?.(res.error || '创建歌单失败');
    return undefined;
  };

  const handleRenameBuiltIn = async (id: string) => {
    const name = editingName.trim();
    if (!name) {
      setEditingId(null);
      return;
    }
    const res = await desktopWindow.renameBuiltInPlaylist(id, name);
    setEditingId(null);
    if (res.ok) {
      await refreshBuiltInPlaylists();
      onToast?.(`已重命名为「${name}」`);
    } else {
      onToast?.(res.error || '重命名失败');
    }
  };

  const handleDeleteBuiltIn = async (pl: PlaylistSummary, e: React.MouseEvent) => {
    e.stopPropagation();
    const res = await desktopWindow.deleteBuiltInPlaylist(String(pl.id));
    if (res.ok) {
      if (expandedKey === `mineradio:${pl.id}`) {
        setExpandedKey('');
      }
      await refreshBuiltInPlaylists();
      onToast?.(`已删除歌单「${pl.name}」`);
    } else {
      onToast?.(res.error || '删除歌单失败');
    }
  };

  const handleAddTrackToBuiltIn = async (
    playlistId: string,
    track: SongRecord,
    e?: React.MouseEvent
  ) => {
    e?.stopPropagation();
    const res = await desktopWindow.addBuiltInPlaylistTrack(playlistId, track);
    if (res.ok) {
      await refreshBuiltInPlaylists();
      if (expandedKey === `mineradio:${playlistId}`) {
        const target = usePlayerStore
          .getState()
          .builtInPlaylists.find((p) => String(p.id) === playlistId);
        if (target) await loadPlaylistTracks(target);
      }
      onToast?.(res.duplicate ? '歌曲已在歌单中' : `已添加到歌单：${track.name}`);
    } else {
      onToast?.(res.error || '添加失败');
    }
  };

  const handleRemoveTrackFromBuiltIn = async (
    playlistId: string,
    index: number,
    e: React.MouseEvent
  ) => {
    e.stopPropagation();
    const res = await desktopWindow.removeBuiltInPlaylistTrack(playlistId, index);
    if (res.ok) {
      await refreshBuiltInPlaylists();
      const target = usePlayerStore
        .getState()
        .builtInPlaylists.find((p) => String(p.id) === playlistId);
      if (target) await loadPlaylistTracks(target);
      onToast?.('已从歌单移除曲目');
    }
  };

  const handleToggleExpandPlaylist = async (pl: PlaylistSummary) => {
    const key = `${pl.provider}:${pl.id}`;
    if (expandedKey === key) {
      setExpandedKey('');
      return;
    }
    setExpandedKey(key);
    setLoadingPlKey(key);
    try {
      await loadPlaylistTracks(pl);
    } finally {
      setLoadingPlKey('');
    }
  };

  const handlePlayQueueIdx = (idx: number) => {
    const song = playQueue[idx];
    if (song) onSongSelected?.(song);
    playSong(idx).catch(() => {});
  };

  const handleRemoveQueueIdx = (idx: number, e: React.MouseEvent) => {
    e.stopPropagation();
    const nextQueue = playQueue.filter((_, i) => i !== idx);
    let nextIdx = currentIdx;
    if (idx < currentIdx) nextIdx = currentIdx - 1;
    else if (idx === currentIdx) nextIdx = Math.min(idx, nextQueue.length - 1);
    usePlayerStore.setState({
      playQueue: nextQueue,
      currentIdx: nextQueue.length === 0 ? -1 : nextIdx,
    });
  };

  const handleClearQueue = () => {
    usePlayerStore.setState({
      playQueue: [],
      currentIdx: -1,
    });
    onToast?.('播放队列已清空');
  };

  const handleShuffleQueue = () => {
    if (playQueue.length < 2) return;
    const copy = [...playQueue];
    for (let i = copy.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [copy[i], copy[j]] = [copy[j], copy[i]];
    }
    const curKey = currentSong ? songIdentityKey(currentSong) : '';
    const nextIdx = curKey ? copy.findIndex((s) => songIdentityKey(s) === curKey) : 0;
    usePlayerStore.setState({
      playQueue: copy,
      currentIdx: nextIdx >= 0 ? nextIdx : 0,
    });
    onToast?.('已随机打乱当前队列');
  };

  const handleChooseLocalImport = async () => {
    const chosen = await desktopWindow.chooseLocalMusicFiles();
    if (chosen.ok && chosen.files?.length) {
      const imported = await importLocalFiles();
      if (imported.length > 0) {
        onToast?.(`已导入 ${imported.length} 首本地音乐`);
        return;
      }
    }
    if (!chosen.canceled && localInputRef.current) {
      localInputRef.current.click();
    }
  };

  const handleLocalFileChange = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    const res = await desktopWindow.importLocalMusicFiles(files);
    if (res.ok && res.tracks.length > 0) {
      const existing = usePlayerStore.getState().localTracks;
      const seen = new Set(existing.map((t) => songIdentityKey(t)));
      const merged = [
        ...res.tracks.filter((t) => !seen.has(songIdentityKey(t))),
        ...existing,
      ];
      usePlayerStore.setState({ localTracks: merged });
      onToast?.(`已导入 ${res.tracks.length} 首本地歌曲`);
    } else {
      onToast?.(res.error || '导入失败');
    }
  };

  const combinedCrossPlatformPlaylists = [
    ...userPlaylists,
    ...(homeDiscover?.playlists || []).filter(
      (hp) => !userPlaylists.some((up) => String(up.id) === String(hp.id) && up.provider === hp.provider)
    ),
  ];

  const isVisible = playlistPanelOpen || fx.shelfPinnedOpen;

  return (
    <>
      <div
        id="playlist-panel"
        className={`${isVisible ? 'show' : ''}${fx.shelfPinnedOpen ? ' pinned' : ''}`.trim()}
        aria-hidden={!isVisible}
      >
        <div className="playlist-panel-sticky">
          <div className="queue-head">
            <div>
              <div className="fx-title">歌单 / 队列</div>
              <div className="fx-sub">QUEUE · {playQueue.length} 首曲目</div>
            </div>
            <div className="queue-head-act">
              <button
                id="playlist-pin-btn"
                className={`fx-mini-btn ghost playlist-pin-btn${fx.shelfPinnedOpen ? ' active' : ''}`}
                type="button"
                onClick={() => updateFx({ shelfPinnedOpen: !fx.shelfPinnedOpen })}
                title="常开歌单"
                aria-label="常开歌单"
              >
                <svg
                  viewBox="0 0 24 24"
                  width="15"
                  height="15"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <path d="M14 4l6 6" />
                  <path d="M5 15l4 4" />
                  <path d="M14 4l-2 5-4 4-3 2 4 4 2-3 4-4 5-2z" />
                  <path d="M9 19l-4 4" />
                </svg>
              </button>
              <button
                className="fx-mini-btn ghost"
                type="button"
                onClick={handleShuffleQueue}
              >
                随机
              </button>
              <button
                className="fx-mini-btn ghost"
                type="button"
                aria-label="关闭歌单面板"
                onClick={() => {
                  updateFx({ shelfPinnedOpen: false });
                  setPlaylistPanelOpen(false);
                }}
              >
                ×
              </button>
            </div>
          </div>

          <div className="panel-tabs" role="tablist" aria-label="歌单与队列标签">
            <button
              id="tab-queue"
              role="tab"
              type="button"
              aria-selected={activeTab === 'queue'}
              className={`panel-tab${activeTab === 'queue' ? ' active' : ''}`}
              onClick={() => setActiveTab('queue')}
            >
              当前队列
            </button>
            <button
              id="tab-pl"
              role="tab"
              type="button"
              aria-selected={activeTab === 'playlists'}
              className={`panel-tab${activeTab === 'playlists' ? ' active' : ''}`}
              onClick={() => setActiveTab('playlists')}
            >
              我的歌单
            </button>
            <button
              id="tab-podcast"
              role="tab"
              type="button"
              aria-selected={activeTab === 'podcast'}
              className={`panel-tab${activeTab === 'podcast' ? ' active' : ''}`}
              onClick={() => {
                setActiveTab('podcast');
                loadMyPodcasts().catch(() => {});
              }}
            >
              我的播客
            </button>
            <button
              id="tab-local"
              role="tab"
              type="button"
              aria-selected={activeTab === 'local'}
              className={`panel-tab${activeTab === 'local' ? ' active' : ''}`}
              onClick={() => {
                setActiveTab('local');
                loadLocalLibrary().catch(() => {});
              }}
            >
              本地曲库
            </button>
          </div>
        </div>

        {activeTab === 'queue' && (
          <div id="queue-pane">
            <div className="queue-toolbar">
              <div id="play-mode-chip" className="queue-chip">
                {PLAY_MODE_LABELS[playMode]}
              </div>
              <div style={{ display: 'flex', gap: 6 }}>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={cyclePlayMode}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  切换模式
                </button>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={handleClearQueue}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  清空
                </button>
              </div>
            </div>

            <div id="queue-list" className="queue-list">
              {playQueue.length === 0 ? (
                <div className="search-empty">当前播放队列为空</div>
              ) : (
                playQueue.map((song, idx) => {
                  const isNow = idx === currentIdx;
                  const coverSrc = song.cover ? coverProxyUrl(song.cover) : '';
                  return (
                    <div
                      key={`${songIdentityKey(song)}-${idx}`}
                      className={`queue-item${isNow ? ' now' : ''}`}
                      data-queue-index={idx}
                      onClick={() => handlePlayQueueIdx(idx)}
                    >
                      {coverSrc ? (
                        <img src={coverSrc} alt={song.name} loading="lazy" />
                      ) : (
                        <div
                          style={{
                            width: 38,
                            height: 38,
                            borderRadius: 6,
                            background: 'rgba(255,255,255,0.08)',
                            display: 'grid',
                            placeItems: 'center',
                            fontSize: 12,
                            flexShrink: 0,
                          }}
                        >
                          ♪
                        </div>
                      )}
                      <div className="qi-info">
                        <div className="qi-name">{song.name || song.title || '未知曲目'}</div>
                        <div className="qi-sub">{song.artist || '未知歌手'}</div>
                      </div>
                      <div className="qi-act">
                        {!isNow && (
                          <button
                            type="button"
                            title="设为下一首"
                            aria-label="设为下一首"
                            onClick={(e) => handleSetNext(idx, e)}
                          >
                            下一首
                          </button>
                        )}
                        <button
                          type="button"
                          title="从队列移除"
                          aria-label="从队列移除"
                          onClick={(e) => handleRemoveQueueIdx(idx, e)}
                        >
                          ×
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        )}

        {activeTab === 'playlists' && (
          <div id="pl-pane">
            <div className="queue-toolbar">
              <div className="queue-chip">内置歌单可混合全部平台</div>
              <div style={{ display: 'flex', gap: 6 }}>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={() => setCreatingPlaylist((v) => !v)}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  + 新建内置
                </button>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={() => {
                    refreshCrossPlatformPlaylists().catch(() => {});
                  }}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  刷新
                </button>
              </div>
            </div>

            {creatingPlaylist && (
              <div style={{ display: 'flex', gap: 6, marginBottom: 10 }}>
                <input
                  type="text"
                  placeholder="输入内置歌单名称"
                  value={newPlaylistName}
                  onChange={(e) => setNewPlaylistName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      handleCreateBuiltIn(newPlaylistName).catch(() => {});
                    }
                  }}
                  style={{
                    flex: 1,
                    height: 30,
                    borderRadius: 8,
                    border: '1px solid rgba(255,255,255,0.14)',
                    background: 'rgba(255,255,255,0.05)',
                    color: '#fff',
                    padding: '0 10px',
                    fontSize: 12,
                  }}
                />
                <button
                  type="button"
                  className="fx-mini-btn"
                  style={{ height: 30, padding: '0 12px' }}
                  onClick={() => {
                    handleCreateBuiltIn(newPlaylistName).catch(() => {});
                  }}
                >
                  创建
                </button>
              </div>
            )}

            <div id="pl-list" style={{ marginTop: 6, display: 'grid', gap: 8 }}>
              {builtInPlaylists.map((pl) => {
                const key = `mineradio:${pl.id}`;
                const expanded = expandedKey === key;
                const isEditing = editingId === String(pl.id);
                return (
                  <div key={key} className={`pl-card${expanded ? ' expanded' : ''}`} style={{ padding: 10 }}>
                    <div
                      style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, cursor: 'pointer' }}
                      onClick={() => {
                        if (!isEditing) handleToggleExpandPlaylist(pl).catch(() => {});
                      }}
                    >
                      <div style={{ minWidth: 0, flex: 1 }}>
                        {isEditing ? (
                          <div
                            style={{ display: 'flex', gap: 6 }}
                            onClick={(e) => e.stopPropagation()}
                          >
                            <input
                              type="text"
                              value={editingName}
                              onChange={(e) => setEditingName(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter') handleRenameBuiltIn(String(pl.id)).catch(() => {});
                                if (e.key === 'Escape') setEditingId(null);
                              }}
                              style={{
                                flex: 1,
                                height: 26,
                                borderRadius: 6,
                                border: '1px solid rgba(255,255,255,0.2)',
                                background: 'rgba(0,0,0,0.4)',
                                color: '#fff',
                                padding: '0 8px',
                                fontSize: 12,
                              }}
                            />
                            <button
                              type="button"
                              className="fx-mini-btn"
                              onClick={() => {
                                handleRenameBuiltIn(String(pl.id)).catch(() => {});
                              }}
                            >
                              保存
                            </button>
                          </div>
                        ) : (
                          <>
                            <div className="qi-name">{pl.name}</div>
                            <div className="qi-sub">内置跨平台歌单 · {pl.trackCount || 0} 首</div>
                          </>
                        )}
                      </div>
                      <div style={{ display: 'flex', gap: 4, flexShrink: 0 }}>
                        {currentSong && (
                          <button
                            type="button"
                            className="fx-mini-btn ghost"
                            title="将当前歌曲加入此歌单"
                            onClick={(e) => {
                              handleAddTrackToBuiltIn(String(pl.id), currentSong, e).catch(() => {});
                            }}
                          >
                            +当前
                          </button>
                        )}
                        <button
                          type="button"
                          className="fx-mini-btn ghost"
                          title="重命名"
                          onClick={(e) => {
                            e.stopPropagation();
                            setEditingId(String(pl.id));
                            setEditingName(pl.name);
                          }}
                        >
                          改名
                        </button>
                        <button
                          type="button"
                          className="fx-mini-btn ghost"
                          title="删除歌单"
                          onClick={(e) => {
                            handleDeleteBuiltIn(pl, e).catch(() => {});
                          }}
                        >
                          删除
                        </button>
                      </div>
                    </div>

                    {expanded && (
                      <div style={{ marginTop: 10, display: 'grid', gap: 6 }}>
                        {loadingPlKey === key ? (
                          <div className="qi-sub">加载曲目中...</div>
                        ) : activePlaylistTracks.length === 0 ? (
                          <div className="qi-sub">歌单暂无曲目，可点击“+当前”添加歌曲</div>
                        ) : (
                          <>
                            <button
                              type="button"
                              className="fx-mini-btn"
                              style={{ justifySelf: 'start', marginBottom: 4 }}
                              onClick={() => {
                                onSongSelected?.(activePlaylistTracks[0]);
                                playSong(0, activePlaylistTracks).catch(() => {});
                              }}
                            >
                              播放全部 ({activePlaylistTracks.length})
                            </button>
                            {activePlaylistTracks.map((track, idx) => (
                              <div
                                key={`${songIdentityKey(track)}-${idx}`}
                                className="queue-item"
                                onClick={() => {
                                  onSongSelected?.(track);
                                  playSong(idx, activePlaylistTracks).catch(() => {});
                                }}
                              >
                                <div className="qi-info">
                                  <div className="qi-name">{track.name || track.title}</div>
                                  <div className="qi-sub">{track.artist || '未知歌手'}</div>
                                </div>
                                <div className="qi-act">
                                  <button
                                    type="button"
                                    title="上移"
                                    disabled={idx === 0}
                                    onClick={(e) => {
                                      handleReorderTrack(String(pl.id), idx, idx - 1, e).catch(() => {});
                                    }}
                                  >
                                    ↑
                                  </button>
                                  <button
                                    type="button"
                                    title="下移"
                                    disabled={idx === activePlaylistTracks.length - 1}
                                    onClick={(e) => {
                                      handleReorderTrack(String(pl.id), idx, idx + 1, e).catch(() => {});
                                    }}
                                  >
                                    ↓
                                  </button>
                                  <button
                                    type="button"
                                    title="从歌单移除"
                                    onClick={(e) => {
                                      handleRemoveTrackFromBuiltIn(String(pl.id), idx, e).catch(() => {});
                                    }}
                                  >
                                    ×
                                  </button>
                                </div>
                              </div>
                            ))}
                          </>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

              {combinedCrossPlatformPlaylists.map((pl) => {
                const key = `${pl.provider}:${pl.id}`;
                const expanded = expandedKey === key;
                return (
                  <div key={key} className={`pl-card${expanded ? ' expanded' : ''}`} style={{ padding: 10 }}>
                    <div
                      style={{ display: 'flex', alignItems: 'center', gap: 10, cursor: 'pointer' }}
                      onClick={() => {
                        handleToggleExpandPlaylist(pl).catch(() => {});
                      }}
                    >
                      {pl.cover && <img src={coverProxyUrl(pl.cover)} alt={pl.name} />}
                      <div style={{ minWidth: 0, flex: 1 }}>
                        <div className="qi-name">{pl.name}</div>
                        <div className="qi-sub">
                          {pl.provider.toUpperCase()} · {pl.trackCount || 0} 首
                        </div>
                      </div>
                    </div>
                    {expanded && (
                      <div style={{ marginTop: 10, display: 'grid', gap: 6 }}>
                        {loadingPlKey === key ? (
                          <div className="qi-sub">加载曲目中...</div>
                        ) : activePlaylistTracks.length === 0 ? (
                          <div className="qi-sub">暂无曲目</div>
                        ) : (
                          activePlaylistTracks.map((track, idx) => (
                            <div
                              key={`${songIdentityKey(track)}-${idx}`}
                              className="queue-item"
                              onClick={() => {
                                onSongSelected?.(track);
                                playSong(idx, activePlaylistTracks).catch(() => {});
                              }}
                            >
                              <div className="qi-info">
                                <div className="qi-name">{track.name || track.title}</div>
                                <div className="qi-sub">{track.artist || '未知歌手'}</div>
                              </div>
                            </div>
                          ))
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

              {builtInPlaylists.length === 0 && combinedCrossPlatformPlaylists.length === 0 && (
                <div className="search-empty">暂无歌单，点击“+ 新建内置”创建跨平台歌单</div>
              )}
            </div>
          </div>
        )}

        {activeTab === 'podcast' && (
          <div id="podcast-pane">
            <div className="queue-toolbar">
              <div className="queue-chip">我的播客 · {myPodcasts.length} 个</div>
              <button
                className="fx-mini-btn ghost"
                type="button"
                onClick={() => {
                  loadMyPodcasts().catch(() => {});
                }}
                style={{ height: 26, padding: '0 10px', fontSize: 11 }}
              >
                刷新
              </button>
            </div>
            <div className="queue-list">
              {podcastLoading ? (
                <div className="search-empty">加载中...</div>
              ) : myPodcasts.length === 0 ? (
                <div className="search-empty">暂无订阅播客，请先登录网易云</div>
              ) : (
                myPodcasts.map((pod) => (
                  <div key={String(pod.id)} className="pl-card" style={{ padding: 10 }}>
                    <div
                      style={{ display: 'flex', alignItems: 'center', gap: 10, cursor: 'pointer' }}
                      onClick={() => {
                        toggleMyPodcast(pod).catch(() => {});
                      }}
                    >
                      {pod.cover && <img src={coverProxyUrl(pod.cover)} alt={pod.name} />}
                      <div style={{ minWidth: 0, flex: 1 }}>
                        <div className="qi-name">
                          {pod.name}
                          {typeof pod.count === 'number' ? ` (${pod.count})` : ''}
                        </div>
                        <div className="qi-sub">{pod.sub || '播客'}</div>
                      </div>
                    </div>
                    {openPodcastId === pod.id && (
                      <div style={{ marginTop: 10, display: 'grid', gap: 6 }}>
                        {podcastItemsLoading ? (
                          <div className="qi-sub">加载节目中...</div>
                        ) : podcastItems.length === 0 ? (
                          <div className="qi-sub">暂无节目</div>
                        ) : (
                          podcastItems.map((item, idx) => {
                            const isRadio = podcastItemsType === 'radio' || item.type === 'podcast-radio';
                            const rid = item.id ?? item.rid ?? idx;
                            return (
                              <div key={`${songIdentityKey(item)}-${idx}`}>
                                <div
                                  className="queue-item"
                                  onClick={() => {
                                    if (isRadio) {
                                      toggleCollectionRadio(item).catch(() => {});
                                    } else {
                                      onSongSelected?.(item);
                                      playSong(idx, podcastItems).catch(() => {});
                                    }
                                  }}
                                >
                                  <div className="qi-info">
                                    <div className="qi-name">{item.name || item.title}</div>
                                    <div className="qi-sub">
                                      {isRadio ? `${item.artist || pod.name} · 点击展开节目` : item.artist || pod.name}
                                    </div>
                                  </div>
                                </div>
                                {isRadio && openRadioId === rid && (
                                  <div style={{ paddingLeft: 12, marginTop: 4, display: 'grid', gap: 4 }}>
                                    {radioProgramsLoading ? (
                                      <div className="qi-sub">加载电台节目中...</div>
                                    ) : radioPrograms.length === 0 ? (
                                      <div className="qi-sub">该电台暂无可播节目</div>
                                    ) : (
                                      radioPrograms.map((prog, pIdx) => (
                                        <div
                                          key={`${songIdentityKey(prog)}-${pIdx}`}
                                          className="queue-item"
                                          onClick={() => {
                                            onSongSelected?.(prog);
                                            playSong(pIdx, radioPrograms).catch(() => {});
                                          }}
                                        >
                                          <div className="qi-info">
                                            <div className="qi-name">{prog.name || prog.title}</div>
                                            <div className="qi-sub">{prog.artist || item.name}</div>
                                          </div>
                                        </div>
                                      ))
                                    )}
                                  </div>
                                )}
                              </div>
                            );
                          })
                        )}
                      </div>
                    )}
                  </div>
                ))
              )}
            </div>
          </div>
        )}

        {activeTab === 'local' && (
          <div id="local-pane">
            <div className="queue-toolbar">
              <div className="queue-chip">本地音频 · {localTracks.length} 首</div>
              <div style={{ display: 'flex', gap: 6 }}>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={() => {
                    handleChooseLocalImport().catch(() => {});
                  }}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  导入文件
                </button>
                <button
                  className="fx-mini-btn ghost"
                  type="button"
                  onClick={() => {
                    loadLocalLibrary().catch(() => {});
                  }}
                  style={{ height: 26, padding: '0 10px', fontSize: 11 }}
                >
                  刷新
                </button>
              </div>
            </div>

            <input
              ref={localInputRef}
              type="file"
              accept=".mp3,.flac,.wav,.ogg,.m4a,.aac,.opus,audio/*"
              multiple
              style={{ display: 'none' }}
              onChange={(e) => {
                handleLocalFileChange(e.target.files).catch(() => {});
                e.target.value = '';
              }}
            />

            <div className="queue-list">
              {localTracks.length === 0 ? (
                <div className="search-empty">本地曲库为空，点击“导入文件”添加音频</div>
              ) : (
                localTracks.map((track, idx) => (
                  <div
                    key={songIdentityKey(track) || idx}
                    className="queue-item"
                    onClick={() => {
                      onSongSelected?.(track);
                      playSong(idx, localTracks).catch(() => {});
                    }}
                  >
                    <div
                      style={{
                        width: 38,
                        height: 38,
                        borderRadius: 6,
                        background: 'rgba(255,255,255,0.08)',
                        display: 'grid',
                        placeItems: 'center',
                        fontSize: 12,
                        flexShrink: 0,
                      }}
                    >
                      ♪
                    </div>
                    <div className="qi-info">
                      <div className="qi-name">{track.name || track.title}</div>
                      <div className="qi-sub">{track.artist || '本地音频'}</div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        )}
      </div>

      {/* 收藏到内置歌单模态 */}
      <div
        id="collect-modal"
        className={`modal-mask${collectTargetSong ? ' show' : ''}`}
        onClick={(e) => {
          if (e.target === e.currentTarget) onCloseCollect?.();
        }}
      >
        <div className="modal collect-modal">
          <h2>收藏到歌单</h2>
          {collectTargetSong && (
            <div id="collect-current" className="collect-current">
              {collectTargetSong.cover ? (
                <img src={coverProxyUrl(collectTargetSong.cover)} alt={collectTargetSong.name} />
              ) : (
                <div className="cover-placeholder">♪</div>
              )}
              <div>
                <div className="collect-title">{collectTargetSong.name}</div>
                <div className="collect-sub">{collectTargetSong.artist || '未知歌手'}</div>
              </div>
            </div>
          )}

          <div className="collect-create">
            <input
              id="collect-new-name"
              type="text"
              placeholder="新建 Mineradio 内置歌单"
              autoComplete="off"
              maxLength={40}
              value={collectNewName}
              onChange={(e) => setCollectNewName(e.target.value)}
            />
            <button
              className="modal-btn primary"
              type="button"
              onClick={async () => {
                const created = await handleCreateBuiltIn(collectNewName);
                setCollectNewName('');
                if (created && collectTargetSong) {
                  await handleAddTrackToBuiltIn(String(created.id), collectTargetSong);
                  onCloseCollect?.();
                }
              }}
            >
              创建内置
            </button>
          </div>

          <div id="collect-list" className="collect-list">
            {builtInPlaylists.length === 0 ? (
              <div className="collect-empty">暂无内置歌单，上方可直接新建</div>
            ) : (
              builtInPlaylists.map((pl) => (
                <button
                  key={pl.id}
                  type="button"
                  className="collect-item"
                  onClick={async () => {
                    if (collectTargetSong) {
                      await handleAddTrackToBuiltIn(String(pl.id), collectTargetSong);
                      onCloseCollect?.();
                    }
                  }}
                >
                  <div className="cover-placeholder built-in">MR</div>
                  <div style={{ textAlign: 'left', flex: 1 }}>
                    <div className="collect-title">{pl.name}</div>
                    <div className="collect-sub">{pl.trackCount || 0} 首</div>
                  </div>
                </button>
              ))
            )}
          </div>

          <div className="btn-row">
            <button className="modal-btn" type="button" onClick={() => onCloseCollect?.()}>
              关闭
            </button>
          </div>
        </div>
      </div>
    </>
  );
}

export default PlaylistDrawer;
