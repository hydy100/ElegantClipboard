use crate::database::{ClipboardRepository, QueryOptions, SettingsRepository};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tauri::State;

use super::AppState;

// ============ 设置命令 ============

/// 获取设置值
#[tauri::command]
pub async fn get_setting(
    state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    let state = state.inner().clone();
    super::run_blocking("get_setting", move || {
        let repo = SettingsRepository::new(&state.db);
        repo.get(&key).map_err(|e| e.to_string())
    }).await
}

/// 设置值
#[tauri::command]
pub async fn set_setting(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("set_setting", move || {
        let repo = SettingsRepository::new(&state.db);
        repo.set(&key, &value).map_err(|e| e.to_string())?;
        // 通知缓存失效
        crate::clipboard::invalidate_settings_cache();
        Ok(())
    }).await
}

/// 获取所有设置
#[tauri::command]
pub async fn get_all_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<HashMap<String, String>, String> {
    let state = state.inner().clone();
    super::run_blocking("get_all_settings", move || {
        let repo = SettingsRepository::new(&state.db);
        repo.get_all().map_err(|e| e.to_string())
    }).await
}

/// 批量获取指定 key 的设置值（减少多次 IPC 往返）
#[tauri::command]
pub async fn get_settings_batch(
    state: State<'_, Arc<AppState>>,
    keys: Vec<String>,
) -> Result<HashMap<String, String>, String> {
    let state = state.inner().clone();
    super::run_blocking("get_settings_batch", move || {
        let repo = SettingsRepository::new(&state.db);
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        repo.get_multiple(&key_refs).map_err(|e| e.to_string())
    }).await
}

// ============ 监控命令 ============

/// 暂停剪贴板监控
#[tauri::command]
pub async fn pause_monitor(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.monitor.pause();
    tracing::info!("Clipboard monitor paused by user");
    Ok(())
}

/// 恢复剪贴板监控
#[tauri::command]
pub async fn resume_monitor(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.monitor.resume();
    tracing::info!("Clipboard monitor resumed by user");
    Ok(())
}

/// 获取监控状态
#[tauri::command]
pub async fn get_monitor_status(state: State<'_, Arc<AppState>>) -> Result<MonitorStatus, String> {
    Ok(MonitorStatus {
        running: state.monitor.is_running(),
        paused: state.monitor.is_paused(),
    })
}

#[derive(serde::Serialize)]
pub struct MonitorStatus {
    pub running: bool,
    pub paused: bool,
}

// ============ 托盘图标命令 ============

/// 设置托盘图标可见性
#[tauri::command]
pub async fn set_tray_visible(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    visible: bool,
) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("set_tray_visible", move || {
        if let Some(tray) = app.tray_by_id("main-tray") {
            tray.set_visible(visible).map_err(|e| e.to_string())?;
        }
        let repo = SettingsRepository::new(&state.db);
        let _ = repo.set("show_tray_icon", if visible { "true" } else { "false" });
        Ok(())
    }).await
}

// ============ 数据库命令 ============

/// 优化数据库
#[tauri::command]
pub async fn optimize_database(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("optimize_database", move || {
        state.db.optimize().map_err(|e| e.to_string())?;
        tracing::info!("Database optimized");
        Ok(())
    }).await
}

/// 整理数据库
#[tauri::command]
pub async fn vacuum_database(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("vacuum_database", move || {
        state.db.vacuum().map_err(|e| e.to_string())?;
        tracing::info!("Database vacuumed");
        Ok(())
    }).await
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupExpiredInvalidResult {
    pub expired_count: i64,
    pub invalid_count: i64,
    pub deleted_count: i64,
    pub image_files_deleted: i64,
    pub retention_days: i64,
}

fn local_media_path_exists(path: &str, data_dir: &Path, subdir: &str) -> bool {
    let path = Path::new(path);
    path.is_file()
        || (!path.is_absolute() && data_dir.join(subdir).join(path).is_file())
}

fn item_has_invalid_media(
    item: &crate::database::ClipboardItem,
    data_dir: &Path,
) -> bool {
    match item.content_type.as_str() {
        "image" => item
            .image_path
            .as_deref()
            .map(|path| !local_media_path_exists(path, data_dir, "images"))
            .unwrap_or(true),
        "files" | "video" => {
            let Some(paths_json) = item.file_paths.as_deref() else {
                return true;
            };
            let Ok(paths) = serde_json::from_str::<Vec<String>>(paths_json) else {
                return true;
            };
            paths.is_empty() || !paths.iter().all(|path| Path::new(path).is_file())
        }
        _ => false,
    }
}

fn item_is_expired(created_at: &str, retention_days: i64) -> bool {
    if retention_days <= 0 {
        return false;
    }

    let threshold = chrono::Local::now().naive_local()
        - chrono::Duration::days(retention_days);
    let created = chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S%.f").ok()
        })
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(created_at)
                .ok()
                .map(|value| value.with_timezone(&chrono::Local).naive_local())
        });

    created.is_some_and(|value| value < threshold)
}

/// 清理未置顶、未收藏、未加标签的过期或失效记录，并释放关联图片缓存。
#[tauri::command]
pub async fn cleanup_expired_invalid_data(
    state: State<'_, Arc<AppState>>,
) -> Result<CleanupExpiredInvalidResult, String> {
    let state = state.inner().clone();
    super::run_blocking("cleanup_expired_invalid_data", move || {
        let settings_repo = SettingsRepository::new(&state.db);
        let retention_days = settings_repo
            .get("auto_cleanup_days")
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(30)
            .max(0);

        let data_dir = crate::config::AppConfig::load().get_data_dir();
        let repo = ClipboardRepository::new(&state.db);
        let candidates = repo
            .list(QueryOptions {
                exclude_favorited: true,
                exclude_tagged: true,
                ..QueryOptions::default()
            })
            .map_err(|error| format!("读取清理候选记录失败: {error}"))?;

        let mut ids = Vec::new();
        let mut expired_count = 0i64;
        let mut invalid_count = 0i64;

        for item in candidates {
            if item.is_pinned {
                continue;
            }

            let expired = item_is_expired(&item.created_at, retention_days);
            let invalid = item_has_invalid_media(&item, &data_dir);
            if expired {
                expired_count += 1;
            }
            if invalid {
                invalid_count += 1;
            }
            if expired || invalid {
                ids.push(item.id);
            }
        }

        let mut deleted_count = 0i64;
        let mut image_paths = Vec::new();
        // SQLite 默认参数上限通常为 999，分块避免大量历史记录清理失败。
        for chunk in ids.chunks(500) {
            let (deleted, paths) = repo
                .batch_delete(chunk)
                .map_err(|error| format!("删除清理候选记录失败: {error}"))?;
            deleted_count += deleted;
            image_paths.extend(paths);
        }

        let image_files_deleted = crate::clipboard::cleanup_image_files(&image_paths) as i64;
        if deleted_count > 0 {
            state.db.vacuum().ok();
        }

        tracing::info!(
            "Cleanup removed {} items (expired: {}, invalid: {}, images: {})",
            deleted_count,
            expired_count,
            invalid_count,
            image_files_deleted
        );

        Ok(CleanupExpiredInvalidResult {
            expired_count,
            invalid_count,
            deleted_count,
            image_files_deleted,
            retention_days,
        })
    }).await
}

// ============ 文件夹命令 ============

/// 打开文件夹选择对话框
#[tauri::command]
pub async fn select_folder_for_settings(app: tauri::AppHandle) -> Result<Option<String>, String> {
    super::run_blocking("select_folder_for_settings", move || {
        use tauri_plugin_dialog::DialogExt;

        let result = app
            .dialog()
            .file()
            .set_title("选择数据存储文件夹")
            .blocking_pick_folder();

        Ok(result.map(|p| p.to_string()))
    }).await
}

/// 在文件资源管理器中打开数据目录
#[tauri::command]
pub async fn open_data_folder() -> Result<(), String> {
    super::run_blocking("open_data_folder", move || {
        let config = crate::config::AppConfig::load();
        let data_dir = config.get_data_dir();
        super::open_path_in_explorer(&data_dir)
    }).await
}

// ============ 数据清理命令 ============

/// 重置所有设置为默认值（保留剪贴板数据）
#[tauri::command]
pub async fn reset_settings(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("reset_settings", move || {
        let repo = SettingsRepository::new(&state.db);
        repo.clear_all().map_err(|e| e.to_string())?;
        crate::clipboard::invalidate_settings_cache();
        tracing::info!("All settings reset to defaults");
        Ok(())
    }).await
}

/// 重置所有数据（删除剪贴板条目 + 自定义分组 + 设置 + 图片文件）
#[tauri::command]
pub async fn reset_all_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("reset_all_data", move || {
        use crate::database::ClipboardRepository;
        use std::fs;
        use tracing::info;

        let clipboard_repo = ClipboardRepository::new(&state.db);
        let image_paths = clipboard_repo.get_all_image_paths().unwrap_or_default();
        clipboard_repo.clear_all().map_err(|e| e.to_string())?;
        crate::clipboard::cleanup_image_files(&image_paths);

        // 清空设置
        let settings_repo = SettingsRepository::new(&state.db);
        settings_repo.clear_all().map_err(|e| e.to_string())?;
        crate::clipboard::invalidate_settings_cache();

        // 删除图片/图标目录（清理残留文件）
        let config = crate::config::AppConfig::load();
        let data_dir = config.get_data_dir();
        for dir_name in &["images", "icons"] {
            let dir = data_dir.join(dir_name);
            if dir.exists() {
                let _ = fs::remove_dir_all(&dir);
            }
        }

        state.db.vacuum().ok();

        info!("Reset all data completed");
        Ok(())
    }).await
}

// ============ 自启动命令 ============
// 始终使用 tauri_plugin_autostart（注册表 Run 键）。
// 管理员模式下应用会在启动后自行提权，无需单独的自启动机制。

/// 判断是否为便携版（同级目录无 NSIS 卸载程序）
#[tauri::command]
pub fn is_portable_mode() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| !d.join("unins000.exe").exists()))
        .unwrap_or(true)
}

/// 检查自启动是否启用
#[tauri::command]
pub async fn is_autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    super::run_blocking("is_autostart_enabled", move || {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().is_enabled().map_err(|e| e.to_string())
    }).await
}

/// 启用自启动
#[tauri::command]
pub async fn enable_autostart(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("enable_autostart", move || {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().enable().map_err(|e| e.to_string())?;
        // 持久化偏好到数据库，安装更新后可自动恢复
        let repo = SettingsRepository::new(&state.db);
        let _ = repo.set("autostart_enabled", "true");
        Ok(())
    }).await
}

/// 禁用自启动
#[tauri::command]
pub async fn disable_autostart(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("disable_autostart", move || {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().disable().map_err(|e| e.to_string())?;
        let repo = SettingsRepository::new(&state.db);
        let _ = repo.set("autostart_enabled", "false");
        Ok(())
    }).await
}

// ============ 运行中应用列表（应用过滤选择器） ============

#[derive(serde::Serialize, Clone)]
pub struct RunningAppInfo {
    pub name: String,
    pub process: String,
    pub icon: Option<String>,
}

/// 获取当前运行中的可见应用列表（用于应用过滤设置的可视化选择器）
#[tauri::command]
pub async fn get_running_apps(_state: State<'_, Arc<AppState>>) -> Result<Vec<RunningAppInfo>, String> {
    let _state = _state.inner().clone();
    super::run_blocking("get_running_apps", move || {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::{HWND, LPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{
                EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
            };
            use windows::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            use windows_core::BOOL;

            struct EnumCtx {
                self_pid: u32,
                apps: Vec<(String, String, String)>, // (name, process, exe_path)
            }

            unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
                unsafe {
                    let ctx = &mut *(lparam.0 as *mut EnumCtx);

                    if !IsWindowVisible(hwnd).as_bool() {
                        return BOOL(1);
                    }

                    let mut pid: u32 = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    if pid == 0 || pid == ctx.self_pid {
                        return BOOL(1);
                    }

                    let mut title_buf = [0u16; 512];
                    let title_len = GetWindowTextW(hwnd, &mut title_buf);
                    if title_len <= 0 {
                        return BOOL(1);
                    }
                    let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
                    if title.trim().is_empty() || title == "Program Manager" {
                        return BOOL(1);
                    }

                    if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_FORMAT};
                        let mut buf = [0u16; 1024];
                        let mut size = buf.len() as u32;
                        if QueryFullProcessImageNameW(
                            handle,
                            PROCESS_NAME_FORMAT(0),
                            windows::core::PWSTR::from_raw(buf.as_mut_ptr()),
                            &mut size,
                        ).is_ok() && size > 0 {
                            let path = String::from_utf16_lossy(&buf[..size as usize]);
                            let process = path.split('\\').next_back().unwrap_or(&path).to_string();
                            ctx.apps.push((title, process, path));
                        }
                        let _ = windows::Win32::Foundation::CloseHandle(handle);
                    }

                    BOOL(1)
                }
            }

            let mut ctx = EnumCtx {
                self_pid: std::process::id(),
                apps: Vec::new(),
            };

            unsafe {
                let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
            }

            // 去重（按进程名）并提取图标
            ctx.apps.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
            ctx.apps.dedup_by(|a, b| a.1.to_lowercase() == b.1.to_lowercase());

            let config = crate::config::AppConfig::load();
            let icons_dir = config.get_data_dir().join("icons");

            let result: Vec<RunningAppInfo> = ctx.apps.into_iter().map(|(_title, process, exe_path)| {
                let name = crate::clipboard::source_app::get_app_display_name_pub(&exe_path);
                let cache_key = crate::clipboard::source_app::compute_icon_cache_key_pub(&exe_path);
                let icon = crate::clipboard::source_app::extract_and_cache_icon(&exe_path, &icons_dir, &cache_key);
                RunningAppInfo { name, process, icon }
            }).collect();

            Ok(result)
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(Vec::new())
        }
    }).await
}

// ============ 系统主题命令 ============

/// RGB (0-255) 转 HSL 字符串 "H S% L%"
fn rgb_to_hsl_string(r: f64, g: f64, b: f64) -> String {
    let r_norm = r / 255.0;
    let g_norm = g / 255.0;
    let b_norm = b / 255.0;

    let max = r_norm.max(g_norm).max(b_norm);
    let min = r_norm.min(g_norm).min(b_norm);
    let delta = max - min;

    let mut h = 0.0;
    let mut s = 0.0;
    let l = (max + min) / 2.0;

    if delta > 0.0 {
        s = if l > 0.5 {
            delta / (2.0 - max - min)
        } else {
            delta / (max + min)
        };

        if max == r_norm {
            h = ((g_norm - b_norm) / delta).rem_euclid(6.0);
        } else if max == g_norm {
            h = (b_norm - r_norm) / delta + 2.0;
        } else {
            h = (r_norm - g_norm) / delta + 4.0;
        }
        h *= 60.0;
    }

    format!(
        "{} {}% {}%",
        h.round(),
        (s * 100.0).round(),
        (l * 100.0).round()
    )
}

/// 从注册表读取当前强调色
#[cfg(target_os = "windows")]
fn read_accent_color_from_registry() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let accent_key = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent")
        .ok()?;
    let color_value: u32 = accent_key.get_value("AccentColorMenu").ok()?;
    // ABGR 格式解析
    let r = (color_value & 0xFF) as f64;
    let g = ((color_value >> 8) & 0xFF) as f64;
    let b = ((color_value >> 16) & 0xFF) as f64;
    Some(rgb_to_hsl_string(r, g, b))
}

/// WndProc 回调用的全局 AppHandle
#[cfg(target_os = "windows")]
static WATCHER_APP_HANDLE: std::sync::OnceLock<parking_lot::Mutex<tauri::AppHandle>> =
    std::sync::OnceLock::new();

/// 启动后台线程监听 `WM_SETTINGCHANGE` 中的 `"ImmersiveColorSet"` 广播
/// 当系统强调色变化时向前端发射 `"system-accent-color-changed"` 事件
#[cfg(target_os = "windows")]
pub fn start_accent_color_watcher(app_handle: tauri::AppHandle) {
    WATCHER_APP_HANDLE.get_or_init(|| parking_lot::Mutex::new(app_handle));

    std::thread::spawn(|| {
        use windows::Win32::Foundation::*;
        use windows::Win32::UI::WindowsAndMessaging::*;

        unsafe extern "system" fn wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe {
                if msg == WM_SETTINGCHANGE {
                    let ptr = lparam.0 as *const u16;
                    if !ptr.is_null() {
                    // 读取 lParam 中的 null 结尾宽字符串
                        let len = (0usize..256).find(|&i| *ptr.add(i) == 0).unwrap_or(0);
                        let slice = std::slice::from_raw_parts(ptr, len);
                        if slice == windows::core::w!("ImmersiveColorSet").as_wide()
                            && let Some(handle) = WATCHER_APP_HANDLE.get() {
                                use tauri::Emitter;
                                let color = read_accent_color_from_registry();
                                let _ = handle.lock().emit("system-accent-color-changed", color);
                            }
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }

        unsafe {
            let class_name = windows::core::w!("ElegantClipboardAccentWatcher");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            // 创建隐藏顶级窗口接收广播消息
            // 不能用 HWND_MESSAGE，纯消息窗口无法收 WM_SETTINGCHANGE
            let _ = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                windows::core::w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            );

            // 消息循环
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

/// 获取 Windows 系统强调色（HSL 格式）
#[tauri::command]
pub async fn get_system_accent_color() -> Result<Option<String>, String> {
    super::run_blocking("get_system_accent_color", move || {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                // 优先注册表 AccentColorMenu
                if let Some(color) = read_accent_color_from_registry() {
                    return Ok(Some(color));
                }

                // 回退 DwmGetColorizationColor（DWM 混合色，可能与强调色略有差异）
                use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
                use windows_core::BOOL;

                let mut colorization: u32 = 0;
                let mut opaque_blend: BOOL = BOOL::from(false);

                if DwmGetColorizationColor(&mut colorization, &mut opaque_blend).is_ok() {
                    let a = ((colorization >> 24) & 0xFF) as f64;
                    let r = ((colorization >> 16) & 0xFF) as f64;
                    let g = ((colorization >> 8) & 0xFF) as f64;
                    let b = (colorization & 0xFF) as f64;

                    if a > 10.0 {
                        return Ok(Some(rgb_to_hsl_string(r, g, b)));
                    }
                }
            }

            Ok(None)
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(None)
        }
    }).await
}

// ============ 系统字体命令 ============

/// 获取系统已安装的字体家族列表
#[tauri::command]
pub fn get_system_fonts() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        use std::collections::BTreeSet;
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::Graphics::Gdi::{
            CreateDCW, DeleteDC, EnumFontFamiliesExW, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
        };

        let mut font_names = BTreeSet::new();

        unsafe extern "system" fn callback(
            lf: *const LOGFONTW,
            _tm: *const TEXTMETRICW,
            _font_type: u32,
            lparam: LPARAM,
        ) -> i32 {
            unsafe {
                let names = &mut *(lparam.0 as *mut BTreeSet<String>);
                let face = &(*lf).lfFaceName;
                let len = face.iter().position(|&c| c == 0).unwrap_or(face.len());
                let name = String::from_utf16_lossy(&face[..len]);
                // 跳过空名称和垂直书写字体（以 @ 开头）
                if !name.is_empty() && !name.starts_with('@') {
                    names.insert(name);
                }
            }
            1
        }

        unsafe {
            let hdc = CreateDCW(
                windows::core::w!("DISPLAY"),
                None,
                None,
                None,
            );
            if hdc.is_invalid() {
                return Vec::new();
            }

            let logfont = LOGFONTW {
                lfCharSet: DEFAULT_CHARSET,
                ..Default::default()
            };

            EnumFontFamiliesExW(
                hdc,
                &logfont,
                Some(callback),
                LPARAM(&mut font_names as *mut _ as isize),
                0,
            );

            let _ = DeleteDC(hdc);
        }

        font_names.into_iter().collect()
    }

    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{item_has_invalid_media, item_is_expired};
    use crate::database::ClipboardItem;

    fn test_item(
        content_type: &str,
        image_path: Option<&str>,
        file_paths: Option<&str>,
    ) -> ClipboardItem {
        ClipboardItem {
            id: 1,
            content_type: content_type.to_string(),
            text_content: None,
            html_content: None,
            rtf_content: None,
            image_path: image_path.map(str::to_string),
            file_paths: file_paths.map(str::to_string),
            content_hash: "content".to_string(),
            semantic_hash: "semantic".to_string(),
            preview: None,
            byte_size: 1,
            image_width: None,
            image_height: None,
            is_pinned: false,
            is_favorite: false,
            sort_order: 1,
            created_at: "2020-01-01 00:00:00".to_string(),
            updated_at: "2020-01-01 00:00:00".to_string(),
            access_count: 0,
            last_accessed_at: None,
            char_count: None,
            source_app_name: None,
            source_app_icon: None,
            files_valid: None,
        }
    }

    #[test]
    fn cleanup_detects_missing_media_and_empty_file_lists() {
        let data_dir = std::env::temp_dir().join(format!(
            "elegant-clipboard-cleanup-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(data_dir.join("images")).unwrap();

        let missing_image = test_item("image", Some("missing.png"), None);
        let empty_files = test_item("files", None, Some("[]"));
        assert!(item_has_invalid_media(&missing_image, &data_dir));
        assert!(item_has_invalid_media(&empty_files, &data_dir));

        std::fs::write(data_dir.join("images").join("ok.png"), b"test").unwrap();
        let valid_image = test_item("image", Some("ok.png"), None);
        assert!(!item_has_invalid_media(&valid_image, &data_dir));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn cleanup_uses_retention_days_and_zero_disables_expiry() {
        assert!(item_is_expired("2020-01-01 00:00:00", 1));
        assert!(!item_is_expired("2020-01-01 00:00:00", 0));
        assert!(!item_is_expired("not-a-timestamp", 1));
    }
}
