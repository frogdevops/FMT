//! Spec-1 agent wiring: if the env var `FROG_WASM` points at a `.wasm` file,
//! read it and run it through the agent-core runtime, logging the result.
//! Absent the env var, does nothing — zero impact on the normal dump.

use crate::paths::log;
use crate::runtime::mem_host::HostState;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};

/// Owns the wasmi Store after `frog_main` returns, so post-frog_main hook
/// callbacks can `try_lock` it from the game thread. Park-on-return + Mutex
/// is the only sound model (Rust's aliasing rules forbid two `&mut Store`
/// simultaneously; reentrant locks would type-check but be UB).
pub struct ParkedStore {
    pub store: wasmi::Store<HostState>,
    pub instance: wasmi::Instance,
    /// Optional: scripts that don't use hooks (e.g. test_invoke.wasm) don't
    /// export a funcref table. `call_hook_handler` rejects with a clear error
    /// at dispatch time, not load time.
    pub funcref_table: Option<wasmi::Table>,
}

static PARKED: OnceLock<Mutex<Option<ParkedStore>>> = OnceLock::new();

/// Global accessor for the parked Store. Initialized lazily; the Mutex's
/// inner Option is `None` until `run_wasm_with_mem` parks the Store after
/// `frog_main` returns.
pub fn parked() -> &'static Mutex<Option<ParkedStore>> {
    PARKED.get_or_init(|| Mutex::new(None))
}

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
    let write_granted = std::env::var("FROG_WASM_WRITE").map(|v| !v.is_empty()).unwrap_or(false);
    log(&format!("  mem API: read=on, write={}", if write_granted { "GRANTED" } else { "off" }));
    match crate::runtime::mem_host::run_wasm_with_mem(&bytes, write_granted) {
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

    // Resolve funcref → Func. Table::get takes `impl AsContext` (shared ref).
    let table = parked.funcref_table.as_ref()
        .ok_or("no funcref table exported — script cannot dispatch hooks")?;
    let val = table
        .get(&parked.store, handler_funcref_idx as u32)
        .ok_or("funcref index out of range")?;
    let func = match val {
        wasmi::Val::FuncRef(fr) => fr.func().ok_or("funcref is null")?.clone(),
        _ => return Err("table entry is not a funcref"),
    };

    let typed = func.typed::<(), ()>(&parked.store)
        .map_err(|_| "handler signature is not () -> ()")?;

    typed.call(&mut parked.store, ())
        .map_err(|_| "wasm handler trapped")?;

    Ok(())
}
