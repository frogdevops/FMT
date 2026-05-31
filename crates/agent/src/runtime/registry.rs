//! Runtime registry — the new container for live wasmi runtimes.
//! Replaces today's `OnceLock<Mutex<Option<ParkedStore>>>` with a
//! `Mutex<Vec<ParkedRuntime>>`. Len is always 1 today (Replace semantics);
//! the Vec shape prefigures future Multi-script support without API churn.
//!
//! See docs/superpowers/specs/2026-05-31-b6a-hot-reload-design.md.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use agent_core::spine::{mem_backend, HookHandle, WriteJournal};

use crate::runtime::mem_host::HostState;

/// Adapter that fits `WriteJournal`'s `fn(usize, usize) -> Option<Vec<u8>>`
/// signature. Wraps `mem_backend::raw_read` with a Vec allocation. Used as
/// the read backend for every ParkedRuntime's write_journal.
fn journal_read_adapter(addr: usize, width: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; width];
    let ok = unsafe { mem_backend::raw_read(addr, buf.as_mut_ptr(), width) };
    if ok { Some(buf) } else { None }
}

/// Opaque id for a runtime in the registry. Monotonically increasing; never
/// reused (a destroyed runtime's id is gone forever). Today the registry has
/// at most one live entry, but ids are stable across spawn/kill cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeId(pub u64);

/// All the state belonging to one live wasm runtime.
pub struct ParkedRuntime {
    /// Registry-assigned id; never reused across spawn/kill cycles.
    pub id: RuntimeId,
    pub store: wasmi::Store<HostState>,
    /// Held to keep the wasmi Instance alive for `funcref_table`'s lifetime.
    /// Never read directly post-spawn; dropping it would invalidate the
    /// funcref table the dispatcher uses. Load-bearing despite the lint.
    #[allow(dead_code)]
    pub instance: wasmi::Instance,
    /// Optional: scripts that don't use hooks (e.g. test_invoke.wasm) don't
    /// export a funcref table. `call_hook_handler` rejects with a clear error
    /// at dispatch time, not load time.
    pub funcref_table: Option<wasmi::Table>,
    /// Hooks installed by this runtime, in install order. Populated by
    /// `install_hook` under the registry lock. Used by the orchestrator at
    /// reload time to iterate-and-`remove_hook` cleanly.
    pub owned_hooks: Vec<HookHandle>,
}

/// Public listing entry used by `.state.json`.
pub struct RuntimeInfo {
    /// The registry-assigned id of this runtime.
    pub id: RuntimeId,
    /// Number of hooks in `owned_hooks` at snapshot time.
    pub hooks_installed: usize,
    /// Number of distinct addresses captured in `write_journal` at snapshot time.
    pub journal_addresses: usize,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Mutex<Vec<ParkedRuntime>>> = OnceLock::new();

/// Global registry accessor. Lazy-init on first use.
pub fn registry() -> &'static Mutex<Vec<ParkedRuntime>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::with_capacity(1)))
}

/// Allocate the next monotonic runtime id.
pub fn next_runtime_id() -> RuntimeId {
    RuntimeId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// Current runtime's id, if any. Future-API used by `install_hook` to tag
/// new `HookCtx`s. Today install_hook tags via the spawn-time path, so this
/// accessor isn't yet called — kept for the multi-runtime future + the
/// .state.json writer path. Caller must NOT hold the registry lock — this fn
/// acquires it briefly.
#[allow(dead_code)]
pub fn current_id() -> Option<RuntimeId> {
    let guard = registry().lock().ok()?;
    guard.first().map(|r| r.id)
}

/// Enumerate live runtimes (today: 0 or 1). For `.state.json` writer.
pub fn list() -> Vec<RuntimeInfo> {
    let guard = match registry().lock() { Ok(g) => g, Err(_) => return Vec::new() };
    guard.iter().map(|r| RuntimeInfo {
        id: r.id,
        hooks_installed: r.owned_hooks.len(),
        // Journal now lives in HostState (inside the Store); read via store.data().
        journal_addresses: r.store.data().write_journal.len(),
    }).collect()
}

/// Insert a freshly-spawned runtime into the registry. Caller must ensure
/// any prior runtime has been torn down (revert journal, unhook, drop) first.
/// Today (len=1) this panics if called while a runtime is already present.
pub fn insert(runtime: ParkedRuntime) {
    let mut guard = registry().lock().expect("REGISTRY mutex poisoned");
    assert!(guard.is_empty(), "registry::insert while runtime already present — orchestrator must tear down first");
    guard.push(runtime);
}

/// Record an installed hook in the current runtime's `owned_hooks`. Called
/// by `install_hook` after `publish_slot` succeeds.
///
/// Uses `try_lock()` (NOT `lock()`) to avoid deadlock when called from a
/// hook handler that triggers another install_hook — `call_hook_handler`
/// holds the REGISTRY lock across wasm execution. On contention, silently
/// fails: the HookCtx.runtime_id is still set correctly, so Task 7's
/// `collect_owned_hook_ids` scan finds the hook. `owned_hooks` is just a
/// fast-iteration optimization / display field; the scan is authoritative.
pub fn record_hook(handle: HookHandle) {
    let mut guard = match registry().try_lock() { Ok(g) => g, Err(_) => return };
    if let Some(r) = guard.first_mut() {
        r.owned_hooks.push(handle);
    }
}

/// Construct an empty `WriteJournal` with the production read backend.
/// Called by `run_wasm_with_mem` (Task 4 wiring) when spawning a fresh
/// runtime. Keeping construction here means the backend is centralized; the
/// spawn-site doesn't need to know about the adapter fn.
pub fn new_journal() -> WriteJournal {
    WriteJournal::new(journal_read_adapter)
}
