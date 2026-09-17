//! Selection acquisition without changing focus or treating old clipboard text as selection.
use std::time::{Duration, Instant};

const UIA_INITIAL_WAIT: Duration = Duration::from_millis(350);
const UIA_WARM_WAIT: Duration = Duration::from_millis(20);
const ROUTE_TTL: Duration = Duration::from_secs(30);
const ROUTE_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyShortcut {
    Copy,
    Insert,
}

fn copy_plan(preferred: CopyShortcut) -> [(CopyShortcut, Duration); 2] {
    let other = match preferred {
        CopyShortcut::Copy => CopyShortcut::Insert,
        CopyShortcut::Insert => CopyShortcut::Copy,
    };
    [
        (preferred, Duration::from_millis(1000)),
        (other, Duration::from_millis(500)),
    ]
}

// Cache only successful routes, never text. PID disambiguates reused HWNDs.
// Short expiry allows changed pages to relearn. No disk persistence.
#[derive(Default)]
struct CopyRoutes(std::collections::VecDeque<((isize, u32), Instant, CopyShortcut)>);

impl CopyRoutes {
    fn preferred(&mut self, key: (isize, u32), now: Instant) -> Option<CopyShortcut> {
        self.0
            .retain(|(_, seen, _)| now.saturating_duration_since(*seen) < ROUTE_TTL);
        self.0
            .iter()
            .find(|(cached, _, _)| *cached == key)
            .map(|(_, _, shortcut)| *shortcut)
    }

    fn remember(&mut self, key: (isize, u32), now: Instant, copied: Option<CopyShortcut>) {
        self.0.retain(|(cached, seen, _)| {
            *cached != key && now.saturating_duration_since(*seen) < ROUTE_TTL
        });
        if let Some(shortcut) = copied {
            if self.0.len() >= ROUTE_CAPACITY {
                self.0.pop_front();
            }
            self.0.push_back((key, now, shortcut));
        }
    }
}

fn initial_uia_wait(copy_preferred: Option<CopyShortcut>) -> Duration {
    if copy_preferred.is_some() {
        UIA_WARM_WAIT
    } else {
        UIA_INITIAL_WAIT
    }
}

/// Poll until text is actually readable, not merely until a sequence number changes.
/// Empty/delayed-rendered clipboard formats and temporary open failures are retried.
fn poll_text(
    timeout: Duration,
    mut read: impl FnMut() -> Result<Option<String>, String>,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(text) = read()? {
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        if Instant::now() >= deadline {
            return Ok(String::new());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(windows)]
pub(super) fn foreground() -> isize {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize }
}

/// UIA runs in its own MTA. At most one provider call may be outstanding, even if
/// an unresponsive application never returns. No COM object crosses the thread.
#[cfg(windows)]
fn start_accessible_selection(target: isize) -> Option<std::sync::mpsc::Receiver<Option<String>>> {
    use super::activity::ActivityGuard;
    use std::sync::atomic::AtomicBool;
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    let guard = ActivityGuard::acquire(&ACTIVE)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("selection-uia".into())
        .spawn(move || {
            let _guard = guard;
            let text = read_accessible_selection(target);
            let _ = tx.send(text);
        })
        .ok()?;
    Some(rx)
}

#[cfg(windows)]
fn wait_for_selection(
    probe: &Option<std::sync::mpsc::Receiver<Option<String>>>,
    timeout: Duration,
    target: isize,
) -> Option<String> {
    probe
        .as_ref()?
        .recv_timeout(timeout)
        .ok()
        .flatten()
        .filter(|text| !text.trim().is_empty() && foreground() == target)
}

/// A known copy-capable window does not repeatedly pay for a slow UIA provider.
/// UIA still runs concurrently and its late answer is retained if copying fails.
#[cfg(windows)]
pub(super) fn selected_text(state: &std::sync::Arc<super::AppState>) -> Result<String, String> {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetWindowThreadProcessId};
    static ROUTES: std::sync::LazyLock<parking_lot::Mutex<CopyRoutes>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(CopyRoutes::default()));
    let started = Instant::now();
    let target = foreground();
    let mut process_id = 0;
    unsafe {
        GetWindowThreadProcessId(HWND(target as *mut _), Some(&mut process_id));
    }
    let key = (target, process_id);
    let prefer_copy = ROUTES.lock().preferred(key, started);
    let probe = start_accessible_selection(target);
    if let Some(text) = wait_for_selection(&probe, initial_uia_wait(prefer_copy), target) {
        ROUTES.lock().remember(key, Instant::now(), None);
        tracing::info!(
            route = "uia",
            elapsed_ms = started.elapsed().as_millis(),
            "Selection acquired"
        );
        return Ok(text);
    }
    let copied = super::with_paused_monitor(state, || {
        copied_selection(target, prefer_copy.unwrap_or(CopyShortcut::Copy))
    });
    if let Ok((text, shortcut)) = &copied {
        if !text.trim().is_empty() {
            ROUTES.lock().remember(key, Instant::now(), Some(*shortcut));
            tracing::info!(
                route = "copy",
                ?prefer_copy,
                ?shortcut,
                elapsed_ms = started.elapsed().as_millis(),
                "Selection acquired"
            );
            return copied.map(|(text, _)| text);
        }
    }
    // Finish clipboard restoration before opening/focusing a result window.
    // Do not race the UI against an outstanding clipboard transaction.
    if let Some(text) = wait_for_selection(
        &probe,
        UIA_INITIAL_WAIT.saturating_sub(started.elapsed()),
        target,
    ) {
        ROUTES.lock().remember(key, Instant::now(), None);
        tracing::info!(
            route = "late_uia",
            elapsed_ms = started.elapsed().as_millis(),
            "Selection acquired"
        );
        return Ok(text);
    }
    ROUTES.lock().remember(key, Instant::now(), None);
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        "Selection acquisition completed without text"
    );
    copied.map(|(text, _)| text)
}

#[cfg(windows)]
fn read_accessible_selection(target: isize) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
    };
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        struct ComGuard;
        impl Drop for ComGuard {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _com = ComGuard;
        if foreground() != target {
            return None;
        }
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let element = automation.GetFocusedElement().ok()?;
        if element.CurrentIsPassword().ok()?.as_bool() {
            return None;
        }
        let pattern: IUIAutomationTextPattern =
            element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
        let ranges = pattern.GetSelection().ok()?;
        let count = ranges.Length().ok()?;
        let mut parts = Vec::new();
        for index in 0..count {
            let text = ranges.GetElement(index).ok()?.GetText(-1).ok()?.to_string();
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
        let text = parts.join("\n");
        (!text.is_empty() && foreground() == target).then_some(text)
    }
}

#[cfg(windows)]
fn send_copy(insert: bool) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        }
    }
    // Release both left/right variants, including RWin (omitted by the old path).
    let modifiers = [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LWIN,
        VK_RWIN,
    ];
    let releases: Vec<_> = modifiers
        .into_iter()
        .filter(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) < 0 })
        .map(|vk| key(vk, true))
        .collect();
    let size = std::mem::size_of::<INPUT>() as i32;
    if !releases.is_empty() {
        if unsafe { SendInput(&releases, size) } != releases.len() as u32 {
            return Err("释放快捷键修饰键失败，请检查目标应用权限级别".into());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let vk = if insert { VK_INSERT } else { VK_C };
    let keys = [
        key(VK_CONTROL, false),
        key(vk, false),
        key(vk, true),
        key(VK_CONTROL, true),
    ];
    if unsafe { SendInput(&keys, size) } != keys.len() as u32 {
        // Best-effort cleanup of partial injection, without leaving Ctrl pressed.
        unsafe {
            SendInput(&[key(vk, true), key(VK_CONTROL, true)], size);
        }
        return Err("发送复制按键失败，请检查目标应用权限级别".into());
    }
    Ok(())
}

#[cfg(windows)]
fn copied_selection(
    target: isize,
    preferred: CopyShortcut,
) -> Result<(String, CopyShortcut), String> {
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
    let sequence = || unsafe { GetClipboardSequenceNumber() };
    if foreground() != target {
        return Err("选中文字期间前台窗口已切换，请重新选择".into());
    }
    // Preserve the previous text behavior; never clear the clipboard to probe it.
    let backup = arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok());
    let before = sequence();
    let mut copied_sequence = before;
    for (shortcut, timeout) in copy_plan(preferred) {
        if foreground() != target {
            return Err("选中文字期间前台窗口已切换，请重新选择".into());
        }
        send_copy(shortcut == CopyShortcut::Insert)?;
        let text = poll_text(timeout, || {
            if foreground() != target {
                return Err("选中文字期间前台窗口已切换，请重新选择".into());
            }
            if sequence() == before {
                return Ok(None);
            }
            let reading_sequence = sequence();
            let text = arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok());
            if sequence() != reading_sequence {
                return Ok(None);
            }
            copied_sequence = reading_sequence;
            Ok(text)
        })?;
        if !text.is_empty() {
            // A user copy after our read wins: do not restore over a newer value.
            if sequence() == copied_sequence {
                if let Some(ref previous) = backup {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        if sequence() == copied_sequence {
                            let _ = cb.set_text(previous);
                        }
                    }
                }
            }
            return Ok((text, shortcut));
        }
        // Try the other shortcut only after the existing timeout, and never
        // after a non-text copy. Avoid racing delayed rendering with early retries.
        if sequence() != before {
            break;
        }
    }
    Ok((String::new(), preferred))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_copy_route_shortens_serial_uia_wait_and_expires() {
        let now = Instant::now();
        let mut routes = CopyRoutes::default();
        let key = (42, 100);
        assert_eq!(
            initial_uia_wait(routes.preferred(key, now)).as_millis(),
            350
        );
        routes.remember(key, now, Some(CopyShortcut::Copy));
        assert_eq!(initial_uia_wait(routes.preferred(key, now)).as_millis(), 20);
        assert!(routes.preferred((42, 101), now).is_none()); // HWND reused by another process
        assert!(routes.preferred(key, now + ROUTE_TTL).is_none());
    }

    #[test]
    fn route_cache_is_bounded_and_uia_success_clears_copy_preference() {
        let now = Instant::now();
        let mut routes = CopyRoutes::default();
        for window in 0..100 {
            routes.remember((window, 10), now, Some(CopyShortcut::Copy));
        }
        assert_eq!(routes.0.len(), ROUTE_CAPACITY);
        assert!(routes.preferred((0, 10), now).is_none());
        assert!(routes.preferred((99, 10), now).is_some());
        routes.remember((99, 10), now, None);
        assert!(routes.preferred((99, 10), now).is_none());
    }

    #[test]
    fn learned_insert_route_skips_failed_ctrl_c_without_shortening_timeouts() {
        let now = Instant::now();
        let key = (42, 100);
        let mut routes = CopyRoutes::default();
        let cold = copy_plan(CopyShortcut::Copy);
        assert_eq!(cold[1].0, CopyShortcut::Insert);
        assert_eq!(cold[0].1.as_millis(), 1000);
        routes.remember(key, now, Some(CopyShortcut::Insert));
        let warm = copy_plan(routes.preferred(key, now).unwrap());
        assert_eq!(warm[0].0, CopyShortcut::Insert);
        assert_eq!(warm[1].0, CopyShortcut::Copy); // recovery if the page changes
        assert_eq!(warm[0].1 + warm[1].1, Duration::from_millis(1500));
        assert_eq!(initial_uia_wait(routes.preferred(key, now)), UIA_WARM_WAIT);
    }

    #[test]
    fn learned_routes_reduce_policy_wait_without_cutting_copy_tolerance() {
        // Deterministic policy budgets, not measurements of an external app.
        // Assume slow UIA and only the indicated copy shortcut responds.
        for successful in [CopyShortcut::Copy, CopyShortcut::Insert] {
            let cold_wait = UIA_INITIAL_WAIT
                + if successful == CopyShortcut::Insert {
                    copy_plan(CopyShortcut::Copy)[0].1
                } else {
                    Duration::ZERO
                };
            let warm_wait = initial_uia_wait(Some(successful));
            assert_eq!(copy_plan(successful)[0].0, successful);
            assert!(warm_wait < cold_wait);
            println!(
                "POLICY_WAIT_MS shortcut={successful:?} baseline={} learned={} copy_read_budget=1500",
                cold_wait.as_millis(),
                warm_wait.as_millis()
            );
        }
    }

    #[test]
    fn ready_text_has_no_polling_delay() {
        let mut reads = 0;
        let text = poll_text(Duration::ZERO, || {
            reads += 1;
            Ok(Some("ready".into()))
        })
        .unwrap();
        assert_eq!(reads, 1);
        assert_eq!(text, "ready");
    }

    #[cfg(windows)]
    #[test]
    fn timed_out_uia_result_is_still_available_for_copy_failure() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        assert!(rx.recv_timeout(Duration::ZERO).is_err());
        tx.send(Some("late selection".to_string())).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::ZERO).unwrap().unwrap(),
            "late selection"
        );
    }

    #[test]
    fn retries_delayed_and_temporarily_unreadable_selection() {
        let mut attempts = 0;
        let result = poll_text(Duration::from_secs(1), || {
            attempts += 1;
            Ok(match attempts {
                1..=6 => None,
                7 => Some(String::new()),
                _ => Some("选中 text".into()),
            })
        })
        .unwrap();
        assert_eq!(result, "选中 text");
        assert_eq!(attempts, 8); // >100ms: regression for the old fixed one-shot delay
    }

    #[test]
    fn no_update_does_not_return_old_clipboard() {
        assert_eq!(poll_text(Duration::ZERO, || Ok(None)).unwrap(), "");
    }

    #[test]
    fn foreground_change_aborts_polling() {
        assert_eq!(
            poll_text(Duration::from_secs(1), || Err("focus changed".into())).unwrap_err(),
            "focus changed"
        );
    }
}
