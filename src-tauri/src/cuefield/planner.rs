use crate::cuefield::feedback::compact_version;
use crate::cuefield::lrc::{parse_lrc, score_lyric_link, LrcLine};
use crate::cuefield::structure::{
    analyze_cache_entry, clamp01, compact_string, compact_structure_map,
    compact_transition_point, compare_local_musical_windows, compare_musical_profiles,
    opt_f64, round3, round_digits, score_candidate_pair, val_f64, NormalizedAnalysis,
    PairEvaluation, StructureMap, TransitionCandidate, ValueWindow,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::f64::consts::PI;

const LANDING_TOLERANCE_SEC: f64 = 0.05;
const MINIMUM_TERMINAL_OVERLAP: f64 = 2.2;
const INVALID_LANDING_ERROR: f64 = 9_007_199_254_740_991.0;

const ACTION_FIELDS: [&str; 29] = [
    "t",
    "deck",
    "op",
    "at",
    "volume",
    "value",
    "duration",
    "curve",
    "type",
    "bpm",
    "delayBeats",
    "feedback",
    "wet",
    "tailMs",
    "depth",
    "pulses",
    "beats",
    "attack",
    "hold",
    "release",
    "low",
    "mid",
    "high",
    "enabled",
    "startAt",
    "loopBeats",
    "slip",
    "optionalWhenLate",
    "maxLateMs",
];

/// Cuefield AutoMix 过渡规划请求参数。
#[derive(Debug, Clone)]
pub struct TransitionRequest<'a> {
    pub from_key: &'a str,
    pub to_key: &'a str,
    pub from_lrc: Option<&'a str>,
    pub to_lrc: Option<&'a str>,
    pub exit_bias: Option<&'a str>,
    pub max_entry_time: Option<f64>,
    pub recent_recipes: Vec<String>,
    pub minimum_listen_until: Option<f64>,
    pub enable_clean_boundary: bool,
    pub enable_live_end_crossfade_fallback: bool,
    pub enable_cadence_fallback: bool,
    pub synthetic_bridge_enabled: bool,
    pub boundary_evidence: Vec<Value>,
    pub tail_evidence: Option<Value>,
    pub version: Option<Value>,
}

impl Default for TransitionRequest<'_> {
    fn default() -> Self {
        Self {
            from_key: "",
            to_key: "",
            from_lrc: None,
            to_lrc: None,
            exit_bias: None,
            max_entry_time: None,
            recent_recipes: Vec::new(),
            minimum_listen_until: None,
            enable_clean_boundary: false,
            enable_live_end_crossfade_fallback: false,
            enable_cadence_fallback: false,
            synthetic_bridge_enabled: false,
            boundary_evidence: Vec::new(),
            tail_evidence: None,
            version: None,
        }
    }
}

// ============================================================================
// Planner Contracts
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct OverlapContract {
    min: f64,
    max: f64,
}

fn overlap_contract_for(recipe: &str) -> OverlapContract {
    match recipe {
        "clean-boundary-handoff" => OverlapContract { min: 0.0, max: 0.0 },
        "end-of-track-crossfade" => OverlapContract { min: 0.1, max: 3.0 },
        "quick-safe-fade" => OverlapContract { min: 1.2, max: 2.6 },
        "safety-long-blend" => OverlapContract { min: 3.0, max: 8.0 },
        "filtered-pickup" => OverlapContract { min: 2.5, max: 4.5 },
        "bass-eq-handoff" => OverlapContract { min: 2.5, max: 4.5 },
        "intro-outro-long-blend" => OverlapContract { min: 5.0, max: 9.0 },
        "echo-out" => OverlapContract { min: 1.5, max: 3.5 },
        "spectral-emergence" => OverlapContract { min: 5.0, max: 7.5 },
        "harmonic-double-drop" => OverlapContract { min: 1.0, max: 2.5 },
        "tease-roll-double-drop" => OverlapContract { min: 0.8, max: 1.6 },
        "source-loop-roll" => OverlapContract { min: 1.5, max: 3.5 },
        "hook-teaser" => OverlapContract { min: 2.0, max: 4.0 },
        _ => OverlapContract { min: 0.0, max: 0.0 },
    }
}

fn score_overlap_contract(overlap: f64, contract: OverlapContract) -> f64 {
    if !overlap.is_finite() || contract.min < 0.0 || contract.max < contract.min {
        return 0.0;
    }
    if overlap >= contract.min && overlap <= contract.max {
        return 1.0;
    }
    let distance = if overlap < contract.min {
        contract.min - overlap
    } else {
        overlap - contract.max
    };
    let scale = 0.5_f64.max(contract.max - contract.min);
    round3((1.0 - distance / scale).clamp(0.0, 1.0))
}

fn fallback_listen_floor(duration_sec: f64) -> f64 {
    let duration = duration_sec.max(0.0);
    if duration <= 0.0 {
        return 0.0;
    }
    round3(
        (duration - 12.0)
            .min((duration * 0.35).clamp(24.0, 72.0))
            .max(0.0),
    )
}

fn resolve_listening_floor(structure_map: &StructureMap, duration_sec: f64) -> f64 {
    let duration = duration_sec.max(0.0);
    let trusted_source = structure_map.structure_source == "lyric+beat"
        || structure_map.structure_source == "beat-only";
    let trusted = trusted_source
        && structure_map.structure_confidence >= 0.6
        && structure_map.protected_until.is_finite()
        && structure_map.protected_until >= 0.0
        && structure_map.protected_until <= duration;
    if trusted {
        round3(structure_map.protected_until)
    } else {
        fallback_listen_floor(duration)
    }
}

// ============================================================================
// Transition Router
// ============================================================================

#[derive(Debug, Clone)]
struct RoutePolicy {
    route: String,
    compatibility_class: String,
    contrast_direction: String,
    preferred_exit_range: Vec<f64>,
    entry_policy: String,
    overlap_class: String,
    recipe: String,
    reasons: Vec<String>,
    metrics: Value,
}

impl RoutePolicy {
    fn to_value(&self) -> Value {
        json!({
            "route": self.route,
            "compatibilityClass": self.compatibility_class,
            "contrastDirection": self.contrast_direction,
            "preferredExitRange": self.preferred_exit_range,
            "entryPolicy": self.entry_policy,
            "overlapClass": self.overlap_class,
            "recipe": self.recipe,
            "reasons": self.reasons,
            "metrics": self.metrics,
        })
    }
}

fn preferred_exit_range(base_min: f64, base_max: f64, protected_ratio: f64) -> Vec<f64> {
    let lower = base_min.max(protected_ratio);
    vec![lower, base_max.max(lower)]
}

fn terminal_policy(protected_ratio: f64, reasons: Vec<String>, metrics: Value) -> RoutePolicy {
    RoutePolicy {
        route: "terminal-rescue".to_string(),
        compatibility_class: "uncertain".to_string(),
        contrast_direction: "unknown".to_string(),
        preferred_exit_range: preferred_exit_range(0.88, 0.96, protected_ratio),
        entry_policy: "start-or-downbeat".to_string(),
        overlap_class: "short".to_string(),
        recipe: "terminal-rescue".to_string(),
        reasons: reasons.into_iter().take(4).collect(),
        metrics,
    }
}

fn nearest_bar<'a>(analysis: &'a NormalizedAnalysis, target: f64) -> Option<&'a crate::cuefield::structure::Bar> {
    if !target.is_finite() || analysis.bars.is_empty() {
        return None;
    }
    analysis
        .bars
        .iter()
        .min_by(|a, b| (a.start - target).abs().total_cmp(&(b.start - target).abs()))
}

fn classify_transition_route(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    protected_until: f64,
    exits: &[TransitionCandidate],
    entries: &[TransitionCandidate],
    risks: &[String],
    enable_clean_boundary: bool,
    overlap_safe: bool,
) -> RoutePolicy {
    let dir_mismatch = if risks.iter().any(|r| r == "directionality mismatch") {
        1
    } else {
        0
    };
    let style_bridge_mismatch = risks.iter().any(|r| r == "style bridge mismatch");
    let dur_a = from.duration;
    let dur_b = to.duration;
    let bpm_a = from.bpm;
    let bpm_b = to.bpm;
    let tempo_known = bpm_a > 0.0 && bpm_b > 0.0;
    let tempo_delta = if tempo_known {
        (bpm_a - bpm_b).abs() / bpm_a.max(bpm_b)
    } else {
        1.0
    };
    let protected_ratio = (protected_until / dur_a.max(1.0)).clamp(0.0, 1.0);
    let base_metrics = json!({
        "fromSnap": 0,
        "toSnap": 0,
        "snapDelta": 0,
        "energyDelta": 0,
        "tempoDelta": tempo_delta,
        "tempoKnown": if tempo_known { 1 } else { 0 },
        "directionalityMismatch": dir_mismatch,
    });

    if !(dur_a > 0.0) || !(dur_b > 0.0) {
        return terminal_policy(protected_ratio, vec!["invalid-duration".to_string()], base_metrics);
    }
    if !tempo_known {
        return terminal_policy(protected_ratio, vec!["invalid-bpm".to_string()], base_metrics);
    }

    let best_exit = exits
        .iter()
        .filter(|e| e.source != "fallback" && e.time.is_finite())
        .max_by(|a, b| {
            let sa = 100.0
                + if a.kind.eq_ignore_ascii_case("release") { 10.0 } else { 0.0 }
                + a.confidence
                - a.late_penalty.unwrap_or(0.0);
            let sb = 100.0
                + if b.kind.eq_ignore_ascii_case("release") { 10.0 } else { 0.0 }
                + b.confidence
                - b.late_penalty.unwrap_or(0.0);
            sa.total_cmp(&sb)
        });
    let best_entry = entries
        .iter()
        .filter(|e| e.source != "fallback" && e.landing_at.or(Some(e.time)).map(|t| t.is_finite()).unwrap_or(false))
        .max_by(|a, b| (100.0 + a.confidence).total_cmp(&(100.0 + b.confidence)));

    let (Some(exit), Some(entry)) = (best_exit, best_entry) else {
        return terminal_policy(protected_ratio, vec!["missing-structure".to_string()], base_metrics);
    };

    let entry_t = entry.landing_at.unwrap_or(entry.time);
    let (Some(from_bar), Some(to_bar)) = (nearest_bar(from, exit.time), nearest_bar(to, entry_t)) else {
        return terminal_policy(protected_ratio, vec!["missing-analysis".to_string()], base_metrics);
    };

    let from_snap = from_bar.snap_density;
    let to_snap = to_bar.snap_density;
    let snap_delta = to_snap - from_snap;
    let energy_delta = to_bar.energy - from_bar.energy;
    let metrics = json!({
        "fromSnap": from_snap,
        "toSnap": to_snap,
        "snapDelta": snap_delta,
        "energyDelta": energy_delta,
        "tempoDelta": tempo_delta,
        "tempoKnown": 1,
        "directionalityMismatch": dir_mismatch,
    });
    let mut reasons = if dir_mismatch == 1 {
        vec!["directionality-mismatch".to_string()]
    } else {
        Vec::new()
    };

    if style_bridge_mismatch && tempo_delta >= 0.18 {
        reasons.push("style-bridge-mismatch".to_string());
        return terminal_policy(protected_ratio, reasons, metrics);
    }

    let urgent_rise = to_snap >= 0.42 && snap_delta >= 0.18;
    let urgent_release = from_snap >= 0.42 && snap_delta <= -0.18;
    let secondary_rise = energy_delta >= 0.25 && tempo_delta >= 0.08 && (dir_mismatch == 1 || snap_delta > 0.0);
    let secondary_release = energy_delta <= -0.25 && tempo_delta >= 0.08 && (dir_mismatch == 1 || snap_delta < 0.0);

    if urgent_rise || secondary_rise {
        reasons.push(if urgent_rise { "snap-rise" } else { "energy-tempo-rise" }.to_string());
        return RoutePolicy {
            route: "late-contrast-rise".to_string(),
            compatibility_class: "contrast".to_string(),
            contrast_direction: "rising".to_string(),
            preferred_exit_range: preferred_exit_range(0.75, 0.9, protected_ratio),
            entry_policy: "filtered-runway".to_string(),
            overlap_class: "short".to_string(),
            recipe: "late-contrast-rise".to_string(),
            reasons: reasons.into_iter().take(4).collect(),
            metrics,
        };
    }

    if urgent_release || secondary_release {
        reasons.push(if urgent_release { "snap-release" } else { "energy-tempo-release" }.to_string());
        return RoutePolicy {
            route: "late-contrast-release".to_string(),
            compatibility_class: "contrast".to_string(),
            contrast_direction: "falling".to_string(),
            preferred_exit_range: preferred_exit_range(0.72, 0.9, protected_ratio),
            entry_policy: "quiet-runway".to_string(),
            overlap_class: "short-or-medium".to_string(),
            recipe: "late-contrast-release".to_string(),
            reasons: reasons.into_iter().take(4).collect(),
            metrics,
        };
    }

    if enable_clean_boundary && !overlap_safe {
        reasons.push("overlap-evidence-absent".to_string());
        return RoutePolicy {
            route: "clean-boundary-handoff".to_string(),
            compatibility_class: "conservative".to_string(),
            contrast_direction: "balanced".to_string(),
            preferred_exit_range: preferred_exit_range(0.32, 0.78, protected_ratio),
            entry_policy: "natural-boundary".to_string(),
            overlap_class: "none".to_string(),
            recipe: "clean-boundary-handoff".to_string(),
            reasons: reasons.into_iter().take(4).collect(),
            metrics,
        };
    }

    RoutePolicy {
        route: "structure-mix".to_string(),
        compatibility_class: "compatible".to_string(),
        contrast_direction: "balanced".to_string(),
        preferred_exit_range: preferred_exit_range(0.32, 0.78, protected_ratio),
        entry_policy: "best-supported".to_string(),
        overlap_class: "adaptive".to_string(),
        recipe: "structure-window".to_string(),
        reasons,
        metrics,
    }
}

// ============================================================================
// Boundary Evidence
// ============================================================================

fn source_family(value: &str) -> &'static str {
    match value.trim().to_lowercase().as_str() {
        "lyric" | "lyrics" | "lrc" => "lyric",
        "audio-envelope" | "rms" | "level" => "audio-envelope",
        "spectral-change" | "spectrum" => "spectral-change",
        "beat-grid" | "bar" | "downbeat" => "beat-grid",
        "section" | "structure" => "section",
        "stem" | "vocal-stem" | "accompaniment-stem" => "stem",
        _ => "",
    }
}

fn normalized_vocal_state(value: Option<&Value>) -> String {
    let s = value.and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
    match s.as_str() {
        "ended" | "active" | "unknown" | "inactive" => s,
        _ => "unknown".to_string(),
    }
}

struct CadenceEval {
    eligible: bool,
    confidence: f64,
    reasons: Vec<String>,
}

fn evaluate_cadence_boundary(evidence: &Value, timed_vocal_section: bool) -> CadenceEval {
    let mut reasons = Vec::new();
    let confidence = opt_f64(evidence.get("confidence"));
    let audio_dist = opt_f64(evidence.get("audioBoundaryDistance"));
    let bar_dist = opt_f64(evidence.get("barBoundaryDistance"));
    let before = opt_f64(evidence.get("levelBeforeDb"));
    let after = opt_f64(evidence.get("levelAfterDb"));
    let spectral_change = opt_f64(evidence.get("spectralChange"));
    let silence_after_ms = opt_f64(evidence.get("silenceAfterMs"));
    let silence_after_db = opt_f64(evidence.get("silenceAfterDb"));
    let vocal_state = normalized_vocal_state(evidence.get("vocalState"));

    let mut families_seen = HashSet::new();
    let mut families = Vec::new();
    if let Some(arr) = evidence.get("sources").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                let fam = source_family(s);
                if !fam.is_empty() && families_seen.insert(fam) {
                    families.push(fam);
                }
            }
        }
    }
    let support_count = families.iter().filter(|&&f| f != "lyric").count();
    let has_audio = families
        .iter()
        .any(|&f| matches!(f, "audio-envelope" | "spectral-change" | "stem"));
    let has_bar = families.contains(&"beat-grid");

    if !has_audio || audio_dist.map(|d| d > 0.1).unwrap_or(true) {
        reasons.push("audio-boundary-missing".to_string());
    }
    if !has_bar || bar_dist.map(|d| d > 0.1).unwrap_or(true) {
        reasons.push("bar-boundary-missing".to_string());
    }
    if support_count < 2 {
        reasons.push("evidence-consensus-missing".to_string());
    }
    if confidence.map(|c| c < 0.72).unwrap_or(true) {
        reasons.push("boundary-confidence-low".to_string());
    }
    if vocal_state == "active" {
        reasons.push("vocal-active".to_string());
    }
    if vocal_state == "unknown" && timed_vocal_section {
        let measured = silence_after_ms.map(|ms| ms >= 120.0).unwrap_or(false)
            && silence_after_db.map(|db| db <= -45.0).unwrap_or(false);
        if !measured {
            reasons.push("vocal-state-unknown".to_string());
        }
    }

    let level_drop = match (before, after) {
        (Some(b), Some(a)) => Some(b - a),
        _ => None,
    };
    let strong_section_change =
        spectral_change.map(|s| s >= 0.65).unwrap_or(false) && families.contains(&"section");
    if level_drop.map(|d| d < 3.0 && !strong_section_change).unwrap_or(true) {
        reasons.push("post-boundary-energy-continues".to_string());
    }

    let mut unique_reasons = Vec::new();
    let mut seen = HashSet::new();
    for r in reasons {
        if seen.insert(r.clone()) {
            unique_reasons.push(r);
            if unique_reasons.len() >= 8 {
                break;
            }
        }
    }

    CadenceEval {
        eligible: unique_reasons.is_empty(),
        confidence: confidence.map(|c| round3(c.clamp(0.0, 1.0))).unwrap_or(0.0),
        reasons: unique_reasons,
    }
}

fn choose_end_crossfade_duration(
    from_available: f64,
    to_duration: f64,
    requested_duration: Option<f64>,
    from_vocal_state: &str,
    to_vocal_state: &str,
) -> f64 {
    let from_avail = from_available.max(0.0);
    let to_dur = to_duration.max(0.0);
    let foreground_safe = from_vocal_state == "inactive" && to_vocal_state == "inactive";
    let target = if foreground_safe {
        requested_duration.unwrap_or(2.0).clamp(0.8, 3.0)
    } else {
        requested_duration.unwrap_or(0.8).clamp(0.8, 1.6)
    };
    round3(target.min(from_avail).min(to_dur))
}

// ============================================================================
// Recipe Planner & Gain Envelope
// ============================================================================

#[derive(Clone)]
struct GainSegment {
    start: f64,
    end: f64,
    gain_start: f64,
    gain_end: f64,
    curve: String,
}

fn curve_gain(progress: f64, curve: &str) -> f64 {
    let p = clamp01(progress);
    match curve {
        "equal-power-in" => (p * PI / 2.0).sin(),
        "equal-power-out" => 1.0 - (p * PI / 2.0).cos(),
        _ => p,
    }
}

fn gain_at_envelope(envelope: &[GainSegment], time: f64) -> f64 {
    let seg = envelope.iter().rev().find(|s| s.start <= time);
    let Some(segment) = seg else {
        return 0.0;
    };
    if !(segment.end > segment.start) || time >= segment.end {
        return clamp01(segment.gain_end);
    }
    let progress = (time - segment.start) / (segment.end - segment.start);
    clamp01(segment.gain_start + (segment.gain_end - segment.gain_start) * curve_gain(progress, &segment.curve))
}

fn build_gain_envelope(timeline: &[Value], deck: &str) -> Vec<GainSegment> {
    let base_gain = if deck == "A" { 1.0 } else { 0.0 };
    let mut envelope = vec![GainSegment {
        start: f64::NEG_INFINITY,
        end: f64::INFINITY,
        gain_start: base_gain,
        gain_end: base_gain,
        curve: String::new(),
    }];

    let mut actions: Vec<(f64, usize, &Value)> = timeline
        .iter()
        .enumerate()
        .filter_map(|(idx, action)| {
            let d = action.get("deck").and_then(|v| v.as_str())?;
            let op = action.get("op").and_then(|v| v.as_str())?;
            if d == deck && (op == "play" || op == "volume") {
                let t = opt_f64(action.get("t"))?;
                Some((t, idx, action))
            } else {
                None
            }
        })
        .collect();
    actions.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    for (start, _, action) in actions {
        let start_gain = gain_at_envelope(&envelope, start);
        envelope.retain(|seg| seg.start < start);
        for seg in &mut envelope {
            if seg.end > start {
                seg.end = start;
            }
        }
        let op = action.get("op").and_then(|v| v.as_str()).unwrap_or("");
        if op == "play" {
            let gain = clamp01(val_f64(action.get("volume"), 0.0));
            envelope.push(GainSegment {
                start,
                end: f64::INFINITY,
                gain_start: gain,
                gain_end: gain,
                curve: String::new(),
            });
            continue;
        }
        let duration = (val_f64(action.get("duration"), 0.0) / 1000.0).max(0.0);
        let target = clamp01(val_f64(action.get("value"), 0.0));
        let curve = action
            .get("curve")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if duration > 0.0 {
            let end = start + duration;
            envelope.push(GainSegment {
                start,
                end,
                gain_start: start_gain,
                gain_end: target,
                curve,
            });
            envelope.push(GainSegment {
                start: end,
                end: f64::INFINITY,
                gain_start: target,
                gain_end: target,
                curve: String::new(),
            });
        } else {
            envelope.push(GainSegment {
                start,
                end: f64::INFINITY,
                gain_start: target,
                gain_end: target,
                curve: String::new(),
            });
        }
    }
    envelope.sort_by(|a, b| a.start.total_cmp(&b.start));
    envelope
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct WindowMeasurement {
    pre_roll_duration: f64,
    audible_overlap: f64,
    audible_start: f64,
    audible_end: f64,
    handoff_offset: f64,
    transition_start: Option<f64>,
    runway_available: bool,
    landing_error: f64,
    a_only_tail_duration: Option<f64>,
    breath_duration: Option<f64>,
    actual_two_deck_overlap: Option<f64>,
    overlap_contract: OverlapContract,
}

fn measure_timeline_window(timeline: &[Value]) -> (f64, f64, f64, f64, f64) {
    let play = timeline.iter().find(|a| {
        a.get("deck").and_then(|v| v.as_str()) == Some("B")
            && a.get("op").and_then(|v| v.as_str()) == Some("play")
    });
    let handoff = timeline
        .iter()
        .filter(|a| a.get("op").and_then(|v| v.as_str()) == Some("handoff"))
        .next_back();
    let play_at = play.and_then(|a| opt_f64(a.get("t"))).unwrap_or(0.0);
    let handoff_at = handoff.and_then(|a| opt_f64(a.get("t"))).unwrap_or(play_at);

    let mut min_t = play_at.min(handoff_at);
    let mut max_t = play_at.max(handoff_at);
    for a in timeline {
        if let Some(s) = opt_f64(a.get("t")) {
            min_t = min_t.min(s);
            max_t = max_t.max(s);
            let dur = (val_f64(a.get("duration"), 0.0) / 1000.0).max(0.0);
            if dur > 0.0 {
                max_t = max_t.max(s + dur);
            }
        }
    }

    let env_a = build_gain_envelope(timeline, "A");
    let env_b = build_gain_envelope(timeline, "B");
    let sample_step = 0.005;
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    let mut active_start: Option<f64> = None;
    let mut time = min_t;
    while time < max_t {
        let start = round3(time);
        let end = max_t.min(start + sample_step);
        let midpoint = start + (end - start) / 2.0;
        let audible = gain_at_envelope(&env_a, midpoint) >= 0.08
            && gain_at_envelope(&env_b, midpoint) >= 0.08;
        if audible && active_start.is_none() {
            active_start = Some(start);
        }
        if !audible {
            if let Some(s) = active_start {
                intervals.push((s, start));
                active_start = None;
            }
        }
        time += sample_step;
    }
    if let Some(s) = active_start {
        intervals.push((s, max_t));
    }
    intervals.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)).then_with(|| a.0.total_cmp(&b.0)));

    let (start, end) = intervals.first().copied().unwrap_or((handoff_at, handoff_at));
    (
        round3((start - play_at).max(0.0)),
        round3((end - start).max(0.0)),
        round3(start),
        round3(end),
        handoff.and_then(|a| opt_f64(a.get("t"))).unwrap_or(0.0),
    )
}

fn landing_diagnostics(timeline: &[Value], b_anchor: f64, b_impact_offset: Option<f64>) -> (bool, f64) {
    let handoff = timeline
        .iter()
        .filter(|a| a.get("op").and_then(|v| v.as_str()) == Some("handoff"))
        .next_back();
    let handoff_t = handoff.and_then(|a| opt_f64(a.get("t")));
    let play = timeline
        .iter()
        .filter(|a| {
            a.get("deck").and_then(|v| v.as_str()) == Some("B")
                && a.get("op").and_then(|v| v.as_str()) == Some("play")
                && handoff_t
                    .map(|ht| opt_f64(a.get("t")).map(|pt| pt <= ht).unwrap_or(false))
                    .unwrap_or(true)
        })
        .next_back();

    let Some(play_action) = play else {
        return (false, INVALID_LANDING_ERROR);
    };
    let play_t = opt_f64(play_action.get("t"));
    let play_at = opt_f64(play_action.get("at"));
    let landing_offset = b_impact_offset.or(handoff_t);

    let rate = timeline
        .iter()
        .filter(|a| {
            a.get("deck").and_then(|v| v.as_str()) == Some("B")
                && a.get("op").and_then(|v| v.as_str()) == Some("rate")
                && opt_f64(a.get("t")).unwrap_or(f64::INFINITY)
                    <= play_t.unwrap_or(f64::NEG_INFINITY)
        })
        .filter_map(|a| opt_f64(a.get("value")))
        .next_back()
        .unwrap_or(1.0);

    let actual_landing = match (landing_offset, play_at, play_t) {
        (Some(lo), Some(pa), Some(pt)) => Some(pa + (lo - pt) * rate),
        _ => None,
    };
    let (Some(actual), Some(pa)) = (actual_landing, play_at) else {
        return (false, INVALID_LANDING_ERROR);
    };
    if !b_anchor.is_finite() || !actual.is_finite() {
        return (false, INVALID_LANDING_ERROR);
    }
    let landing_error = round3(actual - b_anchor);
    (pa >= 0.0 && landing_error.abs() <= LANDING_TOLERANCE_SEC, landing_error)
}

fn natural_entry_diagnostics(timeline: &[Value], natural_entry: f64, audible_at: f64) -> (bool, f64) {
    let play = timeline
        .iter()
        .filter(|a| {
            a.get("deck").and_then(|v| v.as_str()) == Some("B")
                && a.get("op").and_then(|v| v.as_str()) == Some("play")
                && opt_f64(a.get("t")).unwrap_or(f64::INFINITY) <= audible_at
        })
        .next_back();
    let Some(play_action) = play else {
        return (false, INVALID_LANDING_ERROR);
    };
    let play_t = opt_f64(play_action.get("t"));
    let play_at = opt_f64(play_action.get("at"));
    let rate = timeline
        .iter()
        .filter(|a| {
            a.get("deck").and_then(|v| v.as_str()) == Some("B")
                && a.get("op").and_then(|v| v.as_str()) == Some("rate")
                && opt_f64(a.get("t")).unwrap_or(f64::INFINITY)
                    <= play_t.unwrap_or(f64::NEG_INFINITY)
        })
        .filter_map(|a| opt_f64(a.get("value")))
        .next_back()
        .unwrap_or(1.0);

    let actual = match (play_at, play_t) {
        (Some(pa), Some(pt)) if audible_at.is_finite() => Some(pa + (audible_at - pt) * rate),
        _ => None,
    };
    let (Some(act), Some(pa)) = (actual, play_at) else {
        return (false, INVALID_LANDING_ERROR);
    };
    if !natural_entry.is_finite() {
        return (false, INVALID_LANDING_ERROR);
    }
    let landing_error = round3(act - natural_entry);
    (pa >= 0.0 && landing_error.abs() <= LANDING_TOLERANCE_SEC, landing_error)
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RecipeAnchors {
    a_exit: f64,
    b_start: f64,
    b_anchor: f64,
    bar_length: f64,
    a_downbeat_offset: f64,
    b_downbeat_offset: f64,
    downbeat_offset: f64,
}

#[derive(Debug, Clone)]
struct CommonScores {
    a_energy: f64,
    b_energy: f64,
    a_bass: f64,
    b_bass: f64,
    beat_score: f64,
    energy_score: f64,
    bass_score: f64,
    bpm_score: f64,
    outro_completeness: f64,
    b_intro_aggression: f64,
    style_texture_distance: f64,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct TempoAssessment {
    bpm_a: f64,
    bpm_b: f64,
    normalized_bpm_b: f64,
    tempo_scale: f64,
    tempo_family_delta: f64,
    passive_tempo_delta: f64,
    relative_tempo_delta: f64,
    target_playback_rate: f64,
    tempo_lock_available: bool,
}

#[derive(Debug, Clone)]
struct SafetyAssessment {
    entry_source: String,
    entry_confidence: f64,
    entry_trusted: bool,
    exit_type: String,
    exit_source: String,
    exit_confidence: f64,
    exit_trusted: bool,
    source_runway: f64,
    tempo: TempoAssessment,
    beat_grid_trusted: bool,
    overlap_class: String,
    overlap_duration: f64,
    musical_evidence: bool,
    musical_compatibility: f64,
    harmonic_similarity: f64,
    key_compatibility: f64,
    melody_similarity: f64,
    musical_risks: Vec<String>,
}

fn tempo_family_assessment(first_bpm: f64, second_bpm: f64) -> TempoAssessment {
    let bpm_a = first_bpm.max(0.0);
    let bpm_b = second_bpm.max(0.0);
    if !(bpm_a > 0.0) || !(bpm_b > 0.0) {
        return TempoAssessment {
            bpm_a,
            bpm_b,
            normalized_bpm_b: bpm_b,
            tempo_scale: 1.0,
            tempo_family_delta: 1.0,
            passive_tempo_delta: 1.0,
            relative_tempo_delta: 1.0,
            target_playback_rate: 1.0,
            tempo_lock_available: false,
        };
    }
    let mut matches: Vec<(f64, f64, f64)> = [1.0, 0.5, 2.0]
        .iter()
        .map(|&scale| {
            let norm = bpm_b * scale;
            let delta = (bpm_a - norm).abs() / bpm_a.max(norm);
            (scale, norm, delta)
        })
        .collect();
    matches.sort_by(|a, b| {
        a.2.total_cmp(&b.2)
            .then_with(|| (1.0 - a.0).abs().total_cmp(&(1.0 - b.0).abs()))
    });
    let (scale, normalized_bpm_b, family_delta) = matches[0];
    let requested_rate = bpm_a / normalized_bpm_b.max(1.0);
    let tempo_lock_available = (0.94..=1.06).contains(&requested_rate);
    let target_playback_rate = if tempo_lock_available {
        round3(requested_rate)
    } else {
        1.0
    };
    let direct_delta = (bpm_a - bpm_b).abs() / bpm_a.max(bpm_b);
    let passive_delta = if (scale - 1.0).abs() < 1e-9 || (target_playback_rate - 1.0).abs() <= 0.001 {
        family_delta
    } else {
        direct_delta
    };
    TempoAssessment {
        bpm_a,
        bpm_b,
        normalized_bpm_b: round3(normalized_bpm_b),
        tempo_scale: scale,
        tempo_family_delta: round3(family_delta),
        passive_tempo_delta: round3(passive_delta),
        relative_tempo_delta: round3(passive_delta),
        target_playback_rate,
        tempo_lock_available,
    }
}

fn density_at(windows: &[ValueWindow], time: f64) -> f64 {
    if let Some(found) = windows.iter().find(|w| time >= w.start && time < w.end) {
        return clamp01(found.value);
    }
    windows
        .iter()
        .min_by(|a, b| (a.start - time).abs().total_cmp(&(b.start - time).abs()))
        .map(|w| clamp01(w.value))
        .unwrap_or(0.0)
}

fn texture_at(analysis: &NormalizedAnalysis, start: f64, end: f64) -> (f64, f64, f64, f64) {
    let bars: Vec<&crate::cuefield::structure::Bar> = analysis
        .bars
        .iter()
        .filter(|b| b.end > start && b.start < end)
        .collect();
    if bars.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let n = bars.len() as f64;
    (
        clamp01(bars.iter().map(|b| b.energy).sum::<f64>() / n),
        clamp01(bars.iter().map(|b| b.low_density).sum::<f64>() / n),
        clamp01(bars.iter().map(|b| b.body_density).sum::<f64>() / n),
        clamp01(bars.iter().map(|b| b.snap_density).sum::<f64>() / n),
    )
}

fn nearest_downbeat_offset(analysis: &NormalizedAnalysis, time: f64) -> f64 {
    analysis
        .downbeats
        .iter()
        .map(|d| (d.time - time).abs())
        .min_by(|a, b| a.total_cmp(b))
        .map(round3)
        .unwrap_or(f64::NAN)
}

fn pick_anchors(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    exit: Option<&TransitionCandidate>,
    entry: Option<&TransitionCandidate>,
) -> RecipeAnchors {
    let hook = to
        .candidates
        .iter()
        .filter(|c| c.role == "entry" && (c.kind == "hook" || c.kind == "chorus"))
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence).then_with(|| b.time.total_cmp(&a.time)));
    let intro = to
        .candidates
        .iter()
        .filter(|c| c.role == "entry" && c.kind == "intro")
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence).then_with(|| b.time.total_cmp(&a.time)));

    let a_exit = round3(exit.map(|e| e.time).unwrap_or(from.cue_points.outro_start));
    let b_intro = round3(intro.map(|i| i.time).unwrap_or(to.cue_points.intro_start));
    let b_anchor = round3(
        entry
            .and_then(|e| e.resolves_to.as_ref().map(|r| r.time).or(Some(e.time)))
            .or_else(|| hook.map(|h| h.time))
            .unwrap_or(to.cue_points.first_strong_downbeat.max(0.0).min(b_intro.max(to.cue_points.first_strong_downbeat))),
    );
    let b_start = round3(0.0_f64.max(b_intro.min(b_anchor)));
    let bar_len = from
        .bars
        .iter()
        .find(|b| b.end > b.start)
        .map(|b| round3(b.end - b.start))
        .unwrap_or_else(|| (from.grid_step * 4.0).max(1.0));
    let a_db = nearest_downbeat_offset(from, a_exit);
    let b_db = nearest_downbeat_offset(to, b_anchor);
    let downbeat_offset = if a_db.is_finite() && b_db.is_finite() {
        a_db.max(b_db)
    } else {
        f64::NAN
    };
    RecipeAnchors {
        a_exit,
        b_start,
        b_anchor,
        bar_length: bar_len,
        a_downbeat_offset: a_db,
        b_downbeat_offset: b_db,
        downbeat_offset,
    }
}

fn compute_common_scores(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    anchors: &RecipeAnchors,
) -> CommonScores {
    let a_energy = density_at(&from.energy_windows, anchors.a_exit);
    let b_energy = density_at(&to.energy_windows, anchors.b_anchor);
    let a_bass = density_at(&from.bass_windows, anchors.a_exit);
    let b_bass = density_at(&to.bass_windows, anchors.b_start);
    let tempo = tempo_family_assessment(from.bpm, to.bpm);
    let bpm_diff = if (tempo.passive_tempo_delta - tempo.tempo_family_delta).abs() < 1e-9 {
        (tempo.bpm_a - tempo.normalized_bpm_b).abs()
    } else {
        (tempo.bpm_a - tempo.bpm_b).abs()
    };
    let beat_score = if anchors.downbeat_offset.is_finite() {
        1.0 - (anchors.downbeat_offset / (anchors.bar_length / 2.0).max(0.001)).min(1.0)
    } else {
        0.5
    };
    let energy_score = 1.0 - ((a_energy - b_energy).abs() / 0.65).min(1.0);
    let bass_score = 1.0 - ((a_bass + b_bass - 0.88).max(0.0) / 0.7).min(1.0);
    let bpm_score = 1.0 - (bpm_diff / 18.0).min(1.0);
    let outro_tex = texture_at(from, anchors.a_exit, from.duration.max(anchors.a_exit) + 0.001);
    let intro_tex = texture_at(to, anchors.b_start, anchors.b_start + 12.0);
    let exit_cand = from
        .candidates
        .iter()
        .filter(|c| c.role == "exit")
        .min_by(|a, b| (a.time - anchors.a_exit).abs().total_cmp(&(b.time - anchors.a_exit).abs()));
    let outro_len_score = clamp01((from.duration - anchors.a_exit) / 16.0);
    let outro_conf = clamp01(exit_cand.map(|c| c.confidence).unwrap_or(0.0));
    let outro_completeness = clamp01(outro_len_score * 0.45 + outro_conf * 0.35 + (1.0 - outro_tex.0) * 0.2);
    let b_intro_aggression = clamp01(intro_tex.0 * 0.55 + intro_tex.1 * 0.25 + intro_tex.3 * 0.2);
    let style_tex_dist = clamp01(
        ((outro_tex.0 - intro_tex.0).abs()
            + (outro_tex.1 - intro_tex.1).abs()
            + (outro_tex.2 - intro_tex.2).abs()
            + (outro_tex.3 - intro_tex.3).abs())
            / 4.0
            / 0.7,
    );

    CommonScores {
        a_energy,
        b_energy,
        a_bass,
        b_bass,
        beat_score,
        energy_score,
        bass_score,
        bpm_score,
        outro_completeness,
        b_intro_aggression,
        style_texture_distance: style_tex_dist,
    }
}

fn build_safety_assessment(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    exit: Option<&TransitionCandidate>,
    entry: Option<&TransitionCandidate>,
    route: &str,
) -> SafetyAssessment {
    let exit_cand = exit.and_then(|ex| {
        from.candidates
            .iter()
            .filter(|c| c.role == "exit")
            .min_by(|a, b| (a.time - ex.time).abs().total_cmp(&(b.time - ex.time).abs()))
    });
    let entry_source = entry.map(|e| e.source.as_str()).unwrap_or("fallback").to_string();
    let entry_confidence = clamp01(entry.map(|e| e.confidence).unwrap_or(0.0));
    let entry_trusted = entry_source != "fallback" && entry_confidence >= 0.6;
    let exit_type = exit
        .map(|e| e.kind.clone())
        .or_else(|| exit_cand.map(|e| e.kind.clone()))
        .unwrap_or_default();
    let exit_source = exit
        .map(|e| e.source.clone())
        .or_else(|| exit_cand.map(|e| e.source.clone()))
        .unwrap_or_default();
    let exit_confidence = clamp01(
        exit.map(|e| e.confidence)
            .or_else(|| exit_cand.map(|e| e.confidence))
            .unwrap_or(0.0),
    );
    let exit_trusted = exit_confidence >= 0.72;
    let exit_t = exit
        .map(|e| e.time)
        .or_else(|| exit_cand.map(|e| e.time))
        .unwrap_or(0.0);
    let source_runway = (from.duration - exit_t).max(0.0);
    let tempo = tempo_family_assessment(from.bpm, to.bpm);
    let beat_grid_trusted = from.grid_step > 0.0
        && to.grid_step > 0.0
        && from.grid_quality.downbeat_count >= 4
        && to.grid_quality.downbeat_count >= 4
        && from.grid_quality.downbeat_confidence >= 0.65
        && to.grid_quality.downbeat_confidence >= 0.65
        && from.grid_quality.beat_stability >= 0.35
        && to.grid_quality.beat_stability >= 0.35
        && from.grid_quality.timing_stability >= 0.85
        && to.grid_quality.timing_stability >= 0.85;

    let mut overlap_class = if entry_trusted && beat_grid_trusted && tempo.relative_tempo_delta <= 0.08 {
        "long"
    } else if entry_trusted && tempo.relative_tempo_delta <= 0.15 {
        "medium"
    } else {
        "short"
    };
    if route == "late-contrast-rise" || route == "terminal-rescue" {
        overlap_class = "short";
    } else if route == "late-contrast-release" && overlap_class == "long" {
        overlap_class = "medium";
    }
    let overlap_duration = match overlap_class {
        "long" => 10.5,
        "medium" => 5.6,
        _ => 3.4,
    };

    fn reliable_musical_profile(mp: Option<&Value>) -> Option<&Value> {
        let m = mp?;
        if val_f64(m.get("confidence"), 0.0) >= 0.55 && val_f64(m.get("noteCount"), 0.0) >= 12.0 {
            Some(m)
        } else {
            None
        }
    }
    let (musical_evidence, musical_compatibility, harmonic_similarity, key_compatibility, melody_similarity, musical_risks) =
        match (reliable_musical_profile(from.musical_profile.as_ref()), reliable_musical_profile(to.musical_profile.as_ref())) {
            (Some(f), Some(t)) => {
                let cmp = compare_musical_profiles(f, t);
                (
                    true,
                    cmp.score,
                    cmp.harmonic_similarity,
                    cmp.key_compatibility,
                    cmp.melody_similarity,
                    cmp.risks,
                )
            }
            _ => (false, 0.0, 0.0, 0.0, 0.0, Vec::new()),
        };

    SafetyAssessment {
        entry_source,
        entry_confidence: round3(entry_confidence),
        entry_trusted,
        exit_type,
        exit_source,
        exit_confidence: round3(exit_confidence),
        exit_trusted,
        source_runway: round3(source_runway),
        tempo,
        beat_grid_trusted,
        overlap_class: overlap_class.to_string(),
        overlap_duration,
        musical_evidence,
        musical_compatibility,
        harmonic_similarity,
        key_compatibility,
        melody_similarity,
        musical_risks,
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RecipeCandidate {
    recipe: String,
    score: f64,
    confidence: f64,
    timeline: Vec<Value>,
    fallback_timeline: Option<Vec<Value>>,
    fallback_recipe: Option<String>,
    variant: Option<String>,
    a_exit: f64,
    b_start: f64,
    b_anchor: f64,
    window: WindowMeasurement,
    eligible: bool,
    eligibility_reason: String,
    selection_score: f64,
}

fn aligned_b_start(b_anchor: f64, play_at: f64, handoff_at: f64) -> f64 {
    (round3(b_anchor - (handoff_at - play_at))).max(0.0)
}

fn build_safety_timeline(b_landing_at: f64, overlap_class: &str, overlap_duration: f64) -> (f64, bool, f64, Vec<Value>) {
    let duration = if overlap_duration > 0.0 {
        overlap_duration
    } else if overlap_class == "long" {
        10.5
    } else if overlap_class == "medium" {
        5.6
    } else {
        3.4
    };
    let handoff_at = if overlap_class == "long" { 1.0 } else { 0.6 };
    let play_at = round3(handoff_at - duration);
    let b_start = round3((b_landing_at - duration).max(0.0));
    let actual_landing = round3(b_start + (handoff_at - play_at));
    let landing_error = round3(actual_landing - b_landing_at);
    let runway_available = landing_error.abs() <= LANDING_TOLERANCE_SEC;
    let dur_ms = (duration * 1000.0).round() as i64;
    let timeline = if overlap_class == "short" {
        vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "bass", "value": 0.15, "duration": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "volume", "value": 1, "duration": dur_ms, "curve": "equal-power-in" }),
            json!({ "t": -1.6, "deck": "A", "op": "bass", "value": 0.5, "duration": 700 }),
            json!({ "t": play_at, "deck": "A", "op": "volume", "value": 0, "duration": dur_ms, "curve": "equal-power-out" }),
            json!({ "t": -0.8, "deck": "B", "op": "bass", "value": 0.85, "duration": 700 }),
            json!({ "t": 0, "deck": "B", "op": "bass", "value": 1, "duration": 300 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ]
    } else if overlap_class == "medium" {
        let fade_ms = ((duration - 0.2) * 1000.0).round() as i64;
        vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "bass", "value": 0.12, "duration": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "filter", "type": "highpass", "value": 850, "duration": 0 }),
            json!({ "t": round3(play_at + 0.2), "deck": "B", "op": "volume", "value": 1, "duration": fade_ms, "curve": "equal-power-in" }),
            json!({ "t": -1.8, "deck": "A", "op": "bass", "value": 0.3, "duration": 1000 }),
            json!({ "t": -1.2, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 1000 }),
            json!({ "t": round3(play_at + 0.2), "deck": "A", "op": "volume", "value": 0, "duration": fade_ms, "curve": "equal-power-out" }),
            json!({ "t": -0.8, "deck": "B", "op": "bass", "value": 1, "duration": 900 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ]
    } else {
        let fade_ms = ((duration - 1.0) * 1000.0).round() as i64;
        vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "bass", "value": 0.08, "duration": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "filter", "type": "highpass", "value": 1100, "duration": 0 }),
            json!({ "t": round3(play_at + 1.0), "deck": "B", "op": "volume", "value": 1, "duration": fade_ms, "curve": "equal-power-in" }),
            json!({ "t": -2.0, "deck": "A", "op": "filter", "type": "highpass", "value": 160, "duration": 1600 }),
            json!({ "t": -1.8, "deck": "A", "op": "bass", "value": 0.24, "duration": 1200 }),
            json!({ "t": -1.2, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 1100 }),
            json!({ "t": round3(play_at + 1.0), "deck": "A", "op": "volume", "value": 0, "duration": fade_ms, "curve": "equal-power-out" }),
            json!({ "t": -0.8, "deck": "B", "op": "bass", "value": 1, "duration": 900 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ]
    };
    (b_start, runway_available, landing_error, timeline)
}

fn make_base_candidate(
    recipe: &str,
    score: f64,
    confidence: f64,
    anchors: &RecipeAnchors,
    b_start: f64,
    timeline: Vec<Value>,
    b_impact_offset: Option<f64>,
) -> RecipeCandidate {
    let (pre_roll, audible_overlap, audible_start, audible_end, handoff_offset) =
        measure_timeline_window(&timeline);
    let (runway_available, landing_error) =
        landing_diagnostics(&timeline, anchors.b_anchor, b_impact_offset);
    RecipeCandidate {
        recipe: recipe.to_string(),
        score: round3(score),
        confidence: round3(confidence),
        timeline,
        fallback_timeline: None,
        fallback_recipe: None,
        variant: None,
        a_exit: anchors.a_exit,
        b_start,
        b_anchor: anchors.b_anchor,
        window: WindowMeasurement {
            pre_roll_duration: pre_roll,
            audible_overlap,
            audible_start,
            audible_end,
            handoff_offset,
            transition_start: None,
            runway_available,
            landing_error,
            a_only_tail_duration: None,
            breath_duration: None,
            actual_two_deck_overlap: None,
            overlap_contract: overlap_contract_for(recipe),
        },
        eligible: false,
        eligibility_reason: String::new(),
        selection_score: 0.0,
    }
}

fn plan_recipe_candidates(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    exit: &TransitionCandidate,
    entry: &TransitionCandidate,
    section_eval: &PairEvaluation,
    route: &str,
    recent_recipes: &[String],
) -> (Vec<RecipeCandidate>, CommonScores, SafetyAssessment, RecipeAnchors) {
    let anchors = pick_anchors(from, to, Some(exit), Some(entry));
    let scores = compute_common_scores(from, to, &anchors);
    let assessment = build_safety_assessment(from, to, Some(exit), Some(entry), route);

    let severe_overlap_risk = section_eval
        .risks
        .iter()
        .any(|r| matches!(r.as_str(), "style bridge mismatch" | "vocal collision" | "vocal overlap"));

    let mut candidates = Vec::new();

    // 1. safety-long-blend
    {
        let (b_start, runway_available, landing_error, timeline) = build_safety_timeline(
            anchors.b_anchor,
            &assessment.overlap_class,
            assessment.overlap_duration,
        );
        let score = 0.48
            + scores.bpm_score * 0.08
            + scores.beat_score * 0.05
            + scores.outro_completeness * 0.08
            - scores.b_intro_aggression * 0.04
            - scores.style_texture_distance * 0.03;
        let mut c = make_base_candidate("safety-long-blend", score, 0.72, &anchors, b_start, timeline, None);
        c.window.runway_available = runway_available;
        c.window.landing_error = landing_error;
        candidates.push(c);
    }

    // 2. clean-boundary-handoff
    if route == "clean-boundary-handoff" {
        let exit_kind = exit.kind.to_lowercase();
        let (variant, tail_sec, breath_sec) = if exit_kind == "natural-tail" {
            ("a-tail-release", 0.6, 0.0)
        } else if exit_kind == "release" || exit_kind == "outro" {
            ("a-tail-release", 0.45, 0.0)
        } else if exit_kind == "post-hook-boundary" {
            ("short-breath", 0.08, 0.18)
        } else {
            ("dry-downbeat", 0.08, 0.0)
        };
        let audible_at = round3(tail_sec + breath_sec);
        let natural_entry = entry.play_from.unwrap_or(entry.time).max(0.0);
        let pre_roll = 0.25_f64.min(audible_at).min(natural_entry);
        let play_at = round3(audible_at - pre_roll);
        let source_at = round3(natural_entry - pre_roll);
        let handoff_at = round3(audible_at + 0.12);
        let timeline = vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": source_at, "volume": 0 }),
            json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": (tail_sec * 1000.0).round() as i64, "curve": "equal-power-out" }),
            json!({ "t": audible_at, "deck": "B", "op": "volume", "value": 1, "duration": 120, "curve": "equal-power-in" }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut clean_anchors = anchors.clone();
        clean_anchors.b_start = source_at;
        clean_anchors.b_anchor = natural_entry;
        let (runway_available, landing_error) = natural_entry_diagnostics(&timeline, natural_entry, audible_at);
        let mut c = make_base_candidate(
            "clean-boundary-handoff",
            0.68 + scores.beat_score * 0.12 + scores.energy_score * 0.08,
            0.88,
            &clean_anchors,
            source_at,
            timeline,
            None,
        );
        c.variant = Some(variant.to_string());
        c.window.transition_start = Some(0.0);
        c.window.runway_available = runway_available;
        c.window.landing_error = landing_error;
        c.window.a_only_tail_duration = Some(tail_sec);
        c.window.breath_duration = Some(breath_sec);
        c.window.actual_two_deck_overlap = Some(0.0);
        c.window.pre_roll_duration = round3(pre_roll);
        candidates.push(c);
    }

    // 3. intro-outro-long-blend
    {
        let lead = 8.0;
        let handoff_at = 2.6;
        let b_start = aligned_b_start(anchors.b_anchor, -lead, handoff_at);
        let score = 0.28 + scores.beat_score * 0.28 + scores.energy_score * 0.18 + scores.bass_score * 0.18 + scores.bpm_score * 0.08;
        let timeline = vec![
            json!({ "t": -lead, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": -lead, "deck": "B", "op": "volume", "value": 0.58, "duration": 5200 }),
            json!({ "t": -4.5, "deck": "B", "op": "filter", "type": "highpass", "value": 650, "duration": 2600 }),
            json!({ "t": -2.4, "deck": "A", "op": "volume", "value": 0, "duration": 4800, "curve": "equal-power-out" }),
            json!({ "t": -2, "deck": "A", "op": "bass", "value": 0.72, "duration": 3000 }),
            json!({ "t": -1.2, "deck": "B", "op": "bass", "value": 0.82, "duration": 1400 }),
            json!({ "t": 0, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 900 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        candidates.push(make_base_candidate("intro-outro-long-blend", score, (score + 0.04).min(0.9), &anchors, b_start, timeline, None));
    }

    // 4. filtered-pickup
    {
        let short_route = route == "late-contrast-rise";
        let lead = if short_route { 2.8 } else { 4.0 };
        let handoff_at = if short_route { 0.6 } else { 2.2 };
        let b_start = aligned_b_start(anchors.b_anchor, -lead, handoff_at);
        let score = 0.34 + scores.beat_score * 0.3 + scores.energy_score * 0.18 + scores.bass_score * 0.12 + scores.bpm_score * 0.06;
        let timeline = if short_route {
            vec![
                json!({ "t": -lead, "deck": "B", "op": "play", "at": round3(b_start), "volume": 0 }),
                json!({ "t": -lead, "deck": "B", "op": "filter", "type": "highpass", "value": 900, "duration": 1300 }),
                json!({ "t": -2.6, "deck": "B", "op": "volume", "value": 1, "duration": 2500, "curve": "equal-power-in" }),
                json!({ "t": -1.7, "deck": "A", "op": "bass", "value": 0.35, "duration": 900 }),
                json!({ "t": -1.5, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.32, "pulses": 4, "beats": 1, "attack": 18, "hold": 65, "release": 130 }),
                json!({ "t": -0.7, "deck": "A", "op": "volume", "value": 0, "duration": 1000, "curve": "equal-power-out" }),
                json!({ "t": -0.6, "deck": "B", "op": "bass", "value": 0.8, "duration": 700 }),
                json!({ "t": -0.5, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 800 }),
                json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
            ]
        } else {
            vec![
                json!({ "t": -lead, "deck": "B", "op": "play", "at": round3(b_start), "volume": 0 }),
                json!({ "t": -lead, "deck": "B", "op": "filter", "type": "highpass", "value": 900, "duration": 1800 }),
                json!({ "t": -3.6, "deck": "B", "op": "volume", "value": 0.72, "duration": 2600 }),
                json!({ "t": -2.2, "deck": "A", "op": "bass", "value": 0.35, "duration": 1300 }),
                json!({ "t": -2, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.26, "pulses": 4, "beats": 1, "attack": 18, "hold": 65, "release": 140 }),
                json!({ "t": -0.4, "deck": "B", "op": "bass", "value": 0.8, "duration": 900 }),
                json!({ "t": 0, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 900 }),
                json!({ "t": 1.1, "deck": "A", "op": "volume", "value": 0, "duration": 1000 }),
                json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
            ]
        };
        candidates.push(make_base_candidate("filtered-pickup", score, (score + 0.05).min(0.92), &anchors, round3(b_start), timeline, None));
    }

    // 5. bass-eq-handoff
    let bass_handoff_timeline = {
        let lead = 5.2;
        let handoff_at = 1.8;
        let b_start = aligned_b_start(anchors.b_anchor, -lead, handoff_at);
        let score = 0.3 + scores.beat_score * 0.28 + scores.bass_score * 0.24 + scores.energy_score * 0.12 + scores.bpm_score * 0.06;
        let timeline = vec![
            json!({ "t": -lead, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": -lead, "deck": "B", "op": "bass", "value": 0.15, "duration": 0 }),
            json!({ "t": -4.8, "deck": "B", "op": "volume", "value": 0.68, "duration": 2600 }),
            json!({ "t": -2.6, "deck": "A", "op": "bass", "value": 0.18, "duration": 1800 }),
            json!({ "t": -2.2, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.3, "pulses": 4, "beats": 1, "attack": 16, "hold": 70, "release": 145 }),
            json!({ "t": -1, "deck": "B", "op": "bass", "value": 0.92, "duration": 1100 }),
            json!({ "t": 0.3, "deck": "A", "op": "volume", "value": 0, "duration": 1200 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let tl_clone = timeline.clone();
        candidates.push(make_base_candidate("bass-eq-handoff", score, (score + 0.03).min(0.88), &anchors, b_start, timeline, None));
        tl_clone
    };

    let (_, _, _, short_safety_tl) = build_safety_timeline(anchors.b_anchor, "short", 3.4);

    // 6. spectral-emergence
    {
        let play_at = -7.2;
        let handoff_at = 1.0;
        let rate = assessment.tempo.target_playback_rate;
        let b_start = (round3(anchors.b_anchor - (handoff_at - play_at) * rate)).max(0.0);
        let score = 0.44
            + scores.beat_score * 0.12
            + scores.energy_score * 0.08
            + scores.bass_score * 0.08
            + scores.outro_completeness * 0.08
            + assessment.musical_compatibility * 0.14
            - scores.style_texture_distance * 0.05;
        let timeline = vec![
            json!({ "t": play_at, "deck": "B", "op": "rate", "value": rate, "duration": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "spectrum", "low": 0.82, "mid": 0.18, "high": 0.12, "duration": 0 }),
            json!({ "t": round3(play_at + 0.2), "deck": "B", "op": "volume", "value": 0.58, "duration": 2400, "curve": "equal-power-in" }),
            json!({ "t": -4.8, "deck": "B", "op": "spectrum", "low": 0.86, "mid": 0.58, "high": 0.42, "duration": 1900 }),
            json!({ "t": -3.2, "deck": "B", "op": "volume", "value": 0.82, "duration": 1800, "curve": "equal-power-in" }),
            json!({ "t": -2.2, "deck": "A", "op": "bass", "value": 0.18, "duration": 1400 }),
            json!({ "t": -2, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.26, "pulses": 4, "beats": 1, "attack": 18, "hold": 65, "release": 140 }),
            json!({ "t": -0.8, "deck": "B", "op": "spectrum", "low": 1, "mid": 1, "high": 1, "duration": 900 }),
            json!({ "t": -1.8, "deck": "A", "op": "volume", "value": 0, "duration": 2800, "curve": "equal-power-out" }),
            json!({ "t": handoff_at, "deck": "B", "op": "rate", "value": 1, "duration": 2400 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("spectral-emergence", score, (score + 0.06).min(0.94), &anchors, b_start, timeline, None);
        c.fallback_timeline = Some(short_safety_tl.clone());
        candidates.push(c);
    }

    // 7. quick-safe-fade
    {
        let lead = 2.6;
        let handoff_at = 1.0;
        let b_start = aligned_b_start(anchors.b_anchor, -lead, handoff_at);
        let score = 0.24 + scores.beat_score * 0.32 + scores.energy_score * 0.16 + scores.bpm_score * 0.1;
        let timeline = vec![
            json!({ "t": -lead, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": -lead, "deck": "B", "op": "volume", "value": 0.76, "duration": 1800 }),
            json!({ "t": -0.6, "deck": "A", "op": "volume", "value": 0.2, "duration": 800 }),
            json!({ "t": 0.3, "deck": "A", "op": "volume", "value": 0, "duration": 600 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        candidates.push(make_base_candidate("quick-safe-fade", score, score.min(0.78), &anchors, b_start, timeline, None));
    }

    // 8. echo-out
    {
        let play_at = -2.8;
        let handoff_at = 1.8;
        let b_start = aligned_b_start(anchors.b_anchor, play_at, handoff_at);
        let score = 0.4 + scores.beat_score * 0.16 + scores.energy_score * 0.08 + scores.outro_completeness * 0.12 + (1.0 - scores.style_texture_distance) * 0.04;
        let timeline = vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "bass", "value": 0.15, "duration": 0 }),
            json!({ "t": -1.4, "deck": "B", "op": "volume", "value": 1, "duration": 1600, "curve": "equal-power-in" }),
            json!({ "t": -2, "deck": "A", "op": "echo", "enabled": true, "bpm": assessment.tempo.bpm_a, "delayBeats": 0.5, "feedback": 0.56, "wet": 0.34, "duration": 180 }),
            json!({ "t": -1.4, "deck": "A", "op": "bass", "value": 0.45, "duration": 600 }),
            json!({ "t": -1.2, "deck": "A", "op": "volume", "value": 0.32, "duration": 1400, "curve": "equal-power-out" }),
            json!({ "t": -0.4, "deck": "B", "op": "bass", "value": 1, "duration": 800 }),
            json!({ "t": 0.2, "deck": "A", "op": "volume", "value": 0, "duration": 1400, "curve": "equal-power-out" }),
            json!({ "t": 0.7, "deck": "A", "op": "echo", "enabled": false, "bpm": assessment.tempo.bpm_a, "delayBeats": 0.5, "feedback": 0.56, "wet": 0.34, "duration": 160, "tailMs": 1200 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("echo-out", score, (score + 0.08).min(0.9), &anchors, b_start, timeline, None);
        c.fallback_timeline = Some(short_safety_tl.clone());
        candidates.push(c);
    }

    // 9. source-loop-roll
    {
        let play_at = -3.2;
        let handoff_at = 0.8;
        let b_start = aligned_b_start(anchors.b_anchor, play_at, handoff_at);
        let loop_start = round3((anchors.a_exit - 4.0).max(0.0));
        let score = 0.36 + scores.beat_score * 0.18 + scores.energy_score * 0.12 + scores.bpm_score * 0.08;
        let timeline = vec![
            json!({ "t": -4, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 4, "slip": true }),
            json!({ "t": -3.2, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": -3.2, "deck": "B", "op": "bass", "value": 0.12, "duration": 0 }),
            json!({ "t": -3, "deck": "B", "op": "volume", "value": 0.72, "duration": 2400, "curve": "equal-power-in" }),
            json!({ "t": -2.4, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 2, "slip": true }),
            json!({ "t": -1.2, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 1, "slip": true }),
            json!({ "t": -1, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.36, "pulses": 4, "beats": 0.5, "attack": 12, "hold": 55, "release": 100 }),
            json!({ "t": -0.6, "deck": "A", "op": "loop", "enabled": false, "slip": true }),
            json!({ "t": -0.5, "deck": "A", "op": "volume", "value": 0, "duration": 700, "curve": "equal-power-out" }),
            json!({ "t": -0.4, "deck": "B", "op": "bass", "value": 1, "duration": 700 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("source-loop-roll", score, (score + 0.08).min(0.86), &anchors, b_start, timeline, None);
        c.window.audible_start = -4.0;
        c.window.pre_roll_duration = 0.0;
        c.fallback_timeline = Some(short_safety_tl.clone());
        candidates.push(c);
    }

    // 10. hook-teaser
    {
        let final_play_at = -3.2;
        let handoff_at = 0.8;
        let b_start = aligned_b_start(anchors.b_anchor, final_play_at, handoff_at);
        let teaser_at = round3(anchors.b_anchor).max(0.0);
        let score = 0.38 + scores.beat_score * 0.12 + scores.energy_score * 0.08 + assessment.musical_compatibility * 0.14;
        let timeline = vec![
            json!({ "t": -7, "deck": "B", "op": "play", "at": teaser_at, "volume": 0 }),
            json!({ "t": -6.8, "deck": "B", "op": "filter", "type": "highpass", "value": 1200, "duration": 0 }),
            json!({ "t": -7, "deck": "B", "op": "volume", "value": 0.32, "duration": 260 }),
            json!({ "t": -5.8, "deck": "B", "op": "volume", "value": 0, "duration": 320 }),
            json!({ "t": -5.4, "deck": "B", "op": "stop" }),
            json!({ "t": final_play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": final_play_at, "deck": "B", "op": "bass", "value": 0.12, "duration": 0 }),
            json!({ "t": -3, "deck": "B", "op": "volume", "value": 0.78, "duration": 2400, "curve": "equal-power-in" }),
            json!({ "t": -1.8, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.28, "pulses": 4, "beats": 1, "attack": 18, "hold": 60, "release": 130 }),
            json!({ "t": -0.7, "deck": "A", "op": "bass", "value": 0.2, "duration": 600 }),
            json!({ "t": -0.5, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 600 }),
            json!({ "t": -0.4, "deck": "B", "op": "bass", "value": 1, "duration": 700 }),
            json!({ "t": -0.3, "deck": "A", "op": "volume", "value": 0, "duration": 700, "curve": "equal-power-out" }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("hook-teaser", score, (score + 0.08).min(0.88), &anchors, b_start, timeline, None);
        c.window.audible_start = -7.0;
        c.window.pre_roll_duration = 0.0;
        c.fallback_timeline = Some(short_safety_tl.clone());
        candidates.push(c);
    }

    // 11. harmonic-double-drop
    {
        let play_at = -1.6;
        let handoff_at = 0.6;
        let b_start = aligned_b_start(anchors.b_anchor, play_at, handoff_at);
        let score = 0.42 + scores.beat_score * 0.12 + scores.energy_score * 0.1 + assessment.musical_compatibility * 0.18;
        let timeline = vec![
            json!({ "t": play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": play_at, "deck": "B", "op": "bass", "value": 0.08, "duration": 0 }),
            json!({ "t": -1.3, "deck": "B", "op": "volume", "value": 0.82, "duration": 900, "curve": "equal-power-in" }),
            json!({ "t": -0.8, "deck": "A", "op": "duck", "bpm": assessment.tempo.bpm_a, "depth": 0.42, "pulses": 4, "beats": 0.5, "attack": 10, "hold": 55, "release": 95 }),
            json!({ "t": -0.45, "deck": "A", "op": "bass", "value": 0.18, "duration": 320 }),
            json!({ "t": -0.2, "deck": "B", "op": "bass", "value": 1, "duration": 320 }),
            json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": 420, "curve": "equal-power-out" }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("harmonic-double-drop", score, (score + 0.08).min(0.94), &anchors, b_start, timeline, None);
        c.fallback_timeline = Some(short_safety_tl);
        candidates.push(c);
    }

    // 12. tease-roll-double-drop
    {
        let teaser_at = -7.2;
        let final_play_at = -1.6;
        let handoff_at = 0.6;
        let four_beat_dur = 240.0 / assessment.tempo.bpm_a.max(1.0);
        let loop_start = round3(anchors.a_exit - four_beat_dur).max(0.0);
        let b_start = round3(anchors.b_anchor + final_play_at).max(0.0);
        let score = 0.44 + scores.beat_score * 0.14 + scores.energy_score * 0.1 + assessment.musical_compatibility * 0.16;
        let timeline = vec![
            json!({ "t": teaser_at, "deck": "B", "op": "play", "at": anchors.b_anchor, "volume": 0 }),
            json!({ "t": teaser_at, "deck": "B", "op": "filter", "type": "highpass", "value": 1200, "duration": 0 }),
            json!({ "t": teaser_at, "deck": "B", "op": "bass", "value": 0.1, "duration": 0 }),
            json!({ "t": -7, "deck": "B", "op": "volume", "value": 0.32, "duration": 220 }),
            json!({ "t": -6.1, "deck": "B", "op": "volume", "value": 0, "duration": 180 }),
            json!({ "t": -5.8, "deck": "B", "op": "stop" }),
            json!({ "t": -4, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 4, "slip": true }),
            json!({ "t": -2, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 2, "slip": true }),
            json!({ "t": final_play_at, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
            json!({ "t": final_play_at, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 0 }),
            json!({ "t": final_play_at, "deck": "B", "op": "bass", "value": 0.08, "duration": 0 }),
            json!({ "t": -1, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 1, "slip": true }),
            json!({ "t": -0.5, "deck": "A", "op": "loop", "enabled": true, "startAt": loop_start, "bpm": assessment.tempo.bpm_a, "loopBeats": 0.5, "slip": true }),
            json!({ "t": -0.14, "deck": "A", "op": "loop", "enabled": false, "slip": true }),
            json!({ "t": -0.14, "deck": "A", "op": "volume", "value": 0.08, "duration": 0, "optionalWhenLate": true, "maxLateMs": 60 }),
            json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": 0 }),
            json!({ "t": 0, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": 0 }),
            json!({ "t": 0, "deck": "B", "op": "volume", "value": 1, "duration": 0 }),
            json!({ "t": 0, "deck": "B", "op": "bass", "value": 1, "duration": 0 }),
            json!({ "t": handoff_at, "deck": "B", "op": "handoff" }),
        ];
        let mut c = make_base_candidate("tease-roll-double-drop", score, (score + 0.08).min(0.96), &anchors, b_start, timeline, Some(0.0));
        c.window.audible_start = teaser_at;
        c.window.pre_roll_duration = 0.0;
        c.fallback_timeline = Some(bass_handoff_timeline);
        c.fallback_recipe = Some("bass-eq-handoff".to_string());
        candidates.push(c);
    }

    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.confidence.total_cmp(&a.confidence))
    });

    let entry_type = entry.kind.as_str();
    for cand in &mut candidates {
        let (elig, reason, pref) = evaluate_recipe_eligibility(
            cand,
            &assessment,
            route,
            &scores,
            severe_overlap_risk,
            &section_eval.tier,
            &section_eval.risks,
            entry_type,
            assessment.entry_confidence,
            recent_recipes,
            from.duration,
        );
        cand.eligible = elig;
        cand.eligibility_reason = reason;
        cand.selection_score = round3(cand.score + pref);
    }

    (candidates, scores, assessment, anchors)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_recipe_eligibility(
    cand: &RecipeCandidate,
    assessment: &SafetyAssessment,
    route: &str,
    scores: &CommonScores,
    severe_overlap_risk: bool,
    section_tier: &str,
    section_risks: &[String],
    entry_type: &str,
    entry_confidence: f64,
    recent_recipes: &[String],
    source_duration: f64,
) -> (bool, String, f64) {
    if !cand.window.runway_available {
        return (false, "insufficient B runway".to_string(), 0.0);
    }
    if route == "clean-boundary-handoff" && cand.recipe != "clean-boundary-handoff" {
        return (false, "clean route forbids foreground overlap".to_string(), 0.0);
    }
    match cand.recipe.as_str() {
        "clean-boundary-handoff" => {
            if route != "clean-boundary-handoff" {
                return (false, "clean route is not selected".to_string(), 0.0);
            }
            let clean_struct = assessment.exit_source == "structure"
                && assessment.exit_confidence >= 0.55
                && matches!(
                    assessment.exit_type.to_lowercase().as_str(),
                    "cadence-boundary"
                        | "post-hook-boundary"
                        | "phrase-boundary"
                        | "vocal-end-boundary"
                        | "release"
                        | "outro"
                        | "natural-tail"
                );
            if !assessment.entry_trusted || !(assessment.exit_trusted || clean_struct) {
                return (false, "entry or exit evidence is not trusted".to_string(), 0.0);
            }
            (true, String::new(), 0.5)
        }
        "safety-long-blend" => (false, "fallback only".to_string(), 0.0),
        "intro-outro-long-blend" => {
            if !route.is_empty() && route != "structure-mix" {
                return (false, "route requires shorter overlap".to_string(), 0.0);
            }
            if !assessment.entry_trusted {
                return (false, "entry evidence is not trusted".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted {
                return (false, "beat grid is not trusted".to_string(), 0.0);
            }
            if assessment.tempo.relative_tempo_delta > 0.08 {
                return (false, "tempo delta exceeds long blend limit".to_string(), 0.0);
            }
            if assessment.overlap_class != "long" {
                return (false, "long overlap is not available".to_string(), 0.0);
            }
            (true, String::new(), 0.12)
        }
        "bass-eq-handoff" => {
            if route == "late-contrast-rise" || route == "terminal-rescue" {
                return (false, "route requires a short protected handoff".to_string(), 0.0);
            }
            if !assessment.entry_trusted {
                return (false, "entry evidence is not trusted".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted {
                return (false, "beat grid is not trusted".to_string(), 0.0);
            }
            if assessment.tempo.relative_tempo_delta > 0.12 {
                return (false, "tempo delta exceeds bass handoff limit".to_string(), 0.0);
            }
            if assessment.overlap_class == "short" {
                return (false, "bass handoff needs medium runway".to_string(), 0.0);
            }
            let pref = if route == "late-contrast-release" { 0.28 } else { 0.18 };
            (true, String::new(), pref)
        }
        "spectral-emergence" => {
            if route != "structure-mix" {
                return (false, "route does not support a staged spectrum reveal".to_string(), 0.0);
            }
            if !assessment.entry_trusted || !assessment.exit_trusted {
                return (false, "entry or exit evidence is not trusted".to_string(), 0.0);
            }
            if !matches!(
                assessment.exit_type.as_str(),
                "release" | "phrase-boundary" | "outro" | "natural-tail"
            ) {
                return (false, "exit is not a spectrum-safe release".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted
                || assessment.tempo.tempo_family_delta > 0.06
                || !assessment.tempo.tempo_lock_available
            {
                return (false, "beat or tempo evidence is unsafe".to_string(), 0.0);
            }
            if !assessment.musical_evidence
                || assessment.musical_compatibility < 0.68
                || assessment.key_compatibility < 0.58
            {
                return (false, "musical commonality is not strong enough".to_string(), 0.0);
            }
            if scores.a_bass > 0.5 || scores.b_bass < 0.25 {
                return (false, "low-first reveal would cause weak or stacked bass".to_string(), 0.0);
            }
            if severe_overlap_risk {
                return (false, "vocal or style overlap is unsafe".to_string(), 0.0);
            }
            (true, String::new(), 0.4)
        }
        "filtered-pickup" => {
            if !assessment.entry_trusted {
                return (false, "entry evidence is not trusted".to_string(), 0.0);
            }
            if route == "terminal-rescue" || route == "late-contrast-release" {
                return (false, "route does not support an energy pickup".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted {
                return (false, "beat grid is not trusted".to_string(), 0.0);
            }
            if assessment.tempo.relative_tempo_delta > 0.1 {
                return (false, "tempo delta exceeds filtered pickup limit".to_string(), 0.0);
            }
            let rising = route == "late-contrast-rise"
                || scores.b_energy > scores.a_energy + 0.08
                || scores.b_intro_aggression >= 0.58;
            if !rising {
                return (false, "no controlled energy rise".to_string(), 0.0);
            }
            let pref = if route == "late-contrast-rise" { 0.35 } else { 0.08 };
            (true, String::new(), pref)
        }
        "echo-out" => {
            let late_route = matches!(
                route,
                "late-contrast-rise" | "late-contrast-release" | "terminal-rescue"
            );
            if !late_route && !severe_overlap_risk {
                return (false, "echo is reserved for difficult transitions".to_string(), 0.0);
            }
            let pref = if severe_overlap_risk {
                0.7
            } else if route == "late-contrast-release" {
                0.22
            } else {
                0.08
            };
            (true, String::new(), pref)
        }
        "source-loop-roll" => {
            if route != "structure-mix" && route != "late-contrast-release" {
                return (false, "route does not support a loop roll".to_string(), 0.0);
            }
            if !assessment.entry_trusted || !assessment.exit_trusted {
                return (false, "entry or exit evidence is not trusted".to_string(), 0.0);
            }
            if !matches!(
                assessment.exit_type.as_str(),
                "release" | "phrase-boundary" | "outro" | "natural-tail"
            ) {
                return (false, "exit is not a loop-safe phrase".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted {
                return (false, "beat grid is not trusted".to_string(), 0.0);
            }
            if assessment.tempo.relative_tempo_delta > 0.08 {
                return (false, "tempo delta exceeds loop roll limit".to_string(), 0.0);
            }
            if assessment.source_runway < 2.0 {
                return (false, "source runway is too short for loop release".to_string(), 0.0);
            }
            if severe_overlap_risk {
                return (false, "vocal or style overlap is unsafe".to_string(), 0.0);
            }
            (true, String::new(), 0.16)
        }
        "hook-teaser" => {
            if route != "structure-mix" {
                return (false, "route does not support a hook teaser".to_string(), 0.0);
            }
            if !assessment.entry_trusted || !matches!(entry_type, "hook" | "chorus" | "drop") {
                return (false, "landing is not a trusted climax".to_string(), 0.0);
            }
            if !assessment.musical_evidence
                || assessment.musical_compatibility < 0.72
                || assessment.melody_similarity < 0.55
            {
                return (false, "musical evidence is not compatible enough".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted || assessment.tempo.relative_tempo_delta > 0.1 {
                return (false, "beat or tempo evidence is unsafe".to_string(), 0.0);
            }
            if severe_overlap_risk {
                return (false, "vocal or style overlap is unsafe".to_string(), 0.0);
            }
            (true, String::new(), 0.18)
        }
        "harmonic-double-drop" => {
            if route != "structure-mix" || section_tier != "magic" {
                return (false, "double drop requires a magic structure route".to_string(), 0.0);
            }
            if !assessment.entry_trusted || !matches!(entry_type, "hook" | "chorus" | "drop") {
                return (false, "landing is not a trusted climax".to_string(), 0.0);
            }
            if !assessment.musical_evidence
                || assessment.musical_compatibility < 0.78
                || assessment.key_compatibility < 0.72
            {
                return (false, "harmonic match is not tight enough".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted
                || assessment.tempo.relative_tempo_delta > 0.06
                || scores.energy_score < 0.7
            {
                return (false, "beat, tempo, or energy match is unsafe".to_string(), 0.0);
            }
            if severe_overlap_risk {
                return (false, "vocal or style overlap is unsafe".to_string(), 0.0);
            }
            (true, String::new(), 0.42)
        }
        "tease-roll-double-drop" => {
            if recent_recipes.iter().any(|r| r == "tease-roll-double-drop") {
                return (false, "impact recipe cooldown".to_string(), 0.0);
            }
            if route != "structure-mix" {
                return (false, "route does not support the impact recipe".to_string(), 0.0);
            }
            if section_risks.iter().any(|r| r == "directionality mismatch") {
                return (false, "outgoing phrase direction is not loop-safe".to_string(), 0.0);
            }
            if assessment.entry_source == "fallback"
                || entry_confidence < 0.78
                || !matches!(entry_type, "hook" | "chorus" | "drop")
            {
                return (false, "landing is not a trusted climax".to_string(), 0.0);
            }
            if !assessment.exit_trusted {
                return (false, "entry or exit evidence is not trusted".to_string(), 0.0);
            }
            if !matches!(
                assessment.exit_type.as_str(),
                "release" | "phrase-boundary" | "outro" | "natural-tail"
            ) {
                return (false, "exit is not a loop-safe phrase".to_string(), 0.0);
            }
            if !assessment.beat_grid_trusted || assessment.tempo.relative_tempo_delta > 0.06 {
                return (false, "beat or tempo evidence is unsafe".to_string(), 0.0);
            }
            let clash = assessment
                .musical_risks
                .iter()
                .any(|r| r == "harmonic-clash" || r == "melody-contour-contrast");
            if !assessment.musical_evidence || assessment.musical_compatibility < 0.72 || clash {
                return (false, "musical evidence is not compatible enough".to_string(), 0.0);
            }
            let four_beat_dur = 240.0 / assessment.tempo.bpm_a.max(1.0);
            let source_exit = cand.a_exit;
            let source_start = source_exit - four_beat_dur;
            if !source_start.is_finite() || source_start < 0.0 || source_exit > source_duration {
                return (false, "source runway is too short for four beats".to_string(), 0.0);
            }
            if severe_overlap_risk {
                return (false, "vocal or style overlap is unsafe".to_string(), 0.0);
            }
            (true, String::new(), 0.56)
        }
        "quick-safe-fade" => (true, String::new(), 0.04),
        _ => (false, "unsupported recipe".to_string(), 0.0),
    }
}

// ============================================================================
// Window Planner & Terminal Rescue & Cadence Fallback
// ============================================================================

fn landing_kind(entry: &TransitionCandidate) -> String {
    if let Some(ref lt) = entry.landing_type {
        let lower = lt.to_lowercase();
        if !lower.is_empty() {
            return lower;
        }
    }
    let kind = entry.kind.to_lowercase();
    if matches!(kind.as_str(), "pre-hook" | "hook" | "chorus") {
        return "hook".to_string();
    }
    if kind == "low-density-start" {
        return "start".to_string();
    }
    if matches!(kind.as_str(), "intro" | "drop" | "start") {
        return kind;
    }
    if entry.source == "fallback" {
        "start".to_string()
    } else {
        kind
    }
}

fn normalize_entry(candidate: &TransitionCandidate) -> TransitionCandidate {
    let lt = landing_kind(candidate);
    let landing_at = candidate
        .landing_at
        .or_else(|| candidate.resolves_to.as_ref().map(|r| r.time))
        .unwrap_or(candidate.time);
    let play_from = candidate.play_from.unwrap_or(candidate.time);
    let mut out = candidate.clone();
    out.landing_at = Some(landing_at);
    out.play_from = Some(play_from);
    out.landing_type = Some(lt);
    out
}

fn credible_hook_evidence_cand(c: &TransitionCandidate) -> bool {
    if let Some(ref ev) = c.evidence {
        return ev.repeated_line_count >= 2 && ev.repeated_block_count >= 2 && ev.sustained_energy;
    }
    c.repeated_line_count.unwrap_or(0) >= 2
        && c.repeated_block_count.unwrap_or(0) >= 2
        && c.sustained_energy.unwrap_or(false)
}

fn credible_hook_evidence_sec(s: &crate::cuefield::structure::StructureSection) -> bool {
    if let Some(ref ev) = s.evidence {
        return ev.repeated_line_count >= 2 && ev.repeated_block_count >= 2 && ev.sustained_energy;
    }
    s.repeated_line_count.unwrap_or(0) >= 2
        && s.repeated_block_count.unwrap_or(0) >= 2
        && s.sustained_energy.unwrap_or(false)
}

fn same_landing(t1: f64, t2: f64) -> bool {
    t1.is_finite() && t2.is_finite() && (t1 - t2).abs() < 1.5
}

fn landing_time_cand(c: &TransitionCandidate) -> f64 {
    c.landing_at
        .or_else(|| c.resolves_to.as_ref().map(|r| r.time))
        .unwrap_or(c.time)
}

fn trusted_structure_hook_entries(structure: &StructureMap) -> Vec<TransitionCandidate> {
    if structure.structure_source != "lyric+beat" {
        return Vec::new();
    }
    let sections: Vec<&crate::cuefield::structure::StructureSection> = structure
        .sections
        .iter()
        .filter(|s| {
            matches!(s.kind.to_lowercase().as_str(), "hook" | "chorus")
                && s.confidence >= 0.65
                && credible_hook_evidence_sec(s)
        })
        .collect();

    let direct_hooks: Vec<TransitionCandidate> = structure
        .entry_candidates
        .iter()
        .filter(|c| {
            c.role == "entry"
                && matches!(c.kind.to_lowercase().as_str(), "hook" | "chorus")
                && landing_kind(c) == "hook"
                && c.confidence >= 0.65
                && credible_hook_evidence_cand(c)
                && sections.iter().any(|s| same_landing(landing_time_cand(c), s.start))
        })
        .cloned()
        .collect();

    let pre_hooks: Vec<TransitionCandidate> = structure
        .entry_candidates
        .iter()
        .filter(|c| {
            c.role == "entry"
                && c.kind.eq_ignore_ascii_case("pre-hook")
                && landing_kind(c) == "hook"
                && c.confidence >= 0.65
                && c.resolves_to
                    .as_ref()
                    .map(|r| {
                        matches!(r.kind.to_lowercase().as_str(), "hook" | "chorus")
                            && direct_hooks.iter().any(|h| {
                                let ht = landing_time_cand(h);
                                same_landing(r.time, ht) && same_landing(landing_time_cand(c), ht)
                            })
                    })
                    .unwrap_or(false)
        })
        .cloned()
        .collect();

    let mut out = direct_hooks;
    out.extend(pre_hooks);
    out
}

fn natural_landing(c: &TransitionCandidate) -> bool {
    if c.role != "entry" || c.source.eq_ignore_ascii_case("lyric") {
        return false;
    }
    let k = c.kind.to_lowercase();
    if matches!(k.as_str(), "hook" | "chorus" | "pre-hook" | "pre-section") {
        return false;
    }
    matches!(landing_kind(c).as_str(), "start" | "intro" | "drop")
}

fn source_landings(analysis: &NormalizedAnalysis) -> Vec<TransitionCandidate> {
    let mut out: Vec<TransitionCandidate> = analysis
        .structure_map
        .entry_candidates
        .iter()
        .filter(|c| natural_landing(c))
        .cloned()
        .collect();
    out.extend(trusted_structure_hook_entries(&analysis.structure_map));
    out.extend(analysis.candidates.iter().filter(|c| natural_landing(c)).cloned());
    out
}

fn landing_options(analysis: &NormalizedAnalysis) -> Vec<TransitionCandidate> {
    let mut candidates: Vec<TransitionCandidate> =
        source_landings(analysis).iter().map(normalize_entry).collect();
    candidates.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.time.total_cmp(&b.time))
    });
    let mut unique: Vec<TransitionCandidate> = Vec::new();
    for cand in candidates {
        let lt = landing_kind(&cand);
        let la = cand.landing_at.unwrap_or(cand.time);
        let dup = unique.iter().any(|item| {
            landing_kind(item) == lt && (item.landing_at.unwrap_or(item.time) - la).abs() < 1.5
        });
        if !dup {
            unique.push(cand);
            if unique.len() >= 6 {
                break;
            }
        }
    }
    unique
}

fn exit_source_candidates(analysis: &NormalizedAnalysis) -> Vec<TransitionCandidate> {
    if !analysis.structure_map.exit_candidates.is_empty() {
        analysis
            .structure_map
            .exit_candidates
            .iter()
            .filter(|c| c.role == "exit")
            .cloned()
            .collect()
    } else {
        analysis
            .candidates
            .iter()
            .filter(|c| c.role == "exit")
            .cloned()
            .collect()
    }
}

fn source_exits(analysis: &NormalizedAnalysis, protected_until: f64) -> Vec<TransitionCandidate> {
    exit_source_candidates(analysis)
        .into_iter()
        .filter(|c| c.time >= protected_until)
        .collect()
}

fn normalize_penalty(v: Option<f64>) -> f64 {
    match v {
        Some(x) if x == f64::INFINITY => 1.0,
        Some(x) if x.is_finite() && x > 0.0 => x.min(1.0),
        _ => 0.0,
    }
}

fn score_window_exit(c: &TransitionCandidate) -> f64 {
    let mut score = clamp01(c.confidence);
    if c.kind == "release" {
        score += 0.18;
    }
    if c.energy_after < c.energy_before {
        score += 0.08;
    }
    clamp01(score - normalize_penalty(c.late_penalty))
}

fn active_vocal_window(windows: &[crate::cuefield::structure::TimeWindow], time: f64) -> Option<(f64, f64)> {
    windows
        .iter()
        .find(|w| w.start + LANDING_TOLERANCE_SEC < time && w.end > time)
        .map(|w| (w.start, w.end))
}

fn exit_options(analysis: &NormalizedAnalysis, protected_until: f64, policy: &RoutePolicy) -> Vec<TransitionCandidate> {
    let all = source_exits(analysis, protected_until);
    let in_range: Vec<TransitionCandidate> = if policy.preferred_exit_range.len() == 2 {
        let min_r = policy.preferred_exit_range[0];
        let max_r = policy.preferred_exit_range[1];
        let filtered: Vec<TransitionCandidate> = all
            .iter()
            .filter(|c| {
                let ratio = c.time / analysis.duration.max(1.0);
                ratio >= min_r && ratio <= max_r
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            filtered
        } else if policy.route == "structure-mix" {
            all
        } else {
            Vec::new()
        }
    } else {
        all
    };

    let mut completed: Vec<TransitionCandidate> = if policy.route == "clean-boundary-handoff" {
        in_range
            .into_iter()
            .map(|mut c| {
                if let Some((_, end)) = active_vocal_window(&analysis.structure_map.vocal_windows, c.time) {
                    let completed_at = round3(end);
                    c.kind = "vocal-end-boundary".to_string();
                    c.time = completed_at;
                    c.exit_ratio = Some(if analysis.duration > 0.0 {
                        round3(completed_at / analysis.duration)
                    } else {
                        0.0
                    });
                }
                c
            })
            .collect()
    } else {
        in_range
    };

    completed.sort_by(|a, b| {
        score_window_exit(b)
            .total_cmp(&score_window_exit(a))
            .then_with(|| a.time.total_cmp(&b.time))
    });
    completed.truncate(8);
    completed
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct PlannedWindow {
    exit: Option<TransitionCandidate>,
    entry: TransitionCandidate,
    section_score: f64,
    evaluation: Option<PairEvaluation>,
    recipe: String,
    recipe_candidate_value: Value,
    timeline: Vec<Value>,
    effective_source_end: Option<f64>,
    mix_start: Option<f64>,
    handoff_at: Option<f64>,
    audible_overlap: Option<f64>,
    pre_roll_duration: Option<f64>,
    exit_ratio: Option<f64>,
    energy_continuity: Option<f64>,
    groove_continuity: Option<f64>,
    tempo_compatibility: Option<f64>,
    local_musical: Option<crate::cuefield::structure::LocalMusicalComparison>,
    score: f64,
    selection_score: f64,
    rejection_reasons: Vec<String>,
    rescue_class: String,
    rescue_reason: String,
    route_fallback_used: bool,
    technical_failure: bool,
    error_code: String,
    cadence_fallback: Option<Value>,
    clean_boundary: Option<Value>,
    policy: RoutePolicy,
    // diagnostics
    a_energy: f64,
    b_energy: f64,
    a_bass: f64,
    b_bass: f64,
    beat_score: f64,
    energy_score: f64,
    bass_score: f64,
    bpm_score: f64,
    outro_completeness: f64,
    b_intro_aggression: f64,
    style_texture_distance: f64,
    relative_tempo_delta: f64,
    beat_grid_trusted: bool,
    musical_evidence: bool,
    musical_compatibility: f64,
    harmonic_similarity: f64,
    key_compatibility: f64,
    melody_similarity: f64,
    musical_risks: Vec<String>,
    overlap_class: String,
    overlap_duration: f64,
}

fn compact_window_summary(w: &PlannedWindow) -> Value {
    json!({
        "exit": w.exit.as_ref().map(|e| json!({
            "type": e.kind,
            "time": e.time,
            "exitRatio": w.exit_ratio,
        })).unwrap_or(json!({ "type": Value::Null, "time": Value::Null, "exitRatio": w.exit_ratio })),
        "entry": {
            "type": w.entry.kind,
            "source": w.entry.source,
            "landingType": w.entry.landing_type,
            "landingAt": w.entry.landing_at,
        },
        "recipe": w.recipe,
        "score": clamp01(w.score),
        "audibleOverlap": w.audible_overlap.unwrap_or(0.0),
        "mixStart": w.mix_start,
        "handoffAt": w.handoff_at,
        "localMusicalEvidence": w.local_musical.as_ref().map(|lm| json!({
            "score": lm.score,
            "harmonicSimilarity": lm.harmonic_similarity,
            "keyCompatibility": lm.key_compatibility,
            "melodySimilarity": lm.melody_similarity,
            "confidence": lm.confidence,
            "aWindowStart": lm.a_window_start,
            "bWindowStart": lm.b_window_start,
            "aDistance": lm.a_distance,
            "bDistance": lm.b_distance,
            "risks": lm.risks.iter().take(4).cloned().collect::<Vec<_>>(),
        })),
        "rejectionReasons": w.rejection_reasons,
    })
}

fn ceil_ms(v: f64) -> f64 {
    ((v * 1000.0 - 0.000001).ceil()) / 1000.0
}

fn floor_ms(v: f64) -> f64 {
    ((v * 1000.0 + 0.000001).floor()) / 1000.0
}

fn effective_source_end(analysis: &NormalizedAnalysis) -> f64 {
    let duration = analysis.duration.max(0.0);
    let mut energy = analysis.energy_windows.clone();
    energy.retain(|w| w.end > w.start && w.value.is_finite());
    energy.sort_by(|a, b| a.start.total_cmp(&b.start));
    if energy.is_empty() || energy.last().map(|w| w.end < duration - 0.25).unwrap_or(true) {
        return duration;
    }
    let mut idx = energy.len() as isize - 1;
    while idx >= 0 && energy[idx as usize].value <= 0.001 {
        idx -= 1;
    }
    if idx < 0 || idx as usize == energy.len() - 1 {
        return duration;
    }
    let trailing_start = energy[idx as usize + 1].start;
    if duration - trailing_start >= MINIMUM_TERMINAL_OVERLAP {
        floor_ms(trailing_start)
    } else {
        duration
    }
}

fn terminal_start_after_vocal(windows: &[crate::cuefield::structure::TimeWindow], requested: f64, duration: f64) -> f64 {
    let mut sorted = windows.to_vec();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut candidate = requested;
    for w in sorted {
        if w.end > candidate && w.start < candidate + 0.6 {
            candidate = w.end + 0.12;
        }
    }
    if duration - candidate >= MINIMUM_TERMINAL_OVERLAP {
        ceil_ms(candidate)
    } else {
        ceil_ms(requested)
    }
}

fn technical_failure_window(error_code: &str, rejected: &[PlannedWindow], policy: RoutePolicy) -> PlannedWindow {
    let mut reasons = vec!["no valid complete transition window".to_string()];
    if !rejected.is_empty() {
        reasons.push("structural windows rejected".to_string());
    }
    PlannedWindow {
        exit: None,
        entry: TransitionCandidate {
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
        },
        section_score: 0.0,
        evaluation: None,
        recipe: "technical-failure".to_string(),
        recipe_candidate_value: json!({
            "recipe": "technical-failure",
            "score": 0,
            "timeline": [],
        }),
        timeline: Vec::new(),
        effective_source_end: None,
        mix_start: None,
        handoff_at: None,
        audible_overlap: None,
        pre_roll_duration: None,
        exit_ratio: None,
        energy_continuity: None,
        groove_continuity: None,
        tempo_compatibility: None,
        local_musical: None,
        score: 0.0,
        selection_score: 0.0,
        rejection_reasons: reasons,
        rescue_class: String::new(),
        rescue_reason: String::new(),
        route_fallback_used: false,
        technical_failure: true,
        error_code: error_code.to_string(),
        cadence_fallback: None,
        clean_boundary: None,
        policy,
        a_energy: 0.0,
        b_energy: 0.0,
        a_bass: 0.0,
        b_bass: 0.0,
        beat_score: 0.0,
        energy_score: 0.0,
        bass_score: 0.0,
        bpm_score: 0.0,
        outro_completeness: 0.0,
        b_intro_aggression: 0.0,
        style_texture_distance: 0.0,
        relative_tempo_delta: 0.0,
        beat_grid_trusted: false,
        musical_evidence: false,
        musical_compatibility: 0.0,
        harmonic_similarity: 0.0,
        key_compatibility: 0.0,
        melody_similarity: 0.0,
        musical_risks: Vec::new(),
        overlap_class: String::new(),
        overlap_duration: 0.0,
    }
}

fn terminal_rescue(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    protected_until: f64,
    policy: RoutePolicy,
    rejected: &[PlannedWindow],
    entries: &[TransitionCandidate],
) -> PlannedWindow {
    let duration = from.duration.max(0.0);
    let src_end = effective_source_end(from);
    let target_dur = to.duration.max(0.0);
    if !(duration > 0.0) {
        return technical_failure_window("TERMINAL_RESCUE_INVALID_DURATION", rejected, policy);
    }
    if !(target_dur > 0.0) {
        return technical_failure_window("TERMINAL_RESCUE_INVALID_TARGET_DURATION", rejected, policy);
    }
    if target_dur < MINIMUM_TERMINAL_OVERLAP - 0.000001 {
        return technical_failure_window("TERMINAL_RESCUE_INSUFFICIENT_TARGET_RUNWAY", rejected, policy);
    }
    if src_end - protected_until < MINIMUM_TERMINAL_OVERLAP - 0.000001 {
        return technical_failure_window(
            "TERMINAL_RESCUE_INSUFFICIENT_POST_PROTECTION_RUNWAY",
            rejected,
            policy,
        );
    }

    let min_ratio = policy.preferred_exit_range.first().copied().unwrap_or(0.88);
    let max_ratio = policy.preferred_exit_range.get(1).copied().unwrap_or(0.96);
    let mut range_exits: Vec<TransitionCandidate> = source_exits(from, protected_until)
        .into_iter()
        .filter(|c| {
            let ratio = c.time / duration.max(1.0);
            ratio >= min_ratio
                && ratio <= max_ratio
                && src_end - c.time >= MINIMUM_TERMINAL_OVERLAP - 0.000001
        })
        .collect();
    range_exits.sort_by(|a, b| b.time.total_cmp(&a.time));
    let selected_exit = range_exits.into_iter().next();

    let preferred_start = selected_exit
        .as_ref()
        .map(|e| e.time)
        .unwrap_or_else(|| protected_until.max((src_end - 3.6).min(duration * 0.92)));
    let requested_mix_start = if let Some(ref ex) = selected_exit {
        ceil_ms(protected_until.max(ex.time))
    } else {
        ceil_ms(protected_until.max((src_end - MINIMUM_TERMINAL_OVERLAP).min(preferred_start)))
    };

    let safe_mix_start = terminal_start_after_vocal(
        &from.structure_map.vocal_windows,
        requested_mix_start,
        duration,
    );
    let safe_overlap = floor_ms(0.0_f64.max(3.4_f64.min(src_end - safe_mix_start).min(target_dur)));

    let vocal_conflict = from.structure_map.vocal_windows.iter().any(|w| {
        w.end > safe_mix_start + 0.05 && w.start < safe_mix_start + safe_overlap
    });
    let contrast_reasons = [
        "snap-rise",
        "snap-release",
        "energy-tempo-rise",
        "energy-tempo-release",
        "style-bridge-mismatch",
    ];
    let (rescue_class, variant, rescue_reason) = if vocal_conflict {
        ("A", "vocal-release", "outgoing-vocal-conflict")
    } else if policy
        .reasons
        .iter()
        .any(|r| contrast_reasons.contains(&r.as_str()))
    {
        ("B", "staged-energy-bridge", "energy-contrast")
    } else {
        ("C", "limited-window-crossfade", "limited-transition-window")
    };

    let vocal_bed_lead = round3((safe_mix_start - requested_mix_start).max(0.0));
    let use_vocal_bed = rescue_class == "C"
        && vocal_bed_lead >= 0.5
        && src_end - requested_mix_start >= 4.6
        && target_dur >= 4.6;
    let mix_start = if use_vocal_bed {
        requested_mix_start
    } else {
        safe_mix_start
    };
    let max_ov: f64 = if use_vocal_bed { 5.8 } else { 3.4 };
    let overlap_duration = floor_ms(0.0_f64.max(max_ov.min(src_end - mix_start).min(target_dur)));
    let a_fade_at = if use_vocal_bed { vocal_bed_lead } else { 0.0 };
    let landing_offset = round3((overlap_duration - 0.35).min(overlap_duration * 0.72).max(1.65));

    let trusted_entry = entries.iter().find(|c| {
        matches!(landing_kind(c).as_str(), "hook" | "drop")
            && c.confidence >= 0.6
            && c.landing_at.unwrap_or(c.time) >= landing_offset
            && c.landing_at.unwrap_or(c.time) + overlap_duration - landing_offset <= target_dur
    });
    let requested_landing = trusted_entry
        .and_then(|c| c.landing_at)
        .unwrap_or(landing_offset);
    let b_start = round3((requested_landing - landing_offset).max(0.0));
    let actual_landing = round3(b_start + landing_offset);

    let entry = if let Some(te) = trusted_entry {
        let mut e = te.clone();
        e.play_from = Some(b_start);
        e.landing_at = Some(requested_landing);
        e
    } else {
        TransitionCandidate {
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
            play_from: Some(b_start),
            landing_at: Some(actual_landing),
            landing_type: Some("start".to_string()),
            exit_ratio: None,
            late_penalty: None,
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        }
    };

    let (a_fade_dur_ms, b_rise_at, b_rise_dur_ms, timeline) = match rescue_class {
        "A" => {
            let dur_ms = (landing_offset.max(0.8) * 1000.0).round() as i64;
            let tl = vec![
                json!({ "t": 0, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
                json!({ "t": a_fade_at, "deck": "A", "op": "volume", "value": 0, "duration": dur_ms, "curve": "equal-power-out" }),
                json!({ "t": 0, "deck": "B", "op": "volume", "value": 1, "duration": dur_ms, "curve": "equal-power-in" }),
                json!({ "t": overlap_duration, "deck": "B", "op": "handoff" }),
            ];
            (dur_ms, 0.0, dur_ms, tl)
        }
        "B" => {
            let a_dur_ms = (overlap_duration * 1000.0).round() as i64;
            let b_at = round3(0.35_f64.min(landing_offset * 0.14));
            let b_dur_ms = ((landing_offset - b_at).max(0.8) * 1000.0).round() as i64;
            let stage_at = round3(0.35_f64.min(landing_offset * 0.16));
            let final_at = round3((stage_at + 0.9).max(overlap_duration * 0.5));
            let stage_ms = ((final_at - stage_at).max(0.2) * 1000.0).round() as i64;
            let final_ms = ((overlap_duration - final_at).max(0.2) * 1000.0).round() as i64;
            let tl = vec![
                json!({ "t": 0, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
                json!({ "t": 0, "deck": "B", "op": "bass", "value": 0.25, "duration": 0 }),
                json!({ "t": 0, "deck": "B", "op": "filter", "type": "highpass", "value": 900, "duration": 0 }),
                json!({ "t": 0, "deck": "A", "op": "bass", "value": 0.82, "duration": a_dur_ms }),
                json!({ "t": a_fade_at, "deck": "A", "op": "volume", "value": 0, "duration": a_dur_ms, "curve": "equal-power-out" }),
                json!({ "t": b_at, "deck": "B", "op": "volume", "value": 1, "duration": b_dur_ms, "curve": "equal-power-in" }),
                json!({ "t": stage_at, "deck": "B", "op": "filter", "type": "highpass", "value": 450, "duration": stage_ms }),
                json!({ "t": stage_at, "deck": "B", "op": "bass", "value": 0.55, "duration": stage_ms }),
                json!({ "t": final_at, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": final_ms }),
                json!({ "t": final_at, "deck": "B", "op": "bass", "value": 1, "duration": final_ms }),
                json!({ "t": overlap_duration, "deck": "B", "op": "handoff" }),
            ];
            (a_dur_ms, b_at, b_dur_ms, tl)
        }
        _ => {
            let a_dur_ms = ((overlap_duration - a_fade_at).max(0.2) * 1000.0).round() as i64;
            let b_dur_ms = (landing_offset * 1000.0).round() as i64;
            let tl = vec![
                json!({ "t": 0, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
                json!({ "t": a_fade_at, "deck": "A", "op": "volume", "value": 0, "duration": a_dur_ms, "curve": "equal-power-out" }),
                json!({ "t": 0, "deck": "B", "op": "volume", "value": 1, "duration": b_dur_ms, "curve": "equal-power-in" }),
                json!({ "t": overlap_duration, "deck": "B", "op": "handoff" }),
            ];
            (a_dur_ms, 0.0, b_dur_ms, tl)
        }
    };

    let audible_start = b_rise_at + (b_rise_dur_ms as f64 / 1000.0) * (0.08_f64.asin() / (PI / 2.0));
    let audible_end = a_fade_at + (a_fade_dur_ms as f64 / 1000.0) * (0.08_f64.acos() / (PI / 2.0));
    let audible_overlap = round3((audible_end - audible_start).max(0.0));

    let exit = if let Some(mut se) = selected_exit {
        se.time = mix_start;
        se
    } else {
        TransitionCandidate {
            kind: "terminal-boundary".to_string(),
            role: "exit".to_string(),
            source: "fallback".to_string(),
            time: mix_start,
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
            exit_ratio: None,
            late_penalty: None,
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        }
    };
    let local_musical = compare_local_musical_windows(
        from.musical_profile.as_ref(),
        to.musical_profile.as_ref(),
        exit.time,
        entry.play_from.unwrap_or(entry.time),
    );
    let mut rej_reasons = vec!["no valid complete transition window".to_string()];
    if !rejected.is_empty() {
        rej_reasons.push("structural windows rejected".to_string());
    }

    let fallback_tl = vec![
        json!({ "t": 0, "deck": "B", "op": "play", "at": b_start, "volume": 0 }),
        json!({ "t": a_fade_at, "deck": "A", "op": "volume", "value": 0, "duration": a_fade_dur_ms, "curve": "equal-power-out" }),
        json!({ "t": b_rise_at, "deck": "B", "op": "volume", "value": 1, "duration": b_rise_dur_ms, "curve": "equal-power-in" }),
        json!({ "t": overlap_duration, "deck": "B", "op": "handoff" }),
    ];

    PlannedWindow {
        exit: Some(exit),
        entry,
        section_score: 0.0,
        evaluation: None,
        recipe: "terminal-rescue".to_string(),
        recipe_candidate_value: json!({
            "recipe": "terminal-rescue",
            "variant": variant,
            "rescueClass": rescue_class,
            "rescueReason": rescue_reason,
            "score": 0,
            "timeline": timeline,
            "fallbackTimeline": fallback_tl,
        }),
        timeline,
        effective_source_end: Some(src_end),
        mix_start: Some(mix_start),
        handoff_at: Some(round3(mix_start + overlap_duration)),
        audible_overlap: Some(audible_overlap),
        pre_roll_duration: Some(round3(audible_start)),
        exit_ratio: Some(round3(mix_start / duration.max(1.0))),
        energy_continuity: Some(0.35),
        groove_continuity: Some(0.35),
        tempo_compatibility: Some(0.0),
        local_musical,
        score: 0.0,
        selection_score: 0.0,
        rejection_reasons: rej_reasons,
        rescue_class: rescue_class.to_string(),
        rescue_reason: rescue_reason.to_string(),
        route_fallback_used: true,
        technical_failure: false,
        error_code: String::new(),
        cadence_fallback: None,
        clean_boundary: None,
        policy,
        a_energy: 0.0,
        b_energy: 0.0,
        a_bass: 0.0,
        b_bass: 0.0,
        beat_score: 0.0,
        energy_score: 0.0,
        bass_score: 0.0,
        bpm_score: 0.0,
        outro_completeness: 0.0,
        b_intro_aggression: 0.0,
        style_texture_distance: 0.0,
        relative_tempo_delta: 0.0,
        beat_grid_trusted: false,
        musical_evidence: false,
        musical_compatibility: 0.0,
        harmonic_similarity: 0.0,
        key_compatibility: 0.0,
        melody_similarity: 0.0,
        musical_risks: Vec::new(),
        overlap_class: "short".to_string(),
        overlap_duration,
    }
}

fn end_of_track_crossfade_window(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    cadence_reasons: &[String],
    tail_evidence: Option<&Value>,
) -> PlannedWindow {
    let source_dur = from.duration.max(0.0);
    let target_dur = to.duration.max(0.0);
    let measured_end = opt_f64(tail_evidence.and_then(|v| v.get("audibleEnd")));
    let audible_end = measured_end
        .map(|m| m.clamp(0.0, source_dur))
        .unwrap_or(source_dur);
    let from_vocal = normalized_vocal_state(tail_evidence.and_then(|v| v.get("fromVocalState")));
    let to_vocal = normalized_vocal_state(tail_evidence.and_then(|v| v.get("toVocalState")));
    let requested_dur = opt_f64(tail_evidence.and_then(|v| v.get("requestedDuration")));
    let fade_sec = choose_end_crossfade_duration(audible_end, target_dur, requested_dur, &from_vocal, &to_vocal);
    let b_pre_roll = round3(
        opt_f64(tail_evidence.and_then(|v| v.get("toAudibleStart")))
            .unwrap_or(0.0)
            .clamp(0.0, 5.0_f64.min(target_dur)),
    );
    let mix_start = round3((audible_end - fade_sec).max(0.0));
    let fade_ms = (fade_sec * 1000.0).round() as i64;
    let b_play_at = if b_pre_roll > 0.0 { -b_pre_roll } else { 0.0 };
    let timeline = vec![
        json!({ "t": b_play_at, "deck": "B", "op": "play", "at": 0, "volume": 0 }),
        json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": fade_ms, "curve": "equal-power-out" }),
        json!({ "t": 0, "deck": "B", "op": "volume", "value": 1, "duration": fade_ms, "curve": "equal-power-in" }),
        json!({ "t": fade_sec, "deck": "B", "op": "handoff" }),
    ];

    let mut reasons = cadence_reasons.to_vec();
    if !reasons.iter().any(|r| r == "outro-or-intro-untrusted") {
        reasons.push("outro-or-intro-untrusted".to_string());
    }
    reasons.truncate(4);

    let policy = RoutePolicy {
        route: "cadence-fallback".to_string(),
        compatibility_class: "conservative".to_string(),
        contrast_direction: "unknown".to_string(),
        preferred_exit_range: vec![0.8, 1.0],
        entry_policy: "source-zero".to_string(),
        overlap_class: "short".to_string(),
        recipe: "end-of-track-crossfade".to_string(),
        reasons: reasons.clone(),
        metrics: json!({}),
    };

    let exit = TransitionCandidate {
        kind: "terminal-boundary".to_string(),
        role: "exit".to_string(),
        source: "fallback".to_string(),
        time: mix_start,
        confidence: 1.0,
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
        exit_ratio: None,
        late_penalty: None,
        original_type: None,
        repeated_line_count: None,
        repeated_block_count: None,
        energy_lift: None,
        sustained_energy: None,
        evidence: None,
    };
    let entry = TransitionCandidate {
        kind: "start".to_string(),
        role: "entry".to_string(),
        source: "fallback".to_string(),
        time: 0.0,
        confidence: 1.0,
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

    let cadence_fallback = json!({
        "mode": "end-of-track-crossfade",
        "confidence": 1,
        "reasons": reasons,
        "crossfadeDuration": fade_sec,
        "audibleEnd": audible_end,
        "containerEnd": source_dur,
        "bSourceAt": 0,
        "bPreRollDuration": b_pre_roll,
    });

    PlannedWindow {
        exit: Some(exit),
        entry,
        section_score: 0.0,
        evaluation: None,
        recipe: "end-of-track-crossfade".to_string(),
        recipe_candidate_value: json!({
            "recipe": "end-of-track-crossfade",
            "score": 0,
            "timeline": timeline,
        }),
        timeline,
        effective_source_end: Some(audible_end),
        mix_start: Some(mix_start),
        handoff_at: Some(audible_end),
        audible_overlap: Some(fade_sec),
        pre_roll_duration: Some(b_pre_roll),
        exit_ratio: Some(round3(mix_start / source_dur.max(1.0))),
        energy_continuity: Some(0.5),
        groove_continuity: Some(0.5),
        tempo_compatibility: Some(0.5),
        local_musical: None,
        score: 0.0,
        selection_score: 0.0,
        rejection_reasons: reasons,
        rescue_class: String::new(),
        rescue_reason: String::new(),
        route_fallback_used: true,
        technical_failure: false,
        error_code: String::new(),
        cadence_fallback: Some(cadence_fallback),
        clean_boundary: None,
        policy,
        a_energy: 0.0,
        b_energy: 0.0,
        a_bass: 0.0,
        b_bass: 0.0,
        beat_score: 0.0,
        energy_score: 0.0,
        bass_score: 0.0,
        bpm_score: 0.0,
        outro_completeness: 0.0,
        b_intro_aggression: 0.0,
        style_texture_distance: 0.0,
        relative_tempo_delta: 0.0,
        beat_grid_trusted: false,
        musical_evidence: false,
        musical_compatibility: 0.0,
        harmonic_similarity: 0.0,
        key_compatibility: 0.0,
        melody_similarity: 0.0,
        musical_risks: Vec::new(),
        overlap_class: "short".to_string(),
        overlap_duration: fade_sec,
    }
}

fn overlaps_window(windows: &[crate::cuefield::structure::TimeWindow], start: f64, end: f64) -> bool {
    windows.iter().any(|w| w.end > start && w.start < end)
}

#[allow(clippy::too_many_arguments)]
fn build_ranked_window(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    exit: &TransitionCandidate,
    entry: &TransitionCandidate,
    section_score: f64,
    evaluation: PairEvaluation,
    rc: RecipeCandidate,
    scores: &CommonScores,
    assessment: &SafetyAssessment,
    policy: &RoutePolicy,
    musical_score: Option<f64>,
    local_musical: Option<crate::cuefield::structure::LocalMusicalComparison>,
) -> PlannedWindow {
    let trans_start = rc.window.transition_start.unwrap_or(rc.window.audible_start);
    let mix_start = round3(exit.time + trans_start);
    let handoff_at = round3(exit.time + rc.window.handoff_offset);
    let energy_continuity = clamp01(scores.energy_score);
    let tempo_compat = clamp01(scores.bpm_score);
    let groove = if assessment.beat_grid_trusted {
        match (nearest_bar(from, exit.time), nearest_bar(to, entry.landing_at.unwrap_or(entry.time))) {
            (Some(a), Some(b)) => {
                let d = ((a.beat_stability - b.beat_stability).abs()
                    + (a.snap_density - b.snap_density).abs()
                    + (a.body_density - b.body_density).abs())
                    / 3.0;
                round3(clamp01(1.0 - d))
            }
            _ => 0.35,
        }
    } else {
        0.35
    };
    let continuity = clamp01((energy_continuity + groove + tempo_compat + clamp01(scores.bass_score)) / 4.0);
    let exit_ratio = exit
        .exit_ratio
        .map(clamp01)
        .filter(|&r| r > 0.0)
        .unwrap_or_else(|| clamp01(exit.time / from.duration.max(1.0)));
    let late_pen = normalize_penalty(exit.late_penalty).max(
        if policy.route == "structure-mix" && exit_ratio > 0.78 {
            0.45
        } else {
            0.0
        },
    );
    let range_pen = if policy.preferred_exit_range.len() == 2 {
        let min_r = policy.preferred_exit_range[0];
        let max_r = policy.preferred_exit_range[1];
        if min_r <= max_r {
            let dist = if exit_ratio < min_r {
                min_r - exit_ratio
            } else if exit_ratio > max_r {
                exit_ratio - max_r
            } else {
                0.0
            };
            clamp01(dist / (max_r - min_r).max(0.04))
        } else {
            0.0
        }
    } else {
        0.0
    };
    let entry_pen = if policy.route == "late-contrast-rise"
        && landing_kind(entry) == "hook"
        && !entry.kind.eq_ignore_ascii_case("pre-hook")
    {
        0.18
    } else {
        0.0
    };
    let route_pen = range_pen + entry_pen;
    let musical_adj = musical_score.map(|ms| (ms - 0.5) * 0.12).unwrap_or(0.0);
    let local_adj = local_musical
        .as_ref()
        .map(|lm| ((lm.score - 0.5) * 0.16).clamp(-0.08, 0.08))
        .unwrap_or(0.0);
    let max_sel = if rc.recipe == "tease-roll-double-drop" { 1.7 } else { 1.0 };
    let recipe_sel_score = rc.selection_score.clamp(0.0, max_sel);
    let overlap_fit = score_overlap_contract(rc.window.audible_overlap, rc.window.overlap_contract);
    let score = round3(clamp01(
        clamp01(section_score) * 0.34
            + recipe_sel_score * 0.2
            + ((clamp01(exit.confidence) + clamp01(entry.confidence)) / 2.0) * 0.16
            + overlap_fit * 0.12
            + continuity * 0.18
            - late_pen
            - route_pen
            + musical_adj
            + local_adj,
    ));

    let clean_boundary = if rc.recipe == "clean-boundary-handoff" {
        Some(json!({
            "variant": rc.variant.clone().unwrap_or_default(),
            "outgoingBoundaryType": exit.kind,
            "incomingBoundaryType": entry.landing_type.clone().unwrap_or_else(|| entry.kind.clone()),
            "aDownbeatOffset": if rc.b_start.is_finite() { json!(round3(nearest_downbeat_offset(from, exit.time))) } else { Value::Null },
            "bDownbeatOffset": if entry.time.is_finite() { json!(round3(nearest_downbeat_offset(to, entry.time))) } else { Value::Null },
            "landingError": rc.window.landing_error,
            "aOnlyTailDuration": rc.window.a_only_tail_duration,
            "breathDuration": rc.window.breath_duration,
            "actualTwoDeckOverlap": rc.window.actual_two_deck_overlap,
        }))
    } else {
        None
    };

    let overlap_dur = round3((rc.window.handoff_offset - rc.window.audible_start).max(rc.window.audible_overlap));
    let overlap_class = if overlap_dur <= 4.0 {
        "short"
    } else if overlap_dur <= 7.0 {
        "medium"
    } else {
        "long"
    };

    PlannedWindow {
        exit: Some(exit.clone()),
        entry: entry.clone(),
        section_score,
        evaluation: Some(evaluation),
        recipe: rc.recipe.clone(),
        recipe_candidate_value: json!({
            "recipe": rc.recipe,
            "score": rc.score,
            "selectionScore": rc.selection_score,
            "fallbackTimeline": rc.fallback_timeline,
            "fallbackRecipe": rc.fallback_recipe,
            "timeline": rc.timeline,
        }),
        timeline: rc.timeline,
        effective_source_end: None,
        mix_start: Some(mix_start),
        handoff_at: Some(handoff_at),
        audible_overlap: Some(rc.window.audible_overlap),
        pre_roll_duration: Some(rc.window.pre_roll_duration),
        exit_ratio: Some(round3(exit_ratio)),
        energy_continuity: Some(round3(energy_continuity)),
        groove_continuity: Some(groove),
        tempo_compatibility: Some(round3(tempo_compat)),
        local_musical,
        score,
        selection_score: rc.selection_score,
        rejection_reasons: Vec::new(),
        rescue_class: String::new(),
        rescue_reason: String::new(),
        route_fallback_used: false,
        technical_failure: false,
        error_code: String::new(),
        cadence_fallback: None,
        clean_boundary,
        policy: policy.clone(),
        a_energy: round3(scores.a_energy),
        b_energy: round3(scores.b_energy),
        a_bass: round3(scores.a_bass),
        b_bass: round3(scores.b_bass),
        beat_score: round3(scores.beat_score),
        energy_score: round3(scores.energy_score),
        bass_score: round3(scores.bass_score),
        bpm_score: round3(scores.bpm_score),
        outro_completeness: round3(scores.outro_completeness),
        b_intro_aggression: round3(scores.b_intro_aggression),
        style_texture_distance: round3(scores.style_texture_distance),
        relative_tempo_delta: assessment.tempo.relative_tempo_delta,
        beat_grid_trusted: assessment.beat_grid_trusted,
        musical_evidence: assessment.musical_evidence,
        musical_compatibility: assessment.musical_compatibility,
        harmonic_similarity: assessment.harmonic_similarity,
        key_compatibility: assessment.key_compatibility,
        melody_similarity: assessment.melody_similarity,
        musical_risks: assessment.musical_risks.clone(),
        overlap_class: overlap_class.to_string(),
        overlap_duration: overlap_dur,
    }
}

fn choose_cadence_fallback_window(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    protected_until: f64,
    exits: &[TransitionCandidate],
    entries: &[TransitionCandidate],
    boundary_evidence: &[Value],
    tail_evidence: Option<&Value>,
    recent_recipes: &[String],
) -> (PlannedWindow, Vec<Value>) {
    let mut cadence_rejected = Vec::new();
    let mut cadence_reasons = Vec::new();
    let mut accepted = Vec::new();

    for ex in exits {
        let ev = boundary_evidence
            .iter()
            .find(|r| opt_f64(r.get("time")).map(|t| (t - ex.time).abs() <= LANDING_TOLERANCE_SEC).unwrap_or(false))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let timed_vocal = overlaps_window(
            &from.structure_map.vocal_windows,
            ex.time - LANDING_TOLERANCE_SEC,
            ex.time + LANDING_TOLERANCE_SEC,
        );
        let res = evaluate_cadence_boundary(&ev, timed_vocal);
        if res.eligible {
            accepted.push((ex.clone(), res));
        } else {
            let short_reasons: Vec<String> = res.reasons.iter().take(4).cloned().collect();
            for r in &short_reasons {
                if !cadence_reasons.contains(r) && cadence_reasons.len() < 4 {
                    cadence_reasons.push(r.clone());
                }
            }
            cadence_rejected.push(json!({ "time": ex.time, "reasons": short_reasons }));
        }
    }
    accepted.sort_by(|a, b| a.0.time.total_cmp(&b.0.time));

    // 1. 尝试 cadence-complete
    if let Some(entry) = entries.iter().find(|c| {
        matches!(landing_kind(c).as_str(), "start" | "intro" | "drop") && c.confidence >= 0.65
    }) {
        let policy = RoutePolicy {
            route: "clean-boundary-handoff".to_string(),
            compatibility_class: "conservative".to_string(),
            contrast_direction: "unknown".to_string(),
            preferred_exit_range: vec![clamp01(protected_until / from.duration.max(1.0)), 1.0],
            entry_policy: "natural-entry".to_string(),
            overlap_class: "none".to_string(),
            recipe: "clean-boundary-handoff".to_string(),
            reasons: vec!["cadence-evidence-verified".to_string()],
            metrics: json!({}),
        };
        for (mut exit, res) in accepted {
            exit.original_type = Some(exit.kind.clone());
            exit.kind = "cadence-boundary".to_string();
            exit.confidence = exit.confidence.min(res.confidence);
            let (recipe_name, sec_score, eval) =
                score_candidate_pair(&exit, entry, from, to, score_window_exit(&exit), exits);
            let mut eval_mut = eval;
            if eval_mut.recipe.is_empty() {
                eval_mut.recipe = recipe_name;
            }
            let (cands, scores, assessment, _) =
                plan_recipe_candidates(from, to, &exit, entry, &eval_mut, &policy.route, recent_recipes);
            if let Some(rc) = cands
                .into_iter()
                .find(|c| c.recipe == "clean-boundary-handoff" && c.eligible)
            {
                let local_musical = compare_local_musical_windows(
                    from.musical_profile.as_ref(),
                    to.musical_profile.as_ref(),
                    exit.time,
                    entry.play_from.unwrap_or(entry.time),
                );
                let mut chosen = build_ranked_window(
                    from,
                    to,
                    &exit,
                    entry,
                    clamp01(sec_score),
                    eval_mut,
                    rc,
                    &scores,
                    &assessment,
                    &policy,
                    if assessment.musical_evidence {
                        Some(assessment.musical_compatibility)
                    } else {
                        None
                    },
                    local_musical,
                );
                chosen.cadence_fallback = Some(json!({
                    "mode": "cadence-complete",
                    "confidence": res.confidence,
                    "reasons": [],
                    "crossfadeDuration": 0,
                    "audibleEnd": Value::Null,
                    "containerEnd": from.duration,
                    "bSourceAt": entry.play_from.unwrap_or(entry.time),
                }));
                return (chosen, cadence_rejected);
            }
        }
    }

    // 2. 尝试 full-outro-intro
    let audible_end = opt_f64(tail_evidence.and_then(|v| v.get("audibleEnd")))
        .map(|m| m.clamp(0.0, from.duration.max(0.0)))
        .unwrap_or_else(|| from.duration.max(0.0));
    let mut outro_cands: Vec<&TransitionCandidate> = exits
        .iter()
        .filter(|c| {
            matches!(c.kind.to_lowercase().as_str(), "outro" | "natural-tail")
                && c.confidence >= 0.65
                && c.time >= protected_until
        })
        .collect();
    outro_cands.sort_by(|a, b| a.time.total_cmp(&b.time));
    let incoming_intro = entries.iter().find(|c| {
        let play_from = c.play_from.unwrap_or(c.time);
        let landing_at = c.landing_at.unwrap_or(c.time);
        c.kind.eq_ignore_ascii_case("intro")
            && c.confidence >= 0.65
            && play_from.abs() <= 0.001
            && (0.8..=6.0).contains(&landing_at)
            && landing_at <= to.duration
    });
    if let (Some(out), Some(inc)) = (outro_cands.first().copied(), incoming_intro) {
        let intro_dur = inc.landing_at.unwrap_or(inc.time);
        let mix_start = round3(audible_end - intro_dur);
        if mix_start >= out.time
            && mix_start >= protected_until
            && !overlaps_window(&from.structure_map.vocal_windows, mix_start, audible_end)
            && !overlaps_window(&to.structure_map.vocal_windows, 0.0, intro_dur)
        {
            let fade_ms = (intro_dur * 1000.0).round() as i64;
            let timeline = vec![
                json!({ "t": 0, "deck": "B", "op": "play", "at": 0, "volume": 0 }),
                json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": fade_ms, "curve": "equal-power-out" }),
                json!({ "t": 0, "deck": "B", "op": "volume", "value": 1, "duration": fade_ms, "curve": "equal-power-in" }),
                json!({ "t": intro_dur, "deck": "B", "op": "handoff" }),
            ];
            let mut ex = out.clone();
            ex.time = mix_start;
            let mut en = inc.clone();
            en.play_from = Some(0.0);
            let policy = RoutePolicy {
                route: "cadence-fallback".to_string(),
                compatibility_class: "conservative".to_string(),
                contrast_direction: "unknown".to_string(),
                preferred_exit_range: vec![0.8, 1.0],
                entry_policy: "full-intro".to_string(),
                overlap_class: "medium".to_string(),
                recipe: "intro-outro-long-blend".to_string(),
                reasons: cadence_reasons.iter().take(4).cloned().collect(),
                metrics: json!({}),
            };
            let chosen = PlannedWindow {
                exit: Some(ex),
                entry: en,
                section_score: 0.0,
                evaluation: None,
                recipe: "intro-outro-long-blend".to_string(),
                recipe_candidate_value: json!({
                    "recipe": "intro-outro-long-blend",
                    "score": 0,
                    "timeline": timeline,
                }),
                timeline,
                effective_source_end: Some(audible_end),
                mix_start: Some(mix_start),
                handoff_at: Some(audible_end),
                audible_overlap: Some(intro_dur),
                pre_roll_duration: Some(0.0),
                exit_ratio: Some(round3(mix_start / from.duration.max(1.0))),
                energy_continuity: Some(0.5),
                groove_continuity: Some(0.5),
                tempo_compatibility: Some(0.5),
                local_musical: None,
                score: 0.0,
                selection_score: 0.0,
                rejection_reasons: cadence_reasons.iter().take(4).cloned().collect(),
                rescue_class: String::new(),
                rescue_reason: String::new(),
                route_fallback_used: true,
                technical_failure: false,
                error_code: String::new(),
                cadence_fallback: Some(json!({
                    "mode": "full-outro-intro",
                    "confidence": out.confidence.min(inc.confidence),
                    "reasons": cadence_reasons.iter().take(4).cloned().collect::<Vec<_>>(),
                    "crossfadeDuration": intro_dur,
                    "audibleEnd": audible_end,
                    "containerEnd": from.duration,
                    "bSourceAt": 0,
                })),
                clean_boundary: None,
                policy,
                a_energy: 0.0,
                b_energy: 0.0,
                a_bass: 0.0,
                b_bass: 0.0,
                beat_score: 0.0,
                energy_score: 0.0,
                bass_score: 0.0,
                bpm_score: 0.0,
                outro_completeness: 0.0,
                b_intro_aggression: 0.0,
                style_texture_distance: 0.0,
                relative_tempo_delta: 0.0,
                beat_grid_trusted: false,
                musical_evidence: false,
                musical_compatibility: 0.0,
                harmonic_similarity: 0.0,
                key_compatibility: 0.0,
                melody_similarity: 0.0,
                musical_risks: Vec::new(),
                overlap_class: "medium".to_string(),
                overlap_duration: intro_dur,
            };
            return (chosen, cadence_rejected);
        }
    }

    // 3. 兜底 end-of-track-crossfade
    let chosen = end_of_track_crossfade_window(from, to, &cadence_reasons, tail_evidence);
    (chosen, cadence_rejected)
}

#[allow(clippy::too_many_arguments)]
fn check_rejection_reasons(
    protected_until: f64,
    source_duration: f64,
    exit: &TransitionCandidate,
    entry: &TransitionCandidate,
    rc: &RecipeCandidate,
    relative_tempo_delta: f64,
    from_vocals: &[crate::cuefield::structure::TimeWindow],
    to_vocals: &[crate::cuefield::structure::TimeWindow],
    musical_score: Option<f64>,
    local_musical: Option<&crate::cuefield::structure::LocalMusicalComparison>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let trans_start = rc.window.transition_start.unwrap_or(rc.window.audible_start);
    let mix_start = exit.time + trans_start;
    let handoff_at = exit.time + rc.window.handoff_offset;
    let outgoing_audible_end = if rc.recipe == "clean-boundary-handoff" {
        rc.window
            .a_only_tail_duration
            .map(|tail| mix_start + tail)
            .unwrap_or(handoff_at)
    } else {
        handoff_at
    };
    let lk = landing_kind(entry);
    let anchored = matches!(lk.as_str(), "hook" | "intro" | "drop");

    if mix_start < protected_until {
        reasons.push("mix start precedes protected section".to_string());
    }
    if from_vocals
        .iter()
        .any(|v| v.end > mix_start && v.start < outgoing_audible_end)
    {
        reasons.push("outgoing vocal phrase incomplete".to_string());
    }
    if rc.recipe == "clean-boundary-handoff" {
        let entry_start = entry.play_from.unwrap_or(entry.time);
        if to_vocals
            .iter()
            .any(|v| v.start + LANDING_TOLERANCE_SEC < entry_start && v.end > entry_start)
        {
            reasons.push("incoming vocal phrase already active".to_string());
        }
    }
    if let Some(ms) = musical_score {
        if ms < 0.45 {
            if !matches!(
                exit.kind.to_lowercase().as_str(),
                "release" | "outro" | "natural-tail"
            ) {
                reasons.push("musical mismatch needs release exit".to_string());
            } else if exit.time / source_duration.max(1.0) < 0.72 {
                reasons.push("musical mismatch needs late release".to_string());
            } else if rc.window.audible_overlap > 3.5 {
                reasons.push("musical mismatch needs short overlap".to_string());
            }
        }
    }
    if let Some(lm) = local_musical {
        if lm.confidence >= 0.55 && (lm.score < 0.42 || lm.harmonic_similarity < 0.4) {
            if rc.recipe == "harmonic-double-drop" {
                reasons.push("local musical clash forbids harmonic double drop".to_string());
            } else if rc.window.audible_overlap > 3.5 {
                reasons.push("local musical clash needs short overlap".to_string());
            }
        }
    }
    if anchored && !rc.window.runway_available {
        reasons.push("landing has no runway".to_string());
    }
    if anchored && !rc.window.landing_error.is_finite() {
        reasons.push("landing diagnostic unavailable".to_string());
    } else if anchored && rc.window.landing_error.abs() > LANDING_TOLERANCE_SEC {
        reasons.push("landing error exceeds .05".to_string());
    }
    if lk == "hook"
        && entry.play_from.unwrap_or(entry.time) == entry.landing_at.unwrap_or(entry.time)
        && relative_tempo_delta > 0.15
    {
        reasons.push("direct hook has no compatible runway".to_string());
    }
    if exit.time + rc.window.handoff_offset > source_duration + 0.000001 {
        reasons.push("handoff exceeds source duration".to_string());
    }
    if rc.timeline.is_empty() {
        reasons.push("missing executable timeline".to_string());
    }
    reasons
}

#[allow(dead_code)]
struct WindowPlanResult {
    chosen: PlannedWindow,
    candidates: Vec<Value>,
    rejected: Vec<Value>,
    diagnostics: Value,
    policy: RoutePolicy,
}

#[allow(clippy::too_many_arguments)]
fn choose_transition_window(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    recent_recipes: &[String],
    enable_clean_boundary: bool,
    enable_cadence_fallback: bool,
    boundary_evidence: &[Value],
    tail_evidence: Option<&Value>,
) -> WindowPlanResult {
    let protected_until = from.structure_map.protected_until;
    let source_exit_options = source_exits(from, protected_until);
    let entries = landing_options(to);

    if enable_cadence_fallback {
        let (chosen, cadence_rejected) = choose_cadence_fallback_window(
            from,
            to,
            protected_until,
            &source_exit_options,
            &entries,
            boundary_evidence,
            tail_evidence,
            recent_recipes,
        );
        let policy = chosen.policy.clone();
        let mode = chosen
            .cadence_fallback
            .as_ref()
            .and_then(|cf| cf.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return WindowPlanResult {
            chosen,
            candidates: Vec::new(),
            rejected: Vec::new(),
            diagnostics: json!({
                "protectedUntil": protected_until,
                "sourceExitCount": exit_source_candidates(from).len(),
                "sourceLandingCount": source_landings(to).len(),
                "consideredExitCount": source_exit_options.len(),
                "consideredLandingCount": entries.len(),
                "cadenceRejected": cadence_rejected,
                "cadenceFallbackMode": mode,
            }),
            policy,
        };
    }

    fn reliable_mp(mp: Option<&Value>) -> Option<&Value> {
        let m = mp?;
        if val_f64(m.get("confidence"), 0.0) >= 0.55 && val_f64(m.get("noteCount"), 0.0) >= 12.0 {
            Some(m)
        } else {
            None
        }
    }
    let musical_cmp = match (reliable_mp(from.musical_profile.as_ref()), reliable_mp(to.musical_profile.as_ref())) {
        (Some(f), Some(t)) => Some(compare_musical_profiles(f, t)),
        _ => None,
    };

    let rep_risks: Vec<String> = {
        let mut sorted_exits = source_exit_options.clone();
        sorted_exits.sort_by(|a, b| {
            score_window_exit(b)
                .total_cmp(&score_window_exit(a))
                .then_with(|| a.time.total_cmp(&b.time))
        });
        if let (Some(ex), Some(en)) = (sorted_exits.first(), entries.first()) {
            let (_, _, eval) = score_candidate_pair(ex, en, from, to, score_window_exit(ex), &source_exit_options);
            eval.risks.into_iter().take(4).collect()
        } else {
            Vec::new()
        }
    };

    // positive overlap evidence
    let mut ov_reasons = Vec::new();
    if musical_cmp.as_ref().map(|m| m.score < 0.72).unwrap_or(true) {
        ov_reasons.push("musical-evidence".to_string());
    }
    if musical_cmp.as_ref().map(|m| !m.risks.is_empty()).unwrap_or(false) {
        ov_reasons.push("musical-risk".to_string());
    }
    if rep_risks
        .iter()
        .any(|r| matches!(r.as_str(), "vocal collision" | "vocal overlap" | "style bridge mismatch"))
    {
        ov_reasons.push("relationship-risk".to_string());
    }
    if !(from.grid_step > 0.0 && to.grid_step > 0.0 && from.downbeats.len() >= 4 && to.downbeats.len() >= 4) {
        ov_reasons.push("beat-grid".to_string());
    }
    if !source_exit_options
        .iter()
        .any(|e| e.source != "fallback" && e.confidence >= 0.6 && e.time >= protected_until)
    {
        ov_reasons.push("outgoing-boundary".to_string());
    }
    let to_vocals = &to.structure_map.vocal_windows;
    let vocal_avail = to.structure_map.structure_source == "lyric+beat" && !to_vocals.is_empty();
    let instr_runway = vocal_avail
        && entries.iter().any(|en| {
            let k = en.kind.to_lowercase();
            let pf = en.play_from.unwrap_or(f64::NAN);
            let la = en.landing_at.unwrap_or(f64::NAN);
            en.source != "fallback"
                && en.confidence >= 0.72
                && (k == "intro" || k == "pre-hook")
                && pf < la
                && !to_vocals.iter().any(|w| w.end > pf && w.start < la)
        });
    if !instr_runway {
        ov_reasons.push("instrumental-runway".to_string());
    }
    let overlap_safe = ov_reasons.is_empty();
    ov_reasons.truncate(4);

    let policy = classify_transition_route(
        from,
        to,
        protected_until,
        &source_exit_options,
        &entries,
        &rep_risks,
        enable_clean_boundary,
        overlap_safe,
    );

    let exits = exit_options(from, protected_until, &policy);
    let planned_entries: Vec<TransitionCandidate> = if policy.route == "clean-boundary-handoff" {
        entries
            .iter()
            .map(|c| {
                let pf = c.play_from.unwrap_or(c.time);
                if let Some((start, _)) = active_vocal_window(to_vocals, pf) {
                    let mut out = c.clone();
                    out.play_from = Some(round3(start));
                    out
                } else {
                    c.clone()
                }
            })
            .collect()
    } else {
        entries
    };

    let mut diag = json!({
        "protectedUntil": protected_until,
        "sourceExitCount": exit_source_candidates(from).len(),
        "sourceLandingCount": source_landings(to).len(),
        "consideredExitCount": exits.len(),
        "consideredLandingCount": planned_entries.len(),
        "exitCount": exits.len(),
        "landingCount": planned_entries.len(),
        "recipeCandidatesConsidered": 0,
        "musicalEvidence": musical_cmp.is_some(),
        "musicalCompatibility": musical_cmp.as_ref().map(|m| json!(m.score)).unwrap_or(Value::Null),
        "harmonicSimilarity": musical_cmp.as_ref().map(|m| json!(m.harmonic_similarity)).unwrap_or(Value::Null),
        "keyCompatibility": musical_cmp.as_ref().map(|m| json!(m.key_compatibility)).unwrap_or(Value::Null),
        "melodySimilarity": musical_cmp.as_ref().map(|m| json!(m.melody_similarity)).unwrap_or(Value::Null),
        "musicalRisks": musical_cmp.as_ref().map(|m| m.risks.clone()).unwrap_or_default(),
        "overlapEvidence": overlap_safe,
        "overlapEvidenceReasons": ov_reasons,
    });

    if policy.route == "terminal-rescue" {
        let chosen = terminal_rescue(from, to, protected_until, policy.clone(), &[], &planned_entries);
        return WindowPlanResult {
            chosen,
            candidates: Vec::new(),
            rejected: Vec::new(),
            diagnostics: diag,
            policy,
        };
    }

    let mut candidate_windows = Vec::new();
    let mut rejected = Vec::new();
    let mut considered_count = 0usize;

    for exit in &exits {
        for entry in &planned_entries {
            let (rec_name, raw_sec_score, mut eval) =
                score_candidate_pair(exit, entry, from, to, score_window_exit(exit), &exits);
            if eval.recipe.is_empty() {
                eval.recipe = rec_name;
            }
            let sec_score = clamp01(raw_sec_score);
            let local_musical = compare_local_musical_windows(
                from.musical_profile.as_ref(),
                to.musical_profile.as_ref(),
                exit.time,
                entry.play_from.unwrap_or(entry.landing_at.unwrap_or(f64::NAN)),
            );
            let (recipe_cands, scores, assessment, _) = plan_recipe_candidates(
                from,
                to,
                exit,
                entry,
                &eval,
                &policy.route,
                recent_recipes,
            );
            for rc in recipe_cands {
                let allowed = if !rc.eligible {
                    false
                } else if policy.route == "clean-boundary-handoff" {
                    rc.recipe == "clean-boundary-handoff"
                } else if policy.route == "late-contrast-rise" {
                    matches!(
                        rc.recipe.as_str(),
                        "filtered-pickup" | "echo-out" | "quick-safe-fade" | "safety-long-blend"
                    ) && rc.window.audible_overlap <= 3.5
                } else if policy.route == "late-contrast-release" {
                    rc.window.audible_overlap <= 6.0
                } else {
                    true
                };
                if !allowed {
                    continue;
                }
                considered_count += 1;
                let rej = check_rejection_reasons(
                    protected_until,
                    from.duration,
                    exit,
                    entry,
                    &rc,
                    assessment.tempo.relative_tempo_delta,
                    &from.structure_map.vocal_windows,
                    &to.structure_map.vocal_windows,
                    musical_cmp.as_ref().map(|m| m.score),
                    local_musical.as_ref(),
                );
                let mut w = build_ranked_window(
                    from,
                    to,
                    exit,
                    entry,
                    sec_score,
                    eval.clone(),
                    rc,
                    &scores,
                    &assessment,
                    &policy,
                    musical_cmp.as_ref().map(|m| m.score),
                    local_musical.clone(),
                );
                if !rej.is_empty() {
                    w.rejection_reasons = rej;
                    rejected.push(w);
                } else {
                    candidate_windows.push(w);
                }
            }
        }
    }

    if let Some(obj) = diag.as_object_mut() {
        obj.insert("recipeCandidatesConsidered".to_string(), json!(considered_count));
    }

    candidate_windows.sort_by(|a, b| {
        let imp_a = if a.recipe == "tease-roll-double-drop" { a.selection_score } else { 0.0 };
        let imp_b = if b.recipe == "tease-roll-double-drop" { b.selection_score } else { 0.0 };
        b.score
            .total_cmp(&a.score)
            .then_with(|| imp_b.total_cmp(&imp_a))
            .then_with(|| {
                a.exit
                    .as_ref()
                    .map(|e| e.time)
                    .unwrap_or(0.0)
                    .total_cmp(&b.exit.as_ref().map(|e| e.time).unwrap_or(0.0))
            })
            .then_with(|| {
                a.entry
                    .landing_at
                    .unwrap_or(a.entry.time)
                    .total_cmp(&b.entry.landing_at.unwrap_or(b.entry.time))
            })
    });

    if !candidate_windows.is_empty() {
        let chosen = candidate_windows.remove(0);
        let cands: Vec<Value> = candidate_windows.iter().map(compact_window_summary).collect();
        let rejs: Vec<Value> = rejected.iter().map(compact_window_summary).collect();
        WindowPlanResult {
            chosen,
            candidates: cands,
            rejected: rejs,
            diagnostics: diag,
            policy,
        }
    } else {
        let mut rescue_reasons = policy.reasons.clone();
        rescue_reasons.push("no-valid-window".to_string());
        rescue_reasons.truncate(4);
        let selected_policy = RoutePolicy {
            route: "terminal-rescue".to_string(),
            compatibility_class: "uncertain".to_string(),
            contrast_direction: "unknown".to_string(),
            preferred_exit_range: vec![
                0.88_f64.max(clamp01(protected_until / from.duration.max(1.0))),
                0.96_f64.max(clamp01(protected_until / from.duration.max(1.0))),
            ],
            entry_policy: "start-or-downbeat".to_string(),
            overlap_class: "short".to_string(),
            recipe: "terminal-rescue".to_string(),
            reasons: rescue_reasons,
            metrics: policy.metrics,
        };
        let chosen = terminal_rescue(
            from,
            to,
            protected_until,
            selected_policy.clone(),
            &rejected,
            &planned_entries,
        );
        let rejs: Vec<Value> = rejected.iter().map(compact_window_summary).collect();
        WindowPlanResult {
            chosen,
            candidates: Vec::new(),
            rejected: rejs,
            diagnostics: diag,
            policy: selected_policy,
        }
    }
}

// ============================================================================
// Bridge Planner
// ============================================================================

struct TrustedClimax {
    time: f64,
    kind: String,
    confidence: f64,
}

fn find_trusted_climax(analysis: &NormalizedAnalysis) -> Option<TrustedClimax> {
    let mut cands: Vec<(f64, String, f64)> = Vec::new();
    for s in &analysis.structure_map.sections {
        let k = s.kind.to_lowercase();
        if (k == "hook" || k == "drop") && s.confidence >= 0.72 && s.start.is_finite() {
            cands.push((s.start, k, s.confidence));
        }
    }
    for c in &analysis.structure_map.entry_candidates {
        let k = c
            .landing_type
            .as_deref()
            .unwrap_or(&c.kind)
            .to_lowercase();
        let t = c.time;
        if (k == "hook" || k == "drop") && c.confidence >= 0.72 && t.is_finite() {
            cands.push((t, k, c.confidence));
        }
    }
    cands.sort_by(|a, b| {
        let ta = if a.1 == "hook" { 0 } else { 1 };
        let tb = if b.1 == "hook" { 0 } else { 1 };
        ta.cmp(&tb)
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| a.0.total_cmp(&b.0))
    });
    cands.into_iter().next().map(|(time, kind, confidence)| TrustedClimax {
        time,
        kind,
        confidence,
    })
}

struct BridgePlan {
    template: String,
    bars: usize,
    bpm_from: f64,
    bpm_to: f64,
    climax: TrustedClimax,
    stage_durations: [f64; 3],
    total_duration: f64,
    mix_start: f64,
    handoff_at: f64,
    predicted_score: f64,
    improvement: f64,
    lyric_link_score: f64,
    reasons: Vec<String>,
    fallback_timeline: Vec<Value>,
    timeline: Vec<Value>,
}

fn usable_grid(a: &NormalizedAnalysis) -> bool {
    (40.0..=240.0).contains(&a.bpm) && a.grid_step > 0.0 && a.downbeats.len() >= 4
}

fn plan_bridge(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    direct: &PlannedWindow,
    lyric_link_score: f64,
) -> Option<BridgePlan> {
    if !usable_grid(from) || !usable_grid(to) {
        return None;
    }
    let climax = find_trusted_climax(to)?;
    let bpm_from = from.bpm;
    let bpm_to = to.bpm;
    let avg_bpm = ((bpm_from + bpm_to) / 2.0).max(40.0);
    let dur_for = |bars: usize| round3(bars as f64 * 4.0 * 60.0 / avg_bpm);
    let available = (from.duration - from.structure_map.protected_until).max(0.0) + climax.time.max(0.0);
    if available < dur_for(4) {
        return None;
    }
    let route = direct.policy.route.as_str();
    let compat = direct.policy.compatibility_class.as_str();
    let bars = if route == "terminal-rescue" {
        4
    } else if compat == "contrast" && available >= dur_for(16) {
        16
    } else if available >= dur_for(8) {
        8
    } else {
        4
    };
    let total_dur = dur_for(bars);
    let s1 = round3(total_dur * 0.25);
    let s2 = round3(total_dur * 0.5);
    let s3 = round3(total_dur - s1 - s2);
    if climax.time < s3 {
        return None;
    }
    let valid_exit = |c: &TransitionCandidate| {
        c.time.is_finite()
            && c.time >= from.structure_map.protected_until
            && from.duration - c.time >= 1.5_f64.min(s1 * 0.6)
    };
    let exit = direct
        .exit
        .as_ref()
        .filter(|c| valid_exit(c))
        .cloned()
        .or_else(|| {
            let mut cands: Vec<TransitionCandidate> = from
                .structure_map
                .exit_candidates
                .iter()
                .filter(|c| valid_exit(c))
                .cloned()
                .collect();
            cands.sort_by(|a, b| a.time.total_cmp(&b.time));
            cands.into_iter().next()
        })?;

    let contrast_bonus = if compat == "contrast" { 0.12 } else { 0.0 };
    let score = round3(clamp01(
        0.52 + climax.confidence * 0.18 + clamp01(lyric_link_score) * 0.15 + contrast_bonus + 0.05,
    ));
    let direct_score = direct
        .evaluation
        .as_ref()
        .map(|e| e.score)
        .unwrap_or(direct.score);
    let improvement = round3(score - clamp01(direct_score));
    let strong_lyric = lyric_link_score >= 0.65;
    let contrast_route = compat == "contrast" || route == "terminal-rescue" || route.contains("contrast");
    if improvement < 0.12 && !contrast_route && !strong_lyric {
        return None;
    }

    let template = if route == "terminal-rescue" {
        "echo-break"
    } else if strong_lyric {
        "loop-rise"
    } else if direct.policy.contrast_direction == "rising" || route.contains("rise") {
        "drum-build"
    } else {
        "impact-drop"
    };

    let b_start_t = round3(s1 + s2);
    let b_play_at = round3((climax.time - s3).max(0.0));
    let fallback_timeline = direct.timeline.clone();
    let bridge_payload = json!({
        "template": template,
        "bars": bars,
        "bpmFrom": round_digits(bpm_from, 2),
        "bpmTo": round_digits(bpm_to, 2),
        "stageDurations": [s1, s2, s3],
    });
    let timeline = vec![
        json!({ "t": 0, "deck": "A", "op": "bridge", "duration": (total_dur * 1000.0).round() as i64, "bridge": bridge_payload, "fallbackTimeline": fallback_timeline }),
        json!({ "t": 0, "deck": "A", "op": "bass", "value": 0.18, "duration": (s1 * 800.0).round() as i64 }),
        json!({ "t": 0, "deck": "A", "op": "filter", "type": "highpass", "value": 1100, "duration": (s1 * 900.0).round() as i64 }),
        json!({ "t": 0, "deck": "A", "op": "volume", "value": 0, "duration": (s1 * 1000.0).round() as i64, "curve": "equal-power-out" }),
        json!({ "t": b_start_t, "deck": "B", "op": "play", "at": b_play_at, "volume": 0 }),
        json!({ "t": b_start_t, "deck": "B", "op": "filter", "type": "highpass", "value": 1200, "duration": 0 }),
        json!({ "t": b_start_t, "deck": "B", "op": "bass", "value": 0.2, "duration": 0 }),
        json!({ "t": b_start_t, "deck": "B", "op": "volume", "value": 1, "duration": (s3 * 1000.0).round() as i64, "curve": "equal-power-in" }),
        json!({ "t": b_start_t, "deck": "B", "op": "filter", "type": "none", "value": 0, "duration": (s3 * 1000.0).round() as i64 }),
        json!({ "t": b_start_t, "deck": "B", "op": "bass", "value": 1, "duration": (s3 * 1000.0).round() as i64 }),
        json!({ "t": total_dur, "deck": "B", "op": "handoff" }),
    ];
    let mut reasons = vec![
        if contrast_route { "contrast-route" } else { "score-improvement" }.to_string(),
        format!("trusted-{}", climax.kind),
    ];
    if strong_lyric {
        reasons.push("lyric-link".to_string());
    }

    Some(BridgePlan {
        template: template.to_string(),
        bars,
        bpm_from: round_digits(bpm_from, 2),
        bpm_to: round_digits(bpm_to, 2),
        climax,
        stage_durations: [s1, s2, s3],
        total_duration: total_dur,
        mix_start: round3(exit.time),
        handoff_at: round3(exit.time + total_dur),
        predicted_score: score,
        improvement,
        lyric_link_score: round3(lyric_link_score),
        reasons,
        fallback_timeline,
        timeline,
    })
}

// ============================================================================
// Transition Artifact & SHA-256
// ============================================================================

fn compact_action(value: &Value, depth: usize) -> Value {
    let mut out = Map::new();
    if let Some(obj) = value.as_object() {
        for key in ACTION_FIELDS {
            if key == "deck" {
                if let Some(d) = obj.get("deck").and_then(|v| v.as_str()) {
                    if d == "A" || d == "B" {
                        out.insert("deck".to_string(), Value::String(d.to_string()));
                    }
                }
                continue;
            }
            if let Some(v) = obj.get(key) {
                match v {
                    Value::Null | Value::Bool(_) => {
                        out.insert(key.to_string(), v.clone());
                    }
                    Value::Number(n) if n.as_f64().map(|x| x.is_finite()).unwrap_or(false) => {
                        out.insert(key.to_string(), v.clone());
                    }
                    Value::String(s) => {
                        let truncated: String = s.chars().take(160).collect();
                        out.insert(key.to_string(), Value::String(truncated));
                    }
                    _ => {}
                }
            }
        }
        if depth < 2 {
            if let Some(fb) = obj.get("fallbackTimeline").and_then(|v| v.as_array()) {
                let compacted: Vec<Value> = fb.iter().take(64).map(|a| compact_action(a, depth + 1)).collect();
                out.insert("fallbackTimeline".to_string(), Value::Array(compacted));
            }
        }
    }
    Value::Object(out)
}

fn stable_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => if *b { "true".to_string() } else { "false".to_string() },
        Value::Number(n) => serde_json::to_string(n).unwrap_or_else(|_| "null".to_string()),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let parts: Vec<String> = sorted
                .into_iter()
                .map(|(k, v)| {
                    let ks = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{ks}:{}", stable_json(v))
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// 根据规范化稳定 JSON 负载计算 64 位小写 Hex SHA-256 `artifactId`。
pub fn compute_transition_artifact_id(artifact: &Value) -> String {
    let mut payload = artifact.as_object().cloned().unwrap_or_default();
    payload.remove("artifactId");
    let canonical = stable_json(&Value::Object(payload));
    let digest = Sha256::digest(canonical.as_bytes());
    hex::encode(digest)
}

/// 校验 transition artifact 的 `artifactId` SHA-256 完整性。
pub fn verify_transition_artifact(artifact: &Value) -> bool {
    let Some(actual) = artifact.get("artifactId").and_then(|v| v.as_str()) else {
        return false;
    };
    if actual.len() != 64 || !actual.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return false;
    }
    compute_transition_artifact_id(artifact) == actual
}

fn build_transition_artifact(
    from: &NormalizedAnalysis,
    to: &NormalizedAnalysis,
    chosen: &Value,
    version: Option<&Value>,
) -> Value {
    let mut map = Map::new();
    map.insert("schema".to_string(), json!("cuefield-transition-artifact-v1"));
    map.insert(
        "from".to_string(),
        json!({
            "key": compact_string(&from.track.id, 120),
            "title": compact_string(&from.track.title, 160),
            "artist": compact_string(&from.track.artist, 160),
            "duration": from.track.duration,
        }),
    );
    map.insert(
        "to".to_string(),
        json!({
            "key": compact_string(&to.track.id, 120),
            "title": compact_string(&to.track.title, 160),
            "artist": compact_string(&to.track.artist, 160),
            "duration": to.track.duration,
        }),
    );
    let recipe = chosen
        .get("transitionRecipe")
        .or_else(|| chosen.get("recipe"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    map.insert("recipe".to_string(), json!(compact_string(recipe, 80)));

    let pol = chosen.get("policy").and_then(|v| v.as_object());
    let pol_reasons: Vec<String> = pol
        .and_then(|p| p.get("reasons"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| compact_string(s, 96)))
                .filter(|s| !s.is_empty())
                .take(4)
                .collect()
        })
        .unwrap_or_default();
    map.insert(
        "policy".to_string(),
        json!({
            "route": compact_string(pol.and_then(|p| p.get("route")).and_then(|v| v.as_str()).unwrap_or(""), 40),
            "level": compact_string(
                pol.and_then(|p| p.get("level").or_else(|| p.get("compatibilityClass"))).and_then(|v| v.as_str()).unwrap_or(""),
                40
            ),
            "reasons": pol_reasons,
        }),
    );

    map.insert("protectedUntil".to_string(), chosen.get("protectedUntil").cloned().unwrap_or(Value::Null));
    map.insert("mixStart".to_string(), chosen.get("mixStart").cloned().unwrap_or(Value::Null));
    map.insert("handoffAt".to_string(), chosen.get("handoffAt").cloned().unwrap_or(Value::Null));
    let entry_time = chosen
        .get("entryTime")
        .filter(|v| !v.is_null())
        .or_else(|| chosen.get("entry").and_then(|e| e.get("time")))
        .cloned()
        .unwrap_or(Value::Null);
    map.insert("entryTime".to_string(), entry_time);

    let timeline: Vec<Value> = chosen
        .get("timeline")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().take(128).map(|a| compact_action(a, 0)).collect())
        .unwrap_or_default();
    map.insert("timeline".to_string(), Value::Array(timeline));
    map.insert("version".to_string(), compact_version(version));

    if let Some(cb) = chosen.get("cleanBoundary").filter(|v| v.is_object()) {
        map.insert("cleanBoundary".to_string(), cb.clone());
    }
    if let Some(cf) = chosen.get("cadenceFallback").filter(|v| v.is_object()) {
        map.insert("cadenceFallback".to_string(), cf.clone());
    }

    let payload = Value::Object(map.clone());
    let artifact_id = compute_transition_artifact_id(&payload);
    map.insert("artifactId".to_string(), Value::String(artifact_id));
    Value::Object(map)
}

// ============================================================================
// Public Entrypoint: plan_cuefield_transition_from_cache
// ============================================================================

fn cached_edge_evidence(entry: &Value) -> Option<(f64, f64, f64, f64)> {
    let map = entry.get("map")?;
    let source = map.get("edgeEvidence")?.as_object()?;
    let duration = opt_f64(map.get("duration"))?;
    let audible_start = opt_f64(source.get("audibleStart"))?;
    let audible_end = opt_f64(source.get("audibleEnd"))?;
    let container_end = opt_f64(source.get("containerEnd"))?;
    let confidence = opt_f64(source.get("confidence"))?;
    if duration <= 0.0
        || !(0.0..=5.05).contains(&audible_start)
        || audible_end < audible_start
        || container_end <= 0.0
        || audible_end > container_end + 0.05
        || (container_end - duration).abs() > 0.5
        || !(0.5..=1.0).contains(&confidence)
    {
        return None;
    }
    Some((
        audible_start,
        audible_end.min(container_end).min(duration),
        container_end.min(duration),
        confidence,
    ))
}

fn build_live_tail_evidence(from_entry: &Value, to_entry: &Value) -> Option<Value> {
    let (_, from_end, from_container, from_conf) = cached_edge_evidence(from_entry)?;
    let (to_start, _, _, to_conf) = cached_edge_evidence(to_entry)?;
    Some(json!({
        "audibleEnd": from_end,
        "containerEnd": from_container,
        "toAudibleStart": to_start,
        "requestedDuration": 1.6,
        "fromVocalState": "unknown",
        "toVocalState": "unknown",
        "confidence": round3(from_conf.min(to_conf)),
    }))
}

fn trusted_executable_window_plan(w: &PlannedWindow) -> bool {
    let tier = w.evaluation.as_ref().map(|e| e.tier.as_str()).unwrap_or("");
    let trusted_tier = matches!(tier, "magic" | "usable" | "usable_but_not_magic");
    !w.technical_failure
        && !w.recipe.is_empty()
        && w.recipe != "technical-failure"
        && w.recipe != "honest-start-fallback"
        && trusted_tier
        && w.timeline
            .iter()
            .any(|a| a.get("op").and_then(|v| v.as_str()) == Some("handoff"))
}

fn valid_live_end_crossfade_window(w: &PlannedWindow) -> bool {
    let plays: Vec<&Value> = w
        .timeline
        .iter()
        .filter(|a| {
            a.get("deck").and_then(|v| v.as_str()) == Some("B")
                && a.get("op").and_then(|v| v.as_str()) == Some("play")
        })
        .collect();
    let dur = w
        .cadence_fallback
        .as_ref()
        .and_then(|cf| opt_f64(cf.get("crossfadeDuration")));
    !w.technical_failure
        && w.recipe == "end-of-track-crossfade"
        && plays.len() == 1
        && opt_f64(plays[0].get("at")) == Some(0.0)
        && dur.map(|d| d > 0.0).unwrap_or(false)
        && w.mix_start.is_some()
        && w.handoff_at.is_some()
        && w.handoff_at > w.mix_start
}

fn normalize_boundary_evidence(items: &[Value]) -> Vec<Value> {
    const ALLOWED_SOURCES: [&str; 6] = [
        "audio-envelope",
        "spectral-change",
        "beat-grid",
        "lyric",
        "section",
        "stem",
    ];
    const NUMERIC_FIELDS: [&str; 11] = [
        "time",
        "audioBoundaryDistance",
        "barBoundaryDistance",
        "levelBeforeDb",
        "levelAfterDb",
        "levelDropDb",
        "spectralChange",
        "sustainRatio",
        "silenceAfterMs",
        "silenceAfterDb",
        "confidence",
    ];
    items
        .iter()
        .filter(|item| item.is_object())
        .take(16)
        .map(|item| {
            let mut m = Map::new();
            for field in NUMERIC_FIELDS {
                if let Some(n) = opt_f64(item.get(field)) {
                    m.insert(field.to_string(), json!(n));
                }
            }
            m.insert("vocalState".to_string(), Value::String(normalized_vocal_state(item.get("vocalState"))));
            let mut seen = HashSet::new();
            let mut sources = Vec::new();
            if let Some(arr) = item.get("sources").and_then(|v| v.as_array()) {
                for s in arr {
                    if let Some(raw) = s.as_str() {
                        let norm = raw.trim().to_lowercase();
                        if ALLOWED_SOURCES.contains(&norm.as_str()) && seen.insert(norm.clone()) {
                            sources.push(Value::String(norm));
                            if sources.len() >= 5 {
                                break;
                            }
                        }
                    }
                }
            }
            m.insert("sources".to_string(), Value::Array(sources));
            Value::Object(m)
        })
        .collect()
}

fn normalize_tail_evidence(value: Option<&Value>) -> Value {
    let mut m = Map::new();
    if let Some(src) = value.filter(|v| v.is_object()) {
        for field in [
            "audibleEnd",
            "containerEnd",
            "toAudibleStart",
            "requestedDuration",
            "confidence",
        ] {
            if let Some(n) = opt_f64(src.get(field)) {
                m.insert(field.to_string(), json!(n));
            }
        }
        m.insert("fromVocalState".to_string(), Value::String(normalized_vocal_state(src.get("fromVocalState"))));
        m.insert("toVocalState".to_string(), Value::String(normalized_vocal_state(src.get("toVocalState"))));
    } else {
        m.insert("fromVocalState".to_string(), Value::String("unknown".to_string()));
        m.insert("toVocalState".to_string(), Value::String("unknown".to_string()));
    }
    Value::Object(m)
}

/// 基于本地节拍图谱缓存规划双曲目 AutoMix 过渡方案、时间轴动作序列与 SHA-256 `artifactId`。
pub fn plan_cuefield_transition_from_cache<F>(
    req: &TransitionRequest<'_>,
    mut read_cache_fn: F,
) -> Result<Value, String>
where
    F: FnMut(&str) -> Option<Value>,
{
    let from_key = req.from_key.trim();
    let to_key = req.to_key.trim();
    if from_key.is_empty() || to_key.is_empty() {
        return Err("CUEFIELD_CACHE_KEYS_REQUIRED".to_string());
    }

    let from_entry = read_cache_fn(from_key)
        .filter(|e| e.get("map").is_some())
        .ok_or_else(|| format!("BEATMAP_CACHE_MISS:{from_key}"))?;
    let to_entry = read_cache_fn(to_key)
        .filter(|e| e.get("map").is_some())
        .ok_or_else(|| format!("BEATMAP_CACHE_MISS:{to_key}"))?;

    let from_lrc_lines: Vec<LrcLine> = req.from_lrc.map(parse_lrc).unwrap_or_default();
    let to_lrc_lines: Vec<LrcLine> = req.to_lrc.map(parse_lrc).unwrap_or_default();

    let mut from = analyze_cache_entry(&from_entry, from_key, &from_lrc_lines);
    let to = analyze_cache_entry(&to_entry, to_key, &to_lrc_lines);

    let duration = from.duration.max(0.0);
    let structural_floor = resolve_listening_floor(&from.structure_map, duration);
    let listen_floor = match req.minimum_listen_until.filter(|v| v.is_finite()) {
        Some(requested) => structural_floor.max(requested.clamp(0.0, duration)),
        None => structural_floor,
    };
    from.structure_map.protected_until = listen_floor;

    let recent_recipes: Vec<String> = req
        .recent_recipes
        .iter()
        .map(|s| compact_string(s, 80))
        .filter(|s| !s.is_empty())
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let cadence_enabled = req.enable_cadence_fallback;
    let boundary_ev = if cadence_enabled {
        normalize_boundary_evidence(&req.boundary_evidence)
    } else {
        Vec::new()
    };
    let tail_ev = if cadence_enabled {
        Some(normalize_tail_evidence(req.tail_evidence.as_ref()))
    } else {
        None
    };

    let mut window_plan = choose_transition_window(
        &from,
        &to,
        &recent_recipes,
        req.enable_clean_boundary,
        cadence_enabled,
        &boundary_ev,
        tail_ev.as_ref(),
    );

    let mut live_end_diag: Option<Value> = None;
    if req.enable_live_end_crossfade_fallback && !cadence_enabled {
        if trusted_executable_window_plan(&window_plan.chosen) {
            live_end_diag = Some(json!({
                "mode": "preserved-current-plan",
                "duration": Value::Null,
                "preRollDuration": Value::Null,
                "evidenceConfidence": Value::Null,
                "failureCode": "",
            }));
        } else if let Some(live_tail) = build_live_tail_evidence(&from_entry, &to_entry) {
            let fallback_plan = choose_transition_window(
                &from,
                &to,
                &recent_recipes,
                false,
                true,
                &[],
                Some(&live_tail),
            );
            if valid_live_end_crossfade_window(&fallback_plan.chosen) {
                let dur = fallback_plan
                    .chosen
                    .cadence_fallback
                    .as_ref()
                    .and_then(|cf| cf.get("crossfadeDuration"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let pre = fallback_plan.chosen.pre_roll_duration;
                let conf = live_tail.get("confidence").cloned().unwrap_or(Value::Null);
                window_plan = fallback_plan;
                live_end_diag = Some(json!({
                    "mode": "end-of-track-crossfade",
                    "duration": dur,
                    "preRollDuration": pre,
                    "evidenceConfidence": conf,
                    "failureCode": "",
                }));
            } else {
                live_end_diag = Some(json!({
                    "mode": "",
                    "duration": Value::Null,
                    "preRollDuration": Value::Null,
                    "evidenceConfidence": live_tail.get("confidence").cloned().unwrap_or(Value::Null),
                    "failureCode": "UNEXPECTED_FALLBACK_RECIPE",
                }));
            }
        } else {
            live_end_diag = Some(json!({
                "mode": "",
                "duration": Value::Null,
                "preRollDuration": Value::Null,
                "evidenceConfidence": Value::Null,
                "failureCode": "EDGE_EVIDENCE_UNAVAILABLE",
            }));
        }
    }

    let mut chosen_w = window_plan.chosen;
    let exit_time_for_lyric = chosen_w
        .exit
        .as_ref()
        .map(|e| e.time)
        .unwrap_or(f64::INFINITY);
    let climax = find_trusted_climax(&to);
    let lyric_link = score_lyric_link(
        &from_lrc_lines,
        &to_lrc_lines,
        exit_time_for_lyric,
        climax.as_ref().map(|c| c.time).unwrap_or(0.0),
        0.0,
    );

    let synthetic_bridge_enabled = req.synthetic_bridge_enabled && !cadence_enabled;
    let bridge = if !synthetic_bridge_enabled || chosen_w.technical_failure {
        None
    } else {
        plan_bridge(&from, &to, &chosen_w, lyric_link.score)
    };

    let direct_recipe_for_bridge = chosen_w.recipe.clone();
    let direct_score_for_bridge = chosen_w
        .evaluation
        .as_ref()
        .map(|e| e.score)
        .unwrap_or(chosen_w.score);

    let bridge_plan_val = if let Some(ref b) = bridge {
        let s3 = b.stage_durations[2];
        let entry_t = b.climax.time;
        chosen_w.recipe = "synthetic-bridge".to_string();
        chosen_w.score = b.predicted_score;
        chosen_w.evaluation = Some(PairEvaluation {
            recipe: "synthetic-bridge".to_string(),
            score: b.predicted_score,
            tier: if b.predicted_score >= 0.84 { "magic" } else { "usable" }.to_string(),
            lyric_handoff: 0.0,
            reasons: Vec::new(),
            risks: Vec::new(),
        });
        if let Some(ref mut ex) = chosen_w.exit {
            ex.role = "exit".to_string();
            ex.source = "structure".to_string();
            ex.time = b.mix_start;
        }
        chosen_w.entry = TransitionCandidate {
            kind: b.climax.kind.clone(),
            role: "entry".to_string(),
            source: "bridge".to_string(),
            time: entry_t,
            confidence: b.climax.confidence,
            energy_before: 0.0,
            energy_after: 0.0,
            vocal_density: 0.0,
            low_density: 0.0,
            beat_stability: 0.0,
            text: None,
            repeats: None,
            resolves_to: None,
            play_from: Some((entry_t - s3).max(0.0)),
            landing_at: Some(entry_t),
            landing_type: Some(b.climax.kind.clone()),
            exit_ratio: None,
            late_penalty: None,
            original_type: None,
            repeated_line_count: None,
            repeated_block_count: None,
            energy_lift: None,
            sustained_energy: None,
            evidence: None,
        };
        chosen_w.timeline = b.timeline.clone();
        chosen_w.recipe_candidate_value = json!({
            "recipe": "synthetic-bridge",
            "fallbackTimeline": b.fallback_timeline,
        });
        chosen_w.mix_start = Some(b.mix_start);
        chosen_w.handoff_at = Some(b.handoff_at);
        chosen_w.audible_overlap = Some(0.0);
        chosen_w.pre_roll_duration = Some(s3);
        chosen_w.exit_ratio = Some(if from.duration > 0.0 {
            b.mix_start / from.duration
        } else {
            0.0
        });
        chosen_w.clean_boundary = None;
        Some(json!({
            "template": b.template,
            "bars": b.bars,
            "bpmFrom": b.bpm_from,
            "bpmTo": b.bpm_to,
            "climax": {
                "time": b.climax.time,
                "type": b.climax.kind,
                "confidence": b.climax.confidence,
            },
            "stageDurations": b.stage_durations,
            "totalDuration": b.total_duration,
            "predictedScore": b.predicted_score,
            "improvement": b.improvement,
            "lyricLinkScore": b.lyric_link_score,
            "reasons": b.reasons,
        }))
    } else {
        None
    };

    let structure_source = if from.structure_map.structure_source == "lyric+beat"
        && to.structure_map.structure_source == "lyric+beat"
    {
        "lyric+beat"
    } else {
        "beat-only"
    };

    let section_recipe = chosen_w
        .evaluation
        .as_ref()
        .map(|e| e.recipe.clone())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| {
            if chosen_w.recipe.is_empty() {
                "honest-start-fallback".to_string()
            } else {
                chosen_w.recipe.clone()
            }
        });
    let evaluation_val = chosen_w
        .evaluation
        .as_ref()
        .map(|e| json!(e))
        .unwrap_or_else(|| {
            json!({
                "score": chosen_w.score,
                "tier": "weak",
                "risks": chosen_w.rejection_reasons,
            })
        });

    let mut chosen_obj = Map::new();
    chosen_obj.insert("recipe".to_string(), json!(section_recipe));
    chosen_obj.insert("score".to_string(), json!(chosen_w.score));
    chosen_obj.insert("evaluation".to_string(), evaluation_val);
    chosen_obj.insert(
        "exit".to_string(),
        chosen_w
            .exit
            .as_ref()
            .map(compact_transition_point)
            .unwrap_or(Value::Null),
    );
    chosen_obj.insert("entry".to_string(), compact_transition_point(&chosen_w.entry));
    chosen_obj.insert("protectedUntil".to_string(), json!(from.structure_map.protected_until));
    chosen_obj.insert("transitionRecipe".to_string(), json!(chosen_w.recipe));
    chosen_obj.insert("timeline".to_string(), Value::Array(chosen_w.timeline.clone()));
    chosen_obj.insert("recipeCandidate".to_string(), chosen_w.recipe_candidate_value.clone());
    chosen_obj.insert(
        "effectiveSourceEnd".to_string(),
        chosen_w.effective_source_end.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "mixStart".to_string(),
        chosen_w.mix_start.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "handoffAt".to_string(),
        chosen_w.handoff_at.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "audibleOverlap".to_string(),
        chosen_w.audible_overlap.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "preRollDuration".to_string(),
        chosen_w.pre_roll_duration.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "exitRatio".to_string(),
        chosen_w.exit_ratio.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "energyContinuity".to_string(),
        chosen_w.energy_continuity.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "grooveContinuity".to_string(),
        chosen_w.groove_continuity.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "tempoCompatibility".to_string(),
        chosen_w.tempo_compatibility.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    chosen_obj.insert(
        "rejectionReasons".to_string(),
        json!(chosen_w.rejection_reasons),
    );
    chosen_obj.insert("policy".to_string(), chosen_w.policy.to_value());
    chosen_obj.insert(
        "rescueClass".to_string(),
        json!(compact_string(&chosen_w.rescue_class, 1)),
    );
    chosen_obj.insert(
        "rescueReason".to_string(),
        json!(compact_string(&chosen_w.rescue_reason, 40)),
    );
    chosen_obj.insert("routeFallbackUsed".to_string(), json!(chosen_w.route_fallback_used));
    chosen_obj.insert("technicalFailure".to_string(), json!(chosen_w.technical_failure));
    chosen_obj.insert(
        "errorCode".to_string(),
        json!(compact_string(&chosen_w.error_code, 80)),
    );
    if let Some(ref cb) = chosen_w.clean_boundary {
        chosen_obj.insert("cleanBoundary".to_string(), cb.clone());
    }
    if let Some(ref cf) = chosen_w.cadence_fallback {
        chosen_obj.insert("cadenceFallback".to_string(), cf.clone());
    }
    if let Some(bp) = bridge_plan_val {
        chosen_obj.insert("bridgePlan".to_string(), bp);
        chosen_obj.insert(
            "directTransition".to_string(),
            json!({
                "recipe": direct_recipe_for_bridge,
                "score": direct_score_for_bridge,
            }),
        );
    }

    let chosen_val = Value::Object(chosen_obj);

    let from_hook = from.structure_map.sections.iter().find(|s| {
        from.structure_map.structure_source == "lyric+beat"
            && s.kind.eq_ignore_ascii_case("hook")
            && s.confidence >= 0.65
    });
    let raw_landing_type = chosen_w
        .entry
        .landing_type
        .as_deref()
        .unwrap_or(&chosen_w.entry.kind)
        .to_lowercase();
    let entry_type_diag = if to.structure_map.structure_source != "lyric+beat" && raw_landing_type == "hook" {
        if chosen_w.entry.source == "fallback" {
            "start".to_string()
        } else {
            "drop".to_string()
        }
    } else {
        raw_landing_type
    };
    let struct_conf = from
        .structure_map
        .structure_confidence
        .min(to.structure_map.structure_confidence);

    let mut diag_obj = window_plan.diagnostics.as_object().cloned().unwrap_or_default();
    diag_obj.insert("structureSource".to_string(), json!(structure_source));
    diag_obj.insert(
        "shadow".to_string(),
        json!({
            "schema": "cuefield-shadow-v1",
            "from": from.shadow,
            "to": to.shadow,
        }),
    );
    diag_obj.insert("structureConfidence".to_string(), json!(struct_conf));
    diag_obj.insert("protectedUntil".to_string(), json!(from.structure_map.protected_until));
    if let Some(hook) = from_hook {
        diag_obj.insert("firstHookStart".to_string(), json!(hook.start));
        diag_obj.insert("firstHookEnd".to_string(), json!(hook.end));
        diag_obj.insert("hookConfidence".to_string(), json!(hook.confidence));
        if let Some(ref ev) = hook.evidence {
            diag_obj.insert("hookEvidence".to_string(), json!(ev));
        } else {
            diag_obj.insert("hookEvidence".to_string(), json!([]));
        }
    } else {
        diag_obj.insert("firstHookStart".to_string(), Value::Null);
        diag_obj.insert("firstHookEnd".to_string(), Value::Null);
        diag_obj.insert("hookConfidence".to_string(), Value::Null);
        diag_obj.insert("hookEvidence".to_string(), json!([]));
    }
    diag_obj.insert(
        "exitType".to_string(),
        json!(chosen_w.exit.as_ref().map(|e| e.kind.as_str()).unwrap_or("")),
    );
    diag_obj.insert(
        "exitConfidence".to_string(),
        chosen_w.exit.as_ref().map(|e| json!(e.confidence)).unwrap_or(Value::Null),
    );
    diag_obj.insert("exitRatio".to_string(), chosen_w.exit_ratio.map(|v| json!(v)).unwrap_or(Value::Null));
    diag_obj.insert("entryType".to_string(), json!(entry_type_diag));
    diag_obj.insert("entrySource".to_string(), json!(chosen_w.entry.source));
    diag_obj.insert("entryConfidence".to_string(), json!(chosen_w.entry.confidence));
    diag_obj.insert(
        "landingAt".to_string(),
        chosen_w.entry.landing_at.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    diag_obj.insert(
        "mixStart".to_string(),
        chosen_w.mix_start.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    diag_obj.insert(
        "handoffAt".to_string(),
        chosen_w.handoff_at.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    diag_obj.insert(
        "audibleOverlap".to_string(),
        chosen_w.audible_overlap.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    diag_obj.insert(
        "preRollDuration".to_string(),
        chosen_w.pre_roll_duration.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    diag_obj.insert("bpmA".to_string(), json!(from.bpm));
    diag_obj.insert("bpmB".to_string(), json!(to.bpm));
    diag_obj.insert("relativeTempoDelta".to_string(), json!(chosen_w.relative_tempo_delta));
    diag_obj.insert("beatGridTrusted".to_string(), json!(chosen_w.beat_grid_trusted));
    diag_obj.insert("overlapClass".to_string(), json!(chosen_w.overlap_class));
    diag_obj.insert("overlapDuration".to_string(), json!(chosen_w.overlap_duration));
    diag_obj.insert("route".to_string(), json!(chosen_w.policy.route));
    diag_obj.insert("compatibilityClass".to_string(), json!(chosen_w.policy.compatibility_class));
    diag_obj.insert("contrastDirection".to_string(), json!(chosen_w.policy.contrast_direction));
    diag_obj.insert("preferredExitRange".to_string(), json!(chosen_w.policy.preferred_exit_range));
    diag_obj.insert("routeReasons".to_string(), json!(chosen_w.policy.reasons));
    diag_obj.insert("routeFallbackUsed".to_string(), json!(chosen_w.route_fallback_used));
    diag_obj.insert("terminalRescueClass".to_string(), json!(compact_string(&chosen_w.rescue_class, 1)));
    diag_obj.insert("terminalRescueReason".to_string(), json!(compact_string(&chosen_w.rescue_reason, 40)));
    diag_obj.insert("technicalFailure".to_string(), json!(chosen_w.technical_failure));
    diag_obj.insert("errorCode".to_string(), json!(compact_string(&chosen_w.error_code, 80)));
    diag_obj.insert("exitCandidateCount".to_string(), json!(from.structure_map.exit_candidates.len()));
    diag_obj.insert("entryCandidateCount".to_string(), json!(to.structure_map.entry_candidates.len()));
    diag_obj.insert("lyricLinkScore".to_string(), json!(lyric_link.score));
    diag_obj.insert("lyricLinkReasons".to_string(), json!(lyric_link.reasons.iter().take(4).cloned().collect::<Vec<_>>()));
    diag_obj.insert("bridgeSelected".to_string(), json!(bridge.is_some()));
    diag_obj.insert("syntheticBridgeEnabled".to_string(), json!(synthetic_bridge_enabled));
    diag_obj.insert("bridgeTemplate".to_string(), json!(bridge.as_ref().map(|b| b.template.as_str()).unwrap_or("")));
    diag_obj.insert("bridgeBars".to_string(), bridge.as_ref().map(|b| json!(b.bars)).unwrap_or(Value::Null));
    diag_obj.insert("cadenceFallback".to_string(), chosen_w.cadence_fallback.clone().unwrap_or(Value::Null));
    if let Some(led) = live_end_diag {
        diag_obj.insert("liveEndCrossfade".to_string(), led);
    }

    let artifact = build_transition_artifact(&from, &to, &chosen_val, req.version.as_ref());
    let mut response = Map::new();
    response.insert("ok".to_string(), json!(!chosen_w.technical_failure));
    if chosen_w.technical_failure {
        let err = if chosen_w.error_code.is_empty() {
            "CUEFIELD_TECHNICAL_FAILURE".to_string()
        } else {
            chosen_w.error_code.clone()
        };
        response.insert("error".to_string(), json!(err));
    }
    response.insert(
        "from".to_string(),
        json!({
            "track": {
                "id": compact_string(&from.track.id, 120),
                "title": compact_string(&from.track.title, 160),
                "artist": compact_string(&from.track.artist, 160),
                "duration": from.track.duration,
            },
            "structureMap": compact_structure_map(&from.structure_map),
        }),
    );
    response.insert(
        "to".to_string(),
        json!({
            "track": {
                "id": compact_string(&to.track.id, 120),
                "title": compact_string(&to.track.title, 160),
                "artist": compact_string(&to.track.artist, 160),
                "duration": to.track.duration,
            },
            "structureMap": compact_structure_map(&to.structure_map),
        }),
    );
    response.insert("chosen".to_string(), chosen_val);
    response.insert("artifact".to_string(), artifact);
    response.insert("candidates".to_string(), Value::Array(window_plan.candidates));
    response.insert("rejected".to_string(), Value::Array(window_plan.rejected));
    response.insert("diagnostics".to_string(), Value::Object(diag_obj));

    Ok(Value::Object(response))
}
