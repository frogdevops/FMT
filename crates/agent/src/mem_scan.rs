use std::ffi::c_void;

use agent_core::metadata::{find_and_parse, find_magic_offsets, layout_for_version};
use agent_core::model::Dump;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

fn is_readable(protect: u32) -> bool {
    const MASK: u32 = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    (protect & MASK) != 0 && (protect & PAGE_GUARD) == 0
}

/// Walk this process's committed, readable memory regions looking for the
/// decrypted global-metadata blob. Returns the first region that parses into a
/// non-empty `Dump`. Read-only; never calls into the game.
pub fn scan_process_for_metadata() -> Option<Dump> {
    unsafe {
        let mut addr: usize = 0;
        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let n = VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if n == 0 {
                break;
            }
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            let next = base.saturating_add(size);

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) && size >= 8 {
                let slice = std::slice::from_raw_parts(base as *const u8, size);
                if let Some(dump) = find_and_parse(slice, layout_for_version) {
                    return Some(dump);
                }
            }

            if next <= addr {
                break;
            }
            addr = next;
        }
    }
    None
}

/// Diagnostic: find every metadata-magic marker in committed/readable memory and
/// return (absolute_address, version) for each (version = the u32 at the marker's
/// byte offset +4). Capped to avoid log spam. Read-only.
pub fn scan_metadata_candidates() -> Vec<(usize, u32)> {
    const CAP: usize = 64;
    let mut out = Vec::new();
    unsafe {
        let mut addr: usize = 0;
        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let n = VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if n == 0 {
                break;
            }
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            let next = base.saturating_add(size);

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) && size >= 8 {
                let slice = std::slice::from_raw_parts(base as *const u8, size);
                for off in find_magic_offsets(slice) {
                    if off + 8 <= slice.len() {
                        let v = u32::from_le_bytes([
                            slice[off + 4],
                            slice[off + 5],
                            slice[off + 6],
                            slice[off + 7],
                        ]);
                        out.push((base + off, v));
                        if out.len() >= CAP {
                            return out;
                        }
                    }
                }
            }

            if next <= addr {
                break;
            }
            addr = next;
        }
    }
    out
}
