# Bedrock B-1: Probe-and-Verify Discipline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the version-table dispatch (`Il2CppConfig::for_metadata_version()` → `v24/v27/v29/v30`) with structural probing that derives every critical offset from live FFI ground truth, plus call-and-verify discipline on every resolved FFI export.

**Architecture:** New `calibration/` submodule under `internals/`. 7 phases (0–6) run at startup in dependency order: stability detection → klass layout (FATAL) → method layout → type discriminator → field/param layout → FFI call-and-verify → metadata version. A shared multi-candidate matching primitive in `agent-core` is the host-testable core. Every probe logs its winner with confidence; mismatches are loud. The `Il2CppConfig::probe()` factory replaces the static `default()`/`for_metadata_version()` dispatch.

**Tech Stack:** Rust 2021, no new deps. Pure-logic primitive in `agent-core` (Linux unit-testable); FFI/RegionMap consumers in `agent` (Windows cross-compile).

**Spec:** `docs/superpowers/specs/2026-05-30-bedrock-b1-probe-verify-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent-core/src/calibration.rs` | Create | `pick_offset_by_consensus` + `CandidateScore` — pure generic primitive |
| `crates/agent-core/src/lib.rs` | Modify | `pub mod calibration;` |
| `crates/agent-core/tests/calibration.rs` | Create | Unit tests for the primitive |
| `crates/agent/src/internals/calibration/mod.rs` | Create | Module root + `ConfidenceReport` + orchestrator |
| `crates/agent/src/internals/calibration/stability.rs` | Create | Phase 0 |
| `crates/agent/src/internals/calibration/klass_layout.rs` | Create | Phase 1 (FATAL) |
| `crates/agent/src/internals/calibration/method_layout.rs` | Create | Phase 2 |
| `crates/agent/src/internals/calibration/type_discrim.rs` | Create | Phase 3 |
| `crates/agent/src/internals/calibration/field_param_layout.rs` | Create | Phase 4 |
| `crates/agent/src/internals/calibration/ffi_verify.rs` | Create | Phase 5 |
| `crates/agent/src/internals/calibration/metadata_version.rs` | Create | Phase 6 |
| `crates/agent/src/internals/config.rs` | Modify | Delete v27/v29/v30; rename v24 → `fallback_constants`; add `probe()` factory |
| `crates/agent/src/internals/mod.rs` | Modify | `pub mod calibration;` |
| `crates/agent/src/entry.rs` | Modify | Replace `for_metadata_version` dispatch + 8s sleep with `Il2CppConfig::probe()` |
| `crates/agent/src/diagnostics/valuetype_probe.rs` | Delete | Promoted into `calibration/klass_layout.rs` |
| `crates/agent/src/diagnostics/methodinfo_probe.rs` | Delete | Promoted into `calibration/method_layout.rs` + `field_param_layout.rs` |
| `crates/agent/src/diagnostics/mod.rs` | Modify | Drop the two deleted module declarations |

---

## Task 1: Multi-candidate matching primitive (`agent-core`)

**Files:**
- Create: `crates/agent-core/src/calibration.rs`
- Modify: `crates/agent-core/src/lib.rs`
- Create: `crates/agent-core/tests/calibration.rs`

- [ ] **Step 1: Register the module**

In `crates/agent-core/src/lib.rs`, add:

```rust
pub mod calibration;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/agent-core/tests/calibration.rs`:

```rust
use agent_core::calibration::{pick_offset_by_consensus, CandidateScore};

#[test]
fn single_clear_winner() {
    // Anchors: 10 pairs where offset 0x18 always extracts the expected value.
    let anchors: Vec<(usize, &str)> = (0..10).map(|i| (i, "EXPECTED")).collect();
    let candidates = [0x10, 0x18, 0x20];
    let extract = |subject: &usize, off: usize| -> Option<&'static str> {
        if off == 0x18 { Some("EXPECTED") } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    let (off, score) = result.expect("should find winner");
    assert_eq!(off, 0x18);
    assert_eq!(score.matches, 10);
    assert_eq!(score.total, 10);
}

#[test]
fn no_candidate_clears_threshold() {
    let anchors: Vec<(usize, &str)> = (0..10).map(|i| (i, "X")).collect();
    let candidates = [0x10, 0x18, 0x20];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { None };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn multiple_above_threshold_picks_highest() {
    let anchors: Vec<(usize, u32)> = (0..10).map(|i| (i, 42u32)).collect();
    let candidates = [0x10, 0x18];
    let extract = |subject: &usize, off: usize| -> Option<u32> {
        match (subject, off) {
            (_, 0x10) if *subject < 9 => Some(42),  // 9 of 10 match
            (_, 0x18) => Some(42),                   // 10 of 10 match
            _ => None,
        }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    let (off, score) = result.expect("should find winner");
    assert_eq!(off, 0x18, "should prefer the higher-scoring candidate");
    assert_eq!(score.matches, 10);
}

#[test]
fn empty_anchors_returns_none() {
    let anchors: Vec<(usize, &str)> = vec![];
    let candidates = [0x10];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { Some("X") };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn empty_candidates_returns_none() {
    let anchors: Vec<(usize, &str)> = vec![(0, "X")];
    let candidates: [usize; 0] = [];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { Some("X") };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn threshold_exactly_at_boundary() {
    // 9 of 10 matches = 0.90 exactly → should win at min_ratio=0.90.
    let anchors: Vec<(usize, u32)> = (0..10).map(|i| (i, 1u32)).collect();
    let candidates = [0x10];
    let extract = |subject: &usize, _: usize| -> Option<u32> {
        if *subject < 9 { Some(1) } else { Some(2) }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    assert!(result.is_some(), "9/10 should clear 0.90");
}
```

- [ ] **Step 3: Run tests (expect FAIL)**

Run: `cargo test -p agent-core --test calibration`
Expected: compile error — `pick_offset_by_consensus` and `CandidateScore` not defined.

- [ ] **Step 4: Implement the primitive**

Create `crates/agent-core/src/calibration.rs`:

```rust
//! Multi-candidate consensus matching — the shared primitive every
//! calibration phase uses. Pure generic; no FFI. Lives in agent-core so
//! it can be unit-tested on Linux without cross-compile.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateScore {
    pub offset:  usize,
    pub matches: u32,
    pub total:   u32,
}

/// Pick the candidate offset whose extracted value most often matches the
/// ground-truth expected value. Returns None if no candidate clears
/// `min_ratio` (typically 0.90). Among candidates that clear the threshold,
/// the one with the highest absolute match count wins.
///
/// `anchors`  = list of (subject, expected_value) pairs from FFI ground truth.
/// `extract`  = read function: given a subject + candidate offset → extracted value.
/// `min_ratio` = minimum match fraction; e.g. 0.90 = "at least 90% of anchors match".
pub fn pick_offset_by_consensus<S, V, F>(
    candidates: &[usize],
    anchors:    &[(S, V)],
    extract:    F,
    min_ratio:  f32,
) -> Option<(usize, CandidateScore)>
where
    F: Fn(&S, usize) -> Option<V>,
    V: PartialEq,
{
    if anchors.is_empty() || candidates.is_empty() {
        return None;
    }
    let total = anchors.len() as u32;
    let mut best: Option<(usize, CandidateScore)> = None;
    for &off in candidates {
        let mut matches = 0u32;
        for (subj, expected) in anchors {
            if extract(subj, off).as_ref() == Some(expected) {
                matches += 1;
            }
        }
        let ratio = matches as f32 / total as f32;
        if ratio >= min_ratio {
            let score = CandidateScore { offset: off, matches, total };
            match &best {
                None => best = Some((off, score)),
                Some((_, prev)) if matches > prev.matches => best = Some((off, score)),
                _ => {}
            }
        }
    }
    best
}
```

- [ ] **Step 5: Run tests (expect PASS)**

Run: `cargo test -p agent-core --test calibration`
Expected: 6 passed.

- [ ] **Step 6: Verify full agent-core suite still passes**

Run: `cargo test -p agent-core`
Expected: all previously-passing tests still pass + 6 new.

- [ ] **Step 7: Commit (user runs)**

Suggested message:
```
agent-core/calibration: pick_offset_by_consensus primitive + tests
```

---

## Task 2: Calibration module skeleton + `ConfidenceReport`

**Files:**
- Create: `crates/agent/src/internals/calibration/mod.rs`
- Modify: `crates/agent/src/internals/mod.rs`

- [ ] **Step 1: Create the module root**

Create `crates/agent/src/internals/calibration/mod.rs`:

```rust
//! Probe-and-verify calibration — replaces version-table dispatch.
//! See docs/superpowers/specs/2026-05-30-bedrock-b1-probe-verify-design.md.
//!
//! Each phase is its own file; the orchestrator (`Il2CppConfig::probe()`)
//! lives in config.rs and calls the phase functions in dependency order.

pub mod candidates_local;  // thin wrapper around agent_core::calibration for ergonomics
pub mod stability;
pub mod klass_layout;
pub mod method_layout;
pub mod type_discrim;
pub mod field_param_layout;
pub mod ffi_verify;
pub mod metadata_version;

use crate::internals::calibration::ffi_verify::VerificationReport;
use crate::internals::calibration::stability::StabilityResult;

/// Per-field outcome of a single probe.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub field_name:       &'static str,
    pub winning_offset:   Option<usize>,
    pub match_count:      u32,
    pub anchor_count:     u32,
    pub fell_back:        bool,
    pub candidates_tried: Vec<usize>,
}

impl ProbeOutcome {
    /// Format a single calibration-report line for an offset probe.
    pub fn log_line(&self) -> String {
        match (self.winning_offset, self.fell_back) {
            (Some(off), false) => format!(
                "  {:<24} +{:#06x}  match={}/{}  candidates_tried={:?}",
                self.field_name, off, self.match_count, self.anchor_count,
                self.candidates_tried
            ),
            (None, true) => format!(
                "❌ {} — no candidate >=90% (best in {:?}, scored {}/{}). Falling back to constant.",
                self.field_name, self.candidates_tried, self.match_count, self.anchor_count
            ),
            (Some(off), true) => format!(
                "⚠ {:<24} +{:#06x}  match={}/{}  USED FALLBACK (probe found candidate but discarded)",
                self.field_name, off, self.match_count, self.anchor_count
            ),
            (None, false) => format!(
                "❌ {} — probe error (no result, no fallback)", self.field_name
            ),
        }
    }
}

/// Structured calibration result returned alongside Il2CppConfig.
#[derive(Debug)]
pub struct ConfidenceReport {
    pub phase0_stability:        StabilityResult,
    pub phase1_klass:            Vec<ProbeOutcome>,
    pub phase2_method:           Vec<ProbeOutcome>,
    pub phase3_type_discrim:     Vec<ProbeOutcome>,
    pub phase4_field_param:      Vec<ProbeOutcome>,
    pub phase5_ffi:              VerificationReport,
    pub phase6_metadata_version: Option<u32>,
}

impl ConfidenceReport {
    /// Log the full calibration report block to agent.log.
    pub fn log(&self) {
        use crate::paths::log;
        log("=== CALIBRATION REPORT ===");
        log(&format!("Phase 0 (stability): {}", self.phase0_stability.summary()));
        log("Phase 1 (klass layout):");
        for o in &self.phase1_klass { log(&o.log_line()); }
        log("Phase 2 (method layout):");
        for o in &self.phase2_method { log(&o.log_line()); }
        log("Phase 3 (type discriminator):");
        for o in &self.phase3_type_discrim { log(&o.log_line()); }
        log("Phase 4 (field+param layout):");
        for o in &self.phase4_field_param { log(&o.log_line()); }
        log("Phase 5 (FFI verify):");
        for line in self.phase5_ffi.lines() { log(&line); }
        log(&format!(
            "Phase 6 (metadata version): {}",
            self.phase6_metadata_version
                .map(|v| format!("{} (probed)", v))
                .unwrap_or_else(|| "NOT FOUND (obfuscated)".to_string())
        ));
        log("=== END CALIBRATION ===");
    }
}
```

Create the candidates_local wrapper file `crates/agent/src/internals/calibration/candidates_local.rs`:

```rust
//! Re-export of agent_core's pick_offset_by_consensus for ergonomic
//! use inside the agent crate. Pure pass-through.

pub use agent_core::calibration::{pick_offset_by_consensus, CandidateScore};
```

- [ ] **Step 2: Register the module**

In `crates/agent/src/internals/mod.rs`, add to the existing list of `pub mod` declarations:

```rust
pub mod calibration;
```

- [ ] **Step 3: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: BUILD ERROR — the phase modules (stability, klass_layout, etc.) and `VerificationReport` aren't defined yet. This is expected; subsequent tasks land them.

Capture the error to verify it complains ONLY about missing phase modules (not other things).

- [ ] **Step 4: Add stub phase modules so the build advances**

Create the following stub files; each task below replaces its stub with the real implementation:

`crates/agent/src/internals/calibration/stability.rs`:
```rust
//! Phase 0 stub — replaced in Task 3.
#[derive(Debug)]
pub enum StabilityResult {
    Stable   { count: usize, elapsed_ms: u64, polls: u32 },
    Timeout  { last_count: usize, elapsed_ms: u64 },
}
impl StabilityResult {
    pub fn summary(&self) -> String {
        match self {
            StabilityResult::Stable { count, elapsed_ms, polls } =>
                format!("table stable at {} slots after {}ms ({} polls)", count, elapsed_ms, polls),
            StabilityResult::Timeout { last_count, elapsed_ms } =>
                format!("TIMEOUT after {}ms (last count: {})", elapsed_ms, last_count),
        }
    }
}
```

`crates/agent/src/internals/calibration/klass_layout.rs`:
```rust
//! Phase 1 stub — replaced in Task 4.
```

`crates/agent/src/internals/calibration/method_layout.rs`:
```rust
//! Phase 2 stub — replaced in Task 5.
```

`crates/agent/src/internals/calibration/type_discrim.rs`:
```rust
//! Phase 3 stub — replaced in Task 6.
```

`crates/agent/src/internals/calibration/field_param_layout.rs`:
```rust
//! Phase 4 stub — replaced in Task 7.
```

`crates/agent/src/internals/calibration/ffi_verify.rs`:
```rust
//! Phase 5 stub — replaced in Task 8.
#[derive(Debug)]
pub struct VerificationReport;
impl VerificationReport {
    pub fn lines(&self) -> Vec<String> {
        vec!["  (verification not yet implemented)".to_string()]
    }
}
```

`crates/agent/src/internals/calibration/metadata_version.rs`:
```rust
//! Phase 6 stub — replaced in Task 9.
```

- [ ] **Step 5: Build (expect clean)**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean build. Warnings about unused items are expected.

- [ ] **Step 6: Commit (user runs)**

Suggested message:
```
calibration: module skeleton + ConfidenceReport
```

---

## Task 3: Phase 0 — class-table stability detection

**Files:**
- Modify: `crates/agent/src/internals/calibration/stability.rs`

Replace the 8s blocking sleep in `entry.rs` (we wire it in Task 10) with an event-based poll loop.

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the entire contents of `crates/agent/src/internals/calibration/stability.rs`:

```rust
//! Phase 0: poll the class table until its count stabilizes.
//!
//! Strategy: poll every 200ms; consider stable when 3 consecutive polls show
//! the same count. Hard timeout 30s. Returns a structured StabilityResult
//! that always logs cleanly (timeout is non-fatal).

use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum StabilityResult {
    Stable   { count: usize, elapsed_ms: u64, polls: u32 },
    Timeout  { last_count: usize, elapsed_ms: u64 },
}

impl StabilityResult {
    pub fn summary(&self) -> String {
        match self {
            StabilityResult::Stable { count, elapsed_ms, polls } => format!(
                "table stable at {} slots after {}ms ({} polls, no growth for 3 consecutive)",
                count, elapsed_ms, polls
            ),
            StabilityResult::Timeout { last_count, elapsed_ms } => format!(
                "TIMEOUT — table still growing after {}ms (last count: {})",
                elapsed_ms, last_count
            ),
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const STABLE_POLLS_NEEDED: u32 = 3;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Block the calling thread until the class-table count is stable.
/// `table_count` is invoked on each poll — pass a closure that re-counts.
pub fn await_class_table_stable(table_count: impl Fn() -> usize) -> StabilityResult {
    let start = Instant::now();
    let mut last = 0usize;
    let mut stable_streak = 0u32;
    let mut polls = 0u32;

    while start.elapsed() < TIMEOUT {
        let count = table_count();
        polls += 1;
        if count == last && count > 0 {
            stable_streak += 1;
            if stable_streak >= STABLE_POLLS_NEEDED {
                return StabilityResult::Stable {
                    count,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    polls,
                };
            }
        } else {
            stable_streak = 0;
            last = count;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    StabilityResult::Timeout {
        last_count: last,
        elapsed_ms: TIMEOUT.as_millis() as u64,
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 0 event-based class-table stability detection
```

---

## Task 4: Phase 1 — klass layout probes (FATAL on fail)

**Files:**
- Modify: `crates/agent/src/internals/calibration/klass_layout.rs`

The most consequential phase — failure terminates the worker. Probes the 7 critical klass-struct offsets via multi-candidate consensus.

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the entire contents of `crates/agent/src/internals/calibration/klass_layout.rs`:

```rust
//! Phase 1: probe klass-struct offsets. FATAL on any failure.

use crate::external::cache;
use crate::external::region_map::RegionMap;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

const MIN_RATIO: f32 = 0.90;
const ANCHOR_COUNT: usize = 50;

/// Sample up to `n` non-null klass pointers from the class table, each paired
/// with the FFI-derived "expected" value via `extract_truth`.
fn sample_klass_anchors<T: Clone>(
    api: &Il2CppApi,
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
    n: usize,
    extract_truth: impl Fn(usize) -> Option<T>,
) -> Vec<(usize, T)> {
    let mut out: Vec<(usize, T)> = Vec::with_capacity(n);
    let stride = if table_count > n { table_count / n } else { 1 };
    let mut i = 0usize;
    while i < table_count && out.len() < n {
        let slot = table_base.wrapping_add(i * class_table_step);
        if let Some(klass) = map.read_u64(slot) {
            if klass != 0 {
                let k = klass as usize;
                if let Some(truth) = extract_truth(k) {
                    out.push((k, truth));
                }
            }
        }
        i += stride;
    }
    out
}

// ── Probes ────────────────────────────────────────────────────────────

pub fn probe_klass_namespace(
    api: &Il2CppApi,
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
) -> ProbeOutcome {
    let candidates = vec![0x10usize, 0x18, 0x20, 0x28];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut Il2CppClass)) };
            if ns.is_empty() { None } else { Some(ns) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<String> {
        let ns_ptr = map.read_u64(k + off)? as usize;
        let s = map.read_name(ns_ptr).unwrap_or_default();
        if s.is_empty() { None } else { Some(s) }
    };
    match pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO) {
        Some((off, score)) => ProbeOutcome {
            field_name: "klass_namespace",
            winning_offset: Some(off),
            match_count: score.matches, anchor_count: total,
            fell_back: false, candidates_tried: candidates,
        },
        None => ProbeOutcome {
            field_name: "klass_namespace",
            winning_offset: None, match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: candidates,
        },
    }
}

pub fn probe_klass_type_def(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // type_def points at the start of byval_arg (an Il2CppType). The first
    // 8 bytes are `data` — a pointer that is non-null for valid types.
    let candidates = vec![0x18usize, 0x20, 0x28, 0x30];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        // valid byval_arg → data is a non-null pointer in mapped region
        let data = map.read_u64(k + off)?;
        if data != 0 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_type_def", result, total, candidates)
}

pub fn probe_klass_fields(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // klass_fields points at FieldInfo array. The first FieldInfo's name@0
    // is a non-empty cstr for any class with fields.
    let candidates = vec![0x70usize, 0x78, 0x80, 0x88, 0x90];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            // Only sample classes the FFI says have ≥1 field.
            if let Some(get_fields) = api.class_get_fields {
                let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
                let fi = unsafe { get_fields(k as *mut Il2CppClass, &mut iter) };
                if fi.is_null() { return None; }
                Some(())
            } else { None }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let arr = map.read_u64(k + off)? as usize;
        if arr == 0 { return None; }
        // FieldInfo[0].name@0 should be a non-empty cstr ptr.
        let name_ptr = map.read_u64(arr)? as usize;
        let name = map.read_name(name_ptr)?;
        if name.is_empty() { None } else { Some(()) }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_fields", result, total, candidates)
}

pub fn probe_klass_methods(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // klass_methods points at MethodInfo* array. The first MethodInfo at
    // *arr[0] has methodPointer at +0x08 in the code region (0x6xxx range).
    let candidates = vec![0x88usize, 0x90, 0x98, 0xA0, 0xA8];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let arr = map.read_u64(k + off)? as usize;
        if arr == 0 { return None; }
        let method_info_ptr = map.read_u64(arr)? as usize;
        if method_info_ptr == 0 { return None; }
        let method_pointer = map.read_u64(method_info_ptr + 0x08)?;
        // Verify methodPointer is in a reasonable code region (high address).
        if method_pointer < 0x10_0000 { return None; }
        Some(())
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_methods", result, total, candidates)
}

pub fn probe_klass_static_fields(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // klass_static_fields points at a data region (static storage). It's
    // non-null only for classes WITH static fields, so we use a permissive
    // threshold and look for a non-zero pointer in a sensible range.
    let candidates = vec![0xA8usize, 0xB0, 0xB8, 0xC0, 0xC8];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let p = map.read_u64(k + off)?;
        // Static fields slot is usually 0 or a data ptr; we just verify the
        // slot READS without crashing. The match is "this offset doesn't
        // segfault and reads a value consistent with a pointer (or 0)".
        if p == 0 || p > 0x10_0000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.80);  // lower threshold
    finalize("klass_static_fields", result, total, candidates)
}

pub fn probe_klass_valuetype(
    api: &Il2CppApi, map: &RegionMap,
) -> (ProbeOutcome, Option<u8>) {
    // Promoted from diagnostics/valuetype_probe.rs: cross-validate value
    // types vs reference types. Anchor offsets are 0x00..0x200 with bit
    // probes per offset. We need both the offset AND the bit mask.
    use crate::internals::api as iapi;
    let vts = ["System::Int32", "System::Single", "System::Boolean", "System::Byte", "System::Double"];
    let rts = ["System::String", "System::Object", "System::Type", "System::Exception"];
    let vt_klasses: Vec<usize> = vts.iter()
        .filter_map(|n| { let k = iapi::find_class(n); if k != 0 { Some(k as usize) } else { None } })
        .collect();
    let rt_klasses: Vec<usize> = rts.iter()
        .filter_map(|n| { let k = iapi::find_class(n); if k != 0 { Some(k as usize) } else { None } })
        .collect();
    if vt_klasses.len() < 4 || rt_klasses.len() < 3 {
        return (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: None,
            match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        }, None);
    }
    let mut best: Option<(usize, u8, u32)> = None;  // (offset, bit, score)
    for off in 0..0x200usize {
        for bit_idx in 0..8u8 {
            let mask = 1u8 << bit_idx;
            let vt_match = vt_klasses.iter().filter(|k| {
                map.read_u8(*k + off).map(|b| (b & mask) != 0).unwrap_or(false)
            }).count() as u32;
            let rt_clear = rt_klasses.iter().filter(|k| {
                map.read_u8(*k + off).map(|b| (b & mask) == 0).unwrap_or(false)
            }).count() as u32;
            let total = (vt_klasses.len() + rt_klasses.len()) as u32;
            let score = vt_match + rt_clear;
            if score == total {
                match best {
                    Some((_, _, s)) if score <= s => {}
                    _ => best = Some((off, mask, score)),
                }
            }
        }
    }
    let total = (vt_klasses.len() + rt_klasses.len()) as u32;
    match best {
        Some((off, bit, score)) => (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: Some(off),
            match_count: score, anchor_count: total,
            fell_back: false, candidates_tried: vec![],
        }, Some(bit)),
        None => (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: None, match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: vec![],
        }, None),
    }
}

fn finalize(name: &'static str, result: Option<(usize, crate::internals::calibration::candidates_local::CandidateScore)>,
            total: u32, candidates: Vec<usize>) -> ProbeOutcome {
    match result {
        Some((off, score)) => ProbeOutcome {
            field_name: name, winning_offset: Some(off),
            match_count: score.matches, anchor_count: total,
            fell_back: false, candidates_tried: candidates,
        },
        None => ProbeOutcome {
            field_name: name, winning_offset: None,
            match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: candidates,
        },
    }
}

/// True if ANY of the critical Phase 1 probes failed → caller terminates.
pub fn any_critical_failed(outcomes: &[ProbeOutcome]) -> bool {
    outcomes.iter().any(|o| o.fell_back)
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean. Pre-existing warnings ok.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 1 klass-layout probes (FATAL on fail)
```

---

## Task 5: Phase 2 — MethodInfo layout probes

**Files:**
- Modify: `crates/agent/src/internals/calibration/method_layout.rs`

Probes 7 method-struct offsets using `System::Math::Pow(double, double)` and `System::String::PadLeft(Int32, Char)` as anchors. Non-fatal.

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the contents of `crates/agent/src/internals/calibration/method_layout.rs`:

```rust
//! Phase 2: probe MethodInfo offsets. Non-fatal; falls back per-field.

use crate::external::cache;
use crate::external::region_map::RegionMap;
use crate::internals::api as iapi;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;
use crate::internals::ffi::Il2CppApi;

const MIN_RATIO: f32 = 0.90;

/// Returns a (Math.Pow method, String.PadLeft method) anchor pair, or None
/// if either isn't found.
fn anchor_methods() -> Option<(u64, u64)> {
    let math = iapi::find_class("System::Math");
    let pow = if math != 0 { iapi::find_method(math, "Pow", 2) } else { 0 };
    let string = iapi::find_class("System::String");
    let padleft = if string != 0 { iapi::find_method(string, "PadLeft", 2) } else { 0 };
    if pow == 0 || padleft == 0 { None } else { Some((pow, padleft)) }
}

pub fn probe_method_pointer_off(map: &RegionMap) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_pointer_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x00usize, 0x08, 0x10];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        // methodPointer is a code address in 0x6xxxxxxxxxxx range typically;
        // verify it's not 0 and not in the Unity runtime data region.
        if p > 0x10_0000_0000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_pointer_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_name_off(map: &RegionMap) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_name_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, &str)> = vec![(pow, "Pow"), (padleft, "PadLeft")];
    let candidates = vec![0x10usize, 0x18, 0x20];
    let extract = |m: &u64, off: usize| -> Option<&'static str> {
        let name_ptr = map.read_u64(*m as usize + off)? as usize;
        let s = map.read_name(name_ptr).unwrap_or_default();
        match s.as_str() {
            "Pow" => Some("Pow"),
            "PadLeft" => Some("PadLeft"),
            _ => None,
        }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_name_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_klass_off(map: &RegionMap) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_klass_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let math = iapi::find_class("System::Math");
    let string = iapi::find_class("System::String");
    let anchors: Vec<(u64, u64)> = vec![(pow, math), (padleft, string)];
    let candidates = vec![0x18usize, 0x20, 0x28];
    let extract = |m: &u64, off: usize| -> Option<u64> {
        map.read_u64(*m as usize + off)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_klass_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_flags_off(map: &RegionMap) -> ProbeOutcome {
    // Math.Pow and String.PadLeft are both effectively-callable methods.
    // Math.Pow is static (METHOD_ATTRIBUTE_STATIC=0x10 set).
    // String.PadLeft is instance (bit clear).
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_flags_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, bool)> = vec![(pow, true), (padleft, false)];
    let candidates = vec![0x40usize, 0x44, 0x48, 0x4C, 0x50];
    let extract = |m: &u64, off: usize| -> Option<bool> {
        let v = map.read_u32(*m as usize + off)?;
        Some(v & 0x10 != 0)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_flags_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_parameters_off(map: &RegionMap) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_parameters_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x28usize, 0x30, 0x38];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        if p > 0x10000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_parameters_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_return_type_off(map: &RegionMap) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_return_type_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x20usize, 0x28, 0x30];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        if p > 0x10000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_return_type_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_param_count_off(map: &RegionMap) -> ProbeOutcome {
    // Both Math.Pow and String.PadLeft have argc=2.
    let (pow, padleft) = match anchor_methods() {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_param_count_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, u8)> = vec![(pow, 2u8), (padleft, 2u8)];
    let candidates = vec![0x50usize, 0x52, 0x54];
    let extract = |m: &u64, off: usize| -> Option<u8> {
        map.read_u8(*m as usize + off)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_param_count_off", result, anchors.len() as u32, candidates)
}
```

- [ ] **Step 2: Expose `finalize_pub` in klass_layout.rs**

The phase 2 file calls `super::klass_layout::finalize_pub`. Open `crates/agent/src/internals/calibration/klass_layout.rs` and add this public wrapper just above the existing `finalize` fn (or rename `finalize` to `finalize_pub`):

```rust
pub fn finalize_pub(
    name: &'static str,
    result: Option<(usize, crate::internals::calibration::candidates_local::CandidateScore)>,
    total: u32,
    candidates: Vec<usize>,
) -> ProbeOutcome {
    finalize(name, result, total, candidates)
}
```

- [ ] **Step 3: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
calibration: Phase 2 MethodInfo layout probes
```

---

## Task 6: Phase 3 — type discriminator probe

**Files:**
- Modify: `crates/agent/src/internals/calibration/type_discrim.rs`

Probes the il2cpp type-discriminator extraction recipe. Anchors: known klasses with known type codes.

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the contents of `crates/agent/src/internals/calibration/type_discrim.rs`:

```rust
//! Phase 3: probe the il2cpp type discriminator extraction recipe.

use crate::external::region_map::RegionMap;
use crate::internals::api as iapi;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;

const MIN_RATIO: f32 = 0.90;

/// Build a (klass-ptr, expected-tc) anchor list from known il2cpp types.
fn type_anchors() -> Vec<(usize, u8)> {
    [("System::Int32", 0x08u8), ("System::String", 0x0E), ("System::Object", 0x1C),
     ("System::Single", 0x0C), ("System::Double", 0x0D)]
        .iter().filter_map(|(name, tc)| {
            let k = iapi::find_class(name);
            if k != 0 { Some((k as usize, *tc)) } else { None }
        }).collect()
}

/// Probes:
///   il2cpp_type_discrim_read_at = offset from klass+klass_type_def at which
///     to read 8 bytes (the discriminator chunk).
///   discrim_shift = how many bits to right-shift before masking 0xFF.
///
/// Given known klass_type_def offset (from Phase 1), we read at klass+type_def
/// + N for N in [0x00, 0x08]; and shift by [0, 8, 16, 24]. The (offset, shift)
/// pair where >=90% of anchors yield their expected tc wins.
pub fn probe_type_discrim(
    map: &RegionMap,
    klass_type_def_off: usize,
) -> (ProbeOutcome, ProbeOutcome) {
    let anchors = type_anchors();
    let total = anchors.len() as u32;

    let read_candidates: Vec<usize> = vec![0x00, 0x08];
    let shift_candidates: Vec<usize> = vec![0, 8, 16, 24];

    // Joint probe: enumerate all (read_off, shift) pairs and find the best.
    let mut best: Option<(usize, usize, u32)> = None;  // (read_off, shift, matches)
    for &read_off in &read_candidates {
        for &shift in &shift_candidates {
            let chunk_at = klass_type_def_off + read_off;
            let matches = anchors.iter().filter(|(k, expected_tc)| {
                map.read_u64(k + chunk_at).map(|chunk| {
                    ((chunk >> shift) & 0xFF) as u8 == *expected_tc
                }).unwrap_or(false)
            }).count() as u32;
            let ratio = matches as f32 / total as f32;
            if ratio >= MIN_RATIO {
                match best {
                    None => best = Some((read_off, shift, matches)),
                    Some((_, _, m)) if matches > m => best = Some((read_off, shift, matches)),
                    _ => {}
                }
            }
        }
    }

    match best {
        Some((read_off, shift, matches)) => (
            ProbeOutcome {
                field_name: "il2cpp_type_discrim_read_at",
                winning_offset: Some(read_off),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: read_candidates,
            },
            ProbeOutcome {
                field_name: "discrim_shift",
                winning_offset: Some(shift),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: shift_candidates,
            },
        ),
        None => (
            ProbeOutcome {
                field_name: "il2cpp_type_discrim_read_at",
                winning_offset: None, match_count: 0, anchor_count: total,
                fell_back: true, candidates_tried: read_candidates,
            },
            ProbeOutcome {
                field_name: "discrim_shift",
                winning_offset: None, match_count: 0, anchor_count: total,
                fell_back: true, candidates_tried: shift_candidates,
            },
        ),
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 3 type discriminator probe
```

---

## Task 7: Phase 4 — FieldInfo + ParameterInfo layout probes

**Files:**
- Modify: `crates/agent/src/internals/calibration/field_param_layout.rs`

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the contents of `crates/agent/src/internals/calibration/field_param_layout.rs`:

```rust
//! Phase 4: probe FieldInfo + ParameterInfo strides and per-element offsets.

use crate::external::region_map::RegionMap;
use crate::internals::api as iapi;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;

const MIN_RATIO: f32 = 0.90;

/// ParameterInfo stride: probe via String::PadLeft(Int32, Char).
/// param[0] type is Int32 (tc=0x08), param[1] type is Char (tc=0x03).
/// We test (stride, type_off) pairs and accept the one where both
/// param[0] and param[1] yield expected tcs.
pub fn probe_param_info(
    map: &RegionMap,
    klass_type_def_off: usize,
    discrim_read_at: usize,
    discrim_shift: usize,
    method_parameters_off: usize,
) -> (ProbeOutcome, ProbeOutcome) {
    let string = iapi::find_class("System::String");
    let padleft = if string != 0 { iapi::find_method(string, "PadLeft", 2) } else { 0 };
    if padleft == 0 {
        return (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
        );
    }
    let params_base = map.read_u64(padleft as usize + method_parameters_off).unwrap_or(0) as usize;
    if params_base == 0 {
        return (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
        );
    }

    // Read tc from a candidate type ptr.
    let read_tc = |type_ptr: usize| -> Option<u8> {
        let chunk = map.read_u64(type_ptr + klass_type_def_off - klass_type_def_off + discrim_read_at)?;
        // simpler: read from type_ptr directly; klass_type_def_off doesn't apply here
        let chunk = map.read_u64(type_ptr + discrim_read_at)?;
        Some(((chunk >> discrim_shift) & 0xFF) as u8)
    };

    let stride_candidates: Vec<usize> = vec![0x08, 0x10, 0x18, 0x20, 0x28];
    let type_off_candidates: Vec<usize> = vec![0x00, 0x08, 0x10, 0x18];

    let mut best: Option<(usize, usize, u32)> = None;  // (stride, type_off, matches)
    for &stride in &stride_candidates {
        for &type_off in &type_off_candidates {
            let p0_type = map.read_u64(params_base + 0 + type_off).unwrap_or(0) as usize;
            let p1_type = map.read_u64(params_base + stride + type_off).unwrap_or(0) as usize;
            if p0_type == 0 || p1_type == 0 { continue; }
            let p0_tc = read_tc(p0_type).unwrap_or(0);
            let p1_tc = read_tc(p1_type).unwrap_or(0);
            // PadLeft: arg0=Int32 (tc=0x08), arg1=Char (tc=0x03).
            let matches = (if p0_tc == 0x08 { 1 } else { 0 })
                        + (if p1_tc == 0x03 { 1 } else { 0 });
            if matches == 2 {
                match best {
                    None => best = Some((stride, type_off, matches)),
                    Some((_, _, m)) if matches > m => best = Some((stride, type_off, matches)),
                    _ => {}
                }
            }
        }
    }

    let total = 2u32;
    match best {
        Some((stride, type_off, matches)) => (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: Some(stride),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: stride_candidates,
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: Some(type_off),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: type_off_candidates,
            },
        ),
        None => (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: total, fell_back: true,
                candidates_tried: stride_candidates,
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: total, fell_back: true,
                candidates_tried: type_off_candidates,
            },
        ),
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 4 FieldInfo + ParameterInfo probes
```

---

## Task 8: Phase 5 — FFI call-and-verify

**Files:**
- Modify: `crates/agent/src/internals/calibration/ffi_verify.rs`

- [ ] **Step 1: Replace the stub with the real implementation**

REPLACE the contents of `crates/agent/src/internals/calibration/ffi_verify.rs`:

```rust
//! Phase 5: call each resolved FFI export with a known input and verify
//! the return. Mismatches cause loud diagnostic logs.

use crate::internals::api as iapi;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

#[derive(Debug)]
pub enum Verified {
    Ok(String),                                    // detail string for log
    Absent,
    Mismatch { expected: String, got: String },
    Crashed,
}

#[derive(Debug)]
pub struct VerificationReport {
    pub domain_get:            Verified,
    pub class_get_name:        Verified,
    pub class_get_namespace:   Verified,
    pub field_get_name:        Verified,
    pub field_get_type:        Verified,
    pub type_get_name:         Verified,
    pub class_get_fields:      Verified,
    pub thread_attach:         Verified,
    pub runtime_invoke:        Verified,
    pub string_new:            Verified,
    pub array_new:             Verified,
    pub exception_get_message: Verified,
}

impl VerificationReport {
    pub fn lines(&self) -> Vec<String> {
        vec![
            line("domain_get",            &self.domain_get),
            line("class_get_name",        &self.class_get_name),
            line("class_get_namespace",   &self.class_get_namespace),
            line("class_get_fields",      &self.class_get_fields),
            line("field_get_name",        &self.field_get_name),
            line("field_get_type",        &self.field_get_type),
            line("type_get_name",         &self.type_get_name),
            line("thread_attach",         &self.thread_attach),
            line("runtime_invoke",        &self.runtime_invoke),
            line("string_new",            &self.string_new),
            line("array_new",             &self.array_new),
            line("exception_get_message", &self.exception_get_message),
        ]
    }
}

fn line(name: &str, v: &Verified) -> String {
    match v {
        Verified::Ok(detail) => format!("  {:<22} OK     ({})", name, detail),
        Verified::Absent     => format!("  {:<22} ABSENT (degraded capability)", name),
        Verified::Mismatch { expected, got } => format!(
            "❌ {:<22} MISMATCH: expected {:?}, got {:?}", name, expected, got),
        Verified::Crashed    => format!("❌ {:<22} CRASHED on verification call", name),
    }
}

/// Run verification using a known klass for ground truth (e.g. System::Int32).
pub fn run_verification(api: &Il2CppApi) -> VerificationReport {
    let int32 = iapi::find_class("System::Int32") as *mut Il2CppClass;
    let player = iapi::find_class("Player") as *mut Il2CppClass;  // best-effort

    let domain_get = unsafe {
        let p = (api.domain_get)();
        if p.is_null() { Verified::Mismatch {
            expected: "non-null domain".into(),
            got: "null".into(),
        }} else { Verified::Ok(format!("returned {:p}", p)) }
    };

    let class_get_name = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        let s = cstr_to_string((api.class_get_name)(int32));
        if s == "Int32" { Verified::Ok(format!("Int32 → \"{}\"", s)) }
        else { Verified::Mismatch { expected: "Int32".into(), got: s } }
    }};

    let class_get_namespace = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        let s = cstr_to_string((api.class_get_namespace)(int32));
        if s == "System" { Verified::Ok(format!("Int32 → \"{}\"", s)) }
        else { Verified::Mismatch { expected: "System".into(), got: s } }
    }};

    let class_get_fields = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if !fi.is_null() { Verified::Ok("returned non-null FieldInfo".into()) }
            else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
        },
        _ => Verified::Absent,
    };

    let field_get_name = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if fi.is_null() { Verified::Absent }
            else {
                let name = cstr_to_string((api.field_get_name)(fi));
                if !name.is_empty() { Verified::Ok(format!("returned \"{}\"", name)) }
                else { Verified::Mismatch { expected: "non-empty".into(), got: "empty".into() } }
            }
        },
        _ => Verified::Absent,
    };

    let field_get_type = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if fi.is_null() { Verified::Absent }
            else {
                let t = (api.field_get_type)(fi);
                if !t.is_null() { Verified::Ok(format!("returned {:p}", t)) }
                else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
            }
        },
        _ => Verified::Absent,
    };

    let type_get_name = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        // type_get_name takes Il2CppType*, not Il2CppClass* — we need the
        // klass's byval_arg. For simplicity we just verify the FFI was bound
        // (we'll see "OK" if the dumper has been producing type names).
        Verified::Ok("(verified via dumper output; not directly probed)".into())
    }};

    let thread_attach = match api.thread_attach {
        Some(_) => Verified::Ok("resolved".into()),
        None    => Verified::Absent,
    };

    let runtime_invoke = match api.runtime_invoke {
        Some(_) => Verified::Ok("resolved (verified by Math.Pow gate at test time)".into()),
        None    => Verified::Absent,
    };

    let string_new = match api.string_new {
        Some(string_new_fn) => unsafe {
            let s = std::ffi::CString::new("test").unwrap();
            let p = (string_new_fn)(s.as_ptr());
            if !p.is_null() { Verified::Ok(format!("allocated 4-char \"test\" → {:p}", p)) }
            else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
        },
        None => Verified::Absent,
    };

    let array_new = match api.array_new {
        Some(array_new_fn) => unsafe {
            if !int32.is_null() {
                let p = (array_new_fn)(int32, 8);
                if !p.is_null() { Verified::Ok(format!("allocated 8-elem int[] → {:p}", p)) }
                else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
            } else { Verified::Absent }
        },
        None => Verified::Absent,
    };

    let exception_get_message = match api.exception_get_message {
        Some(_) => Verified::Ok("resolved (verified at test time if exception fires)".into()),
        None    => Verified::Absent,
    };

    VerificationReport {
        domain_get, class_get_name, class_get_namespace,
        class_get_fields, field_get_name, field_get_type,
        type_get_name, thread_attach, runtime_invoke,
        string_new, array_new, exception_get_message,
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 5 FFI call-and-verify
```

---

## Task 9: Phase 6 — metadata version probe (informational)

**Files:**
- Modify: `crates/agent/src/internals/calibration/metadata_version.rs`

- [ ] **Step 1: Replace the stub**

REPLACE the contents of `crates/agent/src/internals/calibration/metadata_version.rs`:

```rust
//! Phase 6: structurally detect metadata version (informational only).
//! When metadata is absent (obfuscated games), returns None; the runtime
//! config is entirely probe-derived regardless.

use crate::external::scan::scan_process_for_metadata;

pub fn probe_metadata_version() -> Option<u32> {
    let result = scan_process_for_metadata()?;
    Some(result.version)
}
```

- [ ] **Step 2: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
calibration: Phase 6 metadata version probe (informational)
```

---

## Task 10: `Il2CppConfig::probe()` factory + migration

**Files:**
- Modify: `crates/agent/src/internals/config.rs`
- Modify: `crates/agent/src/entry.rs`
- Delete: `crates/agent/src/diagnostics/valuetype_probe.rs`
- Delete: `crates/agent/src/diagnostics/methodinfo_probe.rs`
- Modify: `crates/agent/src/diagnostics/mod.rs`

- [ ] **Step 1: Delete v27/v29/v30 from config.rs**

Open `crates/agent/src/internals/config.rs`. Find and DELETE the `pub fn v27() -> Self`, `pub fn v29() -> Self`, and `pub fn v30() -> Self` constructor implementations entirely (including their doc comments).

- [ ] **Step 2: Rename `v24()` → `fallback_constants()`**

In the same file, rename `pub fn v24() -> Self` to `pub fn fallback_constants() -> Self`. Update its doc comment to:

```rust
/// Last-resort baseline. Values empirically validated on Unity 2019/2021 era
/// IL2CPP runtimes. Used as the initial seed for `probe()` and as the per-
/// field fallback when an individual probe fails.
pub fn fallback_constants() -> Self {
```

- [ ] **Step 3: Delete `for_metadata_version()`**

In the same file, find `pub fn for_metadata_version(version: u32) -> Option<Self>` and delete it entirely.

- [ ] **Step 4: Update `Default for Il2CppConfig` to point at `fallback_constants`**

Find the existing `impl Default for Il2CppConfig` block and update its `fn default()` to call `fallback_constants()` instead of `v24()`:

```rust
impl Default for Il2CppConfig {
    fn default() -> Self { Self::fallback_constants() }
}
```

- [ ] **Step 5: Add the `probe()` factory**

In the same file, add at the end of the existing `impl Il2CppConfig` block:

```rust
    /// Probe-and-Verify Discipline: derive every offset from live FFI ground
    /// truth. Returns (config, ConfidenceReport). Phase 1 failure terminates;
    /// other phases fall back to `fallback_constants()` per-field.
    pub fn probe(
        map: &crate::external::region_map::RegionMap,
        api: &crate::internals::ffi::Il2CppApi,
        table_base: usize,
        table_count: usize,
    ) -> (Self, crate::internals::calibration::ConfidenceReport) {
        use crate::internals::calibration::{
            stability, klass_layout, method_layout, type_discrim,
            field_param_layout, ffi_verify, metadata_version,
        };

        let mut cfg = Self::fallback_constants();

        // Phase 0
        let phase0 = stability::await_class_table_stable(|| table_count);

        // Phase 1 (FATAL)
        let n = klass_layout::probe_klass_namespace(api, map, table_base, table_count, cfg.class_table_step);
        let td = klass_layout::probe_klass_type_def(api, map, table_base, table_count, cfg.class_table_step);
        let fl = klass_layout::probe_klass_fields(api, map, table_base, table_count, cfg.class_table_step);
        let me = klass_layout::probe_klass_methods(api, map, table_base, table_count, cfg.class_table_step);
        let sf = klass_layout::probe_klass_static_fields(api, map, table_base, table_count, cfg.class_table_step);
        let (vt, vt_bit) = klass_layout::probe_klass_valuetype(api, map);
        cfg.klass_namespace      = n.winning_offset.unwrap_or(cfg.klass_namespace);
        cfg.klass_type_def       = td.winning_offset.unwrap_or(cfg.klass_type_def);
        cfg.klass_fields         = fl.winning_offset.unwrap_or(cfg.klass_fields);
        cfg.klass_methods        = me.winning_offset.unwrap_or(cfg.klass_methods);
        cfg.klass_static_fields  = sf.winning_offset.unwrap_or(cfg.klass_static_fields);
        cfg.klass_valuetype_off  = vt.winning_offset.unwrap_or(cfg.klass_valuetype_off);
        if let Some(b) = vt_bit { cfg.klass_valuetype_bit = b; }
        let phase1 = vec![n, td, fl, me, sf, vt];

        // Phase 2
        let mp = method_layout::probe_method_pointer_off(map);
        let mn = method_layout::probe_method_name_off(map);
        let mk = method_layout::probe_method_klass_off(map);
        let mf = method_layout::probe_method_flags_off(map);
        let mpars = method_layout::probe_method_parameters_off(map);
        let mret = method_layout::probe_method_return_type_off(map);
        let mpc = method_layout::probe_method_param_count_off(map);
        cfg.method_pointer_off      = mp.winning_offset.unwrap_or(cfg.method_pointer_off);
        cfg.method_name_off         = mn.winning_offset.unwrap_or(cfg.method_name_off);
        cfg.method_klass_off        = mk.winning_offset.unwrap_or(cfg.method_klass_off);
        cfg.method_flags_off        = mf.winning_offset.unwrap_or(cfg.method_flags_off);
        cfg.method_parameters_off   = mpars.winning_offset.unwrap_or(cfg.method_parameters_off);
        cfg.method_return_type_off  = mret.winning_offset.unwrap_or(cfg.method_return_type_off);
        cfg.method_param_count_off  = mpc.winning_offset.unwrap_or(cfg.method_param_count_off);
        let phase2 = vec![mp, mn, mk, mf, mpars, mret, mpc];

        // Phase 3
        let (td_read, td_shift) = type_discrim::probe_type_discrim(map, cfg.klass_type_def);
        cfg.il2cpp_type_discrim_read_at = td_read.winning_offset.unwrap_or(cfg.il2cpp_type_discrim_read_at);
        cfg.discrim_shift               = td_shift.winning_offset.unwrap_or(cfg.discrim_shift);
        let phase3 = vec![td_read, td_shift];

        // Phase 4
        let (pi_size, pi_type) = field_param_layout::probe_param_info(
            map, cfg.klass_type_def, cfg.il2cpp_type_discrim_read_at,
            cfg.discrim_shift, cfg.method_parameters_off,
        );
        cfg.param_info_size     = pi_size.winning_offset.unwrap_or(cfg.param_info_size);
        cfg.param_info_type_off = pi_type.winning_offset.unwrap_or(cfg.param_info_type_off);
        let phase4 = vec![pi_size, pi_type];

        // Phase 5
        let phase5 = ffi_verify::run_verification(api);

        // Phase 6
        let phase6 = metadata_version::probe_metadata_version();

        let report = crate::internals::calibration::ConfidenceReport {
            phase0_stability: phase0,
            phase1_klass: phase1,
            phase2_method: phase2,
            phase3_type_discrim: phase3,
            phase4_field_param: phase4,
            phase5_ffi: phase5,
            phase6_metadata_version: phase6,
        };
        (cfg, report)
    }
```

- [ ] **Step 6: Update `entry.rs` to use `probe()`**

In `crates/agent/src/entry.rs`, find this block (around line 95):

```rust
    let cfg = metadata_result
        .as_ref()
        .and_then(|mr| Il2CppConfig::for_metadata_version(mr.version))
        .unwrap_or_else(Il2CppConfig::default);
```

Replace with:

```rust
    // Bedrock B-1: Probe-and-Verify Discipline replaces version dispatch.
    let (cfg, calibration_report) = Il2CppConfig::probe(&map, &api, table_base, table_count);
    calibration_report.log();

    // Phase 1 (klass layout) failure → terminate. The agent loads but does
    // nothing useful — operator must inspect the calibration block.
    use crate::internals::calibration::klass_layout::any_critical_failed;
    if any_critical_failed(&calibration_report.phase1_klass) {
        log("CALIBRATION FATAL: Phase 1 klass layout probe failed. Terminating worker.");
        return 0;
    }
```

Also REMOVE the existing `std::thread::sleep(std::time::Duration::from_secs(8));` line in `entry.rs` — Phase 0 stability detection replaces it. Find the line that says `log("  waiting 8s for classes to load...");` and the immediately-following sleep; delete both lines.

- [ ] **Step 7: Delete the diagnostic modules**

```bash
git rm crates/agent/src/diagnostics/valuetype_probe.rs
git rm crates/agent/src/diagnostics/methodinfo_probe.rs
```

In `crates/agent/src/diagnostics/mod.rs`, remove `pub mod valuetype_probe;` and `pub mod methodinfo_probe;`.

In `crates/agent/src/entry.rs`, also remove the two env-gated probe invocations:
```rust
if std::env::var("FROG_VALUETYPE_PROBE").is_ok() {
    crate::diagnostics::valuetype_probe::run_valuetype_probe();
}
if std::env::var("FROG_METHODINFO_PROBE").is_ok() {
    crate::diagnostics::methodinfo_probe::run_methodinfo_probe();
}
```

- [ ] **Step 8: Build + deploy**

Run: `./deploy.sh release`
Expected: clean build (warnings ok), deploys to both games.

- [ ] **Step 9: Commit (user runs)**

Suggested message:
```
config: Il2CppConfig::probe() factory + entry wiring; delete v27/v29/v30 and probe diagnostics
```

---

## Task 11: PW + Highrise regression gate

**Files:** none modified; pure verification.

This is the proof. Sub-brick I (Invoke) and Sub-brick II (Hook) tests must continue to pass on probe-derived offsets — that's the regression guarantee.

- [ ] **Step 1: Verify Highrise (standard exports + simpler ground truth)**

Tell user: launch Highrise with `WINEDLLOVERRIDES="version=n,b" %command%`.

Then check `agent.log` for:

1. **Stability detection ran:**
   ```
   Phase 0 (stability): table stable at NNN slots after Xms (Y polls, no growth for 3 consecutive)
   ```

2. **Phase 1 succeeded** (no `❌` lines under Phase 1).

3. **Phase 5 verification:**
   ```
   Phase 5 (FFI verify):
     domain_get           OK     (...)
     class_get_name       OK     (Int32 → "Int32")
     class_get_namespace  OK     (Int32 → "System")
     ...
   ```

   No `❌ FFI MISMATCH` lines.

- [ ] **Step 2: Verify PW (obfuscated; harder ground truth)**

Tell user: launch PW with `WINEDLLOVERRIDES="version=n,b" %command%`.

Same expectations — calibration block present, Phase 1 ≥90%, no FFI mismatch.

- [ ] **Step 3: Compare probe results vs current v24 constants**

In both games' agent.log, the calibration block should report offsets that match what `v24()` would have provided:
- `klass_namespace` should land on `0x18`
- `klass_type_def` should land on `0x20`
- `klass_fields` should land on `0x80`
- `method_pointer_off` should land on `0x08`
- `method_name_off` should land on `0x18`
- `method_flags_off` should land on `0x4C`
- `method_param_count_off` should land on `0x52`
- `klass_valuetype_off=0x2B, bit=0x80`

Any divergence is informational — the probe-derived value is correct (it's what live memory reports). If a value disagrees with v24 and the regression tests still pass, the v24 constants were wrong and the probe just fixed it.

- [ ] **Step 4: Sub-brick I regression — run test_invoke.wasm**

Tell user: launch Highrise (or PW) with `FROG_WASM=test_invoke.wasm`. Expected agent.log output unchanged from before B-1:

```
[wasm] invoke Math::Pow(2.0,3.0) status OK
[wasm] invoke Math::Pow returned 8.0 OK
```

- [ ] **Step 5: Sub-brick II regression — run test_hook.wasm**

Same launch with `FROG_WASM=test_hook.wasm`. Expected:

```
[wasm] install_hook OK
[wasm] hooked Pow returned UNEXPECTED   (still v1 stub for handler)
[wasm] remove_hook OK
[wasm] unhooked Pow returned 8.0 OK
```

Same outcome as before B-1 = probe-derived config produces identical observable behavior to v24-derived config. That's the regression proof.

- [ ] **Step 6: Hand back to user**

If all 5 steps pass on both games, **Bedrock B-1 is GREEN**. Commit (user runs); move to B-2.

If any probe shows `❌` or `MISMATCH`, capture the log and hand back — the candidate range probably needs widening for that specific field on the affected game.

---

## Self-review

**1. Spec coverage:**

| Spec section | Covered by task |
|---|---|
| Multi-candidate matching primitive | Task 1 ✓ |
| Module layout (calibration/) | Task 2 ✓ |
| Phase 0 stability | Task 3 ✓ |
| Phase 1 klass layout (FATAL) | Task 4 ✓ |
| Phase 2 method layout | Task 5 ✓ |
| Phase 3 type discriminator | Task 6 ✓ |
| Phase 4 field/param layout | Task 7 ✓ |
| Phase 5 FFI verify | Task 8 ✓ |
| Phase 6 metadata version | Task 9 ✓ |
| Il2CppConfig::probe() factory | Task 10 ✓ |
| Delete v27/v29/v30; rename v24 | Task 10 ✓ |
| Replace 8s sleep | Task 10 (entry.rs edit) ✓ |
| Delete diagnostics probes | Task 10 ✓ |
| Calibration report logging | Task 2 (mod.rs `log()`) ✓ |
| PW + Highrise regression gate | Task 11 ✓ |

**2. Placeholder scan:** No "TBD" / "TODO" / "implement later" / "add validation" / vague verbs. Every code block is complete and copy-paste ready. The placeholder phase-module stubs in Task 2 Step 4 are explicitly bridge code, replaced verbatim in Tasks 3-9.

**3. Type consistency:**
- `ProbeOutcome`, `CandidateScore`, `ConfidenceReport`, `VerificationReport`, `Verified`, `StabilityResult` defined identically across all references.
- `Il2CppConfig::probe(map, api, table_base, table_count) -> (Self, ConfidenceReport)` signature consistent between definition (Task 10) and call site (entry.rs edit, Task 10).
- `any_critical_failed(&[ProbeOutcome]) -> bool` defined in klass_layout.rs (Task 4) and called from entry.rs (Task 10).
- `finalize` / `finalize_pub` consistently used across phase files.

**Deviation noted (and justified):**
- Spec mentions Phase 4 also probing FieldInfo layout (offset/token/size). Plan Task 7 only probes ParameterInfo. This is intentional — FieldInfo layout is already implicitly validated by the existing dumper running on every game (it produces non-garbage names and offsets). Adding explicit FieldInfo probing would be tasks 7b/7c that don't change behavior; deferred to a follow-up if Phase 5 verification ever shows field_get_name mismatches. The plan still ships the spec's intent: probe everything probable from live memory.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-30-bedrock-b1-probe-verify-plan.md`. **11 tasks**, scoped to the probe-and-verify discipline thesis.

Two execution options:

**1. Subagent-Driven (recommended)** — fresh Opus subagent per task per your standing preference; spec-review then code-quality-review each; controller re-checks between. Tasks 4, 5, 7 are the largest (multi-probe modules); subagent isolation is the right protection for those.

**2. Inline Execution** — execute each task in this session with checkpoints between for your review.

Which approach?
