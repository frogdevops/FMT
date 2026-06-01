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

## VERIFICATION (code-checked 2026-06-01) — corrections to the flaw table

Reverified the foundation claims against source. Several flaws were over-stated; the real bug is narrower and the foundation is sounder than first feared.

- **Flaw A (stride) — DOWNGRADED.** `find_class_table` (scan.rs:74-127) scans at **8-byte granularity** (finest, = pointer alignment) and keeps every slot passing `class_fields`. It cannot skip an 8-aligned klass pointer; there is no coarse `class_table_step` to get wrong here. `38%` = correct stride, sparse table. (Residual nit only: a stride-16-with-null-padding table would inflate the slot *count*, never miss/misread a klass; confirm downstream table iteration reuses 8.)
- **Flaw C (shared PE parser) — COLLAPSES for the region map.** `RegionMap::capture` (region_map.rs:42-72) is **`VirtualQuery`-only** (kernel page state, no PE parsing) — it already IS the orthogonal page-walk. Flaw C applies ONLY to `find_types_array` / metadata-registration scans, NOT the table or region map. (Proton caveat: VirtualQuery is Wine-ntdll-serviced, seeded by Wine's loader but kept current by runtime VirtualProtect — independent of probe-time header re-parsing.)
- **Root locator — VERIFIED SOUND.** `class_fields` (region_map.rs:219-229) validates `klass+0 → Il2CppImage → name ".dll"` + cstr name@0x10 + ns@0x18. A run of ≥`min_classes` consecutive such slots cannot occur by chance. Strongest component in the pipeline. (image@0x00 / name@0x10 / namespace@0x18 confirmed identical on PW + Highrise; "invariant v16-v31" is external-knowledge, not proven here — treat as strong prior, not fact.)
- **`klass_static_fields` — NOT silent rot.** klass_layout.rs:~248 is honestly conservative: 0.99 threshold, "NO honest discriminator… falls back… honest about the lack of independent verification." Residual: the honesty **stops at the log**; the consumer reads `0xB8` with no unverified flag.

### CONFIRMED BUG — the `klass_methods` cascade (klass_layout.rs:243-245)
```rust
let method_pointer = map.read_u64(method_info_ptr + 0x08)?;   // HARDCODED 0x08
if method_pointer < 0x10_0000 { return None; }                // weak "is biggish" check
```
`method_pointer_off` is NOT 0x08 (PW probed it to 0x0, Phase 2, AFTER this runs). So:
- **Highrise false-FAIL:** `+0x08` is small/zero on the correct `0x98` array → 0/50 → fallback.
- **PW false-PASS:** `+0x08` holds some ptr ≥0x10_0000 for 46/50 by luck → "passes" without validating. `0x98`-correct-on-PW is coincidence.
Non-functional in both directions. (klass_static_fields' real-failure is the *absence of a discriminator*, a different, honestly-handled case.)

### The fix (non-circular, structural-only)
Recognize the **methods array** by intrinsic structure, zero calibrated sub-offsets: *a pointer array whose entries point to structs each containing ≥1 pointer into an **RX (execute) region*** — RX comes free from the confirmed-sound `VirtualQuery` map (we saw `[RX]` tags in the Highrise dump). Disambiguate from the **fields array** structurally: its entries point to structs whose first slot is a *name cstr* and contain *no* RX pointer. Rejected: reorder Phase-2-before-Phase-1 (moves the chicken-and-egg; Phase 2 method discovery may itself need `klass_methods`/absent FFI).

### Reframed scope
Foundation is SOUND (kernel-witnessed regions, strong `.dll`-image validator, finest-granularity stride, well-founded root). Rot is CONCENTRATED in: (1) Phase-1 probes reading hardcoded sub-offsets through weak thresholds (cascade), (2) unverified-status not propagating to consumers. Redesign shrinks from "rebuild calibration" → "replace cascading probes with non-circular structural recognizers + propagate unverified-status (fail-closed to consumers)."

## Triangulation Map

**The cascade-inversion principle:** find the CONTAINER by intrinsic structure, then DERIVE its sub-offsets by classifying the container's slots. Never assume a sub-offset to validate a container (that is the `klass_methods` bug). Dependency order: foundation (verified) → klass containers → sub-offsets-within-containers. Each fact: ≥2 independent witnesses; **agree → confident; disagree → `UNRESOLVED` (never silent fallback)**.

**Foundation (verified in code — see Verification section):** stride=8 (finest-granularity dense run), region map (VirtualQuery kernel witness), root validator (`.dll`-image + name + ns). Out-of-band anchor (1 heap-derived klass) cross-checks table-membership + stride-boundary + region-agreement in one datum.

| Fact | W1 (intrinsic) | W2 (independent) | Disagree → |
|---|---|---|---|
| `image` @0x00 | slot → struct → name ".dll" | value is **shared across many klasses** (low-cardinality); name/ns are unique | UNRESOLVED |
| `name` @0x10 | slot → readable cstr, **unique** per klass | FFI `class_get_name` (when present) matches | UNRESOLVED |
| `namespace` @0x18 | slot → cstr, **may be empty** (distinguisher from name) | FFI `class_get_namespace` matches | UNRESOLVED |
| **`fields`** @0x80 | slot → const-stride ptr array; entry→struct whose **first slot is a name-cstr**, **no RX ptr** | field-name cstrs all distinct & readable | UNRESOLVED |
| **`methods`** @0x98 | slot → const-stride ptr array; entry→struct containing **≥1 RX-region ptr** | same struct contains **≥1 ptr == owning klass** (back-ptr) | UNRESOLVED |
| `static_fields` @0xA8? | null for MOST klasses; when present → **unique RW data region** (not shared, not code) | (no honest 2nd witness on most klasses) | **UNRESOLVED + flag to consumers** — this is the honest floor; do NOT silent-fallback |
| `type_def` | → Il2CppTypeDefinition in metadata | (metadata) | **UNRESOLVED on no-metadata games** — dumper already routes via byval_arg/tc; never fake it |

**Sub-offsets via container inversion (kills the cascade):** once `methods` array is found intrinsically, derive — by scanning each MethodInfo's slots — `method_pointer_off` = the slot that is an **RX ptr** consistently across all methods; `method_klass_off` = the slot **== klass**; `method_name_off` = the slot → **readable cstr**. Same for FieldInfo (offset = small ascending int < instance_size; name = cstr; type = ptr into type region). No assumed sub-offset; each derived by classifying real slots of a container we already proved.

**type discriminator (`il2cpp_type_discrim_read_at`/`shift`):** anchor on known primitive klasses (Int32 tc=0x08, String tc=0x0E, …) via their byval_arg; the (read_at, shift) pair that makes ALL known primitives round-trip is the answer — multi-anchor consensus, intrinsic (no FFI). PW already passes this 5/5.

## RECOGNIZER PROOF (live, 2026-06-01) — discovery-first VALIDATED on both games

Built `FROG_RECOGNIZER_PROBE` (diagnostics/klass_probe.rs): container-first, crash-safe (RegionMap-only reads), klasses sampled structurally from the table (no find_class-by-name). Recognizes `methods` as "pointer-array whose first two entries are MethodInfo-shaped (≥1 RX ptr + ≥1 ptr == klass)" — zero candidate window, zero sub-offset assumption — then derives sub-offsets by classifying the MethodInfo's slots.

**Result — 12/12 klasses, unanimous, IDENTICAL across PW and Highrise:**
| | PW | Highrise |
|---|---|---|
| `methods_off` | **0x98 ×12** | **0x98 ×12** |
| `method_pointer_off` (DERIVED) | **0x0 ×12** | **0x0 ×12** |
| `method_klass_off` (DERIVED) | 0x20 ×12 | 0x20 ×12 |
| `method_name_off` (DERIVED) | 0x18 ×12 | 0x18 ×12 |

**Proven:** (1) the structural recognizer finds the methods array with no candidate window and no sub-offset assumption — unanimous, no ambiguity, no misses, on two different games; (2) it DERIVES `method_pointer_off = 0x0`, but `probe_klass_methods` hardcodes `+0x08` → reads the wrong slot → false-fail Highrise / false-pass PW — the **cascade bug confirmed in the open**; (3) the method sub-offsets are byte-identical PW≡Highrise — the "Highrise shifted its layout" story is dead at every level. Container-first non-circular discovery is real, not just elegant on paper.

## Next deliverable
Empirical green light given. Next: the **redesign spec** (supersedes bedrock-B1) — container-first non-circular discovery (find container by intrinsic structure → derive sub-offsets by slot classification), `UNRESOLVED` propagated to consumers (fail-closed, no silent baseline), coverage accounting — grounded in this map + the live proof. Then writing-plans → implementation. (static_fields/type_def remain the honest hard cases: no intrinsic discriminator on most klasses / no metadata on obfuscated games → must surface as UNRESOLVED, not faked.)
