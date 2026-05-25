use std::collections::{HashMap, HashSet};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;
use agent_core::logfile::{append_log, write_text};
use agent_core::model::{Dump, DumpedClass};

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::mem_scan::{find_class_table, find_types_array, scan_process_for_metadata, RegionMap, Tunables};
use crate::il2cpp_ffi::{Il2CppApi, cstr_to_string};
use crate::il2cpp_config::Il2CppConfig;
use crate::host;
use agent_core::respect::{should_decline, DeclineReason};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Per-run cap on how many noisy diagnostic lines we emit for unresolved
/// CLASS/VALUETYPE references and the GENERICINST struct-shape probe. These
/// are informational — the runtime path falls back gracefully — so we keep a
/// few samples for triage and drop the rest. Override at runtime by setting
/// the `FROG_DEBUG=1` environment variable.
const DIAG_SAMPLE_CAP: u32 = 5;

fn diag_cap() -> u32 {
    if std::env::var("FROG_DEBUG").map(|v| v != "0" && !v.is_empty()).unwrap_or(false) {
        u32::MAX
    } else {
        DIAG_SAMPLE_CAP
    }
}

type LpthreadStartRoutine = unsafe extern "system" fn(*mut c_void) -> u32;

extern "system" {
    fn CreateThread(
        lp_thread_attributes: *const c_void,
        dw_stack_size: usize,
        lp_start_address: Option<LpthreadStartRoutine>,
        lp_parameter: *const c_void,
        dw_creation_flags: u32,
        lp_thread_id: *mut u32,
    ) -> HANDLE;
}

/// Resolve a path next to the agent DLL itself, falling back to the launcher's
/// working directory if the loader can't locate our module (very rare). This
/// keeps `agent.log` and `internals.txt` together no matter where the game was
/// launched from — essential when the IDE plugin tails them remotely.
fn output_path(filename: &str) -> PathBuf {
    host::agent_dir()
        .map(|d| d.join(filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

fn log_path() -> PathBuf { output_path("agent.log") }
fn dump_path() -> PathBuf { output_path("internals.txt") }

fn log(line: &str) {
    let _ = append_log(&log_path(), line);
}

/// Two lookup strategies:
/// - `td_map`: keyed by `klass + klass_type_def` value (packed typeDefIndex + flags)
/// - `klass_map`: keyed by klass address (direct pointer)
/// The klass_map is a fallback for when the td_map key doesn't match (packed flags differ).
struct TypeMaps {
    td_map: HashMap<usize, (usize, String, String)>,
    klass_map: HashMap<usize, (String, String)>,
}

fn build_type_maps(
    table_base: usize, table_count: usize,
    api: &Il2CppApi, map: &RegionMap, cfg: &Il2CppConfig,
) -> TypeMaps {
    let mut td_map: HashMap<usize, (usize, String, String)> = HashMap::new();
    let mut klass_map: HashMap<usize, (String, String)> = HashMap::new();
    let max = table_count.min(Tunables::load().table_max_slots);
    let mut c_slot = 0usize; let mut c_nonzero = 0usize;
    let mut c_td_ok = 0usize; let mut c_td_fail = 0usize;
    let mut c_ns_ok = 0usize;
    for i in 0..max {
        let a = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(slot) = map.read_u64(a) {
            c_slot += 1;
            let k = slot as usize;
            if k == 0 { continue; }
            c_nonzero += 1;
            let _ns_ptr = map.read_u64(k + cfg.klass_namespace);
            if _ns_ptr.is_some() { c_ns_ok += 1; }
            match map.read_u64(k + cfg.klass_type_def) {
                Some(td) => {
                    c_td_ok += 1;
                    if td == 0 { continue; }
                    let td = td as usize;
                    let cn = unsafe { cstr_to_string((api.class_get_name)(k as *mut c_void)) };
                    if cn.is_empty() { continue; }
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut c_void)) };
                    if !td_map.contains_key(&td) {
                        td_map.insert(td, (k, cn.clone(), ns.clone()));
                    }
                    klass_map.insert(k, (cn, ns));
                }
                None => { c_td_fail += 1; }
            }
        }
    }
    log(&format!("  type maps: td={} klass={} (slots={}, ptrs={}, ns_ok={}, td_ok={}, td_fail={})",
        td_map.len(), klass_map.len(), c_slot, c_nonzero, c_ns_ok, c_td_ok, c_td_fail));
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
/// All high‑volume arms are sampled at most `diag_cap()` times (normally 5)
/// to keep log noise manageable.  Set `FROG_DEBUG=1` for full output.
fn il2cpp_type_name(
    map: &RegionMap, type_ptr: usize,
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
                        let elem_name = il2cpp_type_name(map, elem_type_addr as usize, type_maps, cfg, api);
                        return format!("{}[]", elem_name);
                    }
                }
            }
            return "System.Array".into();
        }
        0x15 => {
            // GENERICINST — data64 points to Il2CppGenericClass. Log a handful of
            // raw struct dumps so we can fingerprint the layout offline; the rest
            // are silenced unless `FROG_DEBUG=1`.
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
                        return if cns.is_empty() { cn.clone() } else { format!("{}::{}", cns, cn) };
                    }
                }
                if let Some((cn, cns)) = type_maps.klass_map.get(&klass_ptr) {
                    return if cns.is_empty() { cn.clone() } else { format!("{}::{}", cns, cn) };
                }
                // Ultimate fallback: query class_get_name directly via FFI for dynamic types
                let cn = unsafe { cstr_to_string((api.class_get_name)(klass_ptr as *mut c_void)) };
                if !cn.is_empty() {
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(klass_ptr as *mut c_void)) };
                    return if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
                }
            }
            // CLASS/VALUETYPE whose klass_type_def isn't in either map. Most are
            // benign races (class loaded after we built the map) or generics
            // mis-tagged as plain class — the FFI fallback above already handled
            // the resolvable cases. We sample a few for diagnosis and drop the
            // rest unless the user asked for full debug output.
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
                log(&format!("  MISSING tc={:#x} k={:#x} td_rdable={} td_val={:#x} in_td={} in_kl={} td_sz={} kl_sz={} tptr={:#x}: {}",
                    tc, klass, td_readable, td_val, in_td, in_kl, type_maps.td_map.len(), type_maps.klass_map.len(), type_ptr, raw));
            }
            return if tc == 0x11 { "System.ValueType".into() } else { "System.Object".into() };
        }
        0x1C => return "System.Object".into(),
        0x1D => return "System.Array".into(),
        _ => {}
    }
    format!("<type:{}>", tc)
}

fn format_class(c: &DumpedClass, fields: &[String]) -> Vec<String> {
    let full = if c.namespace.is_empty() { c.name.clone() } else { format!("{}::{}", c.namespace, c.name) };
    let mut out = vec![format!("{} ({} fields):", full, fields.len())];
    out.extend_from_slice(fields);
    out
}

fn field_line(name: &str, type_name: &str) -> String {
    if type_name.is_empty() {
        format!("    {}: <?>", name)
    } else {
        format!("    {}: {}", name, type_name)
    }
}

fn build_metadata_index(meta: &Dump) -> HashMap<(String, String), usize> {
    let mut idx = HashMap::new();
    for (i, c) in meta.classes.iter().enumerate() {
        idx.insert((c.namespace.clone(), c.name.clone()), i);
    }
    idx
}

extern "system" fn worker(_param: *mut c_void) -> u32 {
    let _ = write_text(&log_path(), "");
    log("agent loaded");

    // Anti-cheat respect gate — enumerate loaded modules and bail out cleanly
    // if any known anti-tamper system is present. We never try to bypass these;
    // running our scanners under EAC/BattlEye/Vanguard would (a) get the user
    // banned and (b) is explicitly out of scope for this project.
    let modules = host::enumerate_loaded_modules();
    if let Some(DeclineReason::AntiCheat(name)) = should_decline(&modules) {
        log(&format!("declining: anti-cheat present ({}); not engaging", name));
        log("agent terminated: respect gate");
        return 0;
    }

    log("=== RAPID CLASS DUMP ===");

    // Phase 0: find decrypted global-metadata in memory
    log("  scanning memory for global-metadata...");
    let metadata_result = scan_process_for_metadata();
    let meta_index = metadata_result.as_ref().map(|mr| {
        let idx = build_metadata_index(&mr.dump);
        log(&format!("  metadata: {} classes, blob @ {:#x} v{}", mr.dump.classes.len(), mr.blob_addr, mr.version));
        idx
    });

    let table = (0..30).find_map(|_| {
        let t = find_class_table();
        if t.is_none() { std::thread::sleep(Duration::from_millis(500)); }
        t
    });
    let (table_base, table_count) = match table {
        Some(t) => { log(&format!("  table @ {:#x}, {} slots", t.0, t.1)); t }
        None => {
            log("  FAILED to locate class table");
            log("agent terminated: no class table");
            return 0;
        }
    };
    let api = match unsafe { Il2CppApi::resolve() } {
        Some(a) => a,
        None => {
            log("  FAILED to resolve il2cpp API (neither standard exports nor signature scan succeeded)");
            log("agent terminated: no il2cpp api");
            return 0;
        }
    };
    let cfg = metadata_result.as_ref()
        .and_then(|mr| Il2CppConfig::for_metadata_version(mr.version))
        .unwrap_or_else(Il2CppConfig::default);
    let ver_str = metadata_result.as_ref().map_or("unknown".into(), |mr| mr.version.to_string());
    log(&format!("  config: metadata v{}, klass_namespace={:#x}, klass_type_def={:#x}",
        ver_str, cfg.klass_namespace, cfg.klass_type_def));
    let mut map = RegionMap::capture(8192); // capture before wait (unused, but ensures regions are mapped)
    log("  waiting 8s for classes to load...");
    std::thread::sleep(Duration::from_secs(8));
    map = RegionMap::capture(8192);
    let type_maps = build_type_maps(table_base, table_count, &api, &map, &cfg);

    // Phase 0b: find Il2CppMetadataRegistration.types array for typeIndex resolution
    // (requires `map` which is only available after the 8s settle wait)
    let types_array = metadata_result.as_ref().and_then(|mr| {
        log(&format!("  metadata: {} type definitions", mr.type_count));
        let arr = find_types_array(mr.type_count, &map);
        if let Some(a) = arr {
            log(&format!("  types array @ {:#x}", a));
        } else {
            log("  types array: not found");
        }
        arr
    });

    // Build metadata reverse map: type_byval_type_index → (namespace, name)
    // Used as fallback when the types array can't be found in memory.
    let type_index_to_name: HashMap<u32, (String, String)> = metadata_result.as_ref().map_or_else(HashMap::new, |mr| {
        let mut m = HashMap::new();
        for c in &mr.dump.classes {
            let ti = c.type_index;
            if ti != 0 && !m.contains_key(&ti) {
                m.insert(ti, (c.namespace.clone(), c.name.clone()));
            }
        }
        log(&format!("  type_index→name map: {} entries", m.len()));
        m
    });

    let mut all_lines: Vec<String> = Vec::new();
    let mut seen_in_runtime: HashSet<(String, String)> = HashSet::new();
    let mut runtime_field_count = 0usize;

    for i in 0..table_count.min(Tunables::load().table_max_slots) {
        let a = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(slot) = map.read_u64(a) {
            let cls = slot as *mut std::ffi::c_void;
            if cls.is_null() { continue; }
            let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
            let cns = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
            let key = (cns.clone(), cname.clone());

            // FFI field enumeration. When `class_get_fields` couldn't be
            // fingerprinted in this build (obfuscated, no clean prologue), we
            // skip the per-class field walk and fall back to memory-walking
            // klass->fields directly below. Class names still come through.
            let mut rt_fields: Vec<(String, String)> = Vec::new();
            if let Some(get_fields) = api.class_get_fields {
                let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
                loop {
                    let f = unsafe { get_fields(cls, &mut iter) };
                    if f.is_null() { break; }
                    if rt_fields.len() >= 30 { break; }
                    let fname = unsafe { cstr_to_string((api.field_get_name)(f)) };
                    let ftype_ptr = unsafe { (api.field_get_type)(f) };
                    let ftype = if !ftype_ptr.is_null() {
                        il2cpp_type_name(&map, ftype_ptr as usize, &type_maps, &cfg, &api)
                    } else { "?".to_string() };
                    rt_fields.push((fname, ftype));
                    runtime_field_count += 1;
                }
            } else {
                // Memory-walk fallback: klass->fields pointer at klass+0x80
                // (stable across Unity 2017–2022). Each FieldInfo slot is 32
                // bytes: { name*(8), type*(8), parent*(8), offset(4), token(4) }.
                // Instead of reading field_count from a version-dependent struct
                // offset, we scan the array until we hit a null name pointer
                // (end sentinel) or the cap — this works for ANY Unity version.
                // All reads go through `map` (bounds-checked) to avoid crashes.
                let klass = cls as usize;
                let fields_ptr = map.read_u64(klass + 0x80).unwrap_or(0) as usize;
                if fields_ptr != 0 {
                    for fi in 0..256 {
                        let f = fields_ptr + fi * 32;
                        let name_ptr = map.read_u64(f).unwrap_or(0) as usize;
                        if name_ptr == 0 { break; }
                        let fname = match map.read_name(name_ptr) {
                            Some(n) => n,
                            None => break,  // unmapped or non-ASCII → end of array
                        };
                        if fname.is_empty() { break; }
                        let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
                        let ftype = if type_ptr != 0 {
                            il2cpp_type_name(&map, type_ptr, &type_maps, &cfg, &api)
                        } else { "?".to_string() };
                        rt_fields.push((fname, ftype));
                        runtime_field_count += 1;
                    }
                }
            }

            seen_in_runtime.insert(key.clone());

            // Try type_index resolution as fallback when name matching fails
            let type_from_idx = |type_idx: u32| -> String {
                if let Some(ta) = types_array {
                    let ptr_addr = ta + (type_idx as usize) * 8;
                    if let Some(type_ptr) = map.read_u64(ptr_addr) {
                        let tn = il2cpp_type_name(&map, type_ptr as usize, &type_maps, &cfg, &api);
                        if !tn.is_empty() && tn != "?" {
                            return tn;
                        }
                    }
                }
                // Fallback: reverse map (type_byval_type_index → type def name)
                if let Some((ns, cn)) = type_index_to_name.get(&type_idx) {
                    return if ns.is_empty() { cn.clone() } else { format!("{}::{}", ns, cn) };
                }
                String::new()
            };

            if let Some(ref idx) = meta_index {
                if let Some(&ci) = idx.get(&key) {
                    let dump = &metadata_result.as_ref().unwrap().dump;
                    let meta_class = &dump.classes[ci];
                    let mut fields: Vec<String> = Vec::new();
                    let rt_lookup: HashMap<&str, &str> = rt_fields.iter()
                        .map(|(n, t)| (n.as_str(), t.as_str())).collect();
                    let is_missing = |s: &str| s == "System.ValueType" || s == "System.Object" || s == "?" || s.is_empty();
                    for mf in &meta_class.fields {
                        let tn = mf.type_index.and_then(|ti| {
                                let r = type_from_idx(ti);
                                if r.is_empty() { None } else { Some(r) }
                            })
                            .or_else(|| {
                                let rt = rt_lookup.get(mf.name.as_str())?;
                                if is_missing(rt) { None } else { Some((*rt).to_string()) }
                            })
                            .unwrap_or_else(|| "<?>".to_string());
                        fields.push(field_line(&mf.name, &tn));
                    }
                    all_lines.extend(format_class(meta_class, &fields));
                    continue;
                }
            }

            if !rt_fields.is_empty() {
                let mut fields: Vec<String> = Vec::new();
                for (fn_, ft) in &rt_fields {
                    fields.push(field_line(fn_, ft));
                }
                let full = if cns.is_empty() { cname } else { format!("{}::{}", cns, cname) };
                all_lines.push(format!("{} ({} fields):", full, fields.len()));
                all_lines.extend(fields);
            }
        }
    }

    if let Some(ref mr) = metadata_result {
        let mut meta_only = 0usize;
        for c in &mr.dump.classes {
            let key = (c.namespace.clone(), c.name.clone());
            if !seen_in_runtime.contains(&key) && !c.fields.is_empty() {
                let fields: Vec<String> = c.fields.iter().map(|f| {
                    let tn = f.type_index.and_then(|ti| {
                        if let Some(ta) = types_array {
                            let ptr_addr = ta + (ti as usize) * 8;
                            if let Some(type_ptr) = map.read_u64(ptr_addr) {
                                let r = il2cpp_type_name(&map, type_ptr as usize, &type_maps, &cfg, &api);
                                if !r.is_empty() && r != "?" { return Some(r); }
                            }
                        }
                        // Fallback: reverse map
                        type_index_to_name.get(&ti).map(|(ns, cn)| {
                            if ns.is_empty() { cn.clone() } else { format!("{}::{}", ns, cn) }
                        })
                    }).unwrap_or_else(|| "<?>".to_string());
                    field_line(&f.name, &tn)
                }).collect();
                all_lines.extend(format_class(c, &fields));
                meta_only += 1;
            }
        }
        log(&format!("  metadata-only (not loaded yet): {} classes", meta_only));
    }

    let report = all_lines.join("\n");
    let summary = format!("dumped {} classes, {} fields (runtime)\n",
        all_lines.iter().filter(|l| l.contains(" fields)")).count(),
        runtime_field_count);
    let _ = write_text(&dump_path(), &format!("{}{}", summary, report));
    log(&summary.trim());
    log("  wrote internals.txt");
    log("=== end RAPID CLASS DUMP ===");
    log("agent terminated: ok");
    0
}

#[no_mangle]
pub extern "system" fn DllMain(_module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            CreateThread(ptr::null(), 0, Some(worker), ptr::null(), 0, ptr::null_mut());
        }
    }
    TRUE
}
