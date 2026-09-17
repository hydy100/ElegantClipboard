//! Limits used before a file-backed image is handed to WebView2.
//!
//! Network/UNC paths deliberately use a smaller fixed limit because metadata
//! and image decoding can otherwise block the clipboard window for a long time.

pub const DEFAULT_MAX_IMAGE_SIZE_KB: u64 = 51_200;
pub const MAX_PREVIEW_UNC_BYTES: u64 = 10 * 1024 * 1024;
pub const PREVIEW_LOCAL_SAFETY_CAP_BYTES: u64 = 100 * 1024 * 1024;

pub fn is_unc_path(path: &str) -> bool {
    path.starts_with(r"\\")
}

pub fn local_preview_limit_bytes(max_image_size_kb: u64) -> u64 {
    if max_image_size_kb == 0 {
        PREVIEW_LOCAL_SAFETY_CAP_BYTES
    } else {
        max_image_size_kb.saturating_mul(1024)
    }
}

pub fn preview_limit_bytes(path: &str, max_image_size_kb: u64) -> u64 {
    if is_unc_path(path) {
        MAX_PREVIEW_UNC_BYTES
    } else {
        local_preview_limit_bytes(max_image_size_kb)
    }
}

pub fn is_too_large_for_preview(
    path: &str,
    byte_size: i64,
    max_image_size_kb: u64,
    default_unknown: bool,
) -> bool {
    if byte_size > 0 {
        return byte_size as u64 > preview_limit_bytes(path, max_image_size_kb);
    }
    if is_unc_path(path) {
        return true;
    }
    default_unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_unc_and_local_limits() {
        assert!(is_unc_path(r"\\nas\share\image.png"));
        assert!(!is_unc_path(r"C:\image.png"));
        assert_eq!(preview_limit_bytes(r"C:\image.png", 20_480), 20 * 1024 * 1024);
        assert_eq!(preview_limit_bytes(r"\\nas\image.png", 20_480), MAX_PREVIEW_UNC_BYTES);
        assert_eq!(local_preview_limit_bytes(0), PREVIEW_LOCAL_SAFETY_CAP_BYTES);
    }

    #[test]
    fn unknown_unc_paths_are_rejected() {
        assert!(is_too_large_for_preview(r"\\nas\image.png", 0, 51_200, false));
        assert!(!is_too_large_for_preview(r"C:\image.png", 0, 51_200, false));
        assert!(is_too_large_for_preview(r"C:\image.png", 0, 51_200, true));
    }
}
