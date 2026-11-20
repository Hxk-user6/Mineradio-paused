import * as THREE from 'three';
import { coverProxyUrl, resolveMediaUrl } from '../bridge/tauriBridge';
import {
  DEFAULT_FX_SETTINGS,
  type AudioReactiveMetrics,
  type BeatMapData,
  type FxSettings,
  type LyricLine,
  type ShelfItem,
  type SongRecord,
} from '../types/mineradio';
import { audioEngine } from './audioEngine';

interface ExternalPresetPlugin {
  INDEX?: number;
  isActive?: (fx: FxSettings) => boolean;
  update?: (dt: number, ctx: Record<string, unknown>) => void;
  clear?: () => void;
  onPresetChange?: (prev: number, next: number, ctx?: Record<string, unknown>) => void;
  pointerRipple?: (x: number, z: number, strength?: number) => void;
  pushProperties?: (force?: boolean) => void;
}

declare global {
  interface Window {
    THREE?: typeof THREE;
    MineradioSonicTopography?: ExternalPresetPlugin;
    MineradioSonicWorkshop?: ExternalPresetPlugin;
    fx?: FxSettings;
    playing?: boolean;
    audio?: HTMLAudioElement | null;
    frequencyData?: Uint8Array;
    playQueue?: SongRecord[];
    currentIdx?: number;
    stageLyrics?: {
      coverPalette?: { primary: string; secondary: string; highlight: string };
      palette?: { primary: string; secondary: string; highlight: string };
    };
    orbit?: { theta: number; phi: number; radius: number };
  }
}

export interface VisualEngineCallbacks {
  onShelfSelect?: (item: ShelfItem, index: number) => void;
  onShelfHover?: (item: ShelfItem | null, index: number) => void;
  onShelfCenterChange?: (item: ShelfItem | null, index: number) => void;
  onShelfFocusRequest?: (item: ShelfItem, index: number) => void;
  onFreeCameraChange?: (active: boolean) => void;
}

interface ShelfCardSlot {
  mesh: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  texture: THREE.CanvasTexture;
  item: ShelfItem | null;
  itemIndex: number;
  drawKey: string;
}

const PLANE_SIZE = 4.8;
const RIPPLE_MAX = 12;
const SKULL_PRESET_INDEX = 6;
const BACKGROUND_STAR_RIVER_COUNT = 1400;
const SHELF_VISIBLE_RADIUS = 5;
const SHELF_MAX_RENDER = SHELF_VISIBLE_RADIUS * 2 + 1;
const MAX_NEW_LYRIC_TEXTURES_PER_FRAME = 1;

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Number.isFinite(v) ? v : min));
}

function coverParticleGridForResolution(res: number): number {
  const n = clamp(Number(res) || 1.0, 0.5, 2.0);
  return Math.max(48, Math.min(220, Math.round(n * 118)));
}

function makeDotTexture(): THREE.CanvasTexture {
  const cv = document.createElement('canvas');
  cv.width = 64;
  cv.height = 64;
  const ctx = cv.getContext('2d');
  if (ctx) {
    const g = ctx.createRadialGradient(32, 32, 0, 32, 32, 31);
    g.addColorStop(0.0, 'rgba(255,255,255,0.96)');
    g.addColorStop(0.42, 'rgba(255,255,255,0.78)');
    g.addColorStop(0.72, 'rgba(255,255,255,0.22)');
    g.addColorStop(1.0, 'rgba(255,255,255,0)');
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, 64, 64);
  }
  const tex = new THREE.CanvasTexture(cv);
  tex.minFilter = THREE.LinearFilter;
  tex.magFilter = THREE.LinearFilter;
  return tex;
}

function makeSolidCanvasTexture(color: string): THREE.CanvasTexture {
  const cv = document.createElement('canvas');
  cv.width = 4;
  cv.height = 4;
  const ctx = cv.getContext('2d');
  if (ctx) {
    ctx.fillStyle = color;
    ctx.fillRect(0, 0, 4, 4);
  }
  const tex = new THREE.CanvasTexture(cv);
  tex.minFilter = THREE.LinearFilter;
  tex.magFilter = THREE.LinearFilter;
  tex.needsUpdate = true;
  return tex;
}

function buildCoverParticleGeometry(grid: number): THREE.BufferGeometry {
  const count = grid * grid;
  const geo = new THREE.BufferGeometry();
  const positions = new Float32Array(count * 3);
  const uvs = new Float32Array(count * 2);
  const rand = new Float32Array(count);
  const step = 1 / grid;
  for (let i = 0; i < count; i++) {
    const gx = i % grid;
    const gy = Math.floor(i / grid);
    const u = (gx + 0.5) * step;
    const v = (gy + 0.5) * step;
    const px = gx / Math.max(1, grid - 1);
    const py = gy / Math.max(1, grid - 1);
    positions[i * 3] = (px - 0.5) * PLANE_SIZE;
    positions[i * 3 + 1] = (py - 0.5) * PLANE_SIZE;
    positions[i * 3 + 2] = 0;
    uvs[i * 2] = u;
    uvs[i * 2 + 1] = v;
    rand[i] = Math.random();
  }
  geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geo.setAttribute('aUv', new THREE.BufferAttribute(uvs, 2));
  geo.setAttribute('aRand', new THREE.BufferAttribute(rand, 1));
  geo.userData.grid = grid;
  return geo;
}

function buildBackgroundStarRiverGeometry(count: number): THREE.BufferGeometry {
  const geo = new THREE.BufferGeometry();
  const seeds = new Float32Array(count);
  const lanes = new Float32Array(count);
  const depths = new Float32Array(count);
  const positions = new Float32Array(count * 3);
  for (let i = 0; i < count; i++) {
    seeds[i] = Math.random() * 1000 + i * 0.37;
    lanes[i] = Math.random();
    depths[i] = Math.random();
  }
  geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geo.setAttribute('aSeed', new THREE.BufferAttribute(seeds, 1));
  geo.setAttribute('aLane', new THREE.BufferAttribute(lanes, 1));
  geo.setAttribute('aDepthSeed', new THREE.BufferAttribute(depths, 1));
  return geo;
}

const PARTICLE_VS = `
precision highp float;
uniform float uTime, uBass, uMid, uTreble, uBeat, uEnergy, uBurstAmt;
uniform float uPreset, uIntensity, uDepth, uPointScale, uSpeed, uTwist;
uniform float uVinylSpin;
uniform float uColorBoost, uScatter, uCoverRes, uBgFade;
uniform float uHasCover, uHasDepth, uEdgeEnabled, uAiBoost;
uniform float uMouseActive, uPixel, uColorMixT, uLoading;
uniform sampler2D uCoverTex, uPrevCoverTex, uEdgeTex, uRippleTex;
uniform int uRippleCount;
uniform vec2 uMouseXY;
uniform vec3 uTintColor;
uniform float uTintStrength;
attribute vec2 aUv;
attribute float aRand;
varying vec3 vColor;
varying float vBright, vRipple, vEdgeBoost, vAlpha, vSourceLum;

#define PI 3.14159265359

float hash11(float p){ return fract(sin(p * 127.1) * 43758.5453123); }

vec3 mod289(vec3 x){return x-floor(x*(1.0/289.0))*289.0;}
vec4 mod289v(vec4 x){return x-floor(x*(1.0/289.0))*289.0;}
vec4 perm(vec4 x){return mod289v(((x*34.0)+1.0)*x);}
float snoise(vec3 v){
  const vec2 C=vec2(1.0/6.0,1.0/3.0);
  const vec4 D=vec4(0.0,0.5,1.0,2.0);
  vec3 i=floor(v+dot(v,C.yyy));
  vec3 x0=v-i+dot(i,C.xxx);
  vec3 g=step(x0.yzx,x0.xyz); vec3 l=1.0-g;
  vec3 i1=min(g.xyz,l.zxy); vec3 i2=max(g.xyz,l.zxy);
  vec3 x1=x0-i1+C.xxx;
  vec3 x2=x0-i2+C.yyy;
  vec3 x3=x0-D.yyy;
  i=mod289(i);
  vec4 p=perm(perm(perm(i.z+vec4(0.0,i1.z,i2.z,1.0))+i.y+vec4(0.0,i1.y,i2.y,1.0))+i.x+vec4(0.0,i1.x,i2.x,1.0));
  float n_=0.142857142857;
  vec3 ns=n_*D.wyz-D.xzx;
  vec4 j=p-49.0*floor(p*ns.z*ns.z);
  vec4 x_=floor(j*ns.z); vec4 y_=floor(j-7.0*x_);
  vec4 x=x_*ns.x+ns.yyyy; vec4 y=y_*ns.x+ns.yyyy;
  vec4 h=1.0-abs(x)-abs(y);
  vec4 b0=vec4(x.xy,y.xy); vec4 b1=vec4(x.zw,y.zw);
  vec4 s0=floor(b0)*2.0+1.0; vec4 s1=floor(b1)*2.0+1.0;
  vec4 sh=-step(h,vec4(0.0));
  vec4 a0=b0.xzyw+s0.xzyw*sh.xxyy;
  vec4 a1=b1.xzyw+s1.xzyw*sh.zzww;
  vec3 p0=vec3(a0.xy,h.x); vec3 p1=vec3(a0.zw,h.y);
  vec3 p2=vec3(a1.xy,h.z); vec3 p3=vec3(a1.zw,h.w);
  vec4 norm=1.79284291400159-0.85373472095314*vec4(dot(p0,p0),dot(p1,p1),dot(p2,p2),dot(p3,p3));
  p0*=norm.x; p1*=norm.y; p2*=norm.z; p3*=norm.w;
  vec4 m=max(0.6-vec4(dot(x0,x0),dot(x1,x1),dot(x2,x2),dot(x3,x3)),0.0);
  m=m*m;
  return 42.0*dot(m*m,vec4(dot(p0,x0),dot(p1,x1),dot(p2,x2),dot(p3,x3)));
}

vec2 safeCoverUv(vec2 uv){ return clamp(uv, vec2(0.002), vec2(0.998)); }
vec3 sampleNewCoverColor(vec2 uv){ return texture2D(uCoverTex, safeCoverUv(uv)).rgb; }
vec3 samplePrevCoverColor(vec2 uv){ return texture2D(uPrevCoverTex, safeCoverUv(uv)).rgb; }

float rippleSumAt(vec2 p, out float maxAmp){
  float sum = 0.0;
  maxAmp = 0.0;
  for (int i = 0; i < 12; i++) {
    if (i >= uRippleCount) break;
    vec4 rp = texture2D(uRippleTex, vec2(0.5, (float(i) + 0.5) / 12.0));
    float age = rp.z;
    float str = rp.w;
    if (age < 0.0 || age > 3.2 || str <= 0.001) continue;
    float d = length(p - rp.xy);
    float radius = age * 2.1;
    float ring = exp(-pow((d - radius) / 0.42, 2.0));
    float wave = sin((d - radius) * 14.0) * ring * str * exp(-age * 1.15);
    sum += wave;
    maxAmp = max(maxAmp, ring * str * exp(-age * 1.15));
  }
  return sum;
}

void main(){
  float t = uTime * max(0.05, uSpeed);
  float K = max(0.0, uIntensity);
  vec2 sampleUv = safeCoverUv(aUv);
  vec3 newCol = sampleNewCoverColor(sampleUv);
  vec3 prevCol = samplePrevCoverColor(sampleUv);
  vec3 coverColor = mix(prevCol, newCol, clamp(uColorMixT, 0.0, 1.0));
  vec3 defaultColor = mix(vec3(0.40, 0.72, 0.96), vec3(0.94, 0.78, 0.62), aUv.y);
  vColor = mix(defaultColor, coverColor, uHasCover);
  vec4 edgeData = texture2D(uEdgeTex, sampleUv);
  float depthVal = edgeData.r;
  float edgeVal = edgeData.g;
  float fgMask = edgeData.b;
  vAlpha = 1.0;
  float maxRippleAmp = 0.0;
  vec3 pos = position;

  if (uPreset < 0.5) {
    pos = position;
    float rippleZ = rippleSumAt(pos.xy, maxRippleAmp);
    float midN = snoise(vec3(pos.x*1.4, pos.y*1.4, t*0.55)) * 0.6
               + snoise(vec3(pos.x*2.8+5.0, pos.y*2.8-3.0, t*0.85)) * 0.4;
    float midMask = 0.55 + 0.45 * snoise(vec3(pos.x*0.4, pos.y*0.4, t*0.18));
    float midDisp = midN * uMid * 0.55 * midMask * K;
    float trebleJ = snoise(vec3(pos.x*6.5, pos.y*6.5, t*3.5 + aRand*4.0)) * uTreble * 0.18 * K;
    float bassBreath = snoise(vec3(pos.x*0.35, pos.y*0.35, t*0.4)) * uBass * 0.42 * K;
    float depthZ = (depthVal - 0.5) * uAiBoost * uDepth * 1.40 * uHasDepth;
    pos.z = rippleZ * 1.30 + midDisp + trebleJ + bassBreath + depthZ;
  } else if (uPreset < 1.5) {
    float spin = t * 0.12;
    float angle = aUv.x * 2.0 * PI + spin;
    float flow = fract(aUv.y - t * 0.08 * (1.0 + uBass * 0.55));
    float zPos = (flow - 0.5) * 9.0;
    float baseR = 2.0 - uBass * 0.28 * K;
    float ripG = sin(angle * 5.0 + zPos * 1.4 + t * 2.2) * 0.10 * (uMid + uTreble) * K;
    float r = baseR + ripG;
    pos.x = cos(angle) * r;
    pos.y = sin(angle) * r;
    pos.z = zPos;
    sampleUv = safeCoverUv(vec2(aUv.x, flow));
    coverColor = mix(samplePrevCoverColor(sampleUv), sampleNewCoverColor(sampleUv), clamp(uColorMixT, 0.0, 1.0));
    vColor = mix(defaultColor, coverColor, uHasCover) * (0.4 + smoothstep(-4.5, 4.5, zPos) * 0.7);
  } else if (uPreset < 2.5) {
    float theta = aUv.x * 2.0 * PI;
    float phi = (aUv.y - 0.5) * PI;
    float r = 2.2 * (1.0 + uBass * 0.35 * K) + snoise(vec3(theta * 1.5, phi * 1.5, t * 0.7)) * uTreble * 0.85 * K;
    pos.x = r * cos(phi) * cos(theta);
    pos.y = r * sin(phi);
    pos.z = r * cos(phi) * sin(theta);
    float yaw = t * 0.18;
    float cy = cos(yaw), sy = sin(yaw);
    pos.xz = mat2(cy, -sy, sy, cy) * pos.xz;
  } else if (uPreset < 3.5) {
    pos = vec3((aUv.x - 0.5) * 0.01, (aUv.y - 0.5) * 0.01, -90.0);
    vAlpha = 0.0;
    vColor = vec3(0.0);
  } else if (uPreset < 4.5) {
    float bassDrive = smoothstep(0.08, 0.78, uBass + uBeat * 0.82);
    vec2 p = (aUv - 0.5) * 5.12;
    float cs = cos(uVinylSpin), sn = sin(uVinylSpin);
    vec2 rp = mat2(cs, -sn, sn, cs) * p;
    float d = length(p);
    float recordR = 2.46;
    float coverR = 1.18;
    float recordAlpha = 1.0 - smoothstep(recordR - 0.02, recordR + 0.05, d);
    float coverMask = 1.0 - smoothstep(coverR - 0.012, coverR + 0.018, d);
    float border = exp(-pow((d - coverR) / 0.064, 2.0));
    pos = vec3(rp * (1.0 + bassDrive * 0.012 + uBeat * 0.026), 0.0);
    vAlpha = recordAlpha;
    if (coverMask > 0.02) {
      vec2 coverUv = p / (coverR * 2.0) + 0.5;
      coverColor = mix(samplePrevCoverColor(coverUv), sampleNewCoverColor(coverUv), clamp(uColorMixT, 0.0, 1.0));
      vColor = mix(mix(defaultColor, coverColor, uHasCover), vec3(1.0), border * 0.54);
      pos.z = 0.04 + border * 0.026 + uBeat * 0.018;
      maxRippleAmp = border * 0.30 + uBeat * 0.08;
    } else {
      float groove = 0.5 + 0.5 * sin((d - coverR) * 72.0);
      vColor = mix(vec3(0.052, 0.054, 0.058) + vec3(0.045) * groove, vec3(0.92, 0.94, 0.94), border * 0.88);
      pos.z = groove * 0.01 + border * 0.024;
      maxRippleAmp = border * 0.30;
    }
  } else if (uPreset < 8.5) {
    float bassGlow = smoothstep(0.07, 0.78, uBass) * 0.34 + uBeat * 0.014;
    float lane = aUv.y;
    float bandN = clamp(lane, 0.0, 1.0);
    float flow = fract(aUv.x + t * (0.004 + bandN * 0.004));
    float arc = (flow - 0.5) * PI * 1.8;
    float r = 9.0 + bandN * 12.0;
    pos.x = cos(arc * 0.75) * r + (flow - 0.5) * 14.0;
    pos.y = (bandN - 0.5) * 12.0 + sin(arc + t * 0.12) * 2.2;
    pos.z = mix(-20.0, 10.0, bandN) + cos(arc + t * 0.1) * 1.8;
    vec3 aurora = mix(vec3(0.52, 0.86, 1.0), vec3(0.70, 0.58, 1.0), bandN);
    vColor = mix(coverColor, aurora, 0.62) * (0.85 + bassGlow * 0.35);
    vAlpha = 0.55;
    if (uPreset > 5.5 && uPreset < 6.5) {
      vAlpha = 0.0;
    }
  } else if (uPreset < 9.5) {
    float ringIndex = floor(aUv.y * 8.0);
    float ringLocal = fract(aUv.y * 8.0);
    float ringN = (ringIndex + 0.5) / 8.0;
    float theta = aUv.x * 2.0 * PI + ringIndex * 0.47 + t * (0.035 + ringN * 0.026);
    float radius = 0.92 + ringN * 3.18 + (ringLocal - 0.5) * 0.24 + (uBass * 0.17 + uBeat * 0.08) * (0.18 + ringN * 0.34);
    float yh = sin(theta) * radius * (0.49 + ringN * 0.20);
    float zh = (ringN - 0.5) * 1.34;
    float tilt = -0.34 + ringN * 0.72;
    pos = vec3(cos(theta) * radius * 1.2, yh * cos(tilt) - zh * sin(tilt), yh * sin(tilt) + zh * cos(tilt));
    vec3 haloColor = mix(vec3(0.48, 0.82, 1.0), vec3(0.93, 0.72, 0.38), smoothstep(0.18, 0.84, ringN));
    vColor = mix(coverColor * 0.72, haloColor, 0.66);
    vAlpha = 0.72;
  } else if (uPreset < 10.5) {
    float colIdx = floor(aUv.x * 52.0);
    float colN = (colIdx + 0.5) / 52.0;
    float seed = hash11(colIdx * 41.17);
    float fall = fract(1.0 - aUv.y + t * (0.03 + seed * 0.05) + seed * 0.87);
    pos = vec3((colN - 0.5) * 11.8, (0.5 - fall) * 8.8, mix(-3.6, 2.4, seed));
    vec3 neon = mix(vec3(0.20, 0.88, 1.0), vec3(1.0, 0.26, 0.63), seed);
    vColor = mix(coverColor * 0.82, neon, 0.62);
    vAlpha = 0.56;
  } else if (uPreset < 11.5) {
    float flockIndex = floor(aUv.y * 12.0);
    float wingY = fract(aUv.y * 12.0) * 2.0 - 1.0;
    float wingX = aUv.x * 2.0 - 1.0;
    float gx = mod(flockIndex, 4.0) - 1.5;
    float gy = floor(flockIndex / 4.0) - 1.0;
    float flap = sin(t * 0.85 + flockIndex * 1.7) * (0.34 + uMid * 0.42 + uBeat * 0.16);
    pos = vec3(gx * 2.4 + wingX, gy * 2.1 + wingY * 0.5, abs(wingX) * (0.42 + flap));
    vColor = mix(coverColor * 0.78, mix(vec3(0.42, 0.94, 0.82), vec3(0.96, 0.62, 0.92), aUv.x), 0.64);
    vAlpha = 0.62;
  } else {
    float bloomR = pow(aUv.y, 0.72);
    float bloomTheta = aUv.x * 2.0 * PI + bloomR * 0.72 + t * 0.018;
    float petalWave = 0.5 + 0.5 * cos(bloomTheta * 9.0 - bloomR * 3.8);
    float radiusBloom = bloomR * (3.30 + uBass * 0.26 + uBeat * 0.10) * (0.78 + petalWave * 0.34);
    pos = vec3(cos(bloomTheta) * radiusBloom, sin(bloomTheta) * radiusBloom * 0.82, (1.0 - bloomR) * 1.12 - bloomR * bloomR * 0.52);
    vColor = mix(coverColor * 0.72, mix(vec3(0.12, 0.58, 0.56), vec3(0.38, 0.32, 0.95), bloomR), 0.68);
    vAlpha = 0.68 * smoothstep(0.01, 0.10, bloomR);
  }

  if (uMouseActive > 0.5 && uPreset < 0.5) {
    float md = length(pos.xy - uMouseXY);
    if (md < 1.0) {
      float push = (1.0 - md) * (1.0 - md);
      pos.z += push * 0.55;
    }
  }

  if (uScatter > 0.001) {
    vec2 jdir = vec2(cos(aRand * 6.2831), sin(aRand * 6.2831));
    pos.xy += jdir * uScatter * (0.05 + uTreble * 0.10);
  }
  if (uTwist > 0.001 && uPreset < 0.5) {
    float ta = uTwist * pos.z * 0.6;
    float cs = cos(ta), sn = sin(ta);
    pos.xy = mat2(cs, -sn, sn, cs) * pos.xy;
  }

  vSourceLum = dot(max(vColor, vec3(0.0)), vec3(0.299, 0.587, 0.114));
  float blackGuard = 1.0 - smoothstep(0.025, 0.115, vSourceLum);
  float edgeBoost = uEdgeEnabled * edgeVal;
  vEdgeBoost = edgeBoost * (1.0 - blackGuard);
  vColor = pow(max(vColor, vec3(0.0)), vec3(1.0 / max(0.35, uColorBoost)));
  vec3 tintedColor = uTintColor * max(0.24, max(max(vColor.r, vColor.g), vColor.b) * 1.12);
  vColor = mix(vColor, tintedColor, clamp(uTintStrength, 0.0, 1.0) * (1.0 - blackGuard));

  vBright = 0.82 + maxRippleAmp * 0.55 + uBass * 0.10 + edgeBoost * 0.30 + uEnergy * 0.05 + uBurstAmt * 0.40;
  vRipple = clamp(maxRippleAmp * 1.5, 0.0, 1.0);
  if (uHasDepth > 0.5 && uPreset < 0.5) {
    vBright *= mix(1.0, 0.55, uBgFade * (1.0 - fgMask));
  }

  vec4 mvPos = modelViewMatrix * vec4(pos, 1.0);
  float depthSize = 36.0 / max(0.5, -mvPos.z);
  float sz = clamp(depthSize * (1.0 + maxRippleAmp * 0.7 + edgeBoost * 0.55 + uBeat * 0.30 + uBurstAmt * 0.5), 1.05, 4.95);
  gl_PointSize = sz * uPixel * uPointScale;
  gl_Position = projectionMatrix * mvPos;
}
`;

const PARTICLE_FS = `
precision highp float;
uniform sampler2D uDotTex;
uniform float uAlpha, uPreset, uParticleDim, uBackdropAdapt;
varying vec3 vColor;
varying float vBright, vRipple, vEdgeBoost, vAlpha, vSourceLum;

void main(){
  vec4 tex = texture2D(uDotTex, gl_PointCoord);
  if (tex.a < 0.02) discard;
  vec3 col = vColor * vBright;
  col = mix(col, col * 1.3 + vec3(0.05), vEdgeBoost * 0.35);
  col = mix(col, col * 1.2, vRipple * 0.4);
  col = clamp(col, vec3(0.0), vec3(1.6));
  gl_FragColor = vec4(col, tex.a * uAlpha * uParticleDim * vAlpha);
}
`;

const SKULL_VS = `
precision highp float;
attribute float seed, kind;
uniform float uTime, uPixel, uPointScale, uBloomStrength, uColorBoost;
uniform float uBass, uMid, uTreble, uBeat, uJawOpen, uSkullFlash;
varying float vKind, vLight, vRim, vAmp, vDensity, vFlash;
void main(){
  vec3 pos = position;
  float jawGroup = step(1.0, kind);
  float boneKind = fract(kind);
  vKind = boneKind;
  vec3 n = normalize(vec3(position.x * 0.82, position.y * 0.68, position.z * 1.22 + 0.16));
  vec2 jawHinge = vec2(-0.45, 0.18);
  float jawAngle = uJawOpen * 0.52 * jawGroup;
  float jc = cos(jawAngle), js = sin(jawAngle);
  vec2 jr = pos.yz - jawHinge;
  vec2 openedJaw = vec2(jr.x * jc - jr.y * js, jr.x * js + jr.y * jc) + jawHinge;
  pos.yz = mix(pos.yz, openedJaw, jawGroup);
  float ampDrive = smoothstep(0.20, 0.82, uBass * 0.44 + uMid * 0.22 + uBeat * 0.72);
  vFlash = clamp(uSkullFlash, 0.0, 1.0);
  vAmp = clamp(ampDrive * 0.045 + vFlash * 0.92 + uTreble * 0.012, 0.0, 1.0);
  vec4 mv = modelViewMatrix * vec4(pos, 1.0);
  float dist = max(0.55, -mv.z);
  vec3 vn = normalize(normalMatrix * n);
  float key = pow(max(dot(vn, normalize(vec3(-0.48, 0.64, 0.60))), 0.0), 1.18);
  vRim = pow(max(dot(vn, normalize(vec3(0.88, 0.18, -0.44))), 0.0), 2.50) * (0.24 + vFlash * 0.62);
  vDensity = clamp(0.30 + key * 0.70 + vRim * 0.24 + vFlash * 0.08, 0.16, 1.20);
  vLight = clamp(0.115 + key * 1.02 + boneKind * 0.070 + vAmp * 0.56, 0.035, 1.72);
  float size = (0.035 + boneKind * 0.026) * (0.84 + vDensity * 0.22 + vLight * 0.13 + vFlash * 0.18);
  gl_PointSize = clamp(size * uPixel * clamp(uPointScale, 0.48, 2.35) * 128.0 / dist, 0.95, 7.60);
  gl_Position = projectionMatrix * mv;
}
`;

const SKULL_FS = `
precision highp float;
uniform sampler2D uMap;
uniform vec3 uColorA, uColorB, uShadow, uLight;
uniform float uOpacity, uBloomStrength, uColorBoost;
varying float vKind, vLight, vRim, vAmp, vDensity, vFlash;
void main(){
  vec4 tex = texture2D(uMap, gl_PointCoord);
  if(tex.a < 0.070) discard;
  float lit = clamp(vLight, 0.0, 1.28);
  vec3 bone = mix(uColorA, uColorB, clamp((vKind - 0.34) * 2.0 + lit * 0.18, 0.0, 1.0));
  vec3 col = mix(uShadow, bone, clamp(lit, 0.0, 1.0));
  col = mix(col, uLight, clamp(vRim * (0.14 + vFlash * 0.40) + vAmp * 0.12, 0.0, 0.68));
  float alpha = tex.a * uOpacity * clamp(0.20 + lit * 0.44 + vDensity * 0.40 + vFlash * 0.46, 0.12, 1.56);
  gl_FragColor = vec4(col, alpha);
}
`;

const STAR_RIVER_VS = `
precision highp float;
attribute float aSeed, aLane, aDepthSeed;
uniform float uTime, uBass, uTreble, uBeat, uEnergy, uPixel, uPointScale, uAlpha, uParticleDim;
uniform vec3 uTintColor;
varying vec3 vColor;
varying float vAlpha, vTwinkle;
float hash11(float p){ return fract(sin(p * 127.1) * 43758.5453123); }
void main(){
  float band = floor(aLane * 6.0);
  float local = fract(aLane * 6.0);
  float bandN = (band + 0.5) / 6.0;
  float seed = aSeed + band * 19.17;
  float flow = fract(hash11(seed * 2.13) + uTime * (0.0022 + bandN * 0.0028));
  float arc = (flow - 0.5) * 6.2831853 * (0.68 + bandN * 0.46) + bandN * 2.4 + hash11(seed) * 6.2831853;
  float wave = sin(arc * (1.18 + bandN * 0.28) + uTime * 0.014 + seed * 0.07);
  float radius = 7.2 + bandN * 15.8 + hash11(seed * 3.7) * 6.2 + local * 1.8;
  vec3 pos = vec3(
    cos(arc * 0.76 + bandN * 0.84) * radius + (flow - 0.5) * 20.0,
    (bandN - 0.5) * 13.2 + wave * 1.8,
    mix(-31.0, -4.8, aDepthSeed) + wave * 1.2
  );
  float twinkle = pow(0.5 + 0.5 * sin(uTime * 0.35 + seed * 9.0), 5.0);
  vColor = mix(vec3(0.34, 0.76, 1.0), vec3(1.0, 0.78, 0.58), twinkle * 0.4 + uBass * 0.06);
  vColor = mix(vColor, max(uTintColor, vec3(0.08)), 0.22);
  vTwinkle = twinkle;
  vAlpha = uAlpha * uParticleDim * (0.14 + twinkle * 0.42 + uBeat * 0.05);
  vec4 mv = modelViewMatrix * vec4(pos, 1.0);
  float depthSize = 30.0 / max(0.65, -mv.z);
  gl_PointSize = clamp((1.2 + twinkle * 2.8 + uTreble * 0.8) * depthSize * uPixel * uPointScale, 0.75, 5.6);
  gl_Position = projectionMatrix * mv;
}
`;

const STAR_RIVER_FS = `
precision highp float;
uniform sampler2D uDotTex;
varying vec3 vColor;
varying float vAlpha, vTwinkle;
void main(){
  vec4 tex = texture2D(uDotTex, gl_PointCoord);
  if (tex.a < 0.02) discard;
  gl_FragColor = vec4(clamp(vColor * (0.66 + vTwinkle * 0.72), vec3(0.0), vec3(1.45)), tex.a * vAlpha);
}
`;

export class VisualEngine {
  private container: HTMLElement | null = null;
  private scene: THREE.Scene | null = null;
  private camera: THREE.PerspectiveCamera | null = null;
  private renderer: THREE.WebGLRenderer | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private rafId = 0;
  private mounted = false;
  private disposed = false;

  private fx: FxSettings = { ...DEFAULT_FX_SETTINGS };
  private playing = false;
  private callbacks: VisualEngineCallbacks = {};

  // 主粒子系统
  private dotTexture: THREE.CanvasTexture | null = null;
  private coverTex: THREE.Texture | null = null;
  private prevCoverTex: THREE.Texture | null = null;
  private coverEdgeTex: THREE.Texture | null = null;
  private rippleData = new Float32Array(RIPPLE_MAX * 4);
  private rippleTex: THREE.DataTexture | null = null;
  private ripples: Array<{ x: number; y: number; age: number; str: number }> = Array.from(
    { length: RIPPLE_MAX },
    () => ({ x: 0, y: 0, age: -10, str: 0 })
  );
  private rippleCursor = 0;
  private particles: THREE.Points | null = null;
  private bloomParticles: THREE.Points | null = null;
  private backCoverParticles: THREE.Points | null = null;
  private starRiverParticles: THREE.Points | null = null;
  private uniforms: Record<string, THREE.IUniform> | null = null;
  private starRiverUniforms: Record<string, THREE.IUniform> | null = null;
  private currentGrid = 0;
  private coverPalette = { primary: '#9db8cf', secondary: '#7ec8d8', highlight: '#fff0b8' };
  private currentCoverUrl = '';
  private playQueueRef: SongRecord[] = [];
  private currentIdxRef = -1;
  private particleGroupAlpha = 1;

  // 骷髅粒子层 (Preset 6)
  private skullGroup: THREE.Points | null = null;
  private skullAssetData: Float32Array | null = null;
  private skullAssetLoading = false;
  private skullOpacity = 0;
  private skullWheelZoom = 0;
  private skullWheelZoomTarget = 0;

  // BeatMap 与手势旋转
  private beatMap: BeatMapData | null = null;
  private gestureRotation = { x: 0, y: 0 };

  // 舞台歌词 CanvasTexture 渲染（低配帧预算控制：每帧最多上传 1 个纹理）
  private lyricsGroup: THREE.Group | null = null;
  private lyricMesh: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial> | null = null;
  private lyricCanvas: HTMLCanvasElement | null = null;
  private lyricCtx: CanvasRenderingContext2D | null = null;
  private lyricTexture: THREE.CanvasTexture | null = null;
  private lyricLines: LyricLine[] = [];
  private activeLyricIdx = -1;
  private lyricCurrentTime = 0;
  private lyricDirty = false;
  private lyricRenderKey = '';
  private lastLyricUploadAt = 0;

  // 3D 歌单架
  private shelfGroup: THREE.Group | null = null;
  private shelfSlots: ShelfCardSlot[] = [];
  private shelfItems: ShelfItem[] = [];
  private shelfVisible = false;
  private shelfVisibilityMix = 0;
  private shelfCenterTarget = 0;
  private shelfCenterSmooth = 0;
  private shelfLastCenterIdx = -1;
  private shelfHoveredSlot = -1;
  private shelfCoverCache = new Map<string, HTMLImageElement>();

  // 相机轨道与自由镜头
  private orbit = {
    userTheta: 0,
    userPhi: 0.08,
    userRadius: 6.6,
    theta: 0,
    phi: 0.08,
    radius: 6.6,
    rotating: false,
    lastX: 0,
    lastY: 0,
    velTheta: 0,
    velPhi: 0,
    hadDrag: false,
    downX: 0,
    downY: 0,
  };
  private freeCamera = {
    active: false,
    pos: new THREE.Vector3(0, 0, 6.6),
    yaw: 0,
    pitch: 0.08,
    fov: 45,
    keys: new Set<string>(),
    resetting: false,
  };
  private raycaster = new THREE.Raycaster();
  private pointerNdc = new THREE.Vector2();
  private mousePlane = new THREE.Plane(new THREE.Vector3(0, 0, 1), 0);
  private mouseHit = new THREE.Vector3();
  private prevFrameTime = 0;
  private elapsedSec = 0;
  private vinylSpin = 0;
  private colorMixT = 1;
  private burstAmt = 0;

  // 事件解绑清理函数
  private unbindDomEvents: (() => void) | null = null;

  public mount(container: HTMLElement): void {
    if (this.mounted && this.container === container) return;
    this.dispose();
    this.disposed = false;
    this.container = container;

    const width = Math.max(1, container.clientWidth || (typeof window !== 'undefined' ? window.innerWidth : 1280));
    const height = Math.max(1, container.clientHeight || (typeof window !== 'undefined' ? window.innerHeight : 720));

    this.scene = new THREE.Scene();
    this.scene.background = null;
    this.camera = new THREE.PerspectiveCamera(45, width / height, 0.1, 100);
    this.camera.position.set(0, 0, this.orbit.radius);

    try {
      this.renderer = new THREE.WebGLRenderer({
        alpha: true,
        antialias: true,
        powerPreference: 'high-performance',
      });
    } catch {
      // 无 WebGL 上下文环境（例如 headless 测试 / 软件环境）优雅降级
      this.mounted = true;
      return;
    }

    this.renderer.setPixelRatio(this.computePixelRatio(width, height));
    this.renderer.setSize(width, height);
    this.renderer.domElement.style.width = '100%';
    this.renderer.domElement.style.height = '100%';
    this.renderer.domElement.style.display = 'block';
    container.innerHTML = '';
    container.appendChild(this.renderer.domElement);

    this.initParticleSystem();
    this.initStarRiver();
    this.initLyricsStage();
    this.initShelfGroup();
    this.bindInteractionEvents(this.renderer.domElement);
    this.bindResize(container);

    this.syncFx(this.fx);
    this.mounted = true;
    this.prevFrameTime = typeof performance !== 'undefined' ? performance.now() : Date.now();
    this.startLoop();
  }

  private computePixelRatio(width: number, height: number): number {
    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const q = String(this.fx.performanceQuality || 'eco');
    const budget = q === 'ultra' ? 7800000 : q === 'high' ? 5200000 : q === 'balanced' ? 3800000 : 2400000;
    const cap = q === 'ultra' ? 1.75 : q === 'high' ? 1.35 : q === 'balanced' ? 1.12 : 0.95;
    const budgetCap = Math.sqrt(budget / Math.max(1, width * height));
    return clamp(Math.min(dpr, cap, budgetCap), 0.55, 2);
  }

  private initParticleSystem(): void {
    if (!this.scene || !this.renderer) return;
    this.dotTexture = makeDotTexture();
    this.coverTex = makeSolidCanvasTexture('#1c1c28');
    this.prevCoverTex = makeSolidCanvasTexture('#1c1c28');
    this.coverEdgeTex = makeSolidCanvasTexture('rgba(128,0,0,255)');
    this.rippleTex = new THREE.DataTexture(
      this.rippleData,
      1,
      RIPPLE_MAX,
      THREE.RGBAFormat,
      THREE.FloatType
    );
    this.rippleTex.minFilter = THREE.NearestFilter;
    this.rippleTex.magFilter = THREE.NearestFilter;

    this.currentGrid = coverParticleGridForResolution(this.fx.coverResolution);
    const geo = buildCoverParticleGeometry(this.currentGrid);

    this.uniforms = {
      uTime: { value: 0 },
      uBass: { value: 0 },
      uMid: { value: 0 },
      uTreble: { value: 0 },
      uBeat: { value: 0 },
      uEnergy: { value: 0 },
      uBurstAmt: { value: 0 },
      uVinylSpin: { value: 0 },
      uPreset: { value: this.fx.preset },
      uIntensity: { value: this.fx.intensity },
      uDepth: { value: this.fx.depth },
      uPointScale: { value: this.fx.point },
      uSpeed: { value: this.fx.speed },
      uTwist: { value: this.fx.twist },
      uColorBoost: { value: this.fx.color },
      uScatter: { value: this.fx.scatter },
      uCoverRes: { value: this.fx.coverResolution },
      uBgFade: { value: this.fx.bgFade },
      uBloomStrength: { value: this.fx.bloomStrength },
      uTintColor: { value: new THREE.Color(this.fx.visualTintColor || '#9db8cf') },
      uTintStrength: { value: 0 },
      uCoverTex: { value: this.coverTex },
      uPrevCoverTex: { value: this.prevCoverTex },
      uColorMixT: { value: 1.0 },
      uEdgeTex: { value: this.coverEdgeTex },
      uRippleTex: { value: this.rippleTex },
      uRippleCount: { value: 0 },
      uDotTex: { value: this.dotTexture },
      uHasCover: { value: 0 },
      uHasDepth: { value: 0 },
      uEdgeEnabled: { value: this.fx.edge ? 1 : 0 },
      uAiBoost: { value: this.fx.aiDepth ? 1 : 0 },
      uMouseXY: { value: new THREE.Vector2(-999, -999) },
      uMouseActive: { value: 0 },
      uPixel: { value: this.renderer.getPixelRatio() },
      uAlpha: { value: 1 },
      uParticleDim: { value: 1 },
      uBackdropAdapt: { value: 0.72 },
      uLoading: { value: 0 },
    };

    const mat = new THREE.ShaderMaterial({
      uniforms: this.uniforms,
      vertexShader: PARTICLE_VS,
      fragmentShader: PARTICLE_FS,
      transparent: true,
      depthWrite: false,
      blending: THREE.NormalBlending,
    });
    this.particles = new THREE.Points(geo, mat);
    this.particles.frustumCulled = false;
    this.particles.renderOrder = 1;
    this.scene.add(this.particles);

    const bloomMat = new THREE.ShaderMaterial({
      uniforms: this.uniforms,
      vertexShader: PARTICLE_VS,
      fragmentShader: PARTICLE_FS,
      transparent: true,
      depthWrite: false,
      depthTest: false,
      blending: THREE.AdditiveBlending,
    });
    this.bloomParticles = new THREE.Points(geo, bloomMat);
    this.bloomParticles.frustumCulled = false;
    this.bloomParticles.renderOrder = 0;
    this.bloomParticles.visible = Boolean(this.fx.bloom);
    this.scene.add(this.bloomParticles);

    const backMat = new THREE.ShaderMaterial({
      uniforms: this.uniforms,
      vertexShader: PARTICLE_VS,
      fragmentShader: PARTICLE_FS,
      transparent: true,
      depthWrite: false,
      blending: THREE.NormalBlending,
    });
    this.backCoverParticles = new THREE.Points(geo, backMat);
    this.backCoverParticles.frustumCulled = false;
    this.backCoverParticles.renderOrder = 0;
    this.backCoverParticles.position.set(0, 0, -0.06);
    this.backCoverParticles.rotation.set(0, Math.PI, 0);
    this.backCoverParticles.visible = Boolean(this.fx.backCover);
    this.scene.add(this.backCoverParticles);
  }

  private initStarRiver(): void {
    if (!this.scene || !this.uniforms) return;
    this.starRiverUniforms = {
      uDotTex: this.uniforms.uDotTex,
      uTime: this.uniforms.uTime,
      uBass: this.uniforms.uBass,
      uTreble: this.uniforms.uTreble,
      uBeat: this.uniforms.uBeat,
      uEnergy: this.uniforms.uEnergy,
      uPixel: this.uniforms.uPixel,
      uPointScale: this.uniforms.uPointScale,
      uParticleDim: this.uniforms.uParticleDim,
      uTintColor: this.uniforms.uTintColor,
      uAlpha: { value: 0.34 },
    };
    const mat = new THREE.ShaderMaterial({
      uniforms: this.starRiverUniforms,
      vertexShader: STAR_RIVER_VS,
      fragmentShader: STAR_RIVER_FS,
      transparent: true,
      depthWrite: false,
      depthTest: false,
      blending: THREE.AdditiveBlending,
    });
    this.starRiverParticles = new THREE.Points(
      buildBackgroundStarRiverGeometry(BACKGROUND_STAR_RIVER_COUNT),
      mat
    );
    this.starRiverParticles.frustumCulled = false;
    this.starRiverParticles.renderOrder = -2;
    this.scene.add(this.starRiverParticles);
  }

  private initLyricsStage(): void {
    if (!this.scene) return;
    this.lyricsGroup = new THREE.Group();
    this.lyricsGroup.position.set(0, 0.2, 1.46);
    this.lyricsGroup.renderOrder = 42;

    this.lyricCanvas = document.createElement('canvas');
    this.lyricCanvas.width = 1408;
    this.lyricCanvas.height = 480;
    this.lyricCtx = this.lyricCanvas.getContext('2d');
    this.lyricTexture = new THREE.CanvasTexture(this.lyricCanvas);
    this.lyricTexture.minFilter = THREE.LinearFilter;
    this.lyricTexture.magFilter = THREE.LinearFilter;

    const mat = new THREE.MeshBasicMaterial({
      map: this.lyricTexture,
      transparent: true,
      opacity: 0.96,
      depthWrite: false,
      depthTest: false,
      side: THREE.DoubleSide,
    });
    const geo = new THREE.PlaneGeometry(5.6, 5.6 * (this.lyricCanvas.height / this.lyricCanvas.width));
    this.lyricMesh = new THREE.Mesh(geo, mat);
    this.lyricMesh.renderOrder = 43;
    this.lyricsGroup.add(this.lyricMesh);
    this.scene.add(this.lyricsGroup);
  }

  private initShelfGroup(): void {
    if (!this.scene) return;
    this.shelfGroup = new THREE.Group();
    this.shelfGroup.renderOrder = 24;
    this.shelfGroup.visible = false;

    for (let i = 0; i < SHELF_MAX_RENDER; i++) {
      const cv = document.createElement('canvas');
      cv.width = 512;
      cv.height = 176;
      const ctx = cv.getContext('2d')!;
      const tex = new THREE.CanvasTexture(cv);
      tex.minFilter = THREE.LinearFilter;
      tex.magFilter = THREE.LinearFilter;
      const mat = new THREE.MeshBasicMaterial({
        map: tex,
        transparent: true,
        opacity: 0,
        depthWrite: false,
        side: THREE.DoubleSide,
      });
      const mesh = new THREE.Mesh(new THREE.PlaneGeometry(2.15, 0.74), mat);
      mesh.visible = false;
      mesh.userData.slotIndex = i;
      this.shelfGroup.add(mesh);
      this.shelfSlots.push({
        mesh,
        canvas: cv,
        ctx,
        texture: tex,
        item: null,
        itemIndex: -1,
        drawKey: '',
      });
    }
    this.scene.add(this.shelfGroup);
  }

  private bindInteractionEvents(dom: HTMLCanvasElement): void {
    const onMouseDown = (e: MouseEvent) => {
      if (e.button === 2) return;
      this.orbit.rotating = true;
      this.orbit.lastX = e.clientX;
      this.orbit.lastY = e.clientY;
      this.orbit.downX = e.clientX;
      this.orbit.downY = e.clientY;
      this.orbit.hadDrag = false;
      this.orbit.velTheta = 0;
      this.orbit.velPhi = 0;
    };

    const onMouseMove = (e: MouseEvent) => {
      const rect = dom.getBoundingClientRect();
      const nx = ((e.clientX - rect.left) / Math.max(1, rect.width)) * 2 - 1;
      const ny = -((e.clientY - rect.top) / Math.max(1, rect.height)) * 2 + 1;
      this.pointerNdc.set(nx, ny);

      if (this.orbit.rotating) {
        const dx = e.clientX - this.orbit.lastX;
        const dy = e.clientY - this.orbit.lastY;
        this.orbit.lastX = e.clientX;
        this.orbit.lastY = e.clientY;
        if (this.freeCamera.active) {
          this.freeCamera.yaw -= dx * 0.0028;
          this.freeCamera.pitch = clamp(
            this.freeCamera.pitch - dy * 0.0024,
            -Math.PI * 0.48,
            Math.PI * 0.48
          );
          this.freeCamera.resetting = false;
        } else {
          const dTheta = -dx * 0.0052;
          const dPhi = dy * 0.0044;
          this.orbit.userTheta += dTheta;
          this.orbit.userPhi = clamp(this.orbit.userPhi + dPhi, -Math.PI * 0.45, Math.PI * 0.45);
          this.orbit.velTheta = dTheta;
          this.orbit.velPhi = dPhi;
        }
        if (
          Math.hypot(e.clientX - this.orbit.downX, e.clientY - this.orbit.downY) > 6
        ) {
          this.orbit.hadDrag = true;
        }
        return;
      }

      // 悬停主粒子平面交互
      if (this.camera && this.uniforms) {
        this.raycaster.setFromCamera(this.pointerNdc, this.camera);
        if (this.raycaster.ray.intersectPlane(this.mousePlane, this.mouseHit)) {
          (this.uniforms.uMouseXY.value as THREE.Vector2).set(this.mouseHit.x, this.mouseHit.y);
          this.uniforms.uMouseActive.value = 1;
        } else {
          this.uniforms.uMouseActive.value = 0;
        }

        // 3D 歌单架悬停检测
        if (this.shelfVisible && this.shelfGroup) {
          const hit = this.raycastShelf(this.pointerNdc);
          const prev = this.shelfHoveredSlot;
          this.shelfHoveredSlot = hit ? hit.slotIndex : -1;
          if (prev !== this.shelfHoveredSlot) {
            this.callbacks.onShelfHover?.(hit ? hit.item : null, hit ? hit.itemIndex : -1);
          }
        }
      }
    };

    const onMouseUp = () => {
      const wasDrag = this.orbit.hadDrag;
      this.orbit.rotating = false;
      if (wasDrag) return;
      if (this.shelfVisible && this.camera) {
        const hit = this.raycastShelf(this.pointerNdc);
        if (hit) {
          const isAlreadyCentered =
            Math.abs(hit.itemIndex - this.shelfCenterSmooth) < 0.35 &&
            Math.round(this.shelfCenterTarget) === hit.itemIndex;
          if (isAlreadyCentered) {
            this.callbacks.onShelfSelect?.(hit.item, hit.itemIndex);
          } else {
            this.shelfCenterTarget = hit.itemIndex;
            this.callbacks.onShelfFocusRequest?.(hit.item, hit.itemIndex);
          }
          return;
        }
      }
      if (this.fx.preset === 7 && typeof window !== 'undefined') {
        window.MineradioSonicTopography?.pointerRipple?.(
          this.mouseHit.x * 6,
          this.mouseHit.y * 6,
          1.25
        );
      } else {
        this.spawnRipple((Math.random() - 0.5) * 1.4, (Math.random() - 0.5) * 1.4, 0.85);
      }
    };

    const onMouseLeave = () => {
      this.orbit.rotating = false;
      if (this.uniforms) this.uniforms.uMouseActive.value = 0;
      if (this.shelfHoveredSlot >= 0) {
        this.shelfHoveredSlot = -1;
        this.callbacks.onShelfHover?.(null, -1);
      }
    };

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (this.freeCamera.active) {
        this.freeCamera.fov = clamp(this.freeCamera.fov + e.deltaY * 0.02, 26, 72);
        this.freeCamera.resetting = false;
        return;
      }
      if (this.shelfVisible && this.shelfHoveredSlot >= 0 && this.shelfItems.length > 0) {
        const step = e.deltaY > 0 ? 1 : -1;
        this.shelfCenterTarget = clamp(
          Math.round(this.shelfCenterTarget + step),
          0,
          Math.max(0, this.shelfItems.length - 1)
        );
        return;
      }
      if (this.fx.preset === SKULL_PRESET_INDEX) {
        this.skullWheelZoomTarget = clamp(
          this.skullWheelZoomTarget + e.deltaY * 0.00155,
          -0.95,
          1.28
        );
        return;
      }
      this.orbit.userRadius = clamp(this.orbit.userRadius + e.deltaY * 0.005, 2.4, 14.0);
    };

    const onDblClick = () => {
      if (this.freeCamera.active) {
        this.resetFreeCamera();
      } else {
        this.recenterCamera();
      }
    };

    const isEditableTarget = (target: EventTarget | null): boolean => {
      const el = target as HTMLElement | null;
      return Boolean(
        el &&
          (el.tagName === 'INPUT' ||
            el.tagName === 'TEXTAREA' ||
            el.tagName === 'SELECT' ||
            el.isContentEditable)
      );
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return;
      if (!e.ctrlKey && !e.metaKey && !e.altKey) {
        if (e.code === 'KeyR') {
          this.toggleFreeCamera();
          return;
        }
        if (e.code === 'KeyK') {
          this.resetFreeCamera();
          return;
        }
      }
      if (this.freeCamera.active) {
        if (
          e.code === 'KeyW' ||
          e.code === 'KeyA' ||
          e.code === 'KeyS' ||
          e.code === 'KeyD' ||
          e.code === 'KeyQ' ||
          e.code === 'KeyE' ||
          e.code === 'Space' ||
          e.code === 'ControlLeft' ||
          e.code === 'ControlRight'
        ) {
          this.freeCamera.keys.add(e.code);
          this.freeCamera.resetting = false;
        }
      }
    };

    const onKeyUp = (e: KeyboardEvent) => {
      this.freeCamera.keys.delete(e.code);
    };

    dom.addEventListener('mousedown', onMouseDown);
    window.addEventListener('mousemove', onMouseMove);
    window.addEventListener('mouseup', onMouseUp);
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    dom.addEventListener('mouseleave', onMouseLeave);
    dom.addEventListener('wheel', onWheel, { passive: false });
    dom.addEventListener('dblclick', onDblClick);

    this.unbindDomEvents = () => {
      dom.removeEventListener('mousedown', onMouseDown);
      window.removeEventListener('mousemove', onMouseMove);
      window.removeEventListener('mouseup', onMouseUp);
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('keyup', onKeyUp);
      dom.removeEventListener('mouseleave', onMouseLeave);
      dom.removeEventListener('wheel', onWheel);
      dom.removeEventListener('dblclick', onDblClick);
    };
  }

  private bindResize(container: HTMLElement): void {
    const updateSize = () => {
      if (!this.renderer || !this.camera) return;
      const w = Math.max(1, container.clientWidth || window.innerWidth || 1280);
      const h = Math.max(1, container.clientHeight || window.innerHeight || 720);
      this.camera.aspect = w / h;
      this.camera.updateProjectionMatrix();
      const pr = this.computePixelRatio(w, h);
      this.renderer.setPixelRatio(pr);
      this.renderer.setSize(w, h);
      if (this.uniforms) this.uniforms.uPixel.value = pr;
    };

    if (typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(updateSize);
      this.resizeObserver.observe(container);
    } else if (typeof window !== 'undefined') {
      window.addEventListener('resize', updateSize);
      const prevUnbind = this.unbindDomEvents;
      this.unbindDomEvents = () => {
        prevUnbind?.();
        window.removeEventListener('resize', updateSize);
      };
    }
  }

  private spawnRipple(x: number, y: number, strength = 0.85): void {
    const slot = this.ripples[this.rippleCursor % RIPPLE_MAX];
    this.rippleCursor = (this.rippleCursor + 1) % RIPPLE_MAX;
    slot.x = x;
    slot.y = y;
    slot.age = 0;
    slot.str = clamp(strength, 0.1, 2.0);
  }

  // ==================== 公共命令式接口 ====================

  private syncPresetGlobals(): void {
    if (typeof window === 'undefined') return;
    window.THREE = THREE;
    window.fx = this.fx;
    window.playing = this.playing;
    window.audio = audioEngine.getActiveAudioElement();
    window.frequencyData = audioEngine.getFrequencyData();
    window.playQueue = this.playQueueRef;
    window.currentIdx = this.currentIdxRef;
    window.stageLyrics = {
      coverPalette: this.coverPalette,
      palette: this.coverPalette,
    };
    window.orbit = {
      theta: this.orbit.theta,
      phi: this.orbit.phi,
      radius: this.orbit.radius,
    };
  }

  public setCurrentSongContext(
    song: SongRecord | null,
    queue?: SongRecord[],
    currentIdx?: number
  ): void {
    if (Array.isArray(queue)) {
      this.playQueueRef = queue;
    } else if (song && this.playQueueRef.length === 0) {
      this.playQueueRef = [song];
    }
    if (typeof currentIdx === 'number') {
      this.currentIdxRef = currentIdx;
    } else if (song && this.playQueueRef.length > 0) {
      const idx = this.playQueueRef.indexOf(song);
      this.currentIdxRef = idx >= 0 ? idx : 0;
    }
    this.syncPresetGlobals();
  }

  public setBeatMap(map: BeatMapData | null): void {
    this.beatMap = map || null;
  }

  public isFreeCameraActive(): boolean {
    return this.freeCamera.active;
  }

  public toggleFreeCamera(force?: boolean): boolean {
    const next = typeof force === 'boolean' ? force : !this.freeCamera.active;
    if (next === this.freeCamera.active) return next;
    this.freeCamera.active = next;
    this.freeCamera.resetting = false;
    this.freeCamera.keys.clear();
    if (next && this.camera) {
      this.freeCamera.pos.copy(this.camera.position);
      const dir = new THREE.Vector3(0, 0, 0).sub(this.camera.position).normalize();
      this.freeCamera.yaw = Math.atan2(-dir.x, -dir.z);
      this.freeCamera.pitch = Math.asin(clamp(dir.y, -0.99, 0.99));
      this.freeCamera.fov = this.camera.fov || 45;
    } else if (!next && this.camera && this.camera.fov !== 45) {
      this.camera.fov = 45;
      this.camera.updateProjectionMatrix();
    }
    this.callbacks.onFreeCameraChange?.(next);
    return next;
  }

  public resetFreeCamera(): void {
    if (this.freeCamera.active) {
      this.freeCamera.resetting = true;
    } else {
      this.recenterCamera();
    }
  }

  public applyGestureRotation(rx: number, ry: number): void {
    this.gestureRotation.x += Number(rx) || 0;
    this.gestureRotation.y += Number(ry) || 0;
  }

  public resetGestureRotation(): void {
    this.gestureRotation.x = 0;
    this.gestureRotation.y = 0;
  }

  public setShelfCenter(index: number): void {
    if (!this.shelfItems.length) return;
    this.shelfCenterTarget = clamp(Math.round(index), 0, this.shelfItems.length - 1);
  }

  public setCallbacks(callbacks: VisualEngineCallbacks): void {
    this.callbacks = { ...this.callbacks, ...callbacks };
  }

  public syncFx(nextFx: FxSettings): void {
    const prevPreset = this.fx.preset;
    this.fx = { ...this.fx, ...nextFx };
    const nextPreset = Number(this.fx.preset) || 0;
    if (prevPreset !== nextPreset) {
      this.burstAmt = 1.0;
    }

    this.syncPresetGlobals();

    if (this.uniforms) {
      this.uniforms.uPreset.value = nextPreset;
      this.uniforms.uIntensity.value = clamp(this.fx.intensity, 0, 2);
      this.uniforms.uDepth.value = clamp(this.fx.depth, 0, 2);
      this.uniforms.uPointScale.value = clamp(this.fx.point, 0.3, 3);
      this.uniforms.uSpeed.value = clamp(this.fx.speed, 0.05, 4);
      this.uniforms.uTwist.value = clamp(this.fx.twist, 0, 3);
      this.uniforms.uColorBoost.value = clamp(this.fx.color, 0.35, 2.5);
      this.uniforms.uScatter.value = clamp(this.fx.scatter, 0, 2);
      this.uniforms.uCoverRes.value = clamp(this.fx.coverResolution, 0.5, 2);
      this.uniforms.uBgFade.value = clamp(this.fx.bgFade, 0, 1);
      this.uniforms.uBloomStrength.value = clamp(this.fx.bloomStrength, 0, 2);
      this.uniforms.uEdgeEnabled.value = this.fx.edge ? 1 : 0;
      this.uniforms.uAiBoost.value = this.fx.aiDepth ? 1 : 0;
      this.uniforms.uBackdropAdapt.value = clamp(this.fx.lyricBackgroundAdapt, 0, 1);

      const tintHex =
        this.fx.visualTintMode === 'custom'
          ? this.fx.visualTintColor
          : this.coverPalette.secondary || this.fx.visualTintColor || '#9db8cf';
      (this.uniforms.uTintColor.value as THREE.Color).set(tintHex);
      this.uniforms.uTintStrength.value = this.fx.visualTintMode === 'custom' ? 0.82 : 0.22;
    }

    if (this.bloomParticles) {
      this.bloomParticles.visible = Boolean(this.fx.bloom) && nextPreset !== 8;
    }
    if (this.backCoverParticles) {
      this.backCoverParticles.visible =
        Boolean(this.fx.backCover) &&
        nextPreset !== 3 &&
        nextPreset !== SKULL_PRESET_INDEX &&
        nextPreset !== 8;
    }

    const nextGrid = coverParticleGridForResolution(this.fx.coverResolution);
    if (nextGrid !== this.currentGrid && this.particles && this.bloomParticles) {
      const oldGeo = this.particles.geometry;
      const newGeo = buildCoverParticleGeometry(nextGrid);
      this.particles.geometry = newGeo;
      this.bloomParticles.geometry = newGeo;
      if (this.backCoverParticles) {
        this.backCoverParticles.geometry = newGeo;
      }
      this.currentGrid = nextGrid;
      oldGeo.dispose();
    }

    if (this.renderer && this.container) {
      const w = Math.max(1, this.container.clientWidth || 1280);
      const h = Math.max(1, this.container.clientHeight || 720);
      const pr = this.computePixelRatio(w, h);
      this.renderer.setPixelRatio(pr);
      if (this.uniforms) this.uniforms.uPixel.value = pr;
    }

    if (nextPreset === SKULL_PRESET_INDEX && !this.skullAssetData && !this.skullAssetLoading) {
      this.loadSkullAsset().catch(() => {});
    }

    if (prevPreset !== nextPreset && typeof window !== 'undefined') {
      const ctx = { fx: this.fx, scene: this.scene, camera: this.camera };
      window.MineradioSonicTopography?.onPresetChange?.(prevPreset, nextPreset, ctx);
      window.MineradioSonicWorkshop?.onPresetChange?.(prevPreset, nextPreset, ctx);
    }

    this.lyricDirty = true;
  }

  public setPlaying(playing: boolean): void {
    this.playing = Boolean(playing);
  }

  public recenterCamera(): void {
    this.orbit.userTheta = 0;
    this.orbit.userPhi = 0.08;
    this.orbit.userRadius = 6.6;
    this.orbit.velTheta = 0;
    this.orbit.velPhi = 0;
    this.skullWheelZoomTarget = 0;
  }

  public setCover(url: string): void {
    const normalized = String(url || '').trim();
    if (normalized === this.currentCoverUrl) return;
    this.currentCoverUrl = normalized;
    if (!normalized) {
      if (this.uniforms) this.uniforms.uHasCover.value = 0;
      return;
    }
    if (typeof Image === 'undefined') return;

    const resolved = coverProxyUrl(normalized);
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.onload = () => {
      if (this.disposed || this.currentCoverUrl !== normalized) return;
      this.applyLoadedCoverImage(img);
    };
    img.onerror = () => {
      if (this.uniforms && this.currentCoverUrl === normalized) {
        this.uniforms.uHasCover.value = 0;
      }
    };
    img.src = resolved;
  }

  private applyLoadedCoverImage(img: HTMLImageElement): void {
    if (!this.uniforms) return;
    const size = 256;
    const cv = document.createElement('canvas');
    cv.width = size;
    cv.height = size;
    const ctx = cv.getContext('2d');
    if (!ctx) return;

    ctx.drawImage(img, 0, 0, size, size);
    const imgData = ctx.getImageData(0, 0, size, size);
    const data = imgData.data;

    // 采样封面主色与边缘 Sobel 纹理
    let rSum = 0;
    let gSum = 0;
    let bSum = 0;
    let count = 0;
    const edgeCanvas = document.createElement('canvas');
    edgeCanvas.width = size;
    edgeCanvas.height = size;
    const edgeCtx = edgeCanvas.getContext('2d')!;
    const edgeImg = edgeCtx.createImageData(size, size);
    const ed = edgeImg.data;
    const lum = new Float32Array(size * size);

    for (let i = 0; i < size * size; i++) {
      const idx = i * 4;
      const r = data[idx];
      const g = data[idx + 1];
      const b = data[idx + 2];
      lum[i] = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
      if (i % 13 === 0) {
        rSum += r;
        gSum += g;
        bSum += b;
        count++;
      }
    }

    for (let y = 1; y < size - 1; y++) {
      for (let x = 1; x < size - 1; x++) {
        const i = y * size + x;
        const gx =
          -lum[i - size - 1] +
          lum[i - size + 1] -
          2 * lum[i - 1] +
          2 * lum[i + 1] -
          lum[i + size - 1] +
          lum[i + size + 1];
        const gy =
          -lum[i - size - 1] -
          2 * lum[i - size] -
          lum[i - size + 1] +
          lum[i + size - 1] +
          2 * lum[i + size] +
          lum[i + size + 1];
        const edge = clamp(Math.sqrt(gx * gx + gy * gy) * 1.35, 0, 1);
        const outIdx = i * 4;
        ed[outIdx] = Math.round(lum[i] * 255);
        ed[outIdx + 1] = Math.round(edge * 255);
        ed[outIdx + 2] = lum[i] > 0.25 ? 255 : 64;
        ed[outIdx + 3] = 255;
      }
    }
    edgeCtx.putImageData(edgeImg, 0, 0);

    if (count > 0) {
      const ar = Math.min(255, Math.round((rSum / count) * 1.15 + 24));
      const ag = Math.min(255, Math.round((gSum / count) * 1.15 + 24));
      const ab = Math.min(255, Math.round((bSum / count) * 1.15 + 24));
      this.coverPalette.primary = `#${ar.toString(16).padStart(2, '0')}${ag
        .toString(16)
        .padStart(2, '0')}${ab.toString(16).padStart(2, '0')}`;
      this.coverPalette.secondary = this.coverPalette.primary;
      this.lyricDirty = true;
    }

    const oldPrev = this.prevCoverTex;
    this.prevCoverTex = this.coverTex;
    const nextCoverTex = new THREE.CanvasTexture(cv);
    nextCoverTex.minFilter = THREE.LinearFilter;
    nextCoverTex.magFilter = THREE.LinearFilter;
    nextCoverTex.needsUpdate = true;
    this.coverTex = nextCoverTex;

    const oldEdge = this.coverEdgeTex;
    const nextEdgeTex = new THREE.CanvasTexture(edgeCanvas);
    nextEdgeTex.minFilter = THREE.LinearFilter;
    nextEdgeTex.magFilter = THREE.LinearFilter;
    nextEdgeTex.needsUpdate = true;
    this.coverEdgeTex = nextEdgeTex;

    this.uniforms.uPrevCoverTex.value = this.prevCoverTex;
    this.uniforms.uCoverTex.value = this.coverTex;
    this.uniforms.uEdgeTex.value = this.coverEdgeTex;
    this.uniforms.uHasCover.value = 1;
    this.colorMixT = 0;
    this.uniforms.uColorMixT.value = 0;

    if (oldPrev && oldPrev !== this.coverTex) oldPrev.dispose();
    if (oldEdge && oldEdge !== this.coverEdgeTex) oldEdge.dispose();
  }

  public setLyrics(lines: LyricLine[], activeIdx: number, currentTime: number): void {
    this.lyricLines = Array.isArray(lines) ? lines : [];
    this.activeLyricIdx = activeIdx;
    this.lyricCurrentTime = currentTime;
    const activeLine = this.lyricLines[activeIdx];
    const progressBucket = this.playing ? Math.floor(currentTime * 24) : 0;
    const glitchBucket =
      this.fx.lyricMotionStyle === 'glitch' && this.playing ? Math.floor(currentTime * 15) : 0;
    const nextKey = [
      activeIdx,
      activeLine?.text || '',
      activeLine?.translation || '',
      activeLine?.roma || '',
      this.fx.lyricDisplayMode,
      this.fx.lyricTranslationMode,
      this.fx.lyricColor,
      this.fx.lyricHighlightColor,
      this.fx.lyricFont,
      this.fx.lyricTextureClarity,
      this.fx.lyricMotionStyle,
      progressBucket,
      glitchBucket,
    ].join('|');
    if (nextKey !== this.lyricRenderKey) {
      this.lyricRenderKey = nextKey;
      this.lyricDirty = true;
    }
  }

  public setShelfItems(items: ShelfItem[], visible: boolean): void {
    this.shelfItems = Array.isArray(items) ? items : [];
    this.shelfVisible = Boolean(visible) && this.fx.shelf !== 'off';
    if (this.shelfCenterTarget >= this.shelfItems.length) {
      this.shelfCenterTarget = Math.max(0, this.shelfItems.length - 1);
    }
    this.shelfSlots.forEach((slot) => {
      slot.drawKey = '';
    });
  }

  // ==================== 骷髅粒子资产加载 ====================

  private async loadSkullAsset(): Promise<void> {
    if (this.skullAssetData || this.skullAssetLoading) return;
    this.skullAssetLoading = true;
    try {
      const url = resolveMediaUrl('/assets/skull-decimation-points.bin', {
        preferHttpFallback: false,
      });
      const resp = await fetch(url);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const buf = await resp.arrayBuffer();
      if (buf.byteLength >= 20 && buf.byteLength % 20 === 0) {
        this.skullAssetData = new Float32Array(buf);
        if (!this.disposed) this.createSkullGroup();
      }
    } catch {
      // ignore missing asset in test/offline mode
    } finally {
      this.skullAssetLoading = false;
    }
  }

  private createSkullGroup(): void {
    if (this.skullGroup || !this.scene || !this.uniforms || !this.skullAssetData) return;
    const pts = this.skullAssetData;
    const count = Math.floor(pts.length / 5);
    const geo = new THREE.BufferGeometry();
    const pos = new Float32Array(count * 3);
    const seeds = new Float32Array(count);
    const kinds = new Float32Array(count);
    for (let i = 0; i < count; i++) {
      pos[i * 3] = pts[i * 5];
      pos[i * 3 + 1] = pts[i * 5 + 1];
      pos[i * 3 + 2] = pts[i * 5 + 2];
      kinds[i] = pts[i * 5 + 3];
      seeds[i] = pts[i * 5 + 4];
    }
    geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    geo.setAttribute('seed', new THREE.BufferAttribute(seeds, 1));
    geo.setAttribute('kind', new THREE.BufferAttribute(kinds, 1));

    const mat = new THREE.ShaderMaterial({
      uniforms: {
        uMap: { value: this.dotTexture },
        uTime: this.uniforms.uTime,
        uPixel: this.uniforms.uPixel,
        uBass: this.uniforms.uBass,
        uMid: this.uniforms.uMid,
        uTreble: this.uniforms.uTreble,
        uBeat: this.uniforms.uBeat,
        uJawOpen: { value: 0 },
        uSkullFlash: { value: 0 },
        uPointScale: this.uniforms.uPointScale,
        uBloomStrength: this.uniforms.uBloomStrength,
        uColorBoost: this.uniforms.uColorBoost,
        uOpacity: { value: 0 },
        uColorA: { value: new THREE.Color('#b8ae98') },
        uColorB: { value: new THREE.Color('#fff4d8') },
        uShadow: { value: new THREE.Color('#100d0d') },
        uLight: { value: new THREE.Color('#ffe3a0') },
      },
      vertexShader: SKULL_VS,
      fragmentShader: SKULL_FS,
      transparent: true,
      depthWrite: false,
      depthTest: true,
      blending: THREE.NormalBlending,
    });
    this.skullGroup = new THREE.Points(geo, mat);
    this.skullGroup.frustumCulled = false;
    this.skullGroup.visible = false;
    this.skullGroup.position.set(0, 0.22, 0.1);
    this.skullGroup.scale.setScalar(2.34);
    this.skullGroup.rotation.set(-0.26, 0, 0);
    this.skullGroup.renderOrder = 32;
    this.scene.add(this.skullGroup);
  }

  // ==================== 舞台歌词渲染（帧预算控制） ====================

  private computeActiveLyricProgress(active: LyricLine, nextLine: LyricLine | null): number {
    if (Array.isArray(active.words) && active.words.length > 0) {
      const t = this.lyricCurrentTime;
      const totalChars = active.words.reduce((acc, w) => acc + Math.max(1, w.text.length), 0);
      if (totalChars <= 0) return 1;
      let doneChars = 0;
      for (const word of active.words) {
        const wLen = Math.max(1, word.text.length);
        if (t >= word.end) {
          doneChars += wLen;
        } else if (t > word.start && word.end > word.start) {
          doneChars += wLen * clamp((t - word.start) / (word.end - word.start), 0, 1);
          break;
        } else {
          break;
        }
      }
      return clamp(doneChars / totalChars, 0, 1);
    }
    const endTime = nextLine?.time != null && nextLine.time > active.time ? nextLine.time : active.time + 3.6;
    const span = Math.max(0.4, endTime - active.time);
    return clamp((this.lyricCurrentTime - active.time) / span, 0, 1);
  }

  private resolveLyricFontFamily(fontKey: string | undefined): string {
    switch (String(fontKey || 'sans').toLowerCase()) {
      case 'song':
      case 'serif':
        return '"Noto Serif SC", "Songti SC", serif';
      case 'hei':
        return '"PingFang SC", "Microsoft YaHei", "Noto Sans SC", sans-serif';
      case 'mono':
        return '"JetBrains Mono", monospace';
      case 'cinzel':
        return '"Cinzel Decorative", "Noto Serif SC", serif';
      case 'fraktur':
        return '"UnifrakturCook", "Cinzel Decorative", serif';
      default:
        return 'Inter, "Noto Sans SC", sans-serif';
    }
  }

  private renderStageLyricsTexture(): void {
    if (!this.lyricCtx || !this.lyricCanvas || !this.lyricTexture || !this.lyricMesh) return;

    const clarity = clamp(Number(this.fx.lyricTextureClarity ?? 1), 0.75, 2.0);
    const targetW = Math.round(1408 * clarity);
    const targetH = Math.round(480 * clarity);
    if (this.lyricCanvas.width !== targetW || this.lyricCanvas.height !== targetH) {
      this.lyricCanvas.width = targetW;
      this.lyricCanvas.height = targetH;
      this.lyricTexture.needsUpdate = true;
    }

    const ctx = this.lyricCtx;
    const w = this.lyricCanvas.width;
    const h = this.lyricCanvas.height;
    ctx.clearRect(0, 0, w, h);

    if (!this.fx.particleLyrics || this.lyricLines.length === 0) {
      this.lyricTexture.needsUpdate = true;
      return;
    }

    const activeIdx = clamp(
      this.activeLyricIdx < 0 ? 0 : this.activeLyricIdx,
      0,
      Math.max(0, this.lyricLines.length - 1)
    );
    const active = this.lyricLines[activeIdx];
    if (!active) {
      this.lyricTexture.needsUpdate = true;
      return;
    }

    const singleMode = this.fx.lyricDisplayMode === 'single';
    const prevLine = !singleMode && activeIdx > 0 ? this.lyricLines[activeIdx - 1] : null;
    const nextLine =
      !singleMode && activeIdx + 1 < this.lyricLines.length ? this.lyricLines[activeIdx + 1] : null;

    const mainColor =
      this.fx.lyricHighlightMode === 'custom'
        ? this.fx.lyricHighlightColor || '#fff0b8'
        : this.coverPalette.highlight || '#fff0b8';
    const subColor =
      this.fx.lyricColorMode === 'custom'
        ? this.fx.lyricColor || '#7ec8d8'
        : this.coverPalette.secondary || '#7ec8d8';

    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const weight = clamp(Number(this.fx.lyricWeight) || 750, 400, 900);
    const fontFamily = this.resolveLyricFontFamily(this.fx.lyricFont);
    const s = clarity;

    if (prevLine?.text) {
      ctx.save();
      ctx.globalAlpha = clamp(this.fx.lyricContextOpacity, 0.1, 0.9);
      ctx.font = `${weight} ${Math.round(34 * s)}px ${fontFamily}`;
      ctx.fillStyle = subColor;
      ctx.fillText(prevLine.text, w / 2, h * 0.2, w * 0.88);
      ctx.restore();
    }

    // 主歌词逐字/逐句高亮裁切绘制 + Glitch 效果
    const mainFontSize = Math.round(60 * s);
    const mainY = h * 0.44;
    const maxMainW = w * 0.92;
    const progress = this.computeActiveLyricProgress(active, nextLine);
    const isGlitch = this.fx.lyricMotionStyle === 'glitch';
    const glitchIntensity = isGlitch ? clamp(this.fx.lyricGlitchIntensity ?? 1, 0, 2) : 0;
    const glitchJitter =
      isGlitch && glitchIntensity > 0
        ? Math.sin(this.elapsedSec * 42 * (this.fx.lyricGlitchRate || 1)) *
          6 *
          s *
          clamp(this.fx.lyricGlitchJitter ?? 0.72, 0, 2) *
          glitchIntensity
        : 0;
    const chromaShift =
      isGlitch && glitchIntensity > 0
        ? 4.5 * s * clamp(this.fx.lyricGlitchChroma ?? 0.86, 0, 2) * glitchIntensity
        : 0;

    ctx.save();
    ctx.font = `${weight} ${mainFontSize}px ${fontFamily}`;
    const measuredW = Math.min(maxMainW, ctx.measureText(active.text).width);
    const textLeft = w / 2 - measuredW / 2;

    if (chromaShift > 0.2) {
      ctx.save();
      ctx.globalAlpha = 0.42;
      ctx.fillStyle = '#00f5d4';
      ctx.fillText(active.text, w / 2 - chromaShift + glitchJitter, mainY, maxMainW);
      ctx.fillStyle = '#ff4d94';
      ctx.fillText(active.text, w / 2 + chromaShift - glitchJitter * 0.6, mainY, maxMainW);
      ctx.restore();
    }

    // 未唱底色层
    ctx.save();
    ctx.globalAlpha = 0.46;
    ctx.fillStyle = subColor;
    ctx.fillText(active.text, w / 2 + glitchJitter * 0.3, mainY, maxMainW);
    ctx.restore();

    // 已唱高亮裁切层
    ctx.save();
    if (this.fx.lyricGlow) {
      ctx.shadowColor = this.fx.lyricGlowLinked ? mainColor : this.fx.lyricGlowColor || '#9db8cf';
      ctx.shadowBlur = 18 * s * clamp(this.fx.lyricGlowStrength, 0, 2);
    }
    const clipWidth = Math.max(0, measuredW * progress);
    ctx.beginPath();
    ctx.rect(textLeft - 8 * s, 0, clipWidth + 16 * s, h);
    ctx.clip();
    ctx.fillStyle = mainColor;
    ctx.fillText(active.text, w / 2 + glitchJitter * 0.3, mainY, maxMainW);
    ctx.restore();

    // Glitch 水平切片位移微闪
    if (isGlitch && glitchIntensity > 0.2 && clamp(this.fx.lyricGlitchSlice ?? 0.72, 0, 2) > 0.1) {
      const sliceOffset =
        Math.cos(this.elapsedSec * 57 * (this.fx.lyricGlitchRate || 1)) *
        8 *
        s *
        clamp(this.fx.lyricGlitchSlice ?? 0.72, 0, 2);
      ctx.save();
      ctx.beginPath();
      ctx.rect(0, mainY - 8 * s, w, 14 * s);
      ctx.clip();
      ctx.globalAlpha = 0.55;
      ctx.fillStyle = mainColor;
      ctx.fillText(active.text, w / 2 + sliceOffset, mainY, maxMainW);
      ctx.restore();
    }
    ctx.restore();

    // 副行翻译 / 音译渲染
    const transMode = String(this.fx.lyricTranslationMode || 'multi');
    if (transMode !== 'off') {
      const subLines: string[] = [];
      if ((transMode === 'translation' || transMode === 'multi') && active.translation) {
        subLines.push(active.translation);
      }
      if ((transMode === 'roma' || transMode === 'multi') && active.roma) {
        subLines.push(active.roma);
      }
      if (subLines.length > 0) {
        const transScale = clamp(this.fx.lyricTranslationScale ?? 0.65, 0.4, 1.1);
        const transSize = Math.round(48 * transScale * s);
        const transGap = clamp(this.fx.lyricTranslationGap ?? 0.92, 0.4, 2) * 34 * s;
        ctx.save();
        ctx.globalAlpha = clamp(this.fx.lyricTranslationOpacity ?? 0.86, 0.1, 1);
        ctx.font = `${Math.max(400, weight - 150)} ${transSize}px ${fontFamily}`;
        ctx.fillStyle = subColor;
        subLines.slice(0, 2).forEach((lineText, idx) => {
          ctx.fillText(lineText, w / 2, h * 0.63 + idx * transGap, w * 0.88);
        });
        ctx.restore();
      }
    }

    if (nextLine?.text) {
      ctx.save();
      ctx.globalAlpha = clamp(this.fx.lyricContextOpacity, 0.1, 0.9);
      ctx.font = `${weight} ${Math.round(34 * s)}px ${fontFamily}`;
      ctx.fillStyle = subColor;
      ctx.fillText(nextLine.text, w / 2, h * 0.88, w * 0.88);
      ctx.restore();
    }

    this.lyricTexture.needsUpdate = true;
  }

  // ==================== 3D 歌单架 ====================

  private raycastShelf(ndc: THREE.Vector2): { slotIndex: number; item: ShelfItem; itemIndex: number } | null {
    if (!this.camera || !this.shelfGroup || !this.shelfGroup.visible) return null;
    const meshes = this.shelfSlots.filter((s) => s.mesh.visible && s.item).map((s) => s.mesh);
    if (!meshes.length) return null;
    this.raycaster.setFromCamera(ndc, this.camera);
    const hits = this.raycaster.intersectObjects(meshes, false);
    if (!hits.length) return null;
    const slotIndex = Number(hits[0].object.userData.slotIndex);
    const slot = this.shelfSlots[slotIndex];
    if (!slot || !slot.item) return null;
    return { slotIndex, item: slot.item, itemIndex: slot.itemIndex };
  }

  private drawShelfCard(slot: ShelfCardSlot, item: ShelfItem, isCenter: boolean, isHovered: boolean): void {
    const key = `${item.title}|${item.sub}|${item.tag}|${isCenter ? 1 : 0}|${isHovered ? 1 : 0}|${this.fx.shelfAccentColor}|${this.fx.shelfBgOpacity}`;
    if (slot.drawKey === key) return;
    slot.drawKey = key;

    const { canvas, ctx, texture } = slot;
    const W = canvas.width;
    const H = canvas.height;
    ctx.clearRect(0, 0, W, H);

    const bgAlpha = clamp(this.fx.shelfBgOpacity ?? 0.79, 0.15, 0.96);
    ctx.fillStyle = isCenter
      ? `rgba(22, 26, 34, ${Math.min(0.95, bgAlpha + 0.12)})`
      : `rgba(12, 15, 20, ${bgAlpha})`;
    ctx.beginPath();
    ctx.roundRect?.(6, 6, W - 12, H - 12, 20);
    if (!ctx.roundRect) ctx.rect(6, 6, W - 12, H - 12);
    ctx.fill();

    ctx.strokeStyle = isHovered || isCenter ? this.fx.shelfAccentColor || '#ffffff' : 'rgba(255,255,255,0.22)';
    ctx.lineWidth = isHovered ? 3 : isCenter ? 2.2 : 1.2;
    ctx.stroke();

    // 封面占位 / 已缓存图片
    const thumbSize = H - 36;
    ctx.fillStyle = 'rgba(255,255,255,0.08)';
    ctx.fillRect(20, 18, thumbSize, thumbSize);
    if (item.cover) {
      const resolvedCover = coverProxyUrl(item.cover);
      const cached = this.shelfCoverCache.get(resolvedCover);
      if (cached && cached.complete && cached.naturalWidth > 0) {
        ctx.drawImage(cached, 20, 18, thumbSize, thumbSize);
      } else if (!cached && typeof Image !== 'undefined') {
        const img = new Image();
        img.crossOrigin = 'anonymous';
        img.onload = () => {
          slot.drawKey = '';
        };
        img.src = resolvedCover;
        this.shelfCoverCache.set(resolvedCover, img);
        if (this.shelfCoverCache.size > 48) {
          const firstKey = this.shelfCoverCache.keys().next().value;
          if (firstKey) this.shelfCoverCache.delete(firstKey);
        }
      }
    }

    const tx = 20 + thumbSize + 20;
    ctx.fillStyle = 'rgba(255,255,255,0.65)';
    ctx.font = '600 20px Inter, "Noto Sans SC", sans-serif';
    ctx.fillText(item.tag || '', tx, 46);

    ctx.fillStyle = '#ffffff';
    ctx.font = '700 30px Inter, "Noto Sans SC", sans-serif';
    ctx.fillText(item.title || '', tx, 94, W - tx - 22);

    ctx.fillStyle = 'rgba(255,255,255,0.66)';
    ctx.font = '400 22px Inter, "Noto Sans SC", sans-serif';
    ctx.fillText(item.sub || '', tx, 134, W - tx - 22);

    texture.needsUpdate = true;
  }

  private updateShelf(dt: number): void {
    if (!this.shelfGroup) return;
    const targetVis = this.shelfVisible && this.shelfItems.length > 0 ? 1 : 0;
    this.shelfVisibilityMix += (targetVis - this.shelfVisibilityMix) * Math.min(1, dt * 8);
    if (this.shelfVisibilityMix < 0.01 && targetVis === 0) {
      this.shelfGroup.visible = false;
      return;
    }
    this.shelfGroup.visible = true;
    this.shelfCenterSmooth += (this.shelfCenterTarget - this.shelfCenterSmooth) * Math.min(1, dt * 10);

    const centerIdx = Math.round(this.shelfCenterSmooth);
    if (centerIdx !== this.shelfLastCenterIdx) {
      this.shelfLastCenterIdx = centerIdx;
      this.callbacks.onShelfCenterChange?.(this.shelfItems[centerIdx] || null, centerIdx);
    }
    const startIdx = Math.max(0, centerIdx - SHELF_VISIBLE_RADIUS);
    const shelfScale = clamp(this.fx.shelfSize ?? 0.92, 0.4, 2);
    this.shelfGroup.position.set(
      (this.fx.shelfOffsetX ?? -0.34) + (this.fx.shelf === 'side' ? 1.85 : 0),
      this.fx.shelfOffsetY ?? -0.2,
      this.fx.shelfOffsetZ ?? 0.12
    );
    this.shelfGroup.rotation.y = ((this.fx.shelfAngleY ?? -11) * Math.PI) / 180;
    this.shelfGroup.scale.setScalar(shelfScale);

    const baseOpacity = clamp(this.fx.shelfOpacity ?? 1, 0, 1) * this.shelfVisibilityMix;

    for (let s = 0; s < this.shelfSlots.length; s++) {
      const slot = this.shelfSlots[s];
      const itemIdx = startIdx + s;
      const item = this.shelfItems[itemIdx];
      if (!item || Math.abs(itemIdx - this.shelfCenterSmooth) > SHELF_VISIBLE_RADIUS + 0.5) {
        slot.mesh.visible = false;
        slot.item = null;
        slot.itemIndex = -1;
        continue;
      }
      slot.mesh.visible = true;
      slot.item = item;
      slot.itemIndex = itemIdx;

      const offset = itemIdx - this.shelfCenterSmooth;
      const absOff = Math.abs(offset);
      const isCenter = absOff < 0.35;
      const isHovered = s === this.shelfHoveredSlot;
      slot.mesh.position.set(
        Math.sin(offset * 0.16) * 0.28 + (isHovered ? -0.14 : 0),
        -offset * 0.78,
        -absOff * 0.26 + (isCenter ? 0.18 : 0)
      );
      slot.mesh.scale.setScalar(isCenter ? 1.06 : Math.max(0.76, 1 - absOff * 0.06));
      slot.mesh.material.opacity = baseOpacity * Math.max(0.22, 1 - absOff * 0.18);
      this.drawShelfCard(slot, item, isCenter, isHovered);
    }
  }

  // ==================== 主渲染循环 ====================

  private startLoop(): void {
    const tick = (now: number) => {
      if (this.disposed) return;
      this.rafId = requestAnimationFrame(tick);
      const dt = clamp((now - this.prevFrameTime) / 1000, 0.001, 0.08);
      this.prevFrameTime = now;
      this.stepFrame(dt);
    };
    this.rafId = requestAnimationFrame(tick);
  }

  private computeBeatMapPulse(currentTime: number): number {
    if (!this.beatMap) return 0;
    const beats =
      (this.beatMap.cameraBeats && this.beatMap.cameraBeats.length > 0
        ? this.beatMap.cameraBeats
        : this.beatMap.kicks && this.beatMap.kicks.length > 0
        ? this.beatMap.kicks
        : this.beatMap.beats) || [];
    if (!beats.length) return 0;

    let lo = 0;
    let hi = beats.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (beats[mid] <= currentTime) lo = mid + 1;
      else hi = mid - 1;
    }
    const prevBeat = hi >= 0 ? beats[hi] : -999;
    const dtSince = currentTime - prevBeat;
    if (dtSince >= 0 && dtSince <= 0.16) {
      return clamp(1 - dtSince / 0.16, 0, 1);
    }
    return 0;
  }

  private updateFreeCamera(dt: number): void {
    if (!this.camera) return;
    const fc = this.freeCamera;
    if (fc.resetting) {
      const lerpAlpha = Math.min(1, dt * 6);
      fc.pos.lerp(new THREE.Vector3(0, 0, 6.6), lerpAlpha);
      fc.yaw += (0 - fc.yaw) * lerpAlpha;
      fc.pitch += (0.08 - fc.pitch) * lerpAlpha;
      fc.fov += (45 - fc.fov) * lerpAlpha;
      if (
        fc.pos.distanceTo(new THREE.Vector3(0, 0, 6.6)) < 0.02 &&
        Math.abs(fc.yaw) < 0.01 &&
        Math.abs(fc.pitch - 0.08) < 0.01
      ) {
        fc.pos.set(0, 0, 6.6);
        fc.yaw = 0;
        fc.pitch = 0.08;
        fc.fov = 45;
        fc.resetting = false;
      }
    } else {
      const moveSpeed = 4.2 * dt;
      const forward = new THREE.Vector3(
        -Math.sin(fc.yaw) * Math.cos(fc.pitch),
        Math.sin(fc.pitch),
        -Math.cos(fc.yaw) * Math.cos(fc.pitch)
      ).normalize();
      const right = new THREE.Vector3().crossVectors(forward, new THREE.Vector3(0, 1, 0)).normalize();
      if (fc.keys.has('KeyW')) fc.pos.addScaledVector(forward, moveSpeed);
      if (fc.keys.has('KeyS')) fc.pos.addScaledVector(forward, -moveSpeed);
      if (fc.keys.has('KeyD')) fc.pos.addScaledVector(right, moveSpeed);
      if (fc.keys.has('KeyA')) fc.pos.addScaledVector(right, -moveSpeed);
      if (fc.keys.has('Space')) fc.pos.y += moveSpeed;
      if (fc.keys.has('ControlLeft') || fc.keys.has('ControlRight')) fc.pos.y -= moveSpeed;
      if (fc.keys.has('KeyQ')) fc.yaw += 1.4 * dt;
      if (fc.keys.has('KeyE')) fc.yaw -= 1.4 * dt;
    }

    if (Math.abs(this.camera.fov - fc.fov) > 0.05) {
      this.camera.fov = fc.fov;
      this.camera.updateProjectionMatrix();
    }
    this.camera.position.copy(fc.pos);
    const lookTarget = new THREE.Vector3(
      fc.pos.x - Math.sin(fc.yaw) * Math.cos(fc.pitch),
      fc.pos.y + Math.sin(fc.pitch),
      fc.pos.z - Math.cos(fc.yaw) * Math.cos(fc.pitch)
    );
    this.camera.lookAt(lookTarget);
  }

  private stepFrame(dt: number): void {
    if (!this.renderer || !this.scene || !this.camera || !this.uniforms) return;
    const metrics: AudioReactiveMetrics = audioEngine.getAudioReactiveMetrics();
    this.syncPresetGlobals();

    this.elapsedSec += dt;
    this.uniforms.uTime.value = this.elapsedSec;
    this.uniforms.uBass.value = metrics.bass;
    this.uniforms.uMid.value = metrics.mid;
    this.uniforms.uTreble.value = metrics.treble;
    this.uniforms.uEnergy.value = metrics.energy;
    this.uniforms.uBeat.value = metrics.beatPulse;

    // Preset 8 时优雅淡出主粒子让 #sonic-workshop-layer 显现
    const targetParticleAlpha = this.fx.preset === 8 ? 0 : 1;
    this.particleGroupAlpha += (targetParticleAlpha - this.particleGroupAlpha) * Math.min(1, dt * 6);
    this.uniforms.uAlpha.value = this.particleGroupAlpha;
    if (this.particles) {
      this.particles.visible = this.particleGroupAlpha > 0.01 && this.fx.preset !== 3;
      this.particles.rotation.set(this.gestureRotation.x, this.gestureRotation.y, 0);
    }
    if (this.bloomParticles) {
      this.bloomParticles.visible =
        Boolean(this.fx.bloom) && this.particleGroupAlpha > 0.01 && this.fx.preset !== 8;
      this.bloomParticles.rotation.set(this.gestureRotation.x, this.gestureRotation.y, 0);
    }
    if (this.backCoverParticles) {
      this.backCoverParticles.visible =
        Boolean(this.fx.backCover) &&
        this.particleGroupAlpha > 0.01 &&
        this.fx.preset !== 3 &&
        this.fx.preset !== SKULL_PRESET_INDEX &&
        this.fx.preset !== 8;
      this.backCoverParticles.rotation.set(
        this.gestureRotation.x,
        this.gestureRotation.y + Math.PI,
        0
      );
    }

    if (this.colorMixT < 1) {
      this.colorMixT = Math.min(1, this.colorMixT + dt * 1.4);
      this.uniforms.uColorMixT.value = this.colorMixT;
    }
    if (this.burstAmt > 0) {
      this.burstAmt = Math.max(0, this.burstAmt - dt * 1.6);
      this.uniforms.uBurstAmt.value = this.burstAmt;
    }
    if (this.fx.preset === 4) {
      this.vinylSpin += dt * (this.playing ? 0.85 : 0.15);
      this.uniforms.uVinylSpin.value = this.vinylSpin;
    }

    if (metrics.isBeat) {
      this.spawnRipple((Math.random() - 0.5) * 2.2, (Math.random() - 0.5) * 2.2, 0.65 + metrics.bass * 0.5);
    }

    // 更新涟漪数据纹理
    let activeRipples = 0;
    for (let i = 0; i < RIPPLE_MAX; i++) {
      const rp = this.ripples[i];
      if (rp.age >= 0 && rp.age <= 3.2) {
        rp.age += dt;
        this.rippleData[i * 4] = rp.x;
        this.rippleData[i * 4 + 1] = rp.y;
        this.rippleData[i * 4 + 2] = rp.age;
        this.rippleData[i * 4 + 3] = rp.str;
        activeRipples = i + 1;
      } else {
        this.rippleData[i * 4 + 2] = -10;
        this.rippleData[i * 4 + 3] = 0;
      }
    }
    this.uniforms.uRippleCount.value = activeRipples;
    if (this.rippleTex) this.rippleTex.needsUpdate = true;

    // 自由轨道惯性与电影/BeatMap 运镜
    if (!this.orbit.rotating) {
      this.orbit.userTheta += this.orbit.velTheta;
      this.orbit.userPhi = clamp(this.orbit.userPhi + this.orbit.velPhi, -Math.PI * 0.45, Math.PI * 0.45);
      this.orbit.velTheta *= 0.9;
      this.orbit.velPhi *= 0.9;
    }
    this.orbit.theta += (this.orbit.userTheta - this.orbit.theta) * Math.min(1, dt * 8);
    this.orbit.phi += (this.orbit.userPhi - this.orbit.phi) * Math.min(1, dt * 8);
    this.orbit.radius += (this.orbit.userRadius - this.orbit.radius) * Math.min(1, dt * 8);

    const shake = this.fx.cinema ? clamp(this.fx.cinemaShake ?? 0.5, 0, 1.8) : 0;
    const beatMapPulse = this.playing ? this.computeBeatMapPulse(metrics.currentTime) : 0;
    const pulseAmt = Math.max(metrics.beatPulse, beatMapPulse * 0.85);
    const cineTheta = Math.sin(this.elapsedSec * 0.14) * 0.055 * shake + beatMapPulse * 0.014 * shake * Math.sin(this.elapsedSec * 2.4);
    const cinePhi = Math.cos(this.elapsedSec * 0.11) * 0.032 * shake;
    const cineRad = -pulseAmt * 0.16 * shake;

    if (this.freeCamera.active) {
      if (this.skullGroup && this.fx.preset !== SKULL_PRESET_INDEX) {
        this.skullGroup.visible = false;
      }
      this.updateFreeCamera(dt);
    } else if (this.fx.preset === SKULL_PRESET_INDEX && this.skullGroup) {
      this.skullOpacity += (1 - this.skullOpacity) * Math.min(1, dt * 3.2);
      this.skullWheelZoom += (this.skullWheelZoomTarget - this.skullWheelZoom) * Math.min(1, dt * 8);
      this.skullGroup.visible = true;
      this.skullGroup.rotation.set(
        -0.26 + this.gestureRotation.x * 0.65,
        this.gestureRotation.y * 0.65,
        0
      );
      const su = (this.skullGroup.material as THREE.ShaderMaterial).uniforms;
      su.uOpacity.value = this.skullOpacity;
      su.uSkullFlash.value = pulseAmt;
      su.uJawOpen.value = clamp(0.6 + metrics.bass * 0.09 + pulseAmt * 0.1, 0.52, 0.88);
      this.camera.position.set(0, -2.5, 4.98 + this.skullWheelZoom);
      this.camera.lookAt(0, -0.2, 0.02);
    } else {
      if (this.skullGroup) {
        this.skullOpacity += (0 - this.skullOpacity) * Math.min(1, dt * 3.2);
        this.skullGroup.visible = this.skullOpacity > 0.01;
        (this.skullGroup.material as THREE.ShaderMaterial).uniforms.uOpacity.value = this.skullOpacity;
      }
      const totalTheta = this.orbit.theta + cineTheta;
      const totalPhi = this.orbit.phi + cinePhi;
      const totalRad = Math.max(2.0, this.orbit.radius + cineRad);
      this.camera.position.set(
        totalRad * Math.cos(totalPhi) * Math.sin(totalTheta),
        totalRad * Math.sin(totalPhi),
        totalRad * Math.cos(totalPhi) * Math.cos(totalTheta)
      );
      this.camera.lookAt(0, 0, 0);
    }

    // Preset 7 & Preset 8 外部插件更新
    if (typeof window !== 'undefined') {
      const audioFrame = {
        bass: metrics.bass,
        mid: metrics.mid,
        treble: metrics.treble,
        energy: metrics.energy,
        beat: pulseAmt,
        kickEnvelope: metrics.kickLevel,
        subBass: clamp(metrics.bass * 0.8 + pulseAmt * 0.2, 0, 1),
        lowMid: clamp(metrics.mid * 0.7 + metrics.bass * 0.3, 0, 1),
        highMid: clamp(metrics.mid * 0.5 + metrics.treble * 0.5, 0, 1),
        presence: metrics.treble,
        brilliance: metrics.treble,
        air: clamp(metrics.treble * 0.8, 0, 1),
      };
      const pluginCtx = {
        fx: this.fx,
        scene: this.scene,
        camera: this.camera,
        audio: audioFrame,
        time: this.elapsedSec,
      };
      window.MineradioSonicTopography?.update?.(dt, pluginCtx);
      window.MineradioSonicWorkshop?.update?.(dt, pluginCtx);
    }

    // 舞台歌词位置与低配帧预算控制（每帧最多上传 1 个 CanvasTexture）
    if (this.lyricsGroup) {
      this.lyricsGroup.visible = Boolean(this.fx.particleLyrics);
      const floatY = this.fx.lyricVerticalFloat
        ? Math.sin(this.elapsedSec * 1.15) * 0.035
        : 0;
      this.lyricsGroup.position.set(
        this.fx.lyricOffsetX || 0,
        0.2 + (this.fx.lyricOffsetY || 0) + floatY,
        1.46 + (this.fx.lyricOffsetZ || 0)
      );
      this.lyricsGroup.scale.setScalar(clamp(this.fx.lyricScale ?? 1, 0.4, 2.2));
      this.lyricsGroup.rotation.set(
        ((this.fx.lyricTiltX || 0) * Math.PI) / 180,
        ((this.fx.lyricTiltY || 0) * Math.PI) / 180,
        0
      );
      if (this.lyricDirty) {
        const nowMs = typeof performance !== 'undefined' ? performance.now() : Date.now();
        if (nowMs - this.lastLyricUploadAt >= 16) {
          let uploaded = 0;
          if (uploaded < MAX_NEW_LYRIC_TEXTURES_PER_FRAME) {
            this.renderStageLyricsTexture();
            this.lyricDirty = false;
            this.lastLyricUploadAt = nowMs;
            uploaded++;
          }
        }
      }
    }

    // 星河背景显隐
    if (this.starRiverParticles && this.starRiverUniforms) {
      const showStar =
        this.fx.backgroundStarRiver !== false && this.fx.preset !== 5 && this.fx.preset !== 8;
      this.starRiverParticles.visible = showStar;
      this.starRiverUniforms.uAlpha.value = showStar ? 0.34 : 0;
    }

    this.updateShelf(dt);
    this.renderer.render(this.scene, this.camera);
  }

  public getDebugSnapshot(): {
    mounted: boolean;
    preset: number;
    lyricCount: number;
    shelfCount: number;
    lyricCurrentTime: number;
  } {
    return {
      mounted: this.mounted,
      preset: this.fx.preset,
      lyricCount: this.lyricLines.length,
      shelfCount: this.shelfItems.length,
      lyricCurrentTime: this.lyricCurrentTime,
    };
  }

  public dispose(): void {
    this.disposed = true;
    this.mounted = false;
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.rafId = 0;
    }
    this.unbindDomEvents?.();
    this.unbindDomEvents = null;
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;

    this.particles?.geometry.dispose();
    (this.particles?.material as THREE.Material | undefined)?.dispose();
    (this.bloomParticles?.material as THREE.Material | undefined)?.dispose();
    (this.backCoverParticles?.material as THREE.Material | undefined)?.dispose();
    if (typeof window !== 'undefined') {
      window.MineradioSonicTopography?.clear?.();
      window.MineradioSonicWorkshop?.clear?.();
    }
    this.starRiverParticles?.geometry.dispose();
    (this.starRiverParticles?.material as THREE.Material | undefined)?.dispose();
    this.skullGroup?.geometry.dispose();
    (this.skullGroup?.material as THREE.Material | undefined)?.dispose();
    this.lyricMesh?.geometry.dispose();
    this.lyricMesh?.material.dispose();
    this.lyricTexture?.dispose();
    this.dotTexture?.dispose();
    this.coverTex?.dispose();
    this.prevCoverTex?.dispose();
    this.coverEdgeTex?.dispose();
    this.rippleTex?.dispose();
    for (const slot of this.shelfSlots) {
      slot.mesh.geometry.dispose();
      slot.mesh.material.dispose();
      slot.texture.dispose();
    }
    this.shelfSlots = [];
    this.shelfCoverCache.clear();
    this.renderer?.dispose();
    this.renderer?.domElement?.remove();

    this.scene = null;
    this.camera = null;
    this.renderer = null;
    this.particles = null;
    this.bloomParticles = null;
    this.backCoverParticles = null;
    this.starRiverParticles = null;
    this.skullGroup = null;
    this.lyricsGroup = null;
    this.lyricMesh = null;
    this.shelfGroup = null;
    this.uniforms = null;
  }
}

export const visualEngine = new VisualEngine();
