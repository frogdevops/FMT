//! Memory probes (opt-in), each proving one reliability claim with real numbers
//! before we build a live memory API:
//!   * `run_staleness_probe` (`FROG_MEM_PROBE`): do the addresses Frog
//!     dereferences stay valid as the game runs? Re-snapshots on a bounded timer,
//!     measures region churn, re-validates a sample of klass pointers each tick.
//!     Read-only.
//!   * `run_write_probe` (`FROG_WRITE_PROBE`): does the guarded write mechanism
//!     work, does its guard reject bad targets, and can we write a genuine game
//!     address? Writes only self-owned memory or identical bytes back.
//!
//! Both are bounded and crash-safe (reads via `RegionMap`, writes via the
//! validated `guarded_write`).

use std::time::Duration;

use agent_core::region_churn::{region_churn, Churn};

use crate::internals::config::Il2CppConfig;
use crate::external::write::guarded_write;
use crate::paths::log;
use crate::external::region_map::RegionMap;

/// How many snapshots to take and how long to wait between them. Env-overridable
/// (`FROG_MEM_PROBE_SNAPSHOTS`, `FROG_MEM_PROBE_INTERVAL_MS`) but hard-capped so a
/// bad value can't turn the probe into a multi-minute stall.
const DEFAULT_SNAPSHOTS: usize = 15;
const DEFAULT_INTERVAL_MS: u64 = 2000;
const MAX_SNAPSHOTS: usize = 60;
const MAX_INTERVAL_MS: u64 = 10_000;
/// How many klass pointers to sample and track across the run.
const SAMPLE_TARGET: usize = 32;

fn env_capped(name: &str, default: u64, cap: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
        .min(cap)
        .max(1)
}

/// One tracked klass pointer plus the name it had at T0.
struct Sample {
    klass: usize,
    name: String,
}

/// Read a klass's name the same way the resolver does: name pointer at
/// `klass + 0x10`, then a bounds-checked C-string. `None` if either read leaves
/// the captured regions (i.e. the pointer is no longer valid).
fn read_klass_name(map: &RegionMap, klass: usize) -> Option<String> {
    let name_ptr = map.read_u64(klass.checked_add(0x10)?)? as usize;
    map.read_name(name_ptr)
}

/// Sample up to `SAMPLE_TARGET` non-null klass pointers spread across the table,
/// recording each one's name at T0 as the baseline to compare against later.
fn collect_samples(
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    cfg: &Il2CppConfig,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    if table_count == 0 {
        return samples;
    }
    // Stride across the whole table so the sample isn't clustered at the start.
    let step = (table_count / SAMPLE_TARGET).max(1);
    let mut i = 0;
    while i < table_count && samples.len() < SAMPLE_TARGET {
        let slot = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(k) = map.read_u64(slot) {
            let klass = k as usize;
            if klass != 0 {
                if let Some(name) = read_klass_name(map, klass) {
                    if !name.is_empty() {
                        samples.push(Sample { klass, name });
                    }
                }
            }
        }
        i += step;
    }
    samples
}

/// How many samples are still valid (in-region) with an unchanged name.
fn revalidate(map: &RegionMap, samples: &[Sample]) -> usize {
    samples
        .iter()
        .filter(|s| read_klass_name(map, s.klass).as_deref() == Some(s.name.as_str()))
        .count()
}

/// Run the bounded staleness probe and log a verdict block. Opt-in: the caller
/// only invokes this when `FROG_MEM_PROBE` is set.
pub fn run_staleness_probe(table_base: usize, table_count: usize, cfg: &Il2CppConfig) {
    let snapshots = env_capped("FROG_MEM_PROBE_SNAPSHOTS", DEFAULT_SNAPSHOTS as u64, MAX_SNAPSHOTS as u64) as usize;
    let interval = Duration::from_millis(env_capped("FROG_MEM_PROBE_INTERVAL_MS", DEFAULT_INTERVAL_MS, MAX_INTERVAL_MS));
    let max_regions = crate::external::region_map::Tunables::load().max_regions;

    log("=== MEMORY STALENESS PROBE ===");
    log(&format!(
        "  config: {} snapshots, {} ms apart (~{}s), tracking up to {} klass pointers",
        snapshots,
        interval.as_millis(),
        (snapshots as u128 * interval.as_millis()) / 1000,
        SAMPLE_TARGET
    ));

    let mut prev = RegionMap::capture(max_regions);
    let samples = collect_samples(&prev, table_base, table_count, cfg);
    let total = samples.len();
    log(&format!(
        "  T0: {} regions, sampled {} klass pointers, table_base {} in-region",
        prev.regions.len(),
        total,
        if prev.in_region(table_base, 8) { "is" } else { "NOT" }
    ));
    if total == 0 {
        log("  no klass pointers sampled; cannot assess staleness");
        log("=== end MEMORY STALENESS PROBE ===");
        return;
    }

    let mut cumulative = Churn::default();
    let mut min_region_count = prev.regions.len();
    let mut max_region_count = prev.regions.len();
    let mut worst_valid = total; // fewest samples valid at any snapshot
    let mut worst_snapshot = 0usize;
    let mut table_base_always_valid = prev.in_region(table_base, 8);

    for n in 1..=snapshots {
        std::thread::sleep(interval);
        let cur = RegionMap::capture(max_regions);

        let churn = region_churn(&prev.regions, &cur.regions);
        cumulative.added += churn.added;
        cumulative.removed += churn.removed;
        cumulative.changed += churn.changed;

        let rc = cur.regions.len();
        min_region_count = min_region_count.min(rc);
        max_region_count = max_region_count.max(rc);

        let valid = revalidate(&cur, &samples);
        if valid < worst_valid {
            worst_valid = valid;
            worst_snapshot = n;
        }
        table_base_always_valid &= cur.in_region(table_base, 8);

        log(&format!(
            "  snapshot {:>2}: {} regions (churn +{} -{} ~{}), {}/{} samples still valid",
            n, rc, churn.added, churn.removed, churn.changed, valid, total
        ));
        prev = cur;
    }

    log("  --- verdict ---");
    log(&format!(
        "  region count over run: {}..{} (cumulative churn: +{} added, -{} removed, ~{} resized)",
        min_region_count, max_region_count, cumulative.added, cumulative.removed, cumulative.changed
    ));
    log(&format!(
        "  table_base stayed in-region every snapshot: {}",
        if table_base_always_valid { "YES" } else { "NO" }
    ));
    if worst_valid == total && table_base_always_valid {
        log(&format!(
            "  RELIABLE: all {}/{} sampled pointers stayed valid + name-stable across all {} snapshots.",
            total, total, snapshots
        ));
        log("  -> one-shot snapshot holds for our access pattern; per-read live validation NOT mandatory.");
    } else {
        log(&format!(
            "  STALE: worst case {}/{} samples valid (at snapshot {}); table_base_always_valid={}.",
            worst_valid, total, worst_snapshot, table_base_always_valid
        ));
        log("  -> snapshot rots under us; the Spec-2 memory API must re-capture often or validate per-read.");
    }
    log("=== end MEMORY STALENESS PROBE ===");
}

/// Prove the guarded write primitive three ways, then log a verdict:
///   1. mechanism — write to a self-owned `Box<u64>`, read it back, confirm changed;
///   2. guard — a write to an uncommitted address (`0x10`) returns `Err`, no crash;
///   3. real game memory — rewrite a live klass field with its *own* bytes (a no-op
///      value-wise), confirming we can write a genuine game address safely.
/// Opt-in: the caller only invokes this when `FROG_WRITE_PROBE` is set.
pub fn run_write_probe(table_base: usize, table_count: usize, cfg: &Il2CppConfig) {
    log("=== MEMORY WRITE PROBE ===");

    // 1. Mechanism — self-owned target.
    let owned: Box<u64> = Box::new(0xAAAA_AAAA_AAAA_AAAA);
    let owned_addr = owned.as_ref() as *const u64 as usize;
    let new_val: u64 = 0x5555_5555_5555_5555;
    let r1 = unsafe { guarded_write(owned_addr, &new_val.to_le_bytes()) };
    let mech_ok = r1.is_ok() && *owned == new_val;
    log(&format!(
        "  1. self-owned write @ {:#x}: result={:?}, readback={:#x} -> {}",
        owned_addr, r1, *owned, if mech_ok { "PASS" } else { "FAIL" }
    ));

    // 2. Guard — uncommitted address must be refused, not faulted.
    let r2 = unsafe { guarded_write(0x10, &[0u8; 8]) };
    let guard_ok = r2.is_err();
    log(&format!(
        "  2. guard rejects bad addr 0x10: result={:?} -> {}",
        r2, if guard_ok { "PASS" } else { "FAIL" }
    ));

    // 3. Real game memory — identical rewrite of a live klass field (no-op value).
    let map = RegionMap::capture(crate::external::region_map::Tunables::load().max_regions);
    let step = (table_count / 32).max(1);
    let mut target = None;
    let mut i = 0;
    while i < table_count && target.is_none() {
        let slot = table_base.wrapping_add(i * cfg.class_table_step);
        if let Some(k) = map.read_u64(slot) {
            let klass = k as usize;
            if klass != 0 {
                let field = klass + 0x10; // name pointer; rewriting it with itself is a no-op
                if let Some(orig) = map.read_u64(field) {
                    target = Some((field, orig));
                }
            }
        }
        i += step;
    }
    let game_ok = match target {
        Some((field, orig)) => {
            let r3 = unsafe { guarded_write(field, &orig.to_le_bytes()) };
            let after = map.read_u64(field);
            let ok = r3.is_ok() && after == Some(orig);
            log(&format!(
                "  3. identical rewrite of game addr {:#x} (={:#x}): result={:?}, readback={:#x?} -> {}",
                field, orig, r3, after, if ok { "PASS" } else { "FAIL" }
            ));
            ok
        }
        None => {
            log("  3. real game-address test SKIPPED: no writable klass field sampled");
            false
        }
    };

    log("  --- verdict ---");
    if mech_ok && guard_ok && game_ok {
        log("  RELIABLE: guarded write works, the guard rejects bad targets, and a genuine game address is writable.");
        log("  -> the write primitive is safe to expose as a Spec-2 mem.write API.");
    } else {
        log(&format!(
            "  ISSUE: mechanism={}, guard={}, game={} — investigate before exposing mem.write.",
            mech_ok, guard_ok, game_ok
        ));
    }
    log("=== end MEMORY WRITE PROBE ===");
}
