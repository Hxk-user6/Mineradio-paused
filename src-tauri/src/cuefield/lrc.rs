use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// 单行解析后的 LRC 歌词记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrcLine {
    pub time: f64,
    pub text: String,
    pub normalized: String,
}

/// 跨曲目歌词连结评分结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLinkScore {
    pub score: f64,
    pub reasons: Vec<String>,
}

fn clamp01(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

fn round3(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    (value * 1000.0).round() / 1000.0
}

/// 折叠连续空白字符并去除首尾空白。
pub fn clean_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 对歌词文本进行大小写与标点归一化（保留 Unicode 字母与数字，折叠空格）。
pub fn normalize_text(text: &str) -> String {
    let cleaned = clean_text(text).to_lowercase();
    let mut out = String::with_capacity(cleaned.len());
    let mut prev_space = true;
    for ch in cleaned.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_string()
}

fn parse_timestamp_prefix(raw: &str) -> Option<(f64, &str)> {
    let rest = raw.strip_prefix('[')?;
    let close_idx = rest.find(']')?;
    let tag = &rest[..close_idx];
    let after = &rest[close_idx + 1..];

    let colon_idx = tag.find(':')?;
    let mm_str = &tag[..colon_idx];
    if mm_str.is_empty() || mm_str.len() > 2 || !mm_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sec_part = &tag[colon_idx + 1..];
    let (ss_str, frac_str) = match sec_part.find(['.', ':']) {
        Some(sep) => (&sec_part[..sep], Some(&sec_part[sep + 1..])),
        None => (sec_part, None),
    };
    if ss_str.len() != 2 || !ss_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(frac) = frac_str {
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }

    let mm: f64 = mm_str.parse().ok()?;
    let ss: f64 = ss_str.parse().ok()?;
    let frac: f64 = match frac_str {
        Some(f) => format!("0.{f}").parse().ok()?,
        None => 0.0,
    };
    Some((mm * 60.0 + ss + frac, after))
}

/// 解析 LRC 文本为按时间升序排列的 `LrcLine` 列表。
pub fn parse_lrc(text: &str) -> Vec<LrcLine> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_end_matches('\r');
        let Some((time, after)) = parse_timestamp_prefix(trimmed) else {
            continue;
        };
        let lyric = clean_text(after);
        if lyric.is_empty() {
            continue;
        }
        let normalized = normalize_text(&lyric);
        lines.push(LrcLine {
            time,
            text: lyric,
            normalized,
        });
    }
    lines.sort_by(|a, b| a.time.total_cmp(&b.time));
    lines
}

/// 识别至少重复出现两次的歌词行组（归一化字符长度 >= 4）。
pub fn repeated_groups(lines: &[LrcLine]) -> Vec<Vec<LrcLine>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<LrcLine>> = BTreeMap::new();
    for line in lines {
        if line.normalized.is_empty() || line.normalized.chars().count() < 4 {
            continue;
        }
        if !groups.contains_key(&line.normalized) {
            order.push(line.normalized.clone());
        }
        groups
            .entry(line.normalized.clone())
            .or_default()
            .push(line.clone());
    }
    order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .filter(|group| group.len() >= 2)
        .collect()
}

fn is_link_char(ch: char) -> bool {
    let cp = ch as u32;
    ch.is_ascii_lowercase()
        || ch.is_ascii_digit()
        || ch == '_'
        || (0x3400..=0x9FFF).contains(&cp)
        || (0x3040..=0x30FF).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
}

fn is_cjk_unit(ch: char) -> bool {
    let cp = ch as u32;
    (0x3400..=0x9FFF).contains(&cp)
        || (0x3040..=0x30FF).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
}

fn normalize_for_link(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_space = true;
    for ch in lower.chars() {
        if is_link_char(ch) {
            out.push(ch);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_string()
}

fn extract_units(value: &str) -> Vec<String> {
    let normalized = normalize_for_link(value);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut latin_buf = String::new();
    let mut cjk_units = Vec::new();

    for ch in normalized.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            latin_buf.push(ch);
        } else {
            if !latin_buf.is_empty() {
                if seen.insert(latin_buf.clone()) {
                    out.push(latin_buf.clone());
                }
                latin_buf.clear();
            }
            if is_cjk_unit(ch) {
                cjk_units.push(ch.to_string());
            }
        }
    }
    if !latin_buf.is_empty() && seen.insert(latin_buf.clone()) {
        out.push(latin_buf);
    }
    for unit in cjk_units {
        if seen.insert(unit.clone()) {
            out.push(unit);
        }
    }
    out
}

fn line_link_text(line: &LrcLine) -> &str {
    if !line.normalized.is_empty() {
        &line.normalized
    } else {
        &line.text
    }
}

fn last_line_before(lines: &[LrcLine], before: f64) -> Option<LrcLine> {
    let mut filtered: Vec<LrcLine> = lines
        .iter()
        .filter(|line| line.time.is_finite() && line.time <= before && !normalize_for_link(line_link_text(line)).is_empty())
        .cloned()
        .collect();
    filtered.sort_by(|a, b| a.time.total_cmp(&b.time));
    filtered.pop()
}

fn lines_at_climax(lines: &[LrcLine], time: f64) -> Vec<LrcLine> {
    let mut sorted: Vec<LrcLine> = lines
        .iter()
        .filter(|line| line.time.is_finite() && !normalize_for_link(line_link_text(line)).is_empty())
        .cloned()
        .collect();
    sorted.sort_by(|a, b| a.time.total_cmp(&b.time));

    let near: Vec<LrcLine> = sorted
        .iter()
        .filter(|line| line.time >= time - 0.75 && line.time <= time + 12.0)
        .take(2)
        .cloned()
        .collect();
    if !near.is_empty() {
        return near;
    }
    sorted
        .into_iter()
        .filter(|line| line.time >= time)
        .take(2)
        .collect()
}

fn contains_pronoun(text: &str, values: &[&str]) -> bool {
    let normalized = format!(" {} ", normalize_for_link(text));
    values.iter().any(|value| {
        if value.chars().count() == 1 {
            normalized.contains(value)
        } else {
            normalized.contains(&format!(" {value} "))
        }
    })
}

fn is_call_response(from_text: &str, to_text: &str) -> bool {
    let from_first = contains_pronoun(from_text, &["我", "i", "me", "we", "us"]);
    let from_second = contains_pronoun(from_text, &["你", "you"]);
    let to_first = contains_pronoun(to_text, &["我", "i", "me", "we", "us"]);
    let to_second = contains_pronoun(to_text, &["你", "you"]);
    (from_first && to_second) || (from_second && to_first)
}

fn final_unit(value: &str) -> String {
    let normalized = normalize_for_link(value);
    normalized
        .split(' ')
        .filter(|s| !s.is_empty())
        .next_back()
        .unwrap_or(&normalized)
        .to_string()
}

fn suffix_linked(from_text: &str, to_text: &str) -> bool {
    let from = final_unit(from_text);
    let to = final_unit(to_text);
    if from.is_empty() || to.is_empty() || from == to {
        return false;
    }
    let from_chars: Vec<char> = from.chars().collect();
    let to_chars: Vec<char> = to.chars().collect();
    let max_len = 4.min(from_chars.len()).min(to_chars.len());
    for len in (2..=max_len).rev() {
        if from_chars[from_chars.len() - len..] == to_chars[to_chars.len() - len..] {
            return true;
        }
    }
    false
}

/// 评估退场句与入场高潮句之间的歌词连结度（词汇重叠、呼应代词、尾韵衔接与人声碰撞惩罚）。
pub fn score_lyric_link(
    from_lines: &[LrcLine],
    to_lines: &[LrcLine],
    exit_time: f64,
    climax_time: f64,
    vocal_overlap_sec: f64,
) -> LyricLinkScore {
    let exit_t = if exit_time.is_finite() { exit_time } else { f64::INFINITY };
    let climax_t = if climax_time.is_finite() { climax_time } else { 0.0 };

    let Some(outgoing) = last_line_before(from_lines, exit_t) else {
        return LyricLinkScore {
            score: 0.0,
            reasons: vec!["missing-lines".to_string()],
        };
    };
    let incoming_lines = lines_at_climax(to_lines, climax_t);
    if incoming_lines.is_empty() {
        return LyricLinkScore {
            score: 0.0,
            reasons: vec!["missing-lines".to_string()],
        };
    }

    let from_text = line_link_text(&outgoing).to_string();
    let to_text = incoming_lines
        .iter()
        .map(line_link_text)
        .collect::<Vec<_>>()
        .join(" ");

    let from_units = extract_units(&from_text);
    let to_units = extract_units(&to_text);
    let to_set: HashSet<&str> = to_units.iter().map(String::as_str).collect();
    let overlap = from_units.iter().filter(|u| to_set.contains(u.as_str())).count();
    let denom = from_units.len().min(to_units.len()).max(1);
    let overlap_ratio = overlap as f64 / denom as f64;

    let mut reasons = Vec::new();
    let mut score = overlap_ratio * 0.65;
    if overlap_ratio >= 0.2 {
        reasons.push("token-overlap".to_string());
    }
    if is_call_response(&from_text, &to_text) {
        score += 0.18;
        reasons.push("call-response".to_string());
    }
    if suffix_linked(&from_text, &to_text) {
        score += 0.17;
        reasons.push("suffix-link".to_string());
    }
    if vocal_overlap_sec.is_finite() && vocal_overlap_sec > 1.5 {
        score -= 0.25;
        reasons.push("vocal-collision".to_string());
    }
    if reasons.is_empty() {
        reasons.push("no-link".to_string());
    }

    LyricLinkScore {
        score: round3(clamp01(score)),
        reasons,
    }
}
