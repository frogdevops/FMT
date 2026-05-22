//! Minimal `version.dll` proxy for Proton/Wine.
//! UnityPlayer statically imports version.dll, so Wine loads this at game
//! startup (with WINEDLLOVERRIDES="version=n,b"). We forward the imported
//! version functions to the real system version.dll and LoadLibrary our agent.

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{BOOL, HMODULE, TRUE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA, LoadLibraryW};

const DLL_PROCESS_ATTACH: u32 = 1;

struct RealVersion {
    get_file_version_info_a: usize,
    get_file_version_info_w: usize,
    get_file_version_info_size_a: usize,
    get_file_version_info_size_w: usize,
    ver_query_value_a: usize,
    ver_query_value_w: usize,
}

static REAL: OnceLock<RealVersion> = OnceLock::new();

fn real() -> &'static RealVersion {
    REAL.get_or_init(|| unsafe {
        // Load the genuine version.dll by absolute system path so we don't
        // recursively load ourselves (we are the game-dir version.dll).
        let path: Vec<u16> = "C:\\windows\\system32\\version.dll\0".encode_utf16().collect();
        let h = LoadLibraryW(path.as_ptr());
        let get = |name: &[u8]| -> usize {
            if h.is_null() {
                return 0;
            }
            match GetProcAddress(h, name.as_ptr()) {
                Some(p) => p as usize,
                None => 0,
            }
        };
        RealVersion {
            get_file_version_info_a: get(b"GetFileVersionInfoA\0"),
            get_file_version_info_w: get(b"GetFileVersionInfoW\0"),
            get_file_version_info_size_a: get(b"GetFileVersionInfoSizeA\0"),
            get_file_version_info_size_w: get(b"GetFileVersionInfoSizeW\0"),
            ver_query_value_a: get(b"VerQueryValueA\0"),
            ver_query_value_w: get(b"VerQueryValueW\0"),
        }
    })
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoA(
    filename: *const u8,
    handle: u32,
    len: u32,
    data: *mut c_void,
) -> BOOL {
    let f: extern "system" fn(*const u8, u32, u32, *mut c_void) -> BOOL =
        std::mem::transmute(real().get_file_version_info_a);
    f(filename, handle, len, data)
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoW(
    filename: *const u16,
    handle: u32,
    len: u32,
    data: *mut c_void,
) -> BOOL {
    let f: extern "system" fn(*const u16, u32, u32, *mut c_void) -> BOOL =
        std::mem::transmute(real().get_file_version_info_w);
    f(filename, handle, len, data)
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoSizeA(filename: *const u8, handle: *mut u32) -> u32 {
    let f: extern "system" fn(*const u8, *mut u32) -> u32 =
        std::mem::transmute(real().get_file_version_info_size_a);
    f(filename, handle)
}

#[no_mangle]
pub unsafe extern "system" fn GetFileVersionInfoSizeW(filename: *const u16, handle: *mut u32) -> u32 {
    let f: extern "system" fn(*const u16, *mut u32) -> u32 =
        std::mem::transmute(real().get_file_version_info_size_w);
    f(filename, handle)
}

#[no_mangle]
pub unsafe extern "system" fn VerQueryValueA(
    block: *const c_void,
    sub_block: *const u8,
    buffer: *mut *mut c_void,
    len: *mut u32,
) -> BOOL {
    let f: extern "system" fn(*const c_void, *const u8, *mut *mut c_void, *mut u32) -> BOOL =
        std::mem::transmute(real().ver_query_value_a);
    f(block, sub_block, buffer, len)
}

#[no_mangle]
pub unsafe extern "system" fn VerQueryValueW(
    block: *const c_void,
    sub_block: *const u16,
    buffer: *mut *mut c_void,
    len: *mut u32,
) -> BOOL {
    let f: extern "system" fn(*const c_void, *const u16, *mut *mut c_void, *mut u32) -> BOOL =
        std::mem::transmute(real().ver_query_value_w);
    f(block, sub_block, buffer, len)
}

extern "system" fn loader(_p: *mut c_void) -> u32 {
    let _ = std::fs::write("version_proxy.log", "version proxy loaded; loading agent.dll\n");
    unsafe {
        let h = LoadLibraryA(b"agent.dll\0".as_ptr());
        let _ = std::fs::write(
            "version_proxy_result.log",
            if h.is_null() {
                "LoadLibrary(agent.dll) FAILED\n"
            } else {
                "agent.dll loaded\n"
            },
        );
    }
    0
}

type LpthreadStartRoutine = unsafe extern "system" fn(*mut c_void) -> u32;
extern "system" {
    fn CreateThread(
        attrs: *const c_void,
        stack: usize,
        start: Option<LpthreadStartRoutine>,
        param: *const c_void,
        flags: u32,
        id: *mut u32,
    ) -> *mut c_void;
}

#[no_mangle]
pub extern "system" fn DllMain(_m: HMODULE, reason: u32, _r: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            CreateThread(ptr::null(), 0, Some(loader), ptr::null(), 0, ptr::null_mut());
        }
    }
    TRUE
}
