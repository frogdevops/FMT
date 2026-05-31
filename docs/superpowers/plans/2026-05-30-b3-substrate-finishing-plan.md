# B-3: Substrate Finishing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Hook H12 (real wasmi handler dispatch + nested-hook safety + cross-thread Store via parked Mutex) + a planning-artifact audit document for the In-flight Modify brick that follows.

**Architecture:** Four mechanical Rust changes across three files (api.rs CURRENT-stack, host.rs ParkedStore + real call_hook_handler, mem_host.rs run_wasm_with_mem refactor) plus one markdown audit doc. The Store is held by `run_wasm_with_mem` during frog_main (PARKED = None, in-frog_main hook invokes hit transparent observer), then transferred into a global `Mutex<Option<ParkedStore>>` so post-return hook callbacks can `try_lock` from the game thread without violating Rust's aliasing rules.

**Tech Stack:** Rust 2021. wasmi 0.32 (already a dep). No new deps. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (agent-core tests).

**Spec:** `docs/superpowers/specs/2026-05-30-b3-substrate-finishing-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent/src/internals/hook_runtime/api.rs` | Modify | CURRENT becomes `Vec<CurrentContext>`; with_current_context push/pop; 5 host fn callers flip as_ref/as_mut → last/last_mut |
| `crates/agent/src/runtime/host.rs` | Modify | Add `ParkedStore` struct + `parked()` accessor + `WASM_CONTENDED` atomic + `note_wasm_handler_contended()` helper; replace call_hook_handler stub with real wasmi dispatch body; remove `#[allow(dead_code)]` |
| `crates/agent/src/runtime/mem_host.rs` | Modify | run_wasm_with_mem refactored to clone logs + park Store + instance + funcref table |
| `docs/superpowers/audits/2026-05-30-read-write-api-readiness.md` | Create | Planning artifact for In-flight Modify (priority #3) |

**No agent-core changes.** All structural logic lives in `agent` (Windows-only).

---

## Task 1: CURRENT-stack — convert `Option<CurrentContext>` to `Vec<CurrentContext>` + flip 5 callsites

**Files:**
- Modify: `crates/agent/src/internals/hook_runtime/api.rs`

This task converts CURRENT from a single-slot Option to a per-thread stack. **All 6 sites (1 thread_local + 1 with_current_context push/pop + 5 host fn readers) must change in the SAME edit** — intermediate states don't compile.

- [ ] **Step 1: Convert the thread_local declaration (line ~35-36)**

In `crates/agent/src/internals/hook_runtime/api.rs`, find:

```rust
thread_local! {
    static CURRENT: RefCell<Option<CurrentContext>> = RefCell::new(None);
}
```

Replace with:

```rust
thread_local! {
    // Per-thread STACK so nested hook dispatches (handler A's call_original
    // triggering hooked method B on the same thread) don't corrupt each
    // other's context. Each dispatch pushes; the same dispatch pops at the
    // end of with_current_context. Host fns operate on `.last()` / `.last_mut()`
    // — the top of stack = the currently-active hook context.
    static CURRENT: RefCell<Vec<CurrentContext>> = RefCell::new(Vec::new());
}
```

- [ ] **Step 2: Convert `with_current_context` push (line ~57-66)**

Find:

```rust
    // Push our context onto the per-thread slot.
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(CurrentContext {
            method:          ctx.method,
            regs,
            args:            args.to_vec(),
            explicit_return: None,
            called_original: false,
        });
    });
```

Replace with:

```rust
    // PUSH our context onto the per-thread stack. Pairs with the pop below.
    CURRENT.with(|c| {
        c.borrow_mut().push(CurrentContext {
            method:          ctx.method,
            regs,
            args:            args.to_vec(),
            explicit_return: None,
            called_original: false,
        });
    });
```

- [ ] **Step 3: Convert `with_current_context` pop (line ~76-83)**

Find:

```rust
    let result = CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.take().expect("context vanished");
        HandlerResult {
            return_value: cc.explicit_return,
            called_original: cc.called_original,
        }
    });
```

Replace with:

```rust
    // POP the top of stack — the context we pushed above. Underflow would
    // mean a push/pop pairing bug elsewhere; expect-panic is the right
    // visibility on that invariant.
    let result = CURRENT.with(|c| {
        let cc = c.borrow_mut().pop().expect("context underflow");
        HandlerResult {
            return_value: cc.explicit_return,
            called_original: cc.called_original,
        }
    });
```

- [ ] **Step 4: Flip `hook_arg_read` (line ~214)**

Find the body:

```rust
pub fn hook_arg_read(arg_idx: usize) -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = borrow.as_ref()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_arg outside handler"))?;
        let arg = cc.args.get(arg_idx)
            .ok_or(agent_core::spine::InvokeError::ArgCountMismatch {
                expected: cc.args.len() as u8,
                got: arg_idx as u8,
            })?;
        Ok(arg.encode())
    })
}
```

Replace `borrow.as_ref()` (line ~217) with `borrow.last()`:

```rust
pub fn hook_arg_read(arg_idx: usize) -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = borrow.last()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_arg outside handler"))?;
        let arg = cc.args.get(arg_idx)
            .ok_or(agent_core::spine::InvokeError::ArgCountMismatch {
                expected: cc.args.len() as u8,
                got: arg_idx as u8,
            })?;
        Ok(arg.encode())
    })
}
```

- [ ] **Step 5: Flip `hook_arg_write` (line ~231)**

Find:

```rust
pub fn hook_arg_write(arg_idx: usize, bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    use crate::internals::marshal::args_to_regargs;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.as_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_arg outside handler"))?;
```

Replace `borrow.as_mut()` with `borrow.last_mut()`:

```rust
pub fn hook_arg_write(arg_idx: usize, bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    use crate::internals::marshal::args_to_regargs;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_arg outside handler"))?;
```

(Rest of the function body unchanged.)

- [ ] **Step 6: Flip `hook_this_get` (line ~254)**

Find:

```rust
pub fn hook_this_get() -> u64 {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = match borrow.as_ref() { Some(x) => x, None => return 0 };
```

Replace `borrow.as_ref()` with `borrow.last()`:

```rust
pub fn hook_this_get() -> u64 {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = match borrow.last() { Some(x) => x, None => return 0 };
```

(Rest of the function body unchanged.)

- [ ] **Step 7: Flip `hook_set_return` (line ~270)**

Find:

```rust
pub fn hook_set_return(bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    let (decoded, _) = agent_core::spine::InvokeArg::decode(bytes)
        .ok_or(agent_core::spine::InvokeError::MarshalFailed { idx: 0, reason: "decode failed" })?;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.as_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_return outside handler"))?;
        cc.explicit_return = Some(decoded);
        Ok(())
    })
}
```

Replace `borrow.as_mut()` with `borrow.last_mut()`:

```rust
pub fn hook_set_return(bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    let (decoded, _) = agent_core::spine::InvokeArg::decode(bytes)
        .ok_or(agent_core::spine::InvokeError::MarshalFailed { idx: 0, reason: "decode failed" })?;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_return outside handler"))?;
        cc.explicit_return = Some(decoded);
        Ok(())
    })
}
```

- [ ] **Step 8: Flip `call_original_now` (line ~284-290)**

Find the function (the only `borrow.as_mut()?` in the file, line ~290):

```rust
pub fn call_original_now() -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    // ... (preamble) ...
    let (regs_ptr, sig_return_type, sig_return_tc, trampoline) = CURRENT.with(|c| -> Option<_> {
        let mut borrow = c.borrow_mut();
        let cc = borrow.as_mut()?;
```

Replace `borrow.as_mut()?` with `borrow.last_mut()?`:

```rust
pub fn call_original_now() -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    // ... (preamble unchanged) ...
    let (regs_ptr, sig_return_type, sig_return_tc, trampoline) = CURRENT.with(|c| -> Option<_> {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()?;
```

(Rest of `call_original_now` unchanged.)

- [ ] **Step 9: Build cross-compile + agent-core tests**

```bash
cargo build --target x86_64-pc-windows-gnu --release
cargo test -p agent-core
```

Both clean.

- [ ] **Step 10: DO NOT commit**

User commits own work.

**Sanity verification:**

1. `grep -n "RefCell<Vec<CurrentContext>>\|RefCell<Option<CurrentContext>>" crates/agent/src/internals/hook_runtime/api.rs` → exactly 1 hit, on the `Vec<>` form.
2. `grep -n "c.borrow_mut().push\|c.borrow_mut().pop()" crates/agent/src/internals/hook_runtime/api.rs` → 1 push, 1 pop.
3. `grep -n "borrow.last()\|borrow.last_mut()" crates/agent/src/internals/hook_runtime/api.rs` → 5 hits total (3 last_mut for hook_arg_write/hook_set_return/call_original_now + 2 last for hook_arg_read/hook_this_get).
4. `grep -n "borrow.as_ref()\|borrow.as_mut()" crates/agent/src/internals/hook_runtime/api.rs` → 0 hits.
5. `grep -n "expect(\"context vanished\")" crates/agent/src/internals/hook_runtime/api.rs` → 0 hits (old message removed; `expect("context underflow")` is the new one).
6. Build + tests clean.

---

## Task 2: `ParkedStore` + `parked()` accessor + `WASM_CONTENDED` atomic (additive; no behavior change yet)

**Files:**
- Modify: `crates/agent/src/runtime/host.rs`

This task adds the new types and the contention counter. The call_hook_handler stub stays in place — Task 4 replaces it. Additive change: compiles clean, runtime behavior identical.

- [ ] **Step 1: Add imports**

In `crates/agent/src/runtime/host.rs`, find the top-of-file imports (currently `use crate::paths::log;`). Add a new use line for the wasmi types:

```rust
use crate::paths::log;
use crate::runtime::mem_host::HostState;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};
```

(If `HostState` isn't already `pub` in mem_host.rs, you'll need to add `pub` to its declaration — see Task 3 Step 1 which makes it public.)

- [ ] **Step 2: Add `ParkedStore` struct + `parked()` accessor**

After the imports (before the existing `pub fn maybe_run_configured`), add:

```rust
/// Owns the wasmi Store after `frog_main` returns, so post-frog_main hook
/// callbacks can `try_lock` it from the game thread. Park-on-return + Mutex
/// is the only sound model (Rust's aliasing rules forbid two `&mut Store`
/// simultaneously; reentrant locks would type-check but be UB).
pub struct ParkedStore {
    pub store: wasmi::Store<HostState>,
    pub instance: wasmi::Instance,
    pub funcref_table: wasmi::Table,
}

static PARKED: OnceLock<Mutex<Option<ParkedStore>>> = OnceLock::new();

/// Global accessor for the parked Store. Initialized lazily; the Mutex's
/// inner Option is `None` until `run_wasm_with_mem` parks the Store after
/// `frog_main` returns.
pub fn parked() -> &'static Mutex<Option<ParkedStore>> {
    PARKED.get_or_init(|| Mutex::new(None))
}
```

- [ ] **Step 3: Add `WASM_CONTENDED` atomic + `note_wasm_handler_contended()` helper**

Append (after the parked() accessor):

```rust
/// Tracks Mutex-contention events on PARKED (silent-degradation signal:
/// when the game thread can't acquire the Store, the dispatcher falls
/// through to transparent observer instead of running the wasm handler).
/// One-shot log on first hit + every-1000 thereafter; process-lifetime
/// cumulative. Mirrors the IOCP CAP_HIT_COUNT pattern from B-2bc.
static WASM_CONTENDED: AtomicU64 = AtomicU64::new(0);

fn note_wasm_handler_contended() {
    let prev = WASM_CONTENDED.fetch_add(1, Ordering::Relaxed);
    if prev == 0 {
        log("⚠ WASM_HANDLER_CONTENDED — PARKED store held; transparent observer fired");
    } else if (prev + 1) % 1000 == 0 {
        log(&format!("⚠ WASM_HANDLER_CONTENDED count={} (degraded handler dispatch)", prev + 1));
    }
}
```

- [ ] **Step 4: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean. You'll see one or two `dead_code` warnings on the new items (ParkedStore, parked, WASM_CONTENDED, note_wasm_handler_contended) — those go away in Task 4 when call_hook_handler starts using them. Pre-existing warnings ok.

If the build fails with "cannot find type `HostState` in this scope" or similar, Task 3 Step 1 (making HostState pub) needs to land first. In that case, jump to Task 3 Step 1 only, then resume here.

- [ ] **Step 5: DO NOT commit**

User commits own work.

**Sanity verification:**

1. `grep -n "pub struct ParkedStore\|static PARKED\|pub fn parked\|static WASM_CONTENDED\|fn note_wasm_handler_contended" crates/agent/src/runtime/host.rs` → 5 hits.
2. `grep -n "use crate::runtime::mem_host::HostState" crates/agent/src/runtime/host.rs` → 1 hit.
3. Cross-compile build clean.

---

## Task 3: `run_wasm_with_mem` refactor — clone logs + park Store

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs`

The current ending `Ok(store.into_data().logs)` consumes the Store. New ending clones the logs (so we still return them) then moves Store ownership into PARKED so handler dispatch can `try_lock` it post-frog_main.

- [ ] **Step 1: Make `HostState` public**

In `crates/agent/src/runtime/mem_host.rs`, find the struct declaration around line 13:

```rust
struct HostState {
```

Make it `pub`:

```rust
pub struct HostState {
```

This allows `runtime::host::ParkedStore` to name `wasmi::Store<HostState>` as a field type.

- [ ] **Step 2: Find the end of `run_wasm_with_mem`**

The function ends at line ~296-297 with:

```rust
    let frog_main = instance
        .get_typed_func::<(), ()>(&store, "frog_main")
        .map_err(|_| WasmError::NoEntry)?;
    frog_main.call(&mut store, ()).map_err(|e| WasmError::Trap(e.to_string()))?;
    Ok(store.into_data().logs)
}
```

- [ ] **Step 3: Replace the closing block**

Replace the lines above (from `let frog_main = ...` through `Ok(store.into_data().logs)`) with:

```rust
    let frog_main = instance
        .get_typed_func::<(), ()>(&store, "frog_main")
        .map_err(|_| WasmError::NoEntry)?;
    frog_main.call(&mut store, ()).map_err(|e| WasmError::Trap(e.to_string()))?;

    // B-3 Section 1+2: park the Store + instance + funcref table so post-
    // frog_main hook callbacks (fired from game thread) can try_lock and
    // invoke the registered handler funcref via wasmi typed call. Clone
    // logs out FIRST (into_data() consumes; we need to keep Store alive).
    let logs = store.data().logs.clone();

    let funcref_table = instance
        .get_table(&store, "__indirect_function_table")
        .ok_or_else(|| WasmError::Instantiate(
            "missing __indirect_function_table export (Hook H12 requires it)".into(),
        ))?;

    *crate::runtime::host::parked().lock().unwrap() = Some(
        crate::runtime::host::ParkedStore { store, instance, funcref_table }
    );

    Ok(logs)
}
```

The `store` and `instance` bindings move into the ParkedStore — they're not used after this point in the function.

- [ ] **Step 4: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean. If you see "borrow of moved value" errors, double-check that `let logs = store.data().logs.clone();` runs BEFORE the line that moves `store` into ParkedStore. The order in the snippet above is correct.

- [ ] **Step 5: DO NOT commit**

User commits own work.

**Sanity verification:**

1. `grep -n "pub struct HostState" crates/agent/src/runtime/mem_host.rs` → 1 hit (was `struct HostState` without pub).
2. `grep -n "store.into_data().logs" crates/agent/src/runtime/mem_host.rs` → 0 hits (the consume-on-return pattern is gone).
3. `grep -n "store.data().logs.clone()\|crate::runtime::host::parked()" crates/agent/src/runtime/mem_host.rs` → at least 2 hits (clone + parked()).
4. `grep -n "__indirect_function_table" crates/agent/src/runtime/mem_host.rs` → 1 hit.
5. Cross-compile build clean.

---

## Task 4: Real `call_hook_handler` body — replace the stub

**Files:**
- Modify: `crates/agent/src/runtime/host.rs`

Replaces the H10 stub at line ~46-49 with the real wasmi dispatch. After this task, the `#[allow(dead_code)]` marker comes off — the function gains callers via Task 1's preserved `with_current_context::call_hook_handler` site.

- [ ] **Step 1: Locate the existing stub**

In `crates/agent/src/runtime/host.rs`, find the doc-comment block + stub:

```rust
/// Cache the wasm handler funcref dispatch path. Called by hook dispatcher
/// by its function-table index. In v1 this is a stub that returns Ok(()) —
/// the actual wasmi typed call requires holding the Store handle, which the
/// agent doesn't yet thread through to hook dispatch. The hook host fns
/// hook_arg / hook_set_arg / etc. still work — they read CURRENT regardless.
///
/// To enable a real handler dispatch, we'd need to either:
///   (a) cache the Store/Linker handle globally and call it here
///   (b) wire a callback at install-time from mem_host.rs into the api module
///
/// For the PW gate (H11), we use (a) with a Mutex<Option<Box<dyn ...>>> set up
/// at module instantiation. See H11 for the gate-only path.
#[allow(dead_code)]
pub fn call_hook_handler(_handler_funcref_idx: u64) -> Result<(), &'static str> {
    Ok(())
}
```

- [ ] **Step 2: Replace with the real body**

Replace the entire block (from the `///` doc comment through the closing `}` of the stub) with:

```rust
/// Dispatch the hook handler at `handler_funcref_idx` in the wasm module's
/// funcref table. Called by `dispatch_rust` on the GAME thread; the parked
/// Store was placed by `run_wasm_with_mem` on the agent worker thread after
/// `frog_main` returned.
///
/// SAFETY MODEL (do not "optimize" with ReentrantMutex — see B-3 spec):
/// Rust forbids two `&mut Store` simultaneously. We only ever hold one:
/// during frog_main, run_wasm_with_mem owns it (PARKED is None → this fn
/// returns Err → transparent observer fires); after frog_main returns, the
/// Store moves into PARKED and the guard provides the unique `&mut`.
///
/// Returns Err on contention (transparent observer fires; logged once per
/// 1000), Err when PARKED is None (frog_main still running, no script
/// loaded, or no hooks installed before exit), Err when funcref/sig is bad
/// (script error), or Err if the wasm handler trapped.
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
        None => return Err("PARKED not yet populated (frog_main running, or no script loaded)"),
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
```

Note: the `#[allow(dead_code)]` is GONE — `call_hook_handler` now has a real caller (the existing `with_current_context` site at api.rs:72).

- [ ] **Step 3: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean. The previous `dead_code` warnings on ParkedStore/parked/WASM_CONTENDED/note_wasm_handler_contended (from Task 2) should also disappear — `call_hook_handler` now uses all of them.

If you see a `wasmi::Val::FuncRef` not found / pattern mismatch error: the wasmi 0.32 enum variant might be named differently in the installed version (check the lockfile for the exact wasmi version). The expected variant in wasmi 0.32 is `Val::FuncRef(FuncRef)`. If it's `Val::Ref(Ref::Func(...))` or similar in your version, adapt the pattern accordingly (the resolution semantic is identical).

- [ ] **Step 4: DO NOT commit**

User commits own work.

**Sanity verification:**

1. `grep -n "Ok(())\s*$" crates/agent/src/runtime/host.rs` → at most 1 hit in `maybe_run_configured` area (NOT in call_hook_handler). The stub body is gone.
2. `grep -n "pub fn call_hook_handler" crates/agent/src/runtime/host.rs` → 1 hit; the signature is now `call_hook_handler(handler_funcref_idx: u64)` (no underscore prefix, parameter is read).
3. `grep -n "#\\[allow(dead_code)\\]" crates/agent/src/runtime/host.rs` → 0 hits (the H10 marker is removed).
4. `grep -n "parked().try_lock()\|note_wasm_handler_contended\|funcref_table.get" crates/agent/src/runtime/host.rs` → 3 hits (one each).
5. `grep -n "func.typed::<(), (), _>" crates/agent/src/runtime/host.rs` → 1 hit.
6. Cross-compile build clean; no new warnings on the new items from Task 2 (they're all used now).

---

## Task 5: Read+write API readiness audit document

**Files:**
- Create: `docs/superpowers/audits/2026-05-30-read-write-api-readiness.md`

Planning artifact for In-flight Modify (priority #3). No code. ~1-2 page markdown report. The In-flight Modify brainstorm reads this and uses it to pick scope.

- [ ] **Step 1: Create the audit doc**

Create `docs/superpowers/audits/2026-05-30-read-write-api-readiness.md` with this content:

```markdown
# Read+Write API Readiness Audit

**Date:** 2026-05-30
**Purpose:** Map the gap between today's read/write APIs and what In-flight Modify (priority #3) will need. Input artifact for #3's brainstorm.
**Scope:** Audit-only — no decisions, no implementation. Real design lands at #3 brainstorm.

---

## What's ready (read paths)

**WASM-side host fns** (all in `crates/agent/src/runtime/mem_host.rs`, registered in `run_wasm_with_mem`):

| Group | Host fn | Notes |
|---|---|---|
| Memory | `mem.read` | Bounds-checked typed read via `external::api::read` |
| Memory | `mem.scan` | AOB scan via `external::scan::aob_scan` (streaming) |
| Memory | `mem.regions` | List committed-readable regions |
| il2cpp | `il2cpp.find_class` | Walk class table by name |
| il2cpp | `il2cpp.field_info` | Lookup field metadata (offset + type) |
| il2cpp | `il2cpp.get_field` | Read field value at klass+offset |
| il2cpp | `il2cpp.klass_of` | Get klass from instance ptr |
| il2cpp | `il2cpp.static_field` | Address of static-field storage |
| il2cpp | `il2cpp.find_method` | Walk method array by name + argc |
| il2cpp | `il2cpp.invoke` | runtime_invoke with marshalled args |
| il2cpp | `il2cpp.install_hook` | Install handler funcref |
| il2cpp | `il2cpp.remove_hook` | Uninstall |
| il2cpp | `il2cpp.hook_arg` | Read current hook's arg by index |
| il2cpp | `il2cpp.hook_this` | Get instance ptr (or 0 for static) |
| il2cpp | `il2cpp.call_original` | Run trampoline from within handler |

**Rust-side typed siblings** (architectural contract — see [[spec2-domain-audit-and-cleanup]] memory: these are LOAD-BEARING, not dead code):

- `external::api::{read_t<T, C>, read_bytes_t<C>, read_cstr_t<C>}` — capability-discipline via `MemAddr<ReadOnly>` / `MemAddr<ReadWrite>`
- `internals::api::{find_class_t, find_method_t, field_addr_t, static_field_t, klass_of_t, invoke_method_t}` — return typed handles instead of raw u64
- `invoke_method_t` is the ONE typed sibling actively called today (by `mem_host::host_invoke`)

The Rust-side typed surface is for future composers (frontend plugin, native plugin layer, Rust callers beyond the WASM-host-fn boundary). The WASM boundary uses untyped i64s because WASM only has i32/i64 — marshalling at the boundary, typed Rust side.

---

## What exists for writes (partial)

**WASM-side write host fns** (gated by `FROG_WASM_WRITE` env var):

- `mem.write` — typed byte-level write at a raw address
- `mem.write_if` — compare-and-swap (read → confirm expected → write)

**Rust-side typed write sibling** (architectural — also load-bearing):

- `external::api::write_t<T>(addr: MemAddr<ReadWrite>, val)` — capability-gated typed write; the `ReadWrite` requirement is a compile-time guarantee enforced by Spine T5 doc-tests at `external/api.rs:108-121`

**Gap A — typed write host fn not yet registered:** the WASM boundary today has untyped `mem.write` only. To match the read-side ergonomics (typed-then-marshalled), `mem.write_t` should be registered alongside `mem.write` and route to `external::api::write_t`. ~20 lines.

---

## What's missing (field-set + method-set paths) — the actual #3 blockers

**There is no field-write path through il2cpp today.** Two routes are possible; both need work:

### Route 1 — `field_set_value` FFI

The il2cpp library exports `il2cpp_field_set_value` (instance fields) and `il2cpp_field_static_set_value` (static fields). Our FFI resolver (`internals::ffi::resolve_*`) handles `field_get_name` / `field_get_type` via standard exports + sig-scan, but does NOT resolve the `*_set_value` variants.

**Work:** add the `*_set_value` symbols to the standard-exports resolver block AND to the sig-scan path (the latter needs a new byte pattern — `il2cpp_field_set_value` is a small function: validate + memcpy). Pattern matches what B-1 Phase 5 calibration already does for the `_get_*` variants.

### Route 2 — direct memory write at field address

If we already have `field_addr_t(instance, klass, field_name) → MemAddr<ReadWrite>` (we don't, but it's straightforward via `instance + field.offset` for instance fields, or `static_storage + field.offset` for static fields), then `mem.write_t(addr, value)` writes the field directly. Faster than the FFI route (no function-call overhead), bypasses any il2cpp lifecycle hooks (could miss `runtime_class_init` for statics).

**Work:** add `field_addr_t` to `internals::api` (Rust-side), expose `il2cpp.field_addr` host fn, ensure the `MemAddr<ReadWrite>` capability flows correctly when paired with `mem.write_t`.

### Combined approach (likely #3's choice)

Most il2cpp dumpers use Route 2 for read (we do too — `get_field` reads at `instance + offset`) and Route 1 for write (correctness over micro-perf). The `#3 brainstorm should evaluate both and pick.

---

## Smallest viable In-flight Modify brick (sketch only)

Not a commitment — #3 brainstorm refines. This is what's possible with the current substrate:

1. Add `field_set_value` to the FFI resolver (standard exports + sig-scan). ~40 lines.
2. Register `mem.write_t` host fn (typed write through existing `external::api::write_t`). ~20 lines.
3. Register `il2cpp.set_field` host fn (parallel to `il2cpp.get_field`). ~50 lines (handle value-type vs reference-type per the existing `get_field` pattern).
4. Optional: `field_addr_t` Rust-side typed sibling + `il2cpp.field_addr` host fn. ~30 lines.
5. Verification: `scratch/test_modify.wat` — read Player.position field, write a new value, read back, log result.

**Total: ~140 lines of code + 1 test fixture.** Comparable in scope to the B-2bc bundle.

---

## Risks for #3

| Risk | Notes |
|---|---|
| `il2cpp_field_set_value` not exported on PW (obfuscated) | Sig-scan path handles this — pattern matches existing _get_* discipline. May need cross-validation against a known field to confirm the resolved address actually writes. |
| Value-type vs reference-type set has different ABIs | `il2cpp_field_set_value` takes a `void*` for value types (pointer to the value bytes) vs `Il2CppObject*` for reference types. Pattern matches what `marshal::pack_return_into_regargs` already handles. |
| Static field write needs class init | Most modders write to instance fields; static-field write can be a stretch goal. `il2cpp_runtime_class_init` is the gate; can be a separate task. |
| Field-write triggering serialization or anti-cheat | Out of scope for our agent — user's modding script policy decision. We provide the primitive; the actor decides when to use it. |

---

## Out-of-scope for #3

- Method-body REWRITE (vs the existing method-hook, which intercepts) — the inline_detour patcher could theoretically rewrite method bodies but the use cases for "modify a method's existing instructions" vs "intercept via hook" are vanishingly small for modders.
- Generic-instantiation modification — modifying open generics requires re-instantiating + re-registering with il2cpp, far beyond In-flight Modify.

---

## Verdict

The current substrate is ready for In-flight Modify with ~140 lines of additive work. No bedrock blockers. No spine restructure needed. The audit's recommendation: pick Route 2 (direct mem write at field address) as the primary path for instance fields, add Route 1 (FFI) as fallback for static-field-with-init. Plan accordingly at #3 brainstorm.
```

- [ ] **Step 2: Verify file exists + parses**

```bash
ls -la docs/superpowers/audits/2026-05-30-read-write-api-readiness.md
wc -l docs/superpowers/audits/2026-05-30-read-write-api-readiness.md
```

Expected: file exists, ~120-160 lines.

- [ ] **Step 3: DO NOT commit**

User commits own work.

**Sanity verification:**

1. `ls docs/superpowers/audits/2026-05-30-read-write-api-readiness.md` → file present.
2. The doc explicitly references the spine `_t` load-bearing warning + cites the memory file.
3. The doc gives an honest ~140-line sketch for #3, not a 500-line over-detailed proto-spec.

---

## Task 6: Live-game regression gate (manual; user)

**Files:** none modified.

The verification standard: every existing capability still works exactly as before B-3, and the only observable B-3 change is that the H12 path is now LIVE (not stubbed).

- [ ] **Step 1: Deploy**

Run: `./deploy.sh release`
Expected: clean build, deployed to both Pixel Worlds and Highrise.

- [ ] **Step 2: PW Invoke**

User launches PW with:
```
WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_invoke.wasm %command%
```

Expected in agent.log:
```
[wasm] invoke Math::Pow(2.0,3.0) status OK
[wasm] invoke Math::Pow returned 8.0 OK
```

Tells us: Invoke path unaffected by H12 changes.

- [ ] **Step 3: PW Hook (the H12 test)**

User launches PW with:
```
WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_hook.wasm %command%
```

Expected in agent.log:
```
[wasm] install_hook OK
[wasm] hooked Pow returned UNEXPECTED   ← STAYS: in-frog_main invoke hits PARKED=None → transparent observer → 8.0
[wasm] remove_hook OK
[wasm] unhooked Pow returned 8.0 OK
```

**Critical:** the "hooked Pow returned UNEXPECTED" log line PERSISTS post-B-3 because test_hook.wasm's invoke happens INSIDE frog_main, before the Store parks. This is per-spec; verifying handler mutation requires a two-phase test fixture (deferred). What matters: agent did NOT panic on "context underflow" / "context vanished" (CURRENT-stack discipline working) and did NOT crash on the call_hook_handler real body (no UB from aliasing).

- [ ] **Step 4: Highrise Invoke (no regression)**

Same launch options + `FROG_WASM=test_invoke.wasm` on Highrise. Same expected output (8.0 OK). Tells us: cross-game integrity preserved.

- [ ] **Step 5: PW normal launch (no regression on dumper)**

User launches PW without FROG_WASM:
```
WINEDLLOVERRIDES="version=n,b" %command%
```

Verify after closing game:
```bash
DUMP="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"
LOG="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log"
echo "dumped:           $(grep 'dumped' "$LOG" | tail -1)"
echo "WASM_HANDLER_CONTENDED:  $(grep -c 'WASM_HANDLER_CONTENDED' "$LOG")  (expect 0)"
echo "<garbage-tc:      $(grep -c '<garbage-tc:' "$DUMP")  (expect 0-5; baseline from B-2bc)"
echo "META offsets:     $(grep -c 'Offset: META' "$DUMP")  (expect ~21)"
```

Expected: dumped ≈ 2,496 classes / 30,977 fields (B-2bc baseline). `WASM_HANDLER_CONTENDED` should be 0 on a normal-no-WASM session (the metric only fires when WASM is loaded and contended).

- [ ] **Step 6: Highrise normal launch (no regression on dumper)**

Same as Step 5 on Highrise. Expected: dumped ≈ 15,414 classes / 80,427 fields.

- [ ] **Step 7: Hand back to user**

If all six runs match expectations (and especially: no panic on "context underflow" in any Hook run, no game crash from call_hook_handler real body), **B-3 is GREEN**.

Most likely diagnostic paths if anything regresses:
- "context underflow" panic → Task 1 push/pop pairing has a missed flip somewhere; grep for any remaining `borrow.as_mut()` / `borrow.as_ref()` / `borrow.take()` in api.rs
- Game crashes on first hook fire → call_hook_handler real body has a wasmi version mismatch (e.g. `Val::FuncRef` pattern); check wasmi 0.32 lockfile + adapt pattern
- "missing __indirect_function_table" error → script's funcref table has a different export name; check the wat file (probably needs `(table (export "__indirect_function_table") ...)` explicit form)
- WASM_HANDLER_CONTENDED appears mid-game → unexpected, since only one game thread typically fires a given hook at a time; check for hook-on-hook reentrancy scenarios

---

## Self-review

**1. Spec coverage:**

| Spec section | Task |
|---|---|
| Section 1: PARKED architecture (ownership transfer model) | Task 2 (types) + Task 3 (transfer site) |
| Section 1: Same-thread reentrancy explanation (frog_main + hook = PARKED=None branch) | Documented in Task 4 Step 2 doc-comment + verified in Task 6 Step 3 |
| Section 2a: CURRENT-stack (Vec<CurrentContext>) | Task 1 |
| Section 2a: 5 host fn callers flip as_mut → last_mut | Task 1 Steps 4-8 |
| Section 2b: call_hook_handler real body | Task 4 |
| Section 2c: run_wasm_with_mem refactor | Task 3 |
| Section 2d: METHOD_ATTRIBUTE_STATIC_BIT incidental cleanup | NOT in plan — folded into B-3 followup; can be a 1-line cleanup task added later if it bothers anyone |
| Section 3: Audit doc | Task 5 |
| Testing: live-game regression | Task 6 |
| Testing: unit-test CURRENT-stack | Not in plan — per spec, the live-game regression is sufficient proof; unit test was nice-to-have, deferred |

**Gap addressed:** the METHOD_ATTRIBUTE_STATIC_BIT cleanup was non-essential incidental in the spec. Dropping it from the plan keeps the brick focused on the load-bearing H12 work. A 1-line constant deletion can land in any future PR that touches marshal.rs.

**2. Placeholder scan:** No TBD/TODO/vague verbs. Every code block is complete copy-paste ready. The wasmi 0.32 API surface (Val::FuncRef, Table::get, Func::typed) is specified with the diagnostic fallback ("if it's named differently...") explicitly stated for Task 4.

**3. Type consistency:**
- `ParkedStore { store, instance, funcref_table }` defined identically in Task 2 + Task 3.
- `parked()` function name consistent (Task 2 def + Task 3 call site + Task 4 call site).
- `note_wasm_handler_contended` name + `WASM_CONTENDED` static + `WASM_HANDLER_CONTENDED` log string consistent across Tasks 2, 4, 6.
- `borrow.last()` vs `borrow.last_mut()` chosen per-callsite based on whether it was previously `as_ref()` or `as_mut()` — verified in Task 1's per-step diff.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-30-b3-substrate-finishing-plan.md`. **6 tasks**, scoped to the H12 substantive work + audit doc.

Two execution options:

**1. Subagent-Driven (recommended)** — per the [[subagents-use-opus]] memory pattern: **Sonnet on Tasks 1, 2, 3, 5, 6** (mechanical refactor / additive / doc / manual). **Opus on Task 4** (the real call_hook_handler body — wasmi API specifics, the most non-mechanical of the four code tasks, and the one where wrong implementation silently corrupts production via failed handler dispatch).

**2. Inline Execution** — execute each task in this session with checkpoints between for your review.

Which approach?
