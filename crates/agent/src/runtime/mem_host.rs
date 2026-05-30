//! WASM runtime with the external `mem.*` host API. Read trio always registered;
//! write pair only when `write_granted` (the gate — a non-granted module that
//! imports `mem.write` fails at instantiation). Results cross into the guest's own
//! linear memory via guest-provided buffers, bounds-checked exactly like `log`.

use wasmi::{Caller, Config, Engine, Linker, Module, Store};

use agent_core::mem_value::{status, ValType, Value};
use agent_core::wasm::WasmError;

use crate::external::api;

pub struct HostState {
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

fn host_find_class(caller: Caller<'_, HostState>, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    let name = String::from_utf8_lossy(&name);
    crate::internals::api::find_class(&name) as i64
}

fn host_field_info(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return -1 };
    let name = String::from_utf8_lossy(&name);
    match crate::internals::api::field_info(klass as u64, &name) {
        Some((offset, vt)) => ((vt as u8 as i64) << 32) | (offset as i64),
        None => -1,
    }
}

fn host_get_field(mut caller: Caller<'_, HostState>, instance: i64, klass: i64, name_ptr: i32, name_len: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let name = match read_guest(&caller, name_ptr, name_len) {
        Some(b) => b,
        None => return agent_core::mem_value::status::ERR_BAD_TYPE,
    };
    let name = String::from_utf8_lossy(&name).into_owned();
    let value = match crate::internals::api::get_field(instance as u64, klass as u64, &name) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let bytes = value.encode();
    if bytes.len() > out_cap.max(0) as usize {
        return agent_core::mem_value::status::ERR_BUF_TOO_SMALL;
    }
    if !write_guest(&mut caller, out_ptr, &bytes) {
        return agent_core::mem_value::status::ERR_BUF_TOO_SMALL;
    }
    bytes.len() as i32
}

fn host_klass_of(_caller: Caller<'_, HostState>, instance: i64) -> i64 {
    crate::internals::api::klass_of(instance as u64) as i64
}

fn host_static_field(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    crate::internals::api::static_field(klass as u64, &String::from_utf8_lossy(&name)) as i64
}

fn host_find_method(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32, argc: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    crate::internals::api::find_method(klass as u64, &String::from_utf8_lossy(&name), argc.max(0) as u32) as i64
}

fn host_install_hook(
    _caller: wasmi::Caller<'_, HostState>,
    method_ptr: i64,
    handler_funcref_table_idx: i32,
) -> i64 {
    use agent_core::spine::MethodPtr;
    let method = MethodPtr::from_raw(method_ptr as u64);
    match crate::internals::hook_runtime::api::install_hook(method, handler_funcref_table_idx as u64) {
        Ok(handle) => handle.as_u64() as i64,
        Err(e)     => i32::from(e) as i64,   // negative codes -200..-205 sign-extend
    }
}

fn host_remove_hook(
    _caller: wasmi::Caller<'_, HostState>,
    handle: i64,
) -> i32 {
    use agent_core::spine::HookHandle;
    match crate::internals::hook_runtime::api::remove_hook(HookHandle::from_raw(handle as u64)) {
        Ok(())  => 0,
        Err(e)  => i32::from(e),
    }
}

fn host_hook_arg(mut caller: wasmi::Caller<'_, HostState>, arg_idx: i32, out_buf: i32, out_cap: i32) -> i32 {
    match crate::internals::hook_runtime::api::hook_arg_read(arg_idx as usize) {
        Ok(bytes) => {
            if bytes.len() > out_cap as usize { return -4; }
            let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) { Some(m) => m, None => return -3 };
            if mem.write(&mut caller, out_buf as usize, &bytes).is_err() { return -1; }
            bytes.len() as i32
        }
        Err(e) => i32::from(e),
    }
}

fn host_hook_set_arg(mut caller: wasmi::Caller<'_, HostState>, arg_idx: i32, val_buf: i32, val_len: i32) -> i32 {
    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) { Some(m) => m, None => return -3 };
    let mut buf = vec![0u8; val_len as usize];
    if mem.read(&caller, val_buf as usize, &mut buf).is_err() { return -1; }
    match crate::internals::hook_runtime::api::hook_arg_write(arg_idx as usize, &buf) {
        Ok(()) => 0,
        Err(e) => i32::from(e),
    }
}

fn host_hook_this(_caller: wasmi::Caller<'_, HostState>) -> i64 {
    crate::internals::hook_runtime::api::hook_this_get() as i64
}

fn host_call_original(mut caller: wasmi::Caller<'_, HostState>, out_buf: i32, out_cap: i32) -> i32 {
    match crate::internals::hook_runtime::api::call_original_now() {
        Ok(bytes) => {
            if bytes.len() > out_cap as usize { return -4; }
            let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) { Some(m) => m, None => return -3 };
            if mem.write(&mut caller, out_buf as usize, &bytes).is_err() { return -1; }
            0
        }
        Err(e) => i32::from(e),
    }
}

fn host_hook_set_return(mut caller: wasmi::Caller<'_, HostState>, val_buf: i32, val_len: i32) -> i32 {
    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) { Some(m) => m, None => return -3 };
    let mut buf = vec![0u8; val_len as usize];
    if mem.read(&caller, val_buf as usize, &mut buf).is_err() { return -1; }
    match crate::internals::hook_runtime::api::hook_set_return(&buf) {
        Ok(()) => 0,
        Err(e) => i32::from(e),
    }
}

fn host_invoke(mut caller: Caller<'_, HostState>, method_ptr: i64, instance_ptr: i64, args_buf: i32, args_len: i32, out_buf: i32, out_cap: i32) -> i32 {
    use agent_core::spine::{Instance, InvokeArg, MethodPtr};

    let method = MethodPtr::from_raw(method_ptr as u64);
    let instance = if instance_ptr == 0 { None } else { Some(Instance::from_raw(instance_ptr as u64)) };

    // Read packed args from wasm memory.
    let buf = match read_guest(&caller, args_buf, args_len) {
        Some(b) => b,
        None => return -1,  // ERR_UNREADABLE
    };

    // Decode args: first u32 is arg_count, then per-arg [tag, payload].
    if buf.len() < 4 { return -3; }  // ERR_BAD_TYPE — malformed buffer
    let arg_count = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let mut args = Vec::with_capacity(arg_count);
    let mut cursor = 4usize;
    for _ in 0..arg_count {
        match InvokeArg::decode(&buf[cursor..]) {
            Some((a, consumed)) => { args.push(a); cursor += consumed; }
            None => return -104, // MarshalFailed
        }
    }

    // Call the typed core.
    match crate::internals::api::invoke_method_t(method, instance, &args) {
        Ok(ret_val) => {
            let encoded = ret_val.encode();
            if encoded.len() > out_cap.max(0) as usize {
                return -4;  // ERR_BUF_TOO_SMALL
            }
            if !write_guest(&mut caller, out_buf, &encoded) {
                return -1;  // ERR_UNREADABLE (write failed)
            }
            0  // OK
        }
        Err(e) => i32::from(e),
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
    linker.func_wrap("il2cpp", "find_class", host_find_class).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "field_info", host_field_info).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "get_field", host_get_field).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "klass_of", host_klass_of).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "static_field", host_static_field).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "find_method", host_find_method).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "invoke", host_invoke).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "install_hook", host_install_hook).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "remove_hook", host_remove_hook).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "hook_arg",        host_hook_arg).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "hook_set_arg",    host_hook_set_arg).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "hook_this",       host_hook_this).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "call_original",   host_call_original).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "hook_set_return", host_hook_set_return).map_err(|e| WasmError::Instantiate(e.to_string()))?;
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

    // B-3 Section 1+2: park the Store + instance + funcref table so post-
    // frog_main hook callbacks (fired from game thread) can try_lock and
    // invoke the registered handler funcref via wasmi typed call. Clone
    // logs out FIRST (into_data() consumes; we need to keep Store alive).
    let logs = store.data().logs.clone();

    // B-3 Section 2: funcref table is OPTIONAL. Scripts that use hooks
    // (install_hook) must export a funcref table under the LLVM-compatible
    // name "__indirect_function_table". Scripts that don't use hooks
    // (e.g. test_invoke.wasm) don't need one — the field is None and
    // call_hook_handler returns a clear error at dispatch time.
    let funcref_table = instance
        .get_table(&store, "__indirect_function_table");

    *crate::runtime::host::parked().lock().unwrap() = Some(
        crate::runtime::host::ParkedStore { store, instance, funcref_table }
    );

    Ok(logs)
}
