//! Global region cache for near-zero read validation. A background thread
//! re-captures committed-readable regions every ~500 ms; `validate_read` does an
//! O(log n) binary search (no syscall) on the hot path and falls back to a single
//! live VirtualQuery on a cache miss (a freshly-allocated region).

use std::ffi::c_void;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
};

use crate::external::region_map::{is_readable, RegionMap, Tunables};

static REGIONS: OnceLock<RwLock<RegionMap>> = OnceLock::new();

fn regions() -> &'static RwLock<RegionMap> {
    REGIONS.get_or_init(|| RwLock::new(RegionMap::capture(Tunables::load().max_regions)))
}

/// Start the background refresher. Call once from the worker.
pub fn start_refresher() {
    std::thread::spawn(|| {
        let max = Tunables::load().max_regions;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let fresh = RegionMap::capture(max);
            if let Ok(mut g) = regions().write() {
                *g = fresh;
            }
        }
    });
}

/// True if [addr, addr+len) is readable. Hot path: binary search the cache.
/// Miss: one live VirtualQuery (correct for new regions, rare).
pub fn validate_read(addr: usize, len: usize) -> bool {
    if let Ok(g) = regions().read() {
        if g.in_region(addr, len) {
            return true;
        }
    }
    live_readable(addr, len)
}

fn live_readable(addr: usize, len: usize) -> bool {
    let end = match addr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    unsafe {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let n = VirtualQuery(
            addr as *const c_void,
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        );
        if n == 0 || mbi.State != MEM_COMMIT || !is_readable(mbi.Protect) {
            return false;
        }
        let base = mbi.BaseAddress as usize;
        addr >= base && end <= base.saturating_add(mbi.RegionSize)
    }
}

/// Snapshot of current cached regions (for the `regions` op and the AOB scan).
pub fn snapshot() -> Vec<(usize, usize)> {
    regions().read().map(|g| g.regions.clone()).unwrap_or_default()
}

/// Validated raw reads for structural walks (klass/FieldInfo). Each validates
/// against the region cache (binary search, miss → VirtualQuery) before reading.
pub fn read_u64(addr: usize) -> Option<u64> {
    if validate_read(addr, 8) { Some(unsafe { *(addr as *const u64) }) } else { None }
}
pub fn read_u32(addr: usize) -> Option<u32> {
    if validate_read(addr, 4) { Some(unsafe { *(addr as *const u32) }) } else { None }
}
/// NUL-terminated printable-ASCII string (<=255 bytes) at `addr`, validated.
pub fn read_cstr(addr: usize) -> Option<String> {
    if !validate_read(addr, 1) { return None; }
    let mut out = String::new();
    for i in 0..255usize {
        let b = read_u32(addr + i).map(|v| (v & 0xFF) as u8)?;
        if b == 0 { return Some(out); }
        if !(0x20..=0x7E).contains(&b) { return None; }
        out.push(b as char);
    }
    Some(out)
}
