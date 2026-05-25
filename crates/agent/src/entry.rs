use std::collections::{HashMap, HashSet};
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;
use agent_core::logfile::{append_log, write_text};
use agent_core::model::{Dump, DumpedClass};

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::mem_scan::{find_class_table, find_types_array, scan_process_for_metadata, RegionMap};
use crate::il2cpp_ffi::{Il2CppApi, cstr_to_string};
use crate::il2cpp_config::Il2CppConfig;

const DLL_PROCESS_ATTACH: u32 = 1;

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

fn log_path() -> PathBuf {
    PathBuf::from("agent.log")
}

fn dump_path() -> PathBuf {
    PathBuf::from("internals.txt")
}

fn log(line: &str) {
    let _ = append_log(&log_path(), line);
}

fn build_type_map(
    table_base: usize, table_count: usize,
    api: &Il2CppApi, map: &RegionMap, cfg: &Il2CppConfig,
) -> HashMap<usize, (usize, String, String)> {
    let mut tm: HashMap<usize, (usize, String, String)> = HashMap::new();
    let max = table_count.min(15000);
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
                    if tm.contains_key(&td) { continue; }
                    let cn = unsafe { cstr_to_string((api.class_get_name)(k as *mut c_void)) };
                    if cn.is_empty() { continue; }
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut c_void)) };
                    tm.insert(td, (k, cn, ns));
                }
                None => { c_td_fail += 1; }
            }
        }
    }
    log(&format!("  type map: {} built (slots={}, ptrs={}, ns_ok={}, td_ok={}, td_fail={})",
        tm.len(), c_slot, c_nonzero, c_ns_ok, c_td_ok, c_td_fail));
    tm
}

fn il2cpp_type_name(
    map: &RegionMap, type_ptr: usize,
    type_map: &HashMap<usize, (usize, String, String)>,
    cfg: &Il2CppConfig,
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
            // ARRAY (multi-dim or bounded) — data64 is pointer to Il2CppArrayType.
            // First 8 bytes of Il2CppArrayType = pointer to element Il2CppType.
            let arr_struct = data64 as usize;
            if arr_struct != 0 {
                if let Some(elem_type_addr) = map.read_u64(arr_struct) {
                    if elem_type_addr != 0 {
                        let elem_name = il2cpp_type_name(map, elem_type_addr as usize, type_map, cfg);
                        return format!("{}[]", elem_name);
                    }
                }
            }
            return "System.Array".into();
        }
        0x15 => {
            // GENERICINST — data64 points to Il2CppGenericClass.
            // Log first few raw dumps to figure out the layout.
            static GEN_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if GEN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
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
                // data64 is the Il2CppClass pointer; type_map is keyed by klass->klass_type_def
                if let Some(td_raw) = map.read_u64(klass_ptr + cfg.klass_type_def) {
                    let td = td_raw as usize;
                    if let Some((_, cn, cns)) = type_map.get(&td) {
                        return if cns.is_empty() { cn.clone() } else { format!("{}::{}", cns, cn) };
                    }
                }
            }
            // Diagnostic: log first few unresolved CLASS/VALUETYPE
            static MISSING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if MISSING.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                let mut raw = String::new();
                for off in (0..48).step_by(8) {
                    if let Some(v) = map.read_u64(type_ptr + off) {
                        raw.push_str(&format!("+{:#x}={:#018x} ", off, v));
                    }
                }
                let klass = data64 as usize;
                let td_readable = map.in_region(klass + cfg.klass_type_def, 8);
                let td_val = map.read_u64(klass + cfg.klass_type_def).unwrap_or(0);
                let in_map = type_map.contains_key(&(td_val as usize));
                let map_len = type_map.len();
                log(&format!("  MISSING tc={:#x} k={:#x} td_rdable={} td_val={:#x} in_map={} map_sz={} tptr={:#x}: {}",
                    tc, klass, td_readable, td_val, in_map, map_len, type_ptr, raw));
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
        None => { log("  FAILED to locate class table"); return 0; }
    };
    let api = match unsafe { Il2CppApi::resolve_obfuscated_api() } {
        Some(a) => a,
        None => { log("  FAILED to resolve obfuscated API"); return 0; }
    };
    let cfg = Il2CppConfig::default();
    let mut map = RegionMap::capture(8192);
    log("  waiting 8s for classes to load...");
    std::thread::sleep(Duration::from_secs(8));
    map = RegionMap::capture(8192);
    let type_map = build_type_map(table_base, table_count, &api, &map, &cfg);

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

    let mut all_lines: Vec<String> = Vec::new();
    let mut seen_in_runtime: HashSet<(String, String)> = HashSet::new();
    let mut runtime_field_count = 0usize;

    for i in 0..table_count.min(15000) {
        let a = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(slot) = map.read_u64(a) {
            let cls = slot as *mut std::ffi::c_void;
            if cls.is_null() { continue; }
            let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
            let cns = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
            let key = (cns.clone(), cname.clone());

            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut rt_fields: Vec<(String, String)> = Vec::new();
            loop {
                let f = unsafe { (api.class_get_fields)(cls, &mut iter) };
                if f.is_null() { break; }
                if rt_fields.len() >= 30 { break; }
                let fname = unsafe { cstr_to_string((api.field_get_name)(f)) };
                let ftype_ptr = unsafe { (api.field_get_type)(f) };
                let ftype = if !ftype_ptr.is_null() {
                    il2cpp_type_name(&map, ftype_ptr as usize, &type_map, &cfg)
                } else { "?".to_string() };
                rt_fields.push((fname, ftype));
                runtime_field_count += 1;
            }

            seen_in_runtime.insert(key.clone());

            // Try type_index resolution as fallback when name matching fails
            let type_from_idx = |type_idx: u32| -> String {
                if let Some(ta) = types_array {
                    let ptr_addr = ta + (type_idx as usize) * 8;
                    if let Some(type_ptr) = map.read_u64(ptr_addr) {
                        let tn = il2cpp_type_name(&map, type_ptr as usize, &type_map, &cfg);
                        if !tn.is_empty() && tn != "?" {
                            return tn;
                        }
                    }
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
                    for mf in &meta_class.fields {
                        let tn = rt_lookup.get(mf.name.as_str()).map(|s| s.to_string())
                            .or_else(|| mf.type_index.and_then(|ti| {
                                let r = type_from_idx(ti);
                                if r.is_empty() { None } else { Some(r) }
                            }))
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
                                let r = il2cpp_type_name(&map, type_ptr as usize, &type_map, &cfg);
                                if !r.is_empty() && r != "?" { return Some(r); }
                            }
                        }
                        None
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
