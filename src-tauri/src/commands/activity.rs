use std::sync::atomic::{AtomicBool, Ordering};

// Production recovery relies on destructors and catch_unwind, not process abort.
#[cfg(all(not(debug_assertions), panic = "abort"))]
compile_error!("Clipboard worker recovery requires panic = unwind in release builds");

/// Non-queuing single-flight guard. Repeated hotkeys must not spawn unbounded work.
pub(crate) struct ActivityGuard(&'static AtomicBool);

impl ActivityGuard {
    pub(crate) fn acquire(active: &'static AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(active))
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_recovers_after_worker_panic_or_unstarted_worker_drop() {
        static ACTIVE: AtomicBool = AtomicBool::new(false);
        let guard = ActivityGuard::acquire(&ACTIVE).unwrap();
        let worker = move || { let _guard = guard; panic!("injected worker panic"); };
        assert!(std::panic::catch_unwind(worker).is_err());
        let guard = ActivityGuard::acquire(&ACTIVE).unwrap();
        let unstarted = move || drop(guard);
        drop(unstarted);
        assert!(ActivityGuard::acquire(&ACTIVE).is_some());
    }

    #[test]
    fn single_flight_rejects_overlap_and_recovers_after_error() {
        static ACTIVE: AtomicBool = AtomicBool::new(false);
        let operation = || -> Result<(), ()> {
            let _guard = ActivityGuard::acquire(&ACTIVE).unwrap();
            assert!(ActivityGuard::acquire(&ACTIVE).is_none());
            Err(())
        };
        assert!(operation().is_err());
        assert!(ActivityGuard::acquire(&ACTIVE).is_some());
    }
}
