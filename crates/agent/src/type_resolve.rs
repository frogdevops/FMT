//! Mapping `Il2CppType*` pointers to human-readable names.
//!
//! The runtime never stores type names in plain text on the type itself; you
//! have to walk through one of:
//!   * the primitives table (compiled in below),
//!   * the type-def map built from the class table at startup, or
//!   * `class_get_name` via FFI as a last resort.
//!
//! This module owns the name-resolution chain and the per-class lookup tables
//! that feed it. Everything is read-only and bounds-checked through `RegionMap`.

use std::collections::HashMap;
use std::os::raw::c_void;

use crate::il2cpp_config::Il2CppConfig;
use crate::il2cpp_ffi::{cstr_to_string, Il2CppApi};
use crate::paths::log;
use crate::region_map::{RegionMap, Tunables};

/// Per-run cap on how many noisy diagnostic lines we emit for unresolved
/// CLASS/VALUETYPE references and the GENERICINST struct-shape probe. These
/// are informational — the runtime path falls back gracefully — so we keep a
/// few samples for triage and drop the rest. Override at runtime by setting
/// the `FROG_DEBUG=1` environment variable.
const DIAG_SAMPLE_CAP: u32 = 5;

pub fn diag_cap() -> u32 {
    if std::env::var("FROG_DEBUG").map(|v| v != "0" && !v.is_empty()).unwrap_or(false) {
        u32::MAX
    } else {
        DIAG_SAMPLE_CAP
    }
}

/// Two lookup strategies:
/// - `td_map`: keyed by `klass + klass_type_def` value (packed typeDefIndex + flags)
/// - `klass_map`: keyed by klass address (direct pointer)
/// The klass_map is a fallback for when the td_map key doesn't match (packed flags differ).
pub struct TypeMaps {
    pub td_map: HashMap<usize, (usize, String, String)>,
    pub klass_map: HashMap<usize, (String, String)>,
}

pub fn build_type_maps(
    table_base: usize,
    table_count: usize,
    api: &Il2CppApi,
    map: &RegionMap,
    cfg: &Il2CppConfig,
) -> TypeMaps {
    let mut td_map: HashMap<usize, (usize, String, String)> = HashMap::new();
    let mut klass_map: HashMap<usize, (String, String)> = HashMap::new();
    let max = table_count.min(Tunables::load().table_max_slots);
    let mut c_slot = 0usize;
    let mut c_nonzero = 0usize;
    let mut c_td_ok = 0usize;
    let mut c_td_fail = 0usize;
    let mut c_ns_ok = 0usize;
    for i in 0..max {
        let a = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(slot) = map.read_u64(a) {
            c_slot += 1;
            let k = slot as usize;
            if k == 0 {
                continue;
            }
            c_nonzero += 1;
            let _ns_ptr = map.read_u64(k + cfg.klass_namespace);
            if _ns_ptr.is_some() {
                c_ns_ok += 1;
            }
            match map.read_u64(k + cfg.klass_type_def) {
                Some(td) => {
                    c_td_ok += 1;
                    if td == 0 {
                        continue;
                    }
                    let td = td as usize;
                    let cn = unsafe { cstr_to_string((api.class_get_name)(k as *mut c_void)) };
                    if cn.is_empty() {
                        continue;
                    }
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut c_void)) };
                    if !td_map.contains_key(&td) {
                        td_map.insert(td, (k, cn.clone(), ns.clone()));
                    }
                    klass_map.insert(k, (cn, ns));
                }
                None => {
                    c_td_fail += 1;
                }
            }
        }
    }
    log(&format!(
        "  type maps: td={} klass={} (slots={}, ptrs={}, ns_ok={}, td_ok={}, td_fail={})",
        td_map.len(),
        klass_map.len(),
        c_slot,
        c_nonzero,
        c_ns_ok,
        c_td_ok,
        c_td_fail
    ));
    TypeMaps { td_map, klass_map }
}

/// Resolve an `Il2CppType*` to a human-readable type name.
///
/// ## Resolution chain (tc = IL2CPP_TYPE_* discriminator)
/// 1. **Primitives** (0x01–0x10, 0x18–0x19, 0x1C–0x1D): hardcoded lookup.
/// 2. **VAR** (0x13): generic parameter `!0`, `!1`, … encoded in `data64`.
/// 3. **ARRAY** (0x14): recursive on element type.
/// 4. **GENERICINST** (0x15): dumps raw struct bytes (diag-capped) → `System.Generic`.
/// 5. **CLASS** (0x12) / **VALUETYPE** (0x11):
///    a. `td_map` — match `data64` as klass pointer, read klass+klass_type_def,
///       look up the packed type-def index in the pre-built map.
///    b. `klass_map` — direct pointer lookup (fallback for matches not in td_map).
///    c. FFI `class_get_name` — dynamic/proxy classes the maps missed.
///    d. Diag dump of first few MISSING instances (capped), then placeholder.
/// 6. **Unknown** → `<type:{tc}>`.
///
/// All high-volume arms are sampled at most `diag_cap()` times (normally 5)
/// to keep log noise manageable. Set `FROG_DEBUG=1` for full output.
pub fn il2cpp_type_name(
    map: &RegionMap,
    type_ptr: usize,
    type_maps: &TypeMaps,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
) -> String {
    let data64 = match map.read_u64(type_ptr) {
        Some(v) => v,
        None => return "?".into(),
    };
    let discrim = map.read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    let tc = ((discrim >> cfg.discrim_shift) & 0xFF) as u8;
    match tc {
        0x01 => return "System.Void".into(),
        0x02 => return "System.Boolean".into(),
        0x03 => return "System.Char".into(),
        0x04 => return "System.SByte".into(),
        0x05 => return "System.Byte".into(),
        0x06 => return "System.Int16".into(),
        0x07 => return "System.UInt16".into(),
        0x08 => return "System.Int32".into(),
        0x09 => return "System.UInt32".into(),
        0x0A => return "System.Int64".into(),
        0x0B => return "System.UInt64".into(),
        0x0C => return "System.Single".into(),
        0x0D => return "System.Double".into(),
        0x0E => return "System.String".into(),
        0x0F | 0x18 => return "System.IntPtr".into(),
        0x10 | 0x19 => return "System.UIntPtr".into(),
        0x13 => {
            // VAR — generic type parameter (!0, !1, etc.)
            return format!("!{}", data64 as u16);
        }
        0x14 => {
            let arr_struct = data64 as usize;
            if arr_struct != 0 {
                if let Some(elem_type_addr) = map.read_u64(arr_struct) {
                    if elem_type_addr != 0 {
                        let elem_name =
                            il2cpp_type_name(map, elem_type_addr as usize, type_maps, cfg, api);
                        return format!("{}[]", elem_name);
                    }
                }
            }
            return "System.Array".into();
        }
        0x15 => {
            // GENERICINST — data64 points to Il2CppGenericClass. Log a handful
            // of raw struct dumps so we can fingerprint the layout offline; the
            // rest are silenced unless `FROG_DEBUG=1`.
            static GEN_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if GEN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < diag_cap() {
                let mut raw = String::new();
                let gc_addr = data64 as usize;
                for off in (0..48).step_by(8) {
                    if let Some(v) = map.read_u64(gc_addr + off) {
                        raw.push_str(&format!("+{:#x}={:#018x} ", off, v));
                    }
                }
                log(&format!("  GENERICINST @ {:#x}: {}", gc_addr, raw));
            }
            return "System.Generic".into();
        }
        0x11 | 0x12 => {
            let klass_ptr = data64 as usize;
            if klass_ptr != 0 {
                if let Some(td_raw) = map.read_u64(klass_ptr + cfg.klass_type_def) {
                    let td = td_raw as usize;
                    if let Some((_, cn, cns)) = type_maps.td_map.get(&td) {
                        return if cns.is_empty() {
                            cn.clone()
                        } else {
                            format!("{}::{}", cns, cn)
                        };
                    }
                }
                if let Some((cn, cns)) = type_maps.klass_map.get(&klass_ptr) {
                    return if cns.is_empty() {
                        cn.clone()
                    } else {
                        format!("{}::{}", cns, cn)
                    };
                }
                // Ultimate fallback: query class_get_name directly via FFI for dynamic types
                let cn = unsafe { cstr_to_string((api.class_get_name)(klass_ptr as *mut c_void)) };
                if !cn.is_empty() {
                    let ns =
                        unsafe { cstr_to_string((api.class_get_namespace)(klass_ptr as *mut c_void)) };
                    return if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
                }
            }
            // CLASS/VALUETYPE whose klass_type_def isn't in either map. Most
            // are benign races (class loaded after we built the map) or
            // generics mis-tagged as plain class — the FFI fallback above
            // already handled the resolvable cases. We sample a few for
            // diagnosis and drop the rest unless full debug output is on.
            static MISSING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if MISSING.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < diag_cap() {
                let mut raw = String::new();
                for off in (0..48).step_by(8) {
                    if let Some(v) = map.read_u64(type_ptr + off) {
                        raw.push_str(&format!("+{:#x}={:#018x} ", off, v));
                    }
                }
                let klass = data64 as usize;
                let td_readable = map.in_region(klass + cfg.klass_type_def, 8);
                let td_val = map.read_u64(klass + cfg.klass_type_def).unwrap_or(0);
                let in_td = type_maps.td_map.contains_key(&(td_val as usize));
                let in_kl = type_maps.klass_map.contains_key(&klass);
                log(&format!(
                    "  MISSING tc={:#x} k={:#x} td_rdable={} td_val={:#x} in_td={} in_kl={} td_sz={} kl_sz={} tptr={:#x}: {}",
                    tc, klass, td_readable, td_val, in_td, in_kl,
                    type_maps.td_map.len(), type_maps.klass_map.len(), type_ptr, raw
                ));
            }
            return if tc == 0x11 {
                "System.ValueType".into()
            } else {
                "System.Object".into()
            };
        }
        0x1C => return "System.Object".into(),
        0x1D => return "System.Array".into(),
        _ => {}
    }
    format!("<type:{}>", tc)
}
