use std::collections::HashSet;
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

/// Read-only scan for loaded il2cpp classes. Builds a map of committed, readable
/// regions, then treats every 8-byte slot as a candidate `Il2CppClass*` and keeps
/// the ones that are class-shaped: an `image` back-pointer at +0x00 pointing to a
/// real image, a readable ASCII name at +0x10, and a readable ASCII namespace at
/// +0x18 (offsets derived from this build's own getter bytecode, not hardcoded).
/// Results are deduped by class pointer, so we capture every loaded class no
/// matter how many places reference it. NEVER dereferences an address that isn't
/// proven inside a committed, readable region. Never calls into the game.
pub fn scan_for_classes(cap: usize) -> Vec<(String, String)> {
    const MAX_REGIONS: usize = 8192;

    // --- Step 1: build a sorted map of committed, readable regions. ---
    let mut regions: Vec<(usize, usize)> = Vec::new();
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
                regions.push((base, next));
                if regions.len() >= MAX_REGIONS {
                    break;
                }
            }

            if next <= addr {
                break;
            }
            addr = next;
        }
    }
    regions.sort_by_key(|r| r.0);

    // --- Step 2: read helpers gated by `in_region`. Plain fns taking &regions
    // so they only ever borrow the region map immutably. ---

    /// True iff [addr, addr+len) fits entirely within one sorted region.
    fn in_region(regions: &[(usize, usize)], addr: usize, len: usize) -> bool {
        let end = match addr.checked_add(len) {
            Some(e) => e,
            None => return false, // overflow -> not safe to read
        };
        // Find the last region whose start <= addr.
        let idx = match regions.binary_search_by(|r| r.0.cmp(&addr)) {
            Ok(i) => i,
            Err(0) => return false, // no region starts at/below addr
            Err(i) => i - 1,
        };
        let (start, region_end) = regions[idx];
        addr >= start && end <= region_end
    }

    /// Read a u64 at `addr` only if the 8 bytes are inside a known region.
    fn read_u64(regions: &[(usize, usize)], addr: usize) -> Option<u64> {
        if in_region(regions, addr, 8) {
            Some(unsafe { *(addr as *const u64) })
        } else {
            None
        }
    }

    /// Read a NUL-terminated, printable-ASCII string of up to 63 chars at `addr`.
    /// Requires 64 bytes be readable. Empty string is allowed. Returns None if no
    /// NUL within 64 bytes or any byte is non-printable.
    fn read_name(regions: &[(usize, usize)], addr: usize) -> Option<String> {
        if !in_region(regions, addr, 64) {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 64) };
        let mut s = String::new();
        for &b in bytes {
            if b == 0 {
                return Some(s);
            }
            if !(0x20..=0x7E).contains(&b) {
                return None; // non-printable -> not a class name
            }
            s.push(b as char);
        }
        None // no NUL in 64 bytes
    }

    /// True iff `p` points at an Il2CppImage-shaped struct: its name pointer
    /// (offset 0) resolves to a readable ASCII string ending in ".dll". Every
    /// il2cpp image is named "<something>.dll" — adaptive, not hardcoded.
    fn is_image(regions: &[(usize, usize)], p: usize) -> bool {
        if p == 0 {
            return false;
        }
        let name_ptr = match read_u64(regions, p) {
            Some(v) => v as usize,
            None => return false,
        };
        match read_name(regions, name_ptr) {
            Some(name) => name.len() > 4 && name.ends_with(".dll"),
            None => false,
        }
    }

    /// If `p` points at an Il2CppClass-shaped struct, return (name, namespace).
    /// Anchored on the class's `image` back-pointer (offset 0) pointing at a real
    /// image — the structural invariant that rejects look-alikes (e.g. profiler
    /// stat descriptors that merely happen to have two string pointers).
    fn class_fields(regions: &[(usize, usize)], p: usize) -> Option<(String, String)> {
        let image_ptr = read_u64(regions, p.checked_add(0x00)?)? as usize;
        if !is_image(regions, image_ptr) {
            return None;
        }
        let name_ptr = read_u64(regions, p.checked_add(0x10)?)? as usize;
        let ns_ptr = read_u64(regions, p.checked_add(0x18)?)? as usize;
        let name = read_name(regions, name_ptr)?;
        if name.is_empty() {
            return None; // class names are never empty
        }
        let ns = read_name(regions, ns_ptr)?; // namespace may be empty
        Some((name, ns))
    }

    // --- Step 3: scan every slot, validate as an image-anchored class pointer,
    // and dedup by class pointer — captures every loaded class regardless of
    // table layout or how many places reference it. ---
    const SLOT_BUDGET: u64 = 250_000_000;
    // Reading every committed region faults under Wine (some mapped regions report
    // readable but throw on access). The il2cpp metadata heap sits in the first
    // regions, so bound the scan — this is the known crash-safe envelope.
    const MAX_SCAN_REGIONS: usize = 64;
    let mut results: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut slots_scanned: u64 = 0;

    'outer: for (ri, &(start, end)) in regions.iter().enumerate() {
        if ri >= MAX_SCAN_REGIONS {
            break;
        }
        let mut a = start;
        while a + 8 <= end {
            if slots_scanned >= SLOT_BUDGET || results.len() >= cap {
                break 'outer;
            }
            slots_scanned += 1;
            // Safe: [a, a+8) is within [start, end), a committed readable region.
            let candidate = unsafe { *(a as *const u64) } as usize;
            if candidate != 0 && !seen.contains(&candidate) {
                if let Some(pair) = class_fields(&regions, candidate) {
                    seen.insert(candidate);
                    results.push(pair);
                }
            }
            a += 8;
        }
    }

    results.truncate(cap);
    results
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
