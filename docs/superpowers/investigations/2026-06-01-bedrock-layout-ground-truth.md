# Bedrock Investigation — Struct-Layout Ground Truth (discovery-first feasibility)

**Branch:** `bedrock-investigation`
**Status:** OPEN — accreting from live runs
**Method:** observe-and-analyze. Extend the existing env-gated `FROG_KLASS_PROBE` (`diagnostics/klass_probe.rs`) to dump real `Il2CppClass` structs on live games, classify each pointer-slot by *what it points at*, and pin the real offsets — then stress-test the calibration's assumptions against that truth.
**Targets:** Pixel Worlds (obfuscated, field-FFI absent, metadata NOT FOUND) + Highrise (`com.pz.highrise`, modern Unity).
**Supersedes thinking in:** `specs/2026-05-30-bedrock-b1-probe-verify-design.md` (its fail-OPEN-to-baseline + candidate-window-near-baseline model is the rot — see [[calibration-silent-hardcode-rot]]).

---

## Run 1 — `Player` `Il2CppClass`, PW vs Highrise (FROG_KLASS_PROBE)

Decoded by structural classification of each pointer-slot:

| Offset | PW `Player` @0x31a83130-era | Highrise `Player` @0x31a83130 | Field (inferred) |
|---|---|---|---|
| `+0x00` | → "Assembly-CSharp.dll" | → "com.pz.highrise.game.cli…" | `image` |
| `+0x08` | 0 | deadptr/flags | (gc/init) |
| `+0x10` | = "Player" | = "Player" | **`name` = 0x10** |
| `+0x18` | = "" (empty) | = "Highrise.Client" | **`namespace` = 0x18** ✓ |
| `+0x20` | RO Il2CppType-ish | RO Il2CppType-ish | byval_arg/this_arg |
| `+0x28` | → "MZ" (PE header) | → "MZ" (PE header) | (module ptr) |
| `+0x30` | = `+0x20` | = `+0x20` | this_arg (pairs byval) |
| `+0x40`,`+0x48` | self-ptr | self-ptr | element_class/castClass |
| `+0x80` | → FieldInfo[0].name "myGameObject" | → FieldInfo[0].name "_self" | **`fields` = 0x80** ✓ |
| `+0x98` | ptr-array, **0x58 stride** | ptr-array, **0x58 stride** | **`methods` = 0x98** ✓ |
| `+0xa8` | → 0x6ffff… runtime region | → 0x319… (RW) | static_fields? (UNCONFIRMED) |

**Structural recognizers that worked (zero candidates, pure walk):**
- `name` / `namespace`: slot → readable cstr; `namespace` is the one that can be empty.
- `fields`: slot → array where `entry[0]` leads to a real field-name cstr.
- `methods`: slot → array of pointers at a constant **0x58 stride** (the MethodInfo block); each entry is a MethodInfo.
- self-pointers (`element_class`/`castClass`) literally equal the klass address — a free anchor.

---

## KEY FINDING #1 — the "Highrise shifted its layout" assumption is REFUTED (for Player)

PW and Highrise `Player` have **identical** `name=0x10, namespace=0x18, fields=0x80, methods=0x98`. The earlier story ("v30 moved `byval_arg` to a pointer, everything shifted, `0x98` is wrong on Highrise") does **not** hold for `Player`. `methods=0x98` is *correct* on Highrise, yet the calibration scored `klass_methods` **0/50** and fell back to `0x98` — i.e. it landed on the right value *for the wrong reason*, and on a game where methods truly differed it would have been silently wrong.

## KEY FINDING #2 — the probe failures are FALSE NEGATIVES from a cascade, not window misses

`0x98` was *in* the candidate window `[0x88,0x90,0x98,0xA0,0xA8]` and is the *correct* offset, but the validator rejected it. Hypothesis (strongly supported, to be confirmed by dumping the MethodInfo structs): the `klass_methods` validator checks "`methodPointer` at `MethodInfo+0x08` is executable," but `method_pointer_off` is **not 0x08** — PW's own log shows `PROBE OVERRIDE: method_pointer_off → 0x0`. The validator walks the correct array, reads the wrong sub-offset for the code pointer, sees non-code bytes, and rejects every anchor → 0/50 → silent fallback.

**Implication for the redesign:** the disease is **brittle, inter-dependent validation with hardcoded sub-offsets**, not (only) narrow candidate windows or version tables. A discovery-first calibrator must:
1. recognize structures by **intrinsic, dependency-free invariants** (e.g. "array of pointers at a constant stride into a contiguous block" identifies the methods array without needing `method_pointer_off` first), and
2. **distinguish "offset wrong" from "my validator is wrong"** — a 0/50 must not silently become a baseline constant ([[stage-bedrock-principle]] fail-closed).

## FINDING #3 — the instrument is not crash-safe (PW crashed)

PW crashed during the probe; Highrise survived. The probe follows pointers (the `t0→` transitive decode, and the `FROG_MEMBER_PROBE` MethodInfo walk) **without routing through `RegionMap`**, so an unmapped/`deadptr`/PE-header pointer segfaults the target. A probe that crashes the game is non-viable for a sweep — and crash-safe reads are themselves bedrock.

## FINDING #4 — find-by-name is hardcoded

Highrise `GameManager/World/PlayerData` → `NOT FOUND`: the probe looks up class names that don't exist in that game. The sweep must select classes structurally (walk the table), not by a hardcoded name list.

---

## Open items / next observations needed
- [ ] **Confirm the cascade:** dump the `MethodInfo` structs of the `+0x98` array on both games; read `method_pointer_off` per game; show the `klass_methods` validator's 0x08 assumption fails on the correct array.
- [ ] **Sweep a diverse sample** (named / fields / methods / value type / ref type / static fields / generic), ~15 classes/game, to confirm the layout-match holds *beyond* `Player` (or find where it doesn't).
- [ ] **`static_fields` (`+0xa8`?) and `type_def`:** confirm real offsets; PW `type_def` failure is expected (metadata NOT FOUND) — verify that's the real reason, not a window miss.
- [ ] **Crash-harden** the instrument (every follow via `RegionMap`, depth/width bounded) before any sweep.
- [ ] **Structural class selection** (walk the table) instead of hardcoded names.

## Working verdict (provisional, 1 class observed)
Discovery-first looks **viable and necessary**: the offsets are findable by intrinsic structure (stride-array = methods, field-name-array = fields, self-ptr anchors), and the current probe's failures trace to dependency cascades + silent fallback rather than to anything unknowable. Next runs must confirm across a diverse sample before committing to the redesign.

---

## Diagnostic Architecture — what bedrock actually needs (assumption autopsy)

The investigation almost repeated the original sin: deepen ONE probe (klass struct) and trust it. The assumptions that do NOT survive the real games:

- **Bad assumption A — "validate against FFI / known-class ground truth."** On the games that matter there is **no answer key** (PW: metadata NOT FOUND, field-FFI ABSENT; obfuscated builds strip both). Ground truth must come from **self-consistency / triangulation**: N independent structural derivations of a fact must AGREE; disagreement = loud `UNRESOLVED`, never silent fallback. FFI, when present, is one more witness to cross-check — not the oracle.
- **Bad assumption B — "the offset is the unit of truth."** The unit is the **derivation path**. The cascade bug (`klass_methods` rejected via un-calibrated `method_pointer_off`) proves validation must be **non-circular**: each fact from raw bytes + intrinsic structure only.
- **Bad assumption C — "a probe that runs = a probe that measures."** PW crashed → region knowledge is incomplete, and a perturbing/crashing diagnostic isn't measuring the undisturbed system. Crash-safety is a **region-completeness test**.
- **Bad assumption D — "emitted = complete."** PW: 7077 populated klasses (ns_ok=7077) but only **2496 dumped** — ~4581 unaccounted. Maybe legitimate (no-field classes skipped), maybe a 65% silent hole. We don't know, because **nothing measures coverage.** Silence is read as success.

### The suite (each diagnostic: FFI-optional, non-circular, crash-safe, coverage-reporting)
1. **Root-integrity** — class-table base/count correct & complete; every slot klass-shaped; independent sig paths agree.
2. **Region-coverage** — every pointer a structural walk reaches is in a known region or correctly flagged; crash-impossible. (Would have prevented the PW crash.)
3. **Structural-offset discovery** — each offset from intrinsic invariants; multiple recognizers per offset that must agree; zero cross-offset dependency.
4. **Self-consistency invariants** (no-answer-key truth test) — field offsets monotonic & < instance size; method stride constant; name/ns cstrs valid; tc round-trips for known primitive shapes; self-pointers present.
5. **FFI cross-check** (when present) — call-and-verify AND compare to the structural derivation; mismatch names which witness lies.
6. **Temporal stability** — re-probe over the game's life: GC movement, table growth, drift. (Extend `mem_probe`.)
7. **Coverage accountant** — % classes/fields/methods resolved vs dropped, with the reason for EVERY drop.

### The principle
Bedrock is stable not when one probe says `0x98`, but when **independent witnesses agree and every disagreement and every drop is surfaced loudly.** No oracle, no silent fallback. The current calibration has one witness (a candidate-window probe with circular validation) and treats its silence as truth — that is the architecture flaw, beneath the offset numbers.

### Open architectural question
Which witnesses are *cheap and independent enough* to be the minimal trustworthy set on a no-FFI/no-metadata game? (Triangulation needs ≥2 genuinely independent derivations per fact — enumerate them per offset before building.)

---

## Foundation flaws — shared blind spots that make naïve triangulation a lie

The seven witnesses are NOT flat or independent — they sit on a foundation stack, and several share hidden common-mode failures. A bad foundation makes every witness agree on garbage ("self-consistently wrong").

**Foundation stack:** `class_table_step (stride) ⊃ root/table base+count ⊃ RegionMap (PE-parser) ⊃ every klass we read ⊃ the sample ⊃ all offset/consistency witnesses`.

| # | Flaw (shared blind spot) | Why it's invisible from inside | Orthogonal cross-check (different MECHANISM) |
|---|---|---|---|
| **A** | **Stride** (`class_table_step`) feeds every klass pointer. Wrong stride (too large) is indistinguishable from a sparse table — `7077/18515 = 38%` fits both "correct stride, sparse" and "wrong stride, every 2.6th entry." | You can't detect a wrong stride from within the stride — it's self-referential; mis-strided klasses can still look individually valid. | **Period autocorrelation:** read the table at 8-byte granularity, detect the recurrence period of "is-klass-pointer." The period *derives* the stride without assuming it. |
| **B** | **Per-kind layout variance.** Value/generic/array/enum classes may not share `Player`'s (ref-type) arrangement. A real variant masquerades as a probe "failure." (Belief: il2cpp *header* fields are uniform; variance is in trailing `vtable[]`, null-ness of `element_class`/`generic_class`, valuetype bit — **must be tested, not assumed.**) | Without a type classifier, "Player says fields@0x80, Int32 says 0x88" is indistinguishable from a misread. | **Type-classify before compare:** label each sample (ref/value/generic/array/enum) and compare offsets *within* a kind; uniformity becomes a measured result, not an assumption. |
| **C** | **RegionMap + sig-scanner share the PE parser** + memory enumeration. Obfuscated `MZ` headers → a parser misread is "confirmed" by probes landing in the mis-classified region. | Both foundation and probe agree because they consume the same parser output. | **Raw page-walk** (`/proc/<pid>/maps` under Proton, or `VirtualQuery`) classifies regions by *kernel page permissions*, bypassing the PE parser entirely. Disagreement = parser is lying. |
| **D** | **Temporal:** class structs are metadata-region (pinned, stable); *instances* are GC-movable. Snapshot ≠ lifetime truth; classes also load after the stability window. | A startup snapshot looks complete. | Re-probe over time (witness #6); separate the pinned-class layer from the movable-instance layer explicitly. |
| **E** | **Sample drawn THROUGH the foundation.** Every swept class is found via table+stride+RegionMap. If the foundation is wrong, the whole sample is misread identically → self-consistently wrong → false confidence. | Intra-sample agreement cannot validate the foundation the sample was drawn through. | **Out-of-band anchor:** locate ≥1 klass by a mechanism entirely outside table+stride+PE — e.g. back-derive from a known managed string constant ("mscorlib"/a known type name) on the heap — then check it appears in the table at the predicted stride and its region matches the page-walk. Disagreement = foundation corrupt. |

**Revised minimal trustworthy set:** not seven flat witnesses, but **the foundations (stride, root, region) each validated by a mechanically-orthogonal cross-check, + one out-of-band anchor** — and ONLY then do the offset/self-consistency witnesses carry weight.

## Next artifact
The **triangulation map**: for the root and each Phase-1/2 offset, list the ≥2 derivations that establish it, tag the foundation each shares, and mark which need an orthogonal-mechanism witness (per the table above). Build the foundation cross-checks FIRST; everything downstream inherits their honesty.
