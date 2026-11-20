use serde::{Deserialize, Serialize};

use crate::desktop::win32::{
    self, normalize_memory_mask, MemorySnapshot, PurgeMemoryResult, TrimMemoryResult,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PurgeSystemMemoryPayload {
    pub mask: Option<u32>,
    pub manual: Option<bool>,
    pub pids: Option<Vec<u32>>,
}

fn system_purge_available() -> bool {
    cfg!(target_os = "windows")
        && std::env::var("MINERADIO_DISABLE_SYSTEM_MEMORY_PURGE").ok().as_deref() != Some("1")
}

fn automatic_system_purge_enabled() -> bool {
    system_purge_available()
        && std::env::var("MINERADIO_DISABLE_AUTOMATIC_SYSTEM_MEMORY_PURGE")
            .ok()
            .as_deref()
            != Some("1")
}

pub fn get_memory_snapshot() -> MemorySnapshot {
    win32::get_memory_snapshot_win32()
}

pub fn trim_app_memory(reason: Option<&str>) -> TrimMemoryResult {
    let mut result = win32::trim_app_memory_win32(&[]);
    let normalized_reason = reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(128).collect::<String>());
    result.reason = normalized_reason;
    result
}

pub fn purge_system_memory(payload: Option<PurgeSystemMemoryPayload>) -> PurgeMemoryResult {
    let options = payload.unwrap_or_default();
    let manual = options.manual == Some(true);

    if cfg!(target_os = "windows") && !system_purge_available() {
        return PurgeMemoryResult {
            ok: false,
            unsupported: None,
            disabled: Some(true),
            need_admin: None,
            partial: None,
            before_mb: None,
            after_mb: None,
            freed_mb: None,
            load_before: None,
            load_after: None,
            steps: Vec::new(),
            message: Some(
                "System memory purge is unavailable on this machine or disabled by environment."
                    .to_string(),
            ),
        };
    }

    if cfg!(target_os = "windows") && !automatic_system_purge_enabled() && !manual {
        return PurgeMemoryResult {
            ok: false,
            unsupported: None,
            disabled: Some(true),
            need_admin: None,
            partial: None,
            before_mb: None,
            after_mb: None,
            freed_mb: None,
            load_before: None,
            load_after: None,
            steps: Vec::new(),
            message: Some(
                "Automatic system memory purge is disabled by default to avoid foreground CPU spikes."
                    .to_string(),
            ),
        };
    }

    let mask = normalize_memory_mask(options.mask);
    win32::purge_system_memory_win32(mask, manual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_snapshot_has_consistent_fields() {
        let snap = get_memory_snapshot();
        assert!(!snap.platform.is_empty());
        assert_eq!(snap.used_bytes, snap.total_bytes.saturating_sub(snap.free_bytes));
        #[cfg(not(target_os = "windows"))]
        assert!(!snap.supported);
    }

    #[test]
    fn trim_app_memory_preserves_reason_and_degrades_cleanly() {
        let res = trim_app_memory(Some("  manual-cleanup  "));
        assert!(res.ok);
        assert_eq!(res.scope, "app");
        assert_eq!(res.reason.as_deref(), Some("manual-cleanup"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(res.unsupported, Some(true));
    }

    #[test]
    fn purge_system_memory_gracefully_degrades_on_non_windows() {
        let res = purge_system_memory(Some(PurgeSystemMemoryPayload {
            mask: Some(0),
            manual: Some(true),
            pids: None,
        }));
        #[cfg(not(target_os = "windows"))]
        {
            assert!(!res.ok);
            assert_eq!(res.unsupported, Some(true));
        }
    }
}
