use chrono::{SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

fn round3(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn opt_f64(value: Option<&Value>) -> Option<f64> {
    match value {
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

fn opt_round3(value: Option<&Value>) -> Value {
    match opt_f64(value) {
        Some(n) => json!(round3(n)),
        None => Value::Null,
    }
}

fn compact_string(value: Option<&Value>, max_length: usize) -> String {
    let raw = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(_) => String::new(),
    };
    let trimmed = raw.trim();
    if trimmed.chars().count() <= max_length {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_length).collect()
    }
}

fn compact_string_or(value: Option<&Value>, max_length: usize, fallback: &str) -> String {
    let s = compact_string(value, max_length);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn compact_list(value: Option<&Value>, max_items: usize, item_max_len: usize) -> Vec<String> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .take(max_items)
        .map(|item| compact_string(Some(item), item_max_len))
        .filter(|s| !s.is_empty())
        .collect()
}

fn compact_unique_list(value: Option<&Value>, max_items: usize, item_max_len: usize) -> Vec<String> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in arr {
        let s = compact_string(Some(item), item_max_len);
        if !s.is_empty() && seen.insert(s.clone()) {
            out.push(s);
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

fn is_valid_version_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(crate) fn compact_version(version: Option<&Value>) -> Value {
    let obj = version.and_then(|v| v.as_object());
    let get = |k: &str| obj.and_then(|m| m.get(k));
    json!({
        "buildSha": compact_string_or(get("buildSha"), 40, "dev"),
        "plannerVersion": compact_string_or(get("plannerVersion"), 48, "legacy-planner"),
        "runtimeVersion": compact_string_or(get("runtimeVersion"), 48, "legacy-runtime"),
        "capabilityLevel": compact_string_or(get("capabilityLevel"), 40, "whole-track-baseline"),
        "stemAnalyzer": compact_string(get("stemAnalyzer"), 40),
        "stemModelVersion": compact_string(get("stemModelVersion"), 64),
        "auditionCohort": compact_string_or(get("auditionCohort"), 48, "live"),
        "variantId": compact_string_or(get("variantId"), 48, "current"),
    })
}

fn compact_pair(pair: Option<&Value>) -> Value {
    let obj = pair.and_then(|v| v.as_object());
    let get = |k: &str| obj.and_then(|m| m.get(k));
    json!({
        "fromKey": compact_string(get("fromKey"), 120),
        "toKey": compact_string(get("toKey"), 120),
        "fromTitle": compact_string(get("fromTitle"), 160),
        "fromArtist": compact_string(get("fromArtist"), 160),
        "toTitle": compact_string(get("toTitle"), 160),
        "toArtist": compact_string(get("toArtist"), 160),
    })
}

fn normalize_rating(value: Option<&Value>) -> Result<u8, String> {
    let rating = match value {
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                if (1..=3).contains(&u) {
                    Some(u as u8)
                } else {
                    None
                }
            } else if let Some(f) = n.as_f64() {
                if (f - 1.0).abs() == 0.0 {
                    Some(1)
                } else if (f - 2.0).abs() == 0.0 {
                    Some(2)
                } else if (f - 3.0).abs() == 0.0 {
                    Some(3)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Some(Value::String(s)) => match s.trim() {
            "1" => Some(1),
            "2" => Some(2),
            "3" => Some(3),
            _ => None,
        },
        _ => None,
    };
    rating.ok_or_else(|| "RATING_MUST_BE_1_2_OR_3".to_string())
}

fn normalize_hook_count(value: Option<&Value>) -> Value {
    match opt_f64(value) {
        Some(n) => json!(n.round().max(0.0) as u64),
        None => Value::Null,
    }
}

fn normalize_sustained_energy(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Bool(b)) => Value::Bool(*b),
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|x| x.is_finite())
            .map(|x| json!(round3(x)))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn compact_preferred_exit_range(value: Option<&Value>) -> Vec<f64> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    if arr.len() != 2 {
        return Vec::new();
    }
    let Some(a) = opt_f64(arr.first()) else {
        return Vec::new();
    };
    let Some(b) = opt_f64(arr.get(1)) else {
        return Vec::new();
    };
    let ba = round3(a.clamp(0.0, 1.0));
    let bb = round3(b.clamp(0.0, 1.0));
    vec![ba.min(bb), ba.max(bb)]
}

fn compact_fake_out_ms(value: Option<&Value>) -> Value {
    match opt_f64(value) {
        Some(n) => json!(n.clamp(0.0, 200.0).round() as i64),
        None => Value::Null,
    }
}

fn bounded_rounded_number(value: Option<&Value>, min: f64, max: f64, fallback: f64) -> f64 {
    match opt_f64(value) {
        Some(n) => round3(n).clamp(min, max),
        None => fallback,
    }
}

fn validated_rounded_number(value: Option<&Value>, min: f64, max: f64) -> Value {
    match opt_f64(value) {
        Some(n) if n >= min && n <= max => json!(round3(n)),
        _ => Value::Null,
    }
}

fn sanitize_shadow_side(value: Option<&Value>) -> Value {
    let source = value.and_then(|v| v.as_object());
    let phrase = source.and_then(|s| s.get("phrase")).and_then(|v| v.as_object());
    let tempo = source.and_then(|s| s.get("tempo")).and_then(|v| v.as_object());
    let downbeat = source.and_then(|s| s.get("downbeat")).and_then(|v| v.as_object());
    let loudness = source.and_then(|s| s.get("loudness")).and_then(|v| v.as_object());

    let selected_bars = match opt_f64(phrase.and_then(|p| p.get("selectedBars"))).map(|n| n as i64) {
        Some(4) => 4,
        Some(8) => 8,
        Some(16) => 16,
        Some(32) => 32,
        _ => 8,
    };

    let short_term_lufs = validated_rounded_number(loudness.and_then(|l| l.get("shortTermLufs")), -80.0, 6.0);
    let true_peak_dbtp = validated_rounded_number(loudness.and_then(|l| l.get("truePeakDbtp")), -80.0, 6.0);
    let loudness_available = loudness
        .and_then(|l| l.get("available"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && !short_term_lufs.is_null()
        && !true_peak_dbtp.is_null();

    json!({
        "phrase": {
            "selectedBars": selected_bars,
            "offsetBars": bounded_rounded_number(phrase.and_then(|p| p.get("offsetBars")), 0.0, 31.0, 0.0).round() as i64,
            "confidence": bounded_rounded_number(phrase.and_then(|p| p.get("confidence")), 0.0, 1.0, 0.0),
            "candidateCount": bounded_rounded_number(phrase.and_then(|p| p.get("candidateCount")), 0.0, 128.0, 0.0).round() as i64,
            "stable": phrase.and_then(|p| p.get("stable")).and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "tempo": {
            "medianStep": validated_rounded_number(tempo.and_then(|t| t.get("medianStep")), 0.1, 4.0),
            "localBpm": validated_rounded_number(tempo.and_then(|t| t.get("localBpm")), 20.0, 400.0),
            "driftRatio": validated_rounded_number(tempo.and_then(|t| t.get("driftRatio")), 0.0, 1.0),
            "confidence": bounded_rounded_number(tempo.and_then(|t| t.get("confidence")), 0.0, 1.0, 0.0),
            "stable": tempo.and_then(|t| t.get("stable")).and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "downbeat": {
            "confidence": bounded_rounded_number(downbeat.and_then(|d| d.get("confidence")), 0.0, 1.0, 0.0),
            "phaseErrorMs": validated_rounded_number(downbeat.and_then(|d| d.get("phaseErrorMs")), 0.0, 5000.0),
            "stable": downbeat.and_then(|d| d.get("stable")).and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "loudness": {
            "available": loudness_available,
            "shortTermLufs": if loudness_available { short_term_lufs } else { Value::Null },
            "truePeakDbtp": if loudness_available { true_peak_dbtp } else { Value::Null },
            "reason": compact_string(loudness.and_then(|l| l.get("reason")), 40),
        }
    })
}

fn sanitize_shadow_diagnostics(value: Option<&Value>) -> Option<Value> {
    let obj = value.and_then(|v| v.as_object())?;
    if obj.get("schema").and_then(|v| v.as_str()) != Some("cuefield-shadow-v1") {
        return None;
    }
    let from = obj.get("from").filter(|v| v.is_object())?;
    let to = obj.get("to").filter(|v| v.is_object())?;
    Some(json!({
        "schema": "cuefield-shadow-v1",
        "from": sanitize_shadow_side(Some(from)),
        "to": sanitize_shadow_side(Some(to)),
    }))
}

fn compact_diagnostics(diagnostics: Option<&Value>) -> Value {
    let obj = diagnostics.and_then(|v| v.as_object());
    let get = |k: &str| obj.and_then(|m| m.get(k));
    let mut map = Map::new();
    map.insert("outroCompleteness".to_string(), opt_round3(get("outroCompleteness")));
    map.insert("bIntroAggression".to_string(), opt_round3(get("bIntroAggression")));
    map.insert("styleTextureDistance".to_string(), opt_round3(get("styleTextureDistance")));
    if let Some(shadow) = sanitize_shadow_diagnostics(get("shadow")) {
        map.insert("shadow".to_string(), shadow);
    }
    Value::Object(map)
}

fn compact_bridge(transition: &Map<String, Value>) -> Value {
    let bars = match opt_f64(transition.get("bridgeBars")).map(|n| n as i64) {
        Some(4) => json!(4),
        Some(8) => json!(8),
        Some(16) => json!(16),
        _ => Value::Null,
    };
    json!({
        "selected": transition.get("bridgeSelected").and_then(|v| v.as_bool()).unwrap_or(false),
        "template": compact_string(transition.get("bridgeTemplate"), 32),
        "bars": bars,
        "climaxType": compact_string(transition.get("bridgeClimaxType"), 16),
        "climaxTime": opt_round3(transition.get("bridgeClimaxTime")),
        "climaxConfidence": opt_round3(transition.get("bridgeClimaxConfidence")),
        "lyricLinkScore": opt_round3(transition.get("lyricLinkScore")),
        "lyricLinkReasons": compact_list(transition.get("lyricLinkReasons"), 4, 80),
    })
}

fn compact_musical(transition: &Map<String, Value>) -> Value {
    json!({
        "evidence": transition.get("musicalEvidence").and_then(|v| v.as_bool()).unwrap_or(false),
        "compatibility": opt_round3(transition.get("musicalCompatibility")),
        "harmonicSimilarity": opt_round3(transition.get("harmonicSimilarity")),
        "keyCompatibility": opt_round3(transition.get("keyCompatibility")),
        "melodySimilarity": opt_round3(transition.get("melodySimilarity")),
        "risks": compact_list(transition.get("musicalRisks"), 3, 80),
    })
}

fn compact_local_musical(transition: &Map<String, Value>) -> Value {
    let local = transition.get("localMusical").and_then(|v| v.as_object());
    let first_present = |flat: &str, nested: &[&str]| -> Option<Value> {
        for key in nested {
            if let Some(v) = local.and_then(|m| m.get(*key)) {
                if !v.is_null() {
                    return Some(v.clone());
                }
            }
        }
        transition.get(flat).filter(|v| !v.is_null()).cloned()
    };
    let val = |flat: &str, nested: &[&str]| -> Value {
        first_present(flat, nested)
            .as_ref()
            .and_then(|v| opt_f64(Some(v)))
            .map(|n| json!(round3(n)))
            .unwrap_or(Value::Null)
    };
    let evidence = first_present("localMusicalEvidence", &["evidence"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let risks_val = first_present("localMusicalRisks", &["risks"]);

    json!({
        "evidence": evidence,
        "compatibility": val("localMusicalCompatibility", &["compatibility"]),
        "harmonicSimilarity": val("localHarmonicSimilarity", &["harmonicSimilarity"]),
        "keyCompatibility": val("localKeyCompatibility", &["keyCompatibility"]),
        "melodySimilarity": val("localMelodySimilarity", &["melodySimilarity"]),
        "confidence": val("localMusicalConfidence", &["confidence"]),
        "aWindowStart": val("localAWindowStart", &["aWindowStart"]),
        "bWindowStart": val("localBWindowStart", &["bWindowStart"]),
        "aDistance": val("localAWindowDistance", &["aDistance", "aWindowDistance"]),
        "bDistance": val("localBWindowDistance", &["bDistance", "bWindowDistance"]),
        "risks": compact_list(risks_val.as_ref(), 3, 80),
    })
}

fn compact_structure(structure: &Map<String, Value>) -> Value {
    let count = |key: &str| -> u64 {
        opt_f64(structure.get(key))
            .map(|n| n.max(0.0).min(12.0) as u64)
            .unwrap_or(0)
    };
    json!({
        "source": compact_string(structure.get("structureSource").or_else(|| structure.get("source")), 24),
        "confidence": opt_round3(structure.get("structureConfidence").or_else(|| structure.get("confidence"))),
        "protectedUntil": opt_round3(structure.get("protectedUntil")),
        "exitType": compact_string(structure.get("exitType"), 32),
        "exitConfidence": opt_round3(structure.get("exitConfidence")),
        "entryType": compact_string(structure.get("entryType"), 32),
        "entryConfidence": opt_round3(structure.get("entryConfidence")),
        "exitCandidateCount": count("exitCandidateCount"),
        "entryCandidateCount": count("entryCandidateCount"),
    })
}

fn compact_window(window: &Map<String, Value>) -> Value {
    let hook_ev = window.get("hookEvidence").and_then(|v| v.as_object());
    let rej = window
        .get("rejectionReasons")
        .or_else(|| window.get("windowRejectionReasons"));
    json!({
        "firstHookStart": opt_round3(window.get("firstHookStart")),
        "firstHookEnd": opt_round3(window.get("firstHookEnd")),
        "hookConfidence": opt_round3(window.get("hookConfidence")),
        "hookEvidence": {
            "repeatedLineCount": normalize_hook_count(hook_ev.and_then(|h| h.get("repeatedLineCount"))),
            "repeatedBlockCount": normalize_hook_count(hook_ev.and_then(|h| h.get("repeatedBlockCount"))),
            "energyLift": opt_round3(hook_ev.and_then(|h| h.get("energyLift"))),
            "sustainedEnergy": normalize_sustained_energy(hook_ev.and_then(|h| h.get("sustainedEnergy"))),
        },
        "exitRatio": opt_round3(window.get("exitRatio")),
        "effectiveSourceEnd": opt_round3(window.get("effectiveSourceEnd")),
        "mixStart": opt_round3(window.get("mixStart")),
        "handoffAt": opt_round3(window.get("handoffAt")),
        "landingAt": opt_round3(window.get("landingAt")),
        "audibleOverlap": opt_round3(window.get("audibleOverlap")),
        "preRollDuration": opt_round3(window.get("preRollDuration")),
        "energyContinuity": opt_round3(window.get("energyContinuity")),
        "grooveContinuity": opt_round3(window.get("grooveContinuity")),
        "tempoCompatibility": opt_round3(window.get("tempoCompatibility")),
        "rejectionReasons": compact_unique_list(rej, 8, 96),
    })
}

fn compact_transition(transition: Option<&Value>) -> Value {
    let empty = Map::new();
    let t = transition.and_then(|v| v.as_object()).unwrap_or(&empty);
    let structure = t.get("structure").and_then(|v| v.as_object()).unwrap_or(t);
    let window = t.get("window").and_then(|v| v.as_object()).unwrap_or(t);
    let set_mode = match t.get("setMode").and_then(|v| v.as_str()) {
        Some("sequential") => "sequential",
        Some("smart") => "smart",
        _ => "",
    };
    let rescue_class_raw = compact_string(t.get("terminalRescueClass"), 1).to_uppercase();
    let rescue_class = if matches!(rescue_class_raw.as_str(), "A" | "B" | "C") {
        rescue_class_raw
    } else {
        String::new()
    };

    json!({
        "recipe": compact_string(t.get("recipe"), 80),
        "transitionRecipe": compact_string(t.get("transitionRecipe"), 80),
        "executionMode": compact_string(t.get("executionMode"), 80),
        "tier": compact_string(t.get("tier"), 60),
        "score": opt_round3(t.get("score")),
        "evalScore": opt_round3(t.get("evalScore")),
        "exitTime": opt_round3(t.get("exitTime")),
        "entryTime": opt_round3(t.get("entryTime")),
        "overlapClass": compact_string(t.get("overlapClass"), 24),
        "overlapDuration": opt_round3(t.get("overlapDuration")),
        "entrySource": compact_string(t.get("entrySource"), 24),
        "entryConfidence": opt_round3(t.get("entryConfidence")),
        "bpmA": opt_round3(t.get("bpmA")),
        "bpmB": opt_round3(t.get("bpmB")),
        "relativeTempoDelta": opt_round3(t.get("relativeTempoDelta")),
        "beatGridTrusted": t.get("beatGridTrusted").and_then(|v| v.as_bool()).unwrap_or(false),
        "impactEligible": t.get("impactEligible").and_then(|v| v.as_bool()).unwrap_or(false),
        "teaserUsed": t.get("teaserUsed").and_then(|v| v.as_bool()).unwrap_or(false),
        "fakeOutMs": compact_fake_out_ms(t.get("fakeOutMs")),
        "impactFallbackRecipe": compact_string(t.get("impactFallbackRecipe"), 80),
        "runtimeDowngrade": compact_string(t.get("runtimeDowngrade"), 40),
        "setMode": set_mode,
        "minimumListenUntil": opt_round3(t.get("minimumListenUntil")),
        "musical": compact_musical(t),
        "localMusical": compact_local_musical(t),
        "bridge": compact_bridge(t),
        "route": compact_string(t.get("route"), 40),
        "compatibilityClass": compact_string(t.get("compatibilityClass"), 40),
        "contrastDirection": compact_string(t.get("contrastDirection"), 40),
        "preferredExitRange": compact_preferred_exit_range(t.get("preferredExitRange")),
        "routeReasons": compact_unique_list(t.get("routeReasons"), 4, 96),
        "routeFallbackUsed": t.get("routeFallbackUsed").and_then(|v| v.as_bool()).unwrap_or(false),
        "terminalRescueClass": rescue_class,
        "terminalRescueReason": compact_string(t.get("terminalRescueReason"), 40),
        "diagnostics": compact_diagnostics(t.get("diagnostics")),
        "structure": compact_structure(structure),
        "risks": compact_list(t.get("risks"), 8, 80),
        "window": compact_window(window),
    })
}

fn feedback_cohort(record: &Value) -> String {
    let schema = record.get("schema").and_then(|v| v.as_str());
    let Some(version) = record.get("version").and_then(|v| v.as_object()) else {
        return "legacy-unversioned".to_string();
    };
    let build_sha = version.get("buildSha").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
    let cohort = version.get("auditionCohort").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
    if schema != Some("cuefield-feedback-v2") || build_sha.is_empty() || cohort.is_empty() {
        return "legacy-unversioned".to_string();
    }
    let compacted = compact_version(Some(&Value::Object(version.clone())));
    let sha = compacted.get("buildSha").and_then(|v| v.as_str()).unwrap_or("");
    let aud = compacted.get("auditionCohort").and_then(|v| v.as_str()).unwrap_or("");
    if !is_valid_version_identifier(sha) || !is_valid_version_identifier(aud) {
        return "legacy-unversioned".to_string();
    }
    format!("{aud}@{sha}")
}

#[derive(Clone)]
struct Bucket {
    key: String,
    total: u64,
    passed: u64,
    failed: u64,
    pending: u64,
}

impl Bucket {
    fn new(key: String) -> Self {
        Self {
            key,
            total: 0,
            passed: 0,
            failed: 0,
            pending: 0,
        }
    }
}

fn add_to_bucket(map: &mut HashMap<String, Bucket>, key: &str, rating: u8) {
    let trimmed = compact_string(Some(&Value::String(key.to_string())), 120);
    let bucket_key = if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    };
    let bucket = map
        .entry(bucket_key.clone())
        .or_insert_with(|| Bucket::new(bucket_key));
    bucket.total += 1;
    match rating {
        1 => bucket.passed += 1,
        2 => bucket.failed += 1,
        3 => bucket.pending += 1,
        _ => {}
    }
}

fn finalize_buckets(map: HashMap<String, Bucket>) -> Vec<Value> {
    let mut items: Vec<Bucket> = map.into_values().collect();
    items.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| b.failed.cmp(&a.failed))
            .then_with(|| a.key.cmp(&b.key))
    });
    items
        .into_iter()
        .map(|b| {
            let pass_rate = if b.total > 0 {
                round3(b.passed as f64 / b.total as f64)
            } else {
                0.0
            };
            json!({
                "key": b.key,
                "total": b.total,
                "passed": b.passed,
                "failed": b.failed,
                "pending": b.pending,
                "passRate": pass_rate,
            })
        })
        .collect()
}

/// 验证并追加一条 Cuefield 反馈日志到指定 JSONL 文件。
/// 仅接受 `rating` 为 `1`、`2` 或 `3`。
pub fn append_cuefield_feedback(path: &Path, body: &Value) -> Result<Value, String> {
    let obj = body.as_object();
    let rating = normalize_rating(obj.and_then(|m| m.get("rating")))?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let record = json!({
        "schema": "cuefield-feedback-v2",
        "version": compact_version(obj.and_then(|m| m.get("version"))),
        "createdAt": now,
        "rating": rating,
        "note": compact_string(obj.and_then(|m| m.get("note")), 240),
        "pair": compact_pair(obj.and_then(|m| m.get("pair"))),
        "transition": compact_transition(obj.and_then(|m| m.get("transition"))),
    });

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("CUEFIELD_FEEDBACK_MKDIR_FAILED:{e}"))?;
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("CUEFIELD_FEEDBACK_OPEN_FAILED:{e}"))?;
    let serialized = serde_json::to_string(&record)
        .map_err(|e| format!("CUEFIELD_FEEDBACK_SERIALIZE_FAILED:{e}"))?;
    writeln!(file, "{serialized}")
        .map_err(|e| format!("CUEFIELD_FEEDBACK_WRITE_FAILED:{e}"))?;

    Ok(record)
}

/// 读取 Cuefield JSONL 反馈日志并按维度汇总统计分布。
pub fn read_cuefield_feedback_stats(path: &Path) -> Result<Value, String> {
    let raw = if path.exists() {
        fs::read_to_string(path).map_err(|e| format!("CUEFIELD_FEEDBACK_READ_FAILED:{e}"))?
    } else {
        String::new()
    };

    let mut by_cohort: HashMap<String, Bucket> = HashMap::new();
    let mut by_recipe: HashMap<String, Bucket> = HashMap::new();
    let mut by_tier: HashMap<String, Bucket> = HashMap::new();
    let mut by_overlap_class: HashMap<String, Bucket> = HashMap::new();
    let mut by_risk: HashMap<String, Bucket> = HashMap::new();
    let mut by_pair: HashMap<String, Bucket> = HashMap::new();
    let mut rating_counts = [0u64; 4];

    #[derive(Clone)]
    struct FailedSample {
        created_at: String,
        rating: u8,
        value: Value,
    }
    let mut failed_samples: Vec<FailedSample> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Ok(rating) = normalize_rating(record.get("rating")) else {
            continue;
        };

        let pair = record.get("pair");
        let transition = record.get("transition");
        let t_obj = transition.and_then(|v| v.as_object());

        let recipe = t_obj
            .and_then(|m| {
                ["transitionRecipe", "recipe", "executionMode"]
                    .iter()
                    .find_map(|k| m.get(*k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()))
            })
            .unwrap_or("unknown");
        let tier = t_obj
            .and_then(|m| m.get("tier"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        let overlap_class = t_obj
            .and_then(|m| m.get("overlapClass"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");

        let p_obj = pair.and_then(|v| v.as_object());
        let from_part = p_obj
            .and_then(|m| m.get("fromTitle").or_else(|| m.get("fromKey")))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("A");
        let to_part = p_obj
            .and_then(|m| m.get("toTitle").or_else(|| m.get("toKey")))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("B");
        let pair_key = format!("{from_part} -> {to_part}");

        rating_counts[rating as usize] += 1;
        add_to_bucket(&mut by_cohort, &feedback_cohort(&record), rating);
        add_to_bucket(&mut by_recipe, recipe, rating);
        add_to_bucket(&mut by_tier, tier, rating);
        add_to_bucket(&mut by_overlap_class, overlap_class, rating);
        add_to_bucket(&mut by_pair, &pair_key, rating);

        let risks: Vec<String> = t_obj
            .and_then(|m| m.get("risks"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec!["none".to_string()]);
        for risk in risks {
            add_to_bucket(&mut by_risk, &risk, rating);
        }

        if rating != 1 {
            let created_at = compact_string(record.get("createdAt"), 40);
            let sample = json!({
                "createdAt": created_at,
                "rating": rating,
                "note": compact_string(record.get("note"), 180),
                "pair": compact_pair(pair),
                "transition": compact_transition(transition),
            });
            failed_samples.push(FailedSample {
                created_at,
                rating,
                value: sample,
            });
        }
    }

    let total = rating_counts[1] + rating_counts[2] + rating_counts[3];
    let pass_rate = if total > 0 {
        round3(rating_counts[1] as f64 / total as f64)
    } else {
        0.0
    };
    failed_samples.sort_by(|a, b| {
        a.rating
            .cmp(&b.rating)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });

    Ok(json!({
        "total": total,
        "allVersions": true,
        "ratingCounts": {
            "1": rating_counts[1],
            "2": rating_counts[2],
            "3": rating_counts[3],
        },
        "passRate": pass_rate,
        "byCohort": finalize_buckets(by_cohort),
        "byRecipe": finalize_buckets(by_recipe),
        "byTier": finalize_buckets(by_tier),
        "byOverlapClass": finalize_buckets(by_overlap_class),
        "byRisk": finalize_buckets(by_risk),
        "byPair": finalize_buckets(by_pair).into_iter().take(20).collect::<Vec<_>>(),
        "failedSamples": failed_samples.into_iter().take(20).map(|s| s.value).collect::<Vec<_>>(),
    }))
}
