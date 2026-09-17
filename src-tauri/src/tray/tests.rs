use super::*;
use crate::{clipboard::ClipboardMonitor, commands, database::Database};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs only when explicitly requested. No clipboard hook, hotkey, user DB,
/// autostart, network API or single-instance plugin is initialized.
#[test]
#[ignore = "desktop WebView2 smoke test; run alone with --test-threads=1"]
fn native_window_smoke() {
    let dir = std::env::temp_dir().join(format!("ec-window-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // This test runs alone in a dedicated process, before WebView2 starts.
    unsafe {
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir.join("webview"));
    }
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "com.aslant.elegant-clipboard.smoke".into();
    context.config_mut().app.windows.clear();
    let passed = Arc::new(AtomicBool::new(false));
    let passed_in_setup = passed.clone();
    let db_dir = dir.clone();
    let app = tauri::Builder::default()
        .any_thread()
        .setup(move |app| {
            app.manage(Arc::new(commands::AppState {
                db: Database::new(db_dir.join("clipboard.db"))?,
                monitor: ClipboardMonitor::new(),
            }));
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .visible(false)
            .build()?;
            let monitor_images = db_dir.join("monitor-images");
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // First prove recording while the main WebView has NEVER been shown.
                // Clipboard source is injected; no system clipboard data is read/written.
                let check = app.clone();
                if let Err(error) = commands::run_blocking("smoke_hidden_recording", move || {
                    ClipboardMonitor::verify_hidden_recording(check, monitor_images)
                }).await {
                    eprintln!("Hidden monitor smoke failed: {error}");
                    app.exit(4);
                    return;
                }
                // Exercise the actual tray handler on the event-loop thread.
                let menu_app = app.clone();
                app.run_on_main_thread(move || handle_menu_event(&menu_app, "settings"))
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    let check = app.clone();
                    if commands::run_blocking("smoke_settings_ready", move || {
                        Ok(check.get_webview_window("settings").is_some())
                    })
                    .await
                    .unwrap()
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        app.exit(1);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                // Two simultaneous requests must serialize creation, not occupy
                // all runtime workers or nest WebView2 initialization callbacks.
                let translate_app = app.clone();
                let translate = tauri::async_runtime::spawn(async move {
                    commands::translate::open_translate_result_window(
                        translate_app,
                        "fixture selection".into(),
                    )
                    .await
                });
                let ocr =
                    commands::ocr::open_ocr_result_window(app.clone(), "fixture OCR".into()).await;
                let translation = translate.await.unwrap();
                if ocr.is_err() || translation.is_err() {
                    app.exit(2);
                    return;
                }
                let check = app.clone();
                let result = commands::run_blocking("smoke_window_roundtrip", move || {
                    for label in ["settings", "translate-result", "ocr-result"] {
                        let window = check
                            .get_webview_window(label)
                            .ok_or_else(|| format!("missing {label}"))?;
                        window.hide().map_err(|e| e.to_string())?;
                        window.inner_size().map_err(|e| e.to_string())?;
                    }
                    let state = check.state::<Arc<commands::AppState>>();
                    let repo = crate::database::ClipboardRepository::new(&state.db);
                    let id = repo
                        .insert(crate::database::NewClipboardItem {
                            text_content: Some("record after window creation".into()),
                            content_hash: "smoke-unique".into(),
                            semantic_hash: "smoke-unique".into(),
                            ..Default::default()
                        })
                        .map_err(|e| e.to_string())?;
                    if repo.get_by_id(id).map_err(|e| e.to_string())?.is_none() {
                        return Err("record missing".into());
                    }
                    Ok(())
                })
                .await;
                if result.is_ok() {
                    passed_in_setup.store(true, Ordering::Release);
                }
                app.exit(if result.is_ok() { 0 } else { 3 });
            });
            Ok(())
        })
        .build(context)
        .expect("build isolated smoke app");
    let exit_code = app.run_return(|_, _| {});
    assert_eq!(exit_code, 0);
    assert!(
        passed.load(Ordering::Acquire),
        "window creation/IPC roundtrip did not complete"
    );
    // WebView2 child processes may still be releasing their files; best effort.
    let _ = std::fs::remove_dir_all(dir);
}
