import React, { useEffect, useMemo, useRef, useState } from 'react';
import { apiJson, coverProxyUrl } from '../bridge/tauriBridge';
import { songIdentityKey, visualEngine } from '../engines';
import { usePlayerStore } from '../store/playerStore';
import type { SearchMode, SongRecord } from '../types/mineradio';

export interface SearchBarProps {
  onToast?: (message: string) => void;
  onSongSelected?: (song: SongRecord) => void;
  onOpenCollect?: (song: SongRecord) => void;
}

const SEARCH_TABS: Array<{ id: string; mode: SearchMode; label: string }> = [
  { id: 'search-mode-song', mode: 'song', label: 'All' },
  { id: 'search-mode-netease', mode: 'netease', label: 'NE' },
  { id: 'search-mode-qq', mode: 'qq', label: 'QQ' },
  { id: 'search-mode-kugou', mode: 'kugou', label: 'KG' },
  { id: 'search-mode-qishui', mode: 'qishui', label: 'QS' },
  { id: 'search-mode-podcast', mode: 'podcast', label: 'Podcast' },
];

const PAGE_SIZE = 8;

interface PodcastRadio {
  id: string | number;
  name: string;
  cover?: string;
  desc?: string;
  programCount?: number;
  dj?: string;
}

function normalizeRadio(raw: Record<string, unknown>): PodcastRadio | null {
  const id = (raw.id ?? raw.radioId ?? raw.podcastId) as string | number | undefined;
  if (id === undefined || id === null || id === '') return null;
  return {
    id,
    name: String(raw.name || raw.title || '未命名电台'),
    cover: String(raw.cover || raw.picUrl || raw.coverUrl || ''),
    desc: String(raw.desc || raw.description || raw.rcmdText || ''),
    programCount: Number(raw.programCount || raw.trackCount || 0) || undefined,
    dj: String(raw.dj || raw.artist || raw.nickname || ''),
  };
}

function pickArray(res: Record<string, unknown>, keys: string[]): Array<Record<string, unknown>> {
  for (const k of keys) {
    const v = res[k];
    if (Array.isArray(v)) return v as Array<Record<string, unknown>>;
  }
  const data = res.data;
  if (Array.isArray(data)) return data as Array<Record<string, unknown>>;
  if (data && typeof data === 'object') return pickArray(data as Record<string, unknown>, keys);
  return [];
}

function providerLabel(song: SongRecord): string {
  const p = String(song.provider || song.source || '').toLowerCase();
  if (p === 'qq') return 'QQ';
  if (p === 'kugou') return 'KG';
  if (p === 'qishui') return 'QS';
  if (p === 'local') return '本地';
  if (p === 'podcast' || song.type === 'podcast' || song.podcastId) return '播客';
  return 'NE';
}

export function SearchBar({
  onToast,
  onSongSelected,
  onOpenCollect,
}: SearchBarProps): React.ReactElement {
  const searchMode = usePlayerStore((s) => s.searchMode);
  const searchQuery = usePlayerStore((s) => s.searchQuery);
  const searchResults = usePlayerStore((s) => s.searchResults);
  const searchLoading = usePlayerStore((s) => s.searchLoading);
  const currentSong = usePlayerStore((s) => s.currentSong);
  const splashDone = usePlayerStore((s) => s.splashDone);
  const setSearchMode = usePlayerStore((s) => s.setSearchMode);
  const setSearchQuery = usePlayerStore((s) => s.setSearchQuery);
  const searchTracks = usePlayerStore((s) => s.searchTracks);
  const playSong = usePlayerStore((s) => s.playSong);
  const importLocalFiles = usePlayerStore((s) => s.importLocalFiles);
  const searchHistory = usePlayerStore((s) => s.searchHistory);
  const addSearchHistory = usePlayerStore((s) => s.addSearchHistory);
  const clearSearchHistory = usePlayerStore((s) => s.clearSearchHistory);
  const [hotRadios, setHotRadios] = useState<PodcastRadio[]>([]);
  const [hotLoading, setHotLoading] = useState(false);
  const [openRadioId, setOpenRadioId] = useState<string | number | null>(null);
  const [radioPrograms, setRadioPrograms] = useState<SongRecord[]>([]);
  const [programsLoading, setProgramsLoading] = useState(false);
  const [likedKeys, setLikedKeys] = useState<Record<string, boolean>>({});

  const [resultsOpen, setResultsOpen] = useState(false);
  const [hasSearched, setHasSearched] = useState(false);
  const [page, setPage] = useState(1);
  const [uploadPanelOpen, setUploadPanelOpen] = useState(false);
  const [uploadTipOpen, setUploadTipOpen] = useState(false);
  const [customCoverMap, setCustomCoverMap] = useState<Record<string, string>>({});

  const areaRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const coverInputRef = useRef<HTMLInputElement | null>(null);
  const audioInputRef = useRef<HTMLInputElement | null>(null);
  const folderInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    setPage(1);
  }, [searchResults, searchMode]);

  useEffect(() => {
    const onDocPointerDown = (e: MouseEvent) => {
      if (!areaRef.current) return;
      if (!areaRef.current.contains(e.target as Node)) {
        setResultsOpen(false);
        setUploadPanelOpen(false);
      }
    };
    document.addEventListener('mousedown', onDocPointerDown);
    return () => document.removeEventListener('mousedown', onDocPointerDown);
  }, []);

  const podcastBrowse = searchMode === 'podcast' && !searchQuery.trim();

  useEffect(() => {
    if (!podcastBrowse) return;
    let cancelled = false;
    setHotLoading(true);
    apiJson<Record<string, unknown>>('/api/podcast/hot?limit=24', { timeoutMs: 8000 })
      .then((res) => {
        if (cancelled) return;
        const list = pickArray(res || {}, ['podcasts', 'radios', 'djRadios', 'list', 'items'])
          .map(normalizeRadio)
          .filter((r): r is PodcastRadio => Boolean(r));
        setHotRadios(list);
      })
      .catch(() => {
        if (!cancelled) setHotRadios([]);
      })
      .finally(() => {
        if (!cancelled) setHotLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [podcastBrowse]);

  const toggleRadio = async (radio: PodcastRadio) => {
    if (openRadioId === radio.id) {
      setOpenRadioId(null);
      setRadioPrograms([]);
      return;
    }
    setOpenRadioId(radio.id);
    setRadioPrograms([]);
    setProgramsLoading(true);
    try {
      const res = await apiJson<Record<string, unknown>>(
        `/api/podcast/programs?id=${encodeURIComponent(String(radio.id))}&limit=30`,
        { timeoutMs: 10000 }
      );
      const list = pickArray(res || {}, ['programs', 'songs', 'tracks', 'list']) as unknown as SongRecord[];
      setRadioPrograms(
        list.map((p) => ({
          ...p,
          provider: p.provider || 'netease',
          type: 'podcast',
          podcastId: p.podcastId ?? radio.id,
          cover: p.cover || radio.cover,
        }))
      );
    } catch {
      onToast?.('读取电台节目失败');
    } finally {
      setProgramsLoading(false);
    }
  };

  const handleLikeResult = async (song: SongRecord, e: React.MouseEvent) => {
    e.stopPropagation();
    const key = songIdentityKey(song);
    const provider = String(song.provider || song.source || 'netease').toLowerCase();
    const id = song.id ?? song.mid ?? song.songmid ?? song.hash;
    const next = !likedKeys[key];
    try {
      if (provider === 'qq') {
        await apiJson('/api/qq/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ id, mid: song.mid || song.songmid, like: next }),
        });
      } else if (provider === 'kugou') {
        await apiJson('/api/kugou/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ hash: song.hash || song.fileHash, id, like: next }),
        });
      } else if (provider === 'qishui') {
        await apiJson('/api/qishui/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ id, like: next }),
        });
      } else {
        await apiJson('/api/song/like', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ id, like: next }),
        });
      }
      setLikedKeys((prev) => ({ ...prev, [key]: next }));
      onToast?.(next ? `已喜欢：${song.name}` : `已取消喜欢：${song.name}`);
    } catch (err) {
      onToast?.(`操作失败：${(err as Error)?.message || '请先登录对应平台'}`);
    }
  };

  const totalPages = Math.max(1, Math.ceil(searchResults.length / PAGE_SIZE));
  const safePage = Math.min(page, totalPages);
  const pagedResults = useMemo(() => {
    const start = (safePage - 1) * PAGE_SIZE;
    return searchResults.slice(start, start + PAGE_SIZE);
  }, [searchResults, safePage]);

  const currentKey = currentSong ? songIdentityKey(currentSong) : '';
  const hasCustomCover = Boolean(currentKey && customCoverMap[currentKey]);

  const runSearch = async (query: string, mode: SearchMode = searchMode) => {
    const trimmed = query.trim();
    if (!trimmed) {
      setHasSearched(false);
      setResultsOpen(false);
      await searchTracks('', mode);
      return;
    }
    setHasSearched(true);
    setResultsOpen(true);
    setPage(1);
    addSearchHistory(trimmed);
    await searchTracks(query, mode);
  };

  const handleModeClick = (mode: SearchMode) => {
    setSearchMode(mode);
    setPage(1);
    if (searchQuery.trim()) {
      runSearch(searchQuery, mode).catch(() => {});
    }
  };

  const handlePlayResult = async (song: SongRecord) => {
    setResultsOpen(false);
    onSongSelected?.(song);
    await playSong(song, searchResults);
  };

  const handleAppendQueue = (song: SongRecord, e: React.MouseEvent) => {
    e.stopPropagation();
    const st = usePlayerStore.getState();
    const key = songIdentityKey(song);
    const exists = st.playQueue.some((item) => songIdentityKey(item) === key);
    if (!exists) {
      usePlayerStore.setState({ playQueue: [...st.playQueue, song] });
      onToast?.(`已加入队列：${song.name}`);
    } else {
      onToast?.(`已在队列中：${song.name}`);
    }
  };

  const handleAudioImport = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setUploadPanelOpen(false);
    const tracks = await importLocalFiles(files);
    if (tracks.length > 0) {
      onToast?.(`已导入 ${tracks.length} 首本地音乐`);
      onSongSelected?.(tracks[0]);
      await playSong(tracks[0], [...tracks, ...usePlayerStore.getState().playQueue]);
    } else {
      onToast?.('未识别到可导入的音频文件');
    }
  };

  const handleCustomCoverChange = (files: FileList | null) => {
    const file = files?.[0];
    if (!file) return;
    setUploadPanelOpen(false);
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = typeof reader.result === 'string' ? reader.result : '';
      if (!dataUrl) return;
      const st = usePlayerStore.getState();
      if (st.currentSong) {
        const key = songIdentityKey(st.currentSong);
        const origCover = String(st.currentSong.cover || '');
        if (key && !customCoverMap[key]) {
          setCustomCoverMap((prev) => ({ ...prev, [key]: origCover }));
        }
        const updatedSong: SongRecord = { ...st.currentSong, cover: dataUrl };
        usePlayerStore.setState({ currentSong: updatedSong });
      }
      visualEngine.setCover(dataUrl);
      onToast?.('已应用自定义封面');
    };
    reader.readAsDataURL(file);
  };

  const handleClearCustomCover = () => {
    const st = usePlayerStore.getState();
    if (!st.currentSong) return;
    const key = songIdentityKey(st.currentSong);
    const original = (key && customCoverMap[key]) || '';
    const updatedSong: SongRecord = { ...st.currentSong, cover: original };
    usePlayerStore.setState({ currentSong: updatedSong });
    visualEngine.setCover(original);
    if (key) {
      setCustomCoverMap((prev) => {
        const next = { ...prev };
        delete next[key];
        return next;
      });
    }
    onToast?.('已还原默认封面');
  };

  const showResults = resultsOpen && (searchLoading || hasSearched || searchResults.length > 0);

  return (
    <div
      ref={areaRef}
      id="search-area"
      className={`${splashDone ? 'peek' : ''}${showResults ? ' has-results' : ''}${
        hasCustomCover ? ' has-cover-action' : ''
      }`.trim()}
    >
      <div id="search-stack">
        <div
          id="search-box"
          onClick={() => {
            inputRef.current?.focus();
          }}
        >
          <svg
            id="search-icon"
            width="17"
            height="17"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            viewBox="0 0 24 24"
            aria-hidden="true"
          >
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.35-4.35" />
          </svg>
          <input
            ref={inputRef}
            id="search-input"
            type="text"
            placeholder="搜索歌曲、歌手..."
            autoComplete="off"
            spellCheck={false}
            value={searchQuery}
            onFocus={() => {
              if (searchResults.length > 0 || hasSearched) setResultsOpen(true);
            }}
            onChange={(e) => {
              const val = e.target.value;
              setSearchQuery(val);
              if (!val.trim()) {
                setResultsOpen(false);
                setHasSearched(false);
              }
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                runSearch(searchQuery, searchMode).catch(() => {});
              } else if (e.key === 'Escape') {
                setResultsOpen(false);
              }
            }}
          />
          {searchQuery && (
            <button
              type="button"
              className="fx-mini-btn ghost"
              style={{ height: 24, padding: '0 8px', fontSize: 11 }}
              aria-label="清空搜索"
              onClick={(e) => {
                e.stopPropagation();
                setSearchQuery('');
                setHasSearched(false);
                setResultsOpen(false);
                searchTracks('', searchMode).catch(() => {});
              }}
            >
              ×
            </button>
          )}
        </div>

        <div
          id="search-mode-tabs"
          className="search-mode-tabs"
          role="tablist"
          aria-label="Search mode"
        >
          {SEARCH_TABS.map((tab) => {
            const active = searchMode === tab.mode;
            return (
              <button
                key={tab.mode}
                id={tab.id}
                role="tab"
                className={active ? 'active' : undefined}
                type="button"
                aria-selected={active}
                onClick={() => handleModeClick(tab.mode)}
              >
                {tab.label}
              </button>
            );
          })}
        </div>

        {searchHistory.length > 0 && !searchQuery.trim() && (
          <div id="search-history" className="search-history" aria-label="搜索历史">
            {searchHistory.slice(0, 8).map((kw) => (
              <button
                key={kw}
                type="button"
                className="search-history-chip"
                onClick={() => {
                  setSearchQuery(kw);
                  runSearch(kw, searchMode).catch(() => {});
                }}
              >
                {kw}
              </button>
            ))}
            <button
              type="button"
              className="search-history-chip clear"
              onClick={() => clearSearchHistory()}
              aria-label="清空搜索历史"
            >
              清空
            </button>
          </div>
        )}

        {podcastBrowse && (
          <div id="podcast-browse" className="show search-podcast-browse">
            {hotLoading ? (
              <div className="search-empty">正在读取热门电台...</div>
            ) : hotRadios.length === 0 ? (
              <div className="search-empty">暂无热门电台</div>
            ) : (
              hotRadios.map((radio) => (
                <div key={String(radio.id)} className="podcast-radio-block">
                  <div
                    className="search-result"
                    role="button"
                    tabIndex={0}
                    onClick={() => {
                      toggleRadio(radio).catch(() => {});
                    }}
                  >
                    {radio.cover ? (
                      <img src={coverProxyUrl(radio.cover)} alt={radio.name} loading="lazy" />
                    ) : (
                      <div style={{ width: 40, height: 40, flexShrink: 0 }}>♪</div>
                    )}
                    <div className="search-result-info">
                      <div className="search-result-title">
                        <span>{radio.name}</span>
                        <span className="tag-source">播客</span>
                      </div>
                      <div className="search-result-meta">
                        {radio.dj || radio.desc || ''}
                        {radio.programCount ? ` · ${radio.programCount} 期` : ''}
                      </div>
                    </div>
                    <span aria-hidden="true">{openRadioId === radio.id ? '▾' : '▸'}</span>
                  </div>
                  {openRadioId === radio.id && (
                    <div className="podcast-programs">
                      {programsLoading ? (
                        <div className="search-empty">正在读取节目...</div>
                      ) : radioPrograms.length === 0 ? (
                        <div className="search-empty">暂无节目</div>
                      ) : (
                        radioPrograms.map((prog, i) => (
                          <div
                            key={songIdentityKey(prog) || i}
                            className="search-result"
                            role="button"
                            tabIndex={0}
                            onClick={() => {
                              onSongSelected?.(prog);
                              playSong(prog, radioPrograms).catch(() => {});
                            }}
                          >
                            <div className="search-result-info">
                              <div className="search-result-title">
                                <span>{prog.name || prog.title || '未命名节目'}</span>
                              </div>
                              <div className="search-result-meta">{prog.artist || radio.name}</div>
                            </div>
                            <button
                              type="button"
                              className="fx-mini-btn ghost"
                              style={{ height: 26, padding: '0 8px', fontSize: 11 }}
                              onClick={(e) => handleAppendQueue(prog, e)}
                            >
                              +队列
                            </button>
                          </div>
                        ))
                      )}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        )}

        <div id="search-results" className={showResults ? 'show' : undefined}>
          {searchLoading ? (
            <div className="search-empty">正在检索曲库...</div>
          ) : pagedResults.length === 0 ? (
            <div className="search-empty">未找到匹配结果，请尝试更换关键词或平台</div>
          ) : (
            <>
              {pagedResults.map((song, idx) => {
                const key = songIdentityKey(song) || `${song.name}-${idx}`;
                const coverSrc = song.cover ? coverProxyUrl(song.cover) : '';
                const isVip = Boolean(song.vipRequired || song.needVip || song.onlyVipPlayable || song.fee === 1);
                return (
                  <div
                    key={key}
                    className="search-result"
                    role="button"
                    tabIndex={0}
                    onClick={() => {
                      handlePlayResult(song).catch(() => {});
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        handlePlayResult(song).catch(() => {});
                      }
                    }}
                  >
                    {coverSrc ? (
                      <img src={coverSrc} alt={song.name} loading="lazy" />
                    ) : (
                      <div
                        style={{
                          width: 40,
                          height: 40,
                          borderRadius: 6,
                          background: 'rgba(255,255,255,0.08)',
                          display: 'grid',
                          placeItems: 'center',
                          fontSize: 12,
                          color: 'rgba(255,255,255,0.55)',
                          flexShrink: 0,
                        }}
                      >
                        ♪
                      </div>
                    )}
                    <div className="search-result-info">
                      <div className="search-result-title">
                        <span>{song.name || song.title || '未知曲目'}</span>
                        <span className="tag-source">{providerLabel(song)}</span>
                        {isVip && <span className="tag-vip">VIP</span>}
                      </div>
                      <div className="search-result-meta">
                        {song.artist || '未知歌手'}
                        {song.album ? ` · ${song.album}` : ''}
                      </div>
                    </div>
                    <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
                      <button
                        type="button"
                        className="fx-mini-btn ghost"
                        style={{ height: 26, padding: '0 8px', fontSize: 11 }}
                        title="加入播放队列"
                        onClick={(e) => handleAppendQueue(song, e)}
                      >
                        +队列
                      </button>
                      <button
                        type="button"
                        className={`fx-mini-btn ghost search-like-btn${
                          likedKeys[songIdentityKey(song)] ? ' liked' : ''
                        }`}
                        style={{ height: 26, padding: '0 8px', fontSize: 11 }}
                        title="喜欢"
                        aria-label="喜欢"
                        aria-pressed={Boolean(likedKeys[songIdentityKey(song)])}
                        onClick={(e) => {
                          handleLikeResult(song, e).catch(() => {});
                        }}
                      >
                        {likedKeys[songIdentityKey(song)] ? '♥' : '♡'}
                      </button>
                      {onOpenCollect && (
                        <button
                          type="button"
                          className="fx-mini-btn ghost"
                          style={{ height: 26, padding: '0 8px', fontSize: 11 }}
                          title="收藏到内置歌单"
                          onClick={(e) => {
                            e.stopPropagation();
                            onOpenCollect(song);
                          }}
                        >
                          收藏
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}

              {totalPages > 1 && (
                <div
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    padding: '8px 14px',
                    borderTop: '1px solid rgba(255,255,255,0.06)',
                    fontSize: 11,
                    color: 'rgba(255,255,255,0.6)',
                  }}
                >
                  <button
                    type="button"
                    className="fx-mini-btn ghost"
                    disabled={safePage <= 1}
                    onClick={() => setPage((p) => Math.max(1, p - 1))}
                  >
                    上一页
                  </button>
                  <span>
                    第 {safePage} / {totalPages} 页（共 {searchResults.length} 首）
                  </span>
                  <button
                    type="button"
                    className="fx-mini-btn ghost"
                    disabled={safePage >= totalPages}
                    onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                  >
                    下一页
                  </button>
                </div>
              )}
            </>
          )}
        </div>
      </div>

      <div id="upload-actions">
        <button
          id="upload-btn"
          className="icon-btn"
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setUploadPanelOpen((v) => !v);
          }}
          title="导入音乐或封面"
          aria-label="导入音乐或封面"
          aria-expanded={uploadPanelOpen}
        >
          <svg
            width="18"
            height="18"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            viewBox="0 0 24 24"
            aria-hidden="true"
          >
            <path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4" />
            <polyline points="17 8 12 3 7 8" />
            <line x1="12" y1="3" x2="12" y2="15" />
          </svg>
        </button>

        <button
          id="clear-cover-btn"
          className="icon-btn"
          type="button"
          onClick={handleClearCustomCover}
          title="取消自定义封面"
          aria-label="取消自定义封面"
        >
          ×
        </button>

        <div
          id="upload-panel"
          className={`upload-panel${uploadPanelOpen ? ' show' : ''}`}
          onClick={(e) => e.stopPropagation()}
          aria-label="导入面板"
        >
          <button
            className="upload-choice"
            type="button"
            onClick={() => coverInputRef.current?.click()}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="3" y="4" width="18" height="16" rx="3" />
              <circle cx="8.5" cy="9.5" r="1.5" />
              <path d="M21 16l-5.4-5.4a2 2 0 0 0-2.8 0L5 18" />
            </svg>
            <span>
              <strong>封面图片</strong>
              <small>给当前歌曲换封面</small>
            </span>
          </button>
          <button
            className="upload-choice"
            type="button"
            onClick={() => audioInputRef.current?.click()}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M9 18V5l12-2v13" />
              <circle cx="6" cy="18" r="3" />
              <circle cx="18" cy="16" r="3" />
            </svg>
            <span>
              <strong>单曲文件</strong>
              <small>导入并立即播放</small>
            </span>
          </button>
          <button
            className="upload-choice"
            type="button"
            onClick={() => folderInputRef.current?.click()}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
              <path d="M8 13h8" />
              <path d="M12 9v8" />
            </svg>
            <span>
              <strong>多曲文件夹</strong>
              <small>批量导入到队列</small>
            </span>
          </button>
        </div>

        {uploadTipOpen && (
          <div id="upload-tip" className="show" role="status" aria-live="polite">
            <button
              className="upload-tip-close"
              type="button"
              onClick={() => setUploadTipOpen(false)}
              aria-label="关闭提示"
            >
              ×
            </button>
            <span className="upload-tip-title">导入入口</span>
            这里支持上传歌曲，也可以给当前曲目换自定义封面。
          </div>
        )}

        <input
          ref={audioInputRef}
          id="file-input"
          type="file"
          accept=".mp3,.flac,.wav,.ogg,.m4a,.aac,.opus,audio/*"
          multiple
          style={{ display: 'none' }}
          onChange={(e) => {
            handleAudioImport(e.target.files).catch(() => {});
            e.target.value = '';
          }}
        />
        <input
          ref={coverInputRef}
          id="cover-input"
          type="file"
          accept=".jpg,.jpeg,.png,.webp,image/jpeg,image/png,image/webp"
          style={{ display: 'none' }}
          onChange={(e) => {
            handleCustomCoverChange(e.target.files);
            e.target.value = '';
          }}
        />
        <input
          ref={folderInputRef}
          id="folder-input"
          type="file"
          accept=".mp3,.flac,.wav,.ogg,.m4a,.aac,.opus,audio/*"
          multiple
          // @ts-expect-error webkitdirectory is supported by Chromium/WebView
          webkitdirectory=""
          directory=""
          style={{ display: 'none' }}
          onChange={(e) => {
            handleAudioImport(e.target.files).catch(() => {});
            e.target.value = '';
          }}
        />
      </div>
    </div>
  );
}

export default SearchBar;
