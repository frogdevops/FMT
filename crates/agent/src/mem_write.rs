//! Guarded memory write — the one operation that can crash the game.
//!
//! Every write does its own live `VirtualQuery` (never trusts a possibly-stale
//! `RegionMap` snapshot), runs the pure `can_write` safety decision, flips the
//! page writable while saving the old protection, writes, then restores. A bad
//! or uncommitted target returns `Err` — it never faults. A write to a *valid*
//! but semantically-wrong game address can still corrupt game state; that is the
//! actor's responsibility, not something the guard can catch.

use std::ffi::c_void;
use std::mem::size_of;

use windows_sys::Win32::System::Memory::{
    VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE,
};

pub use agent_core::mem_write::WriteError;
use agent_core::mem_write::can_write;

/// Write `bytes` into this process at `addr`, guarded. See module docs.
///
/// # Safety
/// `addr` may be any value; the function validates it before touching memory.
/// The caller is responsible for the *meaning* of the bytes written.
pub unsafe fn guarded_write(addr: usize, bytes: &[u8]) -> Result<(), WriteError> {
    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let queried = VirtualQuery(
        addr as *const c_void,
        &mut mbi,
        size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if queried == 0 {
        return Err(WriteError::NotCommitted);
    }

    can_write(
        mbi.State,
        mbi.Protect,
        mbi.BaseAddress as usize,
        mbi.RegionSize,
        addr,
        bytes.len(),
    )?;

    let mut old_protect: u32 = 0;
    if VirtualProtect(addr as *mut c_void, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old_protect) == 0 {
        return Err(WriteError::ProtectFailed);
    }

    std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());

    // Restore the original protection (best-effort; the bytes are already in).
    let mut discard: u32 = 0;
    VirtualProtect(addr as *mut c_void, bytes.len(), old_protect, &mut discard);
    Ok(())
}
