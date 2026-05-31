# Bedrock B-1: Probe-and-Verify Discipline — Design

**Date:** 2026-05-30
**Branch:** `ffi-class-table` (or successor)
**Status:** approved, ready for plan-writing
**Builds on:** existing Sub-brick I (Invoke) + Sub-brick II (Hook) substrate
**Audit reference:** `docs/superpowers/audit-2026-05-29-architecture-review.md`

---

## Goal

Establish the agent's runtime substrate (offsets, FFI exports, timing, metadata version) on **structural probing with multi-candidate consensus matching against live FFI ground truth**, replacing version-table dispatch theater. Make every silent failure mode loud. Make every assumption testable from the agent.log calibration block alone.

## The thesis (one line)

> **Probe everything from live memory at startup. Multi-candidate match against FFI ground truth. Log every field with its confidence. Fail-OPEN with fallback; only Phase 1 failure is fatal.**

## Why this is the bedrock brick

Audit-identified problems concentrate on one root cause: the agent currently *guesses* its runtime parameters from a version table that doesn't run on obfuscated games (`scan_process_for_metadata` returns None → falls back to `Il2CppConfig::default() = v24()`). Every obfuscated game uses v24 offsets regardless of actual Unity version. The dumper gets away with it because FFI calls like `class_get_name` work irrespective of struct layout — only the **structural readers** (resolver, field walk, type-discriminator extraction) depend on offsets being correct, and they have no validation feedback. A wrong sig-scan match is indistinguishable from a healthy result.

This brick replaces guessing with probing. Every critical field gets cross-validated against ≥50 FFI ground-truth anchors. Every resolved FFI export gets *called* with known input and the return value checked. Any mismatch is logged loudly and the resolution is dropped, not silently used.

## Non-goals (deferred)

| Item | Deferred to |
|---|---|
| IOCP pending-map leak | B-2 |
| `read_name` 64-byte + printable-ASCII cap | B-2 |
| Anti-cheat gate periodic re-check (currently startup-only) | B-2 |
| Dumper type-resolution rework (the 200+ `<type:NNN>` from the audit) | B-2 |
| Cross-domain transactionality docs | B-2 |
| `i64↔u64` sign-extension docs | B-2 |
| `InvokeArg` round-trip test | B-2 |
| wasmi `Store` wiring for real hook handler dispatch (Hook H12) | B-3 |
| `aob_scan` cache coherence | B-4 |

Each deferred item is concretely addressable on top of B-1 — none of them block B-1, and B-1 doesn't make them harder.

## Status code allocation

No new status codes. Failures during probing are diagnostic-only (logs); they don't enter the WASM ABI error space.

---

## Architecture

### Module layout

```
crates/agent/src/internals/
├── config.rs                    ← Modify: v27/v29/v30 deleted;
│                                          v24 renamed `fallback_constants()`;
│                                          add `probe(map, api, table_base, table_count)` factory
└── calibration/
    ├── mod.rs                   ← Phase orchestrator + ConfidenceReport
    ├── candidates.rs            ← Multi-candidate matching primitive (shared)
    ├── stability.rs             ← Phase 0: class-table stability detection
    ├── klass_layout.rs          ← Phase 1: klass_namespace, klass_type_def, klass_fields,
    │                                       klass_methods, klass_static_fields,
    │                                       klass_valuetype_off, klass_valuetype_bit
    ├── method_layout.rs         ← Phase 2: method_pointer_off, method_name_off,
    │                                       method_klass_off, method_flags_off,
    │                                       method_parameters_off, method_return_type_off,
    │                                       method_param_count_off
    ├── type_discrim.rs          ← Phase 3: il2cpp_type_discrim_read_at, discrim_shift
    ├── field_param_layout.rs    ← Phase 4: param_info_size, param_info_type_off (+ FieldInfo)
    ├── ffi_verify.rs            ← Phase 5: call-and-verify each resolved FFI fn
    └── metadata_version.rs      ← Phase 6: structural metadata version detection
```

### Phase dependency graph

```
Phase 0 (stability)  ─── (independent) ──┐
                                          │
Phase 1 (klass)      ─── [FATAL if fails] ┤
   │                                      │
   ├──────► Phase 2 (method) ─────────────┤
   ├──────► Phase 3 (type tc) ────────────┤
   └──────► Phase 4 (field/param) ────────┤
                                          │
Phase 5 (FFI verify) ── (after API ready)─┤
Phase 6 (metadata)   ── (independent) ────┘
                                          ▼
                              Il2CppConfig::probe() returns
                              full populated config + ConfidenceReport
```

**Phase 1 failure → loud diagnostic + agent terminates** ("no class table = no tool"). All other phase failures → log + fall back to `fallback_constants()` field-by-field + continue with degraded confidence.

### Top-level API surface

```rust
impl Il2CppConfig {
    /// Probe every critical offset from live memory. Falls back to
    /// `fallback_constants()` per-field on probe failure. Phase 1 failure is fatal.
    pub fn probe(
        map:         &RegionMap,
        api:         &Il2CppApi,
        table_base:  usize,
        table_count: usize,
    ) -> (Self, ConfidenceReport);

    /// Last-resort baseline. Formerly v24(). Used as initial seed for `probe()`
    /// and as fallback for individual fields whose probe fails.
    fn fallback_constants() -> Self;
}
```

**Zero changes to 30+ `cfg.X` read sites** across dumper, resolver, hook_runtime, marshal. The Config struct's read interface stays identical.

---

## Multi-candidate matching primitive

The reusable building block every phase uses. Lives in `calibration/candidates.rs`.

```rust
pub struct CandidateScore {
    pub offset:  usize,
    pub matches: u32,
    pub total:   u32,
}

/// Pick the candidate offset whose extracted value most often matches the
/// ground-truth expected value. Returns None if no candidate clears `min_ratio`.
///
/// `anchors` = list of (subject, expected_value) pairs from FFI ground truth.
/// `extract` = read function: given a subject + candidate offset → extracted value.
pub fn pick_offset_by_consensus<S, V, F>(
    candidates: &[usize],
    anchors:    &[(S, V)],
    extract:    F,
    min_ratio:  f32,        // standard: 0.90
) -> Option<(usize, CandidateScore)>
where
    F: Fn(&S, usize) -> Option<V>,
    V: PartialEq;
```

**Selection rules:**
- If no candidate clears `min_ratio` → return `None` (caller falls back to seed constant).
- If multiple candidates clear → pick highest match count.
- Always log every candidate's score, not just the winner, so the operator can see how tight the consensus was.

**Worked example — Phase 1 probing `klass_namespace`:**
```rust
fn probe_klass_namespace(map: &RegionMap, api: &Il2CppApi,
                         table_base: usize, table_count: usize) -> Option<usize>
{
    // 1. Gather 50 ground-truth anchors from FFI.
    let anchors = sample_anchors_with_ffi(api, table_base, table_count, 50,
        |klass| unsafe { cstr_to_string((api.class_get_namespace)(klass)) });

    // 2. Candidate offsets.
    let candidates = [0x10, 0x18, 0x20, 0x28];

    // 3. Extract: read u64 at klass+offset → treat as char*, decode cstr.
    let extract = |klass: &usize, off: usize| -> Option<String> {
        let ns_ptr = map.read_u64(klass + off)? as usize;
        map.read_cstr(ns_ptr).filter(|s| !s.is_empty())
    };

    // 4. Consensus pick.
    pick_offset_by_consensus(&candidates, &anchors, extract, 0.90)
        .map(|(off, score)| {
            log(&format!("probe klass_namespace: WINNER +{:#04x} match={}/{}",
                         off, score.matches, score.total));
            off
        })
}
```

Same primitive applies to every phase — only the anchors and extract function change.

---

## Phase specifications

### Phase 0 — Class-table stability detection (replaces 8s sleep)

**Purpose:** wait until the class table has finished growing before probing layout. Currently we sleep 8s and pray. A slow disk or modded game can have classes loading past 8s, producing an incomplete dump. A fast game may need only 1s.

**Strategy:** poll the class table size every 200 ms; consider stable when the count hasn't changed for **N=3 consecutive polls** (600ms of no growth). Hard timeout: 30s.

```rust
pub fn await_class_table_stable(
    table_base:  usize,
    table_count: &dyn Fn() -> usize,     // closure that re-counts on each call
    map:         &RegionMap,
) -> StabilityResult {
    let timeout = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(200);
    let stable_polls_needed = 3;

    let start = Instant::now();
    let mut last_count = 0;
    let mut stable_streak = 0;

    while start.elapsed() < timeout {
        let count = table_count();
        if count == last_count && count > 0 {
            stable_streak += 1;
            if stable_streak >= stable_polls_needed {
                return StabilityResult::Stable {
                    count, elapsed: start.elapsed(), polls: total_polls,
                };
            }
        } else {
            stable_streak = 0;
            last_count = count;
        }
        sleep(poll_interval);
    }
    StabilityResult::Timeout { last_count, elapsed: timeout }
}
```

**Output to log:**
```
Phase 0 (stability): table stable at 18515 slots after 4.2s (12 polls, no growth for 3 consecutive)
```
or:
```
Phase 0 (stability): TIMEOUT — table still growing after 30s (last count: 12453)
```

Timeout is non-fatal: log the warning, proceed to Phase 1 with whatever's there.

### Phase 1 — klass layout (FATAL on failure)

**Probes:**
- `klass_namespace` (offset of namespace ptr) — anchor: 50 random classes from table; ground truth: `api.class_get_namespace(klass)`
- `klass_name`-equivalent via `class_get_name` for cross-check
- `klass_type_def` (offset of TypeDefinition handle) — anchor: 50 classes; ground truth: extracted tc-discriminator after offset
- `klass_fields` (offset of FieldInfo array ptr) — anchor: 30 classes with ≥1 field; ground truth: walk array, verify name@0 of each FieldInfo is a non-empty cstr
- `klass_methods` (offset of MethodInfo* array ptr) — anchor: 30 classes with ≥1 method; ground truth: walk array, verify methodPointer@0x08 is in the code region
- `klass_static_fields` (offset of static-fields base ptr) — anchor: classes with static fields; ground truth: ptr to data region (non-null, non-class-ptr)
- `klass_valuetype_off`, `klass_valuetype_bit` — anchor: 5 known value types (`Int32`, `Single`, `Boolean`, `Byte`, `Double`) + 4 known reference types (`String`, `Object`, `Type`, `Exception`); ground truth: bit set in all VTs AND clear in all REFs (this is the existing valuetype probe pattern; promoted to Phase 1)

**FATAL behavior:** if **any** of the 7 probes fails to clear 90%, log the full diagnostic and terminate the worker thread. The agent loads but does nothing useful. Subsequent investigation requires examining the calibration block.

### Phase 2 — MethodInfo layout (depends on Phase 1)

**Probes:** all 7 method offsets via the existing methodinfo probe pattern (already proven on PW + Highrise) — `method_pointer_off`, `method_name_off`, `method_klass_off`, `method_flags_off`, `method_parameters_off`, `method_return_type_off`, `method_param_count_off`.

**Anchors:** `System::Math::Pow(double, double)` (unambiguous signature, 2 R8 args) + `System::String::PadLeft(Int32, Char)` (distinct-typed param disambiguator for stride).

**Non-fatal:** failure → fall back to fallback_constants() for that field; log loudly.

### Phase 3 — Type discriminator (depends on Phase 1)

**Probes:** `il2cpp_type_discrim_read_at` and `discrim_shift` — the bitfield extraction recipe for reading `tc` from an `Il2CppType`.

**Anchors:** 5 known klasses (Int32 tc=0x08, String tc=0x0E, Object tc=0x1C, Single tc=0x0C, Double tc=0x0D) — read each klass's `byval_arg` (which is at `klass_type_def`), extract candidate discriminator chunks, check that `(chunk >> shift) & 0xFF` returns the expected tc.

**Non-fatal:** failure → fallback. (Marshalling layer would degrade — most types resolve to U64 default.)

### Phase 4 — FieldInfo + ParameterInfo layout (depends on Phase 1)

**FieldInfo probes:** `field_info_name_off`, `field_info_type_off`, `field_info_offset_off`, `field_info_token_off`, `field_info_size` (stride).

**Anchors:** walk one well-known class's FieldInfo array (`System::Int32`-style); cross-reference each field with `api.field_get_name`, `api.field_get_type`.

**ParameterInfo probes:** `param_info_size`, `param_info_type_off` — promoted from the existing methodinfo probe.

**Anchors:** `String::PadLeft(Int32, Char)` for distinct-typed disambiguation.

**Non-fatal:** failure → fallback.

### Phase 5 — FFI export call-and-verify (after Phase 1 ground truth available)

**Purpose:** catch sig-scan false positives by *actually calling* each resolved FFI export with known input and checking the output. The "got lucky" failure mode the audit identified (e.g., `class_get_namespace` resolving to `class_get_name` because both share the same prologue shape) becomes catchable here.

**Strategy:** after `Il2CppApi::resolve()` returns a candidate `api`, run verification. Mismatches **drop the resolution** — the standard exports path may have failed verification but the sig-scan path may succeed (or vice versa). Fall through to whichever passes.

```rust
pub struct VerificationReport {
    pub domain_get:            Verified,   // REQUIRED — fail-open if bad
    pub class_get_name:        Verified,   // REQUIRED
    pub class_get_namespace:   Verified,   // REQUIRED
    pub field_get_name:        Verified,   // REQUIRED
    pub field_get_type:        Verified,   // REQUIRED
    pub type_get_name:         Verified,   // REQUIRED
    pub class_get_fields:      Verified,   // OPTIONAL — None if absent
    pub thread_attach:         Verified,   // OPTIONAL
    pub runtime_invoke:        Verified,   // OPTIONAL — gates Invoke capability
    pub string_new:            Verified,   // OPTIONAL
    pub array_new:             Verified,   // OPTIONAL
    pub exception_get_message: Verified,   // OPTIONAL
}

pub enum Verified {
    Ok,
    Absent,                                       // resolved as None
    Mismatch { expected: String, got: String },   // RED FLAG — silent corruption
    Crashed,                                      // call panicked or returned garbage
}
```

**Per-export verification:**

| Export | Verification |
|---|---|
| `domain_get` | non-null return; pointer in il2cpp data region (`0x3xxxxxxxx`) |
| `class_get_name(Int32_klass)` | expect `"Int32"` |
| `class_get_namespace(Int32_klass)` | expect `"System"` |
| `class_get_fields(Player_klass, iter)` | first call returns non-null FieldInfo |
| `field_get_name(known_field)` | non-empty match against known FieldInfo |
| `field_get_type(known_field)` | non-null Il2CppType in expected region |
| `type_get_name(known_type)` | non-empty match against known type name |
| `runtime_invoke` | NOT verified at calibration time (would need a managed method call). Deferred to test suite (`test_invoke.wat` Math.Pow gate is the proof). |
| `string_new("test")` | non-null Il2CppString*; optionally read back as "test" |
| `array_new(Int32_klass, 8)` | non-null Il2CppArray* with length 8 |
| `thread_attach` | optional; no verification |
| `exception_get_message` | optional; verified at test time if a managed exception fires |

**Failure policy:**
- **Required Absent / Crashed** → return None from `Il2CppApi::resolve()` (drops this resolution path); caller tries the other path.
- **Required Mismatch** → loud `"❌ FFI MISMATCH: X resolved to addr=0x... but called with KNOWN_INPUT returned WRONG_VALUE — DROP RESOLUTION"` + return None.
- **Optional Absent** → log `"FFI ABSENT: X (degraded capability)"`; `api.X = None`.
- **Optional Mismatch / Crashed** → log + `api.X = None`.

### Phase 6 — Metadata version (independent)

**Purpose:** replace the hardcoded v16–v31 dispatch with structural detection from the metadata header bytes (when metadata is present and parseable).

**Strategy:** if `scan_process_for_metadata()` produces a candidate blob, parse its header version field structurally (read the version at a known offset; sanity-check it falls in a reasonable range). Log the detected version.

**Non-fatal:** if metadata isn't found (obfuscated games), Phase 6 logs `"metadata: NOT FOUND (obfuscated; using probe-derived runtime config exclusively)"` and exits. No fallback to v24 dispatch — the runtime config is already complete from Phases 1-5.

When metadata IS found, the version is informational only (logged for diagnostic context); the runtime config is still entirely probe-derived.

---

## Logging surface

The single calibration report block is the bedrock proof:

```
=== CALIBRATION REPORT ===
Phase 0 (stability):    table stable at 18515 slots after 4.2s (12 polls, no growth for 3 consecutive)
Phase 1 (klass layout): all probes ≥90% confidence
  klass_namespace      +0x18  match=48/50  candidates_tried=[0x10,0x18,0x20,0x28]
  klass_type_def       +0x20  match=50/50  candidates_tried=[0x18,0x20,0x28,0x30]
  klass_fields         +0x80  match=46/50  candidates_tried=[0x70,0x78,0x80,0x88]
  klass_methods        +0x98  match=47/50  candidates_tried=[0x88,0x90,0x98,0xA0]
  klass_static_fields  +0xB8  match=45/50  candidates_tried=[0xA8,0xB0,0xB8,0xC0]
  klass_valuetype      +0x2B  bit=0x80  match=9/9 (5VT + 4REF)
Phase 2 (method layout): all probes ≥90%
  method_pointer_off   +0x08  match=50/50
  method_name_off      +0x18  match=50/50
  method_parameters    +0x30  match=49/50
  method_return_type   +0x28  match=50/50
  method_flags_off     +0x4C  match=50/50  (METHOD_ATTRIBUTE_STATIC bit verified)
  method_param_count   +0x52  match=50/50
Phase 3 (type discriminator): all probes ≥90%
  il2cpp_type_discrim_read_at +0x08  shift=16  match=50/50
Phase 4 (field+param layout): all probes ≥90%
  field_info_size      0x20  match=50/50
  param_info_size      0x18  match=50/50
  param_info_type_off  +0x00  match=50/50
Phase 5 (FFI verify):
  domain_get           OK     (returned 0x32xxxxxxxx)
  class_get_name       OK     (Int32 → "Int32")
  class_get_namespace  OK     (Int32 → "System")
  class_get_fields     OK     (Player → 23 fields)
  field_get_name       OK     (Player.position field → "position")
  field_get_type       OK     (returned valid Il2CppType*)
  type_get_name        OK     (Int32 type → "System.Int32")
  thread_attach        ABSENT (degraded: caller must auto-attach OS threads)
  runtime_invoke       OK     (resolved; verified by Math.Pow gate at test time)
  string_new           OK     (allocated 4-char "test" → 0x32xxxxxxxx)
  array_new            OK     (allocated 8-elem int[] → 0x32xxxxxxxx)
  exception_get_message ABSENT (degraded: ManagedException msg will show as "<unreadable>")
Phase 6 (metadata version): 24 (probed from header bytes at 0x6ffff5e..., informational only)
=== END CALIBRATION ===
```

Loud failures look like:
```
❌ PROBE FAIL: klass_methods — no candidate >90% (best 0x98 scored 12/50). Falling back to constant 0x98.
```
or:
```
❌ FFI MISMATCH: class_get_namespace returned "" for System::Int32_klass — DROPPING API RESOLUTION
```

The `ConfidenceReport` struct returned alongside `Il2CppConfig` carries the structured data for programmatic consumption (e.g., the frontend plugin can display "calibration confidence: 6/6 phases ≥90%" or surface specific probe failures).

```rust
pub struct ConfidenceReport {
    pub phase0_stability:       StabilityResult,
    pub phase1_klass:            Vec<ProbeOutcome>,
    pub phase2_method:           Vec<ProbeOutcome>,
    pub phase3_type_discrim:     Vec<ProbeOutcome>,
    pub phase4_field_param:      Vec<ProbeOutcome>,
    pub phase5_ffi:              VerificationReport,
    pub phase6_metadata_version: Option<u32>,
}

pub struct ProbeOutcome {
    pub field_name:        &'static str,
    pub winning_offset:    Option<usize>,
    pub match_count:       u32,
    pub anchor_count:      u32,
    pub fell_back:         bool,
    pub candidates_tried:  Vec<usize>,
}
```

---

## Migration: deleting v27/v29/v30, renaming v24

1. Delete the `Il2CppConfig::v27()`, `v29()`, `v30()` constructor functions and their associated comments about "guessed" offsets.
2. Rename `Il2CppConfig::v24()` → `Il2CppConfig::fallback_constants()`. Comment it as "last-resort seed used when probe fails; values empirically validated on Unity 2019/2021 era IL2CPP runtimes".
3. Delete the `for_metadata_version` dispatch function entirely. (`scan_process_for_metadata` still runs in Phase 6 for informational logging, but no longer dispatches to a layout table.)
4. Replace the call site in `worker()` (currently `Il2CppConfig::for_metadata_version(mr.version).unwrap_or_else(Il2CppConfig::default)`) with `Il2CppConfig::probe(map, api, table_base, table_count)`.
5. Move the existing static valuetype probe code from `diagnostics/valuetype_probe.rs` into `calibration/klass_layout.rs` (it's the Phase 1 valuetype probe; the env-var-gated diagnostic version can be deleted since the calibration always runs).
6. Move the existing methodinfo probe code from `diagnostics/methodinfo_probe.rs` into `calibration/method_layout.rs` + `field_param_layout.rs`. Same rationale — calibration always runs.

The diagnostics probe modules become dead code; delete them.

---

## Testing strategy

**In `agent-core` (Linux-runnable, no FFI):**
- `candidates::pick_offset_by_consensus` unit tests with synthetic anchors + extractors. Test cases:
  - Single clear winner above min_ratio
  - Multiple candidates above min_ratio (highest matches wins)
  - No candidate above min_ratio (returns None)
  - All candidates score 0 (returns None)
  - Edge case: anchors empty (defensive: returns None)
- `Il2CppConfig::fallback_constants` returns the same byte-for-byte values as the former `v24()`.

**In `agent` (cross-compile to Windows):**
- Calibration module compiles.
- Worker `entry.rs` is updated to call the new `probe()` factory and route results into the same `cfg` variable downstream code uses.

**Live verification (PW + Highrise):**
- Launch each game; verify the `=== CALIBRATION REPORT ===` block appears in `agent.log`.
- For PW: confirm all probes report ≥90% confidence. Compare winning offsets to the values currently in v24 — they should match (this is the regression test: probing the v24-tested game produces v24 numbers).
- For Highrise: confirm all probes report ≥90% confidence. Winning offsets may match v24 or may differ (depending on actual Unity version); either way, the dump output should now reflect probe-derived offsets.
- Sub-brick I (Invoke) `test_invoke.wasm` and Sub-brick II (Hook) `test_hook.wasm` continue to PW-gate cleanly — same outcomes as before B-1, proving the probe-derived config is at minimum as good as the v24 constants.

The PW-gate regression is the proof of correctness: the existing test_invoke.wasm and test_hook.wasm produce identical Math.Pow outcomes before and after B-1. If they don't, a probe is silently wrong and B-1 doesn't ship.

---

## What ships when B-1 lands

- `Il2CppConfig::probe()` factory replaces version-table dispatch.
- 7-phase calibration with multi-candidate consensus matching.
- FFI call-and-verify catches sig-scan false positives.
- Class-table stability detection replaces 8s sleep.
- Structural metadata version detection (informational).
- Loud per-probe diagnostic logging.
- 30+ `cfg.X` read sites unchanged.
- Existing test_invoke + test_hook PW gates still green (regression proof).

The bedrock now stands on probes against live ground truth, not guesses against a stale table.

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| A probe's anchor sampling picks unrepresentative classes (e.g., 50 anonymous generics) | Phase 1 explicitly diversifies anchors: requires representation of named classes with non-empty namespaces. The candidate primitive also requires ≥90% match, which is robust against partial-mismatch anchor noise. |
| Multi-candidate sample size of 50 is too few | The threshold is configurable; 50 was chosen because it's well above the consensus-matching law-of-large-numbers point for binary discrimination. The diagnostic log makes it trivial to increase the threshold post-ship if a false-positive case emerges. |
| FFI verification calls crash the process (some FFI exports panic on unexpected input) | Each verification is wrapped in `catch_unwind` (or, for `extern "C"` calls, in a SEH-style guard using `try { } catch (...) { }` semantics via `windows-sys`). On crash → `Verified::Crashed` → API resolution dropped. |
| Phase 0 stability detection times out on a slow game | 30s hard timeout is non-fatal; logs the warning and proceeds with whatever's in the table. The PW empirical data shows ~5s is typical; 30s is a generous safety margin. |
| Probe overhead at startup (currently ~0s of probing; B-1 adds ~50 anchors × 4 candidates × 8 bytes/read = ~1600 cache reads per probe × 25 probes ≈ 40K reads) | At cache-validated 50ns/read, total probing overhead is ~2ms. Negligible. |
| Operator misreads the calibration block as noise | The block is bounded (~30 lines), structured, and clearly demarcated. Loud `❌` markers ensure failures are unmissable. |
| A future Unity version changes a struct in a way that breaks the candidate ranges in a phase | The candidate ranges are intentionally generous (e.g., 0x10..0x30 for klass_namespace) and easy to extend. If a future game probes None, the operator can add candidates to the list — the rest of the agent keeps running on fallback. |
