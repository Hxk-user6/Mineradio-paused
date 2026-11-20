import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  apiFetch,
  audioProxyUrl,
  coverProxyUrl,
  desktopWindow,
  emitLocalEvent,
  getRuntimeEndpoints,
  isTauriRuntime,
  listenEvent,
  resetRuntimeEndpointsCache,
  resolveMediaUrl,
  type TauriInternals,
} from './tauriBridge';

describe('tauriBridge', () => {
  beforeEach(() => {
    delete window.__TAURI_INTERNALS__;
    delete window.__TAURI__;
    try {
      window.localStorage?.clear?.();
    } catch {
      // ignore Node localStorage experimental warning
    }
    resetRuntimeEndpointsCache();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    delete window.__TAURI_INTERNALS__;
    delete window.__TAURI__;
    resetRuntimeEndpointsCache();
  });

  it('在非 Tauri 环境下正确降级为纯浏览器模式与默认 httpBase', async () => {
    expect(isTauriRuntime()).toBe(false);
    expect(desktopWindow.isDesktop).toBe(false);

    const endpoints = await getRuntimeEndpoints();
    expect(endpoints).toEqual({
      httpBase: 'http://127.0.0.1:3000',
      mediaToken: '',
    });

    const state = await desktopWindow.getState();
    expect(state.isDesktop).toBe(false);
    expect(state.isPrimaryDisplay).toBe(true);
  });

  it('audioProxyUrl 与 coverProxyUrl 能正确转换外部链接、本地协议与 data/blob URL', async () => {
    await getRuntimeEndpoints();

    expect(audioProxyUrl('')).toBe('');
    expect(audioProxyUrl('blob:http://localhost/123')).toBe('blob:http://localhost/123');
    expect(audioProxyUrl('https://music.example.com/track.mp3?a=1')).toBe(
      'http://127.0.0.1:3000/api/audio?url=https%3A%2F%2Fmusic.example.com%2Ftrack.mp3%3Fa%3D1'
    );

    expect(coverProxyUrl('')).toBe('');
    expect(coverProxyUrl('data:image/png;base64,AAA')).toBe('data:image/png;base64,AAA');
    expect(coverProxyUrl('https://img.example.com/cover.jpg')).toBe(
      'http://127.0.0.1:3000/api/cover?url=https%3A%2F%2Fimg.example.com%2Fcover.jpg'
    );

    expect(resolveMediaUrl('mineradio-local://audio/track-1')).toBe(
      'http://127.0.0.1:3000/local-media/audio/track-1'
    );
    expect(resolveMediaUrl('mineradio-wallpaper://localhost/scene-9')).toBe(
      'http://127.0.0.1:3000/we-media/scene-9'
    );
  });

  it('Mock __TAURI_INTERNALS__.invoke 时能路由 get_runtime_endpoints、媒体令牌与桌面命令', async () => {
    const invokeSpy = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case 'get_runtime_endpoints':
          return { httpBase: 'http://127.0.0.1:45678/', mediaToken: 'tok-999' };
        case 'get_login_easter_egg_status':
          return {
            ok: true,
            gateVersion: 'world-peace-v1',
            unlocked: false,
            resetComplete: true,
          };
        case 'unlock_login_easter_egg':
          return {
            ok: args?.password === '世界和平',
            gateVersion: 'world-peace-v1',
            unlocked: args?.password === '世界和平',
            resetComplete: true,
          };
        case 'authorize_local_music':
          return { ok: true, token: 'auth-tok-1' };
        case 'import_local_music':
          return {
            ok: true,
            count: 1,
            tracks: [{ id: 'loc-1', localFileId: 'loc-1', name: 'Demo', provider: 'local' }],
          };
        case 'desktop_window_minimize':
          return { ok: true };
        case 'set_desktop_lyrics_enabled':
          return { ok: true, enabled: Boolean(args?.enabled) };
        default:
          return { ok: true };
      }
    });

    window.__TAURI_INTERNALS__ = {
      invoke: invokeSpy as unknown as TauriInternals['invoke'],
    };

    expect(isTauriRuntime()).toBe(true);
    expect(desktopWindow.isDesktop).toBe(true);

    const endpoints = await getRuntimeEndpoints();
    expect(endpoints).toEqual({
      httpBase: 'http://127.0.0.1:45678',
      mediaToken: 'tok-999',
    });

    // Tauri 下显式 HTTP 兜底会自动携带 cap=mediaToken
    expect(resolveMediaUrl('mineradio-local://audio/abc', { preferHttpFallback: true })).toBe(
      'http://127.0.0.1:45678/local-media/audio/abc?cap=tok-999'
    );
    // Tauri 下默认保留自定义协议
    expect(resolveMediaUrl('mineradio-local://audio/abc')).toBe('mineradio-local://audio/abc');

    expect(audioProxyUrl('https://cdn.example.com/a.flac')).toBe(
      'http://127.0.0.1:45678/api/audio?url=https%3A%2F%2Fcdn.example.com%2Fa.flac'
    );

    const gateStatus = await desktopWindow.getLoginEasterEggStatus();
    expect(gateStatus.unlocked).toBe(false);

    const unlocked = await desktopWindow.unlockLoginEasterEgg('世界和平');
    expect(unlocked.ok).toBe(true);
    expect(unlocked.unlocked).toBe(true);

    const imported = await desktopWindow.authorizeAndImport([{ path: '/music/demo.flac' }]);
    expect(imported.ok).toBe(true);
    expect(imported.count).toBe(1);
    expect(invokeSpy).toHaveBeenCalledWith('authorize_local_music', {
      files: [{ path: '/music/demo.flac' }],
    });
    expect(invokeSpy).toHaveBeenCalledWith('import_local_music', { token: 'auth-tok-1' });

    await desktopWindow.minimize();
    expect(invokeSpy).toHaveBeenCalledWith('desktop_window_minimize', undefined);

    const lrcRes = await desktopWindow.setDesktopLyricsEnabled(true, { line: 'Hello' });
    expect(lrcRes).toEqual({ ok: true, enabled: true });
  });

  it('apiFetch 会将相对路径拼接至当前 runtime httpBase', async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ ok: true }), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);

    await apiFetch('/api/app/version');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [calledUrl] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(calledUrl).toBe('http://127.0.0.1:3000/api/app/version');
  });

  it('listenEvent 在非 Tauri 环境可通过本地事件总线收发并正确退订', async () => {
    const received: Array<{ locked: boolean }> = [];
    const unlisten = await listenEvent<{ locked: boolean }>(
      'mineradio-desktop-lyrics-lock-state',
      (payload) => {
        received.push(payload);
      }
    );

    emitLocalEvent('mineradio-desktop-lyrics-lock-state', { locked: true });
    expect(received).toEqual([{ locked: true }]);

    unlisten();
    emitLocalEvent('mineradio-desktop-lyrics-lock-state', { locked: false });
    expect(received).toEqual([{ locked: true }]);
  });
});
