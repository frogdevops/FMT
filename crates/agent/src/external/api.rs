//! The 5 external memory ops over raw process memory. Reads validate via the
//! near-zero region cache; writes use the proven guarded write. Returns typed
//! Values / negative status codes (see agent_core::mem_value::status).

use agent_core::mem_value::{status, ValType, Value};

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
