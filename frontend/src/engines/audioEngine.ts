import { apiJson, audioProxyUrl, resolveMediaUrl } from '../bridge/tauriBridge';
import type {
  AudioReactiveMetrics,
  BeatMapData,
  CuefieldPlan,
  CuefieldStatus,
  CuefieldTimelineAction,
  CuefieldTimelineExecution,
  NormalizedCuefieldAction,
  QualityPreference,
  SongRecord,
} from '../types/mineradio';

declare global {
  interface Window {
    MusicTempo?: new (
      audioData: Float32Array | number[],
      options?: Record<string, unknown>
    ) => { tempo: number; beats: number[] };
    webkitAudioContext?: typeof AudioContext;
    webkitOfflineAudioContext?: typeof OfflineAudioContext;
  }
}

export type DeckId = 'A' | 'B';

interface DeckGraph {
  id: DeckId;
  audio: HTMLAudioElement;
  source: MediaElementAudioSourceNode | MediaStreamAudioSourceNode | null;
  mediaSourceBound: boolean;
  usesCapture: boolean;
  filterNode: BiquadFilterNode | null;
  bassNode: BiquadFilterNode | null;
  midNode: BiquadFilterNode | null;
  highNode: BiquadFilterNode | null;
  analyser: AnalyserNode | null;
  beatAnalyser: AnalyserNode | null;
  gainNode: GainNode | null;
  echoSendNode: GainNode | null;
  echoDelayNode: DelayNode | null;
  echoFeedbackNode: GainNode | null;
  echoWetNode: GainNode | null;
}

export interface CuefieldPendingTransition {
  token: number;
  currentIndex: number;
  nextIndex: number;
  fromKey: string;
  toKey: string;
  plan: CuefieldPlan;
  timeline: CuefieldTimelineAction[];
  execution: CuefieldTimelineExecution;
  audioUrl: string;
  executionMode: string;
  entryTime: number;
  exitTime: number;
  protectedUntil: number;
  minimumListenUntil: number;
  triggerAt: number;
  mixStart?: number;
  handoffAt?: number;
  nextSong: SongRecord;
}

export interface AudioEngineCallbacks {
  onTimeUpdate?: (currentTime: number, duration: number) => void;
  onPlayStateChange?: (playing: boolean) => void;
  onEnded?: () => void;
  onError?: (message: string) => void;
  onCuefieldStatus?: (status: CuefieldStatus) => void;
  onCuefieldHandoff?: (nextSong: SongRecord, nextIndex: number) => void;
  getNextTrackContext?: () => {
    currentSong: SongRecord | null;
    currentIndex: number;
    nextSong: SongRecord | null;
    nextIndex: number;
    quality: QualityPreference;
    fromLrc?: string;
    toLrc?: string;
  } | null;
}

const FFT_SIZE = 2048;
const BEAT_FFT_SIZE = 1024;
const EXECUTABLE_TIERS = new Set(['magic', 'usable', 'usable_but_not_magic']);
const HARD_RISKS = new Set(['closed outgoing phrase', 'near closed outgoing phrase']);

function toNum(value: unknown, fallback: number): number {
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
}

function clamp(value: unknown, min: number, max: number): number {
  const n = toNum(value, min);
  return Math.max(min, Math.min(max, n));
}

function round(value: unknown, digits = 3): number {
  const factor = Math.pow(10, digits);
  return Math.round(toNum(value, 0) * factor) / factor;
}

function finiteOpt(value: unknown): number | null {
  if (value == null) return null;
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

export function songIdentityKey(song: SongRecord | null | undefined): string {
  if (!song) return '';
  const provider = String(song.provider || song.source || 'netease').toLowerCase();
  if (provider === 'local') {
    return `local:${song.localFileId || song.id || song.name || ''}`;
  }
  if (provider === 'qq') {
    return `qq:${song.mid || song.songmid || song.id || ''}`;
  }
  if (provider === 'kugou') {
    const hash = String(song.hash || song.fileHash || song.audioHash || song.id || '').toLowerCase();
    return `kugou:${hash}`;
  }
  if (provider === 'qishui') {
    return `qishui:${song.id || song.providerSongId || ''}`;
  }
  return `${provider}:${song.id || song.mid || song.hash || song.name || ''}`;
}

export function buildEqualPowerCurve(direction: 'in' | 'out', points = 33): number[] {
  const count = Math.max(2, Math.round(toNum(points, 33)));
  const incoming = direction !== 'out';
  const values: number[] = [];
  for (let i = 0; i < count; i++) {
    const progress = i / (count - 1);
    const val = incoming
      ? Math.sin((progress * Math.PI) / 2)
      : Math.cos((progress * Math.PI) / 2);
    values.push(round(val, 6));
  }
  values[0] = incoming ? 0 : 1;
  values[count - 1] = incoming ? 1 : 0;
  return values;
}

export function executionModeForPlan(plan: CuefieldPlan | null | undefined): string {
  const chosen = plan?.chosen;
  const recipe = chosen?.transitionRecipe || chosen?.recipeCandidate?.recipe || '';
  if (recipe) return recipe;
  return chosen?.evaluation?.tier === 'weak' ? 'intro-bed' : 'filtered-pickup';
}

export function isExecutableCuefieldPlan(
  plan: CuefieldPlan | null | undefined,
  opts?: { allowWeak?: boolean; allowSafetyFallback?: boolean; allowLiveEndCrossfadeFallback?: boolean; minWeakScore?: number }
): boolean {
  if (!plan || !plan.ok || !plan.chosen) return false;
  const deps = {
    allowWeak: opts?.allowWeak ?? false,
    allowSafetyFallback: opts?.allowSafetyFallback ?? true,
    allowLiveEndCrossfadeFallback: opts?.allowLiveEndCrossfadeFallback ?? true,
    minWeakScore: opts?.minWeakScore ?? 0.58,
  };
  const chosen = plan.chosen;
  if (chosen.technicalFailure === true) return false;
  const tier = chosen.evaluation?.tier || '';
  const recipe = executionModeForPlan(plan);
  const timeline = Array.isArray(chosen.timeline) ? chosen.timeline : [];

  if (recipe === 'honest-start-fallback') {
    return deps.allowSafetyFallback && timeline.length > 0;
  }

  if (recipe === 'terminal-rescue') {
    if (!deps.allowSafetyFallback) return false;
    const tol = 0.01;
    const mixStart = chosen.mixStart;
    const handoffAt = chosen.handoffAt;
    if (!Number.isFinite(mixStart) || !Number.isFinite(handoffAt) || (handoffAt as number) <= (mixStart as number)) {
      return false;
    }
    const span = (handoffAt as number) - (mixStart as number);
    const handoffs = timeline.filter((a) => a && a.op === 'handoff' && Number.isFinite(a.t));
    if (handoffs.length !== 1 || Math.abs(handoffs[0].t - span) > tol) return false;
    const handoffT = handoffs[0].t;
    const aligned = timeline.every((a) => {
      if (!a || !Number.isFinite(a.t) || a.t < 0 || a.t > handoffT) return false;
      const dur = a.duration == null ? 0 : a.duration;
      return Number.isFinite(dur) && dur >= 0 && a.t + dur / 1000 <= span + tol;
    });
    if (!aligned) return false;
    const hasBPlay = timeline.some(
      (a) => a && a.deck === 'B' && a.op === 'play' && Math.abs(a.t) <= tol && Number.isFinite(a.at)
    );
    const rampFor = (deck: 'A' | 'B') =>
      timeline.find(
        (a) =>
          a &&
          a.deck === deck &&
          a.op === 'volume' &&
          Number.isFinite(a.value) &&
          Number.isFinite(a.duration) &&
          (a.duration as number) > 0
      );
    const aRamp = rampFor('A');
    const bRamp = rampFor('B');
    return (
      hasBPlay &&
      Boolean(aRamp) &&
      Boolean(bRamp) &&
      (aRamp!.t >= 0) &&
      (bRamp!.t >= 0) &&
      aRamp!.t + (aRamp!.duration as number) / 1000 <= handoffT + tol
    );
  }

  if (recipe === 'end-of-track-crossfade') {
    if (!deps.allowSafetyFallback || !deps.allowLiveEndCrossfadeFallback) return false;
    const tol = 0.01;
    const mixStart = chosen.mixStart;
    const handoffAt = chosen.handoffAt;
    const preRoll = chosen.preRollDuration;
    if (!Number.isFinite(mixStart) || !Number.isFinite(handoffAt) || (handoffAt as number) <= (mixStart as number)) {
      return false;
    }
    if (!Number.isFinite(preRoll) || (preRoll as number) < 0 || (preRoll as number) > 5) return false;
    const span = (handoffAt as number) - (mixStart as number);
    if (timeline.length !== 4) return false;
    const plays = timeline.filter((a) => a && a.deck === 'B' && a.op === 'play');
    const handoffs = timeline.filter((a) => a && a.deck === 'B' && a.op === 'handoff');
    const aRamps = timeline.filter((a) => a && a.deck === 'A' && a.op === 'volume');
    const bRamps = timeline.filter((a) => a && a.deck === 'B' && a.op === 'volume');
    if (plays.length !== 1 || handoffs.length !== 1 || aRamps.length !== 1 || bRamps.length !== 1) return false;
    const [play] = plays;
    const [handoff] = handoffs;
    const [aRamp] = aRamps;
    const [bRamp] = bRamps;
    if (!Number.isFinite(play.t) || !Number.isFinite(play.at) || Math.abs(play.at as number) > tol) return false;
    if (Math.abs(play.t + (preRoll as number)) > tol || Number(play.volume) !== 0) return false;
    if (!Number.isFinite(handoff.t) || Math.abs(handoff.t - span) > tol) return false;
    if (aRamp.curve !== 'equal-power-out' || bRamp.curve !== 'equal-power-in') return false;
    if (Number(aRamp.value) !== 0 || Number(bRamp.value) !== 1) return false;
    for (const ramp of [aRamp, bRamp]) {
      if (!Number.isFinite(ramp.t) || ramp.t < 0 || !Number.isFinite(ramp.duration) || (ramp.duration as number) <= 0) {
        return false;
      }
      if (ramp.t + (ramp.duration as number) / 1000 > span + tol) return false;
    }
    return timeline.every((a) => a && Number.isFinite(a.t) && (a === play || a.t >= 0));
  }

  if (EXECUTABLE_TIERS.has(tier)) return true;
  if (recipe === 'safety-long-blend') return deps.allowSafetyFallback;
  if (tier !== 'weak' || !deps.allowWeak) return false;
  const risks = chosen.evaluation?.risks || [];
  if (risks.some((r) => HARD_RISKS.has(r))) return false;
  const score = Number.isFinite(Number(chosen.evaluation?.score))
    ? Number(chosen.evaluation?.score)
    : toNum(chosen.score, 0);
  return score >= deps.minWeakScore;
}

function normalizeCuefieldAction(
  action: CuefieldTimelineAction,
  offsetSec: number,
  targetVolume: number,
  originT: number | null,
  playbackRate: number,
  sourceZeroPreRoll: boolean
): NormalizedCuefieldAction {
  const actionTime = toNum(action?.t, 0);
  const value = clamp(action?.value == null ? 1 : action.value, 0, 1);
  const normalized: NormalizedCuefieldAction = {
    t: round(actionTime),
    delayMs: Math.max(0, Math.round((actionTime + offsetSec) * 1000)),
    durationMs: Math.max(0, Math.round(toNum(action?.duration, 0))),
    deck: action?.deck === 'A' ? 'A' : 'B',
    op: String(action?.op || ''),
    type: String(action?.type || ''),
    curve: String(action?.curve || ''),
    value,
    at: Math.max(0, toNum(action?.at, 0)),
    optionalWhenLate: action?.optionalWhenLate === true,
    maxLateMs: Math.round(clamp(action?.maxLateMs, 0, 200)),
    sourceZeroPreRoll: sourceZeroPreRoll === true,
  };

  if (
    normalized.deck === 'B' &&
    normalized.op === 'play' &&
    originT != null &&
    actionTime < originT &&
    !sourceZeroPreRoll
  ) {
    normalized.at = round(normalized.at + (originT - actionTime) * clamp(playbackRate, 0.94, 1.06));
  }
  if (normalized.op === 'volume') {
    normalized.target = round(targetVolume * value);
  }
  if (normalized.op === 'filter') {
    normalized.value =
      normalized.type === 'none' ? 20 : round(clamp(toNum(action?.value, 650), 20, 6000));
  }
  if (normalized.op === 'echo') {
    normalized.enabled = action?.enabled !== false;
    normalized.bpm = round(clamp(toNum(action?.bpm, 120), 40, 240));
    normalized.delayBeats = round(clamp(toNum(action?.delayBeats, 0.5), 0.125, 2));
    normalized.feedback = round(clamp(toNum(action?.feedback, 0), 0, 0.72));
    normalized.wet = round(clamp(toNum(action?.wet, 0), 0, 0.5));
    normalized.tailMs = Math.round(clamp(toNum(action?.tailMs, 1200), 300, 4000));
  }
  if (normalized.op === 'duck') {
    normalized.bpm = round(clamp(toNum(action?.bpm, 120), 40, 240));
    normalized.depth = round(clamp(toNum(action?.depth, 0.35), 0.08, 0.75));
    normalized.pulses = Math.round(clamp(toNum(action?.pulses, 4), 1, 16));
    normalized.beats = round(clamp(toNum(action?.beats, 1), 0.25, 4));
    normalized.attack = Math.round(clamp(toNum(action?.attack, 24), 5, 120));
    normalized.hold = Math.round(clamp(toNum(action?.hold, 70), 10, 180));
    normalized.release = Math.round(clamp(toNum(action?.release, 180), 40, 320));
  }
  if (normalized.op === 'spectrum') {
    normalized.low = round(clamp(toNum(action?.low, 1), 0, 1));
    normalized.mid = round(clamp(toNum(action?.mid, 1), 0, 1));
    normalized.high = round(clamp(toNum(action?.high, 1), 0, 1));
  }
  if (normalized.op === 'rate') {
    normalized.value = round(clamp(toNum(action?.value, 1), 0.94, 1.06));
  }
  if (normalized.op === 'loop') {
    normalized.enabled = action?.enabled !== false;
    normalized.startAt = Math.max(0, round(toNum(action?.startAt, normalized.at)));
    normalized.bpm = round(clamp(toNum(action?.bpm, 120), 40, 240));
    normalized.loopBeats = round(clamp(toNum(action?.loopBeats, 1), 0.5, 8));
    normalized.loopSeconds = round((60 / normalized.bpm) * normalized.loopBeats);
    normalized.slip = action?.slip !== false;
  }
  if (normalized.op === 'bridge') {
    const bridge = action?.bridge || ({} as Record<string, unknown>);
    const rawTpl = String(bridge.template || '');
    const template = ['drum-build', 'echo-break', 'loop-rise', 'impact-drop'].includes(rawTpl)
      ? rawTpl
      : 'drum-build';
    const reqBars = toNum(bridge.bars, 4);
    const bars = reqBars >= 12 ? 16 : reqBars >= 6 ? 8 : 4;
    normalized.durationMs = Math.min(64000, normalized.durationMs);
    normalized.bridge = {
      template,
      bars,
      bpmFrom: round(clamp(toNum(bridge.bpmFrom, 120), 40, 240)),
      bpmTo: round(clamp(toNum(bridge.bpmTo, 120), 40, 240)),
      stageDurations: Array.isArray(bridge.stageDurations)
        ? bridge.stageDurations.slice(0, 3).map((v) => round(Math.max(0, toNum(v, 0))))
        : [],
    };
    normalized.fallbackTimeline = Array.isArray(action?.fallbackTimeline)
      ? action.fallbackTimeline.slice()
      : [];
  }
  return normalized;
}

function leadFromTimeline(timeline: CuefieldTimelineAction[], fallback: number): number {
  let lead = 0;
  for (const item of timeline) {
    const t = toNum(item?.t, 0);
    if (t < 0) lead = Math.max(lead, Math.abs(t));
  }
  return lead > 0 ? round(lead) : fallback;
}

function bStartFromTimeline(timeline: CuefieldTimelineAction[], fallback: number): number {
  for (const item of timeline) {
    if (item?.deck === 'B' && item?.op === 'play') {
      return Math.max(0, toNum(item.at, fallback));
    }
  }
  return fallback;
}

function defaultFallbackTimeline(opts: {
  executionMode?: string;
  entryTime?: number;
}): { leadSec: number; bStart: number; actions: CuefieldTimelineAction[] } {
  const mode = opts.executionMode || 'filtered-pickup';
  const entryTime = Math.max(0, toNum(opts.entryTime, 0));
  if (mode === 'intro-bed') {
    const introLead = 5.2;
    return {
      leadSec: introLead,
      bStart: Math.max(0, entryTime - Math.min(5.2, Math.max(2.2, entryTime * 0.7))),
      actions: [
        { t: -introLead, deck: 'B', op: 'play', at: Math.max(0, entryTime - introLead), volume: 0 },
        { t: -introLead, deck: 'B', op: 'volume', value: 0.32, duration: 1700 },
        { t: -3.5, deck: 'A', op: 'volume', value: 0, duration: 2700 },
        { t: -3.5, deck: 'B', op: 'volume', value: 1, duration: 2700 },
        { t: -1.06, deck: 'B', op: 'handoff' },
      ],
    };
  }
  return {
    leadSec: 2.8,
    bStart: Math.max(0, entryTime - Math.min(2.4, Math.max(0.8, entryTime * 0.45))),
    actions: [
      { t: -2.8, deck: 'B', op: 'play', at: Math.max(0, entryTime - 2.8), volume: 0 },
      { t: -2.8, deck: 'B', op: 'volume', value: 1, duration: 2600 },
      { t: -2.8, deck: 'A', op: 'volume', value: 0, duration: 2600 },
      { t: -0.46, deck: 'B', op: 'handoff' },
    ],
  };
}

export function buildCuefieldTimelineExecution(opts: {
  timeline?: CuefieldTimelineAction[];
  entryTime?: number;
  executionMode?: string;
  targetVolume?: number;
  mixStart?: number;
  handoffAt?: number;
  audibleOverlap?: number;
  preRollDuration?: number;
}): CuefieldTimelineExecution {
  const rawTimeline = Array.isArray(opts?.timeline) ? opts.timeline.slice() : [];
  const targetVolume = clamp(opts?.targetVolume == null ? 1 : opts.targetVolume, 0, 1);
  const fallback = rawTimeline.length ? null : defaultFallbackTimeline(opts || {});
  const timeline = rawTimeline.length ? rawTimeline : fallback!.actions;
  const leadSec = rawTimeline.length ? leadFromTimeline(timeline, 2.8) : fallback!.leadSec;
  const mixStart = finiteOpt(opts?.mixStart);
  const handoffAt = finiteOpt(opts?.handoffAt);
  const rawHandoff = timeline.filter((a) => a && a.op === 'handoff').slice(-1)[0];
  const explicitWindow =
    mixStart != null && handoffAt != null && handoffAt > mixStart && Boolean(rawHandoff);
  const originT = explicitWindow
    ? toNum(rawHandoff.t, 0) - Math.max(0, (handoffAt as number) - (mixStart as number))
    : null;
  const requestedPreRoll = Math.max(0, toNum(opts?.preRollDuration, 0));
  const rawPlay = timeline.find((a) => a && a.deck === 'B' && a.op === 'play') || null;
  const sourceZeroPreRoll = Boolean(
    explicitWindow &&
      rawPlay &&
      originT != null &&
      toNum(rawPlay.t, 0) < originT &&
      Math.abs(toNum(rawPlay.at, 0)) <= 0.001 &&
      requestedPreRoll > 0 &&
      Math.abs(Math.abs(toNum(rawPlay.t, 0)) - requestedPreRoll) <= 0.01
  );
  const executionStartT =
    sourceZeroPreRoll && rawPlay ? toNum(rawPlay.t, originT ?? 0) : originT;
  const offsetSec = explicitWindow && executionStartT != null ? -executionStartT : leadSec;
  const entryTime = Math.max(0, toNum(opts?.entryTime, 0));

  const rateActions = timeline
    .filter((a) => a && a.deck === 'B' && a.op === 'rate')
    .sort((a, b) => toNum(a.t, 0) - toNum(b.t, 0));

  const rateAt = (time: number): number => {
    let val = 1;
    for (const item of rateActions) {
      if (toNum(item.t, 0) > time) break;
      val = clamp(toNum(item.value, 1), 0.94, 1.06);
    }
    return val;
  };

  const actions = timeline
    .map((action) =>
      normalizeCuefieldAction(
        action,
        offsetSec,
        targetVolume,
        originT,
        rateAt(toNum(action?.t, 0)),
        sourceZeroPreRoll && action === rawPlay
      )
    )
    .filter((a) => Boolean(a.op))
    .sort((a, b) => a.delayMs - b.delayMs || a.t - b.t);

  const play = actions.find((a) => a.deck === 'B' && a.op === 'play');
  const bStart = play
    ? play.at
    : rawTimeline.length
    ? bStartFromTimeline(timeline, entryTime)
    : fallback!.bStart;

  const requiresBGraph = actions.some(
    (a) =>
      a.deck === 'B' &&
      (a.op === 'filter' ||
        a.op === 'bass' ||
        a.op === 'spectrum' ||
        a.op === 'echo' ||
        a.op === 'duck' ||
        (a.op === 'volume' && a.curve.startsWith('equal-power-')))
  );
  const requiresAGraph = actions.some(
    (a) => a.deck === 'A' && (a.op === 'echo' || a.op === 'duck')
  );
  const requiresBridge = actions.some((a) => a.op === 'bridge');
  const requiresSourceLoop = actions.some((a) => a.op === 'loop');
  const handoff = actions.filter((a) => a.op === 'handoff').slice(-1)[0];
  const lastAction = actions[actions.length - 1] || null;
  const handoffDelayMs = handoff
    ? handoff.delayMs
    : lastAction
    ? lastAction.delayMs + Math.max(520, lastAction.durationMs)
    : Math.round(leadSec * 1000);

  return {
    leadSec: round(leadSec),
    bStart: round(bStart),
    handoffDelayMs: Math.max(explicitWindow ? 0 : 520, handoffDelayMs),
    audibleStartDelayMs:
      explicitWindow && originT != null && executionStartT != null
        ? Math.max(0, Math.round((originT - executionStartT) * 1000))
        : null,
    audibleOverlap: finiteOpt(opts?.audibleOverlap),
    preRollDuration: finiteOpt(opts?.preRollDuration),
    requiresAGraph,
    requiresBGraph,
    requiresBridge,
    requiresSourceLoop,
    actions,
  };
}

function createEmptyAudioElement(): HTMLAudioElement {
  if (typeof Audio === 'undefined') {
    return {} as HTMLAudioElement;
  }
  const el = new Audio();
  el.crossOrigin = 'anonymous';
  el.preload = 'auto';
  return el;
}

export class AudioEngine {
  private ctx: AudioContext | null = null;
  private decks: Record<DeckId, DeckGraph>;
  private activeDeckId: DeckId = 'A';
  private volume = 0.72;
  private muted = false;
  private playing = false;
  private callbacks: AudioEngineCallbacks = {};

  // 零堆分配实时频域缓存
  private freqData = new Uint8Array(FFT_SIZE / 2);
  private beatFreqData = new Uint8Array(BEAT_FFT_SIZE / 2);
  private prevEnergy = 0;
  private beatPulse = 0;
  private lastBeatAt = 0;

  // 本地节拍分析缓存
  private beatMapCache = new Map<string, BeatMapData>();
  private beatAnalysisInFlight = new Map<string, Promise<BeatMapData | null>>();

  // Cuefield AutoMix 状态机
  private automixEnabled = false;
  private cuefieldStatus: CuefieldStatus = 'disabled';
  private pendingTransition: CuefieldPendingTransition | null = null;
  private cuefieldSerial = 0;
  private prepareTimer: ReturnType<typeof setTimeout> | null = null;
  private timelineTimers: Array<ReturnType<typeof setTimeout>> = [];
  private executingTransition = false;
  private trackToken = 0;

  constructor() {
    this.decks = {
      A: this.createDeck('A'),
      B: this.createDeck('B'),
    };
    this.bindDeckEvents('A');
    this.bindDeckEvents('B');
  }

  private createDeck(id: DeckId): DeckGraph {
    return {
      id,
      audio: createEmptyAudioElement(),
      source: null,
      mediaSourceBound: false,
      usesCapture: false,
      filterNode: null,
      bassNode: null,
      midNode: null,
      highNode: null,
      analyser: null,
      beatAnalyser: null,
      gainNode: null,
      echoSendNode: null,
      echoDelayNode: null,
      echoFeedbackNode: null,
      echoWetNode: null,
    };
  }

  private bindDeckEvents(id: DeckId): void {
    const deck = this.decks[id];
    const audio = deck.audio;
    if (!audio || typeof audio.addEventListener !== 'function') return;

    audio.addEventListener('timeupdate', () => {
      if (this.activeDeckId !== id) return;
      const current = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
      const dur = Number.isFinite(audio.duration) ? audio.duration : 0;
      this.callbacks.onTimeUpdate?.(current, dur);
      this.checkCuefieldWindow(current, dur);
    });

    audio.addEventListener('loadedmetadata', () => {
      if (this.activeDeckId !== id) return;
      const current = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
      const dur = Number.isFinite(audio.duration) ? audio.duration : 0;
      this.callbacks.onTimeUpdate?.(current, dur);
    });

    audio.addEventListener('play', () => {
      if (this.activeDeckId !== id) return;
      this.playing = true;
      this.callbacks.onPlayStateChange?.(true);
    });

    audio.addEventListener('pause', () => {
      if (this.activeDeckId !== id) return;
      this.playing = false;
      this.callbacks.onPlayStateChange?.(false);
    });

    audio.addEventListener('ended', () => {
      if (this.activeDeckId !== id) return;
      if (this.executingTransition) return;
      this.playing = false;
      this.callbacks.onPlayStateChange?.(false);
      this.callbacks.onEnded?.();
    });

    audio.addEventListener('error', () => {
      if (this.activeDeckId !== id) return;
      const code = audio.error?.code;
      this.callbacks.onError?.(`音频加载失败 (code=${code ?? 'unknown'})`);
    });
  }

  public setCallbacks(callbacks: AudioEngineCallbacks): void {
    this.callbacks = { ...this.callbacks, ...callbacks };
  }

  private ensureAudioContext(): AudioContext | null {
    if (typeof window === 'undefined') return null;
    const Ctor = window.AudioContext || window.webkitAudioContext;
    if (!Ctor) return null;
    if (!this.ctx || this.ctx.state === 'closed') {
      this.ctx = new Ctor();
    }
    if (this.ctx.state === 'suspended') {
      this.ctx.resume().catch(() => {});
    }
    return this.ctx;
  }

  private ensureDeckGraph(id: DeckId): boolean {
    const deck = this.decks[id];
    const ctx = this.ensureAudioContext();
    if (!ctx || !deck.audio || typeof ctx.createAnalyser !== 'function') return false;
    if (deck.source && deck.analyser && deck.gainNode && deck.filterNode) return true;

    try {
      if (!deck.source) {
        if (!deck.mediaSourceBound && typeof ctx.createMediaElementSource === 'function') {
          deck.source = ctx.createMediaElementSource(deck.audio);
          deck.mediaSourceBound = true;
          deck.usesCapture = false;
        } else if (
          typeof (deck.audio as HTMLAudioElement & { captureStream?: () => MediaStream })
            .captureStream === 'function' &&
          typeof ctx.createMediaStreamSource === 'function'
        ) {
          const stream = (
            deck.audio as HTMLAudioElement & { captureStream: () => MediaStream }
          ).captureStream();
          deck.source = ctx.createMediaStreamSource(stream);
          deck.usesCapture = true;
        }
      }
      if (!deck.source) return false;

      deck.filterNode = ctx.createBiquadFilter();
      deck.filterNode.type = 'highpass';
      deck.filterNode.frequency.value = 20;

      deck.bassNode = ctx.createBiquadFilter();
      deck.bassNode.type = 'lowshelf';
      deck.bassNode.frequency.value = 180;
      deck.bassNode.gain.value = 0;

      deck.midNode = ctx.createBiquadFilter();
      deck.midNode.type = 'peaking';
      deck.midNode.frequency.value = 1200;
      deck.midNode.Q.value = 0.72;
      deck.midNode.gain.value = 0;

      deck.highNode = ctx.createBiquadFilter();
      deck.highNode.type = 'highshelf';
      deck.highNode.frequency.value = 5200;
      deck.highNode.gain.value = 0;

      deck.analyser = ctx.createAnalyser();
      deck.analyser.fftSize = FFT_SIZE;
      deck.analyser.smoothingTimeConstant = 0.58;

      deck.beatAnalyser = ctx.createAnalyser();
      deck.beatAnalyser.fftSize = BEAT_FFT_SIZE;
      deck.beatAnalyser.smoothingTimeConstant = 0.1;

      deck.gainNode = ctx.createGain();
      const targetGain = id === this.activeDeckId ? this.effectiveVolume() : 0;
      deck.gainNode.gain.value = targetGain;

      if (typeof ctx.createDelay === 'function') {
        deck.echoSendNode = ctx.createGain();
        deck.echoDelayNode = ctx.createDelay(2);
        deck.echoFeedbackNode = ctx.createGain();
        deck.echoWetNode = ctx.createGain();
        deck.echoSendNode.gain.value = 0;
        deck.echoFeedbackNode.gain.value = 0;
        deck.echoWetNode.gain.value = 0;
      }

      deck.source.connect(deck.filterNode);
      deck.filterNode.connect(deck.bassNode);
      deck.bassNode.connect(deck.midNode);
      deck.midNode.connect(deck.highNode);
      deck.highNode.connect(deck.analyser);
      deck.source.connect(deck.beatAnalyser);
      deck.analyser.connect(deck.gainNode);
      deck.gainNode.connect(ctx.destination);

      if (
        deck.echoSendNode &&
        deck.echoDelayNode &&
        deck.echoFeedbackNode &&
        deck.echoWetNode
      ) {
        deck.highNode.connect(deck.echoSendNode);
        deck.echoSendNode.connect(deck.echoDelayNode);
        deck.echoDelayNode.connect(deck.echoFeedbackNode);
        deck.echoFeedbackNode.connect(deck.echoDelayNode);
        deck.echoDelayNode.connect(deck.echoWetNode);
        deck.echoWetNode.connect(ctx.destination);
      }

      return true;
    } catch {
      return false;
    }
  }

  private get activeDeck(): DeckGraph {
    return this.decks[this.activeDeckId];
  }

  private get standbyDeck(): DeckGraph {
    return this.decks[this.activeDeckId === 'A' ? 'B' : 'A'];
  }

  private effectiveVolume(): number {
    return this.muted ? 0 : clamp(this.volume, 0, 1);
  }

  private applyDeckGain(deck: DeckGraph, gainValue: number): void {
    const val = clamp(gainValue, 0, 1);
    if (deck.gainNode) {
      try {
        deck.gainNode.gain.setValueAtTime(val, this.ctx?.currentTime || 0);
      } catch {
        deck.gainNode.gain.value = val;
      }
      deck.audio.volume = 1;
      deck.audio.muted = false;
    } else {
      deck.audio.volume = val;
      deck.audio.muted = val <= 0.0001;
    }
  }

  private resetDeckProcessing(deck: DeckGraph): void {
    const now = this.ctx?.currentTime || 0;
    if (deck.filterNode) {
      deck.filterNode.type = 'highpass';
      deck.filterNode.frequency.setValueAtTime(20, now);
    }
    if (deck.bassNode) deck.bassNode.gain.setValueAtTime(0, now);
    if (deck.midNode) deck.midNode.gain.setValueAtTime(0, now);
    if (deck.highNode) deck.highNode.gain.setValueAtTime(0, now);
    if (deck.echoSendNode) deck.echoSendNode.gain.setValueAtTime(0, now);
    if (deck.echoFeedbackNode) deck.echoFeedbackNode.gain.setValueAtTime(0, now);
    if (deck.echoWetNode) deck.echoWetNode.gain.setValueAtTime(0, now);
    deck.audio.playbackRate = 1;
  }

  public async resolveSongPlaybackUrl(
    song: SongRecord,
    quality: QualityPreference = 'exhigh'
  ): Promise<string> {
    if (!song) return '';
    const provider = String(song.provider || song.source || 'netease').toLowerCase();
    if (provider === 'local') {
      if (song.localUrl) return resolveMediaUrl(String(song.localUrl), { preferHttpFallback: true });
      if (song.localFileId) {
        return resolveMediaUrl(`mineradio-local://audio/${song.localFileId}`, {
          preferHttpFallback: true,
        });
      }
      return '';
    }

    let endpoint = '';
    if (provider === 'qq') {
      const mid = encodeURIComponent(String(song.mid || song.songmid || song.id || ''));
      const mediaMid = encodeURIComponent(String(song.mediaMid || song.media_mid || ''));
      endpoint = `/api/qq/song/url?mid=${mid}&mediaMid=${mediaMid}&quality=${encodeURIComponent(quality)}`;
    } else if (provider === 'kugou') {
      const hash = encodeURIComponent(
        String(song.hash || song.fileHash || song.audioHash || song.id || '')
      );
      endpoint = `/api/kugou/song/url?hash=${hash}&quality=${encodeURIComponent(quality)}`;
    } else if (provider === 'qishui') {
      const id = encodeURIComponent(String(song.id || song.providerSongId || ''));
      endpoint = `/api/qishui/song/url?id=${id}&quality=${encodeURIComponent(quality)}`;
    } else {
      const id = encodeURIComponent(String(song.id || ''));
      endpoint = `/api/song/url?id=${id}&level=${encodeURIComponent(quality)}`;
    }

    const res = await apiJson<{ url?: string; error?: string }>(endpoint, { timeoutMs: 14000 });
    if (!res?.url) {
      throw new Error(res?.error || 'AUDIO_URL_UNAVAILABLE');
    }
    return audioProxyUrl(res.url);
  }

  public async playTrack(
    song: SongRecord,
    quality: QualityPreference = 'exhigh',
    autoplay = true
  ): Promise<string> {
    this.trackToken++;
    this.clearCuefieldTimers();
    this.pendingTransition = null;
    this.executingTransition = false;

    const standby = this.standbyDeck;
    try {
      standby.audio.pause?.();
      standby.audio.removeAttribute?.('src');
      this.resetDeckProcessing(standby);
      this.applyDeckGain(standby, 0);
    } catch {
      // ignore
    }

    const url = await this.resolveSongPlaybackUrl(song, quality);
    if (!url) throw new Error('EMPTY_AUDIO_URL');

    const active = this.activeDeck;
    this.resetDeckProcessing(active);
    this.ensureDeckGraph(this.activeDeckId);
    this.applyDeckGain(active, this.effectiveVolume());

    active.audio.src = url;
    active.audio.currentTime = 0;
    if (typeof active.audio.load === 'function') {
      active.audio.load();
    }

    if (autoplay && typeof active.audio.play === 'function') {
      await active.audio.play();
      this.playing = true;
      this.callbacks.onPlayStateChange?.(true);
    }

    // 异步预分析当前歌曲节拍（不阻塞起播）
    this.ensureBeatMap(song, url).catch(() => {});
    if (this.automixEnabled) {
      this.setCuefieldStatus('waiting');
      this.scheduleCuefieldPrepare(1800);
    }
    return url;
  }

  public async play(): Promise<void> {
    this.ensureDeckGraph(this.activeDeckId);
    if (typeof this.activeDeck.audio.play === 'function') {
      await this.activeDeck.audio.play();
    }
    this.playing = true;
    this.callbacks.onPlayStateChange?.(true);
  }

  public pause(): void {
    if (typeof this.activeDeck.audio.pause === 'function') {
      this.activeDeck.audio.pause();
    }
    this.playing = false;
    this.callbacks.onPlayStateChange?.(false);
  }

  public seek(seconds: number): void {
    const audio = this.activeDeck.audio;
    if (!audio) return;
    const dur = Number.isFinite(audio.duration) ? audio.duration : Infinity;
    audio.currentTime = clamp(seconds, 0, dur);
  }

  public setVolume(value: number): void {
    this.volume = clamp(value, 0, 1);
    this.applyDeckGain(this.activeDeck, this.effectiveVolume());
  }

  public setMuted(muted: boolean): void {
    this.muted = Boolean(muted);
    this.applyDeckGain(this.activeDeck, this.effectiveVolume());
  }

  public setLoop(loop: boolean): void {
    this.activeDeck.audio.loop = Boolean(loop);
  }

  public getCurrentTime(): number {
    const t = this.activeDeck.audio.currentTime;
    return Number.isFinite(t) ? t : 0;
  }

  public getDuration(): number {
    const d = this.activeDeck.audio.duration;
    return Number.isFinite(d) ? d : 0;
  }

  public isPlaying(): boolean {
    return this.playing;
  }

  // ==================== Cuefield AutoMix ====================

  public setAutomixEnabled(enabled: boolean): void {
    this.automixEnabled = Boolean(enabled);
    if (!this.automixEnabled) {
      this.clearCuefieldTimers();
      this.pendingTransition = null;
      this.setCuefieldStatus('disabled');
    } else {
      this.setCuefieldStatus('waiting');
      this.scheduleCuefieldPrepare(800);
    }
  }

  private setCuefieldStatus(status: CuefieldStatus): void {
    this.cuefieldStatus = status;
    this.callbacks.onCuefieldStatus?.(status);
  }

  private clearCuefieldTimers(): void {
    this.cuefieldSerial++;
    if (this.prepareTimer) {
      clearTimeout(this.prepareTimer);
      this.prepareTimer = null;
    }
    while (this.timelineTimers.length) {
      const timer = this.timelineTimers.pop();
      if (timer) clearTimeout(timer);
    }
  }

  private scheduleCuefieldPrepare(delayMs = 1500): void {
    if (!this.automixEnabled) return;
    if (this.prepareTimer) clearTimeout(this.prepareTimer);
    this.prepareTimer = setTimeout(() => {
      this.prepareTimer = null;
      this.prepareNextTransition().catch(() => {});
    }, Math.max(200, delayMs));
  }

  public async prepareNextTransition(): Promise<void> {
    if (!this.automixEnabled || this.executingTransition) return;
    const ctx = this.callbacks.getNextTrackContext?.();
    if (!ctx?.currentSong || !ctx.nextSong) {
      this.setCuefieldStatus('waiting');
      return;
    }

    const fromKey = songIdentityKey(ctx.currentSong);
    const toKey = songIdentityKey(ctx.nextSong);
    if (!fromKey || !toKey || fromKey === toKey) return;
    if (this.pendingTransition && this.pendingTransition.fromKey === fromKey && this.pendingTransition.toKey === toKey) {
      return;
    }

    const serial = this.cuefieldSerial;
    const token = this.trackToken;
    this.setCuefieldStatus('preparing');

    try {
      const nextUrl = await this.resolveSongPlaybackUrl(ctx.nextSong, ctx.quality);
      if (serial !== this.cuefieldSerial || token !== this.trackToken) return;

      await Promise.all([
        this.ensureBeatMap(ctx.currentSong),
        this.ensureBeatMap(ctx.nextSong, nextUrl),
      ]);
      if (serial !== this.cuefieldSerial || token !== this.trackToken) return;

      const plan = await apiJson<CuefieldPlan>('/api/cuefield/transition', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          fromKey,
          toKey,
          fromLrc: ctx.fromLrc || '',
          toLrc: ctx.toLrc || '',
          exitBias: 'late',
          maxEntryTime: 32,
          enableLiveEndCrossfadeFallback: true,
        }),
        timeoutMs: 8000,
      });

      if (serial !== this.cuefieldSerial || token !== this.trackToken) return;
      if (!isExecutableCuefieldPlan(plan)) {
        this.setCuefieldStatus(plan?.chosen?.technicalFailure ? 'technical-error' : 'fallback');
        return;
      }

      const chosen = plan.chosen!;
      const executionMode = executionModeForPlan(plan);
      const timeline = Array.isArray(chosen.timeline) ? chosen.timeline : [];
      const entryTime = Math.max(0, toNum(chosen.entry?.time, 0));
      const exitTime = toNum(chosen.exit?.time, NaN);
      const protectedUntil = Math.max(0, toNum(chosen.protectedUntil, 0));

      const execution = buildCuefieldTimelineExecution({
        timeline,
        entryTime,
        executionMode,
        targetVolume: this.effectiveVolume(),
        mixStart: chosen.mixStart,
        handoffAt: chosen.handoffAt,
        audibleOverlap: chosen.audibleOverlap,
        preRollDuration: chosen.preRollDuration,
      });

      const hasExplicitWindow =
        Number.isFinite(chosen.mixStart) &&
        Number.isFinite(chosen.handoffAt) &&
        (chosen.handoffAt as number) > (chosen.mixStart as number);

      let triggerAt = hasExplicitWindow
        ? Math.max(
            protectedUntil,
            executionMode === 'end-of-track-crossfade'
              ? (chosen.mixStart as number) - execution.leadSec
              : (chosen.mixStart as number)
          )
        : Number.isFinite(exitTime)
        ? Math.max(protectedUntil, exitTime - execution.leadSec)
        : Math.max(protectedUntil, Math.max(0, this.getDuration() - execution.leadSec - 1));
      triggerAt = Math.max(triggerAt, protectedUntil);

      // 预加载 Deck B
      const standby = this.standbyDeck;
      this.resetDeckProcessing(standby);
      this.ensureDeckGraph(standby.id);
      this.applyDeckGain(standby, 0);
      standby.audio.src = nextUrl;
      standby.audio.currentTime = execution.bStart;
      if (typeof standby.audio.load === 'function') {
        standby.audio.load();
      }

      this.pendingTransition = {
        token,
        currentIndex: ctx.currentIndex,
        nextIndex: ctx.nextIndex,
        fromKey,
        toKey,
        plan,
        timeline,
        execution,
        audioUrl: nextUrl,
        executionMode,
        entryTime,
        exitTime,
        protectedUntil,
        minimumListenUntil: protectedUntil,
        triggerAt,
        mixStart: chosen.mixStart,
        handoffAt: chosen.handoffAt,
        nextSong: ctx.nextSong,
      };
      this.setCuefieldStatus('ready');
    } catch {
      if (serial === this.cuefieldSerial && token === this.trackToken) {
        this.setCuefieldStatus('error');
      }
    }
  }

  private checkCuefieldWindow(currentTime: number, duration: number): void {
    if (!this.automixEnabled || this.executingTransition) return;
    if (!this.pendingTransition) {
      if (duration > 18 && duration - currentTime <= 45 && this.cuefieldStatus === 'waiting') {
        this.scheduleCuefieldPrepare(50);
      }
      return;
    }
    if (this.pendingTransition.token !== this.trackToken) return;
    if (currentTime >= this.pendingTransition.triggerAt) {
      this.executeCuefieldTransition(this.pendingTransition);
    }
  }

  private executeCuefieldTransition(pending: CuefieldPendingTransition): void {
    this.executingTransition = true;
    this.pendingTransition = null;
    this.setCuefieldStatus('handoff');

    const outDeck = this.activeDeck;
    const inDeck = this.standbyDeck;
    const { actions, handoffDelayMs } = pending.execution;

    for (const action of actions) {
      const timer = setTimeout(() => {
        const deck = action.deck === 'A' ? outDeck : inDeck;
        this.applyNormalizedAction(deck, action, inDeck, pending);
      }, action.delayMs);
      this.timelineTimers.push(timer);
    }

    if (!actions.some((a) => a.op === 'handoff')) {
      const handoffTimer = setTimeout(() => {
        this.completeCuefieldHandoff(inDeck, outDeck, pending);
      }, handoffDelayMs);
      this.timelineTimers.push(handoffTimer);
    }
  }

  private applyNormalizedAction(
    deck: DeckGraph,
    action: NormalizedCuefieldAction,
    inDeck: DeckGraph,
    pending: CuefieldPendingTransition
  ): void {
    const now = this.ctx?.currentTime || 0;
    const durSec = action.durationMs / 1000;

    switch (action.op) {
      case 'play': {
        deck.audio.currentTime = Math.max(0, action.at);
        if (typeof deck.audio.play === 'function') {
          deck.audio.play().catch(() => {});
        }
        break;
      }
      case 'volume': {
        const target = clamp(action.target ?? this.effectiveVolume() * action.value, 0, 1);
        if (deck.gainNode && this.ctx) {
          const g = deck.gainNode.gain;
          g.cancelScheduledValues(now);
          g.setValueAtTime(g.value, now);
          if (action.curve.startsWith('equal-power-') && durSec > 0.02) {
            const curve = buildEqualPowerCurve(
              action.curve === 'equal-power-out' ? 'out' : 'in',
              33
            ).map((v) => v * this.effectiveVolume());
            g.setValueCurveAtTime(new Float32Array(curve), now, durSec);
          } else if (durSec > 0.01) {
            g.linearRampToValueAtTime(target, now + durSec);
          } else {
            g.setValueAtTime(target, now);
          }
        } else {
          this.applyDeckGain(deck, target);
        }
        break;
      }
      case 'filter': {
        if (!deck.filterNode) break;
        deck.filterNode.type = action.type === 'lowpass' ? 'lowpass' : 'highpass';
        const freq = clamp(action.value, 20, 20000);
        if (durSec > 0.01) {
          deck.filterNode.frequency.linearRampToValueAtTime(freq, now + durSec);
        } else {
          deck.filterNode.frequency.setValueAtTime(freq, now);
        }
        break;
      }
      case 'spectrum': {
        const gainDb = (norm: number | undefined) => (clamp(norm ?? 1, 0, 1) - 1) * 18;
        if (deck.bassNode) deck.bassNode.gain.setTargetAtTime(gainDb(action.low), now, 0.04);
        if (deck.midNode) deck.midNode.gain.setTargetAtTime(gainDb(action.mid), now, 0.04);
        if (deck.highNode) deck.highNode.gain.setTargetAtTime(gainDb(action.high), now, 0.04);
        break;
      }
      case 'echo': {
        if (!deck.echoSendNode || !deck.echoDelayNode || !deck.echoFeedbackNode || !deck.echoWetNode) {
          break;
        }
        if (action.enabled === false) {
          deck.echoSendNode.gain.setTargetAtTime(0, now, 0.05);
          deck.echoWetNode.gain.setTargetAtTime(0, now, 0.05);
        } else {
          const delaySec = clamp((60 / (action.bpm || 120)) * (action.delayBeats || 0.5), 0.02, 1.8);
          deck.echoDelayNode.delayTime.setValueAtTime(delaySec, now);
          deck.echoFeedbackNode.gain.setTargetAtTime(action.feedback || 0.3, now, 0.02);
          deck.echoSendNode.gain.setTargetAtTime(1, now, 0.02);
          deck.echoWetNode.gain.setTargetAtTime(action.wet || 0.25, now, 0.02);
        }
        break;
      }
      case 'rate': {
        deck.audio.playbackRate = clamp(action.value, 0.94, 1.06);
        break;
      }
      case 'stop': {
        deck.audio.pause?.();
        break;
      }
      case 'handoff': {
        const outDeck = inDeck.id === 'A' ? this.decks.B : this.decks.A;
        this.completeCuefieldHandoff(inDeck, outDeck, pending);
        break;
      }
      default:
        break;
    }
  }

  private completeCuefieldHandoff(
    inDeck: DeckGraph,
    outDeck: DeckGraph,
    pending: CuefieldPendingTransition
  ): void {
    if (!this.executingTransition) return;
    this.executingTransition = false;
    this.activeDeckId = inDeck.id;
    this.applyDeckGain(inDeck, this.effectiveVolume());
    this.resetDeckProcessing(inDeck);

    try {
      outDeck.audio.pause?.();
      outDeck.audio.removeAttribute?.('src');
      this.resetDeckProcessing(outDeck);
      this.applyDeckGain(outDeck, 0);
    } catch {
      // ignore
    }

    this.callbacks.onCuefieldHandoff?.(pending.nextSong, pending.nextIndex);
    this.setCuefieldStatus('waiting');
    this.scheduleCuefieldPrepare(2200);
  }

  // ==================== 节拍分析与缓存 ====================

  public getCachedBeatMap(song: SongRecord): BeatMapData | null {
    const key = songIdentityKey(song);
    return key ? this.beatMapCache.get(key) || null : null;
  }

  public async ensureBeatMap(song: SongRecord, resolvedUrl?: string): Promise<BeatMapData | null> {
    const key = songIdentityKey(song);
    if (!key) return null;
    if (this.beatMapCache.has(key)) return this.beatMapCache.get(key)!;
    if (this.beatAnalysisInFlight.has(key)) return this.beatAnalysisInFlight.get(key)!;

    const task = (async (): Promise<BeatMapData | null> => {
      try {
        const remote = await apiJson<{ ok?: boolean; beatMap?: BeatMapData; data?: BeatMapData }>(
          `/api/beatmap/cache?key=${encodeURIComponent(key)}`,
          { timeoutMs: 3500 }
        ).catch(() => null);
        const remoteMap = remote?.beatMap || remote?.data;
        if (remoteMap && Array.isArray(remoteMap.beats) && remoteMap.beats.length > 0) {
          this.beatMapCache.set(key, remoteMap);
          return remoteMap;
        }

        if (!resolvedUrl || typeof window === 'undefined') return null;
        const DecodeCtx = window.AudioContext || window.webkitAudioContext;
        if (!DecodeCtx) return null;

        const resp = await fetch(resolvedUrl);
        if (!resp.ok) return null;
        const ab = await resp.arrayBuffer();
        const decodeCtx = new DecodeCtx();
        let buffer: AudioBuffer | null = null;
        try {
          buffer = await decodeCtx.decodeAudioData(ab.slice(0));
        } finally {
          decodeCtx.close?.().catch(() => {});
        }
        if (!buffer) return null;

        const map = this.computeBeatMapFromBuffer(buffer, key);
        this.beatMapCache.set(key, map);

        // 异步上报 /api/beatmap/cache
        apiJson('/api/beatmap/cache', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ key, beatMap: map, song }),
          timeoutMs: 4000,
        }).catch(() => {});

        return map;
      } catch {
        return null;
      } finally {
        this.beatAnalysisInFlight.delete(key);
      }
    })();

    this.beatAnalysisInFlight.set(key, task);
    return task;
  }

  private computeBeatMapFromBuffer(buffer: AudioBuffer, key: string): BeatMapData {
    const ch0 = buffer.getChannelData(0);
    const sr = buffer.sampleRate || 44100;
    const duration = buffer.duration || ch0.length / sr;

    // 优先使用 MusicTempo
    if (typeof window !== 'undefined' && typeof window.MusicTempo === 'function') {
      try {
        const step = Math.max(1, Math.floor(sr / 11025));
        const downsampled = new Float32Array(Math.floor(ch0.length / step));
        for (let i = 0; i < downsampled.length; i++) {
          downsampled[i] = ch0[i * step];
        }
        const mt = new window.MusicTempo(downsampled);
        const bpm = clamp(mt.tempo || 120, 48, 220);
        const beats = Array.isArray(mt.beats) ? mt.beats.map((t) => round(t, 3)) : [];
        if (beats.length >= 4) {
          return {
            key,
            bpm: round(bpm, 2),
            gridStep: round(60 / bpm, 4),
            duration: round(duration, 3),
            beats,
            kicks: beats.filter((_, idx) => idx % 2 === 0),
            tempoSource: 'music-tempo',
            updatedAt: Date.now(),
          };
        }
      } catch {
        // fallback to PCM frame energy
      }
    }

    // 10ms 窗口 PCM 能量包络 + 局部峰值检测
    const winSize = Math.max(64, Math.floor(sr * 0.01));
    const nFrames = Math.floor(ch0.length / winSize);
    const energy = new Float32Array(nFrames);
    for (let f = 0; f < nFrames; f++) {
      let sum = 0;
      const offset = f * winSize;
      for (let i = 0; i < winSize; i++) {
        const v = ch0[offset + i];
        sum += v * v;
      }
      energy[f] = Math.sqrt(sum / winSize);
    }

    const beats: number[] = [];
    let lastBeatFrame = -35;
    for (let f = 2; f < nFrames - 2; f++) {
      const localAvg =
        (energy[f - 2] + energy[f - 1] + energy[f] + energy[f + 1] + energy[f + 2]) / 5;
      if (
        energy[f] > localAvg * 1.38 &&
        energy[f] >= energy[f - 1] &&
        energy[f] >= energy[f + 1] &&
        f - lastBeatFrame >= 28
      ) {
        beats.push(round(f * 0.01, 3));
        lastBeatFrame = f;
      }
    }

    const intervals: number[] = [];
    for (let i = 1; i < beats.length; i++) {
      const dt = beats[i] - beats[i - 1];
      if (dt >= 0.28 && dt <= 1.2) intervals.push(dt);
    }
    intervals.sort((a, b) => a - b);
    const medianStep = intervals.length ? intervals[Math.floor(intervals.length / 2)] : 0.5;
    const bpm = round(clamp(60 / Math.max(0.25, medianStep), 50, 210), 2);

    return {
      key,
      bpm,
      gridStep: round(60 / bpm, 4),
      duration: round(duration, 3),
      beats,
      kicks: beats,
      tempoSource: 'pcm-energy',
      updatedAt: Date.now(),
    };
  }

  // ==================== 零开销实时音频指标 ====================

  public getAudioReactiveMetrics(): AudioReactiveMetrics {
    const deck = this.activeDeck;
    const currentTime = this.getCurrentTime();
    const duration = this.getDuration();

    if (!this.playing || !deck.analyser) {
      this.beatPulse *= 0.88;
      return {
        bass: 0,
        mid: 0,
        treble: 0,
        energy: 0,
        beatPulse: this.beatPulse,
        kickLevel: 0,
        isBeat: false,
        playing: this.playing,
        currentTime,
        duration,
      };
    }

    deck.analyser.getByteFrequencyData(this.freqData);
    const len = this.freqData.length;
    const bassEnd = Math.min(len, 16);
    const midEnd = Math.min(len, 96);
    const trebleEnd = Math.min(len, 320);

    let bassSum = 0;
    for (let i = 1; i < bassEnd; i++) bassSum += this.freqData[i];
    const bass = bassSum / Math.max(1, bassEnd - 1) / 255;

    let midSum = 0;
    for (let i = bassEnd; i < midEnd; i++) midSum += this.freqData[i];
    const mid = midSum / Math.max(1, midEnd - bassEnd) / 255;

    let trebleSum = 0;
    for (let i = midEnd; i < trebleEnd; i++) trebleSum += this.freqData[i];
    const treble = trebleSum / Math.max(1, trebleEnd - midEnd) / 255;

    const energy = bass * 0.52 + mid * 0.32 + treble * 0.16;

    let kickLevel = bass;
    if (deck.beatAnalyser) {
      deck.beatAnalyser.getByteFrequencyData(this.beatFreqData);
      let kSum = 0;
      const kEnd = Math.min(this.beatFreqData.length, 8);
      for (let i = 1; i < kEnd; i++) kSum += this.beatFreqData[i];
      kickLevel = kSum / Math.max(1, kEnd - 1) / 255;
    }

    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    const flux = Math.max(0, kickLevel - this.prevEnergy);
    this.prevEnergy = this.prevEnergy * 0.82 + kickLevel * 0.18;

    let isBeat = false;
    if (flux > 0.11 && kickLevel > 0.34 && now - this.lastBeatAt > 240) {
      this.beatPulse = Math.min(1, 0.62 + flux * 1.8);
      this.lastBeatAt = now;
      isBeat = true;
    } else {
      this.beatPulse *= 0.9;
      if (this.beatPulse < 0.005) this.beatPulse = 0;
    }

    return {
      bass,
      mid,
      treble,
      energy,
      beatPulse: this.beatPulse,
      kickLevel,
      isBeat,
      playing: this.playing,
      currentTime,
      duration,
    };
  }

  public dispose(): void {
    this.clearCuefieldTimers();
    (['A', 'B'] as DeckId[]).forEach((id) => {
      const deck = this.decks[id];
      try {
        deck.audio.pause?.();
        deck.audio.removeAttribute?.('src');
      } catch {
        // ignore
      }
    });
    if (this.ctx && this.ctx.state !== 'closed') {
      this.ctx.close?.().catch(() => {});
    }
    this.ctx = null;
  }
}

export const audioEngine = new AudioEngine();
