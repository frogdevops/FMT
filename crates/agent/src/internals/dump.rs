//! Build the human-readable `internals.txt` from a located class table and
//! (optionally) the parsed global-metadata blob.
//!
//! Two inputs feed the dump:
//!  1. The **live class table** — walked slot-by-slot. For each loaded
//!     `Il2CppClass*` we ask the FFI for its name/namespace, then enumerate
//!     fields either via `class_get_fields` (preferred) or by walking
//!     `klass->fields` directly when the FFI accessor is unavailable.
//!  2. The **metadata blob** (optional) — provides class shape (field names,
//!     `type_index`) for classes that may not be loaded yet, plus accurate
//!     field/method counts. We merge these in alongside the runtime walk.
//!
//! Output format (matches what the IDE plugin will eventually parse):
//! ```text
//! Namespace::ClassName (<N> fields):
//!     fieldName: TypeName // Offset: 0x..., Token: 0x...
//! ```

use std::collections::{HashMap, HashSet};
use std::os::raw::c_void;

use agent_core::model::{Dump, DumpedClass};

use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::{cstr_to_string, Il2CppApi};
use crate::external::scan::MetadataResult;
use crate::paths::log;
use crate::external::region_map::{RegionMap, Tunables};
use crate::internals::resolve::{il2cpp_type_name, GenericCtx, TypeMaps};

// Generic type-argument resolution lives entirely in `resolve.rs`:
//   * GENERICINST fields (e.g. `List<System.Int32>`) resolve via the
//     `Il2CppGenericClass*` read from the *type pointer's own data field*
//     (resolve.rs `il2cpp_type_name_depth`, type-code 0x15) — no klass
//     offset is consulted.
//   * VAR/MVAR params (e.g. `T` inside `List<T>`) resolve via the enclosing
//     class's `GenericCtx`, produced by `read_generic_context` below, which
//     reads `klass + cfg.klass_generic_class` (a version-aware offset, default
//     0x48). resolve.rs `resolve_var_param` substitutes the concrete arg.
// There was a `PROBED_GC_OFF` static + `calibrate_generic_class_offset`
// here that was meant to runtime-probe this offset for obfuscated builds, but
// it never stored a result (it only logged raw memory layout) and its return
// was discarded — so the probed branch was permanently dead and VAR/MVAR
// already ran on `cfg.klass_generic_class`. Removed (Task 21): deletion is
// behavior-identical (same offset used) and a half-wired probe storing an
// unvalidated offset would have been *less* safe than the config fallback.

/// Render one class block: header line + indented field lines.
fn format_class(c: &DumpedClass, fields: &[String]) -> Vec<String> {
    let full = if c.namespace.is_empty() {
        c.name.clone()
    } else {
        format!("{}::{}", c.namespace, c.name)
    };
    let mut out = vec![format!("{} ({} fields):", full, fields.len())];
    out.extend_from_slice(fields);
    out
}

/// Maximum FieldInfo slots the per-class field walk inspects. When a walk hits
/// this cap, the dump emits an honesty signal (some fields may be missing).
const MAX_FIELDS_PER_CLASS: usize = 256;

/// Render a single field line. `type_name` may be empty for un-resolvable
/// metadata-only fields — we print `<?>` so the user sees the hole instead of
/// silently dropping the field.
fn field_line(name: &str, type_name: &str, offset: u32, token: u32, is_static: bool) -> String {
    // il2cpp uses 0xffffffff as the "field exists in metadata but runtime
    // offset not computed" sentinel (e.g. thread_local_static_fields_index).
    // Display as META so modders see intent rather than what looks like garbage.
    let offset_str = if offset == 0xffffffff {
        "META".to_string()
    } else {
        format!("{:#x}", offset)
    };
    // `static ` prefix iff FIELD_ATTRIBUTE_STATIC; driven by the same `chunk & 0x10`
    // bit the rest of the codebase uses (fields_at / static_field).
    let stat = if is_static { "static " } else { "" };
    if type_name.is_empty() {
        format!("    {}{}: <?> // Offset: {}, Token: {:#x}", stat, name, offset_str, token)
    } else {
        format!("    {}{}: {} // Offset: {}, Token: {:#x}", stat, name, type_name, offset_str, token)
    }
}

/// Build `(namespace, name) → metadata-class-index` for fast lookup during the
/// runtime walk's merge step.
fn build_metadata_index(meta: &Dump) -> HashMap<(String, String), usize> {
    let mut idx = HashMap::new();
    for (i, c) in meta.classes.iter().enumerate() {
        idx.insert((c.namespace.clone(), c.name.clone()), i);
    }
    idx
}

/// Build `type_index → (namespace, name)` from a metadata `Dump`. Used as the
/// last-resort fallback when the runtime types array can't be located in
/// memory but we still need to resolve a field's `type_index`.
fn build_type_index_reverse(meta: &Dump) -> HashMap<u32, (String, String)> {
    let mut m = HashMap::new();
    for c in &meta.classes {
        let ti = c.type_index;
        if ti != 0 && !m.contains_key(&ti) {
            m.insert(ti, (c.namespace.clone(), c.name.clone()));
        }
    }
    m
}

/// Walk the live class table and produce the formatted dump lines plus a
/// runtime-field count for the summary header. `metadata_result` is optional —
/// when present, we merge its field/method shape in; when absent, we emit only
/// what the runtime walk found.
pub fn build_internals_lines(
    table_base: usize,
    table_count: usize,
    api: &Il2CppApi,
    cfg: &Il2CppConfig,
    map: &RegionMap,
    type_maps: &TypeMaps,
    metadata_result: Option<&MetadataResult>,
    types_array: Option<usize>,
) -> (Vec<String>, usize) {
    let meta_index = metadata_result.map(|mr| build_metadata_index(&mr.dump));

    let type_index_to_name: HashMap<u32, (String, String)> = metadata_result
        .map(|mr| {
            let m = build_type_index_reverse(&mr.dump);
            log(&format!("  type_index→name map: {} entries", m.len()));
            m
        })
        .unwrap_or_default();

    let mut all_lines: Vec<String> = Vec::new();
    let mut seen_in_runtime: HashSet<(String, String)> = HashSet::new();
    let mut runtime_field_count = 0usize;

    for i in 0..table_count.min(Tunables::load().table_max_slots) {
        let a = table_base.wrapping_add(i * cfg.class_table_step);
        let slot = match map.read_u64(a) {
            Some(s) => s,
            None => continue,
        };
        let cls = slot as *mut c_void;
        if cls.is_null() {
            continue;
        }
        let mut cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
        let     cns   = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
        if cname.is_empty() && cns.is_empty() {
            cname = format!("<generic @ {:#x}>", cls as usize);
        }
        let key = (cns.clone(), cname.clone());

        let (rt_fields, cap_hit) = collect_runtime_fields(cls, api, cfg, map, type_maps);
        runtime_field_count += rt_fields.len();
        seen_in_runtime.insert(key.clone());
        // Honesty signal: the field walk capped out, so some fields may be missing.
        let cap_note = if cap_hit {
            Some(format!(
                "    // ⚠ field walk hit MAX cap ({}); some fields may be missing",
                MAX_FIELDS_PER_CLASS
            ))
        } else {
            None
        };

        // Collect method and instance lines once — appended after fields in both paths.
        let method_lines   = collect_runtime_methods(cls as usize);
        let instance_lines = collect_runtime_instances(cls as usize);

        // Type-from-index helper used during the metadata merge.
        let type_from_idx = |type_idx: u32| -> String {
            if let Some(ta) = types_array {
                let ptr_addr = ta + (type_idx as usize) * 8;
                if let Some(type_ptr) = map.read_u64(ptr_addr) {
                    let tn = il2cpp_type_name(map, type_ptr as usize, type_maps, cfg, api, None);
                    if !tn.is_empty() && tn != "?" {
                        return tn;
                    }
                }
            }
            // Fallback: reverse map (type_byval_type_index → type def name)
            if let Some((ns, cn)) = type_index_to_name.get(&type_idx) {
                return if ns.is_empty() {
                    cn.clone()
                } else {
                    format!("{}::{}", ns, cn)
                };
            }
            String::new()
        };

        // Prefer the metadata view when we have one — it provides accurate
        // field shape (e.g. type_index for resolving generics) that the live
        // walk can miss.
        if let (Some(idx), Some(mr)) = (meta_index.as_ref(), metadata_result) {
            if let Some(&ci) = idx.get(&key) {
                let meta_class = &mr.dump.classes[ci];
                let rt_lookup: HashMap<&str, (&str, u32, u32, bool)> = rt_fields
                    .iter()
                    .map(|f| (f.name.as_str(), (f.type_name.as_str(), f.offset, f.token, f.is_static)))
                    .collect();
                let mut fields: Vec<String> = Vec::new();
                for mf in &meta_class.fields {
                    let (tn, off, tk, is_static) = rt_lookup
                        .get(mf.name.as_str())
                        .map(|(t, o, tk, st)| {
                            let resolved_type = mf
                                .type_index
                                .and_then(|ti| {
                                    let r = type_from_idx(ti);
                                    if r.is_empty() {
                                        None
                                    } else {
                                        Some(r)
                                    }
                                })
                                .unwrap_or_else(|| t.to_string());
                            (resolved_type, *o, *tk, *st)
                        })
                        .unwrap_or_else(|| ("<?>".to_string(), 0, 0, false));
                    fields.push(field_line(&mf.name, &tn, off, tk, is_static));
                }
                if let Some(note) = &cap_note {
                    fields.push(note.clone());
                }
                all_lines.extend(format_class(meta_class, &fields));
                all_lines.extend(method_lines.iter().cloned());
                all_lines.extend(instance_lines.iter().cloned());
                continue;
            }
        }

        // No metadata match — emit whatever the runtime walk found.
        if !rt_fields.is_empty() || !method_lines.is_empty() || !instance_lines.is_empty() {
            let mut fields: Vec<String> = Vec::new();
            for f in &rt_fields {
                fields.push(field_line(&f.name, &f.type_name, f.offset, f.token, f.is_static));
            }
            if let Some(note) = &cap_note {
                fields.push(note.clone());
            }
            let full = if cns.is_empty() {
                cname
            } else {
                format!("{}::{}", cns, cname)
            };
            all_lines.push(format!("{} ({} fields):", full, fields.len()));
            all_lines.extend(fields);
            all_lines.extend(method_lines);
            all_lines.extend(instance_lines);
        }
    }

    // Append metadata-only classes (not yet loaded into the runtime).
    if let Some(mr) = metadata_result {
        let mut meta_only = 0usize;
        for c in &mr.dump.classes {
            let key = (c.namespace.clone(), c.name.clone());
            if !seen_in_runtime.contains(&key) && !c.fields.is_empty() {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| {
                        let tn = f
                            .type_index
                            .and_then(|ti| {
                                if let Some(ta) = types_array {
                                    let ptr_addr = ta + (ti as usize) * 8;
                                    if let Some(type_ptr) = map.read_u64(ptr_addr) {
                                        let r = il2cpp_type_name(
                                            map,
                                            type_ptr as usize,
                                            type_maps,
                                            cfg,
                                            api,
                                            None,
                                        );
                                        if !r.is_empty() && r != "?" {
                                            return Some(r);
                                        }
                                    }
                                }
                                type_index_to_name.get(&ti).map(|(ns, cn)| {
                                    if ns.is_empty() {
                                        cn.clone()
                                    } else {
                                        format!("{}::{}", ns, cn)
                                    }
                                })
                            })
                            .unwrap_or_else(|| "<?>".to_string());
                        // metadata-only class: not loaded into the runtime, so there
                        // is no live Il2CppType* to read the static bit from. We do
                        // not fabricate a `static ` marker we can't verify.
                        field_line(&f.name, &tn, 0, 0, false)
                    })
                    .collect();
                all_lines.extend(format_class(c, &fields));
                meta_only += 1;
            }
        }
        log(&format!("  metadata-only (not loaded yet): {} classes", meta_only));
    }

    (all_lines, runtime_field_count)
}

/// Collect the live fields of one class, preferring the FFI iterator when
/// available and falling back to a memory walk of `klass->fields` otherwise.
/// Read the generic context (concrete type arguments) from a klass pointer.
/// Returns `Some(GenericCtx)` when the class is a generic instantiation with
/// resolvable type args, or `None` for non-generic classes.
fn read_generic_context(cls: usize, map: &RegionMap, cfg: &Il2CppConfig) -> Option<GenericCtx> {
    // Version-aware klass→generic_class offset (default 0x48). See the
    // generic-resolution note near the top of this file.
    let gc_off = cfg.klass_generic_class;
    // Il2CppGenericClass: type@+0x00, class_inst@+0x08, cached_class@+0x18
    let gc = map.read_u64(cls + gc_off)? as usize;
    let inst = map.read_u64(gc + 0x8)? as usize;
    let argc = map.read_u32(inst).unwrap_or(0) as usize;
    if argc == 0 { return None; }
    let argv = map.read_u64(inst + 0x8).unwrap_or(0) as usize;
    if argv == 0 { return None; }
    let mut args = Vec::with_capacity(argc);
    for i in 0..argc.min(32) {
        if let Some(ptr) = map.read_u64(argv + i * 8) {
            args.push(ptr as usize);
        }
    }
    if args.is_empty() { None } else { Some(GenericCtx { args }) }
}

/// One live field collected from a class. The `is_static` bit is derived from
/// `chunk & 0x10` read at `type_ptr + il2cpp_type_discrim_read_at` — the SAME
/// source/offset/mask that `fields_at` and `static_field` use in api.rs, so the
/// dump's `static ` marker agrees with the rest of the runtime.
struct RtField {
    name: String,
    type_name: String,
    offset: u32,
    token: u32,
    is_static: bool,
}

/// Read the FIELD_ATTRIBUTE_STATIC bit for a field whose `Il2CppType*` is
/// `type_ptr`. Mirrors api.rs `fields_at`/`static_field` exactly.
fn read_field_is_static(type_ptr: usize, cfg: &Il2CppConfig, map: &RegionMap) -> bool {
    if type_ptr == 0 {
        return false;
    }
    let chunk = map.read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    (chunk & crate::internals::api::METHOD_ATTRIBUTE_STATIC_BIT as u64) != 0
}

/// Collect a class's live fields plus a flag indicating the field-walk cap was
/// hit (so the caller can emit an honesty signal). DECISION (Task 11): this keeps
/// the dumper's OWN two-path walk — FFI `class_get_fields` first, then the
/// `klass->fields` memory-walk fallback — rather than switching to
/// `Iter<FieldInfo>`/`for_each_field`. The spine iterator is backed by
/// `fields_at`, which is memory-walk ONLY (no FFI path) and reads via the global
/// `cache`/`ctx` instead of the dumper's `RegionMap`. Switching to it would drop
/// the FFI walk and change the read backend at dump time = canary blindness.
/// This walk is the strictly-more-complete one, so it stays.
fn collect_runtime_fields(
    cls: *mut c_void,
    api: &Il2CppApi,
    cfg: &Il2CppConfig,
    map: &RegionMap,
    type_maps: &TypeMaps,
) -> (Vec<RtField>, bool) {
    let ctx = read_generic_context(cls as usize, map, cfg);
    let mut rt_fields: Vec<RtField> = Vec::new();
    let mut cap_hit = false;
    if let Some(get_fields) = api.class_get_fields {
        // FFI iterator (preferred — uses the runtime's own enumeration).
        let mut iter: *mut c_void = std::ptr::null_mut();
        loop {
            let f = unsafe { get_fields(cls, &mut iter) };
            if f.is_null() {
                break;
            }
            if rt_fields.len() >= MAX_FIELDS_PER_CLASS {
                cap_hit = true;
                break;
            }
            let fname = unsafe { cstr_to_string((api.field_get_name)(f)) };
            let ftype_ptr = unsafe { (api.field_get_type)(f) };
            let ftype = if !ftype_ptr.is_null() {
                il2cpp_type_name(map, ftype_ptr as usize, type_maps, cfg, api, ctx.as_ref())
            } else {
                "?".to_string()
            };
            let raw_offset = map.read_u32(f as usize + 24).unwrap_or(0);
            let offset = if crate::internals::api::klass_is_valuetype_via_map(cls as u64, cfg, map) {
                raw_offset.saturating_sub(0x10)
            } else {
                raw_offset
            };
            let token = map.read_u32(f as usize + 28).unwrap_or(0);
            if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
            let is_static = read_field_is_static(ftype_ptr as usize, cfg, map);
            rt_fields.push(RtField { name: fname, type_name: ftype, offset, token, is_static });
        }
    } else {
        // Memory-walk fallback.  klass->fields is at `cfg.klass_fields` offset.
        // Each FieldInfo slot is 32 bytes:
        //   { name*(8), type*(8), parent*(8), offset(4), token(4) }
        // We scan until we hit a null name pointer (end sentinel) or the cap,
        // which works for ANY Unity version. All reads go through `map`
        // (bounds-checked) to avoid crashes.
        let klass = cls as usize;
        let fields_ptr = map.read_u64(klass + cfg.klass_fields).unwrap_or(0) as usize;
        if fields_ptr != 0 {
            for fi in 0..MAX_FIELDS_PER_CLASS {
                let f = fields_ptr + fi * 32;
                let name_ptr = map.read_u64(f).unwrap_or(0) as usize;
                if name_ptr == 0 {
                    break;
                }
                // The cap truncated the walk while a real slot was still present
                // (no null sentinel reached): some fields may be missing.
                if fi == MAX_FIELDS_PER_CLASS - 1 {
                    cap_hit = true;
                }
                let fname = match map.read_name(name_ptr) {
                    Some(n) => n,
                    None => continue,
                };
                if fname.is_empty() {
                    continue;
                }
                // Read token first; bail early if scanner garbage. Mirrors api.rs ordering.
                let token = map.read_u32(f + 28).unwrap_or(0);
                if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
                let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
                // Validate type_ptr produces a plausible type code. Garbage FieldInfo
                // entries past the real array end have type_ptr pointing to random
                // memory that doesn't decode as a valid tc in 0x01..=0x45.
                if type_ptr == 0 { continue; }
                let chunk = map.read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
                let tc = ((chunk >> cfg.discrim_shift) & 0xFF) as u8;
                if tc == 0 || tc > 0x45 { continue; }
                let ftype = il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref());
                let raw_offset = map.read_u32(f + 24).unwrap_or(0);
                let offset = if crate::internals::api::klass_is_valuetype_via_map(cls as usize as u64, cfg, map) {
                    raw_offset.saturating_sub(0x10)
                } else {
                    raw_offset
                };
                // FIELD_ATTRIBUTE_STATIC (0x10) lives in the low byte of the same
                // `chunk` read above — identical source/offset/mask to fields_at.
                let is_static = (chunk & crate::internals::api::METHOD_ATTRIBUTE_STATIC_BIT as u64) != 0;
                rt_fields.push(RtField { name: fname, type_name: ftype, offset, token, is_static });
            }
        }
    }
    (rt_fields, cap_hit)
}

/// Collect formatted method lines for one class. Mirrors the read pattern of
/// `find_method` in api.rs exactly: `ctx::get()` + `cache::read_u64/read_u8/
/// read_cstr` with `cfg.method_name_off` and `cfg.method_param_count_off`.
/// Returns an empty Vec when the class has no methods or ctx is not live.
///
/// HONESTY SIGNAL: when the returned count equals `MAX_METHODS_PER_CLASS` the
/// `Iter<MethodPtr>` may have capped early — we emit a warning line so the
/// reader knows the list may be truncated.
fn collect_runtime_methods(cls: usize) -> Vec<String> {
    use agent_core::spine::KlassPtr;
    let klass = KlassPtr::from_raw(cls as u64);
    let methods = crate::internals::api::methods_of(klass);
    let count = methods.len();
    let mut lines = Vec::new();
    if count == 0 {
        return lines;
    }
    lines.push(format!("    methods ({}):", count));
    for m in &methods {
        let mi = m.as_u64() as usize;
        let (name, argc) = match crate::internals::ctx::get() {
            Some(c) => {
                let name_ptr = crate::external::cache::read_u64(mi + c.cfg.method_name_off)
                    .unwrap_or(0) as usize;
                let name = crate::external::cache::read_cstr(name_ptr)
                    .unwrap_or_else(|| "?".into());
                let argc = crate::external::cache::read_u8(mi + c.cfg.method_param_count_off)
                    .unwrap_or(0);
                (name, argc)
            }
            None => ("?".into(), 0u8),
        };
        lines.push(format!("        {}({} args)", name, argc));
    }
    // When the iterator returned exactly the cap the walk may have been cut short.
    if count == agent_core::spine::access::MAX_METHODS_PER_CLASS {
        lines.push("        // ⚠ method walk hit MAX cap; some methods may be missing".to_string());
    }
    lines
}

/// Collect formatted live-instance lines for one class. Asks `instances_of`
/// for DISPLAY_CAP+1 so we can distinguish "exactly 10" from "10 or more".
/// Returns an empty Vec when no live instances exist — the section is omitted
/// entirely so classes with zero instances don't clutter the dump.
///
/// SCAN COST NOTE: `instances_of` triggers an AOB scan on first call per klass
/// (results are cached). Over a large class table (thousands of classes) the
/// aggregate scan cost is non-trivial, but the dump runs off the hot path so
/// this is acceptable. The scan does NOT block the game thread.
fn collect_runtime_instances(cls: usize) -> Vec<String> {
    use agent_core::spine::KlassPtr;
    let klass = KlassPtr::from_raw(cls as u64);
    const DISPLAY_CAP: usize = 10;
    // Request one extra so we can tell whether there are more than DISPLAY_CAP.
    let instances = crate::internals::api::instances_of(klass, DISPLAY_CAP + 1);
    let mut lines = Vec::new();
    if instances.is_empty() {
        return lines;
    }
    let shown  = instances.len().min(DISPLAY_CAP);
    let suffix = if instances.len() > DISPLAY_CAP { " (10+ shown)" } else { "" };
    lines.push(format!("    live instances ({}){}", shown, suffix));
    for inst in instances.iter().take(DISPLAY_CAP) {
        lines.push(format!("        {:#x}", inst.as_u64()));
    }
    lines
}
