//! A native, message-only listener with sequence-number reconciliation.
//! No WebView, window visibility, frontend subscription or Tauri IPC is needed.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, WAIT_FAILED};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
const REGISTER_RETRY: Duration = Duration::from_secs(2);
const MAX_READ_ATTEMPTS: u8 = 5;

pub(super) struct Listener(HWND);

impl Listener {
    pub(super) fn new() -> Result<Self, String> {
        let listener = Self(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("ElegantClipboardMonitor"),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .map_err(|e| e.to_string())?,
        );
        // A failed registration still drops the just-created HWND on this thread.
        unsafe { AddClipboardFormatListener(listener.0) }.map_err(|e| e.to_string())?;
        Ok(listener)
    }

    fn wait(&self) -> Result<(), String> {
        unsafe {
            if MsgWaitForMultipleObjectsEx(
                None,
                RECONCILE_INTERVAL.as_millis() as u32,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            ) == WAIT_FAILED
            {
                return Err(windows::core::Error::from_win32().to_string());
            }
            let mut msg = MSG::default();
            // Bound draining so message floods cannot starve sequence reconciliation.
            for _ in 0..128 {
                if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    break;
                }
                if msg.message == WM_QUIT {
                    return Err("clipboard message loop quit".into());
                }
                if msg.message != WM_CLIPBOARDUPDATE {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Ok(())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveClipboardFormatListener(self.0);
            let _ = DestroyWindow(self.0);
        }
    }
}

struct SequenceTracker {
    acknowledged: u32,
    pending: Option<(u32, u8, Instant)>,
}

impl SequenceTracker {
    fn new(baseline: u32) -> Self {
        Self {
            acknowledged: baseline,
            pending: None,
        }
    }

    fn should_read(&mut self, sequence: u32, paused: bool, now: Instant) -> bool {
        // Zero can indicate that the current desktop's clipboard is unavailable.
        if sequence == 0 {
            return false;
        }
        if paused {
            self.acknowledged = sequence;
            self.pending = None;
            return false;
        }
        if sequence == self.acknowledged {
            return false;
        }
        if let Some((pending, _, due)) = self.pending {
            if pending == sequence {
                return now >= due;
            }
        }
        self.pending = Some((sequence, 0, now));
        true
    }

    fn finish(&mut self, sequence: u32, consumed: bool, now: Instant) {
        let attempts = self
            .pending
            .filter(|(seq, _, _)| *seq == sequence)
            .map(|(_, attempts, _)| attempts)
            .unwrap_or(0)
            + 1;
        if consumed || attempts >= MAX_READ_ATTEMPTS {
            self.acknowledged = sequence;
            self.pending = None;
            if !consumed {
                tracing::warn!(sequence, attempts, "Clipboard read retries exhausted");
            }
        } else {
            self.pending = Some((sequence, attempts, now + RECONCILE_INTERVAL));
        }
    }
}

pub(super) fn run(
    running: &AtomicBool,
    baseline: u32,
    sample: impl FnMut() -> u32,
    paused: impl FnMut() -> bool,
    process: impl FnMut() -> bool,
) {
    run_with_factory(running, baseline, sample, paused, process, Listener::new);
}

pub(super) fn run_with_factory(
    running: &AtomicBool,
    baseline: u32,
    mut sample: impl FnMut() -> u32,
    mut paused: impl FnMut() -> bool,
    mut process: impl FnMut() -> bool,
    mut create_listener: impl FnMut() -> Result<Listener, String>,
) {
    let mut tracker = SequenceTracker::new(baseline);
    let mut listener = None;
    let mut register_due = Instant::now();
    while running.load(Ordering::Acquire) {
        if listener.is_none() && Instant::now() >= register_due {
            match create_listener() {
                Ok(value) => {
                    listener = Some(value);
                    tracing::info!("Clipboard listener ready (independent of main window)");
                }
                Err(error) => {
                    tracing::warn!(%error, "Clipboard listener unavailable; sequence polling remains active")
                }
            }
            register_due = Instant::now() + REGISTER_RETRY;
        }
        let sequence = sample();
        if tracker.should_read(sequence, paused(), Instant::now()) {
            // A Rust panic in one item must not permanently stop future recording.
            let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut process))
                .unwrap_or_else(|_| {
                    tracing::error!("Clipboard item processing panicked; retrying");
                    false
                });
            tracker.finish(sequence, consumed, Instant::now());
        }
        if !running.load(Ordering::Acquire) {
            break;
        }
        if let Some(active) = &listener {
            if let Err(error) = active.wait() {
                tracing::warn!(%error, "Recreating clipboard listener");
                listener = None;
                register_due = Instant::now();
            }
        } else {
            std::thread::sleep(RECONCILE_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_sequence_does_not_wait_for_failed_older_item() {
        let now = Instant::now();
        let mut state = SequenceTracker::new(10);
        assert!(state.should_read(11, false, now));
        state.finish(11, false, now);
        assert!(!state.should_read(11, false, now));
        assert!(state.should_read(12, false, now));
        state.finish(12, true, now);
        assert!(!state.should_read(12, false, now));
    }

    #[test]
    fn processing_panic_does_not_stop_polling_worker() {
        let running = AtomicBool::new(true);
        let mut attempts = 0;
        run_with_factory(
            &running,
            10,
            || 11,
            || false,
            || {
                attempts += 1;
                if attempts == 1 {
                    panic!("injected clipboard processing panic");
                }
                running.store(false, Ordering::Release);
                true
            },
            || Err("injected registration failure".into()),
        );
        assert_eq!(attempts, 2);
    }

    #[test]
    fn missed_notification_is_reconciled_once_without_showing_a_window() {
        let now = Instant::now();
        let mut state = SequenceTracker::new(10);
        assert!(!state.should_read(10, false, now));
        assert!(state.should_read(11, false, now));
        state.finish(11, true, now);
        assert!(!state.should_read(11, false, now));
        assert!(state.should_read(12, false, now));
    }

    #[test]
    fn paused_changes_are_not_backfilled_and_unavailable_desktop_is_retried() {
        let now = Instant::now();
        let mut state = SequenceTracker::new(10);
        assert!(!state.should_read(0, false, now));
        assert!(!state.should_read(11, true, now));
        assert!(!state.should_read(11, false, now));
        assert!(state.should_read(12, false, now));
    }

    #[test]
    fn failed_reads_retry_without_another_copy_but_are_bounded() {
        let mut now = Instant::now();
        let mut state = SequenceTracker::new(10);
        for _ in 0..MAX_READ_ATTEMPTS {
            assert!(state.should_read(11, false, now));
            state.finish(11, false, now);
            assert!(!state.should_read(11, false, now));
            now += RECONCILE_INTERVAL;
        }
        assert!(!state.should_read(11, false, now));
        assert!(state.should_read(12, false, now));
        state.finish(12, true, now);
        assert!(!state.should_read(12, false, now));
    }
}
