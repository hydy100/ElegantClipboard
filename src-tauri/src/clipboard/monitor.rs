use super::{ClipboardContent, ClipboardHandler, handler::is_video_files};
use crate::database::Database;
#[cfg(not(windows))]
use clipboard_master::Master;
use clipboard_master::{CallbackResult, ClipboardHandler as CMHandler};
#[cfg(all(test, windows, feature = "native-smoke"))]
#[path = "monitor_smoke.rs"]
mod smoke;
#[cfg(windows)]
#[path = "windows_monitor.rs"]
mod windows_monitor;

fn clipboard_sequence() -> u32 {
    #[cfg(windows)]
    {
        unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use tauri::{AppHandle, Emitter};
use tracing::{debug, error, info, warn};

/// 剪贴板监听服务
#[derive(Clone)]
pub struct ClipboardMonitor {
    running: Arc<AtomicBool>,
    baseline_sequence: Arc<AtomicU32>,
    /// 暂停计数器：> 0 时忽略剪贴板变化，防止并发复制操作竞态
    pause_count: Arc<AtomicU32>,
    /// 用户手动暂停（托盘菜单），独立于内部 pause_count
    user_paused: Arc<AtomicBool>,
    handler: Arc<Mutex<Option<ClipboardHandler>>>,
    thread_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ClipboardMonitor {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            baseline_sequence: Arc::new(AtomicU32::new(clipboard_sequence())),
            pause_count: Arc::new(AtomicU32::new(0)),
            user_paused: Arc::new(AtomicBool::new(false)),
            handler: Arc::new(Mutex::new(None)),
            thread_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// 初始化监控器（数据库与图片路径）
    pub fn init(&self, db: &Database, images_path: std::path::PathBuf) {
        let handler = ClipboardHandler::new(db, images_path);
        *self.handler.lock() = Some(handler);
        info!("Clipboard monitor initialized");
    }

    /// Start once; even a missing native listener keeps sequence reconciliation alive.
    pub fn start(&self, app_handle: AppHandle) {
        let mut slot = self.thread_handle.lock();
        if slot.as_ref().is_some_and(|thread| !thread.is_finished()) {
            warn!("Clipboard monitor is already running or still stopping");
            return;
        }
        if let Some(old) = slot.take() {
            let _ = old.join();
        }
        self.running.store(true, Ordering::Release);
        let running = self.running.clone();
        let baseline_sequence = self.baseline_sequence.clone();
        let pause_count = self.pause_count.clone();
        let user_paused = self.user_paused.clone();
        let handler = self.handler.clone();
        let result = std::thread::Builder::new()
            .name("clipboard-monitor".into())
            .spawn(move || {
                struct Finished(Arc<AtomicBool>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _finished = Finished(running.clone());
                info!("Clipboard monitor worker started (main window may remain hidden)");
                let mut clipboard_handler = MonitorHandler {
                    running: running.clone(),
                    pause_count: pause_count.clone(),
                    user_paused: user_paused.clone(),
                    handler,
                    app_handle,
                    retry_pending: false,
                };
                #[cfg(windows)]
                windows_monitor::run(
                    &running,
                    baseline_sequence.load(Ordering::Acquire),
                    clipboard_sequence,
                    || {
                        pause_count.load(Ordering::Acquire) > 0
                            || user_paused.load(Ordering::Acquire)
                    },
                    || {
                        let _ = clipboard_handler.on_clipboard_change();
                        !clipboard_handler.retry_pending
                    },
                );
                #[cfg(not(windows))]
                while running.load(Ordering::Acquire) {
                    match Master::new(clipboard_handler.clone()) {
                        Ok(mut master) => {
                            if let Err(error) = master.run() {
                                error!(%error, "Clipboard monitor failed; retrying");
                            }
                        }
                        Err(error) => {
                            error!(%error, "Clipboard monitor initialization failed; retrying")
                        }
                    }
                    if running.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
                baseline_sequence.store(clipboard_sequence(), Ordering::Release);
                info!("Clipboard monitor worker stopped");
            });
        match result {
            Ok(thread) => *slot = Some(thread),
            Err(error) => {
                self.running.store(false, Ordering::Release);
                error!(%error, "Failed to spawn clipboard monitor worker");
            }
        }
    }

    /// Stop without an unbounded join on an external delayed-rendering provider.
    #[allow(dead_code)]
    pub fn stop(&self) {
        let mut slot = self.thread_handle.lock();
        self.running.store(false, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while slot.as_ref().is_some_and(|thread| !thread.is_finished())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if slot.as_ref().is_some_and(|thread| thread.is_finished()) {
            if let Some(thread) = slot.take() {
                let _ = thread.join();
            }
        } else if slot.is_some() {
            // Retain the handle: start() must not create a duplicate reader.
            warn!("Clipboard provider still returning; worker will stop when the call finishes");
        }
    }

    /// 暂停监控（递增暂停计数，支持多个并发暂停）
    pub fn pause(&self) {
        let count = self.pause_count.fetch_add(1, Ordering::SeqCst);
        debug!("Clipboard monitor paused (count: {})", count + 1);
    }

    /// 恢复监控（递减暂停计数，归零时真正恢复）
    pub fn resume(&self) {
        // 原子递减，仅当 > 0 时执行，避免 u32 下溢
        match self
            .pause_count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                if current > 0 { Some(current - 1) } else { None }
            }) {
            Ok(prev) => debug!("Clipboard monitor resume (count: {})", prev - 1),
            Err(_) => warn!("Resume called when not paused"),
        }
    }

    /// 是否已暂停（计数 > 0）
    pub fn is_paused(&self) -> bool {
        self.pause_count.load(Ordering::SeqCst) > 0 || self.user_paused.load(Ordering::SeqCst)
    }

    /// 是否运行中
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 用户手动切换暂停状态，返回切换后的暂停状态
    pub fn toggle_user_pause(&self) -> bool {
        let was = self.user_paused.fetch_xor(true, Ordering::SeqCst);
        let now = !was;
        info!("Clipboard monitor user pause toggled: {}", now);
        now
    }
}

impl Default for ClipboardMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// clipboard-master 事件处理器
#[derive(Clone)]
struct MonitorHandler {
    running: Arc<AtomicBool>,
    pause_count: Arc<AtomicU32>,
    user_paused: Arc<AtomicBool>,
    handler: Arc<Mutex<Option<ClipboardHandler>>>,
    app_handle: AppHandle,
    retry_pending: bool,
}

impl MonitorHandler {
    fn process_change(
        &mut self,
        source: impl FnOnce() -> Option<super::source_app::SourceAppInfo>,
        read: impl FnOnce() -> Option<ClipboardContent>,
    ) -> CallbackResult {
        self.retry_pending = false;
        // 检查是否应停止
        if !self.running.load(Ordering::SeqCst) {
            return CallbackResult::Stop;
        }

        // 检查是否已暂停（内部计数或用户手动）
        if self.pause_count.load(Ordering::SeqCst) > 0 || self.user_paused.load(Ordering::SeqCst) {
            debug!("Clipboard change ignored (paused)");
            return CallbackResult::Next;
        }

        // 先获取来源应用（在读取内容之前）
        let source = source();

        // 检查来源应用是否在排除列表中（使用缓存设置，避免数据库查询）
        if let Some(ref handler) = *self.handler.lock() {
            let settings = handler.get_filter_settings();
            if handler.is_source_app_excluded_cached(&source, &settings) {
                debug!(
                    "Clipboard change ignored (source app excluded: {:?})",
                    source.as_ref().map(|s| &s.app_name)
                );
                return CallbackResult::Next;
            }
        }

        // 读取剪贴板内容（带重试，应对剪贴板锁竞争）
        let content = match read() {
            Some(c) => c,
            None => {
                self.retry_pending = true;
                return CallbackResult::Next;
            }
        };

        // 检查内容类型 + 处理内容（单次加锁，使用缓存设置）
        if let Some(ref handler) = *self.handler.lock() {
            let settings = handler.get_filter_settings();
            if !handler.is_content_type_allowed_cached(&content, &settings) {
                debug!("Clipboard change ignored (content type not allowed)");
                return CallbackResult::Next;
            }
            if handler.is_content_excluded_by_rules_cached(&content, &settings) {
                debug!("剪贴板变化已忽略（内容被过滤规则排除）");
                return CallbackResult::Next;
            }
            match handler.process(content, source) {
                Ok(Some(id)) => {
                    debug!("Processed clipboard item: {}", id);
                    // emit may wait for the GUI runtime. Clipboard recording must
                    // not stop inside WM_CLIPBOARDUPDATE while a webview is created.
                    let app = self.app_handle.clone();
                    if let Err(error) = self.app_handle.run_on_main_thread(move || {
                        let _ = app.emit("clipboard-updated", id);
                    }) {
                        warn!("Failed to dispatch clipboard update: {}", error);
                    }
                }
                Ok(None) => {
                    debug!("Clipboard content already exists");
                }
                Err(e) => {
                    error!("Failed to process clipboard: {}", e);
                    self.retry_pending = true;
                }
            }
        }

        CallbackResult::Next
    }
}

impl CMHandler for MonitorHandler {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        self.process_change(
            super::source_app::get_clipboard_source_app,
            read_clipboard_content_with_retry,
        )
    }

    fn on_clipboard_error(&mut self, error: std::io::Error) -> CallbackResult {
        error!("Clipboard error: {}", error);
        CallbackResult::Next
    }
}

/// 带重试的剪贴板读取，应对剪贴板锁竞争（如截图工具延迟渲染）
fn read_clipboard_content_with_retry() -> Option<ClipboardContent> {
    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_MS: u64 = 50;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                RETRY_DELAY_MS * attempt as u64,
            ));
            debug!("Clipboard read retry {}/{}", attempt + 1, MAX_RETRIES);
        }

        match read_clipboard_content() {
            Some(content) => return Some(content),
            None if attempt + 1 < MAX_RETRIES => {
                debug!("Clipboard read returned nothing, will retry");
                continue;
            }
            None => {
                warn!("Clipboard read failed after {} attempts", MAX_RETRIES);
                return None;
            }
        }
    }
    None
}

/// 读取当前剪贴板内容（单次尝试）
fn read_clipboard_content() -> Option<ClipboardContent> {
    use clipboard_rs::common::RustImage;
    use clipboard_rs::{Clipboard, ClipboardContext};

    let ctx = match ClipboardContext::new() {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "Failed to create clipboard context: {} (clipboard may be locked by another app)",
                e
            );
            return None;
        }
    };

    // 优先尝试获取文件
    match ctx.get_files() {
        Ok(files) if !files.is_empty() => {
            debug!("Got {} files from clipboard", files.len());
            return if is_video_files(&files) {
                Some(ClipboardContent::Video(files))
            } else {
                Some(ClipboardContent::Files(files))
            };
        }
        Ok(_) => {} // 空文件列表，继续尝试其他格式
        Err(e) => debug!("Clipboard get_files failed: {}", e),
    }

    // 尝试获取图片
    match ctx.get_image() {
        Ok(img) => {
            let (width, height) = img.get_size();
            debug!("Got image from clipboard: {}x{}", width, height);

            match img.to_png() {
                Ok(png_buffer) => {
                    let bytes: Vec<u8> = png_buffer.get_bytes().to_vec();
                    debug!("Got PNG image: {} bytes", bytes.len());
                    return Some(ClipboardContent::Image(bytes));
                }
                Err(e) => warn!("Failed to convert clipboard image to PNG: {}", e),
            }
        }
        Err(e) => debug!(
            "Clipboard get_image failed: {} (may not contain image data or format unsupported)",
            e
        ),
    }

    // 尝试获取 HTML
    if ctx.has(clipboard_rs::ContentFormat::Html) {
        match ctx.get_html() {
            Ok(html) if !html.is_empty() => {
                let text = ctx.get_text().ok().filter(|t| !t.is_empty());
                debug!("Got HTML from clipboard: {} bytes", html.len());
                return Some(ClipboardContent::Html { html, text });
            }
            Ok(_) => {}
            Err(e) => debug!("Clipboard get_html failed: {}", e),
        }
    }

    // 尝试获取 RTF 富文本
    if ctx.has(clipboard_rs::ContentFormat::Rtf) {
        match ctx.get_rich_text() {
            Ok(rtf) if !rtf.is_empty() => {
                let text = ctx.get_text().ok().filter(|t| !t.is_empty());
                debug!("Got RTF from clipboard: {} bytes", rtf.len());
                return Some(ClipboardContent::Rtf { rtf, text });
            }
            Ok(_) => {}
            Err(e) => debug!("Clipboard get_rich_text failed: {}", e),
        }
    }

    // 尝试获取纯文本
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => match clipboard.get_text() {
            Ok(text) if !text.is_empty() => {
                return Some(ClipboardContent::Text(text));
            }
            Ok(_) => debug!("Clipboard text is empty"),
            Err(e) => debug!("Clipboard get_text failed: {}", e),
        },
        Err(e) => warn!("Failed to create arboard clipboard: {}", e),
    }

    debug!("No recognizable content in clipboard");
    None
}
