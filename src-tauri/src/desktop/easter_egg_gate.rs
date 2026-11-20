use crate::config::write_json_pretty_atomic;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOGIN_EASTER_EGG_GATE_VERSION: &str = "world-peace-v1";
pub const LOGIN_EASTER_EGG_STATE_FILE: &str = "login-easter-egg.json";
pub const LOGIN_EASTER_EGG_PASSWORD: &str = "世界和平";
pub const LOGIN_EASTER_EGG_CREDENTIAL_FILES: &[&str] = &[
    ".cookie",
    ".qq-cookie",
    ".kugou-cookie",
    ".kugou-vip-evidence.json",
    ".qishui-cookie",
    ".qishui-token",
    ".spotify-token.json",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GateState {
    pub schema: u32,
    pub gate_version: String,
    pub cookie_reset_version: String,
    pub reset_complete: bool,
    pub unlocked: bool,
    pub reset_at: u64,
    pub unlocked_at: u64,
    pub reset_error: String,
}

impl Default for GateState {
    fn default() -> Self {
        Self {
            schema: 1,
            gate_version: String::new(),
            cookie_reset_version: String::new(),
            reset_complete: false,
            unlocked: false,
            reset_at: 0,
            unlocked_at: 0,
            reset_error: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicGateStatus {
    pub ok: bool,
    pub gate_version: String,
    pub unlocked: bool,
    pub reset_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub ok: bool,
    pub gate_version: String,
    pub unlocked: bool,
    pub reset_complete: bool,
    pub reset_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_reset: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockResult {
    pub ok: bool,
    pub gate_version: String,
    pub unlocked: bool,
    pub reset_complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 恒定时间密码比对（比较 UTF-8 字节，避免短路泄露长度与前缀信息）。
pub fn secure_password_match(input: &str) -> bool {
    let expected = LOGIN_EASTER_EGG_PASSWORD.as_bytes();
    let received = input.as_bytes();
    let max_len = expected.len().max(received.len());
    let mut diff: u8 = if expected.len() == received.len() { 0 } else { 1 };
    for i in 0..max_len {
        let a = *expected.get(i).unwrap_or(&0);
        let b = *received.get(i).unwrap_or(&0);
        diff |= black_box(a ^ b);
    }
    black_box(diff) == 0
}

fn default_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct LoginEasterEggGate {
    user_data_dir: PathBuf,
    extra_roots: Vec<PathBuf>,
    state_file: PathBuf,
    now_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
    state: Mutex<GateState>,
}

impl LoginEasterEggGate {
    /// 创建登录彩蛋门禁实例并加载持久化状态。
    pub fn new(user_data_dir: impl AsRef<Path>, extra_roots: Vec<PathBuf>) -> Self {
        Self::with_now(user_data_dir, extra_roots, Arc::new(default_now_millis))
    }

    pub fn with_now(
        user_data_dir: impl AsRef<Path>,
        extra_roots: Vec<PathBuf>,
        now_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        let user_data_dir = user_data_dir.as_ref().to_path_buf();
        let state_file = user_data_dir.join(LOGIN_EASTER_EGG_STATE_FILE);
        let initial = read_state_from_disk(&state_file);
        Self {
            user_data_dir,
            extra_roots,
            state_file,
            now_fn,
            state: Mutex::new(initial),
        }
    }

    fn resolve_credential_roots(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut roots = Vec::new();
        for candidate in std::iter::once(&self.user_data_dir).chain(self.extra_roots.iter()) {
            if candidate.as_os_str().is_empty() {
                continue;
            }
            let normalized = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone());
            if seen.insert(normalized.clone()) {
                roots.push(normalized);
            }
        }
        roots
    }

    fn build_public_status(state: &GateState) -> PublicGateStatus {
        PublicGateStatus {
            ok: true,
            gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
            unlocked: state.gate_version == LOGIN_EASTER_EGG_GATE_VERSION && state.unlocked,
            reset_complete: state.gate_version == LOGIN_EASTER_EGG_GATE_VERSION
                && state.reset_complete,
        }
    }

    /// 返回前端可消费的公开门禁状态。
    pub fn public_status(&self) -> PublicGateStatus {
        let guard = self.state.lock().unwrap();
        Self::build_public_status(&guard)
    }

    /// 检查是否已完成重置并且彩蛋密码已解锁。
    pub fn is_unlocked(&self) -> bool {
        let status = self.public_status();
        status.unlocked && status.reset_complete
    }

    /// 清理所有配置根目录下的平台登录凭证文件。
    pub fn clear_credential_files(&self) -> Result<(), String> {
        for root in self.resolve_credential_roots() {
            for name in LOGIN_EASTER_EGG_CREDENTIAL_FILES {
                let file = root.join(name);
                if file.exists() {
                    fs::remove_file(&file).map_err(|err| {
                        format!("LOGIN_CREDENTIAL_CLEAR_FAILED:{}:{err}", file.display())
                    })?;
                }
            }
        }
        Ok(())
    }

    fn clear_credential_state<F>(&self, clear_provider_sessions_fn: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.clear_credential_files()?;
        clear_provider_sessions_fn()?;
        // 登出回调在关闭会话期间可能刷新旧凭证，二次清理确保不会残留迁移副本
        self.clear_credential_files()?;
        Ok(())
    }

    fn write_state_locked(&self, state: &mut GateState) -> Result<(), String> {
        state.schema = 1;
        write_json_pretty_atomic(&self.state_file, state)
            .map_err(|e| format!("LOGIN_EASTER_EGG_STATE_WRITE_FAILED:{e}"))
    }

    /// 初始化门禁；首次或版本变更时会强制清空历史凭证并原子持久化状态。
    pub fn initialize<F>(&self, clear_provider_sessions_fn: F) -> InitializeResult
    where
        F: FnOnce() -> Result<(), String>,
    {
        let mut guard = self.state.lock().unwrap();
        *guard = read_state_from_disk(&self.state_file);

        if guard.gate_version == LOGIN_EASTER_EGG_GATE_VERSION
            && guard.cookie_reset_version == LOGIN_EASTER_EGG_GATE_VERSION
            && guard.reset_complete
        {
            if !guard.unlocked {
                if let Err(reset_error) = self.clear_credential_state(clear_provider_sessions_fn) {
                    guard.cookie_reset_version.clear();
                    guard.reset_complete = false;
                    guard.reset_error = reset_error.clone();
                    let _ = self.write_state_locked(&mut guard);
                    let status = Self::build_public_status(&guard);
                    return InitializeResult {
                        ok: status.ok,
                        gate_version: status.gate_version,
                        unlocked: status.unlocked,
                        reset_complete: status.reset_complete,
                        reset_performed: false,
                        replay_reset: None,
                        error: if reset_error.is_empty() {
                None
            } else {
                Some(reset_error)
            },
                    };
                }
            }
            let status = Self::build_public_status(&guard);
            return InitializeResult {
                ok: status.ok,
                gate_version: status.gate_version,
                unlocked: status.unlocked,
                reset_complete: status.reset_complete,
                reset_performed: false,
                replay_reset: None,
                error: None,
            };
        }

        let mut reset_error = match self.clear_credential_state(clear_provider_sessions_fn) {
            Ok(()) => String::new(),
            Err(err) => err,
        };

        let mut next = GateState {
            schema: 1,
            gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
            cookie_reset_version: if reset_error.is_empty() {
                LOGIN_EASTER_EGG_GATE_VERSION.to_string()
            } else {
                String::new()
            },
            reset_complete: reset_error.is_empty(),
            unlocked: false,
            reset_at: (self.now_fn)(),
            unlocked_at: 0,
            reset_error: reset_error.clone(),
        };

        if let Err(write_err) = self.write_state_locked(&mut next) {
            reset_error = write_err;
            next.cookie_reset_version.clear();
            next.reset_complete = false;
            next.reset_error = reset_error.clone();
        }
        *guard = next;

        let status = Self::build_public_status(&guard);
        InitializeResult {
            ok: status.ok,
            gate_version: status.gate_version,
            unlocked: status.unlocked,
            reset_complete: status.reset_complete,
            reset_performed: true,
            replay_reset: None,
            error: if reset_error.is_empty() {
                None
            } else {
                Some(reset_error)
            },
        }
    }

    /// 使用彩蛋密码解锁门禁。
    pub fn unlock(&self, input: &str) -> UnlockResult {
        let mut guard = self.state.lock().unwrap();
        *guard = read_state_from_disk(&self.state_file);

        if !guard.reset_complete || guard.gate_version != LOGIN_EASTER_EGG_GATE_VERSION {
            return UnlockResult {
                ok: false,
                gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
                unlocked: false,
                reset_complete: false,
                error: Some("LOGIN_EASTER_EGG_RESET_INCOMPLETE".to_string()),
                message: None,
            };
        }
        if !secure_password_match(input) {
            return UnlockResult {
                ok: false,
                gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
                unlocked: false,
                reset_complete: true,
                error: Some("LOGIN_EASTER_EGG_INVALID".to_string()),
                message: None,
            };
        }

        let mut next = guard.clone();
        next.unlocked = true;
        next.unlocked_at = (self.now_fn)();
        next.reset_error.clear();

        if let Err(err) = self.write_state_locked(&mut next) {
            return UnlockResult {
                ok: false,
                gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
                unlocked: false,
                reset_complete: true,
                error: Some("LOGIN_EASTER_EGG_STATE_WRITE_FAILED".to_string()),
                message: Some(err),
            };
        }
        *guard = next;
        let status = Self::build_public_status(&guard);
        UnlockResult {
            ok: status.ok,
            gate_version: status.gate_version,
            unlocked: status.unlocked,
            reset_complete: status.reset_complete,
            error: None,
            message: None,
        }
    }

    /// 强制重置门禁状态并清理所有登录凭证，用于重播彩蛋。
    pub fn reset_for_replay<F>(&self, clear_provider_sessions_fn: F) -> InitializeResult
    where
        F: FnOnce() -> Result<(), String>,
    {
        let mut guard = self.state.lock().unwrap();
        *guard = read_state_from_disk(&self.state_file);

        let mut reset_error = match self.clear_credential_state(clear_provider_sessions_fn) {
            Ok(()) => String::new(),
            Err(err) => err,
        };

        let mut next = GateState {
            schema: 1,
            gate_version: LOGIN_EASTER_EGG_GATE_VERSION.to_string(),
            cookie_reset_version: if reset_error.is_empty() {
                LOGIN_EASTER_EGG_GATE_VERSION.to_string()
            } else {
                String::new()
            },
            reset_complete: reset_error.is_empty(),
            unlocked: false,
            reset_at: (self.now_fn)(),
            unlocked_at: 0,
            reset_error: reset_error.clone(),
        };

        if let Err(write_err) = self.write_state_locked(&mut next) {
            reset_error = write_err;
            next.cookie_reset_version.clear();
            next.reset_complete = false;
            next.reset_error = reset_error.clone();
        }
        *guard = next;

        let status = Self::build_public_status(&guard);
        InitializeResult {
            ok: status.ok,
            gate_version: status.gate_version,
            unlocked: status.unlocked,
            reset_complete: status.reset_complete,
            reset_performed: true,
            replay_reset: Some(true),
            error: if reset_error.is_empty() {
                None
            } else {
                Some(reset_error)
            },
        }
    }
}

fn read_state_from_disk(state_file: &Path) -> GateState {
    fs::read_to_string(state_file)
        .ok()
        .and_then(|raw| serde_json::from_str::<GateState>(&raw).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    #[test]
    fn constant_time_password_match_works() {
        assert!(secure_password_match("世界和平"));
        assert!(!secure_password_match(""));
        assert!(!secure_password_match("世界"));
        assert!(!secure_password_match("世界和平!"));
        assert!(!secure_password_match("world-peace"));
    }

    #[test]
    fn initialize_unlock_persist_and_replay_reset() {
        let tmp = TempDir::new().unwrap();
        let user_data = tmp.path().join("userData");
        let extra = tmp.path().join("extra");
        fs::create_dir_all(&user_data).unwrap();
        fs::create_dir_all(&extra).unwrap();

        // 在两个目录中放置旧凭证文件
        for name in LOGIN_EASTER_EGG_CREDENTIAL_FILES {
            fs::write(user_data.join(name), "stale").unwrap();
            fs::write(extra.join(name), "stale").unwrap();
        }

        let gate = LoginEasterEggGate::new(&user_data, vec![extra.clone()]);
        assert!(!gate.is_unlocked());

        // 未 initialize 前不能解锁
        let pre_unlock = gate.unlock(LOGIN_EASTER_EGG_PASSWORD);
        assert!(!pre_unlock.ok);
        assert_eq!(
            pre_unlock.error.as_deref(),
            Some("LOGIN_EASTER_EGG_RESET_INCOMPLETE")
        );

        let callback_count = Arc::new(AtomicUsize::new(0));
        let cb_clone = callback_count.clone();
        let init_res = gate.initialize(move || {
            cb_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(init_res.ok);
        assert!(init_res.reset_performed);
        assert!(init_res.reset_complete);
        assert!(!init_res.unlocked);
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);

        // 所有凭证文件应已被清理
        for name in LOGIN_EASTER_EGG_CREDENTIAL_FILES {
            assert!(!user_data.join(name).exists());
            assert!(!extra.join(name).exists());
        }

        // 错误密码拒绝
        let bad = gate.unlock("wrong");
        assert!(!bad.ok);
        assert_eq!(bad.error.as_deref(), Some("LOGIN_EASTER_EGG_INVALID"));
        assert!(!gate.is_unlocked());

        // 正确密码解锁
        let good = gate.unlock("世界和平");
        assert!(good.ok);
        assert!(good.unlocked);
        assert!(gate.is_unlocked());

        // 重新实例化后状态持久化且不再重复清理
        let gate_reloaded = LoginEasterEggGate::new(&user_data, vec![extra.clone()]);
        assert!(gate_reloaded.is_unlocked());
        let init_again = gate_reloaded.initialize(|| Ok(()));
        assert!(!init_again.reset_performed);
        assert!(init_again.unlocked);

        // 重放重置
        fs::write(user_data.join(".cookie"), "new-session").unwrap();
        let replay = gate_reloaded.reset_for_replay(|| Ok(()));
        assert!(replay.reset_performed);
        assert_eq!(replay.replay_reset, Some(true));
        assert!(!replay.unlocked);
        assert!(!gate_reloaded.is_unlocked());
        assert!(!user_data.join(".cookie").exists());
    }

    #[test]
    fn initialize_handles_session_clear_failure() {
        let tmp = TempDir::new().unwrap();
        let gate = LoginEasterEggGate::new(tmp.path(), vec![]);
        let res = gate.initialize(|| Err("SESSION_CLOSE_TIMEOUT".to_string()));
        assert!(!res.reset_complete);
        assert!(!res.unlocked);
        assert_eq!(res.error.as_deref(), Some("SESSION_CLOSE_TIMEOUT"));
    }
}
