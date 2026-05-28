//! Memory addresses with a compile-time capability marker. `MemAddr<ReadOnly>`
//! is the safe default; `MemAddr<ReadWrite>` is required by `mem::write`. The
//! producer of the address picks the capability based on intent (e.g.
//! `il2cpp::field_addr` returns ReadWrite — instance fields are writable;
//! `mem::scan` results are ReadOnly — the caller knows nothing).
//!
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
