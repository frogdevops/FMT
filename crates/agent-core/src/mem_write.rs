//! Pure safety decision for a guarded memory write.
//!
//! Writing into the live game is the one operation with game-level blast radius,
//! so the "is this write safe?" decision lives here — pure and host-testable —
//! while the Windows agent does the `VirtualQuery`/`VirtualProtect` FFI around it.

/// Windows memory constants (stable, public ABI values) so this pure crate can
/// interpret a `VirtualQuery` result without depending on `windows-sys`.
pub const MEM_COMMIT: u32 = 0x1000;
pub const PAGE_GUARD: u32 = 0x100;
pub const PAGE_NOACCESS: u32 = 0x01;

/// Why a guarded write was refused. `ProtectFailed` is only ever produced by the
/// FFI write path (the `VirtualProtect` call), never by `can_write` itself.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WriteError {
    /// Zero-length write requested.
    ZeroLen,
    /// Target region is not committed memory.
    NotCommitted,
    /// Target page is a guard page or no-access.
    GuardPage,
    /// The write would run past the end of the region containing `addr`.
    SpansRegion,
    /// `VirtualProtect` failed to make the page writable (FFI path only).
    ProtectFailed,
}

/// Decide whether writing `len` bytes at `addr` is safe, given the `VirtualQuery`
/// result for the region containing `addr` (`state`, `protect`, `base`, `size`).
/// `VirtualQuery` always returns the region that contains `addr`, so normally
/// `base <= addr`; we still reject the degenerate `addr < base` case.
pub fn can_write(
    state: u32,
    protect: u32,
    base: usize,
    size: usize,
    addr: usize,
    len: usize,
) -> Result<(), WriteError> {
    if len == 0 {
        return Err(WriteError::ZeroLen);
    }
    if state != MEM_COMMIT {
        return Err(WriteError::NotCommitted);
    }
    if protect & PAGE_GUARD != 0 || protect == PAGE_NOACCESS {
        return Err(WriteError::GuardPage);
    }
    let region_end = base.saturating_add(size);
    let write_end = match addr.checked_add(len) {
        Some(e) => e,
        None => return Err(WriteError::SpansRegion),
    };
    if addr < base || write_end > region_end {
        return Err(WriteError::SpansRegion);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RW: u32 = 0x04; // PAGE_READWRITE
    const RO: u32 = 0x02; // PAGE_READONLY (writable after VirtualProtect)

    #[test]
    fn accepts_a_valid_write_inside_a_committed_region() {
        assert_eq!(can_write(MEM_COMMIT, RW, 0x1000, 0x1000, 0x1400, 8), Ok(()));
    }

    #[test]
    fn accepts_a_read_only_page_the_ffi_will_flip() {
        // can_write only checks safety; making it writable is the FFI's job.
        assert_eq!(can_write(MEM_COMMIT, RO, 0x1000, 0x1000, 0x1000, 4), Ok(()));
    }

    #[test]
    fn rejects_zero_length() {
        assert_eq!(can_write(MEM_COMMIT, RW, 0x1000, 0x1000, 0x1400, 0), Err(WriteError::ZeroLen));
    }

    #[test]
    fn rejects_uncommitted_memory() {
        let free = 0x10000; // MEM_FREE
        assert_eq!(can_write(free, RW, 0x1000, 0x1000, 0x1400, 8), Err(WriteError::NotCommitted));
    }

    #[test]
    fn rejects_guard_page() {
        assert_eq!(can_write(MEM_COMMIT, RW | PAGE_GUARD, 0x1000, 0x1000, 0x1400, 8), Err(WriteError::GuardPage));
    }

    #[test]
    fn rejects_no_access_page() {
        assert_eq!(can_write(MEM_COMMIT, PAGE_NOACCESS, 0x1000, 0x1000, 0x1400, 8), Err(WriteError::GuardPage));
    }

    #[test]
    fn rejects_write_that_spans_past_region_end() {
        // region [0x1000, 0x2000); writing 8 bytes at 0x1FFC ends at 0x2004 > 0x2000.
        assert_eq!(can_write(MEM_COMMIT, RW, 0x1000, 0x1000, 0x1FFC, 8), Err(WriteError::SpansRegion));
    }

    #[test]
    fn rejects_addr_below_region_base() {
        assert_eq!(can_write(MEM_COMMIT, RW, 0x1000, 0x1000, 0x0FF8, 8), Err(WriteError::SpansRegion));
    }

    #[test]
    fn rejects_addr_len_overflow() {
        assert_eq!(can_write(MEM_COMMIT, RW, 0, usize::MAX, usize::MAX, 8), Err(WriteError::SpansRegion));
    }
}
