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

/// Called by hook_runtime::api::with_current_context to invoke a wasm handler
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
