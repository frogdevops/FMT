# Trait-Architecture Spine — Design

**Date:** 2026-05-28
**Branch:** `ffi-class-table`
**Status:** approved, ready for plan-writing

---

## Goal

Lay the structural backbone that lets the three Spec-2 domains — **mem / il2cpp / proto** — compose by **type** rather than by raw `u64` handoff. The spine catches agent-side wrong-by-construction at `cargo check`, gives every future op (invoke / hook / poll / inject / in-flight modify) a shared shape so they don't proliferate ad-hoc ABIs, and gives the post-merge ISLI research a contract to synthesize toward.

Charter constraint preserved: **actors hit their own landmines; the stage must hold.** The spine is how the stage holds at the type level — independent of how complex or careless an actor script becomes.

## The specific problem this solves

Today every cross-domain handle is `u64`. Three concrete bug classes are silent at compile time:

1. **Handle confusion** — a klass-struct pointer accidentally passed to `mem.read`.
2. **Width mismatch** — reading 4 bytes into a `u8` register.
3. **Capability violation** — writing into a region (e.g. klass internals) that should never be mutated by a script.

The recent audit found two of these in shipped code (`read_cstr` width-confused, unvalidated FFI on a klass pointer). A type-correct internal API prevents the next ones from compiling.

## Non-goals (what does NOT change)

- **Script-facing WASM ABI** stays byte-for-byte identical. Scripts still call `mem.read(addr, U32_tag, out_ptr, out_cap) -> i32` etc. No actor-side breakage.
- **Existing raw-`u64` agent-internal API** stays callable. The typed surface lands beside it; raw becomes a 2-line bridge. Removing raw bridges is a **post-merge** cleanup, not this brick.
- **No lifetimes on handles.** Considered (Approach C in brainstorming) and rejected — borrow-checker tax propagates through every struct/Box/Mutex without proportional safety win.
- **No generic `Resolve<T>` / `Iter<Item=T>` traits.** Additive; can land later non-breakingly when a concrete need appears.

---

## Handle catalogue + capability markers

Five `#[repr(transparent)]` newtype handles around `u64` (zero runtime cost):

| Newtype | Identifies |
|---|---|
| `MemAddr<C = ReadOnly>` | memory address + capability |
| `KlassPtr` | `Il2CppClass*` |
| `MethodPtr` | `MethodInfo*` |
| `Instance` | object instance pointer |
| `FrameSeq` | protocol bookmark (already shaped) |
| `SocketHandle` | a tracked socket (for proto.send when 3b lands) |

Capability markers (zero-sized): `pub struct ReadOnly;` and `pub struct ReadWrite;`. **Only `MemAddr` carries a marker** — `KlassPtr`/`MethodPtr`/`Instance` have no read/write distinction (you don't "write" a klass).

**Conversion rules** (the discipline that makes markers honest):

```rust
MemAddr::from_raw(u64)                      // -> MemAddr<ReadOnly>           safe default
unsafe fn from_raw_writable(u64)            // -> MemAddr<ReadWrite>          caller asserts
MemAddr<ReadWrite>::as_readonly(self)       // -> MemAddr<ReadOnly>           safe downgrade
unsafe fn MemAddr<ReadOnly>::mark_writable  // -> MemAddr<ReadWrite>          unsafe upgrade
fn as_u64(&self) -> u64                     // for FFI / dispatcher boundaries only
```

**Origin discipline** (capability set by the function that produces the address, based on intent):

| Producer | Capability returned | Why |
|---|---|---|
| `il2cpp::field_addr` | `MemAddr<ReadWrite>` | instance fields are writable by intent |
| `il2cpp::static_field` | `MemAddr<ReadWrite>` | static fields are writable by intent |
| `mem::scan` results | `MemAddr<ReadOnly>` | caller knows nothing — must explicitly upgrade |
| (future) `il2cpp::klass_data_addr` | `MemAddr<ReadOnly>` | klass internals must not be mutated by a script |

---

## `MemValue` trait + `MemError`

```rust
pub trait MemValue: Sized + Copy {
    fn read_at<C>(addr: MemAddr<C>) -> Result<Self, MemError>;
    fn write_at(addr: MemAddr<ReadWrite>, val: Self) -> Result<(), MemError>;
    fn val_type() -> ValType;   // for the WASM-boundary dispatcher
}
```

**Impls:** `u8 · u16 · u32 · u64 · i8 · i16 · i32 · i64 · f32 · f64`.

**Variable-length values are NOT MemValue impls** (they need a length arg, don't fit `Sized + Copy`). They live as free functions: `mem::read_bytes(addr, len)`, `mem::read_cstr(addr, cap)`.

**Read works on any capability; write requires `MemAddr<ReadWrite>`** — that bound *is* the compile-time gate.

Error model:

```rust
pub enum MemError {
    Unreadable, Unwritable, BadType,
    BufTooSmall, Denied, Changed,
}
impl From<MemError> for i32 { /* -> existing mem_value::status codes */ }
```

**Wrong-by-construction, illustrated:**

```rust
let p:    KlassPtr           = il2cpp::find_class("Player")?;
let addr: MemAddr<ReadWrite> = il2cpp::field_addr(p, "hp", inst)?;
let hp:   u32                = mem::read(addr)?;                  // typed read
mem::write(addr, 100u32)?;                                        // OK
mem::write(addr.as_readonly(), 100u32)?;                          // ❌ trait bound fails
mem::read::<u8>(some_klass_ptr)?;                                 // ❌ KlassPtr is not MemAddr<_>
let v: u8 = mem::read::<u32>(addr)?;                              // ❌ type mismatch
```

Three distinct mistake classes, all caught by `cargo check`.

---

## Crate split

Pure types in `agent-core` so the Linux test suite keeps passing without FFI shims; the implementations stay where they are.

| `agent-core` (host-testable, no FFI) | `agent` (Windows, FFI) |
|---|---|
| `spine::{MemAddr<C>, KlassPtr, MethodPtr, Instance, FrameSeq, SocketHandle}` | `mem::read::<T>` / `mem::write::<T>` impls (over `cache::read_*` / `guarded_write`) |
| `spine::{ReadOnly, ReadWrite}` markers | `il2cpp::{find_class, find_method, field_addr, static_field, …}` typed wrappers |
| `spine::MemValue` trait + impls for `u8..u64 / i8..i64 / f32 / f64` | `proto::{poll, send, …}` typed surface (when 3a lands) |
| `spine::MemError` + `From<MemError> for i32` | WASM host-fn dispatchers |

**Module layout:** `agent-core/src/spine/{mod, addr, handles, value, error}.rs`.

---

## Per-domain typed surfaces (this brick)

```rust
// crates/agent/src/internals/api.rs  — typed siblings added beside existing raw fns
pub fn find_class(name: &str) -> Option<KlassPtr>;
pub fn find_method(k: KlassPtr, name: &str, argc: u32) -> Option<MethodPtr>;
pub fn field_addr(k: KlassPtr, name: &str, inst: Instance) -> Option<MemAddr<ReadWrite>>;
pub fn static_field(k: KlassPtr, name: &str) -> Option<MemAddr<ReadWrite>>;
pub fn klass_of(inst: Instance) -> Option<KlassPtr>;
// klass_data_addr (a ReadOnly helper for reading klass-struct fields) is
// reserved for a future brick; not required by this one.

// crates/agent/src/external/api.rs
pub fn read<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError>;
pub fn write<T: MemValue>(addr: MemAddr<ReadWrite>, v: T) -> Result<(), MemError>;
pub fn read_bytes<C>(addr: MemAddr<C>, len: usize) -> Result<Vec<u8>, MemError>;
pub fn read_cstr<C>(addr: MemAddr<C>, cap: usize) -> Result<String, MemError>;
```

Each typed function wraps the existing raw implementation in 2–3 lines. Raw functions stay callable for the existing host-fn dispatchers.

---

## WASM host-fn dispatchers (the target shape)

This is the **end-state shape** every host fn converges on: **parse tagged ABI → call typed core → encode result.** New host fns (for invoke / hook / poll / inject) land in this shape natively. Existing host fns keep their current bodies in this brick (see Migration discipline) and are re-pointed at typed core post-merge.

```rust
fn host_mem_read(addr: i64, ty_tag: i32, out_ptr: i32, out_cap: i32) -> i32 {
    let addr = MemAddr::<ReadOnly>::from_raw(addr as u64);
    let bytes = match ValType::from_tag(ty_tag) {
        Some(ValType::U32) => api::read::<u32, _>(addr).map(|v| v.to_le_bytes().to_vec()),
        Some(ValType::U64) => api::read::<u64, _>(addr).map(|v| v.to_le_bytes().to_vec()),
        // ... one arm per MemValue impl
        _ => return MemError::BadType.into(),
    };
    write_to_wasm_mem(out_ptr, out_cap, bytes)   // -> i32 status (MemError::into)
}
```

**Properties:**
- Script-facing ABI is byte-for-byte identical to today.
- Typed core never sees `wasmi` types.
- Adding a new value type = one `MemValue` impl + one match arm. Mechanical.

---

## Migration discipline (scope of this brick)

| Code | Treatment in this brick |
|---|---|
| New ops (invoke / hook / poll / inject / in-flight modify) | Born typed. No raw twin. |
| Existing 6 internals ops (`find_class`, `find_method`, `field_info`, `get_field`, `static_field`, `klass_of`) | Keep raw `u64` signature; add typed sibling that delegates to the same body. |
| Existing 4 mem ops (`read`, `write`, `read_bytes`, `read_cstr`) | Keep raw; add typed sibling. |
| Existing WASM host-fn bodies | **Unchanged** — keep calling raw. Re-pointing them to typed is a post-merge cleanup. |
| Existing raw bridges | Marked `#[deprecated(note = "use spine-typed sibling")]` for visibility; deletion deferred. |

**No churn to working code in this brick.** The spine is purely additive.

---

## Cross-brick proof — "does it hold under complexity?"

Stress-test: every planned op slots into the spine without changing it.

| Future op | Typed signature | New spine pieces |
|---|---|---|
| 2c invoke | `il2cpp::invoke(m: MethodPtr, args: &[Value]) -> Result<Value, MemError>` | none |
| 2d hook | `il2cpp::hook(m: MethodPtr, h: HookHandler) -> Result<HookGuard, MemError>` | one handle (`HookGuard`) |
| 3a poll | `proto::poll(since: FrameSeq) -> Result<(Vec<RawFrame>, FrameSeq), MemError>` | none — `FrameSeq` already in catalogue |
| 3b inject | `proto::send(s: SocketHandle, bytes: &[u8]) -> Result<(), MemError>` | none — `SocketHandle` already in catalogue |
| in-flight modify | `proto::on_frame(filter, replace: impl Fn(&mut RawFrame))` | none |

**Cross-domain chain** (four domains, eight lines, every handoff type-checked):

```rust
let p:        KlassPtr           = il2cpp::find_class("Player")?;
let inst:     Instance           = il2cpp::singleton(p)?;
let hp_addr:  MemAddr<ReadWrite> = il2cpp::field_addr(p, "hp", inst)?;
let old:      u32                = mem::read(hp_addr)?;
let m:        MethodPtr          = il2cpp::find_method(p, "TakeDamage", 1)?;
              il2cpp::invoke(m, &[Value::U32(old / 2)])?;
let new:      u32                = mem::read(hp_addr)?;
```

That's the magic, made concrete.

---

## Testing

In `agent-core` (Linux-runnable, no FFI):
- `MemAddr<C>` size = 8 bytes (`std::mem::size_of` assertion).
- Conversion identities: `from_raw(x).as_u64() == x` for both capabilities.
- Trait bound proof: a test module that fails to compile when `mem::write(addr.as_readonly(), …)` is attempted (use `trybuild` or a `compile_fail` doc-test).
- `MemValue` round-trip via a stub backend: `write_at` then `read_at` returns input for every numeric impl.
- `MemError::from(_)` round-trips to the same i32 codes the existing `mem_value::status` constants use.

In `agent` (PW gate, manual):
- The existing WASM probes (`test_internals2.wat` etc.) keep passing — proves no regression in the script-facing ABI.

---

## Risks + tradeoffs

- **Verbosity in the typed core.** Generic `MemValue` dispatch adds a few lines per host fn. Mitigated by the dispatcher being mechanical and 1:1 with the impl list.
- **Two parallel surfaces (raw + typed) for the lifetime of this brick.** A real, deliberate cost. Justified by zero-churn-to-working-code and a clean post-merge removal pass.
- **`unsafe` on capability upgrade (`mark_writable`, `from_raw_writable`).** Intentional — the caller is asserting that a piece of memory is safely writable. This is the same trust boundary `guarded_write` already enforces at runtime; the `unsafe` keyword makes the assertion visible at the call site.
- **Capability marker is intent, not enforcement.** Runtime safety still lives in `guarded_write` (VirtualProtect-checked). The marker prevents *the agent's own code* from accidentally calling `mem::write` on something it shouldn't — that's where it earns its keep.

---

## What ships when this brick lands

- `agent-core/src/spine/` module (handles, markers, MemValue trait, MemError).
- Typed siblings added to `internals::api` and `external::api`.
- All existing tests + WASM probes still pass.
- Zero changes to deployed behavior (script-facing surface unchanged).
- Ready for 2c invoke and 2d hook to land natively on the spine, no further structural work.
