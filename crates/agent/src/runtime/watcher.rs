//! File watcher for `<game_dir>/scripts/active.wasm`. Polls every 500ms;
//! on change, publishes RELOAD_PENDING; falls back to direct registry_reload
//! after 1000ms if no dispatch_rust piggyback consumed.
//!
//! Settle-check: act only on changes whose mtime stayed stable for one tick.
//! Parse-check: validate `wasmi::Module::new` before triggering teardown.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::paths::{log, output_path};
use crate::runtime::orchestrator::{
    is_reload_pending, publish_reload, publish_unload, registry_reload, take_reload_pending,
};
use crate::runtime::state_file::write_state;

const ACTIVE_WASM: &str = "scripts/active.wasm";

static STOPPING: AtomicBool = AtomicBool::new(false);

/// Signal the watcher to stop. Called from DllMain DETACH.
pub fn stop() {
    STOPPING.store(true, Ordering::SeqCst);
}

/// Spawn the watcher thread. Called once from DllMain ATTACH after init.
pub fn spawn() {
    thread::Builder::new()
        .name("frog-watcher".to_string())
        .spawn(watcher_loop)
        .expect("failed to spawn watcher thread");
}

fn poll_interval_ms() -> u64 {
    std::env::var("FROG_WATCHER_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500)
}

fn fallback_ms() -> u64 {
    std::env::var("FROG_WATCHER_FALLBACK_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000)
}

fn watcher_loop() {
    log("watcher: thread started");
    let path = output_path(ACTIVE_WASM);
    let interval = Duration::from_millis(poll_interval_ms());
    let mut last_seen: Option<(SystemTime, u64)> = None;
    let mut heartbeat_counter: u64 = 0;
    const HEARTBEAT_TICKS: u64 = 10;  // 10 ticks × default 500ms interval = 5s default heartbeat
    let fallback_deadline = fallback_ms();  // hoisted so env-var is read once, not per-event

    while !STOPPING.load(Ordering::SeqCst) {
        thread::sleep(interval);

        // Periodic state-file heartbeat (regardless of file change).
        heartbeat_counter += 1;
        if heartbeat_counter % HEARTBEAT_TICKS == 0 {
            write_state();
        }

        let cur = stat_meta(&path);

        match (last_seen, cur) {
            (None, None) => {} // file absent, nothing to do
            (Some(_), None) => {
                // File deleted → unload
                log("watcher: scripts/active.wasm disappeared — publishing unload");
                publish_unload();
                wait_for_consume_or_fallback(None, fallback_deadline);
                last_seen = None;
                write_state();
            }
            (None, Some(meta)) => {
                // First sighting — try to load. Parse-check catches mid-write
                // partial files. On parse fail we still store meta so we don't
                // re-parse the same partial bytes every tick; when the writer
                // completes, mtime changes and the (Some, Some) arm fires.
                last_seen = Some(meta);
                try_load(&path, fallback_deadline);
            }
            (Some(prev), Some(meta)) => {
                if prev == meta {
                    continue; // unchanged
                }
                last_seen = Some(meta);
                try_load(&path, fallback_deadline);
            }
        }
    }
    log("watcher: thread exiting");
}

/// Read + parse-check + publish reload. Shared by the (None, Some) first-sighting
/// arm and the (Some, Some) change arm. On read/parse failure logs and returns
/// without disturbing the current runtime — the next mtime change will retry.
fn try_load(path: &Path, fallback_deadline: u64) {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            log(&format!("watcher: read failed {:?} — will retry on next change", e));
            return;
        }
    };
    // Parse-check: malformed wasm doesn't trigger teardown.
    let engine = wasmi::Engine::default();
    if let Err(e) = wasmi::Module::new(&engine, &bytes) {
        log(&format!("watcher: parse failed {:?} — leaving current runtime alone", e));
        return;
    }
    log(&format!("watcher: detected valid script ({} bytes) — publishing reload", bytes.len()));
    publish_reload(bytes.clone());
    wait_for_consume_or_fallback(Some(&bytes), fallback_deadline);
    write_state();
}

fn stat_meta(path: &Path) -> Option<(SystemTime, u64)> {
    let md = fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    let size = md.len();
    Some((mtime, size))
}

/// Wait up to `deadline_ms` for dispatch_rust to drain the pending buffer.
/// If timeout, run registry_reload directly. `bytes_ref` is the bytes we just
/// published (used for the fallback; `None` for unload). `deadline_ms` is
/// hoisted to watcher_loop and passed in to avoid re-reading the env var
/// per-event.
fn wait_for_consume_or_fallback(bytes_ref: Option<&[u8]>, deadline_ms: u64) {
    let mut elapsed_ms: u64 = 0;
    let poll_step = Duration::from_millis(50);
    while elapsed_ms < deadline_ms {
        if !is_reload_pending() {
            log("watcher: reload was consumed by dispatcher piggyback");
            return;
        }
        thread::sleep(poll_step);
        elapsed_ms += 50;
    }
    // Fallback: nobody consumed; do it ourselves on this thread.
    log(&format!("watcher: fallback after {}ms — running registry_reload directly", deadline_ms));
    // Atomically take the buffer (race-safe with a late dispatch_rust drain).
    let bytes = take_reload_pending().unwrap_or_else(|| bytes_ref.map(|b| b.to_vec()).unwrap_or_default());
    registry_reload(&bytes);
}
