//! Native worker telemetry and deterministic supervision policy. No clipboard,
//! database, UI, or wall-clock access: callers supply observations and time.
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const STALL_GRACE: Duration = Duration::from_secs(60);
const RESTART_WINDOW: Duration = Duration::from_secs(10 * 60);
const HEALTHY_WINDOW: Duration = Duration::from_secs(60);
const MAX_PROGRESS_SAMPLE_GAP: Duration = Duration::from_secs(5);
const MAX_REPLACEMENTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestartDecision {
    Replace,
    Recover,
    Fused,
}

/// Separate from per-worker stall policy: replacement must not reset the budget.
/// Attempts (including failed spawns) are bounded. Once fused, only a new monitor
/// session/application recovery can re-arm it, not elapsed time or late progress.
#[derive(Default)]
pub(super) struct RestartBudget {
    attempts: VecDeque<Instant>,
    previous_progress: Option<(Instant, u64)>,
    healthy_since: Option<Instant>,
    fused: bool,
}

impl RestartBudget {
    pub fn observe_progress(&mut self, now: Instant, alive: bool, loops: u64) {
        if self.fused {
            return;
        }
        if !alive {
            self.previous_progress = None;
            self.healthy_since = None;
            return;
        }
        let progressing = self.previous_progress.is_some_and(|(previous, count)| {
            loops != count && now.saturating_duration_since(previous) <= MAX_PROGRESS_SAMPLE_GAP
        });
        self.previous_progress = Some((now, loops));
        if !progressing {
            self.healthy_since = None;
            return;
        }
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= HEALTHY_WINDOW {
            self.attempts.clear();
        }
    }

    /// Call only after verifying the old worker is finished (or absent).
    pub fn replacement(&mut self, now: Instant) -> RestartDecision {
        if self.fused {
            return RestartDecision::Fused;
        }
        // A replacement breaks the continuous healthy-worker observation window.
        self.previous_progress = None;
        self.healthy_since = None;
        while self
            .attempts
            .front()
            .is_some_and(|attempt| now.saturating_duration_since(*attempt) >= RESTART_WINDOW)
        {
            self.attempts.pop_front();
        }
        if self.attempts.len() >= MAX_REPLACEMENTS {
            self.fused = true;
            return RestartDecision::Recover;
        }
        self.attempts.push_back(now);
        RestartDecision::Replace
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub loops: u64,
    pub entered: u64,
    pub completed: u64,
    pub acknowledged: u32,
    pub callback_sequence: u32,
}

#[derive(Default)]
pub(super) struct Health(Mutex<Snapshot>);

impl Health {
    pub fn new(baseline: u32) -> Self {
        Self(Mutex::new(Snapshot {
            acknowledged: baseline,
            ..Snapshot::default()
        }))
    }

    pub fn snapshot(&self) -> Snapshot {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn progress(&self, acknowledged: u32) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.loops = state.loops.wrapping_add(1);
        state.acknowledged = acknowledged;
    }

    /// Publish consumption (including paused skips) before the next fallible
    /// listener/sample operation. This is the Windows restart checkpoint, not
    /// a successful-insert heartbeat; failed reads must never advance it.
    pub fn acknowledge(&self, sequence: u32) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .acknowledged = sequence;
    }

    pub fn callback(&self, sequence: u32) -> Completion<'_> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.entered = state.entered.wrapping_add(1);
        state.callback_sequence = sequence;
        Completion(self)
    }
}

pub(super) struct Completion<'a>(&'a Health);
impl Drop for Completion<'_> {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        state.completed = state.completed.wrapping_add(1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Stopped,
    Restart,
    Paused,
    SequenceUnavailable,
    Idle,
    Progress,
    Grace,
    Recover,
    RecoveryRequested,
}

pub(super) struct Supervisor {
    previous: Snapshot,
    evidence_since: Option<Instant>,
    recovery_requested: bool,
}

impl Supervisor {
    pub fn new(previous: Snapshot) -> Self {
        Self {
            previous,
            evidence_since: None,
            recovery_requested: false,
        }
    }

    pub fn observe(
        &mut self,
        now: Instant,
        wanted: bool,
        finished: bool,
        paused: bool,
        sequence: u32,
        state: Snapshot,
    ) -> Status {
        let progressed = state != self.previous;
        self.previous = state;
        if !wanted {
            self.evidence_since = None;
            return Status::Stopped;
        }
        // Only a joined/terminated worker can be replaced, even while paused.
        if finished {
            return Status::Restart;
        }
        if paused || sequence == 0 {
            self.evidence_since = None;
            return if paused {
                Status::Paused
            } else {
                Status::SequenceUnavailable
            };
        }
        let in_callback = state.entered != state.completed;
        let evidence =
            sequence != state.acknowledged || (in_callback && state.callback_sequence != 0);
        if progressed || !evidence {
            self.evidence_since = None;
            return if progressed {
                Status::Progress
            } else {
                Status::Idle
            };
        }
        if self.recovery_requested {
            return Status::RecoveryRequested;
        }
        let since = *self.evidence_since.get_or_insert(now);
        if now.saturating_duration_since(since) < STALL_GRACE {
            return Status::Grace;
        }
        self.recovery_requested = true;
        Status::Recover
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_budget_allows_three_attempts_then_recovers_once_and_fuses() {
        let t = Instant::now();
        let mut budget = RestartBudget::default();
        for n in 0..3 {
            assert_eq!(
                budget.replacement(t + Duration::from_secs(n)),
                RestartDecision::Replace
            );
        }
        assert_eq!(
            budget.replacement(t + Duration::from_secs(3)),
            RestartDecision::Recover
        );
        assert_eq!(
            budget.replacement(t + RESTART_WINDOW * 2),
            RestartDecision::Fused
        );
        // Neither late progress nor time re-arms an application recovery fuse.
        for n in 0..100 {
            budget.observe_progress(t + RESTART_WINDOW * 3 + Duration::from_secs(n), true, n);
        }
        assert_eq!(
            budget.replacement(t + RESTART_WINDOW * 4),
            RestartDecision::Fused
        );
    }

    #[test]
    fn restart_budget_is_rolling_and_expires_at_exact_ten_minute_boundary() {
        let t = Instant::now();
        let mut budget = RestartBudget::default();
        for n in 0..3 {
            assert_eq!(
                budget.replacement(t + Duration::from_secs(n)),
                RestartDecision::Replace
            );
        }
        assert_eq!(
            budget.replacement(t + RESTART_WINDOW),
            RestartDecision::Replace
        );
        // Only the oldest attempt expired, not all three.
        assert_eq!(
            budget.replacement(t + RESTART_WINDOW),
            RestartDecision::Recover
        );
    }

    #[test]
    fn sustained_healthy_loop_progress_resets_restart_attempts() {
        let t = Instant::now();
        let mut budget = RestartBudget::default();
        for _ in 0..3 {
            assert_eq!(budget.replacement(t), RestartDecision::Replace);
        }
        for n in 0..=61 {
            // Idle/paused workers still advance their loop counter; no insert
            // or callback is needed to establish continuous worker health.
            budget.observe_progress(t + Duration::from_secs(n), true, n);
        }
        for _ in 0..3 {
            assert_eq!(
                budget.replacement(t + Duration::from_secs(62)),
                RestartDecision::Replace
            );
        }
        assert_eq!(
            budget.replacement(t + Duration::from_secs(62)),
            RestartDecision::Recover
        );
    }

    #[test]
    fn brief_progress_or_long_observation_gap_does_not_reset_attempts() {
        let t = Instant::now();
        let mut budget = RestartBudget::default();
        for _ in 0..3 {
            assert_eq!(budget.replacement(t), RestartDecision::Replace);
        }
        budget.observe_progress(t, true, 1);
        budget.observe_progress(t + Duration::from_secs(1), true, 2);
        budget.observe_progress(t + Duration::from_secs(90), true, 100);
        assert_eq!(
            budget.replacement(t + Duration::from_secs(91)),
            RestartDecision::Recover
        );
    }

    #[test]
    fn exit_or_stalled_loop_breaks_continuous_healthy_window() {
        for exit in [false, true] {
            let t = Instant::now();
            let mut budget = RestartBudget::default();
            for _ in 0..3 {
                assert_eq!(budget.replacement(t), RestartDecision::Replace);
            }
            for n in 0..=40 {
                budget.observe_progress(t + Duration::from_secs(n), true, n);
            }
            budget.observe_progress(t + Duration::from_secs(41), !exit, 40);
            for n in 42..=82 {
                budget.observe_progress(t + Duration::from_secs(n), true, n);
            }
            assert_eq!(
                budget.replacement(t + Duration::from_secs(83)),
                RestartDecision::Recover
            );
        }
    }

    #[test]
    fn acknowledgement_checkpoint_does_not_forge_loop_or_callback_progress() {
        let health = Health::new(10);
        health.acknowledge(11);
        assert_eq!(
            health.snapshot(),
            Snapshot {
                acknowledged: 11,
                ..Snapshot::default()
            }
        );
    }

    fn poll(s: &mut Supervisor, t: Instant, sequence: u32, state: Snapshot) -> Status {
        s.observe(t, true, false, false, sequence, state)
    }

    #[test]
    fn idle_and_unavailable_are_not_stalls() {
        let t = Instant::now();
        let state = Snapshot {
            acknowledged: 10,
            ..Snapshot::default()
        };
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 10, state), Status::Idle);
        assert_eq!(poll(&mut s, t + STALL_GRACE * 20, 10, state), Status::Idle);
        assert_eq!(
            poll(&mut s, t + STALL_GRACE * 30, 0, state),
            Status::SequenceUnavailable
        );
    }

    #[test]
    fn blocked_callback_requires_full_grace_and_recovers_only_once() {
        let t = Instant::now();
        let state = Snapshot {
            entered: 1,
            callback_sequence: 11,
            acknowledged: 10,
            ..Snapshot::default()
        };
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 11, state), Status::Grace);
        assert_eq!(
            poll(
                &mut s,
                t + STALL_GRACE - Duration::from_millis(1),
                11,
                state
            ),
            Status::Grace
        );
        assert_eq!(poll(&mut s, t + STALL_GRACE, 11, state), Status::Recover);
        assert_eq!(
            poll(&mut s, t + STALL_GRACE * 2, 12, state),
            Status::RecoveryRequested
        );
    }

    #[test]
    fn stalled_loop_requires_pending_sequence_evidence_including_wraparound() {
        let t = Instant::now();
        let state = Snapshot {
            acknowledged: u32::MAX,
            ..Snapshot::default()
        };
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 1, state), Status::Grace);
        assert_eq!(poll(&mut s, t + STALL_GRACE, 1, state), Status::Recover);
    }

    #[test]
    fn pause_and_unavailable_reset_evidence_grace() {
        let t = Instant::now();
        let state = Snapshot::default();
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 11, state), Status::Grace);
        assert_eq!(
            s.observe(t + STALL_GRACE, true, false, true, 11, state),
            Status::Paused
        );
        assert_eq!(poll(&mut s, t + STALL_GRACE * 2, 11, state), Status::Grace);
        assert_eq!(
            poll(&mut s, t + STALL_GRACE * 3, 0, state),
            Status::SequenceUnavailable
        );
        assert_eq!(poll(&mut s, t + STALL_GRACE * 4, 11, state), Status::Grace);
    }

    #[test]
    fn loop_progress_and_completed_filtered_or_duplicate_callbacks_are_healthy() {
        let t = Instant::now();
        let health = Health::new(10);
        let mut s = Supervisor::new(health.snapshot());
        for n in 0..100 {
            // No database insert, including repeated read failures: completion
            // and loop progress alone establish that the reader is not blocked.
            {
                let _completion = health.callback(11);
            }
            health.progress(10);
            assert_eq!(
                poll(&mut s, t + STALL_GRACE * n, 11, health.snapshot()),
                Status::Progress
            );
        }
        assert_eq!(health.snapshot().entered, health.snapshot().completed);
    }

    #[test]
    fn unwind_records_callback_completion() {
        let health = Health::new(10);
        let _ = std::panic::catch_unwind(|| {
            let _completion = health.callback(11);
            panic!("injected callback failure");
        });
        assert_eq!(health.snapshot().entered, 1);
        assert_eq!(health.snapshot().completed, 1);
    }

    #[test]
    fn progress_just_before_deadline_requires_a_new_full_grace() {
        let t = Instant::now();
        let mut state = Snapshot::default();
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 11, state), Status::Grace);
        state.loops += 1;
        assert_eq!(poll(&mut s, t + STALL_GRACE, 11, state), Status::Progress);
        assert_eq!(poll(&mut s, t + STALL_GRACE * 2, 11, state), Status::Grace);
        assert_eq!(
            poll(&mut s, t + STALL_GRACE * 3, 11, state),
            Status::Recover
        );
    }

    #[test]
    fn in_flight_callback_is_evidence_even_if_sequence_matches_acknowledged() {
        let t = Instant::now();
        let state = Snapshot {
            acknowledged: 11,
            entered: 1,
            callback_sequence: 11,
            ..Snapshot::default()
        };
        let mut s = Supervisor::new(state);
        assert_eq!(poll(&mut s, t, 11, state), Status::Grace);
        assert_eq!(poll(&mut s, t + STALL_GRACE, 11, state), Status::Recover);
    }

    #[test]
    fn exit_restarts_but_intentional_stop_never_does() {
        let t = Instant::now();
        let state = Snapshot::default();
        let mut s = Supervisor::new(state);
        assert_eq!(s.observe(t, true, true, true, 0, state), Status::Restart);
        assert_eq!(s.observe(t, false, true, false, 11, state), Status::Stopped);
        assert_eq!(
            s.observe(t, false, false, false, 11, state),
            Status::Stopped
        );
    }
}
