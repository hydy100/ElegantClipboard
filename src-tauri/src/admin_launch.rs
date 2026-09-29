//! 管理员启动配置
//!
//! 偏好存储在 `AppConfig.run_as_admin` 中（替代旧版 `AppCompatFlags\Layers` 注册表方案）。
//!
//! 提权流程：
//! 1. 应用通过注册表 `Run` 或手动启动（非提权）
//! 2. 检测到 `run_as_admin == true` 且未提权
//! 3. 通过预创建的计划任务（`schtasks /Run`，免 UAC）或 UAC 弹窗提权
//! 4. 当前非提权实例退出

use crate::config::AppConfig;
use std::path::PathBuf;

fn get_exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

// ─── 偏好设置（配置文件） ─────────────────────────────────────────────────────

/// 检查是否启用了管理员启动（读取配置文件）
pub fn is_admin_launch_enabled() -> bool {
    AppConfig::load().run_as_admin.unwrap_or(false)
}

/// 启用管理员启动
/// 保存偏好，若已提权则同时创建计划任务以便后续免 UAC 提权
pub fn enable_admin_launch() -> Result<(), String> {
    let mut config = AppConfig::load();
    config.run_as_admin = Some(true);
    config.save()?;

    // 已提权时创建/更新计划任务，后续重启可跳过 UAC 弹窗
    #[cfg(target_os = "windows")]
    if is_running_as_admin() {
        let _ = crate::task_scheduler::create_elevation_task();
    }

    Ok(())
}

/// 禁用管理员启动
/// 保存偏好、删除计划任务并清理旧版注册表项
pub fn disable_admin_launch() -> Result<(), String> {
    let mut config = AppConfig::load();
    config.run_as_admin = Some(false);
    config.save()?;

    let _ = crate::task_scheduler::delete_elevation_task();

    // 清理旧版兼容性注册表项
    #[cfg(target_os = "windows")]
    cleanup_compat_flags();

    Ok(())
}

// ─── 权限检查 ─────────────────────────────────────────────────────────────────

/// 检查当前进程是否已提权（管理员）
#[cfg(target_os = "windows")]
pub fn is_running_as_admin() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = Default::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_running_as_admin() -> bool {
    false
}

// ─── 自提权 ───────────────────────────────────────────────────────────────────

/// 尝试启动一个新的提权实例
/// 优先使用计划任务（免 UAC 弹窗），失败则回退到 UAC 提示
/// 返回 `true` 表示新实例已启动（调用方应退出）
#[cfg(target_os = "windows")]
pub fn self_elevate() -> bool {
    use crate::task_scheduler;

    // 优先尝试计划任务路径（免 UAC）
    if task_scheduler::is_elevation_task_exists()
        && task_scheduler::is_elevation_task_path_valid()
        && task_scheduler::run_elevation_task()
    {
        return true;
    }

    // 回退到 UAC 弹窗提权
    elevate_with_uac()
}

#[cfg(not(target_os = "windows"))]
pub fn self_elevate() -> bool {
    false
}

/// 通过 ShellExecute "runas" 启动新实例（会弹出 UAC 提示）
#[cfg(target_os = "windows")]
fn elevate_with_uac() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe_path = match get_exe_path() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let op: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    let file: Vec<u16> = exe_path.as_os_str().encode_wide().chain(Some(0)).collect();

    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0 as usize
            > 32
    }
}

// ─── 重启 ─────────────────────────────────────────────────────────────────────

/// 重启应用
/// 管理员模式：通过计划任务/UAC 提权启动新实例
/// 普通模式：通过 explorer.exe 确保非提权启动
#[cfg(target_os = "windows")]
pub fn restart_app() -> bool {
    if is_admin_launch_enabled() {
        return self_elevate();
    }

    // 非管理员：通过 explorer.exe 非提权启动
    launch_via_explorer()
}

/// Recover without calling the UI event loop; the child inherits the current
/// elevation and waits for this instance and its WebView browser to exit.
pub fn perform_restart(browser_pid: u32) -> Result<(), String> {
    // Recovery preserves the current security token. Never synchronously query
    // window geometry here: a broken UI event loop could block the restart.
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command.arg(format!("--webview-recovery-parent={}", std::process::id()));
    if browser_pid != 0 {
        command.arg(format!("--webview-recovery-browser={browser_pid}"));
    }
    command.spawn().map_err(|error| format!("启动恢复进程失败: {error}"))?;
    crate::webview_runtime::mark_intentional_exit();
    std::process::exit(0);
}

/// Run before single-instance registration and before creating any WebView.
pub(crate) fn wait_for_recovery_processes() -> Result<(), String> {
    for prefix in ["--webview-recovery-parent=", "--webview-recovery-browser="] {
        for argument in std::env::args().filter(|argument| argument.starts_with(prefix)) {
            let pid = parse_recovery_pid(&argument, prefix)?;
            #[cfg(windows)]
            wait_for_process_exit(pid, 30_000)?;
            #[cfg(not(windows))]
            let _ = pid;
        }
    }
    Ok(())
}

fn parse_recovery_pid(argument: &str, prefix: &str) -> Result<u32, String> {
    argument.strip_prefix(prefix).and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .ok_or_else(|| "无效的 WebView2 恢复进程参数".to_string())
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32, timeout_ms: u32) -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => handle,
        // The old process may already have exited before this child runs.
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => return Ok(()),
        Err(error) => return Err(format!("访问恢复进程 {pid} 失败: {error}")),
    };
    let result = unsafe { WaitForSingleObject(handle, timeout_ms) };
    let _ = unsafe { CloseHandle(handle) };
    if result == WAIT_OBJECT_0 { Ok(()) }
    else { Err(format!("等待恢复进程 {pid} 退出失败: {}", result.0)) }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn recovery_arguments_reject_invalid_or_self_pid() {
        let prefix = "--webview-recovery-parent=";
        assert!(parse_recovery_pid("--webview-recovery-parent=42", prefix).is_ok());
        for value in ["0".to_string(), "bad".to_string(), std::process::id().to_string()] {
            assert!(parse_recovery_pid(&format!("{prefix}{value}"), prefix).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn recovery_wait_times_out_instead_of_starting_a_duplicate_instance() {
        assert!(wait_for_process_exit(std::process::id(), 1).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn recovery_wait_observes_real_process_exit() {
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Milliseconds 200"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn().unwrap();
        let result = wait_for_process_exit(child.id(), 10_000);
        if result.is_err() { let _ = child.kill(); }
        let status = child.wait().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(status.success());
    }
}

#[cfg(not(target_os = "windows"))]
pub fn restart_app() -> bool {
    false
}

/// 通过 explorer.exe 启动，确保新进程不继承管理员权限
#[cfg(target_os = "windows")]
fn launch_via_explorer() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe_path = match get_exe_path() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let explorer: Vec<u16> = OsStr::new("explorer.exe")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let file: Vec<u16> = exe_path.as_os_str().encode_wide().chain(Some(0)).collect();

    unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(explorer.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0 as usize
            > 32
    }
}

// ─── 旧版兼容清理 ─────────────────────────────────────────────────────────────

/// 清理旧版 `AppCompatFlags\Layers` 中的 `RUNASADMIN` 注册表项
/// 不存在时安全跳过
#[cfg(target_os = "windows")]
pub fn cleanup_compat_flags() {
    use winreg::enums::*;
    use winreg::RegKey;

    const COMPAT_LAYERS_PATH: &str =
        r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";

    let exe_path = match get_exe_path() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return,
    };

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey_with_flags(COMPAT_LAYERS_PATH, KEY_ALL_ACCESS) {
        let _ = key.delete_value(&exe_path);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn cleanup_compat_flags() {}
