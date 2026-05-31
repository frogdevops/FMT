# B-6b — Internal API Completeness (substrate work, not polish)

**Date:** 2026-05-31
**Status:** Brainstorm complete; ready for plan
**Predecessor:** B-6a Hot Reload (shipped 2026-05-31)
**Successor:** BYOL demo (was originally B-6b; deferred to after this brick)
**Reframes:** The dump-expansion proposal at `docs/proposals/2026-05-31-dump-accuracy-expansion.md` — its substrate-relevant pieces are now bedrock work, not operator polish.

## Motivation — what we discovered during B-6b brainstorm

The original B-6b ("BYOL demo across C/C++/Go") hit a wall the moment we tried to design the first script: `PlayerData.gems` is an instance field, and the substrate today exposes no way to enumerate live instances of a klass. The agent's Internal API has `find_class`, `find_method`, `field_info`, `static_field`, `get_field`, `klass_of`, `invoke_method`. It does NOT have:

- Static-vs-instance field discrimination on `field_info` returns
- Live instance enumeration (`instances_of(klass)`)
- Method listing per class (`methods_of(klass)`)
- Honest field counts (current 256-sample cap with no count surface)

This isn't a feature gap — it's a substrate gap that cascades:

- **External + Internal + Spine + Dumper are stacked**, not independent (per [[stacked-substrate-failure-cascades]]). When one is incomplete, all are. External's precision (pinpoint memory I/O via typed dispatch) is wasted if the discovery layer above it can't tell scripts WHAT to target.
- **The dumper is the canary** (per [[dumper-is-the-canary]]) — its output IS the substrate's exposed surface. When it can't show static markers / live counts / method lists, that's not "the dumper is incomplete" — it's "the substrate has no truth to surface." Trust in the dumper IS trust in the project; failure marks the project dead.
- **Internal is the hotspot.** External does plain memory I/O — failure = wrong bytes. Internal does invoke / hook / method dispatch — failure = game state corruption, scheduler reentrancy violations. The highest-stakes domain.
- **Shipping Protocol (B-7) on this incomplete Spine would compound the immaturity.** Spine would grow ad-hoc types alongside its existing surface; the substrate becomes two-headed; the cascade gets worse.

**The right work is to complete the substrate before any new capability lands.** That's this brick.

## Locked decisions (from 2026-05-31 brainstorm)

| # | Decision | Rationale |
|---|---|---|
| 1 | One flattened brick, not 5 sub-bricks | User: "must know that this one might be good flattened and optimize making sure this one is not just another if-state-machine". Substrate work cohesive as one effort. |
| 2 | Cleanup pass embedded inside the brick (not after) | User: "this is also where i want to put the rechecking code optimizing code quality refractions stripping processes". Floor consistency before new weight. |
| 3 | Medium cleanup scope | Tight under-delivers on stripping; Wide rabbit-holes into structural audits. Medium = the 11 pre-existing warnings + audited debt items, each per-item-decided. |
| 4 | Separate `scan_backend.rs` for `Iter<Instance>` | Scan-based discovery has different lifecycle semantics from structural field/method walks (which use already-calibrated klass offsets). Same iterator pattern in `access.rs`; separate backend vtable, like `mem_backend` and `metadata_backend` already do. |
| 5 | Dumper is downstream Phase 4, not co-equal work | Dumper rewrites as a thin serializer of substrate primitives. When substrate gains a primitive, dumper auto-shows it. |
| 6 | No speculative Spine pre-design for Protocol | Earlier B-6e proposal cut. B-7's brainstorm will define Spine needs honestly. Don't grow speculative surface now. |
| 7 | Instance validation: structural inside iterator, NOT per-klass | Spine pattern (per `Iter<FieldInfo>`): validate inside the iterator so the yielded type MEANS the thing. NO per-klass branching (that's the if-state-machine the user warned against). Universal structural checks only. |
| 8 | Iterators are LAZY | Mirror existing `Iter<FieldInfo>`. Cheap when callers want `.next()` for first match. Eager-collect available via `.collect()` for the dumper. |

## Architecture — one brick, five phases

```
Phase 1: Spine extensions
       ↓ (types + traits — the contract)
Phase 2: Internal implementations
       ↓ (memory walks via existing backends)
Phase 3: WASM host API
       ↓ (exposure to scripts)
Phase 4: Dumper rewrite
       ↓ (canary surface = visible substrate truth)
Phase 5: Cleanup pass
       (medium scope — itemized below)
```

Phases are sequential within the brick. Each can be independently verified by the build + Windows cross-compile + agent-core unit tests. Phase 5 happens after Phases 1-4 because it addresses debt those phases surface.

---

## Phase 1 — Spine extensions

**File touches:**
- Modify: `crates/agent-core/src/spine/field_info.rs` — add `is_static: bool`
- Modify: `crates/agent-core/src/spine/access.rs` — add `Iter<MethodPtr> for KlassPtr`, add `Iter<Instance> for KlassPtr`
- Create: `crates/agent-core/src/spine/scan_backend.rs` — vtable for instance-discovery backend (separate from `metadata_backend` per decision #4)
- Modify: `crates/agent-core/src/spine/mod.rs` — re-exports

### 1a. `FieldInfo.is_static: bool`

```rust
// crates/agent-core/src/spine/field_info.rs (additive)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldInfo {
    pub name_ptr: usize,
    pub offset:   u32,
    pub val_type: ValType,
    pub token:    u32,
    pub is_static: bool,  // NEW
}
```

Additive. The static bit comes from `chunk & 0x10` (FIELD_ATTRIBUTE_STATIC) which `static_field` (in `internals/api.rs`) already reads but discards. Phase 2 surfaces it through `for_each_field`.

### 1b. `Iter<MethodPtr> for KlassPtr`

Mirror the existing `Iter<FieldInfo> for KlassPtr` shape exactly. Backed by `metadata_backend::methods_at(klass, cursor)` (new fn in metadata_backend, analogous to existing `fields_at`). Yields `MethodPtr` newtype.

```rust
// crates/agent-core/src/spine/access.rs (additive)
pub struct MethodPtrIter { klass: usize, cursor: usize, limit: usize }

impl Iterator for MethodPtrIter {
    type Item = MethodPtr;
    fn next(&mut self) -> Option<MethodPtr> {
        if self.cursor >= self.limit { return None; }
        let raw = metadata_backend::methods_at(self.klass, self.cursor)?;
        self.cursor = raw.next_cursor;
        Some(MethodPtr::from_raw(raw.method_ptr))
    }
}

impl Iter<MethodPtr> for KlassPtr {
    type Iter = MethodPtrIter;
    fn iter(&self) -> Self::Iter {
        MethodPtrIter {
            klass: self.as_u64() as usize,
            cursor: 0,
            limit: MAX_METHODS_PER_CLASS,
        }
    }
}
```

Internal validation lives in `metadata_backend::methods_at` (skip garbage, validate methodPointer is in RX region, etc.) — same pattern as `fields_at`.

### 1c. `Iter<Instance> for KlassPtr` via `scan_backend`

Different lifecycle: scan-based, not structural walk. Lives in same `access.rs` for trait-impl ergonomics but backed by separate `scan_backend.rs` vtable.

```rust
// crates/agent-core/src/spine/access.rs (additive)
pub struct InstanceIter { klass: usize, scan_cursor: usize, validated_yield: bool }

impl Iterator for InstanceIter {
    type Item = Instance;
    fn next(&mut self) -> Option<Instance> {
        loop {
            let candidate = scan_backend::next_match(self.klass, &mut self.scan_cursor)?;
            // Structural validation, NO per-klass branching:
            if !scan_backend::passes_structural_validation(candidate, self.klass) { continue; }
            return Some(Instance::from_raw(candidate as u64));
        }
    }
}

impl Iter<Instance> for KlassPtr {
    type Iter = InstanceIter;
    fn iter(&self) -> Self::Iter {
        InstanceIter {
            klass: self.as_u64() as usize,
            scan_cursor: 0,
            validated_yield: false,
        }
    }
}
```

`scan_backend::passes_structural_validation(addr, target_klass)` checks:
1. Address is in a writable region (filter via mem regions — excludes RX code, RO data)
2. Address is aligned to pointer size (8 on x86_64)
3. `klass_of(addr) == target_klass`
4. The klass at `addr+0` passes `is_klass_shape()` (its name/namespace pointers are valid cstrs in mapped memory)

All four are universal — same code path for every klass. No per-klass field-shape lookup. No if-this-class-special-case. Mirrors how `Iter<FieldInfo>` does structural validation inside `metadata_backend::fields_at`.

### 1d. Scan backend vtable

```rust
// crates/agent-core/src/spine/scan_backend.rs (new)
pub type NextMatchFn = unsafe fn(target_klass: usize, cursor: &mut usize) -> Option<usize>;
pub type ValidationFn = unsafe fn(addr: usize, target_klass: usize) -> bool;

// Statics + register() function follow the mem_backend pattern (Task 3 of B-6a's registry).
// agent crate registers Windows-backed implementations at startup.
```

The agent-side implementation (Phase 2) uses `external::scan::aob_scan` for the underlying scan (already exists, exposed via `mem.scan` host fn) plus the validation logic.

---

## Phase 2 — Internal implementations

**File touches:**
- Modify: `crates/agent/src/internals/api.rs` — surface `is_static` via `field_info` and `for_each_field`; add `methods_of`; add `instances_of`; remove or honest-cap field walks
- Modify: `crates/agent/src/internals/api.rs::register_metadata_backend` — wire `methods_at` (Phase 1's new fn)
- Create or modify scan_backend registration — wire `scan_backend::register(...)` at startup alongside existing backends

### 2a. Static-bit through `field_info`

Current `field_info` returns `Option<(u32, ValType)>`. Change to `Option<(u32, ValType, bool)>` (third is `is_static`). `for_each_field` reads `chunk & 0x10` inline (it already does this for static_field's filter — refactor to share).

ABI impact: `host_field_info` in `mem_host.rs` packs into i64. Phase 3 extends the packing format.

### 2b. `methods_of(klass: KlassPtr) -> Vec<MethodPtr>`

New fn in `internals/api.rs`. Walks the klass's methods array via the existing offsets (`klass_methods`, `method_klass_off` as termination sentinel). Same pattern as `for_each_field`. Returns `Vec<MethodPtr>` via the new `Iter<MethodPtr>` (or wraps it).

### 2c. `instances_of(klass: KlassPtr) -> impl Iterator<Item = Instance>`

New fn in `internals/api.rs`. Builds the scan pattern from `klass.as_u64().to_le_bytes()` (8 bytes — the klass pointer as little-endian u64). Returns the `Iter<Instance>` from Phase 1.

The agent-side scan backend wires `aob_scan` + validation:

```rust
// Sketch — Phase 2 wiring
unsafe fn scan_next_match(target_klass: usize, cursor: &mut usize) -> Option<usize> {
    // On first call (cursor == 0): perform the scan, store results, return first
    // On subsequent calls: return next stored result
    // Pre-filter scan to writable regions only (skip RX/RO)
}

unsafe fn validation_passes(addr: usize, target_klass: usize) -> bool {
    // 1. Writable region check (already filtered at scan time; redundant safety)
    // 2. Alignment: (addr & 7) == 0
    // 3. klass_of(addr) == target_klass
    // 4. is_klass_shape(klass_of_result)
}
```

### 2d. Field walk honesty

Today: 256-sample cap in `for_each_field` with no count surface. Decision: keep the cap (defensive — real classes don't have 256+ fields, but cap prevents infinite walk on garbage), BUT surface a "capped_at" signal in the iterator metadata. The dumper emits `(N fields)` honestly OR `(N fields, capped at 256)` if the cap fired.

---

## Phase 3 — WASM host API

**File touches:**
- Modify: `crates/agent/src/runtime/mem_host.rs` — extend `host_field_info` return; add `host_list_methods`, `host_list_instances`

### 3a. Extended `field_info` return — breaking ABI change

Current packed format: `((vt as u8 as i64) << 32) | (offset as i64)`. New format:

```
bits  0-31:  offset (u32)
bits 32-39:  ValType tag (u8)
bit  40:     is_static (1 bit)
bits 41-63:  reserved (zero)
```

Still fits in i64. Existing scripts that decoded as `(packed & 0xFFFFFFFF, (packed >> 32) & 0xFF)` continue to work (they'd just ignore the static bit). New scripts can also decode bit 40.

This IS a breaking change in the sense that the spec's old contract has changed, but in practice no script today depends on bits 40+. We control all in-tree wasm scripts; verify each rebuilds cleanly.

### 3b. `il2cpp.list_methods(klass: i64, out_buf: i32, out_cap_count: i32) -> i32`

Pattern follows existing `host_scan` / `host_regions`:
- Caller provides buffer + capacity (in entries, not bytes).
- Host writes packed entries to buffer. Each entry: `MethodPtr as u64 le-bytes` (8 bytes per entry).
- Returns count written, or negative status code on error.

### 3c. `il2cpp.list_instances(klass: i64, out_buf: i32, out_cap_count: i32) -> i32`

Same pattern as `list_methods`. Each entry: `Instance as u64 le-bytes`. Iterator is lazy on the Rust side but eager-collected up to `out_cap_count` for the host fn return.

---

## Phase 4 — Dumper rewrite as substrate output

**File touches:**
- Modify: `crates/agent/src/internals/dump.rs` — replace ad-hoc walks with Spine iterator calls

Dump format additions:

```
ClassName (N fields, M methods, K live instances):
    static someField: System.Int32 // Offset: 0x0, Token: ...     ← NEW: "static " prefix
    instanceField: System.String // Offset: 0x10, Token: ...
    ... (capped at 256 if hit)
    methods:                                                       ← NEW section
      MethodA(2 args) → System.Int32 [static]
      MethodB(0 args) → System.Void
      ... (capped at MAX_METHODS_PER_CLASS if hit)
    live instances: [0x12345678, 0x12345700, ...] (10 of K)        ← NEW section
```

Implementation:
- Per-class field walk: use `KlassPtr.iter::<FieldInfo>()`. Emit `static ` prefix when `is_static`.
- Per-class method walk: use `KlassPtr.iter::<MethodPtr>()`. Resolve name/argc/return type via existing helpers.
- Per-class instance enumeration: use `KlassPtr.iter::<Instance>().take(10)` for display + `.count()` (or estimated count) for total.

Dumper has NO new walk logic — only serialization. When substrate gains a primitive, dumper gains visibility automatically.

---

## Phase 5 — Cleanup pass (medium scope) — **investigate-intent philosophy**

**The default isn't "delete if unused." It's "investigate intent, wire to original purpose where that's possible, only delete when proven vestigial."** Code that's holding to itself exists for a reason. Removing without addressing the reason hides design intent. Per the 2026-05-31 user correction during brainstorm: *"Cleaning up because nothing uses it means wrong a cleanup is even needed if that function is really holding to itself."*

Each item per-item-investigated. The 11 pre-existing warnings:

| Item | Decision | Reason |
|---|---|---|
| `Hook.detour` field never read (inline_detour.rs:12) | **Investigate:** likely the detour-jmp target address used for restore validation. If yes, **WIRE** it as part of unhook safety check (verify bytes we wrote are still there before restore). Don't delete on surface "no callers." | Field name `detour` suggests intent; unused doesn't mean vestigial. |
| `MIN_RATIO` const unused (calibration/field_param_layout.rs:11) | **Investigate:** calibration probe threshold. Find which probe code path SHOULD reference it and **WIRE** there. Only delete (with documenting comment) if truly no probe needs it post-investigation. | Calibration constants don't appear by accident. |
| `Verified::Crashed` variant never constructed (ffi_verify.rs:12) | **Investigate:** designed for FFI verify "crashed" outcomes. If never constructed, FFI crash detection (SEH on Windows / signal on Linux-test) was never wired. **WIRE** the construction path (substrate work, not cleanup) OR document why we accept "not detecting crashes" as the current trade. | Variant exists because the design intended to detect this case. |
| `field_addr` never used (internals/api.rs) | KEEP with retained `#[allow(dead_code)]` | Load-bearing typed-API per [[spec2-domain-audit-and-cleanup]]. |
| `METHOD_ATTRIBUTE_STATIC_BIT` const unused (marshal.rs:260) | WIRE into Phase 1's static bit detection (becomes the named constant for `0x10`) | Replaces magic number with named const; turns dead code live. |
| `ParkedRuntime.instance` field never read | KEEP with `#[allow(dead_code)]` + comment | Load-bearing per B-6a Task 11 review — keeps wasmi Instance alive for funcref_table lifetime. |
| `unnecessary unsafe block` (calibration/ffi_verify.rs:136) | **Investigate:** if the `unsafe` was placed defensively (wrapping FFI that was later made safe by a wrapper), it may signal a stale safety contract. Read surrounding code. Remove only if structurally unnecessary, not just because compiler flagged. | Compiler "unnecessary" doesn't mean "originally pointless." |
| `mut caller` unused on host_hook_set_arg (mem_host.rs:322) | Remove `mut` | Already a B-5 pre-existing concern; trivial fix. |
| `mut caller` unused on host_hook_set_return (mem_host.rs:348) | Remove `mut` | Same. |
| `chunk` unused var (field_param_layout.rs:63) | Underscore-prefix or use it | Trivial. |
| `unused doc comment` (hook_runtime/shim.rs:66) | Fix the comment association | Trivial. |

Plus audited debt items:

| Item | Decision | Reason |
|---|---|---|
| `marshal.rs` 4 `last_mut().unwrap()` calls | **Audit invariant first.** Trace each push site to verify the slab invariant ("there's always a current slab pushed when we call last_mut") is provably inviolable. If yes: `debug_assert!` + comment documenting the proof. If can be violated: propagate via `Result`. Don't blindly pick a style. | Banked debt per [[codebase-audit-findings]] — addressing requires proof, not preference. |
| `calibrate_generic_class_offset` log-without-store | **Reversal of earlier deferral.** Comment claims "Generic context not needed — VAR/MVAR resolution works without it." That rationale may be stale. **Audit** generic field/method paths to verify VAR/MVAR resolution actually works without this calibration. If audit confirms: delete WITH a doc-comment naming the references that prove sufficiency. If audit finds silent failures: WIRE the calibration to PROBED_GC_OFF as the original proposal suggested. | The "intent" of this function deserves audit, not deferral. |

Out of cleanup scope (deferred to potential Wide-scope brick):
- Module structure pattern audit (`api.rs` vs not, etc.)
- Unsafe-block boundary audit
- Error-handling convention audit
- Calibration phase ordering review

---

## What B-6b explicitly does NOT fix (substrate-stability gaps deferred to B-6c/d/e)

These gaps are real, code-verified during 2026-05-31 brainstorm, and substrate-stability concerns — **but they exceed B-6b's scope.** Listed here so they remain visible, not hidden:

| Gap | Severity | Code citation | Why not in B-6b | Target brick |
|---|---|---|---|---|
| **4-arg hook register limit.** Only rcx/rdx/r8/r9 + xmm0-3 captured; methods with >4 params can't see args 5+. `stack_args: *const u64` is captured but unread. | Medium | `regargs.rs:11-16` | Needs shim asm extension + RegArgs layout change + per-arg-physical-loc API. Disjoint from dump/visibility work. | B-6d |
| **`try_lock` REGISTRY silently drops hooks under contention.** `call_hook_handler` uses `try_lock`; if contended, returns Err and transparent observer fires. No telemetry signals the dropped hook. | Medium | `host.rs::call_hook_handler` (post-B-6a) | Needs either telemetry surface or sync model redesign. Substrate concern but disjoint from completeness. | B-6c |
| **`expect("context underflow")` panic vector on game thread.** Push/pop pairing bug crashes the process. | High | `api.rs:85` (`with_current_context`) | Needs audit (prove invariant holds) or Result-ify. Safety work, not visibility work. | B-6c |
| **No per-arg type API.** `hook_arg(i)` returns raw bytes; script must externally know the type. | Medium | No `hook_arg_type` fn in `mem_host.rs` | Needs ParamInfo walks per method + WASM host fn extension. Naturally with B-6d. | B-6d |
| **Protocol has NO WASM API surface.** SocketHandle / FrameSeq / FrameRing / RawFrame + `Iter<RawFrame>` all exist in agent-core spine. Zero `proto.*` host fns in `mem_host.rs`. Scripts cannot touch protocol state. | High | grep `"proto"` in `mem_host.rs` returns nothing | Distinct substrate work — Protocol domain hasn't shipped its WASM surface yet. | B-6e |
| **`MemAddr<Proto>` capability missing.** Passing a `SocketHandle` to `mem.write` compiles fine because the type system has no proto-vs-general distinction at the address level. | Medium | spine/addr.rs — only `ReadOnly`/`ReadWrite` capability markers | Cross-domain Spine work, naturally with B-6e. | B-6e |
| **`Value` ↔ `InvokeArg` friction.** They share tag space 0-11 but every External↔Internal pipe needs manual `InvokeArg::Prim(v)` conversion. | Low | `mem_value.rs::Value` vs `spine/invoke_arg.rs::InvokeArg` | Spine unification work. Naturally with B-6e. | B-6e |

**The base isn't stable at end of B-6b.** B-6b makes the substrate honestly DESCRIBE itself (dump = truth); B-6c-d-e harden it to actually BEAR capabilities. Hiding these in vague "future improvements" would be the trap we already escaped — making them explicit + sequenced is the honest path.

---

## Revised sequencing (substrate stability is a multi-brick arc)

| | |
|---|---|
| ✅ B-6a | Hot reload (shipped) |
| **⏭ B-6b (this spec)** | Internal API Completeness + Dumper truthfulness + investigated cleanup |
| ⏭ B-6c | **Substrate-stability hardening.** Audit `expect("context underflow")` (Result-ify or prove invariant); address REGISTRY contention silent-drop (telemetry minimum); investigate `Verified::Crashed` wiring (FFI crash detection); investigate `Hook.detour` wiring (unhook safety check). Items surfaced by B-6b's investigated cleanup that exceed B-6b's scope land here. |
| ⏭ B-6d | **Hook arg surface completeness.** RegArgs extension for >4 args (stack-args reading); per-arg type API (`hook_arg_type(i) → ValType` via ParamInfo walks); paired WASM host fn extensions. |
| ⏭ B-6e | **Spine cross-domain unification.** `MemAddr<Proto>` capability; Value↔InvokeArg shared encoding; Protocol WASM API surface (`proto.poll`, `proto.send`, `proto.iter_frames`, etc.). |
| Then | BYOL demo (C/C++/Go) on now-stable substrate |
| Then | Rigorous stress test |
| Then | B-7 In-flight Modify (capability brick on stable bedrock) |
| Then | B-8 and beyond |

**4 bedrock bricks (B-6b through B-6e) before any capability work.** Substantial — but accurately scoped to what code-level verification revealed.

---

## Out of scope

- **BYOL demo across C/C++/Go** — was originally B-6b; moves to AFTER this brick because it needs substrate completeness to land cleanly. Per the locked priority sequence post-B-6b: BYOL → stress test → B-7 → B-8.
- **C# language demo** — separately deferred per the dump-expansion-proposal context.
- **Per-klass shape validation for `Iter<Instance>`** — would introduce per-klass branching (the if-state-machine the user warned against). Scripts can compose `.filter(|inst| custom_check(inst))` if they need stricter validation.
- **B-7 Protocol API + Spine extensions for protocol-domain types** — depend on this brick; no speculative pre-design now.
- **Frontend / live dashboard** — long-arc per [[frog-real-time-runtime-vision]], separate concern.
- **Wide-scope structural cleanup** — module/unsafe/error pattern audits banked for a future brick if needed.
- **Name de-obfuscation** — banked B-2d per [[spec2-domain-audit-and-cleanup]]; far-future.

---

## Acceptance criteria

1. **All 5 phases land in one cohesive brick** — single ship, not staggered.
2. **Build clean** — `cargo build --target x86_64-pc-windows-gnu --release` succeeds. Warning count REDUCED from the 10-baseline (Phase 5 cleanup removes some).
3. **agent-core tests pass** — including any new ones added for Phase 1 (FieldInfo additive field, MethodPtr iterator skeleton, scan_backend mock-tested with fake region/scan backend).
4. **Live verification on PW** — drop a wasm script that:
   - Calls `il2cpp.list_methods(playerData_klass, ...)` → returns plausible count (≥ 5 methods)
   - Calls `il2cpp.list_instances(playerData_klass, ...)` → returns ≥ 1 instance with the game running
   - Calls extended `field_info` → returns a known static field (e.g. on `SteamManager.s_instance` per the dump) with `is_static` bit set
5. **internals.txt visibly shows** — per-field `static ` prefix where applicable; per-class method listing; per-class live instance count (with first N addresses); honest field-count-with-cap-signal.
6. **No regression** — B-5 typed dispatch + B-6a hot reload behaviors preserved; existing wasm scripts that don't decode the new field_info bit continue working unchanged.

## Risks

| Risk | Mitigation |
|---|---|
| **Instance scan performance** — first call could be 100s of ms for a klass with many possible matches across the heap. | Bounded scan range (writable regions only); lazy iteration so `.take(1)` is cheap. If perf bites, can add scan-result caching as a separate brick. |
| **Field walk cap (256) becomes wrong for some Unity version** — silently truncates without honest signal. | Phase 2d surfaces the cap-hit signal honestly. Future-proofs without removing the safety cap. |
| **ABI break on `field_info` return** — scripts depending on old packed format see static bit as extra noise. | All in-tree scripts rebuilt during this brick. Document the new packing in the WASM API surface notes. |
| **Cleanup pass surfaces unexpected dead-code chains** — removing one item reveals another was load-bearing for it. | Per-item review in Phase 5; revert any item whose deletion breaks the build; document with `#[allow(dead_code)]` if revealed as load-bearing. |
| **Scope creep mid-brick** — biggest brick since project start; risk of additional "while we're in here" work. | Phases are sequential within the brick; finish each before starting next; out-of-scope list above is the firm boundary. |

## Banked memories supporting this spec

- [[stacked-substrate-failure-cascades]] — the architectural truth this brick addresses
- [[dumper-is-the-canary]] — the corollary: dumper as substrate-output, not feature
- [[lead-through-traps-teaching]] — how the user surfaced these realizations
- [[bedrock-before-capability]] — the discipline that says substrate ships before capability
- [[wild-west-platform-philosophy]] — what we owe (substrate honesty), what we don't (per-klass rules)
- [[scripter-vs-modder-experience]] — the audience this substrate completeness serves
- [[frog-real-time-runtime-vision]] — the long arc this enables
- [[spec2-domain-audit-and-cleanup]] — the load-bearing typed-API pattern; the priority order
- [[codebase-audit-findings]] — Phase 5's debt items
- [[deploy-setup]] — Windows cross-compile testing discipline
- [[hooks-are-the-sync-primitive]] — context for why Internal is the hotspot

## Note on the deferred dump-expansion proposal

`docs/proposals/2026-05-31-dump-accuracy-expansion.md` was deferred 2026-05-31 with the framing "directionally aligned but premise predates B-6a." That framing was incomplete. The proposal's substrate-relevant pieces (static-field marking, method walks, complete walks) are now THIS BRICK'S Phase 1-4 work. The proposal's "PROBED_GC_OFF fix" remains rejected (it's a feature-decision-as-bug-fix, separately considered). The proposal can be superseded after this brick ships, with a note pointing at this spec.
