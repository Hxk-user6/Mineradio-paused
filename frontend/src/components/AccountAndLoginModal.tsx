import React, { useCallback, useEffect, useRef, useState } from 'react';
import { apiJson, desktopWindow } from '../bridge/tauriBridge';
import { usePlayerStore } from '../store/playerStore';
import type { ProviderType } from '../types/mineradio';

export interface AccountAndLoginModalProps {
  loginModalOpen?: boolean;
  onOpenLoginModal?: () => void;
  onCloseLoginModal?: () => void;
  updateModalOpen?: boolean;
  onCloseUpdateModal?: () => void;
  onGoHome?: () => void;
  onToast?: (message: string) => void;
}

type LoginProvider = 'netease' | 'qq' | 'kugou' | 'qishui';

const PROVIDERS: Array<{ key: LoginProvider; code: string; name: string; desc: string }> = [
  { key: 'netease', code: 'NE', name: '网易云', desc: '官方扫码 / Cookie' },
  { key: 'qq', code: 'QQ', name: 'QQ 音乐', desc: '弹窗状态 / Cookie' },
  { key: 'kugou', code: 'KG', name: '酷狗音乐', desc: '官方窗口 / Cookie' },
  { key: 'qishui', code: 'QS', name: '汽水音乐', desc: '抖音扫码 / Cookie' },
];

function isSafeHttpsUrl(value: string | undefined): boolean {
  const raw = String(value || '').trim();
  if (!raw || raw.length > 2048) return false;
  try {
    return new URL(raw).protocol === 'https:';
  } catch {
    return false;
  }
}

export function AccountAndLoginModal({
  loginModalOpen: controlledLoginOpen,
  onOpenLoginModal,
  onCloseLoginModal,
  updateModalOpen: controlledUpdateOpen,
  onCloseUpdateModal,
  onGoHome,
  onToast,
}: AccountAndLoginModalProps): React.ReactElement {
  const easterEggUnlocked = usePlayerStore((s) => s.easterEggUnlocked);
  const platformLogins = usePlayerStore((s) => s.platformLogins);
  const updateInfo = usePlayerStore((s) => s.updateInfo);
  const unlockEasterEgg = usePlayerStore((s) => s.unlockEasterEgg);
  const resetEasterEgg = usePlayerStore((s) => s.resetEasterEgg);
  const refreshLoginStatuses = usePlayerStore((s) => s.refreshLoginStatuses);
  const refreshAllUserPlaylists = usePlayerStore((s) => s.refreshAllUserPlaylists);
  const refreshHomeDiscover = usePlayerStore((s) => s.refreshHomeDiscover);
  const checkUpdates = usePlayerStore((s) => s.checkUpdates);

  const [internalLoginOpen, setInternalLoginOpen] = useState(false);
  const [internalUpdateOpen, setInternalUpdateOpen] = useState(false);

  const loginOpen = controlledLoginOpen ?? internalLoginOpen;
  const updateOpen = controlledUpdateOpen ?? internalUpdateOpen;

  const openLogin = () => {
    setInternalLoginOpen(true);
    onOpenLoginModal?.();
  };
  const closeLogin = () => {
    setInternalLoginOpen(false);
    onCloseLoginModal?.();
  };
  const closeUpdate = () => {
    setInternalUpdateOpen(false);
    onCloseUpdateModal?.();
  };

  // 彩蛋解锁状态
  const [wishText, setWishText] = useState('');
  const [eyeTaps, setEyeTaps] = useState(0);
  const [cinematic, setCinematic] = useState(false);
  const [confirmLogoutAll, setConfirmLogoutAll] = useState(false);
  const wishRevealed = eyeTaps >= 5;
  const cinematicTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [eggStatusText, setEggStatusText] = useState('');
  const [eggStatusMode, setEggStatusMode] = useState<'hint' | 'error' | 'success' | ''>('');
  const [eggBusy, setEggBusy] = useState(false);

  // 登录面板状态
  const [provider, setProvider] = useState<LoginProvider>('netease');
  const [qrImg, setQrImg] = useState('');
  const [qrStatus, setQrStatus] = useState('选择平台并开始登录');
  const [qrStateClass, setQrStateClass] = useState<'' | 'scan' | 'fail' | 'preview'>('');
  const [cookieOpen, setCookieOpen] = useState(false);
  const [cookieText, setCookieText] = useState('');
  const [authBusy, setAuthBusy] = useState(false);

  // 更新详情状态
  const [selectedPageIdx, setSelectedPageIdx] = useState(0);

  const pollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pollTokenRef = useRef(0);

  const stopPolling = useCallback(() => {
    pollTokenRef.current += 1;
    if (pollTimerRef.current) {
      clearTimeout(pollTimerRef.current);
      clearInterval(pollTimerRef.current);
      pollTimerRef.current = null;
    }
  }, []);

  useEffect(() => {
    if (!loginOpen) {
      stopPolling();
      setConfirmLogoutAll(false);
    }
    return () => stopPolling();
  }, [loginOpen, stopPolling]);

  useEffect(() => {
    const off = desktopWindow.onPlatformLoginSuccess((ev) => {
      refreshLoginStatuses().catch(() => {});
      refreshAllUserPlaylists().catch(() => {});
      refreshHomeDiscover().catch(() => {});
      setQrStatus('官方弹窗登录成功，已同步账号信息');
      setQrStateClass('scan');
      if (ev?.provider) {
        onToast?.('平台账号登录成功');
      }
    });
    return () => {
      if (typeof off === 'function') off();
      if (cinematicTimerRef.current) clearTimeout(cinematicTimerRef.current);
    };
  }, [refreshLoginStatuses, refreshAllUserPlaylists, refreshHomeDiscover, onToast]);

  const handleSubmitEasterEgg = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (eggBusy) return;
    const trimmed = wishText.trim();
    if (!trimmed) {
      setEggStatusMode('hint');
      setEggStatusText('请输入四个字的愿望');
      return;
    }
    setEggBusy(true);
    try {
      const res = await unlockEasterEgg(trimmed);
      if (res.unlocked) {
        setEggStatusMode('success');
        setEggStatusText(res.message || '已解锁全平台登录入口');
        setWishText('');
        setCinematic(true);
        if (cinematicTimerRef.current) clearTimeout(cinematicTimerRef.current);
        cinematicTimerRef.current = setTimeout(() => setCinematic(false), 2600);
        onToast?.('已达成成就：世界和平！');
      } else {
        setEggStatusMode('error');
        setEggStatusText(res.message || '暗号不正确，请再想想');
      }
    } catch {
      setEggStatusMode('error');
      setEggStatusText('解锁验证失败');
    } finally {
      setEggBusy(false);
    }
  };

  const pollNeteaseQr = (key: string, token: number) => {
    let consecutiveErrors = 0;
    const finishSuccess = (info: {
      loggedIn?: boolean;
      userId?: string | number;
      nickname?: string;
      avatar?: string;
      avatarUrl?: string;
      vipType?: number;
      vipLevel?: 'none' | 'vip' | 'svip';
      isVip?: boolean;
      isSvip?: boolean;
      vipLabel?: string;
    }) => {
      stopPolling();
      setQrStatus('登录成功！正在同步账号信息…');
      setQrStateClass('scan');
      usePlayerStore.setState((state) => ({
        platformLogins: {
          ...state.platformLogins,
          netease: {
            ...state.platformLogins.netease,
            loggedIn: true,
            userId: info.userId ?? state.platformLogins.netease.userId,
            nickname: info.nickname || state.platformLogins.netease.nickname || '网易云用户',
            avatar: info.avatar || info.avatarUrl || state.platformLogins.netease.avatar,
            avatarUrl: info.avatarUrl || info.avatar || state.platformLogins.netease.avatarUrl,
            vipType: info.vipType ?? state.platformLogins.netease.vipType,
            cookieValid: true,
            updatedAt: Date.now(),
          },
        },
      }));
      refreshLoginStatuses().catch(() => {});
      refreshAllUserPlaylists().catch(() => {});
      refreshHomeDiscover().catch(() => {});
      onToast?.(`网易云已登录${info.nickname ? `：${info.nickname}` : ''}`);
      closeLogin();
    };

    const check = async () => {
      if (pollTokenRef.current !== token) return;
      try {
        const res = await apiJson<{
          code?: number;
          loggedIn?: boolean;
          hasCookie?: boolean;
          userId?: string | number;
          nickname?: string;
          avatar?: string;
          avatarUrl?: string;
          vipType?: number;
          vipLevel?: 'none' | 'vip' | 'svip';
          isVip?: boolean;
          isSvip?: boolean;
          vipLabel?: string;
        }>(`/api/login/qr/check?key=${encodeURIComponent(key)}&t=${Date.now()}`, {
          timeoutMs: 8000,
          cache: 'no-store',
        });
        if (pollTokenRef.current !== token) return;
        consecutiveErrors = 0;

        if (res.code === 800) {
          setQrStatus('二维码已过期，请重新生成');
          setQrStateClass('fail');
          stopPolling();
          return;
        }
        if (res.code === 801) {
          setQrStatus('请使用网易云音乐 App 扫码');
          setQrStateClass('');
        } else if (res.code === 802) {
          setQrStatus('已扫码，请在手机确认登录…');
          setQrStateClass('scan');
        } else if (res.code === 803) {
          if (res.loggedIn || res.hasCookie) {
            finishSuccess(res);
            return;
          }
          const fallbackStatus = await apiJson<{
            loggedIn?: boolean;
            userId?: string | number;
            nickname?: string;
            avatar?: string;
            avatarUrl?: string;
          }>(`/api/login/status?t=${Date.now()}`, {
            timeoutMs: 4000,
            cache: 'no-store',
          }).catch(() => null);
          if (pollTokenRef.current !== token) return;
          if (fallbackStatus?.loggedIn) {
            finishSuccess(fallbackStatus);
            return;
          }
          setQrStatus('扫码已确认，但未获取到登录凭证，请重新获取二维码');
          setQrStateClass('fail');
          stopPolling();
          return;
        }
        pollTimerRef.current = setTimeout(check, 1800);
      } catch {
        if (pollTokenRef.current === token) {
          consecutiveErrors += 1;
          if (consecutiveErrors >= 3) {
            setQrStatus('扫码状态轮询异常，正在重试…');
            setQrStateClass('preview');
          }
          pollTimerRef.current = setTimeout(check, 2200);
        }
      }
    };
    pollTimerRef.current = setTimeout(check, 1000);
  };

  const pollQishuiQr = (qrToken: string, pollId: number) => {
    const check = async () => {
      if (pollTokenRef.current !== pollId) return;
      try {
        const res = await apiJson<{
          loggedIn?: boolean;
          expired?: boolean;
          nickname?: string;
          status?: string;
        }>(`/api/qishui/login/check?token=${encodeURIComponent(qrToken)}&t=${Date.now()}`, {
          timeoutMs: 8000,
          cache: 'no-store',
        });
        if (pollTokenRef.current !== pollId) return;
        if (res.loggedIn) {
          stopPolling();
          setQrStatus('汽水音乐登录成功！');
          setQrStateClass('scan');
          refreshLoginStatuses().catch(() => {});
          refreshAllUserPlaylists().catch(() => {});
          onToast?.(`汽水音乐已登录${res.nickname ? `：${res.nickname}` : ''}`);
          closeLogin();
          return;
        }
        if (res.expired) {
          stopPolling();
          setQrStatus('二维码已过期，请刷新');
          setQrStateClass('fail');
          return;
        }
        pollTimerRef.current = setTimeout(check, 2200);
      } catch {
        if (pollTokenRef.current === pollId) {
          pollTimerRef.current = setTimeout(check, 3000);
        }
      }
    };
    pollTimerRef.current = setTimeout(check, 1200);
  };

  const startProviderLogin = async (target: LoginProvider) => {
    stopPolling();
    setQrImg('');
    setAuthBusy(true);
    const token = pollTokenRef.current;

    try {
      if (target === 'netease') {
        setQrStatus('正在获取网易云二维码...');
        setQrStateClass('preview');
        let keyRes = await apiJson<{ key?: string; error?: string }>('/api/login/qr/key');
        if (keyRes.error === 'LOGIN_EASTER_EGG_LOCKED' && easterEggUnlocked) {
          await desktopWindow.unlockLoginEasterEgg('世界和平').catch(() => {});
          keyRes = await apiJson<{ key?: string; error?: string }>('/api/login/qr/key');
        }
        if (!keyRes.key) throw new Error('获取二维码 Key 失败');
        const qrRes = await apiJson<{ img?: string }>(
          `/api/login/qr/create?key=${encodeURIComponent(keyRes.key)}`
        );
        if (!qrRes.img) throw new Error('生成二维码失败');
        if (pollTokenRef.current !== token) return;
        setQrImg(qrRes.img);
        setQrStatus('请使用网易云音乐 App 扫码');
        setQrStateClass('');
        pollNeteaseQr(keyRes.key, token);
      } else if (target === 'qishui') {
        setQrStatus('正在获取汽水音乐二维码...');
        setQrStateClass('preview');
        let qrRes = await apiJson<{ token?: string; qrcode?: string; error?: string }>(
          `/api/qishui/login/qrcode?t=${Date.now()}`
        );
        if (qrRes.error === 'LOGIN_EASTER_EGG_LOCKED' && easterEggUnlocked) {
          await desktopWindow.unlockLoginEasterEgg('世界和平').catch(() => {});
          qrRes = await apiJson<{ token?: string; qrcode?: string; error?: string }>(
            `/api/qishui/login/qrcode?t=${Date.now()}`
          );
        }
        if (!qrRes.token || !qrRes.qrcode) throw new Error('生成汽水音乐二维码失败');
        if (pollTokenRef.current !== token) return;
        setQrImg(qrRes.qrcode);
        setQrStatus('请使用抖音 App 扫码并确认登录');
        setQrStateClass('');
        pollQishuiQr(qrRes.token, token);
      } else if (target === 'qq') {
        const info = await apiJson<{ loggedIn?: boolean; nickname?: string }>(
          `/api/qq/login/status?t=${Date.now()}`
        ).catch(() => ({ loggedIn: false, nickname: '' }));
        if (info.loggedIn) {
          await refreshLoginStatuses();
          setQrStatus(`QQ 音乐已登录 · ${info.nickname || '已保存会话'}`);
          setQrStateClass('scan');
        } else {
          setCookieOpen(true);
          setQrStatus('请粘贴 QQ 音乐网页 Cookie 或使用官方窗口登录后导入');
          setQrStateClass('preview');
        }
      } else if (target === 'kugou') {
        const info = await apiJson<{ loggedIn?: boolean; nickname?: string }>(
          `/api/kugou/login/status?t=${Date.now()}`
        ).catch(() => ({ loggedIn: false, nickname: '' }));
        if (info.loggedIn) {
          await refreshLoginStatuses();
          setQrStatus(`酷狗音乐已登录 · ${info.nickname || '已保存会话'}`);
          setQrStateClass('scan');
        } else {
          setCookieOpen(true);
          setQrStatus('请粘贴酷狗音乐 Cookie 导入会话');
          setQrStateClass('preview');
        }
      }
    } catch (err) {
      setQrStatus(err instanceof Error ? err.message : '登录请求失败');
      setQrStateClass('fail');
    } finally {
      setAuthBusy(false);
    }
  };

  const handleOfficialLogin = async (target: LoginProvider) => {
    setAuthBusy(true);
    setQrStatus('正在打开官方登录弹窗...');
    setQrStateClass('preview');
    try {
      const forceReauth = Boolean(platformLogins[target]?.loggedIn);
      const res =
        target === 'netease'
          ? await desktopWindow.openNeteaseMusicLogin(forceReauth)
          : target === 'qq'
          ? await desktopWindow.openQQMusicLogin(forceReauth)
          : target === 'kugou'
          ? await desktopWindow.openKugouMusicLogin(forceReauth)
          : await desktopWindow.openQishuiAuthLogin(forceReauth);
      if (!res.ok) throw new Error(res.error || '无法打开官方登录弹窗');
      setQrStatus('请在弹出的官方窗口中完成登录');
      setQrStateClass('scan');
    } catch (err) {
      setQrStatus(err instanceof Error ? err.message : '官方登录弹窗打开失败');
      setQrStateClass('fail');
    } finally {
      setAuthBusy(false);
    }
  };

  const handleSubmitCookie = async () => {
    const trimmed = cookieText.trim();
    if (!trimmed) {
      setQrStatus('请先粘贴 Cookie 内容');
      setQrStateClass('fail');
      return;
    }
    setAuthBusy(true);
    setQrStatus('正在同步登录会话...');
    setQrStateClass('preview');
    try {
      const endpoint =
        provider === 'qq'
          ? '/api/qq/login/cookie'
          : provider === 'kugou'
          ? '/api/kugou/login/cookie'
          : provider === 'qishui'
          ? '/api/qishui/login/cookie'
          : '/api/login/cookie';

      const res = await apiJson<{ loggedIn?: boolean; nickname?: string; message?: string; error?: string }>(
        endpoint,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ cookie: trimmed }),
        }
      );
      if (!res.loggedIn) {
        throw new Error(res.message || res.error || 'Cookie 无效或已过期');
      }
      setCookieText('');
      setCookieOpen(false);
      await refreshLoginStatuses();
      setQrStatus(`已登录：${res.nickname || provider.toUpperCase()}`);
      setQrStateClass('scan');
      onToast?.(`${PROVIDERS.find((p) => p.key === provider)?.name || provider} 登录成功`);
    } catch (err) {
      setQrStatus(err instanceof Error ? err.message : 'Cookie 导入失败');
      setQrStateClass('fail');
    } finally {
      setAuthBusy(false);
    }
  };

  const handleLogoutProvider = async (target: LoginProvider) => {
    stopPolling();
    const endpoint =
      target === 'qq'
        ? '/api/qq/logout'
        : target === 'kugou'
        ? '/api/kugou/logout'
        : target === 'qishui'
        ? '/api/qishui/logout'
        : '/api/logout';
    try {
      await apiJson(endpoint, { method: 'POST' }).catch(() => apiJson(endpoint));
    } catch {
      // ignore
    }
    await desktopWindow.clearPlatformLoginSession(target).catch(() => ({ ok: false }));
    await refreshLoginStatuses();
    setQrImg('');
    setQrStatus('已退出当前平台登录');
    setQrStateClass('');
    onToast?.('已退出平台登录');
  };

  const handleLogoutAllAndReset = async () => {
    stopPolling();
    await Promise.all([
      apiJson('/api/logout').catch(() => {}),
      apiJson('/api/qq/logout').catch(() => {}),
      apiJson('/api/kugou/logout').catch(() => {}),
      apiJson('/api/qishui/logout').catch(() => {}),
    ]);
    await Promise.all(
      (['netease', 'qq', 'kugou', 'qishui'] as LoginProvider[]).map((p) =>
        desktopWindow.clearPlatformLoginSession(p).catch(() => ({ ok: false }))
      )
    );
    await resetEasterEgg();
    setConfirmLogoutAll(false);
    setEyeTaps(0);
    setQrImg('');
    setCookieText('');
    setWishText('');
    setEggStatusText('');
    onToast?.('已退出全部平台并锁定登录入口');
  };

  const loggedInProviders = (['netease', 'qq', 'kugou', 'qishui'] as ProviderType[]).filter(
    (p) => platformLogins[p as LoginProvider]?.loggedIn
  );
  const primaryAccount = loggedInProviders.length
    ? platformLogins[loggedInProviders[0] as LoginProvider]
    : null;
  const activeEntry = platformLogins[provider];

  const release = updateInfo?.release;
  const hasUpdate = Boolean(updateInfo?.hasUpdate ?? updateInfo?.updateAvailable);
  const rawPages =
    updateInfo?.downloadPages?.length ? updateInfo.downloadPages : release?.downloadPages || [];
  const downloadPages = rawPages.filter((p) => isSafeHttpsUrl(p.url));
  const fallbackCandidates = [updateInfo?.releaseUrl, release?.downloadPageUrl, release?.htmlUrl];
  const safeFallbackUrl = fallbackCandidates.find((u) => isSafeHttpsUrl(u)) || '';
  const rawNotes = updateInfo?.notes || release?.notes || release?.summary || '';
  const updateNotes = Array.isArray(rawNotes) ? rawNotes.join('\n') : String(rawNotes || '');
  const targetUpdateUrl = downloadPages[selectedPageIdx]?.url || downloadPages[0]?.url || safeFallbackUrl;

  const handleOpenUpdateExternal = async () => {
    if (!targetUpdateUrl || !isSafeHttpsUrl(targetUpdateUrl)) {
      onToast?.('暂无可用的安全下载链接');
      return;
    }
    const res = await desktopWindow.openUpdatePage(targetUpdateUrl);
    if (!res.ok) {
      onToast?.(res.error || '无法打开更新页面');
    }
  };

  const wishChars = Array.from(wishText).slice(0, 4);

  return (
    <>
      <div id="top-right">
        <button
          id="home-btn"
          className="icon-btn"
          type="button"
          onClick={() => onGoHome?.()}
          title="回到 Home"
          aria-label="回到 Home"
        >
          <svg width="19" height="19" fill="none" stroke="currentColor" strokeWidth="1.9" viewBox="0 0 24 24">
            <path d="M3 10.8 12 3l9 7.8" />
            <path d="M5 10v10h14V10" />
            <path d="M9.5 20v-5h5v5" />
          </svg>
        </button>

        <button
          id="user-btn"
          className={`icon-btn ${
            primaryAccount?.loggedIn ? 'logged-in' : 'logged-out login-eye-avatar'
          }`}
          type="button"
          onClick={openLogin}
          title={
            primaryAccount?.loggedIn
              ? `已登录：${primaryAccount.nickname || '账号中心'}`
              : '登录账号'
          }
          aria-label="账号与登录"
        >
          {primaryAccount?.avatar || primaryAccount?.avatarUrl ? (
            <img
              src={primaryAccount.avatar || primaryAccount.avatarUrl}
              alt={primaryAccount.nickname || 'user'}
              style={{ width: '100%', height: '100%', borderRadius: '50%', objectFit: 'cover' }}
            />
          ) : (
            <span className="login-easter-eyes compact" aria-hidden="true">
              <i className="login-easter-eye login-easter-eye-big" />
              <i className="login-easter-eye login-easter-eye-small" />
            </span>
          )}
        </button>
      </div>

      {/* 登录 / 彩蛋解锁模态 */}
      <div
        id="login-modal"
        className={`modal-mask${loginOpen ? ' show' : ''}${
          easterEggUnlocked ? '' : ' login-easter-egg-locked'
        }`}
        role="dialog"
        aria-modal="true"
        aria-hidden={!loginOpen}
        onClick={(e) => {
          if (e.target === e.currentTarget) closeLogin();
        }}
      >
        <div className={`modal dual-login-modal${easterEggUnlocked ? ' login-details-open' : ''}`}>
          <div className="login-panel-head">
            <div>
              <b>{easterEggUnlocked ? '多平台账号接入' : '登录解锁'}</b>
              <small>{easterEggUnlocked ? '支持网易云 / QQ / 酷狗 / 汽水' : '输入心愿暗号'}</small>
            </div>
            {easterEggUnlocked && (
              <button
                id="login-reset-all-btn"
                className="login-reset-all-btn"
                type="button"
                onClick={() => {
                  if (!confirmLogoutAll) {
                    setConfirmLogoutAll(true);
                    return;
                  }
                  handleLogoutAllAndReset().catch(() => {});
                }}
                title="清除所有平台登录并重新锁定彩蛋"
              >
                {confirmLogoutAll ? '再次点击确认退出全部账号' : '退出登录'}
              </button>
            )}
            <button
              className="login-panel-close"
              type="button"
              aria-label="关闭登录弹窗"
              onClick={closeLogin}
            >
              ×
            </button>
          </div>

          {!easterEggUnlocked ? (
            <section
              id="login-easter-egg-gate"
              className={`login-easter-egg-gate${wishRevealed ? ' is-revealed' : ''}`}
            >
              <button
                id="login-easter-eye-trigger"
                className="login-easter-eye-trigger"
                type="button"
                onClick={() => setEyeTaps((n) => Math.min(5, n + 1))}
                aria-label="轻触大小眼"
              >
                <span className="login-easter-eyes" aria-hidden="true">
                  <i className="login-easter-eye login-easter-eye-big" />
                  <i className="login-easter-eye login-easter-eye-small" />
                </span>
              </button>
              <div id="login-easter-hint" className="login-easter-hint" aria-live="polite">
                {wishRevealed ? '许下四个字的心愿' : '轻触大小眼'}
              </div>

              {wishRevealed && (
              <form
                id="login-easter-egg-wish"
                className="login-easter-egg-wish"
                onSubmit={handleSubmitEasterEgg}
              >
                <h1>心愿是</h1>
                <div className="login-easter-input-shell">
                  <div id="login-easter-egg-cells" className="login-easter-cells" aria-hidden="true">
                    {[0, 1, 2, 3].map((i) => (
                      <span
                        key={i}
                        className={`login-easter-cell${wishChars[i] ? ' filled' : ''}`}
                      >
                        {wishChars[i] || ''}
                      </span>
                    ))}
                  </div>
                  <input
                    id="login-easter-egg-input"
                    className="login-easter-input"
                    type="text"
                    inputMode="text"
                    autoComplete="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    maxLength={12}
                    aria-label="输入四个字的愿望"
                    value={wishText}
                    onChange={(e) => {
                      setWishText(e.target.value);
                      setEggStatusText('');
                      setEggStatusMode('');
                    }}
                  />
                </div>
                <div
                  id="login-easter-status"
                  className="login-easter-status"
                  data-mode={eggStatusMode || undefined}
                  aria-live="polite"
                >
                  {eggStatusText}
                </div>
                <div className="btn-row" style={{ marginTop: 14, justifyContent: 'center' }}>
                  <button
                    type="submit"
                    className="modal-btn primary"
                    disabled={eggBusy}
                  >
                    {eggBusy ? '验证中...' : '解锁登录'}
                  </button>
                </div>
              </form>
              )}
            </section>
          ) : (
            <>
              <div id="login-node-graph" className="login-node-graph" data-provider={provider}>
                <svg
                  id="login-workflow-svg"
                  className="workflow-link-layer login-link-layer"
                  viewBox="0 0 528 220"
                  preserveAspectRatio="none"
                  aria-hidden="true"
                >
                  {PROVIDERS.map((p, idx) => {
                    const isSelected = provider === p.key;
                    const isLogged = Boolean(platformLogins[p.key]?.loggedIn);
                    if (!isSelected && !isLogged) return null;
                    const y1 = 28 + idx * 54;
                    const y2 = 110;
                    const d = `M 250 ${y1} C 306 ${y1}, 304 ${y2}, 360 ${y2}`;
                    return (
                      <path
                        key={p.key}
                        d={d}
                        className={`workflow-link ${isLogged ? 'active' : 'pending'}${
                          isSelected ? ' selected' : ''
                        }`}
                      />
                    );
                  })}
                </svg>

                <div className="login-platform-tabs login-node-providers" id="login-platform-tabs">
                  {PROVIDERS.map((p) => {
                    const active = provider === p.key;
                    const entry = platformLogins[p.key];
                    const loggedIn = Boolean(entry?.loggedIn);
                    const avatar = entry?.avatar || entry?.avatarUrl;
                    return (
                      <button
                        key={p.key}
                        id={`login-provider-${p.key}`}
                        className={`${p.key}${active ? ' active' : ''}${
                          loggedIn ? ' connected external-on' : ''
                        } workflow-node`}
                        type="button"
                        data-login-provider={p.key}
                        onClick={() => {
                          stopPolling();
                          setProvider(p.key);
                          setQrImg('');
                          setCookieOpen(p.key === 'qq' || p.key === 'kugou');
                          setQrStatus(
                            loggedIn
                              ? `已登录 · ${entry?.nickname || p.name}`
                              : `选择下方登录方式连接 ${p.name}`
                          );
                          setQrStateClass(loggedIn ? 'scan' : '');
                          if (!loggedIn && (p.key === 'netease' || p.key === 'qishui')) {
                            setTimeout(() => {
                              startProviderLogin(p.key).catch(() => {});
                            }, 30);
                          }
                        }}
                      >
                        <span className="login-provider-sort-handle" aria-hidden="true">
                          <i />
                          <i />
                          <i />
                        </span>
                        <span className={`provider-logo${loggedIn && avatar ? ' has-avatar' : ''}`}>
                          {loggedIn && avatar ? <img src={avatar} alt="" /> : p.code}
                        </span>
                        <b>{p.name}</b>
                        <span
                          className={`login-provider-state-badge ${
                            loggedIn ? 'normal' : 'hidden'
                          }`}
                        >
                          {loggedIn ? '已登录' : ''}
                        </span>
                        <span
                          className="login-provider-external-switch"
                          aria-hidden="true"
                        >
                          <span className="login-provider-external-label">
                            {loggedIn ? '在线' : '待连'}
                          </span>
                          <i />
                        </span>
                        <span
                          className="flow-port out"
                          data-login-provider-output={p.key}
                          title="已连接至 MR 接入口"
                        />
                      </button>
                    );
                  })}
                </div>

                <div className="login-node-hub workflow-node" data-login-node="mr">
                  <span className="flow-port in" data-login-mr-target="mr" title="MR 接入口" />
                  <span className="login-node-orb">MR</span>
                  <div className="login-node-copy">
                    <b>MR</b>
                    <small>
                      {activeEntry?.loggedIn
                        ? `已接入 · ${activeEntry.nickname || provider.toUpperCase()}`
                        : `${PROVIDERS.find((p) => p.key === provider)?.name} 接入口`}
                    </small>
                  </div>
                  <div className="login-mode-nodes">
                    <button
                      id="login-mode-official"
                      className={`login-mode-node workflow-node${!cookieOpen ? ' active' : ''}`}
                      type="button"
                      data-login-mode="official"
                      onClick={() => {
                        setCookieOpen(false);
                        if (
                          !qrImg &&
                          !authBusy &&
                          (provider === 'netease' || provider === 'qishui')
                        ) {
                          startProviderLogin(provider).catch(() => {});
                        }
                      }}
                    >
                      <span className="provider-logo">QR</span>
                      <b>{provider === 'kugou' ? '官网' : '扫码'}</b>
                      <small>官方窗口</small>
                    </button>
                    <button
                      id="login-mode-cookie"
                      className={`login-mode-node workflow-node${cookieOpen ? ' active' : ''}`}
                      type="button"
                      data-login-mode="cookie"
                      onClick={() => setCookieOpen(true)}
                    >
                      <span className="provider-logo">CK</span>
                      <b>Cookie</b>
                      <small>保存会话</small>
                    </button>
                  </div>
                </div>
              </div>

              <div id="login-auth-drawer" className="login-auth-drawer show">
                <div className="login-auth-copy">
                  <h2 id="login-modal-title">
                    {PROVIDERS.find((p) => p.key === provider)?.name} 账号连接
                  </h2>
                  <div id="login-modal-desc" className="desc">
                    {activeEntry?.loggedIn
                      ? `当前账号：${activeEntry.nickname || activeEntry.userId || '已登录'}`
                      : '支持官方二维码扫码、官方登录窗口或手动导入 Cookie 会话。'}
                  </div>
                  <div id="qr-status" className={qrStateClass || undefined}>
                    {qrStatus}
                  </div>

                  <div
                    id="qq-cookie-panel"
                    className={`qq-cookie-panel${
                      cookieOpen || provider === 'qq' || provider === 'kugou' ? ' show' : ''
                    }`}
                  >
                    <textarea
                      id="qq-cookie-input"
                      className="qq-cookie-input"
                      spellCheck={false}
                      autoComplete="off"
                      placeholder={
                        provider === 'qq'
                          ? 'uin=...; qqmusic_key=...; qm_keyst=...'
                          : provider === 'kugou'
                          ? '粘贴酷狗音乐 Cookie'
                          : provider === 'qishui'
                          ? '粘贴汽水音乐 Cookie 兜底'
                          : 'MUSIC_U=...'
                      }
                      value={cookieText}
                      onChange={(e) => setCookieText(e.target.value)}
                    />
                    <div className="qq-cookie-actions">
                      <div className="qq-cookie-note">导入后将保存到本地安全会话。</div>
                      <button
                        id="qq-cookie-save-btn"
                        className="modal-btn primary"
                        type="button"
                        disabled={authBusy}
                        onClick={() => {
                          handleSubmitCookie().catch(() => {});
                        }}
                      >
                        保存 Cookie
                      </button>
                    </div>
                  </div>
                </div>

                <div className="login-auth-body">
                  <div
                    id="qr-shell"
                    className={`qr-shell${!qrImg ? ' web-login-preview' : ''}${
                      provider === 'netease' ? ' netease-preview' : ''
                    }`}
                  >
                    {qrImg && <img id="qr-img" src={qrImg} alt={`${provider} 登录二维码`} />}
                    <button
                      id="qq-web-login-card"
                      className="qq-login-mark"
                      type="button"
                      disabled={authBusy}
                      onClick={() => {
                        if (provider === 'netease' || provider === 'qishui') {
                          startProviderLogin(provider).catch(() => {});
                        } else {
                          handleOfficialLogin(provider).catch(() => {});
                        }
                      }}
                    >
                      <b>{PROVIDERS.find((p) => p.key === provider)?.code}</b>
                      <span>
                        {provider === 'netease' || provider === 'qishui'
                          ? '点击获取扫码二维码'
                          : '打开官方扫码窗口'}
                      </span>
                    </button>
                  </div>
                </div>

                <div className="btn-row login-auth-actions">
                  {activeEntry?.loggedIn && (
                    <button
                      type="button"
                      className="modal-btn"
                      onClick={() => {
                        handleLogoutProvider(provider).catch(() => {});
                      }}
                    >
                      退出当前平台
                    </button>
                  )}
                  <button
                    id="official-login-btn"
                    className="modal-btn"
                    type="button"
                    disabled={authBusy}
                    onClick={() => {
                      handleOfficialLogin(provider).catch(() => {});
                    }}
                  >
                    官方弹窗登录
                  </button>
                  <button
                    id="qq-cookie-toggle-btn"
                    className="modal-btn show"
                    type="button"
                    onClick={() => setCookieOpen((v) => !v)}
                  >
                    {cookieOpen ? '收起 Cookie' : '手动导入 Cookie'}
                  </button>
                  <button
                    id="refresh-qr-btn"
                    className="modal-btn primary"
                    type="button"
                    disabled={authBusy}
                    onClick={() => {
                      startProviderLogin(provider).catch(() => {});
                    }}
                  >
                    {provider === 'netease' || provider === 'qishui'
                      ? '获取二维码登录'
                      : '检测 / 登录'}
                  </button>
                </div>
              </div>
            </>
          )}
        </div>
      </div>

      {cinematic && (
        <div
          id="login-easter-unlock-cinematic"
          className="login-easter-unlock-cinematic is-mounted is-positioned is-extracting is-ready"
          role="button"
          tabIndex={-1}
          aria-label="世界和平，点击继续"
          onClick={() => setCinematic(false)}
        >
          <div id="login-easter-unlock-phrase" className="login-easter-unlock-phrase" aria-hidden="true">
            <span>世</span>
            <span>界</span>
            <span>和</span>
            <span>平</span>
          </div>
        </div>
      )}

      {/* 版本更新详情模态 */}
      <div
        id="update-modal"
        className={`modal-mask${updateOpen ? ' show' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-hidden={!updateOpen}
        onClick={(e) => {
          if (e.target === e.currentTarget) closeUpdate();
        }}
      >
        <div className="modal update-modal">
          <div className="update-panel-inner">
            <div className="update-panel-head">
              <div>
                <div className="update-kicker">MINERADIO</div>
                <div id="update-modal-title" className="update-title">
                  {hasUpdate ? '发现新版本' : '版本信息'}
                </div>
                <div id="update-modal-version" className="update-version">
                  {updateInfo?.latestVersion || updateInfo?.currentVersion || 'v2.2.0'}
                </div>
              </div>
            </div>

            <div className="update-hero">
              <div id="update-hero-main" className="update-hero-main">
                {updateInfo?.hasUpdate
                  ? `当前版本 ${updateInfo.currentVersion || 'v2.2.0'}，最新版本 ${
                      updateInfo.latestVersion || ''
                    } 已发布。`
                  : `当前已是最新版本（${updateInfo?.currentVersion || 'v2.2.0'}）。`}
              </div>
            </div>

            {updateNotes && (
              <div id="update-list" className="update-list" style={{ marginBottom: 14, fontSize: 12 }}>
                {updateNotes}
              </div>
            )}

            {downloadPages.length > 0 && (
              <div
                id="update-download-sources"
                className="update-download-sources"
                aria-label="下载线路"
                style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 14 }}
              >
                {downloadPages.map((page, idx) => (
                  <button
                    key={page.url}
                    type="button"
                    className={`fx-mini-btn${selectedPageIdx === idx ? '' : ' ghost'}`}
                    onClick={() => setSelectedPageIdx(idx)}
                  >
                    {page.label}
                  </button>
                ))}
              </div>
            )}

            <div className="update-actions">
              <button
                id="update-primary-btn"
                className="update-primary-btn"
                type="button"
                disabled={!targetUpdateUrl}
                onClick={() => {
                  handleOpenUpdateExternal().catch(() => {});
                }}
              >
                <span id="update-btn-fill" className="update-btn-fill" />
                <span id="update-btn-label" className="update-btn-label">
                  {targetUpdateUrl ? '前往下载页面' : '重新检查更新'}
                </span>
              </button>
              {!targetUpdateUrl && (
                <button
                  className="update-secondary-btn"
                  type="button"
                  onClick={() => {
                    checkUpdates().catch(() => {});
                  }}
                >
                  检查更新
                </button>
              )}
              <button
                className="update-secondary-btn"
                type="button"
                onClick={closeUpdate}
              >
                关闭
              </button>
            </div>
            <div id="update-footnote" className="update-footnote">
              将在浏览器打开安全 HTTPS 下载页面，软件不会在本地执行未知补丁。
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

export default AccountAndLoginModal;
