/** File-backed image preview limits shared by cards and WebView2 previews. */
export const DEFAULT_MAX_IMAGE_SIZE_KB = 51_200;
export const MAX_PREVIEW_UNC_BYTES = 10 * 1024 * 1024;
export const PREVIEW_LOCAL_SAFETY_CAP_BYTES = 100 * 1024 * 1024;

let maxLocalPreviewBytes = DEFAULT_MAX_IMAGE_SIZE_KB * 1024;

export function setMaxLocalPreviewBytesFromKb(maxImageSizeKb: number) {
  maxLocalPreviewBytes =
    Number.isFinite(maxImageSizeKb) && maxImageSizeKb > 0
      ? maxImageSizeKb * 1024
      : PREVIEW_LOCAL_SAFETY_CAP_BYTES;
}

export async function syncFilePreviewLimitsFromSettings(): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const raw = await invoke<string | null>("get_setting", {
      key: "max_image_size_kb",
    });
    const value = raw == null ? DEFAULT_MAX_IMAGE_SIZE_KB : Number(raw);
    setMaxLocalPreviewBytesFromKb(
      Number.isFinite(value) ? value : DEFAULT_MAX_IMAGE_SIZE_KB,
    );
  } catch {
    setMaxLocalPreviewBytesFromKb(DEFAULT_MAX_IMAGE_SIZE_KB);
  }
}

export function getMaxLocalPreviewBytes() {
  return maxLocalPreviewBytes;
}

export function isUncPath(path: string) {
  return path.startsWith("\\\\");
}

export function previewLimitBytes(path: string) {
  return isUncPath(path) ? MAX_PREVIEW_UNC_BYTES : maxLocalPreviewBytes;
}

export function isFileTooLargeForPreview(
  path: string,
  byteSize?: number,
  defaultForUnknown = false,
) {
  if (byteSize !== undefined && byteSize > 0) {
    return byteSize > previewLimitBytes(path);
  }
  return isUncPath(path) || defaultForUnknown;
}

export function shouldSkipFileImagePreview(
  path: string,
  byteSize: number | undefined,
  backendTooLarge: boolean,
) {
  return (
    backendTooLarge || isFileTooLargeForPreview(path, byteSize, true)
  );
}

export function isKnownTooLargeForPreview(path: string, byteSize: number) {
  return byteSize > 0 && isFileTooLargeForPreview(path, byteSize, true);
}
