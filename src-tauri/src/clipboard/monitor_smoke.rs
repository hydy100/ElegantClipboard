//! Desktop test only: real hidden WebView, listener, processing and SQLite;
//! injected sequence/text so neither the user's clipboard nor history is touched.
use super::*;
use crate::{
    commands::AppState,
    database::{ClipboardRepository, QueryOptions},
};
use std::time::{Duration, Instant};
use tauri::Manager;

impl ClipboardMonitor {
    pub(crate) fn verify_hidden_recording(
        app: AppHandle,
        images: std::path::PathBuf,
    ) -> Result<(), String> {
        let main = app.get_webview_window("main").ok_or("main missing")?;
        if main.is_visible().map_err(|e| e.to_string())? {
            return Err("main was shown".into());
        }
        let state = app.state::<Arc<AppState>>();
        let repo = ClipboardRepository::new(&state.db);
        let initial_count = repo
            .count(QueryOptions::default())
            .map_err(|e| e.to_string())?;
        let running = Arc::new(AtomicBool::new(true));
        let sequence = Arc::new(AtomicU32::new(100));
        let paused = Arc::new(AtomicBool::new(false));
        let registered = Arc::new(AtomicBool::new(false));
        let mut handler = MonitorHandler {
            running: running.clone(),
            pause_count: Arc::new(AtomicU32::new(0)),
            user_paused: paused.clone(),
            handler: Arc::new(Mutex::new(Some(ClipboardHandler::new(&state.db, images)))),
            app_handle: app.clone(),
            retry_pending: false,
        };
        let stop = running.clone();
        let seq = sequence.clone();
        let pause = paused.clone();
        let ready = registered.clone();
        let thread = std::thread::spawn(move || {
            let mut registrations = 0;
            let mut reads = 0;
            windows_monitor::run_with_factory(
                &stop,
                100,
                || seq.load(Ordering::Acquire),
                || pause.load(Ordering::Acquire),
                || {
                    let current = seq.load(Ordering::Acquire);
                    handler.process_change(
                        || None,
                        || {
                            reads += 1;
                            if reads == 1 {
                                None
                            }
                            // clipboard temporarily unavailable
                            else {
                                Some(ClipboardContent::Text(format!(
                                    "hidden-startup-fixture-{current}"
                                )))
                            }
                        },
                    );
                    !handler.retry_pending
                },
                || {
                    registrations += 1;
                    if registrations == 1 {
                        return Err("injected startup registration failure".into());
                    }
                    let listener = windows_monitor::Listener::new()?;
                    ready.store(true, Ordering::Release);
                    Ok(listener)
                },
            );
        });
        struct Worker(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for Worker {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
                if let Some(thread) = self.1.take() {
                    let _ = thread.join();
                }
            }
        }
        let _worker = Worker(running, Some(thread));
        let wait_count = |wanted| -> Result<(), String> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let actual = repo
                    .count(QueryOptions::default())
                    .map_err(|e| e.to_string())?;
                if actual == initial_count + wanted {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "expected {wanted} new records, got {}",
                        actual - initial_count
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        sequence.store(101, Ordering::Release);
        wait_count(1)?; // works while registration is failing, after a failed read
        let deadline = Instant::now() + Duration::from_secs(5);
        while !registered.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("listener registration did not recover".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        sequence.store(102, Ordering::Release);
        wait_count(2)?; // deliberately no notification: reconcile while hidden
        paused.store(true, Ordering::Release);
        sequence.store(103, Ordering::Release);
        std::thread::sleep(Duration::from_millis(600));
        paused.store(false, Ordering::Release);
        std::thread::sleep(Duration::from_millis(300));
        if repo
            .count(QueryOptions::default())
            .map_err(|e| e.to_string())?
            != initial_count + 2
        {
            return Err("paused clipboard change was backfilled".into());
        }
        sequence.store(104, Ordering::Release);
        wait_count(3)?;
        if main.is_visible().map_err(|e| e.to_string())? {
            return Err("recording showed the main window".into());
        }
        println!(
            "HIDDEN_STARTUP_PASS: 3 records before first show; registration recovered; failed read retried; missing notification reconciled; pause preserved"
        );
        Ok(())
    }
}
