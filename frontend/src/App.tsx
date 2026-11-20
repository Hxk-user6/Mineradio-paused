import React, { useCallback, useEffect, useRef, useState } from 'react';
import { AccountAndLoginModal } from './components/AccountAndLoginModal';
import { DesktopModeDock } from './components/DesktopModeDock';
import { FxConsoleModal } from './components/FxConsoleModal';
import { HomeDashboard } from './components/HomeDashboard';
import { PlayerControls } from './components/PlayerControls';
import { PlaylistDrawer } from './components/PlaylistDrawer';
import { SearchBar } from './components/SearchBar';
import { SplashScreen } from './components/SplashScreen';
import { TitleBar } from './components/TitleBar';
import { coverProxyUrl, desktopWindow } from './bridge/tauriBridge';
import { visualEngine } from './engines';
import { usePlayerStore } from './store/playerStore';
import type { SongRecord } from './types/mineradio';

function hexToRgbTuple(hex: string | undefined, fallback = '0, 245, 212'): string {
  const raw = String(hex || '').trim().replace(/^#/, '');
  if (!/^[0-9a-fA-F]{6}$/.test(raw)) return fallback;
  const r = parseInt(raw.slice(0, 2), 16);
  const g = parseInt(raw.slice(2, 4), 16);
  const b = parseInt(raw.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

const GUIDE_STEPS = [
  {
    kicker: 'Step 1 · Search & Import',
    title: '跨平台搜歌与本地曲库',
    body: '顶部搜索框支持 All / 网易云 / QQ / 酷狗 / 汽水 / 播客检索，右侧按钮可快速导入本地音乐或自定义封面。',
  },
  {
    kicker: 'Step 2 · Visual FX',
    title: '视觉控制台与 3D 歌单架',
    body: '点击右下角或底部控制台 FX 按钮，切换 13 套粒子/音域预设、调整歌词排版或展开 3D 歌单架。',
  },
  {
    kicker: 'Step 3 · Shortcuts',
    title: '常用快捷键',
    body: 'Space 播放/暂停，左右方向键快进退，Alt/Ctrl + 左右方向键切歌，Esc 退出全屏或关闭弹窗。',
  },
];

export function App(): React.ReactElement {
  const currentSong = usePlayerStore((s) => s.currentSong);
  const playing = usePlayerStore((s) => s.playing);
  const currentTime = usePlayerStore((s) => s.currentTime);
  const duration = usePlayerStore((s) => s.duration);
  const lyrics = usePlayerStore((s) => s.lyrics);
  const activeLyricIdx = usePlayerStore((s) => s.activeLyricIdx);
  const desktopLyricsEnabled = usePlayerStore((s) => s.desktopLyricsEnabled);
  const diyMode = usePlayerStore((s) => s.diyMode);
  const splashDone = usePlayerStore((s) => s.splashDone);
  const fx = usePlayerStore((s) => s.fx);
  const audioError = usePlayerStore((s) => s.audioError);

  const initApp = usePlayerStore((s) => s.initApp);
  const setDiyMode = usePlayerStore((s) => s.setDiyMode);
  const setFxPanelOpen = usePlayerStore((s) => s.setFxPanelOpen);
  const setPlaylistPanelOpen = usePlayerStore((s) => s.setPlaylistPanelOpen);

  const canvasContainerRef = useRef<HTMLDivElement | null>(null);
  const lastDesktopPushAtRef = useRef(0);
  const lastDesktopKeyRef = useRef('');
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [showHome, setShowHome] = useState(true);
  const [loginModalOpen, setLoginModalOpen] = useState(false);
  const [updateModalOpen, setUpdateModalOpen] = useState(false);
  const [collectTargetSong, setCollectTargetSong] = useState<SongRecord | null>(null);
  const [guideOpen, setGuideOpen] = useState(false);
  const [guideStep, setGuideStep] = useState(0);
  const [toastMsg, setToastMsg] = useState('');
  const [isMaximized, setIsMaximized] = useState(false);
  const [isFullScreen, setIsFullScreen] = useState(false);

  const showToast = useCallback((msg: string) => {
    if (!msg) return;
    setToastMsg(msg);
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    toastTimerRef.current = setTimeout(() => {
      setToastMsg('');
    }, 2600);
  }, []);

  useEffect(() => {
    return () => {
      if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    };
  }, []);

  // 初始化应用与读取 DIY 偏好
  useEffect(() => {
    try {
      if (window.localStorage?.getItem('mineradio-diy-player-mode-v1') === '1') {
        setDiyMode(true);
      }
    } catch {
      // ignore
    }
    initApp().catch(() => {});
  }, [initApp, setDiyMode]);

  // 命令式挂载 Three.js VisualEngine（严禁 React state 逐帧驱动 WebGL）
  useEffect(() => {
    const container = canvasContainerRef.current;
    if (!container) return;

    visualEngine.setCallbacks({
      onShelfSelect: (item) => {
        const st = usePlayerStore.getState();
        if (item.type === 'queue' && typeof item.queueIndex === 'number') {
          setShowHome(false);
          st.playSong(item.queueIndex).catch(() => {});
          return;
        }
        if (item.playlistId) {
          const all = [...st.builtInPlaylists, ...st.userPlaylists];
          const found = all.find((p) => `${p.provider}:${p.id}` === item.playlistId);
          if (found) {
            st.loadPlaylistTracks(found)
              .then((tracks) => {
                if (tracks.length > 0) {
                  setShowHome(false);
                  st.playSong(0, tracks).catch(() => {});
                }
              })
              .catch(() => {});
          }
        }
      },
    });

    visualEngine.mount(container);

    return () => {
      visualEngine.dispose();
    };
  }, []);

  // 播放报错提示
  useEffect(() => {
    if (audioError) {
      showToast(`播放失败：${audioError}`);
    }
  }, [audioError, showToast]);

  // 同步 document.body 状态类与主题 CSS 变量以复用 /css/index.css 样式
  useEffect(() => {
    if (typeof document === 'undefined') return;
    const body = document.body;
    body.classList.add('desktop-shell');
    body.classList.toggle('diy-mode', diyMode);
    body.classList.toggle('simple-mode', !diyMode);
    body.classList.toggle('splash-active', !splashDone);
    body.classList.toggle('empty-home-active', splashDone && showHome);
    body.classList.toggle('controls-visible', splashDone);
    body.classList.toggle('desktop-maximized', isMaximized);
    body.classList.toggle('desktop-fullscreen', isFullScreen);
    body.classList.toggle('custom-background-album-cover', Boolean(fx.backgroundAlbumCover));
    body.classList.toggle(
      'custom-background-override',
      fx.backgroundColorMode === 'custom' || Boolean(fx.backgroundImage)
    );

    const rootStyle = document.documentElement.style;
    rootStyle.setProperty('--fc-accent-rgb', hexToRgbTuple(fx.uiAccentColor, '0, 245, 212'));
    if (fx.uiAccentColor) {
      rootStyle.setProperty('--fc-accent', fx.uiAccentColor);
    }
  }, [
    diyMode,
    splashDone,
    showHome,
    isMaximized,
    isFullScreen,
    fx.backgroundAlbumCover,
    fx.backgroundColorMode,
    fx.backgroundImage,
    fx.uiAccentColor,
  ]);

  // 监听桌面窗口状态与全局热键
  useEffect(() => {
    desktopWindow
      .getState()
      .then((state) => {
        if (typeof state?.isMaximized === 'boolean') setIsMaximized(state.isMaximized);
        if (typeof state?.isFullScreen === 'boolean') setIsFullScreen(state.isFullScreen);
      })
      .catch(() => {});

    const unlistenState = desktopWindow.onStateChange((state) => {
      if (typeof state?.isMaximized === 'boolean') setIsMaximized(state.isMaximized);
      if (typeof state?.isFullScreen === 'boolean') setIsFullScreen(state.isFullScreen);
    });

    const unlistenHotkey = desktopWindow.onGlobalHotkey((payload) => {
      const action = String(payload?.action || payload?.id || '');
      const st = usePlayerStore.getState();
      if (action === 'togglePlay' || action === 'playPause') {
        st.togglePlay().catch(() => {});
      } else if (action === 'next' || action === 'nextTrack') {
        st.nextTrack().catch(() => {});
      } else if (action === 'prev' || action === 'prevTrack') {
        st.prevTrack().catch(() => {});
      } else if (action === 'toggleDesktopLyrics') {
        st.toggleDesktopLyrics().catch(() => {});
      }
    });

    return () => {
      unlistenState();
      unlistenHotkey();
    };
  }, []);

  // 键盘快捷键（Space 播放暂停、左右切歌/进度、Esc 退出全屏/面板）
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const isEditable =
        target &&
        (target.tagName === 'INPUT' ||
          target.tagName === 'TEXTAREA' ||
          target.tagName === 'SELECT' ||
          target.isContentEditable);

      if (e.key === 'Escape') {
        if (isFullScreen) {
          desktopWindow.exitFullscreenWindowed().catch(() => {});
        }
        setGuideOpen(false);
        setLoginModalOpen(false);
        setUpdateModalOpen(false);
        setCollectTargetSong(null);
        setFxPanelOpen(false);
        setPlaylistPanelOpen(false);
        if (usePlayerStore.getState().fx.wallpaperMode) {
          desktopWindow.setWallpaperMode(false).catch(() => {});
          usePlayerStore.getState().updateFx({ wallpaperMode: false });
        }
        return;
      }

      if (isEditable) return;

      const st = usePlayerStore.getState();
      if (e.code === 'Space' || e.key === ' ') {
        e.preventDefault();
        st.togglePlay().catch(() => {});
      } else if (e.key === 'ArrowLeft') {
        e.preventDefault();
        if (e.altKey || e.ctrlKey || e.metaKey) {
          st.prevTrack().catch(() => {});
        } else {
          st.seekTo(Math.max(0, st.currentTime - 5));
        }
      } else if (e.key === 'ArrowRight') {
        e.preventDefault();
        if (e.altKey || e.ctrlKey || e.metaKey) {
          st.nextTrack().catch(() => {});
        } else {
          const maxDur = st.duration || st.currentTime + 5;
          st.seekTo(Math.min(maxDur, st.currentTime + 5));
        }
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isFullScreen, setFxPanelOpen, setPlaylistPanelOpen]);

  // 同步桌面歌词悬浮窗状态
  useEffect(() => {
    if (!desktopLyricsEnabled) return;
    const line = activeLyricIdx >= 0 ? lyrics[activeLyricIdx] : lyrics[0];
    const nextLine = activeLyricIdx >= 0 ? lyrics[activeLyricIdx + 1] : lyrics[1];
    const nextTime = nextLine ? nextLine.time : duration || currentTime + 4;
    const startTime = line ? line.time : 0;
    const span = Math.max(0.5, nextTime - startTime);
    const progress = line
      ? Math.max(0, Math.min(1, (currentTime - startTime) / span))
      : duration > 0
      ? Math.max(0, Math.min(1, currentTime / duration))
      : 0;

    const stateKey = `${currentSong?.name || ''}|${activeLyricIdx}|${playing}|${line?.text || ''}`;
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    if (stateKey === lastDesktopKeyRef.current && now - lastDesktopPushAtRef.current < 220) {
      return;
    }
    lastDesktopKeyRef.current = stateKey;
    lastDesktopPushAtRef.current = now;

    desktopWindow
      .updateDesktopLyrics({
        title: currentSong?.name || 'Mineradio',
        artist: currentSong?.artist || '',
        line: line?.text || currentSong?.name || 'Mineradio',
        translation: line?.translation || '',
        nextLine: nextLine?.text || '',
        playing,
        currentTime,
        duration,
        progress,
        size: fx.desktopLyricsSize,
        opacity: fx.desktopLyricsOpacity,
        y: fx.desktopLyricsY,
        clickThrough: fx.desktopLyricsClickThrough,
        cinema: fx.desktopLyricsCinema,
        highlight: fx.desktopLyricsHighlight,
        fps: fx.desktopLyricsFps,
      })
      .catch(() => {});
  }, [
    desktopLyricsEnabled,
    currentSong,
    lyrics,
    activeLyricIdx,
    currentTime,
    duration,
    playing,
    fx.desktopLyricsSize,
    fx.desktopLyricsOpacity,
    fx.desktopLyricsY,
    fx.desktopLyricsClickThrough,
    fx.desktopLyricsCinema,
    fx.desktopLyricsHighlight,
    fx.desktopLyricsFps,
  ]);

  const albumBgUrl = currentSong?.cover ? coverProxyUrl(currentSong.cover) : '';
  const activeGuide = GUIDE_STEPS[guideStep] || GUIDE_STEPS[0];

  return (
    <>
      <div id="desktop-window-shell">
        <TitleBar
          isMaximized={isMaximized}
          isFullScreen={isFullScreen}
          onOpenGuide={() => {
            setGuideStep(0);
            setGuideOpen(true);
          }}
          onOpenUpdate={() => setUpdateModalOpen(true)}
        />

        <div
          id="custom-bg"
          style={{
            backgroundColor:
              fx.backgroundColorMode === 'custom' ? fx.backgroundColor : undefined,
            backgroundImage: fx.backgroundImage ? `url("${fx.backgroundImage}")` : undefined,
            opacity: fx.backgroundOpacity,
          }}
        >
          <video id="custom-bg-video" muted loop playsInline preload="metadata" />
        </div>

        <div id="wallpaper-engine-layer" aria-hidden="true">
          <img id="wallpaper-engine-image" alt="" />
          <canvas id="wallpaper-engine-freeze" aria-hidden="true" />
          <video id="wallpaper-engine-video" muted loop playsInline preload="metadata" />
        </div>

        <div
          id="album-bg"
          style={
            albumBgUrl
              ? { backgroundImage: `url("${albumBgUrl}")` }
              : undefined
          }
        />
        <div id="album-bg-next" aria-hidden="true" />

        <div id="canvas-container" ref={canvasContainerRef} />
        <canvas id="idle-guide-canvas" aria-hidden="true" />

        <SplashScreen />

        <div id="hint" aria-hidden="true" />

        <SearchBar
          onToast={showToast}
          onSongSelected={() => setShowHome(false)}
          onOpenCollect={(song) => setCollectTargetSong(song)}
        />

        <HomeDashboard
          onOpenLibrary={() => setPlaylistPanelOpen(true)}
          onOpenPlayerConsole={() => setFxPanelOpen(true)}
          onExitHome={() => setShowHome(false)}
          onToast={showToast}
        />

        <AccountAndLoginModal
          loginModalOpen={loginModalOpen}
          onOpenLoginModal={() => setLoginModalOpen(true)}
          onCloseLoginModal={() => setLoginModalOpen(false)}
          updateModalOpen={updateModalOpen}
          onCloseUpdateModal={() => setUpdateModalOpen(false)}
          onGoHome={() => setShowHome((prev) => (currentSong ? !prev : true))}
          onToast={showToast}
        />

        <FxConsoleModal onToast={showToast} />

        <PlaylistDrawer
          onToast={showToast}
          collectTargetSong={collectTargetSong}
          onCloseCollect={() => setCollectTargetSong(null)}
          onSongSelected={() => setShowHome(false)}
        />

        <PlayerControls
          visible={splashDone}
          onOpenCollect={(song) => setCollectTargetSong(song)}
          onToast={showToast}
          onSongSelected={() => setShowHome(false)}
        />

        <div id="toast" className={toastMsg ? 'show' : undefined} role="status" aria-live="polite">
          {toastMsg}
        </div>

        {/* 使用引导浮层 */}
        <div
          id="visual-guide"
          className={guideOpen ? 'show' : undefined}
          aria-live="polite"
          aria-hidden={!guideOpen}
          style={guideOpen ? { display: 'block', opacity: 1, pointerEvents: 'auto' } : undefined}
        >
          <div className="visual-guide-scrim" onClick={() => setGuideOpen(false)} />
          <div id="visual-guide-ring" className="visual-guide-ring" />
          <div id="visual-guide-card" className="visual-guide-card">
            <div id="visual-guide-kicker" className="visual-guide-kicker">
              {activeGuide.kicker}
            </div>
            <div id="visual-guide-title" className="visual-guide-title">
              {activeGuide.title}
            </div>
            <div id="visual-guide-body" className="visual-guide-body">
              {activeGuide.body}
            </div>
            <div className="visual-guide-actions">
              <button type="button" onClick={() => setGuideOpen(false)}>
                跳过
              </button>
              <div id="visual-guide-progress" className="visual-guide-progress">
                {guideStep + 1} / {GUIDE_STEPS.length}
              </div>
              <button
                id="visual-guide-next"
                className="primary"
                type="button"
                onClick={() => {
                  if (guideStep + 1 < GUIDE_STEPS.length) {
                    setGuideStep((s) => s + 1);
                  } else {
                    setGuideOpen(false);
                  }
                }}
              >
                {guideStep + 1 < GUIDE_STEPS.length ? '下一步' : '完成'}
              </button>
            </div>
          </div>
        </div>
      </div>

      <DesktopModeDock onToast={showToast} />
    </>
  );
}

export default App;
