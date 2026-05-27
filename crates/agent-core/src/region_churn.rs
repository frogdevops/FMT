//! Pure diff between two memory-region snapshots. Cross-platform / host-testable
//! so the staleness probe's core logic is unit-tested without the game.
//!
//! A "region" is `(start, end)`. Both inputs are assumed sorted by start address
//! (which is how `RegionMap::capture` stores them). The diff answers: between two
//! snapshots, how many regions appeared, vanished, or changed size?

/// Counts of how a region set changed between two snapshots.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Churn {
    /// Regions present in `cur` but not in `prev` (by start address).
    pub added: usize,
    /// Regions present in `prev` but gone from `cur` (by start address).
    pub removed: usize,
    /// Regions with the same start in both but a different end (resized).
    pub changed: usize,
}

impl Churn {
    /// Total regions that differ in any way — the single staleness signal.
    pub fn total(&self) -> usize {
        self.added + self.removed + self.changed
    }
}

/// Diff two region snapshots, each sorted ascending by start address.
/// Linear two-pointer merge; never allocates.
pub fn region_churn(prev: &[(usize, usize)], cur: &[(usize, usize)]) -> Churn {
    let mut churn = Churn::default();
    let (mut i, mut j) = (0usize, 0usize);
    while i < prev.len() && j < cur.len() {
        let (ps, pe) = prev[i];
        let (cs, ce) = cur[j];
        if ps == cs {
            if pe != ce {
                churn.changed += 1;
            }
            i += 1;
            j += 1;
        } else if ps < cs {
            churn.removed += 1; // in prev, no matching start in cur
            i += 1;
        } else {
            churn.added += 1; // in cur, no matching start in prev
            j += 1;
        }
    }
    churn.removed += prev.len() - i; // tail of prev = all removed
    churn.added += cur.len() - j; // tail of cur = all added
    churn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_snapshots_have_no_churn() {
        let r = [(0x1000, 0x2000), (0x3000, 0x4000)];
        assert_eq!(region_churn(&r, &r), Churn::default());
    }

    #[test]
    fn detects_one_added_region() {
        let prev = [(0x1000, 0x2000)];
        let cur = [(0x1000, 0x2000), (0x3000, 0x4000)];
        assert_eq!(region_churn(&prev, &cur), Churn { added: 1, ..Default::default() });
    }

    #[test]
    fn detects_one_removed_region() {
        let prev = [(0x1000, 0x2000), (0x3000, 0x4000)];
        let cur = [(0x3000, 0x4000)];
        assert_eq!(region_churn(&prev, &cur), Churn { removed: 1, ..Default::default() });
    }

    #[test]
    fn detects_a_resized_region() {
        let prev = [(0x1000, 0x2000)];
        let cur = [(0x1000, 0x9000)]; // same start, grew
        assert_eq!(region_churn(&prev, &cur), Churn { changed: 1, ..Default::default() });
    }

    #[test]
    fn mixed_add_remove_change() {
        let prev = [(0x1000, 0x2000), (0x3000, 0x4000), (0x5000, 0x6000)];
        let cur = [(0x1000, 0x2500), (0x5000, 0x6000), (0x7000, 0x8000)];
        // 0x1000 resized (changed), 0x3000 gone (removed), 0x5000 same, 0x7000 new (added)
        assert_eq!(region_churn(&prev, &cur), Churn { added: 1, removed: 1, changed: 1 });
    }

    #[test]
    fn empty_prev_means_all_added() {
        let cur = [(0x1000, 0x2000), (0x3000, 0x4000)];
        assert_eq!(region_churn(&[], &cur), Churn { added: 2, ..Default::default() });
    }

    #[test]
    fn empty_cur_means_all_removed() {
        let prev = [(0x1000, 0x2000), (0x3000, 0x4000)];
        assert_eq!(region_churn(&prev, &[]), Churn { removed: 2, ..Default::default() });
    }

    #[test]
    fn total_sums_all_differences() {
        let c = Churn { added: 2, removed: 3, changed: 4 };
        assert_eq!(c.total(), 9);
    }
}
