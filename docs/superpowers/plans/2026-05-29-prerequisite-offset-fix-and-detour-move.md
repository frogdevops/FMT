# Invoke+Hook Prerequisites Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land two small focused changes that BLOCK the Invoke+Hook brick: (P-1) value-type FieldInfo offset fix + scanner-noise filter; (P-2) move the domain-agnostic inline patcher out of `protocol/`.

**Architecture:** P-1 detects whether a klass is a value type via a probed offset+bit pattern, then subtracts `sizeof(Il2CppObject) = 0x10` from FieldInfo offsets when reading value-type fields (sites: `dump.rs` + `internals/api.rs`). Also filters `token == 0` entries (scanner garbage). P-2 moves `protocol/hook.rs` (which is generic x86_64 inline patching, not protocol-specific) to a crate-root `inline_detour.rs` and updates two import sites.

**Tech Stack:** Rust 2021, no new deps. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (agent-core tests).

**Spec:** `docs/superpowers/specs/2026-05-29-invoke-hook-design.md` (Prerequisites section)

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent/src/protocol/hook.rs` | **Delete (after git mv)** | Was generic inline patcher; moves out of protocol/. |
| `crates/agent/src/inline_detour.rs` | **Create (via git mv)** | New home of the inline x86_64 patcher (Hook struct + install/remove + iced-x86 byte stealing). |
| `crates/agent/src/lib.rs` | Modify | Register `mod inline_detour;`. |
| `crates/agent/src/protocol/mod.rs` | Modify | Drop `pub mod hook;`. |
| `crates/agent/src/protocol/capture.rs` | Modify (2 lines) | Update `crate::protocol::hook::*` → `crate::inline_detour::*`. |
| `crates/agent/src/diagnostics/valuetype_probe.rs` | Create | One-shot opt-in probe (`FROG_VALUETYPE_PROBE`) that derives `klass_valuetype_off` + `klass_valuetype_bit`. |
| `crates/agent/src/internals/config.rs` | Modify | Add `klass_valuetype_off: usize` and `klass_valuetype_bit: u8` fields, populated per-version from probe results. |
| `crates/agent/src/internals/api.rs` | Modify | Add `klass_is_valuetype(klass: u64) -> bool` helper. Update `for_each_field` offset reads (lines 42 & 55) to subtract 0x10 when parent is value type. Filter `token == 0`. |
| `crates/agent/src/internals/dump.rs` | Modify | Apply same offset adjustment at lines 349 & 382; filter `token == 0`. |
| `crates/agent/src/entry.rs` | Modify | Wire `FROG_VALUETYPE_PROBE` opt-in invocation. |

---

## Task 1: Move `protocol/hook.rs` → `inline_detour.rs` (P-2)

**Files:**
- `git mv crates/agent/src/protocol/hook.rs crates/agent/src/inline_detour.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/protocol/mod.rs`
- Modify: `crates/agent/src/protocol/capture.rs:172,582`

- [ ] **Step 1: Move the file**

Run: `git mv crates/agent/src/protocol/hook.rs crates/agent/src/inline_detour.rs`

Expected: file appears at new path; git tracks as rename.

- [ ] **Step 2: Register the new module in `lib.rs`**

Add to `crates/agent/src/lib.rs` between `mod paths;` and `mod protocol;`:

```rust
#[cfg(target_os = "windows")]
mod inline_detour;
```

- [ ] **Step 3: Drop `pub mod hook;` from `protocol/mod.rs`**

Modify `crates/agent/src/protocol/mod.rs` — replace:

```rust
pub mod capture;
pub mod hook;
```

with:

```rust
pub mod capture;
```

- [ ] **Step 4: Update import sites in `protocol/capture.rs`**

In `crates/agent/src/protocol/capture.rs`:

Line 172 — replace:
```rust
static HOOKS: Mutex<Vec<crate::protocol::hook::Hook>> = Mutex::new(Vec::new());
```
with:
```rust
static HOOKS: Mutex<Vec<crate::inline_detour::Hook>> = Mutex::new(Vec::new());
```

Line 582 — replace:
```rust
if let Some(h) = crate::protocol::hook::install(addr_usize, $detour as *const () as usize) {
```
with:
```rust
if let Some(h) = crate::inline_detour::install(addr_usize, $detour as *const () as usize) {
```

- [ ] **Step 5: Cross-compile build to verify no behavior change**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean build (no errors; pre-existing warnings only).

- [ ] **Step 6: Commit (user will run this)**

Tell the user the move is staged; suggested message:
```
inline_detour: move protocol/hook.rs to crate root (shared with internals/hook_runtime)
```

---

## Task 2: Probe to derive value-type detection (P-1, Phase 0)

**Files:**
- Create: `crates/agent/src/diagnostics/valuetype_probe.rs`
- Modify: `crates/agent/src/diagnostics/mod.rs` (declare the new module)
- Modify: `crates/agent/src/entry.rs` (wire `FROG_VALUETYPE_PROBE` env)

The probe finds the offset + bit-mask of the `Il2CppClass::valuetype` flag by checking a class we **know is a value type** (`System.Int32`) against one we **know is a reference type** (`System.String`). The byte that differs (and only for known value types) is the flag.

- [ ] **Step 1: Create the probe module**

Create `crates/agent/src/diagnostics/valuetype_probe.rs`:

```rust
//! One-shot probe (opt-in `FROG_VALUETYPE_PROBE`): derives the offset and bit
//! of `Il2CppClass::valuetype` by diffing a known value type (`System.Int32`)
//! against a known reference type (`System.String`). Logs the candidate
//! offset+bit so it can be banked into `internals/config.rs`.

use crate::external::cache;
use crate::internals::api;
use crate::paths::log;

/// Scan bytes 0x00..0x200 of `vt_klass` and `ref_klass`. For every byte offset,
/// if `(vt_byte & b) != 0` and `(ref_byte & b) == 0` for some bit `b`, that's
/// a candidate (offset, bit). Print all candidates; the operator picks the
/// stable one (typically a small numeric mask like 0x01 or 0x02).
pub fn run_valuetype_probe() {
    log("=== VALUETYPE PROBE ===");
    let vt_klass = api::find_class("System.Int32");
    let ref_klass = api::find_class("System.String");
    if vt_klass == 0 || ref_klass == 0 {
        log(&format!(
            "valuetype probe: classes not found (Int32={:#x}, String={:#x})",
            vt_klass, ref_klass
        ));
        return;
    }
    log(&format!(
        "valuetype probe: Int32 @ {:#x}, String @ {:#x}",
        vt_klass, ref_klass
    ));
    let mut candidates: Vec<(usize, u8)> = Vec::new();
    for off in 0..0x200usize {
        let vt_byte = match cache::read_u8(vt_klass as usize + off) {
            Some(b) => b,
            None => continue,
        };
        let ref_byte = match cache::read_u8(ref_klass as usize + off) {
            Some(b) => b,
            None => continue,
        };
        // For each bit set in vt_byte but not in ref_byte → candidate.
        for bit_idx in 0..8u8 {
            let mask = 1u8 << bit_idx;
            if (vt_byte & mask) != 0 && (ref_byte & mask) == 0 {
                candidates.push((off, mask));
            }
        }
    }
    log(&format!("valuetype probe: {} candidates", candidates.len()));
    for (off, bit) in candidates.iter().take(32) {
        log(&format!("  +{:#05x} bit={:#04x}", off, bit));
    }
    log("=== end VALUETYPE PROBE ===");
}
```

- [ ] **Step 2: Register the module**

Add to `crates/agent/src/diagnostics/mod.rs`:

```rust
pub mod valuetype_probe;
```

(If `diagnostics/mod.rs` already has other `pub mod` declarations like `klass_probe;`, place this new line next to them.)

- [ ] **Step 3: Wire the env in `entry.rs`**

In `crates/agent/src/entry.rs`, after the existing `FROG_MEMBER_PROBE` block (around line 173), add:

```rust
if std::env::var("FROG_VALUETYPE_PROBE").is_ok() {
    crate::diagnostics::valuetype_probe::run_valuetype_probe();
}
```

- [ ] **Step 4: Build + deploy**

Run: `./deploy.sh release`
Expected: clean build, deployed to PW + Highrise.

- [ ] **Step 5: Run on PW and bank the result (user action)**

Tell user: launch Pixel Worlds with `WINEDLLOVERRIDES="version=n,b" FROG_VALUETYPE_PROBE=1 %command%`. Report back the offset+bit pair that has the smallest, most stable mask (typically `0x01` or `0x02` at an offset between `0xA0`–`0x110`).

Hand off the chosen `(klass_valuetype_off, klass_valuetype_bit)` for Task 3.

- [ ] **Step 6: Commit (user will run)**

Suggested message:
```
valuetype-probe: derive Il2CppClass::valuetype offset+bit structurally
```

---

## Task 3: Bank probed offsets + add `klass_is_valuetype` helper (P-1)

**Files:**
- Modify: `crates/agent/src/internals/config.rs`
- Modify: `crates/agent/src/internals/api.rs`

- [ ] **Step 1: Add config fields**

In `crates/agent/src/internals/config.rs`, add to the `Il2CppConfig` struct (alongside `klass_fields`, `klass_methods`, etc.):

```rust
/// Offset of the `valuetype` flag byte in Il2CppClass. Derived structurally
/// via diagnostics::valuetype_probe.
pub klass_valuetype_off: usize,
/// Bit mask within the valuetype flag byte. Typically 0x01 or 0x02.
pub klass_valuetype_bit: u8,
```

In the v24 default constructor block (around line 127), add:

```rust
klass_valuetype_off:         /* probed value, e.g. 0xA8 */ 0xA8,
klass_valuetype_bit:         /* probed bit, e.g. 0x01  */ 0x01,
```

In the v30 block (around line 172), add equivalent lines (use the v30-probed values; if not yet probed for v30, use the same v24 values and bank a TODO comment to re-probe when we touch a v30 game).

**The literal numbers above are placeholders — replace with the actual probe output from Task 2 Step 5.**

- [ ] **Step 2: Add the helper in `internals/api.rs`**

Append to `crates/agent/src/internals/api.rs` (after the existing functions, before any `_t` typed siblings):

```rust
/// True if the klass is a value type (struct or primitive). Detected via the
/// probed valuetype flag bit in Il2CppClass. Falls back to `false` if the
/// klass is unreadable.
pub fn klass_is_valuetype(klass: u64) -> bool {
    let c = match ctx::get() { Some(c) => c, None => return false };
    let byte = cache::read_u8(klass as usize + c.cfg.klass_valuetype_off).unwrap_or(0);
    byte & c.cfg.klass_valuetype_bit != 0
}
```

- [ ] **Step 3: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 4: Commit (user will run)**

Suggested message:
```
internals: bank valuetype offset+bit + klass_is_valuetype helper
```

---

## Task 4: Apply offset fix at all four field-read sites (P-1)

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (lines 42 + 55)
- Modify: `crates/agent/src/internals/dump.rs` (lines 349 + 382)

Each site reads `offset = read_u32(field_ptr + 24)`. We compute `effective_offset = if klass_is_valuetype(parent_klass) { offset.saturating_sub(0x10) } else { offset }`.

- [ ] **Step 1: Fix `internals/api.rs::for_each_field` (FFI path)**

In `crates/agent/src/internals/api.rs`, locate the existing line ~42:
```rust
let offset = cache::read_u32(fi as usize + 24).unwrap_or(0);
if f(&name, offset, type_ptr) { return; }
```

Replace with:
```rust
let raw_offset = cache::read_u32(fi as usize + 24).unwrap_or(0);
let offset = if klass_is_valuetype(klass as u64) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
if f(&name, offset, type_ptr) { return; }
```

- [ ] **Step 2: Fix `internals/api.rs::for_each_field` (memory fallback path)**

Same file, the equivalent block ~line 55:
```rust
let offset = cache::read_u32(slot + 24).unwrap_or(0);
if f(&name, offset, type_ptr) { return; }
```

Replace with:
```rust
let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
let offset = if klass_is_valuetype(klass as u64) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
if f(&name, offset, type_ptr) { return; }
```

- [ ] **Step 3: Fix `internals/dump.rs::collect_runtime_fields` (FFI path)**

In `crates/agent/src/internals/dump.rs` around line 349, locate:
```rust
let offset = map.read_u32(f as usize + 24).unwrap_or(0);
let token = map.read_u32(f as usize + 28).unwrap_or(0);
rt_fields.push((fname, ftype, offset, token));
```

Replace with:
```rust
let raw_offset = map.read_u32(f as usize + 24).unwrap_or(0);
let token = map.read_u32(f as usize + 28).unwrap_or(0);
let offset = if crate::internals::api::klass_is_valuetype(cls as u64) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
rt_fields.push((fname, ftype, offset, token));
```

- [ ] **Step 4: Fix `internals/dump.rs::collect_runtime_fields` (memory fallback)**

Around line 382 in the same file:
```rust
let offset = map.read_u32(f + 24).unwrap_or(0);
let token = map.read_u32(f + 28).unwrap_or(0);
rt_fields.push((fname, ftype, offset, token));
```

Replace with:
```rust
let raw_offset = map.read_u32(f + 24).unwrap_or(0);
let token = map.read_u32(f + 28).unwrap_or(0);
let offset = if crate::internals::api::klass_is_valuetype(cls as u64) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
rt_fields.push((fname, ftype, offset, token));
```

- [ ] **Step 5: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 6: Commit (user will run)**

Suggested message:
```
internals+dump: subtract 0x10 from FieldInfo::offset for value-type parents
```

---

## Task 5: Filter `token == 0` scanner garbage (P-1, scanner-noise fix)

**Files:**
- Modify: `crates/agent/src/internals/api.rs` (memory fallback path, around line 50–58)
- Modify: `crates/agent/src/internals/dump.rs` (around lines 349–351 and 382–384)

Scanner garbage shows up as FieldInfo slots that happen to have non-null name pointers and plausible-looking offsets, but no token (IL2CPP only assigns tokens to fields defined in metadata). Filtering `token == 0` drops the bulk of false positives.

- [ ] **Step 1: Filter in `internals/api.rs::for_each_field` (memory fallback only — FFI path won't see garbage)**

In the memory-fallback block of `for_each_field` (around line 50), after the `let name = ...` line and BEFORE the offset is read, add the token filter:

```rust
let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => continue };
let token = cache::read_u32(slot + 28).unwrap_or(0);
if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
// ... rest of the block (offset adjustment from Task 4) follows
```

- [ ] **Step 2: Filter in `internals/dump.rs::collect_runtime_fields` (both paths)**

In `dump.rs`, BOTH the FFI block (~line 349) and the memory-fallback block (~line 382) — right after `let token = ...`, add:

```rust
if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
```

Place the `continue` BEFORE the `rt_fields.push((fname, ftype, offset, token));` line in each block.

- [ ] **Step 3: Build**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 4: Commit (user will run)**

Suggested message:
```
internals+dump: drop token=0 FieldInfo entries (scanner garbage)
```

---

## Task 6: PW integration gate — verify the fix

**Files:** none modified; pure verification.

- [ ] **Step 1: Deploy**

Run: `./deploy.sh release`
Expected: clean build, deployed to PW + Highrise.

- [ ] **Step 2: Hand the gate back to the user**

Tell user: launch Pixel Worlds, let it write `internals.txt`. Inspect the dump for `ObscuredVector3` and `ObscuredFloat`. Expected outcomes:

- `ObscuredVector3::hash` at offset `0x00` (was `0x10`)
- `ObscuredVector3::currentCryptoKey` at offset `0x10` (was `0x20`)
- `ObscuredFloat::hash` at offset `0x00` (was `0x10`)
- ObscuredVector3 / ObscuredFloat field counts should drop substantially (no more `BLOCKTYPE_MAX` / `ReadArrayFromBSON` etc. scanner-noise fields)
- Reference-type classes (e.g. `Player`) unchanged

- [ ] **Step 3: Stop and report**

If the user confirms the dump matches expectations, prerequisites are done — invoke+hook can start. If any expectation fails, STOP and report (don't continue to Sub-brick I).

---

## Self-review

**1. Spec coverage:**
- P-1 offset fix — Tasks 2 (probe), 3 (helper + config), 4 (fix sites) ✓
- P-1 scanner noise — Task 5 ✓
- P-2 inline_detour move — Task 1 ✓
- Verification — Task 6 ✓

**2. Placeholder scan:**
- The `klass_valuetype_off: 0xA8` and `klass_valuetype_bit: 0x01` literals in Task 3 are explicit placeholders documented inline as "replace with probe output" — not "TBD", just a parameterized step the user fills in from Task 2.
- No other vague verbs.

**3. Type consistency:**
- `klass_is_valuetype` name used identically across Tasks 3 and 4.
- `klass_valuetype_off` / `klass_valuetype_bit` field names match between config and api.
- `raw_offset` local variable used consistently across all four fix sites.
- `cls as u64` cast pattern used identically in both dump.rs sites (`cls` is the local variable name in `collect_runtime_fields` per the read above).

**Deviation noted:**
- The probe in Task 2 logs candidates but doesn't auto-pick — operator selects. This is intentional: the probe surface is small (one Unity-version pair per game), and human picking from a candidate list is more honest than a heuristic that might pick a coincidentally-matching unrelated byte. Same pattern as the existing probes (FROG_KLASS_PROBE, FROG_MEMBER_PROBE) — operator reads log + bank values manually.
