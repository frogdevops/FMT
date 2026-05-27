//! WASM runtime with the external `mem.*` host API. Read trio always registered;
//! write pair only when `write_granted` (the gate — a non-granted module that
//! imports `mem.write` fails at instantiation). Results cross into the guest's own
//! linear memory via guest-provided buffers, bounds-checked exactly like `log`.

use wasmi::{Caller, Config, Engine, Linker, Module, Store};

use agent_core::mem_value::{status, ValType, Value};
use agent_core::wasm::WasmError;

use crate::external::api;

struct HostState {
    logs: Vec<String>,
}

fn read_guest(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let mem = caller.get_export("memory").and_then(|e| e.into_memory())?;
    let (ptr, len) = (ptr as usize, len as usize);
    mem.data(caller).get(ptr..ptr.checked_add(len)?).map(|s| s.to_vec())
}

fn write_guest(caller: &mut Caller<'_, HostState>, ptr: i32, bytes: &[u8]) -> bool {
    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return false,
    };
    let ptr = ptr as usize;
    let end = match ptr.checked_add(bytes.len()) { Some(e) => e, None => return false };
    let data = mem.data_mut(caller);
    match data.get_mut(ptr..end) {
        Some(dst) => { dst.copy_from_slice(bytes); true }
        None => false,
    }
}

fn host_log(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) {
    if let Some(bytes) = read_guest(&caller, ptr, len) {
        caller.data_mut().logs.push(String::from_utf8_lossy(&bytes).into_owned());
    }
}

fn host_read(mut caller: Caller<'_, HostState>, addr: i64, ty: i32, len: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let value = match api::read(addr as usize, ty, len.max(0) as usize) { Ok(v) => v, Err(c) => return c };
    let bytes = value.encode();
    if bytes.len() > out_cap.max(0) as usize { return status::ERR_BUF_TOO_SMALL; }
    if !write_guest(&mut caller, out_ptr, &bytes) { return status::ERR_BUF_TOO_SMALL; }
    bytes.len() as i32
}

fn host_scan(mut caller: Caller<'_, HostState>, pat_ptr: i32, pat_len: i32, out_ptr: i32, out_cap_count: i32) -> i32 {
    let pattern = match read_guest(&caller, pat_ptr, pat_len) { Some(p) => p, None => return status::ERR_BAD_TYPE };
    let hits = api::scan(&pattern, out_cap_count.max(0) as usize);
    let mut buf = Vec::with_capacity(hits.len() * 8);
    for a in &hits { buf.extend_from_slice(&(*a as u64).to_le_bytes()); }
    if !write_guest(&mut caller, out_ptr, &buf) { return status::ERR_BUF_TOO_SMALL; }
    hits.len() as i32
}

fn host_regions(mut caller: Caller<'_, HostState>, out_ptr: i32, out_cap_count: i32) -> i32 {
    let regs = api::regions();
    let take = regs.len().min(out_cap_count.max(0) as usize);
    let mut buf = Vec::with_capacity(take * 20);
    for (base, size, prot) in regs.iter().take(take) {
        buf.extend_from_slice(&(*base as u64).to_le_bytes());
        buf.extend_from_slice(&(*size as u64).to_le_bytes());
        buf.extend_from_slice(&prot.to_le_bytes());
    }
    if !write_guest(&mut caller, out_ptr, &buf) { return status::ERR_BUF_TOO_SMALL; }
    take as i32
}

fn host_write(caller: Caller<'_, HostState>, addr: i64, ty: i32, in_ptr: i32, in_len: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let bytes = match read_guest(&caller, in_ptr, in_len) { Some(b) => b, None => return status::ERR_BAD_TYPE };
    let value = match Value::decode(ty, &bytes) { Some(v) => v, None => return status::ERR_BAD_TYPE };
    match api::write(addr as usize, &value) { Ok(()) => status::OK, Err(c) => c }
}

fn host_write_if(caller: Caller<'_, HostState>, addr: i64, ty: i32, exp_ptr: i32, exp_len: i32, new_ptr: i32, new_len: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let exp_b = match read_guest(&caller, exp_ptr, exp_len) { Some(b) => b, None => return status::ERR_BAD_TYPE };
    let new_b = match read_guest(&caller, new_ptr, new_len) { Some(b) => b, None => return status::ERR_BAD_TYPE };
    let (exp, new) = match (Value::decode(ty, &exp_b), Value::decode(ty, &new_b)) {
        (Some(a), Some(b)) => (a, b), _ => return status::ERR_BAD_TYPE,
    };
    match api::write_if(addr as usize, &exp, &new) {
        Ok(true) => status::OK, Ok(false) => status::CHANGED, Err(c) => c,
    }
}

/// Run a module with the mem API. `write_granted` decides whether the write
/// imports exist at all (the gate). Returns the lines it logged.
pub fn run_wasm_with_mem(wasm_bytes: &[u8], write_granted: bool) -> Result<Vec<String>, WasmError> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm_bytes).map_err(|e| WasmError::Parse(e.to_string()))?;
    let mut store = Store::new(&engine, HostState { logs: Vec::new() });
    store.set_fuel(1_000_000).map_err(|e| WasmError::Instantiate(e.to_string()))?;

    let mut linker = Linker::<HostState>::new(&engine);
    linker.func_wrap("env", "log", host_log).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("mem", "read", host_read).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("mem", "scan", host_scan).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("mem", "regions", host_regions).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    if write_granted {
        linker.func_wrap("mem", "write", host_write).map_err(|e| WasmError::Instantiate(e.to_string()))?;
        linker.func_wrap("mem", "write_if", host_write_if).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    }

    let instance = linker
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|e| WasmError::Instantiate(e.to_string()))?;
    let frog_main = instance
        .get_typed_func::<(), ()>(&store, "frog_main")
        .map_err(|_| WasmError::NoEntry)?;
    frog_main.call(&mut store, ()).map_err(|e| WasmError::Trap(e.to_string()))?;
    Ok(store.into_data().logs)
}
