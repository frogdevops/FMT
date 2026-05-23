use std::ffi::c_void;

use agent_core::metadata::{find_and_parse, find_magic_offsets, layout_for_version};
use agent_core::model::Dump;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
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

/// Like scan_for_strings, but bounded to GameAssembly.dll's PE image range only.
/// Module-backed memory is reliably readable (unlike arbitrary heap/mapped regions
/// that can fault), and the "global-metadata.dat" literal lives in the module's
/// data. Read-only. Caps per needle.
pub fn scan_gameassembly_for_strings(needles: &[&str]) -> Vec<(String, usize)> {
    const PER_NEEDLE_CAP: usize = 16;
    let mut out = Vec::new();
    let mut counts = vec![0usize; needles.len()];
    unsafe {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module.is_null() {
            return out;
        }
        let base = module as usize;
        // Bound the scan to the module image via the PE header's SizeOfImage.
        let e_lfanew = *((base + 0x3C) as *const u32) as usize; // IMAGE_DOS_HEADER.e_lfanew
        let opt_header = base + e_lfanew + 24; // skip Signature(4) + IMAGE_FILE_HEADER(20)
        let size_of_image = *((opt_header + 0x38) as *const u32) as usize; // IMAGE_OPTIONAL_HEADER64.SizeOfImage
        let end = base.saturating_add(size_of_image);

        let mut addr = base;
        while addr < end {
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                break;
            }
            let rbase = mbi.BaseAddress as usize;
            let rsize = mbi.RegionSize;
            let next = rbase.saturating_add(rsize);

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) {
                let start = addr.max(rbase);
                let stop = next.min(end);
                if stop > start {
                    let slice = std::slice::from_raw_parts(start as *const u8, stop - start);
                    for (ni, needle) in needles.iter().enumerate() {
                        let nb = needle.as_bytes();
                        if nb.is_empty() || slice.len() < nb.len() || counts[ni] >= PER_NEEDLE_CAP {
                            continue;
                        }
                        let mut i = 0usize;
                        while i + nb.len() <= slice.len() {
                            if &slice[i..i + nb.len()] == nb {
                                out.push((needle.to_string(), start + i));
                                counts[ni] += 1;
                                if counts[ni] >= PER_NEEDLE_CAP {
                                    break;
                                }
                                i += nb.len();
                            } else {
                                i += 1;
                            }
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

/// Diagnostic: search committed/readable memory for each needle string and
/// return (needle, absolute_address) for every hit, capped. Read-only.
pub fn scan_for_strings(needles: &[&str]) -> Vec<(String, usize)> {
    const PER_NEEDLE_CAP: usize = 16;
    let mut out = Vec::new();
    let mut counts = vec![0usize; needles.len()];
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

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) && size >= 1 {
                let slice = std::slice::from_raw_parts(base as *const u8, size);
                for (ni, needle) in needles.iter().enumerate() {
                    let nb = needle.as_bytes();
                    if nb.is_empty() || slice.len() < nb.len() || counts[ni] >= PER_NEEDLE_CAP {
                        continue;
                    }
                    let mut i = 0usize;
                    while i + nb.len() <= slice.len() {
                        if &slice[i..i + nb.len()] == nb {
                            out.push((needle.to_string(), base + i));
                            counts[ni] += 1;
                            if counts[ni] >= PER_NEEDLE_CAP {
                                break;
                            }
                            i += nb.len();
                        } else {
                            i += 1;
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
