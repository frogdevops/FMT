# B-4: Trait-Architecture Spine — Design

**Date:** 2026-05-30
**Branch:** `ffi-class-table` (or successor)
**Status:** approved, ready for plan-writing
**Builds on:** B-3 Substrate Finishing (H12 + audit) — shipped + game-verified
**Banked from:** [[spec2-domain-audit-and-cleanup]] memory ("trait-architecture spine for Spec 2 — TRAITS FIRST, DISCOVERY LATER")

---

## Goal

Turn the existing typed-but-mostly-unused `_t` siblings into a **trait architecture** that lets the 3 domains (external, internal, protocol) compose uniformly. A script's `addr.read::<u32>()` works whether `addr` is a memory address, an il2cpp field address, or a packet byte position — the seam between domains becomes EMERGENT via trait bounds, not buried in domain-specific function names.

Bundled secondary: **strengthen the two loose Phase 2 probes** (`parameters_off`, `return_type_off`) with structural validators so they can use the relaxed Phase 2 gate safely — closes the workaround from B-3's last fix.

## The bedrock principle, applied to API design

> **Behavior lives on the handle, not in domain-specific function names. A handle's type DECLARES its capabilities via trait impls. Composition is type-driven: a script binding `T: Read<u32> + Write<u32>` works against any handle whose type system permits it. YAGNI ruthlessly — one method per trait until a real caller demands more.**

## Non-goals (deferred)

| Item | Deferred to |
|---|---|
| Defaults-if-no-override WASM ABI (script calls `read(addr)` defaulting to a primary type) | Own brainstorm — ABI-shaping; not a pure-Rust trait question |
| Batch / CAS / offset variants on Read/Write traits | Add when a real caller asks (YAGNI) |
| HS Hook H13 trampoline crash diagnosis | Own micro-brick (asm instrumentation work) |
| Iter impls for newer collections (e.g., type table, image table) | Add when a real caller asks |
| Removing the `_t` façade functions | B-4 keeps them; future micro-brick can retire if call sites all migrate to method-call style |

## Strict no-hardcoding discipline

Per [[no-hardcoding-adaptive-resolution]] memory: the probe strengthening MUST use structural validators (walk pointed-to memory shape, cross-check against known-shape anchors). NO game-specific paths, NO version checks, NO magic numbers tied to specific Unity builds.

---

## Section 1 — The three trait definitions

New module `crates/agent-core/src/spine/access.rs`, re-exported via `spine/mod.rs`. Pure agent-core, no FFI, Linux-unit-testable.

```rust
// crates/agent-core/src/spine/access.rs

use crate::mem_value::MemValue;
use crate::spine::error::MemError;

/// Read a typed value of `T` from this handle. Capability-disciplined: any
/// handle the type system permits via `impl Read<T>` is safe to read from.
pub trait Read<T: MemValue> {
    fn read(&self) -> Result<T, MemError>;
}

/// Write a typed value of `T` through this handle. Capability-disciplined:
/// only handles whose impl explicitly opts in are writable. `MemAddr<ReadOnly>`
/// has no `Write<T>` impl, so `read_only_addr.write(...)` won't compile.
pub trait Write<T: MemValue> {
    fn write(&self, value: T) -> Result<(), MemError>;
}

/// Lazily iterate items of type `T`. The associated `Iter` type lets impls
/// define their own state struct (e.g., walking a FieldInfo array with a klass
/// cursor) without allocating a Vec.
pub trait Iter<T> {
    type Iter: Iterator<Item = T>;
    fn iter(&self) -> Self::Iter;
}
```

**Single-method-per-trait discipline (YAGNI):** every additional method commits every future impl to providing it. Batch reads, CAS, offset-variants all get added when a real caller demands — not before. The opposite anti-pattern is the existing typed `_t` siblings: 9 of 10 have zero non-test callers because they were shipped ahead of need.

**Trait bounds:**
- `Read<T>` / `Write<T>` require `T: MemValue` — inherits the existing typed-byte vocabulary; no new value encoding.
- `Iter<T>` has NO `MemValue` bound — items can be handles (`KlassPtr → Iter<FieldInfo>`), not just primitives.

**Why associated type for `Iter`, not `-> impl Iterator`:** RPITIT (`impl Trait` in trait method return position) needs Rust 2024 syntax; workspace is on 2021. Named iterator state structs are portable.

---

## Section 2 — `FieldAddr` handle (new in `spine/handles.rs`)

Add a new newtype to `crates/agent-core/src/spine/handles.rs`. Unlike the existing `handle_newtype!` macro (which produces `pub struct X(u64)`), `FieldAddr` carries TWO pieces of information: the address AND the field's known `ValType`.

```rust
/// An il2cpp instance-field address with its known type. Distinct from
/// `MemAddr<ReadWrite>` because il2cpp field writes may need value-type
/// boxing semantics that raw memory writes don't — keeping the type
/// distinction now means In-flight Modify (priority #3) can specialize
/// `Write<T> for FieldAddr` differently from `Write<T> for MemAddr<RW>`
/// without retrofitting callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldAddr {
    pub addr:    MemAddr<ReadWrite>,
    pub val_type: ValType,
}

impl FieldAddr {
    pub fn new(addr: MemAddr<ReadWrite>, val_type: ValType) -> Self {
        Self { addr, val_type }
    }
}
```

**Why a newtype, not a type alias:**
- Type system distinguishes "this is specifically a field" from "this is any writable memory" at compile time — callers can't pass raw `MemAddr<RW>` where a `FieldAddr` is required
- Carries `ValType` at construction time — `Write<T> for FieldAddr` can verify `T::TYPE == self.val_type` at write time (catches type-mismatch bugs the WASM script can't statically prevent)
- Composition contract: future `il2cpp.set_field` host fn returns `FieldAddr`; modders writing to it know they're going through field semantics, not raw mem

**Migration of `field_addr_t`:**
```rust
// Before (in internals/api.rs):
pub fn field_addr_t(...) -> Option<MemAddr<ReadWrite>> { ... }

// After:
pub fn field_addr_t(...) -> Option<FieldAddr> { ... }
```

One call-site change (the `_t` façade itself). Existing tests in the spine T7 compile_fail block reference `MemAddr<ReadWrite>` directly — those keep working since `FieldAddr` is composed of one.

---

## Section 3 — Trait impls per handle

```text
┌────────────────────────┬──────────────────┬──────────────────┬──────────────────────────┐
│ Handle                 │ Read<T>          │ Write<T>         │ Iter<T>                  │
├────────────────────────┼──────────────────┼──────────────────┼──────────────────────────┤
│ MemAddr<ReadOnly>      │ ✅ Read<T>       │ — (compile fail) │ —                        │
│ MemAddr<ReadWrite>     │ ✅ Read<T>       │ ✅ Write<T>      │ —                        │
│ FieldAddr              │ ✅ Read<T>       │ ✅ Write<T>      │ —                        │
│ KlassPtr               │ —                │ —                │ ✅ Iter<FieldInfo>       │
│                        │                  │                  │ ✅ Iter<MethodPtr>       │
│ FrameRing              │ —                │ —                │ ✅ Iter<RawFrame>        │
└────────────────────────┴──────────────────┴──────────────────┴──────────────────────────┘
```

**`Read<T> for MemAddr<C>` (any capability):**
```rust
impl<T: MemValue, C> Read<T> for MemAddr<C> {
    fn read(&self) -> Result<T, MemError> {
        // Delegates to the existing read_t logic — pure refactor.
        // Body lives in agent (not agent-core) because RegionMap is FFI-side.
    }
}
```

Note: the actual `read` impl body needs access to `external::api::read` which lives in the `agent` crate, NOT agent-core. **The impl block goes in `crates/agent/src/external/api.rs`** (where read_t already lives), referencing the trait defined in agent-core. This is the standard Rust pattern: trait in lib, impl wherever the concrete type's behavior lives.

**`Write<T> for FieldAddr`:**
```rust
impl<T: MemValue> Write<T> for FieldAddr {
    fn write(&self, value: T) -> Result<(), MemError> {
        if T::VAL_TYPE != self.val_type {
            return Err(MemError::TypeMismatch {
                expected: self.val_type,
                got: T::VAL_TYPE,
            });
        }
        // Delegates to the existing write_t logic via self.addr.
    }
}
```

(Requires adding `VAL_TYPE: ValType` associated constant to the `MemValue` trait — small additive change.)

**`Iter<FieldInfo> for KlassPtr`:**
```rust
pub struct FieldInfoIter {
    klass:  KlassPtr,
    cursor: usize,
    limit:  usize,
}

impl Iterator for FieldInfoIter {
    type Item = FieldInfo;
    fn next(&mut self) -> Option<FieldInfo> {
        // Walks the klass's FieldInfo array using cfg.klass_fields offset.
        // Lazy — only reads the next entry when next() is called.
    }
}

impl Iter<FieldInfo> for KlassPtr {
    type Iter = FieldInfoIter;
    fn iter(&self) -> Self::Iter { ... }
}
```

Same pattern for `Iter<MethodPtr> for KlassPtr` and `Iter<RawFrame> for FrameRing` (FrameRing's `Iter` impl wraps the existing ring iteration; mostly mechanical).

**`FieldInfo` type:** introduce a small struct in agent-core (`spine/field_info.rs` or fold into handles.rs):
```rust
#[derive(Debug, Clone, Copy)]
pub struct FieldInfo {
    pub name_offset: u32,   // offset into klass's string heap
    pub offset:      u32,   // field offset within instance
    pub val_type:    ValType,
    pub token:       u32,
}
```
(Names are read lazily by the iterator consumer, not eagerly during iteration — keeps iter cost low.)

---

## Section 4 — Probe strengthening (parameters_off + return_type_off)

In `crates/agent/src/internals/calibration/method_layout.rs`, the two loose-discriminator probes today use `if p > 0x10000` — accepts any pointer-shaped value. Strengthen with **structural validators** that walk the pointed-to memory and verify it looks like the structure it's supposed to be.

### `probe_method_parameters_off`

A valid `parameters_off` points to a `ParameterInfo[]` array. The array's length is in the method's `param_count_off` (we already probe that — it's strong). A valid extract:

```rust
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)? as usize;
    if p == 0 { return None; }
    // Read the param count from the method (we trust method_param_count_off — it's strong).
    let count = map.read_u8(*m as usize + cfg.method_param_count_off)? as usize;
    if count == 0 { return Some(()); }  // valid: zero-arg method has zero params; any non-null ptr OK
    if count > 32 { return None; }       // structural sanity: il2cpp methods don't have 32+ params

    // Walk the supposed ParameterInfo array; each entry's first u64 is the
    // parameter's Il2CppType*. Each Il2CppType*'s tc (via the discrim recipe)
    // must be in the valid 0x01..=0x45 range.
    let stride = cfg.param_info_size;  // already probed in Phase 4
    let type_off = cfg.param_info_type_off;  // already probed in Phase 4
    for i in 0..count.min(4) {  // sample up to 4 params; 4 is enough for structural validation
        let pi = p + i * stride;
        let tp = map.read_u64(pi + type_off)? as usize;
        if tp == 0 { return None; }
        let chunk = map.read_u64(tp + cfg.il2cpp_type_discrim_read_at)?;
        let tc = ((chunk >> cfg.discrim_shift) & 0xFF) as u8;
        if !(0x01..=0x45).contains(&tc) { return None; }
    }
    Some(())
};
```

For Math.Pow (2 doubles) and PadLeft (int, char), this extract walks the parameter array and verifies each entry has a valid il2cpp type code. A random pointer in the il2cpp data region would fail this check — the structure isn't a ParameterInfo array.

### `probe_method_return_type_off`

A valid `return_type_off` points to an `Il2CppType*`. The Il2CppType has a tc in the valid range:

```rust
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)? as usize;
    if p == 0 { return None; }
    let chunk = map.read_u64(p + cfg.il2cpp_type_discrim_read_at)?;
    let tc = ((chunk >> cfg.discrim_shift) & 0xFF) as u8;
    if !(0x01..=0x45).contains(&tc) { return None; }
    Some(())
};
```

For Math.Pow (returns double, tc=0x0D) and PadLeft (returns string, tc=0x0E), this passes. Garbage pointers fail.

### Gate restoration

With strengthened probes, both fields can now use the **relaxed Phase 2 gate** (`apply_offset_phase2`) safely. In `config.rs`, change Phase 2 wiring:

```rust
// Was (B-3 last fix):
apply_offset(&mut cfg.method_parameters_off, &mpars);     // strict, kept fallback
apply_offset(&mut cfg.method_return_type_off, &mret);     // strict, kept fallback

// Now (B-4):
apply_offset_phase2(&mut cfg.method_parameters_off, &mpars);  // strengthened probe; safe with relaxed gate
apply_offset_phase2(&mut cfg.method_return_type_off, &mret);  // strengthened probe; safe with relaxed gate
```

Both fields will now override on Highrise (probe will yield 0x28 and 0x20 respectively, both structurally validated) instead of keeping the v24 fallbacks. PW stays correct because either the strengthened probe agrees with the fallback OR the structural check fails on the wrong-shape pointer at the rejected candidate.

---

## Section 5 — `_t` façade refactor + regression strategy

The 10 existing `_t` functions stay as ergonomic façades. Each body becomes a one-line call to the trait method.

**Before:**
```rust
// crates/agent/src/external/api.rs
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    // existing 8-line implementation
}
```

**After:**
```rust
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    addr.read::<T>()
}
```

The actual logic moves into `impl Read<T> for MemAddr<C>`. The façade becomes a 1-line wrapper. **Zero changes at any call site** — `read_t(addr)` and `addr.read::<u32>()` both work.

**Special case: `field_addr_t`'s return type change.** It now returns `Option<FieldAddr>` instead of `Option<MemAddr<ReadWrite>>`. Callers downstream:
- `mem_host::host_invoke` uses `invoke_method_t` (the one with a real production caller) — no change there
- Tests in spine T7 compile_fail block reference `MemAddr<ReadWrite>` directly — those reference the wrapped `addr` field if needed
- No other production callers of `field_addr_t` today (verified via grep) — no fan-out

**Regression strategy:** the existing tests + the existing PW + Highrise gates ALL pass before B-4. B-4 ships when all of them pass after B-4. The new traits get tested via:
- Unit tests in `agent-core/tests/access_traits.rs` for Read/Write/Iter shape (synthetic impls; no FFI)
- `agent-core/tests/field_addr.rs` for `FieldAddr` construction + type-mismatch detection
- Live-game regression: `test_invoke.wasm` + `test_hook.wasm` on PW (and HS for invoke; HS hook is the known H13 crash) — same outcomes as before B-4

---

## Architecture summary

```
B-4: Trait-Architecture Spine
────────────────────────────────────────────────────
agent-core/src/spine/
  access.rs (NEW)            Read<T> + Write<T> + Iter<T> trait defs
  handles.rs (MODIFY)        Add FieldAddr newtype
  field_info.rs (NEW)        FieldInfo struct (lightweight: name_offset/offset/val_type/token)
  value.rs (MODIFY)          MemValue::VAL_TYPE associated constant
  mod.rs (MODIFY)            Re-export new types

agent/src/external/api.rs
  + impl Read<T> for MemAddr<C>
  + impl Write<T> for MemAddr<ReadWrite>
  + impl Write<T> for FieldAddr
  + impl Read<T> for FieldAddr
  + read_t/write_t/read_bytes_t/read_cstr_t become 1-line façades

agent/src/internals/api.rs
  + impl Iter<FieldInfo> for KlassPtr   (FieldInfoIter state struct)
  + impl Iter<MethodPtr> for KlassPtr   (MethodInfoIter state struct)
  + find_class_t/find_method_t/etc. become 1-line façades
  + field_addr_t returns Option<FieldAddr> (type change; one-line body update)

agent/src/protocol/<somewhere>
  + impl Iter<RawFrame> for FrameRing   (wraps existing ring iteration)

agent/src/internals/calibration/method_layout.rs
  + probe_method_parameters_off: structural validator
  + probe_method_return_type_off: structural validator

agent/src/internals/config.rs
  + Phase 2 wiring: parameters_off + return_type_off → apply_offset_phase2 (with strengthened probes)
```

**Total touched code:** ~350 lines (~150 traits + impls, ~80 probe strengthening, ~80 façade refactors, ~40 FieldAddr + FieldInfo types).

---

## Testing strategy

### Unit tests (host-runnable; agent-core)

- `tests/access_traits.rs`: synthetic struct implementing Read<u32> + Write<u32>; verify the trait surface compiles and behaves
- `tests/field_addr.rs`: construct FieldAddr; verify type-mismatch returns MemError::TypeMismatch
- `tests/value_val_type.rs`: each MemValue impl returns the correct VAL_TYPE constant

### Compile-fail tests (preserve from Spine T5)

- `compile_fail` doc-test: `let read_only: MemAddr<ReadOnly>; read_only.write(42u32)` must NOT compile (no `Write<T>` impl for ReadOnly capability)
- Add: `let raw: MemAddr<ReadWrite>; raw.read::<u32>()` MUST compile (Read works)
- Add: `let raw: MemAddr<ReadOnly>; raw.read::<u32>()` MUST compile (Read works on ReadOnly too)

### Live-game regression (manual; PW + Highrise)

- Deploy via `./deploy.sh release`
- `test_invoke.wasm` on PW: `[wasm] invoke Math::Pow returned 8.0 OK`
- `test_invoke.wasm` on Highrise: `[wasm] invoke Math::Pow returned 8.0 OK`
- `test_hook.wasm` on PW: 4-line lifecycle ending in `unhooked Pow returned 8.0 OK`
- Calibration block on PW: Phase 2 should show `PROBE OVERRIDE` lines for parameters_off + return_type_off (because the strengthened probes now satisfy the gate)
- Calibration block on Highrise: same (strengthened probes pass the structural validation on HS's MethodInfo layout)
- Dump counts unchanged from B-3 baseline (PW ≈2,496 classes / 30,977 fields; HS ≈15,414 / 80,427)

### HS Hook H13 status

H13 (Highrise hook trampoline crash) stays banked. B-4 doesn't touch the trampoline path; H13's the same crash it was post-B-3.

---

## What ships when B-4 lands

- `Read<T>` / `Write<T>` / `Iter<T>` traits — the composition currency the user originally asked for in the post-B-3 brainstorm
- `FieldAddr` newtype carrying `(MemAddr<ReadWrite>, ValType)` — In-flight Modify (priority #3) designs against `Write<T> for FieldAddr`
- Lazy `Iter` impls for the 3 most-iterated collections (klass fields, klass methods, frame ring)
- Strengthened Phase 2 probes — parameters_off + return_type_off now override safely on Highrise (closes B-3's workaround)
- All 10 typed `_t` façades preserved (load-bearing per the spine-isn't-dead-code lesson) — bodies become 1-line trait-method delegates
- The composition story: `for field in klass.iter() { field.read::<u32>()? }` works against typed handles, with capability checks at compile time

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Trait coherence rules force orphan-rule workarounds (impl Read<T> for foreign type from agent-core when T lives in agent) | All Read/Write/Iter impls happen in the `agent` crate where the concrete handle types' BEHAVIOR lives; the trait DEFINITION is in agent-core. Standard Rust pattern — orphan rule satisfied because we own the impl crate for the concrete type. |
| Strengthened probes reject ALL candidates on a future game (the structural validator is too strict) | Phase 2's relaxed gate falls back to v24 constants on probe failure; same safety net as B-3. Future game gets degraded calibration with a PROBE WEAK log, not a crash. |
| `FieldAddr` adds friction at call sites that previously used `MemAddr<ReadWrite>` directly | Field-write call sites today: zero in production. The `_t` façade migration is mechanical. The friction is paid forward to priority #3 callers, who will naturally use `FieldAddr` because that's what `field_addr_t` returns. |
| `Iter<FieldInfo>` iterator state holds a `KlassPtr` — handle-aliasing concerns | Iterator state is `Copy` (FieldInfoIter struct is small + Copy). Multiple concurrent iterators on the same KlassPtr are safe by construction (they each own a cursor). |
| `MemValue::VAL_TYPE` associated constant breaking existing impls | Existing MemValue impls (u8/u16/u32/u64/i8/i16/i32/i64/f32/f64) all have a known ValType — one-line addition per impl. No external impls exist; agent-core owns the trait. |
