mod dedup;
mod handler;
mod monitor;
pub mod source_app;

pub(crate) use dedup::{
    canonical_url_text, compute_semantic_hash, is_url, normalize_rtf_for_hash,
    semantic_hash_from_text,
};
pub use handler::*;
pub use monitor::*;

/// 从磁盘删除图片文件，失败时记录日志，返回成功删除数。
pub fn cleanup_image_files(paths: &[String]) -> usize {
    let mut deleted = 0;
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::debug!("Deleted image file: {}", path);
                deleted += 1;
            }
            Err(e) => {
                tracing::debug!("Failed to delete image file {}: {}", path, e);
            }
        }

        // 清理旧版本写入的同名 CF_DIB 伴侣文件；当前版本不再生成它。
        let dib_path = std::path::Path::new(path).with_extension("dib");
        if std::fs::remove_file(&dib_path).is_ok() {
            tracing::debug!("Deleted legacy DIB companion: {}", dib_path.display());
            deleted += 1;
        }
    }
    deleted
}

/// 启动时清理孤立的图片和图标文件（磁盘上存在但数据库中无引用）。
pub fn cleanup_orphan_files(db: &crate::database::Database, data_dir: &std::path::Path) {
    use std::collections::HashSet;

    let repo = crate::database::ClipboardRepository::new(db);

    // 清理孤立图片
    let images_dir = data_dir.join("images");
    if images_dir.is_dir() {
        let referenced: HashSet<String> = repo
            .get_all_image_paths()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut orphan_count = 0usize;
        if let Ok(entries) = std::fs::read_dir(&images_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("dib")) {
                        if std::fs::remove_file(&path).is_ok() {
                            orphan_count += 1;
                            tracing::debug!("Removed legacy DIB companion: {}", path.display());
                        }
                        continue;
                    }
                    let path_str = path.to_string_lossy().to_string();
                    if !referenced.contains(&path_str) {
                        match std::fs::remove_file(&path) {
                            Ok(()) => orphan_count += 1,
                            Err(e) => tracing::debug!("Failed to remove orphan image {}: {}", path_str, e),
                        }
                    }
                }
            }
        }
        if orphan_count > 0 {
            tracing::info!("Cleaned up {} orphan image file(s)", orphan_count);
        }
    }

    // 清理孤立图标
    let icons_dir = data_dir.join("icons");
    if icons_dir.is_dir() {
        let referenced: HashSet<String> = repo
            .get_all_icon_paths()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut orphan_count = 0usize;
        if let Ok(entries) = std::fs::read_dir(&icons_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let path_str = path.to_string_lossy().to_string();
                    if !referenced.contains(&path_str) {
                        match std::fs::remove_file(&path) {
                            Ok(()) => orphan_count += 1,
                            Err(e) => tracing::debug!("Failed to remove orphan icon {}: {}", path_str, e),
                        }
                    }
                }
            }
        }
        if orphan_count > 0 {
            tracing::info!("Cleaned up {} orphan icon file(s)", orphan_count);
        }
    }
}
