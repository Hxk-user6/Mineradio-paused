import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  AudioEngine,
  buildCuefieldTimelineExecution,
  buildEqualPowerCurve,
  isExecutableCuefieldPlan,
  songIdentityKey,
} from './audioEngine';
import { GestureEngine } from './gestureEngine';
import { VisualEngine } from './visualEngine';
import {
  applyFxCssVariables,
  computeNextTrackIndex,
  computePrevTrackIndex,
  parseLrcText,
  resolveNextPlayMode,
  usePlayerStore,
} from '../store/playerStore';
import { DEFAULT_FX_SETTINGS, type CuefieldPlan, type SongRecord } from '../types/mineradio';

const SAMPLE_QUEUE: SongRecord[] = [
  { id: 101, name: 'Track One', artist: 'Artist A', provider: 'netease', duration: 210 },
  { id: 'qq-202', mid: 'qq-202', name: 'Track Two', artist: 'Artist B', provider: 'qq', duration: 195 },
  { id: 'loc-303', localFileId: 'loc-303', localUrl: 'mineradio-local://audio/loc-303', name: 'Local Three', artist: 'Artist C', provider: 'local', duration: 240 },
];

describe('Cuefield 时间轴归一化与执行规划', () => {
  it('buildEqualPowerCurve 生成平滑等功率淡入淡出曲线且边界精确', () => {
    const inCurve = buildEqualPowerCurve('in', 9);
    const outCurve = buildEqualPowerCurve('out', 9);

    expect(inCurve).toHaveLength(9);
    expect(outCurve).toHaveLength(9);
    expect(inCurve[0]).toBe(0);
    expect(inCurve[inCurve.length - 1]).toBe(1);
    expect(outCurve[0]).toBe(1);
    expect(outCurve[outCurve.length - 1]).toBe(0);

    // 等功率恒等式：sin^2(theta) + cos^2(theta) ≈ 1
    for (let i = 0; i < inCurve.length; i++) {
      const powerSum = inCurve[i] * inCurve[i] + outCurve[i] * outCurve[i];
      expect(powerSum).toBeCloseTo(1, 4);
    }
  });

  it('buildCuefieldTimelineExecution 能正确归一化时间轴、延迟毫秒、音量、滤波器与桥接动作', () => {
    const execution = buildCuefieldTimelineExecution({
      targetVolume: 0.8,
      mixStart: 180,
      handoffAt: 186,
      preRollDuration: 2.0,
      timeline: [
        { t: -6, deck: 'B', op: 'play', at: 10, volume: 0 },
        { t: -4, deck: 'B', op: 'rate', value: 1.02 },
        { t: -4, deck: 'B', op: 'filter', type: 'highpass', value: 420, duration: 2000 },
        { t: -4, deck: 'B', op: 'volume', curve: 'equal-power-in', value: 1, duration: 4000 },
        { t: -4, deck: 'A', op: 'volume', curve: 'equal-power-out', value: 0, duration: 4000 },
        { t: -3, deck: 'A', op: 'echo', bpm: 128, delayBeats: 0.5, feedback: 0.45, wet: 0.3 },
        { t: -2, deck: 'B', op: 'spectrum', low: 0.9, mid: 1.0, high: 0.8 },
        {
          t: -2,
          deck: 'B',
          op: 'bridge',
          duration: 70000,
          bridge: { template: 'invalid-tpl', bars: 7, bpmFrom: 120, bpmTo: 128 },
        },
        { t: 0, deck: 'B', op: 'handoff' },
      ],
    });

    expect(execution.leadSec).toBe(6);
    expect(execution.requiresAGraph).toBe(true);
    expect(execution.requiresBGraph).toBe(true);
    expect(execution.requiresBridge).toBe(true);
    expect(execution.handoffDelayMs).toBe(6000);

    const playAct = execution.actions.find((a) => a.op === 'play');
    expect(playAct?.delayMs).toBe(0);
    expect(playAct?.at).toBe(10);

    const volInAct = execution.actions.find((a) => a.deck === 'B' && a.op === 'volume');
    expect(volInAct?.target).toBeCloseTo(0.8, 3);

    const bridgeAct = execution.actions.find((a) => a.op === 'bridge');
    expect(bridgeAct?.durationMs).toBe(64000);
    expect(bridgeAct?.bridge?.template).toBe('drum-build');
    expect(bridgeAct?.bridge?.bars).toBe(8);
  });

  it('空时间轴时 buildCuefieldTimelineExecution 回退默认过渡时间轴', () => {
    const execution = buildCuefieldTimelineExecution({
      executionMode: 'intro-bed',
      entryTime: 16,
      targetVolume: 0.75,
    });
    expect(execution.leadSec).toBe(5.2);
    expect(execution.actions.length).toBeGreaterThanOrEqual(4);
    expect(execution.actions.some((a) => a.op === 'handoff')).toBe(true);
  });

  it('isExecutableCuefieldPlan 严格校验 tier、hard risk 与 end-of-track-crossfade', () => {
    const validPlan: CuefieldPlan = {
      ok: true,
      chosen: {
        evaluation: { tier: 'magic', score: 0.91, risks: [] },
        timeline: [{ t: -3, deck: 'B', op: 'play', at: 0 }],
      },
    };
    expect(isExecutableCuefieldPlan(validPlan)).toBe(true);

    const weakHardRiskPlan: CuefieldPlan = {
      ok: true,
      chosen: {
        evaluation: { tier: 'weak', score: 0.75, risks: ['closed outgoing phrase'] },
      },
    };
    expect(isExecutableCuefieldPlan(weakHardRiskPlan, { allowWeak: true })).toBe(false);

    const crossfadePlan: CuefieldPlan = {
      ok: true,
      chosen: {
        transitionRecipe: 'end-of-track-crossfade',
        mixStart: 190,
        handoffAt: 194,
        preRollDuration: 2,
        timeline: [
          { t: -2, deck: 'B', op: 'play', at: 0, volume: 0 },
          { t: 0, deck: 'A', op: 'volume', curve: 'equal-power-out', value: 0, duration: 4000 },
          { t: 0, deck: 'B', op: 'volume', curve: 'equal-power-in', value: 1, duration: 4000 },
          { t: 4, deck: 'B', op: 'handoff' },
        ],
      },
    };
    expect(isExecutableCuefieldPlan(crossfadePlan)).toBe(true);
  });
});

describe('播放模式切换与队列索引计算', () => {
  it('resolveNextPlayMode 按 loop -> single -> shuffle 循环切换', () => {
    expect(resolveNextPlayMode('loop')).toBe('single');
    expect(resolveNextPlayMode('single')).toBe('shuffle');
    expect(resolveNextPlayMode('shuffle')).toBe('loop');
  });

  it('computeNextTrackIndex / computePrevTrackIndex 正确处理 loop、single 与 shuffle', () => {
    expect(computeNextTrackIndex(3, 2, 'loop')).toBe(0);
    expect(computePrevTrackIndex(3, 0, 'loop')).toBe(2);

    expect(computeNextTrackIndex(3, 1, 'single')).toBe(1);
    expect(computePrevTrackIndex(3, 1, 'single')).toBe(1);

    // shuffle 即使随机数命中当前索引也应跳过到下一首
    expect(computeNextTrackIndex(3, 1, 'shuffle', () => 0.4)).toBe(2);
    expect(computePrevTrackIndex(3, 1, 'shuffle', () => 0.4)).toBe(0);

    expect(computeNextTrackIndex(0, -1, 'loop')).toBe(-1);
  });

  it('songIdentityKey 与 parseLrcText 输出规范键名与双语歌词', () => {
    expect(songIdentityKey(SAMPLE_QUEUE[0])).toBe('netease:101');
    expect(songIdentityKey(SAMPLE_QUEUE[1])).toBe('qq:qq-202');
    expect(songIdentityKey(SAMPLE_QUEUE[2])).toBe('local:loc-303');

    const lines = parseLrcText(
      '[00:05.50]First line\n[00:12.00]Second line',
      '[00:05.50]第一行\n[00:12.00]第二行'
    );
    expect(lines).toHaveLength(2);
    expect(lines[0]).toEqual({
      time: 5.5,
      text: 'First line',
      translation: '第一行',
      roma: undefined,
    });
  });
});

describe('playerStore 队列与 FX 状态控制', () => {
  beforeEach(() => {
    try {
      window.localStorage?.clear?.();
    } catch {
      // ignore Node localStorage experimental warning
    }
    if (typeof HTMLMediaElement !== 'undefined') {
      vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementation(async () => undefined);
      vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => undefined);
      vi.spyOn(HTMLMediaElement.prototype, 'load').mockImplementation(() => undefined);
    }
    usePlayerStore.setState({
      playQueue: [...SAMPLE_QUEUE],
      currentIdx: 0,
      currentSong: SAMPLE_QUEUE[0],
      playing: false,
      currentTime: 0,
      duration: 210,
      volume: 0.72,
      muted: false,
      playMode: 'loop',
      fx: { ...DEFAULT_FX_SETTINGS },
      activePreset: DEFAULT_FX_SETTINGS.preset,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('cyclePlayMode 与 setPlayMode 能同步更新状态', () => {
    const store = usePlayerStore.getState();
    expect(store.playMode).toBe('loop');
    store.cyclePlayMode();
    expect(usePlayerStore.getState().playMode).toBe('single');
    store.cyclePlayMode();
    expect(usePlayerStore.getState().playMode).toBe('shuffle');
    store.setPlayMode('loop');
    expect(usePlayerStore.getState().playMode).toBe('loop');
  });

  it('playSong / nextTrack / prevTrack 能正确驱动队列切换', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ url: 'https://cdn.example.com/a.mp3' }), { status: 200 }))
    );

    const store = usePlayerStore.getState();
    await store.nextTrack();
    expect(usePlayerStore.getState().currentIdx).toBe(1);
    expect(usePlayerStore.getState().currentSong?.name).toBe('Track Two');

    await usePlayerStore.getState().prevTrack();
    expect(usePlayerStore.getState().currentIdx).toBe(0);
    expect(usePlayerStore.getState().currentSong?.name).toBe('Track One');

    await usePlayerStore.getState().playSong(2);
    expect(usePlayerStore.getState().currentIdx).toBe(2);
    expect(usePlayerStore.getState().currentSong?.provider).toBe('local');
    expect(usePlayerStore.getState().playing).toBe(true);
    expect(usePlayerStore.getState().audioError).toBeNull();

    await usePlayerStore.getState().togglePlay();
    expect(usePlayerStore.getState().playing).toBe(false);
    await usePlayerStore.getState().togglePlay();
    expect(usePlayerStore.getState().playing).toBe(true);
  });

  it('updateFx、applyVisualPreset 与 resetFxDefaults 能正确维护 FX 参数', () => {
    const store = usePlayerStore.getState();
    store.updateFx({ intensity: 1.4, bloom: true });
    expect(usePlayerStore.getState().fx.intensity).toBe(1.4);
    expect(usePlayerStore.getState().fx.bloom).toBe(true);

    store.applyVisualPreset(6);
    expect(usePlayerStore.getState().activePreset).toBe(6);
    expect(usePlayerStore.getState().fx.preset).toBe(6);

    store.resetFxDefaults();
    expect(usePlayerStore.getState().fx.preset).toBe(DEFAULT_FX_SETTINGS.preset);
    expect(usePlayerStore.getState().fx.intensity).toBe(DEFAULT_FX_SETTINGS.intensity);
    expect(usePlayerStore.getState().fx.bloom).toBe(DEFAULT_FX_SETTINGS.bloom);
  });

  it('setVolume 与 toggleMute 能正确限制区间并同步静音状态', () => {
    const store = usePlayerStore.getState();
    store.setVolume(1.5);
    expect(usePlayerStore.getState().volume).toBe(1);
    expect(usePlayerStore.getState().muted).toBe(false);

    store.setVolume(0);
    expect(usePlayerStore.getState().volume).toBe(0);
    expect(usePlayerStore.getState().muted).toBe(true);

    store.toggleMute();
    expect(usePlayerStore.getState().muted).toBe(false);
  });
});

describe('新增 Engine 与 Store 契约补完验证', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('audioEngine.resolveSongPlaybackUrl 请求附带 name/artist/album/duration MatchHints 参数', async () => {
    const fetchMock = vi.fn(async () =>
      new Response(JSON.stringify({ url: 'https://cdn.example.com/hit.mp3' }), { status: 200 })
    );
    vi.stubGlobal('fetch', fetchMock);

    const engine = new AudioEngine();
    await engine.resolveSongPlaybackUrl(
      {
        id: '9001',
        name: '晴天',
        artist: '周杰伦',
        album: '叶惠美',
        duration: 269,
        provider: 'netease',
      },
      'lossless'
    );

    const [calledUrl] = fetchMock.mock.calls[0] as unknown as [string];
    expect(calledUrl).toContain('/api/song/url?id=9001&level=lossless');
    expect(calledUrl).toContain('name=%E6%99%B4%E5%A4%A9');
    expect(calledUrl).toContain('artist=%E5%91%A8%E6%9D%B0%E4%BC%A6');
    expect(calledUrl).toContain('album=%E5%8F%B6%E6%83%A0%E7%BE%8E');
    expect(calledUrl).toContain('duration=269');
    engine.dispose();
  });

  it('audioEngine.ensureBeatMap 命中远程 map 并触发 onBeatMapReady 回调', async () => {
    const sampleMap = {
      bpm: 120,
      gridStep: 0.5,
      duration: 180,
      beats: [0.5, 1.0, 1.5, 2.0],
    };
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        new Response(JSON.stringify({ ok: true, hit: true, map: sampleMap }), { status: 200 })
      )
    );

    const engine = new AudioEngine();
    const readySpy = vi.fn();
    engine.setCallbacks({ onBeatMapReady: readySpy });

    const res = await engine.ensureBeatMap({
      id: 777,
      name: 'Beat Track',
      provider: 'netease',
    });

    expect(res).toEqual(sampleMap);
    expect(readySpy).toHaveBeenCalledWith('netease:777', sampleMap);
    engine.dispose();
  });

  it('gestureEngine 在无摄像头媒体设备环境下安全报错退出而不崩溃', async () => {
    const ge = new GestureEngine();
    const onError = vi.fn();
    const onStatus = vi.fn();
    ge.setCallbacks({ onError, onStatusChange: onStatus });

    const ok = await ge.start();
    expect(ok).toBe(false);
    expect(ge.isActive()).toBe(false);
    expect(onError).toHaveBeenCalledWith('MEDIA_DEVICES_UNAVAILABLE');
    ge.stop();
  });

  it('visualEngine 支持自由镜头切换与 BeatMap 注入', () => {
    const ve = new VisualEngine();
    const camChanges: boolean[] = [];
    ve.setCallbacks({ onFreeCameraChange: (active) => camChanges.push(active) });
    expect(ve.isFreeCameraActive()).toBe(false);
    expect(ve.toggleFreeCamera()).toBe(true);
    expect(ve.isFreeCameraActive()).toBe(true);
    expect(ve.toggleFreeCamera(false)).toBe(false);
    expect(camChanges).toEqual([true, false]);

    ve.setBeatMap({ bpm: 128, gridStep: 0.46875, duration: 180, beats: [0.5, 1.0] });
    ve.dispose();
  });

  it('playerStore 多平台歌单合并、发现页/天气电台归一化与歌单架滤波正确工作', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/discover/home')) {
          return new Response(
            JSON.stringify({
              loggedIn: true,
              dailySongs: [{ id: 'd1', name: 'Daily Song', provider: 'netease' }],
              playlists: [{ id: 'p1', name: 'Daily PL', trackCount: 10, provider: 'netease' }],
              podcasts: [],
            }),
            { status: 200 }
          );
        }
        if (url.includes('/api/weather/radio')) {
          return new Response(
            JSON.stringify({
              ok: true,
              weather: { city: '上海', temperature: 22, weatherCode: 0, summary: '晴朗' },
              radio: {
                title: '晴朗电台',
                subtitle: '微风正好',
                songs: [{ id: 'w1', name: 'Weather Song', provider: 'netease' }],
              },
            }),
            { status: 200 }
          );
        }
        if (url.includes('/api/user/playlists')) {
          return new Response(
            JSON.stringify({
              playlists: [
                { id: '1', name: '我的歌单', trackCount: 5, provider: 'netease', subscribed: false },
                { id: '2', name: '收藏歌单', trackCount: 8, provider: 'netease', subscribed: true },
              ],
            }),
            { status: 200 }
          );
        }
        if (url.includes('/api/qq/user/playlists')) {
          return new Response(
            JSON.stringify({
              playlists: [{ id: 'q1', name: 'QQ歌单', trackCount: 3, provider: 'qq' }],
            }),
            { status: 200 }
          );
        }
        return new Response(JSON.stringify({ playlists: [] }), { status: 200 });
      })
    );

    const store = usePlayerStore.getState();
    const discover = await store.loadHomeDiscover();
    expect(discover?.songs?.[0]?.name).toBe('Daily Song');

    const weather = await store.loadWeatherRadio();
    expect(weather?.city).toBe('上海');
    expect(weather?.tracks?.[0]?.name).toBe('Weather Song');
    expect(`${weather?.weather}`).toBe('晴朗');

    const playlists = await store.refreshAllUserPlaylists();
    expect(playlists).toHaveLength(3);

    store.updateFx({ shelfMergeCollections: false });
    expect(store.getShelfPlaylists().some((p) => p.subscribed)).toBe(false);

    store.updateFx({ shelfMergeCollections: true });
    expect(store.getShelfPlaylists().some((p) => p.subscribed)).toBe(true);

    applyFxCssVariables({ ...DEFAULT_FX_SETTINGS, homeAccentColor: '#112233', playlistPanelGlassBlur: 24 });
    expect(document.documentElement.style.getPropertyValue('--home-accent')).toBe('#112233');
    expect(document.documentElement.style.getPropertyValue('--playlist-panel-glass-blur')).toBe('24px');
  });
});
