use std::collections::HashMap;
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;
use agent_core::logfile::{append_log, write_text};

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::mem_scan::{find_class_table, RegionMap};
use crate::il2cpp_ffi::{Il2CppApi, cstr_to_string};

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

/// Build a reverse map: type_def_ptr → (klass_ptr, name, namespace)
/// klass+0x20 points to the type definition struct (which is what Il2CppType.data.klass references).
fn build_type_map(table_base: usize, table_count: usize, api: &Il2CppApi, map: &RegionMap) -> HashMap<usize, (usize, String, String)> {
    let mut tm: HashMap<usize, (usize, String, String)> = HashMap::new();
    let max = table_count.min(15000);
    let mut c_slot = 0usize; let mut c_nonzero = 0usize;
    let mut c_20read = 0usize; let mut c_20fail = 0usize;
    let mut c_18ok = 0usize;
    for i in 0..max {
        let a = table_base.wrapping_add(i * 8);
        if let Some(slot) = map.read_u64(a) {
            c_slot += 1;
            let k = slot as usize;
            if k == 0 { continue; }
            c_nonzero += 1;
            // Offset +0x18 should always be readable (class_get_namespace reads it)
            let _ns_ptr = map.read_u64(k + 0x18);
            if _ns_ptr.is_some() { c_18ok += 1; }
            match map.read_u64(k + 0x20) {
                Some(td) => {
                    c_20read += 1;
                    if td == 0 { continue; }
                    let td = td as usize;
                    if tm.contains_key(&td) { continue; }
                    let cn = unsafe { cstr_to_string((api.class_get_name)(k as *mut c_void)) };
                    if cn.is_empty() { continue; }
                    let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut c_void)) };
                    tm.insert(td, (k, cn, ns));
                }
                None => { c_20fail += 1; }
            }
        }
    }
    log(&format!("  type map: {} built (slots={}, ptrs={}, +0x18_ok={}, +0x20_ok={}, +0x20_fail={})",
        tm.len(), c_slot, c_nonzero, c_18ok, c_20read, c_20fail));
    tm
}

fn il2cpp_type_name(map: &RegionMap, type_ptr: usize, type_map: &HashMap<usize, (usize, String, String)>) -> String {
    // Il2CppType layout (verified by diagnostic dump):
    //   +0x00: data union (8 bytes) — type def ptr or type enum
    //   +0x08: 2 bytes — name_idx / mods
    //   +0x0A: 1 byte — Il2CppTypeEnum discriminator
    //   +0x0B: 1 byte — padding
    //   +0x0C: 4 bytes — attrs
    let data64 = match map.read_u64(type_ptr) {
        Some(v) => v,
        None => return "?".into(),
    };
    let discrim = map.read_u64(type_ptr + 0x08).unwrap_or(0);
    let tc = ((discrim >> 16) & 0xFF) as u8;
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
                        let elem_name = il2cpp_type_name(map, elem_type_addr as usize, type_map);
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
            let td = data64 as usize;
            if td != 0 {
                if let Some((_, cn, cns)) = type_map.get(&td) {
                    return if cns.is_empty() { cn.clone() } else { format!("{}::{}", cns, cn) };
                }
            }
            // Diagnostic: log first few unresolved valuetypes to see what data.klass is
            static MISSING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if MISSING.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                let mut raw = String::new();
                for off in (0..48).step_by(8) {
                    if let Some(v) = map.read_u64(type_ptr + off) {
                        raw.push_str(&format!("+{:#x}={:#018x} ", off, v));
                    }
                }
                log(&format!("  MISSING type tc={:#x} data64={:#x} tptr={:#x}: {}", tc, data64, type_ptr, raw));
            }
            return if tc == 0x11 { "System.ValueType".into() } else { "System.Object".into() };
        }
        0x1C => return "System.Object".into(),
        0x1D => return "System.Array".into(),
        _ => {}
    }
    format!("<type:{}>", tc)
}

extern "system" fn worker(_param: *mut c_void) -> u32 {
    let _ = write_text(&log_path(), "");
    log("agent loaded");
    log("=== RAPID CLASS DUMP ===");
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
    let mut map = RegionMap::capture(8192);
    // Wait 8 seconds for classes to populate (from old polling: stable ~7s)
    log("  waiting 8s for classes to load...");
    std::thread::sleep(Duration::from_secs(8));
    // Re-capture RegionMap after new memory regions may have been allocated
    map = RegionMap::capture(8192);
    let type_map = build_type_map(table_base, table_count, &api, &map);
    let mut all_classes: Vec<String> = Vec::new();
    let mut field_count = 0usize;
    for i in 0..table_count.min(15000) {
        let a = table_base.wrapping_add(i * 8);
        if let Some(slot) = map.read_u64(a) {
            let cls = slot as *mut std::ffi::c_void;
            if cls.is_null() { continue; }
            let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
            let cns = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
            let full = if cns.is_empty() { cname } else { format!("{}::{}", cns, cname) };
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut fields: Vec<String> = Vec::new();
            loop {
                let f = unsafe { (api.class_get_fields)(cls, &mut iter) };
                if f.is_null() { break; }
                if fields.len() >= 30 { break; }
                let fname = unsafe { cstr_to_string((api.field_get_name)(f)) };
                let ftype_ptr = unsafe { (api.field_get_type)(f) };
                let ftype = if !ftype_ptr.is_null() {
                    il2cpp_type_name(&map, ftype_ptr as usize, &type_map)
                } else { "?".to_string() };
                fields.push(format!("    {}: {}", fname, ftype));
                field_count += 1;
            }
            if !fields.is_empty() {
                all_classes.push(format!("{} ({} fields):", full, fields.len()));
                for ff in &fields { all_classes.push(ff.clone()); }
            }
        }
    }
    let report = all_classes.join("\n");
    let summary = format!("dumped {} classes, {} fields\n", all_classes.iter().filter(|l| l.contains(" fields)")).count(), field_count);
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
