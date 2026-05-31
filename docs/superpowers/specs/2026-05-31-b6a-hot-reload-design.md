# B-6a — WASM Hot Reload + Runtime Registry + Write Journal

**Date:** 2026-05-31
**Status:** Approved (brainstorm complete; ready for plan)
**Predecessor:** B-5 WASM Typed Dispatch (shipped 2026-05-31)
**Successor:** B-6b BYOL demo + test fixture; B-6c rigorous stress test
**Sequencing:** Ships before B-7 (Protocol API) and B-8 (In-flight Modify).

## Motivation

Today's dev loop for a WASM script is `edit → cargo build agent → ./deploy.sh → restart Steam → wait for game load → trigger event → observe`. That's 30+ seconds per iteration and breaks any flow state the actor is building. Per [[scripter-vs-modder-experience]] this is unacceptable for the scripter-grade experience the FDK promises (fast + stable + rewarding).

B-6a closes this loop by adding hot reload: drop a new `.wasm` in a watched folder, the agent swaps the running script in-place without game restart. The replacement is clean — per [[script-lifetime-revert-boundary]], every memory write the old script made is automatically reverted before the new script starts, so each iteration begins from a known baseline.

The unlock cascade: B-6a makes B-6b's BYOL demonstration (writing a script in C / Zig / AssemblyScript and seeing it run) feasible; B-6c's rigorous stress test becomes tractable because matrix-style verification doesn't require minutes per scenario. Both are gates for the FDK being usable by anyone other than the project's authors.

## Locked decisions (from 2026-05-31 brainstorm)

| # | Decision | Rationale |
|---|---|---|
| 1 | Filesystem-only API. No socket, no IPC daemon. | [[wild-west-platform-philosophy]] — files are universally understood; any frontend / shell script / `mv` can drive the agent. Socket transport is a separate brick if polling lag bites. |
| 2 | Replace semantics (one runtime live at a time). | Multi-runtime fleet requires a frontend to be meaningful (introspection / per-runtime control). Future extension via Vec-of-1 prefigure pattern + comments. |
| 3 | scripts/ folder lives next to `agent.dll` (i.e., game DIR). | Existing `paths::output_path` resolves there. Convention path = `<game_dir>/scripts/active.wasm`. |
| 4 | Write journal (HashMap<addr, original_bytes>) with auto-revert on script stop. | [[script-lifetime-revert-boundary]] — substrate owns the lifecycle boundary; scripts manage their own mid-life state. |
| 5 | `mem.write_permanent` host fn opts out of journaling. | The actor's explicit "this change outlives me" declaration. |
| 6 | `FROG_WASM_WRITE` env gate REMOVED. Writes are unconditional. | [[wild-west-platform-philosophy]] — gate violates "no rules laid"; was transitional discipline during substrate maturation; B-5 proved the write path live. |
| 7 | `FROG_WASM` env var DELETED. Watcher is the only loader. | Eliminates two-loader confusion. Migration cost: drop the .wasm at `scripts/active.wasm` instead of via env var. |
| 8 | Game-thread sync via `dispatch_rust` piggyback — NOT via a separate tick primitive. | [[hooks-are-the-sync-primitive]] — the audit proved dispatch_rust already runs on game thread with game frozen; no Unity player-loop discovery needed. Saves a full bedrock pre-brick. |
| 9 | Hook ownership tracking via `runtime_id` field on `HookCtx` + scan. | Simpler than threading the registry through `install_hook`; 256-slot scan is microseconds. |
| 10 | Single-game-thread assumption documented; future epoch-based fix is a separate brick if/when violated. | Per Unity's typical execution model; sufficient for Pixel Worlds and similar games. |

## Architecture

Three new components + a touch to two existing globals + a refactor of the parking model.

```
┌──────────────────────────┐
│  Watcher thread          │   spawned at entry.rs::DllMain
│  - polls scripts/active   │   shuts down via STOPPING flag at DLL_DETACH
│    .wasm every 500ms      │
│  - publishes RELOAD_      │
│    PENDING on change      │
│  - direct fallback after  │
│    1000ms if not consumed │
└───────────┬──────────────┘
            │
            ▼ atomic publish
┌──────────────────────────┐
│  RELOAD_PENDING            │
│  Mutex<Option<Vec<u8>>>    │
└───────────┬──────────────┘
            │ drained by either:
            ▼
┌──────────────────────────┐     ┌───────────────────────┐
│  dispatch_rust piggyback │     │  watcher fallback     │
│  (game thread, frozen)   │     │  (agent thread)       │
│  PREFERRED PATH           │     │  fallback if no hook  │
└───────────┬──────────────┘     │  consumes within 1s   │
            │                     └───────────┬───────────┘
            │                                 │
            └──────────────┬──────────────────┘
                           ▼
              ┌──────────────────────────┐
              │  reload_orchestrator      │
              │  1. lock registry         │
              │  2. revert journal        │
              │  3. unhook owned_hooks    │
              │  4. drop old runtime      │
              │  5. spawn new runtime     │
              └───────────┬──────────────┘
                          │
                          ▼
              ┌──────────────────────────┐
              │  RuntimeRegistry          │
              │  Mutex<Vec<ParkedRuntime>>│
              │  len always 1 today       │
              └───────────┬──────────────┘
                          ▼
              ParkedRuntime {
                store, instance, funcref_table,
                runtime_id: u64,
                owned_hooks: Vec<HookHandle>,
                write_journal: HashMap<usize, Vec<u8>>,
              }
                          ▲
                          │ try_lock from game thread
                          │
              ┌──────────────────────────┐
              │  HOOK_SLOTS               │
              │  (unchanged, +runtime_id  │
              │   field on HookCtx)       │
              └──────────────────────────┘
```

### Registry replaces `PARKED`

Today's `static PARKED: OnceLock<Mutex<Option<ParkedStore>>>` (host.rs:23) becomes `static REGISTRY: OnceLock<Mutex<Vec<ParkedRuntime>>>`. Accessor `parked()` becomes `registry()`. The `call_hook_handler` callsite rewrites from `parked().try_lock()` to `registry().try_lock()?.get_mut(0)` (Vec-of-1 access). Concurrency contract unchanged — still try_lock with transparent-observer fallback on contention.

`ParkedRuntime` extends today's `ParkedStore` with three new fields:
- `runtime_id: u64` — monotonically-increasing id, assigned at spawn
- `owned_hooks: Vec<HookHandle>` — populated by `install_hook` (which gains access to the current runtime's id via registry lookup)
- `write_journal: HashMap<usize, Vec<u8>>` — populated by `host_write` / `host_write_if` on first-touch of each address

### Write journal: first-touch capture, full revert

Every call to `mem.write(addr, ty, bytes)` and `mem.write_if(addr, ty, exp, new)`:
1. Take registry lock.
2. Look up current runtime.
3. `journal.entry(addr).or_insert_with(|| backend_read_bytes(addr, ty.fixed_width()))` — captures original bytes on first touch only. Subsequent writes to same address do not overwrite the captured original.
4. Perform the typed write via existing B-5 path.

Revert (called by orchestrator step 2):
- Iterate journal; for each `(addr, original_bytes)`, write `original_bytes` back via the guarded-write backend.
- Free the journal.
- Order: arbitrary (HashMap iteration). Overlapping writes self-resolve because each affected address has its first-observed original bytes stored separately.

`mem.write_permanent(addr, ty, bytes)` — new host fn. Same backend write as `mem.write`. Skips journal recording. Doc-warn: "this change survives script stop; the actor takes responsibility for cleanup."

### Hook ownership tracking

`HookCtx` (registry.rs:30-41) gains one field:
```rust
pub struct HookCtx {
    pub method:     MethodPtr,
    pub sig:        MethodSignature,
    pub thunk_addr: usize,
    pub patch:      Hook,
    pub handler_func_ref: u64,
    pub runtime_id: u64,  // NEW
}
```

`install_hook` (api.rs:200's sibling install path) reads the current runtime's id from the registry at install time, stores it in the new `HookCtx`. The handle is also pushed into `current_runtime.owned_hooks` for symmetry (the Vec is useful for fast iteration; the runtime_id scan is the authoritative source if the Vec ever drifts).

At reload time, `registry_reload` performs:
1. Scan `HOOK_SLOTS[0..256]` for entries where `SLOT_VALID[i] && HOOK_SLOTS[i].runtime_id == current.runtime_id`.
2. Call `remove_hook(HookHandle::from_raw(i))` on each — invokes the existing 5-step teardown (api.rs:200-217) that restores game-code bytes and frees thunks.

### Watcher

Single dedicated OS thread spawned from `DllMain` after the existing init flow. Owns the polling loop:

```
loop {
    sleep FROG_WATCHER_INTERVAL_MS (default 500)
    if STOPPING.load() { break }

    let cur = stat("<game_dir>/scripts/active.wasm").ok()
    match (last_seen, cur):
        (None, Some(meta)) | (Some(prev), Some(meta)) if changed:
            // settle: act only when mtime stable for one tick
            if not settled: last_seen = Some(meta); continue
            let bytes = read(path).unwrap_or_else(continue)
            if wasmi::Module::new(&engine, &bytes).is_err():
                log("hot-reload: parse failed; leaving current runtime alone")
                last_seen = Some(meta); continue
            publish_reload(bytes)
            wait_for_consume_or_fallback()
        (Some(_), None):
            publish_unload()
            wait_for_consume_or_fallback()
        _: nothing
    last_seen = cur
}
```

`wait_for_consume_or_fallback` polls `RELOAD_PENDING.is_none()` every 50ms for up to `FROG_WATCHER_FALLBACK_MS` (default 1000). If consumed, done. If timeout, the watcher acquires the registry lock and calls `registry_reload` directly. Per [[hooks-are-the-sync-primitive]] this fallback is safe: scripts that haven't fired a hook in 1s are either hookless (no race possible) or rarely-firing (the agent-thread reload races the same low-frequency events the script was racing anyway).

**Atomicity:** the watcher's `stat → settle-check → read → parse → publish` sequence is non-atomic per-step, but each step is idempotent — re-running the loop iteration after any failure converges. The parse-before-swap check ensures a broken file (mid-recompile, corrupt) never triggers a teardown; the previous runtime stays intact.

**Tuning knobs** (operator-side overrides, per [[diagnostic-env-gates-stay]]):
- `FROG_WATCHER_INTERVAL_MS` (default 500) — poll interval
- `FROG_WATCHER_FALLBACK_MS` (default 1000) — wait before fallback

### `dispatch_rust` piggyback

Modified `dispatch_rust` (incorporates all audit fixes from 2026-05-31):

```rust
pub extern "system" fn dispatch_rust(method_id: u64, regs: *mut RegArgs) {
    let regs = unsafe { &mut *regs };

    // Scope-block: ctx lives only while we need it. After this block,
    // no live reference to HookCtx remains — safe to unpublish.
    {
        let ctx = match ctx_for(method_id) {
            Some(c) => c,
            None => { regs.ret_int = 0; regs.ret_float = 0.0; return; }
        };
        if try_enter_reentry(method_id) {
            unsafe { call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs); }
            return;  // outer frame clears reentry
        }
        let args = match regargs_to_args(ctx.method, regs) {
            Ok(a) => a,
            Err(_) => {
                unsafe { call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs); }
                clear_reentry(method_id);
                return;
            }
        };
        let regs_ptr = regs as *mut RegArgs;
        super::api::with_current_context(ctx, regs_ptr, &args, |hr| {
            /* existing closure body — pack return, transparent observer, etc. */
        });
    }  // ctx dropped here — no live reference

    clear_reentry(method_id);

    // PIGGYBACK: per [[hooks-are-the-sync-primitive]], we're on game thread
    // with the game frozen by the inline-detour call stack. Safe to mutate
    // the registry (drop old runtime, spawn new) here because:
    //   - ctx is out of scope (no dangling reference)
    //   - reentry is cleared (a re-firing won't read the soon-stale slot)
    //   - SLOT_VALID release-stores by unpublish prevent fresh ctx_for hits
    //
    // Single-game-thread assumption documented: if a hooked method fires
    // from multiple OS threads, the unpublish-while-other-thread-mid-read
    // race surfaces. Mitigation deferred to a separate epoch-counter brick.
    if let Some(bytes) = crate::runtime::host::take_reload_pending() {
        crate::runtime::host::registry_reload(&bytes);
    }
}
```

### `DllMain` DETACH extension

```rust
DLL_PROCESS_DETACH => {
    unsafe {
        crate::runtime::host::watcher_stop();         // sets STOPPING flag
        crate::runtime::host::registry_unhook_all();  // restores all patches
        crate::protocol::remove_packet_hooks();        // existing
    }
}
```

Journal revert is INTENTIONALLY skipped on DLL_DETACH — the game process is terminating; reverting game state about to vanish is wasted work. What matters is that no game-code patches survive into the next session (in case the game has a longer shutdown sequence that calls hooked methods after our DLL unloads).

### Filesystem behavior

**Watched input:** `<game_dir>/scripts/active.wasm`

| Watcher observation | Orchestrator action |
|---|---|
| File appears, no current runtime | Parse → spawn |
| File mtime changes, current runtime exists | Parse → revert journal → unhook → drop old → spawn new |
| File disappears, current runtime exists | Revert journal → unhook → drop; no respawn |
| File mid-write (mtime moved within last poll-tick) | Wait |

**Emitted output:** `<game_dir>/scripts/.state.json` — atomic temp-rename write on every state change + 5s heartbeat.

```json
{
  "version": 1,
  "ts": "2026-05-31T14:32:11Z",
  "runtimes": [
    {
      "id": 1,
      "source_path": "...scripts/active.wasm",
      "source_hash": "sha256:ab12...",
      "hooks_installed": 3,
      "journal_addresses": 17,
      "loaded_at": "2026-05-31T14:30:45Z"
    }
  ]
}
```

`write_granted` is NOT in the schema (per locked decision #6, writes are unconditional).

**Logs:** unchanged — agent log continues to `<output_dir>/agent.log` via existing `paths::log`.

**Startup:** agent creates `scripts/` directory if absent (`std::fs::create_dir_all(output_path("scripts"))`).

## Registry API (the frontend-facing Rust surface)

```rust
pub fn spawn(bytes: &[u8]) -> Result<RuntimeId, SpawnError>
pub fn kill(id: RuntimeId) -> Result<(), KillError>   // triggers revert+unhook+drop
pub fn list() -> Vec<RuntimeInfo>                      // populates .state.json
pub fn current_mut() -> Option<MutexGuard<ParkedRuntime>>  // for hook dispatcher
```

Today the watcher and dispatcher are the only callers. Tomorrow's frontend (separate spec) drives the same API via filesystem operations on `active.wasm`. The API shape is intentionally generic over single-vs-multi runtime — `spawn` returns an id; `kill` takes one; future Multi just lets `list()` return >1 entries.

## Out of scope

- **No commands files, `.stop` sentinel, RPC-over-files.** Two operations only: write `active.wasm` (spawn/reload), delete `active.wasm` (kill).
- **No socket transport.** Filesystem-only. Socket is the explicit successor brick if polling lag becomes painful for the frontend.
- **No multi-runtime concurrency.** Replace only. Multi is the explicit future extension via Vec-of-1 prefigure.
- **No `.dll` runtime support.** Per [[frog-product-surface]] that's a separate future direction.
- **No script-side "I'm done, unload me" hook.** A script that wants to terminate can simply not install hooks and let `frog_main` return. The agent does NOT auto-revert on `frog_main` return (banked open question in [[script-lifetime-revert-boundary]]); revert happens at watcher-driven stop or DLL_DETACH (skipped per above).
- **No frontend.** B-6a is backend only. Any UI that consumes `.state.json` is a separate frontend spec.
- **No BYOL demo, no stress test.** Those are B-6b and B-6c respectively.

## Testing strategy

**Layer 1 — agent-core unit tests (Linux-native):**
- `registry_spawn_kill_round_trip`
- `registry_kill_invokes_journal_revert_with_correct_addresses`
- `journal_first_touch_only` (multiple writes to same address; only first original stored)
- `journal_revert_handles_overlapping_writes`
- `hook_ownership_scan_finds_only_runtime_id_matches`

Decision deferred to plan-time: which crate hosts the journal/registry types depends on orphan-rule analysis. Likely agent crate with `#[cfg(test)]` cfg-lift since `ParkedRuntime` holds `wasmi::Store` (agent-only).

**Layer 2 — integration check (Linux-native, no game):**
A test that spins up wasmi in-process with mocked HostState + mocked memory backend; exercises the host-fn surface; calls registry API directly to verify reload-as-orchestrator.

**Layer 3 — live game verification (deferred to B-6c):**
The dispatch_rust piggyback, the watcher, and journal revert under real game state all require a live game session. B-6a's acceptance is Layer 1 + Layer 2 green + a single manual smoke test on PW.

## Acceptance criteria

1. `cargo test -p agent-core` passes (Layer 1 if types live there).
2. `cargo test -p agent` (relevant suite) passes (Layer 1 if types live in agent crate; Layer 2 always).
3. `cargo build --target x86_64-pc-windows-gnu --release` clean — zero new warnings vs. pre-B-6a baseline (per [[deploy-setup]] critical warning).
4. Live smoke test on Pixel Worlds:
   - Drop wasm at `<game_dir>/scripts/active.wasm` → script runs, visible in agent.log
   - Replace with different wasm → old script stops cleanly (journal reverts visible in game state), new script starts
   - Delete the file → runtime stops, game state reverts
5. No regression in existing hook fire paths.
6. `FROG_WASM` env var removed from code; existing usage migrated to `scripts/active.wasm`.
7. `FROG_WASM_WRITE` env var removed; `mem.write` / `mem.write_if` always registered.

## Risks and non-risks

**Non-risks:**
- Watcher mutex contention with game thread is bounded by `try_lock` → transparent observer fallback. Same pattern as today's `PARKED` access.
- Rename of `PARKED` → `REGISTRY` is purely lexical; compiler enforces full migration.

**Real risks:**
- **Single-game-thread assumption.** If a hooked method ever fires from multiple OS threads (worker, network, async loader), the unpublish-while-mid-read race surfaces. Mitigation: documented; epoch-based fix is a separate brick. Probability: low for typical Unity games; verify on Pixel Worlds.
- **First reload after a long-running script may have a large journal revert latency.** A script that wrote millions of addresses takes proportional time to revert. Mitigation: actors use `write_permanent` for changes they don't want journaled. Probability: low for typical scripts.
- **Migration from `FROG_WASM`.** Existing Steam launch args break until updated. Mitigation: documented in release notes; one-line user fix. Probability: certain (every existing user); blast radius: small.

## Banked memories supporting this spec

- [[script-lifetime-revert-boundary]] — write journal + revert-on-stop principle
- [[wild-west-platform-philosophy]] — `write_granted` removal rationale; filesystem-only API
- [[scripter-vs-modder-experience]] — the fast+stable+rewarding requirement
- [[hooks-are-the-sync-primitive]] — dispatch_rust piggyback (saves the player-loop discovery pre-brick)
- [[bedrock-before-capability]] — sequencing (B-6a IS the bedrock for B-6b/c)
- [[frog-product-surface]] — FDK naming, .dll escape hatch, Frog Loader variant context
- [[imgui-frontend-process-not-wasm]] — frontend architecture (separate process)
- [[diagnostic-env-gates-stay]] — env vars NOT touched by this brick
- [[deploy-setup]] — Windows cross-compile requirement; `./deploy.sh` interaction
- [[codebase-audit-findings]] — existing test discipline and warning baseline

## Note on the earlier pseudo-spec

This document supersedes `2026-05-31-b6a-hot-reload-pseudo-spec.md`. The pseudo was written when we believed a player-loop discovery pre-brick was needed; the subsequent audit ([[hooks-are-the-sync-primitive]]) collapsed that need. The pseudo can be deleted.
