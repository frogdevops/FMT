# WASM Runtime (Spec 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Human commits their own work:** treat every `git commit` step as a *checkpoint* — hand the diff back to the human to commit; do **not** auto-commit.

**Goal:** Embed a `wasmi` WASM runtime in the agent that can load and run one sandboxed, fuel-limited module which calls a single `log` host function — the "hello world" that proves the playpen.

**Architecture:** The engine logic lives in `agent-core` (pure Rust, host-testable with no game). `run_wasm(bytes) -> Result<Vec<String>, WasmError>` parses a module, runs it under a fuel cap, exposes one host import `env.log(ptr,len)` that reads the module's own linear memory bounds-safely and collects messages, then returns them. The Windows `agent` crate is a thin wirer: if `FROG_WASM` points at a file, it reads it and calls `run_wasm`, logging each line.

**Tech Stack:** Rust; `wasmi` 0.32 (pure-Rust WASM interpreter, cross-compiles to `x86_64-pc-windows-gnu`); `wat` (dev-dependency, compiles WAT fixtures to bytes in tests).

**Scope:** ONLY the bare runtime + `log`. The `mem`/`il2cpp`/`proto` read/write APIs, the event model, the game-frame hook, panels, and frontend wiring are later specs (Spec 2+).

---

### Task 1: Add dependencies + module skeleton

**Files:**
- Modify: `crates/agent-core/Cargo.toml`
- Create: `crates/agent-core/src/wasm.rs`
- Modify: `crates/agent-core/src/lib.rs`

- [ ] **Step 1: Add the dependencies**

In `crates/agent-core/Cargo.toml`, under `[dependencies]` add:
```toml
wasmi = "0.32"
```
Add a `[dev-dependencies]` section (or extend it) with:
```toml
[dev-dependencies]
wat = "1"
```

- [ ] **Step 2: Create the module skeleton**

Create `crates/agent-core/src/wasm.rs`:
```rust
//! Embedded WASM runtime (Spec 1: bare playpen).
//!
//! Runs one sandboxed, fuel-limited module that may call a single host import
//! `env.log(ptr, len)`. The module's linear memory is its own sealed arena; the
//! `log` reader is bounds-checked, so a bad pointer yields an empty string rather
//! than a panic. Pure / host-testable: no FFI, no OS, no game.

use wasmi::{Caller, Config, Engine, Linker, Module, Store};

/// Instruction budget for a single `run_wasm` invocation. Generous for a
/// hello-world; small enough that an infinite loop traps quickly instead of
/// hanging. (Spec 2 will make this context-dependent.)
const DEFAULT_FUEL: u64 = 1_000_000;

/// Why a module failed to run. Never panics; every failure is one of these.
#[derive(Debug)]
pub enum WasmError {
    /// Module bytes did not parse as valid WASM.
    Parse(String),
    /// Engine/linker/instantiation failure (incl. fuel setup, missing `memory`).
    Instantiate(String),
    /// The module has no `frog_main` export to call.
    NoEntry,
    /// Runtime trap during execution (incl. fuel exhaustion).
    Trap(String),
}

/// Per-run host state: collects whatever the module logged.
struct HostState {
    logs: Vec<String>,
}

/// Run one WASM module to completion. Returns the lines it logged via `env.log`,
/// or a `WasmError`. Sandboxed + fuel-limited; cannot affect anything outside the
/// module except by appending to the returned log list.
pub fn run_wasm(wasm_bytes: &[u8]) -> Result<Vec<String>, WasmError> {
    // Implemented in Task 2.
    let _ = wasm_bytes;
    Err(WasmError::NoEntry)
}
```

- [ ] **Step 3: Register the module**

In `crates/agent-core/src/lib.rs`, add:
```rust
pub mod wasm;
```

- [ ] **Step 4: Verify it builds (host + cross-compile)**

Run: `cargo build -p agent-core`
Expected: compiles (a `dead_code`/unused warning for `HostState`/consts is fine at this stage).

Run: `cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -3`
Expected: compiles — confirms `wasmi` cross-compiles to the Windows target.

- [ ] **Step 5: Commit (checkpoint — hand diff to human)**

```bash
git add crates/agent-core/Cargo.toml crates/agent-core/src/wasm.rs crates/agent-core/src/lib.rs
git commit -m "feat(agent-core): add wasmi dependency + wasm module skeleton"
```

---

### Task 2: The runtime — run a module, the `log` import, fuel, bounds-safety

**Files:**
- Modify: `crates/agent-core/src/wasm.rs`

- [ ] **Step 1: Write the failing tests (the full Spec-1 contract)**

Append to `crates/agent-core/src/wasm.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("WAT should compile")
    }

    /// frog_main logs a constant string from its linear memory.
    #[test]
    fn hello_world_module_logs() {
        let bytes = wasm(
            r#"
            (module
              (import "env" "log" (func $log (param i32 i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "hello from wasm")
              (func (export "frog_main")
                i32.const 0
                i32.const 15
                call $log))
            "#,
        );
        let logs = run_wasm(&bytes).expect("should run");
        assert_eq!(logs, vec!["hello from wasm".to_string()]);
    }

    /// A module with no `frog_main` export is rejected, not run.
    #[test]
    fn missing_entry_is_no_entry() {
        let bytes = wasm(r#"(module (memory (export "memory") 1))"#);
        assert!(matches!(run_wasm(&bytes), Err(WasmError::NoEntry)));
    }

    /// An infinite loop exhausts fuel and traps — it does NOT hang.
    #[test]
    fn infinite_loop_traps_on_fuel() {
        let bytes = wasm(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "frog_main") (loop br 0)))
            "#,
        );
        assert!(matches!(run_wasm(&bytes), Err(WasmError::Trap(_))));
    }

    /// An out-of-range log pointer yields an empty string, never a panic/OOB.
    #[test]
    fn out_of_range_log_is_safe() {
        let bytes = wasm(
            r#"
            (module
              (import "env" "log" (func $log (param i32 i32)))
              (memory (export "memory") 1)
              (func (export "frog_main")
                i32.const 0
                i32.const 999999
                call $log))
            "#,
        );
        let logs = run_wasm(&bytes).expect("should run without panicking");
        assert_eq!(logs, vec![String::new()]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p agent-core wasm::tests`
Expected: FAIL — `hello_world_module_logs`/`infinite_loop_traps_on_fuel`/`out_of_range_log_is_safe` fail because `run_wasm` is the stub returning `NoEntry` (only `missing_entry_is_no_entry` passes by accident).

- [ ] **Step 3: Implement `run_wasm` + the `log` host function**

Replace the stub `run_wasm` body in `crates/agent-core/src/wasm.rs` with the real implementation, and add the `host_log` helper above it:
```rust
/// Host function bound as `env.log(ptr, len)`. Reads `len` bytes from the
/// module's own linear memory at `ptr`, bounds-checked, and records the string.
fn host_log(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) {
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return, // no exported memory -> nothing to read
    };
    let (ptr, len) = (ptr as usize, len as usize);
    let text = {
        let data = memory.data(&caller); // immutable borrow of caller
        match data.get(ptr..ptr.saturating_add(len)) {
            Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            None => String::new(), // out of range -> empty, never OOB
        }
    }; // immutable borrow ends here
    caller.data_mut().logs.push(text);
}

pub fn run_wasm(wasm_bytes: &[u8]) -> Result<Vec<String>, WasmError> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);

    let module =
        Module::new(&engine, wasm_bytes).map_err(|e| WasmError::Parse(e.to_string()))?;

    let mut store = Store::new(&engine, HostState { logs: Vec::new() });
    store
        .set_fuel(DEFAULT_FUEL)
        .map_err(|e| WasmError::Instantiate(e.to_string()))?;

    let mut linker = Linker::<HostState>::new(&engine);
    linker
        .func_wrap("env", "log", host_log)
        .map_err(|e| WasmError::Instantiate(e.to_string()))?;

    let instance = linker
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|e| WasmError::Instantiate(e.to_string()))?;

    let frog_main = instance
        .get_typed_func::<(), ()>(&store, "frog_main")
        .map_err(|_| WasmError::NoEntry)?;

    frog_main
        .call(&mut store, ())
        .map_err(|e| WasmError::Trap(e.to_string()))?;

    Ok(store.into_data().logs)
}
```
Delete the old stub body (the `let _ = wasm_bytes; Err(WasmError::NoEntry)`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p agent-core wasm::tests`
Expected: PASS — all 4 (`hello_world_module_logs`, `missing_entry_is_no_entry`, `infinite_loop_traps_on_fuel`, `out_of_range_log_is_safe`).

Run: `cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -3`
Expected: still compiles for Windows.

- [ ] **Step 5: Commit (checkpoint — hand diff to human)**

```bash
git add crates/agent-core/src/wasm.rs
git commit -m "feat(agent-core): wasmi runtime with log import, fuel cap, bounds-safe reads"
```

---

### Task 3: Wire the runtime into the agent (`FROG_WASM`)

**Files:**
- Create: `crates/agent/src/wasm_host.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/entry.rs`

- [ ] **Step 1: Create the agent-side wirer**

Create `crates/agent/src/wasm_host.rs`:
```rust
//! Spec-1 agent wiring: if the env var `FROG_WASM` points at a `.wasm` file,
//! read it and run it through the agent-core runtime, logging the result.
//! Absent the env var, does nothing — zero impact on the normal dump.

use crate::paths::log;

pub fn maybe_run_configured() {
    let path = match std::env::var("FROG_WASM") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    log(&format!("=== WASM: loading {} ===", path));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            log(&format!("  WASM load failed: {}", e));
            return;
        }
    };
    match agent_core::wasm::run_wasm(&bytes) {
        Ok(lines) => {
            log(&format!("  WASM ran ok, {} log line(s):", lines.len()));
            for l in &lines {
                log(&format!("    [wasm] {}", l));
            }
        }
        Err(e) => log(&format!("  WASM error: {:?}", e)),
    }
    log("=== end WASM ===");
}
```

- [ ] **Step 2: Register the module**

In `crates/agent/src/lib.rs`, alongside the other `#[cfg(target_os = "windows")] mod ...;` lines, add:
```rust
#[cfg(target_os = "windows")]
mod wasm_host;
```

- [ ] **Step 3: Call it from the worker**

In `crates/agent/src/entry.rs`, inside the `worker` function, AFTER the existing dump work completes (just before the worker returns / its final log line), add:
```rust
    crate::wasm_host::maybe_run_configured();
```
(Place it so it runs once after the internals dump; it no-ops unless `FROG_WASM` is set, so it never disturbs the normal path.)

- [ ] **Step 4: Verify the Windows build**

Run: `cargo build -p agent --target x86_64-pc-windows-gnu --release 2>&1 | grep -E "warning|error|Finished" | head`
Expected: `Finished`, no errors. (No new warnings introduced.)

- [ ] **Step 5: Commit (checkpoint — hand diff to human)**

```bash
git add crates/agent/src/wasm_host.rs crates/agent/src/lib.rs crates/agent/src/entry.rs
git commit -m "feat(agent): run a FROG_WASM module after the dump"
```

---

### Task 4: Integration gate — prove the playpen in the live game (manual)

No host test can prove a module runs *inside the game process*. This is the manual gate.

- [ ] **Step 1: Produce the hello-world `.wasm`**

Save this as `scratch/hello.wat`:
```wat
(module
  (import "env" "log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from wasm")
  (func (export "frog_main")
    i32.const 0
    i32.const 15
    call $log))
```
Convert it to bytes. Primary (wabt): `wat2wasm scratch/hello.wat -o scratch/hello.wasm`.
Fallback if `wat2wasm` isn't installed (wasm-tools): `wasm-tools parse scratch/hello.wat -o scratch/hello.wasm`.

- [ ] **Step 2: Build + stage the agent**

```bash
cargo build -p agent --target x86_64-pc-windows-gnu --release
GD="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds"
cp target/x86_64-pc-windows-gnu/release/agent.dll "$GD/agent.dll"
cp scratch/hello.wasm "$GD/hello.wasm"
rm -f "$GD/agent.log"
```

- [ ] **Step 3: Run with `FROG_WASM` set**

Launch Pixel Worlds via Steam with the launch option `FROG_WASM=hello.wasm %command%` (the agent runs in the game's working directory, so a bare filename resolves next to the staged `agent.dll`). Reach the menu, wait a few seconds, quit.

- [ ] **Step 4: Confirm the gate**

Read `"$GD/agent.log"`. Expected, near the end:
```
=== WASM: loading hello.wasm ===
  WASM ran ok, 1 log line(s):
    [wasm] hello from wasm
=== end WASM ===
```
If `hello from wasm` appears, the runtime is proven: a sandboxed WASM module ran inside the live game process and called back into the agent. If `WASM error: ...` appears, the message names the failure (Parse/Instantiate/NoEntry/Trap) — fix from there. The game must survive the run.

- [ ] **Step 5: Record the result (checkpoint)**

This task has no code commit; it's the proof. When `hello from wasm` is confirmed, Spec 1 is done.

---

## Self-Review

**Spec coverage:**
- "Embed `wasmi`, run a sandboxed module" → Tasks 1–2 (`run_wasm`, engine/linker/instantiate).
- "One `log` host import reading guest linear memory, bounds-checked" → Task 2 `host_log` + `out_of_range_log_is_safe` test.
- "Fuel limit; runaway script can't hang" → Task 2 `DEFAULT_FUEL` + `infinite_loop_traps_on_fuel` test.
- "Engine logic in agent-core, host-testable with no game" → Tasks 1–2 are pure agent-core unit tests (`wat` fixtures).
- "Agent wiring behind `FROG_WASM`, opt-in, no impact on normal dump" → Task 3.
- "Integration gate: hello-world on PW → message in agent.log" → Task 4.
- "WasmError enum covers parse / no memory / missing frog_main / trap" → Task 1 enum; exercised in Task 2 tests.
- Out-of-scope items (APIs, events, frame hook, panels) → correctly absent.

**Placeholder scan:** No TBD/TODO. Every code step shows complete code; every command shows expected output. The one version-sensitive call is `store.set_fuel` (wasmi **0.32**, pinned in Task 1); if a different `wasmi` is pinned, the fuel call may be `add_fuel` instead — noted here so it's not a silent gap.

**Type consistency:** `run_wasm(&[u8]) -> Result<Vec<String>, WasmError>` is identical in Task 1 (decl), Task 2 (impl), Task 3 (call). `WasmError` variants (`Parse`/`Instantiate`/`NoEntry`/`Trap`) defined in Task 1, produced in Task 2, matched/printed in Tasks 2–3. `HostState { logs: Vec<String> }` consistent between definition (Task 1) and use (Task 2 `host_log`/`into_data`). Entry export name `frog_main` consistent across the impl and all WAT fixtures and the integration `.wat`. Host import `env.log(i32,i32)` consistent in `func_wrap`, the WAT fixtures, and the integration `.wat`.
