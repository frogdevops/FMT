# Bedrock: Discovery-First Layout — Design

**Date:** 2026-06-01
**Branch:** `bedrock-investigation`
**Status:** draft — pending user review
**Supersedes:** `specs/2026-05-30-bedrock-b1-probe-verify-design.md` (its fail-OPEN-to-baseline + candidate-window-near-baseline model is the rot)
**Grounded in:** `investigations/2026-06-01-bedrock-layout-ground-truth.md` (foundation verified in code; cascade bug + container-first fix proven live on PW + Highrise, 12/12 unanimous)
**Memory:** [[calibration-silent-hardcode-rot]], [[bedrock-triangulation-no-oracle]], [[root-first-philosophy]], [[no-hardcoding-adaptive-resolution]], [[stage-bedrock-principle]], [[dumper-is-the-canary]]

---

## Goal (one line)

> **Every runtime layout fact is DISCOVERED from intrinsic structure with ≥2 independent witnesses, carries its provenance, and is `Unresolved` (never a silent fallback) when witnesses disagree. Capabilities attach to a single `Layout` contract — no capability reads a raw offset or a constant. No version table, no candidate windows, no hardcode fallbacks. Accuracy is cross-checked against an independent reference.**

This is bedrock-only. It makes the foundation *hold* before any capability is rebuilt on it (per [[stage-bedrock-principle]] / [[bedrock-before-capability]]). It works on PW (obfuscated, FFI-absent, metadata absent) **and** Highrise (modern Unity) — chosen because a method that holds on both will hold on most others.

## The real diagnosis: bedrock rot is a TRUTH-MANAGEMENT failure, not a code bug

The bedrock did not rot because a probe had a bug. It rotted because **the documented architecture and the living code diverged, and no one killed the old docs.** Three layers of stale truth coexist in `config.rs`, and the runtime behaves by whichever won the startup race:

1. **Documented (config.rs:1-29):** "version table; when a version is unknown the fallback estimates heuristically." That system *never existed* — the "table" is a single hardcoded `fallback_constants()` set.
2. **Bolted-on correction (config.rs:88-92):** "we always patch methodPointer at +0x08, never the struct base." *Now wrong in both directions* — the structural walk proved `0x0 ×12` on both games. The hook patches `0x0` only because Phase 2 silently overrode the `0x08` baseline before the hook ran. Correctness hangs on a race the comment actively lies about.
3. **Probes that silently override** the baseline at startup, pretending to be the "override layer" for a version table that was never populated.

These are not outdated comments — they are **architecturally misleading prose** that installs a false model in every future reader. The values and their documentation are two separate artifacts that drifted apart.

**The cure is structural, and it is the point of `Fact<T>`:** a value that carries *which recognizers agreed and what each observed* is self-documenting and cannot go stale, because it is recomputed every run from live memory. The prose lie has nowhere to live.

**The rule that prevents recurrence (codebase law):** *comments describe MECHANISM (how a discoverer recognizes a structure), NEVER VALUES (what an offset is).* No comment, ever, asserts a layout number. Values are discovered and carried in `Provenance`. The derivation trail (the old "calibration report") is generated from the `Fact`s themselves, not written by hand.

## What dies (the hardcode inventory — "remove any hardcodes is the rule")

- `Il2CppConfig::v24/v27/v29/v30` version table + the `for_metadata_version` dispatch.
- `fallback_constants()` as a *value source* (the "verified-correct prior / floor"). It survives ONLY as test fixtures / sanity ranges, never as a runtime answer.
- `apply_offset*` silent no-op-to-baseline (config.rs:139).
- All candidate-offset windows (`klass_layout.rs` `vec![0x88,0x90,...]` etc.).
- The `klass_methods` cascade: hardcoded `MethodInfo+0x08` + weak `≥0x10_0000` check (klass_layout.rs:243-245).
- The stale **landmine**: config.rs:88-92 "we always patch +0x08, never the struct base" — contradicted by the proof (`method_pointer_off = 0x0 ×12` on both games). The hook path must consume the *discovered* value, not a comment.
- Every magic struct offset that a capability reads directly.
- **Every fact-asserting comment** — config.rs:1-29 (the version-table fiction), every `/// = klass_fields + 0x18`, `/// +0x80 is stable for v24–v30`, `/// Probed: type is the FIRST field`, etc. These are deleted, not corrected. Per the codebase law (above), no comment asserts a value; the value is discovered and carries its own `Provenance`. A comment may describe *how a discoverer recognizes a structure*, never *what the number is*. This is the rule that stops the rot from regrowing: documentation that can drift from the value is forbidden — the value documents itself.

## Principles (from the investigation)

1. **Container-first, non-circular.** Find a container by intrinsic structure; then DERIVE its sub-offsets by classifying the container's own slots. Never assume a sub-offset to validate a container (that is the cascade). *Proven:* methods array = pointer-array whose entries are MethodInfo-shaped (≥1 RX ptr + ≥1 ptr == klass) → derive `method_pointer_off`/`klass_off`/`name_off` from the MethodInfo's slots. 12/12, both games.
2. **≥2 independent witnesses; agreement = confidence; disagreement = `Unresolved`.** No oracle on obfuscated games → truth from self-consistent triangulation. Witnesses must arrive by *mechanically orthogonal* paths (the foundation stack shares hidden layers — §6).
3. **Fail-closed, structurally.** No fallback exists to fall back to. `Unresolved` propagates to consumers; the dumper *shows* the gap (canary), the hook *refuses* rather than patching a guessed slot.
4. **Crash-safe = region-complete.** Every read through the never-fault `RegionMap` (VirtualQuery). A probe that crashes the target is a region-knowledge defect, not a nuisance.
5. **Coverage honesty.** Silence ≠ success. Account every class/field/method resolved vs dropped, with a reason; compare to the reference's per-image counts.

---

## The contract — `Fact<T>` + `Layout`

```rust
/// A discovered fact. There is no third "fallback" state by design.
#[derive(Debug, Clone, Copy)]
pub enum Fact<T> {
    /// ≥2 independent witnesses agreed on `value`.
    Resolved { value: T, witnesses: u8, provenance: Provenance },
    /// Witnesses were absent or disagreed. Carries WHY. No value — callers must handle.
    Unresolved { reason: UnresolvedReason },
}

/// Provenance IS the documentation. It records which witnesses agreed and what
/// each observed, so the value documents its own derivation and cannot go stale
/// (it is recomputed from live memory every run). The derivation trail / the old
/// "calibration report" is GENERATED from this — never hand-written prose.
#[derive(Debug, Clone)]
pub struct Provenance {
    pub witnesses: Vec<Witness>, // every agreeing derivation, with what it saw
    pub sampled:   u16,          // e.g. 12 → "12/12 klasses unanimous"
}

#[derive(Debug, Clone, Copy)]
pub struct Witness {
    pub method:   DerivationMethod, // Structural | ReferenceCrossCheck | FfiCrossCheck | OutOfBandAnchor
    pub observed: u64,              // the raw value this witness saw (must equal the agreed value)
    pub signal:   &'static str,     // the intrinsic signal, e.g. "RX-slot consistent across sample"
}

#[derive(Debug, Clone, Copy)]
pub enum DerivationMethod { Structural, ReferenceCrossCheck, FfiCrossCheck, OutOfBandAnchor }
// Example a log/dump prints straight from a Fact (no prose, no staleness):
//   method_pointer_off = 0x0  [Structural: "RX-slot consistent" 12/12;  ReferenceCrossCheck: "RVA match" 50/50]

#[derive(Debug, Clone, Copy)]
pub enum UnresolvedReason {
    NoWitness,           // nothing recognized the structure
    WitnessDisagreement, // ≥2 derivations produced different values
    NoMetadata,          // requires metadata that is absent (obfuscated) — e.g. type_def
    NoDiscriminator,     // no honest intrinsic signal exists (e.g. static_fields on most klasses)
}

impl<T: Copy> Fact<T> {
    pub fn get(self) -> Option<T> { matches!(self, Fact::Resolved{..}).then(|| /* value */) }
    pub fn require(self) -> Result<T, UnresolvedReason> { /* ... */ }
}
```

```rust
/// THE bedrock contract. Capabilities consume this; nothing else carries offsets.
/// Replaces `Il2CppConfig` at all ~30 read sites.
pub struct Layout {
    // foundation (verified)
    pub table_base:        Fact<usize>,
    pub table_count:       Fact<usize>,
    pub class_table_step:  Fact<usize>,   // proven 8 via period autocorrelation
    // klass
    pub klass_namespace:   Fact<usize>,
    pub klass_fields:      Fact<usize>,
    pub klass_methods:     Fact<usize>,
    pub klass_static_fields: Fact<usize>,
    pub klass_type_def:    Fact<usize>,   // Unresolved{NoMetadata} on obfuscated games
    pub klass_generic_class: Fact<usize>,
    pub klass_valuetype_off: Fact<usize>,
    pub klass_valuetype_bit: Fact<u8>,
    // il2cpp type
    pub type_discrim_read_at: Fact<usize>,
    pub discrim_shift:     Fact<u8>,
    // method info (sub-offsets DERIVED from the methods container)
    pub method_pointer_off: Fact<usize>,  // proven 0x0 (NOT 0x08)
    pub method_klass_off:   Fact<usize>,  // proven 0x20
    pub method_name_off:    Fact<usize>,  // proven 0x18
    pub method_param_count_off: Fact<usize>,
    pub method_return_type_off: Fact<usize>,
    pub method_parameters_off:  Fact<usize>,
    pub method_flags_off:   Fact<usize>,
    // parameter info
    pub param_info_size:    Fact<usize>,
    pub param_info_type_off: Fact<usize>,
}
```

`Layout` is produced ONCE by the discovery pass and is immutable thereafter. It is the *only* type capabilities depend on.

---

## Discovery architecture — Discoverers, in dependency order

A **Discoverer** takes the `RegionMap` + already-resolved facts and emits a `Fact`. They run in strict dependency order so nothing is circular:

```
FOUNDATION (verified in code)
  stride        : period autocorrelation @8-byte granularity (find_class_table already does the dense-run)
  regions       : RegionMap::capture (VirtualQuery kernel witness)
  root/table    : .dll-image + name + ns validator (class_fields) → dense run
  ── new stability witnesses ──
  region_coverage : every pointer a walk reaches is in a known region or flagged (crash-impossible)
  root_integrity  : table slots all klass-shaped; count sane vs reference image totals
  oob_anchor      : 1 heap-string→klass located OFF the table → cross-checks table+stride+region

CONTAINERS (intrinsic, no sub-offset assumed)
  klass_fields  : klass+off → array of inline FieldInfo (slot0 = name cstr, contains ptr==klass, NO RX ptr), const stride
  klass_methods : klass+off → ptr-array; entries[0..2] MethodInfo-shaped (≥1 RX ptr + ≥1 ptr==klass)   [PROVEN]

SUB-OFFSETS (derived by classifying a found container's slots)
  method_pointer_off  = MethodInfo slot that is an RX ptr (consistent across methods)   [PROVEN 0x0]
  method_klass_off    = MethodInfo slot == klass                                        [PROVEN 0x20]
  method_name_off     = MethodInfo slot → readable cstr                                 [PROVEN 0x18]
  method_param_count_off / flags_off / parameters_off / return_type_off : by typed-slot classification (u8 small / u32 flags / ptr→ParameterInfo[] / ptr→Il2CppType)
  field sub-offsets   : offset (small ascending int < instance_size) / name (cstr) / type (ptr into type region) / token

TYPE DISCRIMINATOR
  (read_at, shift) : the pair that makes ALL known primitive klasses round-trip (Int32=0x08, String=0x0E, …) — multi-anchor consensus, intrinsic. PW already 5/5.

HARD CASES (honest Unresolved, never faked)
  klass_static_fields : null on most klasses → NoDiscriminator unless a unique-RW-region signal is near-unanimous
  klass_type_def      : NoMetadata on obfuscated games → Unresolved; dumper routes via byval_arg/tc
```

Each container/sub-offset discoverer requires **agreement across the sampled klasses** (e.g. methods_off must be unanimous across N≥12 structurally-sampled classes, as proven) — that unanimity IS the second witness. Where a second independent mechanism exists (FFI, reference dump), it is added as a stronger witness and a mismatch downgrades to `Unresolved`.

Per-fact witness recipes are the triangulation map in the investigation doc (§Triangulation Map) — this spec adopts it verbatim as the discoverer set.

---

## Accuracy — cross-check against the reference (and the bar)

`ref/pw_reference_pack/dump/dump_v245.cs` (Il2CppDumper static output, ~3714 classes in Assembly-CSharp) is an **independent oracle for PW** and the **accuracy bar**:

- **Method RVA cross-check:** discovered `methodPointer − image_base` must equal the reference `// 0x…` RVA for the same method. A `ReferenceCrossCheck` witness; mismatch → `Unresolved` for the offending derivation.
- **Field offset cross-check:** discovered field offsets must equal the reference's `// 0xNN`.
- **Name/shape cross-check:** class, field, method names + signatures must match → this is the dumper accuracy target (match dumper-grade output).
- **Coverage:** per-image class counts vs the reference header (`Assembly-CSharp.dll (3714 classes)`) — the coverage accountant reports `found/ref` per image, turning the `7077→2496` mystery into a measured ratio.

Games without a reference (Highrise) rely on triangulation + self-consistency; the reference path is a *bonus* witness, never a dependency (no hardcoded answer key in the shipped agent — the reference is a dev-time validation input, gated like a diagnostic).

---

## The API capabilities attach to (and migration)

Today: ~30 sites read `cfg.<offset>` (dumper, resolver, hook_runtime, marshal, scan, internals/api). Each is a place a silent-wrong fallback leaks.

After: those sites take `&Layout` and read `layout.<fact>`. Because each is a `Fact`, the consumer MUST decide what `Unresolved` means *for it* — this is where fail-closed becomes real:

| Capability | On `Unresolved` |
|---|---|
| Dumper (canary) | emit the section with an explicit `// UNRESOLVED: <fact> (<reason>)` marker — show the gap, never fake a value |
| Hook (inline_detour) | **refuse to install** (return an error) rather than patch a guessed slot — the landmine fix |
| Marshal / invoke | refuse the typed op; degrade to raw or error |
| Scan / instances | proceed only with the facts it actually needs |

A capability "just attaches" by depending on `Layout` and handling the `Fact`s it consumes — no offset threading, no `cfg` plumbing. New capabilities compose against the same contract.

---

## Module layout

```
crates/agent/src/bedrock/                 (new — the discovery engine + contract)
├── fact.rs            Fact<T>, Provenance, UnresolvedReason
├── layout.rs          Layout struct + discover(map, root) -> Layout
├── discover/
│   ├── foundation.rs  stride / regions / root / region_coverage / root_integrity / oob_anchor
│   ├── containers.rs  klass_fields / klass_methods recognizers (seeded by the proven recognizer)
│   ├── suboffsets.rs  derive-from-container slot classification
│   ├── type_discrim.rs primitive round-trip consensus
│   └── hard_cases.rs  static_fields / type_def honest-Unresolved
├── crosscheck.rs      reference-dump (dev-gated) + FFI cross-check witnesses
└── coverage.rs        per-image accounting vs reference
crates/agent/src/internals/config.rs      → DELETED (Il2CppConfig retired; ranges move to test fixtures)
crates/agent/src/internals/calibration/   → DELETED (replaced by bedrock/discover)
```
The diagnostics `FROG_RECOGNIZER_PROBE` is the seed of `containers.rs` + `suboffsets.rs` and stays as the live validation probe ([[diagnostic-env-gates-stay]]).

---

## Testing

- **agent-core (Linux):** `Fact` semantics; discoverer logic against synthetic klass/MethodInfo byte fixtures (mock RegionMap) — every recipe gets a fixture that proves Resolved AND a fixture that forces Unresolved.
- **Live (PW + Highrise):** the discovery pass produces a `Layout` whose resolved values equal the recognizer-proven offsets; every `Unresolved` is expected + logged.
- **Reference cross-check (PW):** discovered field offsets + method RVAs match `dump_v245.cs` within the sampled set; coverage ratios reported.
- **Regression:** existing `test_invoke.wasm` / `test_hook.wasm` Pow gates stay green — proves the discovered `Layout` is at least as good as the retired cfg.

## Non-goals (this brick is bedrock only)

- Rebuilding the dumper/hook/marshal capabilities (they migrate to `&Layout` mechanically; their *redesign* is later bricks).
- The B-6b dumper method/instance ordering bug (fixed once the dumper consumes `Layout` post-discovery).
- New host-API surface.

## Integration safety — the blast radius is the real risk (DO NOT let it fall apart)

Bedrock is NOT greenfield. Cross-check (2026-06-01): **96 references** to `Il2CppConfig`/`cfg.X` across `dump.rs`, `resolve.rs`, `marshal.rs`, `hook_runtime/api.rs`, `internals/api.rs`, `ctx.rs`, `diagnostics/*`, and `calibration/*`. **`ctx.rs` holds the cfg** (`entry.rs:175`), and most consumers read `c.cfg.X` via `ctx::get()`. Changing the field type from `usize` to `Fact<usize>` in one shot breaks all 96 simultaneously — forbidden.

**The seam: `ctx` carries BOTH during migration.** `InternalsCtx` gains `layout: Layout` while keeping `cfg: Il2CppConfig`. `discover()` produces the `Layout`; the legacy `cfg` is *derived from it* during migration (`cfg.method_pointer_off = layout.method_pointer_off.require().unwrap_or(<old-baseline>)` — the ONLY place a baseline survives, and only transiently). Consumers migrate **one module at a time** from `c.cfg.X` → `c.layout.X` + explicit `Unresolved` handling. At every commit: build green, and the **live Pow hook/invoke gate stays green** (the integration canary — user-run on PW). When the last consumer is migrated, the derived-cfg seam and `config.rs` + `calibration/` are deleted together.

**Migration order (lowest-risk first, highest-blast-radius/landmine last):**
1. `dump.rs` (canary — its `Unresolved` markers are *desired* output; safest first mover, and it surfaces discovery gaps immediately)
2. `resolve.rs` (type-name accuracy — validated against the reference dump)
3. `marshal.rs`
4. `internals/api.rs`
5. `hook_runtime/api.rs` + `inline_detour.rs` **last** — the landmine path; `method_pointer_off` now `Resolved 0x0`; hook **refuses** on `Unresolved`. Live Pow gate is the proof.

`ctx.rs` is migrated implicitly (it stops cloning `cfg` once nothing reads it). No consumer is touched until the discoverer for the fact(s) it needs is proven live on PW + Highrise.

## Rollout

1. `bedrock/` engine + `Layout` + discoverers (agent-core-testable core where possible); `FROG_RECOGNIZER_PROBE` graduates into `containers.rs`/`suboffsets.rs`.
2. Live-prove the full `Layout` on PW + Highrise (every fact `Resolved` or expectedly `Unresolved`) BEFORE migrating any consumer.
3. `ctx` carries `layout` + derived-`cfg` seam (coexistence). Build green, Pow gate green.
4. Migrate consumers in the order above, one module per step; build + Pow gate green at each.
5. Delete the derived-cfg seam + `config.rs` + `calibration/` together once the last consumer is on `Layout`.
6. Reference cross-check (PW) + coverage accounting; final live-validate on both games.

---

## Self-Review
- **Placeholders:** none — every fact has a discoverer recipe (or an explicit honest-Unresolved case).
- **Consistency:** `Fact`/`Layout` shape matches the user-chosen option; fail-closed has no escape hatch (no fallback type exists); proven values (methods 0x98, method_pointer 0x0, klass 0x20, name 0x18) are recorded, not assumed.
- **Scope:** bedrock only; capability rebuilds explicitly deferred. Single implementation plan's worth.
- **Ambiguity:** "witness independence" is the sharpest risk — the foundation-stack shared-layer analysis (investigation §Foundation flaws) is the guard; cross-mechanism witnesses (period-autocorrelation, page-walk-is-the-region-map, OOB anchor, reference) are named explicitly.
