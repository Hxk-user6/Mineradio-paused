import React, { useCallback, useEffect, useRef, useState } from 'react';
import { usePlayerStore } from '../store/playerStore';

export const FAST_SKIP_LS_KEY = 'mineradio-startup-fast-skip-v1';

export function SplashScreen(): React.ReactElement | null {
  const splashDone = usePlayerStore((s) => s.splashDone);
  const setSplashDone = usePlayerStore((s) => s.setSplashDone);

  const [ready, setReady] = useState(false);
  const [exiting, setExiting] = useState(false);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const exitTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    try {
      if (window.localStorage?.getItem(FAST_SKIP_LS_KEY) === '1') {
        setSplashDone(true);
        return;
      }
    } catch {
      // ignore storage errors
    }
    const timer = setTimeout(() => {
      setReady(true);
    }, 240);
    return () => clearTimeout(timer);
  }, [setSplashDone]);

  useEffect(() => {
    if (splashDone) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    let ctx: CanvasRenderingContext2D | null = null;
    try {
      ctx = canvas.getContext('2d');
    } catch {
      ctx = null;
    }
    if (!ctx) return;

    let rafId = 0;
    let disposed = false;

    const resize = () => {
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      canvas.width = Math.max(1, Math.floor(window.innerWidth * dpr));
      canvas.height = Math.max(1, Math.floor(window.innerHeight * dpr));
      ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
    };
    resize();
    window.addEventListener('resize', resize);

    const start = performance.now();
    const draw = (now: number) => {
      if (disposed || !ctx) return;
      const t = (now - start) * 0.001;
      const w = window.innerWidth;
      const h = window.innerHeight;
      ctx.clearRect(0, 0, w, h);

      const cx = w * 0.5;
      const cy = h * 0.5;
      const baseR = Math.min(w, h) * 0.24;
      for (let i = 0; i < 3; i++) {
        const phase = (t * 0.35 + i * 0.33) % 1;
        const r = baseR * (0.6 + phase * 1.15);
        const alpha = (1 - phase) * 0.09;
        ctx.beginPath();
        ctx.arc(cx, cy, r, 0, Math.PI * 2);
        ctx.strokeStyle =
          i % 2 === 0
            ? `rgba(244, 210, 138, ${alpha.toFixed(3)})`
            : `rgba(126, 200, 216, ${alpha.toFixed(3)})`;
        ctx.lineWidth = 1.2;
        ctx.stroke();
      }

      rafId = requestAnimationFrame(draw);
    };
    rafId = requestAnimationFrame(draw);

    return () => {
      disposed = true;
      cancelAnimationFrame(rafId);
      window.removeEventListener('resize', resize);
    };
  }, [splashDone]);

  useEffect(() => {
    return () => {
      if (exitTimerRef.current) clearTimeout(exitTimerRef.current);
    };
  }, []);

  const handleEnter = useCallback(() => {
    if (exiting || splashDone) return;
    setExiting(true);
    exitTimerRef.current = setTimeout(() => {
      setSplashDone(true);
    }, 360);
  }, [exiting, splashDone, setSplashDone]);

  if (splashDone) return null;

  return (
    <div
      id="splash"
      className={`${ready ? 'ready' : ''}${exiting ? ' exiting hide' : ''}`.trim()}
      role="button"
      tabIndex={0}
      aria-label="MineradioX 启动页，点击进入"
      onClick={handleEnter}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          handleEnter();
        }
      }}
    >
      <canvas id="splash-canvas" ref={canvasRef} />
      <div className="splash-bg-noise" />
      <div className="splash-content">
        <div className="splash-wordmark" id="splash-wordmark" aria-label="MineradioX">
          <span className="splash-word-mine">Mine</span>
          <span className="splash-word-radio" aria-label="radioX">
            rad
            <span className="splash-word-i" aria-hidden="true" />
            <span className="splash-word-o">o</span>
            <span className="splash-word-x">X</span>
          </span>
        </div>
        <div className="splash-signal-line" />
        <div className="splash-sub">private visual radio</div>
        <div className="splash-enter" aria-hidden="true">
          点击进入
        </div>
      </div>
    </div>
  );
}

export default SplashScreen;
