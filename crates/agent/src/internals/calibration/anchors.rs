//! CTX-FREE anchor resolution for the calibration probes.
//!
//! `internals::api::find_class`/`find_method` both require `ctx::get()` to be
//! `Some`, but `ctx::init` only runs AFTER `probe()` in entry.rs — so any probe
//! that called them during calibration silently received 0 anchors and fell
//! back. These helpers walk the live class table directly using a passed-in
//! `&Il2CppApi` (mirroring `find_class`'s is_klass_shape-gated discipline) and
//! the baseline klass/method offsets, which are correct for the well-known core
//! classes used as anchors on every il2cpp build ≥ v24.
//!
//! Promoted out of `stability.rs` so all phase probes share one implementation.

use crate::external::cache;
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

/// Locate a class by "Namespace::Name" (or bare "Name") via a direct,
/// is_klass_shape-gated walk of the live class table. Self-contained — does NOT
/// depend on `ctx`. Returns the klass ptr, or 0.
pub fn local_find_class(
    api: &Il2CppApi,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
    name: &str,
) -> usize {
    for i in 0..table_count {
        let slot = table_base.wrapping_add(i * class_table_step);
        let klass = match cache::read_u64(slot) { Some(k) if k != 0 => k as usize, _ => continue };
        if !cache::is_klass_shape(klass) { continue; }
        let cn = unsafe { cstr_to_string((api.class_get_name)(klass as *mut Il2CppClass)) };
        if cn.is_empty() { continue; }
        if cn == name { return klass; }
        let ns = unsafe { cstr_to_string((api.class_get_namespace)(klass as *mut Il2CppClass)) };
        let full = if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
        if full == name { return klass; }
    }
    0
}

/// Locate a method by name + arg count on `klass` via a direct walk of the
/// klass's methods array. Self-contained — does NOT depend on `ctx`. Uses the
/// baseline klass/method offsets (the same constants `find_method` falls back
/// to), which are sufficient for the well-known core classes used as anchors.
/// Returns the MethodInfo ptr, or 0.
pub fn local_find_method(cfg: &Il2CppConfig, klass: usize, name: &str, argc: u32) -> usize {
    let methods = cache::read_u64(klass + cfg.klass_methods).unwrap_or(0) as usize;
    if methods == 0 {
        return 0;
    }
    for i in 0..4096usize {
        let mi = match cache::read_u64(methods + i * 8) {
            Some(v) if v != 0 => v as usize,
            _ => break,
        };
        // Array-end / validity: the MethodInfo's declaring-klass must be this klass.
        if cache::read_u64(mi + cfg.method_klass_off).unwrap_or(0) != klass as u64 {
            break;
        }
        let name_ptr = cache::read_u64(mi + cfg.method_name_off).unwrap_or(0) as usize;
        let mname = match cache::read_cstr(name_ptr) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let pcount = cache::read_u8(mi + cfg.method_param_count_off).unwrap_or(0) as u32;
        if mname == name && pcount == argc {
            return mi;
        }
    }
    0
}
