//! Host-process introspection: loaded module list and the agent DLL's own
//! directory. Both are used during startup — the module list feeds the
//! anti-cheat respect gate (`agent_core::respect`), and the agent directory
//! anchors `agent.log` / `internals.txt` so they don't end up in whatever
//! `cwd` the launcher chose.
//!
//! Pure read-only Win32 calls; no allocations on the hot path beyond the
//! returned `Vec`/`PathBuf`.

use std::os::raw::c_void;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE, MAX_PATH};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::ProcessStatus::{EnumProcessModules, GetModuleBaseNameW};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Snapshot every DLL currently mapped into our process and return their base
/// names (e.g. `"GameAssembly.dll"`). Order is unspecified. Capped at ~1024
/// modules; any sane game has far fewer.
pub fn enumerate_loaded_modules() -> Vec<String> {
    const CAP: usize = 1024;
    let mut handles: Vec<HMODULE> = vec![0 as HMODULE; CAP];
    let mut needed: u32 = 0;
    let proc: HANDLE = unsafe { GetCurrentProcess() };

    let buf_bytes = (handles.len() * std::mem::size_of::<HMODULE>()) as u32;
    let ok = unsafe { EnumProcessModules(proc, handles.as_mut_ptr(), buf_bytes, &mut needed) };
    if ok == 0 {
        return Vec::new();
    }

    let count = (needed as usize / std::mem::size_of::<HMODULE>()).min(handles.len());
    let mut out = Vec::with_capacity(count);
    let mut name_buf = [0u16; 260];
    for &h in &handles[..count] {
        let n = unsafe {
            GetModuleBaseNameW(proc, h, name_buf.as_mut_ptr(), name_buf.len() as u32)
        };
        if n > 0 {
            out.push(String::from_utf16_lossy(&name_buf[..n as usize]));
        }
    }
    out
}

/// Return the directory containing this DLL (i.e. where `agent.dll` was
/// loaded from), or `None` if it can't be determined. Anchoring our log/dump
/// files here means they always land next to the DLL regardless of how the
/// launcher set its working directory.
pub fn agent_dir() -> Option<PathBuf> {
    // Pass any address inside this DLL — the address of this very function —
    // and ask the loader which module it belongs to.
    let mut hmod: HMODULE = 0 as HMODULE;
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    let probe = agent_dir as *const c_void as *const u16;
    let ok = unsafe { GetModuleHandleExW(flags, probe, &mut hmod) };
    if ok == 0 || hmod == 0 as HMODULE {
        return None;
    }
    let mut buf = [0u16; MAX_PATH as usize];
    let n = unsafe { GetModuleFileNameW(hmod, buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
    path.parent().map(|p| p.to_path_buf())
}
