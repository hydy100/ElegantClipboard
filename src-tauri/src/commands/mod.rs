pub mod clipboard;
pub mod data_transfer;
pub mod file_ops;
pub mod tags;
pub mod preview;
pub mod settings;
pub mod sync;
pub mod translate;
pub mod ocr;
pub mod tts;
pub mod window;
pub(crate) mod activity;
mod selection;

// Serialize webview creation on callers, never by locking the GUI event loop.
// In particular, concurrent preview/translation/OCR creation must not nest pumps.
pub(crate) static WINDOW_CREATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

use crate::clipboard::ClipboardMonitor;
use crate::database::Database;
use std::sync::Arc;

/// 应用状态：包含数据库与剪贴板监控器
pub struct AppState {
    pub db: Database,
    pub monitor: ClipboardMonitor,
}

/// Keep synchronous database, filesystem and Win32 waits off Tokio's core workers.
/// Merely marking a command `async` does not make these operations non-blocking.
pub(crate) async fn run_blocking<T, F>(name: &'static str, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let result = work();
        if start.elapsed() > std::time::Duration::from_secs(2) {
            tracing::warn!(command = name, elapsed_ms = start.elapsed().as_millis(), "Slow blocking command");
        }
        result
    })
    .await
    .map_err(|error| format!("{name}: {error}"))?
}

/// 多屏/高 DPI 下隐藏窗口后系统可能不自动还原前台窗口，导致 Ctrl+V 无接收者。
/// 仅在目标窗口不是当前前台窗口时才调用 SetForegroundWindow，
/// 避免冗余 WM_ACTIVATE 导致某些应用重置内部焦点/光标位置。
#[cfg(target_os = "windows")]
fn restore_prev_foreground_window() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindow, SetForegroundWindow,
    };

    let prev = crate::input_monitor::get_prev_foreground_hwnd();
    if prev == 0 {
        tracing::warn!("hide: PREV_FOREGROUND_HWND 为 0，无法恢复前台窗口");
        return;
    }

    let hwnd = HWND(prev as *mut _);
    let current_fg = unsafe { GetForegroundWindow() };
    if current_fg.0 as isize == prev {
        tracing::info!("hide: 目标窗口已是前台，跳过 SetForegroundWindow");
    } else if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        let _ = unsafe { SetForegroundWindow(hwnd) };
        tracing::info!("hide: 已恢复前台窗口 hwnd={:#x}", prev);
    } else {
        tracing::warn!("hide: prev_hwnd={:#x} 已无效", prev);
    }
}

/// 隐藏主窗口或还原目标窗口焦点（用于粘贴前确保目标应用在前台）。
pub(crate) fn hide_main_window_if_not_pinned(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};

    if !crate::input_monitor::is_window_pinned() {
        if let Some(window) = app.get_webview_window("main") {
            // 窗口已隐藏时无需操作（快捷粘贴 Alt+N 不经过 UI，窗口本就不可见）
            if !window.is_visible().unwrap_or(false) {
                return;
            }
            window::save_window_size_if_enabled(app, &window);
            let _ = window.set_focusable(false);
            let _ = window.hide();
            crate::keyboard_hook::set_window_state(crate::keyboard_hook::WindowState::Hidden);
            crate::input_monitor::disable_mouse_monitoring();
            let _ = window.emit("window-hidden", ());
        }
        hide_preview_windows(app);

        #[cfg(target_os = "windows")]
        restore_prev_foreground_window();
    }
}

/// All hide paths share the same destruction epoch with show/reuse.
pub(crate) fn hide_image_preview_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    preview::hide_preview_window(app, true);
}

pub(crate) fn hide_text_preview_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    preview::hide_preview_window(app, false);
}

/// 隐藏所有悬浮预览窗口（图片 / 文本）。
pub(crate) fn hide_preview_windows<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    hide_image_preview_window(app);
    hide_text_preview_window(app);
}

/// Each operation has its own deadline; continuous pastes cannot grow a debounce
/// batch forever. The pause counter still protects overlapping operations.
static RESUME_TX: std::sync::LazyLock<std::sync::mpsc::Sender<(std::time::Instant, ClipboardMonitor)>> =
    std::sync::LazyLock::new(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(std::time::Instant, ClipboardMonitor)>();
        std::thread::Builder::new()
            .name("monitor-resume".into())
            .spawn(move || {
                loop {
                    let (deadline, monitor) = match rx.recv() {
                        Ok(request) => request,
                        Err(_) => return,
                    };
                    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
                    monitor.resume();
                }
            })
            .expect("failed to spawn monitor-resume thread");
        tx
    });

/// 暂停剪贴板监控并执行闭包，500ms 后恢复监控。
pub(crate) fn with_paused_monitor<F, T>(state: &Arc<AppState>, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    // Copy/translation/paste are transactions on one system clipboard. Never let
    // a selection backup restore over a concurrent paste from this application.
    static OPERATION: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _operation = OPERATION.try_lock_for(std::time::Duration::from_secs(2))
        .ok_or_else(|| "剪贴板操作忙，请稍后重试".to_string())?;
    state.monitor.pause();
    let _resume = ResumeOnDrop(state.monitor.clone());
    f()
}

struct ResumeOnDrop(ClipboardMonitor);

impl Drop for ResumeOnDrop {
    fn drop(&mut self) {
        let request = (std::time::Instant::now() + std::time::Duration::from_millis(500), self.0.clone());
        if let Err(error) = RESUME_TX.send(request) {
            // A closed worker must not permanently disable clipboard recording.
            error.0.1.resume();
        }
    }
}

/// 用系统文件管理器打开指定路径。
pub(crate) fn open_path_in_explorer(path: &std::path::Path) -> Result<(), String> {
    use std::process::Command;

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn pause_guard_restores_after_error_and_unwind_without_clearing_user_pause() {
        let path = std::env::temp_dir().join(format!("ec-guard-{}", uuid::Uuid::new_v4()));
        let state = Arc::new(AppState {
            db: Database::new(path.join("clipboard.db")).unwrap(),
            monitor: ClipboardMonitor::new(),
        });
        let result: Result<(), String> = with_paused_monitor(&state, || Err("clipboard busy".into()));
        assert!(result.is_err());
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), String> = with_paused_monitor(&state, || panic!("injected unwind"));
        }));
        assert!(panicked.is_err());
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.monitor.is_paused() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!state.monitor.is_paused(), "operation pauses leaked");
        assert!(state.monitor.toggle_user_pause());
        let _: Result<(), String> = with_paused_monitor(&state, || Err("busy".into()));
        std::thread::sleep(Duration::from_millis(650));
        assert!(state.monitor.is_paused(), "internal resume cleared manual pause");
        assert!(!state.monitor.toggle_user_pause());
        assert!(!state.monitor.is_paused());
        drop(state);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn blocking_work_does_not_starve_async_timers() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::channel();
            let task = tokio::spawn(run_blocking("test_blocking", move || {
                wait.recv_timeout(Duration::from_secs(2)).map_err(|e| e.to_string())
            }));
            tokio::time::sleep(Duration::from_millis(50)).await;
            release.send(()).unwrap();
            assert!(task.await.unwrap().is_ok());
        });
    }
}

mod pending_import;

mod paste_keys;
