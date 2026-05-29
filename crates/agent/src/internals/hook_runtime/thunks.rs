//! Per-method thunk emitter + slab allocator. Each hook gets a 32-byte slot
//! in a PAGE_EXECUTE_READWRITE page. The slot contains 23 bytes of x86_64
//! machine code:
//!
//!   49 BA <8-byte method_id>     mov r10, <method_id>    (10 bytes)
//!   49 BB <8-byte shim_addr>     mov r11, <universal_shim addr>  (10 bytes)
//!   41 FF E3                     jmp r11                  (3 bytes)
//!
//! Remaining 9 bytes are 0xCC (int3) for trap-on-overflow safety.

use core::ffi::c_void;
use std::sync::Mutex;

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::shim::universal_shim;

const SLOT_BYTES: usize = 32;
const PAGE_BYTES: usize = 4096;
const SLOTS_PER_PAGE: usize = PAGE_BYTES / SLOT_BYTES;       // 128

struct SlabPage {
    base: usize,
    // Bitmap of free slots — bit i = slot i is free.
    free_mask: u128,
}

impl SlabPage {
    unsafe fn new() -> Option<Self> {
        let p = VirtualAlloc(
            core::ptr::null(),
            PAGE_BYTES,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        );
        if p.is_null() {
            return None;
        }
        // Initialize all slots to 0xCC (int3) — any accidental control transfer
        // to an unallocated slot traps deterministically.
        core::ptr::write_bytes(p as *mut u8, 0xCC, PAGE_BYTES);
        Some(SlabPage { base: p as usize, free_mask: !0u128 >> (128 - SLOTS_PER_PAGE) })
    }

    fn try_alloc(&mut self) -> Option<usize> {
        if self.free_mask == 0 { return None; }
        let idx = self.free_mask.trailing_zeros() as usize;
        self.free_mask &= !(1u128 << idx);
        Some(self.base + idx * SLOT_BYTES)
    }

    fn free(&mut self, slot_addr: usize) -> bool {
        if slot_addr < self.base || slot_addr >= self.base + PAGE_BYTES {
            return false;
        }
        let idx = (slot_addr - self.base) / SLOT_BYTES;
        self.free_mask |= 1u128 << idx;
        true
    }
}

impl Drop for SlabPage {
    fn drop(&mut self) {
        unsafe { VirtualFree(self.base as *mut c_void, 0, MEM_RELEASE); }
    }
}

static SLAB: Mutex<Vec<SlabPage>> = Mutex::new(Vec::new());

/// Allocate a slot and emit the per-method thunk bytes. Returns the address
/// of the slot (this is the `detour` pointer passed to `inline_detour::install`).
pub unsafe fn emit_thunk(method_id: u64) -> Option<usize> {
    let shim_addr = universal_shim as *const () as usize as u64;
    let slot_addr = {
        let mut slab = SLAB.lock().ok()?;
        // Try existing pages first.
        let mut hit = None;
        for page in slab.iter_mut() {
            if let Some(a) = page.try_alloc() {
                hit = Some(a);
                break;
            }
        }
        if let Some(a) = hit {
            a
        } else {
            // All full — allocate a new page.
            let mut new_page = SlabPage::new()?;
            let a = new_page.try_alloc()?;
            slab.push(new_page);
            a
        }
    };

    // Write the thunk bytes.
    let p = slot_addr as *mut u8;
    // mov r10, method_id   (49 BA <imm64>)
    p.add(0).write(0x49);
    p.add(1).write(0xBA);
    (p.add(2) as *mut u64).write_unaligned(method_id);
    // mov r11, shim_addr   (49 BB <imm64>)
    p.add(10).write(0x49);
    p.add(11).write(0xBB);
    (p.add(12) as *mut u64).write_unaligned(shim_addr);
    // jmp r11              (41 FF E3)
    p.add(20).write(0x41);
    p.add(21).write(0xFF);
    p.add(22).write(0xE3);
    // Pad remaining 9 bytes with 0xCC (int3).
    for off in 23..SLOT_BYTES {
        p.add(off).write(0xCC);
    }

    // Flush instruction cache for the modified slot.
    FlushInstructionCache(GetCurrentProcess(), slot_addr as *const c_void, SLOT_BYTES);

    Some(slot_addr)
}

/// Mark a slot as free + overwrite with int3 traps so any leftover jumps
/// fail loudly.
pub unsafe fn free_thunk(slot_addr: usize) {
    let mut slab = match SLAB.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    // Overwrite the slot with int3 so any racing detour-removed call traps.
    let p = slot_addr as *mut u8;
    for off in 0..SLOT_BYTES {
        p.add(off).write(0xCC);
    }
    FlushInstructionCache(GetCurrentProcess(), slot_addr as *const c_void, SLOT_BYTES);
    for page in slab.iter_mut() {
        if page.free(slot_addr) { return; }
    }
}
