//! Phase 0: poll the class table until the structural anchors the probes need
//! are resolvable.
//!
//! The earlier count-plateau heuristic stabilized too early (e.g. 175 classes at
//! 600ms) because transient burst-gaps during loading look like a plateau. This
//! version polls for a STRUCTURAL signal instead: it walks the live class table
//! by name (same is_klass_shape-gated discipline as `internals::api::find_class`)
//! until the specific anchor classes + methods the probes rely on all resolve.
//!
//! Self-contained: it must NOT use `internals::api::find_class`/`find_method`,
//! because those require `ctx::init` — and ctx is not initialized until AFTER
//! `probe()` runs in entry.rs. So this module walks the table directly using the
//! passed-in `Il2CppApi` and the cache reader, mirroring the find_class logic.

use std::time::{Duration, Instant};

use crate::internals::calibration::anchors::{local_find_class, local_find_method};
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;

#[derive(Debug)]
pub enum StabilityResult {
    Stable   { count: usize, elapsed_ms: u64, polls: u32 },
    Timeout  { last_count: usize, elapsed_ms: u64 },
}

impl StabilityResult {
    pub fn summary(&self) -> String {
        match self {
            StabilityResult::Stable { count, elapsed_ms, polls } => format!(
                "anchors resolvable; {} slots populated after {}ms ({} polls)",
                count, elapsed_ms, polls
            ),
            StabilityResult::Timeout { last_count, elapsed_ms } => format!(
                "TIMEOUT — anchor classes/methods still not resolvable after {}ms (last populated: {})",
                elapsed_ms, last_count
            ),
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const TIMEOUT: Duration = Duration::from_secs(30);

/// Block the calling thread until the class table is STRUCTURALLY ready: the
/// specific anchor classes + methods the probes need must all resolve via the
/// battle-tested, is_klass_shape-gated walk (safe to call during loading).
/// 30s backstop. Timeout is non-fatal (the caller proceeds with degraded
/// probing) but always logs cleanly.
pub fn await_class_table_stable(
    api: &Il2CppApi,
    table_base: usize,
    table_capacity: usize,
    class_table_step: usize,
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
    while start.elapsed() < TIMEOUT {
        polls += 1;
        if anchors_ready() {
            return StabilityResult::Stable {
                count: count_populated(),
                elapsed_ms: start.elapsed().as_millis() as u64,
                polls,
            };
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    StabilityResult::Timeout {
        last_count: count_populated(),
        elapsed_ms: TIMEOUT.as_millis() as u64,
    }
}
