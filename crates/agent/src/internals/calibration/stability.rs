//! Phase 0: poll the class table until the structural anchors the probes need
//! are resolvable AND the populated count stabilizes.
//!
//! The earlier single-plateau heuristic stabilized too early (e.g. 175 classes at
//! 600ms) because transient burst-gaps during loading look like a plateau. The
//! anchor-based check (Int32, String, Object, Math + known methods) is more robust
//! but resolves too quickly on obfuscated builds (e.g. Pixel Worlds) where the
//! core types load immediately but game-specific classes arrive lazily over a
//! longer window.
//!
//! This version combines BOTH signals:
//!   1. Anchor classes + methods must resolve (structural readiness)
//!   2. Populated-slot count must be unchanged for N consecutive polls
//!      (`min_stable_polls`) — rejecting transient burst-gaps while waiting for
//!      the game to finish loading its lazy types.
//!
//! Self-contained: it must NOT use `internals::api::find_class`/`find_method`,
//! because those require `ctx::init` — and ctx is not initialized until AFTER
//! `probe()` runs in entry.rs. So this module walks the table directly using the
//! passed-in `Il2CppApi` and the cache reader, mirroring the find_class logic.

use std::time::{Duration, Instant};

use crate::internals::calibration::anchors::{local_find_class, local_find_method};
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;
use crate::paths::log;

#[derive(Debug)]
pub enum StabilityResult {
    Stable   { count: usize, elapsed_ms: u64, polls: u32 },
    Timeout  { last_count: usize, elapsed_ms: u64, anchors_met: bool },
}

impl StabilityResult {
    pub fn summary(&self) -> String {
        match self {
            StabilityResult::Stable { count, elapsed_ms, polls } => format!(
                "stable at {} populated slots after {}ms ({} polls)",
                count, elapsed_ms, polls
            ),
            StabilityResult::Timeout { last_count, elapsed_ms, anchors_met } => {
                let stage = if *anchors_met { "count never stabilized" } else { "anchors never resolved" };
                format!(
                    "TIMEOUT — {} after {}ms (last populated: {})",
                    stage, elapsed_ms, last_count
                )
            }
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const TIMEOUT: Duration = Duration::from_secs(30);

/// Block the calling thread until the class table is STRUCTURALLY ready AND the
/// populated-slot count stabilizes for `min_stable_polls` consecutive polls.
///
/// Two-phase approach:
///   1. Wait for anchor classes (Int32, String, Object, Math + methods) to
///      resolve via the is_klass_shape-gated walk — these are present in every
///      il2cpp build ≥ v24.
///   2. Once anchors resolve, track the populated slot count. Only declare
///      "stable" after the count has been unchanged for `min_stable_polls`
///      consecutive polls. This gives lazy-loading games (Pixel Worlds) time
///      to finish materialising their full class table.
///
/// 30s absolute backstop. Timeout is non-fatal (the caller proceeds with degraded
/// probing) but always logs cleanly.
pub fn await_class_table_stable(
    api: &Il2CppApi,
    table_base: usize,
    table_capacity: usize,
    class_table_step: usize,
    min_stable_polls: usize,
) -> StabilityResult {
    // Baseline offsets for the self-contained method walk. The anchors are
    // well-known core classes that exist in every il2cpp build at the standard
    // layout, so the fallback constants are correct for them.
    let cfg = Il2CppConfig::fallback_constants();

    let anchors_ready = || -> bool {
        let find = |n: &str| local_find_class(api, table_base, table_capacity, class_table_step, n);
        let int32  = find("System::Int32");
        let string = find("System::String");
        let object = find("System::Object");
        if int32 == 0 || string == 0 || object == 0 { return false; }
        let math = find("System::Math");
        if math == 0 { return false; }
        if local_find_method(&cfg, math, "Pow", 2) == 0 { return false; }
        if local_find_method(&cfg, string, "PadLeft", 2) == 0 { return false; }
        true
    };

    // Live populated count, just for the report line (non-null slots).
    let count_populated = || -> usize {
        let mut n = 0usize;
        for i in 0..table_capacity {
            let slot = table_base.wrapping_add(i * class_table_step);
            let klass = unsafe { std::ptr::read_volatile(slot as *const u64) };
            if klass != 0 { n += 1; }
        }
        n
    };

    let start = Instant::now();
    let mut polls = 0u32;

    let mut anchors_met = false;
    let mut prev_count = 0usize;
    let mut stable_run = 0usize;

    while start.elapsed() < TIMEOUT {
        polls += 1;

        // Phase A: wait for anchor classes to resolve.
        if !anchors_met {
            if anchors_ready() {
                anchors_met = true;
                prev_count = count_populated();
                stable_run = 0;
                log(&format!(
                    "  Phase 0: anchors resolved ({} slots populated), waiting for count stability ({} polls)...",
                    prev_count, min_stable_polls
                ));
            } else {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
        }

        // Phase B: wait for populated-slot count to stabilize.
        let count = count_populated();
        if count == prev_count {
            stable_run += 1;
        } else {
            stable_run = 0;
            prev_count = count;
        }
        if stable_run >= min_stable_polls {
            return StabilityResult::Stable {
                count,
                elapsed_ms: start.elapsed().as_millis() as u64,
                polls,
            };
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    StabilityResult::Timeout {
        last_count: count_populated(),
        elapsed_ms: TIMEOUT.as_millis() as u64,
        anchors_met,
    }
}
