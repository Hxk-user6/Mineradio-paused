import React, { useEffect, useMemo, useRef, useState } from 'react';
import { apiJson, coverProxyUrl } from '../bridge/tauriBridge';
import { songIdentityKey } from '../engines';
import { computeNextTrackIndex, usePlayerStore } from '../store/playerStore';
import type { ProviderType, SongRecord } from '../types/mineradio';

export interface HomeDashboardProps {
  onOpenLibrary?: () => void;
  onOpenPlayerConsole?: () => void;
  onExitHome?: () => void;
  onToast?: (message: string) => void;
}

const DAILY_REVIEWS = [
  { text: '有些歌不是突然好听，而是终于听懂了。', source: '每日热评' },
  { text: '慢一点没关系，重要的是一直在向喜欢的生活靠近。', source: '每日热评' },
  { text: '错过落日余晖，还会有满天星辰。', source: '每日热评' },
  { text: '保持热爱，奔赴下一场山海。', source: '每日热评' },
  { text: '答案在路上，自由在风里。', source: '每日热评' },
  { text: '让今天的声音，从你喜欢的地方开始。', source: 'Mineradio' },
];

type RecommendPlatform = 'netease' | 'qishui' | 'qq' | 'kugou';

const RECOMMEND_PLATFORMS: Array<{ key: RecommendPlatform; label: string }> = [
  { key: 'netease', label: '网易云' },
  { key: 'qishui', label: '汽水' },
  { key: 'qq', label: 'QQ' },
  { key: 'kugou', label: '酷狗' },
];

function dailyList(d: { dailySongs?: SongRecord[]; songs?: SongRecord[] } | null | undefined): SongRecord[] {
  if (d?.dailySongs?.length) return d.dailySongs;
  if (d?.songs?.length) return d.songs;
  return [];
}

export function HomeDashboard({
  onOpenLibrary,
  onOpenPlayerConsole,
  onExitHome,
  onToast,
}: HomeDashboardProps): React.ReactElement {
  const homeDiscover = usePlayerStore((s) => s.homeDiscover);
  const weatherRadio = usePlayerStore((s) => s.weatherRadio);
  const todayListeningStats = usePlayerStore((s) => s.todayListeningStats);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const currentIdx = usePlayerStore((s) => s.currentIdx);
  const playMode = usePlayerStore((s) => s.playMode);
  const localTracks = usePlayerStore((s) => s.localTracks);
  const playSong = usePlayerStore((s) => s.playSong);
  const togglePlay = usePlayerStore((s) => s.togglePlay);
  const refreshHomeDiscover = usePlayerStore((s) => s.refreshHomeDiscover);

  const dailySongs = useMemo(() => dailyList(homeDiscover), [homeDiscover]);
  const radioSongs = useMemo<SongRecord[]>(
    () => (weatherRadio?.radio?.songs?.length ? weatherRadio.radio.songs : weatherRadio?.tracks || []),
    [weatherRadio]
  );
  const weatherObj =
    weatherRadio?.weather && typeof weatherRadio.weather === 'object' ? weatherRadio.weather : null;
  const weatherCity = weatherObj?.city || weatherRadio?.city || '';
  const weatherTemp = weatherObj?.temperature ?? weatherRadio?.temperature;
  const weatherSummary =
    weatherObj?.summary ||
    (typeof weatherRadio?.weather === 'string' ? weatherRadio.weather : '');
  const radioTitle = weatherRadio?.radio?.title || '';
  const radioSubtitle = weatherRadio?.radio?.subtitle || weatherRadio?.greeting || '';
  const homePlaylistCount = (homeDiscover?.playlists?.length || 0) + (homeDiscover?.podcasts?.length || 0);

  const [now, setNow] = useState<Date>(() => new Date());
  const [reviewOffset, setReviewOffset] = useState(0);
  const [videoUrl, setVideoUrl] = useState<string>('');
  const [videoName, setVideoName] = useState<string>('');
  const [platformModalOpen, setPlatformModalOpen] = useState(false);
  const [platformTab, setPlatformTab] = useState<RecommendPlatform>('netease');
  const [platformSongs, setPlatformSongs] = useState<SongRecord[]>([]);
  const [platformLoading, setPlatformLoading] = useState(false);
  const [platformStatus, setPlatformStatus] = useState('');

  const videoInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    const timer = setInterval(() => {
      setNow(new Date());
    }, 30000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    return () => {
      if (videoUrl && videoUrl.startsWith('blob:')) {
        try {
          URL.revokeObjectURL(videoUrl);
        } catch {
          // ignore
        }
      }
    };
  }, [videoUrl]);

  const timeStr = `${String(now.getHours()).padStart(2, '0')}:${String(now.getMinutes()).padStart(
    2,
    '0'
  )}`;
  const dateStr = now.toLocaleDateString('zh-CN', {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    weekday: 'long',
  });

  const dayNumber = Math.floor(
    new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 86400000
  );
  const review =
    DAILY_REVIEWS[
      (((dayNumber + reviewOffset) % DAILY_REVIEWS.length) + DAILY_REVIEWS.length) %
        DAILY_REVIEWS.length
    ];

  const nextIdx = computeNextTrackIndex(playQueue.length, currentIdx, playMode);
  const nextSong = nextIdx >= 0 ? playQueue[nextIdx] : null;

  const discoveryTracks = useMemo<SongRecord[]>(() => {
    const candidates: SongRecord[] = [];
    if (dailySongs.length) candidates.push(...dailySongs);
    if (radioSongs.length) candidates.push(...radioSongs);
    if (localTracks.length) candidates.push(...localTracks);
    const seen = new Set<string>();
    const deduped: SongRecord[] = [];
    for (const track of candidates) {
      const k = songIdentityKey(track);
      if (k && seen.has(k)) continue;
      if (k) seen.add(k);
      deduped.push(track);
      if (deduped.length >= 3) break;
    }
    return deduped;
  }, [dailySongs, radioSongs, localTracks]);

  const totalMinutes = Math.floor((todayListeningStats.totalSeconds || 0) / 60);
  const topArtist = todayListeningStats.topArtists?.[0]?.name || '等待记录';

  const handleVideoFile = (files: FileList | null) => {
    const file = files?.[0];
    if (!file) return;
    if (!/\.mp4$/i.test(file.name) && file.type !== 'video/mp4') {
      onToast?.('请选择 MP4 视频文件');
      return;
    }
    if (videoUrl && videoUrl.startsWith('blob:')) {
      try {
        URL.revokeObjectURL(videoUrl);
      } catch {
        // ignore
      }
    }
    const objectUrl = typeof URL !== 'undefined' && URL.createObjectURL ? URL.createObjectURL(file) : '';
    setVideoUrl(objectUrl);
    setVideoName(file.name);
    onToast?.(`已加载首页背景视频：${file.name}`);
  };

  const handleClearVideo = () => {
    if (videoUrl && videoUrl.startsWith('blob:')) {
      try {
        URL.revokeObjectURL(videoUrl);
      } catch {
        // ignore
      }
    }
    setVideoUrl('');
    setVideoName('');
    onToast?.('已移除首页背景视频');
  };

  const handleResumePlayback = async () => {
    if (playQueue.length > 0) {
      onExitHome?.();
      await togglePlay();
    } else if (discoveryTracks.length > 0) {
      onExitHome?.();
      await playSong(discoveryTracks[0], discoveryTracks);
    } else {
      onOpenLibrary?.();
    }
  };

  const handlePlayDaily = async () => {
    const list = dailySongs.length ? dailySongs : radioSongs;
    if (list.length > 0) {
      onExitHome?.();
      await playSong(0, list);
    } else {
      await refreshHomeDiscover();
      const updated = dailyList(usePlayerStore.getState().homeDiscover);
      if (updated.length > 0) {
        onExitHome?.();
        await playSong(0, updated);
      } else {
        onToast?.('暂无每日推荐数据，请先登录或刷新推荐');
      }
    }
  };

  const handlePlayWeatherRadio = async () => {
    const tracks = radioSongs;
    if (tracks.length > 0) {
      onExitHome?.();
      await playSong(0, tracks);
    } else {
      openPlatformModal('netease');
    }
  };

  const fetchPlatformRecommendations = async (source: RecommendPlatform) => {
    setPlatformLoading(true);
    setPlatformStatus('正在读取平台推荐...');
    try {
      if (source === 'qq') {
        // QQ 推荐不再调用 /api/qq/recommend，统一使用 /api/discover/home 的聚合结果
        const fallback = dailyList(usePlayerStore.getState().homeDiscover);
        setPlatformSongs(fallback);
        setPlatformStatus(
          fallback.length ? `已加载 ${fallback.length} 首每日推荐` : 'QQ 暂无独立推荐，请使用每日推荐'
        );
        return;
      }
      let endpoint = '/api/discover/home';
      if (source === 'qishui') endpoint = '/api/qishui/feed?limit=12';
      else if (source === 'kugou') endpoint = '/api/kugou/recommendations?limit=12';

      const res = await apiJson<{
        dailySongs?: SongRecord[];
        songs?: SongRecord[];
        tracks?: SongRecord[];
        data?: { songs?: SongRecord[] };
        message?: string;
      }>(endpoint, { timeoutMs: 8000 });

      const list = res.dailySongs?.length ? res.dailySongs : res.songs || res.tracks || res.data?.songs || [];
      if (list.length > 0) {
        setPlatformSongs(list);
        setPlatformStatus(`已加载 ${list.length} 首推荐曲目`);
      } else if (source === 'netease' && dailySongs.length) {
        setPlatformSongs(dailySongs);
        setPlatformStatus(`已加载 ${dailySongs.length} 首每日推荐`);
      } else {
        setPlatformSongs([]);
        setPlatformStatus(res.message || '当前平台暂无可用推荐，请确认已登录或后端支持该接口');
      }
    } catch {
      if (source === 'netease' && dailySongs.length) {
        setPlatformSongs(dailySongs);
        setPlatformStatus(`已加载 ${dailySongs.length} 首本地缓存推荐`);
      } else {
        setPlatformSongs([]);
        setPlatformStatus('暂无可用推荐接口');
      }
    } finally {
      setPlatformLoading(false);
    }
  };

  const openPlatformModal = (source: RecommendPlatform = 'netease') => {
    setPlatformTab(source);
    setPlatformModalOpen(true);
    fetchPlatformRecommendations(source).catch(() => {});
  };

  return (
    <>
      <section id="empty-home" aria-label="Mineradio 音乐首页">
        <div className="empty-home-shell">
          <div className="home-hero" aria-live="polite">
            <div className="daily-review-card">
              {videoUrl && (
                <video
                  className="home-dashboard-video"
                  src={videoUrl}
                  autoPlay
                  muted
                  loop
                  playsInline
                />
              )}
              <div id="daily-review-date" className="daily-review-date">
                {dateStr}
              </div>
              <div id="daily-review-time" className="daily-review-time">
                {timeStr}
              </div>
              <div className="daily-review-quote">“{review.text}”</div>
              <div className="daily-review-source">— {review.source}</div>
              <div className="daily-review-actions">
                <button
                  type="button"
                  onClick={() => setReviewOffset((n) => n + 1)}
                >
                  换一条
                </button>
                <button
                  id="home-dashboard-video-choose"
                  type="button"
                  title={videoName ? `当前视频: ${videoName}` : '选择本地 MP4 背景'}
                  onClick={() => videoInputRef.current?.click()}
                >
                  {videoUrl ? '更换 MP4' : '选择 MP4'}
                </button>
                <button
                  id="home-dashboard-video-clear"
                  type="button"
                  hidden={!videoUrl}
                  onClick={handleClearVideo}
                >
                  移除视频
                </button>
                <button
                  type="button"
                  onClick={() => {
                    onExitHome?.();
                    onOpenPlayerConsole?.();
                  }}
                >
                  展开播放器控制台
                </button>
              </div>
            </div>
            <input
              ref={videoInputRef}
              id="home-dashboard-video-input"
              type="file"
              accept=".mp4,video/mp4"
              hidden
              aria-hidden="true"
              onChange={(e) => {
                handleVideoFile(e.target.files);
                e.target.value = '';
              }}
            />
          </div>

          <div className="home-grid home-quick-grid" aria-label="常用音乐入口">
            <button
              className="home-card home-card-featured"
              data-home-tone="search"
              type="button"
              onClick={() => {
                handleResumePlayback().catch(() => {});
              }}
            >
              <div className="home-card-label">CONTINUE</div>
              <div className="home-card-title">继续播放</div>
              <div className="home-card-sub">
                {playQueue.length > 0
                  ? `当前队列共 ${playQueue.length} 首曲目`
                  : '从当前队列或最近播放继续'}
              </div>
              <div className="home-card-art" />
            </button>

            <button
              className="home-card home-card-quick"
              data-home-tone="library"
              type="button"
              onClick={() => onOpenLibrary?.()}
            >
              <div className="home-card-label">LIBRARY</div>
              <div className="home-card-title">音乐库</div>
              <div className="home-card-sub">歌单、本地音乐和已登录平台</div>
              <div className="home-card-art" />
            </button>

            <button
              className="home-card home-card-quick"
              data-home-tone="mix"
              type="button"
              onClick={() => {
                handlePlayDaily().catch(() => {});
              }}
            >
              <div className="home-card-label">DAILY MIX</div>
              <div className="home-card-title">每日推荐</div>
              <div className="home-card-sub">
                {dailySongs.length
                  ? `${dailySongs.length} 首推荐已就绪`
                  : '使用当前 Mineradio 推荐数据'}
              </div>
              <div className="home-card-art" />
            </button>

            <button
              className="home-card home-card-quick"
              data-home-tone="playlist"
              type="button"
              onClick={() => {
                if (playQueue.length > 0) {
                  onExitHome?.();
                  playSong(0, playQueue).catch(() => {});
                } else {
                  onOpenLibrary?.();
                }
              }}
            >
              <div className="home-card-label">RECENT</div>
              <div className="home-card-title">最近播放</div>
              <div className="home-card-sub">播放过的歌曲会出现在这里</div>
              <div className="home-card-art" />
            </button>
          </div>

          <div className="home-rail home-insight-rail">
            <div id="home-insight-dock" className="home-insight-dock" aria-label="今日聆听和音乐发现">
              <section className="home-insight-card home-listen-card" aria-labelledby="home-listen-heading">
                <div className="home-insight-head">
                  <div id="home-listen-heading" className="home-insight-kicker">
                    LISTENING TODAY · 今日聆听
                  </div>
                  <button
                    className="home-insight-link"
                    type="button"
                    onClick={() => refreshHomeDiscover().catch(() => {})}
                  >
                    刷新推荐
                  </button>
                </div>
                <div className="home-listen-metrics">
                  <div className="home-listen-metric">
                    <strong id="home-today-time" className="home-listen-value">
                      {totalMinutes} 分钟
                    </strong>
                    <span className="home-listen-label">聆听时长</span>
                  </div>
                  <div className="home-listen-metric">
                    <strong id="home-today-count" className="home-listen-value">
                      {todayListeningStats.playCount} 首
                    </strong>
                    <span className="home-listen-label">今日歌曲</span>
                  </div>
                  <div className="home-listen-metric">
                    <strong id="home-today-artist" className="home-listen-value">
                      {topArtist}
                    </strong>
                    <span id="home-today-streak" className="home-listen-label">
                      {todayListeningStats.playCount > 0
                        ? `今日常听歌手${
                            (todayListeningStats.topArtists?.length || 0) > 1
                              ? ` · ${todayListeningStats.topArtists!
                                  .slice(1, 3)
                                  .map((a) => a.name)
                                  .join(' / ')}`
                              : ''
                          }`
                        : '开始播放后生成'}
                    </span>
                  </div>
                </div>
              </section>

              <button
                id="home-next-card"
                className="home-insight-card home-next-card"
                type="button"
                onClick={() => {
                  if (nextSong && nextIdx >= 0) {
                    onExitHome?.();
                    playSong(nextIdx).catch(() => {});
                  } else {
                    onOpenLibrary?.();
                  }
                }}
                aria-label="播放下一首"
              >
                <span
                  id="home-next-cover"
                  className="home-next-cover"
                  aria-hidden="true"
                  style={
                    nextSong?.cover
                      ? { backgroundImage: `url("${coverProxyUrl(nextSong.cover)}")` }
                      : undefined
                  }
                />
                <span className="home-next-copy">
                  <span className="home-insight-kicker">NEXT UP · 接下来播放</span>
                  <span id="home-next-title" className="home-next-title">
                    {nextSong ? nextSong.name : '队列里还没有歌曲'}
                  </span>
                  <span id="home-next-sub" className="home-next-sub">
                    {nextSong ? nextSong.artist || '未知歌手' : '点击打开音乐库'}
                  </span>
                </span>
                <span className="home-next-arrow" aria-hidden="true">
                  ›
                </span>
              </button>

              <section className="home-discovery-strip" aria-label="为你挑选">
                <div className="home-discovery-copy">
                  <span className="home-insight-kicker">FOR YOU · 为你挑选</span>
                  <span className="home-discovery-title">每日推荐歌曲</span>
                  <span className="home-discovery-sub">从每日推荐、天气电台与本地音乐中挑选</span>
                </div>
                <div
                  id="home-discovery-list"
                  className={`home-discovery-list${discoveryTracks.length === 0 ? ' is-empty' : ''}`}
                  aria-live="polite"
                >
                  {discoveryTracks.length === 0 ? (
                    <button
                      type="button"
                      className="home-discovery-empty"
                      onClick={() => openPlatformModal('netease')}
                    >
                      <strong>暂无推荐歌曲</strong>
                      <span>点击打开平台推荐或前往音乐库导入本地歌曲</span>
                    </button>
                  ) : (
                    discoveryTracks.map((track, idx) => (
                      <button
                        key={songIdentityKey(track) || idx}
                        type="button"
                        className="home-discovery-song"
                        onClick={() => {
                          onExitHome?.();
                          playSong(track, discoveryTracks).catch(() => {});
                        }}
                      >
                        <span
                          className="home-discovery-cover"
                          style={
                            track.cover
                              ? { backgroundImage: `url("${coverProxyUrl(track.cover)}")` }
                              : undefined
                          }
                        />
                        <span className="home-discovery-song-copy">
                          <span className="home-discovery-song-name">
                            {track.name || track.title || '未知曲目'}
                          </span>
                          <span className="home-discovery-song-artist">
                            {track.artist || '未知歌手'}
                          </span>
                        </span>
                      </button>
                    ))
                  )}
                </div>
              </section>

              <button
                className="home-insight-card home-ranking-entry"
                type="button"
                onClick={() => openPlatformModal('netease')}
                aria-label="打开音乐发现"
              >
                <span className="home-ranking-entry-copy">
                  <span className="home-insight-kicker">DISCOVER · 音乐发现</span>
                  <span className="home-ranking-entry-title">平台热歌与推荐中心</span>
                  <span className="home-ranking-entry-sub">
                    {homePlaylistCount > 0
                      ? `${homePlaylistCount} 个推荐歌单与播客电台`
                      : '查看跨平台每日推荐歌曲与精选歌单'}
                  </span>
                </span>
                <span className="home-ranking-entry-art" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                  <i />
                  <i />
                </span>
                <span className="home-next-arrow" aria-hidden="true">
                  ›
                </span>
              </button>

              <button
                className="home-insight-card home-ranking-entry home-radio-entry"
                type="button"
                onClick={() => {
                  handlePlayWeatherRadio().catch(() => {});
                }}
                aria-label="天气电台"
              >
                <span className="home-ranking-entry-copy">
                  <span className="home-insight-kicker">
                    WEATHER RADIO · 天气电台
                    {weatherCity ? ` · ${weatherCity}` : ''}
                  </span>
                  <span className="home-ranking-entry-title">
                    {radioTitle ||
                      (weatherSummary
                        ? `${weatherSummary}${weatherTemp != null ? ` ${weatherTemp}°` : ''}`
                        : '天气电台 / 平台推荐')}
                  </span>
                  <span className="home-ranking-entry-sub">
                    {radioSubtitle ||
                      (radioSongs.length
                        ? `${radioSongs.length} 首场景电台曲目，点击立即收听`
                        : '读取当前气候情绪曲目，或查看平台推荐')}
                  </span>
                </span>
                <span className="home-radio-entry-art" aria-hidden="true">
                  <i />
                  <i />
                </span>
                <span className="home-next-arrow" aria-hidden="true">
                  ›
                </span>
              </button>
            </div>
          </div>
        </div>
      </section>

      <div
        id="home-platform-recommend-mask"
        className={`modal-mask${platformModalOpen ? ' show' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-hidden={!platformModalOpen}
        aria-labelledby="home-platform-recommend-title"
        onClick={(e) => {
          if (e.target === e.currentTarget) setPlatformModalOpen(false);
        }}
      >
        <div className="modal home-platform-recommend-modal">
          <div className="home-platform-recommend-head">
            <div>
              <h2 id="home-platform-recommend-title">平台推荐</h2>
              <p>只读取平台可验证的推荐数据，不用关键词搜索替代。</p>
            </div>
            <button
              id="home-platform-recommend-close"
              className="home-platform-recommend-close"
              type="button"
              aria-label="关闭平台推荐"
              onClick={() => setPlatformModalOpen(false)}
            >
              ×
            </button>
          </div>

          <div
            id="home-platform-recommend-tabs"
            className="home-platform-recommend-tabs"
            role="tablist"
            aria-label="推荐平台"
          >
            {RECOMMEND_PLATFORMS.map((tab) => {
              const selected = platformTab === tab.key;
              return (
                <button
                  key={tab.key}
                  type="button"
                  role="tab"
                  data-home-recommend-source={tab.key}
                  aria-selected={selected}
                  onClick={() => {
                    setPlatformTab(tab.key);
                    fetchPlatformRecommendations(tab.key).catch(() => {});
                  }}
                >
                  {tab.label}
                </button>
              );
            })}
          </div>

          <div
            id="home-platform-recommend-status"
            className="home-platform-recommend-status"
            role="status"
            aria-live="polite"
          >
            {platformLoading ? '加载中...' : platformStatus}
          </div>

          <div id="home-platform-recommend-list" className="home-platform-recommend-list">
            {platformSongs.map((song, idx) => (
              <div
                key={songIdentityKey(song) || idx}
                className="search-result"
                onClick={() => {
                  setPlatformModalOpen(false);
                  onExitHome?.();
                  playSong(
                    { ...song, provider: (song.provider || platformTab) as ProviderType },
                    platformSongs
                  ).catch(() => {});
                }}
              >
                {song.cover ? (
                  <img src={coverProxyUrl(song.cover)} alt={song.name} loading="lazy" />
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
                    }}
                  >
                    ♪
                  </div>
                )}
                <div className="search-result-info">
                  <div className="search-result-title">{song.name || song.title || '未知曲目'}</div>
                  <div className="search-result-meta">{song.artist || '未知歌手'}</div>
                </div>
              </div>
            ))}
          </div>

          <div className="home-platform-recommend-actions">
            <button
              id="home-platform-recommend-refresh"
              type="button"
              onClick={() => {
                fetchPlatformRecommendations(platformTab).catch(() => {});
              }}
            >
              刷新当前平台
            </button>
            <button
              id="home-platform-recommend-done"
              type="button"
              onClick={() => setPlatformModalOpen(false)}
            >
              完成
            </button>
          </div>
        </div>
      </div>
    </>
  );
}

export default HomeDashboard;
