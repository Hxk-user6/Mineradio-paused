import React, { useEffect, useState } from 'react';
import { desktopWindow } from '../bridge/tauriBridge';
import { usePlayerStore } from '../store/playerStore';

export interface DesktopModeDockProps {
  onToast?: (message: string) => void;
}

export function DesktopModeDock({ onToast }: DesktopModeDockProps): React.ReactElement {
  const fx = usePlayerStore((s) => s.fx);
  const updateFx = usePlayerStore((s) => s.updateFx);

  const [dockOpen, setDockOpen] = useState(false);
  const [wallpaperEnabled, setWallpaperEnabled] = useState(Boolean(fx.wallpaperMode));
  const [softwareLocked, setSoftwareLocked] = useState(false);
  const [iconsVisible, setIconsVisible] = useState(true);

  useEffect(() => {
    setWallpaperEnabled(Boolean(fx.wallpaperMode));
  }, [fx.wallpaperMode]);

  useEffect(() => {
    desktopWindow
      .getWallpaperModeStatus()
      .then((res) => {
        if (typeof res?.enabled === 'boolean') {
          setWallpaperEnabled(res.enabled);
        }
      })
      .catch(() => {});

    const unlisten = desktopWindow.onWallpaperModeState((payload) => {
      if (typeof payload?.enabled === 'boolean') {
        setWallpaperEnabled(payload.enabled);
        updateFx({ wallpaperMode: payload.enabled });
      }
      if (typeof payload?.softwareLocked === 'boolean') {
        setSoftwareLocked(payload.softwareLocked);
      }
      if (typeof payload?.iconsVisible === 'boolean') {
        setIconsVisible(payload.iconsVisible);
      }
    });
    return unlisten;
  }, [updateFx]);

  useEffect(() => {
    if (typeof document === 'undefined') return;
    document.body.classList.toggle('desktop-wallpaper-mode', wallpaperEnabled);
    document.body.classList.toggle('desktop-wallpaper-interactive', wallpaperEnabled);
    document.body.classList.toggle('desktop-mode-control-peek', wallpaperEnabled);
    document.body.classList.toggle('desktop-mode-controls-open', dockOpen);
    document.body.classList.toggle('desktop-software-locked', softwareLocked);
  }, [wallpaperEnabled, dockOpen, softwareLocked]);

  const handleToggleWallpaperMode = async () => {
    const next = !wallpaperEnabled;
    const res = await desktopWindow.setWallpaperMode(next, {
      opacity: fx.wallpaperOpacity,
      fps: fx.wallpaperFps,
    });
    const finalEnabled = res.supported === false ? false : Boolean(res.enabled ?? next);
    setWallpaperEnabled(finalEnabled);
    updateFx({ wallpaperMode: finalEnabled });
    if (res.supported === false && next) {
      onToast?.('当前系统或浏览器环境不支持桌面壁纸模式');
    } else {
      onToast?.(finalEnabled ? '已启用桌面壁纸模式（Esc 退出）' : '已退出桌面壁纸模式');
    }
  };

  const handleToggleSoftwareLock = async () => {
    const next = !softwareLocked;
    const res = await desktopWindow.setDesktopSoftwareLocked(next);
    const finalLocked = Boolean(res.locked ?? next);
    setSoftwareLocked(finalLocked);
    onToast?.(finalLocked ? '已锁定软件交互' : '已解锁软件交互');
  };

  const handleToggleDesktopIcons = async () => {
    const next = !iconsVisible;
    const res = await desktopWindow.setDesktopIconsVisible(next);
    const finalVisible = Boolean(res.visible ?? next);
    setIconsVisible(finalVisible);
    onToast?.(finalVisible ? '已显示桌面图标' : '已隐藏桌面图标');
  };

  return (
    <div
      id="desktop-mode-control-dock"
      className={dockOpen ? 'is-open' : undefined}
      aria-label="桌面模式控制器"
    >
      <button
        id="desktop-mode-control-handle"
        type="button"
        title="打开桌面模式控制"
        aria-label="打开桌面模式控制"
        aria-haspopup="true"
        aria-expanded={dockOpen}
        onClick={() => setDockOpen((v) => !v)}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M5 7h14M5 12h14M5 17h14" />
          <circle cx="9" cy="7" r="1.45" />
          <circle cx="15" cy="12" r="1.45" />
          <circle cx="11" cy="17" r="1.45" />
        </svg>
      </button>

      <div
        id="desktop-mode-control-panel"
        role="group"
        aria-label="桌面模式开关"
        aria-hidden={!dockOpen}
      >
        <div className="desktop-mode-control-heading">
          <span>桌面控制</span>
          <small>Esc 退出</small>
        </div>

        <button
          id="desktop-wallpaper-mode-toggle"
          className="desktop-mode-switch"
          type="button"
          role="switch"
          aria-checked={wallpaperEnabled}
          onClick={() => {
            handleToggleWallpaperMode().catch(() => {});
          }}
        >
          <span className="desktop-mode-switch-copy">
            <strong>桌面壁纸模式</strong>
            <small>{wallpaperEnabled ? '壁纸模式运行中' : '点击启用壁纸模式'}</small>
          </span>
          <span className="desktop-mode-switch-track" aria-hidden="true">
            <span />
          </span>
        </button>

        <button
          id="desktop-software-lock-toggle"
          className="desktop-mode-switch"
          type="button"
          role="switch"
          aria-checked={softwareLocked}
          onClick={() => {
            handleToggleSoftwareLock().catch(() => {});
          }}
        >
          <span className="desktop-mode-switch-copy">
            <strong>锁定软件操作</strong>
            <small id="desktop-software-lock-state">
              {softwareLocked ? '已锁定交互防误触' : '软件可正常操作'}
            </small>
          </span>
          <span className="desktop-mode-switch-track" aria-hidden="true">
            <span />
          </span>
        </button>

        <button
          id="desktop-icons-visible-toggle"
          className="desktop-mode-switch"
          type="button"
          role="switch"
          aria-checked={iconsVisible}
          onClick={() => {
            handleToggleDesktopIcons().catch(() => {});
          }}
        >
          <span className="desktop-mode-switch-copy">
            <strong>显示桌面图标</strong>
            <small id="desktop-icons-visible-state">
              {iconsVisible ? '图标已显示' : '图标已隐藏'}
            </small>
          </span>
          <span className="desktop-mode-switch-track" aria-hidden="true">
            <span />
          </span>
        </button>
      </div>
    </div>
  );
}

export default DesktopModeDock;
