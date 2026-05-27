//! The 4 proven-machinery internals ops, by name. Structural walks (klass/
//! FieldInfo) go through external's validated cache reads; instance values go
//! through external's typed read. Emits external's (offset, ValType) currency.

use std::ffi::c_void;

use agent_core::mem_value::{status, valtype_from_tc, ValType, Value};

use crate::external::{api as ext, cache};
use crate::internals::ctx;
use crate::internals::ffi::{cstr_to_string, FieldInfo, Il2CppClass};

/// Search the live class table for a class whose name (or "Namespace::Name")
/// matches `name`. Returns the klass ptr, or 0.
pub fn find_class(name: &str) -> u64 {
    let c = match ctx::get() { Some(c) => c, None => return 0 };
    for i in 0..c.table_count {
        let slot = c.table_base.wrapping_add(i * c.cfg.class_table_step);
        let klass = match cache::read_u64(slot) { Some(k) if k != 0 => k as usize, _ => continue };
        let cn = unsafe { cstr_to_string((c.api.class_get_name)(klass as *mut Il2CppClass)) };
        if cn.is_empty() { continue; }
        if cn == name { return klass as u64; }
        let ns = unsafe { cstr_to_string((c.api.class_get_namespace)(klass as *mut Il2CppClass)) };
        let full = if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
        if full == name { return klass as u64; }
    }
    0
}

/// Walk a klass's FieldInfo array, invoking `f(name, offset, type_ptr)` per field.
/// FFI iterator when available, else the 32-byte memory-walk fallback.
fn for_each_field(klass: usize, mut f: impl FnMut(&str, u32, usize) -> bool) {
    let c = match ctx::get() { Some(c) => c, None => return };
    if let Some(get_fields) = c.api.class_get_fields {
        let mut iter: *mut c_void = std::ptr::null_mut();
        for _ in 0..256 {
            let fi = unsafe { get_fields(klass as *mut Il2CppClass, &mut iter) };
            if fi.is_null() { break; }
            let name = unsafe { cstr_to_string((c.api.field_get_name)(fi)) };
            let type_ptr = unsafe { (c.api.field_get_type)(fi) } as usize;
            let offset = cache::read_u32(fi as usize + 24).unwrap_or(0);
            if f(&name, offset, type_ptr) { return; }
        }
    } else {
        let fields_ptr = match cache::read_u64(klass + c.cfg.klass_fields) {
            Some(p) if p != 0 => p as usize,
            _ => return,
        };
        for fi in 0..256usize {
            let slot = fields_ptr + fi * 32;
            let name_ptr = match cache::read_u64(slot) { Some(p) if p != 0 => p as usize, _ => break };
            let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => break };
            let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
            let offset = cache::read_u32(slot + 24).unwrap_or(0);
            if f(&name, offset, type_ptr) { return; }
        }
    }
}

/// Read the `tc` discriminator of an Il2CppType ptr (same as the resolver).
fn type_tc(type_ptr: usize) -> u8 {
    let c = match ctx::get() { Some(c) => c, None => return 0 };
    let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8
}

/// Field offset + external ValType for `name`, or None. The composition bridge.
pub fn field_info(klass: u64, name: &str) -> Option<(u32, ValType)> {
    let mut found = None;
    for_each_field(klass as usize, |fname, offset, type_ptr| {
        if fname == name {
            let vt = valtype_from_tc(type_tc(type_ptr)).unwrap_or(ValType::U64);
            found = Some((offset, vt));
            true
        } else {
            false
        }
    });
    found
}

/// Read a field by name through external's validated read. The native read.
pub fn get_field(instance: u64, klass: u64, name: &str) -> Result<Value, i32> {
    let (offset, vt) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
    let addr = (instance as usize).wrapping_add(offset as usize);
    ext::read(addr, vt, vt.fixed_width().unwrap_or(8))
}

/// The klass pointer at an object's head ("what is this object?"). 0 = unreadable.
pub fn klass_of(instance: u64) -> u64 {
    cache::read_u64(instance as usize).unwrap_or(0)
}
