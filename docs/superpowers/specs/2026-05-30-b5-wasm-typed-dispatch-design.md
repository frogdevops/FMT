# B-5 WASM Typed Dispatch — Design

**Date:** 2026-05-30
**Status:** Approved (brainstorm complete; ready for plan)
**Predecessor:** B-4 Trait Spine (`2026-05-28-trait-spine-design.md`, shipped 2026-05-30)
**Successor:** B-6 Stability Pass + env-gate removal

## Motivation

B-4 landed the trait spine — `Read<T>`, `Write<T>`, `Iter<T>` traits with typed handles (`MemAddr<C>`, `KlassPtr`, `MethodPtr`, `FieldAddr`, `Instance`) — and typed `_t` siblings on every domain API fn. But the WASM host boundary still calls the *untyped* layer: `mem_host.rs` dispatches to `api::read`, `api::find_class`, `api::field_info`, etc. The typed surface exists; nothing in production uses it (only compile-fail tests do).

The B-4 spec promised this cleanup explicitly:

> *"Removing raw bridges is a post-merge cleanup, not this brick."*
> *"Re-pointing them to typed is a post-merge cleanup."*

**B-5 IS that cleanup.** It deletes the raw bridges, re-points host fns at the typed surface, evolves remaining raw-arg signatures to typed handles, and renames the `_t` siblings to clean names (since the disambiguator role ends with the raw deletion).

After B-5: the typed surface is not just *canonical*, it is the *only* surface. The audit table's "❌ Still raw i64/i32" for External and Internal WASM dispatch goes to ✅, with no asterisk.

## Scope shape

**Internal refactor only. The WASM ABI does not change.** Guest scripts and existing `.wasm`/`.wat` files keep working with no recompilation. All changes are inside the agent crate:

- `mem_host.rs` host-fn implementations
- `external::api` and `internals::api` module surfaces
- `diagnostics::klass_probe` call-site migrations (2 sites)
- B-4 compile-fail test references (3 sites in `api.rs`)

No new wasmi linker registrations. No new host fns. No new ABI shapes. The boundary stays at the same line of code; only the Rust side of that line changes.

## Locked decisions

| # | Decision | Rationale |
|---|---|---|
| 1 | Internal refactor; WASM ABI unchanged | Bedrock-first. Optimize ABI later if profiling shows need. |
| 2 | Delete raw `api::*` fns in this brick | B-4 spec explicitly deferred this; B-5 fulfils the promise. One path is the contract. |
| 3 | Runtime-`ValType` → static-`T` dispatch lives in the host fn | `host_read`/`host_write`/`host_write_if` are *literally* the boundary where WASM's untyped ABI meets Rust's typed world. The match-on-`ValType` belongs there. No new shim fn (`read_dyn`) — that would be a raw bridge under a different name. |
| 4 | `field_info` / `get_field` evolve signatures (take `KlassPtr` / `Instance`); no `_t` suffix | Their returns (`(offset, ValType)` and `Value`) are already the canonical typed reflection/dynamic-data currency. Only the args need typing. |
| 5 | All surviving `_t` siblings rename uniformly (drop suffix) | `_t` was a transition disambiguator. With the raw sibling deleted, the suffix names the absence of something. Avoids "infectious `_t`" (a `_t` fn calling another `_t` fn) — particularly relevant for `field_addr` calling `field_info`. |

## Concrete migration

### External (`crates/agent/src/external/api.rs`)

**Delete:**
```rust
pub fn read(addr: usize, ty: ValType, len: usize) -> Result<Value, i32>      // DELETE
pub fn write(addr: usize, value: &Value) -> Result<(), i32>                  // DELETE
pub fn write_if(addr: usize, expected: &Value, new: &Value) -> Result<bool, i32>  // DELETE
pub fn scan(...) -> Vec<usize>                                               // STAYS (no typed sibling; scan is inherently raw pattern → addresses)
pub fn regions() -> Vec<(usize, usize, u32)>                                 // STAYS (region enumeration is inherently raw)
```

**Rename (drop `_t`):**
```rust
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError>
    → pub fn read<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError>

pub fn write_t<T: MemValue>(addr: MemAddr<ReadWrite>, val: T) -> Result<(), MemError>
    → pub fn write<T: MemValue>(addr: MemAddr<ReadWrite>, val: T) -> Result<(), MemError>

pub fn read_bytes_t<C>(addr: MemAddr<C>, len: usize) -> Result<Vec<u8>, MemError>
    → pub fn read_bytes<C>(addr: MemAddr<C>, len: usize) -> Result<Vec<u8>, MemError>

pub fn read_cstr_t<C>(addr: MemAddr<C>, cap: usize) -> Result<String, MemError>
    → pub fn read_cstr<C>(addr: MemAddr<C>, cap: usize) -> Result<String, MemError>
```

**No new `write_if` typed variant** — `write_if` was the untyped CAS path; today only `host_write_if` uses it. The host fn switches to a typed CAS pattern using `Read` + `Write` traits inline (read current, compare, write new, return `CHANGED` on mismatch) **OR** a new typed helper is added if the dispatch logic warrants it (deferred to plan).

### Internal (`crates/agent/src/internals/api.rs`)

**Delete:**
```rust
pub fn find_class(name: &str) -> u64                                          // DELETE
pub fn find_method(klass: u64, name: &str, argc: u32) -> u64                  // DELETE
pub fn klass_of(instance: u64) -> u64                                         // DELETE
pub fn static_field(klass: u64, name: &str) -> u64                            // DELETE
```

**Rename (drop `_t`):**
```rust
pub fn find_class_t   → pub fn find_class    (returns Option<KlassPtr>)
pub fn find_method_t  → pub fn find_method   (takes KlassPtr, returns Option<MethodPtr>)
pub fn field_addr_t   → pub fn field_addr    (takes KlassPtr+Instance, returns Option<FieldAddr>)
pub fn static_field_t → pub fn static_field  (takes KlassPtr, returns Option<MemAddr<ReadWrite>>)
pub fn klass_of_t     → pub fn klass_of      (takes Instance, returns Option<KlassPtr>)
pub fn invoke_method_t → pub fn invoke_method (takes MethodPtr+Option<Instance>+&[InvokeArg])
```

**Evolve signatures (take typed args, returns unchanged):**
```rust
pub fn field_info(klass: u64, name: &str) -> Option<(usize, ValType)>
    → pub fn field_info(klass: KlassPtr, name: &str) -> Option<(usize, ValType)>

pub fn get_field(instance: u64, klass: u64, name: &str) -> Result<Value, i32>
    → pub fn get_field(instance: Instance, klass: KlassPtr, name: &str) -> Result<Value, i32>
```

`field_addr` already calls `field_info` internally; after this change the call becomes `field_info(klass, name)?` (the `.as_u64()` disappears, types flow naturally).

### Host fns (`crates/agent/src/runtime/mem_host.rs`)

**`host_read` — becomes the dispatch site.** ~10-arm match on `ValType` that picks `T` for `api::read::<T, _>`:

```rust
fn host_read(mut caller: Caller<'_, HostState>, addr: i64, ty: i32, len: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let ty = match ValType::from_tag(ty as u8) { Some(t) => t, None => return status::ERR_BAD_TYPE };
    let addr = unsafe { MemAddr::<ReadOnly>::from_raw(addr as u64) };
    let bytes: Vec<u8> = match ty {
        ValType::U8  => api::read::<u8,  _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::U16 => api::read::<u16, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::U32 => api::read::<u32, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::U64 => api::read::<u64, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::I8  => api::read::<i8,  _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::I16 => api::read::<i16, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::I32 => api::read::<i32, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::I64 => api::read::<i64, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::F32 => api::read::<f32, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::F64 => api::read::<f64, _>(addr).map_err(i32::from)?.to_le_bytes_buf(),
        ValType::Bytes => api::read_bytes(addr, len.max(0) as usize).map_err(i32::from)?,
        ValType::Cstr  => api::read_cstr(addr, len.max(0) as usize).map_err(i32::from)?.into_bytes(),
    };
    if bytes.len() > out_cap.max(0) as usize { return status::ERR_BUF_TOO_SMALL; }
    if !write_guest(&mut caller, out_ptr, &bytes) { return status::ERR_BUF_TOO_SMALL; }
    bytes.len() as i32
}
```

(Note: the `?` in this sketch implies a closure or `(|| -> Result<_, i32> { ... })()` wrapper, or the bodies use `.map(...)?` chains. Exact form deferred to plan.)

**`host_write` and `host_write_if` — same pattern.** Match on `ValType`, decode bytes into the static `T`, call `api::write::<T>(addr, value)`. For `write_if`, the typed CAS reads-compares-writes inline (or via a small helper).

**Migration of metadata host fns:**

```rust
fn host_find_class(...) -> i64 {
    api::find_class(&name).map(|k| k.as_u64() as i64).unwrap_or(0)
}

fn host_find_method(...) -> i64 {
    let klass = KlassPtr::from_raw(klass as u64);
    api::find_method(klass, &name, argc.max(0) as u32)
        .map(|m| m.as_u64() as i64).unwrap_or(0)
}

fn host_klass_of(_caller, instance: i64) -> i64 {
    let instance = Instance::from_raw(instance as u64);
    api::klass_of(instance).map(|k| k.as_u64() as i64).unwrap_or(0)
}

fn host_static_field(...) -> i64 {
    let klass = KlassPtr::from_raw(klass as u64);
    api::static_field(klass, &name).map(|a| a.as_u64() as i64).unwrap_or(0)
}

fn host_field_info(...) -> i64 {
    let klass = KlassPtr::from_raw(klass as u64);
    match api::field_info(klass, &name) {
        Some((offset, vt)) => ((vt as u8 as i64) << 32) | (offset as i64),
        None => -1,
    }
}

fn host_get_field(...) -> i32 {
    let instance = Instance::from_raw(instance as u64);
    let klass = KlassPtr::from_raw(klass as u64);
    // body otherwise unchanged
}
```

`host_invoke` already routes through `invoke_method_t` — only the rename touches it (`invoke_method_t` → `invoke_method`).

### Diagnostics (`crates/agent/src/diagnostics/klass_probe.rs`)

Two call sites migrate (lines 79, 110):

```rust
// Before:
let klass = api::find_class(cname);            // line 79
let klass = api::find_class("Player") as usize;  // line 110

// After:
let klass = api::find_class(cname).map(|k| k.as_u64()).unwrap_or(0);
let klass = api::find_class("Player").map(|k| k.as_u64() as usize).unwrap_or(0);
```

(Or use `unwrap_or_default()` if the diagnostic logic can treat `KlassPtr::null()` cleanly — TBD by the plan.)

### Tests

The three references in `external/api.rs:142-151` update:
```rust
let _: fn(MemAddr<ReadOnly>)  -> Result<u32, MemError> = read::<u32, ReadOnly>;
let _: fn(MemAddr<ReadWrite>) -> Result<u32, MemError> = read::<u32, ReadWrite>;
let _: fn(MemAddr<ReadWrite>, u32) -> Result<(), MemError> = write::<u32>;
```

The compile-fail tests in `agent-core/tests/spine.rs` reference trait methods (`addr.read::<u32>()`, `addr.write(v)`) and do *not* touch `_t` names — unchanged.

## Out of scope

**Hook host fns** (`host_hook_arg`, `host_hook_set_arg`, `host_hook_set_return`, `host_hook_this`, `host_call_original`) are byte/raw-oriented because hook arg-slot reads are inherently dynamic — the method signature isn't known to the hook handler until runtime. They call `hook_runtime::api` directly, which has no typed siblings to dispatch through. These stay as-is. *They are not parallel-surface debt; they are the correct shape for their inherently-dynamic role.*

**`api::scan` and `api::regions`** (external) stay raw. They have no typed sibling and no natural typed signature — `scan` takes a byte pattern and returns raw addresses (that's its purpose); `regions` enumerates memory regions as `(base, size, prot)` tuples. The "raw" framing doesn't apply; they're already at the right abstraction level. They survive B-5 unchanged.

**WASM ABI changes** (new typed host fns like `mem.read_u32`). Deferred until profiling demonstrates the `encode/decode` round-trip is a real bottleneck. Bedrock-before-capability.

**Removal of `Value` type or `ValType` tag.** Both remain canonical for dynamic-data currency (hook args, invoke args, get_field return). Their *required-intermediate* role in primitive reads/writes ends; their *legitimate* role in dynamic contexts continues.

## Error mapping

Today's `From<MemError> for i32` and `From<InvokeError> for i32` impls (per audit memory: distinct ranges `-1..-5`, `-100..-106`, `-200..-205`, no overlap) handle the boundary conversion uniformly. Host fns that previously returned `status::ERR_*` constants from untyped api calls now do `api::read(...).map_err(i32::from)?` (or equivalent). Host-side errors (buffer-too-small, malformed ValType tag, guest-memory-unreadable) continue to use the `status::ERR_*` constants directly — those are host concerns, not memory-backend errors.

No new error variants. No new conversion impls.

## Testing strategy

**No new tests required.** The refactor is mechanical: same WASM behavior, typed Rust path underneath. The safety net is the existing test surface:

1. **B-4 compile-fail tests** (`agent-core/tests/spine.rs`) — already prove the type-system enforces `ReadOnly`/`ReadWrite` discipline; unaffected by rename.
2. **B-4 typecheck tests** (`external/api.rs:142-151`) — rename-touched, but their *purpose* (proving signatures accept both capabilities) survives.
3. **Live WASM probes** — the existing scripts under `crates/agent/tests/*.wasm` (and the `wat2wasm` build pipeline) exercise the full host-fn surface against the live game. Pre/post B-5 run must produce identical script output. **This is the regression gate.**

If any test wants to be added: a Linux-side unit test in `agent-core` covering the new typed-arg `field_info(klass: KlassPtr, ...)` signature would prove the orphan-rule discipline once more — but it's not load-bearing for B-5 acceptance.

## Risks and non-risks

**Non-risks (mechanically safe):**
- Rename is purely lexical; the compiler catches every missed call site.
- Deletion of unused raw fns: the compiler refuses to build until every caller is migrated.
- `host_read`/`host_write` match-on-`ValType` is exhaustive — the compiler enforces every variant is handled.
- No new unsafe. The few `unsafe { MemAddr::from_raw(...) }` in host fns mirror the existing `MethodPtr::from_raw(method_ptr as u64)` pattern in `host_install_hook` (already shipped).

**Real risks:**
- **CAS semantics of `write_if`.** Today `api::write_if` does an internal compare-and-swap with a single MemError-or-status return. After B-5, the host fn does read-compare-write inline. Concurrent writes between the read and the write would not be atomic — but neither was the old path (it wasn't truly atomic CAS, just a read-then-write under the cache mutex). The plan must verify the old behavior is preserved, not improved or regressed. If a small typed `write_if` helper is needed, the plan adds it; mention this as a plan-time decision point.
- **`KlassPtr::from_raw(0)` semantics.** When WASM passes `klass: i64 = 0` (e.g. find_class miss propagated through the script), `KlassPtr::from_raw(0)` produces a null-ish handle. The downstream typed fns must treat this as `None` cleanly (most already do via `0 → None` matches). Verify per call site.
- **Diagnostic call-site behavior change.** `klass_probe.rs` migrations must preserve the exact "0 means missing" semantics the probe logic expects. Mechanical but checkable.

## Acceptance criteria

1. `grep -rn "external::api::read\|external::api::write\|api::find_class(" crates/` returns zero matches (raw fns gone).
2. `cargo build --release` succeeds with no `#[allow(dead_code)]` markers added for B-5.
3. `cargo test -p agent-core` passes (compile-fail and typecheck tests survive the rename).
4. Live WASM probe scripts run against the game and produce output byte-identical to a pre-B-5 baseline.
5. The audit-table cell "Internal — WASM typed dispatch" flips from ❌ to ✅. The cell "External — WASM typed dispatch" likewise.
6. No new env gates introduced. (B-6 *removes* env gates; B-5 must not add any.)

## Size estimate

| Surface | Lines touched |
|---|---|
| `external/api.rs` deletions | ~40 (3 fns gone) |
| `external/api.rs` renames | ~4 fn signature edits + 3 test references |
| `internals/api.rs` deletions | ~30 (4 fns gone) |
| `internals/api.rs` renames | ~6 fn signature edits |
| `internals/api.rs` signature evolutions | ~6 lines (`field_info`, `get_field` arg types) |
| `mem_host.rs` host-fn rewrites | ~60 (`host_read`/`host_write`/`host_write_if` match expansions + metadata host fns boundary typing) |
| `diagnostics/klass_probe.rs` migration | ~4 (2 call sites, mechanical) |
| **Net** | **~150 lines, mostly mechanical** |

Matches the memory estimate ("~150-200 lines refactor"). No surprises.

## Follow-on (not B-5)

After B-5 ships:
- **B-6 Stability Pass + env-gate removal** — with the typed bridge proven stable, remove `FROG_VALUETYPE_PROBE`, `FROG_MEM_PROBE`, etc.; make `FROG_WASM_WRITE` default-on. This is the *removal* of diagnostic scaffolding, which is itself proof-of-stability work.
- **B-7 Protocol API** — protocol-domain analog of what External + Internal now have (host fns + spine traits, typed-by-default convention). Prerequisite for B-8 In-flight Modify.
