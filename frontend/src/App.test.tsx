import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { DesktopLyricsApp } from './DesktopLyricsApp';
import { AccountAndLoginModal } from './components/AccountAndLoginModal';
import { SearchBar } from './components/SearchBar';
import { desktopWindow, emitLocalEvent } from './bridge/tauriBridge';
import { usePlayerStore } from './store/playerStore';
import { DEFAULT_FX_SETTINGS } from './types/mineradio';

describe('Mineradio UI & Desktop Lyrics', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    window.localStorage?.clear();

    if (typeof globalThis.ResizeObserver === 'undefined') {
      globalThis.ResizeObserver = class ResizeObserver {
        observe() {}
        unobserve() {}
        disconnect() {}
      };
    }

    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    );

    usePlayerStore.setState({
      playQueue: [],
      currentIdx: -1,
      currentSong: null,
      playing: false,
      currentTime: 0,
      duration: 0,
      volume: 0.72,
      muted: false,
      playMode: 'loop',
      quality: 'exhigh',
      audioError: null,
      automixEnabled: false,
      cuefieldStatus: 'disabled',
      lyrics: [],
      activeLyricIdx: -1,
      lyricOffsetSec: 0,
      desktopLyricsEnabled: false,
      desktopLyricsLocked: false,
      searchMode: 'song',
      searchQuery: '',
      searchResults: [],
      searchLoading: false,
      homeDiscover: null,
      weatherRadio: null,
      userPlaylists: [],
      builtInPlaylists: [],
      localTracks: [],
      activePlaylistTracks: [],
      easterEggUnlocked: false,
      fx: { ...DEFAULT_FX_SETTINGS },
      activePreset: 0,
      diyMode: false,
      shelfOpen: false,
      fxPanelOpen: false,
      playlistPanelOpen: false,
      splashDone: true,
      updateInfo: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('App 挂载渲染核心区域与 DOM 骨架', async () => {
    const { container } = render(<App />);

    expect(container.querySelector('#desktop-window-shell')).not.toBeNull();
    expect(container.querySelector('#desktop-titlebar')).not.toBeNull();
    expect(container.querySelector('#custom-bg')).not.toBeNull();
    expect(container.querySelector('#wallpaper-engine-layer')).not.toBeNull();
    expect(container.querySelector('#album-bg')).not.toBeNull();
    expect(container.querySelector('#canvas-container')).not.toBeNull();
    expect(container.querySelector('#search-area')).not.toBeNull();
    expect(container.querySelector('#empty-home')).not.toBeNull();
    expect(container.querySelector('#controls')).not.toBeNull();
    expect(container.querySelector('#fx-panel')).not.toBeNull();
    expect(container.querySelector('#playlist-panel')).not.toBeNull();
    expect(container.querySelector('#desktop-mode-control-dock')).not.toBeNull();

    await waitFor(() => {
      expect(document.body.classList.contains('desktop-shell')).toBe(true);
    });
  });

  it('SearchBar 支持搜索模式切换与搜索结果渲染', async () => {
    const searchSpy = vi.fn(async (query: string, mode?: 'song' | 'netease' | 'qq' | 'kugou' | 'qishui' | 'podcast') => {
      const mockResults = [
        {
          id: '101',
          name: `${query}-测试曲目`,
          artist: '测试歌手',
          album: '测试专辑',
          provider: (mode === 'qq' ? 'qq' : 'netease') as 'qq' | 'netease',
        },
      ];
      usePlayerStore.setState({ searchResults: mockResults, searchLoading: false });
      return mockResults;
    });
    usePlayerStore.setState({ searchTracks: searchSpy });

    render(<SearchBar />);

    const input = screen.getByPlaceholderText('搜索歌曲、歌手...');
    fireEvent.change(input, { target: { value: '晴天' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    await waitFor(() => {
      expect(searchSpy).toHaveBeenCalledWith('晴天', 'song');
    });
    expect(await screen.findByText('晴天-测试曲目')).toBeTruthy();

    const qqTab = screen.getByRole('tab', { name: 'QQ' });
    fireEvent.click(qqTab);

    await waitFor(() => {
      expect(usePlayerStore.getState().searchMode).toBe('qq');
      expect(searchSpy).toHaveBeenCalledWith('晴天', 'qq');
    });
  });

  it('AccountAndLoginModal 支持“世界和平”彩蛋解锁并进入多平台登录', async () => {
    const unlockSpy = vi.fn(async (password: string) => {
      const ok = password.trim() === '世界和平';
      if (ok) {
        usePlayerStore.setState({ easterEggUnlocked: true });
      }
      return {
        ok,
        gateVersion: 'world-peace-v1',
        unlocked: ok,
        resetComplete: true,
        message: ok ? '已解锁全平台登录入口' : '暗号错误',
      };
    });
    usePlayerStore.setState({
      easterEggUnlocked: false,
      unlockEasterEgg: unlockSpy,
    });

    render(<AccountAndLoginModal />);

    const userBtn = screen.getByRole('button', { name: '账号与登录' });
    fireEvent.click(userBtn);

    const wishInput = screen.getByLabelText('输入四个字的愿望');
    fireEvent.change(wishInput, { target: { value: '世界和平' } });
    fireEvent.click(screen.getByRole('button', { name: '解锁登录' }));

    await waitFor(() => {
      expect(unlockSpy).toHaveBeenCalledWith('世界和平');
    });

    expect(await screen.findByText('网易云')).toBeTruthy();
    expect(screen.getByText('QQ 音乐')).toBeTruthy();
    expect(screen.getByText('酷狗音乐')).toBeTruthy();
    expect(screen.getByText('汽水音乐')).toBeTruthy();
  });

  it('DesktopLyricsApp 监听 mineradio-desktop-lyrics-state 更新歌词并支持锁定切换', async () => {
    const lockSpy = vi
      .spyOn(desktopWindow, 'setDesktopLyricsLockState')
      .mockResolvedValue({ ok: true, locked: true });
    const enabledSpy = vi
      .spyOn(desktopWindow, 'setDesktopLyricsEnabled')
      .mockResolvedValue({ ok: true, enabled: false });

    render(<DesktopLyricsApp />);

    await waitFor(() => {
      expect(screen.getByLabelText('Mineradio desktop lyrics')).toBeTruthy();
    });

    act(() => {
      emitLocalEvent('mineradio-desktop-lyrics-state', {
        line: '夜空中最亮的星',
        translation: 'The brightest star in the night sky',
        nextLine: '能否听清',
        playing: true,
        progress: 0.45,
        locked: false,
      });
    });

    expect(await screen.findByText('夜空中最亮的星')).toBeTruthy();
    expect(screen.getByTestId('desktop-lyrics-translation').textContent).toBe(
      'The brightest star in the night sky'
    );
    expect(screen.getByTestId('desktop-lyrics-next').textContent).toBe('能否听清');

    const lockBtn = screen.getByRole('button', { name: '锁定穿透' });
    fireEvent.click(lockBtn);
    await waitFor(() => {
      expect(lockSpy).toHaveBeenCalledWith(true);
    });

    const closeBtn = screen.getByRole('button', { name: '关闭桌面歌词' });
    fireEvent.click(closeBtn);
    await waitFor(() => {
      expect(enabledSpy).toHaveBeenCalledWith(false);
    });
  });
});
