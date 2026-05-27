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
