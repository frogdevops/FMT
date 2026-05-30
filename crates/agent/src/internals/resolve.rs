//! Mapping `Il2CppType*` pointers to human-readable names.
//!
//! The runtime never stores type names in plain text on the type itself; you
//! have to walk through one of:
//!   * the primitives table (compiled in below),
//!   * the string-heap-base derived from the class table (primary path),
//!   * `class_get_name` via FFI as a last resort, or
//!   * the generic context (type args from `Il2CppGenericInst`) to resolve
//!     VAR/MVAR generic parameters to their instantiated types.
//!
//! Everything is read-only and bounds-checked through `RegionMap`.

use std::collections::HashMap;
use std::os::raw::c_void;

use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::{cstr_to_string, Il2CppApi};
use crate::paths::log;
use crate::external::region_map::{RegionMap, Tunables};

/// Generic context for resolving VAR/MVAR type parameters.
///
/// Carries the concrete type arguments for a generic class instantiation
/// (the `Il2CppGenericInst.argv` array). When the class has no generic
/// context (is not a generic instantiation), this is empty and VAR/MVAR
/// fall back to printing `!<index>`.
pub struct GenericCtx {
    /// Pointers to `Il2CppType*` for each concrete type argument.
    pub args: Vec<usize>,
}

pub struct TypeMaps {
    pub string_heap_base: Option<usize>,
}

pub fn build_type_maps(
    table_base: usize,
    table_count: usize,
    api: &Il2CppApi,
    map: &RegionMap,
    cfg: &Il2CppConfig,
) -> TypeMaps {
    let mut base_votes: HashMap<usize, usize> = HashMap::new();
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
                    // Guard: skip slots that don't look like a klass struct (image-backptr → *.dll).
                    if map.class_fields(k).is_none() {
                        continue;
                    }
                    // Guard: skip zero/garbage slots that have no readable name.
                    if unsafe { cstr_to_string((api.class_get_name)(k as *mut c_void)) }.is_empty() {
                        continue;
                    }
                    // Derive string_heap_base = name_ptr - nameIndex and vote.
                    // name ptr is at klass+0x10 (stable il2cpp class name offset); td is
                    // byval_arg.data (a typedef ptr); nameIndex is the typedef's first u32.
                    // Consensus across classes gives the real per-launch runtime base.
                    if let (Some(np), Some(name_idx)) = (map.read_u64(k + 0x10), map.read_u32(td)) {
                        let np = np as usize;
                        let name_idx = name_idx as usize;
                        if np > name_idx {
                            *base_votes.entry(np - name_idx).or_insert(0) += 1;
                        }
                    }
                }
                None => {
                    c_td_fail += 1;
                }
            }
        }
    }
    log(&format!(
        "  class table: slots={}, ptrs={}, ns_ok={}, td_ok={}, td_fail={}",
        c_slot, c_nonzero, c_ns_ok, c_td_ok, c_td_fail
    ));
    let string_heap_base = base_votes
        .iter()
        .max_by_key(|(_, &c)| c)
        .filter(|(_, &c)| c >= 8)
        .map(|(&b, _)| b);
    log(&format!(
        "  string_heap_base = {:?} (top candidate votes; {} distinct candidates)",
        string_heap_base.map(|b| format!("{:#x}", b)),
        base_votes.len()
    ));
    TypeMaps { string_heap_base }
}

/// Resolve a type name from a typedef pointer using the derived string-heap base.
/// `typedef_ptr` is `Il2CppType.data` for a CLASS/VALUETYPE — a pointer into the
/// Il2CppTypeDefinition table. nameIndex@+0, namespaceIndex@+4 (u32 each) index
/// the string heap. Returns "Namespace::Name" (or "Name" when namespace empty),
/// or None if anything is unreadable / the name is empty.
fn typedef_name(map: &RegionMap, base: usize, typedef_ptr: usize) -> Option<String> {
    let name_idx = map.read_u32(typedef_ptr)? as usize;
    let name = map.read_name(base.checked_add(name_idx)?)?;
    if name.is_empty() {
        return None;
    }
    let ns = map
        .read_u32(typedef_ptr + 4)
        .and_then(|ni| map.read_name(base.checked_add(ni as usize)?))
        .unwrap_or_default();
    Some(if ns.is_empty() { name } else { format!("{}::{}", ns, name) })
}

/// Strip the il2cpp generic-arity suffix (a backtick followed by digits) from a
/// type's leaf name, e.g. "...Dictionary`2" -> "...Dictionary".
fn strip_arity(name: &str) -> String {
    match name.rfind('`') {
        Some(i) if name[i + 1..].chars().all(|c| c.is_ascii_digit()) && i + 1 < name.len() => {
            name[..i].to_string()
        }
        _ => name.to_string(),
    }
}

/// Resolve an `Il2CppType*` to a human-readable type name.
///
/// ## Resolution chain (tc = IL2CPP_TYPE_* discriminator)
/// 1. **Primitives** (0x01–0x10, 0x18–0x19): hardcoded lookup.
/// 2. **VAR** (0x13) / **MVAR** (0x1E): read `Il2CppGenericParameter.index` from
///    `data64` pointer → show `!0`, `!1`, … .  When `ctx` provides concrete type
///    args, resolve to the instantiated type.
/// 3. **SZARRAY** (0x1D): element type from `data64` pointer.
/// 4. **ARRAY** (0x14): multi-dim array struct at `data64`.
/// 5. **GENERICINST** (0x15): generic class definition + type args from
///    `Il2CppGenericClass` / `Il2CppGenericInst`.
/// 6. **CLASS** (0x12) / **VALUETYPE** (0x11): string-heap → FFI fallback.
/// 7. **Object** (0x1C): hardcoded `System.Object`.
/// 8. **Unknown** → `<type:{tc}>`.
pub fn il2cpp_type_name(
    map: &RegionMap,
    type_ptr: usize,
    type_maps: &TypeMaps,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
    ctx: Option<&GenericCtx>,
) -> String {
    il2cpp_type_name_depth(map, type_ptr, type_maps, cfg, api, ctx, 0)
}

fn resolve_var_param(
    map: &RegionMap,
    param_ptr: usize,
    ctx: Option<&GenericCtx>,
    type_maps: &TypeMaps,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
    depth: u8,
) -> String {
    // Index location varies per obfuscation:
    //   Standard layout: u32 at +0x00 is GenericParameterIndex.
    //   Obfuscated layout: index lives in the upper u16 of the word
    //   at +0x08 (i.e. u16 at +0x0A).  Try both.
    let idx = {
        let direct = map.read_u32(param_ptr).unwrap_or(u32::MAX);
        if direct <= 65535 {
            direct
        } else if let Some(hi) = map.read_u16(param_ptr.wrapping_add(10)) {
            hi as u32
        } else {
            return "!?".into();
        }
    };
    if let Some(ctx) = ctx {
        if let Some(&arg_tp) = ctx.args.get(idx as usize) {
            let concrete = il2cpp_type_name_depth(
                map, arg_tp, type_maps, cfg, api, Some(ctx), depth + 1,
            );
            if !concrete.is_empty() && concrete != "?" {
                return concrete;
            }
        }
    }
    format!("!{}", idx)
}

fn il2cpp_type_name_depth(
    map: &RegionMap,
    type_ptr: usize,
    type_maps: &TypeMaps,
    cfg: &Il2CppConfig,
    api: &Il2CppApi,
    ctx: Option<&GenericCtx>,
    depth: u8,
) -> String {
    if depth > 8 {
        return "?".into();
    }
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
        0x13 | 0x1E => {
            // VAR (0x13) / MVAR (0x1E) — generic/method type parameter.
            return resolve_var_param(map, data64 as usize, ctx, type_maps, cfg, api, depth);
        }
        0x14 => {
            let arr_struct = data64 as usize;
            if arr_struct != 0 {
                if let Some(elem_type_addr) = map.read_u64(arr_struct) {
                    if elem_type_addr != 0 {
                        let elem_name = il2cpp_type_name_depth(
                            map,
                            elem_type_addr as usize,
                            type_maps,
                            cfg,
                            api,
                            ctx,
                            depth + 1,
                        );
                        return format!("{}[]", elem_name);
                    }
                }
            }
            return "System.Array".into();
        }
        0x1D => {
            // SZARRAY — single-dim zero-based array. data64 points to element type.
            let elem_tp = data64 as usize;
            if elem_tp != 0 {
                let elem_name = il2cpp_type_name_depth(
                    map, elem_tp, type_maps, cfg, api, ctx, depth + 1,
                );
                return format!("{}[]", elem_name);
            }
            return "System.Array".into();
        }
        0x15 => {
            let gc = data64 as usize;
            // Generic definition: gc+0x0 = Il2CppType* of the open generic type.
            let def = map
                .read_u64(gc)
                .map(|tp| il2cpp_type_name_depth(map, tp as usize, type_maps, cfg, api, ctx, depth + 1))
                .unwrap_or_default();
            if def.is_empty() || def == "?" {
                return "System.Generic".into();
            }
            let base_name = strip_arity(&def);
            // Type args: gc+0x8 = Il2CppGenericInst* { argc@+0x0, argv@+0x8 (Il2CppType**) }.
            let class_inst = map.read_u64(gc + 0x8).unwrap_or(0) as usize;
            let argc = map.read_u32(class_inst).unwrap_or(0) as usize;
            let argv = map.read_u64(class_inst + 0x8).unwrap_or(0) as usize;
            let mut args = Vec::new();
            for i in 0..argc.min(16) {
                if let Some(arg_tp) = map.read_u64(argv + i * 8) {
                    args.push(il2cpp_type_name_depth(
                        map, arg_tp as usize, type_maps, cfg, api, ctx, depth + 1,
                    ));
                }
            }
            if args.is_empty() {
                return base_name;
            }
            return format!("{}<{}>", base_name, args.join(", "));
        }
        0x11 | 0x12 => {
            let p = data64 as usize;
            if p != 0 {
                // PRIMARY: data64 is a typedef ptr → resolve via string heap.
                if let Some(base) = type_maps.string_heap_base {
                    if let Some(name) = typedef_name(map, base, p) {
                        return name;
                    }
                }
                // SECONDARY: rare builds store a runtime klass ptr here.
                let cn = unsafe { cstr_to_string((api.class_get_name)(p as *mut c_void)) };
                if !cn.is_empty() {
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(p as *mut c_void)) };
                    return if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
                }
            }
            return if tc == 0x11 {
                "<unresolved-struct>".into()
            } else {
                "<unresolved-class>".into()
            };
        }
        0x1F => return "System.TypedReference".into(),
        0x1C => return "System.Object".into(),
        0x20 | 0x21 => {
            // CMOD_REQD / CMOD_OPT — wrap an inner Il2CppType. data64 → inner type ptr.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<cmod-unresolved>".into();
        }
        0x40 => {
            // MODIFIER — wrap an inner type.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<modifier-unresolved>".into();
        }
        0x41 => {
            // SENTINEL — varargs marker; inner type follows.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<sentinel-unresolved>".into();
        }
        0x45 => {
            // PINNED — pinned modifier.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<pinned-unresolved>".into();
        }
        _ => {}
    }
    if tc <= 0x45 {
        format!("<unhandled-tc:0x{:02x}>", tc)
    } else {
        format!("<garbage-tc:0x{:02x} @ {:#x}>", tc, type_ptr)
    }
}
