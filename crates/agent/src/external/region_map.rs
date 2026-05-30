//! Bounds-checked memory access and runtime tunables.
//!
//! `RegionMap` snapshots every committed-readable virtual-memory region in
//! the process and provides safe `read_*` helpers that fault-check every
//! address against that snapshot. All hot-path memory reads in the agent go
//! through here so a stale or fragmented runtime can never crash us.
//!
//! `Tunables` collects the scan-budget knobs (region count caps, slot budgets,
//! minimum class-table density) that callers consult on each scan. Defaults
//! are tuned for Unity 2017–6000.x; override per-game via `FROG_*` env vars.

use std::ffi::c_void;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ,
    PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY,
    PAGE_READWRITE, PAGE_WRITECOPY,
};

/// True iff the page protection bits allow plain reads (and the page isn't a
/// guard page). Used to filter VirtualQuery results before slurping them.
pub fn is_readable(protect: u32) -> bool {
    const MASK: u32 = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    (protect & MASK) != 0 && (protect & PAGE_GUARD) == 0
}

/// A snapshot of committed, readable memory regions, with validated readers.
/// Built once via VirtualQuery (no content reads), then used to safely read
/// pointers/strings — every read is bounds-checked against a region first, so it
/// can never fault.
pub struct RegionMap {
    pub(crate) regions: Vec<(usize, usize)>, // sorted (start, end)
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

    pub fn read_u16(&self, addr: usize) -> Option<u16> {
        if self.in_region(addr, 2) {
            Some(unsafe { *(addr as *const u16) })
        } else {
            None
        }
    }

    pub fn read_u8(&self, addr: usize) -> Option<u8> {
        if self.in_region(addr, 1) {
            Some(unsafe { *(addr as *const u8) })
        } else {
            None
        }
    }

    /// NUL-terminated string at `addr`, decoded as UTF-8 (lossy on invalid sequences).
    /// Reads up to 1024 bytes; rejects any control byte (0x01..=0x1F) as a garbage
    /// signal. Returns `None` if no NUL found in window, the string is empty, or
    /// the address is out of mapped regions. Bounds-checked via `in_region`.
    pub fn read_name(&self, addr: usize) -> Option<String> {
        if !self.in_region(addr, 1024) {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 1024) };
        let mut end = None;
        for (i, &b) in bytes.iter().enumerate() {
            if b == 0 {
                end = Some(i);
                break;
            }
            if b < 0x20 {
                // control bytes = garbage signal (random binary in low control range)
                return None;
            }
        }
        let len = end?;
        if len == 0 {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes[..len]).into_owned())
    }

    /// Structural-predicate variant: NUL-terminated printable-ASCII string
    /// (`0x20..=0x7E`) up to 63 chars at `addr`. Returns `None` for any byte
    /// outside printable ASCII OR when no NUL appears within 64 bytes.
    ///
    /// USE THIS WHEN: validating unknown memory might be a name (e.g. structural
    /// scanners walking arbitrary slots, image-shape predicates). The strict
    /// filter is what makes `find_class_table` discriminate the real class
    /// table from mscorlib-only or other smaller tables.
    ///
    /// Use [`read_name`] (lenient, 1024-byte, UTF-8 lossy) when the pointer is
    /// already known to come from validated il2cpp metadata.
    pub fn read_name_strict(&self, addr: usize) -> Option<String> {
        if !self.in_region(addr, 64) {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 64) };
        let mut s = String::new();
        for &b in bytes {
            if b == 0 {
                if s.is_empty() {
                    return None;
                }
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
        match self.read_name_strict(name_ptr) {
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
        let name = self.read_name_strict(name_ptr)?;
        let ns = self.read_name_strict(ns_ptr).unwrap_or_default();
        Some((name, ns))
    }
}

/// Tunable scan parameters, overrideable via `FROG_*` environment variables.
///
/// | Env var                         | Default          | Purpose                        |
/// |---------------------------------|------------------|--------------------------------|
/// | `FROG_MAX_REGIONS`              | 8192             | Region-capture budget          |
/// | `FROG_MAX_SCAN_REGIONS`         | 64               | Crash-safe scan envelope       |
/// | `FROG_MIN_CLASSES`              | 64               | Min classes to accept a run    |
/// | `FROG_TABLE_MAX_SLOTS`          | 20000            | Max class-table slots to read  |
pub struct Tunables {
    pub max_regions: usize,
    pub max_scan_regions: usize,
    pub min_classes: usize,
    pub table_max_slots: usize,
}

impl Tunables {
    fn from_env(name: &str, default: usize) -> usize {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn load() -> Self {
        Self {
            max_regions:      Self::from_env("FROG_MAX_REGIONS", 8192),
            max_scan_regions: Self::from_env("FROG_MAX_SCAN_REGIONS", 64),
            min_classes:      Self::from_env("FROG_MIN_CLASSES", 64),
            table_max_slots:  Self::from_env("FROG_TABLE_MAX_SLOTS", 20000),
        }
    }
}
