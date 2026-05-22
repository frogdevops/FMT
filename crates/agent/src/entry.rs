use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;

use agent_core::dump::build_dump;
use agent_core::format::format_dump;
use agent_core::logfile::{append_log, write_text};
use agent_core::respect::should_decline;

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::il2cpp_ffi::Il2CppApi;
use crate::real_runtime::RealRuntime;
use crate::win::loaded_module_names;

const DLL_PROCESS_ATTACH: u32 = 1;

// `windows-sys` only exposes `CreateThread` behind the `Win32_Security`
// feature (its first parameter is `*const SECURITY_ATTRIBUTES`), which this
// crate does not enable. We pass a null attributes pointer, so we declare the
// import locally with the pointer typed as an opaque `c_void` — matching the
// kernel32 ABI without pulling in the extra feature.
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

    let modules = loaded_module_names();
    if let Some(reason) = should_decline(&modules) {
        log(&format!("declined: protection detected ({:?})", reason));
        return 0;
    }
    log("respect gate passed");

    let api = unsafe {
        let mut attempts = 0;
        loop {
            if let Some(api) = Il2CppApi::resolve_from_game_assembly() {
                let domain = (api.domain_get)();
                if !domain.is_null() {
                    break api;
                }
            }
            attempts += 1;
            if attempts > 600 {
                log("gave up waiting for il2cpp runtime (60s)");
                return 0;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    log("il2cpp runtime ready");

    let runtime = RealRuntime::new(api);
    unsafe { runtime.attach_thread() };

    let dump = build_dump(&runtime);
    log(&format!(
        "read {} classes, {} fields",
        dump.class_count(),
        dump.total_fields()
    ));

    let text = format_dump(&dump);
    match write_text(&dump_path(), &text) {
        Ok(()) => log("wrote internals.txt"),
        Err(e) => log(&format!("failed to write internals.txt: {}", e)),
    }

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
