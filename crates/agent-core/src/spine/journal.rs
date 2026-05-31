//! Per-runtime write journal: captures first-touched original bytes per address
//! so the reload orchestrator can restore the game world to pre-script state.
//!
//! Concrete type with a `fn` pointer read backend (not a generic) so it can be
//! stored in `ParkedRuntime` without infecting the registry / orchestrator
//! with generics. The agent crate provides a real adapter wrapping
//! `mem_backend::raw_read`; tests pass any `fn(usize, usize) -> Option<Vec<u8>>`.

use std::collections::HashMap;

/// Read-backend signature: given an address and width, return the original
/// bytes at that address, or `None` if unreadable.
pub type JournalReadFn = fn(usize, usize) -> Option<Vec<u8>>;

pub struct WriteJournal {
    entries: HashMap<usize, Vec<u8>>,
    read_backend: JournalReadFn,
}

impl WriteJournal {
    pub fn new(read_backend: JournalReadFn) -> Self {
        Self { entries: HashMap::new(), read_backend }
    }

    /// Record a first-touch at `addr` with `width` bytes if not already recorded.
    /// Subsequent calls for the same address are no-ops (only the first original
    /// is preserved). Returns `true` on first-touch; `false` if already recorded
    /// or if the read backend returns `None`.
    pub fn touch(&mut self, addr: usize, width: usize) -> bool {
        if self.entries.contains_key(&addr) {
            return false;
        }
        match (self.read_backend)(addr, width) {
            Some(bytes) => {
                self.entries.insert(addr, bytes);
                true
            }
            None => false,
        }
    }

    /// Number of distinct addresses captured.
    pub fn len(&self) -> usize { self.entries.len() }

    /// True if no addresses have been touched.
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Extract entries for revert, leaving the journal empty (read_backend intact).
    /// After this, `len()` returns 0. The returned HashMap is iterated by the
    /// orchestrator's revert step.
    pub fn take_entries(&mut self) -> HashMap<usize, Vec<u8>> {
        std::mem::take(&mut self.entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test backends are bare `fn` items so they have function-pointer type
    // (which is what JournalReadFn requires). Closures with captures cannot
    // be coerced to fn pointers.

    fn read_seed_ab(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xAB; width])
    }

    fn read_seed_cd(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xCD; width])
    }

    fn read_seed_ef(_addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![0xEF; width])
    }

    fn read_unreadable(_addr: usize, _width: usize) -> Option<Vec<u8>> {
        None
    }

    fn read_addr_low_byte(addr: usize, width: usize) -> Option<Vec<u8>> {
        Some(vec![addr as u8; width])
    }

    #[test]
    fn touch_records_first_time_only() {
        let mut j = WriteJournal::new(read_seed_ab);
        assert!(j.touch(0x1000, 4));   // first touch returns true
        assert!(!j.touch(0x1000, 4));  // second touch returns false
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn touch_records_multiple_addresses() {
        let mut j = WriteJournal::new(read_seed_cd);
        j.touch(0x1000, 4);
        j.touch(0x2000, 8);
        j.touch(0x3000, 1);
        assert_eq!(j.len(), 3);
    }

    #[test]
    fn touch_preserves_original_bytes() {
        let mut j = WriteJournal::new(read_seed_ef);
        j.touch(0x1000, 4);
        let entries = j.take_entries();
        assert_eq!(entries.len(), 1);
        let bytes = entries.get(&0x1000).expect("addr 0x1000 captured");
        assert_eq!(bytes, &vec![0xEF, 0xEF, 0xEF, 0xEF]);
    }

    #[test]
    fn touch_returns_false_on_unreadable() {
        let mut j = WriteJournal::new(read_unreadable);
        assert!(!j.touch(0x1000, 4));
        assert_eq!(j.len(), 0);
    }

    #[test]
    fn first_touch_wins_under_overlapping_widths() {
        // First touch captures 4 bytes. Second touch with a wider width is a
        // no-op; the journal still has exactly one entry of width 4.
        let mut j = WriteJournal::new(read_addr_low_byte);
        j.touch(0x1000, 4);   // captures 4 bytes
        j.touch(0x1000, 8);   // no-op (already touched)
        let entries = j.take_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(&0x1000).unwrap().len(), 4);  // first-touch width preserved
    }

    #[test]
    fn take_entries_leaves_journal_empty_with_backend_intact() {
        let mut j = WriteJournal::new(read_seed_ab);
        j.touch(0x1000, 4);
        let drained = j.take_entries();
        assert_eq!(drained.len(), 1);
        assert!(j.is_empty());
        // Backend still works; new touches succeed.
        assert!(j.touch(0x2000, 4));
        assert_eq!(j.len(), 1);
    }
}
