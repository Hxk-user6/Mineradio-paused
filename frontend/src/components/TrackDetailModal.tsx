import React, { useCallback, useEffect, useState } from 'react';
import { apiJson, coverProxyUrl } from '../bridge/tauriBridge';
import { songIdentityKey } from '../engines';
import { usePlayerStore } from '../store/playerStore';
import type { LyricLine, SongRecord } from '../types/mineradio';

export interface TrackDetailModalProps {
  onToast?: (message: string) => void;
}

export interface CommentItem {
  id: string;
  content: string;
  nickname: string;
  avatarUrl?: string;
  likedCount: number;
  liked: boolean;
  timeText?: string;
}

function normalizeComment(raw: Record<string, unknown>, idx: number): CommentItem {
  const user =
    raw.user && typeof raw.user === 'object' ? (raw.user as Record<string, unknown>) : {};
  const id = String(raw.commentId ?? raw.id ?? `c-${idx}`);
  const content = String(raw.content ?? raw.text ?? '');
  const nickname = String(
    user.nickname ?? user.name ?? raw.nickname ?? raw.userName ?? '听友'
  );
  const avatarUrl = String(user.avatarUrl ?? user.avatar ?? raw.avatarUrl ?? '');
  const likedCount = Number(raw.likedCount ?? raw.likeCount ?? raw.praiseCount ?? 0) || 0;
  const liked = Boolean(raw.liked);
  const timeText = raw.timeStr
    ? String(raw.timeStr)
    : typeof raw.time === 'number' && raw.time > 0
    ? new Date(raw.time).toLocaleDateString()
    : '';
  return {
    id,
    content,
    nickname,
    avatarUrl,
    likedCount,
    liked,
    timeText,
  };
}

function formatLyricsToLrcText(lines: LyricLine[]): string {
  return lines
    .map((line) => {
      const m = Math.floor(line.time / 60);
      const s = (line.time % 60).toFixed(2).padStart(5, '0');
      return `[${String(m).padStart(2, '0')}:${s}]${line.text}`;
    })
    .join('\n');
}

const PROVIDER_LABELS: Record<string, string> = {
  netease: '网易云音乐',
  qq: 'QQ 音乐',
  kugou: '酷狗音乐',
  qishui: '汽水音乐',
  local: '本地曲库',
  mineradio: 'Mineradio',
};

export const TrackDetailModal: React.FC<TrackDetailModalProps> = ({ onToast }) => {
  const trackDetailOpen = usePlayerStore((s) => s.trackDetailOpen);
  const trackDetailSong = usePlayerStore((s) => s.trackDetailSong);
  const currentSong = usePlayerStore((s) => s.currentSong);
  const lyrics = usePlayerStore((s) => s.lyrics);
  const customLyricModalOpen = usePlayerStore((s) => s.customLyricModalOpen);

  const closeTrackDetail = usePlayerStore((s) => s.closeTrackDetail);
  const setCustomLyricModalOpen = usePlayerStore((s) => s.setCustomLyricModalOpen);
  const saveCustomLyricForSong = usePlayerStore((s) => s.saveCustomLyricForSong);
  const searchTracks = usePlayerStore((s) => s.searchTracks);
  const playSong = usePlayerStore((s) => s.playSong);

  const activeSong = trackDetailSong || currentSong;
  const provider = String(activeSong?.provider || 'netease').toLowerCase();
  const songId = String(activeSong?.mid || activeSong?.id || activeSong?.hash || '');

  const [hotComments, setHotComments] = useState<CommentItem[]>([]);
  const [latestComments, setLatestComments] = useState<CommentItem[]>([]);
  const [commentsLoading, setCommentsLoading] = useState(false);
  const [commentInput, setCommentInput] = useState('');
  const [submittingComment, setSubmittingComment] = useState(false);

  const [crossCandidates, setCrossCandidates] = useState<SongRecord[]>([]);
  const [crossSearching, setCrossSearching] = useState(false);

  const [customLrcText, setCustomLrcText] = useState('');
  const [savingLyric, setSavingLyric] = useState(false);

  const loadComments = useCallback(async (song: SongRecord | null) => {
    if (!song) {
      setHotComments([]);
      setLatestComments([]);
      return;
    }
    const prov = String(song.provider || 'netease').toLowerCase();
    const idVal = String(song.mid || song.id || song.hash || '').trim();
    if (!idVal) {
      setHotComments([]);
      setLatestComments([]);
      return;
    }

    setCommentsLoading(true);
    try {
      const endpoint =
        prov === 'qq'
          ? `/api/qq/song/comments?id=${encodeURIComponent(idVal)}&mid=${encodeURIComponent(idVal)}`
          : prov === 'qishui'
          ? `/api/qishui/comments?id=${encodeURIComponent(idVal)}`
          : `/api/song/comments?id=${encodeURIComponent(String(song.id || idVal))}`;

      const res = await apiJson<{
        hotComments?: Array<Record<string, unknown>>;
        hot?: Array<Record<string, unknown>>;
        comments?: Array<Record<string, unknown>>;
        latestComments?: Array<Record<string, unknown>>;
        data?: {
          hotComments?: Array<Record<string, unknown>>;
          comments?: Array<Record<string, unknown>>;
        };
      }>(endpoint, { timeoutMs: 8000 });

      const rawHot = res.hotComments || res.hot || res.data?.hotComments || [];
      const rawLatest =
        res.comments || res.latestComments || res.data?.comments || [];

      setHotComments(Array.isArray(rawHot) ? rawHot.map(normalizeComment) : []);
      setLatestComments(Array.isArray(rawLatest) ? rawLatest.map(normalizeComment) : []);
    } catch {
      setHotComments([]);
      setLatestComments([]);
    } finally {
      setCommentsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (trackDetailOpen && activeSong) {
      setCrossCandidates([]);
      loadComments(activeSong).catch(() => {});
    }
  }, [trackDetailOpen, activeSong, loadComments]);

  useEffect(() => {
    if (customLyricModalOpen) {
      if (activeSong?.lyric) {
        setCustomLrcText(activeSong.lyric);
      } else if (lyrics.length > 0) {
        setCustomLrcText(formatLyricsToLrcText(lyrics));
      } else {
        setCustomLrcText('');
      }
    }
  }, [customLyricModalOpen, activeSong, lyrics]);

  const handlePostComment = async (e?: React.FormEvent) => {
    e?.preventDefault();
    const text = commentInput.trim();
    if (!text || !activeSong || submittingComment) return;

    setSubmittingComment(true);
    try {
      const endpoint =
        provider === 'qq'
          ? '/api/qq/song/comments'
          : provider === 'qishui'
          ? '/api/qishui/comments'
          : '/api/song/comments';

      await apiJson(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: activeSong.id ?? songId,
          mid: activeSong.mid ?? songId,
          provider,
          content: text,
        }),
      });

      const created: CommentItem = {
        id: `local-${Date.now()}`,
        content: text,
        nickname: '我',
        likedCount: 0,
        liked: false,
        timeText: '刚刚',
      };
      setLatestComments((prev) => [created, ...prev]);
      setCommentInput('');
      onToast?.('评论已发送');
    } catch (err) {
      onToast?.(err instanceof Error ? err.message : '发送评论失败');
    } finally {
      setSubmittingComment(false);
    }
  };

  const handleToggleLikeComment = async (comment: CommentItem, isHot: boolean) => {
    if (!activeSong) return;
    const nextLiked = !comment.liked;
    const updateList = (list: CommentItem[]) =>
      list.map((item) =>
        item.id === comment.id
          ? {
              ...item,
              liked: nextLiked,
              likedCount: Math.max(0, item.likedCount + (nextLiked ? 1 : -1)),
            }
          : item
      );

    if (isHot) setHotComments(updateList);
    else setLatestComments(updateList);

    try {
      await apiJson('/api/song/comments/like', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: activeSong.id ?? songId,
          cid: comment.id,
          liked: nextLiked,
          provider,
        }),
      });
    } catch {
      // optimistic UI update preserved
    }
  };

  const handleCrossPlatformSearch = async () => {
    if (!activeSong || crossSearching) return;
    const kw = `${activeSong.name || ''} ${activeSong.artist || ''}`.trim();
    if (!kw) return;
    setCrossSearching(true);
    try {
      const results = await searchTracks(kw, 'song');
      setCrossCandidates(Array.isArray(results) ? results.slice(0, 10) : []);
    } catch {
      setCrossCandidates([]);
    } finally {
      setCrossSearching(false);
    }
  };

  const handleSaveCustomLyric = async () => {
    if (savingLyric) return;
    const target = activeSong || currentSong;
    const key =
      (target && (songIdentityKey(target) || String(target.id || target.name || ''))) ||
      'custom-song';
    setSavingLyric(true);
    try {
      await saveCustomLyricForSong(key, customLrcText);
      setCustomLyricModalOpen(false);
      onToast?.('自定义歌词已保存并生效');
    } finally {
      setSavingLyric(false);
    }
  };

  const coverUrl = activeSong?.cover ? coverProxyUrl(activeSong.cover) : '';
  const providerLabel = PROVIDER_LABELS[provider] || provider.toUpperCase();

  return (
    <>
      {trackDetailOpen && (
        <div
          id="track-detail-modal"
          className="modal-mask show"
          role="dialog"
          aria-modal="true"
          aria-label="歌曲详情与评论"
          onClick={(e) => {
            if (e.target === e.currentTarget) closeTrackDetail();
          }}
        >
          <div className="modal track-detail-dialog">
            <div className="track-detail-head" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <b>歌曲详情与评论</b>
              <button
                type="button"
                className="login-panel-close"
                aria-label="关闭歌曲详情"
                onClick={closeTrackDetail}
              >
                ×
              </button>
            </div>

            {activeSong ? (
              <div className="track-detail-meta" style={{ display: 'flex', gap: 14, alignItems: 'center', marginTop: 12 }}>
                {coverUrl ? (
                  <img
                    src={coverUrl}
                    alt={activeSong.name}
                    className="track-detail-cover"
                    style={{ width: 72, height: 72, borderRadius: 12, objectFit: 'cover' }}
                  />
                ) : (
                  <div
                    className="track-detail-cover-placeholder"
                    style={{ width: 72, height: 72, borderRadius: 12, background: 'rgba(255,255,255,0.08)' }}
                  />
                )}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <h3 style={{ margin: '0 0 4px' }}>{activeSong.name}</h3>
                  <div style={{ fontSize: 13, opacity: 0.85 }}>
                    歌手：{activeSong.artist || '未知歌手'}
                  </div>
                  <div style={{ fontSize: 12, opacity: 0.72 }}>
                    专辑：{activeSong.album || '未知专辑'} · 平台：{providerLabel}
                  </div>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                  <button
                    id="track-detail-cross-search-btn"
                    type="button"
                    className="modal-btn"
                    disabled={crossSearching}
                    onClick={() => {
                      handleCrossPlatformSearch().catch(() => {});
                    }}
                  >
                    {crossSearching ? '检索中...' : '跨平台搜寻同曲'}
                  </button>
                  <button
                    type="button"
                    className="modal-btn"
                    onClick={() => setCustomLyricModalOpen(true)}
                  >
                    编辑歌词 (.lrc)
                  </button>
                </div>
              </div>
            ) : (
              <div style={{ padding: '16px 0', opacity: 0.7 }}>当前未选择曲目</div>
            )}

            {crossCandidates.length > 0 && (
              <div
                id="track-detail-cross-results"
                className="track-detail-cross-results"
                style={{ marginTop: 12, padding: 10, borderRadius: 10, background: 'rgba(255,255,255,0.05)' }}
              >
                <div style={{ fontSize: 12, fontWeight: 700, marginBottom: 6 }}>
                  跨平台同曲候选
                </div>
                <div style={{ display: 'grid', gap: 6, maxHeight: 160, overflowY: 'auto' }}>
                  {crossCandidates.map((cand, i) => (
                    <div
                      key={`${cand.provider || 'ne'}-${cand.id || i}`}
                      style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, fontSize: 12 }}
                    >
                      <span>
                        [{PROVIDER_LABELS[String(cand.provider || 'netease')] || cand.provider || '网易云'}] {cand.name} - {cand.artist || '未知歌手'}
                      </span>
                      <button
                        type="button"
                        className="fx-mini-btn"
                        onClick={() => {
                          playSong(cand).catch(() => {});
                          onToast?.(`已切换播放：${cand.name}`);
                        }}
                      >
                        切换播放
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            )}

            <form
              id="track-detail-comment-form"
              onSubmit={handlePostComment}
              style={{ display: 'flex', gap: 8, marginTop: 14 }}
            >
              <input
                id="track-detail-comment-input"
                type="text"
                className="fx-console-search"
                style={{ flex: 1 }}
                placeholder="写下你的听歌评论..."
                aria-label="评论内容"
                value={commentInput}
                onChange={(e) => setCommentInput(e.target.value)}
              />
              <button
                type="submit"
                className="modal-btn primary"
                disabled={submittingComment || !commentInput.trim()}
              >
                发表评论
              </button>
            </form>

            <div
              className="track-detail-comments"
              style={{ marginTop: 14, maxHeight: 320, overflowY: 'auto', display: 'grid', gap: 12 }}
            >
              {commentsLoading && <div style={{ fontSize: 12, opacity: 0.7 }}>正在加载评论...</div>}

              {hotComments.length > 0 && (
                <section aria-label="热门评论">
                  <div style={{ fontSize: 12, fontWeight: 700, marginBottom: 6 }}>
                    热门评论 ({hotComments.length})
                  </div>
                  <div style={{ display: 'grid', gap: 8 }}>
                    {hotComments.map((c) => (
                      <div
                        key={`hot-${c.id}`}
                        className="track-comment-item"
                        style={{ padding: 8, borderRadius: 8, background: 'rgba(255,255,255,0.04)' }}
                      >
                        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: 12 }}>
                          <b>{c.nickname}</b>
                          <button
                            type="button"
                            className={`fx-mini-btn ghost${c.liked ? ' active' : ''}`}
                            aria-label={`点赞评论 ${c.id}`}
                            onClick={() => {
                              handleToggleLikeComment(c, true).catch(() => {});
                            }}
                          >
                            ♥ {c.likedCount}
                          </button>
                        </div>
                        <div style={{ fontSize: 13, marginTop: 4 }}>{c.content}</div>
                      </div>
                    ))}
                  </div>
                </section>
              )}

              <section aria-label="最新评论">
                <div style={{ fontSize: 12, fontWeight: 700, marginBottom: 6 }}>
                  最新评论 ({latestComments.length})
                </div>
                {latestComments.length === 0 && !commentsLoading ? (
                  <div style={{ fontSize: 12, opacity: 0.6 }}>暂无评论</div>
                ) : (
                  <div style={{ display: 'grid', gap: 8 }}>
                    {latestComments.map((c) => (
                      <div
                        key={`latest-${c.id}`}
                        className="track-comment-item"
                        style={{ padding: 8, borderRadius: 8, background: 'rgba(255,255,255,0.04)' }}
                      >
                        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: 12 }}>
                          <b>{c.nickname}</b>
                          <button
                            type="button"
                            className={`fx-mini-btn ghost${c.liked ? ' active' : ''}`}
                            aria-label={`点赞评论 ${c.id}`}
                            onClick={() => {
                              handleToggleLikeComment(c, false).catch(() => {});
                            }}
                          >
                            ♥ {c.likedCount}
                          </button>
                        </div>
                        <div style={{ fontSize: 13, marginTop: 4 }}>{c.content}</div>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            </div>
          </div>
        </div>
      )}

      {customLyricModalOpen && (
        <div
          id="custom-lyric-modal"
          className="modal-mask show"
          role="dialog"
          aria-modal="true"
          aria-label="自定义编辑歌词"
          onClick={(e) => {
            if (e.target === e.currentTarget) setCustomLyricModalOpen(false);
          }}
        >
          <div className="modal custom-lyric-dialog">
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <b>自定义编辑歌词 (.lrc)</b>
              <button
                type="button"
                className="login-panel-close"
                aria-label="关闭自定义歌词弹窗"
                onClick={() => setCustomLyricModalOpen(false)}
              >
                ×
              </button>
            </div>
            <div style={{ marginTop: 10 }}>
              <textarea
                id="custom-lyric-textarea"
                aria-label="LRC 歌词文本"
                rows={10}
                style={{ width: '100%', boxSizing: 'border-box' }}
                placeholder="[00:00.00] 粘贴或编辑 .lrc 格式歌词..."
                value={customLrcText}
                onChange={(e) => setCustomLrcText(e.target.value)}
              />
            </div>
            <div className="btn-row" style={{ marginTop: 12, justifyContent: 'flex-end', display: 'flex', gap: 8 }}>
              <button
                type="button"
                className="modal-btn"
                onClick={() => setCustomLyricModalOpen(false)}
              >
                取消
              </button>
              <button
                id="custom-lyric-save-btn"
                type="button"
                className="modal-btn primary"
                disabled={savingLyric}
                onClick={() => {
                  handleSaveCustomLyric().catch(() => {});
                }}
              >
                保存歌词
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
};

export default TrackDetailModal;
