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
import { TrackDetailModal } from './components/TrackDetailModal';
import { coverProxyUrl, desktopWindow, resolveMediaUrl } from './bridge/tauriBridge';
import { gestureEngine, visualEngine } from './engines';
import { applyFxCssVariables, usePlayerStore } from './store/playerStore';
import type { SongRecord } from './types/mineradio';

const VISUAL_GUIDE_SEEN_LS_KEY = 'mineradio-visual-guide-seen-v1';

interface GuideStepSpec {
  selector: string;
  kicker: string;
  title: string;
  body: string;
}

const SIMPLE_GUIDE_STEPS: GuideStepSpec[] = [
  {
    selector: '#search-area',
    kicker: 'Simple 1 / 6 · Search & Import',
    title: '跨平台搜歌与本地曲库',
    body: '顶部搜索框支持 All / 网易云 / QQ / 酷狗 / 汽水 / 播客检索，右侧按钮可快速导入本地音乐或自定义封面。',
  },
  {
    selector: '#empty-home',
    kicker: 'Simple 2 / 6 · Home Dashboard',
    title: '首页今日听歌、推荐与天气电台',
    body: '首页聚合今日听歌时长、天气电台、每日推荐、精选歌单与播客，一键开启沉浸收听。',
  },
  {
    selector: '#control-cover',
    kicker: 'Simple 3 / 6 · Track Detail & Source',
    title: '封面详情与跨平台换源',
    body: '点击左下角封面可查看歌曲详情与评论，点击歌名旁音源标签可一键跨平台搜歌换源。',
  },
  {
    selector: '#cuefield-automix-btn',
    kicker: 'Simple 4 / 6 · Cuefield AutoMix',
    title: '智能节拍混音过渡',
    body: '开启 AutoMix 后，播放器自动分析相邻曲目的 BPM 与结构，在切歌时完成平滑 DJ 过渡。',
  },
  {
    selector: '#lyrics-toggle-btn',
    kicker: 'Simple 5 / 6 · Stage & Desktop Lyrics',
    title: '舞台 3D 歌词与桌面悬浮词',
    body: '支持舞台粒子歌词开关、0.1s 精度时间轴校准，以及独立置顶穿透的桌面歌词窗口。',
  },
  {
    selector: '#diy-mode-btn',
    kicker: 'Simple 6 / 6 · Mode Switch',
    title: '切换 DIY 深度定制模式',
    body: '点击顶部 DIY 按钮即可解锁 13 套视觉预设、3D 歌单架、手势控制与壁纸引擎等高级创作工具。',
  },
];

const DIY_GUIDE_STEPS: GuideStepSpec[] = [
  {
    selector: '#fx-panel-toggle-btn',
    kicker: 'DIY 1 / 6 · FX Console',
    title: 'FX 视觉控制台与 13 套预设',
    body: '展开 FX 控制台可实时调节粒子密度、溢光强度、音域地形、歌词动效与玻璃拟态参数。',
  },
  {
    selector: '#shelf-toggle-btn',
    kicker: 'DIY 2 / 6 · 3D Playlist Shelf',
    title: '3D 空间歌单架',
    body: '在舞台或侧栏唤出 3D 歌单架，支持滚轮与点击交互浏览内置歌单、平台歌单与当前队列。',
  },
  {
    selector: '#canvas-container',
    kicker: 'DIY 3 / 6 · Free Camera & Stage',
    title: '自由镜头与舞台交互',
    body: '在 3D 舞台上拖拽旋转视角、滚轮缩放景深，按 R 键开启自由飞行相机穿梭粒子空间。',
  },
  {
    selector: '#desktop-mode-control-handle',
    kicker: 'DIY 4 / 6 · Wallpaper & Desktop Dock',
    title: '桌面壁纸模式与防误触锁定',
    body: '通过右侧桌面控制坞一键嵌入系统桌面壁纸层，并可锁定软件交互或隐藏系统桌面图标。',
  },
  {
    selector: '#immersive-btn',
    kicker: 'DIY 5 / 6 · Immersive & Pure Stage',
    title: '纯净沉浸模式与隐藏控制栏',
    body: '一键进入纯净视觉舞台或收起底部控制台，配合自定义视频/Wallpaper Engine 背景打造专属视听空间。',
  },
  {
    selector: '#user-btn',
    kicker: 'DIY 6 / 6 · Multi-Platform & Hotkeys',
    title: '多平台账号与全局热键',
    body: '管理多平台登录状态与歌单同步，并在 FX 系统页配置全局快捷键与摄像头隔空手势。',
  },
];

export function App(): React.ReactElement {
  const currentSong = usePlayerStore((s) => s.currentSong);
  const playing = usePlayerStore((s) => s.playing);
  const currentTime = usePlayerStore((s) => s.currentTime);
  const duration = usePlayerStore((s) => s.duration);
  const lyrics = usePlayerStore((s) => s.lyrics);
  const activeLyricIdx = usePlayerStore((s) => s.activeLyricIdx);
  const lyricOffsetSec = usePlayerStore((s) => s.lyricOffsetSec);
  const desktopLyricsEnabled = usePlayerStore((s) => s.desktopLyricsEnabled);
  const diyMode = usePlayerStore((s) => s.diyMode);
  const splashDone = usePlayerStore((s) => s.splashDone);
  const fx = usePlayerStore((s) => s.fx);
  const audioError = usePlayerStore((s) => s.audioError);
  const activeBackgroundMedia = usePlayerStore((s) => s.activeBackgroundMedia);
  const gestureEnabled = usePlayerStore((s) => s.gestureEnabled);
  const gestureStatus = usePlayerStore((s) => s.gestureStatus);
  const gestureHint = usePlayerStore((s) => s.gestureHint);
  const cuefieldFeedbackOpen = usePlayerStore((s) => s.cuefieldFeedbackOpen);
  const controlsHidden = usePlayerStore((s) => s.controlsHidden);
  const immersiveMode = usePlayerStore((s) => s.immersiveMode);

  const initApp = usePlayerStore((s) => s.initApp);
  const setDiyMode = usePlayerStore((s) => s.setDiyMode);
  const setFxPanelOpen = usePlayerStore((s) => s.setFxPanelOpen);
  const setPlaylistPanelOpen = usePlayerStore((s) => s.setPlaylistPanelOpen);
  const setControlsHidden = usePlayerStore((s) => s.setControlsHidden);
  const setGestureEnabled = usePlayerStore((s) => s.setGestureEnabled);
  const submitCuefieldFeedback = usePlayerStore((s) => s.submitCuefieldFeedback);
  const dismissCuefieldFeedback = usePlayerStore((s) => s.dismissCuefieldFeedback);
  const importLocalFiles = usePlayerStore((s) => s.importLocalFiles);
  const playSong = usePlayerStore((s) => s.playSong);
  const updateFx = usePlayerStore((s) => s.updateFx);
  const setActiveBackgroundMedia = usePlayerStore((s) => s.setActiveBackgroundMedia);

  const canvasContainerRef = useRef<HTMLDivElement | null>(null);
  const customBgVideoRef = useRef<HTMLVideoElement | null>(null);
  const weVideoRef = useRef<HTMLVideoElement | null>(null);
  const droppedBgBlobUrlRef = useRef<string>('');
  const lastDesktopPushAtRef = useRef(0);
  const lastDesktopKeyRef = useRef('');
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dragDepthRef = useRef(0);

  const [showHome, setShowHome] = useState(true);
  const [loginModalOpen, setLoginModalOpen] = useState(false);
  const [updateModalOpen, setUpdateModalOpen] = useState(false);
  const [collectTargetSong, setCollectTargetSong] = useState<SongRecord | null>(null);
  const [guideOpen, setGuideOpen] = useState(false);
  const [guideStep, setGuideStep] = useState(0);
  const [guideRingRect, setGuideRingRect] = useState<{
    left: number;
    top: number;
    width: number;
    height: number;
    visible: boolean;
  }>({ left: 0, top: 0, width: 0, height: 0, visible: false });
  const [toastMsg, setToastMsg] = useState('');
  const [hintMsg, setHintMsg] = useState('');
  const [freeCameraActive, setFreeCameraActive] = useState(false);
  const [dragOverActive, setDragOverActive] = useState(false);
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

  // 首次启动完成开屏后自动触发 Visual Guide
  useEffect(() => {
    if (!splashDone) return;
    try {
      if (window.localStorage?.getItem(VISUAL_GUIDE_SEEN_LS_KEY) !== '1') {
        window.localStorage?.setItem(VISUAL_GUIDE_SEEN_LS_KEY, '1');
        setGuideStep(0);
        setGuideOpen(true);
      }
    } catch {
      // ignore
    }
  }, [splashDone]);

  const activeGuideSteps = diyMode ? DIY_GUIDE_STEPS : SIMPLE_GUIDE_STEPS;
  const safeGuideIdx = Math.min(guideStep, activeGuideSteps.length - 1);
  const activeGuide = activeGuideSteps[safeGuideIdx] || activeGuideSteps[0];

  // 根据当前引导步骤的目标 selector 动态计算 #visual-guide-ring 的位置与尺寸
  useEffect(() => {
    if (!guideOpen || typeof document === 'undefined') return;
    const updateRingPosition = () => {
      const targetEl = activeGuide?.selector
        ? (document.querySelector(activeGuide.selector) as HTMLElement | null)
        : null;
      if (!targetEl) {
        setGuideRingRect((prev) => ({ ...prev, visible: false }));
        return;
      }
      const rect = targetEl.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) {
        setGuideRingRect((prev) => ({ ...prev, visible: false }));
        return;
      }
      const pad = 6;
      setGuideRingRect({
        left: Math.round(rect.left - pad),
        top: Math.round(rect.top - pad),
        width: Math.round(rect.width + pad * 2),
        height: Math.round(rect.height + pad * 2),
        visible: true,
      });
    };
    updateRingPosition();
    window.addEventListener('resize', updateRingPosition);
    return () => window.removeEventListener('resize', updateRingPosition);
  }, [guideOpen, safeGuideIdx, diyMode, activeGuide]);

  // 命令式挂载 Three.js VisualEngine（严禁 React state 逐帧驱动 WebGL）
  useEffect(() => {
    const container = canvasContainerRef.current;
    if (!container) return;

    visualEngine.setCallbacks({
      onShelfFocusRequest: (item, index) => {
        const st = usePlayerStore.getState();
        st.setShelfFocusedIndex(index);
        if (item?.playlistId) {
          st.setShelfExpandedPlaylistId(item.playlistId);
        }
      },
      onShelfSelect: (item, index) => {
        const st = usePlayerStore.getState();
        st.setShelfFocusedIndex(index);
        if (item.type === 'queue' && typeof item.queueIndex === 'number') {
          setShowHome(false);
          st.playSong(item.queueIndex).catch(() => {});
          return;
        }
        if (item.playlistId) {
          st.setShelfExpandedPlaylistId(item.playlistId);
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
      onShelfHover: (item) => {
        setHintMsg(item ? `${item.title} · ${item.sub}` : '');
      },
      onShelfCenterChange: (item, index) => {
        const st = usePlayerStore.getState();
        if (st.shelfFocusedIndex !== index) {
          usePlayerStore.setState({ shelfFocusedIndex: index });
        }
        if (item?.playlistId && st.shelfExpandedPlaylistId) {
          st.setShelfExpandedPlaylistId(item.playlistId);
        }
      },
      onFreeCameraChange: (active) => {
        setFreeCameraActive(Boolean(active));
      },
    });

    visualEngine.mount(container);

    return () => {
      visualEngine.dispose();
    };
  }, []);

  // 当 gestureEnabled 变化时启停 gestureEngine 并统一绑定完整状态与动作回调
  useEffect(() => {
    gestureEngine.setCallbacks({
      onStatusChange: (gestureStatus, gestureHint = '') => {
        usePlayerStore.setState({
          gestureEnabled: gestureStatus === 'active' || gestureStatus === 'starting',
          gestureStatus,
          gestureHint,
        });
      },
      onError: (err) => {
        usePlayerStore.setState({
          gestureEnabled: false,
          gestureStatus: 'error',
          gestureHint: err,
        });
      },
      onPlayPause: () => {
        if (usePlayerStore.getState().fx.gesturePlayerActions === false) return;
        usePlayerStore.getState().togglePlay().catch(() => {});
      },
      onNextTrack: () => {
        if (usePlayerStore.getState().fx.gesturePlayerActions === false) return;
        usePlayerStore.getState().nextTrack().catch(() => {});
      },
      onPrevTrack: () => {
        if (usePlayerStore.getState().fx.gesturePlayerActions === false) return;
        usePlayerStore.getState().prevTrack().catch(() => {});
      },
      onVolumeChange: (deltaOrVal, isAbsolute) => {
        if (usePlayerStore.getState().fx.gesturePlayerActions === false) return;
        const st = usePlayerStore.getState();
        const nextVol = isAbsolute ? deltaOrVal : st.volume + deltaOrVal;
        st.setVolume(nextVol);
      },
      onRotateVisual: (rx, ry) => {
        visualEngine.applyGestureRotation(rx, ry);
      },
    });

    if (gestureEnabled) {
      if (!gestureEngine.isActive()) {
        gestureEngine.start().catch(() => {});
      }
    } else if (gestureEngine.isActive()) {
      gestureEngine.stop();
    }
  }, [gestureEnabled]);

  // 卸载时释放通过拖放创建的临时 blob: URL
  useEffect(() => {
    return () => {
      if (droppedBgBlobUrlRef.current && typeof URL !== 'undefined' && typeof URL.revokeObjectURL === 'function') {
        URL.revokeObjectURL(droppedBgBlobUrlRef.current);
        droppedBgBlobUrlRef.current = '';
      }
    };
  }, []);

  // 消费 store activeBackgroundMedia，通过 resolveMediaUrl 驱动 #custom-bg-video 与 #wallpaper-engine-layer
  const resolvedBgMediaUrl =
    activeBackgroundMedia?.url && activeBackgroundMedia.type !== 'none'
      ? resolveMediaUrl(activeBackgroundMedia.url, { preferHttpFallback: true })
      : '';
  const isWeBackground =
    Boolean(resolvedBgMediaUrl) &&
    (activeBackgroundMedia?.source === 'wallpaper-engine' ||
      Boolean(activeBackgroundMedia?.projectId) ||
      String(activeBackgroundMedia?.url || '').startsWith('mineradio-wallpaper://') ||
      String(activeBackgroundMedia?.url || '').startsWith('/we-media/'));
  const customBgVideoUrl =
    !isWeBackground && activeBackgroundMedia?.type === 'video' ? resolvedBgMediaUrl : '';
  const weVideoUrl =
    isWeBackground && activeBackgroundMedia?.type === 'video' ? resolvedBgMediaUrl : '';
  const weImageUrl =
    isWeBackground && activeBackgroundMedia?.type === 'image' ? resolvedBgMediaUrl : '';
  const customBgImageUrl =
    !isWeBackground && activeBackgroundMedia?.type === 'image' && resolvedBgMediaUrl
      ? resolvedBgMediaUrl
      : fx.backgroundImage
      ? resolveMediaUrl(fx.backgroundImage, { preferHttpFallback: true })
      : '';

  useEffect(() => {
    const customVid = customBgVideoRef.current;
    if (customVid) {
      if (customBgVideoUrl) {
        customVid.muted = activeBackgroundMedia?.muted ?? true;
        customVid.play().catch(() => {});
      } else {
        customVid.pause();
      }
    }
    const weVid = weVideoRef.current;
    if (weVid) {
      if (weVideoUrl) {
        weVid.muted = activeBackgroundMedia?.muted ?? true;
        weVid.play().catch(() => {});
      } else {
        weVid.pause();
      }
    }
  }, [customBgVideoUrl, weVideoUrl, activeBackgroundMedia?.muted]);

  // 播放报错提示
  useEffect(() => {
    if (audioError) {
      showToast(`播放失败：${audioError}`);
    }
  }, [audioError, showToast]);

  // 监听 fx 调用 applyFxCssVariables(fx)，并同步 document.documentElement / document.body 类名
  useEffect(() => {
    if (typeof document === 'undefined') return;
    applyFxCssVariables(fx);

    const html = document.documentElement;
    html.classList.toggle('diy-mode-preload', diyMode);
    html.classList.toggle('simple-mode-preload', !diyMode);
    if (splashDone) {
      html.classList.remove('startup-fast-skip-preload');
    }

    const body = document.body;
    body.classList.add('desktop-shell');
    body.classList.toggle('diy-mode', diyMode);
    body.classList.toggle('simple-mode', !diyMode);
    body.classList.toggle('splash-active', !splashDone);
    body.classList.toggle('empty-home-active', splashDone && showHome);
    body.classList.toggle('controls-visible', splashDone && !controlsHidden);
    body.classList.toggle('controls-hidden', controlsHidden);
    body.classList.toggle('immersive-mode', immersiveMode);
    body.classList.toggle('free-camera-active', freeCameraActive);
    body.classList.toggle('desktop-maximized', isMaximized);
    body.classList.toggle('desktop-fullscreen', isFullScreen);
    body.classList.toggle('custom-background-album-cover', Boolean(fx.backgroundAlbumCover));
    body.classList.toggle(
      'custom-background-override',
      fx.backgroundColorMode === 'custom' ||
        Boolean(fx.backgroundImage) ||
        Boolean(resolvedBgMediaUrl)
    );
  }, [
    fx,
    diyMode,
    splashDone,
    showHome,
    controlsHidden,
    immersiveMode,
    freeCameraActive,
    isMaximized,
    isFullScreen,
    resolvedBgMediaUrl,
  ]);

  // 关闭 DIY 模式时自动收起 FX 视觉控制台，避免残留展开状态
  useEffect(() => {
    if (!diyMode) {
      setFxPanelOpen(false);
    }
  }, [diyMode, setFxPanelOpen]);

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

  // 键盘快捷键（Space 播放暂停、左右切歌/进度、Esc 退出全屏/面板/沉浸模式）
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
        const st = usePlayerStore.getState();
        if (st.immersiveMode) {
          st.setImmersiveMode(false);
        }
        if (st.controlsHidden) {
          st.setControlsHidden(false);
        }
        if (st.fx.wallpaperMode) {
          desktopWindow.setWallpaperMode(false).catch(() => {});
          st.updateFx({ wallpaperMode: false });
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
    const effectiveTime = Math.max(0, currentTime + lyricOffsetSec);
    const line = activeLyricIdx >= 0 ? lyrics[activeLyricIdx] : lyrics[0];
    const nextLine = activeLyricIdx >= 0 ? lyrics[activeLyricIdx + 1] : lyrics[1];
    const nextTime = nextLine ? nextLine.time : duration || effectiveTime + 4;
    const startTime = line ? line.time : 0;
    const span = Math.max(0.5, nextTime - startTime);
    const progress = line
      ? Math.max(0, Math.min(1, (effectiveTime - startTime) / span))
      : duration > 0
      ? Math.max(0, Math.min(1, effectiveTime / duration))
      : 0;

    const stateKey = `${currentSong?.name || ''}|${activeLyricIdx}|${playing}|${line?.text || ''}|${lyricOffsetSec}`;
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
        currentTime: effectiveTime,
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
    lyricOffsetSec,
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

  // 全局拖放导入本地音频或更换封面图片
  const handleDragEnter = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    dragDepthRef.current += 1;
    setDragOverActive(true);
  };

  const handleDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    if (!dragOverActive) setDragOverActive(true);
  };

  const handleDragLeave = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (dragDepthRef.current === 0) {
      setDragOverActive(false);
    }
  };

  const handleDrop = async (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    dragDepthRef.current = 0;
    setDragOverActive(false);

    const files = Array.from(e.dataTransfer?.files || []);
    if (!files.length) return;

    const audioFiles = files.filter(
      (f) =>
        f.type.startsWith('audio/') ||
        /\.(mp3|flac|wav|ogg|m4a|aac|opus|webm)$/i.test(f.name)
    );
    const imageFiles = files.filter(
      (f) =>
        f.type.startsWith('image/') ||
        /\.(png|jpe?g|webp|gif|bmp|avif)$/i.test(f.name)
    );

    if (audioFiles.length > 0) {
      const imported = await importLocalFiles(audioFiles).catch(() => []);
      if (imported.length > 0) {
        setShowHome(false);
        await playSong(imported[0], imported).catch(() => {});
        showToast(`已拖放导入并播放 ${imported.length} 首本地音频`);
      } else {
        showToast('未能识别可导入的本地音频文件');
      }
      return;
    }

    if (imageFiles.length > 0) {
      const imgFile = imageFiles[0];
      if (
        droppedBgBlobUrlRef.current &&
        typeof URL !== 'undefined' &&
        typeof URL.revokeObjectURL === 'function'
      ) {
        URL.revokeObjectURL(droppedBgBlobUrlRef.current);
        droppedBgBlobUrlRef.current = '';
      }

      // 小图片优先转为可持久化的 Data URL；大图则仅在当前会话挂载 blob: 而不持久化进 fx.backgroundImage
      const canPersistAsDataUrl = imgFile.size <= 3 * 1024 * 1024 && typeof FileReader !== 'undefined';
      if (canPersistAsDataUrl) {
        const dataUrl = await new Promise<string>((resolve) => {
          const reader = new FileReader();
          reader.onload = () => resolve(typeof reader.result === 'string' ? reader.result : '');
          reader.onerror = () => resolve('');
          reader.readAsDataURL(imgFile);
        });
        if (dataUrl) {
          visualEngine.setCover(dataUrl);
          updateFx({ backgroundImage: dataUrl });
          setActiveBackgroundMedia(null);
          showToast(`已更换封面与背景图片：${imgFile.name}`);
          return;
        }
      }

      const imgUrl =
        typeof URL !== 'undefined' && typeof URL.createObjectURL === 'function'
          ? URL.createObjectURL(imgFile)
          : '';
      if (imgUrl) {
        droppedBgBlobUrlRef.current = imgUrl;
        visualEngine.setCover(imgUrl);
        if (fx.backgroundImage?.startsWith('blob:')) {
          updateFx({ backgroundImage: '' });
        }
        setActiveBackgroundMedia({
          type: 'image',
          url: imgUrl,
          muted: true,
        });
        showToast(`已临时更换封面与背景图片：${imgFile.name}`);
      }
    }
  };

  const albumBgUrl = currentSong?.cover ? coverProxyUrl(currentSong.cover) : '';

  return (
    <>
      <div
        id="desktop-window-shell"
        onDragEnter={handleDragEnter}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={(e) => {
          handleDrop(e).catch(() => {});
        }}
      >
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
            backgroundImage: customBgImageUrl ? `url("${customBgImageUrl}")` : undefined,
            opacity: fx.backgroundOpacity,
          }}
        >
          <video
            ref={customBgVideoRef}
            id="custom-bg-video"
            src={customBgVideoUrl || undefined}
            muted={activeBackgroundMedia?.muted ?? true}
            loop
            playsInline
            preload="metadata"
            style={
              customBgVideoUrl
                ? { display: 'block', opacity: 1 }
                : undefined
            }
          />
        </div>

        <div
          id="wallpaper-engine-layer"
          className={isWeBackground || Boolean(weImageUrl) ? 'active' : undefined}
          aria-hidden="true"
        >
          <img
            id="wallpaper-engine-image"
            src={weImageUrl || undefined}
            alt=""
            style={weImageUrl ? { display: 'block', opacity: 1 } : undefined}
          />
          <canvas id="wallpaper-engine-freeze" aria-hidden="true" />
          <video
            ref={weVideoRef}
            id="wallpaper-engine-video"
            src={weVideoUrl || undefined}
            muted={activeBackgroundMedia?.muted ?? true}
            loop
            playsInline
            preload="metadata"
            style={weVideoUrl ? { display: 'block', opacity: 1 } : undefined}
          />
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

        <div id="hint" className={hintMsg ? 'show' : undefined} aria-hidden={!hintMsg}>
          {hintMsg}
        </div>

        {/* 自由镜头提示条 */}
        <div
          id="free-camera-hint"
          className={`free-camera-hint${freeCameraActive ? ' show active' : ''}`}
          aria-hidden={!freeCameraActive}
          style={freeCameraActive ? { display: 'flex' } : { display: 'none' }}
        >
          <span>自由镜头模式 · WASD/鼠标漫游 · 双击或 K 复位 · R 退出</span>
          <button
            type="button"
            className="fx-mini-btn ghost"
            onClick={() => {
              visualEngine.resetFreeCamera();
            }}
          >
            复位视角
          </button>
          <button
            type="button"
            className="fx-mini-btn"
            onClick={() => {
              visualEngine.toggleFreeCamera(false);
              setFreeCameraActive(false);
            }}
          >
            退出自由镜头
          </button>
        </div>

        {/* 摄像头隔空手势 HUD 与手部骨架画布 */}
        <div
          id="gesture-hud"
          className={`gesture-hud${gestureEnabled ? ' show active' : ''}`}
          aria-hidden={!gestureEnabled}
          style={gestureEnabled ? { display: 'flex' } : { display: 'none' }}
        >
          <canvas
            id="hand-canvas"
            width={240}
            height={180}
            style={{ display: fx.gestureHandOverlay !== false ? 'block' : 'none' }}
          />
          <div className="gesture-hud-info">
            <div className="gesture-hud-title">
              {gestureStatus === 'active'
                ? '手势控制运行中'
                : gestureStatus === 'starting'
                ? '正在启动摄像头…'
                : '手势待命'}
            </div>
            {gestureHint && <div className="gesture-hud-hint">{gestureHint}</div>}
            <button
              type="button"
              className="fx-mini-btn ghost"
              onClick={() => {
                setGestureEnabled(false).catch(() => {});
              }}
            >
              关闭手势
            </button>
          </div>
        </div>

        {/* Cuefield AutoMix 过渡评分浮卡 */}
        {cuefieldFeedbackOpen && (
          <div
            id="cuefield-feedback"
            className="cuefield-feedback show"
            role="region"
            aria-label="Cuefield AutoMix 过渡评分"
          >
            <div className="cuefield-feedback-head">
              <strong>AutoMix 混音过渡评价</strong>
              <button
                type="button"
                className="cuefield-feedback-close fx-mini-btn ghost"
                aria-label="关闭过渡评分"
                onClick={() => dismissCuefieldFeedback()}
              >
                ×
              </button>
            </div>
            <div className="cuefield-feedback-body">
              刚才的智能混音切歌听感如何？
            </div>
            <div className="cuefield-feedback-actions">
              {[
                { rating: 5, label: '惊艳 (5)' },
                { rating: 4, label: '顺滑 (4)' },
                { rating: 3, label: '一般 (3)' },
                { rating: 1, label: '突兀 (1)' },
              ].map((item) => (
                <button
                  key={item.rating}
                  type="button"
                  className="fx-mini-btn"
                  data-rating={item.rating}
                  onClick={() => {
                    submitCuefieldFeedback(item.rating).catch(() => {});
                    showToast('感谢反馈，已提交本次 AutoMix 过渡评分');
                  }}
                >
                  {item.label}
                </button>
              ))}
              <button
                type="button"
                className="fx-mini-btn ghost"
                onClick={() => dismissCuefieldFeedback()}
              >
                忽略
              </button>
            </div>
          </div>
        )}

        {/* 全局拖放音频导入 / 图片更换封面浮层 */}
        <div
          id="drop-overlay"
          className={dragOverActive ? 'show active' : undefined}
          aria-hidden={!dragOverActive}
          style={dragOverActive ? { display: 'flex', opacity: 1 } : undefined}
        >
          <div className="drop-overlay-card">
            <strong>释放文件以导入</strong>
            <span>支持拖放本地音频文件（MP3 / FLAC / WAV 等）立即播放，或拖放图片更换舞台封面</span>
          </div>
        </div>

        <SearchBar
          onToast={showToast}
          onSongSelected={() => setShowHome(false)}
          onOpenCollect={(song) => setCollectTargetSong(song)}
        />

        <HomeDashboard
          onOpenLibrary={() => setPlaylistPanelOpen(true)}
          onOpenPlayerConsole={() => {
            setControlsHidden(false);
            if (diyMode) setFxPanelOpen(true);
          }}
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

        <TrackDetailModal onToast={showToast} />

        <div id="toast" className={toastMsg ? 'show' : undefined} role="status" aria-live="polite">
          {toastMsg}
        </div>

        {/* 使用引导浮层（Simple 6 步 + DIY 6 步） */}
        <div
          id="visual-guide"
          className={guideOpen ? 'show' : undefined}
          aria-live="polite"
          aria-hidden={!guideOpen}
          style={guideOpen ? { display: 'block', opacity: 1, pointerEvents: 'auto' } : undefined}
        >
          <div className="visual-guide-scrim" onClick={() => setGuideOpen(false)} />
          <div
            id="visual-guide-ring"
            className="visual-guide-ring"
            style={
              guideRingRect.visible
                ? {
                    left: `${guideRingRect.left}px`,
                    top: `${guideRingRect.top}px`,
                    width: `${guideRingRect.width}px`,
                    height: `${guideRingRect.height}px`,
                    opacity: 1,
                  }
                : { opacity: 0 }
            }
          />
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
                {safeGuideIdx + 1} / {activeGuideSteps.length}
              </div>
              <button
                id="visual-guide-next"
                className="primary"
                type="button"
                onClick={() => {
                  if (safeGuideIdx + 1 < activeGuideSteps.length) {
                    setGuideStep(safeGuideIdx + 1);
                  } else {
                    setGuideOpen(false);
                  }
                }}
              >
                {safeGuideIdx + 1 < activeGuideSteps.length ? '下一步' : '完成'}
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
