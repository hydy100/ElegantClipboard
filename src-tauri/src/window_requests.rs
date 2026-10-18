//! End-to-end supervision of interactive window requests, including queue time.
use parking_lot::{Condvar, Mutex};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, LazyLock, OnceLock, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Manager;
use tokio::sync::Notify;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
static REQUESTS: LazyLock<Mutex<HashMap<u64, Arc<Request>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static QUEUES: LazyLock<Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct Watch {
    request: Weak<Request>,
    app: tauri::AppHandle,
    deadline: Instant,
}
static WATCHES: Mutex<Vec<Watch>> = Mutex::new(Vec::new());
static WATCHDOG: OnceLock<Result<(), String>> = OnceLock::new();

fn watch(app: &tauri::AppHandle, request: &Arc<Request>, timeout: Duration) -> Result<(), String> {
    WATCHDOG
        .get_or_init(|| {
            std::thread::Builder::new()
                .name("window-operation-watchdog".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                        let mut expired = Vec::new();
                        WATCHES.lock().retain(|watch| {
                            let Some(request) = watch.request.upgrade() else {
                                return false;
                            };
                            if request.result.lock().is_some() {
                                return false;
                            }
                            if Instant::now() >= watch.deadline {
                                expired.push((request, watch.app.clone(), watch.deadline));
                                return false;
                            }
                            true
                        });
                        for (request, app, deadline) in expired {
                            if request.expire(deadline, Instant::now()) {
                                crate::webview_runtime::request_recovery(
                                    &app,
                                    &format!("operation_timeout:{}", request.label),
                                );
                            }
                        }
                    }
                })
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .clone()?;
    WATCHES.lock().push(Watch {
        request: Arc::downgrade(request),
        app: app.clone(),
        deadline: Instant::now() + timeout,
    });
    Ok(())
}

struct Request {
    label: String,
    generation: AtomicUsize,
    result: Mutex<Option<Result<(), String>>>,
    completed: Condvar,
    changed: Notify,
}

impl Request {
    fn new(label: String) -> Self {
        Self {
            label,
            generation: AtomicUsize::new(0),
            result: Mutex::new(None),
            completed: Condvar::new(),
            changed: Notify::new(),
        }
    }

    fn finish(&self, result: Result<(), String>) -> bool {
        let mut current = self.result.lock();
        if current.is_some() {
            return false;
        }
        *current = Some(result);
        self.completed.notify_all();
        self.changed.notify_waiters();
        true
    }

    fn expire(&self, deadline: Instant, now: Instant) -> bool {
        now >= deadline && self.finish(Err(format!("窗口或操作 {} 响应超时，正在恢复", self.label)))
    }

    async fn wait(&self) -> Result<(), String> {
        loop {
            // Enable before checking state, so an acknowledgment cannot be lost.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.result.lock().clone() {
                return result;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn wait_deadline(&self, timeout: Duration) -> bool {
        let mut result = self.result.lock();
        let deadline = std::time::Instant::now() + timeout;
        while result.is_none() {
            if self.completed.wait_until(&mut result, deadline).timed_out() && result.is_none() {
                *result = Some(Err(format!("窗口 {} 打开或响应超时，正在恢复", self.label)));
                self.changed.notify_waiters();
                return true;
            }
        }
        false
    }
}

struct Guard(u64, Arc<Request>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.1.finish(Err("窗口请求已结束".into()));
        REQUESTS.lock().remove(&self.0);
    }
}

/// Ordinary file/network errors return to the caller. Only a missing operation
/// completion or frontend acknowledgment triggers automatic recovery.
pub(crate) async fn run(
    app: &tauri::AppHandle,
    label: &str,
    payload: String,
    operation: impl Future<Output = Result<(), String>>,
) -> Result<(), String> {
    crate::webview_runtime::ensure_runtime_current(app)?;
    let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    let queue = {
        let mut queues = QUEUES.lock();
        queues.retain(|_, queue| queue.strong_count() > 0);
        let queue = queues
            .get(label)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
        queues.insert(label.into(), Arc::downgrade(&queue));
        queue
    };
    let request = Arc::new(Request::new(label.into()));
    {
        let mut requests = REQUESTS.lock();
        if requests
            .values()
            .filter(|request| request.label == label)
            .count()
            >= 4
        {
            return Err("该窗口正在处理请求，请稍后重试".into());
        }
        requests.insert(request_id, request.clone());
    }
    let _guard = Guard(request_id, request.clone());
    watch(app, &request, REQUEST_TIMEOUT)?;

    let deliver = async {
        let _ordered_delivery = queue.lock().await;
        crate::webview_runtime::ensure_runtime_current(app)?;
        operation.await?;
        crate::webview_runtime::wait_for_frontend_ready(label).await?;
        let app = app.clone();
        let label = label.to_string();
        let message = serde_json::json!({ "requestId": request_id, "payload": payload });
        let delivery_request = request.clone();
        crate::commands::run_blocking("deliver_window_request", move || {
            let window = app.get_webview_window(&label).ok_or("窗口已关闭")?;
            if let Err(error) = crate::webview_runtime::bind_window_request(&label, |generation| {
                delivery_request
                    .generation
                    .store(generation, Ordering::Release);
            }) {
                delivery_request.finish(Err(error.clone()));
                return Err(error);
            }
            window
                .eval(format!("window.__EC_HANDLE_OPEN_REQUEST__?.({message})"))
                .map_err(|error| {
                    crate::webview_runtime::window_operation_error(
                        &app,
                        &label,
                        "发送窗口请求失败",
                        error,
                    )
                })
        })
        .await?;
        request.wait().await
    };
    let mut deliver = Box::pin(deliver);
    let mut response = Box::pin(request.wait());
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(result) = deliver.as_mut().poll(cx) {
            return std::task::Poll::Ready(result);
        }
        response.as_mut().poll(cx)
    })
    .await
}

pub(crate) fn cancel_for_destroyed_window(label: &str, generation: usize) {
    for request in REQUESTS.lock().values() {
        if request.label == label
            && generation != 0
            && request.generation.load(Ordering::Acquire) == generation
        {
            request.finish(Err("窗口已关闭，请求已取消".into()));
        }
    }
}

pub(crate) struct OperationGuard(Arc<Request>);
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.finish(Ok(()));
    }
}

/// Covers work before a window request exists (native capture/selection), and
/// preview creation callers sharing the same serialized native window queue.
pub(crate) fn watch_operation(
    app: &tauri::AppHandle,
    label: &str,
) -> Result<OperationGuard, String> {
    let request = Arc::new(Request::new(label.into()));
    watch(app, &request, Duration::from_secs(60))?;
    Ok(OperationGuard(request))
}

#[tauri::command]
pub(crate) async fn window_request_ack(
    window: tauri::WebviewWindow,
    request_id: u64,
    error: Option<String>,
) -> Result<(), String> {
    let request = REQUESTS
        .lock()
        .get(&request_id)
        .cloned()
        .ok_or("窗口请求已过期")?;
    if request.label != window.label() {
        return Err("窗口请求不属于当前窗口".into());
    }
    if let Some(error) = error {
        request.finish(Err(format!("窗口处理失败: {error}")));
    } else {
        crate::commands::run_blocking("verify_window_visible", move || {
            match window.is_visible() {
                Ok(true) => {
                    request.finish(Ok(()));
                }
                Ok(false) => {
                    request.finish(Err("窗口未显示或已关闭".into()));
                }
                Err(error) => {
                    let error = crate::webview_runtime::window_operation_error(
                        window.app_handle(),
                        window.label(),
                        "确认窗口显示失败",
                        error,
                    );
                    request.finish(Err(error));
                }
            }
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ack_times_out_but_completed_and_cancelled_requests_do_not() {
        let pending = Request::new("settings".into());
        assert!(pending.wait_deadline(Duration::from_millis(2)));
        assert!(
            !pending.finish(Ok(())),
            "late ack must not override failure"
        );
        for result in [Ok(()), Err("closed".into())] {
            let request = Request::new("settings".into());
            assert!(request.finish(result));
            assert!(!request.wait_deadline(Duration::ZERO));
        }
    }

    #[test]
    fn ack_before_wait_is_not_lost_and_requests_are_independent() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let first = Request::new("ocr-result".into());
            let second = Request::new("ocr-result".into());
            first.finish(Ok(()));
            assert!(first.wait().await.is_ok());
            assert!(second.wait_deadline(Duration::from_millis(2)));
            assert!(second.wait().await.is_err());
        });
    }

    #[test]
    fn native_watchdog_is_independent_of_a_blocked_async_operation() {
        let request = Arc::new(Request::new("queue".into()));
        let watch = request.clone();
        let thread = std::thread::spawn(move || watch.wait_deadline(Duration::from_millis(5)));
        assert!(thread.join().unwrap());
        assert!(request.result.lock().as_ref().unwrap().is_err());
    }

    #[test]
    fn supervision_tick_expires_only_incomplete_requests_at_the_deadline() {
        let deadline = Instant::now() + Duration::from_secs(45);
        let pending = Request::new("queue".into());
        assert!(!pending.expire(deadline, deadline - Duration::from_nanos(1)));
        assert!(pending.expire(deadline, deadline));
        assert!(!pending.expire(deadline, deadline + Duration::from_secs(1)));
        for result in [Ok(()), Err("cancelled".into())] {
            let finished = Request::new("settings".into());
            finished.finish(result);
            assert!(!finished.expire(deadline, deadline + Duration::from_secs(60)));
        }
    }

    #[test]
    fn destroying_a_window_cancels_only_its_delivered_generation() {
        let label = format!("test-{}", uuid::Uuid::new_v4());
        let old = Arc::new(Request::new(label.clone()));
        old.generation.store(10, Ordering::Release);
        let queued = Arc::new(Request::new(label.clone()));
        let newer = Arc::new(Request::new(label.clone()));
        newer.generation.store(20, Ordering::Release);
        let ids: Vec<_> = (0..3)
            .map(|_| NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
            .collect();
        for (id, request) in ids.iter().zip([old.clone(), queued.clone(), newer.clone()]) {
            REQUESTS.lock().insert(*id, request);
        }
        cancel_for_destroyed_window(&label, 10);
        assert!(old.result.lock().as_ref().unwrap().is_err());
        assert!(queued.result.lock().is_none());
        assert!(newer.result.lock().is_none());
        for id in ids {
            REQUESTS.lock().remove(&id);
        }
    }

    #[test]
    fn abandoned_async_waiter_does_not_release_a_running_native_creation_lock() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let queue = Arc::new(tokio::sync::Mutex::new(()));
            let guard = queue.clone().lock_owned().await;
            let (started, has_started) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let native = std::thread::spawn(move || {
                let _guard = guard;
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(2)).unwrap();
            });
            has_started.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(queue.try_lock().is_err());
            release.send(()).unwrap();
            native.join().unwrap();
            assert!(queue.try_lock().is_ok());
        });
    }
}
