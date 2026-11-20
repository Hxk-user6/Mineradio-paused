use crate::cuefield::lrc::{repeated_groups, LrcLine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const MINERADIO_BEAT_COMBOS: [&str; 6] = ["", "downbeat", "push", "drop", "rebound", "accent"];

/// 将浮点数限制到 `[0.0, 1.0]`，非有限值归零。
pub fn clamp01(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

/// 将浮点数四舍五入至小数点后 3 位，消除 `-0.0`。
pub fn round3(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

pub(crate) fn round_digits(value: f64, digits: u32) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let factor = 10_f64.powi(digits as i32);
    let rounded = (value * factor).round() / factor;
    if rounded == 0.0 { 0.0 } else { rounded }
}

pub(crate) fn val_f64(v: Option<&Value>, fallback: f64) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()).unwrap_or(fallback),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|x| x.is_finite()).unwrap_or(fallback),
        _ => fallback,
    }
}

pub(crate) fn opt_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<f64>().ok().filter(|x| x.is_finite())
            }
        }
        _ => None,
    }
}

fn median(values: &[f64]) -> f64 {
    let mut list: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if list.is_empty() {
        return 0.0;
    }
    list.sort_by(|a, b| a.total_cmp(b));
    let mid = list.len() / 2;
    if list.len() % 2 == 1 {
        list[mid]
    } else {
        (list[mid - 1] + list[mid]) / 2.0
    }
}

fn median_opt(values: &[f64]) -> Option<f64> {
    let mut list: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if list.is_empty() {
        return None;
    }
    list.sort_by(|a, b| a.total_cmp(b));
    let mid = list.len() / 2;
    if list.len() % 2 == 1 {
        Some(list[mid])
    } else {
        Some((list[mid - 1] + list[mid]) / 2.0)
    }
}

fn average(values: &[f64]) -> f64 {
    let valid: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if valid.is_empty() {
        return 0.0;
    }
    valid.iter().sum::<f64>() / valid.len() as f64
}

/// 归一化后的单拍事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatEvent {
    pub time: f64,
    pub index: usize,
    pub strength: f64,
    pub confidence: f64,
    pub impact: f64,
    pub low: f64,
    pub body: f64,
    pub snap: f64,
    pub combo: String,
    pub downbeat: bool,
    pub phrase: bool,
    pub primary: bool,
    pub camera: bool,
    pub pulse: bool,
    pub dj: bool,
    pub grid: bool,
    pub kick_only: bool,
    pub mass: f64,
    pub sharpness: f64,
    pub step: f64,
}

/// 曲目基础摘要信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration: f64,
}

/// 4 拍小节摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bar {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub energy: f64,
    pub low_density: f64,
    pub body_density: f64,
    pub snap_density: f64,
    pub beat_stability: f64,
}

/// 8 小节乐句段落。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phrase {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub bars: usize,
    pub energy: f64,
}

/// 重拍信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownbeatInfo {
    pub time: f64,
    pub confidence: f64,
    pub energy: f64,
}

/// 节拍网格质量指标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GridQuality {
    pub downbeat_count: usize,
    pub downbeat_confidence: f64,
    pub beat_stability: f64,
    pub timing_stability: f64,
    pub tempo_stability: f64,
    pub beat_confidence: f64,
    pub downbeat_stability: f64,
    pub data_confidence: f64,
}

/// 关键 Cue 点汇总。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuePoints {
    pub intro_start: f64,
    pub intro_end: f64,
    pub first_strong_downbeat: f64,
    pub outro_start: f64,
    pub outro_end: f64,
    pub last_safe_phrase_end: f64,
}

/// 音频响度测量指标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMetrics {
    pub short_term_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
}

/// 简单起止时间窗。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeWindow {
    pub start: f64,
    pub end: f64,
}

/// 带能量值的区间窗。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueWindow {
    pub start: f64,
    pub end: f64,
    pub value: f64,
}

/// 前置段落指向目标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvesTo {
    #[serde(rename = "type")]
    pub kind: String,
    pub time: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// 副歌/高潮重复证据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEvidence {
    pub repeated_line_count: usize,
    pub repeated_block_count: usize,
    pub energy_lift: f64,
    pub sustained_energy: bool,
}

/// 出场/入场/避让候选点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionCandidate {
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    #[serde(default)]
    pub source: String,
    pub time: f64,
    pub confidence: f64,
    pub energy_before: f64,
    pub energy_after: f64,
    pub vocal_density: f64,
    pub low_density: f64,
    pub beat_stability: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeats: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolves_to: Option<ResolvesTo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_from: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub late_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeated_line_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeated_block_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_lift: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sustained_energy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<HookEvidence>,
}

/// 曲式结构段落（如 `pre-hook`、`hook`、`drop`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureSection {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub bars: usize,
    pub energy: f64,
    #[serde(rename = "type")]
    pub kind: String,
    pub confidence: f64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeated_line_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeated_block_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_lift: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sustained_energy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<HookEvidence>,
}

/// 曲目结构图谱。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureMap {
    pub duration: f64,
    pub structure_source: String,
    pub structure_confidence: f64,
    pub protected_until: f64,
    pub sections: Vec<StructureSection>,
    pub vocal_windows: Vec<TimeWindow>,
    pub exit_candidates: Vec<TransitionCandidate>,
    pub entry_candidates: Vec<TransitionCandidate>,
}

/// 完整的缓存曲目归一化与结构分析结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedAnalysis {
    pub track: TrackSummary,
    pub duration: f64,
    pub bpm: f64,
    pub grid_step: f64,
    pub beats: Vec<BeatEvent>,
    pub downbeats: Vec<DownbeatInfo>,
    pub bars: Vec<Bar>,
    pub phrases: Vec<Phrase>,
    pub grid_quality: GridQuality,
    pub cue_points: CuePoints,
    pub energy_windows: Vec<ValueWindow>,
    pub bass_windows: Vec<ValueWindow>,
    pub vocal_windows: Vec<ValueWindow>,
    pub candidates: Vec<TransitionCandidate>,
    pub structure_map: StructureMap,
    pub musical_profile: Option<Value>,
    pub audio_metrics: AudioMetrics,
    pub shadow: Value,
}

fn normalize_beat_event(raw: &Value, index: usize, grid_step: f64) -> Option<BeatEvent> {
    if let Some(arr) = raw.as_array() {
        let time = opt_f64(arr.first())?;
        let flags = val_f64(arr.get(8), 0.0).round().max(0.0) as u64;
        let combo_idx = val_f64(arr.get(7), 0.0).round().max(0.0) as usize;
        let combo = MINERADIO_BEAT_COMBOS.get(combo_idx).copied().unwrap_or("").to_string();
        let strength = val_f64(arr.get(1), 0.5);
        let mass = val_f64(arr.get(9), 0.0);
        let sharpness = val_f64(arr.get(10), 0.0);
        return Some(BeatEvent {
            time,
            index,
            strength,
            confidence: val_f64(arr.get(2), 0.5),
            impact: val_f64(arr.get(3), strength),
            low: val_f64(arr.get(4), mass),
            body: val_f64(arr.get(5), 0.0),
            snap: val_f64(arr.get(6), sharpness),
            downbeat: combo == "downbeat",
            phrase: false,
            combo,
            primary: (flags & 1) != 0,
            camera: (flags & 2) != 0,
            pulse: (flags & 4) != 0,
            dj: (flags & 8) != 0,
            grid: (flags & 16) != 0,
            kick_only: (flags & 32) != 0,
            mass,
            sharpness,
            step: val_f64(arr.get(11), grid_step),
        });
    }

    if let Some(num) = raw.as_f64().filter(|x| x.is_finite()) {
        return Some(BeatEvent {
            time: num,
            index,
            strength: 0.5,
            confidence: 0.5,
            impact: 0.25,
            low: 0.0,
            body: 0.0,
            snap: 0.0,
            combo: String::new(),
            downbeat: false,
            phrase: false,
            primary: true,
            camera: true,
            pulse: true,
            dj: false,
            grid: false,
            kick_only: false,
            mass: 0.0,
            sharpness: 0.0,
            step: grid_step,
        });
    }

    let obj = raw.as_object()?;
    let time = opt_f64(obj.get("time"))?;
    let combo = obj
        .get("combo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let downbeat = obj.get("downbeat").and_then(|v| v.as_bool()).unwrap_or(false) || combo == "downbeat";
    Some(BeatEvent {
        time,
        index,
        strength: val_f64(obj.get("strength"), 0.5),
        confidence: val_f64(obj.get("confidence"), 0.5),
        impact: val_f64(obj.get("impact"), 0.25),
        low: val_f64(obj.get("low"), 0.0),
        body: val_f64(obj.get("body"), 0.0),
        snap: val_f64(obj.get("snap"), 0.0),
        combo,
        downbeat,
        phrase: obj.get("phrase").and_then(|v| v.as_bool()).unwrap_or(false),
        primary: obj.get("primary").and_then(|v| v.as_bool()).unwrap_or(true),
        camera: obj.get("camera").and_then(|v| v.as_bool()).unwrap_or(true),
        pulse: obj.get("pulse").and_then(|v| v.as_bool()).unwrap_or(true),
        dj: obj.get("dj").and_then(|v| v.as_bool()).unwrap_or(false),
        grid: obj.get("grid").and_then(|v| v.as_bool()).unwrap_or(false),
        kick_only: obj.get("kickOnly").and_then(|v| v.as_bool()).unwrap_or(false),
        mass: val_f64(obj.get("mass"), 0.0),
        sharpness: val_f64(obj.get("sharpness"), 0.0),
        step: val_f64(obj.get("step"), grid_step),
    })
}

fn beat_energy(beat: &BeatEvent) -> f64 {
    beat.impact
        .max(beat.strength)
        .max(beat.body)
        .max(beat.snap)
        .max(beat.low)
}

fn is_downbeat(beat: &BeatEvent, index: usize, has_explicit_meter: bool) -> bool {
    if beat.downbeat || beat.phrase || beat.combo.eq_ignore_ascii_case("downbeat") {
        return true;
    }
    !has_explicit_meter && index % 4 == 0
}

fn infer_grid_step(map: &Value, beats: &[BeatEvent]) -> f64 {
    let explicit = val_f64(map.get("gridStep"), 0.0);
    if explicit > 0.0 {
        return explicit;
    }
    let mut deltas = Vec::new();
    for i in 1..beats.len() {
        let delta = beats[i].time - beats[i - 1].time;
        if delta > 0.0 && delta < 4.0 {
            deltas.push(delta);
        }
    }
    let avg = round3(average(&deltas));
    if avg > 0.0 { avg } else { 0.5 }
}

fn beat_grid_quality(
    beats: &[BeatEvent],
    grid_step: f64,
    downbeats: &[&BeatEvent],
    map: &Value,
) -> (f64, f64, f64, f64, f64) {
    let mut intervals = Vec::new();
    for i in 1..beats.len() {
        let delta = beats[i].time - beats[i - 1].time;
        if delta > 0.18 && delta < 1.8 {
            intervals.push(delta);
        }
    }
    let step = if grid_step > 0.0 { grid_step } else { median(&intervals) };
    let deviations: Vec<f64> = intervals.iter().map(|v| (v - step).abs()).collect();
    let relative_mad = if step > 0.0 { median(&deviations) / step } else { 1.0 };
    let tempo_stability = clamp01(1.0 - relative_mad / 0.12);
    let beat_confidence = clamp01(if beats.is_empty() {
        0.0
    } else {
        beats.iter().map(|b| clamp01(b.confidence)).sum::<f64>() / beats.len() as f64
    });

    let mut downbeat_intervals = Vec::new();
    for i in 1..downbeats.len() {
        let delta = downbeats[i].time - downbeats[i - 1].time;
        if delta > 0.0 {
            downbeat_intervals.push(delta);
        }
    }
    let expected_bar = if step > 0.0 { step * 4.0 } else { 0.0 };
    let downbeat_error = if expected_bar > 0.0 && !downbeat_intervals.is_empty() {
        let errs: Vec<f64> = downbeat_intervals
            .iter()
            .map(|v| (v - expected_bar).abs() / expected_bar)
            .collect();
        median(&errs)
    } else {
        1.0
    };
    let downbeat_stability = clamp01(1.0 - downbeat_error / 0.18);
    let enough_beats = clamp01(beats.len() as f64 / 48.0);
    let range_valid = if (0.3..=1.0).contains(&step) { 1.0 } else { 0.0 };
    let partial_penalty = if map.get("partial").and_then(|v| v.as_bool()).unwrap_or(false) {
        0.72
    } else {
        1.0
    };
    let data_confidence = clamp01(
        (tempo_stability * 0.36
            + beat_confidence * 0.22
            + downbeat_stability * 0.24
            + enough_beats * 0.18)
            * range_valid
            * partial_penalty,
    );
    (step, tempo_stability, beat_confidence, downbeat_stability, data_confidence)
}

fn build_bars(
    beats: &[BeatEvent],
    downbeats: &[DownbeatInfo],
    grid_step: f64,
    duration: f64,
    stable_confidence: f64,
) -> Vec<Bar> {
    downbeats
        .iter()
        .enumerate()
        .map(|(index, db)| {
            let next = downbeats.get(index + 1);
            let start = db.time;
            let raw_end = next.map(|n| n.time).unwrap_or(start + grid_step * 4.0);
            let end = duration.min(raw_end);
            let window_end = if end > start { end } else { start + grid_step * 4.0 };
            let window: Vec<&BeatEvent> = beats
                .iter()
                .filter(|b| b.time >= start && b.time < window_end)
                .collect();
            let stability = if window.is_empty() {
                0.0
            } else {
                window.iter().filter(|b| b.confidence >= stable_confidence).count() as f64
                    / window.len() as f64
            };
            Bar {
                index,
                start: round3(start),
                end: round3(window_end),
                energy: round3(average(&window.iter().map(|b| beat_energy(b)).collect::<Vec<_>>())),
                low_density: round3(average(&window.iter().map(|b| b.low).collect::<Vec<_>>())),
                body_density: round3(average(&window.iter().map(|b| b.body).collect::<Vec<_>>())),
                snap_density: round3(average(&window.iter().map(|b| b.snap).collect::<Vec<_>>())),
                beat_stability: round3(stability),
            }
        })
        .collect()
}

fn grid_timing_stability(beats: &[BeatEvent], grid_step: f64) -> f64 {
    if grid_step <= 0.0 || beats.len() < 2 {
        return 0.0;
    }
    let mut stable = 0usize;
    for i in 1..beats.len() {
        let delta_in_beats = (beats[i].time - beats[i - 1].time) / grid_step;
        let nearest_multiple = delta_in_beats.round();
        if nearest_multiple >= 1.0 && (delta_in_beats - nearest_multiple).abs() <= 0.18 {
            stable += 1;
        }
    }
    round3(stable as f64 / (beats.len() - 1) as f64)
}

fn build_phrases(bars: &[Bar], duration: f64) -> Vec<Phrase> {
    let mut phrases = Vec::new();
    for chunk in bars.chunks(8) {
        let Some(start_bar) = chunk.first() else {
            continue;
        };
        let end_bar = chunk.last();
        phrases.push(Phrase {
            index: phrases.len(),
            start: start_bar.start,
            end: end_bar.map(|b| b.end).unwrap_or(duration),
            bars: end_bar.map(|b| b.index - start_bar.index + 1).unwrap_or(1),
            energy: round3(average(&chunk.iter().map(|b| b.energy).collect::<Vec<_>>())),
        });
    }
    phrases
}

fn build_windows(beats: &[BeatEvent], duration: f64, size: f64) -> (Vec<ValueWindow>, Vec<ValueWindow>, Vec<ValueWindow>) {
    let mut energy = Vec::new();
    let mut bass = Vec::new();
    let mut vocal = Vec::new();
    let mut start = 0.0;
    while start < duration {
        let end = duration.min(start + size);
        let window: Vec<&BeatEvent> = beats
            .iter()
            .filter(|b| b.time >= start && b.time < end)
            .collect();
        energy.push(ValueWindow {
            start: round3(start),
            end: round3(end),
            value: round3(average(&window.iter().map(|b| beat_energy(b)).collect::<Vec<_>>())),
        });
        bass.push(ValueWindow {
            start: round3(start),
            end: round3(end),
            value: round3(average(&window.iter().map(|b| b.low).collect::<Vec<_>>())),
        });
        vocal.push(ValueWindow {
            start: round3(start),
            end: round3(end),
            value: 0.0,
        });
        start += size;
    }
    (energy, bass, vocal)
}

fn window_stats(beats: &[BeatEvent], start: f64, duration: f64) -> (f64, f64, f64) {
    let window: Vec<&BeatEvent> = beats
        .iter()
        .filter(|b| b.time >= start && b.time < start + duration)
        .collect();
    let energy = round3(average(&window.iter().map(|b| beat_energy(b)).collect::<Vec<_>>()));
    let low_density = round3(average(&window.iter().map(|b| b.low).collect::<Vec<_>>()));
    let beat_stability = round3(if window.is_empty() {
        0.0
    } else {
        window.iter().filter(|b| b.confidence >= 0.75).count() as f64 / window.len() as f64
    });
    (energy, low_density, beat_stability)
}

fn candidate_metric(beats: &[BeatEvent], time: f64) -> (f64, f64, f64, f64) {
    let (energy_before, _, _) = window_stats(beats, (time - 8.0).max(0.0), 8.0);
    let (energy_after, low_density, beat_stability) = window_stats(beats, time, 8.0);
    (energy_before, energy_after, low_density, beat_stability)
}

fn make_candidate(
    kind: &str,
    role: &str,
    source: &str,
    time: f64,
    confidence: f64,
    beats: &[BeatEvent],
) -> TransitionCandidate {
    let (energy_before, energy_after, low_density, beat_stability) = candidate_metric(beats, time);
    TransitionCandidate {
        kind: kind.to_string(),
        role: role.to_string(),
        source: source.to_string(),
        time: round3(time),
        confidence,
        energy_before,
        energy_after,
        vocal_density: 0.0,
        low_density,
        beat_stability,
        text: None,
        repeats: None,
        resolves_to: None,
        play_from: None,
        landing_at: None,
        landing_type: None,
        exit_ratio: None,
        late_penalty: None,
        original_type: None,
        repeated_line_count: None,
        repeated_block_count: None,
        energy_lift: None,
        sustained_energy: None,
        evidence: None,
    }
}

fn role_order(role: &str) -> u8 {
    match role {
        "exit" => 0,
        "entry" => 1,
        "avoid-exit" => 2,
        _ => 5,
    }
}

fn unique_candidates(mut candidates: Vec<TransitionCandidate>) -> Vec<TransitionCandidate> {
    candidates.retain(|c| c.time.is_finite());
    candidates.sort_by(|a, b| {
        role_order(&a.role)
            .cmp(&role_order(&b.role))
            .then_with(|| b.confidence.total_cmp(&a.confidence))
            .then_with(|| a.time.total_cmp(&b.time))
    });
    let mut out: Vec<TransitionCandidate> = Vec::new();
    for cand in candidates {
        let dup = out
            .iter()
            .any(|item| item.kind == cand.kind && (item.time - cand.time).abs() < 1.5);
        if !dup {
            out.push(cand);
        }
    }
    out
}

fn add_energy_candidates(candidates: &mut Vec<TransitionCandidate>, beats: &[BeatEvent], duration: f64) {
    if beats.is_empty() {
        return;
    }
    let step = beats[0].step.max(0.0).max(0.5);
    let scan_step = (step * 8.0).max(4.0);
    let mut windows: Vec<(f64, f64)> = Vec::new();
    let mut time = 0.0;
    while time < duration - 8.0 {
        let (energy, _, _) = window_stats(beats, time, 8.0);
        windows.push((round3(time), energy));
        time += scan_step;
    }
    if windows.is_empty() {
        return;
    }

    let early_limit = 24.0_f64.min(duration - 8.0);
    let early_windows: Vec<(f64, f64)> = windows
        .iter()
        .copied()
        .filter(|(t, _)| *t <= early_limit)
        .collect();

    let mut rise: Option<(f64, f64)> = None;
    for (idx, &(wt, we)) in early_windows.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        let prior = early_windows[..idx]
            .iter()
            .copied()
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let (_, _, beat_stability) = window_stats(beats, wt, 8.0);
        let next = early_windows.get(idx + 1).copied();
        if let Some((_, pe)) = prior {
            if we - pe >= 0.12
                && beat_stability >= 0.35
                && next.map(|(_, ne)| ne >= we - 0.04).unwrap_or(true)
            {
                rise = Some((wt, we));
                break;
            }
        }
    }

    if let Some((rt, re)) = rise {
        let low_window = early_windows
            .iter()
            .copied()
            .filter(|(t, _)| *t < rt)
            .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.total_cmp(&b.0)));
        if let Some((lt, le)) = low_window {
            let contrast = re - le;
            let mut c = make_candidate(
                "intro",
                "entry",
                "energy",
                lt,
                round3((0.58 + contrast * 0.4).min(0.9)),
                beats,
            );
            c.resolves_to = Some(ResolvesTo {
                kind: "strong-entry".to_string(),
                time: round3(rt + 4.0),
                text: None,
            });
            candidates.push(c);
        }
    }

    let mut sorted = windows.clone();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let peak = sorted[0];
    candidates.push(make_candidate(
        "peak",
        "avoid-exit",
        "",
        peak.0,
        round3((0.55 + peak.1 * 0.4).min(0.95)),
        beats,
    ));

    let mut release_candidates: Vec<(f64, f64)> = windows
        .iter()
        .copied()
        .filter(|(t, e)| *t > peak.0 + 8.0 && *e <= peak.1 * 0.82)
        .collect();
    release_candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((rel_t, rel_e)) = release_candidates.first().copied() {
        candidates.push(make_candidate(
            "release",
            "exit",
            "",
            rel_t,
            round3((0.5 + (peak.1 - rel_e).max(0.0) * 0.6).min(0.92)),
            beats,
        ));
    }

    let tail_start = (duration - 16.0).max(0.0);
    candidates.push(make_candidate("outro", "exit", "", tail_start, 0.58, beats));
}

fn add_lyric_candidates(candidates: &mut Vec<TransitionCandidate>, lines: &[LrcLine], beats: &[BeatEvent]) {
    for group in repeated_groups(lines) {
        for line in &group {
            let mut c = make_candidate("chorus", "entry", "lyric", line.time, 0.5, beats);
            let kind = if c.energy_after >= c.energy_before {
                "chorus"
            } else {
                "hook"
            };
            c.kind = kind.to_string();
            c.confidence = round3(
                (0.56 + group.len() as f64 * 0.08 + (c.energy_after - c.energy_before).max(0.0) * 0.5)
                    .min(0.95),
            );
            c.text = Some(line.text.clone());
            c.repeats = Some(group.len());
            candidates.push(c);

            if let Some(before) = lines
                .iter()
                .filter(|item| item.time < line.time && item.time >= line.time - 10.0)
                .next_back()
            {
                let mut pre = make_candidate(
                    "pre-section",
                    "entry",
                    "lyric",
                    before.time,
                    round3((0.5 + group.len() as f64 * 0.07).min(0.9)),
                    beats,
                );
                pre.text = Some(before.text.clone());
                pre.resolves_to = Some(ResolvesTo {
                    kind: kind.to_string(),
                    time: round3(line.time),
                    text: Some(line.text.clone()),
                });
                candidates.push(pre);
            }
        }
    }

    if let Some(last_lyric) = lines.last() {
        let mut c = make_candidate("outro", "exit", "", last_lyric.time, 0.62, beats);
        c.text = Some(last_lyric.text.clone());
        candidates.push(c);
    }
}

fn repeated_lyric_times(lines: &[LrcLine]) -> Vec<f64> {
    let mut groups: HashMap<String, Vec<f64>> = HashMap::new();
    for line in lines {
        let key = line.normalized.trim().to_string();
        if key.chars().count() < 4 {
            continue;
        }
        groups.entry(key).or_default().push(line.time);
    }
    let mut out: Vec<f64> = groups
        .into_values()
        .filter(|times| times.len() >= 2)
        .flatten()
        .collect();
    out.sort_by(|a, b| a.total_cmp(b));
    out
}

struct RepeatedBlock {
    lines: Vec<LrcLine>,
    occurrences: usize,
    first_time: f64,
}

fn repeated_lyric_blocks(lines: &[LrcLine]) -> Vec<RepeatedBlock> {
    let mut unique_order: Vec<String> = Vec::new();
    let mut unique: HashMap<String, LrcLine> = HashMap::new();
    for line in lines {
        let normalized = line.normalized.trim().to_string();
        if normalized.chars().count() < 4 || !line.time.is_finite() {
            continue;
        }
        let key = format!("{normalized}\u{0000}{}", line.time);
        if !unique.contains_key(&key) {
            unique_order.push(key.clone());
            unique.insert(
                key,
                LrcLine {
                    time: line.time,
                    text: line.text.clone(),
                    normalized,
                },
            );
        }
    }
    let mut sorted: Vec<LrcLine> = unique_order
        .into_iter()
        .filter_map(|k| unique.remove(&k))
        .collect();
    sorted.sort_by(|a, b| a.time.total_cmp(&b.time));

    let mut pairs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for idx in 0..sorted.len().saturating_sub(1) {
        let first = sorted[idx].normalized.trim();
        let second = sorted[idx + 1].normalized.trim();
        if first.chars().count() < 4 || second.chars().count() < 4 || first == second {
            continue;
        }
        let key = format!("{first}\u{0000}{second}");
        pairs.entry(key).or_default().push(idx);
    }

    let mut out: Vec<RepeatedBlock> = pairs
        .into_values()
        .filter(|indexes| indexes.len() >= 2)
        .flat_map(|indexes| {
            let occ = indexes.len();
            let sorted_ref = &sorted;
            indexes.into_iter().map(move |first_idx| RepeatedBlock {
                lines: sorted_ref[first_idx..first_idx + 2].to_vec(),
                occurrences: occ,
                first_time: sorted_ref[first_idx].time,
            })
        })
        .collect();
    out.sort_by(|a, b| a.first_time.total_cmp(&b.first_time));
    out
}

fn phrase_for_time(phrases: &[Phrase], time: f64) -> Option<(usize, Phrase)> {
    phrases
        .iter()
        .enumerate()
        .find(|(_, p)| time >= p.start && time < p.end)
        .map(|(idx, p)| (idx, p.clone()))
}

struct SignaturePhrase {
    phrase_idx: Option<usize>,
    phrase: Option<Phrase>,
    end: Option<f64>,
    source: String,
    kind: String,
    confidence: f64,
    repeated_line_count: Option<usize>,
    repeated_block_count: Option<usize>,
    energy_lift: Option<f64>,
    sustained_energy: Option<bool>,
}

fn signature_phrase(phrases: &[Phrase], lrc_lines: &[LrcLine]) -> SignaturePhrase {
    let mean = average(&phrases.iter().map(|p| p.energy).collect::<Vec<_>>());
    for block in repeated_lyric_blocks(lrc_lines) {
        let block_phrases: Vec<(usize, Phrase)> = block
            .lines
            .iter()
            .filter_map(|line| phrase_for_time(phrases, line.time))
            .collect();
        if block_phrases.len() == block.lines.len()
            && !block_phrases.is_empty()
            && block_phrases.iter().all(|(_, p)| p.energy >= mean * 0.95)
        {
            let first = &block_phrases[0];
            let last = &block_phrases[block_phrases.len() - 1];
            let energy_lift = average(&block_phrases.iter().map(|(_, p)| p.energy).collect::<Vec<_>>()) - mean;
            return SignaturePhrase {
                phrase_idx: Some(first.0),
                phrase: Some(first.1.clone()),
                end: Some(last.1.end),
                source: "lyric+beat".to_string(),
                kind: "hook".to_string(),
                confidence: 0.88,
                repeated_line_count: Some(block.lines.len()),
                repeated_block_count: Some(block.occurrences),
                energy_lift: Some(energy_lift),
                sustained_energy: Some(true),
            };
        }
    }

    for time in repeated_lyric_times(lrc_lines) {
        if let Some((idx, phrase)) = phrase_for_time(phrases, time) {
            if phrase.energy >= mean * 0.95 {
                return SignaturePhrase {
                    phrase_idx: Some(idx),
                    phrase: Some(phrase),
                    end: None,
                    source: "lyric+beat".to_string(),
                    kind: "hook-candidate".to_string(),
                    confidence: 0.5,
                    repeated_line_count: None,
                    repeated_block_count: None,
                    energy_lift: None,
                    sustained_energy: None,
                };
            }
        }
    }

    let sustained = phrases.iter().enumerate().find(|(idx, phrase)| {
        *idx > 0
            && phrase.energy >= mean * 1.05
            && phrases.get(idx + 1).map(|n| n.energy >= mean * 0.95).unwrap_or(false)
    });

    if let Some((idx, phrase)) = sustained {
        return SignaturePhrase {
            phrase_idx: Some(idx),
            phrase: Some(phrase.clone()),
            end: None,
            source: "beat-only".to_string(),
            kind: "drop".to_string(),
            confidence: 0.62,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
        };
    }

    let fallback = if phrases.len() > 1 {
        phrases
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|(_, a), (_, b)| a.energy.total_cmp(&b.energy))
            .map(|(idx, p)| (idx, p.clone()))
    } else {
        phrases.first().cloned().map(|p| (0, p))
    };

    SignaturePhrase {
        phrase_idx: fallback.as_ref().map(|(i, _)| *i),
        phrase: fallback.map(|(_, p)| p),
        end: None,
        source: "beat-only".to_string(),
        kind: "drop".to_string(),
        confidence: 0.42,
        repeated_line_count: None,
        repeated_block_count: None,
        energy_lift: None,
        sustained_energy: None,
    }
}

fn late_penalty(exit_ratio: f64) -> f64 {
    if exit_ratio <= 0.65 {
        return 0.0;
    }
    if exit_ratio <= 0.78 {
        return round3((exit_ratio - 0.65) / 0.13 * 0.25);
    }
    round3(0.45 + ((exit_ratio - 0.78) * 0.5).min(0.2))
}

fn build_vocal_windows(lines: &[LrcLine], duration: f64) -> Vec<TimeWindow> {
    let mut seen = HashSet::new();
    let mut timed: Vec<f64> = lines
        .iter()
        .filter(|line| !line.normalized.trim().is_empty() || !line.text.trim().is_empty())
        .map(|line| line.time)
        .filter(|t| t.is_finite())
        .collect();
    timed.sort_by(|a, b| a.total_cmp(b));
    timed.retain(|t| seen.insert(round3(*t).to_bits()));

    timed
        .iter()
        .enumerate()
        .filter_map(|(idx, &start)| {
            let estimated_end = match timed.get(idx + 1).copied() {
                Some(next) => (start + 6.5).min((start + 0.8).max(next - 0.18)),
                None => start + 4.5,
            };
            let end = duration.max(start).min(estimated_end);
            let s = round3(start);
            let e = round3(end);
            if e > s {
                Some(TimeWindow { start: s, end: e })
            } else {
                None
            }
        })
        .collect()
}

fn build_exit_candidates(phrases: &[Phrase], duration: f64, protected_until: f64) -> Vec<TransitionCandidate> {
    let search_start = protected_until;
    let search_end = search_start.max(duration - 8.0);
    let mut candidates = Vec::new();

    for (idx, phrase) in phrases.iter().enumerate() {
        let time = phrase.end;
        if time < search_start || time > search_end {
            continue;
        }
        let before = phrase.energy;
        let after = phrases.get(idx + 1).map(|p| p.energy).unwrap_or(before);
        let delta = after - before;
        let exit_ratio = if duration > 0.0 { round3(time / duration) } else { 0.0 };
        let kind = if (time - protected_until).abs() < 1e-9 {
            "post-hook-boundary"
        } else if delta <= -0.08 {
            "release"
        } else {
            "phrase-boundary"
        };
        candidates.push(TransitionCandidate {
            kind: kind.to_string(),
            role: "exit".to_string(),
            source: "structure".to_string(),
            time: round3(time),
            confidence: round3((0.58 - delta * 0.5).clamp(0.35, 0.9)),
            energy_before: round3(before),
            energy_after: round3(after),
            vocal_density: 0.0,
            low_density: 0.0,
            beat_stability: 0.0,
            text: None,
            repeats: None,
            resolves_to: None,
            play_from: None,
            landing_at: None,
            landing_type: None,
            exit_ratio: Some(exit_ratio),
            late_penalty: Some(late_penalty(exit_ratio)),
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        });
    }

    if candidates.is_empty() && duration > protected_until {
        let t = round3(protected_until.max(duration - 2.0));
        let exit_ratio = if duration > 0.0 { round3(t / duration) } else { 0.0 };
        candidates.push(TransitionCandidate {
            kind: "natural-tail".to_string(),
            role: "exit".to_string(),
            source: "structure".to_string(),
            time: t,
            confidence: 0.35,
            energy_before: 0.0,
            energy_after: 0.0,
            vocal_density: 0.0,
            low_density: 0.0,
            beat_stability: 0.0,
            text: None,
            repeats: None,
            resolves_to: None,
            play_from: None,
            landing_at: None,
            landing_type: None,
            exit_ratio: Some(exit_ratio),
            late_penalty: Some(if duration > 0.0 { late_penalty(t / duration) } else { 0.0 }),
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        });
    }

    candidates
}

fn build_structure_map(phrases: &[Phrase], duration: f64, lrc_lines: &[LrcLine]) -> StructureMap {
    let signature = signature_phrase(phrases, lrc_lines);
    let protected_until = round3(match &signature.phrase {
        Some(p) => signature.end.unwrap_or(p.end),
        None => duration.min(32.0),
    });

    let fallback_entry = TransitionCandidate {
        kind: "start".to_string(),
        role: "entry".to_string(),
        source: "fallback".to_string(),
        time: 0.0,
        confidence: 0.35,
        energy_before: 0.0,
        energy_after: 0.0,
        vocal_density: 0.0,
        low_density: 0.0,
        beat_stability: 0.0,
        text: None,
        repeats: None,
        resolves_to: None,
        play_from: Some(0.0),
        landing_at: Some(0.0),
        landing_type: Some("start".to_string()),
        exit_ratio: None,
        late_penalty: None,
        original_type: None,
        repeated_line_count: None,
        repeated_block_count: None,
        energy_lift: None,
        sustained_energy: None,
        evidence: None,
    };

    let evidence = signature.repeated_line_count.map(|rlc| HookEvidence {
        repeated_line_count: rlc,
        repeated_block_count: signature.repeated_block_count.unwrap_or(0),
        energy_lift: round3(signature.energy_lift.unwrap_or(0.0)),
        sustained_energy: signature.sustained_energy.unwrap_or(false),
    });

    let signature_entry = if let Some(ref phrase) = signature.phrase {
        if signature.confidence >= 0.6 {
            Some(TransitionCandidate {
                kind: signature.kind.clone(),
                role: "entry".to_string(),
                source: signature.source.clone(),
                time: round3(phrase.start),
                confidence: signature.confidence,
                energy_before: 0.0,
                energy_after: round3(phrase.energy),
                vocal_density: if signature.source == "lyric+beat" { 0.7 } else { 0.0 },
                low_density: 0.0,
                beat_stability: 0.0,
                text: None,
                repeats: None,
                resolves_to: None,
                play_from: Some(round3(phrase.start)),
                landing_at: Some(round3(phrase.start)),
                landing_type: Some(signature.kind.clone()),
                exit_ratio: None,
                late_penalty: None,
                original_type: None,
                repeated_line_count: signature.repeated_line_count,
                repeated_block_count: signature.repeated_block_count,
                energy_lift: signature.energy_lift.map(round3),
                sustained_energy: signature.sustained_energy,
                evidence: evidence.clone(),
            })
        } else {
            None
        }
    } else {
        None
    };

    let prior_phrase = signature
        .phrase_idx
        .and_then(|idx| if idx > 0 { phrases.get(idx - 1).cloned() } else { None });

    let pre_hook_entry = if let (Some(ref sig_entry), Some(ref prior), Some(ref sig_phrase)) =
        (&signature_entry, &prior_phrase, &signature.phrase)
    {
        Some(TransitionCandidate {
            kind: "pre-hook".to_string(),
            role: "entry".to_string(),
            source: signature.source.clone(),
            time: round3(prior.start),
            confidence: round3((signature.confidence - 0.08).max(0.35)),
            energy_before: round3(prior.energy),
            energy_after: round3(sig_phrase.energy),
            vocal_density: 0.0,
            low_density: 0.0,
            beat_stability: 0.0,
            text: None,
            repeats: None,
            resolves_to: Some(ResolvesTo {
                kind: sig_entry.kind.clone(),
                time: sig_entry.time,
                text: None,
            }),
            play_from: Some(round3(prior.start)),
            landing_at: sig_entry.landing_at,
            landing_type: sig_entry.landing_type.clone(),
            exit_ratio: None,
            late_penalty: None,
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        })
    } else {
        None
    };

    let mut sections = Vec::new();
    if let (Some(ref prior), Some(ref pre_hook)) = (&prior_phrase, &pre_hook_entry) {
        sections.push(StructureSection {
            index: prior.index,
            start: prior.start,
            end: prior.end,
            bars: prior.bars,
            energy: prior.energy,
            kind: "pre-hook".to_string(),
            confidence: pre_hook.confidence,
            source: signature.source.clone(),
            repeated_line_count: signature.repeated_line_count,
            repeated_block_count: signature.repeated_block_count,
            energy_lift: signature.energy_lift.map(round3),
            sustained_energy: signature.sustained_energy,
            evidence: evidence.clone(),
        });
    }
    if let Some(ref phrase) = signature.phrase {
        sections.push(StructureSection {
            index: phrase.index,
            start: phrase.start,
            end: signature.end.unwrap_or(phrase.end),
            bars: phrase.bars,
            energy: phrase.energy,
            kind: signature.kind.clone(),
            confidence: signature.confidence,
            source: signature.source.clone(),
            repeated_line_count: signature.repeated_line_count,
            repeated_block_count: signature.repeated_block_count,
            energy_lift: signature.energy_lift.map(round3),
            sustained_energy: signature.sustained_energy,
            evidence,
        });
    }

    let mut entry_candidates = vec![fallback_entry];
    if let Some(pre) = pre_hook_entry {
        entry_candidates.push(pre);
    }
    if let Some(sig) = signature_entry {
        entry_candidates.push(sig);
    }

    StructureMap {
        duration: round3(duration),
        structure_source: signature.source,
        structure_confidence: signature.confidence,
        protected_until,
        sections,
        vocal_windows: build_vocal_windows(lrc_lines, duration),
        exit_candidates: build_exit_candidates(phrases, duration, protected_until),
        entry_candidates,
    }
}

fn best_cue_candidate<F>(
    candidates: &[TransitionCandidate],
    pred: F,
) -> Option<&TransitionCandidate>
where
    F: Fn(&TransitionCandidate) -> bool,
{
    candidates
        .iter()
        .filter(|c| c.time.is_finite() && pred(c))
        .max_by(|a, b| {
            a.confidence
                .total_cmp(&b.confidence)
                .then_with(|| b.time.total_cmp(&a.time))
        })
}

fn build_cue_points(
    candidates: &[TransitionCandidate],
    downbeats: &[DownbeatInfo],
    bars: &[Bar],
    duration: f64,
) -> CuePoints {
    let intro = best_cue_candidate(candidates, |c| {
        c.role == "entry" && (c.kind == "intro" || c.time <= 24.0)
    })
    .or_else(|| best_cue_candidate(candidates, |c| c.role == "entry"));
    let outro = best_cue_candidate(candidates, |c| {
        c.role == "exit" && (c.kind == "outro" || c.kind == "release")
    })
    .or_else(|| best_cue_candidate(candidates, |c| c.role == "exit"));

    let avg_energy = average(&bars.iter().map(|b| b.energy).collect::<Vec<_>>());
    let first_strong = bars
        .iter()
        .find(|b| b.energy >= avg_energy * 0.95)
        .or_else(|| bars.first());
    let last_exit = candidates
        .iter()
        .filter(|c| c.role == "exit")
        .max_by(|a, b| a.time.total_cmp(&b.time));

    let first_db_time = downbeats.first().map(|d| d.time).unwrap_or(0.0);
    let intro_time = intro.map(|c| c.time).unwrap_or(first_db_time);

    CuePoints {
        intro_start: round3(intro_time),
        intro_end: round3(match first_strong {
            Some(bar) => bar.start.max(intro.map(|c| c.time).unwrap_or(0.0)),
            None => 16.0_f64.min(duration),
        }),
        first_strong_downbeat: round3(first_strong.map(|b| b.start).unwrap_or(first_db_time)),
        outro_start: round3(outro.map(|c| c.time).unwrap_or((duration - 16.0).max(0.0))),
        outro_end: round3(duration),
        last_safe_phrase_end: round3(last_exit.map(|c| c.time).unwrap_or((duration - 8.0).max(0.0))),
    }
}

fn boundary_lift(prev: &Bar, curr: &Bar) -> f64 {
    let energy = (curr.energy - prev.energy).max(0.0) * 1.6;
    let body = (curr.body_density - prev.body_density).max(0.0) * 0.9;
    let snap = (curr.snap_density - prev.snap_density).max(0.0) * 0.9;
    clamp01(energy + body + snap)
}

fn build_shadow_diagnostics(
    bars: &[Bar],
    beats: &[BeatEvent],
    downbeats: &[DownbeatInfo],
    audio_metrics: &AudioMetrics,
) -> Value {
    // 1. Phrase shadow
    let mut valid_bars: Vec<&Bar> = bars.iter().filter(|b| b.end > b.start).collect();
    valid_bars.sort_by(|a, b| a.start.total_cmp(&b.start));

    #[derive(Clone)]
    struct PhraseCand {
        bar_count: usize,
        offset_bars: usize,
        boundaries: usize,
        stability: f64,
        score: f64,
    }
    let mut phrase_cands: Vec<PhraseCand> = Vec::new();
    for bar_count in [4usize, 8, 16, 32] {
        if valid_bars.len() < bar_count * 2 {
            continue;
        }
        let max_offset = (bar_count - 1).min(valid_bars.len() - bar_count * 2);
        for offset_bars in 0..=max_offset {
            let mut boundaries = Vec::new();
            let mut idx = offset_bars + bar_count;
            while idx < valid_bars.len() {
                boundaries.push(boundary_lift(valid_bars[idx - 1], valid_bars[idx]));
                idx += bar_count;
            }
            if boundaries.is_empty() {
                continue;
            }
            let complete_groups = (valid_bars.len() - offset_bars) / bar_count;
            let coverage = clamp01((complete_groups * bar_count) as f64 / valid_bars.len() as f64);
            let slice = &valid_bars[offset_bars..offset_bars + complete_groups * bar_count];
            let stability = average(&slice.iter().map(|b| clamp01(b.beat_stability)).collect::<Vec<_>>());
            let evidence = clamp01(boundaries.len() as f64 / 3.0);
            let novelty = average(&boundaries);
            let score = clamp01((stability * 0.35 + novelty * 0.45 + coverage * 0.2) * (0.55 + evidence * 0.45));
            phrase_cands.push(PhraseCand {
                bar_count,
                offset_bars,
                boundaries: boundaries.len(),
                stability,
                score,
            });
        }
    }
    phrase_cands.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.boundaries.cmp(&a.boundaries))
            .then_with(|| a.bar_count.cmp(&b.bar_count))
            .then_with(|| a.offset_bars.cmp(&b.offset_bars))
    });

    let phrase_json = if let Some(sel) = phrase_cands.first() {
        let confidence = round3(sel.score);
        json!({
            "selectedBars": sel.bar_count,
            "offsetBars": sel.offset_bars,
            "confidence": confidence,
            "candidateCount": phrase_cands.len(),
            "stable": sel.boundaries >= 2 && sel.stability >= 0.7 && confidence >= 0.65,
        })
    } else {
        json!({
            "selectedBars": 8,
            "offsetBars": 0,
            "confidence": 0.0,
            "candidateCount": 0,
            "stable": false,
        })
    };

    // 2. Tempo shadow
    let mut times: Vec<f64> = beats.iter().map(|b| b.time).filter(|t| t.is_finite()).collect();
    times.sort_by(|a, b| a.total_cmp(b));
    let mut intervals = Vec::new();
    for i in 1..times.len() {
        let interval = times[i] - times[i - 1];
        if (0.15..=2.0).contains(&interval) {
            intervals.push(interval);
        }
    }
    let median_step = median_opt(&intervals);
    let (tempo_json, tempo_stable) = match median_step {
        None => (
            json!({
                "medianStep": Value::Null,
                "localBpm": Value::Null,
                "driftRatio": Value::Null,
                "confidence": 0.0,
                "stable": false,
            }),
            false,
        ),
        Some(ms) => {
            let devs: Vec<f64> = intervals.iter().map(|i| (i - ms).abs()).collect();
            let deviation = median_opt(&devs).unwrap_or(0.0);
            let drift_ratio = deviation / ms;
            let confidence = clamp01((intervals.len() as f64 / 32.0) * (1.0 - clamp01(drift_ratio * 4.0)));
            let stable = intervals.len() >= 16 && drift_ratio <= 0.025 && confidence >= 0.65;
            (
                json!({
                    "medianStep": round_digits(ms, 6),
                    "localBpm": round_digits(60.0 / ms, 2),
                    "driftRatio": round_digits(drift_ratio, 6),
                    "confidence": round3(confidence),
                    "stable": stable,
                }),
                stable,
            )
        }
    };

    // 3. Downbeat shadow
    let mut sorted_db = downbeats.to_vec();
    sorted_db.sort_by(|a, b| a.time.total_cmp(&b.time));
    let downbeat_json = match median_step {
        Some(ms) if sorted_db.len() >= 2 => {
            let expected_bar = ms * 4.0;
            let mut errors = Vec::new();
            for i in 1..sorted_db.len() {
                let interval = sorted_db[i].time - sorted_db[i - 1].time;
                let bar_mult = (interval / expected_bar).round().max(1.0);
                errors.push((interval - expected_bar * bar_mult).abs());
            }
            let phase_error_ms = median_opt(&errors).unwrap_or(0.0) * 1000.0;
            let avg_conf = average(&sorted_db.iter().map(|b| clamp01(b.confidence)).collect::<Vec<_>>());
            let confidence = clamp01(
                avg_conf
                    * clamp01((sorted_db.len() - 1) as f64 / 8.0)
                    * (1.0 - clamp01(phase_error_ms / 120.0)),
            );
            let rounded_conf = round3(confidence);
            let rounded_err = round3(phase_error_ms);
            let stable = tempo_stable && sorted_db.len() >= 4 && phase_error_ms <= 40.0 && confidence >= 0.65;
            json!({
                "confidence": rounded_conf,
                "phaseErrorMs": rounded_err,
                "stable": stable,
            })
        }
        _ => json!({
            "confidence": 0.0,
            "phaseErrorMs": Value::Null,
            "stable": false,
        }),
    };

    // 4. Loudness shadow
    let loudness_json = match (audio_metrics.short_term_lufs, audio_metrics.true_peak_dbtp) {
        (Some(lufs), Some(tp)) if (-80.0..=6.0).contains(&lufs) && (-80.0..=6.0).contains(&tp) => json!({
            "available": true,
            "shortTermLufs": round3(lufs),
            "truePeakDbtp": round3(tp),
            "reason": "",
        }),
        _ => json!({
            "available": false,
            "shortTermLufs": Value::Null,
            "truePeakDbtp": Value::Null,
            "reason": "measurement-unavailable",
        }),
    };

    json!({
        "schema": "cuefield-shadow-v1",
        "phrase": phrase_json,
        "tempo": tempo_json,
        "downbeat": downbeat_json,
        "loudness": loudness_json,
    })
}

/// 解析缓存的节拍图谱条目并结合 LRC 歌词生成完整的曲目结构与候选点分析。
pub fn analyze_cache_entry(entry: &Value, key: &str, lrc_lines: &[LrcLine]) -> NormalizedAnalysis {
    let map = entry.get("map").unwrap_or(&Value::Null);
    let meta = entry.get("meta").unwrap_or(&Value::Null);

    let id = entry
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string();
    let title = meta
        .get("title")
        .or_else(|| entry.get("title"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string();
    let artist = meta
        .get("artist")
        .or_else(|| entry.get("artist"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let raw_duration = val_f64(map.get("duration"), 0.0);

    let raw_grid_step = val_f64(map.get("gridStep"), 0.0);
    let raw_beats_arr = map
        .get("cameraBeats")
        .or_else(|| map.get("beats"))
        .or_else(|| map.get("kicks"))
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let raw_grid_beats_arr = map
        .get("pulseBeats")
        .or_else(|| map.get("beats"))
        .or_else(|| map.get("cameraBeats"))
        .or_else(|| map.get("kicks"))
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut norm_beats: Vec<BeatEvent> = raw_beats_arr
        .iter()
        .enumerate()
        .filter_map(|(i, b)| normalize_beat_event(b, i, raw_grid_step))
        .collect();
    norm_beats.sort_by(|a, b| a.time.total_cmp(&b.time));

    let mut norm_grid_beats: Vec<BeatEvent> = raw_grid_beats_arr
        .iter()
        .enumerate()
        .filter_map(|(i, b)| normalize_beat_event(b, i, raw_grid_step))
        .collect();
    norm_grid_beats.sort_by(|a, b| a.time.total_cmp(&b.time));

    let has_explicit_meter = norm_beats
        .iter()
        .any(|b| b.downbeat || b.phrase || !b.combo.is_empty());
    let downbeat_refs: Vec<&BeatEvent> = norm_beats
        .iter()
        .enumerate()
        .filter(|(i, b)| {
            if b.downbeat || b.phrase || b.combo == "downbeat" {
                true
            } else {
                !has_explicit_meter && raw_grid_step > 0.0 && i % 4 == 0
            }
        })
        .map(|(_, b)| b)
        .collect();

    let (quality_step, tempo_stability, beat_confidence, downbeat_stability, data_confidence) =
        beat_grid_quality(&norm_beats, raw_grid_step, &downbeat_refs, map);

    let effective_grid_step = if quality_step > 0.0 { quality_step } else { raw_grid_step };

    // 按 cue-profile.js normalizeBeats 对时间做 round3 并补充 gridStep
    let mut beats: Vec<BeatEvent> = norm_beats
        .into_iter()
        .map(|mut b| {
            b.time = round3(b.time);
            b
        })
        .collect();
    beats.sort_by(|a, b| a.time.total_cmp(&b.time));

    let grid_step = if effective_grid_step > 0.0 {
        effective_grid_step
    } else {
        infer_grid_step(map, &beats)
    };

    let mut duration = if raw_duration > 0.0 {
        raw_duration
    } else {
        beats.last().map(|b| b.time).unwrap_or(0.0)
    };
    let last_beat_plus_step = beats.last().map(|b| b.time + grid_step).unwrap_or(0.0);
    duration = round3(duration.max(last_beat_plus_step));

    let track = TrackSummary {
        id,
        title,
        artist,
        duration,
    };

    // Section candidates
    let mut base_candidates = Vec::new();
    add_energy_candidates(&mut base_candidates, &beats, duration);
    add_lyric_candidates(&mut base_candidates, lrc_lines, &beats);
    let base_candidates = unique_candidates(base_candidates);

    // Build cue profile
    let has_explicit_for_profile = beats
        .iter()
        .any(|b| b.downbeat || b.phrase || !b.combo.is_empty());
    let downbeats: Vec<DownbeatInfo> = beats
        .iter()
        .enumerate()
        .filter(|(i, b)| is_downbeat(b, *i, has_explicit_for_profile))
        .map(|(_, b)| DownbeatInfo {
            time: b.time,
            confidence: b.confidence,
            energy: round3(beat_energy(b)),
        })
        .collect();

    let bars = build_bars(&beats, &downbeats, grid_step, duration, 0.75);

    let mut grid_beats: Vec<BeatEvent> = if norm_grid_beats.is_empty() {
        beats.clone()
    } else {
        norm_grid_beats
            .into_iter()
            .map(|mut b| {
                b.time = round3(b.time);
                b
            })
            .collect()
    };
    grid_beats.sort_by(|a, b| a.time.total_cmp(&b.time));
    let grid_has_explicit = grid_beats
        .iter()
        .any(|b| b.downbeat || b.phrase || !b.combo.is_empty());
    let grid_downbeats: Vec<DownbeatInfo> = grid_beats
        .iter()
        .enumerate()
        .filter(|(i, b)| is_downbeat(b, *i, grid_has_explicit))
        .map(|(_, b)| DownbeatInfo {
            time: b.time,
            confidence: b.confidence,
            energy: round3(beat_energy(b)),
        })
        .collect();
    let grid_bars = build_bars(&grid_beats, &grid_downbeats, grid_step, duration, 0.7);

    let phrases = build_phrases(&bars, duration);
    let (energy_windows, bass_windows, vocal_windows) = build_windows(&beats, duration, 8.0);

    let structure_map = build_structure_map(&phrases, duration, lrc_lines);

    let mut all_candidates = base_candidates;
    all_candidates.extend(structure_map.exit_candidates.iter().cloned());
    all_candidates.extend(structure_map.entry_candidates.iter().cloned());

    let cue_points = build_cue_points(&all_candidates, &downbeats, &bars, duration);

    let raw_audio_metrics = map.get("audioMetrics");
    let metric_val = |field: &str| -> Option<f64> {
        let num = opt_f64(raw_audio_metrics.and_then(|m| m.get(field)))?;
        if (-80.0..=6.0).contains(&num) {
            Some(round3(num))
        } else {
            None
        }
    };
    let audio_metrics = AudioMetrics {
        short_term_lufs: metric_val("shortTermLufs"),
        true_peak_dbtp: metric_val("truePeakDbtp"),
    };

    let grid_quality = GridQuality {
        downbeat_count: grid_downbeats.len(),
        downbeat_confidence: round3(average(
            &grid_downbeats.iter().map(|b| b.confidence).collect::<Vec<_>>(),
        )),
        beat_stability: round3(average(
            &grid_bars.iter().map(|b| b.beat_stability).collect::<Vec<_>>(),
        )),
        timing_stability: grid_timing_stability(&grid_beats, grid_step),
        tempo_stability,
        beat_confidence,
        downbeat_stability,
        data_confidence,
    };

    let shadow = build_shadow_diagnostics(&bars, &beats, &downbeats, &audio_metrics);
    let bpm = if grid_step > 0.0 {
        round_digits(60.0 / grid_step, 2)
    } else {
        0.0
    };
    let musical_profile = map.get("musicalProfile").filter(|v| !v.is_null()).cloned();

    NormalizedAnalysis {
        track,
        duration,
        bpm,
        grid_step,
        beats,
        downbeats,
        bars,
        phrases,
        grid_quality,
        cue_points,
        energy_windows,
        bass_windows,
        vocal_windows,
        candidates: all_candidates,
        structure_map,
        musical_profile,
        audio_metrics,
        shadow,
    }
}

pub(crate) fn compact_string(value: &str, max_len: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_len {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_len).collect()
    }
}

pub(crate) fn compact_transition_point(point: &TransitionCandidate) -> Value {
    json!({
        "type": compact_string(&point.kind, 32),
        "role": compact_string(&point.role, 16),
        "source": compact_string(&point.source, 24),
        "time": if point.time.is_finite() { json!(point.time) } else { Value::Null },
        "confidence": if point.confidence.is_finite() { json!(point.confidence) } else { Value::Null },
        "playFrom": point.play_from.filter(|v| v.is_finite()),
        "landingAt": point.landing_at.filter(|v| v.is_finite()),
        "landingType": compact_string(point.landing_type.as_deref().unwrap_or(""), 32),
    })
}

/// 生成精简版曲式结构图谱摘要（供 API 响应与诊断使用）。
pub fn compact_structure_map(map: &StructureMap) -> Value {
    let exit_count = map.exit_candidates.len().min(24);
    let entry_count = map.entry_candidates.len().min(24);
    let exit_candidates: Vec<Value> = map
        .exit_candidates
        .iter()
        .take(8)
        .map(compact_transition_point)
        .collect();
    let entry_candidates: Vec<Value> = map
        .entry_candidates
        .iter()
        .take(6)
        .map(compact_transition_point)
        .collect();

    json!({
        "structureSource": compact_string(&map.structure_source, 24),
        "structureConfidence": if map.structure_confidence.is_finite() { json!(map.structure_confidence) } else { Value::Null },
        "protectedUntil": if map.protected_until.is_finite() { json!(map.protected_until) } else { Value::Null },
        "exitCandidateCount": exit_count,
        "entryCandidateCount": entry_count,
        "exitCandidates": exit_candidates,
        "entryCandidates": entry_candidates,
    })
}

// ============================================================================
// 音乐属性对比与过渡候选评估（供 planner.rs 复用）
// ============================================================================

#[derive(Debug, Clone)]
pub(crate) struct MusicalComparison {
    pub score: f64,
    pub harmonic_similarity: f64,
    pub key_compatibility: f64,
    pub melody_similarity: f64,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalMusicalComparison {
    pub score: f64,
    pub harmonic_similarity: f64,
    pub key_compatibility: f64,
    pub melody_similarity: f64,
    pub confidence: f64,
    pub a_window_start: f64,
    pub b_window_start: f64,
    pub a_distance: f64,
    pub b_distance: f64,
    pub risks: Vec<String>,
}

fn cosine_sim(first: &[f64], second: &[f64]) -> f64 {
    let mut dot = 0.0;
    let mut a = 0.0;
    let mut b = 0.0;
    for (x, y) in first.iter().zip(second.iter()) {
        let xv = if x.is_finite() { *x } else { 0.0 };
        let yv = if y.is_finite() { *y } else { 0.0 };
        dot += xv * yv;
        a += xv * xv;
        b += yv * yv;
    }
    if a > 0.0 && b > 0.0 {
        clamp01(dot / (a * b).sqrt())
    } else {
        0.0
    }
}

fn key_compatibility(first: Option<&Value>, second: Option<&Value>) -> f64 {
    let (Some(f), Some(s)) = (first, second) else {
        return 0.5;
    };
    let (Some(fr), Some(sr)) = (opt_f64(f.get("root")), opt_f64(s.get("root"))) else {
        return 0.5;
    };
    let r1 = fr.round() as i32;
    let r2 = sr.round() as i32;
    let m1 = f.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    let m2 = s.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    let d1 = ((r1 - r2).rem_euclid(12)) as i32;
    let d2 = ((r2 - r1).rem_euclid(12)) as i32;
    let distance = d1.min(d2);

    if r1 == r2 && m1 == m2 {
        return 1.0;
    }
    let relative = (m1 == "major" && m2 == "minor" && r2 == (r1 + 9).rem_euclid(12))
        || (m1 == "minor" && m2 == "major" && r2 == (r1 + 3).rem_euclid(12));
    if relative {
        return 0.94;
    }
    if r1 == r2 {
        return 0.78;
    }
    if distance == 5 {
        return if m1 == m2 { 0.86 } else { 0.72 };
    }
    if distance == 2 {
        return if m1 == m2 { 0.68 } else { 0.58 };
    }
    if distance == 1 {
        return 0.42;
    }
    if distance == 6 {
        return 0.18;
    }
    0.5
}

fn f64_vec(v: Option<&Value>) -> Vec<f64> {
    v.and_then(|arr| arr.as_array())
        .map(|items| items.iter().map(|x| val_f64(Some(x), 0.0)).collect())
        .unwrap_or_default()
}

pub(crate) fn compare_musical_profiles(first: &Value, second: &Value) -> MusicalComparison {
    let pcp1 = f64_vec(first.get("pitchClassProfile"));
    let pcp2 = f64_vec(second.get("pitchClassProfile"));
    let harmonic = cosine_sim(&pcp1, &pcp2);
    let key_score = key_compatibility(first.get("key"), second.get("key"));
    let ip1 = f64_vec(first.get("intervalProfile"));
    let ip2 = f64_vec(second.get("intervalProfile"));
    let melody = cosine_sim(&ip1, &ip2);
    let score = clamp01(harmonic * 0.45 + key_score * 0.35 + melody * 0.2);
    let mut risks = Vec::new();
    if harmonic < 0.62 && key_score < 0.5 {
        risks.push("harmonic-clash".to_string());
    }
    if melody < 0.28 {
        risks.push("melody-contour-contrast".to_string());
    }
    MusicalComparison {
        score: round3(score),
        harmonic_similarity: round3(harmonic),
        key_compatibility: round3(key_score),
        melody_similarity: round3(melody),
        risks,
    }
}

fn distance_to_window(time: f64, window: &Value) -> f64 {
    let Some(start) = opt_f64(window.get("start")) else {
        return f64::INFINITY;
    };
    let Some(duration) = opt_f64(window.get("duration")) else {
        return f64::INFINITY;
    };
    if !time.is_finite() || duration < 0.0 {
        return f64::INFINITY;
    }
    let end = start + duration;
    if time < start {
        start - time
    } else if time > end {
        time - end
    } else {
        0.0
    }
}

fn nearest_reliable_window<'a>(profile: &'a Value, time: f64) -> Option<(&'a Value, f64)> {
    let windows = profile.get("windows").and_then(|v| v.as_array())?;
    let mut matches: Vec<(&Value, f64)> = windows
        .iter()
        .filter(|w| {
            val_f64(w.get("confidence"), 0.0) >= 0.55 && val_f64(w.get("noteCount"), 0.0) >= 12.0
        })
        .filter_map(|w| {
            let dist = distance_to_window(time, w);
            let dur = val_f64(w.get("duration"), 0.0);
            if dist <= 2.0_f64.max(dur * 1.5) {
                Some((w, dist))
            } else {
                None
            }
        })
        .collect();
    matches.sort_by(|a, b| {
        a.1.total_cmp(&b.1).then_with(|| {
            val_f64(b.0.get("confidence"), 0.0).total_cmp(&val_f64(a.0.get("confidence"), 0.0))
        })
    });
    matches.into_iter().next()
}

pub(crate) fn compare_local_musical_windows(
    first: Option<&Value>,
    second: Option<&Value>,
    first_time: f64,
    second_time: f64,
) -> Option<LocalMusicalComparison> {
    let (a_win, a_dist) = nearest_reliable_window(first?, first_time)?;
    let (b_win, b_dist) = nearest_reliable_window(second?, second_time)?;
    let cmp = compare_musical_profiles(a_win, b_win);
    Some(LocalMusicalComparison {
        score: cmp.score,
        harmonic_similarity: cmp.harmonic_similarity,
        key_compatibility: cmp.key_compatibility,
        melody_similarity: cmp.melody_similarity,
        confidence: round3(
            val_f64(a_win.get("confidence"), 0.0).min(val_f64(b_win.get("confidence"), 0.0)),
        ),
        a_window_start: round3(val_f64(a_win.get("start"), 0.0)),
        b_window_start: round3(val_f64(b_win.get("start"), 0.0)),
        a_distance: round3(a_dist),
        b_distance: round3(b_dist),
        risks: cmp.risks,
    })
}

fn eval_normalized_text(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_space = true;
    for ch in lower.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            out.push(ch);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_string()
}

fn eval_content_tokens(value: &str) -> HashSet<String> {
    const STOPWORDS: [&str; 17] = [
        "a", "an", "and", "are", "i", "i'm", "in", "is", "it", "me", "my", "of", "on", "the",
        "to", "we", "you",
    ];
    eval_normalized_text(value)
        .split_whitespace()
        .filter(|t| t.chars().count() > 2 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

pub(crate) fn is_closed_outgoing_phrase(value: &str) -> bool {
    let text = eval_normalized_text(value);
    if text.is_empty() {
        return false;
    }
    ["what's going on", "whats going on", "what is going on", "boss bitch"]
        .iter()
        .any(|p| text.contains(p))
}

struct StyleProfile {
    patterns: &'static [&'static str],
    families: &'static [&'static str],
    moods: &'static [&'static str],
}

const STYLE_PROFILES: [StyleProfile; 13] = [
    StyleProfile { patterns: &["avicii"], families: &["electronic", "dance"], moods: &["uplifting", "anthemic"] },
    StyleProfile { patterns: &["bingo players"], families: &["electronic", "dance"], moods: &["uplifting"] },
    StyleProfile { patterns: &["odesza"], families: &["electronic", "cinematic"], moods: &["uplifting", "warm"] },
    StyleProfile { patterns: &["apashe"], families: &["electronic", "cinematic"], moods: &["dark", "theatrical"] },
    StyleProfile { patterns: &["acdc", "highway to hell"], families: &["rock"], moods: &["raw", "anthemic"] },
    StyleProfile { patterns: &["nirvana"], families: &["rock"], moods: &["grunge", "dark"] },
    StyleProfile { patterns: &["marilyn manson", "sweet dreams"], families: &["rock"], moods: &["gothic", "dark"] },
    StyleProfile { patterns: &["glass animals", "heat waves"], families: &["alt-pop", "electronic"], moods: &["warm", "melodic"] },
    StyleProfile { patterns: &["flume", "never be like you"], families: &["electronic", "alt-pop"], moods: &["warm", "future"] },
    StyleProfile { patterns: &["lorde"], families: &["alt-pop"], moods: &["melancholic"] },
    StyleProfile { patterns: &["snakehips"], families: &["electronic", "pop"], moods: &["warm"] },
    StyleProfile { patterns: &["2hollis"], families: &["electronic", "hyperpop"], moods: &["bright"] },
    StyleProfile { patterns: &["doja", "cardi", "lizzo", "lil wayne", "drake", "kanye", "connor price"], families: &["rap"], moods: &["rhythmic"] },
];

fn style_profile_for(analysis: &NormalizedAnalysis) -> Option<&'static StyleProfile> {
    let combined = format!("{} {}", analysis.track.artist, analysis.track.title);
    let text = eval_normalized_text(&combined);
    STYLE_PROFILES
        .iter()
        .find(|p| p.patterns.iter().any(|pat| text.contains(pat)))
}

pub(crate) fn infer_style_compatibility(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
) -> Option<f64> {
    let f = style_profile_for(from)?;
    let t = style_profile_for(to)?;
    let family_overlap = f.families.iter().filter(|x| t.families.contains(x)).count();
    let mood_overlap = f.moods.iter().filter(|x| t.moods.contains(x)).count();
    let mut score = 0.5;
    score += if family_overlap > 0 {
        (family_overlap as f64 * 0.16).min(0.3)
    } else {
        -0.04
    };
    score += if mood_overlap > 0 {
        (mood_overlap as f64 * 0.12).min(0.18)
    } else {
        0.0
    };
    let has_tag = |p: &StyleProfile, tag: &str| p.families.contains(&tag) || p.moods.contains(&tag);
    if has_tag(f, "uplifting") && (has_tag(t, "dark") || has_tag(t, "gothic")) {
        score -= 0.18;
    }
    if has_tag(f, "electronic") && has_tag(t, "rock") && family_overlap == 0 {
        score -= 0.08;
    }
    if has_tag(t, "gothic") && mood_overlap == 0 {
        score -= 0.06;
    }
    Some(round3(clamp01(score)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairEvaluation {
    pub recipe: String,
    pub score: f64,
    pub tier: String,
    pub lyric_handoff: f64,
    pub reasons: Vec<String>,
    pub risks: Vec<String>,
}

pub(crate) fn evaluate_transition_pair(
    exit: Option<&TransitionCandidate>,
    entry: Option<&TransitionCandidate>,
    from_duration: f64,
    style_compatibility: Option<f64>,
) -> PairEvaluation {
    let exit_text = exit.and_then(|e| e.text.as_deref()).unwrap_or("").trim();
    let entry_text = entry.and_then(|e| e.text.as_deref()).unwrap_or("").trim();

    let exit_suitability = match exit {
        None => 0.0,
        Some(ex) => {
            let mut s: f64 = if ex.kind == "outro" { 0.74 } else { 0.64 };
            if ex.kind == "release" {
                s += 0.08;
            }
            if exit_text.is_empty() {
                s += 0.04;
            }
            if ex.energy_after < ex.energy_before {
                s += 0.07;
            }
            if is_closed_outgoing_phrase(exit_text) {
                s = s.min(0.36);
            }
            round3(clamp01(s))
        }
    };

    let entry_promise = match entry {
        None => 0.0,
        Some(en) => {
            let mut s = if en.confidence != 0.0 { en.confidence } else { 0.5 };
            if en.kind == "pre-section" {
                s += 0.16;
            }
            if en.kind == "chorus" || en.kind == "hook" {
                s += 0.12;
            }
            if en
                .resolves_to
                .as_ref()
                .map(|r| r.kind == "chorus" || r.kind == "hook")
                .unwrap_or(false)
            {
                s += 0.12;
            }
            if !entry_text.is_empty() {
                s += 0.04;
            }
            round3(clamp01(s))
        }
    };

    let pair_compat = match (exit, entry) {
        (Some(ex), Some(en)) => {
            let low_diff = (ex.low_density - en.low_density).abs();
            let energy_diff = (ex.energy_after - en.energy_after).abs();
            round3(clamp01(1.0 - low_diff * 1.4 - energy_diff * 0.8))
        }
        _ => 0.35,
    };

    let lyric_handoff = if exit_text.is_empty() || entry_text.is_empty() {
        0.0
    } else {
        let ex_tokens = eval_content_tokens(exit_text);
        let en_tokens = eval_content_tokens(entry_text);
        let shared = ex_tokens.iter().filter(|t| en_tokens.contains(*t)).count();
        let mut s = if shared > 0 {
            (0.48 + shared as f64 * 0.16).min(0.78)
        } else {
            0.0
        };
        let ex_norm = eval_normalized_text(exit_text);
        let en_norm = eval_normalized_text(entry_text);
        let dir_exit = ["break through", "through", "help me", "rise", "above", "away", "out"]
            .iter()
            .any(|p| ex_norm.contains(p));
        let pickup = ["take me to", "take me", "to the", "into", "come with", "go to"]
            .iter()
            .any(|p| en_norm.contains(p));
        if dir_exit && pickup {
            s = s.max(0.84);
        }
        round3(clamp01(s))
    };

    let directionality = match (exit, entry) {
        (Some(ex), Some(en)) => {
            let delta = en.energy_after - ex.energy_after;
            if delta > 0.18 {
                0.46
            } else if delta > 0.04 {
                0.72
            } else if delta < -0.16 {
                0.42
            } else {
                0.68
            }
        }
        _ => 0.5,
    };

    let style_compat = style_compatibility.map(|v| round3(clamp01(v)));
    let exit_is_outro = exit
        .map(|e| e.kind == "outro" || e.kind == "release")
        .unwrap_or(false);
    let exit_late_enough = from_duration <= 0.0
        || exit
            .map(|e| e.time / from_duration >= 0.89)
            .unwrap_or(false);

    let recipe = if lyric_handoff >= 0.75 {
        "lyric-handoff".to_string()
    } else if exit_text.is_empty()
        && !entry_text.is_empty()
        && exit_is_outro
        && exit_late_enough
        && entry_promise >= 0.75
    {
        "instrumental-outro-to-vocal-hook".to_string()
    } else if entry.map(|e| e.kind == "pre-section").unwrap_or(false) {
        "outro-to-chorus".to_string()
    } else {
        "section-jump".to_string()
    };

    let mut reasons = Vec::new();
    let mut risks = Vec::new();
    if lyric_handoff >= 0.75 {
        reasons.push("lyric handoff".to_string());
    }
    if recipe == "instrumental-outro-to-vocal-hook" {
        reasons.push("instrumental outro to vocal hook".to_string());
    }
    if pair_compat >= 0.8 {
        reasons.push("stable energy handoff".to_string());
    }
    if is_closed_outgoing_phrase(exit_text) {
        risks.push("closed outgoing phrase".to_string());
    }
    if directionality < 0.5 {
        risks.push("directionality mismatch".to_string());
    }
    if let Some(sc) = style_compat {
        if sc < 0.62 {
            risks.push("style bridge mismatch".to_string());
        }
    }

    let recipe_bonus = if recipe == "lyric-handoff" {
        0.1
    } else if recipe == "instrumental-outro-to-vocal-hook" {
        0.18
    } else {
        0.0
    };
    let score = round3(clamp01(
        exit_suitability * 0.25
            + entry_promise * 0.25
            + pair_compat * 0.2
            + lyric_handoff * 0.2
            + directionality * 0.1
            + recipe_bonus,
    ));

    let tier = if risks.iter().any(|r| r == "closed outgoing phrase") || score < 0.55 {
        "reject"
    } else if score < 0.7 {
        "weak"
    } else if risks.iter().any(|r| r == "style bridge mismatch") {
        "usable_but_not_magic"
    } else if (recipe == "lyric-handoff" || recipe == "instrumental-outro-to-vocal-hook")
        && pair_compat >= 0.8
        && entry_promise >= 0.75
        && directionality >= 0.6
    {
        "magic"
    } else if score < 0.84 {
        "usable_but_not_magic"
    } else {
        "usable"
    };

    PairEvaluation {
        recipe,
        score,
        tier: tier.to_string(),
        lyric_handoff,
        reasons,
        risks,
    }
}

pub(crate) fn score_entry_candidate(candidate: &TransitionCandidate) -> f64 {
    if candidate.role != "entry" {
        return 0.0;
    }
    let mut score = candidate.confidence;
    if candidate.kind == "pre-section" {
        score += 0.2;
    }
    if candidate.kind == "chorus" || candidate.kind == "hook" {
        score += 0.1;
    }
    if candidate
        .resolves_to
        .as_ref()
        .map(|r| r.kind == "chorus" || r.kind == "hook")
        .unwrap_or(false)
    {
        score += 0.08;
    }
    score
}

pub(crate) fn score_candidate_pair(
    exit: &TransitionCandidate,
    entry: &TransitionCandidate,
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    exit_score: f64,
    exits: &[TransitionCandidate],
) -> (String, f64, PairEvaluation) {
    let base_score = exit_score * 0.5 + score_entry_candidate(entry) * 0.5;
    let style_compat = infer_style_compatibility(from, to);
    let mut evaluation = evaluate_transition_pair(Some(exit), Some(entry), from.duration, style_compat);
    let near_closed = exit.text.is_none()
        && exits.iter().any(|c| {
            c.text.as_deref().map(|t| !t.is_empty() && is_closed_outgoing_phrase(t)).unwrap_or(false)
                && (c.time - exit.time).abs() <= 8.0
        });
    let mut pair_score = base_score * 0.7 + evaluation.score * 0.3;
    if evaluation.recipe == "lyric-handoff" {
        pair_score += 0.22;
    } else if evaluation.recipe == "instrumental-outro-to-vocal-hook" {
        pair_score += 0.07;
    }
    if evaluation.risks.iter().any(|r| r == "closed outgoing phrase") {
        pair_score = pair_score.min(0.74);
    }
    if near_closed {
        pair_score = pair_score.min(0.74);
        if !evaluation.risks.iter().any(|r| r == "near closed outgoing phrase") {
            evaluation.risks.push("near closed outgoing phrase".to_string());
        }
    }
    let recipe = if evaluation.recipe.is_empty() {
        if entry.kind == "pre-section" {
            "outro-to-chorus".to_string()
        } else {
            "section-jump".to_string()
        }
    } else {
        evaluation.recipe.clone()
    };
    (recipe, round3(pair_score.min(0.99)), evaluation)
}
