//! The 5 external memory ops over raw process memory. Reads validate via the
//! near-zero region cache; writes use the proven guarded write. Returns typed
//! Values / negative status codes (see agent_core::mem_value::status).

use agent_core::mem_value::{status, ValType, Value};
use agent_core::spine::{MemAddr, MemError, MemValue, ReadWrite};

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
    cache::snapshot().into_iter().map(|(s, e, p)| (s, e - s, p)).collect()
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

// ── mem_backend registration ─────────────────────────────────────────────────
// The Read<T>/Write<T> impls for MemAddr<C> and FieldAddr live in agent_core
// (where those types are defined, satisfying the orphan rule). They delegate
// I/O to the static vtable in `agent_core::spine::mem_backend`. We register
// the real Windows-backed implementations here, at the external API layer, so
// no FFI reaches into agent_core.

/// Raw validated read: copies `len` bytes from `addr` into `out` after checking
/// the region cache. Returns `true` on success.
///
/// # Safety
/// `out` must point to a buffer of at least `len` bytes.
unsafe fn backend_read(addr: usize, out: *mut u8, len: usize) -> bool {
    if !cache::validate_read(addr, len) {
        return false;
    }
    std::ptr::copy_nonoverlapping(addr as *const u8, out, len);
    true
}

/// Raw guarded write: writes `len` bytes from `src` into `addr` using
/// the proven VirtualProtect guard. Returns `true` on success.
///
/// # Safety
/// `src` must point to a buffer of at least `len` bytes.
unsafe fn backend_write(addr: usize, src: *const u8, len: usize) -> bool {
    let slice = std::slice::from_raw_parts(src, len);
    guarded_write(addr, slice).is_ok()
}

/// Register the Windows-backed read/write implementations with agent_core's
/// mem_backend vtable. Must be called once before any `Read<T>`/`Write<T>`
/// trait methods are invoked (i.e. after `cache::start_refresher()`).
pub fn register_mem_backend() {
    agent_core::spine::mem_backend::register(backend_read, backend_write);
}

/// Typed read: `let v: u32 = api::read_t(addr)?;`. Accepts a `MemAddr` of any
/// capability (reads work on ReadOnly and ReadWrite alike).
pub fn read_t<T: MemValue, C>(addr: MemAddr<C>) -> Result<T, MemError> {
    agent_core::spine::Read::<T>::read(&addr)
}

/// Typed write: requires `MemAddr<ReadWrite>` — passing a ReadOnly handle is a
/// compile-time error (the trait bound on the parameter type rejects it).
pub fn write_t<T: MemValue>(addr: MemAddr<ReadWrite>, val: T) -> Result<(), MemError> {
    agent_core::spine::Write::<T>::write(&addr, val)
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

#[cfg(test)]
mod spine_tests {
    use super::*;
    use agent_core::spine::ReadOnly;

    // These tests exercise only the trait + error mapping (no FFI) by going
    // through encode/decode directly. The actual cache-backed reads are
    // proven by the live WASM probes in Task 8.

    #[test]
    fn read_t_compiles_against_any_capability() {
        // Sanity: the signature accepts both capabilities. We don't read
        // (cache isn't initialized in a unit test), but we prove the
        // bounds typecheck by casting to function pointer types.
        let _: fn(MemAddr<ReadOnly>)  -> Result<u32, MemError> = read_t::<u32, ReadOnly>;
        let _: fn(MemAddr<ReadWrite>) -> Result<u32, MemError> = read_t::<u32, ReadWrite>;
    }

    #[test]
    fn write_t_only_accepts_readwrite() {
        // Compile-time proof: write_t signature is MemAddr<ReadWrite> only.
        // The negative case (passing ReadOnly) is in agent-core/tests/spine.rs
        // and the addr.rs compile_fail doc test.
        let _: fn(MemAddr<ReadWrite>, u32) -> Result<(), MemError> = write_t::<u32>;
    }
}
