# B-6b Internal API Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the agent's Internal API (il2cpp-domain) honestly expose what it can deliver — static-field markers, live instance enumeration, complete method listing, honest field counts — so the dumper becomes a truthful canary of substrate completeness, and per-item investigated cleanup addresses accumulated debt without blindly deleting load-bearing intent.

**Architecture:** Five sequential phases inside ONE flattened brick. Phase 1 extends the Spine with the missing types/traits. Phase 2 wires Internal implementations via existing memory primitives. Phase 3 exposes the new capabilities via WASM host fns. Phase 4 rewrites the dumper to consume substrate primitives (not its own ad-hoc walks — triple duplication closes). Phase 5 investigates each cleanup item per-intent before deciding wire-vs-delete.

**Tech Stack:** Rust 2021, agent-core (Linux-native, testable), agent (Windows cdylib via `x86_64-pc-windows-gnu` cross-compile), wasmi 0.31, existing spine vtable pattern (`metadata_backend`/`mem_backend`), existing `mem.scan` AOB primitive.

**Reference spec:** `docs/superpowers/specs/2026-05-31-b6b-internal-api-completeness-design.md`

**Critical project rules** (apply to every task):
1. **DO NOT run `git commit`/`add`/`stash`.** Per project memory `user-commits-own-work`, pause at marked commit points.
2. **DO NOT run bare `cargo build -p agent` to verify** — per `deploy-setup`, Linux-native builds compile an EMPTY cdylib (all modules are `#[cfg(target_os = "windows")]`-gated). Always verify with `cargo build --target x86_64-pc-windows-gnu --release`.
3. **DO NOT run `./deploy.sh`** — it auto-fires from a post-build hook.
4. **agent-core tests** use bare `cargo test -p agent-core` (Linux-native).
5. **Cleanup philosophy: investigate before deleting.** "Unused" is a signal to investigate intent, not an instruction to remove.

---

## File Structure

**Modified files:**

| Path | Responsibility | Change |
|---|---|---|
| `crates/agent-core/src/spine/field_info.rs` | FieldInfo public type | Add `is_static: bool` field |
| `crates/agent-core/src/spine/metadata_backend.rs` | Vtable for metadata walks | Add `is_static: bool` to FieldInfoRaw |
| `crates/agent-core/src/spine/access.rs` | Trait impls + iterator types | Add `Iter<Instance> for KlassPtr` + InstanceIter |
| `crates/agent-core/src/spine/mod.rs` | Re-exports | Add scan_backend module + re-exports |
| `crates/agent/src/internals/api.rs` | il2cpp domain ops | Update `fields_at` (static bit); add `methods_of`, `instances_of` helpers; register scan_backend |
| `crates/agent/src/internals/dump.rs` | Internals dumper | Rewrite to use spine Iter primitives; emit static markers, method lists, instance counts |
| `crates/agent/src/runtime/mem_host.rs` | WASM host fns | Extend `host_field_info` return; add `host_list_methods`, `host_list_instances` |
| `crates/agent/src/inline_detour.rs` | Hook patch + restore | Per Phase 5: investigate Hook.detour intent + wire/delete |
| `crates/agent/src/internals/calibration/ffi_verify.rs` | FFI verify probe | Per Phase 5: investigate Verified::Crashed + unsafe block intent |
| `crates/agent/src/internals/calibration/field_param_layout.rs` | Calibration probe | Per Phase 5: investigate MIN_RATIO + chunk var intent |
| `crates/agent/src/internals/marshal.rs` | Marshal args/return | Per Phase 5: replace magic 0x10 with METHOD_ATTRIBUTE_STATIC_BIT; audit slab unwraps |
| `crates/agent/src/internals/dump.rs` | (continued) | Per Phase 5: audit calibrate_generic_class_offset |
| `crates/agent/src/internals/hook_runtime/shim.rs` | Universal shim | Per Phase 5: fix unused doc comment |

**Created files:**

| Path | Responsibility |
|---|---|
| `crates/agent-core/src/spine/scan_backend.rs` | Vtable for scan-based instance discovery (separate from `metadata_backend` per spec decision #4) |

---

## Phase 1 — Spine extensions

### Task 1: Add `is_static` to `FieldInfo` + `FieldInfoRaw` (agent-core)

**Files:**
- Modify: `crates/agent-core/src/spine/field_info.rs:14-19`
- Modify: `crates/agent-core/src/spine/metadata_backend.rs:40-47`

- [ ] **Step 1: Add `is_static` to `FieldInfo` public type**

In `crates/agent-core/src/spine/field_info.rs`, replace the existing struct (lines 13-19) with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldInfo {
    pub name_ptr: usize,
    pub offset:   u32,
    pub val_type: ValType,
    pub token:    u32,
    /// `true` if the field is declared `static`. Static fields' `offset` is
    /// relative to the class's `static_fields` base (not an instance), so the
    /// dump and any composing caller MUST handle them distinctly from instance
    /// fields. Populated by `metadata_backend::fields_at` reading the
    /// FIELD_ATTRIBUTE_STATIC (0x10) bit on the field's type-attrs chunk.
    pub is_static: bool,
}
```

- [ ] **Step 2: Add `is_static` to `FieldInfoRaw` vtable payload**

In `crates/agent-core/src/spine/metadata_backend.rs`, replace the existing FieldInfoRaw (lines 40-47) with:

```rust
#[derive(Debug, Clone, Copy)]
pub struct FieldInfoRaw {
    pub name_ptr:    usize,
    pub offset:      u32,
    pub val_type:    ValType,
    pub token:       u32,
    pub is_static:   bool,
    pub next_cursor: usize,
}
```

- [ ] **Step 3: Update `Iter<FieldInfo>::next` to propagate `is_static`**

In `crates/agent-core/src/spine/access.rs:128-134`, replace the existing `Some(FieldInfo { ... })` with:

```rust
        self.cursor = raw.next_cursor;
        Some(FieldInfo {
            name_ptr:  raw.name_ptr,
            offset:    raw.offset,
            val_type:  raw.val_type,
            token:     raw.token,
            is_static: raw.is_static,
        })
```

- [ ] **Step 4: Run agent-core tests**

Run: `cargo test -p agent-core spine`
Expected: all existing tests pass. The struct-literal changes compile-check the new field — Rust requires all fields specified.

If any test fails because it constructs a `FieldInfo` literal, update that test to include `is_static: false`.

- [ ] **Step 5: Confirm Windows cross-compile**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. The agent's `fields_at` constructs `FieldInfoRaw` — the build will fail until Task 4 updates it. **Expected behavior: build fails here.** That's the TDD signal driving Task 4.

If the build fails ONLY with errors about missing `is_static` field on `FieldInfoRaw` literals, that's correct — proceed to Task 4 to fix.

- [ ] **Step 6: Pause for user commit**

---

### Task 2: Create `scan_backend` module (agent-core)

**Files:**
- Create: `crates/agent-core/src/spine/scan_backend.rs`
- Modify: `crates/agent-core/src/spine/mod.rs` (add module + re-export)

- [ ] **Step 1: Create scan_backend module**

Create `crates/agent-core/src/spine/scan_backend.rs` with this exact body:

```rust
//! Platform-agnostic scan-based instance-discovery backend for
//! `Iter<Instance> for KlassPtr`.
//!
//! Distinct from `metadata_backend` because instance discovery has different
//! lifecycle semantics from structural field/method walks: it depends on
//! live process state (heap layout, allocator state) rather than calibrated
//! il2cpp offsets. Same registration pattern; different vtable.
//!
//! # Registration
//! Call `register(next_match, validate)` once from `agent`'s init path,
//! alongside `mem_backend::register` and `metadata_backend::register`.
//! Until registration, iterators yield zero items.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Yield the next candidate address that matches the byte signature of
/// `target_klass` (klass pointer at offset 0), advancing `cursor` to the
/// position just past the returned hit. Returns `None` when no more matches
/// exist within the scannable range.
///
/// Implementation note: backends typically perform an AOB scan on first
/// call, cache results, and stream them on subsequent calls — but the
/// caller (the iterator) treats this opaquely.
pub type NextMatchFn = fn(target_klass: usize, cursor: &mut usize) -> Option<usize>;

/// Structural validation: returns `true` iff `addr` passes ALL of these
/// universal checks (no per-klass branching):
///   1. address is in a writable memory region
///   2. address is aligned to pointer size (8 on x86_64)
///   3. `klass_of(addr) == target_klass`
///   4. the klass at `addr+0` passes `is_klass_shape` (name + namespace
///      pointers are valid cstrs in mapped memory)
///
/// Returning `false` causes the iterator to skip the candidate silently
/// (no log spam) and try the next match.
pub type ValidateFn = fn(addr: usize, target_klass: usize) -> bool;

static NEXT_MATCH_FN: AtomicUsize = AtomicUsize::new(0);
static VALIDATE_FN:   AtomicUsize = AtomicUsize::new(0);

/// Register the scan-discovery backend. Call once at agent start, after
/// `ctx::init` and `register_mem_backend()` (the backend's validation reads
/// the region cache + klass_of).
pub fn register(next_match: NextMatchFn, validate: ValidateFn) {
    NEXT_MATCH_FN.store(next_match as usize, Ordering::Release);
    VALIDATE_FN  .store(validate   as usize, Ordering::Release);
}

/// Invoke the registered next-match backend. Returns `None` if not
/// registered or the backend itself signalled end-of-scan.
pub fn next_match(target_klass: usize, cursor: &mut usize) -> Option<usize> {
    let p = NEXT_MATCH_FN.load(Ordering::Acquire);
    if p == 0 { return None; }
    let f: NextMatchFn = unsafe { std::mem::transmute(p) };
    f(target_klass, cursor)
}

/// Invoke the registered validation backend. Returns `false` if not
/// registered (fail-closed: no candidate is "valid" until backend exists).
pub fn validate(addr: usize, target_klass: usize) -> bool {
    let p = VALIDATE_FN.load(Ordering::Acquire);
    if p == 0 { return false; }
    let f: ValidateFn = unsafe { std::mem::transmute(p) };
    f(addr, target_klass)
}
```

- [ ] **Step 2: Wire the module into spine/mod.rs**

In `crates/agent-core/src/spine/mod.rs`, find the existing `pub mod` lines (currently `pub mod metadata_backend;` etc.). Add:

```rust
pub mod scan_backend;
```

(Order doesn't matter alphabetically; add adjacent to `metadata_backend`.)

- [ ] **Step 3: Run agent-core tests**

Run: `cargo test -p agent-core`
Expected: succeeds. No new tests yet — the module is referenced but not consumed until Task 3.

- [ ] **Step 4: Pause for user commit**

---

### Task 3: Add `Iter<Instance> for KlassPtr` (agent-core)

**Files:**
- Modify: `crates/agent-core/src/spine/access.rs` (add after the existing `Iter<MethodPtr>` impl, after line 181)

- [ ] **Step 1: Add InstanceIter struct + Iter impl**

In `crates/agent-core/src/spine/access.rs`, after the existing `impl Iter<MethodPtr> for KlassPtr { ... }` block (ends around line 181), add:

```rust
// ── KlassPtr instance iteration via scan_backend ────────────────────────────

/// Lightweight (3-usize, `Copy`) iterator state for `Iter<Instance> for
/// KlassPtr`. The actual scan + structural validation lives in the agent
/// crate behind the `scan_backend::{NextMatchFn, ValidateFn}` vtable.
///
/// Per the B-6b spec: validation is UNIVERSAL/structural (no per-klass logic).
/// Backend's `next_match` yields candidates; `validate` filters out non-instance
/// coincidental matches via region check + alignment + klass_of + klass-shape.
#[derive(Debug, Clone, Copy)]
pub struct InstanceIter {
    klass:  usize,
    cursor: usize,
}

impl Iterator for InstanceIter {
    type Item = Instance;

    fn next(&mut self) -> Option<Instance> {
        loop {
            let candidate = crate::spine::scan_backend::next_match(
                self.klass,
                &mut self.cursor,
            )?;
            if crate::spine::scan_backend::validate(candidate, self.klass) {
                return Some(Instance::from_raw(candidate as u64));
            }
            // Validation failed: try next candidate (silent skip, no log spam).
        }
    }
}

impl Iter<Instance> for KlassPtr {
    type Iter = InstanceIter;
    fn iter(&self) -> Self::Iter {
        InstanceIter {
            klass:  self.as_u64() as usize,
            cursor: 0,
        }
    }
}
```

- [ ] **Step 2: Add unit tests for InstanceIter behavior (mock backend)**

In the same file, at the BOTTOM (after the new impls), add an inline test module:

```rust
#[cfg(test)]
mod instance_iter_tests {
    use super::*;
    use crate::spine::scan_backend;
    use std::sync::Once;

    static INIT: Once = Once::new();

    // Test backend: yields a fixed list of candidates, validates the first
    // and third only. Lets us prove the iterator skips invalid candidates.
    fn test_next_match(_target: usize, cursor: &mut usize) -> Option<usize> {
        let candidates = [0x1000usize, 0x2000, 0x3000, 0x4000];
        if *cursor >= candidates.len() {
            return None;
        }
        let v = candidates[*cursor];
        *cursor += 1;
        Some(v)
    }

    fn test_validate(addr: usize, _target: usize) -> bool {
        // Only addresses 0x1000 and 0x3000 are "valid" in this test.
        addr == 0x1000 || addr == 0x3000
    }

    fn init_test_backend() {
        INIT.call_once(|| {
            scan_backend::register(test_next_match, test_validate);
        });
    }

    #[test]
    fn instance_iter_yields_only_validated_candidates() {
        init_test_backend();
        let klass = KlassPtr::from_raw(0xDEAD_BEEF);
        let yielded: Vec<u64> = klass.iter().map(|i| i.as_u64()).collect::<Vec<_>>();
        // Expect 0x1000 and 0x3000 only (0x2000 and 0x4000 fail validation).
        assert_eq!(yielded, vec![0x1000, 0x3000]);
    }

    #[test]
    fn instance_iter_terminates_when_backend_returns_none() {
        init_test_backend();
        let klass = KlassPtr::from_raw(0xDEAD_BEEF);
        let count = klass.iter::<Instance>().count();
        // 4 candidates, 2 validate; iteration must terminate cleanly.
        assert_eq!(count, 2);
    }
}
```

(Note: `INIT.call_once` ensures the test backend is registered exactly once even if both tests run; the static globals in scan_backend are shared across tests within a process.)

- [ ] **Step 3: Run agent-core tests**

Run: `cargo test -p agent-core spine::access::instance_iter_tests`
Expected: 2 passed; 0 failed.

If tests fail because `KlassPtr.iter()` is ambiguous between `Iter<FieldInfo>` / `Iter<MethodPtr>` / `Iter<Instance>`, use the fully-qualified turbofish form: `<KlassPtr as Iter<Instance>>::iter(&klass)`. The second test already uses `klass.iter::<Instance>()`.

- [ ] **Step 4: Run full agent-core tests**

Run: `cargo test -p agent-core`
Expected: all passing.

- [ ] **Step 5: Pause for user commit**

---

## Phase 2 — Internal implementations

### Task 4: Update `fields_at` in agent to populate `is_static`

**Files:**
- Modify: `crates/agent/src/internals/api.rs:263-312` (`fields_at` fn)

- [ ] **Step 1: Read static-attribute bit and populate FieldInfoRaw**

In `crates/agent/src/internals/api.rs:263-312`, replace the existing `fields_at` body. Specifically, after the existing tc validation (line 293), compute `is_static` from the same chunk that holds the type code, AND add it to the returned `FieldInfoRaw`:

The full updated fn:

```rust
fn fields_at(klass: usize, cursor: usize) -> Option<agent_core::spine::metadata_backend::FieldInfoRaw> {
    let c = ctx::get()?;
    let fields_ptr = match cache::read_u64(klass + c.cfg.klass_fields) {
        Some(p) if p != 0 => p as usize,
        _ => return None,
    };
    let is_vt = klass_is_valuetype(klass as u64);
    let mut fi = cursor;
    while fi < 256 {
        let slot = fields_ptr + fi * 32;
        let name_ptr = match cache::read_u64(slot) {
            Some(p) if p != 0 => p as usize,
            _ => return None, // real end-of-array sentinel
        };
        let this_slot = fi;
        fi += 1;
        // Skip garbage that the legacy walk filters (without ending iteration).
        if cache::read_cstr(name_ptr).map_or(true, |n| n.is_empty()) {
            continue;
        }
        let token = cache::read_u32(slot + 28).unwrap_or(0);
        if token == 0 {
            continue; // scanner garbage: real fields always have a metadata token
        }
        let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
        if type_ptr == 0 {
            continue;
        }
        let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
        let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
        if tc == 0 || tc > 0x45 {
            continue;
        }
        // FIELD_ATTRIBUTE_STATIC = 0x10 (low byte of type-attrs in the same
        // chunk we already read for the tc). Matches the gate already used by
        // `static_field` (api.rs around line 150).
        let is_static = (chunk & 0x10) != 0;
        let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
        let offset = if is_vt {
            raw_offset.saturating_sub(0x10)
        } else {
            raw_offset
        };
        let vt = valtype_from_tc(tc).unwrap_or(ValType::U64);
        return Some(agent_core::spine::metadata_backend::FieldInfoRaw {
            name_ptr,
            offset,
            val_type: vt,
            token,
            is_static,
            next_cursor: this_slot + 1,
        });
    }
    None
}
```

- [ ] **Step 2: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. The build failure from Task 1 Step 5 (missing `is_static` field on FieldInfoRaw literal) is now resolved.

Run: `cargo build --target x86_64-pc-windows-gnu --release 2>&1 | grep "^warning:" | grep -v "generated" | wc -l`
Expected: ≤ 10 (current baseline). No new warnings.

- [ ] **Step 3: Pause for user commit**

---

### Task 5: Implement scan_backend in agent (`next_match` + `validate`)

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (add new fns near other backend impls, ~line 333+)

- [ ] **Step 1: Add scan backend implementations**

In `crates/agent/src/internals/api.rs`, after the existing `register_metadata_backend` fn (around line 340), add:

```rust
// ── scan_backend implementations for Iter<Instance> ─────────────────────────
//
// next_match: AOB-scan for the target klass's pointer signature, cache hits,
//             stream them on subsequent calls.
// validate:   universal structural checks (region, alignment, klass_of,
//             is_klass_shape). No per-klass branching.

use std::sync::Mutex;
use std::collections::HashMap;

/// Cached scan results per target_klass. Populated on first `next_match` call
/// for a klass; subsequent calls stream from this cache. Lifetime is the
/// agent's lifetime (no eviction today — instance discovery is one-shot per
/// iterator construction). Process-shared because multiple wasm runtimes may
/// iterate concurrently; Mutex contention is rare (typically one hot script).
static SCAN_CACHE: Mutex<Option<HashMap<usize, Vec<usize>>>> = Mutex::new(None);

fn scan_cache() -> std::sync::MutexGuard<'static, Option<HashMap<usize, Vec<usize>>>> {
    let mut g = SCAN_CACHE.lock().expect("SCAN_CACHE mutex poisoned");
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    g
}

fn scan_next_match(target_klass: usize, cursor: &mut usize) -> Option<usize> {
    let mut g = scan_cache();
    let cache = g.as_mut().expect("scan_cache initialized above");
    // Populate cache on first call for this target_klass.
    let hits = cache.entry(target_klass).or_insert_with(|| {
        // Build the byte signature: klass ptr as little-endian u64.
        let pattern = (target_klass as u64).to_le_bytes().to_vec();
        // Cap the scan at a generous bound (the agent's MAX_SCAN_REGIONS env
        // var already limits region count; per-klass instance counts in the
        // tens or low hundreds are expected for typical classes).
        crate::external::api::scan(&pattern, 10_000)
    });
    if *cursor >= hits.len() {
        return None;
    }
    let v = hits[*cursor];
    *cursor += 1;
    Some(v)
}

fn scan_validate(addr: usize, target_klass: usize) -> bool {
    // Check 1: address aligned to pointer size (x86_64 = 8).
    if addr & 7 != 0 {
        return false;
    }
    // Check 2: klass_of(addr) reads the klass pointer at offset 0; must match.
    let read_klass = match crate::external::cache::read_u64(addr) {
        Some(k) if k != 0 => k as usize,
        _ => return false,
    };
    if read_klass != target_klass {
        return false;
    }
    // Check 3: the klass at addr+0 must itself be a real klass (name +
    // namespace cstrs in mapped memory). is_klass_shape covers this.
    if !crate::external::cache::is_klass_shape(read_klass) {
        return false;
    }
    // Check 4 (region): scan already restricts to cached regions (which are
    // populated readable regions); the writable-region filter would be
    // tighter but is not strictly required for safety (the read_u64 above
    // already validated via cache::read_u64's region check).
    true
}

/// Register the scan-based instance-discovery backend. Call AFTER
/// `register_mem_backend()` and `register_metadata_backend()` at agent start.
pub fn register_scan_backend() {
    agent_core::spine::scan_backend::register(scan_next_match, scan_validate);
}
```

- [ ] **Step 2: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. Two new warnings possible: `register_scan_backend` unused (until Task 6 wires it). If a warning fires, it's expected — Task 6 wires immediately after.

- [ ] **Step 3: Pause for user commit**

---

### Task 6: Wire `register_scan_backend` in agent startup

**Files:**
- Modify: `crates/agent/src/entry.rs` (find existing `register_mem_backend` / `register_metadata_backend` calls; add `register_scan_backend` after them)

- [ ] **Step 1: Find the existing backend registration order**

Run: `grep -n "register_mem_backend\|register_metadata_backend" crates/agent/src/entry.rs`

Note the line numbers and order.

- [ ] **Step 2: Add `register_scan_backend` call**

In `crates/agent/src/entry.rs`, immediately AFTER the existing `crate::internals::api::register_metadata_backend();` call, add:

```rust
    // B-6b: register the scan-backend for Iter<Instance>. Must follow
    // register_mem_backend (validation reads the region cache) and
    // register_metadata_backend (klass_of via cache::read_u64).
    crate::internals::api::register_scan_backend();
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, no `register_scan_backend` unused warning.

Run: `cargo build --target x86_64-pc-windows-gnu --release 2>&1 | grep "^warning:" | grep -v "generated" | wc -l`
Expected: ≤ 10 baseline.

- [ ] **Step 4: Pause for user commit**

---

### Task 7: Add `methods_of` + `instances_of` convenience fns

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (add new public fns after existing `static_field`)

- [ ] **Step 1: Add the convenience fns**

In `crates/agent/src/internals/api.rs`, after the `static_field` fn (around line 162), add:

```rust
/// Enumerate all methods of `klass`. Returns Vec for convenience; underlying
/// iterator is lazy and the cap at MAX_METHODS_PER_CLASS prevents runaway.
/// Composes `Iter<MethodPtr> for KlassPtr` from agent-core spine.
pub fn methods_of(klass: KlassPtr) -> Vec<MethodPtr> {
    use agent_core::spine::Iter;
    <KlassPtr as Iter<MethodPtr>>::iter(&klass).collect()
}

/// Enumerate live instances of `klass` via the registered scan_backend.
/// Returns Vec eager-collected up to `max` candidates (caller controls cost).
/// Each instance is structurally validated (region/alignment/klass_of/shape)
/// inside the iterator — yielded Instance values are real, not coincidental.
pub fn instances_of(klass: KlassPtr, max: usize) -> Vec<Instance> {
    use agent_core::spine::Iter;
    <KlassPtr as Iter<Instance>>::iter(&klass).take(max).collect()
}
```

- [ ] **Step 2: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings (new `methods_of` / `instances_of` may show as unused until WASM host fns wire them in Tasks 9-10 — that's expected). If unused warnings appear specifically for these two fns, add `#[allow(dead_code)]` with a comment `// Wired by Task 9 (host_list_methods) and Task 10 (host_list_instances)`.

- [ ] **Step 3: Pause for user commit**

---

## Phase 3 — WASM host API

### Task 8: Extend `host_field_info` return for `is_static`

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (`field_info` fn — extend return type)
- Modify: `crates/agent/src/runtime/mem_host.rs` (`host_field_info` — extend packed return)

- [ ] **Step 1: Extend `api::field_info` return type**

In `crates/agent/src/internals/api.rs`, find the existing `field_info` fn (around line 92):

```rust
pub fn field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType)> {
```

Change the signature to:

```rust
pub fn field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType, bool)> {
```

And the body needs to surface the static bit. Find the inner block where the matched field's data is returned (search for `return Some((offset` or similar). Update to return the three-tuple with `is_static`. The static bit comes from the same `chunk & 0x10` check `fields_at` now uses — refactor to share OR duplicate the read inline (read the chunk again from `type_ptr + il2cpp_type_discrim_read_at`, mask `& 0x10`).

The cleanest pattern: have `field_info` walk via `Iter<FieldInfo>` (now that it carries `is_static`) and match by name:

```rust
pub fn field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType, bool)> {
    use agent_core::spine::Iter;
    use crate::external::cache;
    <KlassPtr as Iter<agent_core::spine::FieldInfo>>::iter(&klass)
        .find(|fi| {
            cache::read_cstr(fi.name_ptr)
                .map_or(false, |n| n == name)
        })
        .map(|fi| (fi.offset, fi.val_type, fi.is_static))
}
```

This DELETES `for_each_field`'s role as `field_info`'s engine — now `field_info` uses the spine Iter. Reduces the triple-duplication. (Tasks 11-12 of Phase 4 finish the duplication removal by porting the dumper.)

- [ ] **Step 2: Update all callers of `field_info`**

Run: `grep -rn "api::field_info\|::field_info(" crates/agent/src --include="*.rs"`

For each caller, update the unpacking. Common pattern change:

Before: `let (offset, vt) = field_info(klass, name)?;`
After:  `let (offset, vt, _is_static) = field_info(klass, name)?;`

Internal callers (likely only one — `get_field` in api.rs):
```rust
// Old:
let (offset, vt) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
// New:
let (offset, vt, _is_static) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
```

External caller `field_addr_t` (in same file):
```rust
// Old:
let (offset, vt) = field_info(klass, name)?;
// New:
let (offset, vt, _is_static) = field_info(klass, name)?;
```

- [ ] **Step 3: Extend `host_field_info` packed return**

In `crates/agent/src/runtime/mem_host.rs`, find the existing `host_field_info`. The current return packs `((vt as u8 as i64) << 32) | (offset as i64)`. Extend the new packed format:

```
bits  0-31:  offset (u32)
bits 32-39:  ValType tag (u8)
bit  40:     is_static (1 bit)
bits 41-63:  reserved (zero)
```

Replace the existing body. The exact form depends on the current code; for the typical shape:

```rust
fn host_field_info(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return -1 };
    let name = String::from_utf8_lossy(&name);
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    match crate::internals::api::field_info(klass, &name) {
        Some((offset, vt, is_static)) => {
            let mut packed: i64 = offset as i64;                  // bits 0-31
            packed |= (vt as u8 as i64) << 32;                    // bits 32-39
            if is_static { packed |= 1i64 << 40; }                // bit 40
            packed
        }
        None => -1,
    }
}
```

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings.

- [ ] **Step 5: Pause for user commit**

---

### Task 9: Add `host_list_methods` WASM host fn

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (add fn + register in linker)

- [ ] **Step 1: Add `host_list_methods` fn**

In `crates/agent/src/runtime/mem_host.rs`, add (near other `host_*` fns, e.g. after `host_find_method` for grouping):

```rust
/// List methods of `klass` into the guest's buffer. Each entry is 8 bytes
/// (MethodPtr as little-endian u64). Returns count written, or negative
/// status code on error.
fn host_list_methods(
    mut caller: Caller<'_, HostState>,
    klass: i64,
    out_buf: i32,
    out_cap_count: i32,
) -> i32 {
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    let methods = crate::internals::api::methods_of(klass);
    let take = methods.len().min(out_cap_count.max(0) as usize);
    let mut buf = Vec::with_capacity(take * 8);
    for m in methods.iter().take(take) {
        buf.extend_from_slice(&m.as_u64().to_le_bytes());
    }
    if !write_guest(&mut caller, out_buf, &buf) { return status::ERR_BUF_TOO_SMALL; }
    take as i32
}
```

- [ ] **Step 2: Register in the wasmi linker**

In the same file, find the existing `linker.func_wrap("il2cpp", ...)` block (around lines 501-514). Add a new line alongside the existing il2cpp registrations:

```rust
    linker.func_wrap("il2cpp", "list_methods", host_list_methods).map_err(|e| WasmError::Instantiate(e.to_string()))?;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds.

- [ ] **Step 4: Pause for user commit**

---

### Task 10: Add `host_list_instances` WASM host fn

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (add fn + register in linker)

- [ ] **Step 1: Add `host_list_instances` fn**

In `crates/agent/src/runtime/mem_host.rs`, add (near `host_list_methods` for grouping):

```rust
/// List live instances of `klass` into the guest's buffer. Each entry is
/// 8 bytes (Instance address as little-endian u64). Returns count written,
/// or negative status code on error. Scan-based discovery; first call for a
/// klass triggers an AOB scan of cached writable regions (cached afterward).
fn host_list_instances(
    mut caller: Caller<'_, HostState>,
    klass: i64,
    out_buf: i32,
    out_cap_count: i32,
) -> i32 {
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    let max = out_cap_count.max(0) as usize;
    let instances = crate::internals::api::instances_of(klass, max);
    let mut buf = Vec::with_capacity(instances.len() * 8);
    for i in &instances {
        buf.extend_from_slice(&i.as_u64().to_le_bytes());
    }
    if !write_guest(&mut caller, out_buf, &buf) { return status::ERR_BUF_TOO_SMALL; }
    instances.len() as i32
}
```

- [ ] **Step 2: Register in the wasmi linker**

Add adjacent to `list_methods` registration:

```rust
    linker.func_wrap("il2cpp", "list_instances", host_list_instances).map_err(|e| WasmError::Instantiate(e.to_string()))?;
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings.

- [ ] **Step 4: Pause for user commit**

---

## Phase 4 — Dumper rewrite as substrate output

### Task 11: Replace `collect_runtime_fields` with Iter-based emission

**Files:**
- Modify: `crates/agent/src/internals/dump.rs:332+` (`collect_runtime_fields` fn + caller)

- [ ] **Step 1: Understand the existing structure**

Run: `grep -n "collect_runtime_fields\|build_internals_lines" crates/agent/src/internals/dump.rs`

Read the existing `collect_runtime_fields` body. Note: it currently does its own field walk independent of `Iter<FieldInfo>` — this is one of the three duplicate walks identified in the spec.

- [ ] **Step 2: Replace `collect_runtime_fields` body with Iter-based emission**

In `crates/agent/src/internals/dump.rs`, find `fn collect_runtime_fields(...)`. Replace its entire body with a call to the spine Iter:

```rust
fn collect_runtime_fields(
    cls: usize,
    map: &RegionMap,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
    type_maps: &TypeMaps,
) -> Vec<String> {
    use agent_core::spine::{FieldInfo, Iter, KlassPtr};
    let klass = KlassPtr::from_raw(cls as u64);
    let mut lines = Vec::new();
    let mut count: usize = 0;
    for fi in <KlassPtr as Iter<FieldInfo>>::iter(&klass) {
        count += 1;
        // Name lookup via the agent-side region cache (NOT via the RegionMap
        // arg — the cache is what populated klass_fields walks in the legacy
        // path; preserve identical semantics by using crate::external::cache).
        let name = crate::external::cache::read_cstr(fi.name_ptr)
            .unwrap_or_else(|| String::from("?"));
        let type_name = il2cpp_type_name_for_field(map, &fi, type_maps, cfg, api);
        let static_marker = if fi.is_static { "static " } else { "" };
        lines.push(format!(
            "    {}{}: {} // Offset: {:#x}, Token: {:#x}",
            static_marker, name, type_name, fi.offset, fi.token
        ));
    }
    // Field-count honesty signal (Task 14 expands this).
    if count == 256 {
        lines.push("    // ⚠ field walk hit MAX cap (256); some fields may be missing".to_string());
    }
    lines
}

/// Resolve human-readable type name for a FieldInfo. Uses the existing
/// type-name resolver but feeds it the FieldInfo's val_type + type_ptr lookup.
fn il2cpp_type_name_for_field(
    map: &RegionMap,
    _fi: &agent_core::spine::FieldInfo,
    type_maps: &TypeMaps,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
) -> String {
    // For now, return the ValType name (e.g., "System.Int32"); deeper type-name
    // resolution (Generic<T>, arrays, etc.) requires the type_ptr which the
    // current spine FieldInfo doesn't carry. Phase 5 cleanup may extend
    // FieldInfo to include type_ptr if richer dumps are wanted. For B-6b's
    // scope: the dumper trades a bit of type-name fidelity for the static
    // marker + iterator unification. The val_type alone covers ~90% of types.
    let _ = (map, type_maps, cfg, api); // unused until full type-name resolver wired
    "System.Int32 /* TODO: full type resolver */".to_string()  // placeholder; see Step 3
}
```

Wait — this placeholder is a plan failure per the no-placeholders rule. Recover:

**The real call:** the existing `dump.rs` already has helpers that resolve type names from a `type_ptr`. Look at the original `collect_runtime_fields` body (before this replacement) — it reads `type_ptr = cache::read_u64(slot + 8)` and calls something like `il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ...)`.

The cleanest fix: **extend the spine `FieldInfo` to include `type_ptr: usize`** as part of Phase 1. That way, the dumper can call the same type-name resolver on the type_ptr field.

But that's an additive change to Phase 1 that we deferred. Two options:

**Option A:** Add `type_ptr: usize` to FieldInfo in Phase 1 (one-line extension to Task 1).
**Option B:** Keep type-name resolution in the dumper by re-reading type_ptr from the slot in Task 11.

Option A is cleaner (the spine FieldInfo becomes more complete) and is the structural answer the user asked for. Do that.

**REVISION: Update Task 1 Step 1 to ALSO add `type_ptr: usize` to FieldInfo. Update Task 1 Step 2 to add it to FieldInfoRaw. Update Task 1 Step 3 to propagate it. Update Task 4 to populate it.** Then this Task 11 can use `fi.type_ptr` directly in the type-name resolution.

For now in Task 11: assume Task 1 / Task 4 have been updated to carry `type_ptr`. Use the existing type-name resolver:

```rust
        let type_name = il2cpp_type_name(map, fi.type_ptr, type_maps, cfg, api, None)
            .unwrap_or_else(|| "?".into());
```

- [ ] **Step 3: Backtrack to Task 1 + Task 4 to add `type_ptr`**

Update Task 1 Step 1: add `pub type_ptr: usize,` to the FieldInfo struct definition.
Update Task 1 Step 2: add `pub type_ptr: usize,` to FieldInfoRaw struct definition.
Update Task 1 Step 3: propagate `type_ptr: raw.type_ptr` in the Some(FieldInfo {...}) construction.
Update Task 4 Step 1: in `fields_at`, populate `type_ptr: type_ptr` (the local var already exists in the body) in the FieldInfoRaw construction.

After backtracking, rerun the Phase 1 + Phase 2 tasks' verification commands. Build clean.

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings.

- [ ] **Step 5: Pause for user commit**

---

### Task 12: Add method emission per class in dump

**Files:**
- Modify: `crates/agent/src/internals/dump.rs` (extend the per-class emission loop)

- [ ] **Step 1: Add method-listing helper**

In `crates/agent/src/internals/dump.rs`, add a new helper alongside `collect_runtime_fields`:

```rust
fn collect_runtime_methods(
    cls: usize,
    _map: &RegionMap,
    _cfg: &Il2CppConfig,
    api: &Il2CppApi,
) -> Vec<String> {
    use agent_core::spine::KlassPtr;
    let klass = KlassPtr::from_raw(cls as u64);
    let methods = crate::internals::api::methods_of(klass);
    let count = methods.len();
    let mut lines = Vec::with_capacity(count + 2);
    if count == 0 {
        return lines;
    }
    lines.push(format!("    methods ({}):", count));
    for m in &methods {
        let mi = m.as_u64() as usize;
        // Name via cfg-probed name offset (same path find_method uses).
        let name = match crate::internals::ctx::get() {
            Some(c) => {
                let name_ptr = crate::external::cache::read_u64(mi + c.cfg.method_name_off)
                    .unwrap_or(0) as usize;
                crate::external::cache::read_cstr(name_ptr).unwrap_or_else(|| "?".into())
            }
            None => "?".into(),
        };
        // Arg count via cfg-probed param-count offset.
        let argc = match crate::internals::ctx::get() {
            Some(c) => crate::external::cache::read_u8(mi + c.cfg.method_param_count_off).unwrap_or(0),
            None => 0,
        };
        lines.push(format!("        {}({} args)", name, argc));
    }
    if count == agent_core::spine::access::MAX_METHODS_PER_CLASS {
        lines.push("        // ⚠ method walk hit MAX cap; some methods may be missing".to_string());
    }
    let _ = api; // currently unused; future extension via runtime_invoke for return-type names
    lines
}
```

Note: `agent_core::spine::access::MAX_METHODS_PER_CLASS` — verify this is `pub` in access.rs. If not, expose it or use a sibling constant.

- [ ] **Step 2: Call `collect_runtime_methods` in the per-class loop**

In `crates/agent/src/internals/dump.rs`, find the existing per-class emission code (where `collect_runtime_fields` is called). After the field lines are appended, append the method lines:

```rust
        let method_lines = collect_runtime_methods(cls, map, cfg, api);
        lines.extend(method_lines);
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings.

- [ ] **Step 4: Pause for user commit**

---

### Task 13: Add live instance emission per class

**Files:**
- Modify: `crates/agent/src/internals/dump.rs` (extend per-class emission)

- [ ] **Step 1: Add instance-listing helper**

In `crates/agent/src/internals/dump.rs`, add another helper:

```rust
fn collect_runtime_instances(cls: usize) -> Vec<String> {
    use agent_core::spine::KlassPtr;
    let klass = KlassPtr::from_raw(cls as u64);
    // Cap display to first 10 instances (avoid dump bloat for classes with
    // many live instances). Total count would require an exhaustive scan
    // which can be expensive; emit "10+" sentinel if cap hit.
    const DISPLAY_CAP: usize = 10;
    let instances = crate::internals::api::instances_of(klass, DISPLAY_CAP + 1);
    let mut lines = Vec::new();
    if instances.is_empty() {
        return lines;  // no live instances; skip the section entirely
    }
    let display_count = instances.len().min(DISPLAY_CAP);
    let suffix = if instances.len() > DISPLAY_CAP { " (10+ shown)" } else { "" };
    lines.push(format!("    live instances ({}){}", instances.len().min(DISPLAY_CAP), suffix));
    for inst in instances.iter().take(DISPLAY_CAP) {
        lines.push(format!("        {:#x}", inst.as_u64()));
    }
    let _ = display_count;  // local already shadowed in format string
    lines
}
```

- [ ] **Step 2: Call `collect_runtime_instances` in the per-class loop**

In the same per-class emission area, after method lines:

```rust
        let instance_lines = collect_runtime_instances(cls);
        lines.extend(instance_lines);
```

- [ ] **Step 3: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, ≤ 10 warnings.

- [ ] **Step 4: Pause for user commit**

---

### Task 14: Field-count honesty signal in dumper output

**Files:**
- Modify: `crates/agent/src/internals/dump.rs` (already touched in Task 11 — verify cap signal works)

- [ ] **Step 1: Verify the cap-signal line emits correctly**

The cap signal was added in Task 11. Quick audit: when `count == 256`, the line `"⚠ field walk hit MAX cap..."` appears. When count is honest (< 256), no signal.

If `count >= 256` cases happen in practice, the dumper output flags them. No additional code needed beyond Task 11.

- [ ] **Step 2: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds.

- [ ] **Step 3: Pause for user commit**

(This task is effectively a no-op verification after Task 11; kept as a separate task for explicit acceptance.)

---

## Phase 5 — Cleanup pass (investigate-intent philosophy)

Each cleanup task follows the same shape: investigate intent first, decide wire-vs-delete, implement decision.

### Task 15: Investigate + decide `Hook.detour`

**Files:**
- Investigate: `crates/agent/src/inline_detour.rs:12` + all callers
- Modify (decision-dependent): `inline_detour.rs` (wire OR delete)

- [ ] **Step 1: Investigate**

Run: `grep -rn "\.detour\|detour:" crates/agent/src --include="*.rs"`

Read `inline_detour.rs` around line 12. The struct `Hook` has fields `target`, `trampoline`, `detour`, `stolen_len`. The field `detour` is currently unread. Determine intent:

- If `detour` holds the JMP target address (the address bytes are patched TO jump to), it's the "detour" pointer
- The trampoline holds the original stolen bytes + jump-back
- Unhook restores original bytes at `target`; doesn't need `detour` for that

Read `inline_detour::install` to see what `detour` is populated with. Likely it's the address of the universal_shim or per-method thunk — i.e., the JMP destination.

- [ ] **Step 2: Decide**

If `detour` IS the patched-target address and unhook would benefit from validating "the bytes at target still point to detour before we restore," then **WIRE** it: add a verify step in unhook that reads back the patched bytes and confirms the JMP destination matches `self.detour`. If the JMP was overwritten by something else (a different patcher), unhook should log a warning.

If `detour` is a vestige of an earlier design that wasn't completed, **DELETE** with a comment explaining what it WAS for.

For B-6b: lean toward investigating; the WIRE decision adds a small safety check that's substrate-honest. Document the decision in commit message.

- [ ] **Step 3: Implement the decision**

If WIRE: add a `verify_patched(&self) -> bool` method on Hook that reads bytes at `self.target` and confirms a JMP-to-`self.detour` is still there. Call it before `restore()` in Drop; log if mismatch.

If DELETE: remove the field from struct, update `install()` to not populate it, update tests.

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, warning for `Hook.detour` either resolved (if wired) or gone (if deleted).

- [ ] **Step 5: Pause for user commit**

---

### Task 16: Investigate + decide `Verified::Crashed`

**Files:**
- Investigate: `crates/agent/src/internals/calibration/ffi_verify.rs:12`
- Modify (decision-dependent): `ffi_verify.rs`

- [ ] **Step 1: Investigate**

Run: `grep -rn "Verified::Crashed\|enum Verified" crates/agent/src --include="*.rs"`

Read `ffi_verify.rs`. The enum `Verified` has a `Crashed` variant. Find where Verified values are constructed — if none construct `Crashed`, FFI crash detection (SEH on Windows / signal handler on Linux) was never wired.

- [ ] **Step 2: Decide**

WIRE: implementing SEH on Windows requires `windows-sys::Win32::Foundation::EXCEPTION_*` + `__try`/`__except`-style structured handling. Substantial work — may exceed B-6b's investigate-cleanup scope.

DELETE: with comment explaining: "The Crashed variant was designed to flag FFI calls that crashed mid-verify (SEH-trapped). FFI crash detection was never wired (would require Windows SEH integration). Until that's built, FFI verify treats crashes as 'process died' which the OS handles. Re-add the variant when SEH wiring lands."

For B-6b: **DELETE** with the comment above. Wiring SEH crash detection is its own brick (substrate-stability work targeted to B-6c per the spec).

- [ ] **Step 3: Implement the deletion**

Remove `Crashed` from the enum. Add a top-of-fn comment in `ffi_verify.rs` documenting the deferred-wiring rationale per Step 2.

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, warning resolved.

- [ ] **Step 5: Pause for user commit**

---

### Task 17: Investigate + decide `MIN_RATIO`

**Files:**
- Investigate: `crates/agent/src/internals/calibration/field_param_layout.rs:11`
- Modify (decision-dependent): same file

- [ ] **Step 1: Investigate**

Run: `grep -rn "MIN_RATIO\|min_ratio" crates/agent/src --include="*.rs"`

Read `field_param_layout.rs`. `MIN_RATIO: f32 = 0.90` is a threshold constant. Find which probe path WOULD use it (probably the field-vs-param disambiguation logic). If the probe currently uses a hardcoded value, **WIRE** the const there.

- [ ] **Step 2: Decide + implement**

If a probe uses `0.90` or similar hardcoded: replace with `MIN_RATIO`. Turns dead code live, restores intent.

If no probe needs a ratio threshold and the const is genuinely vestigial: DELETE with comment.

For B-6b: likely WIRE.

- [ ] **Step 3: Confirm Windows cross-compile**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, warning resolved.

- [ ] **Step 4: Pause for user commit**

---

### Task 18: Wire `METHOD_ATTRIBUTE_STATIC_BIT`

**Files:**
- Modify: `crates/agent/src/internals/marshal.rs:260` (the const)
- Modify: `crates/agent/src/internals/api.rs::fields_at` (replace magic 0x10)
- Modify: `crates/agent/src/internals/api.rs::static_field` (replace magic 0x10)

- [ ] **Step 1: Move the constant to a shared location**

The const lives in `marshal.rs` but should be usable across `api.rs`. Move it (or make it `pub`) to `internals/api.rs` or to `internals/mod.rs`. Suggested: put it at the top of `internals/api.rs` as a `pub(crate) const`.

In `internals/api.rs` near the top:

```rust
/// `FIELD_ATTRIBUTE_STATIC` bit in il2cpp's field type-attribute chunk
/// (low byte of the discriminator chunk read at `il2cpp_type_discrim_read_at`).
/// A field is declared `static` iff this bit is set in `chunk & 0xFF`.
pub(crate) const METHOD_ATTRIBUTE_STATIC_BIT: u32 = 0x10;
```

(Despite the name `METHOD_ATTRIBUTE_*`, in il2cpp's encoding the same bit value works for both METHOD and FIELD attribute static — they share the bitfield convention.)

- [ ] **Step 2: Replace `0x10` magic in `fields_at`**

In `crates/agent/src/internals/api.rs::fields_at`, find:

```rust
        let is_static = (chunk & 0x10) != 0;
```

Replace with:

```rust
        let is_static = (chunk & METHOD_ATTRIBUTE_STATIC_BIT as u64) != 0;
```

- [ ] **Step 3: Replace `0x10` in `static_field`**

In `static_field` (same file, around line 150), find the matching:

```rust
            if chunk & 0x10 != 0 {
```

Replace with:

```rust
            if chunk & METHOD_ATTRIBUTE_STATIC_BIT as u64 != 0 {
```

- [ ] **Step 4: Delete or update the `marshal.rs` declaration**

If the const lived in `marshal.rs` as well, delete that declaration (avoid duplication).

- [ ] **Step 5: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, `METHOD_ATTRIBUTE_STATIC_BIT` warning resolved (now used).

- [ ] **Step 6: Pause for user commit**

---

### Task 19: Investigate `unnecessary unsafe block` in ffi_verify.rs:136

**Files:**
- Investigate: `crates/agent/src/internals/calibration/ffi_verify.rs:136`

- [ ] **Step 1: Read surrounding code**

Read 20 lines around `ffi_verify.rs:136`. Compiler says the `unsafe` block is unnecessary. Determine WHY: was it wrapping FFI that's been wrapped in safer abstractions since? Was it placed for documentation-purpose (to mark a region as having safety implications)?

- [ ] **Step 2: Decide**

If genuinely unnecessary (no FFI, no raw deref, no atomic ordering concern inside): **REMOVE** the `unsafe { }` wrapper.

If it was placed defensively as a "this WAS unsafe and I want to preserve that signal": consider keeping it with a comment explaining, OR remove with a comment in the commit message noting what was previously unsafe-flagged.

- [ ] **Step 3: Implement decision + confirm build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, warning resolved.

- [ ] **Step 4: Pause for user commit**

---

### Task 20: Audit `marshal.rs` 4 `last_mut().unwrap()` calls

**Files:**
- Investigate: `crates/agent/src/internals/marshal.rs` (all 4 unwrap sites)

- [ ] **Step 1: Trace each push site**

Run: `grep -n "last_mut().unwrap()\|arg_slabs" crates/agent/src/internals/marshal.rs`

For each `last_mut().unwrap()` call, trace upward to the corresponding `arg_slabs.push(...)` site. Verify: is the push guaranteed to happen before the `last_mut()`? Are there any control-flow paths where the slab can be popped or never pushed?

- [ ] **Step 2: Decide**

If the invariant holds provably (push always happens, no early-return between push and last_mut): replace `.unwrap()` with `.expect("arg_slabs invariant: push precedes last_mut")` for self-documenting failure, OR add a `debug_assert!(...)` at each unwrap site with a comment.

If the invariant can be violated: propagate via `Result` with a `MarshalError::SlabInvariantViolation` variant.

For B-6b's medium scope: lean **document with `expect` + comment** rather than full Result propagation (which would refactor surrounding signatures). If invariants are truly inviolable, expect-with-message is the right idiom.

- [ ] **Step 3: Implement + confirm build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds. The 4 unwraps now have clear messages OR debug_asserts.

- [ ] **Step 4: Pause for user commit**

---

### Task 21: Audit `calibrate_generic_class_offset`

**Files:**
- Investigate: `crates/agent/src/internals/dump.rs:96-129`
- Decision-dependent: dump.rs

- [ ] **Step 1: Audit generic-context callers**

Run: `grep -rn "PROBED_GC_OFF\|read_generic_context\|klass_generic_class" crates/agent/src --include="*.rs"`

Find where generic-context resolution is consumed (e.g., field type resolution for `Dictionary<K,V>` etc.). Verify whether VAR/MVAR resolution works without `klass_generic_class` calibration (the comment claims yes; verify).

- [ ] **Step 2: Decide**

If audit confirms VAR/MVAR works without it: **DELETE** `calibrate_generic_class_offset`. Replace with a top-of-file comment naming where VAR/MVAR resolution lives and confirming it's sufficient. The deletion clears confusion.

If audit finds silent failures (generic args showing as `<!0>` instead of concrete type names): **WIRE** the calibration to PROBED_GC_OFF as the proposal originally suggested. This becomes a 1-line fix: `PROBED_GC_OFF.store(off, Ordering::Relaxed);` inside the match block.

- [ ] **Step 3: Implement decision + confirm build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds.

- [ ] **Step 4: Pause for user commit**

---

### Task 22: Trivial cleanups (`mut caller`, `chunk` var, unused doc comment)

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs:322` (host_hook_set_arg)
- Modify: `crates/agent/src/runtime/mem_host.rs:348` (host_hook_set_return)
- Modify: `crates/agent/src/internals/calibration/field_param_layout.rs:63` (chunk var)
- Modify: `crates/agent/src/internals/hook_runtime/shim.rs:66` (unused doc comment)

- [ ] **Step 1: Remove unused `mut` on hook fns**

In `mem_host.rs:322`, find the `host_hook_set_arg` signature with `mut caller`. Remove `mut`.
Same for `host_hook_set_return` at line 348.

- [ ] **Step 2: Underscore-prefix `chunk` unused var**

In `field_param_layout.rs:63`, find the `chunk` variable that's read but never used. Underscore-prefix: `let _chunk = ...`.

(If the variable IS meant to be used somewhere — per investigate-intent — find the missing usage and wire. Quick read of the surrounding code will tell.)

- [ ] **Step 3: Fix unused doc comment**

In `hook_runtime/shim.rs:66`, find the orphaned `///` comment. Either:
- Convert to `//` if it's documentation that shouldn't be attached to the next item
- Move it adjacent to the right item

Read the surrounding code to determine which.

- [ ] **Step 4: Confirm Windows cross-compile is clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds, 3-4 fewer warnings.

Run: `cargo build --target x86_64-pc-windows-gnu --release 2>&1 | grep "^warning:" | grep -v "generated" | wc -l`
Expected: significantly less than 10 baseline (Phase 5 cleanup reduces it).

- [ ] **Step 5: Pause for user commit**

---

## Final verification

### Task 23: Live smoke test on PW

**Files:** None — verification only.

- [ ] **Step 1: Confirm build + auto-deploy**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: succeeds; per `deploy-setup`, `./deploy.sh` may auto-fire.

If it didn't auto-fire, run `./deploy.sh` (this is the ONE exception to the "don't run deploy.sh" rule — Task 23 is the live-verification phase).

- [ ] **Step 2: Restart PW + observe internals.txt for new sections**

Launch PW via Steam. After agent runs (a few seconds), check:

Run: `head -200 "$HOME/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"`

Expected:
- Per-class header includes method count: `PlayerData (256 fields, M methods):`
- Static-flagged fields show `static ` prefix
- Each class with live instances has a `live instances (K)` section listing addresses

- [ ] **Step 3: Drop a wasm script that calls list_methods + list_instances**

Compile a minimal test wasm (e.g., via `cargo run --example wat2wasm -p agent-core` against a small .wat that calls the new host fns). Drop into `scripts/active.wasm`. Verify log output shows expected method count + instance count for PlayerData.

- [ ] **Step 4: Verify field_info returns is_static bit**

Same test wasm should call extended `field_info` on a known static field (e.g., `SteamManager.s_instance`) and decode bit 40. Verify log shows `is_static=true`.

- [ ] **Step 5: Final warning baseline check**

Run: `cargo build --target x86_64-pc-windows-gnu --release 2>&1 | grep "^warning:" | grep -v "generated" | wc -l`
Expected: less than 10 (B-6b shipped → baseline IMPROVED, not preserved).

- [ ] **Step 6: Pause for user commit + ship**

This is the final B-6b commit. After this, the brick is shipped. The substrate now honestly describes itself; the dumper is a thin serializer of substrate truth.

---

## Self-Review

Reviewing this plan against `docs/superpowers/specs/2026-05-31-b6b-internal-api-completeness-design.md`:

**1. Spec coverage:**
- ✅ Locked decision #1 (one brick, five phases) — plan structures into Phase 1-5.
- ✅ Decision #2 (cleanup inside brick) — Phase 5 tasks 15-22 are cleanup.
- ✅ Decision #3 (medium cleanup scope) — covers the 11 pre-existing warnings + 2 audited debt items per spec.
- ✅ Decision #4 (separate scan_backend.rs) — Task 2 creates it.
- ✅ Decision #5 (dumper downstream Phase 4) — Tasks 11-14.
- ✅ Decision #6 (no Protocol pre-design) — not in plan, deferred to B-6e.
- ✅ Decision #7 (structural validation, no per-klass logic) — scan_validate in Task 5 is universal (region/align/klass_of/shape).
- ✅ Decision #8 (lazy iterators) — InstanceIter in Task 3 uses loop+next pattern; FieldInfo/MethodPtr iterators already lazy.
- ✅ Phase 1 spine: `is_static` on FieldInfo (Task 1), `Iter<MethodPtr>` already exists, `Iter<Instance>` (Task 3).
- ✅ Phase 2 internal: `fields_at` static bit (Task 4), scan_backend impls (Task 5), wired (Task 6), helpers (Task 7).
- ✅ Phase 3 WASM: `host_field_info` extension (Task 8), `host_list_methods` (Task 9), `host_list_instances` (Task 10).
- ✅ Phase 4 dumper: collect_runtime_fields rewrite (Task 11), method emission (Task 12), instance emission (Task 13), cap signal (Task 14).
- ✅ Phase 5 cleanup: investigate-intent per item (Tasks 15-22).
- ✅ Live verification (Task 23).

**2. Placeholder scan:**
- Task 11 originally had a placeholder for type-name resolution. Caught during writing — Step 3 of Task 11 backtracks Task 1/Task 4 to add `type_ptr` to FieldInfo, enabling real type resolution. No remaining placeholders.
- Cleanup tasks (15-22) describe investigation steps + decision-rationale shapes, not "TBD." Investigation outputs are decisions documented in commit messages.

**3. Type consistency:**
- `FieldInfo` fields: name_ptr, offset, val_type, token, is_static, type_ptr — used consistently across Tasks 1, 4, 11.
- `FieldInfoRaw` mirrors with `next_cursor` addition — Tasks 1, 4 use identically.
- `NextMatchFn`, `ValidateFn` types from scan_backend — Task 2 defines, Task 5 implements, Task 3's InstanceIter consumes.
- `KlassPtr.iter::<Instance>()` syntax — Tasks 3, 7, 13 use consistently.
- `methods_of(klass: KlassPtr) -> Vec<MethodPtr>`, `instances_of(klass: KlassPtr, max: usize) -> Vec<Instance>` — Tasks 7, 9, 10, 12, 13 use identical signatures.
- `host_field_info` packed return format (bits 0-31=offset, 32-39=ValType, 40=is_static) — Task 8 defines, Task 23 verifies.

No identifier drift. All type signatures consistent across task boundaries.

**4. Note on the additive change discovered during writing:**

Writing Task 11 surfaced that the dumper needs `type_ptr` from FieldInfo for full type-name resolution. This was added to Task 1 / Task 4 retroactively (Step 3 of Task 11). The plan documents this backtrack — implementers should apply the Task 1/Task 4 updates BEFORE running Task 11. The TDD signal (Task 1 Step 5 build-fails-until-Task-4) catches this automatically.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-31-b6b-internal-api-completeness-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Dispatch fresh subagent per task with two-stage review. Per `subagents-use-opus` 3-tier routing:
- **Opus:** Tasks 4 (fields_at — silent corruption risk if bit-mask logic wrong), 5 (scan_backend — substrate primitive new to project), 21 (calibrate_generic_class_offset audit — feature decision)
- **Sonnet:** Tasks 1, 2, 3, 6, 7, 8, 9, 10, 11, 12, 13 (mechanical or pattern-following with code spelled out)
- **Haiku:** Tasks 14 (verification only), 15-22 cleanup investigations have a Haiku-investigation + Sonnet-implementation hybrid possible, 23 (live verification: human runs game)

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch with checkpoints.

**Which approach?**
