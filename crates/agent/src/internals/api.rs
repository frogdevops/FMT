//! The 4 proven-machinery internals ops, by name. Structural walks (klass/
//! FieldInfo) go through external's validated cache reads; instance values go
//! through external's typed read. Emits external's (offset, ValType) currency.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use agent_core::mem_value::{status, valtype_from_tc, ValType, Value};
use agent_core::spine::{KlassPtr, MethodPtr, Instance, FieldAddr, MemAddr, ReadOnly, ReadWrite, InvokeArg, InvokeError};

use crate::external::{api as ext, cache};
use crate::internals::ctx;
use crate::internals::ffi::{cstr_to_string, Il2CppClass};

/// `FIELD_ATTRIBUTE_STATIC` bit in il2cpp's field type-attribute chunk (low byte
/// of the discriminator chunk read at `il2cpp_type_discrim_read_at`). A field is
/// declared `static` iff this bit is set. (il2cpp shares the same 0x10 bit value
/// across METHOD and FIELD attribute encodings, hence the name.)
pub(crate) const METHOD_ATTRIBUTE_STATIC_BIT: u32 = 0x10;

/// Search the live class table for a class whose name (or "Namespace::Name")
/// matches `name`. Returns `Some(KlassPtr)` when found, `None` otherwise.
pub fn find_class(name: &str) -> Option<KlassPtr> {
    let c = ctx::get()?;
    for i in 0..c.table_count {
        let slot = c.table_base.wrapping_add(i * c.cfg.class_table_step);
        let klass = match cache::read_u64(slot) {
            Some(k) if k != 0 => k as usize,
            _ => continue,
        };
        if !cache::is_klass_shape(klass) { continue; }
        let cn = unsafe { cstr_to_string((c.api.class_get_name)(klass as *mut Il2CppClass)) };
        if cn.is_empty() { continue; }
        if cn == name { return Some(KlassPtr::from_raw(klass as u64)); }
        let ns = unsafe { cstr_to_string((c.api.class_get_namespace)(klass as *mut Il2CppClass)) };
        let full = if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
        if full == name { return Some(KlassPtr::from_raw(klass as u64)); }
    }
    None
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
            let raw_offset = cache::read_u32(fi as usize + 24).unwrap_or(0);
            let offset = if klass_is_valuetype(klass as u64) {
                raw_offset.saturating_sub(0x10)
            } else {
                raw_offset
            };
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
            let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => continue };
            let token = cache::read_u32(slot + 28).unwrap_or(0);
            if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
            let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
            // Validate type_ptr produces a plausible type code. Garbage FieldInfo
            // entries past the real array end have type_ptr pointing to random
            // memory that doesn't decode as a valid tc in 0x01..=0x45.
            if type_ptr == 0 { continue; }
            let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
            let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
            if tc == 0 || tc > 0x45 { continue; }
            let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
            let offset = if klass_is_valuetype(klass as u64) {
                raw_offset.saturating_sub(0x10)
            } else {
                raw_offset
            };
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

/// Field offset + external ValType + is_static for `name`, or None. The composition bridge.
pub fn field_info(klass: KlassPtr, name: &str) -> Option<(u32, ValType, bool)> {
    let c = ctx::get()?;
    let mut found = None;
    for_each_field(klass.as_u64() as usize, |fname, offset, type_ptr| {
        if fname == name {
            let vt = valtype_from_tc(type_tc(type_ptr)).unwrap_or(ValType::U64);
            // FIELD_ATTRIBUTE_STATIC (0x10) lives in the low byte of the same chunk
            // that type_tc reads — same source, same offset, matching fields_at/static_field.
            let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
            let is_static = (chunk & METHOD_ATTRIBUTE_STATIC_BIT as u64) != 0;
            found = Some((offset, vt, is_static));
            true
        } else {
            false
        }
    });
    found
}

/// Read a field by name through external's validated read. The native read.
pub fn get_field(instance: Instance, klass: KlassPtr, name: &str) -> Result<Value, i32> {
    let (offset, vt, _is_static) = field_info(klass, name).ok_or(status::ERR_BAD_TYPE)?;
    let addr_raw = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    let addr = MemAddr::<ReadOnly>::from_raw(addr_raw);
    let val = match vt {
        ValType::U8  => Value::U8 (ext::read::<u8 , _>(addr).map_err(i32::from)?),
        ValType::U16 => Value::U16(ext::read::<u16, _>(addr).map_err(i32::from)?),
        ValType::U32 => Value::U32(ext::read::<u32, _>(addr).map_err(i32::from)?),
        ValType::U64 => Value::U64(ext::read::<u64, _>(addr).map_err(i32::from)?),
        ValType::I8  => Value::I8 (ext::read::<i8 , _>(addr).map_err(i32::from)?),
        ValType::I16 => Value::I16(ext::read::<i16, _>(addr).map_err(i32::from)?),
        ValType::I32 => Value::I32(ext::read::<i32, _>(addr).map_err(i32::from)?),
        ValType::I64 => Value::I64(ext::read::<i64, _>(addr).map_err(i32::from)?),
        ValType::F32 => Value::F32(ext::read::<f32, _>(addr).map_err(i32::from)?),
        ValType::F64 => Value::F64(ext::read::<f64, _>(addr).map_err(i32::from)?),
        ValType::Bytes | ValType::Cstr => return Err(status::ERR_BAD_TYPE),
    };
    Ok(val)
}

/// The klass pointer at an object's head ("what is this object?"). Returns
/// `None` if the instance head is unreadable or zero.
pub fn klass_of(instance: Instance) -> Option<KlassPtr> {
    match cache::read_u64(instance.as_u64() as usize) {
        Some(k) if k != 0 => Some(KlassPtr::from_raw(k)),
        _ => None,
    }
}

/// Address of a static field by name. Returns `Some(MemAddr<ReadWrite>)` when
/// found AND the field is actually static, `None` otherwise. Statics are
/// writable by intent.
pub fn static_field(klass: KlassPtr, name: &str) -> Option<MemAddr<ReadWrite>> {
    let c = ctx::get()?;
    let k = klass.as_u64() as usize;
    let static_base = cache::read_u64(k + c.cfg.klass_static_fields).unwrap_or(0);
    if static_base == 0 {
        return None;
    }
    let mut addr_out: Option<MemAddr<ReadWrite>> = None;
    for_each_field(k, |fname, offset, type_ptr| {
        if fname == name {
            let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
            if chunk & METHOD_ATTRIBUTE_STATIC_BIT as u64 != 0 {
                let raw = static_base + offset as u64;
                // SAFETY: static_base lives in a writable region; the static-attr
                // bit confirms this field address is in that region.
                addr_out = Some(unsafe { MemAddr::<ReadWrite>::from_raw_writable(raw) });
            }
            true
        } else {
            false
        }
    });
    addr_out
}

/// Enumerate all methods of `klass`. Composes `Iter<MethodPtr> for KlassPtr`.
pub fn methods_of(klass: KlassPtr) -> Vec<MethodPtr> {
    use agent_core::spine::Iter;
    <KlassPtr as Iter<MethodPtr>>::iter(&klass).collect()
}

/// Enumerate live instances of `klass` via the registered scan_backend, capped
/// at `max` candidates. Each yielded Instance is structurally validated inside
/// the iterator (alignment / klass_of / klass-shape), so results are real.
pub fn instances_of(klass: KlassPtr, max: usize) -> Vec<Instance> {
    use agent_core::spine::Iter;
    <KlassPtr as Iter<Instance>>::iter(&klass).take(max).collect()
}

/// Locate a method by name + arg count → `MethodPtr`, or `None`. Walks the
/// klass's methods array; stops at the array end when an entry's klass back-
/// pointer no longer matches (no method_count needed).
pub fn find_method(klass: KlassPtr, name: &str, argc: u32) -> Option<MethodPtr> {
    let c = ctx::get()?;
    let k = klass.as_u64() as usize;
    let methods = cache::read_u64(k + c.cfg.klass_methods).unwrap_or(0) as usize;
    if methods == 0 {
        return None;
    }
    for i in 0..4096usize {
        let mi = match cache::read_u64(methods + i * 8) {
            Some(v) if v != 0 => v as usize,
            _ => break,
        };
        // Array-end / validity: the MethodInfo's declaring-klass must be this klass.
        if cache::read_u64(mi + c.cfg.method_klass_off).unwrap_or(0) != klass.as_u64() {
            break;
        }
        let name_ptr = cache::read_u64(mi + c.cfg.method_name_off).unwrap_or(0) as usize;
        let mname = match cache::read_cstr(name_ptr) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let pcount = cache::read_u8(mi + c.cfg.method_param_count_off).unwrap_or(0) as u32;
        if mname == name && pcount == argc {
            return Some(MethodPtr::from_raw(mi as u64));
        }
    }
    None
}

/// True if the klass is a value type. Reads `Il2CppClass.byval_arg`'s valuetype
/// bit via the probed offset+mask. Falls back to false on unreadable klass.
pub fn klass_is_valuetype(klass: u64) -> bool {
    let c = match ctx::get() { Some(c) => c, None => return false };
    let byte = cache::read_u8(klass as usize + c.cfg.klass_valuetype_off).unwrap_or(0);
    byte & c.cfg.klass_valuetype_bit != 0
}

/// Map-backed variant of `klass_is_valuetype` for dump-time use, when
/// `ctx::init` has not yet been called and the cache refresher hasn't started.
/// Reads via the captured `RegionMap` snapshot using the explicitly-passed cfg.
pub fn klass_is_valuetype_via_map(
    klass: u64,
    cfg: &crate::internals::config::Il2CppConfig,
    map: &crate::external::region_map::RegionMap,
) -> bool {
    let byte = map.read_u8(klass as usize + cfg.klass_valuetype_off).unwrap_or(0);
    byte & cfg.klass_valuetype_bit != 0
}

/// Typed address of an instance field: `instance + field_info(klass, name).offset`.
/// Returns a `FieldAddr` carrying both the writable raw address and the
/// decoded `ValType` so downstream `Write<T>` calls can enforce type-match.
/// Returns `None` if the field is not found on the class.
///
/// Load-bearing typed-API surface (no caller today; future composers consume it).
#[allow(dead_code)]
pub fn field_addr(
    klass: KlassPtr,
    name: &str,
    instance: Instance,
) -> Option<FieldAddr> {
    let (offset, vt, _is_static) = field_info(klass, name)?;
    let addr_raw = (instance.as_u64() as usize).wrapping_add(offset as usize) as u64;
    // SAFETY: caller obtained `instance` via the spine API; instance fields
    // are writable by their semantic role.
    let addr = unsafe { MemAddr::from_raw_writable(addr_raw) };
    Some(FieldAddr::new(addr, vt))
}

/// Typed sibling: invoke a managed method with the spine vocabulary.
pub fn invoke_method(
    method: MethodPtr,
    instance: Option<Instance>,
    args: &[InvokeArg],
) -> Result<InvokeArg, InvokeError> {
    crate::internals::marshal::invoke_method(method, instance, args)
}

// ── Metadata-backend vtable shims for `Iter<FieldInfo> / Iter<MethodPtr>` ────
//
// These wire the agent-side il2cpp walk (config offsets, tc decoding, klass
// back-pointer sentinel) into `agent_core::spine::metadata_backend` so the
// trait `Iter` impls on `KlassPtr` (defined in agent-core, where the orphan
// rule forces them) can call back into agent-side memory primitives.
//
// Contract: each call MUST advance internally past any garbage entries and
// return the next REAL record at-or-after `cursor`. The iterator on the
// agent-core side bumps cursor by one after each Some return — duplicate
// records or out-of-order returns will mis-iterate. End-of-array is signalled
// by `None`.

/// `metadata_backend::FieldsFn` impl. Mirrors `for_each_field`'s validation
/// logic exactly: token != 0, type_ptr decodes to a tc in 1..=0x45,
/// value-type offset adjustment. Skips garbage internally and returns the
/// physical slot just past the yielded record in `next_cursor` so the
/// agent-core iterator resumes from the right spot on its next call.
fn fields_at(klass: usize, cursor: usize) -> Option<agent_core::spine::metadata_backend::FieldInfoRaw> {
    let c = ctx::get()?;
    let fields_ptr = match cache::read_u64(klass + c.cfg.klass_fields) {
        Some(p) if p != 0 => p as usize,
        _ => return None,
    };
    let is_vt = klass_is_valuetype(klass as u64);
    let mut fi = cursor;
    while fi < 256 {
        let slot = fields_ptr + fi * 32;
        let name_ptr = match cache::read_u64(slot) {
            Some(p) if p != 0 => p as usize,
            _ => return None, // real end-of-array sentinel
        };
        let this_slot = fi;
        fi += 1;
        // Skip garbage that the legacy walk filters (without ending iteration).
        if cache::read_cstr(name_ptr).map_or(true, |n| n.is_empty()) {
            continue;
        }
        let token = cache::read_u32(slot + 28).unwrap_or(0);
        if token == 0 {
            continue; // scanner garbage: real fields always have a metadata token
        }
        let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
        if type_ptr == 0 {
            continue;
        }
        let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
        let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
        if tc == 0 || tc > 0x45 {
            continue;
        }
        let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
        let offset = if is_vt {
            raw_offset.saturating_sub(0x10)
        } else {
            raw_offset
        };
        let vt = valtype_from_tc(tc).unwrap_or(ValType::U64);
        // FIELD_ATTRIBUTE_STATIC (0x10) lives in the low byte of the SAME chunk
        // that `static_field` masks (api.rs:150). Identical source/offset/mask.
        let is_static = (chunk & METHOD_ATTRIBUTE_STATIC_BIT as u64) != 0;
        return Some(agent_core::spine::metadata_backend::FieldInfoRaw {
            name_ptr,
            offset,
            val_type: vt,
            token,
            is_static,
            type_ptr,
            next_cursor: this_slot + 1,
        });
    }
    None
}

/// `metadata_backend::MethodsFn` impl. Mirrors `find_method`'s end-of-array
/// detection: the klass back-pointer must match. The methods array is dense
/// (no garbage to skip), so cursor maps 1-to-1 to array index.
fn methods_at(klass: usize, cursor: usize) -> Option<u64> {
    let c = ctx::get()?;
    let methods = cache::read_u64(klass + c.cfg.klass_methods).unwrap_or(0) as usize;
    if methods == 0 {
        return None;
    }
    let mi = match cache::read_u64(methods + cursor * 8) {
        Some(v) if v != 0 => v as usize,
        _ => return None,
    };
    // Array-end / validity: the MethodInfo's declaring-klass must be this klass.
    if cache::read_u64(mi + c.cfg.method_klass_off).unwrap_or(0) != klass as u64 {
        return None;
    }
    Some(mi as u64)
}

/// Register the metadata-walk shims with agent_core's metadata_backend vtable.
/// Call once at agent start, after `ctx::init` (the shims read `ctx::get()`).
pub fn register_metadata_backend() {
    agent_core::spine::metadata_backend::register(fields_at, methods_at);
}

// ── scan_backend implementations for Iter<Instance> ─────────────────────────
//
// next_match: AOB-scan for the target klass's pointer signature, cache hits,
//             stream them on subsequent calls.
// validate:   universal structural checks (alignment, klass_of, klass-shape).
//             No per-klass branching.

/// Upper bound on AOB-scan hits per klass. Caps scan cost under adversarial
/// heap conditions; live-instance counts for a single class are realistically
/// in the tens-to-hundreds, so 10k is a generous ceiling.
const SCAN_MAX_INSTANCES: usize = 10_000;

/// Per-klass cache of AOB-scan hit lists. Populated on the first `next_match`
/// for a klass; later calls stream from the cached Vec by cursor index. No
/// eviction — instance discovery is one-shot per iterator construction.
///
/// NOTE: on a cache MISS the lock is held for the full duration of `ext::scan`
/// (a whole-process memory walk, potentially tens of ms). Steady-state calls
/// are cache hits and cheap; first-access for a given klass serializes any
/// concurrent iterators of that klass behind the scan. Acceptable because
/// instance enumeration is rare and one-shot, not a hot path.
static SCAN_CACHE: OnceLock<Mutex<HashMap<usize, Vec<usize>>>> = OnceLock::new();

/// `NextMatchFn`: AOB-scan once per klass for the klass pointer (as an 8-byte
/// little-endian signature), cache the hit list, then stream it on subsequent
/// calls. `cursor` is the opaque backend cursor — an index into the cached hit
/// list — and is advanced on every `Some(_)` return (the `InstanceIter`
/// liveness guard terminates the walk if we ever fail to advance it).
fn scan_next_match(target_klass: usize, cursor: &mut usize) -> Option<usize> {
    let mut cache = SCAN_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("SCAN_CACHE mutex poisoned");
    let hits = cache.entry(target_klass).or_insert_with(|| {
        // Byte signature: the klass pointer as little-endian u64. `scan` returns
        // a (possibly empty) Vec<usize>; an empty result is cached, so a klass
        // with zero live instances fails closed rather than re-scanning forever.
        let pattern = (target_klass as u64).to_le_bytes();
        ext::scan(&pattern, SCAN_MAX_INSTANCES)
    });
    if *cursor >= hits.len() {
        return None;
    }
    let v = hits[*cursor];
    *cursor += 1; // MUST advance on every Some(_) — InstanceIter liveness guard depends on it
    Some(v)
}

/// `ValidateFn`: universal structural validation of a scan candidate. No
/// per-klass branching — every check is the same for all klasses. Fails closed
/// (returns `false`) on any unreadable memory or shape mismatch so a coincidental
/// scan hit can never surface as a bogus instance.
fn scan_validate(addr: usize, target_klass: usize) -> bool {
    // Check 1: pointer-size alignment (x86_64 = 8).
    if addr & 7 != 0 {
        return false;
    }
    // Check 2: klass_of(addr) is the klass pointer at offset 0; must match target.
    //          `cache::read_u64` enforces region readability before reading.
    let read_klass = match cache::read_u64(addr) {
        Some(k) if k != 0 => k as usize,
        _ => return false,
    };
    if read_klass != target_klass {
        return false;
    }
    // Check 3: the klass at addr+0 must itself look like a real Il2CppClass
    //          (valid image back-pointer → name cstr ending in ".dll").
    if !cache::is_klass_shape(read_klass) {
        return false;
    }
    true
}

/// Register the scan-based instance-discovery backend. Call AFTER
/// `register_mem_backend()` and `register_metadata_backend()` at agent start.
pub fn register_scan_backend() {
    agent_core::spine::scan_backend::register(scan_next_match, scan_validate);
}
