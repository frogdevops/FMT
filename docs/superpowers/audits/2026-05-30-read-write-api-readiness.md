# Read+Write API Readiness Audit

**Date:** 2026-05-30
**Purpose:** Map the gap between today's read/write APIs and what In-flight Modify (priority #3) will need. Input artifact for #3's brainstorm.
**Scope:** Audit-only — no decisions, no implementation. Real design lands at #3 brainstorm.

---

## What's ready (read paths)

**WASM-side host fns** (all in `crates/agent/src/runtime/mem_host.rs`, registered in `run_wasm_with_mem`):

| Group | Host fn | Notes |
|---|---|---|
| Memory | `mem.read` | Bounds-checked typed read via `external::api::read` |
| Memory | `mem.scan` | AOB scan via `external::scan::aob_scan` (streaming) |
| Memory | `mem.regions` | List committed-readable regions |
| il2cpp | `il2cpp.find_class` | Walk class table by name |
| il2cpp | `il2cpp.field_info` | Lookup field metadata (offset + type) |
| il2cpp | `il2cpp.get_field` | Read field value at klass+offset |
| il2cpp | `il2cpp.klass_of` | Get klass from instance ptr |
| il2cpp | `il2cpp.static_field` | Address of static-field storage |
| il2cpp | `il2cpp.find_method` | Walk method array by name + argc |
| il2cpp | `il2cpp.invoke` | runtime_invoke with marshalled args |
| il2cpp | `il2cpp.install_hook` | Install handler funcref |
| il2cpp | `il2cpp.remove_hook` | Uninstall |
| il2cpp | `il2cpp.hook_arg` | Read current hook's arg by index |
| il2cpp | `il2cpp.hook_this` | Get instance ptr (or 0 for static) |
| il2cpp | `il2cpp.call_original` | Run trampoline from within handler |

**Rust-side typed siblings** (architectural contract — see [[spec2-domain-audit-and-cleanup]] memory: these are LOAD-BEARING, not dead code):

- `external::api::{read_t<T, C>, read_bytes_t<C>, read_cstr_t<C>}` — capability-discipline via `MemAddr<ReadOnly>` / `MemAddr<ReadWrite>`
- `internals::api::{find_class_t, find_method_t, field_addr_t, static_field_t, klass_of_t, invoke_method_t}` — return typed handles instead of raw u64
- `invoke_method_t` is the ONE typed sibling actively called today (by `mem_host::host_invoke`)

The Rust-side typed surface is for future composers (frontend plugin, native plugin layer, Rust callers beyond the WASM-host-fn boundary). The WASM boundary uses untyped i64s because WASM only has i32/i64 — marshalling at the boundary, typed Rust side.

---

## What exists for writes (partial)

**WASM-side write host fns** (gated by `FROG_WASM_WRITE` env var):

- `mem.write` — typed byte-level write at a raw address
- `mem.write_if` — compare-and-swap (read → confirm expected → write)

**Rust-side typed write sibling** (architectural — also load-bearing):

- `external::api::write_t<T>(addr: MemAddr<ReadWrite>, val)` — capability-gated typed write; the `ReadWrite` requirement is a compile-time guarantee enforced by Spine T5 doc-tests at `external/api.rs:108-121`

**Gap A — typed write host fn not yet registered:** the WASM boundary today has untyped `mem.write` only. To match the read-side ergonomics (typed-then-marshalled), `mem.write_t` should be registered alongside `mem.write` and route to `external::api::write_t`. ~20 lines.

---

## What's missing (field-set + method-set paths) — the actual #3 blockers

**There is no field-write path through il2cpp today.** Two routes are possible; both need work:

### Route 1 — `field_set_value` FFI

The il2cpp library exports `il2cpp_field_set_value` (instance fields) and `il2cpp_field_static_set_value` (static fields). Our FFI resolver (`internals::ffi::resolve_*`) handles `field_get_name` / `field_get_type` via standard exports + sig-scan, but does NOT resolve the `*_set_value` variants.

**Work:** add the `*_set_value` symbols to the standard-exports resolver block AND to the sig-scan path (the latter needs a new byte pattern — `il2cpp_field_set_value` is a small function: validate + memcpy). Pattern matches what B-1 Phase 5 calibration already does for the `_get_*` variants.

### Route 2 — direct memory write at field address

If we already have `field_addr_t(instance, klass, field_name) → MemAddr<ReadWrite>` (we don't, but it's straightforward via `instance + field.offset` for instance fields, or `static_storage + field.offset` for static fields), then `mem.write_t(addr, value)` writes the field directly. Faster than the FFI route (no function-call overhead), bypasses any il2cpp lifecycle hooks (could miss `runtime_class_init` for statics).

**Work:** add `field_addr_t` to `internals::api` (Rust-side), expose `il2cpp.field_addr` host fn, ensure the `MemAddr<ReadWrite>` capability flows correctly when paired with `mem.write_t`.

### Combined approach (likely #3's choice)

Most il2cpp dumpers use Route 2 for read (we do too — `get_field` reads at `instance + offset`) and Route 1 for write (correctness over micro-perf). The `#3 brainstorm should evaluate both and pick.

---

## Smallest viable In-flight Modify brick (sketch only)

Not a commitment — #3 brainstorm refines. This is what's possible with the current substrate:

1. Add `field_set_value` to the FFI resolver (standard exports + sig-scan). ~40 lines.
2. Register `mem.write_t` host fn (typed write through existing `external::api::write_t`). ~20 lines.
3. Register `il2cpp.set_field` host fn (parallel to `il2cpp.get_field`). ~50 lines (handle value-type vs reference-type per the existing `get_field` pattern).
4. Optional: `field_addr_t` Rust-side typed sibling + `il2cpp.field_addr` host fn. ~30 lines.
5. Verification: `scratch/test_modify.wat` — read Player.position field, write a new value, read back, log result.

**Total: ~140 lines of code + 1 test fixture.** Comparable in scope to the B-2bc bundle.

---

## Risks for #3

| Risk | Notes |
|---|---|
| `il2cpp_field_set_value` not exported on PW (obfuscated) | Sig-scan path handles this — pattern matches existing _get_* discipline. May need cross-validation against a known field to confirm the resolved address actually writes. |
| Value-type vs reference-type set has different ABIs | `il2cpp_field_set_value` takes a `void*` for value types (pointer to the value bytes) vs `Il2CppObject*` for reference types. Pattern matches what `marshal::pack_return_into_regargs` already handles. |
| Static field write needs class init | Most modders write to instance fields; static-field write can be a stretch goal. `il2cpp_runtime_class_init` is the gate; can be a separate task. |
| Field-write triggering serialization or anti-cheat | Out of scope for our agent — user's modding script policy decision. We provide the primitive; the actor decides when to use it. |

---

## Out-of-scope for #3

- Method-body REWRITE (vs the existing method-hook, which intercepts) — the inline_detour patcher could theoretically rewrite method bodies but the use cases for "modify a method's existing instructions" vs "intercept via hook" are vanishingly small for modders.
- Generic-instantiation modification — modifying open generics requires re-instantiating + re-registering with il2cpp, far beyond In-flight Modify.

---

## Verdict

The current substrate is ready for In-flight Modify with ~140 lines of additive work. No bedrock blockers. No spine restructure needed. The audit's recommendation: pick Route 2 (direct mem write at field address) as the primary path for instance fields, add Route 1 (FFI) as fallback for static-field-with-init. Plan accordingly at #3 brainstorm.
