use serde::{Deserialize, Serialize};

pub const MEMORY_MASK_WORKING_SET: u32 = 1;
pub const MEMORY_MASK_MODIFIED_LIST: u32 = 4;
pub const MEMORY_MASK_STANDBY_LIST: u32 = 8;
pub const MEMORY_MASK_STANDBY_LOW: u32 = 16;
pub const MEMORY_MASK_DEFAULT: u32 =
    MEMORY_MASK_WORKING_SET | MEMORY_MASK_MODIFIED_LIST | MEMORY_MASK_STANDBY_LIST | MEMORY_MASK_STANDBY_LOW;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMemorySnapshot {
    #[serde(rename = "rssMB")]
    pub rss_mb: u64,
    #[serde(rename = "heapMB")]
    pub heap_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub platform: String,
    pub supported: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    #[serde(rename = "totalMB")]
    pub total_mb: u64,
    #[serde(rename = "freeMB")]
    pub free_mb: u64,
    #[serde(rename = "usedMB")]
    pub used_mb: u64,
    pub used_percent: u32,
    pub process: ProcessMemorySnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimMemoryResult {
    pub ok: bool,
    pub scope: String,
    pub trimmed: usize,
    pub pids: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeStepResult {
    pub id: String,
    pub status: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeMemoryResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_admin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    #[serde(rename = "beforeMB", skip_serializing_if = "Option::is_none")]
    pub before_mb: Option<u64>,
    #[serde(rename = "afterMB", skip_serializing_if = "Option::is_none")]
    pub after_mb: Option<u64>,
    #[serde(rename = "freedMB", skip_serializing_if = "Option::is_none")]
    pub freed_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_after: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PurgeStepResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAttachResult {
    pub ok: bool,
    pub supported: bool,
    pub attached: bool,
    pub target_window_id: String,
    pub parent_window_id: String,
    pub parent_class_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn normalize_memory_mask(mask: Option<u32>) -> u32 {
    let raw = mask.unwrap_or(MEMORY_MASK_DEFAULT);
    let masked = raw & MEMORY_MASK_DEFAULT;
    if masked == 0 {
        MEMORY_MASK_DEFAULT
    } else {
        masked
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use core::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, BOOL, HANDLE, HWND, LPARAM, LUID, POINT, TRUE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::Security::{
        AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, TokenElevation,
        SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES,
        TOKEN_QUERY,
    };
    use windows::Win32::System::ProcessStatus::{
        EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetCursorPos, GetParent,
        GetWindowLongPtrW, IsWindow, SendMessageTimeoutW, SetParent, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, GWL_STYLE, HWND_BOTTOM, SEND_MESSAGE_TIMEOUT_FLAGS, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WS_CHILD, WS_POPUP,
    };

    const SYSTEM_MEMORY_LIST_INFORMATION: i32 = 0x50;
    const SYSTEM_FILE_CACHE_INFORMATION_EX: i32 = 0x51;
    const MEMORY_CMD_EMPTY_WORKING_SETS: i32 = 2;
    const MEMORY_CMD_FLUSH_MODIFIED_LIST: i32 = 3;
    const MEMORY_CMD_PURGE_STANDBY_LIST: i32 = 4;
    const MEMORY_CMD_PURGE_STANDBY_LOW: i32 = 5;
    const STATUS_SUCCESS: i32 = 0;
    const STATUS_ACCESS_DENIED: i32 = -1073741790;
    const STATUS_PRIVILEGE_NOT_HELD: i32 = -1073741718;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct MemoryStatusEx {
        dwLength: u32,
        dwMemoryLoad: u32,
        ullTotalPhys: u64,
        ullAvailPhys: u64,
        ullTotalPageFile: u64,
        ullAvailPageFile: u64,
        ullTotalVirtual: u64,
        ullAvailVirtual: u64,
        ullAvailExtendedVirtual: u64,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct SystemFileCacheInformation {
        CurrentSize: usize,
        PeakSize: usize,
        PageFaultCount: u32,
        MinimumWorkingSet: isize,
        MaximumWorkingSet: isize,
        CurrentSizeIncludingTransitionInPages: usize,
        PeakSizeIncludingTransitionInPages: usize,
        TransitionRePurposeCount: u32,
        Flags: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtSetSystemInformation(
            system_information_class: i32,
            system_information: *const c_void,
            system_information_length: u32,
        ) -> i32;
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_global_memory_status() -> Option<MemoryStatusEx> {
        unsafe {
            let mut status: MemoryStatusEx = zeroed();
            status.dwLength = size_of::<MemoryStatusEx>() as u32;
            if GlobalMemoryStatusEx(&mut status) != 0 {
                Some(status)
            } else {
                None
            }
        }
    }

    fn current_process_working_set_mb() -> u64 {
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = zeroed();
            counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .is_ok()
            {
                (counters.WorkingSetSize as u64) / 1_048_576
            } else {
                0
            }
        }
    }

    fn enable_privilege(name: &str) -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
            .is_err()
            {
                return false;
            }
            let wide_name = to_wide(name);
            let mut luid = LUID::default();
            let ok = if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wide_name.as_ptr()), &mut luid)
                .is_ok()
            {
                let mut tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    ..zeroed()
                };
                tp.Privileges[0].Luid = luid;
                tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
                AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok()
            } else {
                false
            };
            let _ = CloseHandle(token);
            ok
        }
    }

    fn is_process_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut ret_len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret_len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    fn purge_memory_list(cmd: i32) -> i32 {
        unsafe {
            NtSetSystemInformation(
                SYSTEM_MEMORY_LIST_INFORMATION,
                (&cmd as *const i32).cast::<c_void>(),
                size_of::<i32>() as u32,
            )
        }
    }

    fn flush_system_file_cache() -> i32 {
        unsafe {
            let mut info: SystemFileCacheInformation = zeroed();
            info.MinimumWorkingSet = -1;
            info.MaximumWorkingSet = -1;
            NtSetSystemInformation(
                SYSTEM_FILE_CACHE_INFORMATION_EX,
                (&info as *const SystemFileCacheInformation).cast::<c_void>(),
                size_of::<SystemFileCacheInformation>() as u32,
            )
        }
    }

    pub fn get_memory_snapshot_win32() -> MemorySnapshot {
        let rss_mb = current_process_working_set_mb();
        if let Some(status) = read_global_memory_status() {
            let total = status.ullTotalPhys;
            let free = status.ullAvailPhys;
            let used = total.saturating_sub(free);
            let used_percent = if status.dwMemoryLoad > 0 {
                status.dwMemoryLoad
            } else if total > 0 {
                ((used as u128 * 100) / total as u128) as u32
            } else {
                0
            };
            MemorySnapshot {
                platform: "win32".to_string(),
                supported: true,
                total_bytes: total,
                free_bytes: free,
                used_bytes: used,
                total_mb: total / 1_048_576,
                free_mb: free / 1_048_576,
                used_mb: used / 1_048_576,
                used_percent,
                process: ProcessMemorySnapshot {
                    rss_mb,
                    heap_mb: rss_mb,
                },
                source: Some("GlobalMemoryStatusEx".to_string()),
            }
        } else {
            MemorySnapshot {
                platform: "win32".to_string(),
                supported: true,
                total_bytes: 0,
                free_bytes: 0,
                used_bytes: 0,
                total_mb: 0,
                free_mb: 0,
                used_mb: 0,
                used_percent: 0,
                process: ProcessMemorySnapshot {
                    rss_mb,
                    heap_mb: rss_mb,
                },
                source: Some("GlobalMemoryStatusEx:unavailable".to_string()),
            }
        }
    }

    pub fn trim_app_memory_win32(pids: &[u32]) -> TrimMemoryResult {
        let target_pids: Vec<u32> = if pids.is_empty() {
            unsafe { vec![GetCurrentProcessId()] }
        } else {
            let mut deduped = Vec::new();
            for &pid in pids {
                if pid > 0 && !deduped.contains(&pid) {
                    deduped.push(pid);
                }
            }
            if deduped.is_empty() {
                unsafe { vec![GetCurrentProcessId()] }
            } else {
                deduped
            }
        };

        let mut trimmed = 0usize;
        for &pid in &target_pids {
            unsafe {
                if let Ok(handle) = OpenProcess(
                    PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                    false,
                    pid,
                ) {
                    if EmptyWorkingSet(handle).is_ok() {
                        trimmed += 1;
                    }
                    let _ = CloseHandle(handle);
                }
            }
        }

        TrimMemoryResult {
            ok: true,
            scope: "app".to_string(),
            trimmed,
            pids: target_pids,
            reason: None,
            unsupported: None,
            message: None,
        }
    }

    pub fn purge_system_memory_win32(mask: u32, _manual: bool) -> PurgeMemoryResult {
        let effective_mask = normalize_memory_mask(Some(mask));
        let _ = enable_privilege("SeProfileSingleProcessPrivilege");
        let _ = enable_privilege("SeIncreaseQuotaPrivilege");

        let before = read_global_memory_status();
        let before_used = before
            .as_ref()
            .map(|s| s.ullTotalPhys.saturating_sub(s.ullAvailPhys))
            .unwrap_or(0);
        let load_before = before.as_ref().map(|s| s.dwMemoryLoad).unwrap_or(0);

        let mut steps = Vec::new();
        if (effective_mask & MEMORY_MASK_WORKING_SET) != 0 {
            steps.push(PurgeStepResult {
                id: "workingSet".to_string(),
                status: purge_memory_list(MEMORY_CMD_EMPTY_WORKING_SETS),
            });
            steps.push(PurgeStepResult {
                id: "systemFileCache".to_string(),
                status: flush_system_file_cache(),
            });
        }
        if (effective_mask & MEMORY_MASK_MODIFIED_LIST) != 0 {
            steps.push(PurgeStepResult {
                id: "modifiedList".to_string(),
                status: purge_memory_list(MEMORY_CMD_FLUSH_MODIFIED_LIST),
            });
        }
        if (effective_mask & MEMORY_MASK_STANDBY_LIST) != 0 {
            steps.push(PurgeStepResult {
                id: "standbyList".to_string(),
                status: purge_memory_list(MEMORY_CMD_PURGE_STANDBY_LIST),
            });
        }
        if (effective_mask & MEMORY_MASK_STANDBY_LOW) != 0 {
            steps.push(PurgeStepResult {
                id: "standbyLow".to_string(),
                status: purge_memory_list(MEMORY_CMD_PURGE_STANDBY_LOW),
            });
        }

        let after = read_global_memory_status();
        let after_used = after
            .as_ref()
            .map(|s| s.ullTotalPhys.saturating_sub(s.ullAvailPhys))
            .unwrap_or(before_used);
        let load_after = after.as_ref().map(|s| s.dwMemoryLoad).unwrap_or(load_before);
        let freed_bytes = before_used.saturating_sub(after_used);

        let denied = steps
            .iter()
            .any(|s| s.status == STATUS_ACCESS_DENIED || s.status == STATUS_PRIVILEGE_NOT_HELD);
        let succeeded = steps.iter().any(|s| s.status == STATUS_SUCCESS);
        let any_failed = steps.iter().any(|s| s.status != STATUS_SUCCESS);

        if denied && !succeeded && freed_bytes == 0 {
            return PurgeMemoryResult {
                ok: false,
                unsupported: None,
                disabled: None,
                need_admin: Some(!is_process_elevated()),
                partial: None,
                before_mb: Some(before_used / 1_048_576),
                after_mb: Some(after_used / 1_048_576),
                freed_mb: Some(0),
                load_before: Some(load_before),
                load_after: Some(load_after),
                steps,
                message: Some(
                    "Need administrator permission for full system memory purge.".to_string(),
                ),
            };
        }

        if freed_bytes > 0 || succeeded {
            return PurgeMemoryResult {
                ok: true,
                unsupported: None,
                disabled: None,
                need_admin: Some(any_failed && !is_process_elevated()),
                partial: Some(any_failed),
                before_mb: Some(before_used / 1_048_576),
                after_mb: Some(after_used / 1_048_576),
                freed_mb: Some(freed_bytes / 1_048_576),
                load_before: Some(load_before),
                load_after: Some(load_after),
                steps,
                message: if any_failed {
                    Some("Partial purge completed; full result requires administrator permission.".to_string())
                } else {
                    None
                },
            };
        }

        PurgeMemoryResult {
            ok: false,
            unsupported: None,
            disabled: None,
            need_admin: Some(!is_process_elevated()),
            partial: None,
            before_mb: Some(before_used / 1_048_576),
            after_mb: Some(after_used / 1_048_576),
            freed_mb: Some(0),
            load_before: Some(load_before),
            load_after: Some(load_after),
            steps,
            message: Some("System memory purge API returned non-zero status.".to_string()),
        }
    }

    unsafe extern "system" fn enum_workerw_proc(top: HWND, lparam: LPARAM) -> BOOL {
        let def_view_class = to_wide("SHELLDLL_DefView");
        let workerw_class = to_wide("WorkerW");
        if let Ok(shell_view) =
            FindWindowExW(top, HWND::default(), PCWSTR(def_view_class.as_ptr()), PCWSTR::null())
        {
            if !shell_view.is_invalid() {
                if let Ok(candidate) = FindWindowExW(
                    HWND::default(),
                    top,
                    PCWSTR(workerw_class.as_ptr()),
                    PCWSTR::null(),
                ) {
                    if !candidate.is_invalid() {
                        let slot = lparam.0 as *mut isize;
                        if !slot.is_null() {
                            *slot = candidate.0 as isize;
                        }
                        return BOOL(0);
                    }
                }
            }
        }
        TRUE
    }

    unsafe extern "system" fn enum_desktop_listview_proc(top: HWND, lparam: LPARAM) -> BOOL {
        let def_view_class = to_wide("SHELLDLL_DefView");
        let list_view_class = to_wide("SysListView32");
        if let Ok(shell_view) =
            FindWindowExW(top, HWND::default(), PCWSTR(def_view_class.as_ptr()), PCWSTR::null())
        {
            if !shell_view.is_invalid() {
                if let Ok(list_view) = FindWindowExW(
                    shell_view,
                    HWND::default(),
                    PCWSTR(list_view_class.as_ptr()),
                    PCWSTR::null(),
                ) {
                    if !list_view.is_invalid() {
                        let slot = lparam.0 as *mut isize;
                        if !slot.is_null() {
                            *slot = list_view.0 as isize;
                        }
                        return BOOL(0);
                    }
                }
            }
        }
        TRUE
    }

    fn window_class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }

    pub fn find_workerw() -> Result<isize, String> {
        unsafe {
            let progman_class = to_wide("Progman");
            let progman = FindWindowW(PCWSTR(progman_class.as_ptr()), PCWSTR::null())
                .map_err(|_| "WALLPAPER_PROGMAN_NOT_FOUND".to_string())?;
            if progman.is_invalid() {
                return Err("WALLPAPER_PROGMAN_NOT_FOUND".to_string());
            }
            let mut result: usize = 0;
            let _ = SendMessageTimeoutW(
                progman,
                0x052C,
                WPARAM(0),
                LPARAM(0),
                SEND_MESSAGE_TIMEOUT_FLAGS(0),
                1000,
                Some(&mut result),
            );

            let mut workerw: isize = 0;
            let _ = EnumWindows(Some(enum_workerw_proc), LPARAM(&mut workerw as *mut isize as isize));
            if workerw == 0 {
                return Err("WALLPAPER_WORKERW_NOT_FOUND".to_string());
            }
            Ok(workerw)
        }
    }

    pub fn set_full_desktop_attached(hwnd_raw: isize, attach: bool) -> Result<DesktopAttachResult, String> {
        if hwnd_raw == 0 {
            return Err("WALLPAPER_NATIVE_HANDLE_INVALID".to_string());
        }
        let target = HWND(hwnd_raw as *mut c_void);
        unsafe {
            if !IsWindow(target).as_bool() {
                return Err("WALLPAPER_TARGET_NOT_FOUND".to_string());
            }
            if attach {
                let workerw_raw = find_workerw()?;
                let workerw = HWND(workerw_raw as *mut c_void);
                let style = GetWindowLongPtrW(target, GWL_STYLE);
                let child_style =
                    (style & !(WS_POPUP.0 as isize)) | (WS_CHILD.0 as isize);
                SetWindowLongPtrW(target, GWL_STYLE, child_style);
                let _ = SetParent(target, workerw)
                    .map_err(|_| "WALLPAPER_WORKERW_ATTACH_FAILED".to_string())?;
                let actual_parent = GetParent(target)
                    .map_err(|_| "WALLPAPER_WORKERW_ATTACH_FAILED".to_string())?;
                if actual_parent != workerw {
                    return Err("WALLPAPER_WORKERW_ATTACH_FAILED".to_string());
                }
                let _ = SetWindowPos(
                    target,
                    HWND_BOTTOM,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                Ok(DesktopAttachResult {
                    ok: true,
                    supported: true,
                    attached: true,
                    target_window_id: hwnd_raw.to_string(),
                    parent_window_id: workerw_raw.to_string(),
                    parent_class_name: window_class_name(workerw),
                    message: None,
                })
            } else {
                let _ = SetParent(target, HWND::default());
                let style = GetWindowLongPtrW(target, GWL_STYLE);
                let popup_style =
                    (style & !(WS_CHILD.0 as isize)) | (WS_POPUP.0 as isize);
                SetWindowLongPtrW(target, GWL_STYLE, popup_style);
                let _ = SetWindowPos(
                    target,
                    HWND::default(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                Ok(DesktopAttachResult {
                    ok: true,
                    supported: true,
                    attached: false,
                    target_window_id: hwnd_raw.to_string(),
                    parent_window_id: "0".to_string(),
                    parent_class_name: String::new(),
                    message: None,
                })
            }
        }
    }

    pub fn set_desktop_icons_visible(visible: bool) -> Result<bool, String> {
        unsafe {
            let mut list_view_raw: isize = 0;
            let _ = EnumWindows(
                Some(enum_desktop_listview_proc),
                LPARAM(&mut list_view_raw as *mut isize as isize),
            );
            if list_view_raw == 0 {
                return Err("FULL_DESKTOP_ICON_HOST_NOT_FOUND".to_string());
            }
            let list_view = HWND(list_view_raw as *mut c_void);
            let _ = ShowWindow(list_view, if visible { SW_SHOW } else { SW_HIDE });
            Ok(visible)
        }
    }

    pub fn get_cursor_client_pos(hwnd_raw: isize) -> Option<(i32, i32)> {
        if hwnd_raw == 0 {
            return None;
        }
        unsafe {
            let hwnd = HWND(hwnd_raw as *mut c_void);
            if !IsWindow(hwnd).as_bool() {
                return None;
            }
            let mut pt = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut pt).is_err() {
                return None;
            }
            if !ScreenToClient(hwnd, &mut pt).as_bool() {
                return None;
            }
            Some((pt.x, pt.y))
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;

    pub fn get_memory_snapshot_win32() -> MemorySnapshot {
        MemorySnapshot {
            platform: std::env::consts::OS.to_string(),
            supported: false,
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
            total_mb: 0,
            free_mb: 0,
            used_mb: 0,
            used_percent: 0,
            process: ProcessMemorySnapshot::default(),
            source: Some("unsupported-non-windows".to_string()),
        }
    }

    pub fn trim_app_memory_win32(pids: &[u32]) -> TrimMemoryResult {
        TrimMemoryResult {
            ok: true,
            scope: "app".to_string(),
            trimmed: 0,
            pids: pids.to_vec(),
            reason: None,
            unsupported: Some(true),
            message: Some("App memory working-set trim is Windows-only.".to_string()),
        }
    }

    pub fn purge_system_memory_win32(_mask: u32, _manual: bool) -> PurgeMemoryResult {
        PurgeMemoryResult {
            ok: false,
            unsupported: Some(true),
            disabled: None,
            need_admin: None,
            partial: None,
            before_mb: None,
            after_mb: None,
            freed_mb: None,
            load_before: None,
            load_after: None,
            steps: Vec::new(),
            message: Some("System memory purge is Windows-only.".to_string()),
        }
    }

    pub fn find_workerw() -> Result<isize, String> {
        Err("WALLPAPER_UNSUPPORTED_PLATFORM".to_string())
    }

    pub fn set_full_desktop_attached(hwnd_raw: isize, attach: bool) -> Result<DesktopAttachResult, String> {
        Ok(DesktopAttachResult {
            ok: false,
            supported: false,
            attached: false,
            target_window_id: hwnd_raw.to_string(),
            parent_window_id: "0".to_string(),
            parent_class_name: String::new(),
            message: Some(if attach {
                "Full desktop WorkerW attach is Windows-only.".to_string()
            } else {
                "Full desktop WorkerW detach is Windows-only.".to_string()
            }),
        })
    }

    pub fn set_desktop_icons_visible(_visible: bool) -> Result<bool, String> {
        Ok(true)
    }

    pub fn get_cursor_client_pos(_hwnd_raw: isize) -> Option<(i32, i32)> {
        None
    }
}

pub use imp::{
    find_workerw, get_cursor_client_pos, get_memory_snapshot_win32, purge_system_memory_win32,
    set_desktop_icons_visible, set_full_desktop_attached, trim_app_memory_win32,
};
