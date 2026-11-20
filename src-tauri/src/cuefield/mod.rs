pub mod feedback;
pub mod lrc;
pub mod planner;
pub mod structure;

pub use feedback::{append_cuefield_feedback, read_cuefield_feedback_stats};
pub use lrc::{
    clean_text, normalize_text, parse_lrc, repeated_groups, score_lyric_link, LrcLine,
    LyricLinkScore,
};
pub use planner::{
    compute_transition_artifact_id, plan_cuefield_transition_from_cache,
    verify_transition_artifact, TransitionRequest,
};
pub use structure::{
    analyze_cache_entry, clamp01, compact_structure_map, round3, Bar, BeatEvent,
    NormalizedAnalysis, Phrase, StructureMap, StructureSection, TrackSummary,
    TransitionCandidate,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    fn make_synthetic_beatmap_entry(
        key: &str,
        title: &str,
        artist: &str,
        duration: f64,
        step: f64,
    ) -> Value {
        let count = (duration / step).floor() as usize;
        let mut beats = Vec::with_capacity(count);
        for i in 0..count {
            let t = round3(i as f64 * step);
            let is_downbeat = i % 4 == 0;
            // 在中段 (32s..80s) 抬高能量模拟副歌，后段 (120s..180s) 能量回落模拟尾奏
            let energy = if t >= 32.0 && t < 80.0 {
                0.86
            } else if t >= 120.0 {
                0.42
            } else {
                0.58
            };
            beats.push(json!({
                "time": t,
                "strength": energy,
                "confidence": 0.92,
                "impact": energy,
                "low": if is_downbeat { 0.72 } else { 0.45 },
                "body": 0.48,
                "snap": if is_downbeat { 0.35 } else { 0.25 },
                "combo": if is_downbeat { "downbeat" } else { "push" },
                "downbeat": is_downbeat,
                "primary": true,
                "camera": true,
                "pulse": true
            }));
        }
        json!({
            "key": key,
            "meta": {
                "title": title,
                "artist": artist
            },
            "map": {
                "duration": duration,
                "gridStep": step,
                "beats": beats
            }
        })
    }

    #[test]
    fn lrc_parses_normalizes_and_scores_lyric_link() {
        let lrc_a = "[00:10.50]  We   break   through   the night \n[00:35.00] Chorus line one\n[00:39.00] Chorus line two\n[01:10.00] Chorus line one\n[01:14.00] Chorus line two\n[02:10.00] I will rise above";
        let lrc_b = "[00:32.00] Take me to the light\n[00:36.00] You hear me tonight";
        let lines_a = parse_lrc(lrc_a);
        let lines_b = parse_lrc(lrc_b);

        assert_eq!(clean_text("  hello   world\t! "), "hello world !");
        assert_eq!(normalize_text("Hello, 世界! 123"), "hello 世界 123");
        assert_eq!(lines_a.len(), 6);
        assert!((lines_a[0].time - 10.5).abs() < 1e-6);

        let groups = repeated_groups(&lines_a);
        assert_eq!(groups.len(), 2);

        let link = score_lyric_link(&lines_a, &lines_b, 135.0, 32.0, 0.0);
        assert!(link.score > 0.0);
        assert!(link.reasons.iter().any(|r| r == "call-response" || r == "suffix-link" || r == "token-overlap"));
    }

    #[test]
    fn structure_normalizes_compact_array_beats() {
        // 紧凑数组格式：[time, strength, confidence, impact, low, body, snap, comboIdx, flags, mass, sharpness, step]
        let mut arr_beats = Vec::new();
        for i in 0..48 {
            let t = round3(i as f64 * 0.5);
            let combo_idx = if i % 4 == 0 { 1 } else { 2 };
            let flags = 1 | 2 | 4;
            arr_beats.push(json!([t, 0.7, 0.9, 0.75, 0.6, 0.4, 0.3, combo_idx, flags, 0.6, 0.3, 0.5]));
        }
        let entry = json!({
            "key": "compact-track",
            "meta": { "title": "Compact Song", "artist": "Tester" },
            "map": {
                "duration": 24.0,
                "gridStep": 0.5,
                "cameraBeats": arr_beats
            }
        });
        let analysis = analyze_cache_entry(&entry, "compact-track", &[]);
        assert_eq!(analysis.track.id, "compact-track");
        assert_eq!(analysis.beats.len(), 48);
        assert!((analysis.bpm - 120.0).abs() < 0.01);
        assert!(!analysis.bars.is_empty());
        assert!(!analysis.phrases.is_empty());

        let compact = compact_structure_map(&analysis.structure_map);
        assert!(compact.get("structureSource").is_some());
        assert!(compact.get("exitCandidates").unwrap().is_array());
        assert!(compact.get("entryCandidates").unwrap().is_array());
    }

    #[test]
    fn cuefield_plans_transition_between_cached_tracks() {
        let entry_a = make_synthetic_beatmap_entry("track-a", "Sunrise", "Artist A", 180.0, 0.5);
        let entry_b = make_synthetic_beatmap_entry("track-b", "Midnight", "Artist B", 180.0, 0.5);

        let lrc_a = "\
[00:32.00] Shining in the morning light\n\
[00:36.00] Running through the open sky\n\
[00:64.00] Shining in the morning light\n\
[00:68.00] Running through the open sky\n\
[01:40.00] Fading out into the night";
        let lrc_b = "\
[00:32.00] Dancing under neon stars\n\
[00:36.00] Feel the rhythm in your heart\n\
[00:64.00] Dancing under neon stars\n\
[00:68.00] Feel the rhythm in your heart";

        let mut cache = HashMap::new();
        cache.insert("track-a".to_string(), entry_a);
        cache.insert("track-b".to_string(), entry_b);

        let req = TransitionRequest {
            from_key: "track-a",
            to_key: "track-b",
            from_lrc: Some(lrc_a),
            to_lrc: Some(lrc_b),
            exit_bias: Some("late"),
            max_entry_time: Some(32.0),
            ..Default::default()
        };

        let plan = plan_cuefield_transition_from_cache(&req, |k| cache.get(k).cloned())
            .expect("transition plan should succeed");

        assert_eq!(plan["ok"], json!(true));
        let chosen = &plan["chosen"];
        let mix_start = chosen["mixStart"].as_f64().expect("mixStart must be f64");
        let handoff_at = chosen["handoffAt"].as_f64().expect("handoffAt must be f64");
        let protected_until = chosen["protectedUntil"].as_f64().expect("protectedUntil must be f64");

        assert!(mix_start >= protected_until - 10.0);
        assert!(handoff_at > mix_start);
        let timeline = chosen["timeline"].as_array().expect("timeline array");
        assert!(!timeline.is_empty());
        assert!(timeline.iter().any(|a| a["op"] == "handoff"));

        let artifact = &plan["artifact"];
        let artifact_id = artifact["artifactId"].as_str().expect("artifactId string");
        assert_eq!(artifact_id.len(), 64);
        assert!(artifact_id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert!(verify_transition_artifact(artifact));
    }

    #[test]
    fn cuefield_end_of_track_fallback_with_tail_evidence() {
        let entry_a = make_synthetic_beatmap_entry("tail-a", "Outro Track", "Artist A", 120.0, 0.5);
        let entry_b = make_synthetic_beatmap_entry("tail-b", "Next Track", "Artist B", 120.0, 0.5);
        let mut cache = HashMap::new();
        cache.insert("tail-a".to_string(), entry_a);
        cache.insert("tail-b".to_string(), entry_b);

        let req = TransitionRequest {
            from_key: "tail-a",
            to_key: "tail-b",
            enable_cadence_fallback: true,
            tail_evidence: Some(json!({
                "audibleEnd": 118.4,
                "containerEnd": 120.0,
                "toAudibleStart": 0.4,
                "requestedDuration": 1.6,
                "fromVocalState": "inactive",
                "toVocalState": "inactive",
                "confidence": 0.9
            })),
            ..Default::default()
        };

        let plan = plan_cuefield_transition_from_cache(&req, |k| cache.get(k).cloned())
            .expect("cadence fallback plan should succeed");

        assert_eq!(plan["ok"], json!(true));
        let chosen = &plan["chosen"];
        assert_eq!(chosen["transitionRecipe"], json!("end-of-track-crossfade"));
        assert_eq!(chosen["cadenceFallback"]["mode"], json!("end-of-track-crossfade"));
        let fade_dur = chosen["cadenceFallback"]["crossfadeDuration"].as_f64().unwrap();
        assert!((fade_dur - 1.6).abs() < 1e-3);
        let mix_start = chosen["mixStart"].as_f64().unwrap();
        let handoff_at = chosen["handoffAt"].as_f64().unwrap();
        assert!((handoff_at - 118.4).abs() < 1e-3);
        assert!((mix_start - 116.8).abs() < 1e-3);
        assert!(verify_transition_artifact(&plan["artifact"]));
    }

    #[test]
    fn cuefield_feedback_validates_rating_and_records_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("nested").join("cuefield-feedback.jsonl");

        // 校验非法评分
        for bad in [json!({"rating": 0}), json!({"rating": 4}), json!({"rating": "invalid"}), json!({})] {
            let err = append_cuefield_feedback(&file_path, &bad).unwrap_err();
            assert_eq!(err, "RATING_MUST_BE_1_2_OR_3");
        }

        // 追加有效评分 1, 2, 3
        let r1 = append_cuefield_feedback(
            &file_path,
            &json!({
                "rating": 1,
                "note": "smooth blend",
                "version": { "buildSha": "abc1234", "auditionCohort": "beta" },
                "pair": { "fromKey": "a", "toKey": "b", "fromTitle": "Song A", "toTitle": "Song B" },
                "transition": { "transitionRecipe": "bass-eq-handoff", "tier": "magic", "overlapClass": "medium" }
            }),
        )
        .expect("rating 1 ok");
        assert_eq!(r1["schema"], json!("cuefield-feedback-v2"));
        assert_eq!(r1["rating"], json!(1));

        append_cuefield_feedback(
            &file_path,
            &json!({
                "rating": 2,
                "note": "clash",
                "pair": { "fromTitle": "Song A", "toTitle": "Song C" },
                "transition": { "recipe": "quick-safe-fade", "tier": "weak", "overlapClass": "short", "risks": ["vocal overlap"] }
            }),
        )
        .expect("rating 2 ok");

        append_cuefield_feedback(
            &file_path,
            &json!({
                "rating": 3,
                "note": "pending check",
                "pair": { "fromKey": "x", "toKey": "y" },
                "transition": { "recipe": "echo-out", "tier": "usable" }
            }),
        )
        .expect("rating 3 ok");

        let stats = read_cuefield_feedback_stats(&file_path).expect("read stats ok");
        assert_eq!(stats["total"], json!(3));
        assert_eq!(stats["ratingCounts"]["1"], json!(1));
        assert_eq!(stats["ratingCounts"]["2"], json!(1));
        assert_eq!(stats["ratingCounts"]["3"], json!(1));
        assert_eq!(stats["passRate"], json!(0.333));
        assert_eq!(stats["failedSamples"].as_array().unwrap().len(), 2);
        assert_eq!(stats["byRecipe"].as_array().unwrap().len(), 3);
    }
}
