//! A native, message-only listener with sequence-number reconciliation.
//! No WebView, window visibility, frontend subscription or Tauri IPC is needed.
use super::monitor_health::Health;
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
const SLOW_READ_RETRY: Duration = Duration::from_secs(2);

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
            .saturating_add(1);
        if consumed {
            self.acknowledged = sequence;
            self.pending = None;
        } else {
            // A busy/delayed-rendering provider may become readable without a
            // second copy event. Failure is not acknowledgement: retain this
            // sequence and retry slowly until success, pause, or a newer copy.
            if attempts == MAX_READ_ATTEMPTS {
                tracing::warn!(
                    sequence,
                    attempts,
                    "Clipboard still unavailable; switching to slow retries"
                );
            }
            let delay = if attempts >= MAX_READ_ATTEMPTS {
                SLOW_READ_RETRY
            } else {
                RECONCILE_INTERVAL
            };
            self.pending = Some((sequence, attempts, now + delay));
        }
    }
}

pub(super) fn run_supervised(
    running: &AtomicBool,
    health: &Health,
    baseline: u32,
    sample: impl FnMut() -> u32,
    paused: impl FnMut() -> bool,
    process: impl FnMut() -> bool,
) {
    run_with_health(
        running,
        baseline,
        sample,
        paused,
        process,
        Listener::new,
        Some(health),
    );
}

#[cfg(test)]
pub(super) fn run_with_factory(
    running: &AtomicBool,
    baseline: u32,
    sample: impl FnMut() -> u32,
    paused: impl FnMut() -> bool,
    process: impl FnMut() -> bool,
    create_listener: impl FnMut() -> Result<Listener, String>,
) {
    run_with_health(
        running,
        baseline,
        sample,
        paused,
        process,
        create_listener,
        None,
    );
}

fn run_with_health(
    running: &AtomicBool,
    baseline: u32,
    mut sample: impl FnMut() -> u32,
    mut paused: impl FnMut() -> bool,
    mut process: impl FnMut() -> bool,
    mut create_listener: impl FnMut() -> Result<Listener, String>,
    health: Option<&Health>,
) {
    let mut tracker = SequenceTracker::new(baseline);
    let mut listener = None;
    let mut register_due = Instant::now();
    while running.load(Ordering::Acquire) {
        if let Some(health) = health {
            health.progress(tracker.acknowledged);
        }
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
        let should_read = tracker.should_read(sequence, paused(), Instant::now());
        if let Some(health) = health {
            // Paused sequences are intentionally consumed without a callback.
            health.acknowledge(tracker.acknowledged);
        }
        if should_read {
            // A Rust panic in one item must not permanently stop future recording.
            let completion = health.map(|health| health.callback(sequence));
            let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut process))
                .unwrap_or_else(|_| {
                    tracing::error!("Clipboard item processing panicked; retrying");
                    false
                });
            drop(completion);
            tracker.finish(sequence, consumed, Instant::now());
            if let Some(health) = health {
                // Checkpoint before wait/re-registration/sample can unwind.
                health.acknowledge(tracker.acknowledged);
            }
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
    fn health_tracks_real_loop_entry_completion_and_caught_callback_panic() {
        let running = AtomicBool::new(true);
        let health = Health::new(10);
        let mut samples = 0;
        let mut registrations = 0;
        let mut callbacks = 0;
        run_with_health(
            &running,
            10,
            || {
                samples += 1;
                assert!(
                    samples <= 2,
                    "worker failed to stop after controlled callbacks"
                );
                let state = health.snapshot();
                assert_eq!(state.loops, samples);
                assert_eq!(state.entered, samples - 1);
                assert_eq!(state.completed, samples - 1);
                assert_eq!(state.acknowledged, 10);
                11
            },
            || false,
            || {
                callbacks += 1;
                let state = health.snapshot();
                assert_eq!(state.entered, callbacks);
                assert_eq!(state.completed, callbacks - 1);
                assert_eq!(state.callback_sequence, 11);
                if callbacks == 1 {
                    panic!("injected callback panic");
                }
                running.store(false, Ordering::Release);
                true // consumed/filtered/deduplicated, without a database
            },
            || {
                registrations += 1;
                Err("injected registration failure; no native listener".into())
            },
            Some(&health),
        );
        let state = health.snapshot();
        assert!(registrations >= 1); // scheduling delays may trigger registration retries
        assert_eq!(state.loops, 2);
        assert_eq!(state.entered, 2);
        assert_eq!(state.completed, 2);
        assert_eq!(state.acknowledged, 11); // published even when stop ends this iteration
    }

    #[test]
    fn health_tracks_unavailable_paused_and_idle_iterations_without_callbacks() {
        use std::cell::Cell;
        let running = AtomicBool::new(true);
        let health = Health::new(10);
        let samples = Cell::new(0);
        run_with_health(
            &running,
            10,
            || {
                let n = samples.get() + 1;
                samples.set(n);
                assert!(n <= 3);
                assert_eq!(health.snapshot().loops, n);
                if n == 3 {
                    // Sequence 11 was skipped while paused in iteration 2.
                    assert_eq!(health.snapshot().acknowledged, 11);
                    running.store(false, Ordering::Release);
                }
                if n == 1 { 0 } else { 11 }
            },
            || samples.get() == 2,
            || panic!("unavailable, paused, and idle sequences must not be read"),
            || Err("injected registration failure".into()),
            Some(&health),
        );
        let state = health.snapshot();
        assert_eq!(state.loops, 3);
        assert_eq!(state.entered, 0);
        assert_eq!(state.completed, 0);
        assert_eq!(state.acknowledged, 11);
    }

    #[test]
    fn worker_unwind_preserves_acknowledgement_and_retries_only_unconsumed_sequence() {
        for pending_failure in [false, true] {
            let running = AtomicBool::new(true);
            let health = Health::new(10);
            let mut samples = 0;
            let mut callbacks = 0;
            let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_with_health(
                    &running,
                    10,
                    || {
                        samples += 1;
                        match samples {
                            1 => 11,
                            2 if pending_failure => 12,
                            _ => panic!("injected worker exit outside callback catch"),
                        }
                    },
                    || false,
                    || {
                        callbacks += 1;
                        callbacks == 1 // acknowledge 11, but retain failed 12
                    },
                    || Err("injected registration failure".into()),
                    Some(&health),
                );
            }));
            assert!(exit.is_err());
            let checkpoint = health.snapshot();
            assert_eq!(checkpoint.acknowledged, 11);
            assert_eq!(checkpoint.entered, checkpoint.completed);

            // The production spawn_worker uses this same shared checkpoint.
            // With no pending failure, first resample 11 to prove no replay.
            let mut restart_samples = 0;
            let mut restart_callbacks = 0;
            run_with_health(
                &running,
                checkpoint.acknowledged,
                || {
                    restart_samples += 1;
                    assert!(restart_samples <= 2);
                    if restart_samples == 1 && !pending_failure {
                        11
                    } else {
                        12
                    }
                },
                || false,
                || {
                    restart_callbacks += 1;
                    assert_eq!(health.snapshot().callback_sequence, 12);
                    running.store(false, Ordering::Release);
                    true
                },
                || Err("injected registration failure".into()),
                Some(&health),
            );
            assert_eq!(restart_callbacks, 1);
            assert_eq!(health.snapshot().acknowledged, 12);
        }
    }

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
    fn failed_reads_back_off_without_abandoning_the_copy() {
        let mut now = Instant::now();
        let mut state = SequenceTracker::new(10);
        for _ in 0..MAX_READ_ATTEMPTS {
            assert!(state.should_read(11, false, now));
            state.finish(11, false, now);
            assert!(!state.should_read(11, false, now));
            now += RECONCILE_INTERVAL;
        }
        assert!(!state.should_read(11, false, now));
        now += SLOW_READ_RETRY;
        assert!(state.should_read(11, false, now));
        state.finish(11, true, now);
        assert!(!state.should_read(11, false, now));
        assert!(state.should_read(12, false, now));
        state.finish(12, true, now);
        assert!(!state.should_read(12, false, now));
    }

    #[test]
    fn prolonged_failure_does_not_overflow_or_block_new_copies() {
        let mut now = Instant::now();
        let mut state = SequenceTracker::new(10);
        for _ in 0..300 {
            assert!(state.should_read(11, false, now));
            state.finish(11, false, now);
            assert!(!state.should_read(11, false, now));
            now += SLOW_READ_RETRY;
        }
        assert!(state.should_read(12, false, now));
        state.finish(12, true, now);
        assert!(!state.should_read(12, false, now));
    }
}
