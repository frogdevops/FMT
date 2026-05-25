use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;
use agent_core::logfile::{append_log, write_text};

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::mem_scan::find_class_table;

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
    let api = match unsafe { crate::il2cpp_ffi::Il2CppApi::resolve_obfuscated_api() } {
        Some(a) => a,
        None => { log("  FAILED to resolve obfuscated API"); return 0; }
    };
    let map = crate::mem_scan::RegionMap::capture(8192);
    let mut all_classes: Vec<String> = Vec::new();
    let mut field_count = 0usize;
    for i in 0..table_count.min(15000) {
        let a = table_base.wrapping_add(i * 8);
        if let Some(slot) = map.read_u64(a) {
            let cls = slot as *mut std::ffi::c_void;
            if cls.is_null() { continue; }
            let cname = unsafe { crate::il2cpp_ffi::cstr_to_string((api.class_get_name)(cls)) };
            let cns = unsafe { crate::il2cpp_ffi::cstr_to_string((api.class_get_namespace)(cls)) };
            let full = if cns.is_empty() { cname } else { format!("{}::{}", cns, cname) };
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut fields: Vec<String> = Vec::new();
            loop {
                let f = unsafe { (api.class_get_fields)(cls, &mut iter) };
                if f.is_null() { break; }
                if fields.len() >= 30 { break; }
                let fname = unsafe { crate::il2cpp_ffi::cstr_to_string((api.field_get_name)(f)) };
                let ftype_ptr = unsafe { (api.field_get_type)(f) };
                let ftype = if !ftype_ptr.is_null() {
                    match map.read_u64(ftype_ptr as usize).and_then(|k| map.class_fields(k as usize)) {
                        Some((tn, _)) => tn,
                        None => "?".to_string(),
                    }
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
