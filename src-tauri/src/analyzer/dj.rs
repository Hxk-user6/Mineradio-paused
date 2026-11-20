use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::io::Cursor;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const FULL_STREAM_QUALITY_LIMIT_SEC: f64 = 7200.0;
const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_RANGE_SAMPLE_BYTES: usize = 24 * 1024 * 1024;
const INTRO_DECODE_MAX_SEC: f64 = 55.0;
const INTRO_FETCH_MAX_BYTES: usize = 8 * 1024 * 1024;

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clamp01(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(0.0, 1.0)
}

fn clamp_range(v: f64, min: f64, max: f64) -> f64 {
    if !v.is_finite() {
        return min;
    }
    v.clamp(min, max)
}

fn percentile(arr: &[f64], p: f64, max_samples: usize) -> f64 {
    let len = arr.len();
    if len == 0 {
        return 0.001;
    }
    let cap = if max_samples == 0 { 16000 } else { max_samples };
    let mut sample: Vec<f64> = if len <= cap {
        arr.to_vec()
    } else {
        let step = (len - 1) as f64 / (cap - 1) as f64;
        (0..cap)
            .map(|i| {
                let idx = ((i as f64 * step).floor() as usize).min(len - 1);
                let val = arr[idx];
                if val.is_finite() { val } else { 0.0 }
            })
            .collect()
    };
    sample.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sample.len() as f64 * p).floor() as usize).min(sample.len() - 1);
    let out = sample[idx];
    if out == 0.0 || !out.is_finite() { 0.001 } else { out }
}

fn median(vals: &[f64]) -> f64 {
    let mut list: Vec<f64> = vals.iter().copied().filter(|v| v.is_finite()).collect();
    if list.is_empty() {
        return 0.0;
    }
    list.sort_by(|a, b| a.total_cmp(b));
    list[list.len() / 2]
}

#[derive(Clone, Copy)]
enum BiquadType {
    Highpass,
    Lowpass,
}

#[derive(Clone, Copy)]
struct BiquadState {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

fn make_biquad(kind: BiquadType, freq: f64, q: f64, sr: f64) -> BiquadState {
    let f = freq.clamp(8.0, (sr * 0.45).max(8.0));
    let w0 = 2.0 * PI * f / sr;
    let cos = w0.cos();
    let sin = w0.sin();
    let q_eff = if q > 0.0 { q } else { 0.707 };
    let alpha = sin / (2.0 * q_eff);
    let (b0, b1, b2) = match kind {
        BiquadType::Highpass => ((1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5),
        BiquadType::Lowpass => ((1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5),
    };
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos;
    let a2 = 1.0 - alpha;
    let inv = 1.0 / a0;
    BiquadState {
        b0: b0 * inv,
        b1: b1 * inv,
        b2: b2 * inv,
        a1: a1 * inv,
        a2: a2 * inv,
        x1: 0.0,
        x2: 0.0,
        y1: 0.0,
        y2: 0.0,
    }
}

fn run_biquad(st: &mut BiquadState, x: f64) -> f64 {
    let y = st.b0 * x + st.b1 * st.x1 + st.b2 * st.x2 - st.a1 * st.y1 - st.a2 * st.y2;
    st.x2 = st.x1;
    st.x1 = x;
    st.y2 = st.y1;
    st.y1 = y;
    y
}

#[derive(Clone, Copy)]
struct OnsetCandidate {
    frame: usize,
    time: f64,
    low_tone: f64,
    hit_tone: f64,
    power: f64,
}

fn estimate_step(list: &[OnsetCandidate]) -> f64 {
    if list.len() < 3 {
        return 0.0;
    }
    let bin = 0.006;
    let mut hist: HashMap<i64, f64> = HashMap::new();
    let mut med_gaps = Vec::new();
    for ai in 0..list.len() {
        let end_bi = (ai + 10).min(list.len());
        for bi in (ai + 1)..end_bi {
            let raw_gap = list[bi].time - list[ai].time;
            if raw_gap < 0.24 {
                continue;
            }
            if raw_gap > 2.55 {
                break;
            }
            for div in 1..=6 {
                let g = raw_gap / div as f64;
                if g < 0.31 {
                    break;
                }
                if g > 0.86 {
                    continue;
                }
                let weight = (list[ai].power * list[bi].power).max(0.001).sqrt()
                    / (((bi - ai) * div) as f64).sqrt();
                let key = (g / bin).round() as i64;
                *hist.entry(key).or_insert(0.0) += weight;
                med_gaps.push(g);
            }
        }
    }
    let mut best_key: Option<i64> = None;
    let mut best_score = 0.0;
    for &key in hist.keys() {
        let score = hist.get(&key).copied().unwrap_or(0.0)
            + hist.get(&(key - 1)).copied().unwrap_or(0.0) * 0.72
            + hist.get(&(key + 1)).copied().unwrap_or(0.0) * 0.72;
        if score > best_score {
            best_score = score;
            best_key = Some(key);
        }
    }
    match best_key {
        Some(k) => k as f64 * bin,
        None => median(&med_gaps),
    }
}

fn build_beat_map_from_low_energy(
    low_energy: &[f64],
    hit_energy: &[f64],
    hop_sec: f64,
    duration_sec: f64,
) -> Value {
    let n_frames = low_energy.len().min(hit_energy.len());
    if n_frames < 20 {
        return json!({
            "kicks": [],
            "beats": [],
            "pulseBeats": [],
            "cameraBeats": [],
            "duration": duration_sec,
            "visualBeatCount": 0,
            "tempoSource": "podcast-dj-server-empty",
            "analyzedAt": now_epoch_ms(),
        });
    }

    let band_at = |arr: &[f64], idx: usize| -> f64 {
        let i = idx.min(n_frames - 1);
        let a = arr[i.saturating_sub(1)];
        let b = arr[i];
        let c = arr[(i + 1).min(n_frames - 1)];
        (a + b * 2.0 + c) * 0.25
    };

    let low_floor = percentile(low_energy, 0.22, 16000).max(0.0004);
    let low_mid = percentile(low_energy, 0.58, 16000).max(low_floor + 0.0002);
    let low_ref = percentile(low_energy, 0.86, 16000).max(low_mid + 0.0002);
    let low_ceil = percentile(low_energy, 0.96, 16000).max(low_ref + 0.0004);
    let hit_ref = percentile(hit_energy, 0.86, 16000).max(0.0004);

    let mut onset = vec![0.0; n_frames];
    for i in 4..n_frames {
        let prev = low_energy[i - 1] * 0.62 + low_energy[i - 2] * 0.28 + low_energy[i - 3] * 0.10;
        let low_rise = (low_energy[i] - prev).max(0.0);
        let wide_rise = ((low_energy[i] + low_energy[i - 1]) * 0.5
            - (low_energy[i - 3] + low_energy[i - 4]) * 0.5)
            .max(0.0);
        let peak_rise = (hit_energy[i] - hit_energy[i - 2] * 0.84).max(0.0);
        onset[i] = low_rise * 1.72 + wide_rise * 0.86 + peak_rise * 0.10;
    }

    let win_n = ((0.82 / hop_sec).round() as usize).max(52);
    let min_frame_gap = ((0.215 / hop_sec).round() as usize).max(18);
    let mut candidates: Vec<OnsetCandidate> = Vec::new();

    if n_frames > win_n + 8 {
        let mut sum_o = 0.0;
        let mut sq_o = 0.0;
        for &o in &onset[..win_n] {
            sum_o += o;
            sq_o += o * o;
        }
        for f in (win_n + 4)..(n_frames - 4) {
            let mean = sum_o / win_n as f64;
            let var = (sq_o / win_n as f64 - mean * mean).max(0.0);
            let std = var.sqrt();
            let th = mean + std * 1.66 + low_ref * 0.0038;
            let o = onset[f];
            if o > th && o >= onset[f - 1] && o > onset[f + 1] {
                let mut peak_f = f;
                let mut peak_score = o + low_energy[f] * 0.10;
                for pf in (f.saturating_sub(2))..=((f + 3).min(n_frames - 1)) {
                    let ps = onset[pf] + low_energy[pf] * 0.10;
                    if ps > peak_score {
                        peak_score = ps;
                        peak_f = pf;
                    }
                }
                let low_tone = (band_at(low_energy, peak_f) / low_ref).min(2.6);
                let hit_tone = (band_at(hit_energy, peak_f) / hit_ref).min(2.6);
                let low_rel = clamp01(
                    (band_at(low_energy, peak_f) - low_floor) / (low_ceil - low_floor).max(0.0001),
                );
                let score = (o - th) / (std + mean * 0.38 + low_ref * 0.012).max(0.0006);
                if score > 0.16 && (low_tone > 0.32 || low_rel > 0.22 || hit_tone > 0.52) {
                    let power = score * 0.56
                        + clamp01((low_tone - 0.22) / 1.42).powf(0.82) * 0.34
                        + hit_tone.min(1.5) * 0.08
                        + low_rel * 0.10;
                    let cand = OnsetCandidate {
                        frame: peak_f,
                        time: peak_f as f64 * hop_sec,
                        low_tone,
                        hit_tone,
                        power,
                    };
                    if let Some(last) = candidates.last_mut() {
                        if cand.frame - last.frame < min_frame_gap {
                            if cand.power > last.power {
                                *last = cand;
                            }
                        } else {
                            candidates.push(cand);
                        }
                    } else {
                        candidates.push(cand);
                    }
                }
            }
            let old = onset[f - win_n];
            let next = onset[f];
            sum_o += next - old;
            sq_o += next * next - old * old;
        }
    }

    let duration = if duration_sec > 0.0 {
        duration_sec
    } else {
        n_frames as f64 * hop_sec
    };

    if candidates.is_empty() {
        return json!({
            "kicks": [],
            "beats": [],
            "pulseBeats": [],
            "cameraBeats": [],
            "duration": duration,
            "visualBeatCount": 0,
            "tempoSource": "podcast-dj-server-empty",
            "analyzedAt": now_epoch_ms(),
        });
    }

    let powers: Vec<f64> = candidates.iter().map(|c| c.power).collect();
    let p30 = percentile(&powers, 0.30, 16000);
    let p50 = percentile(&powers, 0.50, 16000);
    let p90 = percentile(&powers, 0.90, 16000).max(p50 + 0.001);
    let p96 = percentile(&powers, 0.965, 16000).max(p90 + 0.001);

    let mut strong: Vec<OnsetCandidate> = candidates
        .iter()
        .copied()
        .filter(|c| c.power >= p50 && c.low_tone > 0.34)
        .collect();
    if strong.len() < 16 {
        strong = candidates.clone();
    }

    let mut global_step = estimate_step(&strong);
    if global_step == 0.0 {
        global_step = estimate_step(&candidates);
    }
    if global_step == 0.0 {
        global_step = 0.50;
    }
    global_step = clamp_range(global_step, 0.32, 0.86);

    let score_phase = |anchor_time: f64, step: f64| -> f64 {
        let mut start = anchor_time;
        while start - step > 0.05 {
            start -= step;
        }
        let end = duration.min(180.0);
        let win = (step * 0.18).clamp(0.055, 0.125);
        let mut score = 0.0;
        let mut count = 0usize;
        let mut cursor = 0usize;
        let mut gt = start;
        while gt < end {
            while cursor < candidates.len() && candidates[cursor].time < gt - win {
                cursor += 1;
            }
            let mut best_s = 0.0;
            let mut pi = cursor;
            while pi < candidates.len() && candidates[pi].time <= gt + win {
                let dist = (candidates[pi].time - gt).abs();
                let s = candidates[pi].power * (1.0 - dist / win * 0.44);
                if s > best_s {
                    best_s = s;
                }
                pi += 1;
            }
            score += if best_s > 0.0 { best_s } else { -p30 * 0.08 };
            count += 1;
            gt += step;
        }
        if count > 0 {
            score / count as f64
        } else {
            f64::NEG_INFINITY
        }
    };

    let limit_time = duration.min(180.0);
    let mut phase_source: Vec<OnsetCandidate> = strong
        .iter()
        .copied()
        .filter(|c| c.time < limit_time)
        .take(72)
        .collect();
    if phase_source.is_empty() && !strong.is_empty() {
        phase_source.push(strong[0]);
    }

    let mut best_anchor = phase_source.first().map(|c| c.time).unwrap_or(0.0);
    let mut best_anchor_score = f64::NEG_INFINITY;
    for cand in &phase_source {
        let s = score_phase(cand.time, global_step);
        if s > best_anchor_score {
            best_anchor_score = s;
            best_anchor = cand.time;
        }
    }
    let half_step = global_step * 0.5;
    if half_step >= 0.31 {
        let half_score = score_phase(best_anchor, half_step);
        if half_score > best_anchor_score * 1.04 {
            global_step = half_step;
        }
    }
    let mut anchor = best_anchor;
    while anchor - global_step > 0.05 {
        anchor -= global_step;
    }

    let section_len = if duration > 3600.0 { 96.0 } else { 72.0 };
    let section_count = ((duration / section_len).ceil() as usize).max(1);
    let mut section_steps = Vec::with_capacity(section_count);
    for si in 0..section_count {
        let t0 = si as f64 * section_len;
        let t1 = duration.min(t0 + section_len);
        let seg: Vec<OnsetCandidate> = strong
            .iter()
            .copied()
            .filter(|c| c.time >= t0 && c.time < t1)
            .collect();
        let prev_step = section_steps.last().copied().unwrap_or(global_step);
        let mut local_step = estimate_step(&seg);
        if local_step == 0.0 {
            local_step = prev_step;
        }
        if prev_step > 0.0 {
            local_step = clamp_range(local_step, prev_step * 0.94, prev_step * 1.06);
        }
        if global_step > 0.0 {
            local_step = clamp_range(local_step, global_step * 0.86, global_step * 1.14);
        }
        let smoothed = if !section_steps.is_empty() {
            local_step * 0.30 + prev_step * 0.70
        } else {
            local_step
        };
        section_steps.push(smoothed);
    }

    let step_at = |time: f64| -> f64 {
        let idx = ((time / section_len).floor() as isize).clamp(0, (section_steps.len() as isize) - 1) as usize;
        let s = section_steps[idx];
        if s > 0.0 { s } else { global_step }
    };

    let nearest_candidate = |center: f64, window_sec: f64, start_idx: usize| -> Option<OnsetCandidate> {
        let mut best: Option<OnsetCandidate> = None;
        let mut best_score = f64::NEG_INFINITY;
        let mut j = start_idx;
        while j < candidates.len() && candidates[j].time < center - window_sec {
            j += 1;
        }
        let mut ni = j;
        while ni < candidates.len() && candidates[ni].time <= center + window_sec {
            let dist = (candidates[ni].time - center).abs();
            let s = candidates[ni].power * (1.0 - dist / window_sec.max(0.001) * 0.42);
            if s > best_score {
                best_score = s;
                best = Some(candidates[ni]);
            }
            ni += 1;
        }
        best
    };

    let mut beats: Vec<Value> = Vec::new();
    let mut kicks: Vec<f64> = Vec::new();
    let mut camera_beats: Vec<Value> = Vec::new();
    let mut pulse_beats: Vec<Value> = Vec::new();

    let mut grid_index = 0usize;
    let mut cursor_idx = 0usize;
    let mut grid_t = anchor;
    while grid_t < duration - 0.04 {
        let local_step = step_at(grid_t);
        let win_sec = (local_step * 0.20).clamp(0.060, 0.135);
        while cursor_idx < candidates.len() && candidates[cursor_idx].time < grid_t - win_sec {
            cursor_idx += 1;
        }
        let best_cand = nearest_candidate(grid_t, win_sec, cursor_idx);
        let gf = ((grid_t / hop_sec).round() as isize).clamp(0, (n_frames as isize) - 1) as usize;
        let grid_low = band_at(low_energy, gf);
        let grid_hit = band_at(hit_energy, gf);
        let grid_low_tone = (grid_low / low_ref).min(2.6);
        let grid_hit_tone = (grid_hit / hit_ref).min(2.6);
        let low_tone = best_cand
            .map(|c| (grid_low_tone * 0.62).max(c.low_tone))
            .unwrap_or(grid_low_tone);
        let hit_tone = best_cand
            .map(|c| (grid_hit_tone * 0.62).max(c.hit_tone))
            .unwrap_or(grid_hit_tone);
        let dist_penalty = best_cand
            .map(|c| 1.0 - ((c.time - grid_t).abs() / win_sec).min(1.0) * 0.26)
            .unwrap_or(0.54);
        let base_power = best_cand
            .map(|c| c.power * dist_penalty)
            .unwrap_or(grid_low_tone * 0.25 + grid_hit_tone * 0.06);
        let power_rel = clamp01((base_power - p30 * 0.78) / (p96 - p30 * 0.78).max(0.001));
        let low_rel = clamp01((grid_low - low_floor) / (low_ceil - low_floor).max(0.0001));
        let kick_rel = clamp01(
            power_rel * 0.74 + low_rel * 0.22 + clamp01((hit_tone - 0.26) / 1.70) * 0.04,
        );
        let soft_grid = (best_cand.is_none() && low_rel < 0.20) || kick_rel < 0.16;
        let slot = grid_index % 4;
        let mut combo = match slot {
            0 => "downbeat",
            1 => "push",
            2 => "drop",
            _ => "rebound",
        };
        if kick_rel > 0.84 && combo != "downbeat" {
            combo = "accent";
        }
        let visual_rel = if kick_rel > 0.76 {
            0.76 + (kick_rel - 0.76) * 0.52
        } else {
            kick_rel
        };
        let down_lift = if combo == "downbeat" {
            if visual_rel > 0.18 {
                0.016 + visual_rel * 0.036
            } else {
                visual_rel * 0.028
            }
        } else {
            0.0
        };
        let section_gate = clamp01((kick_rel - 0.10) / 0.58);
        let mut impact = (0.022 + visual_rel.powf(1.62) * 0.86 + down_lift).clamp(0.020, 0.88);
        let mut strength = (0.13 + visual_rel.powf(1.12) * 0.68 + down_lift * 0.70).clamp(0.12, 0.93);
        if soft_grid {
            let soft_mul = if combo == "downbeat" { 0.48 } else { 0.30 };
            impact *= soft_mul;
            strength *= 0.58 + section_gate * 0.22;
        }
        let timing_pull = best_cand
            .map(|_| 0.24 + clamp01((kick_rel - 0.25) / 0.65) * 0.46)
            .unwrap_or(0.0);
        let source_time = best_cand
            .map(|c| grid_t * (1.0 - timing_pull) + c.time * timing_pull)
            .unwrap_or(grid_t);
        let camera_active = impact >= 0.13
            || (combo == "downbeat" && kick_rel >= 0.14)
            || (best_cand.is_some() && kick_rel >= 0.18);
        let pulse_active = impact > 0.16 || (combo == "downbeat" && kick_rel >= 0.18);
        let low_mix = (0.52 + visual_rel * 0.32 + low_tone * 0.035
            - if combo == "accent" { 0.10 } else { 0.0 })
            .clamp(0.42, 0.90);
        let body_mix = (0.060
            + visual_rel * 0.12
            + if combo == "push" { 0.18 } else { 0.0 }
            + if combo == "drop" { 0.24 } else { 0.0 })
            .clamp(0.035, 0.54);
        let snap_mix = (0.026
            + if combo == "accent" { 0.40 } else { 0.0 }
            + if combo == "rebound" { 0.08 } else { 0.0 }
            + visual_rel * 0.038)
            .clamp(0.015, 0.62);
        let confidence =
            (0.46 + kick_rel * 0.43 + if best_cand.is_some() { 0.08 } else { -0.03 })
                .clamp(0.44, 0.99);
        let mass = (low_mix * 0.72 + visual_rel.powf(1.22) * 0.24).clamp(0.36, 0.94);
        let sharpness = (snap_mix * 1.18).clamp(0.03, 0.28);

        let beat_obj = json!({
            "time": source_time,
            "strength": strength,
            "confidence": confidence,
            "impact": impact,
            "primary": camera_active,
            "camera": camera_active,
            "pulse": pulse_active,
            "tone": "podcast-dj-server-low-grid",
            "low": low_mix,
            "body": body_mix,
            "snap": snap_mix,
            "mass": mass,
            "sharpness": sharpness,
            "combo": combo,
            "step": local_step,
            "index": beats.len(),
            "dj": true,
            "grid": true,
            "kickOnly": true,
            "server": true,
        });
        kicks.push(source_time);
        if camera_active {
            camera_beats.push(beat_obj.clone());
        }
        if pulse_active && (impact >= 0.16 || combo == "downbeat") {
            pulse_beats.push(json!({
                "time": source_time,
                "strength": strength,
                "impact": impact,
                "combo": combo,
                "low": low_mix,
                "body": body_mix,
                "snap": snap_mix,
                "dj": true,
            }));
        }
        beats.push(beat_obj);
        grid_index += 1;
        grid_t += local_step;
    }

    let visual_beat_count = camera_beats.len();
    json!({
        "kicks": kicks,
        "beats": beats,
        "pulseBeats": pulse_beats,
        "cameraBeats": camera_beats,
        "gridStep": global_step,
        "sectionSteps": section_steps,
        "tempoSource": "podcast-dj-server-low-offline",
        "duration": duration,
        "visualBeatCount": visual_beat_count,
        "analyzedAt": now_epoch_ms(),
        "debug": {
            "candidates": candidates.len(),
            "hopSec": hop_sec,
            "lowRef": low_ref,
            "step": global_step,
        },
    })
}

fn sniff_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some("wav");
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"fLaC" {
        return Some("flac");
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return Some("ogg");
    }
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return Some("m4a");
    }
    if bytes.len() >= 3 && &bytes[0..3] == b"ID3" {
        return Some("mp3");
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"ADIF" {
        return Some("aac");
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF {
        let b1 = bytes[1];
        if (b1 & 0xF6) == 0xF0 {
            return Some("aac");
        }
        if (b1 & 0xE0) == 0xE0 {
            return Some("mp3");
        }
    }
    None
}

struct DecodedEnergy {
    low_energy: Vec<f64>,
    hit_energy: Vec<f64>,
    hop_sec: f64,
    duration: f64,
    chunks: usize,
    decoded_samples: usize,
    sample_rate: u32,
    effective_sample_rate: f64,
}

fn decode_bytes_to_energy(
    bytes: &[u8],
    ext_hint: Option<&str>,
    duration_hint: f64,
    max_decode_sec: Option<f64>,
) -> Result<DecodedEnergy, String> {
    if bytes.is_empty() {
        return Err("Empty audio bytes".to_string());
    }

    let mut hint = Hint::new();
    let effective_ext = ext_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| sniff_extension(bytes));
    if let Some(ext) = effective_ext {
        hint.with_extension(ext.trim_start_matches('.'));
    }

    let probe_result = {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
        symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
    };

    let probed = match probe_result {
        Ok(p) => p,
        Err(first_err) => {
            if let Some(sniffed) = sniff_extension(bytes) {
                let mut retry_hint = Hint::new();
                retry_hint.with_extension(sniffed);
                let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
                symphonia::default::get_probe()
                    .format(
                        &retry_hint,
                        mss,
                        &FormatOptions::default(),
                        &MetadataOptions::default(),
                    )
                    .map_err(|e| format!("Audio probe failed: {e}"))?
            } else {
                return Err(format!("Audio probe failed: {first_err}"));
            }
        }
    };

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No supported audio track found".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Audio decoder init failed: {e}"))?;

    let hop_sec = if duration_hint > 4200.0 { 0.0125 } else { 0.010 };
    let mut low_energy: Vec<f64> = Vec::new();
    let mut hit_energy: Vec<f64> = Vec::new();

    let mut hp: Option<BiquadState> = None;
    let mut lp: Option<BiquadState> = None;
    let mut base_sr: u32 = 0;
    let mut effective_sr: f64 = 0.0;
    let mut sample_step: usize = 1;
    let mut hop_size: usize = 0;
    let mut frame_sum: f64 = 0.0;
    let mut frame_peak: f64 = 0.0;
    let mut frame_count: usize = 0;
    let mut effective_samples: usize = 0;
    let mut chunks: usize = 0;
    let mut decoded_samples: usize = 0;

    let mut reached_limit = false;

    loop {
        if reached_limit {
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(err) => return Err(format!("Audio demux failed: {err}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(err) => return Err(format!("Audio decode failed: {err}")),
        };

        chunks += 1;
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let sr = spec.rate.max(8000);
        if effective_sr == 0.0 {
            base_sr = sr;
            sample_step = if sr >= 44100 { 4 } else if sr >= 32000 { 3 } else { 2 };
            effective_sr = sr as f64 / sample_step as f64;
            hop_size = ((effective_sr * hop_sec).floor() as usize).max(80);
            hp = Some(make_biquad(BiquadType::Highpass, 32.0, 0.72, effective_sr));
            lp = Some(make_biquad(BiquadType::Lowpass, 178.0, 0.82, effective_sr));
        }

        let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();
        let n_frames = samples.len() / channels;
        decoded_samples += n_frames;

        let hp_ref = hp.as_mut().expect("highpass initialized");
        let lp_ref = lp.as_mut().expect("lowpass initialized");

        let mut i = 0usize;
        while i < n_frames {
            let base = i * channels;
            let mono = if channels == 1 {
                samples[base] as f64
            } else if channels == 2 {
                (samples[base] as f64 + samples[base + 1] as f64) * 0.5
            } else {
                let mut sum = 0.0;
                for ch in 0..channels {
                    sum += samples[base + ch] as f64;
                }
                sum / channels as f64
            };
            let y = run_biquad(lp_ref, run_biquad(hp_ref, mono));
            let ay = y.abs();
            frame_sum += y * y;
            if ay > frame_peak {
                frame_peak = ay;
            }
            frame_count += 1;
            effective_samples += 1;
            if frame_count >= hop_size {
                let cnt = frame_count.max(1) as f64;
                low_energy.push((frame_sum / cnt).sqrt());
                hit_energy.push(frame_peak);
                frame_sum = 0.0;
                frame_peak = 0.0;
                frame_count = 0;
            }
            if let Some(limit_sec) = max_decode_sec {
                if effective_sr > 0.0 && (effective_samples as f64 / effective_sr) >= limit_sec {
                    reached_limit = true;
                    break;
                }
            }
            i += sample_step;
        }
    }

    if frame_count > 0 {
        let cnt = frame_count.max(1) as f64;
        low_energy.push((frame_sum / cnt).sqrt());
        hit_energy.push(frame_peak);
    }

    let effective_duration = if effective_sr > 0.0 {
        effective_samples as f64 / effective_sr
    } else {
        0.0
    };

    Ok(DecodedEnergy {
        low_energy,
        hit_energy,
        hop_sec,
        duration: effective_duration,
        chunks,
        decoded_samples,
        sample_rate: base_sr,
        effective_sample_rate: effective_sr,
    })
}

/// 解码内存中的音频字节流（支持 MP3/AAC/M4A/FLAC/OGG/WAV），提取低频节奏特征并构建 Beatmap JSON。
pub fn analyze_audio_bytes(
    bytes: &[u8],
    ext_hint: Option<&str>,
    duration_hint: f64,
) -> Result<Value, String> {
    analyze_audio_bytes_with_limit(bytes, ext_hint, duration_hint, None)
}

/// 解码内存中的音频字节流（支持可选的最大解码秒数限制，用于首段 intro 快速锁拍）。
pub fn analyze_audio_bytes_with_limit(
    bytes: &[u8],
    ext_hint: Option<&str>,
    duration_hint: f64,
    max_decode_sec: Option<f64>,
) -> Result<Value, String> {
    let hint_dur = if duration_hint.is_finite() && duration_hint > 0.0 {
        duration_hint
    } else {
        0.0
    };
    let decoded = decode_bytes_to_energy(bytes, ext_hint, hint_dur, max_decode_sec)?;
    let duration = if decoded.duration > 0.0 {
        decoded.duration
    } else {
        hint_dur
    };
    let mut map = build_beat_map_from_low_energy(
        &decoded.low_energy,
        &decoded.hit_energy,
        decoded.hop_sec,
        duration,
    );
    let intro_mode = max_decode_sec.is_some();
    if let Some(obj) = map.as_object_mut() {
        if intro_mode {
            if obj
                .get("tempoSource")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s != "podcast-dj-server-empty")
            {
                obj.insert(
                    "tempoSource".to_string(),
                    json!("podcast-dj-server-intro-offline"),
                );
            }
            obj.insert("partial".to_string(), json!(true));
            obj.insert("introOnly".to_string(), json!(true));
            obj.insert("partialUntilSec".to_string(), json!(duration));
            obj.insert("fullDuration".to_string(), json!(hint_dur));
        }
        obj.insert(
            "decode".to_string(),
            json!({
                "chunks": decoded.chunks,
                "decodedSamples": decoded.decoded_samples,
                "sampleRate": decoded.sample_rate,
                "effectiveSampleRate": decoded.effective_sample_rate,
                "frames": decoded.low_energy.len(),
                "requestedDurationSec": hint_dur,
                "effectiveDurationSec": decoded.duration,
                "intro": intro_mode,
                "partialUntilSec": if intro_mode { json!(duration) } else { Value::Null },
            }),
        );
    }
    Ok(map)
}

fn infer_ext_from_url_and_ct(audio_url: &str, content_type: Option<&str>) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(audio_url) {
        if let Some(seg) = parsed.path_segments().and_then(|mut s| s.next_back()) {
            if let Some((_, ext)) = seg.rsplit_once('.') {
                let lower = ext.to_lowercase();
                if matches!(
                    lower.as_str(),
                    "mp3" | "aac" | "m4a" | "mp4" | "flac" | "ogg" | "oga" | "wav" | "pcm"
                ) {
                    return Some(if lower == "oga" { "ogg".to_string() } else { lower });
                }
            }
        }
    }
    if let Some(ct) = content_type {
        let lower = ct.to_lowercase();
        if lower.contains("mpeg") || lower.contains("mp3") {
            return Some("mp3".to_string());
        }
        if lower.contains("flac") {
            return Some("flac".to_string());
        }
        if lower.contains("ogg") {
            return Some("ogg".to_string());
        }
        if lower.contains("wav") || lower.contains("wave") {
            return Some("wav".to_string());
        }
        if lower.contains("mp4") || lower.contains("m4a") {
            return Some("m4a".to_string());
        }
        if lower.contains("aac") {
            return Some("aac".to_string());
        }
    }
    None
}

async fn fetch_audio_bytes(
    client: &reqwest::Client,
    audio_url: &str,
    range_header: Option<&str>,
    max_bytes: usize,
) -> Result<(Vec<u8>, Option<String>), String> {
    fetch_audio_bytes_with_mode(client, audio_url, range_header, max_bytes, range_header.is_some()).await
}

async fn fetch_audio_bytes_with_mode(
    client: &reqwest::Client,
    audio_url: &str,
    range_header: Option<&str>,
    max_bytes: usize,
    truncate_on_limit: bool,
) -> Result<(Vec<u8>, Option<String>), String> {
    let mut req = client
        .get(audio_url)
        .header(reqwest::header::USER_AGENT, DEFAULT_UA)
        .header(reqwest::header::REFERER, "https://music.163.com/");
    if let Some(r) = range_header {
        req = req.header(reqwest::header::RANGE, r);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Audio fetch failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(format!("Audio fetch failed: {}", status.as_u16()));
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !truncate_on_limit {
        if let Some(cl) = resp.content_length() {
            if cl as usize > max_bytes {
                return Err(format!("Audio stream exceeds max byte limit ({cl} > {max_bytes})"));
            }
        }
    }

    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.map_err(|e| format!("Audio stream read failed: {e}"))?;
        if bytes.len() + chunk.len() > max_bytes {
            if truncate_on_limit {
                let remaining = max_bytes - bytes.len();
                bytes.extend_from_slice(&chunk[..remaining]);
                break;
            }
            return Err(format!("Audio stream exceeds max byte limit ({max_bytes})"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, ct))
}

fn phase_from_map(map: &Value, base_step: f64) -> (f64, f64) {
    let step = clamp_range(
        if base_step > 0.0 {
            base_step
        } else {
            map.get("gridStep").and_then(|v| v.as_f64()).unwrap_or(0.50)
        },
        0.32,
        0.86,
    );
    let beats_arr = map
        .get("cameraBeats")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .or_else(|| map.get("beats").and_then(|v| v.as_array()));
    let Some(arr) = beats_arr else {
        return (0.0, step);
    };
    let valid: Vec<(f64, f64)> = arr
        .iter()
        .filter_map(|b| {
            let t = b.get("time").and_then(|v| v.as_f64())?;
            if t.is_finite() && t > 0.35 {
                let imp = b
                    .get("impact")
                    .or_else(|| b.get("strength"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.3);
                Some((t, imp))
            } else {
                None
            }
        })
        .collect();
    if valid.is_empty() {
        return (0.0, step);
    }
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut total = 0.0;
    for &(t, imp) in &valid {
        let w = 0.20 + imp.max(0.0).powf(1.45);
        let phase = ((t % step) + step) % step;
        let angle = phase / step * PI * 2.0;
        sx += angle.cos() * w;
        sy += angle.sin() * w;
        total += w;
    }
    if total <= 0.0 {
        let p = ((valid[0].0 % step) + step) % step;
        return (p, step);
    }
    let mut angle = (sy / total).atan2(sx / total);
    if angle < 0.0 {
        angle += PI * 2.0;
    }
    (angle / (PI * 2.0) * step, step)
}

async fn analyze_podcast_dj_range_samples(
    client: &reqwest::Client,
    audio_url: &str,
    duration: f64,
) -> Result<Value, String> {
    if !(duration > 0.0) {
        return Err("Long podcast analysis needs duration".to_string());
    }
    let content_length = match client
        .head(audio_url)
        .header(reqwest::header::USER_AGENT, DEFAULT_UA)
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .send()
        .await
    {
        Ok(resp) => resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    };

    if content_length == 0 {
        let (bytes, ct) = fetch_audio_bytes(client, audio_url, None, MAX_DOWNLOAD_BYTES).await?;
        let ext = infer_ext_from_url_and_ct(audio_url, ct.as_deref());
        return tokio::task::spawn_blocking(move || {
            analyze_audio_bytes(&bytes, ext.as_deref(), duration)
        })
        .await
        .map_err(|e| format!("Analysis task join error: {e}"))?;
    }

    let sample_count = if duration > 14400.0 {
        12
    } else if duration > 9000.0 {
        10
    } else {
        8
    };
    let sample_window = if duration > 14400.0 {
        82.0
    } else if duration > 9000.0 {
        88.0
    } else {
        96.0
    };

    struct SampleMap {
        offset: f64,
        map: Value,
    }
    let mut sample_maps: Vec<SampleMap> = Vec::new();
    let mut total_chunks = 0usize;
    let mut total_decoded = 0usize;

    for i in 0..sample_count {
        let pos = if sample_count == 1 {
            0.0
        } else {
            i as f64 / (sample_count - 1) as f64
        };
        let shaped = if i == 0 {
            0.0
        } else if i == sample_count - 1 {
            0.88
        } else {
            0.08 + pos * 0.80
        };
        let target_time = (duration * shaped).clamp(0.0, (duration - sample_window).max(0.0));
        let byte_per_sec = content_length as f64 / duration.max(1.0);
        let preroll_bytes = if i == 0 {
            0
        } else {
            ((byte_per_sec * 4.0).floor() as u64).min(384 * 1024)
        };
        let start_byte = ((target_time * byte_per_sec).floor() as u64).saturating_sub(preroll_bytes);
        let window_bytes = ((sample_window * byte_per_sec).floor() as u64 + preroll_bytes + 128 * 1024)
            .max(768 * 1024);
        let end_byte = (content_length - 1).min(start_byte + window_bytes);
        let approx_offset = start_byte as f64 / content_length as f64 * duration;
        let range_hdr = format!("bytes={start_byte}-{end_byte}");

        let Ok((bytes, ct)) =
            fetch_audio_bytes(client, audio_url, Some(&range_hdr), MAX_RANGE_SAMPLE_BYTES).await
        else {
            continue;
        };
        let ext = infer_ext_from_url_and_ct(audio_url, ct.as_deref());
        let res = tokio::task::spawn_blocking(move || {
            analyze_audio_bytes(&bytes, ext.as_deref(), sample_window)
        })
        .await;
        let Ok(Ok(map)) = res else {
            continue;
        };
        total_chunks += map["decode"]["chunks"].as_u64().unwrap_or(0) as usize;
        total_decoded += map["decode"]["decodedSamples"].as_u64().unwrap_or(0) as usize;
        let visual_cnt = map["visualBeatCount"].as_u64().unwrap_or(0);
        let grid_step = map["gridStep"].as_f64().unwrap_or(0.0);
        if visual_cnt >= 8 && grid_step > 0.0 {
            sample_maps.push(SampleMap {
                offset: approx_offset,
                map,
            });
        }
    }

    if sample_maps.is_empty() {
        return Ok(json!({
            "kicks": [],
            "beats": [],
            "pulseBeats": [],
            "cameraBeats": [],
            "duration": duration,
            "visualBeatCount": 0,
            "tempoSource": "podcast-dj-server-range-empty",
            "analyzedAt": now_epoch_ms(),
        }));
    }

    let mut step_votes = Vec::new();
    for s in &sample_maps {
        let vc = s.map["visualBeatCount"].as_u64().unwrap_or(0) as f64;
        let w = ((vc / 16.0).round() as usize).clamp(1, 16);
        let gs = s.map["gridStep"].as_f64().unwrap_or(0.5);
        for _ in 0..w {
            step_votes.push(gs);
        }
    }
    let first_step = sample_maps[0].map["gridStep"].as_f64().unwrap_or(0.5);
    let med_step = median(&step_votes);
    let global_step = clamp_range(if med_step > 0.0 { med_step } else { first_step }, 0.32, 0.86);

    let first_beat_t = sample_maps[0].map["cameraBeats"]
        .as_array()
        .and_then(|a| a.first())
        .or_else(|| sample_maps[0].map["beats"].as_array().and_then(|a| a.first()))
        .and_then(|b| b.get("time"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut anchor = first_beat_t;
    while anchor - global_step > 0.05 {
        anchor -= global_step;
    }

    #[derive(Clone)]
    struct RangeProfile {
        time: f64,
        avg: f64,
        hi: f64,
        activity: f64,
        step: f64,
        anchor: f64,
    }

    let mut profiles: Vec<RangeProfile> = sample_maps
        .iter()
        .map(|s| {
            let arr = s.map["cameraBeats"]
                .as_array()
                .filter(|a| !a.is_empty())
                .or_else(|| s.map["beats"].as_array())
                .cloned()
                .unwrap_or_default();
            let impacts: Vec<f64> = arr
                .iter()
                .filter_map(|b| {
                    b.get("impact")
                        .or_else(|| b.get("strength"))
                        .and_then(|v| v.as_f64())
                })
                .filter(|v| v.is_finite())
                .collect();
            let active: Vec<f64> = impacts.iter().copied().filter(|&v| v >= 0.10).collect();
            let avg_impact = if active.is_empty() {
                0.16
            } else {
                active.iter().sum::<f64>() / active.len() as f64
            };
            let hi_impact = if impacts.is_empty() {
                avg_impact.max(0.55)
            } else {
                percentile(&impacts, 0.90, 4000)
            };
            let s_dur = s.map["duration"].as_f64().unwrap_or(20.0).max(20.0);
            let activity = arr.len() as f64 / s_dur;
            let (phase, _) = phase_from_map(&s.map, global_step);
            RangeProfile {
                time: s.offset,
                avg: clamp_range(avg_impact * clamp_range(activity / 1.65, 0.38, 1.05), 0.08, 0.72),
                hi: clamp_range(hi_impact, 0.18, 0.96),
                activity: clamp_range(activity / 1.65, 0.18, 1.12),
                step: global_step,
                anchor: s.offset + phase,
            }
        })
        .collect();
    profiles.sort_by(|a, b| a.time.total_cmp(&b.time));

    let profile_at = |time: f64| -> (f64, f64, f64, f64) {
        if profiles.len() == 1 {
            let p = &profiles[0];
            return (p.avg, p.hi, p.activity, p.step);
        }
        let mut prev = &profiles[0];
        let mut next = &profiles[profiles.len() - 1];
        for p in &profiles {
            if p.time <= time {
                prev = p;
            }
            if p.time >= time {
                next = p;
                break;
            }
        }
        if (prev.time - next.time).abs() < 1e-9 {
            return (prev.avg, prev.hi, prev.activity, prev.step);
        }
        let mix = clamp01((time - prev.time) / (next.time - prev.time).max(1.0));
        (
            prev.avg + (next.avg - prev.avg) * mix,
            prev.hi + (next.hi - prev.hi) * mix,
            prev.activity + (next.activity - prev.activity) * mix,
            prev.step + (next.step - prev.step) * mix,
        )
    };

    let mut beats = Vec::new();
    let mut kicks = Vec::new();
    let mut camera_beats = Vec::new();
    let mut pulse_beats = Vec::new();
    let mut grid_index = 0usize;

    for si in 0..profiles.len() {
        let p = &profiles[si];
        let start = if si == 0 {
            0.0
        } else {
            (profiles[si - 1].time + p.time) * 0.5
        };
        let end = if si == profiles.len() - 1 {
            duration
        } else {
            (p.time + profiles[si + 1].time) * 0.5
        };
        let mut t = if p.anchor.is_finite() { p.anchor } else { anchor };
        while t - global_step > start {
            t -= global_step;
        }
        while t < start {
            t += global_step;
        }
        while t < end - 0.04 {
            let (p_avg, p_hi, p_act, p_step) = profile_at(t);
            let slot = grid_index % 4;
            let mut combo = match slot {
                0 => "downbeat",
                1 => "push",
                2 => "drop",
                _ => "rebound",
            };
            let section_energy = clamp01((p_avg - 0.055) / 0.54) * clamp_range(p_act, 0.30, 1.10);
            let motion = ((grid_index as f64 * 1.618 + p_avg * 9.7).sin() * 0.5
                + (grid_index as f64 * 0.317).sin() * 0.28)
                * (0.08 + section_energy * 0.17);
            let rel = clamp01(
                0.12 + section_energy * 0.70 + motion + if combo == "downbeat" { 0.060 } else { 0.0 },
            );
            if rel > 0.82 && combo != "downbeat" {
                combo = "accent";
            }
            let visual_rel = if rel > 0.78 { 0.78 + (rel - 0.78) * 0.50 } else { rel };
            let combo_lift = match combo {
                "downbeat" => 0.10 * section_energy,
                "drop" => 0.050 * section_energy,
                "accent" => 0.075 * section_energy,
                _ => 0.0,
            };
            let impact = clamp_range(
                0.026 + visual_rel.powf(1.48) * (0.42 + p_hi * 0.34) + combo_lift,
                0.020,
                0.90,
            );
            let strength = clamp_range(
                0.15 + visual_rel.powf(1.02) * 0.66 + combo_lift * 0.68,
                0.12,
                0.93,
            );
            let camera_active = impact >= 0.105 || (combo == "downbeat" && section_energy >= 0.16);
            let pulse_active = impact > 0.16 || (combo == "downbeat" && section_energy >= 0.24);
            let low = clamp_range(
                0.50 + visual_rel * 0.32 + if combo == "downbeat" { 0.050 * section_energy } else { 0.0 }
                    - if combo == "accent" { 0.12 } else { 0.0 },
                0.42,
                0.90,
            );
            let body = clamp_range(
                0.06 + visual_rel * 0.15
                    + if combo == "push" { 0.22 * section_energy } else { 0.0 }
                    + if combo == "drop" { 0.30 * section_energy } else { 0.0 },
                0.045,
                0.56,
            );
            let snap = clamp_range(
                0.025
                    + visual_rel * 0.035
                    + if combo == "accent" { 0.40 * section_energy } else { 0.0 }
                    + if combo == "rebound" { 0.12 * section_energy } else { 0.0 },
                0.02,
                0.62,
            );

            let beat_obj = json!({
                "time": t,
                "strength": strength,
                "confidence": 0.68 + visual_rel * 0.22,
                "impact": impact,
                "primary": camera_active,
                "camera": camera_active,
                "pulse": pulse_active,
                "tone": "podcast-dj-server-range-grid",
                "low": low,
                "body": body,
                "snap": snap,
                "mass": clamp_range(low * 0.72 + visual_rel.powf(1.22) * 0.24, 0.36, 0.94),
                "sharpness": if combo == "accent" { 0.20 } else { 0.08 },
                "combo": combo,
                "step": if global_step > 0.0 { global_step } else { p_step },
                "index": beats.len(),
                "dj": true,
                "grid": true,
                "kickOnly": true,
                "server": true,
                "sampled": true,
            });
            kicks.push(t);
            if camera_active {
                camera_beats.push(beat_obj.clone());
            }
            if pulse_active && (impact >= 0.16 || combo == "downbeat") {
                pulse_beats.push(json!({
                    "time": t,
                    "strength": strength,
                    "impact": impact,
                    "combo": combo,
                    "low": low,
                    "body": body,
                    "snap": snap,
                    "dj": true,
                }));
            }
            beats.push(beat_obj);
            grid_index += 1;
            t += global_step;
        }
    }

    let section_steps: Vec<f64> = profiles.iter().map(|p| p.step).collect();
    let visual_cnt = camera_beats.len();
    Ok(json!({
        "kicks": kicks,
        "beats": beats,
        "pulseBeats": pulse_beats,
        "cameraBeats": camera_beats,
        "gridStep": global_step,
        "sectionSteps": section_steps,
        "tempoSource": "podcast-dj-server-range-offline",
        "duration": duration,
        "visualBeatCount": visual_cnt,
        "analyzedAt": now_epoch_ms(),
        "debug": {
            "rangeSampled": true,
            "samples": sample_maps.len(),
            "contentLength": content_length,
            "decode": { "chunks": total_chunks, "decodedSamples": total_decoded },
        },
    }))
}

/// 异步拉取远程 HTTP/HTTPS 音频流并分析节奏节拍图谱（支持长音频 Range 采样与超时/字节上限防护）。
pub async fn analyze_podcast_dj_stream(audio_url: &str, duration_sec: f64) -> Result<Value, String> {
    analyze_podcast_dj_stream_with_options(audio_url, duration_sec, false).await
}

/// 异步拉取远程 HTTP/HTTPS 音频流并分析节奏节拍图谱；`intro_only == true` 时仅解码分析前约 55 秒以加速首段锁拍。
pub async fn analyze_podcast_dj_stream_with_options(
    audio_url: &str,
    duration_sec: f64,
    intro_only: bool,
) -> Result<Value, String> {
    let trimmed = audio_url.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("Invalid audio url".to_string());
    }

    let dur = if duration_sec.is_finite() && duration_sec > 0.0 {
        duration_sec
    } else {
        0.0
    };

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client build failed: {e}"))?;

    if intro_only {
        let range_hdr = format!("bytes=0-{}", INTRO_FETCH_MAX_BYTES - 1);
        let (bytes, ct) = match fetch_audio_bytes_with_mode(
            &client,
            trimmed,
            Some(&range_hdr),
            INTRO_FETCH_MAX_BYTES,
            true,
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                fetch_audio_bytes_with_mode(
                    &client,
                    trimmed,
                    None,
                    INTRO_FETCH_MAX_BYTES,
                    true,
                )
                .await?
            }
        };
        let ext = infer_ext_from_url_and_ct(trimmed, ct.as_deref());
        return tokio::task::spawn_blocking(move || {
            analyze_audio_bytes_with_limit(&bytes, ext.as_deref(), dur, Some(INTRO_DECODE_MAX_SEC))
        })
        .await
        .map_err(|e| format!("Analysis task join error: {e}"))?;
    }

    if dur > FULL_STREAM_QUALITY_LIMIT_SEC {
        return analyze_podcast_dj_range_samples(&client, trimmed, dur).await;
    }

    if dur > 3300.0 && dur <= FULL_STREAM_QUALITY_LIMIT_SEC {
        if let Ok((bytes, ct)) = fetch_audio_bytes(&client, trimmed, None, MAX_DOWNLOAD_BYTES).await {
            let ext = infer_ext_from_url_and_ct(trimmed, ct.as_deref());
            if let Ok(Ok(mut map)) = tokio::task::spawn_blocking(move || {
                analyze_audio_bytes(&bytes, ext.as_deref(), dur)
            })
            .await
            {
                if let Some(obj) = map.as_object_mut() {
                    if let Some(dbg) = obj.get_mut("debug").and_then(|v| v.as_object_mut()) {
                        dbg.insert("fullStreamQuality".to_string(), json!(true));
                        dbg.insert("requestedDurationSec".to_string(), json!(dur));
                    }
                }
                return Ok(map);
            }
        }
        return analyze_podcast_dj_range_samples(&client, trimmed, dur).await;
    }

    let (bytes, ct) = fetch_audio_bytes(&client, trimmed, None, MAX_DOWNLOAD_BYTES).await?;
    let ext = infer_ext_from_url_and_ct(trimmed, ct.as_deref());
    tokio::task::spawn_blocking(move || analyze_audio_bytes(&bytes, ext.as_deref(), dur))
        .await
        .map_err(|e| format!("Analysis task join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthesize_pulse_wav(sample_rate: u32, duration_sec: f64, beat_period_sec: f64) -> Vec<u8> {
        let num_samples = (sample_rate as f64 * duration_sec) as usize;
        let mut pcm = Vec::with_capacity(num_samples * 2);
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let phase_in_beat = t % beat_period_sec;
            let val = if phase_in_beat < 0.04 {
                (2.0 * PI * 70.0 * phase_in_beat).sin() * 0.9
            } else {
                0.0
            };
            let s16 = (val * 32767.0) as i16;
            pcm.extend_from_slice(&s16.to_le_bytes());
        }
        let data_len = pcm.len() as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&pcm);
        wav
    }

    #[test]
    fn dj_analyzer_detects_periodic_low_end_beats() {
        let wav = synthesize_pulse_wav(44100, 16.0, 0.5);
        let map = analyze_audio_bytes(&wav, Some("wav"), 16.0).expect("analyze_audio_bytes ok");
        let grid_step = map["gridStep"].as_f64().expect("gridStep f64");
        assert!((grid_step - 0.5).abs() < 0.06, "unexpected gridStep: {grid_step}");
        assert!(map["beats"].as_array().unwrap().len() >= 20);
        assert!(map["cameraBeats"].as_array().unwrap().len() >= 10);
        assert!(!map["pulseBeats"].as_array().unwrap().is_empty());
        assert_eq!(map["kicks"].as_array().unwrap().len(), map["beats"].as_array().unwrap().len());
        assert_eq!(map["tempoSource"], json!("podcast-dj-server-low-offline"));
    }

    #[test]
    fn dj_analyzer_sniffs_wav_without_ext_hint_and_rejects_empty() {
        let wav = synthesize_pulse_wav(44100, 12.0, 0.5);
        let map = analyze_audio_bytes(&wav, None, 0.0).expect("sniffed wav ok");
        let grid_step = map["gridStep"].as_f64().unwrap();
        assert!((grid_step - 0.5).abs() < 0.06);
        assert!(map["duration"].as_f64().unwrap() > 11.5);

        assert!(analyze_audio_bytes(&[], Some("wav"), 1.0).is_err());
    }

    #[test]
    fn dj_analyzer_rejects_non_http_url() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(analyze_podcast_dj_stream("file:///tmp/test.mp3", 10.0));
        assert!(err.is_err());
        let err_intro = rt.block_on(analyze_podcast_dj_stream_with_options(
            "file:///tmp/test.mp3",
            10.0,
            true,
        ));
        assert!(err_intro.is_err());
    }

    #[test]
    fn dj_analyzer_intro_only_limits_decoded_duration() {
        let wav = synthesize_pulse_wav(44100, 90.0, 0.5);
        let map = analyze_audio_bytes_with_limit(&wav, Some("wav"), 90.0, Some(55.0))
            .expect("intro analyze ok");
        assert_eq!(map["partial"], json!(true));
        assert_eq!(map["introOnly"], json!(true));
        assert_eq!(map["fullDuration"], json!(90.0));
        assert_eq!(map["tempoSource"], json!("podcast-dj-server-intro-offline"));
        let eff_dur = map["decode"]["effectiveDurationSec"].as_f64().unwrap();
        assert!(
            eff_dur >= 54.0 && eff_dur <= 56.0,
            "expected ~55s decoded duration, got {eff_dur}"
        );
        let grid_step = map["gridStep"].as_f64().unwrap();
        assert!((grid_step - 0.5).abs() < 0.06);
        assert!(map["beats"].as_array().unwrap().len() >= 90);
    }
}
