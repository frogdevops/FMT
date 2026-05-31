# B-5 WASM Typed Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the typed spine surface the canonical (and only) Rust path that WASM host fns call. Delete the raw `api::*` bridges, evolve remaining raw-arg signatures to typed handles, and rewrite `mem_host.rs` host fns to dispatch through the typed surface.

**Architecture:** Mechanical refactor — merge each raw + typed-sibling pair into a unified typed fn, evolve `field_info` / `get_field` to take `KlassPtr` / `Instance`, then rewrite `host_read` / `host_write` / `host_write_if` as ValType match dispatchers that go through `api::read::<T, _>` / `api::write::<T>` (which themselves are façades over the B-4 `Read<T>` / `Write<T>` trait impls). WASM ABI is unchanged — guest scripts and existing `.wasm` files keep working.

**Tech Stack:** Rust 2021 (stable), wasmi 0.31, agent-core spine traits (`Read<T>` / `Write<T>` / `MemValue` / `MemAddr<C>` / `KlassPtr` / `MethodPtr` / `Instance` / `FieldAddr`), Windows host (live game).

**Reference spec:** `docs/superpowers/specs/2026-05-30-b5-wasm-typed-dispatch-design.md`

---

## File Structure

This plan modifies these files; no new files are created.

| File | Responsibility | Change |
|---|---|---|
| `crates/agent/src/internals/api.rs` | Internal il2cpp ops (find_class, field_info, get_field, etc.) | Delete raw fns, merge bodies into typed signatures, evolve `field_info`/`get_field` arg types |
| `crates/agent/src/external/api.rs` | External memory ops (read, write, scan, regions) | Delete raw `read`/`write`/`write_if`, rename `_t` survivors (`read_t`→`read`, etc.) |
| `crates/agent/src/runtime/mem_host.rs` | WASM host fns (`mem.*`, `il2cpp.*` linker fns) | Rewrite `host_read`/`host_write`/`host_write_if` with ValType match dispatch; migrate metadata host fns to use typed siblings |
| `crates/agent/src/diagnostics/klass_probe.rs` | Diagnostic class-struct dumper | Migrate 2 call sites from raw `find_class` → typed |

**Survivors (no change):** `api::scan`, `api::regions` (no parallel typed surface — inherently raw at the right abstraction), all hook host fns (`host_hook_arg`, `host_hook_set_arg`, `host_hook_set_return`, `host_hook_this`, `host_call_original`, `host_install_hook`, `host_remove_hook` — these are out of scope per the spec).

---

## Task 1: Unify `find_class` — merge raw body into typed signature, migrate 3 callers

**Files:**
- Modify: `crates/agent/src/internals/api.rs:15-29` (raw `find_class`) and `:194-199` (`find_class_t`)
- Modify: `crates/agent/src/runtime/mem_host.rs:93-97` (`host_find_class`)
- Modify: `crates/agent/src/diagnostics/klass_probe.rs:79` and `:110`

- [ ] **Step 1: Add `KlassPtr` to the imports in `internals/api.rs`**

In `crates/agent/src/internals/api.rs`, find the existing imports near the top and add a `use` for the spine handle:

```rust
use agent_core::mem_value::{status, valtype_from_tc, ValType, Value};
use agent_core::spine::{KlassPtr, MethodPtr, Instance, FieldAddr, MemAddr, ReadWrite, InvokeArg, InvokeError};
```

(Replace the bare uses with this consolidated `use` block if it's cleaner; existing fully-qualified `agent_core::spine::KlassPtr` callers in the file can be simplified once the import is in scope.)

- [ ] **Step 2: Replace both `find_class` and `find_class_t` with a single unified typed fn**

Delete lines `15-29` (raw `find_class`) and lines `194-199` (`find_class_t`). Insert this unified version where `find_class` used to live (around line 15):

```rust
/// Search the live class table for a class whose name (or "Namespace::Name")
/// matches `name`. Returns `Some(KlassPtr)` when found, `None` otherwise.
pub fn find_class(name: &str) -> Option<KlassPtr> {
    let c = ctx::get()?;
    for i in 0..c.table_count {
        let slot = c.table_base.wrapping_add(i * c.cfg.class_table_step);
        let klass = match cache::read_u64(slot) {
            Some(k) if k != 0 => k as usize,
            _ => continue,
        };
        if !cache::is_klass_shape(klass) { continue; }
        let cn = unsafe { cstr_to_string((c.api.class_get_name)(klass as *mut Il2CppClass)) };
        if cn.is_empty() { continue; }
        if cn == name { return Some(KlassPtr::from_raw(klass as u64)); }
        let ns = unsafe { cstr_to_string((c.api.class_get_namespace)(klass as *mut Il2CppClass)) };
        let full = if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
        if full == name { return Some(KlassPtr::from_raw(klass as u64)); }
    }
    None
}
```

- [ ] **Step 3: Migrate `host_find_class` in `mem_host.rs`**

In `crates/agent/src/runtime/mem_host.rs`, replace the body of `host_find_class` (currently at lines 93-97):

```rust
fn host_find_class(caller: Caller<'_, HostState>, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    let name = String::from_utf8_lossy(&name);
    crate::internals::api::find_class(&name)
        .map(|k| k.as_u64() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 4: Migrate `klass_probe.rs:79`**

In `crates/agent/src/diagnostics/klass_probe.rs`, replace line 79:

```rust
        let klass = api::find_class(cname).map(|k| k.as_u64()).unwrap_or(0);
```

- [ ] **Step 5: Migrate `klass_probe.rs:110`**

In the same file, replace line 110:

```rust
    let klass = api::find_class("Player").map(|k| k.as_u64() as usize).unwrap_or(0);
```

- [ ] **Step 6: Build and verify**

Run: `cargo build --release -p agent`
Expected: `Compiling agent` succeeds with no errors. (`./deploy.sh` may auto-run via the user's hook; that is normal and expected — the deployed DLL is in a known-good intermediate state, since only `find_class` is migrated and the rest of the surface is untouched.)

If the build fails, the most likely cause is a missed `agent_core::spine::KlassPtr` fully-qualified path inside the file — replace with the bare `KlassPtr` after the import in Step 1.

- [ ] **Step 7: Pause for user commit (logical checkpoint)**

This is a coherent atomic change (one fn unified, all callers migrated, build green). The user commits the change; do not run `git commit` yourself per the project memory rule.

---

## Task 2: Unify `find_method`, `klass_of`, `static_field` (single-caller fns)

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (raw definitions + `_t` siblings)
- Modify: `crates/agent/src/runtime/mem_host.rs` (host fn call sites)

Each of these three fns has only one caller (the host fn in `mem_host.rs`), so they consolidate quickly. Do all three in this task — the pattern is identical.

- [ ] **Step 1: Unify `find_method`**

In `crates/agent/src/internals/api.rs`, delete raw `find_method` (lines 142-169) and `find_method_t` (lines 201-211). Insert this unified version where `find_method` used to live (around line 142):

```rust
/// Locate a method by name + arg count → `MethodPtr`, or `None`. Walks the
/// klass's methods array; stops at the array end when an entry's klass back-
/// pointer no longer matches (no method_count needed).
pub fn find_method(klass: KlassPtr, name: &str, argc: u32) -> Option<MethodPtr> {
    let c = ctx::get()?;
    let k = klass.as_u64() as usize;
    let methods = cache::read_u64(k + c.cfg.klass_methods).unwrap_or(0) as usize;
    if methods == 0 {
        return None;
    }
    for i in 0..4096usize {
        let mi = match cache::read_u64(methods + i * 8) {
            Some(v) if v != 0 => v as usize,
            _ => break,
        };
        // Array-end / validity: the MethodInfo's declaring-klass must be this klass.
        if cache::read_u64(mi + c.cfg.method_klass_off).unwrap_or(0) != klass.as_u64() {
            break;
        }
        let name_ptr = cache::read_u64(mi + c.cfg.method_name_off).unwrap_or(0) as usize;
        let mname = match cache::read_cstr(name_ptr) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let pcount = cache::read_u8(mi + c.cfg.method_param_count_off).unwrap_or(0) as u32;
        if mname == name && pcount == argc {
            return Some(MethodPtr::from_raw(mi as u64));
        }
    }
    None
}
```

- [ ] **Step 2: Unify `klass_of`**

Delete raw `klass_of` (lines 110-112) and `klass_of_t` (lines 247-254). Insert:

```rust
/// The klass pointer at an object's head ("what is this object?"). Returns
/// `None` if the instance head is unreadable or zero.
pub fn klass_of(instance: Instance) -> Option<KlassPtr> {
    match cache::read_u64(instance.as_u64() as usize) {
        Some(k) if k != 0 => Some(KlassPtr::from_raw(k)),
        _ => None,
    }
}
```

- [ ] **Step 3: Unify `static_field`**

Delete raw `static_field` (lines 117-137) and `static_field_t` (lines 233-243). Insert:

```rust
/// Address of a static field by name. Returns `Some(MemAddr<ReadWrite>)` when
/// found AND the field is actually static, `None` otherwise. Statics are
/// writable by intent.
pub fn static_field(klass: KlassPtr, name: &str) -> Option<MemAddr<ReadWrite>> {
    let c = ctx::get()?;
    let k = klass.as_u64() as usize;
    let static_base = cache::read_u64(k + c.cfg.klass_static_fields).unwrap_or(0);
    if static_base == 0 {
        return None;
    }
    let mut addr_out: Option<MemAddr<ReadWrite>> = None;
    for_each_field(k, |fname, offset, type_ptr| {
        if fname == name {
            let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
            if chunk & 0x10 != 0 {
                let raw = static_base + offset as u64;
                // SAFETY: static_base lives in a writable region; the static-attr
                // bit confirms this field address is in that region.
                addr_out = Some(unsafe { MemAddr::<ReadWrite>::from_raw_writable(raw) });
            }
            true
        } else {
            false
        }
    });
    addr_out
}
```

- [ ] **Step 4: Migrate `host_find_method` in `mem_host.rs`**

Replace `host_find_method` body (currently lines 137-140):

```rust
fn host_find_method(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32, argc: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    crate::internals::api::find_method(klass, &String::from_utf8_lossy(&name), argc.max(0) as u32)
        .map(|m| m.as_u64() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 5: Migrate `host_klass_of` in `mem_host.rs`**

Replace `host_klass_of` body (currently lines 128-130):

```rust
fn host_klass_of(_caller: Caller<'_, HostState>, instance: i64) -> i64 {
    let instance = agent_core::spine::Instance::from_raw(instance as u64);
    crate::internals::api::klass_of(instance)
        .map(|k| k.as_u64() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 6: Migrate `host_static_field` in `mem_host.rs`**

Replace `host_static_field` body (currently lines 132-135):

```rust
fn host_static_field(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return 0 };
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    crate::internals::api::static_field(klass, &String::from_utf8_lossy(&name))
        .map(|a| a.as_u64() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 7: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds. (Auto-deploy hook may fire; that's fine — three more host fns are now typed-internally, the WASM ABI is unchanged so live game behavior is unaffected.)

- [ ] **Step 8: Pause for user commit**

---

## Task 3: Evolve `field_info` signature to take `KlassPtr`

**Files:**
- Modify: `crates/agent/src/internals/api.rs:88-100` (`field_info`) and `:217-228` (`field_addr_t`)
- Modify: `crates/agent/src/runtime/mem_host.rs:99-106` (`host_field_info`)

`field_info` keeps the same return shape (`Option<(u32, ValType)>`) — only the `klass` arg changes from `u64` to `KlassPtr`. There's no `_t` sibling to merge.

- [ ] **Step 1: Change `field_info` signature**

In `crates/agent/src/internals/api.rs`, change line 88:

```rust
pub fn field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType)> {
    let mut found = None;
    for_each_field(klass.as_u64() as usize, |fname, offset, type_ptr| {
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
```

(Note: the body uses `klass.as_u64() as usize` once where `klass as usize` used to be. Same behavior.)

- [ ] **Step 2: Update `field_addr_t`'s internal call**

In the same file, at the call site inside `field_addr_t` (line 222), the call becomes cleaner — `klass` is already `KlassPtr`, no conversion needed:

```rust
pub fn field_addr_t(
    klass: KlassPtr,
    name: &str,
    instance: Instance,
) -> Option<FieldAddr> {
    let (offset, vt) = field_info(klass, name)?;
    let addr_raw = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    // SAFETY: caller obtained `instance` via the spine API; instance fields
    // are writable by their semantic role.
    let addr = unsafe { MemAddr::from_raw_writable(addr_raw) };
    Some(FieldAddr::new(addr, vt))
}
```

(The full-qualified `agent_core::spine::...` paths simplify to bare names after Task 1's consolidated `use` import.)

- [ ] **Step 3: Update the internal call inside `get_field`**

`get_field` still calls `field_info(klass, name)` — but `klass` in `get_field` is still raw `u64` until Task 4 evolves its signature. Convert at the call site for now (one line edit at line 104):

```rust
    let (offset, vt) = field_info(KlassPtr::from_raw(klass), name).ok_or(status::ERR_BAD_TYPE)?;
```

(Task 4 deletes this conversion when it evolves `get_field` itself.)

- [ ] **Step 4: Migrate `host_field_info` in `mem_host.rs`**

Replace `host_field_info` body (currently lines 99-106):

```rust
fn host_field_info(caller: Caller<'_, HostState>, klass: i64, name_ptr: i32, name_len: i32) -> i64 {
    let name = match read_guest(&caller, name_ptr, name_len) { Some(b) => b, None => return -1 };
    let name = String::from_utf8_lossy(&name);
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    match crate::internals::api::field_info(klass, &name) {
        Some((offset, vt)) => ((vt as u8 as i64) << 32) | (offset as i64),
        None => -1,
    }
}
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds.

- [ ] **Step 6: Pause for user commit**

---

## Task 4: Evolve `get_field` signature to take `Instance` and `KlassPtr`

**Files:**
- Modify: `crates/agent/src/internals/api.rs:103-107` (`get_field`)
- Modify: `crates/agent/src/runtime/mem_host.rs:108-126` (`host_get_field`)

- [ ] **Step 1: Change `get_field` signature**

In `crates/agent/src/internals/api.rs`, change `get_field` (currently lines 103-107):

```rust
/// Read a field by name through external's validated read. The native read.
pub fn get_field(instance: Instance, klass: KlassPtr, name: &str) -> Result<Value, i32> {
    let (offset, vt) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
    let addr = (instance.as_u64() as usize).wrapping_add(offset as usize);
    ext::read(addr, vt, vt.fixed_width().unwrap_or(8))
}
```

Note: this still calls the *raw* `ext::read` — that's intentional; Task 6 rewrites the host fn that's the only other caller of raw `ext::read`, and Task 9 deletes it. For now we tolerate the call temporarily. This is the only file that still calls `ext::read` after Tasks 1-5; the deletion order in Task 9 reflects that.

- [ ] **Step 2: Migrate `host_get_field` in `mem_host.rs`**

Replace `host_get_field` body (currently lines 108-126):

```rust
fn host_get_field(mut caller: Caller<'_, HostState>, instance: i64, klass: i64, name_ptr: i32, name_len: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let name = match read_guest(&caller, name_ptr, name_len) {
        Some(b) => b,
        None => return agent_core::mem_value::status::ERR_BAD_TYPE,
    };
    let name = String::from_utf8_lossy(&name).into_owned();
    let instance = agent_core::spine::Instance::from_raw(instance as u64);
    let klass = agent_core::spine::KlassPtr::from_raw(klass as u64);
    let value = match crate::internals::api::get_field(instance, klass, &name) {
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
```

- [ ] **Step 3: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds.

- [ ] **Step 4: Pause for user commit**

---

## Task 5: Rename remaining `_t` survivors (drop suffix uniformly)

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (`field_addr_t`, `invoke_method_t`)
- Modify: `crates/agent/src/external/api.rs` (`read_t`, `write_t`, `read_bytes_t`, `read_cstr_t`, plus 3 test references)
- Modify: `crates/agent/src/runtime/mem_host.rs:239` (the one runtime caller — `invoke_method_t` in `host_invoke`)

This task is the lexical rename pass. No semantic changes. The compiler enforces full coverage — any missed call site fails the build.

- [ ] **Step 1: Rename in `internals/api.rs`**

In `crates/agent/src/internals/api.rs`:
- Rename `field_addr_t` → `field_addr` (signature unchanged; body's `field_info` call already correct from Task 3).
- Rename `invoke_method_t` → `invoke_method` (signature unchanged).

- [ ] **Step 2: Rename in `external/api.rs`**

In `crates/agent/src/external/api.rs`:
- Rename `read_t` → `read` (this CONFLICTS with the existing raw `read` at line 13; the raw `read` is deleted in Task 9. For now this leaves a name clash — STOP, the rename must happen AFTER raw `read` is deleted. Revise the order: do raw deletion FIRST in this task for the external fns.)

**Revised approach for external renames:** combine raw-deletion with rename in this task. The raw `external::api::{read, write, write_if}` fns have only one caller each: the corresponding host fns. After Tasks 1-4 the metadata host fns are typed; the only raw-`ext::read` caller left is `get_field` (Task 4 acknowledges this; that call goes away when Task 6 rewrites `host_read` and we no longer need `ext::read` as a separate path — but actually `get_field`'s `ext::read` call must be migrated to use typed dispatch too before raw `ext::read` deletes).

**Decision:** rename `read_t`/`write_t`/etc. WITHOUT deleting raw `read`/`write`/`write_if` in this task. Use a temporary disambiguator: don't rename `read_t`/`write_t` here; rename ONLY `read_bytes_t`/`read_cstr_t` (no clash) and the internals fns. The rename of `read_t`/`write_t` happens in Task 9 alongside raw deletion. This keeps each task atomic.

Concretely in `external/api.rs`:
- Rename `read_bytes_t` → `read_bytes` (no clash).
- Rename `read_cstr_t` → `read_cstr` (no clash with `cache::read_cstr` because that's in a different module).
- **Do NOT rename `read_t` or `write_t` yet** — deferred to Task 9.

- [ ] **Step 3: Update the runtime caller of `invoke_method_t`**

In `crates/agent/src/runtime/mem_host.rs`, line 239 inside `host_invoke`:

```rust
    match crate::internals::api::invoke_method(method, instance, &args) {
```

- [ ] **Step 4: Update the 3 test references in `external/api.rs`**

In `crates/agent/src/external/api.rs`, the spine_tests module references `read_t` / `write_t` at lines 142, 143, 151. **Leave these unchanged in this task** — they reference `read_t`/`write_t`, which are NOT renamed yet (Task 9 handles them). Document this in the task; revisit at Task 9.

- [ ] **Step 5: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds. Compile-fail tests in `agent-core/tests/spine.rs` and `external/api.rs:142-151` continue to reference the old names where appropriate — no test rename required yet.

- [ ] **Step 6: Pause for user commit**

---

## Task 6: Rewrite `host_read` with typed-dispatch helper

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (add helper fn + replace `host_read`)

After this task, `host_read` no longer calls `api::read` (the raw fn). It dispatches by `ValType` to typed `api::read_t::<T>` (still named `read_t` until Task 9) — and the `Bytes`/`Cstr` arms call `api::read_bytes`/`api::read_cstr` (renamed in Task 5).

- [ ] **Step 1: Add the typed read helper fn**

Near the top of `crates/agent/src/runtime/mem_host.rs` (after the existing `write_guest` helper, around line 35), add:

```rust
/// Dispatch a typed read of `T` from `addr` and write the little-endian bytes
/// into the guest's `out_ptr` buffer. Returns the number of bytes written, or
/// a negative status code.
fn host_read_typed<T: MemValue>(
    caller: &mut Caller<'_, HostState>,
    addr: MemAddr<ReadOnly>,
    out_ptr: i32,
    out_cap: i32,
) -> i32 {
    match api::read_t::<T, _>(addr) {
        Ok(v) => {
            let bytes = v.to_le_bytes_buf();
            if bytes.len() > out_cap.max(0) as usize {
                return status::ERR_BUF_TOO_SMALL;
            }
            if !write_guest(caller, out_ptr, &bytes) {
                return status::ERR_BUF_TOO_SMALL;
            }
            bytes.len() as i32
        }
        Err(e) => i32::from(e),
    }
}
```

You'll need to add imports near the top of the file (around line 8):

```rust
use agent_core::spine::{MemAddr, MemError, MemValue, ReadOnly, ReadWrite};
```

(Some of these are already imported via `agent_core::spine::*` paths — consolidate as you go.)

- [ ] **Step 2: Replace `host_read` with the ValType dispatcher**

Replace the entire `host_read` body (currently lines 43-50):

```rust
fn host_read(mut caller: Caller<'_, HostState>, addr: i64, ty: i32, len: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let addr = MemAddr::<ReadOnly>::from_raw(addr as u64);
    match ty {
        ValType::U8  => host_read_typed::<u8 >(&mut caller, addr, out_ptr, out_cap),
        ValType::U16 => host_read_typed::<u16>(&mut caller, addr, out_ptr, out_cap),
        ValType::U32 => host_read_typed::<u32>(&mut caller, addr, out_ptr, out_cap),
        ValType::U64 => host_read_typed::<u64>(&mut caller, addr, out_ptr, out_cap),
        ValType::I8  => host_read_typed::<i8 >(&mut caller, addr, out_ptr, out_cap),
        ValType::I16 => host_read_typed::<i16>(&mut caller, addr, out_ptr, out_cap),
        ValType::I32 => host_read_typed::<i32>(&mut caller, addr, out_ptr, out_cap),
        ValType::I64 => host_read_typed::<i64>(&mut caller, addr, out_ptr, out_cap),
        ValType::F32 => host_read_typed::<f32>(&mut caller, addr, out_ptr, out_cap),
        ValType::F64 => host_read_typed::<f64>(&mut caller, addr, out_ptr, out_cap),
        ValType::Bytes => match api::read_bytes(addr, len.max(0) as usize) {
            Ok(bytes) => {
                if bytes.len() > out_cap.max(0) as usize { return status::ERR_BUF_TOO_SMALL; }
                if !write_guest(&mut caller, out_ptr, &bytes) { return status::ERR_BUF_TOO_SMALL; }
                bytes.len() as i32
            }
            Err(e) => i32::from(e),
        },
        ValType::Cstr => match api::read_cstr(addr, len.max(0) as usize) {
            Ok(s) => {
                let bytes = s.into_bytes();
                if bytes.len() > out_cap.max(0) as usize { return status::ERR_BUF_TOO_SMALL; }
                if !write_guest(&mut caller, out_ptr, &bytes) { return status::ERR_BUF_TOO_SMALL; }
                bytes.len() as i32
            }
            Err(e) => i32::from(e),
        },
    }
}
```

**`ValType` has 12 variants** (verified against `agent-core/src/mem_value.rs:8-14`): `U8, U16, U32, U64, I8, I16, I32, I64, F32, F64, Bytes, Cstr`. There is no `Ptr` variant — pointers are `U64`. The match above is exhaustive over all 12. If the enum gains a variant in the future the compiler will flag the missing arm.

- [ ] **Step 3: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds.

If the compiler errors with `unmatched variant` on `ValType`, add the missing arm — for additional integer-ish variants use `host_read_typed::<T>` per the natural type; for variable-length variants use the inline `read_bytes`/`read_cstr` pattern.

- [ ] **Step 4: Pause for user commit**

---

## Task 7: Rewrite `host_write` with typed-dispatch helper

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (add helper fn + replace `host_write`)

- [ ] **Step 1: Add the typed write helper fn**

Near `host_read_typed` (added in Task 6), add:

```rust
/// Dispatch a typed write of `T` from the guest's `in_ptr`+`in_len` buffer to
/// `addr`. Returns OK or a negative status code.
fn host_write_typed<T: MemValue>(
    caller: &Caller<'_, HostState>,
    addr: MemAddr<ReadWrite>,
    in_ptr: i32,
    in_len: i32,
) -> i32 {
    let bytes = match read_guest(caller, in_ptr, in_len) {
        Some(b) => b,
        None => return status::ERR_BAD_TYPE,
    };
    let val = match T::from_le_bytes_spine(&bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    match api::write_t::<T>(addr, val) {
        Ok(()) => status::OK,
        Err(e) => i32::from(e),
    }
}
```

- [ ] **Step 2: Replace `host_write` with the ValType dispatcher**

Replace the entire `host_write` body (currently lines 74-79):

```rust
fn host_write(caller: Caller<'_, HostState>, addr: i64, ty: i32, in_ptr: i32, in_len: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    // SAFETY: writes are gated by `write_granted` at linker registration; reaching
    // host_write means the script declared write intent. The typed-write helper
    // delegates to api::write_t which calls the cache-validated backend.
    let addr = unsafe { MemAddr::<ReadWrite>::from_raw_writable(addr as u64) };
    match ty {
        ValType::U8  => host_write_typed::<u8 >(&caller, addr, in_ptr, in_len),
        ValType::U16 => host_write_typed::<u16>(&caller, addr, in_ptr, in_len),
        ValType::U32 => host_write_typed::<u32>(&caller, addr, in_ptr, in_len),
        ValType::U64 => host_write_typed::<u64>(&caller, addr, in_ptr, in_len),
        ValType::I8  => host_write_typed::<i8 >(&caller, addr, in_ptr, in_len),
        ValType::I16 => host_write_typed::<i16>(&caller, addr, in_ptr, in_len),
        ValType::I32 => host_write_typed::<i32>(&caller, addr, in_ptr, in_len),
        ValType::I64 => host_write_typed::<i64>(&caller, addr, in_ptr, in_len),
        ValType::F32 => host_write_typed::<f32>(&caller, addr, in_ptr, in_len),
        ValType::F64 => host_write_typed::<f64>(&caller, addr, in_ptr, in_len),
        // Variable-length writes are not supported through this host fn; the
        // raw `api::write` path did not support them either (it returned
        // ERR_BAD_TYPE on empty bytes). Preserve that semantic:
        ValType::Bytes | ValType::Cstr => status::ERR_BAD_TYPE,
    }
}
```

(Match is exhaustive over the 12 `ValType` variants — same set as Task 6.)

- [ ] **Step 3: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds.

- [ ] **Step 4: Pause for user commit**

---

## Task 8: Rewrite `host_write_if` with typed CAS pattern

**Files:**
- Modify: `crates/agent/src/runtime/mem_host.rs` (add helper fn + replace `host_write_if`)

**CAS atomicity note:** the old `api::write_if` was *not* truly atomic — it read, compared, then wrote under the cache mutex but with no compare-and-swap CPU primitive. The typed version preserves this exact semantic (read → compare → write); no atomic guarantee is added or removed. Concurrent writes between read and write can still cause TOCTOU. This matches the spec's risk note.

- [ ] **Step 1: Add the typed CAS helper fn**

Near the other helpers in `mem_host.rs`, add:

```rust
/// Typed compare-and-write: read current `T` at `addr`, compare to `exp_bytes`,
/// write `new_bytes` only on match. Returns OK on write, CHANGED on mismatch,
/// or a negative status code on read/write failure. Not atomic — read+compare+
/// write under the cache mutex but no CPU CAS primitive.
fn host_write_if_typed<T: MemValue + PartialEq>(
    caller: &Caller<'_, HostState>,
    addr: MemAddr<ReadWrite>,
    exp_bytes: &[u8],
    new_bytes: &[u8],
) -> i32 {
    let exp = match T::from_le_bytes_spine(exp_bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    let new = match T::from_le_bytes_spine(new_bytes) {
        Some(v) => v,
        None => return status::ERR_BAD_TYPE,
    };
    let cur: T = match api::read_t::<T, _>(addr.as_readonly()) {
        Ok(v) => v,
        Err(e) => return i32::from(e),
    };
    if cur != exp {
        return status::CHANGED;
    }
    match api::write_t::<T>(addr, new) {
        Ok(()) => status::OK,
        Err(e) => i32::from(e),
    }
}
```

- [ ] **Step 2: Replace `host_write_if` with the ValType dispatcher**

Replace the entire `host_write_if` body (currently lines 81-91):

```rust
fn host_write_if(caller: Caller<'_, HostState>, addr: i64, ty: i32, exp_ptr: i32, exp_len: i32, new_ptr: i32, new_len: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let exp_b = match read_guest(&caller, exp_ptr, exp_len) { Some(b) => b, None => return status::ERR_BAD_TYPE };
    let new_b = match read_guest(&caller, new_ptr, new_len) { Some(b) => b, None => return status::ERR_BAD_TYPE };
    // SAFETY: write_if is gated by write_granted at linker registration.
    let addr = unsafe { MemAddr::<ReadWrite>::from_raw_writable(addr as u64) };
    match ty {
        ValType::U8  => host_write_if_typed::<u8 >(&caller, addr, &exp_b, &new_b),
        ValType::U16 => host_write_if_typed::<u16>(&caller, addr, &exp_b, &new_b),
        ValType::U32 => host_write_if_typed::<u32>(&caller, addr, &exp_b, &new_b),
        ValType::U64 => host_write_if_typed::<u64>(&caller, addr, &exp_b, &new_b),
        ValType::I8  => host_write_if_typed::<i8 >(&caller, addr, &exp_b, &new_b),
        ValType::I16 => host_write_if_typed::<i16>(&caller, addr, &exp_b, &new_b),
        ValType::I32 => host_write_if_typed::<i32>(&caller, addr, &exp_b, &new_b),
        ValType::I64 => host_write_if_typed::<i64>(&caller, addr, &exp_b, &new_b),
        ValType::F32 => host_write_if_typed::<f32>(&caller, addr, &exp_b, &new_b),
        ValType::F64 => host_write_if_typed::<f64>(&caller, addr, &exp_b, &new_b),
        ValType::Bytes | ValType::Cstr => status::ERR_BAD_TYPE,
    }
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds.

If you see an "unused import" warning for `Value` in `mem_host.rs` (it was used only by the deleted `host_write_if` body), remove the `Value` import — Task 9 will also need to be careful about this.

- [ ] **Step 4: Pause for user commit**

---

## Task 9: Delete raw `external::api::{read, write, write_if}`; rename `read_t`/`write_t`; migrate `get_field` off raw path

**Files:**
- Modify: `crates/agent/src/external/api.rs` (delete raw 3 fns, rename `read_t`/`write_t`, update 3 test references)
- Modify: `crates/agent/src/internals/api.rs` (`get_field` switches from `ext::read` to typed-dispatch internally)

This is the cleanup task that consolidates all external-side dust.

- [ ] **Step 1: Migrate `get_field` off raw `ext::read`**

In `crates/agent/src/internals/api.rs`, `get_field`'s body currently calls `ext::read(addr, vt, vt.fixed_width().unwrap_or(8))`. Replace with typed dispatch:

```rust
pub fn get_field(instance: Instance, klass: KlassPtr, name: &str) -> Result<Value, i32> {
    let (offset, vt) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
    let addr_raw = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    let addr = MemAddr::<ReadOnly>::from_raw(addr_raw);
    // Dispatch on the discovered field ValType and produce a Value.
    let val = match vt {
        ValType::U8  => Value::U8 (ext::read_t::<u8 , _>(addr).map_err(i32::from)?),
        ValType::U16 => Value::U16(ext::read_t::<u16, _>(addr).map_err(i32::from)?),
        ValType::U32 => Value::U32(ext::read_t::<u32, _>(addr).map_err(i32::from)?),
        ValType::U64 => Value::U64(ext::read_t::<u64, _>(addr).map_err(i32::from)?),
        ValType::I8  => Value::I8 (ext::read_t::<i8 , _>(addr).map_err(i32::from)?),
        ValType::I16 => Value::I16(ext::read_t::<i16, _>(addr).map_err(i32::from)?),
        ValType::I32 => Value::I32(ext::read_t::<i32, _>(addr).map_err(i32::from)?),
        ValType::I64 => Value::I64(ext::read_t::<i64, _>(addr).map_err(i32::from)?),
        ValType::F32 => Value::F32(ext::read_t::<f32, _>(addr).map_err(i32::from)?),
        ValType::F64 => Value::F64(ext::read_t::<f64, _>(addr).map_err(i32::from)?),
        // Variable-length: not meaningful for instance fields by name.
        ValType::Bytes | ValType::Cstr => return Err(status::ERR_BAD_TYPE),
    };
    Ok(val)
}
```

(Verify `Value` variant names match `ValType` variant names; the existing `Value::decode` in `mem_value.rs:74-81` is the reference for the naming convention.)

This step is the last caller of raw `ext::read` — after this, no caller remains.

- [ ] **Step 2: Delete raw `read`, `write`, `write_if` in `external/api.rs`**

In `crates/agent/src/external/api.rs`, delete these three functions entirely:
- Lines 13-23: `pub fn read(addr: usize, ty: ValType, len: usize) -> Result<Value, i32>`
- Lines 34-40: `pub fn write(addr: usize, value: &Value) -> Result<(), i32>`
- Lines 44-56: `pub fn write_if(addr: usize, expected: &Value, new: &Value) -> Result<bool, i32>`

Also remove the `use agent_core::mem_value::{status, ValType, Value}` if `status`/`ValType`/`Value` are no longer used in the remaining file — keep what `read_bytes`/`read_cstr` and the renamed `read`/`write` still need (MemValue, MemAddr, MemError, ReadWrite). The compiler's "unused import" warning will guide you.

- [ ] **Step 3: Rename `read_t` → `read` and `write_t` → `write` in `external/api.rs`**

In the same file:
- Rename `pub fn read_t<T: MemValue, C>(...)` → `pub fn read<T: MemValue, C>(...)`.
- Rename `pub fn write_t<T: MemValue>(...)` → `pub fn write<T: MemValue>(...)`.

(No name conflict now because Step 2 deleted the raw versions.)

- [ ] **Step 4: Update the 3 compile-test references in `external/api.rs`**

Inside the `#[cfg(test)] mod spine_tests` block (lines 128-153):

```rust
        let _: fn(MemAddr<ReadOnly>)  -> Result<u32, MemError> = read::<u32, ReadOnly>;
        let _: fn(MemAddr<ReadWrite>) -> Result<u32, MemError> = read::<u32, ReadWrite>;
```

and

```rust
        let _: fn(MemAddr<ReadWrite>, u32) -> Result<(), MemError> = write::<u32>;
```

- [ ] **Step 5: Update all callers of `read_t` / `write_t` to use the new names**

The only runtime callers are in `mem_host.rs`:
- `host_read_typed` calls `api::read_t::<T, _>` → change to `api::read::<T, _>`
- `host_write_typed` calls `api::write_t::<T>` → change to `api::write::<T>`
- `host_write_if_typed` calls both `api::read_t::<T, _>` and `api::write_t::<T>` → change both

And in `internals/api.rs` (Step 1 of this task):
- `get_field`'s body calls `ext::read_t::<T, _>` → change to `ext::read::<T, _>`

- [ ] **Step 6: Build and verify**

Run: `cargo build --release -p agent`
Expected: succeeds with zero warnings about unused imports / unused fns. If a warning appears, address it before proceeding (this is the "no dead code added" acceptance criterion).

- [ ] **Step 7: Pause for user commit**

---

## Task 10: Verify on live game — auto-deploy + WASM probe parity

**Files:**
- No code changes; this is a verification task.

After Task 9, the build is green and DLLs are auto-deployed to the game directory (per the `deploy-setup` memory: `./deploy.sh` is the auto-deploy hook). The acceptance gate is that existing WASM probe scripts produce output byte-identical to a pre-B-5 baseline.

- [ ] **Step 1: Confirm clean build**

Run: `cargo build --release -p agent && cargo build --release -p agent-core`
Expected: both succeed with no errors and no new warnings.

- [ ] **Step 2: Confirm no raw `api::*` fns survive**

Run from the repo root:

```bash
grep -rn "external::api::read\b\|external::api::write\b\|external::api::write_if\b" crates/ --include="*.rs"
```

Expected: zero matches.

Then:

```bash
grep -rn "fn read_t\|fn write_t\|fn read_bytes_t\|fn read_cstr_t\|fn find_class_t\|fn find_method_t\|fn field_addr_t\|fn static_field_t\|fn klass_of_t\|fn invoke_method_t" crates/ --include="*.rs"
```

Expected: zero matches (all `_t` suffixes dropped).

- [ ] **Step 3: Confirm spine tests still pass**

Run: `cargo test -p agent-core`
Expected: all tests pass (the `spine.rs` integration test uses trait method syntax, not `_t` names; the compile-fail tests in `addr.rs` doc tests still compile).

Run: `cargo test -p agent --release` (the agent crate has no unit tests by design — this should succeed with 0 tests run, per the memory note).

- [ ] **Step 4: Live game verification — run an existing WASM probe**

Per the `wat-wasm-pipeline` memory: WASM modules are precompiled via `cargo run --example wat2wasm -p agent-core`. Identify an existing probe script that exercises read+write+find_class+field_info+get_field (typical candidates live under `crates/agent/tests/` or similar — pick the one that's most representative of the host-fn surface; the user has run it before).

Pre-B-5 baseline: this is the script's output from before the B-5 work began. If no baseline was captured, capture one from the current git branch's last shipped commit (`c77becd` — "Bed rock proven stronger this time") via a quick check-out, run, and capture-then-return-to-B-5.

Run the chosen probe script against the live game. Expected: byte-identical output to baseline. Differences flag a regression in the typed-dispatch path; investigate before declaring done.

- [ ] **Step 5: Audit-table self-update**

In `docs/superpowers/audits/` (the existing audit findings dir), record that B-5 shipped and the audit-table cells now read:

| Domain | WASM typed dispatch |
|---|---|
| External (mem) | ✅ |
| Internal (il2cpp) | ✅ |

(If no audit doc exists, just note it in the commit message — the memory `codebase-audit-findings.md` will get a memory-edit pass after the user confirms.)

- [ ] **Step 6: Pause for user commit + ship**

This is the final B-5 commit. After this, the brick is shipped. The user runs git commit per the project rule.

---

## Self-Review

Reviewing this plan against `docs/superpowers/specs/2026-05-30-b5-wasm-typed-dispatch-design.md`:

**Spec coverage:**
- ✅ Internal refactor only, ABI unchanged — Tasks 1-9 only touch Rust-side; wasmi linker registrations in `mem_host.rs:264-286` are untouched.
- ✅ Delete raw fns — internals raw deleted in Tasks 1-2; external raw deleted in Task 9.
- ✅ Dispatch lives in host fn — Tasks 6-8 build the ValType match site inside `host_read`/`host_write`/`host_write_if`.
- ✅ `field_info` / `get_field` evolve signatures — Tasks 3-4 (and Task 9 finalizes `get_field`'s body migration off raw `ext::read`).
- ✅ Uniform `_t` rename — Task 5 (internals + non-clashing externals) + Task 9 (external `read`/`write`, where the rename has to come after deletion of raw siblings to avoid a name clash).
- ✅ Out-of-scope hook fns — none of the hook host fns are touched.
- ✅ Out-of-scope `scan`/`regions` — these survive unchanged.
- ✅ No new wasmi linker registrations — verified across all tasks.
- ✅ Error mapping via `i32::from(MemError)` — used in every host helper.
- ✅ Acceptance criterion #1 (no raw greps) — Task 10 Step 2.
- ✅ Acceptance criterion #2 (no new `#[allow(dead_code)]`) — Task 9 Step 6.
- ✅ Acceptance criterion #3 (`cargo test -p agent-core` passes) — Task 10 Step 3.
- ✅ Acceptance criterion #4 (live WASM probe parity) — Task 10 Step 4.
- ✅ Acceptance criterion #5 (audit-table flip) — Task 10 Step 5.
- ✅ Acceptance criterion #6 (no new env gates) — implicit; no task adds env-gating.

**Placeholder scan:**
- The "verify ValType variant names" notes in Tasks 6-8 are instructions to the implementer, not placeholders — they point to a concrete next action (read the actual enum, add missing arms). Acceptable.
- The audit-doc-update step (Task 10 Step 5) hedges between "audit doc" and "memory edit" — this is an honest reflection of uncertainty about whether a separate audit doc exists; either action is concrete.

**Type consistency:**
- `find_class(name: &str) -> Option<KlassPtr>` — consistent across Task 1 definition and all callers.
- `find_method(klass: KlassPtr, name: &str, argc: u32) -> Option<MethodPtr>` — consistent across Task 2 definition and `host_find_method`.
- `klass_of(instance: Instance) -> Option<KlassPtr>` — consistent across Task 2 definition and `host_klass_of`.
- `static_field(klass: KlassPtr, name: &str) -> Option<MemAddr<ReadWrite>>` — consistent across Task 2 definition and `host_static_field`.
- `field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType)>` — consistent (Task 3) across `field_addr`, `get_field`, and `host_field_info` call sites.
- `get_field(instance: Instance, klass: KlassPtr, name: &str) -> Result<Value, i32>` — consistent (Task 4) with `host_get_field`.
- `host_read_typed<T: MemValue>` / `host_write_typed<T: MemValue>` / `host_write_if_typed<T: MemValue + PartialEq>` — generic bounds match the trait methods they invoke.
- `api::read::<T, _>(addr)` — final name post-Task 9; intermediate references to `api::read_t::<T, _>` in Tasks 6-8 are intentional (rename deferred to Task 9 to avoid mid-task name clashes).

All identifiers consistent. No type drift between definitions and call sites.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-30-b5-wasm-typed-dispatch-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
