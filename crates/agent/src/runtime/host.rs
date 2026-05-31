//! Agent-side wasm hook dispatcher. The watcher (runtime::watcher) is now the
//! sole loader for WASM scripts; this module only hosts the dispatch path
//! (`call_hook_handler`) and the contention counter.

use crate::paths::log;
use std::sync::atomic::{AtomicU64, Ordering};

/// Tracks Mutex-contention events on REGISTRY (silent-degradation signal:
/// when the game thread can't acquire the Store, the dispatcher falls
/// through to transparent observer instead of running the wasm handler).
/// One-shot log on first hit + every-1000 thereafter; process-lifetime
/// cumulative. Mirrors the IOCP CAP_HIT_COUNT pattern from B-2bc.
static WASM_CONTENDED: AtomicU64 = AtomicU64::new(0);

fn note_wasm_handler_contended() {
    let prev = WASM_CONTENDED.fetch_add(1, Ordering::Relaxed);
    if prev == 0 {
        log("⚠ WASM_HANDLER_CONTENDED — REGISTRY held; transparent observer fired");
    } else if (prev + 1) % 1000 == 0 {
        log(&format!("⚠ WASM_HANDLER_CONTENDED count={} (degraded handler dispatch)", prev + 1));
    }
}

/// Dispatch the hook handler at `handler_funcref_idx` in the wasm module's
/// funcref table. Called by `dispatch_rust` on the GAME thread; the runtime
/// was inserted into REGISTRY by `run_wasm_with_mem` on the agent worker
/// thread after `frog_main` returned.
///
/// SAFETY MODEL (do not "optimize" with ReentrantMutex — see B-3 spec):
/// Rust forbids two `&mut Store` simultaneously. We only ever hold one:
/// during frog_main, run_wasm_with_mem owns it (REGISTRY is empty → this fn
/// returns Err → transparent observer fires); after frog_main returns, the
/// Store moves into REGISTRY and the guard provides the unique `&mut`.
///
/// Returns Err on contention (transparent observer fires; logged once per
/// 1000), Err when REGISTRY is empty (frog_main still running, no script
/// loaded, or no hooks installed before exit), Err when funcref/sig is bad
/// (script error), or Err if the wasm handler trapped.
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
