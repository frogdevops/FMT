# B-4: Trait-Architecture Spine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Read<T>` / `Write<T>` / `Iter<T>` traits + a distinct `FieldAddr` handle so the 3 domains compose uniformly via trait bounds; bundle structural strengthening of the 2 loose Phase 2 probes so they can use the relaxed gate safely.

**Architecture:** Three single-method traits in `agent-core/src/spine/access.rs`. `FieldAddr` newtype in `spine/handles.rs` carrying `(MemAddr<ReadWrite>, ValType)`. Impls live in `agent` for FFI-touching handles (MemAddr, FieldAddr, KlassPtr) and in `agent-core` for the protocol FrameRing. The 10 existing `_t` siblings stay as ergonomic façades (one-line bodies calling the trait methods). Probe strengthening lives in `internals/calibration/method_layout.rs` with structural validators that walk the pointed-to memory shape against the il2cpp tc range.

**Tech Stack:** Rust 2021, no new deps. `MemValue::VAL_TYPE` associated constant ALREADY EXISTS — no value.rs change needed. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (agent-core tests).

**Spec:** `docs/superpowers/specs/2026-05-30-b4-trait-spine-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent-core/src/spine/access.rs` | Create | `Read<T>` + `Write<T>` + `Iter<T>` trait defs |
| `crates/agent-core/src/spine/handles.rs` | Modify | Add `FieldAddr` newtype + constructor + accessors |
| `crates/agent-core/src/spine/field_info.rs` | Create | `FieldInfo` value struct (Copy, lightweight) |
| `crates/agent-core/src/spine/mod.rs` | Modify | Register new modules + re-export `Read`/`Write`/`Iter`/`FieldAddr`/`FieldInfo` |
| `crates/agent-core/tests/access_traits.rs` | Create | Trait-shape compile-tests + synthetic impls |
| `crates/agent-core/tests/field_addr.rs` | Create | FieldAddr construction + type-mismatch test |
| `crates/agent-core/src/protocol.rs` | Modify | `Iter<RawFrame> for FrameRing` impl + iterator state |
| `crates/agent/src/external/api.rs` | Modify | `Read<T>` / `Write<T>` impls for `MemAddr<C>`; refactor `read_t` / `write_t` to one-line façades; new `Read<T>` / `Write<T>` for `FieldAddr` |
| `crates/agent/src/internals/api.rs` | Modify | `Iter<FieldInfo>` + `Iter<MethodPtr>` for `KlassPtr` with iterator state; refactor `field_addr_t` return type to `Option<FieldAddr>` |
| `crates/agent/src/internals/calibration/method_layout.rs` | Modify | Strengthen `probe_method_parameters_off` + `probe_method_return_type_off` extracts |
| `crates/agent/src/internals/config.rs` | Modify | Switch parameters_off + return_type_off to `apply_offset_phase2` (now safe with strengthened probes) |

---

## Task 1: Define the three traits (`agent-core/spine/access.rs`)

**Files:**
- Create: `crates/agent-core/src/spine/access.rs`
- Modify: `crates/agent-core/src/spine/mod.rs`
- Create: `crates/agent-core/tests/access_traits.rs`

Pure-Rust trait scaffolding. Compiles + host-tests on Linux.

- [ ] **Step 1: Create `crates/agent-core/src/spine/access.rs`**

```rust
//! Capability-disciplined access traits: `Read<T>` / `Write<T>` / `Iter<T>`.
//! Spans the three Spec-2 domains — a handle's type DECLARES its capabilities,
//! and scripts compose via trait bounds (`fn f<H: Read<u32>>(h: H)`).
//!
//! YAGNI discipline: one method per trait. Batch reads / CAS / offset variants
//! get added when a real caller demands. The existing typed `_t` free functions
//! become one-line façades calling these trait methods.

use crate::mem_value::MemValue;
use crate::spine::error::MemError;

/// Read a typed value of `T` from this handle.
pub trait Read<T: MemValue> {
    fn read(&self) -> Result<T, MemError>;
}

/// Write a typed value of `T` through this handle. Capability-disciplined:
/// only handles whose impl explicitly opts in are writable. `MemAddr<ReadOnly>`
/// has no `Write<T>` impl, so `read_only.write(...)` won't compile.
pub trait Write<T: MemValue> {
    fn write(&self, value: T) -> Result<(), MemError>;
}

/// Lazily iterate items of type `T`. The associated `Iter` type lets impls
/// define their own state struct without allocating a Vec. Items are NOT
/// bounded by `MemValue` — iterators can yield handles (e.g.
/// `Iter<FieldInfo> for KlassPtr`) or other domain types.
pub trait Iter<T> {
    type Iter: Iterator<Item = T>;
    fn iter(&self) -> Self::Iter;
}
```

- [ ] **Step 2: Register the module in `spine/mod.rs`**

In `crates/agent-core/src/spine/mod.rs`, find the `pub mod` block (around lines 6-10) and add `access` to it. Find the `pub use` block and add the `access` re-exports.

After the existing `pub mod value;`, add:

```rust
pub mod access;
```

After `pub use value::MemValue;`, add:

```rust
pub use access::{Iter, Read, Write};
```

- [ ] **Step 3: Write the failing test**

Create `crates/agent-core/tests/access_traits.rs`:

```rust
//! Trait-shape compile-tests for Read<T> / Write<T> / Iter<T>.
//! Synthetic impls verify the API surface compiles cleanly.

use agent_core::spine::{Iter, MemValue, Read, Write};
use agent_core::spine::error::MemError;

/// Synthetic handle that holds a u32 in-process (no FFI).
#[derive(Debug, Clone, Copy)]
struct FakeHandle(u32);

impl Read<u32> for FakeHandle {
    fn read(&self) -> Result<u32, MemError> {
        Ok(self.0)
    }
}

impl Write<u32> for FakeHandle {
    fn write(&self, _value: u32) -> Result<(), MemError> {
        // FakeHandle's value can't be mutated by &self; the trait shape only
        // requires the call to compile and return Ok. Real impls (MemAddr,
        // FieldAddr) take &self because address-based writes don't need &mut.
        Ok(())
    }
}

#[test]
fn read_trait_compiles_and_returns_value() {
    let h = FakeHandle(42);
    let v: u32 = h.read().unwrap();
    assert_eq!(v, 42);
}

#[test]
fn write_trait_compiles_and_returns_ok() {
    let h = FakeHandle(0);
    assert!(h.write(99u32).is_ok());
}

/// Synthetic iter handle yielding three fixed values.
struct ThreeInts;

impl Iter<u32> for ThreeInts {
    type Iter = std::vec::IntoIter<u32>;
    fn iter(&self) -> Self::Iter {
        vec![1u32, 2, 3].into_iter()
    }
}

#[test]
fn iter_trait_yields_items_lazily() {
    let h = ThreeInts;
    let collected: Vec<u32> = h.iter().collect();
    assert_eq!(collected, vec![1, 2, 3]);
}

#[test]
fn iter_can_be_chained_with_combinators() {
    let h = ThreeInts;
    let doubled: Vec<u32> = h.iter().map(|x| x * 2).collect();
    assert_eq!(doubled, vec![2, 4, 6]);
}

/// Generic function using a `Read<T>` bound — proves composition contract.
fn read_anything<H: Read<u32>>(h: &H) -> Result<u32, MemError> {
    h.read()
}

#[test]
fn generic_read_bound_works() {
    let h = FakeHandle(7);
    assert_eq!(read_anything(&h).unwrap(), 7);
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p agent-core --test access_traits`
Expected: compile error — `agent_core::spine::Read` etc. not yet visible (depends on Step 2 being committed in sequence with Step 1).

Then after Step 1+2 are saved:

Run: `cargo test -p agent-core --test access_traits`
Expected: 5 passed (read_trait_compiles_and_returns_value, write_trait_compiles_and_returns_ok, iter_trait_yields_items_lazily, iter_can_be_chained_with_combinators, generic_read_bound_works).

- [ ] **Step 5: Verify full agent-core suite passes**

Run: `cargo test -p agent-core`
Expected: all previously-passing tests + the 5 new ones.

- [ ] **Step 6: Commit (user runs)**

Suggested message:
```
agent-core/spine: add Read<T> + Write<T> + Iter<T> traits + tests
```

---

## Task 2: `FieldAddr` + `FieldInfo` types

**Files:**
- Modify: `crates/agent-core/src/spine/handles.rs`
- Create: `crates/agent-core/src/spine/field_info.rs`
- Modify: `crates/agent-core/src/spine/mod.rs`
- Create: `crates/agent-core/tests/field_addr.rs`

`FieldAddr` doesn't use the existing `handle_newtype!` macro because it carries TWO fields (address + val_type). `FieldInfo` is a separate value struct yielded by `Iter<FieldInfo> for KlassPtr`.

- [ ] **Step 1: Add `FieldAddr` to `spine/handles.rs`**

In `crates/agent-core/src/spine/handles.rs`, after the `handle_newtype!` invocations (after line 28 `handle_newtype!(HookHandle, ...);`), add:

```rust
use crate::mem_value::ValType;
use crate::spine::addr::{MemAddr, ReadWrite};

/// An il2cpp instance-field address with its known type. Distinct from
/// `MemAddr<ReadWrite>` because il2cpp field writes may need value-type
/// boxing semantics that raw memory writes don't. The type system carries
/// the field's `ValType` from `field_addr_t` construction through any
/// downstream `Write<T>` callsite, where `Write<T> for FieldAddr` can
/// verify `T::VAL_TYPE == self.val_type` at write time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldAddr {
    pub addr:     MemAddr<ReadWrite>,
    pub val_type: ValType,
}

impl FieldAddr {
    #[inline]
    pub fn new(addr: MemAddr<ReadWrite>, val_type: ValType) -> Self {
        Self { addr, val_type }
    }

    #[inline]
    pub fn addr(self) -> MemAddr<ReadWrite> { self.addr }

    #[inline]
    pub fn val_type(self) -> ValType { self.val_type }
}
```

- [ ] **Step 2: Create `crates/agent-core/src/spine/field_info.rs`**

```rust
//! Per-field metadata yielded by `Iter<FieldInfo> for KlassPtr`. Lightweight,
//! `Copy`, decoupled from FFI — the iterator reads the structural offsets via
//! agent-side primitives, then yields these descriptors.

use crate::mem_value::ValType;

/// One il2cpp instance-field's metadata (offset within the parent instance +
/// declared value type + metadata token + name-pointer for lazy resolution).
///
/// `name_ptr` is the raw address of the field's NUL-terminated name in the
/// string heap. Callers that need the name decode it via `RegionMap::read_name`
/// (agent-side); keeping it as a raw pointer means iteration doesn't allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldInfo {
    pub name_ptr: usize,
    pub offset:   u32,
    pub val_type: ValType,
    pub token:    u32,
}
```

- [ ] **Step 3: Register both in `spine/mod.rs`**

In `crates/agent-core/src/spine/mod.rs`, add to the `pub mod` block:

```rust
pub mod field_info;
```

Update the handles re-export:

```rust
pub use handles::{FieldAddr, FrameSeq, HookHandle, Instance, KlassPtr, MethodPtr, SocketHandle};
```

Add:

```rust
pub use field_info::FieldInfo;
```

- [ ] **Step 4: Write the failing test**

Create `crates/agent-core/tests/field_addr.rs`:

```rust
//! FieldAddr construction + type-mismatch detection tests.

use agent_core::mem_value::ValType;
use agent_core::spine::{FieldAddr, FieldInfo, MemAddr, ReadWrite};

#[test]
fn field_addr_construction_carries_addr_and_type() {
    let addr: MemAddr<ReadWrite> = unsafe { MemAddr::from_raw_writable(0x1000) };
    let fa = FieldAddr::new(addr, ValType::U32);
    assert_eq!(fa.addr().as_u64(), 0x1000);
    assert_eq!(fa.val_type(), ValType::U32);
}

#[test]
fn field_addr_is_copy_and_eq() {
    let addr: MemAddr<ReadWrite> = unsafe { MemAddr::from_raw_writable(0x2000) };
    let a = FieldAddr::new(addr, ValType::F32);
    let b = a;  // Copy
    assert_eq!(a, b);
}

#[test]
fn field_info_is_copy_and_struct_fields_accessible() {
    let fi = FieldInfo {
        name_ptr: 0xdeadbeef,
        offset:   0x10,
        val_type: ValType::U64,
        token:    0x04000001,
    };
    let copy = fi;
    assert_eq!(copy.name_ptr, 0xdeadbeef);
    assert_eq!(copy.offset, 0x10);
    assert_eq!(copy.val_type, ValType::U64);
    assert_eq!(copy.token, 0x04000001);
}
```

- [ ] **Step 5: Run tests + verify**

Run: `cargo test -p agent-core --test field_addr`
Expected: 3 passed.

Run: `cargo test -p agent-core`
Expected: all previously-passing tests + the new ones.

- [ ] **Step 6: Commit (user runs)**

Suggested message:
```
agent-core/spine: add FieldAddr + FieldInfo handles
```

---

## Task 3: `Read<T>` / `Write<T>` impls (`external/api.rs`)

**Files:**
- Modify: `crates/agent/src/external/api.rs`

The existing `read_t<T: MemValue, C>` body becomes the trait impl body. The free function becomes a one-line façade calling it. Same for `write_t`. Adds new `Read<T>` / `Write<T>` impls for `FieldAddr` (delegating to the underlying `MemAddr<ReadWrite>` impl).

- [ ] **Step 1: Locate the existing `read_t` and `write_t`**

In `crates/agent/src/external/api.rs`, find `pub fn read_t` (~line 60) and `pub fn write_t` (~line 72). The current bodies are 8 lines each, validating + reading/writing through `cache::validate_read` / `cache::validate_write`.

- [ ] **Step 2: Add the `Read<T> for MemAddr<C>` impl**

Above the existing `read_t` function, insert:

```rust
// ── Trait impls — the load-bearing capability surface ───────────────────────
// `read_t` / `write_t` become 1-line façades calling these (Step 4 below).

impl<T: MemValue, C> agent_core::spine::Read<T> for MemAddr<C> {
    fn read(&self) -> Result<T, MemError> {
        let width = T::VAL_TYPE.fixed_width().ok_or(MemError::BadType)?;
        let a = self.as_u64() as usize;
        if !cache::validate_read(a, width) {
            return Err(MemError::Unreadable);
        }
        let bytes = unsafe { std::slice::from_raw_parts(a as *const u8, width) };
        T::from_le_bytes_spine(bytes).ok_or(MemError::BadType)
    }
}

impl<T: MemValue> agent_core::spine::Write<T> for MemAddr<ReadWrite> {
    fn write(&self, value: T) -> Result<(), MemError> {
        let width = T::VAL_TYPE.fixed_width().ok_or(MemError::BadType)?;
        let a = self.as_u64() as usize;
        if !cache::validate_write(a, width) {
            return Err(MemError::Unreadable);
        }
        let bytes = value.to_le_bytes_buf();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), a as *mut u8, width);
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Add `Read<T>` / `Write<T>` for `FieldAddr`**

Just below the `MemAddr` impls:

```rust
impl<T: MemValue> agent_core::spine::Read<T> for agent_core::spine::FieldAddr {
    fn read(&self) -> Result<T, MemError> {
        if T::VAL_TYPE != self.val_type {
            return Err(MemError::BadType);
        }
        agent_core::spine::Read::<T>::read(&self.addr)
    }
}

impl<T: MemValue> agent_core::spine::Write<T> for agent_core::spine::FieldAddr {
    fn write(&self, value: T) -> Result<(), MemError> {
        if T::VAL_TYPE != self.val_type {
            return Err(MemError::BadType);
        }
        agent_core::spine::Write::<T>::write(&self.addr, value)
    }
}
```

- [ ] **Step 4: Refactor `read_t` + `write_t` into 1-line façades**

Replace the existing `read_t` body (~lines 60-67):

```rust
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    let width = T::VAL_TYPE.fixed_width().ok_or(MemError::BadType)?;
    let a = addr.as_u64() as usize;
    if !cache::validate_read(a, width) {
        return Err(MemError::Unreadable);
    }
    let bytes = unsafe { std::slice::from_raw_parts(a as *const u8, width) };
    T::from_le_bytes_spine(bytes).ok_or(MemError::BadType)
}
```

With:

```rust
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    agent_core::spine::Read::<T>::read(&addr)
}
```

Replace the existing `write_t` body with:

```rust
pub fn write_t<T: MemValue>(addr: MemAddr<ReadWrite>, val: T) -> Result<(), MemError> {
    agent_core::spine::Write::<T>::write(&addr, val)
}
```

`read_bytes_t` and `read_cstr_t` are NOT MemValue impls (they're variable-length); they stay as standalone free functions unchanged.

- [ ] **Step 5: Build cross-compile + tests**

```bash
cargo build --target x86_64-pc-windows-gnu --release
cargo test -p agent-core
```

Both must be clean. The compile_fail doctest at `crates/agent-core/src/spine/addr.rs:18-28` (which proves `MemAddr<ReadOnly>` can't be passed where `MemAddr<ReadWrite>` is expected) MUST STILL FAIL — that's the capability gate proof.

- [ ] **Step 6: Commit (user runs)**

Suggested message:
```
external: Read<T> + Write<T> impls for MemAddr + FieldAddr; _t façades
```

---

## Task 4: `Iter<FieldInfo>` + `Iter<MethodPtr>` for `KlassPtr` + `field_addr_t` return type change

**Files:**
- Modify: `crates/agent/src/internals/api.rs`

Add iterator state structs that walk the klass's FieldInfo array and method array lazily. Change `field_addr_t`'s return type from `Option<MemAddr<ReadWrite>>` to `Option<FieldAddr>` — one-line body update.

- [ ] **Step 1: Add `FieldInfoIter` + `Iter<FieldInfo> for KlassPtr`**

Append to `crates/agent/src/internals/api.rs`:

```rust
// ── Iter trait impls — lazy walks over klass collections ────────────────────

use agent_core::spine::{FieldInfo, Iter, KlassPtr, MethodPtr};

/// Lazy iterator over a klass's FieldInfo array. Walks via the probed
/// `klass_fields` offset; stops when `name_ptr == 0` (FFI iterator convention)
/// or after `MAX_FIELDS_PER_CLASS` (defensive cap).
pub struct FieldInfoIter {
    klass:  usize,
    cursor: usize,
    limit:  usize,
}

const MAX_FIELDS_PER_CLASS: usize = 256;

impl Iterator for FieldInfoIter {
    type Item = FieldInfo;
    fn next(&mut self) -> Option<FieldInfo> {
        if self.cursor >= self.limit {
            return None;
        }
        let c = ctx::get()?;
        // klass_fields points at a contiguous FieldInfo array; stride is
        // probed in Phase 4 calibration (param_info_size happens to be the
        // same struct stride for FieldInfo in il2cpp ≥ v24).
        let fields_ptr = cache::read_u64(self.klass + c.cfg.klass_fields)? as usize;
        if fields_ptr == 0 {
            return None;
        }
        let stride = 32; // FieldInfo stride per the structural offsets banked in memory
        let slot = fields_ptr + self.cursor * stride;
        let name_ptr = cache::read_u64(slot).unwrap_or(0) as usize;
        if name_ptr == 0 {
            return None;
        }
        let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
        let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
        let token = cache::read_u32(slot + 28).unwrap_or(0);
        self.cursor += 1;
        if token == 0 {
            return self.next(); // scanner garbage; skip
        }
        let val_type = type_to_valtype(type_ptr).unwrap_or(agent_core::mem_value::ValType::U64);
        Some(FieldInfo { name_ptr, offset: raw_offset, val_type, token })
    }
}

impl Iter<FieldInfo> for KlassPtr {
    type Iter = FieldInfoIter;
    fn iter(&self) -> Self::Iter {
        FieldInfoIter { klass: self.as_u64() as usize, cursor: 0, limit: MAX_FIELDS_PER_CLASS }
    }
}

fn type_to_valtype(type_ptr: usize) -> Option<agent_core::mem_value::ValType> {
    if type_ptr == 0 { return None; }
    let c = ctx::get()?;
    let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at)?;
    let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
    agent_core::mem_value::valtype_from_tc(tc)
}
```

- [ ] **Step 2: Add `MethodPtrIter` + `Iter<MethodPtr> for KlassPtr`**

Append below the FieldInfoIter block:

```rust
/// Lazy iterator over a klass's MethodInfo*-array. Walks via the probed
/// `klass_methods` offset; stops when the back-pointer no longer matches
/// (the array-end sentinel pattern proven during the find_method probe work).
pub struct MethodPtrIter {
    klass:  usize,
    cursor: usize,
    limit:  usize,
}

const MAX_METHODS_PER_CLASS: usize = 1024;

impl Iterator for MethodPtrIter {
    type Item = MethodPtr;
    fn next(&mut self) -> Option<MethodPtr> {
        if self.cursor >= self.limit {
            return None;
        }
        let c = ctx::get()?;
        let methods_ptr = cache::read_u64(self.klass + c.cfg.klass_methods)? as usize;
        if methods_ptr == 0 {
            return None;
        }
        let method_ptr = cache::read_u64(methods_ptr + self.cursor * 8).unwrap_or(0) as usize;
        if method_ptr == 0 {
            return None;
        }
        // klass back-ptr sentinel: when method.klass != self.klass, we're past the end
        let back = cache::read_u64(method_ptr + c.cfg.method_klass_off).unwrap_or(0) as usize;
        if back != self.klass {
            return None;
        }
        self.cursor += 1;
        Some(MethodPtr::from_raw(method_ptr as u64))
    }
}

impl Iter<MethodPtr> for KlassPtr {
    type Iter = MethodPtrIter;
    fn iter(&self) -> Self::Iter {
        MethodPtrIter { klass: self.as_u64() as usize, cursor: 0, limit: MAX_METHODS_PER_CLASS }
    }
}
```

- [ ] **Step 3: Migrate `field_addr_t` return type**

Find `pub fn field_addr_t` (~line 216). Current body:

```rust
pub fn field_addr_t(
    klass: agent_core::spine::KlassPtr,
    name: &str,
    instance: agent_core::spine::Instance,
) -> Option<agent_core::spine::MemAddr<agent_core::spine::ReadWrite>> {
    let (offset, _vt) = field_info(klass.as_u64(), name)?;
    let addr = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    Some(unsafe { agent_core::spine::MemAddr::from_raw_writable(addr) })
}
```

Replace with:

```rust
pub fn field_addr_t(
    klass: agent_core::spine::KlassPtr,
    name: &str,
    instance: agent_core::spine::Instance,
) -> Option<agent_core::spine::FieldAddr> {
    let (offset, vt) = field_info(klass.as_u64(), name)?;
    let addr_raw = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    let addr = unsafe { agent_core::spine::MemAddr::from_raw_writable(addr_raw) };
    Some(agent_core::spine::FieldAddr::new(addr, vt))
}
```

Now uses the `vt` (val_type) that `field_info` already returns — previously discarded as `_vt`.

- [ ] **Step 4: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean. If you see "ctx not found" / "cache not found" errors, add the existing `use crate::internals::ctx;` and `use crate::external::cache;` imports already present at the top of api.rs — verify they're not missing.

If `valtype_from_tc` isn't found, check the import: the function lives at `agent_core::mem_value::valtype_from_tc`. Add the import to scope if needed.

If the build complains about `field_addr_t`'s changed return type at any callsite, grep first:
```bash
grep -rn "field_addr_t(" crates/agent/src crates/agent-core/src
```
Should show ONLY the definition + the spine T7 test (which references the inner addr field if it does). Any production caller needs updating.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
internals: Iter<FieldInfo> + Iter<MethodPtr> for KlassPtr; field_addr_t returns FieldAddr
```

---

## Task 5: `Iter<RawFrame> for FrameRing` (`agent-core/protocol.rs`)

**Files:**
- Modify: `crates/agent-core/src/protocol.rs`

FrameRing already supports ring iteration via existing methods. Wrap it in the new trait. Pure agent-core change; host-testable.

- [ ] **Step 1: Locate `FrameRing` + its existing iteration**

In `crates/agent-core/src/protocol.rs`, find `pub struct FrameRing` (~line 26) and inspect its existing public methods. Look for `pub fn frames(&self)` or similar — that's the existing iterator entry point if any.

If there's no existing iterator method, the FrameRing internals will need a quick public reading method. Read FrameRing's surface first:

```bash
sed -n '26,80p' crates/agent-core/src/protocol.rs
```

- [ ] **Step 2: Add the iterator state + Iter impl**

Append to `crates/agent-core/src/protocol.rs`:

```rust
// ── Iter trait impl — lazy walk over the bounded ring ───────────────────────

use crate::spine::Iter;

pub struct FrameRingIter<'a> {
    ring:   &'a FrameRing,
    cursor: usize,
    limit:  usize,
}

impl<'a> Iterator for FrameRingIter<'a> {
    type Item = RawFrame;
    fn next(&mut self) -> Option<RawFrame> {
        if self.cursor >= self.limit { return None; }
        let frame = self.ring.get(self.cursor)?;
        self.cursor += 1;
        Some(frame)
    }
}

impl<'a> Iter<RawFrame> for &'a FrameRing {
    type Iter = FrameRingIter<'a>;
    fn iter(&self) -> Self::Iter {
        FrameRingIter { ring: self, cursor: 0, limit: self.len() }
    }
}
```

The impl is on `&'a FrameRing` (reference) because the iterator borrows from the ring. If `FrameRing::get(idx) -> Option<RawFrame>` doesn't exist yet, add it (single accessor that reads frame at index without consuming):

```rust
impl FrameRing {
    pub fn get(&self, idx: usize) -> Option<RawFrame> {
        // existing logic that reads frame N from the ring; mirrors any
        // existing iteration code in this file. If a fn already exists that
        // does this (frames() returning Vec<RawFrame> for instance), inline
        // its body here.
    }
    pub fn len(&self) -> usize {
        // existing count; if `pub fn frame_count` exists use that
    }
}
```

If `get` + `len` already exist with the right shape, skip those additions and just add the iterator impl.

- [ ] **Step 3: Add a unit test**

Append to a new test block in `crates/agent-core/tests/access_traits.rs` (the file you created in Task 1):

```rust
#[test]
fn frame_ring_iter_compiles() {
    // FrameRing is constructed via existing API; just prove the trait surface
    // wires up correctly. Empty ring iterates zero times.
    use agent_core::protocol::{FrameRing, RawFrame};
    use agent_core::spine::Iter;

    let ring = FrameRing::new(8, 1024);  // adjust ctor signature to match real
    let count = (&ring).iter().count();
    assert_eq!(count, 0);
}
```

If `FrameRing::new`'s signature differs, adjust. The test's purpose is just "Iter compiles + 0-length ring iterates 0 times."

- [ ] **Step 4: Build + tests**

```bash
cargo test -p agent-core
cargo build --target x86_64-pc-windows-gnu --release
```

Both clean. The frame_ring_iter_compiles test passes.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
agent-core/protocol: Iter<RawFrame> for FrameRing
```

---

## Task 6: Strengthen Phase 2 probes + flip wiring

**Files:**
- Modify: `crates/agent/src/internals/calibration/method_layout.rs`
- Modify: `crates/agent/src/internals/config.rs`

Strengthen `probe_method_parameters_off` and `probe_method_return_type_off` extracts with structural validators. Then flip their config.rs wiring from `apply_offset` (strict 3-anchor) to `apply_offset_phase2` (relaxed 2-anchor) — safe now because the probes' discrimination is structural.

- [ ] **Step 1: Strengthen `probe_method_parameters_off` extract**

In `crates/agent/src/internals/calibration/method_layout.rs`, find `probe_method_parameters_off` and locate its `extract` closure (~line 149). Current body:

```rust
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)?;
    if p > 0x10000 { Some(()) } else { None }
};
```

Replace with:

```rust
// Structural validator: the value at `off` must be a ParameterInfo array
// whose entries each contain a valid Il2CppType* (tc in 0x01..=0x45). The
// method's param_count_off is already probed strongly (u8 == 2 for both
// anchors); we walk up to 4 entries.
let cfg_fallback = crate::internals::config::Il2CppConfig::fallback_constants();
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)? as usize;
    if p == 0 { return None; }
    let count = map.read_u8(*m as usize + cfg_fallback.method_param_count_off)? as usize;
    if count == 0 { return Some(()); }   // zero-arg method; any non-null ptr OK
    if count > 32 { return None; }        // structural sanity

    let stride = cfg_fallback.param_info_size;
    let type_off = cfg_fallback.param_info_type_off;
    let read_at = cfg_fallback.il2cpp_type_discrim_read_at;
    let shift = cfg_fallback.discrim_shift;

    for i in 0..count.min(4) {
        let pi = p + i * stride;
        let tp = map.read_u64(pi + type_off)? as usize;
        if tp == 0 { return None; }
        let chunk = map.read_u64(tp + read_at)?;
        let tc = ((chunk >> shift) & 0xFF) as u8;
        if !(0x01..=0x45).contains(&tc) { return None; }
    }
    Some(())
};
```

The `cfg_fallback = fallback_constants()` is used here NOT because we don't want to use probed values — but because Phase 4 hasn't run yet at the time Phase 2 runs (Phase 2 → 3 → 4 sequencing). The fallback constants for `param_info_size` (0x18), `param_info_type_off` (0x00), `il2cpp_type_discrim_read_at` (0x08), `discrim_shift` (16) are known-stable v24/v31. If a future game shifts those, Phase 4's probe will catch it later; Phase 2's structural validator just needs CONSISTENT byte layout to discriminate "is this a ParameterInfo array" from "this is a random pointer."

- [ ] **Step 2: Strengthen `probe_method_return_type_off` extract**

Find `probe_method_return_type_off` (~line 171). Current body:

```rust
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)?;
    if p > 0x10000 { Some(()) } else { None }
};
```

Replace with:

```rust
// Structural validator: the value at `off` must be an Il2CppType* whose tc
// (via the fallback discriminator recipe — proven stable v24→v31) is in
// the valid il2cpp type-code range 0x01..=0x45.
let cfg_fallback = crate::internals::config::Il2CppConfig::fallback_constants();
let extract = |m: &u64, off: usize| -> Option<()> {
    let p = map.read_u64(*m as usize + off)? as usize;
    if p == 0 { return None; }
    let chunk = map.read_u64(p + cfg_fallback.il2cpp_type_discrim_read_at)?;
    let tc = ((chunk >> cfg_fallback.discrim_shift) & 0xFF) as u8;
    if !(0x01..=0x45).contains(&tc) { return None; }
    Some(())
};
```

- [ ] **Step 3: Flip the config wiring**

In `crates/agent/src/internals/config.rs`, find the Phase 2 application block (lines ~241-247 from B-3's last fix). The current lines for the two loose-now-strong fields are:

```rust
apply_offset(&mut cfg.method_parameters_off, &mpars);     // strict, kept fallback
apply_offset(&mut cfg.method_return_type_off, &mret);     // strict, kept fallback
```

Replace with:

```rust
apply_offset_phase2(&mut cfg.method_parameters_off, &mpars);  // strengthened probe; safe with relaxed gate
apply_offset_phase2(&mut cfg.method_return_type_off, &mret);  // strengthened probe; safe with relaxed gate
```

The other 5 Phase 2 fields already use `apply_offset_phase2` from B-3 — leave them.

- [ ] **Step 4: Build cross-compile + deploy**

```bash
cargo build --target x86_64-pc-windows-gnu --release
./deploy.sh release
```

Both clean. Deploy to both games.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
calibration: strengthen parameters_off + return_type_off probes; flip to phase2 gate
```

---

## Task 7: Live-game regression gate (user manual)

**Files:** none modified.

The regression standard: every existing capability still works exactly as before B-4, AND the new observables (Phase 2 strengthened probes overriding on Highrise) are present.

- [ ] **Step 1: PW Invoke (no regression)**

User launches PW with `WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_invoke.wasm %command%`.

Expected agent.log:
```
[wasm] invoke Math::Pow returned 8.0 OK
```

- [ ] **Step 2: PW Hook (full lifecycle preserved)**

User launches PW with `FROG_WASM=test_hook.wasm`. Expected:
```
[wasm] install_hook OK
[wasm] hooked Pow returned UNEXPECTED      (H12 stub: transparent observer fires; 8.0 returned)
[wasm] remove_hook OK
[wasm] unhooked Pow returned 8.0 OK
```

- [ ] **Step 3: PW normal dump (baseline maintained)**

User launches PW WITHOUT FROG_WASM. After closing:

```bash
DUMP_PW="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"
LOG_PW="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log"
echo "=== PW B-4 baseline check ==="
echo "dumped:    $(grep 'dumped' "$LOG_PW" | tail -1)"
echo "garbage-tc: $(grep -c '<garbage-tc:' "$DUMP_PW")  (expect 0-5)"
echo "META:       $(grep -c 'Offset: META' "$DUMP_PW")  (expect ~21)"
echo
echo "=== Phase 2 OVERRIDE lines (expect 5+ on PW) ==="
grep "PROBE OVERRIDE" "$LOG_PW" | tail -10
```

Expected: dumped ≈ 2,496 classes / 30,977 fields (B-3 baseline). Phase 2 overrides include parameters_off (0x30→0x28) + return_type_off (0x28→0x20).

- [ ] **Step 4: Highrise Invoke (no regression)**

User launches Highrise with `FROG_WASM=test_invoke.wasm`. Expected `8.0 OK`.

- [ ] **Step 5: Highrise normal dump (Phase 2 overrides land)**

User launches Highrise without FROG_WASM. After closing:

```bash
DUMP_HR="/home/chef/.local/share/Steam/steamapps/common/Highrise/internals.txt"
LOG_HR="/home/chef/.local/share/Steam/steamapps/common/Highrise/agent.log"
echo "=== Highrise B-4 baseline check ==="
echo "dumped:    $(grep 'dumped' "$LOG_HR" | tail -1)"
echo
echo "=== Phase 2 OVERRIDE lines (expect 5+ on Highrise) ==="
grep "PROBE OVERRIDE" "$LOG_HR" | tail -10
```

Expected: dumped ≈ 15,414 classes / 80,427 fields. Phase 2 overrides include `method_parameters_off` and `method_return_type_off` (these were keeping fallback on Highrise pre-B-4 because the probes were too loose; now the structural validators pass → overrides land).

- [ ] **Step 6: Hand back to user**

If all 5 game-runs match expectations (no crash, no regression on counts/values, Phase 2 overrides land on both games for the now-strengthened probes), **B-4 is GREEN**.

H13 (Highrise Hook trampoline crash) stays banked as a separate micro-brick. B-4 doesn't touch the trampoline path.

Most likely diagnostic paths if anything regresses:
- PW dump count drops → the FieldInfoIter `cache::read_u64` calls in Task 4 might be hitting unmapped memory. Check the `MAX_FIELDS_PER_CLASS` cap (256) is being honored.
- `[wasm] invoke Math::Pow returned 8.0 OK` missing on either game → `Read<T> for MemAddr<C>` impl or `Write<T> for MemAddr<ReadWrite>` impl has a bug. Compare against the pre-B-4 `read_t` / `write_t` bodies — they should be byte-equivalent.
- Phase 2 overrides DON'T appear on Highrise → strengthened probe is too strict. Walk the actual MethodInfo[parameters_off=0x28] memory on HS to confirm it points to a valid ParameterInfo array.
- compile_fail doctest fails to fail → capability gate broke; check `Write<T> for MemAddr<ReadOnly>` was NOT accidentally added.

---

## Self-review

**1. Spec coverage:**

| Spec section | Task |
|---|---|
| Section 1: trait defs (Read/Write/Iter) | Task 1 ✓ |
| Section 2: FieldAddr handle | Task 2 ✓ |
| Section 2: FieldInfo struct | Task 2 ✓ |
| Section 3: Read<T> for MemAddr<C> | Task 3 ✓ |
| Section 3: Write<T> for MemAddr<ReadWrite> | Task 3 ✓ |
| Section 3: Read+Write for FieldAddr | Task 3 ✓ |
| Section 3: Iter<FieldInfo> for KlassPtr | Task 4 ✓ |
| Section 3: Iter<MethodPtr> for KlassPtr | Task 4 ✓ |
| Section 3: Iter<RawFrame> for FrameRing | Task 5 ✓ |
| Section 4: probe_method_parameters_off strengthening | Task 6 ✓ |
| Section 4: probe_method_return_type_off strengthening | Task 6 ✓ |
| Section 4: config.rs wiring flip | Task 6 ✓ |
| Section 5: _t façade refactor | Task 3 (read_t/write_t) + Task 4 (field_addr_t) ✓ |
| Section 5: Regression strategy | Task 7 ✓ |

**Deviation noted:** Section 5 says `read_bytes_t` and `read_cstr_t` stay unchanged. The plan honors this — they're not refactored because they don't fit the `T: MemValue` trait bound (variable-length values).

**2. Placeholder scan:** No TBDs / vague verbs. Every code block is complete. The Iter<RawFrame> task (Task 5) has a small "adjust if FrameRing's get/len signatures differ" branch — that's INTENTIONAL because I haven't verified FrameRing's exact API surface. The plan tells the engineer how to verify + adapt.

**3. Type consistency:**
- `Read<T>`, `Write<T>`, `Iter<T>` named identically across all 7 tasks.
- `FieldAddr` shape consistent (Section 2 + Task 2 + Task 3 + Task 4 all reference `.addr` + `.val_type`).
- `FieldInfo` fields (name_ptr/offset/val_type/token) consistent across Task 2 + Task 4.
- `apply_offset_phase2` (lowercase _phase2 suffix) consistent with B-3's existing helper.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-30-b4-trait-spine-plan.md`. **7 tasks**, mapping cleanly to spec sections.

Two execution options:

**1. Subagent-Driven (recommended)** — per the [[subagents-use-opus]] memory pattern:
- **Sonnet:** T1 (trait defs, mechanical), T2 (struct defs, mechanical), T3 (refactor + façade), T5 (wrap existing iteration), T7 (manual gate)
- **Opus:** T4 (iterator state structs touching FFI + safety), T6 (load-bearing probe strengthening — structural validators with cross-phase dependency on Phase 4 fallback constants)

**2. Inline Execution** — execute each task in this session with checkpoints between for your review.

Which approach?
