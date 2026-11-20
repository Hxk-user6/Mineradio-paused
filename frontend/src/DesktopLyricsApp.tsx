import React, { useEffect, useRef, useState } from 'react';
import { desktopWindow, listenEvent } from './bridge/tauriBridge';
import type { DesktopLyricsPayload } from './types/mineradio';

export interface DesktopLyricsStatePayload extends DesktopLyricsPayload {
  text?: string;
  locked?: boolean;
  enabled?: boolean;
  clickThrough?: boolean;
}

const DESKTOP_LYRICS_CSS = `
:root {
  --lyric-primary: #f6fdff;
  --lyric-highlight: #fff0b8;
  --lyric-glow: #9cffdf;
  --lyric-font: Inter, "Noto Sans SC", "PingFang SC", "Microsoft YaHei", Arial, sans-serif;
  --lyric-size: 46px;
  --lyric-progress: 0%;
}
html, body, #lyrics-root {
  margin: 0;
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: transparent;
  user-select: none;
}
.desktop-lyrics-wrap {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0 16px;
  background: transparent;
}
.desktop-lyrics-stage {
  position: relative;
  width: min(100%, 1080px);
  padding: 58px 44px 36px;
  box-sizing: border-box;
  text-align: center;
  border-radius: 20px;
  transition: background 0.18s ease, box-shadow 0.18s ease;
}
.desktop-lyrics-stage.unlocked {
  cursor: move;
}
.desktop-lyrics-stage.hint-visible {
  background: rgba(6, 10, 16, 0.28);
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.14);
}
.desktop-lyrics-toolbar {
  position: absolute;
  top: 12px;
  left: 50%;
  transform: translateX(-50%) translateY(-6px);
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 4px 12px;
  border-radius: 999px;
  border: 1px solid rgba(156, 255, 223, 0.45);
  background: rgba(5, 8, 14, 0.72);
  color: rgba(255, 255, 255, 0.9);
  font-size: 12px;
  font-weight: 700;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.16s ease, transform 0.16s ease;
}
.desktop-lyrics-stage.hint-visible .desktop-lyrics-toolbar {
  opacity: 1;
  pointer-events: auto;
  transform: translateX(-50%) translateY(0);
}
.desktop-lyrics-toolbar button {
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 999px;
}
.desktop-lyrics-toolbar button:hover {
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
}
.desktop-lyrics-main {
  display: inline-block;
  max-width: 100%;
  font-family: var(--lyric-font);
  font-size: var(--lyric-size);
  font-weight: 900;
  line-height: 1.15;
  background: linear-gradient(
    90deg,
    var(--lyric-highlight) 0%,
    var(--lyric-highlight) max(0%, calc(var(--lyric-progress) - 4%)),
    var(--lyric-glow) calc(var(--lyric-progress) + 1.5%),
    var(--lyric-primary) calc(var(--lyric-progress) + 6%),
    var(--lyric-primary) 100%
  );
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.65));
}
.desktop-lyrics-stage.paused .desktop-lyrics-main {
  opacity: 0.82;
}
.desktop-lyrics-sub {
  margin-top: 8px;
  font-family: var(--lyric-font);
  font-size: 18px;
  font-weight: 600;
  color: rgba(235, 248, 255, 0.86);
  text-shadow: 0 2px 10px rgba(0, 0, 0, 0.72);
}
.desktop-lyrics-next {
  margin-top: 6px;
  font-family: var(--lyric-font);
  font-size: 14px;
  font-weight: 500;
  color: rgba(210, 232, 245, 0.62);
  text-shadow: 0 1px 8px rgba(0, 0, 0, 0.65);
}
`;

export function DesktopLyricsApp(): React.ReactElement {
  const [lyricText, setLyricText] = useState('Mineradio');
  const [translation, setTranslation] = useState('');
  const [nextLine, setNextLine] = useState('');
  const [playing, setPlaying] = useState(false);
  const [progress, setProgress] = useState(0);
  const [sizeScale, setSizeScale] = useState(1);
  const [opacity, setOpacity] = useState(0.92);
  const [locked, setLocked] = useState(false);
  const [hovered, setHovered] = useState(false);

  const dragRef = useRef<{ active: boolean; pointerId: number | null; x: number; y: number }>({
    active: false,
    pointerId: null,
    x: 0,
    y: 0,
  });

  useEffect(() => {
    const applyPayload = (payload: DesktopLyricsStatePayload) => {
      if (!payload || typeof payload !== 'object') return;
      const nextText = payload.line ?? payload.text ?? payload.title;
      if (typeof nextText === 'string' && nextText.trim()) {
        setLyricText(nextText);
      }
      if (typeof payload.translation === 'string') {
        setTranslation(payload.translation);
      }
      if (typeof payload.nextLine === 'string') {
        setNextLine(payload.nextLine);
      }
      if (typeof payload.playing === 'boolean') {
        setPlaying(payload.playing);
      }
      if (typeof payload.progress === 'number' && Number.isFinite(payload.progress)) {
        setProgress(Math.max(0, Math.min(1, payload.progress)));
      } else if (
        typeof payload.currentTime === 'number' &&
        typeof payload.duration === 'number' &&
        payload.duration > 0
      ) {
        setProgress(Math.max(0, Math.min(1, payload.currentTime / payload.duration)));
      }
      if (typeof payload.size === 'number' && Number.isFinite(payload.size)) {
        setSizeScale(Math.max(0.65, Math.min(1.6, payload.size)));
      }
      if (typeof payload.opacity === 'number' && Number.isFinite(payload.opacity)) {
        setOpacity(Math.max(0.28, Math.min(1, payload.opacity)));
      }
      if (typeof payload.locked === 'boolean') {
        setLocked(payload.locked);
      } else if (typeof payload.clickThrough === 'boolean') {
        setLocked(payload.clickThrough);
      }
    };

    const unlisteners: Array<() => void> = [];
    let cancelled = false;

    Promise.all([
      listenEvent<DesktopLyricsStatePayload>('mineradio-desktop-lyrics-state', applyPayload),
      listenEvent<DesktopLyricsStatePayload>('mineradio-desktop-lyrics-update', applyPayload),
      listenEvent<{ locked: boolean }>('mineradio-desktop-lyrics-lock-state', (p) => {
        if (typeof p?.locked === 'boolean') setLocked(p.locked);
      }),
    ])
      .then((fns) => {
        if (cancelled) {
          fns.forEach((fn) => fn());
        } else {
          unlisteners.push(...fns);
        }
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const handleToggleLock = async () => {
    const nextLocked = !locked;
    setLocked(nextLocked);
    await desktopWindow.setDesktopLyricsLockState(nextLocked).catch(() => {});
  };

  const handleClose = async () => {
    await desktopWindow.setDesktopLyricsEnabled(false).catch(() => {});
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button === 1) {
      e.preventDefault();
      handleToggleLock().catch(() => {});
      return;
    }
    if (locked || e.button !== 0) return;
    if ((e.target as HTMLElement).closest('button')) return;
    dragRef.current = {
      active: true,
      pointerId: e.pointerId,
      x: e.screenX,
      y: e.screenY,
    };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current.active || dragRef.current.pointerId !== e.pointerId) return;
    const dx = e.screenX - dragRef.current.x;
    const dy = e.screenY - dragRef.current.y;
    if (dx === 0 && dy === 0) return;
    dragRef.current.x = e.screenX;
    dragRef.current.y = e.screenY;
    desktopWindow.moveDesktopLyricsBy(dx, dy).catch(() => {});
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRef.current.pointerId === e.pointerId) {
      dragRef.current.active = false;
      dragRef.current.pointerId = null;
    }
  };

  const progressPct = `${(progress * 100).toFixed(2)}%`;
  const fontSizePx = `${Math.round(46 * sizeScale)}px`;

  return (
    <>
      <style>{DESKTOP_LYRICS_CSS}</style>
      <div className="wrap desktop-lyrics-wrap" style={{ opacity }}>
        <div
          id="stage"
          className={`stage desktop-lyrics-stage${locked ? ' locked' : ' unlocked'}${
            hovered ? ' hint-visible' : ''
          }${playing ? '' : ' paused'}`}
          aria-label="Mineradio desktop lyrics"
          style={
            {
              '--lyric-progress': progressPct,
              '--lyric-size': fontSizePx,
            } as React.CSSProperties
          }
          onMouseEnter={() => setHovered(true)}
          onMouseLeave={() => setHovered(false)}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
        >
          <div id="lockHint" className="lock-hint desktop-lyrics-toolbar">
            <button
              id="desktop-lyrics-lock-btn"
              type="button"
              onClick={() => {
                handleToggleLock().catch(() => {});
              }}
              aria-pressed={locked}
            >
              {locked ? '解锁移动' : '锁定穿透'}
            </button>
            <button
              id="closeLyricsBtn"
              className="lyrics-close"
              type="button"
              aria-label="关闭桌面歌词"
              onClick={() => {
                handleClose().catch(() => {});
              }}
            >
              关闭桌面歌词
            </button>
          </div>

          <div id="lyricViewport" className="lyric-viewport">
            <div id="lyricScroller" className="lyric-scroll">
              <div id="line" className="line desktop-lyrics-main" data-text={lyricText}>
                {lyricText}
              </div>
              {translation && (
                <div className="desktop-lyrics-sub" data-testid="desktop-lyrics-translation">
                  {translation}
                </div>
              )}
              {nextLine && (
                <div className="desktop-lyrics-next" data-testid="desktop-lyrics-next">
                  {nextLine}
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

export default DesktopLyricsApp;
