//! Native desktop notifications (Windows toasts, freedesktop/D-Bus on Linux,
//! Notification Center on macOS). Best effort: a machine without a notification
//! service (a headless server) just logs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use trun_agent::{Notification, Notifier};

static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Identical notifications within this window are dropped.
const DEDUP: Duration = Duration::from_secs(30);

pub fn desktop() -> Notifier {
    let recent: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
    Arc::new(move |n: Notification| {
        let key = format!("{}\u{0}{}", n.title, n.body);
        {
            let mut r = recent.lock().unwrap();
            r.retain(|_, t| t.elapsed() < DEDUP);
            if r.contains_key(&key) {
                return;
            }
            r.insert(key, Instant::now());
        }
        // Showing a notification can block (D-Bus round trips), never do it inline.
        std::thread::spawn(move || {
            let result = notify_rust::Notification::new()
                .appname("trun")
                .summary(&n.title)
                .body(&n.body)
                .show();
            match result {
                Ok(_) => tracing::info!(run = %n.run_id, title = %n.title, "notification sent"),
                // Say it once at warn level (e.g. no D-Bus session on a server), then quietly.
                Err(e) if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) => {
                    tracing::warn!(
                        error = %e,
                        "desktop notifications unavailable on this machine; set [notify] desktop = false in config.toml to silence"
                    )
                }
                Err(e) => {
                    tracing::debug!(run = %n.run_id, error = %e, "desktop notification unavailable")
                }
            }
        });
    })
}

/// Log-only notifier (notifications disabled).
pub fn log_only() -> Notifier {
    Arc::new(
        |n: Notification| tracing::info!(run = %n.run_id, title = %n.title, body = %n.body, "notification (desktop disabled)"),
    )
}
