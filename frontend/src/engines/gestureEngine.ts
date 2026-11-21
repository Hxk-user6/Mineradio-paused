import { desktopWindow } from '../bridge/tauriBridge';

export type GestureSensitivity = 'steady' | 'balanced' | 'quick';

export interface GestureEngineCallbacks {
  onStatusChange?: (status: string, hint?: string) => void;
  onPlayPause?: () => void;
  onNextTrack?: () => void;
  onPrevTrack?: () => void;
  onVolumeChange?: (deltaOrValue: number, isAbsolute?: boolean) => void;
  onRotateVisual?: (rx: number, ry: number) => void;
  onError?: (error: string) => void;
}

interface HandLandmark {
  x: number;
  y: number;
  z?: number;
}

interface MediaPipeHandsInstance {
  setOptions?: (opts: Record<string, unknown>) => void;
  onResults?: (cb: (results: { multiHandLandmarks?: HandLandmark[][] }) => void) => void;
  send?: (input: { image: HTMLVideoElement }) => Promise<void>;
  close?: () => Promise<void> | void;
}

declare global {
  interface Window {
    Hands?: new (config?: { locateFile?: (file: string) => string }) => MediaPipeHandsInstance;
  }
}

const HAND_CONNECTIONS: Array<[number, number]> = [
  [0, 1], [1, 2], [2, 3], [3, 4],
  [0, 5], [5, 6], [6, 7], [7, 8],
  [5, 9], [9, 10], [10, 11], [11, 12],
  [9, 13], [13, 14], [14, 15], [15, 16],
  [13, 17], [17, 18], [18, 19], [19, 20],
  [0, 17],
];

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Number.isFinite(v) ? v : min));
}

function dist(a: HandLandmark | undefined, b: HandLandmark | undefined): number {
  if (!a || !b) return 0;
  return Math.hypot(a.x - b.x, a.y - b.y);
}

export class GestureEngine {
  private active = false;
  private starting = false;
  private startSeq = 0;
  private stream: MediaStream | null = null;
  private video: HTMLVideoElement | null = null;
  private rafId = 0;
  private hands: MediaPipeHandsInstance | null = null;
  private mode: 'mediapipe' | 'motion' = 'motion';
  private sensitivity: GestureSensitivity = 'balanced';
  private overlayEnabled = true;
  private callbacks: GestureEngineCallbacks = {};

  // 状态跟踪
  private smoothLandmarks: HandLandmark[] | null = null;
  private lastPinchActive = false;
  private lastPinchY = 0;
  private lastPalmPos: { x: number; y: number } | null = null;
  private swipeAnchor: { x: number; y: number; time: number } | null = null;
  private lastFistState: 'open' | 'fist' | 'unknown' = 'unknown';
  private fistHoldSince = 0;
  private fistTriggered = false;
  private actionCooldownUntil = 0;
  private inferenceBusy = false;

  // 运动捕捉降级缓冲
  private motionCanvas: HTMLCanvasElement | null = null;
  private motionCtx: CanvasRenderingContext2D | null = null;
  private prevLuma: Float32Array | null = null;
  private prevMotionCentroid: { x: number; y: number } | null = null;
  private motionVolumeAnchorY: number | null = null;

  public setCallbacks(callbacks: GestureEngineCallbacks): void {
    this.callbacks = { ...this.callbacks, ...callbacks };
  }

  public setSensitivity(mode: GestureSensitivity | string): void {
    const normalized = String(mode || '').toLowerCase();
    this.sensitivity =
      normalized === 'steady' || normalized === 'quick' ? normalized : 'balanced';
  }

  public setOverlayEnabled(enabled: boolean): void {
    this.overlayEnabled = Boolean(enabled);
    if (!this.overlayEnabled) {
      this.clearHandCanvas();
    }
  }

  public isActive(): boolean {
    return this.active;
  }

  private getSensitivityProfile() {
    if (this.sensitivity === 'steady') {
      return { holdMs: 750, swipeDist: 0.24, swipeWindowMs: 650, cooldownMs: 1200 };
    }
    if (this.sensitivity === 'quick') {
      return { holdMs: 380, swipeDist: 0.16, swipeWindowMs: 520, cooldownMs: 800 };
    }
    return { holdMs: 540, swipeDist: 0.2, swipeWindowMs: 580, cooldownMs: 1000 };
  }

  private emitStatus(status: string, hint = ''): void {
    this.callbacks.onStatusChange?.(status, hint);
  }

  public async start(): Promise<boolean> {
    if (this.active) return true;
    if (this.starting) return false;
    const seq = ++this.startSeq;
    this.starting = true;
    this.emitStatus('starting', '正在请求摄像头权限…');

    try {
      if (desktopWindow.isDesktop) {
        const perm = await desktopWindow.requestGestureCameraPermission().catch(() => ({
          ok: false,
          granted: false,
        }));
        if (seq !== this.startSeq) return false;
        if (perm && perm.granted === false && perm.ok === false) {
          this.emitStatus('denied', '摄像头权限被拒绝');
          this.callbacks.onError?.('CAMERA_PERMISSION_DENIED');
          return false;
        }
      }

      if (
        typeof navigator === 'undefined' ||
        !navigator.mediaDevices ||
        typeof navigator.mediaDevices.getUserMedia !== 'function'
      ) {
        this.emitStatus('unavailable', '当前环境不支持摄像头访问');
        this.callbacks.onError?.('MEDIA_DEVICES_UNAVAILABLE');
        return false;
      }

      const stream = await navigator.mediaDevices.getUserMedia({
        video: {
          width: { ideal: 640 },
          height: { ideal: 480 },
          facingMode: 'user',
        },
        audio: false,
      });
      if (seq !== this.startSeq) {
        try {
          stream.getTracks().forEach((track) => track.stop());
        } catch {
          // ignore
        }
        return false;
      }

      this.stream = stream;
      if (typeof document !== 'undefined') {
        const video = document.createElement('video');
        video.playsInline = true;
        video.muted = true;
        video.autoplay = true;
        video.srcObject = stream;
        await video.play().catch(() => {});
        if (seq !== this.startSeq) {
          try {
            video.pause();
            video.srcObject = null;
          } catch {
            // ignore
          }
          return false;
        }
        this.video = video;
      }

      const mpLoaded = await this.tryInitMediaPipeHands();
      if (seq !== this.startSeq) {
        if (this.hands) {
          try {
            this.hands.close?.();
          } catch {
            // ignore
          }
          this.hands = null;
        }
        return false;
      }
      this.mode = mpLoaded ? 'mediapipe' : 'motion';
      this.active = true;
      this.emitStatus(
        'active',
        mpLoaded
          ? '手势控制已就绪：握拳播放/暂停 · 挥手切歌 · 捏合调音量'
          : '已启用轻量运动追踪：左右挥手切歌 · 上下移动调音量'
      );
      this.startLoop();
      return true;
    } catch (err) {
      if (seq !== this.startSeq) return false;
      const msg = err instanceof Error ? err.message : String(err);
      this.stop();
      this.emitStatus('error', `无法启动摄像头：${msg}`);
      this.callbacks.onError?.(msg);
      return false;
    } finally {
      if (seq === this.startSeq) {
        this.starting = false;
      }
    }
  }

  public stop(): void {
    this.startSeq++;
    this.active = false;
    this.starting = false;
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.rafId = 0;
    }
    if (this.stream) {
      try {
        this.stream.getTracks().forEach((track) => track.stop());
      } catch {
        // ignore
      }
      this.stream = null;
    }
    if (this.video) {
      try {
        this.video.pause();
        this.video.srcObject = null;
      } catch {
        // ignore
      }
      this.video = null;
    }
    if (this.hands) {
      try {
        this.hands.close?.();
      } catch {
        // ignore
      }
      this.hands = null;
    }
    this.smoothLandmarks = null;
    this.lastPalmPos = null;
    this.swipeAnchor = null;
    this.lastPinchActive = false;
    this.prevLuma = null;
    this.prevMotionCentroid = null;
    this.motionVolumeAnchorY = null;
    this.clearHandCanvas();
    this.emitStatus('off', '');
  }

  private async tryInitMediaPipeHands(): Promise<boolean> {
    if (typeof window === 'undefined') return false;
    if (!window.Hands && typeof document !== 'undefined') {
      await new Promise<void>((resolve) => {
        const existing = document.querySelector('script[data-mineradio-mediapipe="1"]');
        if (existing) {
          resolve();
          return;
        }
        const script = document.createElement('script');
        script.dataset.mineradioMediapipe = '1';
        script.src = '/vendor/mediapipe/hands.js';
        script.async = true;
        const timer = setTimeout(() => resolve(), 2000);
        script.onload = () => {
          clearTimeout(timer);
          resolve();
        };
        script.onerror = () => {
          clearTimeout(timer);
          resolve();
        };
        document.head.appendChild(script);
      });
    }

    if (typeof window.Hands !== 'function') {
      return false;
    }

    try {
      const hands = new window.Hands({
        locateFile: (file: string) => `/vendor/mediapipe/${file}`,
      });
      hands.setOptions?.({
        maxNumHands: 1,
        modelComplexity: 0,
        minDetectionConfidence: 0.65,
        minTrackingConfidence: 0.6,
      });
      hands.onResults?.((results) => {
        const landmarks = results?.multiHandLandmarks?.[0];
        if (landmarks && landmarks.length >= 21) {
          this.handleHandLandmarks(landmarks);
        } else {
          this.smoothLandmarks = null;
          this.lastPalmPos = null;
          this.lastPinchActive = false;
          this.clearHandCanvas();
        }
      });
      this.hands = hands;
      return true;
    } catch {
      this.hands = null;
      return false;
    }
  }

  private startLoop(): void {
    const tick = () => {
      if (!this.active) return;
      this.rafId = requestAnimationFrame(tick);

      if (!this.video || this.video.readyState < 2) return;

      if (this.mode === 'mediapipe' && this.hands?.send) {
        if (!this.inferenceBusy) {
          this.inferenceBusy = true;
          Promise.resolve(this.hands.send({ image: this.video }))
            .catch(() => {
              this.mode = 'motion';
            })
            .finally(() => {
              this.inferenceBusy = false;
            });
        }
      } else {
        this.processMotionFallbackFrame();
      }
    };
    this.rafId = requestAnimationFrame(tick);
  }

  private getHandCanvasContext(): { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | null {
    if (!this.overlayEnabled || typeof document === 'undefined') return null;
    const canvas = document.getElementById('hand-canvas') as HTMLCanvasElement | null;
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    const targetW = Math.max(160, Math.round(rect.width || canvas.width || 320));
    const targetH = Math.max(120, Math.round(rect.height || canvas.height || 240));
    if (canvas.width !== targetW) canvas.width = targetW;
    if (canvas.height !== targetH) canvas.height = targetH;
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    return { canvas, ctx };
  }

  private clearHandCanvas(): void {
    if (typeof document === 'undefined') return;
    const canvas = document.getElementById('hand-canvas') as HTMLCanvasElement | null;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    ctx?.clearRect(0, 0, canvas.width, canvas.height);
  }

  private drawMediaPipeOverlay(lm: HandLandmark[], isPinch: boolean, isFist: boolean): void {
    const target = this.getHandCanvasContext();
    if (!target) return;
    const { canvas, ctx } = target;
    const W = canvas.width;
    const H = canvas.height;
    ctx.clearRect(0, 0, W, H);

    ctx.save();
    ctx.strokeStyle = isPinch ? '#ffd166' : isFist ? '#ff6b9d' : 'rgba(110, 231, 255, 0.85)';
    ctx.lineWidth = 2.2;
    for (const [i, j] of HAND_CONNECTIONS) {
      const a = lm[i];
      const b = lm[j];
      if (!a || !b) continue;
      ctx.beginPath();
      ctx.moveTo((1 - a.x) * W, a.y * H);
      ctx.lineTo((1 - b.x) * W, b.y * H);
      ctx.stroke();
    }

    for (let i = 0; i < lm.length; i++) {
      const p = lm[i];
      ctx.beginPath();
      ctx.arc((1 - p.x) * W, p.y * H, i === 4 || i === 8 ? 5 : 3.2, 0, Math.PI * 2);
      ctx.fillStyle = i === 4 || i === 8 ? '#ffffff' : 'rgba(110, 231, 255, 0.95)';
      ctx.fill();
    }
    ctx.restore();
  }

  private handleHandLandmarks(raw: HandLandmark[]): void {
    const alpha = 0.36;
    if (!this.smoothLandmarks) {
      this.smoothLandmarks = raw.map((p) => ({ x: p.x, y: p.y, z: p.z }));
    } else {
      for (let i = 0; i < raw.length; i++) {
        const prev = this.smoothLandmarks[i];
        prev.x = prev.x * (1 - alpha) + raw[i].x * alpha;
        prev.y = prev.y * (1 - alpha) + raw[i].y * alpha;
      }
    }

    const lm = this.smoothLandmarks;
    const palm = lm[9] || lm[0];
    const span = Math.max(0.05, dist(lm[5], lm[17]));
    const pinchDist = dist(lm[4], lm[8]);
    const isPinch = pinchDist < span * 0.42;

    // 计算手指张开程度判断握拳 / 张掌
    const tips = [8, 12, 16, 20];
    const avgTipDist =
      tips.reduce((sum, idx) => sum + dist(lm[idx], lm[0]), 0) / tips.length;
    const openness = clamp((avgTipDist / span - 0.85) / 1.15, 0, 1);
    const isFist = openness < 0.26 && !isPinch;
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    const profile = this.getSensitivityProfile();

    this.drawMediaPipeOverlay(lm, isPinch, isFist);

    // 1. 捏合上下移动 -> 音量控制
    if (isPinch) {
      const pinchY = (lm[4].y + lm[8].y) * 0.5;
      if (!this.lastPinchActive) {
        this.lastPinchActive = true;
        this.lastPinchY = pinchY;
        this.emitStatus('active', '捏合中：上下移动调节音量');
      } else {
        const dy = this.lastPinchY - pinchY;
        if (Math.abs(dy) > 0.008) {
          this.callbacks.onVolumeChange?.(dy * 1.4, false);
          this.lastPinchY = pinchY;
        }
      }
      this.lastPalmPos = { x: palm.x, y: palm.y };
      return;
    }
    this.lastPinchActive = false;

    // 2. 握拳保持 -> 播放 / 暂停
    if (isFist) {
      if (this.lastFistState !== 'fist') {
        this.lastFistState = 'fist';
        this.fistHoldSince = now;
        this.fistTriggered = false;
      } else if (
        !this.fistTriggered &&
        now - this.fistHoldSince >= profile.holdMs &&
        now >= this.actionCooldownUntil
      ) {
        this.fistTriggered = true;
        this.actionCooldownUntil = now + profile.cooldownMs;
        this.callbacks.onPlayPause?.();
        this.emitStatus('active', '识别到握拳：切换播放 / 暂停');
      }
      return;
    }

    if (this.lastFistState === 'fist' && openness > 0.68 && !this.fistTriggered && now >= this.actionCooldownUntil) {
      // 快速握拳后张开也可触发一次播放切换
      this.actionCooldownUntil = now + profile.cooldownMs;
      this.callbacks.onPlayPause?.();
      this.emitStatus('active', '识别到张掌：切换播放 / 暂停');
    }
    this.lastFistState = openness > 0.62 ? 'open' : 'unknown';

    // 3. 张掌左右挥手 -> 上一首 / 下一首
    if (openness >= 0.62 && now >= this.actionCooldownUntil) {
      if (!this.swipeAnchor || now - this.swipeAnchor.time > profile.swipeWindowMs) {
        this.swipeAnchor = { x: palm.x, y: palm.y, time: now };
      } else {
        const dx = palm.x - this.swipeAnchor.x;
        const dy = palm.y - this.swipeAnchor.y;
        if (Math.abs(dx) >= profile.swipeDist && Math.abs(dx) > Math.abs(dy) * 1.5) {
          this.actionCooldownUntil = now + profile.cooldownMs;
          this.swipeAnchor = null;
          // 镜像视角：x 增大在屏幕上向左挥或向右挥
          if (dx > 0) {
            this.callbacks.onPrevTrack?.();
            this.emitStatus('active', '手势右挥：上一首');
          } else {
            this.callbacks.onNextTrack?.();
            this.emitStatus('active', '手势左挥：下一首');
          }
          return;
        }
      }
    } else {
      this.swipeAnchor = null;
    }

    // 4. 手掌平移 -> 旋转 3D 视觉粒子视角
    if (this.lastPalmPos) {
      const dx = palm.x - this.lastPalmPos.x;
      const dy = palm.y - this.lastPalmPos.y;
      if (Math.abs(dx) > 0.002 || Math.abs(dy) > 0.002) {
        const ry = -dx * 3.8;
        const rx = dy * 3.2;
        this.callbacks.onRotateVisual?.(rx, ry);
      }
    }
    this.lastPalmPos = { x: palm.x, y: palm.y };
  }

  private processMotionFallbackFrame(): void {
    if (!this.video || typeof document === 'undefined') return;
    const W = 64;
    const H = 48;
    if (!this.motionCanvas) {
      this.motionCanvas = document.createElement('canvas');
      this.motionCanvas.width = W;
      this.motionCanvas.height = H;
      this.motionCtx = this.motionCanvas.getContext('2d', { willReadFrequently: true });
    }
    if (!this.motionCtx) return;

    this.motionCtx.drawImage(this.video, 0, 0, W, H);
    const frame = this.motionCtx.getImageData(0, 0, W, H).data;
    const luma = new Float32Array(W * H);

    let sumMotion = 0;
    let sumX = 0;
    let sumY = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const i = y * W + x;
        const p = i * 4;
        const lum = (0.299 * frame[p] + 0.587 * frame[p + 1] + 0.114 * frame[p + 2]) / 255;
        luma[i] = lum;
        if (this.prevLuma) {
          const diff = Math.abs(lum - this.prevLuma[i]);
          if (diff > 0.14) {
            sumMotion += diff;
            sumX += (x / W) * diff;
            sumY += (y / H) * diff;
          }
        }
      }
    }
    this.prevLuma = luma;

    const target = this.getHandCanvasContext();
    if (target) {
      target.ctx.clearRect(0, 0, target.canvas.width, target.canvas.height);
    }

    if (sumMotion < 6) {
      this.prevMotionCentroid = null;
      this.motionVolumeAnchorY = null;
      return;
    }

    const cx = sumX / sumMotion;
    const cy = sumY / sumMotion;
    const now = typeof performance !== 'undefined' ? performance.now() : Date.now();
    const profile = this.getSensitivityProfile();

    if (target) {
      const { canvas, ctx } = target;
      ctx.save();
      ctx.strokeStyle = 'rgba(110, 231, 255, 0.85)';
      ctx.lineWidth = 2.5;
      ctx.beginPath();
      ctx.arc((1 - cx) * canvas.width, cy * canvas.height, 18, 0, Math.PI * 2);
      ctx.stroke();
      ctx.restore();
    }

    if (this.prevMotionCentroid) {
      const dx = cx - this.prevMotionCentroid.x;
      const dy = cy - this.prevMotionCentroid.y;

      this.callbacks.onRotateVisual?.(dy * 2.4, -dx * 2.8);

      if (now >= this.actionCooldownUntil) {
        if (Math.abs(dx) > profile.swipeDist * 0.65 && Math.abs(dx) > Math.abs(dy) * 1.5) {
          this.actionCooldownUntil = now + profile.cooldownMs;
          if (dx > 0) {
            this.callbacks.onPrevTrack?.();
            this.emitStatus('active', '检测到右挥：上一首');
          } else {
            this.callbacks.onNextTrack?.();
            this.emitStatus('active', '检测到左挥：下一首');
          }
        } else if (sumMotion > 110) {
          this.actionCooldownUntil = now + profile.cooldownMs;
          this.callbacks.onPlayPause?.();
          this.emitStatus('active', '检测到手势动作：切换播放 / 暂停');
        }
      }

      if (this.motionVolumeAnchorY != null && Math.abs(dy) > 0.015 && Math.abs(dy) > Math.abs(dx) * 1.5) {
        this.callbacks.onVolumeChange?.(-dy * 1.2, false);
      }
      this.motionVolumeAnchorY = cy;
    }

    this.prevMotionCentroid = { x: cx, y: cy };
  }
}

export const gestureEngine = new GestureEngine();
