# External Brick — `mem` Read/Write API — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.
>
> **COMMITS ARE THE USER'S.** Do NOT run `git commit`/`git push`. Each task ends at a checkpoint — stop and hand the diff back for the user to commit, then continue.

**Goal:** Give WASM scripts a minimal, type-driven, full-control memory API — `mem.read/scan/regions` (always) and `mem.write/write_if` (when granted) — where a bad address returns an error, never a crash.

**Architecture:** A pure typed value model in `agent-core` (host-tested) + the 5 ops over raw memory in `external/` (reads validate against a background-refreshed region cache via binary search = near-zero; writes use the proven guarded write) + the `mem.*` WASM host functions in `runtime/` (the agent crate gains a `wasmi` dep to build the Linker; write imports are registered only for granted modules = the gate).

**Tech Stack:** Rust, `wasmi` (interpreter, now also a direct agent dep), `windows-sys` (VirtualQuery/VirtualProtect), cross-compiled to `x86_64-pc-windows-gnu`. Pure logic host-tested on Linux; FFI verified on Pixel Worlds.

**Verify commands:** host tests `cargo test -p agent-core`; Windows compile `cargo check -p agent --target x86_64-pc-windows-gnu`; deploy `./deploy.sh`.

---

### Task 1: `agent_core::mem_value` — typed value model (pure, TDD)

**Files:**
- Create: `crates/agent-core/src/mem_value.rs`
- Modify: `crates/agent-core/src/lib.rs` (add `pub mod mem_value;`)

- [ ] **Step 1: Write the failing tests** — create `mem_value.rs` with impl + tests (full code in Step 3); tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_widths_are_correct() {
        assert_eq!(ValType::U8.fixed_width(), Some(1));
        assert_eq!(ValType::U32.fixed_width(), Some(4));
        assert_eq!(ValType::F64.fixed_width(), Some(8));
        assert_eq!(ValType::Bytes.fixed_width(), None);
        assert_eq!(ValType::Cstr.fixed_width(), None);
    }

    #[test]
    fn from_tag_round_trips_and_rejects_garbage() {
        assert_eq!(ValType::from_tag(2), Some(ValType::U32));
        assert_eq!(ValType::from_tag(11), Some(ValType::Cstr));
        assert_eq!(ValType::from_tag(99), None);
    }

    #[test]
    fn fixed_value_encode_decode_round_trips() {
        let v = Value::U32(0xDEADBEEF);
        let bytes = v.encode();
        assert_eq!(bytes, 0xDEADBEEFu32.to_le_bytes());
        assert_eq!(Value::decode(ValType::U32, &bytes), Some(Value::U32(0xDEADBEEF)));
    }

    #[test]
    fn float_round_trips() {
        let v = Value::F32(1.5);
        assert_eq!(Value::decode(ValType::F32, &v.encode()), Some(Value::F32(1.5)));
    }

    #[test]
    fn decode_rejects_wrong_length_for_fixed_type() {
        assert_eq!(Value::decode(ValType::U32, &[1, 2, 3]), None); // 3 bytes for a 4-byte type
    }

    #[test]
    fn bytes_and_cstr_decode() {
        assert_eq!(Value::decode(ValType::Bytes, &[1, 2, 3]), Some(Value::Bytes(vec![1, 2, 3])));
        assert_eq!(Value::decode(ValType::Cstr, b"hi\0junk"), Some(Value::Cstr("hi".into())));
    }

    #[test]
    fn val_type_reports_back() {
        assert_eq!(Value::I64(-1).val_type(), ValType::I64);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p agent-core mem_value` → FAIL (types undefined).

- [ ] **Step 3: Implement `mem_value.rs`** (above the tests):

```rust
//! Pure typed value model for the external `mem` API. The closed type set is the
//! anti-garbage gate: every read/write declares its type. Host-testable; the wire
//! encoding here is shared by the WASM host and the future frontend transport.

/// Closed set of value types. `u8` tag for the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValType {
    U8 = 0, U16 = 1, U32 = 2, U64 = 3,
    I8 = 4, I16 = 5, I32 = 6, I64 = 7,
    F32 = 8, F64 = 9,
    Bytes = 10,
    Cstr = 11,
}

impl ValType {
    /// Fixed byte width, or None for variable-length (Bytes/Cstr).
    pub fn fixed_width(self) -> Option<usize> {
        Some(match self {
            ValType::U8 | ValType::I8 => 1,
            ValType::U16 | ValType::I16 => 2,
            ValType::U32 | ValType::I32 | ValType::F32 => 4,
            ValType::U64 | ValType::I64 | ValType::F64 => 8,
            ValType::Bytes | ValType::Cstr => return None,
        })
    }

    pub fn from_tag(tag: u8) -> Option<ValType> {
        Some(match tag {
            0 => ValType::U8, 1 => ValType::U16, 2 => ValType::U32, 3 => ValType::U64,
            4 => ValType::I8, 5 => ValType::I16, 6 => ValType::I32, 7 => ValType::I64,
            8 => ValType::F32, 9 => ValType::F64,
            10 => ValType::Bytes, 11 => ValType::Cstr,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U8(u8), U16(u16), U32(u32), U64(u64),
    I8(i8), I16(i16), I32(i32), I64(i64),
    F32(f32), F64(f64),
    Bytes(Vec<u8>), Cstr(String),
}

impl Value {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Value::U8(v) => v.to_le_bytes().to_vec(),
            Value::U16(v) => v.to_le_bytes().to_vec(),
            Value::U32(v) => v.to_le_bytes().to_vec(),
            Value::U64(v) => v.to_le_bytes().to_vec(),
            Value::I8(v) => v.to_le_bytes().to_vec(),
            Value::I16(v) => v.to_le_bytes().to_vec(),
            Value::I32(v) => v.to_le_bytes().to_vec(),
            Value::I64(v) => v.to_le_bytes().to_vec(),
            Value::F32(v) => v.to_le_bytes().to_vec(),
            Value::F64(v) => v.to_le_bytes().to_vec(),
            Value::Bytes(b) => b.clone(),
            Value::Cstr(s) => s.as_bytes().to_vec(),
        }
    }

    pub fn decode(ty: ValType, bytes: &[u8]) -> Option<Value> {
        if let Some(w) = ty.fixed_width() {
            if bytes.len() != w {
                return None;
            }
        }
        Some(match ty {
            ValType::U8 => Value::U8(bytes[0]),
            ValType::I8 => Value::I8(bytes[0] as i8),
            ValType::U16 => Value::U16(u16::from_le_bytes([bytes[0], bytes[1]])),
            ValType::I16 => Value::I16(i16::from_le_bytes([bytes[0], bytes[1]])),
            ValType::U32 => Value::U32(u32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::I32 => Value::I32(i32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::F32 => Value::F32(f32::from_le_bytes(bytes[..4].try_into().ok()?)),
            ValType::U64 => Value::U64(u64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::I64 => Value::I64(i64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::F64 => Value::F64(f64::from_le_bytes(bytes[..8].try_into().ok()?)),
            ValType::Bytes => Value::Bytes(bytes.to_vec()),
            ValType::Cstr => {
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                Value::Cstr(String::from_utf8_lossy(&bytes[..end]).into_owned())
            }
        })
    }

    pub fn val_type(&self) -> ValType {
        match self {
            Value::U8(_) => ValType::U8, Value::U16(_) => ValType::U16,
            Value::U32(_) => ValType::U32, Value::U64(_) => ValType::U64,
            Value::I8(_) => ValType::I8, Value::I16(_) => ValType::I16,
            Value::I32(_) => ValType::I32, Value::I64(_) => ValType::I64,
            Value::F32(_) => ValType::F32, Value::F64(_) => ValType::F64,
            Value::Bytes(_) => ValType::Bytes, Value::Cstr(_) => ValType::Cstr,
        }
    }
}

/// Status codes shared by the WASM host (and future frontend) ABI.
pub mod status {
    pub const OK: i32 = 0;
    pub const ERR_UNREADABLE: i32 = -1;
    pub const ERR_UNWRITABLE: i32 = -2;
    pub const ERR_BAD_TYPE: i32 = -3;
    pub const ERR_BUF_TOO_SMALL: i32 = -4;
    pub const ERR_DENIED: i32 = -5;
    pub const CHANGED: i32 = 1;
}
```

- [ ] **Step 4: Add the module** — in `crates/agent-core/src/lib.rs` add `pub mod mem_value;` (alphabetical, near `mem_write`).

- [ ] **Step 5: Run to verify pass** — `cargo test -p agent-core mem_value` → 7 passed; `cargo test -p agent-core` → 51 passed (44 + 7).

- [ ] **Step 6: Checkpoint** — hand diff to user (`feat: agent-core mem_value typed value model`).

---

### Task 2: `external::cache` — background-refreshed region cache (near-zero read validation)

**Files:**
- Create: `crates/agent/src/external/cache.rs`
- Modify: `crates/agent/src/external/mod.rs` (add `pub mod cache;`)

- [ ] **Step 1: Implement `cache.rs`**

```rust
//! Global region cache for near-zero read validation. A background thread
//! re-captures committed-readable regions every ~500 ms; `validate` does an
//! O(log n) binary search (no syscall) on the hot path and falls back to a single
//! live VirtualQuery on a cache miss (a freshly-allocated region).

use std::ffi::c_void;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
};

use crate::external::region_map::{is_readable, RegionMap, Tunables};

static REGIONS: OnceLock<RwLock<RegionMap>> = OnceLock::new();

fn regions() -> &'static RwLock<RegionMap> {
    REGIONS.get_or_init(|| RwLock::new(RegionMap::capture(Tunables::load().max_regions)))
}

/// Start the background refresher (idempotent-ish; call once from the worker).
pub fn start_refresher() {
    std::thread::spawn(|| {
        let max = Tunables::load().max_regions;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let fresh = RegionMap::capture(max);
            if let Ok(mut g) = regions().write() {
                *g = fresh;
            }
        }
    });
}

/// True if [addr, addr+len) is readable. Hot path: binary search the cache.
/// Miss: one live VirtualQuery (correct for new regions, rare).
pub fn validate_read(addr: usize, len: usize) -> bool {
    if let Ok(g) = regions().read() {
        if g.in_region(addr, len) {
            return true;
        }
    }
    live_readable(addr, len)
}

fn live_readable(addr: usize, len: usize) -> bool {
    let end = match addr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    unsafe {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let n = VirtualQuery(addr as *const c_void, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
        if n == 0 || mbi.State != MEM_COMMIT || !is_readable(mbi.Protect) {
            return false;
        }
        let base = mbi.BaseAddress as usize;
        addr >= base && end <= base.saturating_add(mbi.RegionSize)
    }
}

/// Snapshot of current regions (for the `regions()` op).
pub fn snapshot() -> Vec<(usize, usize)> {
    regions().read().map(|g| g.regions.clone()).unwrap_or_default()
}
```

- [ ] **Step 2: Export it** — in `crates/agent/src/external/mod.rs` add `pub mod cache;`.

- [ ] **Step 3: Verify compile** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`. (`RegionMap.regions` is `pub(crate)`, accessible within the crate.)

- [ ] **Step 4: Checkpoint** — hand diff to user (`feat: external region cache + background refresh`).

---

### Task 3: `external::scan` — streaming AOB search

**Files:**
- Modify: `crates/agent/src/external/scan.rs` (add `aob_scan`)

- [ ] **Step 1: Add `aob_scan` to `scan.rs`**

```rust
use crate::external::cache;

/// Find up to `max_hits` addresses where `pattern` occurs in committed-readable
/// memory. Streaming + bounded: walks cached regions, reads each region's bytes
/// once, scans with a first-byte skip. No global snapshot-then-search.
pub fn aob_scan(pattern: &[u8], max_hits: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    if pattern.is_empty() || max_hits == 0 {
        return hits;
    }
    let first = pattern[0];
    for (start, end) in cache::snapshot() {
        let len = end - start;
        // Bounds-checked read of the whole region via raw slice (region is committed+readable).
        let bytes = unsafe { std::slice::from_raw_parts(start as *const u8, len) };
        let mut i = 0;
        while i + pattern.len() <= len {
            // first-byte skip
            match bytes[i..len - pattern.len() + 1].iter().position(|&b| b == first) {
                Some(off) => {
                    let at = i + off;
                    if &bytes[at..at + pattern.len()] == pattern {
                        hits.push(start + at);
                        if hits.len() >= max_hits {
                            return hits;
                        }
                    }
                    i = at + 1;
                }
                None => break,
            }
        }
    }
    hits
}
```

- [ ] **Step 2: Verify compile** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`.

- [ ] **Step 3: Checkpoint** — hand diff to user (`feat: external streaming AOB scan`).

---

### Task 4: `external::api` — the 5 core ops

**Files:**
- Create: `crates/agent/src/external/api.rs`
- Modify: `crates/agent/src/external/mod.rs` (add `pub mod api;`)

- [ ] **Step 1: Implement `api.rs`**

```rust
//! The 5 external memory ops over raw process memory. Reads validate via the
//! near-zero region cache; writes use the proven guarded write. Returns typed
//! Values / negative status codes (see agent_core::mem_value::status).

use agent_core::mem_value::{status, ValType, Value};

use crate::external::cache;
use crate::external::scan::aob_scan;
use crate::external::write::guarded_write;

/// Read a typed value at `addr`. `len` is used for Bytes/Cstr; fixed types ignore it.
pub fn read(addr: usize, ty: ValType, len: usize) -> Result<Value, i32> {
    let n = ty.fixed_width().unwrap_or(len);
    if n == 0 {
        return Err(status::ERR_BAD_TYPE);
    }
    if !cache::validate_read(addr, n) {
        return Err(status::ERR_UNREADABLE);
    }
    let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, n) }.to_vec();
    Value::decode(ty, &bytes).ok_or(status::ERR_BAD_TYPE)
}

pub fn scan(pattern: &[u8], max_hits: usize) -> Vec<usize> {
    aob_scan(pattern, max_hits)
}

/// (base, size, protect) for each cached readable region.
pub fn regions() -> Vec<(usize, usize, u32)> {
    cache::snapshot().into_iter().map(|(s, e)| (s, e - s, 0u32)).collect()
}

pub fn write(addr: usize, value: &Value) -> Result<(), i32> {
    let bytes = value.encode();
    if bytes.is_empty() {
        return Err(status::ERR_BAD_TYPE);
    }
    unsafe { guarded_write(addr, &bytes) }.map_err(|_| status::ERR_UNWRITABLE)
}

/// Read-confirm-write: write `new` only if the current value equals `expected`.
/// Ok(true) = written; Ok(false) = current differed (CHANGED), not written.
pub fn write_if(addr: usize, expected: &Value, new: &Value) -> Result<bool, i32> {
    let ty = expected.val_type();
    let len = match ty.fixed_width() {
        Some(w) => w,
        None => expected.encode().len(),
    };
    let current = read(addr, ty, len)?;
    if &current != expected {
        return Ok(false);
    }
    write(addr, new)?;
    Ok(true)
}
```

- [ ] **Step 2: Export it** — in `external/mod.rs` add `pub mod api;`.

- [ ] **Step 3: Verify compile** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`.

- [ ] **Step 4: Checkpoint** — hand diff to user (`feat: external mem api (5 ops)`).

---

### Task 5: `mem.*` WASM host functions + write gate + wiring

**Files:**
- Modify: `crates/agent/Cargo.toml` (add `wasmi`)
- Create: `crates/agent/src/runtime/mem_host.rs`
- Modify: `crates/agent/src/runtime/mod.rs` (add `pub mod mem_host;`), `crates/agent/src/runtime/host.rs` (route to the mem-enabled runner), `crates/agent/src/entry.rs` (start the cache refresher; read the write grant)

- [ ] **Step 1: Add the dep** — in `crates/agent/Cargo.toml` under `[dependencies]` add: `wasmi = "0.32"`.

- [ ] **Step 2: Implement `runtime/mem_host.rs`** — the Linker with `env.log` + `mem.*`, gated:

```rust
//! WASM runtime with the external `mem.*` host API. Read trio always registered;
//! write pair registered only when `write_granted` (the gate — a non-granted
//! module that imports `mem.write` fails at instantiation). Results cross into the
//! guest's own linear memory via guest-provided buffers (bounds-checked).

use wasmi::{Caller, Config, Engine, Linker, Module, Store};

use agent_core::mem_value::{status, ValType, Value};
use agent_core::wasm::WasmError;

use crate::external::api;

struct HostState {
    logs: Vec<String>,
}

/// Bounds-checked view of the guest's exported linear memory.
fn guest_mem<'a>(caller: &'a Caller<'_, HostState>) -> Option<wasmi::Memory> {
    caller.get_export("memory").and_then(|e| e.into_memory())
}

fn read_guest(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let mem = guest_mem(caller)?;
    let (ptr, len) = (ptr as usize, len as usize);
    mem.data(caller).get(ptr..ptr.checked_add(len)?).map(|s| s.to_vec())
}

fn write_guest(caller: &mut Caller<'_, HostState>, ptr: i32, bytes: &[u8]) -> bool {
    let mem = match guest_mem(caller) { Some(m) => m, None => return false };
    let ptr = ptr as usize;
    let data = mem.data_mut(caller);
    match data.get_mut(ptr..ptr.checked_add(bytes.len()).unwrap_or(usize::MAX)) {
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
    let cap = out_cap_count.max(0) as usize;
    let take = regs.len().min(cap);
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
    let map_err = |e: wasmi::errors::LinkerError| WasmError::Instantiate(e.to_string());
    linker.func_wrap("env", "log", host_log).map_err(map_err)?;
    linker.func_wrap("mem", "read", host_read).map_err(map_err)?;
    linker.func_wrap("mem", "scan", host_scan).map_err(map_err)?;
    linker.func_wrap("mem", "regions", host_regions).map_err(map_err)?;
    if write_granted {
        linker.func_wrap("mem", "write", host_write).map_err(map_err)?;
        linker.func_wrap("mem", "write_if", host_write_if).map_err(map_err)?;
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
```
(If a `func_wrap`/`Caller`/`Memory` signature differs in `wasmi` 0.32, match the crate — the existing `agent_core::wasm` `host_log` is the reference for the correct types.)

- [ ] **Step 3: Register the module** — in `crates/agent/src/runtime/mod.rs` add `pub mod mem_host;`.

- [ ] **Step 4: Route `host.rs` to the mem runner** — replace the entire body of `maybe_run_configured` in `crates/agent/src/runtime/host.rs` with this (only the runner call + a grant line change vs the current Spec-1 version):

```rust
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
```
(`agent_core::wasm::run_wasm` is no longer called by the agent but stays in `agent-core` with its own tests — the pure log-only reference.)

- [ ] **Step 5: Start the refresher** — in `crates/agent/src/entry.rs`, before `crate::runtime::host::maybe_run_configured();`, add: `crate::external::cache::start_refresher();`

- [ ] **Step 6: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`; `cargo test -p agent-core 2>&1 | grep "test result" | head -1` → `51 passed`.

- [ ] **Step 7: Checkpoint** — hand diff to user (`feat: mem.* WASM host API + write gate + cache refresher`).

---

### Task 6: PW integration gate (manual)

**No code.** Prove the API on the live game. Build a small test `.wasm` (Rust/WAT) that: scans for a known pattern, reads a hit as a typed value and `log`s it, and (with write granted) `write_if`s it.

- [ ] **Step 1:** `./deploy.sh`.
- [ ] **Step 2 (read path):** launch PW with `FROG_WASM=<test.wasm>` (no write grant); confirm `agent.log` shows `mem API: read=on, write=off`, the scan finds hits, and the typed read logs a sane value. Confirm a read of `0x10` returns `ERR_UNREADABLE` (-1), game survives.
- [ ] **Step 3 (gate):** with a module that imports `mem.write` but **no** `FROG_WASM_WRITE`, confirm it fails to instantiate (the gate) — logged as a WASM error, game fine.
- [ ] **Step 4 (write path):** relaunch with `FROG_WASM_WRITE=1`; confirm `write=GRANTED`, `write_if` reports OK/CHANGED correctly, a bad write target returns `ERR_UNWRITABLE` (-2), game survives.
- [ ] **Step 5:** play a few minutes; agent memory bounded, no crash, dump/internals still clean (the cache refresher isn't a hog).
- [ ] **Step 6: Checkpoint** — report results. External brick done; pattern set for the internals + protocol bricks.

---

## Notes for the executor
- **Never commit** — stop at each checkpoint for the user.
- `./deploy.sh` only at the PW gate (Task 6), not after every task.
- If a `wasmi` 0.32 signature differs from a step's assumption, trust the crate and match it; the *behavior* (decode args → call `external::api` → encode result into the guest buffer → return status/len) is what matters. `agent_core::wasm::host_log` is the working reference for `Caller`/`Memory` usage.
- Reads must never syscall on a cache hit; writes always validate fresh. Keep that split — it's the whole performance/safety design.
