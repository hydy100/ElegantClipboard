use crate::commands::AppState;
use crate::config;
use crate::database::SettingsRepository;
use crate::webdav::{self, SyncOptions, WebDavConfig};
use std::sync::Arc;
use tauri::State;

/// 从数据库读取 WebDAV 配置
fn load_webdav_config(state: &Arc<AppState>) -> Result<WebDavConfig, String> {
    let repo = SettingsRepository::new(&state.db);
    let url = repo.get("webdav_url").ok().flatten().unwrap_or_default();
    let username = repo.get("webdav_username").ok().flatten().unwrap_or_default();
    let password = repo.get("webdav_password").ok().flatten().unwrap_or_default();
    let remote_dir = repo
        .get("webdav_remote_dir")
        .ok()
        .flatten()
        .unwrap_or_else(|| "/elegant-clipboard".to_string());
    let proxy_mode = repo.get("webdav_proxy_mode").ok().flatten().unwrap_or_else(|| "system".to_string());
    let proxy_url = repo.get("webdav_proxy_url").ok().flatten().unwrap_or_default();
    let accept_invalid_certs = repo
        .get("webdav_accept_invalid_certs")
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false);

    if url.is_empty() {
        return Err("WebDAV 地址未配置".to_string());
    }
    Ok(WebDavConfig {
        url,
        username,
        password,
        remote_dir,
        proxy_mode,
        proxy_url,
        accept_invalid_certs,
    })
}

/// 从数据库读取同步选项
fn load_sync_options(state: &Arc<AppState>) -> SyncOptions {
    let repo = SettingsRepository::new(&state.db);
    let get_bool = |key: &str, default: bool| -> bool {
        repo.get(key)
            .ok()
            .flatten()
            .map(|v| v != "false")
            .unwrap_or(default)
    };
    let get_u64 = |key: &str, default: u64| -> u64 {
        repo.get(key)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };

    SyncOptions {
        sync_text: get_bool("webdav_sync_text", true),
        sync_image: get_bool("webdav_sync_image", true),
        sync_files: get_bool("webdav_sync_files", true),
        sync_video: get_bool("webdav_sync_video", false),
        sync_settings: true,
        max_image_size_kb: get_u64("webdav_max_image_size_kb", 5120),
        max_file_size_kb: get_u64("webdav_max_file_size_kb", 5120),
        max_video_size_kb: get_u64("webdav_max_video_size_kb", 5120),
    }
}

/// 获取数据目录
fn get_data_dir() -> std::path::PathBuf {
    config::AppConfig::load().get_data_dir()
}

/// 测试 WebDAV 连接
#[tauri::command]
pub async fn webdav_test_connection(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let config = load_webdav_config(&state)?;
    tokio::task::spawn_blocking(move || webdav::test_connection(&config))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

/// 上传同步（本地 → 远端）
/// 元数据立即上传；图片和文件分别在后台异步上传
#[tauri::command]
pub async fn webdav_upload(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let config = load_webdav_config(&state)?;
    let options = load_sync_options(&state);
    let data_dir = get_data_dir();
    let db = state.db.clone();

    tokio::task::spawn_blocking(move || {
        // 上传元数据
        let zip_data = webdav::export_sync_data(&db, &data_dir, &options)?;
        let size = zip_data.len();
        webdav::upload_sync(&config, &zip_data, "clipboard_sync.zip")?;

        // 没有启用任何媒体同步时，不读取、创建或清理 media_map。
        if options.sync_image || options.sync_files || options.sync_video {
            let device_id = webdav::get_or_create_device_id(&db);
            let local_map = build_local_media_map(&db, &data_dir, &options, &device_id);
            let merged_map = if !local_map.is_empty() {
                match webdav::upload_media_map(&config, &local_map, &device_id) {
                    Ok(map) => Some(map),
                    Err(error) => {
                        tracing::warn!("上传 media_map 失败，跳过云端孤儿清理: {}", error);
                        None
                    }
                }
            } else {
                Some(webdav::download_media_map(&config).unwrap_or_default())
            };

            if let Some(ref merged_map) = merged_map {
                let _ = webdav::cleanup_orphaned_remote_media(&config, merged_map);
                spawn_media_upload_files(&app, &config, &data_dir, &local_map);
            }
        }

        Ok(format!("上传成功 ({})", format_size(size as u64)))
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

/// 下载同步（远端 → 本地）
/// 元数据立即下载；缺失的图片和文件分别在后台异步下载到对应路径
#[tauri::command]
pub async fn webdav_download(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let config = load_webdav_config(&state)?;
    let options = load_sync_options(&state);
    let data_dir = get_data_dir();
    let db = state.db.clone();

    tokio::task::spawn_blocking(move || {
        let zip_data = webdav::download_sync(&config, "clipboard_sync.zip")?;
        let msg = match zip_data {
            Some(data) => {
                let result = webdav::import_sync_data(&db, &data, &options, &data_dir)?;
                let mut parts = Vec::new();
                if result.items_imported > 0 {
                    parts.push(format!("导入 {} 条记录", result.items_imported));
                }
                if result.settings_imported {
                    parts.push("设置已同步".to_string());
                }

                if parts.is_empty() {
                    "下载成功，无新数据".to_string()
                } else {
                    format!("下载成功：{}", parts.join("，"))
                }
            }
            None => "远端无同步数据".to_string(),
        };

        // 关闭媒体同步时不下载媒体映射，也不触碰本地媒体文件。
        if options.sync_image || options.sync_files || options.sync_video {
            let media_map = webdav::download_media_map(&config).unwrap_or_default();
            if !media_map.is_empty() {
                let fixed = webdav::reconcile_local_media(&db, &media_map, &data_dir);
                let needed = webdav::plan_media_downloads(&db, &media_map, &data_dir);
                if needed.is_empty() {
                    if fixed > 0 {
                        webdav::emit_webdav_media_ready(&app);
                    }
                } else {
                    spawn_media_download(&app, &config, &data_dir, &db, &media_map, needed);
                }
            }
        }

        Ok(msg)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

/// 从数据库构建本地媒体映射表
fn build_local_media_map(
    db: &crate::database::Database,
    data_dir: &std::path::Path,
    options: &SyncOptions,
    device_id: &str,
) -> Vec<webdav::MediaEntry> {
    let max_bs = webdav::calc_max_query_size(options);
    let content_types = webdav::build_type_filter(options);
    if content_types.is_empty() {
        return Vec::new();
    }
    let tf = content_types.join(",");
    let items = crate::database::ClipboardRepository::new(db)
        .query_items_for_sync(&tf, max_bs)
        .unwrap_or_default();
    webdav::build_media_map(&items, data_dir, options, device_id)
}

/// 启动单类媒体上传线程
fn spawn_media_upload_worker(
    app: &tauri::AppHandle,
    config: &webdav::WebDavConfig,
    data_dir: &std::path::Path,
    entries: Vec<webdav::MediaEntry>,
    thread_name: &'static str,
    label: &'static str,
) {
    if entries.is_empty() {
        return;
    }

    let cfg = config.clone();
    let dir = data_dir.to_path_buf();
    let handle = app.clone();
    std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let msg = match webdav::upload_media_files(&cfg, &entries, &dir) {
                Ok((u, s, bytes)) => format!("{}上传完成：{} 新 ({})，{} 已存在跳过", label, u, format_size(bytes), s),
                Err(e) => format!("{}上传失败: {}", label, e),
            };
            emit_media_sync_done(&handle, &msg);
        })
        .ok();
}

/// 启动单类媒体下载线程
fn spawn_media_download_worker(
    app: &tauri::AppHandle,
    config: &webdav::WebDavConfig,
    data_dir: &std::path::Path,
    db: &crate::database::Database,
    full_media_map: &[webdav::MediaEntry],
    entries: Vec<webdav::MediaEntry>,
    thread_name: &'static str,
    label: &'static str,
) {
    if entries.is_empty() {
        return;
    }

    let cfg = config.clone();
    let dir = data_dir.to_path_buf();
    let handle = app.clone();
    let database = db.clone();
    let media_map = full_media_map.to_vec();
    std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let msg = match webdav::download_missing_media(&cfg, &entries, &dir) {
                Ok(n) if n > 0 => format!("{}下载完成：{} 个文件", label, n),
                Ok(_) => format!("{}已是最新", label),
                Err(e) => format!("{}下载失败: {}", label, e),
            };
            let _ = webdav::reconcile_local_media(&database, &media_map, &dir);
            emit_media_sync_done(&handle, &msg);
        })
        .ok();
}

/// 后台上传实际媒体文件（图片线程 + 文件线程 + 图标线程）
fn spawn_media_upload_files(
    app: &tauri::AppHandle,
    config: &webdav::WebDavConfig,
    data_dir: &std::path::Path,
    media_map: &[webdav::MediaEntry],
) {
    if media_map.is_empty() {
        return;
    }

    let images: Vec<_> = media_map.iter().filter(|e| e.media_type == "image").cloned().collect();
    let files: Vec<_> = media_map.iter().filter(|e| e.media_type == "file").cloned().collect();
    let videos: Vec<_> = media_map.iter().filter(|e| e.media_type == "video").cloned().collect();
    let icons: Vec<_> = media_map.iter().filter(|e| e.media_type == "icon").cloned().collect();

    spawn_media_upload_worker(app, config, data_dir, images, "webdav-upload-images", "图片");
    spawn_media_upload_worker(app, config, data_dir, files, "webdav-upload-files", "文件");
    spawn_media_upload_worker(app, config, data_dir, videos, "webdav-upload-videos", "视频");
    spawn_media_upload_worker(app, config, data_dir, icons, "webdav-upload-icons", "图标");
}

/// 后台下载缺失媒体：检查本地路径，不存在则从 WebDAV 下载到对应位置
fn spawn_media_download(
    app: &tauri::AppHandle,
    config: &webdav::WebDavConfig,
    data_dir: &std::path::Path,
    db: &crate::database::Database,
    full_media_map: &[webdav::MediaEntry],
    media_map: Vec<webdav::MediaEntry>,
) {
    let images: Vec<_> = media_map.iter().filter(|e| e.media_type == "image").cloned().collect();
    let files: Vec<_> = media_map.iter().filter(|e| e.media_type == "file").cloned().collect();
    let videos: Vec<_> = media_map.iter().filter(|e| e.media_type == "video").cloned().collect();
    let icons: Vec<_> = media_map.iter().filter(|e| e.media_type == "icon").cloned().collect();

    spawn_media_download_worker(app, config, data_dir, db, full_media_map, images, "webdav-download-images", "图片");
    spawn_media_download_worker(app, config, data_dir, db, full_media_map, files, "webdav-download-files", "文件");
    spawn_media_download_worker(app, config, data_dir, db, full_media_map, videos, "webdav-download-videos", "视频");
    spawn_media_download_worker(app, config, data_dir, db, full_media_map, icons, "webdav-download-icons", "图标");
}

/// 向前端发送媒体同步完成事件
fn emit_media_sync_done(app: &tauri::AppHandle, message: &str) {
    use tauri::Emitter;
    let _ = app.emit("media-sync-done", message.to_string());
    let _ = app.emit("clipboard-updated", Option::<i64>::None);
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
