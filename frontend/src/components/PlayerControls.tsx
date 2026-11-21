import React, { useEffect, useRef, useState } from 'react';
import { apiJson, coverProxyUrl, desktopWindow } from '../bridge/tauriBridge';
import { songIdentityKey } from '../engines';
import { usePlayerStore } from '../store/playerStore';
import type { PlayMode, QualityPreference, SongRecord } from '../types/mineradio';

export interface PlayerControlsProps {
  visible?: boolean;
  onOpenCollect?: (song: SongRecord) => void;
  onToast?: (message: string) => void;
  onSongSelected?: (song: SongRecord) => void;
}

const QUALITY_OPTIONS: Array<{ value: QualityPreference; short: string; label: string }> = [
  { value: 'jymaster', short: 'JM', label: '超清母带 · JYMaster' },
  { value: 'hires', short: 'HR', label: 'Hi-Res · 臻品母带' },
  { value: 'lossless', short: 'SQ', label: '无损音质 · FLAC' },
  { value: 'exhigh', short: 'HQ', label: '极高音质 · 320k' },
  { value: 'standard', short: 'PQ', label: '标准音质 · 128k' },
];

const PLAY_MODE_TITLES: Record<PlayMode, string> = {
  loop: '顺序循环',
  single: '单曲循环',
  shuffle: '随机播放',
};

const CUEFIELD_LABELS: Record<string, string> = {
  disabled: 'Cuefield AutoMix 已关闭',
  idle: 'Cuefield 待命',
  waiting: 'Cuefield 等待切歌点',
  preparing: 'Cuefield 分析过渡中...',
  'waiting-beatmap': 'Cuefield 等待节拍图',
  'missing-audio': 'Cuefield 缺少音频流',
  fallback: 'Cuefield 使用兜底过渡',
  'technical-error': 'Cuefield 过渡异常',
  ready: 'Cuefield 过渡已就绪',
  handoff: 'Cuefield 正在混音切歌',
  error: 'Cuefield 错误',
};

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  const min = Math.floor(total / 60);
  const sec = total % 60;
  return `${min}:${String(sec).padStart(2, '0')}`;
}

export function PlayerControls({
  visible = true,
  onOpenCollect,
  onToast,
  onSongSelected,
}: PlayerControlsProps): React.ReactElement {
  const currentSong = usePlayerStore((s) => s.currentSong);
  const playing = usePlayerStore((s) => s.playing);
  const currentTime = usePlayerStore((s) => s.currentTime);
  const duration = usePlayerStore((s) => s.duration);
  const volume = usePlayerStore((s) => s.volume);
  const muted = usePlayerStore((s) => s.muted);
  const playMode = usePlayerStore((s) => s.playMode);
  const quality = usePlayerStore((s) => s.quality);
  const automixEnabled = usePlayerStore((s) => s.automixEnabled);
  const cuefieldStatus = usePlayerStore((s) => s.cuefieldStatus);
  const lyricOffsetSec = usePlayerStore((s) => s.lyricOffsetSec);
  const desktopLyricsEnabled = usePlayerStore((s) => s.desktopLyricsEnabled);
  const shelfOpen = usePlayerStore((s) => s.shelfOpen);
  const fxPanelOpen = usePlayerStore((s) => s.fxPanelOpen);
  const playlistPanelOpen = usePlayerStore((s) => s.playlistPanelOpen);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const currentIdx = usePlayerStore((s) => s.currentIdx);
  const fx = usePlayerStore((s) => s.fx);
  const controlsHidden = usePlayerStore((s) => s.controlsHidden);
  const immersiveMode = usePlayerStore((s) => s.immersiveMode);

  const togglePlay = usePlayerStore((s) => s.togglePlay);
  const nextTrack = usePlayerStore((s) => s.nextTrack);
  const prevTrack = usePlayerStore((s) => s.prevTrack);
  const seekTo = usePlayerStore((s) => s.seekTo);
  const setVolume = usePlayerStore((s) => s.setVolume);
  const toggleMute = usePlayerStore((s) => s.toggleMute);
  const cyclePlayMode = usePlayerStore((s) => s.cyclePlayMode);
  const setQuality = usePlayerStore((s) => s.setQuality);
  const setAutomixEnabled = usePlayerStore((s) => s.setAutomixEnabled);
  const setLyricOffsetSec = usePlayerStore((s) => s.setLyricOffsetSec);
  const toggleDesktopLyrics = usePlayerStore((s) => s.toggleDesktopLyrics);
  const setShelfOpen = usePlayerStore((s) => s.setShelfOpen);
  const setFxPanelOpen = usePlayerStore((s) => s.setFxPanelOpen);
  const setPlaylistPanelOpen = usePlayerStore((s) => s.setPlaylistPanelOpen);
  const updateFx = usePlayerStore((s) => s.updateFx);
  const playSong = usePlayerStore((s) => s.playSong);
  const openTrackDetail = usePlayerStore((s) => s.openTrackDetail);
  const setControlsHidden = usePlayerStore((s) => s.setControlsHidden);
  const setImmersiveMode = usePlayerStore((s) => s.setImmersiveMode);

  const [likedKeys, setLikedKeys] = useState<Record<string, boolean>>({});
  const [qualityOpen, setQualityOpen] = useState(false);
  const [volumeOpen, setVolumeOpen] = useState(false);
  const [miniQueueOpen, setMiniQueueOpen] = useState(false);
  const [sourceSwitcherOpen, setSourceSwitcherOpen] = useState(false);
  const [sourceProvider, setSourceProvider] = useState<'netease' | 'qq' | 'kugou' | 'qishui'>('netease');
  const [sourceResults, setSourceResults] = useState<SongRecord[]>([]);
  const [sourceLoading, setSourceLoading] = useState(false);
  const [dragRatio, setDragRatio] = useState<number | null>(null);

  const progressBarRef = useRef<HTMLDivElement | null>(null);
  const controlsRootRef = useRef<HTMLDivElement | null>(null);
  const dragRatioRef = useRef<number | null>(null);
  const suppressNextClickRef = useRef(false);
  const progressDragCleanupRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    const onDocDown = (e: MouseEvent) => {
      if (!controlsRootRef.current) return;
      if (!controlsRootRef.current.contains(e.target as Node)) {
        setQualityOpen(false);
        setVolumeOpen(false);
        setMiniQueueOpen(false);
        setSourceSwitcherOpen(false);
      }
    };
    document.addEventListener('mousedown', onDocDown);
    return () => {
      document.removeEventListener('mousedown', onDocDown);
      progressDragCleanupRef.current?.();
      progressDragCleanupRef.current = null;
    };
  }, []);

  const currentKey = currentSong ? songIdentityKey(currentSong) : '';
  const isLiked = Boolean(currentKey && likedKeys[currentKey]);

  const computeRatioFromClientX = (clientX: number): number => {
    const el = progressBarRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    if (!rect.width) return 0;
    return Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  };

  const handleProgressPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!duration) return;
    progressDragCleanupRef.current?.();
    suppressNextClickRef.current = true;
    const ratio = computeRatioFromClientX(e.clientX);
    dragRatioRef.current = ratio;
    setDragRatio(ratio);

    let suppressTimer: number | null = null;
    const onMove = (ev: PointerEvent) => {
      const next = computeRatioFromClientX(ev.clientX);
      dragRatioRef.current = next;
      setDragRatio(next);
    };
    const cleanup = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      if (suppressTimer !== null) {
        window.clearTimeout(suppressTimer);
        suppressTimer = null;
      }
      if (progressDragCleanupRef.current === cleanup) {
        progressDragCleanupRef.current = null;
      }
    };
    const onUp = (ev: PointerEvent) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      const finalRatio = computeRatioFromClientX(ev.clientX);
      dragRatioRef.current = null;
      setDragRatio(null);
      if (duration > 0) {
        seekTo(finalRatio * duration);
      }
      suppressTimer = window.setTimeout(() => {
        suppressNextClickRef.current = false;
        suppressTimer = null;
        if (progressDragCleanupRef.current === cleanup) {
          progressDragCleanupRef.current = null;
        }
      }, 150);
    };
    progressDragCleanupRef.current = cleanup;
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  };

  const displayRatio =
    dragRatio !== null
      ? dragRatio
      : duration > 0
      ? Math.max(0, Math.min(1, currentTime / duration))
      : 0;
  const displayCurrentSec = dragRatio !== null ? dragRatio * duration : currentTime;

  const handleToggleLike = async () => {
    if (!currentSong) return;
    const provider = String(currentSong.provider || currentSong.source || 'netease').toLowerCase();
    if (provider === 'qq') {
      onToast?.('QQ 音乐暂不支持红心收藏，请前往 QQ 音乐客户端操作');
      return;
    }
    const nextLiked = !isLiked;
    setLikedKeys((prev) => ({ ...prev, [currentKey]: nextLiked }));
    try {
      if (provider === 'kugou') {
        await apiJson('/api/kugou/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            hash: currentSong.hash ?? currentSong.fileHash ?? currentSong.id,
            id: currentSong.id,
            like: nextLiked,
            song: currentSong,
          }),
        });
      } else if (provider === 'qishui') {
        await apiJson('/api/qishui/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            id: currentSong.id ?? currentSong.trackId,
            trackId: currentSong.trackId ?? currentSong.id,
            like: nextLiked,
            song: currentSong,
          }),
        });
      } else {
        await apiJson('/api/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            id: currentSong.id,
            like: nextLiked,
            song: currentSong,
          }),
        });
      }
    } catch {
      // keep optimistic UI toggle
    }
    onToast?.(nextLiked ? `已喜欢：${currentSong.name}` : `已取消喜欢：${currentSong.name}`);
  };

  const fetchAlternativeSources = async (targetProvider: 'netease' | 'qq' | 'kugou' | 'qishui') => {
    setSourceProvider(targetProvider);
    if (!currentSong) {
      setSourceResults([]);
      return;
    }
    const kw = `${currentSong.name || currentSong.title || ''} ${currentSong.artist || ''}`.trim();
    if (!kw) return;
    setSourceLoading(true);
    try {
      const endpoint =
        targetProvider === 'qq'
          ? `/api/qq/search?keywords=${encodeURIComponent(kw)}&limit=12`
          : targetProvider === 'kugou'
          ? `/api/kugou/search?keywords=${encodeURIComponent(kw)}&limit=12`
          : targetProvider === 'qishui'
          ? `/api/qishui/search?keywords=${encodeURIComponent(kw)}&limit=12`
          : `/api/search?keywords=${encodeURIComponent(kw)}&limit=12`;
      const res = await apiJson<{
        songs?: SongRecord[];
        tracks?: SongRecord[];
        results?: SongRecord[];
        data?: { songs?: SongRecord[] };
      }>(endpoint, { timeoutMs: 10000 });
      const list = (res.songs || res.tracks || res.results || res.data?.songs || []).map((item) => ({
        ...item,
        provider: item.provider || targetProvider,
      }));
      setSourceResults(list);
    } catch {
      setSourceResults([]);
    } finally {
      setSourceLoading(false);
    }
  };

  const activeQualityMeta = QUALITY_OPTIONS.find((q) => q.value === quality) || QUALITY_OPTIONS[2];
  const volumePercent = Math.round((muted ? 0 : volume) * 100);
  const coverSrc = currentSong?.cover ? coverProxyUrl(currentSong.cover) : '';

  return (
    <div ref={controlsRootRef}>
      <button
        id="bottom-handle"
        type="button"
        aria-label="展开播放器控制台"
        title="播放器控制台"
        onClick={() => {
          if (controlsHidden) {
            setControlsHidden(false);
            return;
          }
          setPlaylistPanelOpen(!playlistPanelOpen);
        }}
      >
        <span />
      </button>

      <div id="bottom-bar" className={visible && !controlsHidden ? 'visible' : 'soft-hidden'}>
        <div
          id="mini-queue-popover"
          className={`mini-queue-popover${miniQueueOpen ? ' show' : ''}`}
          onClick={(e) => e.stopPropagation()}
        >
          <div className="mini-queue-head">
            <div>
              <div className="mini-queue-title">当前队列</div>
              <div id="mini-queue-count" className="mini-queue-count">
                {playQueue.length} 首
              </div>
            </div>
            <button
              className="fx-mini-btn ghost"
              type="button"
              onClick={() => setMiniQueueOpen(false)}
              title="关闭"
              style={{ height: 26, padding: '0 9px', fontSize: 13 }}
            >
              ×
            </button>
          </div>
          <div id="mini-queue-list" className="mini-queue-list">
            {playQueue.length === 0 ? (
              <div className="search-empty">暂无队列曲目</div>
            ) : (
              playQueue.map((song, idx) => (
                <div
                  key={`${songIdentityKey(song)}-${idx}`}
                  className={`queue-item${idx === currentIdx ? ' now' : ''}`}
                  onClick={() => {
                    onSongSelected?.(song);
                    playSong(idx).catch(() => {});
                    setMiniQueueOpen(false);
                  }}
                >
                  <div className="qi-info">
                    <div className="qi-name">{song.name || song.title}</div>
                    <div className="qi-sub">{song.artist || '未知歌手'}</div>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>

        <div
          ref={progressBarRef}
          id="progress-bar"
          className={dragRatio !== null ? 'is-dragging' : undefined}
          role="slider"
          aria-label="播放进度"
          aria-valuemin={0}
          aria-valuemax={Math.round(duration || 0)}
          aria-valuenow={Math.round(displayCurrentSec)}
          tabIndex={0}
          onPointerDown={handleProgressPointerDown}
          onClick={(e) => {
            if (suppressNextClickRef.current) {
              suppressNextClickRef.current = false;
              return;
            }
            if (!duration) return;
            const ratio = computeRatioFromClientX(e.clientX);
            seekTo(ratio * duration);
          }}
        >
          <div id="progress-fill" style={{ width: `${(displayRatio * 100).toFixed(2)}%` }} />
          <div
            id="progress-thumb"
            aria-hidden="true"
            style={{ left: `${(displayRatio * 100).toFixed(2)}%` }}
          />
        </div>

        <div id="controls">
          {/* 左侧：歌曲信息 + 音质 + 喜欢 + 收藏 */}
          <div className="control-cluster actions">
            <div className="control-track">
              <div
                id="control-cover"
                className={`control-cover${coverSrc ? '' : ' cover-empty'}`}
                role="button"
                tabIndex={0}
                title="查看歌曲详情"
                aria-label="查看歌曲详情"
                style={coverSrc ? { backgroundImage: `url("${coverSrc}")` } : undefined}
                onClick={() => openTrackDetail(currentSong || undefined)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    openTrackDetail(currentSong || undefined);
                  }
                }}
              />
              <div className="control-meta">
                <div id="control-title" className="control-title">
                  <span id="control-title-text" className="control-title-text">
                    {currentSong ? currentSong.name || currentSong.title : 'MineradioX'}
                  </span>
                  <span id="control-title-badges" className="control-title-badges">
                    <div
                      id="control-source-switcher"
                      className={`control-source-switcher${sourceSwitcherOpen ? ' open' : ''}`}
                    >
                      <button
                        id="control-source-btn"
                        type="button"
                        className="tag-source control-source-btn"
                        title="跨平台搜歌换源"
                        aria-label="跨平台搜歌换源"
                        aria-expanded={sourceSwitcherOpen}
                        onClick={(e) => {
                          e.stopPropagation();
                          const nextOpen = !sourceSwitcherOpen;
                          setSourceSwitcherOpen(nextOpen);
                          if (nextOpen && currentSong) {
                            const initProvider =
                              currentSong.provider === 'qq' ||
                              currentSong.provider === 'kugou' ||
                              currentSong.provider === 'qishui'
                                ? currentSong.provider
                                : 'netease';
                            fetchAlternativeSources(initProvider).catch(() => {});
                          }
                        }}
                      >
                        {String(currentSong?.provider || 'NETEASE').toUpperCase()}
                      </button>

                      {sourceSwitcherOpen && (
                        <div
                          id="control-source-popover"
                          className="control-source-popover quality-popover"
                          onClick={(e) => e.stopPropagation()}
                          style={{
                            minWidth: 260,
                            maxWidth: 320,
                            padding: 10,
                            zIndex: 80,
                          }}
                        >
                          <div
                            className="control-source-tabs"
                            style={{ display: 'flex', gap: 6, marginBottom: 8 }}
                          >
                            {(
                              [
                                { id: 'netease', label: '网易云' },
                                { id: 'qq', label: 'QQ' },
                                { id: 'kugou', label: '酷狗' },
                                { id: 'qishui', label: '汽水' },
                              ] as const
                            ).map((tab) => (
                              <button
                                key={tab.id}
                                type="button"
                                className={`fx-mini-btn${sourceProvider === tab.id ? ' active' : ' ghost'}`}
                                style={{ flex: 1, height: 26, fontSize: 11, padding: '0 6px' }}
                                onClick={() => {
                                  fetchAlternativeSources(tab.id).catch(() => {});
                                }}
                              >
                                {tab.label}
                              </button>
                            ))}
                          </div>
                          <div
                            className="control-source-results"
                            style={{ maxHeight: 210, overflowY: 'auto' }}
                          >
                            {!currentSong ? (
                              <div className="search-empty" style={{ padding: '12px 6px', fontSize: 12 }}>
                                请先播放一首歌曲以搜索换源
                              </div>
                            ) : sourceLoading ? (
                              <div className="search-empty" style={{ padding: '12px 6px', fontSize: 12 }}>
                                正在检索同名音源…
                              </div>
                            ) : sourceResults.length === 0 ? (
                              <div className="search-empty" style={{ padding: '12px 6px', fontSize: 12 }}>
                                未找到可用替代音源
                              </div>
                            ) : (
                              sourceResults.map((cand, i) => (
                                <button
                                  key={`${songIdentityKey(cand)}-${i}`}
                                  type="button"
                                  className="quality-option"
                                  style={{
                                    width: '100%',
                                    textAlign: 'left',
                                    display: 'flex',
                                    flexDirection: 'column',
                                    alignItems: 'flex-start',
                                    gap: 2,
                                    padding: '6px 8px',
                                  }}
                                  onClick={() => {
                                    onSongSelected?.(cand);
                                    playSong(cand).catch(() => {});
                                    setSourceSwitcherOpen(false);
                                    onToast?.(
                                      `已切换至 ${String(cand.provider || sourceProvider).toUpperCase()} 音源：${
                                        cand.name || cand.title
                                      }`
                                    );
                                  }}
                                >
                                  <strong style={{ fontSize: 12 }}>{cand.name || cand.title}</strong>
                                  <span style={{ fontSize: 11, opacity: 0.72 }}>
                                    {cand.artist || '未知歌手'} · {String(cand.provider || sourceProvider).toUpperCase()}
                                  </span>
                                </button>
                              ))
                            )}
                          </div>
                        </div>
                      )}
                    </div>
                  </span>
                  <div
                    id="quality-control"
                    className={`quality-control control-quality-chip${qualityOpen ? ' open' : ''}`}
                  >
                    <button
                      id="quality-btn"
                      className="ctrl-btn quality-pill"
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        setQualityOpen((v) => !v);
                      }}
                      title={`音质：${activeQualityMeta.label}`}
                    >
                      <span id="quality-btn-label">{activeQualityMeta.short}</span>
                    </button>
                    <div className="quality-popover" onClick={(e) => e.stopPropagation()}>
                      <div id="quality-option-list" className="quality-option-list">
                        {QUALITY_OPTIONS.map((opt) => (
                          <button
                            key={opt.value}
                            type="button"
                            className={`quality-option${quality === opt.value ? ' active' : ''}`}
                            onClick={() => {
                              setQuality(opt.value);
                              setQualityOpen(false);
                              onToast?.(`音质已切换为 ${opt.label}`);
                            }}
                          >
                            <strong>{opt.short}</strong>
                            <span>{opt.label}</span>
                          </button>
                        ))}
                      </div>
                    </div>
                  </div>
                </div>
                <div id="control-artist" className="control-artist">
                  {currentSong ? currentSong.artist || '未知歌手' : '点击上方搜索或选择曲目'}
                </div>
              </div>
            </div>

            <button
              id="heart-btn"
              className={`ctrl-btn${isLiked ? ' liked active' : ''}`}
              type="button"
              onClick={() => {
                handleToggleLike().catch(() => {});
              }}
              title="红心喜欢"
              aria-label="红心喜欢"
              aria-pressed={isLiked}
            >
              <svg className="heart-svg" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 21.45c-.32 0-.62-.12-.86-.34l-1.23-1.12C5.54 16.03 2.25 13.05 2.25 8.9 2.25 5.48 4.88 2.9 8.28 2.9c1.7 0 3.35.72 4.52 1.96C13.97 3.62 15.62 2.9 17.32 2.9c3.4 0 6.03 2.58 6.03 6 0 4.15-3.29 7.13-7.66 11.09l-1.23 1.12c-.24.22-.54.34-.86.34z" />
              </svg>
            </button>

            <button
              id="collect-btn"
              className="ctrl-btn"
              type="button"
              onClick={() => {
                if (currentSong) {
                  onOpenCollect?.(currentSong);
                } else {
                  onToast?.('请先播放一首歌曲');
                }
              }}
              title="收藏到歌单"
              aria-label="收藏到歌单"
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                <path d="M12 5v14" />
                <path d="M5 12h14" />
              </svg>
            </button>
          </div>

          {/* 中间：播放模式 + AutoMix + 上一曲/播放暂停/下一曲 + 队列 */}
          <div className="control-cluster transport">
            <button
              id="play-mode-btn"
              className="ctrl-btn"
              type="button"
              onClick={() => {
                cyclePlayMode();
                const next = usePlayerStore.getState().playMode;
                onToast?.(`播放模式：${PLAY_MODE_TITLES[next]}`);
              }}
              title={`播放模式：${PLAY_MODE_TITLES[playMode]}`}
              aria-label={`播放模式：${PLAY_MODE_TITLES[playMode]}`}
            >
              {playMode === 'single' ? (
                <svg id="play-mode-icon" width="19" height="19" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                  <path d="M17 2l4 4-4 4" />
                  <path d="M3 11V9a4 4 0 0 1 4-4h14" />
                  <path d="M7 22l-4-4 4-4" />
                  <path d="M21 13v2a4 4 0 0 1-4 4H3" />
                  <path d="M11 10l1.5-1v6" />
                </svg>
              ) : playMode === 'shuffle' ? (
                <svg id="play-mode-icon" width="19" height="19" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                  <polyline points="16 3 21 3 21 8" />
                  <line x1="4" y1="20" x2="21" y2="3" />
                  <polyline points="21 16 21 21 16 21" />
                  <line x1="15" y1="15" x2="21" y2="21" />
                  <line x1="4" y1="4" x2="9" y2="9" />
                </svg>
              ) : (
                <svg id="play-mode-icon" width="19" height="19" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                  <path d="M17 2l4 4-4 4" />
                  <path d="M3 11V9a4 4 0 0 1 4-4h14" />
                  <path d="M7 22l-4-4 4-4" />
                  <path d="M21 13v2a4 4 0 0 1-4 4H3" />
                </svg>
              )}
            </button>

            <button
              id="cuefield-automix-btn"
              className={`ctrl-btn${automixEnabled ? ' active' : ''}`}
              type="button"
              onClick={() => {
                const next = !automixEnabled;
                setAutomixEnabled(next);
                onToast?.(next ? 'Cuefield AutoMix 已开启' : 'Cuefield AutoMix 已关闭');
              }}
              title={CUEFIELD_LABELS[cuefieldStatus] || 'Cuefield AutoMix'}
              aria-label="Cuefield AutoMix"
              aria-pressed={automixEnabled}
            >
              <svg width="19" height="19" fill="none" stroke="currentColor" strokeWidth="1.8" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M4 7h4l3 5 3-5h6" />
                <path d="M4 17h4l3-5 3 5h6" />
                <path d="M18 4v6M15 7h6" />
              </svg>
            </button>

            <button
              id="prev-btn"
              className="ctrl-btn"
              type="button"
              onClick={() => {
                prevTrack().catch(() => {});
              }}
              title="上一首"
              aria-label="上一首"
            >
              <svg width="18" height="18" fill="currentColor" viewBox="0 0 24 24">
                <path d="M6 6h2v12H6zm3.5 6l8.5 6V6z" />
              </svg>
            </button>

            <button
              id="play-btn"
              className="ctrl-btn"
              type="button"
              onClick={() => {
                togglePlay().catch(() => {});
              }}
              title={playing ? '暂停' : '播放'}
              aria-label={playing ? '暂停' : '播放'}
            >
              {playing ? (
                <svg id="play-icon" width="20" height="20" fill="currentColor" viewBox="0 0 24 24">
                  <rect x="6" y="5" width="4" height="14" rx="1" />
                  <rect x="14" y="5" width="4" height="14" rx="1" />
                </svg>
              ) : (
                <svg id="play-icon" width="20" height="20" fill="currentColor" viewBox="0 0 24 24">
                  <path d="M8 5v14l11-7z" />
                </svg>
              )}
            </button>

            <button
              id="next-btn"
              className="ctrl-btn"
              type="button"
              onClick={() => {
                nextTrack().catch(() => {});
              }}
              title="下一首"
              aria-label="下一首"
            >
              <svg width="18" height="18" fill="currentColor" viewBox="0 0 24 24">
                <path d="M6 18l8.5-6L6 6v12zM16 6v12h2V6h-2z" />
              </svg>
            </button>

            <button
              id="mini-queue-btn"
              className={`ctrl-btn${playlistPanelOpen || miniQueueOpen ? ' active' : ''}`}
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setPlaylistPanelOpen(!playlistPanelOpen);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                setMiniQueueOpen((v) => !v);
              }}
              title="歌单 / 播放队列"
              aria-label="歌单 / 播放队列"
            >
              <svg width="19" height="19" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                <path d="M8 6h13" />
                <path d="M8 12h13" />
                <path d="M8 18h13" />
                <path d="M3 6h.01" />
                <path d="M3 12h.01" />
                <path d="M3 18h.01" />
              </svg>
            </button>
          </div>

          {/* 右侧：歌词/桌面歌词/3D歌单架/FX控制台/音量/全屏/时间 */}
          <div className="control-cluster modes">
            <div
              id="lyric-timing-control"
              className={`lyric-timing-control${lyricOffsetSec !== 0 ? ' has-offset' : ''}`}
            >
              <button
                id="lyrics-toggle-btn"
                className={`ctrl-btn lyrics-toggle-btn${fx.particleLyrics ? ' active' : ''}`}
                type="button"
                onClick={() => updateFx({ particleLyrics: !fx.particleLyrics })}
                title={fx.particleLyrics ? '隐藏舞台歌词' : '显示舞台歌词'}
                aria-label="舞台歌词开关"
                aria-pressed={fx.particleLyrics}
              >
                <span className="lyrics-word-icon">词</span>
              </button>
              <div
                id="lyric-timing-popover"
                className="lyric-timing-popover"
                onClick={(e) => e.stopPropagation()}
              >
                <div className="lyric-timing-head">
                  <span>歌词校准</span>
                  <strong id="lyric-timing-value">{lyricOffsetSec.toFixed(1)}s</strong>
                </div>
                <div id="lyric-timing-song" className="lyric-timing-song">
                  {currentSong?.name || '当前歌曲'}
                </div>
                <div className="lyric-timing-actions">
                  <button
                    type="button"
                    onClick={() => setLyricOffsetSec(Number((lyricOffsetSec - 0.1).toFixed(1)))}
                    title="歌词延后 0.1 秒"
                  >
                    -0.1
                  </button>
                  <button
                    type="button"
                    onClick={() => setLyricOffsetSec(0)}
                    title="重置当前歌曲歌词校准"
                  >
                    0
                  </button>
                  <button
                    type="button"
                    onClick={() => setLyricOffsetSec(Number((lyricOffsetSec + 0.1).toFixed(1)))}
                    title="歌词提前 0.1 秒"
                  >
                    +0.1
                  </button>
                </div>
              </div>
            </div>

            <button
              id="desktop-lyrics-btn"
              className={`ctrl-btn${desktopLyricsEnabled ? ' active' : ''}`}
              type="button"
              onClick={() => {
                toggleDesktopLyrics().catch(() => {});
              }}
              title="桌面歌词"
              aria-label="桌面歌词"
              aria-pressed={desktopLyricsEnabled}
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.9" viewBox="0 0 24 24">
                <rect x="3" y="4" width="18" height="13" rx="2.5" />
                <path d="M7 10h10M9 13h6M8 20h8" />
              </svg>
            </button>

            <button
              id="shelf-toggle-btn"
              className={`ctrl-btn${shelfOpen ? ' active' : ''}`}
              type="button"
              onClick={() => {
                const next = !shelfOpen;
                if (next && fx.shelf === 'off') {
                  updateFx({ shelf: 'side' });
                }
                setShelfOpen(next);
              }}
              title="3D 歌单架"
              aria-label="3D 歌单架"
              aria-pressed={shelfOpen}
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.9" viewBox="0 0 24 24">
                <path d="M4 6h5v12H4z" />
                <path d="M10 4h5v14h-5z" />
                <path d="M16 7h4v11h-4z" />
              </svg>
            </button>

            <button
              id="fx-panel-toggle-btn"
              className={`ctrl-btn${fxPanelOpen ? ' active' : ''}`}
              type="button"
              onClick={() => setFxPanelOpen(!fxPanelOpen)}
              title="FX 视觉控制台"
              aria-label="FX 视觉控制台"
              aria-pressed={fxPanelOpen}
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.9" viewBox="0 0 24 24">
                <path d="M4 7h8M16 7h4" />
                <circle cx="14" cy="7" r="2" />
                <path d="M4 17h4M12 17h8" />
                <circle cx="10" cy="17" r="2" />
              </svg>
            </button>

            <div
              id="volume-control"
              className={`volume-control${muted || volume === 0 ? ' muted' : ''}${
                volumeOpen ? ' open' : ''
              }`}
            >
              <button
                id="volume-btn"
                className="ctrl-btn"
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  toggleMute();
                }}
                onMouseEnter={() => setVolumeOpen(true)}
                title={`音量 ${volumePercent}%（点击静音）`}
                aria-label="音量 / 静音"
              >
                <svg
                  id="volume-icon"
                  width="18"
                  height="18"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  viewBox="0 0 24 24"
                >
                  <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                  {muted || volume === 0 ? (
                    <path d="M22 9l-6 6M16 9l6 6" />
                  ) : (
                    <path d="M15 9.5a4 4 0 0 1 0 5M18 7a7.5 7.5 0 0 1 0 10" />
                  )}
                </svg>
              </button>
              <div className="volume-popover" onClick={(e) => e.stopPropagation()}>
                <div className="volume-main-row">
                  <input
                    id="volume-slider"
                    type="range"
                    min="0"
                    max="1"
                    step="0.01"
                    value={muted ? 0 : volume}
                    aria-label="音量"
                    onChange={(e) => setVolume(Number(e.target.value))}
                  />
                  <span id="volume-value">{volumePercent}%</span>
                </div>
              </div>
            </div>

            <button
              id="immersive-btn"
              className={`ctrl-btn${immersiveMode ? ' active' : ''}`}
              type="button"
              onClick={() => {
                const next = !immersiveMode;
                setImmersiveMode(next);
                onToast?.(next ? '已进入纯净沉浸模式（Esc 退出）' : '已退出沉浸模式');
              }}
              title={immersiveMode ? '退出沉浸模式' : '纯净沉浸模式'}
              aria-label="沉浸模式"
              aria-pressed={immersiveMode}
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.9" viewBox="0 0 24 24">
                <circle cx="12" cy="12" r="3" />
                <path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6z" />
              </svg>
            </button>

            <button
              id="controls-hide-btn"
              className={`ctrl-btn${controlsHidden ? ' active' : ''}`}
              type="button"
              onClick={() => {
                const next = !controlsHidden;
                setControlsHidden(next);
                onToast?.(next ? '已隐藏底部控制台（点击底部细柄恢复）' : '已显示底部控制台');
              }}
              title={controlsHidden ? '显示控制台' : '隐藏控制台'}
              aria-label="隐藏控制台"
              aria-pressed={controlsHidden}
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                <path d="M6 9l6 6 6-6" />
              </svg>
            </button>

            <button
              className="ctrl-btn fullscreen-toggle-btn"
              type="button"
              onClick={() => {
                desktopWindow.toggleFullscreen().catch(() => {});
              }}
              title="全屏 (F)"
              aria-label="全屏"
            >
              <svg width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24">
                <path d="M3 7V3h4" />
                <path d="M21 7V3h-4" />
                <path d="M3 17v4h4" />
                <path d="M21 17v4h-4" />
              </svg>
            </button>

            <div id="time-display">
              {formatTime(displayCurrentSec)} / {formatTime(duration)}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default PlayerControls;
