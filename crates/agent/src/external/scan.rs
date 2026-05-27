use std::ffi::c_void;

use agent_core::metadata::{find_and_parse_with_offset, layout_for_version};
use agent_core::model::Dump;

pub struct MetadataResult {
    pub dump: Dump,
    /// Absolute address of the metadata blob in process memory.
    pub blob_addr: usize,
    /// Parsed metadata layout version.
    pub version: u32,
    /// Total type definitions count (from metadata header type_defs section).
    pub type_count: u32,
}

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
};

pub use crate::external::region_map::{is_readable, RegionMap, Tunables};

/// Locate + parse the global-metadata blob in memory. Only succeeds on
/// **non-obfuscated** games where the metadata magic is present; on Pixel Worlds
/// the magic is stripped, so this returns `None` and the FFI/class-table path
/// (the rest of the worker) carries the dump. Kept as the easy-game fallback.
pub fn scan_process_for_metadata() -> Option<MetadataResult> {
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
                if let Some((off, dump)) = find_and_parse_with_offset(slice, layout_for_version) {
                    let blob_addr = base + off;
                    let version = if off + 8 <= slice.len() {
                        u32::from_le_bytes([slice[off + 4], slice[off + 5], slice[off + 6], slice[off + 7]])
                    } else { 0 };
                    let type_count = agent_core::metadata::layout_for_version(version).and_then(|lay| {
                        let blob = &slice[off..];
                        agent_core::metadata::compute_type_count(blob, &lay)
                    }).unwrap_or(0);
                    return Some(MetadataResult { dump, blob_addr, version, type_count });
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

/// Locate il2cpp's class table (`s_TypeInfoTable`) ONCE: the densest contiguous
/// array of slots that are each either NULL (an unloaded type) or a pointer to a
/// class-shaped struct. Returns `(base_addr, slot_count)` of the best run, or
/// None. Bounded to the crash-safe region envelope; adjacent sub-regions are
/// coalesced so a table split across them stays one run.
pub fn find_class_table() -> Option<(usize, usize)> {
    let tunables = Tunables::load();
    let map = RegionMap::capture(tunables.max_regions);

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(s, e) in map.regions.iter().take(tunables.max_scan_regions) {
        if let Some(last) = merged.last_mut() {
            if last.1 == s {
                last.1 = e;
                continue;
            }
        }
        merged.push((s, e));
    }

    let mut best: Option<(usize, usize, usize)> = None; // (base, slots, class_count)
    for &(start, end) in merged.iter() {
        let mut a = start;
        let mut run_start = 0usize;
        let mut run_slots = 0usize;
        let mut run_classes = 0usize;
        let mut in_run = false;
        while a + 8 <= end {
            // Safe: [a, a+8) is inside this committed, readable region.
            let slot = unsafe { *(a as *const u64) } as usize;
            let classy = slot != 0 && map.class_fields(slot).is_some();
            if slot == 0 || classy {
                if !in_run {
                    in_run = true;
                    run_start = a;
                    run_slots = 0;
                    run_classes = 0;
                }
                run_slots += 1;
                if classy {
                    run_classes += 1;
                }
            } else if in_run {
                if run_classes >= tunables.min_classes && best.map_or(true, |(_, _, bc)| run_classes > bc) {
                    best = Some((run_start, run_slots, run_classes));
                }
                in_run = false;
            }
            a += 8;
        }
        if in_run
            && run_classes >= tunables.min_classes
            && best.map_or(true, |(_, _, bc)| run_classes > bc)
        {
            best = Some((run_start, run_slots, run_classes));
        }
    }
    best.map(|(base, slots, _)| (base, slots))
}

/// Scan GameAssembly.dll's data section for `Il2CppMetadataRegistration` and
/// return the `types` array address (array of `Il2CppType*` pointers indexed by
/// metadata field `typeIndex`). Returns `None` if not found.
///
/// The struct layout (for x64, MSVC default packing):
/// ```text
/// Off  Field                       Size
/// 0    genericClassesCount         4
/// 4    genericInstsCount           4
/// 8    genericMethodTableCount     4
/// 12   typesCount                  4  ← matched against metadata type_count
/// 16   methodSpecsCount            4
/// 20   padding                     4  (for 8-byte alignment)
/// 24   types                       8  ← the pointer we return
/// ```
pub fn find_types_array(type_count: u32, map: &RegionMap) -> Option<usize> {
    unsafe {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module.is_null() {
            return None;
        }
        let base = module as usize;
        let e_lfanew = *((base + 0x3C) as *const u32) as usize;
        let opt_header = base + e_lfanew + 24;
        let size_of_image = *((opt_header + 0x38) as *const u32) as usize;
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

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) && rsize >= 32 {
                let start = addr.max(rbase);
                let stop = next.min(end);
                if stop <= start { continue; }
                let slice = std::slice::from_raw_parts(start as *const u8, stop - start);

                // Scan for the 4-byte type_count value that matches
                let needle = type_count.to_le_bytes();
                let mut i = 0usize;
                while i + 28 <= slice.len() {
                    // typesCount must appear at offset 12 within the struct
                    if i >= 12 && slice[i - 12..i - 8] == needle {
                        // Found candidate: read the types pointer at offset 24 (12 + 12 = 24)
                        // Also try offset 20 (no padding) for older layouts
                        for types_off in [20usize, 24] {
                            let ptr_addr = start + i - 12 + types_off;
                            if let Some(ptr) = map.read_u64(ptr_addr) {
                                let p = ptr as usize;
                                // Verify: pointer should be non-null and point to readable memory
                                if p != 0 && map.in_region(p, 8) {
                                    return Some(p);
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }

            if next <= addr { break; }
            addr = next;
        }
    }
    None
}
