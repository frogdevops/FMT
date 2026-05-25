use std::collections::HashSet;
use std::ffi::c_void;

use agent_core::metadata::{
    find_and_parse_with_offset, find_magic_offsets, layout_for_version,
};
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
/// non-empty `Dump`, along with its absolute address and version. Read-only.
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

/// Diagnostic: find every metadata-magic marker in committed/readable memory and
/// return (absolute_address, version) for each (version = the u32 at the marker's
/// byte offset +4). Capped to avoid log spam. Read-only.
///
/// Not on the current dump path — retained for the upcoming TCP query layer so
/// the IDE plugin can ask "how many metadata blob candidates are live in this
/// process?" for triage on unknown games.
#[allow(dead_code)]
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
///
/// Retained for IDE-plugin queries (e.g. locating string literals in the binary
/// for hook anchoring); not on the current dump path.
#[allow(dead_code)]
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
///
/// Standalone variant of `find_class_table` — useful when the table itself is
/// fragmented or relocated. Kept for the IDE plugin's "force a full memory
/// rescan" command.
#[allow(dead_code)]
pub fn scan_for_classes(cap: usize) -> Vec<(String, String)> {
    let tunables = Tunables::load();

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
                if regions.len() >= tunables.max_regions {
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
    let mut results: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut slots_scanned: u64 = 0;

    'outer: for (ri, &(start, end)) in regions.iter().enumerate() {
        if ri >= tunables.max_scan_regions {
            break;
        }
        let mut a = start;
        while a + 8 <= end {
            if slots_scanned >= tunables.slot_budget || results.len() >= cap {
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
///
/// Used by the IDE plugin for ad-hoc "find this string in process memory"
/// queries (e.g. locating a known PlayFab URL, an asset path, a token).
#[allow(dead_code)]
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

/// A snapshot of committed, readable memory regions, with validated readers.
/// Built once via VirtualQuery (no content reads), then used to safely read
/// pointers/strings — every read is bounds-checked against a region first, so it
/// can never fault.
pub struct RegionMap {
    regions: Vec<(usize, usize)>, // sorted (start, end)
}

impl RegionMap {
    /// Capture up to `max_regions` committed, readable regions. VirtualQuery only.
    pub fn capture(max_regions: usize) -> RegionMap {
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
                    if regions.len() >= max_regions {
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
        RegionMap { regions }
    }

    /// True iff [addr, addr+len) fits entirely within one region.
    pub fn in_region(&self, addr: usize, len: usize) -> bool {
        let end = match addr.checked_add(len) {
            Some(e) => e,
            None => return false,
        };
        let idx = match self.regions.binary_search_by(|r| r.0.cmp(&addr)) {
            Ok(i) => i,
            Err(0) => return false,
            Err(i) => i - 1,
        };
        let (start, region_end) = self.regions[idx];
        addr >= start && end <= region_end
    }

    pub fn read_u64(&self, addr: usize) -> Option<u64> {
        if self.in_region(addr, 8) {
            Some(unsafe { *(addr as *const u64) })
        } else {
            None
        }
    }

    pub fn read_u32(&self, addr: usize) -> Option<u32> {
        if self.in_region(addr, 4) {
            Some(unsafe { *(addr as *const u32) })
        } else {
            None
        }
    }

    /// NUL-terminated printable-ASCII string (<= 63 chars) at `addr`, or None.
    /// Bounds-checked via `in_region`; safe to call on any address.
    pub fn read_name(&self, addr: usize) -> Option<String> {
        if !self.in_region(addr, 64) {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 64) };
        let mut s = String::new();
        for &b in bytes {
            if b == 0 {
                return Some(s);
            }
            if !(0x20..=0x7E).contains(&b) {
                return None;
            }
            s.push(b as char);
        }
        None
    }

    /// True iff `p` points at an image-shaped struct (name at +0 ends ".dll").
    fn is_image(&self, p: usize) -> bool {
        if p == 0 {
            return false;
        }
        let name_ptr = match self.read_u64(p) {
            Some(v) => v as usize,
            None => return false,
        };
        match self.read_name(name_ptr) {
            Some(name) => name.len() > 4 && name.ends_with(".dll"),
            None => false,
        }
    }

    /// If `p` is an Il2CppClass-shaped struct (image back-ptr @0, name @0x10,
    /// namespace @0x18), return (name, namespace).
    pub fn class_fields(&self, p: usize) -> Option<(String, String)> {
        let image_ptr = self.read_u64(p.checked_add(0x00)?)? as usize;
        if !self.is_image(image_ptr) {
            return None;
        }
        let name_ptr = self.read_u64(p.checked_add(0x10)?)? as usize;
        let ns_ptr = self.read_u64(p.checked_add(0x18)?)? as usize;
        let name = self.read_name(name_ptr)?;
        if name.is_empty() {
            return None;
        }
        let ns = self.read_name(ns_ptr)?;
        Some((name, ns))
    }
}

/// Tunable scan parameters, overrideable via `FROG_*` environment variables.
///
/// | Env var                         | Default          | Purpose                        |
/// |---------------------------------|------------------|--------------------------------|
/// | `FROG_MAX_REGIONS`              | 8192             | Region‑capture budget          |
/// | `FROG_MAX_SCAN_REGIONS`         | 64               | Crash‑safe scan envelope       |
/// | `FROG_SLOT_BUDGET`              | 250_000_000      | Max slots to scan              |
/// | `FROG_MIN_CLASSES`              | 64               | Min classes to accept a run    |
/// | `FROG_TABLE_MAX_SLOTS`          | 20000            | Max class‑table slots to read  |
pub struct Tunables {
    pub max_regions: usize,
    pub max_scan_regions: usize,
    pub slot_budget: u64,
    pub min_classes: usize,
    pub table_max_slots: usize,
}

impl Tunables {
    fn from_env(name: &str, default: usize) -> usize {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    fn from_env_u64(name: &str, default: u64) -> u64 {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn load() -> Self {
        Self {
            max_regions:      Self::from_env("FROG_MAX_REGIONS", 8192),
            max_scan_regions: Self::from_env("FROG_MAX_SCAN_REGIONS", 64),
            slot_budget:      Self::from_env_u64("FROG_SLOT_BUDGET", 250_000_000),
            min_classes:      Self::from_env("FROG_MIN_CLASSES", 64),
            table_max_slots:  Self::from_env("FROG_TABLE_MAX_SLOTS", 20000),
        }
    }
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

/// Re-read a located class table: walk `count` slots from `base` and collect
/// (name, namespace) for every slot that points to a class-shaped struct. Cheap
/// relative to a full scan — this is the per-tick "watch" read. Read-only.
///
/// Designed for the upcoming IDE plugin's live-class-list view: call this every
/// N seconds with the cached (base, count) from `find_class_table` to refresh
/// the loaded-class set without redoing the full memory walk.
#[allow(dead_code)]
pub fn read_class_table(base: usize, count: usize) -> Vec<(String, String)> {
    let tunables = Tunables::load();
    let map = RegionMap::capture(tunables.max_regions);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < count {
        let a = base.wrapping_add(i * 8);
        if let Some(slot) = map.read_u64(a) {
            let p = slot as usize;
            if p != 0 {
                if let Some(pair) = map.class_fields(p) {
                    out.push(pair);
                }
            }
        }
        i += 1;
    }
    out
}
