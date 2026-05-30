//! Platform-agnostic memory-operation backend for `Read<T>` / `Write<T>` impls.
//!
//! `agent_core` is a pure-Rust library with no FFI. The actual read/write
//! primitives (cache validation on Windows, guarded VirtualProtect writes) live
//! in the `agent` crate. This module provides a static function-pointer vtable
//! that `agent` registers at startup so the trait impls in `spine/access.rs`
//! can delegate to the real implementations without introducing a build-time
//! dependency on Windows APIs.
//!
//! # Registration
//! Call `register(ops)` once from `agent`'s initialisation path (after the
//! region-cache refresher is started). Until registration, every `Read<T>` or
//! `Write<T>` call returns `MemError::Unreadable` / `MemError::Unwritable`.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Raw read backend: validates [addr, addr+len) and, if readable, copies `len`
/// bytes into `out`. Returns `true` on success, `false` if the range is not
/// readable. The caller guarantees `out.len() >= len`.
///
/// # Safety
/// The backend implementor must not read beyond [addr, addr+len).
pub type ReadFn  = unsafe fn(addr: usize, out: *mut u8, len: usize) -> bool;

/// Raw write backend: validates [addr, addr+len) and writes `len` bytes from
/// `src`. Returns `true` on success, `false` if the range is not writable.
///
/// # Safety
/// The backend implementor must not write beyond [addr, addr+len).
pub type WriteFn = unsafe fn(addr: usize, src: *const u8, len: usize) -> bool;

// AtomicUsize is the portable way to store a fn pointer atomically.
static RAW_READ:  AtomicUsize = AtomicUsize::new(0);
static RAW_WRITE: AtomicUsize = AtomicUsize::new(0);

/// Register the platform-specific memory operations. Call once at agent start,
/// after the region cache is initialised.
pub fn register(read: ReadFn, write: WriteFn) {
    RAW_READ .store(read  as usize, Ordering::Release);
    RAW_WRITE.store(write as usize, Ordering::Release);
}

/// Invoke the registered read backend. Returns `None` if the backend has not
/// been registered yet or if the read fails.
///
/// # Safety
/// Same contract as `ReadFn`.
pub unsafe fn raw_read(addr: usize, out: *mut u8, len: usize) -> bool {
    let p = RAW_READ.load(Ordering::Acquire);
    if p == 0 { return false; }
    let f: ReadFn = std::mem::transmute(p);
    f(addr, out, len)
}

/// Invoke the registered write backend. Returns `false` if the backend has not
/// been registered yet or if the write fails.
///
/// # Safety
/// Same contract as `WriteFn`.
pub unsafe fn raw_write(addr: usize, src: *const u8, len: usize) -> bool {
    let p = RAW_WRITE.load(Ordering::Acquire);
    if p == 0 { return false; }
    let f: WriteFn = std::mem::transmute(p);
    f(addr, src, len)
}
