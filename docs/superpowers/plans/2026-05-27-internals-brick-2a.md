# Internals Brick 2a — `il2cpp` Read/Resolve API — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.
>
> **COMMITS ARE THE USER'S.** Do NOT `git commit`/`git push`. Each task ends at a checkpoint — hand the diff back for the user to commit.

**Goal:** Give WASM scripts a by-name `il2cpp` read/resolve API — `find_class`, `field_info`, `get_field`, `klass_of` — that emits external's exact `(address, ValType)` currency, so internals stands alone *and* composes with `mem.*`.

**Architecture:** A pure `tc → ValType` map in `agent-core` + a global resolved-context (`InternalsCtx`) the worker populates after il2cpp resolution + the 4 ops in `internals::api` (wrapping the proven FieldInfo walk + class-table walk + external's validated read) + `il2cpp.*` host functions registered alongside `mem.*`. **Read-only — no gate.**

**Tech Stack:** Rust, `wasmi` (already an agent dep), `windows-sys`, cross-compiled `x86_64-pc-windows-gnu`. Pure logic host-tested on Linux; FFI verified on Pixel Worlds.

**Scope:** This plan builds the **4 proven-machinery ops** (Part 1). `static_field` and `find_method` need fresh structural derivation (no existing offsets/accessors) — they are **Part 2: FIND-FIRST**, probe tasks to run on PW *before* their implementation. Build Part 1 fully; Part 2 is a research phase.

**Verify:** host tests `cargo test -p agent-core`; Windows compile `cargo check -p agent --target x86_64-pc-windows-gnu`; deploy `./deploy.sh`.

**Grounded facts (from the current code):**
- FieldInfo walk: `Il2CppApi.class_get_fields` (Option; iterator) or memory-walk `klass + cfg.klass_fields` → 32-byte slots `{ name*(0), type*(8), parent*(16), offset(24), token(28) }`.
- A field's `tc`: from its type ptr, `(read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at) >> cfg.discrim_shift) & 0xFF` (exactly as `resolve.rs` does).
- `Il2CppApi` has `class_get_name`, `class_get_namespace`, `class_get_fields: Option`, `field_get_name`, `field_get_type`. **No method accessors** (→ `find_method` is FIND-FIRST).
- `Il2CppConfig` has `class_table_step`, `klass_namespace`, `klass_type_def`, `klass_fields`, `il2cpp_type_discrim_read_at`, `discrim_shift`. **No static-fields offset** (→ `static_field` is FIND-FIRST).
- `external::api::read(addr, ValType, len) -> Result<Value, i32>` + `external::cache::validate_read` exist and are proven.

---

## PART 1 — the 4 proven-machinery ops

### Task 1: `agent_core` — `valtype_from_tc` (pure, TDD)

**Files:** Modify `crates/agent-core/src/mem_value.rs` (append fn + tests).

- [ ] **Step 1: Write the failing tests** (append to the existing `tests` mod in `mem_value.rs`):

```rust
    #[test]
    fn tc_numeric_primitives_map_exact() {
        assert_eq!(valtype_from_tc(0x02), Some(ValType::U8));   // Boolean
        assert_eq!(valtype_from_tc(0x03), Some(ValType::U16));  // Char
        assert_eq!(valtype_from_tc(0x04), Some(ValType::I8));   // I1
        assert_eq!(valtype_from_tc(0x05), Some(ValType::U8));   // U1
        assert_eq!(valtype_from_tc(0x08), Some(ValType::I32));  // I4
        assert_eq!(valtype_from_tc(0x0A), Some(ValType::I64));  // I8
        assert_eq!(valtype_from_tc(0x0C), Some(ValType::F32));  // R4
        assert_eq!(valtype_from_tc(0x0D), Some(ValType::F64));  // R8
    }

    #[test]
    fn tc_void_is_none_refs_are_u64() {
        assert_eq!(valtype_from_tc(0x01), None);               // Void
        assert_eq!(valtype_from_tc(0x0E), Some(ValType::U64)); // String (ref → ptr)
        assert_eq!(valtype_from_tc(0x12), Some(ValType::U64)); // CLASS (ref → ptr)
        assert_eq!(valtype_from_tc(0x1C), Some(ValType::U64)); // Object
    }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p agent-core valtype_from_tc` → FAIL (undefined).

- [ ] **Step 3: Implement** (append above the `tests` mod in `mem_value.rs`):

```rust
/// Map an il2cpp `Il2CppType` discriminator (`tc`) to the external `ValType` a
/// field of that type reads as. Numeric primitives map exactly; everything that
/// is a reference / pointer-sized / inline aggregate reads as `U64` (the pointer,
/// or the first 8 bytes — chase it); `Void` has no value.
pub fn valtype_from_tc(tc: u8) -> Option<ValType> {
    Some(match tc {
        0x02 => ValType::U8,   // Boolean
        0x03 => ValType::U16,  // Char
        0x04 => ValType::I8,   // I1 / SByte
        0x05 => ValType::U8,   // U1 / Byte
        0x06 => ValType::I16,  // I2
        0x07 => ValType::U16,  // U2
        0x08 => ValType::I32,  // I4 / Int32
        0x09 => ValType::U32,  // U4 / UInt32
        0x0A => ValType::I64,  // I8 / Int64
        0x0B => ValType::U64,  // U8 / UInt64
        0x0C => ValType::F32,  // R4 / Single
        0x0D => ValType::F64,  // R8 / Double
        0x01 => return None,   // Void — no value
        _ => ValType::U64,     // String/Object/CLASS/ARRAY/IntPtr/GENERICINST/… → pointer-sized
    })
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p agent-core valtype_from_tc` → 2 passed; `cargo test -p agent-core` → 53 passed (51 + 2).

- [ ] **Step 5: Checkpoint** — hand diff (`feat: agent-core valtype_from_tc (il2cpp tc → ValType)`).

### Task 2: `external::cache` — validated raw read helpers

internals' structural walks (klass / FieldInfo) need bounds-checked raw reads. Add cache-backed helpers so internals reads go through the same validated path external uses.

**Files:** Modify `crates/agent/src/external/cache.rs` (append).

- [ ] **Step 1: Append to `cache.rs`**

```rust
/// Validated raw reads for structural walks (klass/FieldInfo). Each validates
/// against the region cache (binary search, miss → VirtualQuery) before reading.
pub fn read_u64(addr: usize) -> Option<u64> {
    if validate_read(addr, 8) { Some(unsafe { *(addr as *const u64) }) } else { None }
}
pub fn read_u32(addr: usize) -> Option<u32> {
    if validate_read(addr, 4) { Some(unsafe { *(addr as *const u32) }) } else { None }
}
/// NUL-terminated printable-ASCII string (<=255 bytes) at `addr`, validated.
pub fn read_cstr(addr: usize) -> Option<String> {
    if !validate_read(addr, 1) { return None; }
    let mut out = String::new();
    for i in 0..255usize {
        let b = read_u32(addr + i).map(|v| (v & 0xFF) as u8)?;
        if b == 0 { return Some(out); }
        if !(0x20..=0x7E).contains(&b) { return None; }
        out.push(b as char);
    }
    Some(out)
}
```

- [ ] **Step 2: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`.

- [ ] **Step 3: Checkpoint** — hand diff (`feat: external cache validated read helpers`).

### Task 3: `internals::ctx` — resolved context + wire the worker

The host functions run after il2cpp resolution and need the table + API + config. Stash them in a global the worker populates.

**Files:** Create `crates/agent/src/internals/ctx.rs`; modify `crates/agent/src/internals/mod.rs` (add `pub mod ctx;`), `crates/agent/src/entry.rs`.

- [ ] **Step 1: Create `internals/ctx.rs`**

```rust
//! Resolved il2cpp context, populated by the worker once after resolution and
//! read by the `il2cpp.*` host functions. Holds only Send+Sync data (fn pointers
//! + offsets + table bounds).

use std::sync::OnceLock;

use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;

pub struct InternalsCtx {
    pub table_base: usize,
    pub table_count: usize,
    pub api: Il2CppApi,
    pub cfg: Il2CppConfig,
}

static CTX: OnceLock<InternalsCtx> = OnceLock::new();

/// Called once by the worker after il2cpp resolution. Later calls are ignored.
pub fn init(ctx: InternalsCtx) {
    let _ = CTX.set(ctx);
}

pub fn get() -> Option<&'static InternalsCtx> {
    CTX.get()
}
```

- [ ] **Step 2: Register** — in `internals/mod.rs` add `pub mod ctx;`.

- [ ] **Step 3: Populate from the worker** — in `crates/agent/src/entry.rs`, after the dump is built (after the `build_type_maps` call / before `maybe_run_configured`), add (the worker has `table_base`, `table_count`, `api`, `cfg` in scope):

```rust
    crate::internals::ctx::init(crate::internals::ctx::InternalsCtx {
        table_base,
        table_count,
        api: api.clone(),
        cfg: cfg.clone(),
    });
```

- [ ] **Step 4: Make `Il2CppApi` and `Il2CppConfig` `Clone`** — if they aren't already, add `#[derive(Clone)]` to both structs (`internals/ffi.rs`, `internals/config.rs`). They are plain fn-pointers/offsets, so `Clone` (and `Send`/`Sync`) derive cleanly. If `api` is later used in `entry.rs` after the move, the `.clone()` already handles it.

- [ ] **Step 5: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`. (If `OnceLock<InternalsCtx>` complains about `Sync`, add `unsafe impl Send for InternalsCtx {}` / `unsafe impl Sync for InternalsCtx {}` — fn pointers are safe to share; document why.)

- [ ] **Step 6: Checkpoint** — hand diff (`feat: internals resolved-context global + worker wiring`).

### Task 4: `internals::api` — find_class, field_info, get_field, klass_of

**Files:** Create `crates/agent/src/internals/api.rs`; modify `internals/mod.rs` (add `pub mod api;`).

- [ ] **Step 1: Implement `api.rs`**

```rust
//! The 4 proven-machinery internals ops, by name. Structural walks (klass/
//! FieldInfo) go through external's validated cache reads; instance values go
//! through external's typed read. Emits external's (offset, ValType) currency.

use std::ffi::c_void;

use agent_core::mem_value::{status, valtype_from_tc, ValType, Value};

use crate::external::{api as ext, cache};
use crate::internals::ctx;
use crate::internals::ffi::cstr_to_string;

/// Search the live class table for a class whose name (or "Namespace::Name")
/// matches `name`. Returns the klass ptr, or 0.
pub fn find_class(name: &str) -> u64 {
    let c = match ctx::get() { Some(c) => c, None => return 0 };
    for i in 0..c.table_count {
        let slot = c.table_base.wrapping_add(i * c.cfg.class_table_step);
        let klass = match cache::read_u64(slot) { Some(k) if k != 0 => k as usize, _ => continue };
        let cn = unsafe { cstr_to_string((c.api.class_get_name)(klass as *mut c_void)) };
        if cn.is_empty() { continue; }
        if cn == name {
            return klass as u64;
        }
        let ns = unsafe { cstr_to_string((c.api.class_get_namespace)(klass as *mut c_void)) };
        let full = if ns.is_empty() { cn.clone() } else { format!("{}::{}", ns, cn) };
        if full == name {
            return klass as u64;
        }
    }
    0
}

/// Walk a klass's FieldInfo array, invoking `f(name, offset, type_ptr)` per field.
/// Uses the FFI iterator when available, else the 32-byte memory-walk fallback.
fn for_each_field(klass: usize, mut f: impl FnMut(&str, u32, usize) -> bool) {
    let c = match ctx::get() { Some(c) => c, None => return };
    if let Some(get_fields) = c.api.class_get_fields {
        let mut iter: *mut c_void = std::ptr::null_mut();
        for _ in 0..256 {
            let fi = unsafe { get_fields(klass as *mut c_void, &mut iter) };
            if fi.is_null() { break; }
            let name = unsafe { cstr_to_string((c.api.field_get_name)(fi)) };
            let type_ptr = unsafe { (c.api.field_get_type)(fi) } as usize;
            let offset = cache::read_u32(fi as usize + 24).unwrap_or(0);
            if f(&name, offset, type_ptr) { return; }
        }
    } else {
        let fields_ptr = match cache::read_u64(klass + c.cfg.klass_fields) { Some(p) if p != 0 => p as usize, _ => return };
        for fi in 0..256usize {
            let slot = fields_ptr + fi * 32;
            let name_ptr = match cache::read_u64(slot) { Some(p) if p != 0 => p as usize, _ => break };
            let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => break };
            let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
            let offset = cache::read_u32(slot + 24).unwrap_or(0);
            if f(&name, offset, type_ptr) { return; }
        }
    }
}

/// Read the `tc` discriminator of an Il2CppType ptr (same as the resolver).
fn type_tc(type_ptr: usize) -> u8 {
    let c = match ctx::get() { Some(c) => c, None => return 0 };
    let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8
}

/// Field offset + external ValType for `name`, or None. The composition bridge.
pub fn field_info(klass: u64, name: &str) -> Option<(u32, ValType)> {
    let mut found = None;
    for_each_field(klass as usize, |fname, offset, type_ptr| {
        if fname == name {
            let vt = valtype_from_tc(type_tc(type_ptr)).unwrap_or(ValType::U64);
            found = Some((offset, vt));
            true
        } else {
            false
        }
    });
    found
}

/// Read a field by name through external's validated read. The native read.
pub fn get_field(instance: u64, klass: u64, name: &str) -> Result<Value, i32> {
    let (offset, vt) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
    let addr = (instance as usize).wrapping_add(offset as usize);
    ext::read(addr, vt, vt.fixed_width().unwrap_or(8))
}

/// The klass pointer at an object's head ("what is this object?"). 0 = unreadable.
pub fn klass_of(instance: u64) -> u64 {
    cache::read_u64(instance as usize).unwrap_or(0)
}
```

**Note for the implementer:** confirm the exact signatures of `cstr_to_string`, `class_get_name`, `class_get_namespace`, `class_get_fields`, `field_get_name`, `field_get_type` against `internals/ffi.rs` and match them (e.g. whether `field_get_type` returns `*mut c_void` or a typed pointer; whether the FFI takes `*mut c_void` for the klass). The behavior — walk fields, match by name, read offset + type `tc` — is what matters.

- [ ] **Step 2: Register** — in `internals/mod.rs` add `pub mod api;`.

- [ ] **Step 3: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`; `cargo test -p agent-core 2>&1 | grep "test result" | head -1` → `53 passed`.

- [ ] **Step 4: Checkpoint** — hand diff (`feat: internals api — find_class/field_info/get_field/klass_of`).

### Task 5: `il2cpp.*` host functions (alongside `mem.*`)

**Files:** Modify `crates/agent/src/runtime/mem_host.rs` (add 4 host fns + register them).

- [ ] **Step 1: Add the host functions** in `mem_host.rs` (reuse the existing `read_guest`/`write_guest` helpers):

```rust
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
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return agent_core::mem_value::status::ERR_BAD_TYPE };
    let name = String::from_utf8_lossy(&name).into_owned();
    let value = match crate::internals::api::get_field(instance as u64, klass as u64, &name) { Ok(v) => v, Err(c) => return c };
    let bytes = value.encode();
    if bytes.len() > out_cap.max(0) as usize { return agent_core::mem_value::status::ERR_BUF_TOO_SMALL; }
    if !write_guest(&mut caller, out_ptr, &bytes) { return agent_core::mem_value::status::ERR_BUF_TOO_SMALL; }
    bytes.len() as i32
}

fn host_klass_of(_caller: Caller<'_, HostState>, instance: i64) -> i64 {
    crate::internals::api::klass_of(instance as u64) as i64
}
```
(`ValType` is `#[repr(u8)]`, so `vt as u8` is its tag for the packed `field_info` return.)

- [ ] **Step 2: Register them** — in `run_wasm_with_mem`, after the `mem.*` registrations (always, read-only — no gate):

```rust
    linker.func_wrap("il2cpp", "find_class", host_find_class).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "field_info", host_field_info).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "get_field", host_get_field).map_err(|e| WasmError::Instantiate(e.to_string()))?;
    linker.func_wrap("il2cpp", "klass_of", host_klass_of).map_err(|e| WasmError::Instantiate(e.to_string()))?;
```

- [ ] **Step 3: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "^error" || echo OK` → `OK`; `cargo test -p agent-core 2>&1 | grep "test result" | head -1` → `53 passed`.

- [ ] **Step 4: Checkpoint** — hand diff (`feat: il2cpp.* host functions (find_class/field_info/get_field/klass_of)`).

### Task 6: Cross-brick PW gate (manual)

**No code beyond a test `.wasm`.** Build a module (compile via the `wat` dev-dep helper, as with `test_mem.wasm`) that:
1. `il2cpp.find_class("<a known class from internals.txt>")` → non-zero,
2. `il2cpp.get_field(instance, klass, "<field>")` → logs a sane value (instance from a `mem.scan` hit or a known address),
3. `il2cpp.field_info(...)` + `mem.read(instance+offset, val_type)` agrees with `get_field` (the composition edge),
4. `il2cpp.klass_of(instance)` → non-zero and equals `find_class` of that instance's type,
5. `il2cpp.find_class("Nonexistent")` → 0; `get_field` of a bad instance → `ERR_UNREADABLE`, game survives.

- [ ] **Step 1:** `./deploy.sh`; stage the test `.wasm` in the game dir.
- [ ] **Step 2:** launch PW with `FROG_WASM=<test>.wasm`; confirm the `[wasm]` log lines show resolve + read + composition agreement, no crash.
- [ ] **Step 3: Checkpoint** — report results. internals-2a foundation proven; the domain graph visibly closes (`read a field by name, live`).

---

## PART 2 — FIND-FIRST (probe on PW *before* implementing)

These two ops have **no existing machinery** — derive the structure on PW first (prove-it), then implement. Do NOT guess offsets in code.

### FIND-FIRST A: `static_field` — locate `klass->static_fields`
**Unknown:** the klass-struct offset of the `static_fields` base pointer, and how to detect a field is static. **Probe approach:** for a class known to have a static field (from `internals.txt`), scan klass-struct offsets (e.g. +0x00..+0x100, step 8) for a pointer into a committed-writable region; cross-check that `that_ptr + (a static field's FieldInfo.offset)` reads a sane value; log candidate offsets + the FieldInfo flag bits that distinguish static vs instance. Gate it behind a `FROG_*` env like the other probes. **Then implement** `static_field(klass, name) → addr` using the derived offset (structurally, never hardcoded).

### FIND-FIRST B: `find_method` — locate methods + `MethodInfo` layout
**Unknown:** either the `class_get_methods`/`class_get_method_from_name` exports (sig-scan) OR the `klass->methods` array offset + `MethodInfo` layout (`name`, `parameters_count`, `methodPointer`). **Probe approach:** scan klass-struct offsets for a pointer to an array-of-pointers whose targets begin with a readable name string (MethodInfo.name); cross-check the count against a method-count field; for a found MethodInfo, dump its first ~0x40 bytes and locate `parameters_count` (matches the known arity of a named method) and `methodPointer` (an address inside the GameAssembly code region). Log candidates. **Then implement** `find_method(klass, name, argc) → MethodInfo*` from the derived layout, and add the method accessors to `Il2CppApi` via the existing sig-scan path if that route proves more robust.

When both probes land, implement the two ops (each: a small TDD-where-pure + FFI task + the PW gate extended to cover them), completing the 6-op 2a surface.

---

## Notes for the executor
- **Never commit** — stop at each checkpoint.
- `./deploy.sh` only at the PW gate (Task 6) and the FIND-FIRST probes — not after every task.
- Match real `wasmi`/`Il2CppApi` signatures (`mem_host`'s existing host fns and `internals/ffi.rs` are the references); behavior is what matters.
- Structural walks read via `external::cache` (validated); instance values read via `external::api::read`. Keep that split.
