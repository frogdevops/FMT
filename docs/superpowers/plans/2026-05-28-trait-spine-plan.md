# Trait-Architecture Spine — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the agent-core trait spine (handles, capability markers, `MemValue`, `MemError`) and typed siblings on `internals::api` + `external::api`, so future Spec-2 ops (invoke / hook / poll / inject) are born type-checked across all three domains.

**Architecture:** Pure-type spine in `agent-core/src/spine/` (newtype handles, `ReadOnly`/`ReadWrite` markers, `MemAddr<C>`, `MemValue` trait that does byte↔T conversion only). Agent-side `read<T>` / `write<T>` free functions consume the trait and call existing `cache::*` / `guarded_write` IO. Existing raw-`u64` API stays unchanged in this brick — typed siblings land beside it. No script-facing WASM ABI change.

**Tech Stack:** Rust 2021, `cargo`, no new dependencies. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (agent-core tests).

**Spec:** `docs/superpowers/specs/2026-05-28-trait-spine-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent-core/src/spine/mod.rs` | Create | Re-exports for handles, markers, `MemAddr`, `MemValue`, `MemError`. |
| `crates/agent-core/src/spine/error.rs` | Create | `MemError` enum + `From<MemError> for i32` (maps to `mem_value::status` codes). |
| `crates/agent-core/src/spine/addr.rs` | Create | `ReadOnly`, `ReadWrite` markers; `MemAddr<C>` with safe/unsafe constructors and capability conversions. |
| `crates/agent-core/src/spine/handles.rs` | Create | `KlassPtr`, `MethodPtr`, `Instance`, `FrameSeq`, `SocketHandle`. |
| `crates/agent-core/src/spine/value.rs` | Create | `MemValue` trait (pure byte↔T) + impls for `u8..u64 / i8..i64 / f32 / f64`. |
| `crates/agent-core/src/lib.rs` | Modify | Register `pub mod spine;`. |
| `crates/agent-core/tests/spine.rs` | Create | Integration tests: handle sizes, conversions, round-trips, error mapping, and `trybuild`-style compile_fail proof for the capability gate. |
| `crates/agent/src/internals/api.rs` | Modify | Add typed siblings: `find_class_t`, `find_method_t`, `field_addr_t`, `static_field_t`, `klass_of_t`. Raw fns stay. |
| `crates/agent/src/external/api.rs` | Modify | Add typed siblings: `read_t<T>`, `write_t<T>`, `read_bytes_t`, `read_cstr_t`. Raw fns stay. |
| `deploy.sh` | (no change) | Used for the PW gate at the end. |

**Naming convention for typed siblings:** existing raw functions keep their names (no churn at WASM-host call sites). The typed sibling is the same name with a `_t` suffix (e.g. `find_class` raw, `find_class_t` typed). This is the path of least churn for the current brick; renaming the typed ones to the canonical names is a post-merge cleanup once raw is deleted.

---

## Task 1: `MemError` + status conversion

**Files:**
- Create: `crates/agent-core/src/spine/mod.rs`
- Create: `crates/agent-core/src/spine/error.rs`
- Modify: `crates/agent-core/src/lib.rs`

- [ ] **Step 1: Register the spine module**

Open `crates/agent-core/src/lib.rs` and add the `pub mod spine;` declaration alongside existing modules. Insert near the other `pub mod` lines (the file currently lists `logfile`, `mem_value`, `mem_write`, etc.):

```rust
pub mod spine;
```

- [ ] **Step 2: Create the module stub**

Create `crates/agent-core/src/spine/mod.rs`:

```rust
//! Trait-architecture spine: typed handles + capability markers + MemValue +
//! MemError. The structural backbone that lets the three Spec-2 domains
//! (mem / il2cpp / proto) compose by type rather than by raw u64 handoff.
//! See docs/superpowers/specs/2026-05-28-trait-spine-design.md.

pub mod error;

pub use error::MemError;
```

- [ ] **Step 3: Write the failing test**

Create `crates/agent-core/tests/spine.rs`:

```rust
use agent_core::mem_value::status;
use agent_core::spine::MemError;

#[test]
fn mem_error_maps_to_existing_status_codes() {
    assert_eq!(i32::from(MemError::Unreadable),   status::ERR_UNREADABLE);
    assert_eq!(i32::from(MemError::Unwritable),   status::ERR_UNWRITABLE);
    assert_eq!(i32::from(MemError::BadType),      status::ERR_BAD_TYPE);
    assert_eq!(i32::from(MemError::BufTooSmall),  status::ERR_BUF_TOO_SMALL);
    assert_eq!(i32::from(MemError::Denied),       status::ERR_DENIED);
}
```

- [ ] **Step 4: Run the test (expect FAIL)**

Run: `cargo test -p agent-core --test spine`
Expected: compilation error — `MemError` unresolved (file doesn't exist yet).

- [ ] **Step 5: Implement `MemError`**

Create `crates/agent-core/src/spine/error.rs`:

```rust
//! Typed error model for the spine. Round-trips to the existing status codes
//! used at the WASM-host ABI boundary.

use crate::mem_value::status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    Unreadable,
    Unwritable,
    BadType,
    BufTooSmall,
    Denied,
    Changed,
}

impl From<MemError> for i32 {
    fn from(e: MemError) -> i32 {
        match e {
            MemError::Unreadable  => status::ERR_UNREADABLE,
            MemError::Unwritable  => status::ERR_UNWRITABLE,
            MemError::BadType     => status::ERR_BAD_TYPE,
            MemError::BufTooSmall => status::ERR_BUF_TOO_SMALL,
            MemError::Denied      => status::ERR_DENIED,
            MemError::Changed     => status::CHANGED,
        }
    }
}
```

- [ ] **Step 6: Run the test (expect PASS)**

Run: `cargo test -p agent-core --test spine`
Expected: 1 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/agent-core/src/lib.rs \
        crates/agent-core/src/spine/mod.rs \
        crates/agent-core/src/spine/error.rs \
        crates/agent-core/tests/spine.rs
git commit -m "spine: MemError enum + status-code round-trip"
```

---

## Task 2: `MemAddr<C>` + capability markers

**Files:**
- Create: `crates/agent-core/src/spine/addr.rs`
- Modify: `crates/agent-core/src/spine/mod.rs`
- Modify: `crates/agent-core/tests/spine.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/agent-core/tests/spine.rs`:

```rust
use agent_core::spine::{MemAddr, ReadOnly, ReadWrite};

#[test]
fn mem_addr_is_pointer_sized() {
    assert_eq!(std::mem::size_of::<MemAddr<ReadOnly>>(), 8);
    assert_eq!(std::mem::size_of::<MemAddr<ReadWrite>>(), 8);
}

#[test]
fn from_raw_round_trips_readonly() {
    let a = MemAddr::from_raw(0x1234_5678_DEAD_BEEF);
    assert_eq!(a.as_u64(), 0x1234_5678_DEAD_BEEF);
}

#[test]
fn from_raw_writable_round_trips() {
    // SAFETY: test only — no real memory at this address.
    let a = unsafe { MemAddr::from_raw_writable(0xAAAA_BBBB_CCCC_DDDD) };
    assert_eq!(a.as_u64(), 0xAAAA_BBBB_CCCC_DDDD);
}

#[test]
fn writable_downgrades_to_readonly() {
    let w = unsafe { MemAddr::from_raw_writable(0x42) };
    let r: MemAddr<ReadOnly> = w.as_readonly();
    assert_eq!(r.as_u64(), 0x42);
}

#[test]
fn readonly_upgrade_round_trips() {
    let r = MemAddr::from_raw(0x99);
    let w: MemAddr<ReadWrite> = unsafe { r.mark_writable() };
    assert_eq!(w.as_u64(), 0x99);
}
```

- [ ] **Step 2: Run tests (expect FAIL)**

Run: `cargo test -p agent-core --test spine`
Expected: compilation errors — `MemAddr`, `ReadOnly`, `ReadWrite` unresolved.

- [ ] **Step 3: Implement `MemAddr<C>` + markers**

Create `crates/agent-core/src/spine/addr.rs`:

```rust
//! Memory addresses with a compile-time capability marker. `MemAddr<ReadOnly>`
//! is the safe default; `MemAddr<ReadWrite>` is required by `mem::write`. The
//! producer of the address picks the capability based on intent (e.g.
//! `il2cpp::field_addr` returns ReadWrite — instance fields are writable;
//! `mem::scan` results are ReadOnly — the caller knows nothing).

use std::marker::PhantomData;

/// Zero-sized capability marker: address is read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnly;

/// Zero-sized capability marker: address is read+write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWrite;

/// A memory address tagged with its capability. `#[repr(transparent)]` over
/// `u64` — zero runtime cost vs. a raw pointer.
#[derive(Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct MemAddr<C = ReadOnly> {
    addr: u64,
    _cap: PhantomData<C>,
}

impl<C> Clone for MemAddr<C> {
    fn clone(&self) -> Self { *self }
}
impl<C> Copy for MemAddr<C> {}

impl<C> MemAddr<C> {
    /// Raw integer value of the address — for FFI / dispatcher boundaries only.
    #[inline]
    pub fn as_u64(self) -> u64 { self.addr }
}

impl MemAddr<ReadOnly> {
    /// Safe constructor — every raw `u64` from outside the spine becomes
    /// ReadOnly by default. Upgrade to ReadWrite requires the explicit unsafe
    /// `mark_writable`, which is the assertion that the caller knows the
    /// address points into a region writable by the agent.
    #[inline]
    pub fn from_raw(addr: u64) -> MemAddr<ReadOnly> {
        MemAddr { addr, _cap: PhantomData }
    }

    /// Upgrade a ReadOnly address to ReadWrite.
    ///
    /// # Safety
    /// The caller asserts that this address points into a region that can be
    /// mutated (this is the same trust boundary that `guarded_write` enforces
    /// at runtime; the `unsafe` keyword makes the assertion visible at the
    /// call site).
    #[inline]
    pub unsafe fn mark_writable(self) -> MemAddr<ReadWrite> {
        MemAddr { addr: self.addr, _cap: PhantomData }
    }
}

impl MemAddr<ReadWrite> {
    /// Construct a ReadWrite address from a raw `u64`.
    ///
    /// # Safety
    /// Same assertion as `mark_writable`: the caller asserts mutability.
    #[inline]
    pub unsafe fn from_raw_writable(addr: u64) -> MemAddr<ReadWrite> {
        MemAddr { addr, _cap: PhantomData }
    }

    /// Downgrade to ReadOnly — always safe (giving callers narrower access).
    #[inline]
    pub fn as_readonly(self) -> MemAddr<ReadOnly> {
        MemAddr { addr: self.addr, _cap: PhantomData }
    }
}
```

- [ ] **Step 4: Re-export from the spine module**

Modify `crates/agent-core/src/spine/mod.rs` — add the module + re-exports:

```rust
//! Trait-architecture spine: typed handles + capability markers + MemValue +
//! MemError. The structural backbone that lets the three Spec-2 domains
//! (mem / il2cpp / proto) compose by type rather than by raw u64 handoff.
//! See docs/superpowers/specs/2026-05-28-trait-spine-design.md.

pub mod addr;
pub mod error;

pub use addr::{MemAddr, ReadOnly, ReadWrite};
pub use error::MemError;
```

- [ ] **Step 5: Run tests (expect PASS)**

Run: `cargo test -p agent-core --test spine`
Expected: 6 passed (1 from Task 1 + 5 new).

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/spine/addr.rs \
        crates/agent-core/src/spine/mod.rs \
        crates/agent-core/tests/spine.rs
git commit -m "spine: MemAddr<C> + ReadOnly/ReadWrite capability markers"
```

---

## Task 3: Domain handle newtypes

**Files:**
- Create: `crates/agent-core/src/spine/handles.rs`
- Modify: `crates/agent-core/src/spine/mod.rs`
- Modify: `crates/agent-core/tests/spine.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/agent-core/tests/spine.rs`:

```rust
use agent_core::spine::{FrameSeq, Instance, KlassPtr, MethodPtr, SocketHandle};

#[test]
fn handles_are_pointer_sized() {
    assert_eq!(std::mem::size_of::<KlassPtr>(),     8);
    assert_eq!(std::mem::size_of::<MethodPtr>(),    8);
    assert_eq!(std::mem::size_of::<Instance>(),     8);
    assert_eq!(std::mem::size_of::<FrameSeq>(),     8);
    assert_eq!(std::mem::size_of::<SocketHandle>(), 8);
}

#[test]
fn handle_round_trips() {
    assert_eq!(KlassPtr::from_raw(0xAAA).as_u64(),     0xAAA);
    assert_eq!(MethodPtr::from_raw(0xBBB).as_u64(),    0xBBB);
    assert_eq!(Instance::from_raw(0xCCC).as_u64(),     0xCCC);
    assert_eq!(FrameSeq::from_raw(7).as_u64(),         7);
    assert_eq!(SocketHandle::from_raw(0xDDD).as_u64(), 0xDDD);
}
```

- [ ] **Step 2: Run tests (expect FAIL — unresolved imports)**

Run: `cargo test -p agent-core --test spine`
Expected: compilation errors for handle imports.

- [ ] **Step 3: Implement handles**

Create `crates/agent-core/src/spine/handles.rs`:

```rust
//! Domain-specific handle newtypes. Each wraps a `u64` with no runtime cost
//! (`#[repr(transparent)]`) and prevents accidental cross-domain confusion at
//! compile time (e.g. a `KlassPtr` cannot be passed where a `MemAddr` is
//! expected). None of these carry capability markers — there is no read/write
//! distinction on a klass, method, or frame sequence number.

macro_rules! handle_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(transparent)]
        pub struct $name(u64);

        impl $name {
            #[inline]
            pub fn from_raw(v: u64) -> Self { Self(v) }
            #[inline]
            pub fn as_u64(self) -> u64 { self.0 }
        }
    };
}

handle_newtype!(KlassPtr,     "An `Il2CppClass*` — the il2cpp class handle.");
handle_newtype!(MethodPtr,    "A `MethodInfo*` — the il2cpp method handle.");
handle_newtype!(Instance,     "An object instance pointer.");
handle_newtype!(FrameSeq,     "A bookmark into the protocol frame ring.");
handle_newtype!(SocketHandle, "A tracked WinSock socket (proto.send / inject).");
```

- [ ] **Step 4: Re-export from the spine module**

Modify `crates/agent-core/src/spine/mod.rs`:

```rust
//! Trait-architecture spine: typed handles + capability markers + MemValue +
//! MemError. The structural backbone that lets the three Spec-2 domains
//! (mem / il2cpp / proto) compose by type rather than by raw u64 handoff.
//! See docs/superpowers/specs/2026-05-28-trait-spine-design.md.

pub mod addr;
pub mod error;
pub mod handles;

pub use addr::{MemAddr, ReadOnly, ReadWrite};
pub use error::MemError;
pub use handles::{FrameSeq, Instance, KlassPtr, MethodPtr, SocketHandle};
```

- [ ] **Step 5: Run tests (expect PASS)**

Run: `cargo test -p agent-core --test spine`
Expected: 8 passed total.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/spine/handles.rs \
        crates/agent-core/src/spine/mod.rs \
        crates/agent-core/tests/spine.rs
git commit -m "spine: KlassPtr/MethodPtr/Instance/FrameSeq/SocketHandle handles"
```

---

## Task 4: `MemValue` trait + numeric impls

**Files:**
- Create: `crates/agent-core/src/spine/value.rs`
- Modify: `crates/agent-core/src/spine/mod.rs`
- Modify: `crates/agent-core/tests/spine.rs`

**Design note:** The trait is pure byte↔T conversion + type tag (no IO). That keeps it in `agent-core` (Linux-testable, no FFI). The actual memory read/write lives in agent-side wrappers (Task 7) that take `T: MemValue` and call `cache::*` / `guarded_write`. The user-facing call site shape `let v: u32 = mem::read(addr)?` is unchanged.

- [ ] **Step 1: Write the failing round-trip tests**

Append to `crates/agent-core/tests/spine.rs`:

```rust
use agent_core::mem_value::ValType;
use agent_core::spine::MemValue;

#[test]
fn mem_value_u32_round_trip() {
    let v: u32 = 0xDEAD_BEEF;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 4);
    let back: u32 = u32::from_le_bytes_spine(&buf).unwrap();
    assert_eq!(back, v);
    assert_eq!(<u32 as MemValue>::VAL_TYPE, ValType::U32);
}

#[test]
fn mem_value_i64_round_trip() {
    let v: i64 = -42;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 8);
    assert_eq!(i64::from_le_bytes_spine(&buf), Some(v));
    assert_eq!(<i64 as MemValue>::VAL_TYPE, ValType::I64);
}

#[test]
fn mem_value_f32_round_trip() {
    let v: f32 = 3.14159;
    let buf = v.to_le_bytes_buf();
    assert_eq!(buf.len(), 4);
    assert_eq!(f32::from_le_bytes_spine(&buf), Some(v));
}

#[test]
fn mem_value_rejects_short_buffer() {
    let buf = [0u8; 2];
    assert!(u32::from_le_bytes_spine(&buf).is_none());
}

#[test]
fn mem_value_all_numerics_have_a_val_type() {
    assert_eq!(<u8  as MemValue>::VAL_TYPE, ValType::U8);
    assert_eq!(<u16 as MemValue>::VAL_TYPE, ValType::U16);
    assert_eq!(<u64 as MemValue>::VAL_TYPE, ValType::U64);
    assert_eq!(<i8  as MemValue>::VAL_TYPE, ValType::I8);
    assert_eq!(<i16 as MemValue>::VAL_TYPE, ValType::I16);
    assert_eq!(<i32 as MemValue>::VAL_TYPE, ValType::I32);
    assert_eq!(<f64 as MemValue>::VAL_TYPE, ValType::F64);
}
```

(Method names are `to_le_bytes_buf` and `from_le_bytes_spine` to avoid colliding with the inherent `from_le_bytes` / `to_le_bytes` on primitives.)

- [ ] **Step 2: Run tests (expect FAIL — unresolved trait)**

Run: `cargo test -p agent-core --test spine`
Expected: compilation errors.

- [ ] **Step 3: Implement `MemValue`**

Create `crates/agent-core/src/spine/value.rs`:

```rust
//! `MemValue`: the pure byte↔T conversion trait shared by every typed `mem::read`
//! / `mem::write` call site. Holding this in `agent-core` keeps it free of FFI
//! and host-testable on Linux. The agent-side `read<T>` / `write<T>` wrappers
//! (in `crates/agent/src/external/api.rs`) consume the trait and do the actual
//! validated memory IO.

use crate::mem_value::ValType;

/// A value that can be read from / written to process memory.
///
/// Variable-length values (`Bytes`, `Cstr`) are not `MemValue` impls — they
/// need a length argument that the trait shape can't carry. They live as free
/// functions on the agent side (`read_bytes_t`, `read_cstr_t`).
pub trait MemValue: Sized + Copy {
    const VAL_TYPE: ValType;

    /// Decode a value from a little-endian byte slice. Returns `None` if the
    /// slice is shorter than the type's width.
    fn from_le_bytes_spine(bytes: &[u8]) -> Option<Self>;

    /// Encode the value as a little-endian byte vector. Length is always
    /// `Self::VAL_TYPE.fixed_width().unwrap()`.
    fn to_le_bytes_buf(self) -> Vec<u8>;
}

macro_rules! impl_mem_value_numeric {
    ($t:ty, $vt:expr, $width:expr) => {
        impl MemValue for $t {
            const VAL_TYPE: ValType = $vt;
            #[inline]
            fn from_le_bytes_spine(bytes: &[u8]) -> Option<Self> {
                if bytes.len() < $width { return None; }
                let mut buf = [0u8; $width];
                buf.copy_from_slice(&bytes[..$width]);
                Some(<$t>::from_le_bytes(buf))
            }
            #[inline]
            fn to_le_bytes_buf(self) -> Vec<u8> {
                self.to_le_bytes().to_vec()
            }
        }
    };
}

impl_mem_value_numeric!(u8,  ValType::U8,  1);
impl_mem_value_numeric!(u16, ValType::U16, 2);
impl_mem_value_numeric!(u32, ValType::U32, 4);
impl_mem_value_numeric!(u64, ValType::U64, 8);
impl_mem_value_numeric!(i8,  ValType::I8,  1);
impl_mem_value_numeric!(i16, ValType::I16, 2);
impl_mem_value_numeric!(i32, ValType::I32, 4);
impl_mem_value_numeric!(i64, ValType::I64, 8);
impl_mem_value_numeric!(f32, ValType::F32, 4);
impl_mem_value_numeric!(f64, ValType::F64, 8);
```

- [ ] **Step 4: Re-export from the spine module**

Modify `crates/agent-core/src/spine/mod.rs`:

```rust
//! Trait-architecture spine: typed handles + capability markers + MemValue +
//! MemError. The structural backbone that lets the three Spec-2 domains
//! (mem / il2cpp / proto) compose by type rather than by raw u64 handoff.
//! See docs/superpowers/specs/2026-05-28-trait-spine-design.md.

pub mod addr;
pub mod error;
pub mod handles;
pub mod value;

pub use addr::{MemAddr, ReadOnly, ReadWrite};
pub use error::MemError;
pub use handles::{FrameSeq, Instance, KlassPtr, MethodPtr, SocketHandle};
pub use value::MemValue;
```

- [ ] **Step 5: Run tests (expect PASS)**

Run: `cargo test -p agent-core --test spine`
Expected: 13 passed total.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/spine/value.rs \
        crates/agent-core/src/spine/mod.rs \
        crates/agent-core/tests/spine.rs
git commit -m "spine: MemValue trait + numeric impls (u8..u64/i8..i64/f32/f64)"
```

---

## Task 5: Compile-fail proof for the capability gate

The whole point of the spine is "wrong-by-construction caught at `cargo check`." This task adds a doc test that proves it.

**Files:**
- Modify: `crates/agent-core/src/spine/addr.rs`

- [ ] **Step 1: Add a `compile_fail` doc test**

Append the following module-level doc test at the top of `crates/agent-core/src/spine/addr.rs`, just below the existing `//!` module docstring:

```rust
//! # Capability gate proof
//!
//! The capability marker prevents an agent-side caller from accidentally
//! writing through a ReadOnly handle. A function constrained to
//! `MemAddr<ReadWrite>` rejects a ReadOnly argument at the compiler:
//!
//! ```compile_fail
//! use agent_core::spine::{MemAddr, ReadOnly, ReadWrite};
//!
//! fn write_only(_a: MemAddr<ReadWrite>) {}
//!
//! let r: MemAddr<ReadOnly> = MemAddr::from_raw(0x1000);
//! write_only(r); // ERROR: expected MemAddr<ReadWrite>, found MemAddr<ReadOnly>
//! ```
//!
//! The safe downgrade is always available:
//!
//! ```
//! use agent_core::spine::{MemAddr, ReadOnly, ReadWrite};
//!
//! let w: MemAddr<ReadWrite> = unsafe { MemAddr::from_raw_writable(0x1000) };
//! let _r: MemAddr<ReadOnly> = w.as_readonly();
//! ```
```

(Place the new doc above the existing `use std::marker::PhantomData;` line. The existing line-doc comment on the module already has `//!`; the new block extends it.)

- [ ] **Step 2: Run doc tests (expect PASS)**

Run: `cargo test -p agent-core --doc spine`
Expected: 2 passed (one `compile_fail` succeeded by failing to compile, one ordinary doc-test passes). Also still: `cargo test -p agent-core --test spine` shows 13 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/agent-core/src/spine/addr.rs
git commit -m "spine: doc test proving capability gate at the compiler"
```

---

## Task 6: Typed siblings on `internals::api`

**Files:**
- Modify: `crates/agent/src/internals/api.rs`

The five typed siblings each delegate to the existing raw fn body, then wrap the result in the spine type. Raw fns stay unchanged.

- [ ] **Step 1: Add the typed `find_class_t`**

In `crates/agent/src/internals/api.rs`, after the existing `find_class` function (ends around line 29), add:

```rust
/// Typed sibling of `find_class`. Returns `Some(KlassPtr)` when found, `None`
/// otherwise. Uses the spine vocabulary; raw `find_class` remains for the
/// existing WASM-host call site.
pub fn find_class_t(name: &str) -> Option<agent_core::spine::KlassPtr> {
    match find_class(name) {
        0 => None,
        k => Some(agent_core::spine::KlassPtr::from_raw(k)),
    }
}
```

- [ ] **Step 2: Add the typed `find_method_t`**

After the existing `find_method` (ends around line 150), add:

```rust
/// Typed sibling of `find_method`. Returns `Some(MethodPtr)` when found.
pub fn find_method_t(
    klass: agent_core::spine::KlassPtr,
    name: &str,
    argc: u32,
) -> Option<agent_core::spine::MethodPtr> {
    match find_method(klass.as_u64(), name, argc) {
        0 => None,
        m => Some(agent_core::spine::MethodPtr::from_raw(m)),
    }
}
```

- [ ] **Step 3: Add the typed `field_addr_t`**

After `find_method_t`, add:

```rust
/// Typed address of an instance field: `instance + field_info(klass, name).offset`.
/// Returns `MemAddr<ReadWrite>` — instance fields are writable by intent.
/// Returns `None` if the field is not found on the class.
pub fn field_addr_t(
    klass: agent_core::spine::KlassPtr,
    name: &str,
    instance: agent_core::spine::Instance,
) -> Option<agent_core::spine::MemAddr<agent_core::spine::ReadWrite>> {
    let (offset, _vt) = field_info(klass.as_u64(), name)?;
    let addr = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    // SAFETY: caller obtained `instance` via the spine API; instance fields
    // are writable by their semantic role.
    Some(unsafe { agent_core::spine::MemAddr::from_raw_writable(addr) })
}
```

- [ ] **Step 4: Add the typed `static_field_t`**

After `field_addr_t`, add:

```rust
/// Typed address of a static field. Returns `MemAddr<ReadWrite>` — statics
/// are writable by intent. Returns `None` if the field is not found or not
/// actually static on this class.
pub fn static_field_t(
    klass: agent_core::spine::KlassPtr,
    name: &str,
) -> Option<agent_core::spine::MemAddr<agent_core::spine::ReadWrite>> {
    match static_field(klass.as_u64(), name) {
        0 => None,
        // SAFETY: static_field returns 0 unless the field is actually marked
        // FIELD_ATTRIBUTE_STATIC; the static base lives in a writable region.
        a => Some(unsafe { agent_core::spine::MemAddr::from_raw_writable(a) }),
    }
}
```

- [ ] **Step 5: Add the typed `klass_of_t`**

After `static_field_t`, add:

```rust
/// Typed sibling of `klass_of`. Returns `None` if the instance head is
/// unreadable or zero.
pub fn klass_of_t(
    instance: agent_core::spine::Instance,
) -> Option<agent_core::spine::KlassPtr> {
    match klass_of(instance.as_u64()) {
        0 => None,
        k => Some(agent_core::spine::KlassPtr::from_raw(k)),
    }
}
```

- [ ] **Step 6: Build (expect PASS, no warnings about unused)**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean build. (If "unused function" warnings appear on the `_t` siblings, ignore them for this brick — Task 7 will exercise them transitively, and the WASM-side ops in 2c/2d will be the real consumers.)

- [ ] **Step 7: Commit**

```bash
git add crates/agent/src/internals/api.rs
git commit -m "internals: typed siblings (find_class_t, find_method_t, field_addr_t, static_field_t, klass_of_t)"
```

---

## Task 7: Typed siblings on `external::api`

**Files:**
- Modify: `crates/agent/src/external/api.rs`

These wrap the existing `cache::validate_read` + raw pointer read pattern, and `guarded_write`. The generic `read_t<T: MemValue, C>` is the cross-domain composition target — it accepts any `MemAddr<ReadOnly>` or `MemAddr<ReadWrite>` and any numeric type that implements `MemValue`.

- [ ] **Step 1: Add the imports**

At the top of `crates/agent/src/external/api.rs`, replace the existing imports block with:

```rust
use agent_core::mem_value::{status, ValType, Value};
use agent_core::spine::{MemAddr, MemError, MemValue, ReadOnly, ReadWrite};

use crate::external::cache;
use crate::external::scan::aob_scan;
use crate::external::write::guarded_write;
```

- [ ] **Step 2: Add typed `read_t` at the end of the file**

Append to `crates/agent/src/external/api.rs`:

```rust
/// Typed read: `let v: u32 = api::read_t(addr)?;`. Accepts a `MemAddr` of any
/// capability (reads work on ReadOnly and ReadWrite alike).
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    let width = T::VAL_TYPE.fixed_width().ok_or(MemError::BadType)?;
    let a = addr.as_u64() as usize;
    if !cache::validate_read(a, width) {
        return Err(MemError::Unreadable);
    }
    let bytes = unsafe { std::slice::from_raw_parts(a as *const u8, width) };
    T::from_le_bytes_spine(bytes).ok_or(MemError::BadType)
}

/// Typed write: requires `MemAddr<ReadWrite>` — passing a ReadOnly handle is a
/// compile-time error (the trait bound on the parameter type rejects it).
pub fn write_t<T: MemValue>(addr: MemAddr<ReadWrite>, val: T) -> Result<(), MemError> {
    let bytes = val.to_le_bytes_buf();
    unsafe { guarded_write(addr.as_u64() as usize, &bytes) }.map_err(|_| MemError::Unwritable)
}

/// Typed variable-length read: bytes. Capability-agnostic.
pub fn read_bytes_t<C>(addr: MemAddr<C>, len: usize) -> Result<Vec<u8>, MemError> {
    if len == 0 {
        return Err(MemError::BadType);
    }
    let a = addr.as_u64() as usize;
    if !cache::validate_read(a, len) {
        return Err(MemError::Unreadable);
    }
    let slice = unsafe { std::slice::from_raw_parts(a as *const u8, len) };
    Ok(slice.to_vec())
}

/// Typed null-terminated C-string read with an upper bound on length.
/// Delegates to the existing crash-safe `cache::read_cstr` (which already
/// honors a 255-byte internal cap); `cap` is a future-proof argument that
/// today is documentary.
pub fn read_cstr_t<C>(addr: MemAddr<C>, _cap: usize) -> Result<String, MemError> {
    cache::read_cstr(addr.as_u64() as usize).ok_or(MemError::Unreadable)
}
```

- [ ] **Step 3: Add round-trip tests at the bottom of `external::api`**

Append at the end of `crates/agent/src/external/api.rs`:

```rust
#[cfg(test)]
mod spine_tests {
    use super::*;

    // These tests exercise only the trait + error mapping (no FFI) by going
    // through encode/decode directly. The actual cache-backed reads are
    // proven by the live WASM probes in Task 8.

    #[test]
    fn read_t_compiles_against_any_capability() {
        // Sanity: the signature accepts both capabilities. We don't read
        // (cache isn't initialized in a unit test), but we prove the
        // bounds typecheck.
        fn _accepts_ro(_a: MemAddr<ReadOnly>)  { let _ = read_t::<u32, _>; }
        fn _accepts_rw(_a: MemAddr<ReadWrite>) { let _ = read_t::<u32, _>; }
    }

    #[test]
    fn write_t_only_accepts_readwrite() {
        // Compile-time proof: write_t signature is MemAddr<ReadWrite> only.
        // The negative case (passing ReadOnly) is in agent-core/tests/spine.rs
        // and the addr.rs compile_fail doc test.
        fn _accepts_rw(_a: MemAddr<ReadWrite>) { let _ = write_t::<u32>; }
    }
}
```

- [ ] **Step 4: Build the agent + run agent-core tests**

Run in parallel:
- `cargo build --target x86_64-pc-windows-gnu --release`
- `cargo test -p agent-core`

Expected: clean build for both; all spine tests still passing (13 unit + 2 doc).

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/external/api.rs
git commit -m "external: typed siblings (read_t/write_t/read_bytes_t/read_cstr_t)"
```

---

## Task 8: PW integration gate — prove zero regression

The spine is purely additive in this brick; no host fn was changed. The proof of "no regression" is that the existing live WASM probes (`scratch/test_internals2.wat` etc.) still pass on Pixel Worlds.

**Files:** none modified; pure verification.

- [ ] **Step 1: Deploy**

Run: `./deploy.sh release`
Expected: build succeeds; deploy to Pixel Worlds (and Highrise if installed). Summary line confirms deployment.

- [ ] **Step 2: Hand the PW gate back to the user**

Pause here. Tell the user: **"Deploy is green. Please launch Pixel Worlds, run `scratch/test_internals2.wat` via the FROG_WASM env (the existing probe), and confirm the four `ok` lines still appear in the log. The spine adds typed siblings only — no host fn changed — so this is a regression check."**

Expected user response: confirms log shows the four `: ok` lines from `test_internals2.wat`. If any line shows `FAIL`, the brick has regressed — STOP and report.

- [ ] **Step 3: Commit-pause (user does the commit)**

No code change here; nothing to commit. Per the standing rule the user commits their own work — this task is verification only.

---

## Self-review

**1. Spec coverage:**
- Handle catalogue (5 newtypes) — Task 3 ✓
- Capability markers + MemAddr<C> + conversions — Task 2 ✓
- MemValue trait + numeric impls — Task 4 ✓
- MemError + status conversion — Task 1 ✓
- Crate split (spine in agent-core; impls in agent) — every task respects it ✓
- Per-domain typed surfaces — Tasks 6 + 7 ✓
- WASM host-fn dispatchers (target shape) — explicitly NOT in this brick per spec migration discipline; deferred to post-2c work ✓
- Migration discipline (raw stays; typed added beside) — Tasks 6 + 7 follow it ✓
- Cross-brick proof (composition chain) — exercised transitively by 2c/2d; trait shapes verified by tests in 4 + 7 ✓
- Testing checklist (size assertions, conversion round-trips, compile_fail capability gate, error mapping) — Tasks 1–5 + 7 cover all items ✓

**2. Placeholder scan:** No "TBD", "TODO", or vague verbs. All code blocks are complete and copy-paste ready.

**3. Type consistency:** `KlassPtr` / `MethodPtr` / `Instance` / `FrameSeq` / `SocketHandle` named identically in spec, mod re-exports, handle macro, and consumer signatures. `MemAddr<C>` parameter name `C` used consistently. `MemValue::VAL_TYPE` / `from_le_bytes_spine` / `to_le_bytes_buf` method names match across trait def, impls, and external::api consumers.

**Deviation noted (and justified):**
- Spec mentioned `#[deprecated(note = ...)]` on raw functions. This plan does NOT add deprecation markers — they would generate warnings at every existing WASM-host call site and force a noisy `#[allow(deprecated)]` sprinkle. Markers can land in a focused post-2c cleanup once the host fns are re-pointed at typed. The structural goal (raw + typed both callable) is unaffected.
- Spec showed `MemValue::read_at<C>(MemAddr<C>) -> Result<Self, MemError>` as a trait method. Plan moves the IO out of the trait (it would force FFI into agent-core, breaking host-testability) and into agent-side free functions `read_t<T, C>(MemAddr<C>) -> Result<T, MemError>`. The user-facing call shape `let v: u32 = mem::read_t(addr)?` is identical; only the implementation locus moved. All wrong-by-construction guarantees (capability + width + handle confusion) are preserved by the trait bound on the free function.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-28-trait-spine-plan.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh Opus subagent per task (per your standing preference), spec-review then code-quality-review each one, controller (me) re-checks between. Fast iteration, controller stays focused.

**2. Inline Execution** — I execute tasks in this session with checkpoints between for your review.

**Which approach?**
