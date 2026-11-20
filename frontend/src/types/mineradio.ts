/**
 * Mineradio 核心前端类型定义与默认 FX 配置
 */

export type ProviderType = 'netease' | 'qq' | 'kugou' | 'qishui' | 'local' | 'mineradio';

export type SearchMode = 'song' | 'netease' | 'qq' | 'kugou' | 'qishui' | 'podcast';

export type PlayMode = 'loop' | 'single' | 'shuffle';

export type QualityPreference = 'jymaster' | 'hires' | 'lossless' | 'exhigh' | 'standard';

export interface SongRecord {
  id?: string | number;
  mid?: string;
  songmid?: string;
  mediaMid?: string;
  media_mid?: string;
  qqId?: string | number;
  hash?: string;
  fileHash?: string;
  audioHash?: string;
  hqHash?: string;
  sqHash?: string;
  resHash?: string;
  providerSongId?: string;
  provider_song_id?: string;
  trackId?: string | number;
  track_id?: string | number;
  albumId?: string | number;
  album_id?: string | number;
  albumMid?: string;
  albummid?: string;
  albumAudioId?: string | number;
  album_audio_id?: string | number;
  mixSongId?: string | number;
  mix_song_id?: string | number;
  localFileId?: string;
  localKey?: string;
  localUrl?: string;
  localPath?: string;
  localMissing?: boolean;
  name: string;
  title?: string;
  artist?: string;
  album?: string;
  cover?: string;
  duration?: number;
  durationMs?: number;
  dt?: number;
  provider?: ProviderType;
  source?: ProviderType | string;
  type?: string;
  fee?: number;
  Fee?: number;
  playable?: boolean;
  playbackMode?: string;
  recommendationSource?: string;
  hasLyric?: boolean;
  lyricSource?: string;
  vipRequired?: boolean;
  needVip?: boolean;
  onlyVipPlayable?: boolean;
  privilege?: Record<string, unknown>;
  lyric?: string;
  tlyric?: string;
  romalrc?: string;
  podcastId?: string | number;
  programId?: string | number;
  radioId?: string | number;
  key?: string;
  [key: string]: unknown;
}

export interface PlaylistSummary {
  id: string | number;
  name: string;
  cover?: string;
  trackCount: number;
  playCount?: number;
  provider: ProviderType;
  subscribed?: boolean;
  shelfPane?: 'mine' | 'fav';
  description?: string;
  createdAt?: number;
  updatedAt?: number;
  tracks?: SongRecord[];
}

export interface LyricWord {
  text: string;
  start: number;
  end: number;
}

export interface LyricLine {
  time: number;
  text: string;
  translation?: string;
  roma?: string;
  words?: LyricWord[];
  fallback?: boolean;
}

export interface FxSettings {
  visualPresetSchema?: string;
  preset: number;
  intensity: number;
  cinemaShake: number;
  depth: number;
  coverResolution: number;
  point: number;
  speed: number;
  twist: number;
  color: number;
  scatter: number;
  bgFade: number;
  bloomStrength: number;
  lyricGlowStrength: number;
  lyricBackgroundAdapt: number;
  lyricScale: number;
  lyricOffsetX: number;
  lyricOffsetY: number;
  lyricOffsetZ: number;
  lyricTiltX: number;
  lyricTiltY: number;
  lyricColorMode: 'auto' | 'custom' | string;
  lyricColor: string;
  lyricHighlightMode: 'auto' | 'custom' | string;
  lyricHighlightColor: string;
  lyricGlowLinked: boolean;
  lyricGlowColor: string;
  lyricDisplayMode: 'cinema' | 'single' | 'multi' | 'custom' | string;
  lyricTranslationMode: 'multi' | 'off' | 'translation' | 'roma' | string;
  lyricMotionStyle: 'float' | 'glitch' | 'minimal' | string;
  lyricCustomLineCount: number;
  lyricGlitchCameraBind: boolean;
  lyricGlitchIntensity: number;
  lyricGlitchSlice: number;
  lyricGlitchChroma: number;
  lyricGlitchRate: number;
  lyricGlitchJitter: number;
  lyricContextOpacity: number;
  lyricContextSpread: number;
  lyricTranslationGap: number;
  lyricTranslationScale: number;
  lyricTranslationOpacity: number;
  lyricEdgeFade: number;
  lyricMotionSoftness: number;
  lyricFont: string;
  lyricLetterSpacing: number;
  lyricLineHeight: number;
  lyricWeight: number;
  lyricTextureClarity: number;
  lyricLiveViewportFit: boolean;
  lyricContextHighQuality: boolean;
  lyricBackdropAdapt: boolean;
  coverBackdropAdapt: boolean;
  visualTintMode: 'auto' | 'custom' | string;
  visualTintColor: string;
  uiAccentColor: string;
  homeAccentColor: string;
  homeIconColor: string;
  visualIconColor: string;
  backgroundColorMode: 'cover' | 'custom' | 'black' | string;
  backgroundColor: string;
  backgroundOpacity: number;
  windowBackgroundOpacity: number;
  backgroundGlassOpacity: number;
  controlGlassChromaticOffset: number;
  playlistPanelGlassBlur: number;
  playlistPanelGlassDensity: number;
  playlistPanelOpenDuration: number;
  playlistPanelCloseDuration: number;
  backgroundColorCustom: boolean;
  backgroundImage: string;
  backgroundMedia: string | null;
  backgroundAlbumCover: boolean;
  backgroundMediaCropX: number;
  backgroundMediaCropY: number;
  backgroundMediaZoom: number;
  desktopLyrics: boolean;
  desktopLyricsSize: number;
  desktopLyricsOpacity: number;
  desktopLyricsY: number;
  desktopLyricsClickThrough: boolean;
  desktopLyricsCinema: boolean;
  desktopLyricsHighlight: boolean;
  desktopLyricsFps: number;
  wallpaperMode: boolean;
  wallpaperOpacity: number;
  wallpaperFps: number;
  floatLayer: boolean;
  cinema: boolean;
  edge: boolean;
  aiDepth: boolean;
  bloom: boolean;
  lyricGlow: boolean;
  lyricGlowBeat: boolean;
  lyricGlowParticles: boolean;
  lyricVerticalFloat: boolean;
  backgroundStarRiver: boolean;
  lyricPauseHold: boolean;
  lyricCameraLock: boolean;
  sonicGroundAmplitude: number;
  sonicGroundMotionSpeed: number;
  sonicGroundDensity: number;
  sonicGroundRange: number;
  sonicGroundLower: number;
  sonicGroundDepth: number;
  sonicGroundAutoRotate: number;
  sonicGroundColorMode: string;
  sonicGroundBaseColor: string;
  sonicGroundCoolColor: string;
  sonicGroundWarmColor: string;
  sonicGroundAccentColor: string;
  sonicGroundGlow: number;
  sonicGroundSubBass: number;
  sonicGroundBass: number;
  sonicGroundLowMid: number;
  sonicGroundMid: number;
  sonicGroundHighMid: number;
  sonicGroundPresence: number;
  sonicGroundBrilliance: number;
  sonicGroundAir: number;
  sonicGroundFloatingEnabled: boolean;
  sonicGroundFloatingIntensity: number;
  sonicGroundFloatingMinSize: number;
  sonicGroundFloatingMaxSize: number;
  sonicGroundFloatingSpeed: number;
  sonicGroundFloatingCount: number;
  sonicAudioMonitorEnabled: boolean;
  sonicAudioAutoTrack: boolean;
  sonicAudioSensitivity: number;
  sonicAudioBandStart: number;
  sonicAudioBandEnd: number;
  sonicAudioThreshold: number;
  sonicAudioPulseStrength: number;
  sonicWorkshopInputGain: number;
  sonicWorkshopAudioIntensity: number;
  sonicWorkshopResponseRange: number;
  sonicWorkshopPeakIntensity: number;
  sonicWorkshopColorMode: string;
  sonicWorkshopTheme: string;
  sonicWorkshopCustomColor: string;
  sonicWorkshopBaseColorMode: string;
  sonicWorkshopBaseColor: string;
  sonicWorkshopWarmColorMode: string;
  sonicWorkshopWarmColor: string;
  sonicWorkshopCoolColorMode: string;
  sonicWorkshopCoolColor: string;
  sonicWorkshopRippleColorMode: string;
  sonicWorkshopRippleColor: string;
  sonicWorkshopPeakColorMode: string;
  sonicWorkshopPeakColor: string;
  particleLyrics: boolean;
  backCover: boolean;
  shelf: 'side' | 'stage' | 'off' | string;
  shelfPinnedOpen: boolean;
  shelfCameraMode: string;
  shelfPresence: string;
  shelfShowPodcasts: boolean;
  shelfMergeCollections: boolean;
  shelfSize: number;
  shelfOffsetX: number;
  shelfOffsetY: number;
  shelfOffsetZ: number;
  shelfAngleY: number;
  shelfAngleYManual: boolean;
  shelfOpacity: number;
  shelfBgOpacity: number;
  shelfAccentColor: string;
  shelfDetailOffsetX: number;
  shelfDetailOffsetY: number;
  shelfDetailOffsetZ: number;
  shelfDetailScale: number;
  shelfDetailAngleX: number;
  shelfDetailAngleY: number;
  shelfDetailRowGap: number;
  shelfDetailOpenDuration: number;
  shelfDetailCloseDuration: number;
  shelfDetailRowDuration: number;
  shelfDetailIntroStrength: number;
  shelfDetailParallax: number;
  shelfSummonOpenDuration: number;
  shelfSummonCloseDuration: number;
  shelfSummonSlide: number;
  shelfSummonStagger: number;
  shelfSummonScale: number;
  shelfSummonParallax: number;
  shelfCameraEnterSpeed: number;
  shelfCameraExitSpeed: number;
  performanceBackground: 'release' | 'pause' | 'keep' | string;
  performanceQuality: 'eco' | 'balanced' | 'high' | 'ultra' | string;
  foregroundFpsMode: 'vsync' | 'adaptive' | '45' | '60' | '75' | '90' | '120' | string;
  memoryAutoTrimApp: boolean;
  memoryAutoTrimOnBackground: boolean;
  memoryAutoSystemTrim: boolean;
  memorySystemAutoElevate: boolean;
  memorySystemIntervalMin: number;
  memorySystemThresholdPercent: number;
  memorySystemMask: number;
  memorySafetyRevision: number;
  liveBackgroundKeep: boolean;
  cam: string;
  gesturePlayerActions: boolean;
  gestureHandOverlay: boolean;
  gestureSensitivity: string;
}

/**
 * 与 public/default-user-fx-archive.json 及 public/js/modules/00-state/04-fx-defaults.js 严格对齐的默认 FX 参数
 */
export const DEFAULT_FX_SETTINGS: FxSettings = {
  visualPresetSchema: 'skull-preset-v2',
  preset: 0,
  intensity: 0.85,
  cinemaShake: 0.5,
  depth: 0.2,
  coverResolution: 1.55,
  point: 1.0,
  speed: 1.0,
  twist: 0.0,
  color: 1.1,
  scatter: 0.0,
  bgFade: 0.2,
  bloomStrength: 0.62,
  lyricGlowStrength: 0.28,
  lyricBackgroundAdapt: 0.72,
  lyricScale: 1.0,
  lyricOffsetX: 0,
  lyricOffsetY: 0,
  lyricOffsetZ: 0,
  lyricTiltX: 0,
  lyricTiltY: 0,
  lyricColorMode: 'auto',
  lyricColor: '#7ec8d8',
  lyricHighlightMode: 'auto',
  lyricHighlightColor: '#fff0b8',
  lyricGlowLinked: true,
  lyricGlowColor: '#9db8cf',
  lyricDisplayMode: 'cinema',
  lyricTranslationMode: 'multi',
  lyricMotionStyle: 'float',
  lyricCustomLineCount: 10,
  lyricGlitchCameraBind: true,
  lyricGlitchIntensity: 1.0,
  lyricGlitchSlice: 0.72,
  lyricGlitchChroma: 0.86,
  lyricGlitchRate: 1.0,
  lyricGlitchJitter: 0.72,
  lyricContextOpacity: 0.54,
  lyricContextSpread: 1.96,
  lyricTranslationGap: 0.92,
  lyricTranslationScale: 0.65,
  lyricTranslationOpacity: 0.86,
  lyricEdgeFade: 0.32,
  lyricMotionSoftness: 0.72,
  lyricFont: 'sans',
  lyricLetterSpacing: 0,
  lyricLineHeight: 1.0,
  lyricWeight: 750,
  lyricTextureClarity: 1,
  lyricLiveViewportFit: true,
  lyricContextHighQuality: true,
  lyricBackdropAdapt: true,
  coverBackdropAdapt: true,
  visualTintMode: 'auto',
  visualTintColor: '#9db8cf',
  uiAccentColor: '#ffffff',
  homeAccentColor: '#ffffff',
  homeIconColor: '#ffffff',
  visualIconColor: '#ffffff',
  backgroundColorMode: 'cover',
  backgroundColor: '#000000',
  backgroundOpacity: 1,
  windowBackgroundOpacity: 1,
  backgroundGlassOpacity: 0,
  controlGlassChromaticOffset: 50,
  playlistPanelGlassBlur: 14,
  playlistPanelGlassDensity: 0.55,
  playlistPanelOpenDuration: 0.72,
  playlistPanelCloseDuration: 0.48,
  backgroundColorCustom: false,
  backgroundImage: '',
  backgroundMedia: null,
  backgroundAlbumCover: false,
  backgroundMediaCropX: 50,
  backgroundMediaCropY: 50,
  backgroundMediaZoom: 1,
  desktopLyrics: false,
  desktopLyricsSize: 1.0,
  desktopLyricsOpacity: 0.92,
  desktopLyricsY: 0.76,
  desktopLyricsClickThrough: false,
  desktopLyricsCinema: false,
  desktopLyricsHighlight: false,
  desktopLyricsFps: 0,
  wallpaperMode: false,
  wallpaperOpacity: 1,
  wallpaperFps: 60,
  floatLayer: false,
  cinema: true,
  edge: false,
  aiDepth: false,
  bloom: false,
  lyricGlow: true,
  lyricGlowBeat: true,
  lyricGlowParticles: false,
  lyricVerticalFloat: true,
  backgroundStarRiver: true,
  lyricPauseHold: true,
  lyricCameraLock: false,
  sonicGroundAmplitude: 50,
  sonicGroundMotionSpeed: 50,
  sonicGroundDensity: 46,
  sonicGroundRange: 82,
  sonicGroundLower: 68,
  sonicGroundDepth: 62,
  sonicGroundAutoRotate: 50,
  sonicGroundColorMode: 'cover',
  sonicGroundBaseColor: '#05070c',
  sonicGroundCoolColor: '#0066ff',
  sonicGroundWarmColor: '#ff3c19',
  sonicGroundAccentColor: '#33e6ff',
  sonicGroundGlow: 20,
  sonicGroundSubBass: 90,
  sonicGroundBass: 92,
  sonicGroundLowMid: 50,
  sonicGroundMid: 50,
  sonicGroundHighMid: 50,
  sonicGroundPresence: 25,
  sonicGroundBrilliance: 50,
  sonicGroundAir: 48,
  sonicGroundFloatingEnabled: true,
  sonicGroundFloatingIntensity: 36,
  sonicGroundFloatingMinSize: 9,
  sonicGroundFloatingMaxSize: 12,
  sonicGroundFloatingSpeed: 59,
  sonicGroundFloatingCount: 80,
  sonicAudioMonitorEnabled: true,
  sonicAudioAutoTrack: true,
  sonicAudioSensitivity: 100,
  sonicAudioBandStart: 1,
  sonicAudioBandEnd: 4,
  sonicAudioThreshold: 32,
  sonicAudioPulseStrength: 62,
  sonicWorkshopInputGain: 82,
  sonicWorkshopAudioIntensity: 1.15,
  sonicWorkshopResponseRange: 1.3,
  sonicWorkshopPeakIntensity: 0.62,
  sonicWorkshopColorMode: 'cover',
  sonicWorkshopTheme: 'minimal-monochrome',
  sonicWorkshopCustomColor: '#d9dde3',
  sonicWorkshopBaseColorMode: 'cover',
  sonicWorkshopBaseColor: '#0b0c0e',
  sonicWorkshopWarmColorMode: 'cover',
  sonicWorkshopWarmColor: '#d9dde3',
  sonicWorkshopCoolColorMode: 'custom',
  sonicWorkshopCoolColor: '#ffffff',
  sonicWorkshopRippleColorMode: 'cover',
  sonicWorkshopRippleColor: '#ffffff',
  sonicWorkshopPeakColorMode: 'cover',
  sonicWorkshopPeakColor: '#f2f5f8',
  particleLyrics: true,
  backCover: false,
  shelf: 'side',
  shelfPinnedOpen: false,
  shelfCameraMode: 'dynamic',
  shelfPresence: 'auto',
  shelfShowPodcasts: false,
  shelfMergeCollections: true,
  shelfSize: 0.92,
  shelfOffsetX: -0.34,
  shelfOffsetY: -0.2,
  shelfOffsetZ: 0.12,
  shelfAngleY: -11,
  shelfAngleYManual: true,
  shelfOpacity: 1,
  shelfBgOpacity: 0.79,
  shelfAccentColor: '#ffffff',
  shelfDetailOffsetX: 0,
  shelfDetailOffsetY: 0,
  shelfDetailOffsetZ: 0,
  shelfDetailScale: 1.35,
  shelfDetailAngleX: 0,
  shelfDetailAngleY: -13,
  shelfDetailRowGap: 1,
  shelfDetailOpenDuration: 0.6,
  shelfDetailCloseDuration: 0.18,
  shelfDetailRowDuration: 0.72,
  shelfDetailIntroStrength: 1,
  shelfDetailParallax: 1,
  shelfSummonOpenDuration: 0.91,
  shelfSummonCloseDuration: 0.46,
  shelfSummonSlide: 1.9,
  shelfSummonStagger: 1,
  shelfSummonScale: 1,
  shelfSummonParallax: 1,
  shelfCameraEnterSpeed: 0.24,
  shelfCameraExitSpeed: 0.24,
  performanceBackground: 'release',
  performanceQuality: 'eco',
  foregroundFpsMode: 'vsync',
  memoryAutoTrimApp: true,
  memoryAutoTrimOnBackground: true,
  memoryAutoSystemTrim: true,
  memorySystemAutoElevate: true,
  memorySystemIntervalMin: 30,
  memorySystemThresholdPercent: 78,
  memorySystemMask: 29,
  memorySafetyRevision: 3,
  liveBackgroundKeep: false,
  cam: 'off',
  gesturePlayerActions: true,
  gestureHandOverlay: true,
  gestureSensitivity: 'balanced',
};

export interface VisualPresetMeta {
  id: number;
  name: string;
  desc: string;
  premiumVisual?: boolean;
  accent?: string;
  accent2?: string;
}

export const VISUAL_PRESETS: VisualPresetMeta[] = [
  { id: 0, name: 'emily专辑封面', desc: '封面粒子 · 快速入场' },
  { id: 1, name: '滚筒', desc: '隧道 · 沉浸感' },
  { id: 2, name: '星球', desc: '星球 · 雕塑感' },
  { id: 3, name: '虚空', desc: '无粒子 · 自定义背景' },
  { id: 4, name: '唱片', desc: '唱片 · 圆形封面' },
  { id: 5, name: '星河', desc: '壁纸粒子 · 音乐律动' },
  { id: 6, name: '安魂', desc: '骷髅·YUI7W' },
  { id: 7, name: '音域回响', desc: 'Sonic-Topography · 作者 Ajin' },
  { id: 8, name: '音域回响', desc: 'Wallpaper Engine · 作者 CmzYa' },
  { id: 9, name: '月蚀圣环', desc: '黑曜轨道 · 冷金日冕', premiumVisual: true, accent: '#e8c98d', accent2: '#8fd8ff' },
  { id: 10, name: '雨幕霓虹', desc: '城市雨丝 · 色谱残光', premiumVisual: true, accent: '#67efff', accent2: '#ff6bb5' },
  { id: 11, name: '折光蝶群', desc: '折纸翼阵 · 光谱迁徙', premiumVisual: true, accent: '#f0d7ff', accent2: '#75e6d1' },
  { id: 12, name: '深海绽放', desc: '生物荧光 · 潮汐花冠', premiumVisual: true, accent: '#75f0d0', accent2: '#8178ff' },
];

export interface DesktopWindowState {
  isDesktop: boolean;
  isMaximized?: boolean;
  isFullScreen?: boolean;
  isMinimized?: boolean;
  isDesktopEmbedded?: boolean;
  isDesktopInteractive?: boolean;
  isPrimaryDisplay?: boolean;
  hasDisplayOnLeft?: boolean;
  closeBehavior?: 'tray' | 'exit' | 'ask' | string;
  desktopLyricsEnabled?: boolean;
  desktopLyricsLocked?: boolean;
  wallpaperMode?: boolean;
  [key: string]: unknown;
}

export interface RuntimeEndpoints {
  httpBase: string;
  mediaToken: string;
}

export interface PlatformLoginEntry {
  loggedIn: boolean;
  userId?: string | number;
  nickname?: string;
  avatar?: string;
  avatarUrl?: string;
  vipType?: number;
  cookieValid?: boolean;
  updatedAt?: number;
}

export type PlatformStatus = PlatformLoginEntry;

export interface LoginStatusMap {
  netease: PlatformLoginEntry;
  qq: PlatformLoginEntry;
  kugou: PlatformLoginEntry;
  qishui: PlatformLoginEntry;
}

export interface EasterEggStatus {
  ok: boolean;
  gateVersion: string;
  unlocked: boolean;
  resetComplete: boolean;
  error?: string;
  message?: string;
}

export interface UpdateDownloadPage {
  label: string;
  url: string;
}

export interface UpdateReleaseInfo {
  tagName?: string;
  name?: string;
  version?: string;
  publishedAt?: string;
  htmlUrl?: string;
  externalUrl?: string;
  downloadPageUrl?: string;
  downloadPages?: UpdateDownloadPage[];
  downloadUrl?: string;
  summary?: string;
  notes?: string[];
  [key: string]: unknown;
}

export interface UpdateCheckResult {
  ok?: boolean;
  configured?: boolean;
  preview?: boolean;
  updateAvailable?: boolean;
  currentVersion?: string;
  latestVersion?: string;
  hasUpdate?: boolean;
  releaseUrl?: string;
  notes?: string | string[];
  downloadPages?: UpdateDownloadPage[];
  release?: UpdateReleaseInfo;
  checkedAt?: number;
  error?: string;
}

export interface CuefieldBridgeSpec {
  template: 'drum-build' | 'echo-break' | 'loop-rise' | 'impact-drop' | string;
  bars: number;
  bpmFrom: number;
  bpmTo: number;
  stageDurations?: number[];
}

export interface CuefieldTimelineAction {
  t: number;
  deck?: 'A' | 'B';
  op: 'play' | 'volume' | 'filter' | 'echo' | 'duck' | 'spectrum' | 'rate' | 'loop' | 'bridge' | 'stop' | 'handoff' | string;
  type?: string;
  curve?: string;
  value?: number;
  target?: number;
  duration?: number;
  at?: number;
  volume?: number;
  enabled?: boolean;
  bpm?: number;
  delayBeats?: number;
  feedback?: number;
  wet?: number;
  tailMs?: number;
  depth?: number;
  pulses?: number;
  beats?: number;
  attack?: number;
  hold?: number;
  release?: number;
  low?: number;
  mid?: number;
  high?: number;
  startAt?: number;
  loopBeats?: number;
  slip?: boolean;
  bridge?: CuefieldBridgeSpec;
  fallbackTimeline?: CuefieldTimelineAction[];
  optionalWhenLate?: boolean;
  maxLateMs?: number;
}

export interface CuefieldEvaluation {
  tier?: 'magic' | 'usable' | 'usable_but_not_magic' | 'weak' | 'reject' | string;
  score?: number;
  confidence?: number;
  risks?: string[];
  reasons?: string[];
}

export interface CuefieldChosenPlan {
  artifactId?: string;
  transitionRecipe?: string;
  recipeCandidate?: { recipe?: string; [key: string]: unknown };
  evaluation?: CuefieldEvaluation;
  score?: number;
  exit?: { time?: number; bar?: number; [key: string]: unknown };
  entry?: { time?: number; bar?: number; [key: string]: unknown };
  mixStart?: number;
  handoffAt?: number;
  preRollDuration?: number;
  protectedUntil?: number;
  audibleOverlap?: number;
  exitRatio?: number;
  technicalFailure?: boolean;
  errorCode?: string;
  bridgePlan?: Record<string, unknown> | null;
  timeline?: CuefieldTimelineAction[];
  [key: string]: unknown;
}

export interface CuefieldPlan {
  ok: boolean;
  error?: string;
  chosen?: CuefieldChosenPlan | null;
  [key: string]: unknown;
}

export type CuefieldStatus =
  | 'idle'
  | 'disabled'
  | 'waiting'
  | 'preparing'
  | 'waiting-beatmap'
  | 'missing-audio'
  | 'fallback'
  | 'technical-error'
  | 'ready'
  | 'handoff'
  | 'error';

export interface NormalizedCuefieldAction {
  t: number;
  delayMs: number;
  durationMs: number;
  deck: 'A' | 'B';
  op: string;
  type: string;
  curve: string;
  value: number;
  target?: number;
  at: number;
  optionalWhenLate: boolean;
  maxLateMs: number;
  sourceZeroPreRoll: boolean;
  enabled?: boolean;
  bpm?: number;
  delayBeats?: number;
  feedback?: number;
  wet?: number;
  tailMs?: number;
  depth?: number;
  pulses?: number;
  beats?: number;
  attack?: number;
  hold?: number;
  release?: number;
  low?: number;
  mid?: number;
  high?: number;
  startAt?: number;
  loopBeats?: number;
  loopSeconds?: number;
  slip?: boolean;
  bridge?: CuefieldBridgeSpec;
  fallbackTimeline?: CuefieldTimelineAction[];
}

export interface CuefieldTimelineExecution {
  leadSec: number;
  bStart: number;
  handoffDelayMs: number;
  audibleStartDelayMs: number | null;
  audibleOverlap: number | null;
  preRollDuration: number | null;
  requiresAGraph: boolean;
  requiresBGraph: boolean;
  requiresBridge: boolean;
  requiresSourceLoop: boolean;
  actions: NormalizedCuefieldAction[];
}

export interface BeatMapData {
  key?: string;
  bpm: number;
  gridStep: number;
  duration: number;
  beats: number[];
  pulseBeats?: number[];
  cameraBeats?: number[];
  kicks?: number[];
  tempoSource?: string;
  updatedAt?: number;
}

export interface AudioReactiveMetrics {
  bass: number;
  mid: number;
  treble: number;
  energy: number;
  beatPulse: number;
  kickLevel: number;
  isBeat: boolean;
  playing: boolean;
  currentTime: number;
  duration: number;
}

export interface TodayListeningStats {
  date: string;
  totalSeconds: number;
  playCount: number;
  topArtists?: Array<{ name: string; count: number; seconds?: number }>;
}

export interface DiscoverHomeData {
  ok?: boolean;
  loggedIn?: boolean;
  dailySongs?: SongRecord[];
  playlists?: PlaylistSummary[];
  podcasts?: PlaylistSummary[];
  songs?: SongRecord[];
  updatedAt?: number;
  error?: string;
  [key: string]: unknown;
}

export type HomeDiscoverData = DiscoverHomeData;

export interface WeatherRadioWeatherInfo {
  city?: string;
  temperature?: number;
  weatherCode?: number;
  summary?: string;
  toString?: () => string;
}

export interface WeatherRadioStationInfo {
  title?: string;
  subtitle?: string;
  seedQueries?: string[];
  songs?: SongRecord[];
  updatedAt?: number;
}

export interface WeatherRadioData {
  ok?: boolean;
  weather?: WeatherRadioWeatherInfo | string;
  radio?: WeatherRadioStationInfo;
  city?: string;
  temperature?: string | number;
  greeting?: string;
  tracks?: SongRecord[];
  [key: string]: unknown;
}

export interface CacheSettingsDetail {
  rootPath: string;
  lyricsPath: string;
  beatmapsPath: string;
  wallpaperEnginePath: string;
  userDataPath: string;
  restartRequired?: boolean;
  [key: string]: unknown;
}

export interface CacheUsageStats {
  lyricsBytes: number;
  beatmapsBytes: number;
  wallpaperEngineBytes: number;
  userDataBytes: number;
  totalManagedBytes: number;
  [key: string]: unknown;
}

export interface CacheSettings {
  ok?: boolean;
  settings?: CacheSettingsDetail;
  usage?: CacheUsageStats;
  restartRequired?: boolean;
  cacheRoot?: string;
  effectiveRoot?: string;
  beatmapDiskEnabled?: boolean;
  lyricCacheEnabled?: boolean;
  cDriveProtected?: boolean;
  [key: string]: unknown;
}

export interface MemorySnapshot {
  supported: boolean;
  platform?: string;
  totalBytes?: number;
  freeBytes?: number;
  usedBytes?: number;
  totalMB?: number;
  freeMB?: number;
  usedMB?: number;
  usedPercent?: number;
  process?: {
    rssBytes?: number;
    rssMB?: number;
    heapMB?: number;
    privateMB?: number;
  };
  workingSetBytes?: number;
  privateBytes?: number;
  systemTotalBytes?: number;
  systemUsedPercent?: number;
  source?: string;
  error?: string;
  [key: string]: unknown;
}

export interface DesktopLyricsPayload {
  title?: string;
  artist?: string;
  line?: string;
  translation?: string;
  nextLine?: string;
  playing?: boolean;
  currentTime?: number;
  duration?: number;
  progress?: number;
  size?: number;
  opacity?: number;
  y?: number;
  clickThrough?: boolean;
  cinema?: boolean;
  highlight?: boolean;
  fps?: number;
}

export interface WallpaperEngineProject {
  id: string;
  title: string;
  type?: string;
  preview?: string;
  previewUrl?: string;
  mediaUrl?: string;
  workshopId?: string;
  file?: string;
  rootId?: string;
  [key: string]: unknown;
}

export interface ShelfItem {
  type: 'playlist' | 'podcastCollection' | 'queue';
  title: string;
  sub: string;
  cover?: string;
  tag: string;
  playlistId?: string;
  provider?: ProviderType;
  queueIndex?: number;
  podcastKey?: string;
  itemType?: string;
}

export interface BackgroundMediaState {
  type: 'none' | 'image' | 'video';
  url: string;
  muted?: boolean;
  source?: string;
  projectId?: string;
}

export interface HotkeyBinding {
  id: string;
  action: string;
  label: string;
  accelerator: string;
  enabled: boolean;
}

export interface FxArchiveSlot {
  name: string;
  label?: string;
  fx: Partial<FxSettings>;
}

export interface PlayerStoreState {
  // 播放与队列状态
  playQueue: SongRecord[];
  currentIdx: number;
  currentSong: SongRecord | null;
  playing: boolean;
  currentTime: number;
  duration: number;
  volume: number;
  muted: boolean;
  playMode: PlayMode;
  quality: QualityPreference;
  audioError: string | null;
  automixEnabled: boolean;
  cuefieldStatus: CuefieldStatus;

  // 歌词状态
  lyrics: LyricLine[];
  activeLyricIdx: number;
  lyricOffsetSec: number;
  desktopLyricsEnabled: boolean;
  desktopLyricsLocked: boolean;

  // 搜索与首页
  searchMode: SearchMode;
  searchQuery: string;
  searchResults: SongRecord[];
  searchLoading: boolean;
  searchHistory: string[];
  homeDiscover: DiscoverHomeData | null;
  weatherRadio: WeatherRadioData | null;
  todayListeningStats: TodayListeningStats;

  // 歌单与本地曲库
  userPlaylists: PlaylistSummary[];
  builtInPlaylists: PlaylistSummary[];
  localTracks: SongRecord[];
  activePlaylistTracks: SongRecord[];

  // 登录态与彩蛋门控
  easterEggUnlocked: boolean;
  platformLogins: LoginStatusMap;

  // 视觉 FX 参数与预设 / UI 面板
  fx: FxSettings;
  activePreset: number;
  diyMode: boolean;
  shelfOpen: boolean;
  fxPanelOpen: boolean;
  playlistPanelOpen: boolean;
  splashDone: boolean;
  updateInfo: UpdateCheckResult | null;

  // 扩展弹窗与交互状态
  activeBackgroundMedia: BackgroundMediaState | null;
  trackDetailOpen: boolean;
  trackDetailSong: SongRecord | null;
  customLyricModalOpen: boolean;
  cuefieldFeedbackOpen: boolean;
  cuefieldLastTransition: Record<string, unknown> | null;
  gestureEnabled: boolean;
  gestureStatus: string;
  gestureHint: string;
  hotkeyBindings: HotkeyBinding[];
  fxArchiveSlots: FxArchiveSlot[];
  controlsHidden: boolean;
  immersiveMode: boolean;
  shelfExpandedPlaylistId: string | null;
  shelfFocusedIndex: number;

  // 核心 Actions
  initApp: () => Promise<void>;
  unlockEasterEgg: (password: string) => Promise<EasterEggStatus>;
  resetEasterEgg: () => Promise<EasterEggStatus>;
  searchTracks: (query: string, mode?: SearchMode) => Promise<SongRecord[]>;
  playSong: (songOrIndex: SongRecord | number, queue?: SongRecord[]) => Promise<void>;
  togglePlay: () => Promise<void>;
  nextTrack: () => Promise<void>;
  prevTrack: () => Promise<void>;
  seekTo: (seconds: number) => void;
  setVolume: (volume: number) => void;
  toggleMute: () => void;
  setPlayMode: (mode: PlayMode) => void;
  cyclePlayMode: () => void;
  setQuality: (quality: QualityPreference) => void;
  setAutomixEnabled: (enabled: boolean) => void;
  setLyricOffsetSec: (offsetSec: number) => void;
  toggleDesktopLyrics: (enabled?: boolean) => Promise<void>;
  importLocalFiles: (files?: FileList | File[]) => Promise<SongRecord[]>;
  loadLocalLibrary: () => Promise<SongRecord[]>;
  loadPlaylistTracks: (playlist: PlaylistSummary) => Promise<SongRecord[]>;
  updateFx: (patch: Partial<FxSettings>) => void;
  resetFxDefaults: () => void;
  applyVisualPreset: (presetId: number) => void;
  setSearchMode: (mode: SearchMode) => void;
  setSearchQuery: (query: string) => void;
  setDiyMode: (diy: boolean) => void;
  setShelfOpen: (open: boolean) => void;
  setFxPanelOpen: (open: boolean) => void;
  setPlaylistPanelOpen: (open: boolean) => void;
  setSplashDone: (done: boolean) => void;
  checkUpdates: () => Promise<UpdateCheckResult | null>;
  loadHomeDiscover: () => Promise<DiscoverHomeData | null>;
  loadWeatherRadio: () => Promise<WeatherRadioData | null>;
  refreshHomeDiscover: () => Promise<void>;
  refreshLoginStatuses: () => Promise<void>;
  refreshAllUserPlaylists: () => Promise<PlaylistSummary[]>;
  getShelfPlaylists: () => PlaylistSummary[];

  // 扩展 Actions
  setActiveBackgroundMedia: (media: BackgroundMediaState | null) => void;
  openTrackDetail: (song?: SongRecord) => void;
  closeTrackDetail: () => void;
  setCustomLyricModalOpen: (open: boolean) => void;
  saveCustomLyricForSong: (songKey: string, lrcText: string) => Promise<void>;
  submitCuefieldFeedback: (rating: number) => Promise<void>;
  dismissCuefieldFeedback: () => void;
  setGestureEnabled: (enabled: boolean) => Promise<void>;
  addSearchHistory: (kw: string) => void;
  clearSearchHistory: () => void;
  updateHotkeyBindings: (bindings: HotkeyBinding[]) => Promise<void>;
  loadFxArchiveDefaults: () => Promise<void>;
  applyFxArchiveSlot: (name: string) => void;
  setControlsHidden: (hidden: boolean) => void;
  setImmersiveMode: (enabled: boolean) => void;
  setShelfExpandedPlaylistId: (id: string | null) => void;
  setShelfFocusedIndex: (idx: number) => void;
}
