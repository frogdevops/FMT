//! Hot-reload orchestrator: the 5-step swap sequence (revert journal →
//! unhook owned hooks → drop old → spawn new) plus the `RELOAD_PENDING`
//! handoff between the watcher thread and the dispatcher piggyback.
//!
//! Synchronization model: per [[hooks-are-the-sync-primitive]], when
//! `take_reload_pending()` is called from inside `dispatch_rust`, the game
//! thread is frozen by the inline-detour call stack — `registry_reload`
//! mutating the registry races nothing. When called from the watcher thread
//! (fallback path after no hook consumes within 1s), we accept the same
//! synchronization level the original write path used.
//!
//! Lock ordering constraint (must hold): `install_hook` acquires
//! INSTALL_GUARD then briefly takes REGISTRY (via `record_hook`). The
//! orchestrator MUST NOT hold REGISTRY across `remove_hook` calls (which
//! take INSTALL_GUARD) — otherwise REGISTRY → INSTALL_GUARD inverts the
//! install-side order and deadlocks. The safe pattern (followed below):
//! snapshot what's needed while holding REGISTRY, drop the lock, then
//! iterate calling `remove_hook` with no lock held.

use std::sync::Mutex;

use agent_core::spine::mem_backend;

use crate::internals::hook_runtime::api::remove_hook;
use crate::paths::log;
use crate::runtime::registry::{registry, RuntimeId};

/// Reload-pending byte buffer. `None` = nothing pending. `Some(bytes)` = a new
/// .wasm is queued for spawn. `Some(empty)` = unload-only (delete the script).
static RELOAD_PENDING: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Publish reload-pending bytes. Called by the watcher thread on file change.
/// If a prior pending is unconsumed, it's REPLACED (most recent wins).
pub fn publish_reload(bytes: Vec<u8>) {
    let mut guard = match RELOAD_PENDING.lock() { Ok(g) => g, Err(_) => return };
    *guard = Some(bytes);
}

/// Publish an unload (file deletion). Equivalent to publishing empty bytes.
pub fn publish_unload() {
    publish_reload(Vec::new());
}

/// Drain the pending buffer. Returns the bytes if any were queued.
/// Called by `dispatch_rust` piggyback (preferred) and by the watcher
/// fallback after 1s of unconsumed pending.
pub fn take_reload_pending() -> Option<Vec<u8>> {
    let mut guard = RELOAD_PENDING.lock().ok()?;
    guard.take()
}

/// `true` if reload bytes are queued for consume.
pub fn is_reload_pending() -> bool {
    RELOAD_PENDING.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// The 5-step swap. Empty `bytes` means unload-only (no respawn).
///
/// Called from:
///   - `dispatch_rust` piggyback (preferred; game thread, frozen)
///   - Watcher fallback (agent thread; safe for hookless / idle scripts)
pub fn registry_reload(bytes: &[u8]) {
    log(&format!("registry_reload: BEGIN (bytes={} {})", bytes.len(),
        if bytes.is_empty() { "[unload]" } else { "[swap]" }));

    // Step 1: snapshot what we need from the current runtime before tearing
    // it down. We need: the runtime_id (to scan-and-unhook), and the journal
    // entries (to revert). Both are extracted while holding REGISTRY briefly;
    // the lock is dropped BEFORE remove_hook is called (lock-order constraint
    // per module-level comment).
    //
    // Note: with the Task 6 design, write_journal lives in HostState (inside
    // the wasmi Store), so we extract entries via store.data_mut() while
    // holding REGISTRY. The Store itself is dropped at Step 4.
    let (current_id, journal_entries) = {
        let mut guard = match registry().lock() { Ok(g) => g, Err(_) => {
            log("registry_reload: REGISTRY mutex poisoned");
            return;
        }};
        match guard.first_mut() {
            None => {
                // No runtime live: just spawn if bytes are non-empty.
                drop(guard);
                if !bytes.is_empty() {
                    spawn_fresh(bytes);
                }
                crate::runtime::state_file::write_state();
                return;
            }
            Some(r) => {
                // Extract journal entries via HostState (Task 6 design).
                // take_entries() leaves the WriteJournal empty but with its
                // read_backend intact (no-op for a runtime about to be dropped).
                let entries = r.store.data_mut().write_journal.take_entries();
                (r.id, entries)
            }
        }
    };  // REGISTRY lock released here — safe to call remove_hook below

    // Step 2: revert journal entries. This writes original bytes back via
    // the cache-validated guarded backend. Run with NO locks held.
    log(&format!("registry_reload: reverting {} journal entries", journal_entries.len()));
    for (addr, original_bytes) in &journal_entries {
        let ok = unsafe {
            mem_backend::raw_write(*addr, original_bytes.as_ptr(), original_bytes.len())
        };
        if !ok {
            log(&format!("registry_reload: revert at {:#x} FAILED (length={})", addr, original_bytes.len()));
        }
    }

    // Step 3: scan HOOK_SLOTS for entries owned by current_id; call remove_hook
    // on each. Iterates 256 slots; constant time. NO REGISTRY lock held here —
    // remove_hook takes INSTALL_GUARD; if we held REGISTRY too, install_hook
    // (which goes INSTALL_GUARD → REGISTRY via record_hook) would deadlock.
    log(&format!("registry_reload: unhooking owned hooks of runtime_id={}", current_id.0));
    let owned: Vec<u64> = collect_owned_hook_ids(current_id);
    for id in owned {
        if let Err(e) = remove_hook(agent_core::spine::HookHandle::from_raw(id)) {
            log(&format!("registry_reload: remove_hook(id={}) failed {:?}", id, e));
        }
    }

    // Step 4: drop the old runtime. Acquire the lock again, pop it out, drop.
    {
        let mut guard = match registry().lock() { Ok(g) => g, Err(_) => {
            log("registry_reload: REGISTRY poisoned at drop");
            return;
        }};
        if let Some(old) = guard.pop() {
            log(&format!("registry_reload: dropping runtime_id={}", old.id.0));
            drop(old);  // wasmi::Store destructor frees the instance + funcref table
        }
    }

    // Step 5: spawn the new runtime (if bytes non-empty).
    if !bytes.is_empty() {
        spawn_fresh(bytes);
    }

    // Refresh the state file so any frontend sees the new state immediately
    // rather than waiting up to 5s for the watcher's heartbeat tick.
    crate::runtime::state_file::write_state();

    log("registry_reload: DONE");
}

/// Run `run_wasm_with_mem` with the new bytes. The fn inserts the new
/// ParkedRuntime into REGISTRY itself (Task 4 wiring).
fn spawn_fresh(bytes: &[u8]) {
    log(&format!("registry_reload: spawning fresh runtime ({} bytes)", bytes.len()));
    match crate::runtime::mem_host::run_wasm_with_mem(bytes) {
        Ok(logs) => {
            log(&format!("registry_reload: spawn ok, {} log line(s)", logs.len()));
            for l in &logs {
                log(&format!("    [wasm] {}", l));
            }
        }
        Err(e) => log(&format!("registry_reload: spawn FAILED: {:?}", e)),
    }
}

/// Scan HOOK_SLOTS for entries owned by `target_id`. Returns the slot ids.
/// Reads SLOT_VALID and HOOK_SLOTS without holding INSTALL_GUARD — safe
/// because we're reading-only and the registry lock prevents concurrent
/// installs of new hooks for the runtime we're tearing down (the runtime
/// is still in REGISTRY at this point — Step 4 drops it).
fn collect_owned_hook_ids(target_id: RuntimeId) -> Vec<u64> {
    use crate::internals::hook_runtime::registry::MAX_HOOKS;
    let mut out = Vec::new();
    for id in 0..MAX_HOOKS {
        if let Some(ctx) = crate::internals::hook_runtime::registry::ctx_for(id as u64) {
            if ctx.runtime_id == target_id.0 {
                out.push(id as u64);
            }
        }
    }
    out
}
