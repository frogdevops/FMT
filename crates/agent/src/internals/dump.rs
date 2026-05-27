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
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_core::model::{Dump, DumpedClass};

use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::{cstr_to_string, Il2CppApi};
use crate::external::scan::MetadataResult;
use crate::paths::log;
use crate::external::region_map::{RegionMap, Tunables};
use crate::internals::resolve::{il2cpp_type_name, GenericCtx, TypeMaps};

/// Cached offset of `klass->generic_class`, probed at runtime from the
/// Il2CppGenericClass::cached_class back-reference.  Once calibrated this
/// is used instead of cfg.klass_generic_class.
static PROBED_GC_OFF: AtomicUsize = AtomicUsize::new(0);

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

/// Render a single field line. `type_name` may be empty for un-resolvable
/// metadata-only fields — we print `<?>` so the user sees the hole instead of
/// silently dropping the field.
fn field_line(name: &str, type_name: &str, offset: u32, token: u32) -> String {
    if type_name.is_empty() {
        format!("    {}: <?> // Offset: {:#x}, Token: {:#x}", name, offset, token)
    } else {
        format!("    {}: {} // Offset: {:#x}, Token: {:#x}", name, type_name, offset, token)
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
fn calibrate_generic_class_offset(
    table_base: usize,
    table_count: usize,
    map: &RegionMap,
    api: &Il2CppApi,
    cfg: &Il2CppConfig,
) -> usize {
    for i in 0..table_count.min(10) {
        let addr = table_base.wrapping_add(i * cfg.class_table_step);
        let Some(cls_raw) = map.read_u64(addr) else { continue };
        let cls = cls_raw as usize;
        if cls == 0 { continue; }
        let n = unsafe { cstr_to_string((api.class_get_name)(cls as *mut c_void)) };
        if n.is_empty() { continue; }
        let mut out = String::new();
        for off in (0x00..=0x88).step_by(8) {
            let val = map.read_u64(cls + off).unwrap_or(0) as usize;
            if val == 0 || val == cls { continue; }
            let v0 = map.read_u64(val).unwrap_or(0);
            let v8 = map.read_u64(val + 0x8).unwrap_or(0);
            let v16 = map.read_u64(val + 0x10).unwrap_or(0);
            let v24 = map.read_u64(val + 0x18).unwrap_or(0);
            let v32 = map.read_u64(val + 0x20).unwrap_or(0);
            out.push_str(&format!(" +{:02x}:{:#x}[{:#x},{:#x},{:#x},{:#x},{:#x}]",
                off, val, v0, v8, v16, v24, v32));
        }
        if !out.is_empty() {
            log(&format!("  CALIB cls={:#x} {} {}", cls, n, out));
        }
    }

    // Generic context not needed — VAR/MVAR resolution works without it.
    cfg.klass_generic_class
}

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
    // Calibrate the klass→generic_class offset at runtime so we don't rely
    // on version-guessed values (which fail for obfuscated builds).
    calibrate_generic_class_offset(table_base, table_count, map, api, cfg);

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
        let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
        let cns = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
        let key = (cns.clone(), cname.clone());

        let rt_fields = collect_runtime_fields(cls, api, cfg, map, type_maps);
        runtime_field_count += rt_fields.len();
        seen_in_runtime.insert(key.clone());

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
                let rt_lookup: HashMap<&str, (String, u32, u32)> = rt_fields
                    .iter()
                    .map(|(n, t, o, tk)| (n.as_str(), (t.clone(), *o, *tk)))
                    .collect();
                let mut fields: Vec<String> = Vec::new();
                for mf in &meta_class.fields {
                    let (tn, off, tk) = rt_lookup
                        .get(mf.name.as_str())
                        .map(|(t, o, tk)| {
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
                                .unwrap_or_else(|| t.clone());
                            (resolved_type, *o, *tk)
                        })
                        .unwrap_or_else(|| ("<?>".to_string(), 0, 0));
                    fields.push(field_line(&mf.name, &tn, off, tk));
                }
                all_lines.extend(format_class(meta_class, &fields));
                continue;
            }
        }

        // No metadata match — emit whatever the runtime walk found.
        if !rt_fields.is_empty() {
            let mut fields: Vec<String> = Vec::new();
            for (fn_, ft, off, tk) in &rt_fields {
                fields.push(field_line(fn_, ft, *off, *tk));
            }
            let full = if cns.is_empty() {
                cname
            } else {
                format!("{}::{}", cns, cname)
            };
            all_lines.push(format!("{} ({} fields):", full, fields.len()));
            all_lines.extend(fields);
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
                        field_line(&f.name, &tn, 0, 0)
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
    // Use probed offset if available (runtime calibration), fall back to config.
    let gc_off = {
        let p = PROBED_GC_OFF.load(Ordering::Relaxed);
        if p != 0 { p } else { cfg.klass_generic_class }
    };
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

fn collect_runtime_fields(
    cls: *mut c_void,
    api: &Il2CppApi,
    cfg: &Il2CppConfig,
    map: &RegionMap,
    type_maps: &TypeMaps,
) -> Vec<(String, String, u32, u32)> {
    const MAX_FIELDS_PER_CLASS: usize = 256;
    let ctx = read_generic_context(cls as usize, map, cfg);
    let mut rt_fields: Vec<(String, String, u32, u32)> = Vec::new();
    if let Some(get_fields) = api.class_get_fields {
        // FFI iterator (preferred — uses the runtime's own enumeration).
        let mut iter: *mut c_void = std::ptr::null_mut();
        loop {
            let f = unsafe { get_fields(cls, &mut iter) };
            if f.is_null() {
                break;
            }
            if rt_fields.len() >= 30 {
                break;
            }
            let fname = unsafe { cstr_to_string((api.field_get_name)(f)) };
            let ftype_ptr = unsafe { (api.field_get_type)(f) };
            let ftype = if !ftype_ptr.is_null() {
                il2cpp_type_name(map, ftype_ptr as usize, type_maps, cfg, api, ctx.as_ref())
            } else {
                "?".to_string()
            };
            let offset = map.read_u32(f as usize + 24).unwrap_or(0);
            let token = map.read_u32(f as usize + 28).unwrap_or(0);
            rt_fields.push((fname, ftype, offset, token));
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
                let fname = match map.read_name(name_ptr) {
                    Some(n) => n,
                    None => break,
                };
                if fname.is_empty() {
                    break;
                }
                let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
                let ftype = if type_ptr != 0 {
                    il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref())
                } else {
                    "?".to_string()
                };
                let offset = map.read_u32(f + 24).unwrap_or(0);
                let token = map.read_u32(f + 28).unwrap_or(0);
                rt_fields.push((fname, ftype, offset, token));
            }
        }
    }
    rt_fields
}
