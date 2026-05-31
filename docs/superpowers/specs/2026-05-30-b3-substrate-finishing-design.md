# B-3: Substrate Finishing — Design

**Date:** 2026-05-30
**Branch:** `ffi-class-table` (or successor)
**Status:** approved (post spine-API correction), ready for plan-writing
**Builds on:** B-1 (Probe-and-Verify), B-2a (Honest Dumper), B-2bc (Concrete Bugs + Honesty) — all shipped + game-verified

---

## Goal

Finish the substrate before capability work resumes. Two deliverables:

1. **Hook H12** — wire wasmi handler dispatch so hooks can mutate game behavior, not just observe. Currently `call_hook_handler` is a `Ok(())` stub; H12 makes it real with safe cross-thread Store access, per-thread CURRENT-stack for nested-hook safety, and never-blocking discipline.
2. **Read+write API readiness audit** — a planning document that maps the gaps between today's APIs and what In-flight Modify (priority #3) will need. No code; just a one-page report that makes #3's brainstorm faster.

## The bedrock principle, applied to handler dispatch

> **The game thread MUST NEVER block on the agent. Handler dispatch acquires the Store via `try_lock` with transparent-observer fall-through on contention. Same-thread reentrancy (frog_main invoking its own hooks) is handled architecturally by never holding the Store reference during dispatch, not by reentrant locking (which would violate Rust's aliasing rules).**

## Non-goals (deferred)

| Item | Deferred to |
|---|---|
| Spine `_t` typed-sibling sweep | NOT happening — these are load-bearing composition contract per [[spec2-domain-audit-and-cleanup]] memory; "zero callers ≠ dead code" lesson |
| Per-marker `#[allow(dead_code)]` investigation | Own focused micro-brick — needs per-item verification, not a bulk sweep |
| Two-phase test fixture for handler-mutation testing (e.g. `FROG_INVOKE_AFTER`) | Future micro-brick — test_hook.wasm's "hooked Pow returned UNEXPECTED" stays as-is; transparent-observer semantics during frog_main are correct |
| In-flight Modify implementation | Priority #3 — uses the audit deliverable from B-3 Section 4 as input |
| `aob_scan` cache coherence, anti-cheat re-check | Banked from B-2bc; no callers / policy decision |
| B-2d name de-obfuscation | Banked far-future |

---

## Section 1 — H12 Architecture

### The change to `run_wasm_with_mem`

Currently the Store is dropped when `frog_main` returns. H12 transfers ownership to a global Mutex so the Store outlives frog_main and is available for hook callbacks.

```rust
// crates/agent/src/runtime/host.rs (new types)

pub(crate) struct ParkedStore {
    pub store: wasmi::Store<HostState>,
    pub instance: wasmi::Instance,
    pub funcref_table: wasmi::Table,
}

static PARKED: std::sync::OnceLock<std::sync::Mutex<Option<ParkedStore>>> = std::sync::OnceLock::new();

pub(crate) fn parked() -> &'static std::sync::Mutex<Option<ParkedStore>> {
    PARKED.get_or_init(|| std::sync::Mutex::new(None))
}
```

### Lifecycle

1. `run_wasm_with_mem` instantiates Store + module + linker, calls `frog_main()` synchronously. **During this entire call, PARKED stays `None`** — handlers cannot fire because there's no parked Store to dispatch them.
2. When `frog_main` returns, **clone the logs** (since we need to return them), then **move Store ownership** into the PARKED Mutex.
3. `call_hook_handler(funcref_idx)` (called from `dispatch_rust` on the game thread):
   - `try_lock` PARKED with a one-shot contention log on `Err`
   - If acquired AND `Some(parked)` inside: resolve funcref_idx → Func → typed call. Release lock.
   - If contended OR still `None`: return Err → dispatcher's existing transparent-observer fallback runs the trampoline. Game observes the original return value.
4. On agent shutdown: OnceLock drops, Store drops, everything cleans up.

### Why this is safe

**Rust's aliasing model is the actual constraint**, not the Mutex. Two `&mut Store` simultaneously is UB regardless of locks. The model above guarantees only ONE `&mut Store` exists at any time:

- During `frog_main` execution: `run_wasm_with_mem` holds the unique `&mut Store`. PARKED is `None`. Any same-thread reentrancy (frog_main invoking a hooked method via `invoke`) triggers `dispatch_rust` → `call_hook_handler` → sees `None` → falls back to transparent observer.
- After `frog_main` returns: Store moves into PARKED. `run_wasm_with_mem`'s reference is gone. Any thread (typically game thread) can `try_lock` and obtain the unique `&mut Store` through the guard.

`parking_lot::ReentrantMutex` would let two `MutexGuard` instances co-exist on the same thread; each guard's `DerefMut` returns `&mut T` → two simultaneous `&mut Store` = UB. Type-safe at runtime, behavior-undefined at the language layer. **Do not use reentrant locking.**

### Why production usage works cleanly

Production scenario: modder script installs hooks on `Player.Move`, returns from frog_main, Store parks. Game runs on its own native threads; player walks; `Player.Move` fires on game thread; `dispatch_rust` runs on game thread; `call_hook_handler` acquires PARKED Mutex (different thread from frog_main's exited worker → free → acquires immediately) → handler runs → handler sets return value via `hook_set_return` → flows back through CURRENT.last_mut() → dispatcher packs into RegArgs → game observes mutated value. **No deadlock, no reentrancy, no UB.**

### Same-thread verification limitation

`test_hook.wasm` currently does install_hook + invoke inside frog_main. With H12 + park-after-return, the in-frog_main invoke triggers transparent observer (PARKED still None), returns 8.0. The test's "hooked Pow returned UNEXPECTED" log line predates H12 and stays — handler-mutation verification needs a two-phase test fixture (e.g. `FROG_INVOKE_AFTER` env var that fires invoke from agent worker thread AFTER frog_main returns and Store is parked). **Deferred as documented above.**

## Section 2 — H12 Implementation Surface

### a) CURRENT becomes a per-thread stack (nested-hook safety)

**Today** (api.rs:35-36): `static CURRENT: RefCell<Option<CurrentContext>>`. Single-slot Option. Nested hook dispatches on the same thread overwrite + take, panicking the outer dispatch with `"context vanished"`.

**The bug becomes live with H12.** Scenario: hook Math.Pow + hook Math.Sqrt. Pow's handler calls `call_original` → Pow runs → Pow internally calls Sqrt → Sqrt's hook fires → nested `dispatch_rust(Sqrt)` overwrites CURRENT with ctx_Sqrt → takes it → CURRENT = None → outer Pow dispatch panics on take. Today this is invisible because the stub never runs handlers.

**Fix:** convert CURRENT to a per-thread stack.

```rust
// crates/agent/src/internals/hook_runtime/api.rs

thread_local! {
    static CURRENT: RefCell<Vec<CurrentContext>> = RefCell::new(Vec::new());
}

pub fn with_current_context(
    ctx: &crate::internals::hook_runtime::registry::HookCtx,
    regs: *mut RegArgs,
    args: &[InvokeArg],
    cont: impl FnOnce(HandlerResult),
) {
    crate::paths::log(&format!("with_current_context: ENTRY method={:#x}", ctx.method.as_u64()));

    // PUSH onto per-thread stack (nested dispatch-safe).
    CURRENT.with(|c| {
        c.borrow_mut().push(CurrentContext {
            method:          ctx.method,
            regs,
            args:            args.to_vec(),
            explicit_return: None,
            called_original: false,
        });
    });

    if let Err(e) = crate::runtime::host::call_hook_handler(ctx.handler_func_ref) {
        crate::paths::log(&format!("hook handler call failed: {:?}", e));
    }

    // POP top of stack (the context we pushed above).
    let result = CURRENT.with(|c| {
        let cc = c.borrow_mut().pop().expect("context underflow");
        HandlerResult {
            return_value: cc.explicit_return,
            called_original: cc.called_original,
        }
    });

    crate::paths::log("with_current_context: invoking cont closure");
    cont(result);
}
```

**Host fn consumer callsites:** every `borrow.as_mut()?` becomes `borrow.last_mut()?` (top of stack = currently-active hook context):
- `hook_arg_read`, `hook_arg_write`, `hook_set_return`, `hook_this_get`, `call_original_now` — all flip `as_mut` → `last_mut`. Same Option<&mut> shape, semantically equivalent for non-nested case, correct for nested.

### b) `call_hook_handler` real body

Replaces the H10 stub at `crates/agent/src/runtime/host.rs:47`.

```rust
pub fn call_hook_handler(handler_funcref_idx: u64) -> Result<(), &'static str> {
    let mut guard = match parked().try_lock() {
        Ok(g) => g,
        Err(_) => {
            note_wasm_handler_contended();
            return Err("PARKED contended; transparent observer fires");
        }
    };
    let parked = match guard.as_mut() {
        Some(p) => p,
        None => return Err("PARKED not yet populated (frog_main not yet returned, or no script loaded)"),
    };

    // Resolve funcref → Func.
    let val = parked.funcref_table
        .get(&mut parked.store, handler_funcref_idx as u32)
        .ok_or("funcref index out of range")?;
    let func = match val {
        wasmi::Val::FuncRef(fr) => fr.func().ok_or("funcref is null")?.clone(),
        _ => return Err("table entry is not a funcref"),
    };

    let typed = func.typed::<(), (), _>(&parked.store)
        .map_err(|_| "handler signature is not () -> ()")?;

    typed.call(&mut parked.store, ())
        .map_err(|_| "wasm handler trapped")?;

    Ok(())
}

static WASM_CONTENDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn note_wasm_handler_contended() {
    let prev = WASM_CONTENDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if prev == 0 {
        crate::paths::log("⚠ WASM_HANDLER_CONTENDED — PARKED store held; transparent observer fired");
    } else if (prev + 1) % 1000 == 0 {
        crate::paths::log(&format!("⚠ WASM_HANDLER_CONTENDED count={}", prev + 1));
    }
}
```

The `#[allow(dead_code)]` marker from the H10 stub comes off naturally — the function now has callers (via `with_current_context`).

### c) `run_wasm_with_mem` refactor

In `crates/agent/src/runtime/mem_host.rs`, the current ending:

```rust
Ok(store.into_data().logs)
```

Becomes:

```rust
// Get the funcref table the wasm exported. Name by wasmi convention:
// scripts produced from wat using a `(table funcref ...)` get this name
// unless they override it via (table (export "X") funcref ...).
let funcref_table = instance
    .get_table(&store, "__indirect_function_table")
    .ok_or(WasmError::Instantiate("missing __indirect_function_table export".into()))?;

// Clone logs out before transferring Store ownership (into_data() consumes).
let logs = store.data().logs.clone();

// Park the Store + instance + funcref table for hook dispatch.
*crate::runtime::host::parked().lock().unwrap() = Some(crate::runtime::host::ParkedStore {
    store, instance, funcref_table,
});

Ok(logs)
```

### d) Incidental cleanup

While touching marshal.rs for any reason during H12 (unlikely but possible), delete the unused `METHOD_ATTRIBUTE_STATIC_BIT: u32 = 0x10` const at marshal.rs:260. One-line removal; grep confirms zero callers. Not its own task — folds into whatever H12 task touches that area, or stays banked for a future per-marker `#[allow(dead_code)]` micro-brick.

---

## Section 3 — Read+Write API Readiness Audit

**Deliverable:** `docs/superpowers/audits/2026-05-30-read-write-api-readiness.md`. ~1-2 page markdown report. No code change. Input artifact for the In-flight Modify brainstorm in priority #3.

The audit answers four questions:

1. **What read paths exist today?**
   - WASM-side: `mem.read`, `mem.scan`, `mem.regions`, `il2cpp.find_class`, `il2cpp.field_info`, `il2cpp.get_field`, `il2cpp.klass_of`, `il2cpp.static_field`, `il2cpp.find_method`, `il2cpp.invoke`, `il2cpp.install_hook`, `il2cpp.remove_hook`, `il2cpp.hook_arg`, `il2cpp.hook_this`, `il2cpp.call_original`. All untyped u64/i64 at the WASM boundary.
   - Rust-side typed siblings: `external::api::{read_t, read_bytes_t, read_cstr_t}`, `internals::api::{find_class_t, find_method_t, field_addr_t, static_field_t, klass_of_t, invoke_method_t}`. Capability-discipline via `MemAddr<ReadOnly>`/`MemAddr<ReadWrite>`.

2. **What write paths exist or are missing?**
   - `external::api::{write, write_if, write_t}` exist (T7/T8 spine work + capability gate). `write_t` requires `MemAddr<ReadWrite>` at compile time.
   - WASM-side: `mem.write` + `mem.write_if` host fns ARE registered, gated by `FROG_WASM_WRITE` env var.
   - **Gap for In-flight Modify**: typed `mem.write_t` host fn not yet registered — scripts get untyped byte-level write today; typed scripting needs the host fn surface to mirror the read side.

3. **What field/method-write paths exist?**
   - **None today.** Setting a field's value through il2cpp requires either:
     - `field_set_value` FFI (we resolve `field_get_name`/`field_get_type` via standard exports + sig-scan; the `set` variants are NOT yet in the FFI resolver)
     - OR computing the field address from `klass + field.offset` and using `mem.write` (works for instance fields; harder for static fields because we'd need `runtime_class_init` to ensure the static storage exists)

4. **What's the smallest viable In-flight Modify brick?**
   Sketch (not commitment — priority #3's brainstorm refines):
   - Add `field_set_value` to the FFI resolver (standard exports + sig-scan; pattern matches the existing `field_get_*` work in B-1 Phase 5)
   - Register `il2cpp.set_field` host fn that takes `(klass, field_name, instance, value)` and writes correctly per value-type vs reference-type
   - Add a typed Rust-side sibling: `field_set_t<T>(addr: MemAddr<ReadWrite>, val: T)` consumes the same currency as `field_addr_t`
   - Wire typed `mem.write_t` host fn (using existing `write_t` — small ~20 lines)
   - Verification: a `test_modify.wasm` that reads a Player.position field, writes a new value, reads back

**Output deliverable shape:** the file above, sectioned as `## What's ready`, `## Gaps`, `## Smallest #3 brick`, `## Risks`. Honest scope; no decisions locked.

---

## Architecture summary

```
B-3: Substrate Finishing
────────────────────────────────────────────────────
Section 1+2 — Hook H12 (the substantive work):
  - api.rs: CURRENT becomes Vec<CurrentContext> with push/pop/last_mut
  - api.rs: 5 host fn callers flip as_mut → last_mut (hook_arg_read/write, hook_set_return, hook_this_get, call_original_now)
  - host.rs: ParkedStore + parked() accessor + real call_hook_handler body
  - host.rs: WASM_CONTENDED atomic + one-shot/per-1000 log
  - mem_host.rs: run_wasm_with_mem refactored to park Store after frog_main returns
  - host.rs:46: remove #[allow(dead_code)] (call_hook_handler now has callers)
  - Incidental: marshal.rs:260 delete METHOD_ATTRIBUTE_STATIC_BIT if convenient

Section 3 — Read+write API readiness audit:
  - docs/superpowers/audits/2026-05-30-read-write-api-readiness.md
  - One-page planning artifact; no code
```

**Total touched code:** ~120 lines new + 5 lines deleted across 3 .rs files + 1 new audit markdown.

---

## Testing strategy

### Live-game regression (manual; PW + Highrise)

Deploy via `./deploy.sh release`. The four B-3 verification signals:

**1. No regression on existing capabilities.** `test_invoke.wasm` and `test_hook.wasm` on both PW and Highrise must produce the same outputs as B-2bc:

| Script | Expected log line |
|---|---|
| test_invoke.wasm | `[wasm] invoke Math::Pow returned 8.0 OK` |
| test_hook.wasm | `[wasm] install_hook OK` / `hooked Pow returned UNEXPECTED` (predates H12, stays) / `remove_hook OK` / `unhooked Pow returned 8.0 OK` |

**2. PARKED-not-yet-populated path verified by absence of failure.** test_hook.wasm's in-frog_main invoke hits the "PARKED None" branch → transparent observer → returns 8.0. Already covered by signal #1.

**3. WASM_CONTENDED absent on routine sessions.** `grep -c WASM_CONTENDED <agent.log>` → 0 on test runs. Appears only if Mutex contention occurs in production (e.g. multiple game threads racing for the same hooked method) — operator-visible signal when it does.

**4. PW dump count baseline maintained.** B-2bc baseline was 2,496 classes / 30,977 fields. B-3 must preserve this (H12 doesn't touch the dumper).

### Nested-hook handler safety: unit-test the CURRENT-stack

Add `crates/agent-core/tests/hook_runtime_stack.rs` — synthetic test that pushes 3 `CurrentContext` instances, pops them in order, verifies the per-thread stack discipline. Lives in agent-core only if the stack type itself can be exposed there; otherwise document deferred.

(Honestly: the stack discipline is small enough that the live-game regression is sufficient proof — if `with_current_context`'s push/pop is wrong, the existing test_hook would panic. Unit test is nice-to-have, not load-bearing.)

### Audit deliverable

Section 3's audit document is its own verification: the user reads it; if it correctly maps gaps for In-flight Modify, B-3 is done on that front. No automated test.

---

## What ships when B-3 lands

- Hook handlers can mutate game behavior (set_return, hook_set_arg) via real wasmi dispatch.
- Nested hook scenarios (hook A's handler calls original which fires hook B) work correctly via per-thread CURRENT-stack.
- Operator-visible Mutex contention signal (`WASM_HANDLER_CONTENDED`) — silent degradation impossible.
- One-page audit document mapping the path to In-flight Modify (priority #3).
- Existing capabilities (Invoke, Hook observation-only, Dumper, Calibration) unchanged.

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| `__indirect_function_table` export name mismatches what wat generates | wat 1.x's default for `(table funcref ...)` is the `__indirect_function_table` name. If a script uses an explicit `(export "X" (table ...))`, the lookup misses → return Err from `run_wasm_with_mem`. Acceptable — script error surfaces cleanly. |
| `try_lock` falls through on legitimate cross-thread contention (multiple game threads racing on same method) | WASM_CONTENDED log surfaces the rate. If routine production shows non-zero rates, a future micro-brick can add a short bounded `lock_timeout` (e.g. 1ms) before falling through. v1 try_lock-only is conservative. |
| Nested-hook stack underflow if push/pop pairing breaks via a panic path inside the handler | wasmi handler errors return `Err` from `call_hook_handler`; `with_current_context` still runs the pop afterwards. The only way pop misses a push is a Rust panic between push and pop — and panics across DllMain are undefined anyway. Acceptable scope; instrument with `expect("context underflow")` for visibility. |
| `Store::into_data()` consuming Store interferes with our clone-then-park sequence | We DON'T call `into_data()` anymore. We call `store.data().logs.clone()` to get logs without consuming, then move `store` into PARKED. |
| Audit document grows into actual #3 design work | Discipline at write-time: limit to 1-2 pages, no proposed implementations beyond "smallest viable brick" sketch. Real design happens at priority #3 brainstorm. |
