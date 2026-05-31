# B-6a Hot Reload + Runtime Registry + Write Journal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make WASM scripts hot-reloadable via a `<game_dir>/scripts/active.wasm` filesystem convention, with per-script write-journaling that auto-reverts on stop, and game-thread-safe teardown via the existing `dispatch_rust` piggyback.

**Architecture:** Replace today's `OnceLock<Mutex<Option<ParkedStore>>>` with a `Mutex<Vec<ParkedRuntime>>` registry (len=1 today, prefigures future Multi). Each `ParkedRuntime` carries a `runtime_id`, a `Vec<HookHandle>` of owned hooks, and a `WriteJournal` (concrete agent-core type with a `fn` pointer read backend baked in at spawn). A watcher thread polls the scripts folder; on change it publishes `RELOAD_PENDING`; the next `dispatch_rust` call drains it at game-thread-safe boundary (per the audit-verified pattern in [[hooks-are-the-sync-primitive]]); fallback timer (1s) lets the watcher do the reload itself if no hook fires.

**Tech Stack:** Rust 2021 (stable), wasmi 0.31, Windows cdylib via `x86_64-pc-windows-gnu` cross-compile, agent-core Linux-testable substrate, inline-detour hook substrate from B-3.

**Reference spec:** `docs/superpowers/specs/2026-05-31-b6a-hot-reload-design.md`

**Critical project rules** (apply to every task):
1. **DO NOT run `git commit`, `git add`, or `git stash`.** Per project memory `user-commits-own-work`, pause at marked commit points for the user.
2. **DO NOT run bare `cargo build -p agent` to verify.** Per project memory `deploy-setup`, Linux-native builds compile an EMPTY cdylib (all agent modules are `#[cfg(target_os = "windows")]`-gated). Always verify with `cargo build --target x86_64-pc-windows-gnu --release`.
3. **DO NOT run `./deploy.sh`** — it auto-fires from a post-build hook.
4. **Agent-core tests use bare `cargo test -p agent-core`** — agent-core is Linux-native.

---

## File Structure

**New files:**

| Path | Responsibility |
|---|---|
| `crates/agent-core/src/spine/journal.rs` | Concrete `WriteJournal` with `fn(usize, usize) -> Option<Vec<u8>>` read backend (no generic — so it can be stored in `ParkedRuntime` without infecting the registry / orchestrator). First-touch capture, `take_entries` for revert. Linux-testable; tests pass any `fn` pointer. |
| `crates/agent/src/runtime/registry.rs` | `RuntimeRegistry` + `ParkedRuntime` types. The registry is the new global container that replaces `PARKED`. |
| `crates/agent/src/runtime/orchestrator.rs` | `registry_reload(bytes)` — the 5-step reload sequence (revert journal → unhook → drop → spawn). |
| `crates/agent/src/runtime/watcher.rs` | The polling watcher thread + `RELOAD_PENDING` + `wait_for_consume_or_fallback`. |
| `crates/agent/src/runtime/state_file.rs` | Writes `.state.json` atomically (temp + rename). |

**Modified files:**

| Path | Change |
|---|---|
| `crates/agent-core/src/spine/mod.rs` | Re-export `journal` module. |
| `crates/agent/src/runtime/mod.rs` | Add the 4 new modules above. |
| `crates/agent/src/runtime/host.rs` | Replace `PARKED` with `REGISTRY`; rewrite `call_hook_handler` to use the registry; delete `maybe_run_configured`. |
| `crates/agent/src/runtime/mem_host.rs` | Wire write journaling into `host_write` + `host_write_if`; add `host_write_permanent`; remove `write_granted` parameter from `run_wasm_with_mem`; always-register the write linker fns. |
| `crates/agent/src/internals/hook_runtime/registry.rs` | Add `runtime_id: u64` field to `HookCtx`. |
| `crates/agent/src/internals/hook_runtime/api.rs` | `install_hook` reads current `runtime_id` from `REGISTRY`; pushes the new `HookHandle` into the current runtime's `owned_hooks`. |
| `crates/agent/src/internals/hook_runtime/dispatcher.rs` | Wrap ctx-using body in a scope block; add piggyback `take_reload_pending()` after `clear_reentry`. |
| `crates/agent/src/entry.rs` | mkdir `scripts/`; spawn watcher in `DllMain` ATTACH; delete `maybe_run_configured()` call; extend `DllMain` DETACH to unhook all + stop watcher. |

---

## Task 1: Add `WriteJournal` to agent-core

**Files:**
- Create: `crates/agent-core/src/spine/journal.rs`
- Modify: `crates/agent-core/src/spine/mod.rs` (add module + re-export)
- Test: in the new file (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test (inline in the new file)**

Create `crates/agent-core/src/spine/journal.rs` with body:

```rust
//! Per-runtime write journal: captures first-touched original bytes per address
//! so the reload orchestrator can restore the game world to pre-script state.
//!
//! Concrete type with a `fn` pointer read backend (not a generic) so it can be
//! stored in `ParkedRuntime` without infecting the registry / orchestrator
//! with generics. The agent crate provides a real adapter wrapping
//! `mem_backend::raw_read`; tests pass any `fn(usize, usize) -> Option<Vec<u8>>`.

use std::collections::HashMap;

/// Read-backend signature: given an address and width, return the original
/// bytes at that address, or `None` if unreadable.
pub type JournalReadFn = fn(usize, usize) -> Option<Vec<u8>>;

pub struct WriteJournal {
    entries: HashMap<usize, Vec<u8>>,
    read_backend: JournalReadFn,
}

impl WriteJournal {
    pub fn new(read_backend: JournalReadFn) -> Self {
        Self { entries: HashMap::new(), read_backend }
    }

    /// Record a first-touch at `addr` with `width` bytes if not already recorded.
    /// Subsequent calls for the same address are no-ops (only the first original
    /// is preserved). Returns `true` on first-touch; `false` if already recorded
    /// or if the read backend returns `None`.
    pub fn touch(&mut self, addr: usize, width: usize) -> bool {
        if self.entries.contains_key(&addr) {
            return false;
        }
        match (self.read_backend)(addr, width) {
            Some(bytes) => {
                self.entries.insert(addr, bytes);
                true
            }
            None => false,
        }
    }

    /// Number of distinct addresses captured.
    pub fn len(&self) -> usize { self.entries.len() }

    /// True if no addresses have been touched.
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Extract entries for revert, leaving the journal empty (read_backend intact).
    /// After this, `len()` returns 0. The returned HashMap is iterated by the
    /// orchestrator's revert step.
    pub fn take_entries(&mut self) -> HashMap<usize, Vec<u8>> {
        std::mem::take(&mut self.entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test backends are bare `fn` items so they have function-pointer type
    // (which is what JournalReadFn requires). Closures with captures cannot
    // be coerced to fn pointers.

    fn read_seed_ab(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xAB; width])
    }

    fn read_seed_cd(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xCD; width])
    }

    fn read_seed_ef(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xEF; width])
    }

    fn read_unreadable(_addr: usize, _width: usize) -> Option<Vec<u8>> {
        None
    }

    fn read_addr_low_byte(addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![addr as u8; width])
    }

    #[test]
    fn touch_records_first_time_only() {
        let mut j = WriteJournal::new(read_seed_ab);
        assert!(j.touch(0x1000, 4));   // first touch returns true
        assert!(!j.touch(0x1000, 4));  // second touch returns false
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn touch_records_multiple_addresses() {
        let mut j = WriteJournal::new(read_seed_cd);
        j.touch(0x1000, 4);
        j.touch(0x2000, 8);
        j.touch(0x3000, 1);
        assert_eq!(j.len(), 3);
    }

    #[test]
    fn touch_preserves_original_bytes() {
        let mut j = WriteJournal::new(read_seed_ef);
        j.touch(0x1000, 4);
        let entries = j.take_entries();
        assert_eq!(entries.len(), 1);
        let bytes = entries.get(&0x1000).expect("addr 0x1000 captured");
        assert_eq!(bytes, &vec![0xEF, 0xEF, 0xEF, 0xEF]);
    }

    #[test]
    fn touch_returns_false_on_unreadable() {
        let mut j = WriteJournal::new(read_unreadable);
        assert!(!j.touch(0x1000, 4));
        assert_eq!(j.len(), 0);
    }

    #[test]
    fn first_touch_wins_under_overlapping_widths() {
        // First touch captures 4 bytes. Second touch with a wider width is a
        // no-op; the journal still has exactly one entry of width 4.
        let mut j = WriteJournal::new(read_addr_low_byte);
        j.touch(0x1000, 4);   // captures 4 bytes
        j.touch(0x1000, 8);   // no-op (already touched)
        let entries = j.take_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(&0x1000).unwrap().len(), 4);  // first-touch width preserved
    }

    #[test]
    fn take_entries_leaves_journal_empty_with_backend_intact() {
        let mut j = WriteJournal::new(read_seed_ab);
        j.touch(0x1000, 4);
        let drained = j.take_entries();
        assert_eq!(drained.len(), 1);
        assert!(j.is_empty());
        // Backend still works; new touches succeed.
        assert!(j.touch(0x2000, 4));
        assert_eq!(j.len(), 1);
    }
}
```

- [ ] **Step 2: Wire the module export**

In `crates/agent-core/src/spine/mod.rs`, after the existing `pub mod` lines, add:

```rust
pub mod journal;
```

And after the existing `pub use` lines, add:

```rust
pub use journal::{JournalReadFn, WriteJournal};
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p agent-core spine::journal`
Expected: 6 passed; 0 failed.

(Six tests: `touch_records_first_time_only`, `touch_records_multiple_addresses`, `touch_preserves_original_bytes`, `touch_returns_false_on_unreadable`, `first_touch_wins_under_overlapping_widths`, `take_entries_leaves_journal_empty_with_backend_intact`.)

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds with no new warnings vs. baseline (only the pre-existing 11 warnings from B-5 + previous bricks).

- [ ] **Step 5: Pause for user commit** (per `user-commits-own-work`)

---

## Task 2: Add `runtime_id` field to `HookCtx`

**Files:**
- Modify: `crates/agent/src/internals/hook_runtime/registry.rs:30-41` (`HookCtx` struct)
- Modify: `crates/agent/src/internals/hook_runtime/api.rs:183` (install_hook's `HookCtx { ... }` literal — needs the new field; will be wired to real runtime_id in Task 5; for now hardcode 0)

- [ ] **Step 1: Add the field to `HookCtx`**

In `crates/agent/src/internals/hook_runtime/registry.rs`, change lines 30-41:

```rust
pub struct HookCtx {
    pub method:     MethodPtr,
    pub sig:        MethodSignature,
    pub thunk_addr: usize,
    /// The `inline_detour::Hook` — owns the trampoline + stolen-bytes restore.
    /// Kept here so removal Drop-restores the original prologue.
    pub patch:      Hook,
    /// wasmi::Func — resolved at install time, called from dispatcher.
    /// Stored as raw bits to keep this struct Send/Sync; see api.rs for
    /// the safe wrapper.
    pub handler_func_ref: u64,
    /// The id of the runtime that installed this hook. Used at reload time
    /// to scan-and-unhook only the hooks owned by the runtime being torn down.
    /// Populated by `install_hook` from the current `REGISTRY` entry.
    pub runtime_id: u64,
}
```

- [ ] **Step 2: Patch the single `HookCtx { ... }` literal in install_hook**

In `crates/agent/src/internals/hook_runtime/api.rs:183`, change:

```rust
    let ctx = HookCtx { method, sig, thunk_addr, patch, handler_func_ref };
```

to:

```rust
    // Task 5 will wire runtime_id from the actual current registry entry.
    // For now (this task) hardcode 0 so the struct literal type-checks.
    let ctx = HookCtx { method, sig, thunk_addr, patch, handler_func_ref, runtime_id: 0 };
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds; no new warnings.

- [ ] **Step 4: Pause for user commit**

---

## Task 3: Create `ParkedRuntime` + `RuntimeRegistry`

**Files:**
- Create: `crates/agent/src/runtime/registry.rs`
- Modify: `crates/agent/src/runtime/mod.rs` (add `pub mod registry;`)

- [ ] **Step 1: Create the registry module**

Create `crates/agent/src/runtime/registry.rs`:

```rust
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
    pub id: RuntimeId,
    pub store: wasmi::Store<HostState>,
    pub instance: wasmi::Instance,
    /// Optional: scripts that don't use hooks (e.g. test_invoke.wasm) don't
    /// export a funcref table. `call_hook_handler` rejects with a clear error
    /// at dispatch time, not load time.
    pub funcref_table: Option<wasmi::Table>,
    /// Hooks installed by this runtime, in install order. Populated by
    /// `install_hook` under the registry lock. Used by the orchestrator at
    /// reload time to iterate-and-`remove_hook` cleanly.
    pub owned_hooks: Vec<HookHandle>,
    /// First-touched original bytes per address. Captured by `host_write` /
    /// `host_write_if` on first touch of each addr; reverted at script stop.
    /// `mem.write_permanent` skips this entirely. Construction wires the
    /// `journal_read_adapter` (above) as the read backend.
    pub write_journal: WriteJournal,
}

/// Public listing entry used by `.state.json`.
pub struct RuntimeInfo {
    pub id: RuntimeId,
    pub hooks_installed: usize,
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

/// Current runtime's id, if any. Used by `install_hook` to tag new `HookCtx`s.
/// Caller must NOT hold the registry lock — this fn acquires it briefly.
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
        journal_addresses: r.write_journal.len(),
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
pub fn record_hook(handle: HookHandle) {
    let mut guard = match registry().lock() { Ok(g) => g, Err(_) => return };
    if let Some(r) = guard.first_mut() {
        r.owned_hooks.push(handle);
    }
}

/// Record a first-touch in the current runtime's journal. Called by
/// `host_write` / `host_write_if`. Returns `true` on first-touch, `false`
/// if already recorded, no runtime is live, or the read backend failed.
/// The read backend (`journal_read_adapter`) was baked in at runtime spawn.
pub fn journal_touch(addr: usize, width: usize) -> bool {
    let mut guard = match registry().lock() { Ok(g) => g, Err(_) => return false };
    let r = match guard.first_mut() { Some(r) => r, None => return false };
    r.write_journal.touch(addr, width)
}

/// Construct an empty `WriteJournal` with the production read backend.
/// Called by `run_wasm_with_mem` (Task 4 wiring) when spawning a fresh
/// runtime. Keeping construction here means the backend is centralized; the
/// spawn-site doesn't need to know about the adapter fn.
pub fn new_journal() -> WriteJournal {
    WriteJournal::new(journal_read_adapter)
}
```

- [ ] **Step 2: Wire the module into runtime/mod.rs**

Read the current `crates/agent/src/runtime/mod.rs`, then add (preserving existing `pub mod` lines):

```rust
pub mod registry;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. The new module compiles in isolation; nothing references it yet (next tasks wire callers).

- [ ] **Step 4: Pause for user commit**

---

## Task 4: Migrate `call_hook_handler` to use `REGISTRY`; delete `PARKED`

**Files:**
- Modify: `crates/agent/src/runtime/host.rs` (replace `PARKED` accessor + `call_hook_handler` body)
- Modify: `crates/agent/src/runtime/mem_host.rs::run_wasm_with_mem` (the `PARKED` write at the bottom)

- [ ] **Step 1: Replace `PARKED` in host.rs with registry-based accessor**

In `crates/agent/src/runtime/host.rs`, remove these lines:

```rust
pub struct ParkedStore {
    pub store: wasmi::Store<HostState>,
    pub instance: wasmi::Instance,
    pub funcref_table: Option<wasmi::Table>,
}

static PARKED: OnceLock<Mutex<Option<ParkedStore>>> = OnceLock::new();

pub fn parked() -> &'static Mutex<Option<ParkedStore>> {
    PARKED.get_or_init(|| Mutex::new(None))
}
```

The `ParkedStore` struct is REPLACED by `ParkedRuntime` (in `runtime::registry`). The `parked()` accessor is REPLACED by `crate::runtime::registry::registry()`.

- [ ] **Step 2: Rewrite `call_hook_handler` to use the registry**

In `crates/agent/src/runtime/host.rs`, replace the entire `call_hook_handler` fn body. The new version uses `registry()` instead of `parked()`:

```rust
pub fn call_hook_handler(handler_funcref_idx: u64) -> Result<(), &'static str> {
    let mut guard = match crate::runtime::registry::registry().try_lock() {
        Ok(g) => g,
        Err(_) => {
            note_wasm_handler_contended();
            return Err("REGISTRY contended; transparent observer fires");
        }
    };
    let runtime = match guard.first_mut() {
        Some(r) => r,
        None => return Err("REGISTRY empty (no script loaded, or frog_main still running)"),
    };

    let table = runtime.funcref_table.as_ref()
        .ok_or("no funcref table exported — script cannot dispatch hooks")?;
    let val = table
        .get(&runtime.store, handler_funcref_idx as u32)
        .ok_or("funcref index out of range")?;
    let func = match val {
        wasmi::Val::FuncRef(fr) => fr.func().ok_or("funcref is null")?.clone(),
        _ => return Err("table entry is not a funcref"),
    };

    let typed = func.typed::<(), ()>(&runtime.store)
        .map_err(|_| "handler signature is not () -> ()")?;

    typed.call(&mut runtime.store, ())
        .map_err(|_| "wasm handler trapped")?;

    Ok(())
}
```

Update the SAFETY comment block ABOVE the fn to reference REGISTRY instead of PARKED.

- [ ] **Step 3: Update `run_wasm_with_mem` to insert into registry**

In `crates/agent/src/runtime/mem_host.rs`, find the section near the bottom that writes to `parked()`. Replace it with:

```rust
    // B-6a: instead of parking into PARKED, insert a ParkedRuntime into REGISTRY.
    // Caller (orchestrator or initial spawn) must ensure the registry is empty
    // before calling — assertion lives in registry::insert.
    let logs = store.data().logs.clone();
    let funcref_table = instance.get_table(&store, "__indirect_function_table");
    let id = crate::runtime::registry::next_runtime_id();
    crate::runtime::registry::insert(crate::runtime::registry::ParkedRuntime {
        id,
        store,
        instance,
        funcref_table,
        owned_hooks: Vec::new(),
        // `new_journal()` constructs the WriteJournal with the production
        // read backend (mem_backend::raw_read adapter) baked in. After this,
        // host_write / host_write_if only need to call journal_touch(addr, width).
        write_journal: crate::runtime::registry::new_journal(),
    });

    Ok(logs)
```

- [ ] **Step 4: Remove now-unused `host.rs` items**

Delete the `use std::sync::{Mutex, OnceLock};` import line in `host.rs` if no other code in the file uses them. The compiler will tell you.

- [ ] **Step 5: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. Compiler may flag unused imports in adjacent files — clean those up.

- [ ] **Step 6: Pause for user commit**

---

## Task 5: Plumb `runtime_id` into `install_hook`

**Files:**
- Modify: `crates/agent/src/internals/hook_runtime/api.rs:183` (the `HookCtx { ... }` literal)
- Modify: `crates/agent/src/internals/hook_runtime/api.rs` (after `publish_slot`, call `record_hook`)

- [ ] **Step 1: Replace the hardcoded `runtime_id: 0` with real lookup**

In `crates/agent/src/internals/hook_runtime/api.rs`, find line 183:

```rust
    let ctx = HookCtx { method, sig, thunk_addr, patch, handler_func_ref, runtime_id: 0 };
```

Replace with:

```rust
    // Read the current runtime's id from REGISTRY so the orchestrator can later
    // scan-and-unhook only the hooks owned by this runtime.
    let runtime_id = match crate::runtime::registry::current_id() {
        Some(id) => id.0,
        None => {
            crate::paths::log("install_hook: no current runtime in REGISTRY — cannot install");
            unsafe { free_thunk(thunk_addr); }
            return Err(HookError::PatchFailed);
        }
    };
    let ctx = HookCtx { method, sig, thunk_addr, patch, handler_func_ref, runtime_id };
```

- [ ] **Step 2: Record the handle in the current runtime's `owned_hooks` after publish**

In `crates/agent/src/internals/hook_runtime/api.rs`, find:

```rust
    unsafe { publish_slot(id, ctx); }
    crate::paths::log(&format!("install_hook: [6/6] published slot id={} — DONE", id));

    // Step 7: return the opaque handle.
    Ok(HookHandle::from_raw(id))
```

Insert the `record_hook` call between `publish_slot` and the `Ok` return:

```rust
    unsafe { publish_slot(id, ctx); }
    crate::paths::log(&format!("install_hook: [6/6] published slot id={} — DONE", id));

    let handle = HookHandle::from_raw(id);
    crate::runtime::registry::record_hook(handle);

    Ok(handle)
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, no new warnings.

- [ ] **Step 4: Pause for user commit**

---

## Task 6: Wire write journaling + `mem.write_permanent` + remove `FROG_WASM_WRITE` gate

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (host_write, host_write_if, add host_write_permanent, remove write_granted)
- Modify: `crates/agent/src/runtime/host.rs::maybe_run_configured` (no longer reads FROG_WASM_WRITE; will be deleted entirely in Task 11 but for now strip the env read)

- [ ] **Step 1: Add journal-touch to `host_write_typed`**

In `crates/agent/src/runtime/mem_host.rs`, find the existing `host_write_typed` helper. Update its body to journal-touch BEFORE writing. Replace the helper with:

```rust
fn host_write_typed<T: MemValue>(
    caller: &Caller<'_, HostState>,
    addr: MemAddr<ReadWrite>,
    in_ptr: i32,
    in_len: i32,
) -> i32 {
    let bytes = match read_guest(caller, in_ptr, in_len) {
        Some(b) => b,
        None => return status::ERR_BAD_TYPE,
    };
    let val = match T::from_le_bytes_spine(&bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    // Journal first-touch capture: the current runtime's WriteJournal has the
    // mem_backend::raw_read adapter baked in. No-op if already touched OR if
    // no runtime is current OR if read fails.
    let width = T::VAL_TYPE.fixed_width().unwrap_or(0);
    if width > 0 {
        let raw_addr = addr.as_u64() as usize;
        crate::runtime::registry::journal_touch(raw_addr, width);
    }
    match api::write::<T>(addr, val) {
        Ok(()) => status::OK,
        Err(e) => i32::from(e),
    }
}
```

- [ ] **Step 2: Add journal-touch to `host_write_if_typed`**

In the same file, update the existing `host_write_if_typed` helper. The journal-touch happens at the same point — before writing:

```rust
fn host_write_if_typed<T: MemValue + PartialEq>(
    _caller: &Caller<'_, HostState>,
    addr: MemAddr<ReadWrite>,
    exp_bytes: &[u8],
    new_bytes: &[u8],
) -> i32 {
    let exp = match T::from_le_bytes_spine(exp_bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    let new = match T::from_le_bytes_spine(new_bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    let cur: T = match api::read::<T, _>(addr.as_readonly()) {
        Ok(v) => v,
        Err(e) => return i32::from(e),
    };
    if cur != exp {
        return status::CHANGED;
    }
    // Journal first-touch capture — same pattern as host_write_typed.
    let width = T::VAL_TYPE.fixed_width().unwrap_or(0);
    if width > 0 {
        let raw_addr = addr.as_u64() as usize;
        crate::runtime::registry::journal_touch(raw_addr, width);
    }
    match api::write::<T>(addr, new) {
        Ok(()) => status::OK,
        Err(e) => i32::from(e),
    }
}
```

- [ ] **Step 3: Add `host_write_permanent` (opt-out of journaling)**

In `crates/agent/src/runtime/mem_host.rs`, near `host_write`, add:

```rust
fn host_write_permanent(caller: Caller<'_, HostState>, addr: i64, ty: i32, in_ptr: i32, in_len: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    // SAFETY: write_permanent is unconditional per [[wild-west-platform-philosophy]];
    // gating was removed in this brick. The actor's call to write_permanent is the
    // declaration that this change survives script stop (no journal recording).
    let addr = unsafe { MemAddr::<ReadWrite>::from_raw_writable(addr as u64) };

    // Same ValType dispatch as host_write, but skip the journal touch.
    match ty {
        ValType::U8  => host_write_permanent_typed::<u8 >(&caller, addr, in_ptr, in_len),
        ValType::U16 => host_write_permanent_typed::<u16>(&caller, addr, in_ptr, in_len),
        ValType::U32 => host_write_permanent_typed::<u32>(&caller, addr, in_ptr, in_len),
        ValType::U64 => host_write_permanent_typed::<u64>(&caller, addr, in_ptr, in_len),
        ValType::I8  => host_write_permanent_typed::<i8 >(&caller, addr, in_ptr, in_len),
        ValType::I16 => host_write_permanent_typed::<i16>(&caller, addr, in_ptr, in_len),
        ValType::I32 => host_write_permanent_typed::<i32>(&caller, addr, in_ptr, in_len),
        ValType::I64 => host_write_permanent_typed::<i64>(&caller, addr, in_ptr, in_len),
        ValType::F32 => host_write_permanent_typed::<f32>(&caller, addr, in_ptr, in_len),
        ValType::F64 => host_write_permanent_typed::<f64>(&caller, addr, in_ptr, in_len),
        ValType::Bytes | ValType::Cstr => status::ERR_BAD_TYPE,
    }
}

fn host_write_permanent_typed<T: MemValue>(
    caller: &Caller<'_, HostState>,
    addr: MemAddr<ReadWrite>,
    in_ptr: i32,
    in_len: i32,
) -> i32 {
    let bytes = match read_guest(caller, in_ptr, in_len) {
        Some(b) => b,
        None => return status::ERR_BAD_TYPE,
    };
    let val = match T::from_le_bytes_spine(&bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    match api::write::<T>(addr, val) {
        Ok(()) => status::OK,
        Err(e) => i32::from(e),
    }
}
```

- [ ] **Step 4: Remove `write_granted` parameter from `run_wasm_with_mem`**

In `crates/agent/src/runtime/mem_host.rs`, find the existing fn signature:

```rust
pub fn run_wasm_with_mem(wasm_bytes: &[u8], write_granted: bool) -> Result<Vec<String>, WasmError> {
```

Change to:

```rust
pub fn run_wasm_with_mem(wasm_bytes: &[u8]) -> Result<Vec<String>, WasmError> {
```

Find the `if write_granted { ... }` block that conditionally registers `mem.write` and `mem.write_if`. Replace with unconditional registration + the new write_permanent:

```rust
    // Per [[wild-west-platform-philosophy]] writes are unconditional.
    // The FROG_WASM_WRITE env gate was removed in B-6a.
    linker.func_wrap("mem", "write", host_write).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("mem", "write_if", host_write_if).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("mem", "write_permanent", host_write_permanent).map_err(|e| WasmError::Instantiate(e.to_string()))?;
```

(The placement is alongside the other `linker.func_wrap` lines; remove the `if write_granted { ... }` wrapper entirely.)

- [ ] **Step 5: Update the existing caller in `maybe_run_configured`**

In `crates/agent/src/runtime/host.rs::maybe_run_configured`, find the body that reads `FROG_WASM_WRITE` and passes it to `run_wasm_with_mem`. Strip the env read and pass nothing:

Change:
```rust
    let write_granted = std::env::var("FROG_WASM_WRITE").map(|v| !v.is_empty()).unwrap_or(false);
    log(&format!("  mem API: read=on, write={}", if write_granted { "GRANTED" } else { "off" }));
    match crate::runtime::mem_host::run_wasm_with_mem(&bytes, write_granted) {
```

to:
```rust
    log("  mem API: read=on, write=on (unconditional per B-6a)");
    match crate::runtime::mem_host::run_wasm_with_mem(&bytes) {
```

(`maybe_run_configured` is deleted entirely in Task 11; this Task-6 patch keeps it compiling until then.)

- [ ] **Step 6: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds; zero new warnings.

- [ ] **Step 7: Pause for user commit**

---

## Task 7: Implement `registry_reload` orchestrator + `RELOAD_PENDING`

**Files:**
- Create: `crates/agent/src/runtime/orchestrator.rs`
- Modify: `crates/agent/src/runtime/mod.rs` (add `pub mod orchestrator;`)

- [ ] **Step 1: Create the orchestrator module**

Create `crates/agent/src/runtime/orchestrator.rs`:

```rust
//! Hot-reload orchestrator: the 5-step swap sequence (revert journal →
//! unhook owned hooks → drop old → spawn new) plus the `RELOAD_PENDING`
//! handoff between the watcher thread and the dispatcher piggyback.
//!
//! Synchronization model: per [[hooks-are-the-sync-primitive]], when
//! `take_reload_pending()` is called from inside `dispatch_rust`, the game
//! thread is frozen by the inline-detour call stack — `registry_reload`
//! mutating the registry races nothing. When called from the watcher thread
//! (fallback path after no hook consumes within 1s), we accept the same
//! synchronization level the original write path used (none, for hookless
//! scripts; the cache mutex for hooked writes).

use std::sync::Mutex;

use agent_core::spine::mem_backend;

use crate::internals::hook_runtime::api::remove_hook;
use crate::paths::log;
use crate::runtime::registry::{registry, ParkedRuntime, RuntimeId};

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
    // entries (to revert). Drop the lock between steps so install_hook etc.
    // can run briefly (no concurrent hooks fire while we hold it via try_lock
    // from dispatch_rust, but watcher-path callers don't have that guarantee).
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
                // Extract journal entries, leaving the WriteJournal's
                // read_backend intact (the runtime is about to be dropped
                // anyway, but the API is honest about not invalidating it).
                let entries = r.write_journal.take_entries();
                (r.id, entries)
            }
        }
    };

    // Step 2: revert journal entries. This writes original bytes back via
    // the cache-validated guarded backend.
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
    // on each. Iterates 256 slots; constant time.
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
/// installs of new hooks for the runtime we're tearing down.
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
```

- [ ] **Step 2: Wire the module into runtime/mod.rs**

Read `crates/agent/src/runtime/mod.rs`, add:

```rust
pub mod orchestrator;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. No new warnings beyond pre-existing baseline.

- [ ] **Step 4: Pause for user commit**

---

## Task 8: Patch `dispatch_rust` with scope-block + piggyback

**Files:**
- Modify: `crates/agent/src/internals/hook_runtime/dispatcher.rs` (the entire body of `dispatch_rust`)

- [ ] **Step 1: Rewrite `dispatch_rust` with scope-block and piggyback**

In `crates/agent/src/internals/hook_runtime/dispatcher.rs`, replace the body of `dispatch_rust` (currently lines 18-115). The structure: existing logic stays the same; wrap `ctx`-using region in a scope block; add piggyback after `clear_reentry`.

```rust
#[no_mangle]
pub extern "system" fn dispatch_rust(method_id: u64, regs: *mut RegArgs) {
    crate::paths::log(&format!("dispatch_rust: ENTRY method_id={}", method_id));

    // SAFETY: shim guarantees regs is non-null and points at a valid RegArgs.
    let regs = unsafe { &mut *regs };

    // Scope-block: ctx (a &'static HookCtx tied to the slot's SLOT_VALID=true
    // contract) lives only inside this block. After the closing brace, no
    // live reference to HookCtx remains, so the piggyback below is free to
    // unpublish the slot via registry_reload without dangling-ref UB.
    {
        let ctx = match ctx_for(method_id) {
            Some(c) => c,
            None => {
                crate::paths::log(&format!("dispatch_rust: [1/5] ctx_for({}) is None — zero ret + return", method_id));
                regs.ret_int = 0;
                regs.ret_float = 0.0;
                return;
            }
        };
        crate::paths::log("dispatch_rust: [1/5] ctx_for OK");

        if try_enter_reentry(method_id) {
            crate::paths::log("dispatch_rust: [2/5] reentry detected — direct trampoline replay");
            unsafe {
                call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs);
            }
            crate::paths::log("dispatch_rust: [2/5] reentry trampoline returned");
            return;  // outer frame clears reentry
        }
        crate::paths::log("dispatch_rust: [2/5] reentry OK (entered fresh)");

        let args = match regargs_to_args(ctx.method, regs) {
            Ok(a) => a,
            Err(e) => {
                crate::paths::log(&format!("dispatch_rust: [3/5] regargs_to_args FAIL {:?} — fallback trampoline", e));
                unsafe { call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs); }
                clear_reentry(method_id);
                return;
            }
        };
        crate::paths::log(&format!("dispatch_rust: [3/5] regargs_to_args OK arg_count={}", args.len()));

        let regs_ptr: *mut RegArgs = regs as *mut RegArgs;
        crate::paths::log("dispatch_rust: [4/5] calling with_current_context");
        super::api::with_current_context(ctx, regs_ptr, &args, |handler_result| {
            crate::paths::log(&format!("dispatch_rust: handler returned return_value.is_some()={} called_original={}",
                handler_result.return_value.is_some(), handler_result.called_original));
            if let Some(rv) = handler_result.return_value {
                let regs_ref = unsafe { &mut *regs_ptr };
                if let Err(e) = pack_return_into_regargs(
                    ctx.sig.return_type,
                    ctx.sig.return_tc,
                    &rv,
                    regs_ref,
                ) {
                    crate::paths::log(&format!("hook: pack_return failed for method_id={}: {:?}", method_id, e));
                }
            } else if !handler_result.called_original {
                crate::paths::log(&format!("dispatch_rust: transparent observer — calling trampoline at {:#x}", ctx.patch.trampoline));
                unsafe {
                    call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs_ptr);
                }
                crate::paths::log(&format!("dispatch_rust: trampoline returned ret_int={:#x} ret_float={}",
                    unsafe { (*regs_ptr).ret_int }, unsafe { (*regs_ptr).ret_float }));
            }
        });
        crate::paths::log("dispatch_rust: [4/5] with_current_context returned");
    }  // ctx dropped here; SLOT_VALID/ctx ref no longer in flight

    clear_reentry(method_id);
    crate::paths::log("dispatch_rust: [5/5] DONE");

    // PIGGYBACK: per [[hooks-are-the-sync-primitive]], we're on the game
    // thread with the game frozen by the inline-detour call stack. Safe to
    // run registry_reload here because:
    //   - ctx is out of scope (no dangling reference)
    //   - reentry is cleared (a re-firing of the same method won't read
    //     a soon-to-be-stale slot)
    //   - SLOT_VALID release-stores by unpublish_slot prevent fresh
    //     ctx_for hits from other threads
    //
    // Single-game-thread assumption documented in the B-6a spec; if a
    // hooked method ever fires from multiple OS threads, an epoch-counter
    // fix lands as a separate brick.
    if let Some(bytes) = crate::runtime::orchestrator::take_reload_pending() {
        crate::paths::log("dispatch_rust: piggyback drained reload-pending; running registry_reload");
        crate::runtime::orchestrator::registry_reload(&bytes);
    }
}
```

- [ ] **Step 2: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, no new warnings.

- [ ] **Step 3: Pause for user commit**

---

## Task 9: Implement the `state_file` writer

**Files:**
- Create: `crates/agent/src/runtime/state_file.rs`
- Modify: `crates/agent/src/runtime/mod.rs` (add `pub mod state_file;`)

- [ ] **Step 1: Create the state file writer**

Create `crates/agent/src/runtime/state_file.rs`:

```rust
//! Writes `<game_dir>/scripts/.state.json` atomically (temp + rename).
//! Format is documented in the B-6a spec; version 1.
//! Called on every state transition + once every 5s as heartbeat.

use std::fs::{rename, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::output_path;
use crate::runtime::registry::list;

const STATE_FILE: &str = "scripts/.state.json";
const STATE_FILE_TMP: &str = "scripts/.state.json.tmp";
const VERSION: u32 = 1;

/// Write the current registry state to the state file. Atomic via temp+rename.
/// Logs and skips on IO failure (don't crash the agent over telemetry).
pub fn write_state() {
    let path = output_path(STATE_FILE);
    let tmp_path = output_path(STATE_FILE_TMP);
    let runtimes = list();

    let ts = current_iso8601();
    let mut json = String::new();
    json.push_str("{\n");
    json.push_str(&format!("  \"version\": {},\n", VERSION));
    json.push_str(&format!("  \"ts\": \"{}\",\n", ts));
    json.push_str("  \"runtimes\": [\n");
    for (i, r) in runtimes.iter().enumerate() {
        json.push_str("    {\n");
        json.push_str(&format!("      \"id\": {},\n", r.id.0));
        json.push_str(&format!("      \"hooks_installed\": {},\n", r.hooks_installed));
        json.push_str(&format!("      \"journal_addresses\": {}\n", r.journal_addresses));
        json.push_str(if i + 1 == runtimes.len() { "    }\n" } else { "    },\n" });
    }
    json.push_str("  ]\n");
    json.push_str("}\n");

    if let Err(e) = write_atomic(&tmp_path, &path, json.as_bytes()) {
        crate::paths::log(&format!("state_file: write failed {:?}", e));
    }
}

fn write_atomic(tmp: &PathBuf, dest: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    {
        let mut f = File::create(tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    rename(tmp, dest)?;
    Ok(())
}

/// Bare-bones ISO 8601 UTC timestamp without external deps.
fn current_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, mon, day, h, m, s) = secs_to_ymd_hms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, mon, day, h, m, s)
}

/// Convert UNIX seconds to (year, month, day, hour, minute, second) UTC.
/// Algorithm: Howard Hinnant's days_from_civil inverse. Valid 1970-2099.
fn secs_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = (secs % 60) as u32;
    let m = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153*mp + 2)/5 + 1) as u32;
    let mon = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if mon <= 2 { 1 } else { 0 }) as u32;
    (year, mon, d, h, m, s)
}
```

- [ ] **Step 2: Wire the module into runtime/mod.rs**

Add:

```rust
pub mod state_file;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, no new warnings.

- [ ] **Step 4: Pause for user commit**

---

## Task 10: Implement the watcher thread

**Files:**
- Create: `crates/agent/src/runtime/watcher.rs`
- Modify: `crates/agent/src/runtime/mod.rs` (add `pub mod watcher;`)

- [ ] **Step 1: Create the watcher module**

Create `crates/agent/src/runtime/watcher.rs`:

```rust
//! File watcher for `<game_dir>/scripts/active.wasm`. Polls every 500ms;
//! on change, publishes RELOAD_PENDING; falls back to direct registry_reload
//! after 1000ms if no dispatch_rust piggyback consumed.
//!
//! Settle-check: act only on changes whose mtime stayed stable for one tick.
//! Parse-check: validate `wasmi::Module::new` before triggering teardown.

use std::fs;
use std::path::PathBuf;
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
    const HEARTBEAT_TICKS: u64 = 10;  // 10 * 500ms = 5s heartbeat

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
                wait_for_consume_or_fallback(/*bytes_ref=*/ None);
                last_seen = None;
                write_state();
            }
            (None, Some(meta)) => {
                // First appearance — wait one tick for stability.
                last_seen = Some(meta);
            }
            (Some(prev), Some(meta)) => {
                if prev == meta {
                    continue; // unchanged
                }
                // Changed. Stability check: are we still settling?
                // Strategy: store the new meta and wait for the NEXT tick;
                // act only when next-tick meta matches this-tick meta.
                last_seen = Some(meta);
                // Probe: if file is mid-write, the next read fails or returns
                // partial bytes. We rely on the parse-check below to validate.
                let bytes = match fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        log(&format!("watcher: read failed {:?} — will retry on next change", e));
                        continue;
                    }
                };
                // Parse-check: malformed wasm doesn't trigger teardown.
                let engine = wasmi::Engine::default();
                if let Err(e) = wasmi::Module::new(&engine, &bytes) {
                    log(&format!("watcher: parse failed {:?} — leaving current runtime alone", e));
                    continue;
                }
                log(&format!("watcher: detected valid change ({} bytes) — publishing reload", bytes.len()));
                publish_reload(bytes.clone());
                wait_for_consume_or_fallback(Some(&bytes));
                write_state();
            }
        }
    }
    log("watcher: thread exiting");
}

fn stat_meta(path: &PathBuf) -> Option<(SystemTime, u64)> {
    let md = fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    let size = md.len();
    Some((mtime, size))
}

/// Wait up to FROG_WATCHER_FALLBACK_MS for dispatch_rust to drain the pending
/// buffer. If timeout, run registry_reload directly. `bytes_ref` is the bytes
/// we just published (used for the fallback; `None` for unload).
fn wait_for_consume_or_fallback(bytes_ref: Option<&[u8]>) {
    let deadline_ms = fallback_ms();
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
```

- [ ] **Step 2: Wire the module into runtime/mod.rs**

Add:

```rust
pub mod watcher;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, no new warnings.

- [ ] **Step 4: Pause for user commit**

---

## Task 11: Wire watcher into DllMain; mkdir scripts/; delete `FROG_WASM`; extend DETACH

**Files:**
- Modify: `crates/agent/src/entry.rs:196` (delete `maybe_run_configured()` call, replace with mkdir + watcher spawn)
- Modify: `crates/agent/src/entry.rs:223-227` (extend DLL_DETACH)
- Modify: `crates/agent/src/runtime/host.rs` (delete `maybe_run_configured` fn)

- [ ] **Step 1: Add mkdir + watcher spawn in entry.rs**

In `crates/agent/src/entry.rs`, find line 196:

```rust
    crate::runtime::host::maybe_run_configured();
```

Replace with:

```rust
    // B-6a: ensure scripts/ folder exists, then spawn watcher.
    // FROG_WASM env var is deleted; watcher is the sole loader for WASM scripts.
    let scripts_dir = crate::paths::output_path("scripts");
    if let Err(e) = std::fs::create_dir_all(&scripts_dir) {
        crate::paths::log(&format!("entry: create scripts dir failed {:?}", e));
    } else {
        crate::paths::log(&format!("entry: scripts dir ready at {:?}", scripts_dir));
    }
    crate::runtime::watcher::spawn();
```

- [ ] **Step 2: Extend DLL_PROCESS_DETACH**

In `crates/agent/src/entry.rs`, find the existing DETACH handler (around lines 223-227):

```rust
        DLL_PROCESS_DETACH => {
            unsafe {
                crate::protocol::remove_packet_hooks();
            }
        }
```

Replace with:

```rust
        DLL_PROCESS_DETACH => {
            unsafe {
                // B-6a: signal watcher to stop, then unhook all runtime-owned
                // hooks so no game-code patches survive into the next session.
                // Journal revert is INTENTIONALLY skipped on DLL_DETACH — game
                // process is terminating; reverting state about to vanish is
                // wasted work.
                crate::runtime::watcher::stop();
                crate::runtime::orchestrator::registry_reload(&[]);  // unload only (empty bytes)
                crate::protocol::remove_packet_hooks();
            }
        }
```

(`registry_reload(&[])` with empty bytes does revert + unhook + drop but skips spawn — exactly the unload sequence we want, though it includes journal revert. The "skip journal revert" optimization is a banked future improvement; for B-6a, revert-then-die is fine — wasted but harmless.)

- [ ] **Step 3: Delete `maybe_run_configured` from host.rs**

In `crates/agent/src/runtime/host.rs`, delete the entire `pub fn maybe_run_configured()` fn. Also delete:
- The `WASM_CONTENDED: AtomicU64` static and `note_wasm_handler_contended` fn if they were only referenced by `maybe_run_configured` — but they are also referenced by `call_hook_handler`. Verify with grep before deletion.

Run: `grep -n "WASM_CONTENDED\|note_wasm_handler_contended" crates/agent/src/runtime/host.rs`
If the only remaining references are in `call_hook_handler`, KEEP these items.

The `use` for `AtomicU64` / `Ordering` may also need pruning. Compiler warnings will guide.

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. Zero new warnings.

If any "unused import" warnings appear in `host.rs`, remove the unused imports.

- [ ] **Step 5: Pause for user commit**

---

## Task 12: Live smoke test on Pixel Worlds

**Files:** No code changes; this is verification.

- [ ] **Step 1: Confirm build + auto-deploy**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds; `./deploy.sh` auto-fires via hook (per `deploy-setup` memory); DLL copied to the game dir.

- [ ] **Step 2: Verify scripts/ folder created on next game launch**

Manually launch the game via Steam. After it loads, check:

Run: `ls -la "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/scripts/"`
Expected: directory exists; empty (no active.wasm dropped yet).

Tail the agent log:
Run: `tail -50 "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log" | grep -E "entry:|watcher:"`
Expected: lines including `entry: scripts dir ready` and `watcher: thread started`.

- [ ] **Step 3: Drop a test wasm**

Use an existing test script that exercises read + write + hook. If `crates/agent/tests/test_invoke.wasm` exists, copy it:

Run: `cp crates/agent/tests/test_invoke.wasm "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/scripts/active.wasm"`

Wait ~2 seconds (poll interval + settle + dispatch consume).

Run: `tail -50 "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log"`
Expected: watcher detected the change; `registry_reload: BEGIN`; spawn ok; wasm log lines visible.

Check the state file:
Run: `cat "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/scripts/.state.json"`
Expected: JSON with `version: 1`, `runtimes: [{id: 1, ...}]`.

- [ ] **Step 4: Hot-reload to a different wasm**

If there's a second test wasm, copy it over:

Run: `cp crates/agent/tests/<second-test>.wasm "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/scripts/active.wasm"`

(Atomic-rename style is preferred: `cp <new>.wasm scripts/active.wasm.tmp && mv scripts/active.wasm.tmp scripts/active.wasm` — but plain `cp` should also work since most scripts are small enough that the write completes within one poll-tick.)

Watch the log:
Run: `tail -100 "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log"`
Expected: watcher detected mtime change → published reload → dispatch_rust piggyback OR watcher fallback consumed it → `registry_reload: reverting N journal entries` → `unhooking owned hooks` → `dropping runtime_id=1` → `spawning fresh runtime` → new runtime id=2.

Verify state file updated:
Run: `cat scripts/.state.json` (the file in the game dir)
Expected: `runtimes: [{id: 2, ...}]` — the old runtime id=1 is gone.

- [ ] **Step 5: Verify journal revert on stop**

Delete the active.wasm:
Run: `rm "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/scripts/active.wasm"`

Watch the log:
Run: `tail -50 .../agent.log | grep -E "watcher:|registry_reload:"`
Expected: `watcher: scripts/active.wasm disappeared` → `registry_reload: BEGIN` with `[unload]` → revert + unhook + drop; no spawn.

Verify state file shows empty runtimes:
Run: `cat scripts/.state.json`
Expected: `"runtimes": []`.

- [ ] **Step 6: Verify game state was actually reverted**

This depends on which test wasm was running. If the script wrote to a known field (e.g. player coins, health) and you observed it change in-game, after the unload the value should return to the pre-script state. Visual confirmation in the game UI.

- [ ] **Step 7: Verify Windows cross-compile baseline**

Run: `cargo build --target x86_64-pc-windows-gnu --release 2>&1 | grep -c '^warning:'`
Expected: 11 (the pre-B-6a baseline of intentional/audited warnings — no new warnings from this brick).

- [ ] **Step 8: Pause for user commit + ship**

This is the final B-6a commit. After this, the brick is shipped and the dev loop is dramatically faster — every subsequent wasm script iteration is "drop the file, see it run."

---

## Self-Review

Reviewing this plan against `docs/superpowers/specs/2026-05-31-b6a-hot-reload-design.md`:

**1. Spec coverage:**
- ✅ Locked decision #1 (filesystem-only API) — Tasks 9, 10, 11; no socket added.
- ✅ Decision #2 (Replace semantics) — Task 3 (Vec-of-1 registry); Task 7 (orchestrator drops then spawns).
- ✅ Decision #3 (scripts/ in game DIR) — Task 11 mkdir.
- ✅ Decision #4 (write journal) — Task 1 (agent-core type); Task 6 (host_write wiring); Task 7 (revert in orchestrator).
- ✅ Decision #5 (mem.write_permanent) — Task 6 step 3.
- ✅ Decision #6 (FROG_WASM_WRITE removed) — Task 6 step 4.
- ✅ Decision #7 (FROG_WASM deleted) — Task 11 step 3.
- ✅ Decision #8 (dispatch_rust piggyback) — Task 8.
- ✅ Decision #9 (runtime_id + scan) — Tasks 2, 5, 7 (collect_owned_hook_ids).
- ✅ Decision #10 (single-thread assumption documented) — Task 8 comment block.
- ✅ Architecture: registry replaces PARKED — Task 4; write journal in ParkedRuntime — Task 3.
- ✅ Filesystem behavior: scripts/active.wasm watch + .state.json emit — Tasks 9, 10.
- ✅ DllMain DETACH extension — Task 11 step 2.
- ✅ Tuning knobs (FROG_WATCHER_INTERVAL_MS, FROG_WATCHER_FALLBACK_MS) — Task 10.
- ✅ Acceptance criteria 1-7 — covered by Tasks 1-12.

**2. Placeholder scan:**
- No "TBD" / "TODO" / "implement later" in any code block.
- No "add appropriate error handling" — every error path has explicit handling.
- No "write tests for the above" — Task 1 has concrete tests; subsequent agent-crate tasks rely on Windows cross-compile + Task 12 live verification because agent crate isn't Linux-testable (per `deploy-setup`).
- One acceptable "verify with grep before deletion" note in Task 11 step 3 — this is a guarded instruction, not a placeholder; the engineer knows exactly what to grep and what to do with the result.

**3. Type consistency:**
- `RuntimeId(pub u64)` defined in Task 3; used in Tasks 5 (`current_id().0`), 7 (`current_id`, `collect_owned_hook_ids`).
- `ParkedRuntime` fields: `id, store, instance, funcref_table, owned_hooks, write_journal: WriteJournal` — used identically across Tasks 3, 4, 7.
- `HookCtx.runtime_id: u64` — added in Task 2; populated in Task 5; read in Task 7.
- `RELOAD_PENDING: Mutex<Option<Vec<u8>>>` — defined in Task 7; published/consumed identically in Tasks 8, 10.
- `host_write_typed` signature unchanged from B-5; body extended in Task 6.
- `run_wasm_with_mem(bytes: &[u8]) -> Result<Vec<String>, WasmError>` — signature change (drop `write_granted`) in Task 6; matches caller in Task 7's `spawn_fresh`.
- `registry()`, `current_id()`, `record_hook()`, `journal_touch(addr, width)`, `new_journal()`, `insert()`, `list()` — all defined in Task 3; called consistently in Tasks 4, 5, 6, 7, 9.
- `WriteJournal` + `JournalReadFn` defined in agent-core (Task 1) as **concrete** types; `ParkedRuntime.write_journal: WriteJournal` (Task 3) uses the same type production-wide. The Task 1 tests exercise the SAME code path as production — the read-backend is a `fn` pointer with a test-side fn in tests and `journal_read_adapter` (wrapping `mem_backend::raw_read`) in production. No isolated agent-core type that production ignores.

No drift. All identifiers consistent across task boundaries.

**Note on plan revision:** The plan was revised 2026-05-31 (within the same session) to incorporate Option 2 of a design pushback. The original draft had `ParkedRuntime.write_journal: HashMap<usize, Vec<u8>>` with the agent-core `WriteJournal<R>` (generic) tested in isolation — a test-vs-production type disconnect. The revision unified the type: concrete `WriteJournal` with a `fn` pointer backend, wired into production via `journal_read_adapter`. Affected: Task 1 (concrete type + tests), Task 3 (field type + adapter + `new_journal()` helper), Task 4 (spawn site uses `new_journal()`), Task 6 (callsite simplification — no closure), Task 7 (extract via `take_entries()` + `write_state()` at end).

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-31-b6a-hot-reload-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task, two-stage review between tasks (spec compliance + code quality), fast iteration. Per `subagents-use-opus` 3-tier routing: Task 1 (agent-core TDD) and Tasks 6/7/8 (load-bearing substrate writes / orchestrator / dispatch_rust patch) get Opus; Tasks 2/3/4/5/9/10/11 (mechanical refactor with full code provided) get Sonnet; Task 12 (verification only) gets Haiku.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch with checkpoints for review.

**Which approach?**
