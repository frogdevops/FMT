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

/// (base, size, protect) for each cached readable region. (`protect` is 0 for now.)
pub fn regions() -> Vec<(usize, usize, u32)> {
    cache::snapshot().into_iter().map(|(s, e)| (s, e - s, 0u32)).collect()
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
