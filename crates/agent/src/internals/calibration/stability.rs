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

use crate::external::cache;
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

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

/// Locate a class by "Namespace::Name" (or bare "Name") via a direct,
/// is_klass_shape-gated walk of the live class table. Self-contained — does NOT
/// depend on `ctx`. Returns the klass ptr, or 0.
fn local_find_class(
    api: &Il2CppApi,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
    name: &str,
) -> usize {
    for i in 0..table_count {
        let slot = table_base.wrapping_add(i * class_table_step);
        let klass = match cache::read_u64(slot) { Some(k) if k != 0 => k as usize, _ => continue };
        if !cache::is_klass_shape(klass) { continue; }
        let cn = unsafe { cstr_to_string((api.class_get_name)(klass as *mut Il2CppClass)) };
        if cn.is_empty() { continue; }
        if cn == name { return klass; }
        let ns = unsafe { cstr_to_string((api.class_get_namespace)(klass as *mut Il2CppClass)) };
        let full = if ns.is_empty() { cn } else { format!("{}::{}", ns, cn) };
        if full == name { return klass; }
    }
    0
}

/// Locate a method by name + arg count on `klass` via a direct walk of the
/// klass's methods array. Self-contained — does NOT depend on `ctx`. Uses the
/// baseline klass/method offsets (the same constants `find_method` falls back
/// to), which is sufficient for the well-known core classes used as anchors.
/// Returns the MethodInfo ptr, or 0.
fn local_find_method(cfg: &Il2CppConfig, klass: usize, name: &str, argc: u32) -> usize {
    let methods = cache::read_u64(klass + cfg.klass_methods).unwrap_or(0) as usize;
    if methods == 0 {
        return 0;
    }
    for i in 0..4096usize {
        let mi = match cache::read_u64(methods + i * 8) {
            Some(v) if v != 0 => v as usize,
            _ => break,
        };
        // Array-end / validity: the MethodInfo's declaring-klass must be this klass.
        if cache::read_u64(mi + cfg.method_klass_off).unwrap_or(0) != klass as u64 {
            break;
        }
        let name_ptr = cache::read_u64(mi + cfg.method_name_off).unwrap_or(0) as usize;
        let mname = match cache::read_cstr(name_ptr) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let pcount = cache::read_u8(mi + cfg.method_param_count_off).unwrap_or(0) as u32;
        if mname == name && pcount == argc {
            return mi;
        }
    }
    0
}

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
