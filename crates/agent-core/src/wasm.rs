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
