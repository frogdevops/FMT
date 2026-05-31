# B-2a: Honest Dumper — Design

**Date:** 2026-05-30
**Branch:** `ffi-class-table` (or successor)
**Status:** approved, ready for plan-writing
**Builds on:** Bedrock B-1 (Probe-and-Verify) — shipped + game-verified
**Audit reference:** `docs/superpowers/audit-2026-05-29-architecture-review.md` + PW post-B-1 `internals.txt` empirical counts

---

## Goal

Make `internals.txt` honest. Eliminate the 229 garbage `<type:NNN>` entries and dozens of absurd `Offset: 0xf9xxxxxx` lines visible on Pixel Worlds today, restore identifiability for open-generic class headers, and add diagnostic clarity so future garbage-vs-unhandled cases are operator-distinguishable rather than silently lumped together.

## Why this matters

Modders consume `internals.txt` directly. Today on PW:
- **229 `<type:NNN>` field entries** (1.1% of 20,336 fields) are unreadable garbage. Only 4 of them are valid-but-unhandled type codes (CMOD_REQD); the remaining 225 are corrupted memory reads at the catch-all.
- **Dozens of `Offset: 0xf9xxxxxx`** entries leak code-region pointers into a u32 offset display. Same root cause: FieldInfo entries past the real array end.
- **Open-generic class headers** show `System.Collections.Generic: (0 fields):` because `class_get_name`/`namespace` both return empty on obfuscated builds. Field data is orphaned, class is unidentifiable.

Highrise has zero of these issues — it uses the FFI iterator (`api.class_get_fields`), which knows when to stop. PW uses the memory-walk fallback because `class_get_fields` resolves to `None` via sig-scan.

## The bedrock principle, applied to the dumper

After B-1's probe-and-verify discipline, this brick extends the same posture to user-facing output:

> **A dump entry that exists is honest. Garbage entries are filtered upstream, not shown. Where output IS shown, valid-but-unhandled and corrupted-memory cases are visually distinct so operators can tell the difference.**

## Non-goals (deferred to subsequent bricks)

| Item | Deferred to |
|---|---|
| Full generic-name resolution from `Il2CppGenericClass::type` chain (Fix D ships a stopgap placeholder header, not the real name walk) | B-2 follow-up |
| Tier-2 type codes 0x16 (FNPTR), 0x1A (I), 0x1B (U), 0x1F (TYPEDBYREF) — never observed in practice | B-2 follow-up if ever observed |
| Dead-code sweep (unused `_t` siblings, `METHOD_ATTRIBUTE_STATIC_BIT`, `#[allow(dead_code)]` cleanup) | Own micro-brick |
| Concrete-bug bundle (IOCP pending-map leak, `read_name` 64-byte cap, anti-cheat re-check, `aob_scan` cache coherence) | B-2b |
| Cross-domain transactionality docs / `i64↔u64` sign-extension docs / `InvokeArg` round-trip test | B-2c |
| Name de-obfuscation by behavioral signature | B-2d (banked) |

Each deferral is concrete and addressable on top of B-2a without B-2a complicating it.

---

## The four fixes

### Fix A — Add match arms for 5 unhandled type codes

**Where:** `crates/agent/src/internals/resolve.rs:324` (insert before the existing `_ => {}` catch-all).

**What's broken:** type codes 0x20 (CMOD_REQD), 0x21 (CMOD_OPT), 0x40 (MODIFIER), 0x41 (SENTINEL), 0x45 (PINNED) all wrap an inner Il2CppType*. The current resolver lacks handlers for them; they fall through to the catch-all and print `<type:32>` / `<type:33>` / `<type:64>` / `<type:65>` / `<type:69>` instead of the wrapped inner type.

**Fix:** four match arms; each reads `data64` as a pointer to the inner Il2CppType and recurses with `depth + 1`:

```rust
0x20 | 0x21 => {
    // CMOD_REQD / CMOD_OPT — wrap an inner type. data64 → inner Il2CppType*.
    let inner = data64 as usize;
    if inner != 0 {
        return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
    }
    return "<cmod-unresolved>".into();
}
0x40 => {
    // MODIFIER — wrap an inner type.
    let inner = data64 as usize;
    if inner != 0 {
        return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
    }
    return "<modifier-unresolved>".into();
}
0x41 => {
    // SENTINEL — varargs marker; inner type follows.
    let inner = data64 as usize;
    if inner != 0 {
        return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
    }
    return "<sentinel-unresolved>".into();
}
0x45 => {
    // PINNED — pinned modifier.
    let inner = data64 as usize;
    if inner != 0 {
        return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
    }
    return "<pinned-unresolved>".into();
}
```

The pattern matches the existing 0x14/0x1D/0x15 generic/array recursion in the same function — no architectural change.

**Impact:** fixes 4 of the 229 PW entries today; future-proof for other games.

### Fix B — Smart catch-all (split valid-but-unhandled from garbage)

**Where:** `crates/agent/src/internals/resolve.rs:326` (replace `format!("<type:{}>", tc)`).

**What's broken:** the catch-all prints the same `<type:N>` whether `tc` is `0x21` (valid il2cpp tc the resolver doesn't handle) or `0xA5` (garbage memory the resolver read tc out of). Operators can't tell the difference. All 229 PW garbage entries look identical to legitimate-but-unhandled cases.

**Fix:** split based on whether `tc` is in the valid il2cpp type-code range `0x01..=0x45`:

```rust
if tc <= 0x45 {
    format!("<unhandled-tc:0x{:02x}>", tc)
} else {
    format!("<garbage-tc:0x{:02x} @ {:#x}>", tc, type_ptr)
}
```

`type_ptr` is the function parameter, already in scope at line 326.

**Impact:** 225 PW entries that would still hit the catch-all (because they're memory garbage, not unhandled-but-valid) now read as `<garbage-tc:0xa5 @ 0x...>` — instantly diagnosable. Operators see addresses they can investigate.

### Fix C — `type_ptr` validation in the memory-walk fallback

**Where:** `crates/agent/src/internals/dump.rs` (`collect_runtime_fields`, memory-walk path) AND `crates/agent/src/internals/api.rs::for_each_field` (memory-walk fallback). Parallel patches with the same logic.

**What's broken:** the FieldInfo memory-walk doesn't know exactly where the array ends. The `token == 0` filter (already in place) drops most garbage entries, but some bytes past the real array end happen to read non-zero token AND non-empty name. Those entries then have type_ptr / offset pointing at random memory — producing `<type:NNN>` garbage AND `Offset: 0xf9xxxxxx` absurd values in the same row.

**Root cause:** PW uses the memory-walk fallback because `api.class_get_fields` is None (sig-scan didn't find it). Highrise uses the FFI iterator and has zero garbage entries — proving the iteration-end is what's wrong, not the memory layout.

**Fix:** validate that the FieldInfo's `type_ptr` produces a plausible type code before accepting the row. The same `tc` extraction recipe Phase 3 verified at B-1 calibration.

**Fix C-prep — order normalization in `dump.rs`:**

Currently `dump.rs:382-395` reads type_ptr FIRST (then does 11 lines of work like `il2cpp_type_name` resolution + offset adjustment), and only THEN reads the token to check `token == 0`. `api.rs:60-61` is opposite — token check FIRST, type_ptr SECOND. Mirror api.rs in dump.rs:

```rust
// Read token first; bail early if scanner garbage.
let token = map.read_u32(f + 28).unwrap_or(0);
if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token

let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
// ... [the new tc-validation lands here] ...
let ftype = if type_ptr != 0 { ... };
// ... rest of the existing row processing ...
```

Pure reordering — no behavioral change in isolation, but makes the new tc-guard land in the cheap-fast-fail position in both files.

**Fix C — the validation itself:**

After `type_ptr` is read (in both files), insert:

```rust
// Validate type_ptr produces a plausible type code. Garbage FieldInfo
// entries past the real array end have type_ptr pointing to random
// memory that doesn't decode as a valid tc in 0x01..=0x45.
if type_ptr == 0 { continue; }
let chunk = /* map | cache */ ::read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
let tc = ((chunk >> cfg.discrim_shift) & 0xFF) as u8;
if tc == 0 || tc > 0x45 { continue; }
```

- `dump.rs` path uses `map.read_u64(...)` and has `cfg: &Il2CppConfig` already in scope as a parameter.
- `api.rs` path uses `cache::read_u64(...)` and reaches the same offsets via `c.cfg.il2cpp_type_discrim_read_at` / `c.cfg.discrim_shift` (where `c = ctx::get()...`).

**Impact:** eliminates all 225 garbage tc entries AND all `Offset: 0xf9xxxxxx` lines (same root-cause rows — they're filtered before either field is emitted). One filter, three symptoms gone. Also gates field access for any host fn going through `for_each_field` (so script-side `field_info_t`/`get_field` won't see garbage either).

### Fix D — Open-generic class header stopgap

**Where:** `crates/agent/src/internals/dump.rs:161-162` (after `cname` and `cns` are read via `class_get_name`/`class_get_namespace`).

**What's broken:** open-generic instantiations (like `List<>` before specialization) return empty strings from both `class_get_name` and `class_get_namespace` on obfuscated builds. The dumper emits `"namespace: (N fields):"` with empty cname, producing the `System.Collections.Generic:` orphan lines.

**Fix:** when both are empty, synthesize an identifying header:

```rust
// Current (immutable bindings at 161-162):
let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
let cns   = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };

// Replace with mutable cname and the fallback:
let mut cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
let     cns   = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
if cname.is_empty() && cns.is_empty() {
    cname = format!("<generic @ {:#x}>", cls as usize);
}
```

(Implementation detail noted during review — `cname` must become a `let mut` binding. The reviewer flagged this in advance.)

**Impact:** open-generic classes get an identifiable, address-anchored header so their fields aren't orphaned. NOT the full fix for "what generic IS this" — that requires walking `Il2CppGenericClass::type` chain and is deferred to a follow-up brick. This stopgap makes the dump readable in the meantime.

---

## Architecture summary

```
B-2a fix layout
────────────────────────────────────────────────────
resolve.rs
  Fix A: 4 match arms (5 tcs)  →  unwrap + recurse
  Fix B: catch-all split        →  <unhandled-tc>|<garbage-tc>

dump.rs::collect_runtime_fields (memory-walk path)
  Fix C-prep: move token check before type_ptr read
  Fix C:     after type_ptr,
             validate tc ∈ 0x01..=0x45 → continue if not

dump.rs (class-emission loop)
  Fix D: empty cname + empty cns
         → synthesize "<generic @ 0x...>" header

api.rs::for_each_field (memory-walk fallback)
  Fix C: same tc-validation as dump.rs (parallel patch)
```

**No new types, no new modules, no architectural change.** Fixes A + B are local to resolve.rs's existing match. Fix C extends existing field-filtering patterns (`token == 0 { continue }` is already there; tc-guard sits next to it). Fix D adds a single-line synthesized header.

**Total touched code:** ~30 lines across 3 files.

---

## Testing strategy

### Unit tests (host-runnable; agent-core)

Create `crates/agent-core/tests/resolver.rs` (new) with synthetic Il2CppType byte patterns to exercise Fix A and Fix B in isolation. Mock RegionMap returns canned `data64` / `discrim` bytes. Cases:

1. **tc=0x20 unwraps to inner type** — synthesize Int32 (tc=0x08) wrapped in CMOD_REQD; expect resolver output `"System.Int32"` (not `<type:32>` or `<unhandled-tc:0x20>`).
2. **tc=0x21 unwraps to inner type** — same, CMOD_OPT wrapping String; expect `"System.String"`.
3. **tc=0x40 / 0x41 / 0x45 unwrap** — same, with MODIFIER, SENTINEL, PINNED. Each should return inner type.
4. **tc=0x44 (valid unhandled)** — falls through to catch-all; expect `"<unhandled-tc:0x44>"` (not `<garbage-tc>`).
5. **tc=0xA5 (garbage)** — catch-all; expect `"<garbage-tc:0xa5 @ 0x...>"` (address present, byte-formatted).
6. **Recursion depth bound** — a CMOD wrapping a CMOD wrapping ... ad nauseam respects the existing `depth > 8` guard and returns `"?"`.

### Live-game regression (manual; PW + Highrise)

Deploy via `./deploy.sh release`. Launch each game normally (`WINEDLLOVERRIDES="version=n,b" %command%`). After `internals.txt` is written:

```bash
# Garbage tc count — expect 0 (B-2a goal)
grep -c "<garbage-tc:" "/path/to/internals.txt"

# Unhandled-tc count — expect ≤ pre-B-2a + small (legitimate but undiscovered tcs)
grep -c "<unhandled-tc:" "/path/to/internals.txt"

# Absurd offsets — expect 0
grep -c "Offset: 0xf" "/path/to/internals.txt"

# Open-generic stopgap headers — expect handful on PW, 0 on Highrise (it has names)
grep -c "<generic @ 0x" "/path/to/internals.txt"

# Dump count regression check
grep "dumped" "/path/to/agent.log"
```

**Expected outcomes:**

| Metric | PW pre-B-2a | PW post-B-2a | Highrise post-B-2a |
|---|---|---|---|
| `<type:NNN>` count | 229 | 0 | 0 |
| `<garbage-tc:>` count | n/a | 0 | 0 |
| `<unhandled-tc:>` count | n/a | ≤4 (the CMOD entries Fix A now resolves; possibly 0) | 0 |
| `Offset: 0xf` count | dozens | 0 | 0 |
| `<generic @ 0x>` headers | 0 (was `<type:165>` etc.) | handful (orphans now named) | 0 |
| Classes dumped | 1,543 | 1,543 (or higher; the open-generic stopgap surfaces classes that were filtered before) | 15,226 |
| Fields dumped | 20,336 | ~20,107 (225 garbage rows removed) | 79,340 |

### Sub-brick I / II regression

`scratch/test_invoke.wasm` (Math.Pow) and `scratch/test_hook.wasm` (Math.Pow + hook) must still PASS. They're the load-bearing proof that B-2a's filter doesn't accidentally drop legitimate field/method access used by Invoke + Hook.

---

## What ships when B-2a lands

- Zero `<type:NNN>` lines on either game's dump.
- Zero absurd `Offset: 0xf9xxxxxx` lines.
- Open-generic class headers identifiable via address-anchored stopgap (`<generic @ 0x...>`).
- Diagnostic clarity: `<garbage-tc:0xNN @ ptr>` vs `<unhandled-tc:0xNN>` are visually distinct in any future case.
- `internals.txt` on PW becomes operator-readable for modders.
- The 5 CMOD/MODIFIER/SENTINEL/PINNED unwrapped tcs work for any game (future-proofing, even though only 4 PW entries today benefit).

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| The tc-validation guard might reject legitimate fields whose Il2CppType pointer happens to read tc outside `0x01..=0x45` (unlikely — every real tc is in this range) | If observed, widen to `0x01..=0x50` or add a structural integrity probe alongside (B-2 follow-up). Game data confirms valid tcs all fall in `0x01..=0x21` today, with `0x40/0x41/0x45` being the rare modifier outliers we already handle. |
| Fix D's synthesized header could collide with a real class named `<generic @ ...>` (impossible — angle brackets aren't valid C# class-name characters) | None needed; angle brackets guarantee uniqueness. |
| The token-check reorder in Fix C-prep could change order of side effects in `dump.rs` if any cache hit had subtle effects | Both reads are pure (no writes, no logs), so reordering is observably identical. Verified during scope review. |
| The catch-all changes break a downstream parser that grep'd `<type:N>` | None known — `internals.txt` is human-readable, no automated parser depends on the old format. Frontend plugin reads typed data from a different surface (B-2 follow-up). |
| Highrise regression from Fix C if the FFI path somehow triggers the new tc-guard | Doesn't happen — Fix C lands only in the memory-walk fallback (the `else` branch of `for_each_field`, the `else` branch of `collect_runtime_fields`'s FFI iterator vs memory walk). Highrise's FFI iterator path is untouched. |
