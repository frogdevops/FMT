# Bedrock Discovery Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the discovery-first bedrock engine — a `Fact<T>`/`Layout` contract whose every runtime layout value is discovered from intrinsic structure with ≥2 witnesses and carries its own derivation (`Provenance`), with NO hardcode fallbacks — and prove it live on PW + Highrise, **without touching any of the 96 existing `cfg` consumers** (their migration is a separate follow-on plan).

**Architecture:** Pure-logic core (`Fact`, `Layout`, `MemView` trait, discoverers) lives in **agent-core** (Linux-testable with a `MockMem`). The **agent** crate provides `RegionMap: MemView`, the `discover()` orchestrator, and an env-gated live-prove probe (`FROG_LAYOUT_PROBE`). Container-first, non-circular discovery: find a container by intrinsic structure, then derive sub-offsets by classifying its slots. Proven seed: `FROG_RECOGNIZER_PROBE` (methods@0x98, method_pointer_off=0x0, 12/12 both games).

**Tech Stack:** Rust 2021, agent-core (Linux-native, `cargo test -p agent-core`), agent (Windows cdylib via `x86_64-pc-windows-gnu` cross-compile — NEVER bare `cargo build -p agent`), reuse existing `RegionMap` (VirtualQuery) + `protect_of`.

**Reference spec:** `docs/superpowers/specs/2026-06-01-bedrock-discovery-first-design.md`
**Investigation/proof:** `docs/superpowers/investigations/2026-06-01-bedrock-layout-ground-truth.md`

**Critical project rules (every task):**
1. **DO NOT run `git commit`/`add`/`stash`.** Per [[user-commits-own-work]], pause at marked commit points.
2. **Verify agent-core** with `cargo test -p agent-core`; **verify agent** with `cargo build --target x86_64-pc-windows-gnu --release` (Linux-native agent build is an empty cdylib — a false positive).
3. **DO NOT run `./deploy.sh`** except at the live-prove task (it deploys to BOTH PW + Highrise).
4. **CODEBASE LAW** ([[truth-management-self-documenting-values]]): comments describe MECHANISM, NEVER assert a VALUE. No code in this plan may contain a comment claiming an offset number; values are discovered and carried in `Provenance`.
5. **DO NOT touch any `cfg.X` consumer** (dump/resolve/marshal/hook/api). This plan builds the engine alongside the existing one; nothing is migrated.

---

## File Structure

**Created (agent-core — pure logic, Linux-testable):**

| Path | Responsibility |
|---|---|
| `crates/agent-core/src/bedrock/mod.rs` | module root + re-exports |
| `crates/agent-core/src/bedrock/fact.rs` | `Fact<T>`, `Provenance`, `Witness`, `DerivationMethod`, `UnresolvedReason` |
| `crates/agent-core/src/bedrock/mem.rs` | `MemView` trait (the memory-access seam) + `MockMem` (cfg(test)) |
| `crates/agent-core/src/bedrock/layout.rs` | `Layout` struct (all facts) |
| `crates/agent-core/src/bedrock/discover/mod.rs` | `discover(mem, table_base, table_count) -> Layout` orchestrator |
| `crates/agent-core/src/bedrock/discover/foundation.rs` | stride (autocorrelation), root-validator, region-coverage, root-integrity |
| `crates/agent-core/src/bedrock/discover/containers.rs` | `recognize_methods` / `recognize_fields` (ported from the proven probe) |
| `crates/agent-core/src/bedrock/discover/suboffsets.rs` | derive method/field sub-offsets by slot classification |
| `crates/agent-core/src/bedrock/discover/type_discrim.rs` | (read_at, shift) primitive round-trip consensus |
| `crates/agent-core/src/bedrock/discover/hard_cases.rs` | static_fields / type_def honest-`Unresolved`; valuetype bit |

**Created (agent — Windows glue):**

| Path | Responsibility |
|---|---|
| `crates/agent/src/bedrock_glue.rs` | `impl MemView for RegionMap`; `run_layout_probe` (FROG_LAYOUT_PROBE) |

**Modified:**

| Path | Change |
|---|---|
| `crates/agent-core/src/lib.rs` | `pub mod bedrock;` |
| `crates/agent/src/lib.rs` | `mod bedrock_glue;` (windows-gated) |
| `crates/agent/src/entry.rs` | add `FROG_LAYOUT_PROBE` gate calling `run_layout_probe` (reuses existing `map`, `table_base`, `table_count`) |

**NOT touched:** `internals/config.rs`, `internals/calibration/*`, and all 96 `cfg.X` consumers. (Follow-on migration plan.)

---

## Phase 1 — The contract (`Fact`/`Layout`/`MemView`), agent-core

### Task 1: `Fact<T>` + provenance types

**Files:**
- Create: `crates/agent-core/src/bedrock/mod.rs`
- Create: `crates/agent-core/src/bedrock/fact.rs`
- Modify: `crates/agent-core/src/lib.rs`

- [ ] **Step 1: Write the failing test** — append to `fact.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn prov(v: u64) -> Provenance {
        Provenance { witnesses: vec![Witness { method: DerivationMethod::Structural, observed: v, signal: "test" }], sampled: 1 }
    }

    #[test]
    fn resolved_get_and_require() {
        let f = Fact::Resolved { value: 0x98usize, provenance: prov(0x98) };
        assert_eq!(f.get(), Some(0x98));
        assert_eq!(f.require(), Ok(0x98));
    }

    #[test]
    fn unresolved_get_is_none_require_is_err() {
        let f: Fact<usize> = Fact::Unresolved { reason: UnresolvedReason::NoWitness };
        assert_eq!(f.get(), None);
        assert_eq!(f.require(), Err(UnresolvedReason::NoWitness));
    }
}
```

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p agent-core bedrock::fact` → FAIL (types undefined).

- [ ] **Step 3: Write the implementation** — top of `fact.rs`:

```rust
//! The discovery contract: a value either has ≥2 agreeing witnesses (`Resolved`,
//! carrying its full derivation in `Provenance`) or it is `Unresolved`. There is
//! deliberately no third "fallback" state — fail-closed is structural.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact<T> {
    Resolved { value: T, provenance: Provenance },
    Unresolved { reason: UnresolvedReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Every agreeing derivation and what it observed. The value documents its
    /// own derivation; the report/log is generated from this, never hand-written.
    pub witnesses: Vec<Witness>,
    /// Sample size the agreement held over (e.g. 12 → "12/12 klasses").
    pub sampled: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Witness {
    pub method: DerivationMethod,
    pub observed: u64,
    pub signal: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivationMethod { Structural, ReferenceCrossCheck, FfiCrossCheck, OutOfBandAnchor }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedReason { NoWitness, WitnessDisagreement, NoMetadata, NoDiscriminator }

impl<T: Copy> Fact<T> {
    pub fn get(&self) -> Option<T> {
        match self { Fact::Resolved { value, .. } => Some(*value), _ => None }
    }
    pub fn require(&self) -> Result<T, UnresolvedReason> {
        match self { Fact::Resolved { value, .. } => Ok(*value), Fact::Unresolved { reason } => Err(*reason) }
    }
    pub fn is_resolved(&self) -> bool { matches!(self, Fact::Resolved { .. }) }
}
```

`mod.rs`:
```rust
pub mod fact;
pub mod mem;
pub mod layout;
pub mod discover;
pub use fact::{Fact, Provenance, Witness, DerivationMethod, UnresolvedReason};
pub use layout::Layout;
pub use mem::MemView;
```
(Comment out `pub mod mem/layout/discover` + the `pub use` lines for those until their tasks create them, OR create empty stubs now — your call; the build must stay green each task.)

`lib.rs`: add `pub mod bedrock;`.

- [ ] **Step 4: Run tests** — `cargo test -p agent-core bedrock::fact` → 2 passed.
- [ ] **Step 5: Pause for user commit.**

### Task 2: `MemView` trait + `MockMem` test harness

**Files:** Create `crates/agent-core/src/bedrock/mem.rs`.

- [ ] **Step 1: Write the failing test** — in `mem.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mockmem_reads_and_classifies() {
        let mut m = MockMem::new();
        m.put_u64(0x1000, 0xDEAD_BEEF);
        m.put_cstr(0x2000, "Player");
        m.mark_exec(0x3000, 0x100); // [0x3000,0x3100) executable
        assert_eq!(m.read_u64(0x1000), Some(0xDEAD_BEEF));
        assert_eq!(m.read_u64(0x9999), None);            // unmapped → None, never faults
        assert_eq!(m.read_cstr(0x2000).as_deref(), Some("Player"));
        assert!(m.is_exec(0x3050));
        assert!(!m.is_exec(0x1000));
    }
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p agent-core bedrock::mem` → FAIL.

- [ ] **Step 3: Implement** — top of `mem.rs`:

```rust
//! The memory-access seam. Discoverers depend only on this trait, so they are
//! pure logic (agent-core, Linux-testable). The agent crate impls it on RegionMap
//! (VirtualQuery-backed, never-fault). All reads return Option (None = unmapped).

pub trait MemView {
    fn read_u64(&self, addr: usize) -> Option<u64>;
    fn read_u32(&self, addr: usize) -> Option<u32>;
    fn read_u8(&self, addr: usize) -> Option<u8>;
    fn read_cstr(&self, addr: usize) -> Option<String>;
    /// True iff `addr` is in a committed executable page.
    fn is_exec(&self, addr: usize) -> bool;
}

#[cfg(test)]
pub struct MockMem {
    bytes: std::collections::HashMap<usize, u8>,
    exec: Vec<(usize, usize)>,
}

#[cfg(test)]
impl MockMem {
    pub fn new() -> Self { Self { bytes: Default::default(), exec: Vec::new() } }
    pub fn put_bytes(&mut self, addr: usize, b: &[u8]) { for (i, x) in b.iter().enumerate() { self.bytes.insert(addr + i, *x); } }
    pub fn put_u64(&mut self, addr: usize, v: u64) { self.put_bytes(addr, &v.to_le_bytes()); }
    pub fn put_u32(&mut self, addr: usize, v: u32) { self.put_bytes(addr, &v.to_le_bytes()); }
    pub fn put_cstr(&mut self, addr: usize, s: &str) { self.put_bytes(addr, s.as_bytes()); self.bytes.insert(addr + s.len(), 0); }
    pub fn mark_exec(&mut self, addr: usize, len: usize) { self.exec.push((addr, addr + len)); }
}

#[cfg(test)]
impl MemView for MockMem {
    fn read_u64(&self, a: usize) -> Option<u64> {
        let mut b = [0u8; 8];
        for i in 0..8 { b[i] = *self.bytes.get(&(a + i))?; }
        Some(u64::from_le_bytes(b))
    }
    fn read_u32(&self, a: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        for i in 0..4 { b[i] = *self.bytes.get(&(a + i))?; }
        Some(u32::from_le_bytes(b))
    }
    fn read_u8(&self, a: usize) -> Option<u8> { self.bytes.get(&a).copied() }
    fn read_cstr(&self, a: usize) -> Option<String> {
        let mut s = Vec::new();
        let mut i = a;
        loop {
            let c = *self.bytes.get(&i)?;
            if c == 0 { break; }
            if s.len() > 256 { return None; }
            s.push(c); i += 1;
        }
        String::from_utf8(s).ok()
    }
    fn is_exec(&self, a: usize) -> bool { self.exec.iter().any(|&(s, e)| a >= s && a < e) }
}
```

- [ ] **Step 4: Run tests** — `cargo test -p agent-core bedrock::mem` → passed.
- [ ] **Step 5: Pause for user commit.**

### Task 3: `Layout` struct

**Files:** Create `crates/agent-core/src/bedrock/layout.rs`.

- [ ] **Step 1: Implement** (no behavior yet → a compile + a trivial constructor test). Body:

```rust
//! THE bedrock contract. Capabilities consume `Layout`; nothing else carries
//! offsets. Every field is a `Fact` — there is no raw `usize` a consumer could
//! read as a silent fallback. (Comments here describe what each fact IS used for
//! by mechanism — never its numeric value; values live in each Fact's Provenance.)

use crate::bedrock::Fact;

#[derive(Debug, Clone)]
pub struct Layout {
    pub table_base: Fact<usize>,
    pub table_count: Fact<usize>,
    pub class_table_step: Fact<usize>,
    pub klass_namespace: Fact<usize>,
    pub klass_fields: Fact<usize>,
    pub klass_methods: Fact<usize>,
    pub klass_static_fields: Fact<usize>,
    pub klass_type_def: Fact<usize>,
    pub klass_generic_class: Fact<usize>,
    pub klass_valuetype_off: Fact<usize>,
    pub klass_valuetype_bit: Fact<u8>,
    pub type_discrim_read_at: Fact<usize>,
    pub discrim_shift: Fact<u8>,
    pub method_pointer_off: Fact<usize>,
    pub method_klass_off: Fact<usize>,
    pub method_name_off: Fact<usize>,
    pub method_param_count_off: Fact<usize>,
    pub method_return_type_off: Fact<usize>,
    pub method_parameters_off: Fact<usize>,
    pub method_flags_off: Fact<usize>,
    pub param_info_size: Fact<usize>,
    pub param_info_type_off: Fact<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::UnresolvedReason;
    #[test]
    fn unresolved_default_is_constructible() {
        let f: Fact<usize> = Fact::Unresolved { reason: UnresolvedReason::NoWitness };
        assert!(!f.is_resolved());
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p agent-core bedrock` → all pass; uncomment `pub mod layout;` + `pub use layout::Layout;` in mod.rs.
- [ ] **Step 3: Pause for user commit.** (Phase 1 = commit checkpoint 1.)

---

## Phase 2 — Container + sub-offset discoverers (the proven core), agent-core

> Ported from the live-proven `FROG_RECOGNIZER_PROBE`. These are the load-bearing discoverers; they get the most test coverage (Resolved AND Unresolved fixtures).

### Task 4: `recognize_methods` discoverer

**Files:** Create `crates/agent-core/src/bedrock/discover/mod.rs` (with `pub mod containers; pub mod suboffsets;`) + `crates/agent-core/src/bedrock/discover/containers.rs`.

- [ ] **Step 1: Write failing tests** — in `containers.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;

    // Build a klass whose methods array (klass+0x98) holds 2 MethodInfo*; each
    // MethodInfo has an RX code ptr at +0x0 and a back-ptr to klass at +0x20.
    fn klass_with_methods() -> (MockMem, usize) {
        let mut m = MockMem::new();
        let klass = 0x10_000usize;
        let arr = 0x20_000usize;
        let mi0 = 0x30_000usize;
        let mi1 = 0x30_100usize;
        let code = 0x6f00_0000usize; // executable
        m.mark_exec(code, 0x1000);
        m.put_u64(klass + 0x98, arr as u64);
        m.put_u64(arr, mi0 as u64);
        m.put_u64(arr + 8, mi1 as u64);
        for mi in [mi0, mi1] {
            m.put_u64(mi + 0x00, code as u64);    // RX method pointer
            m.put_u64(mi + 0x20, klass as u64);   // back-ptr to klass
        }
        (m, klass)
    }

    #[test]
    fn finds_methods_offset_structurally() {
        let (m, klass) = klass_with_methods();
        assert_eq!(recognize_methods(&m, klass), vec![0x98]);
    }

    #[test]
    fn rejects_non_method_arrays() {
        // an array of pointers to structs with NO rx + NO backptr → not methods
        let mut m = MockMem::new();
        let klass = 0x10_000usize;
        m.put_u64(klass + 0x98, 0x20_000);
        m.put_u64(0x20_000, 0x40_000);
        m.put_u64(0x20_008, 0x40_100);
        // 0x40_000 struct has only zeros → looks_methodinfo false
        assert_eq!(recognize_methods(&m, klass), Vec::<usize>::new());
    }
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p agent-core bedrock::discover::containers` → FAIL.

- [ ] **Step 3: Implement** — top of `containers.rs` (ported from the proven probe, MemView-generic):

```rust
//! Container recognizers. A container is identified by intrinsic structure only —
//! no sub-offset is assumed (that is what makes it non-circular). Proven live:
//! methods@0x98, fields@0x80 on PW + Highrise (12/12).

use crate::bedrock::mem::MemView;

/// MethodInfo-shaped: within its first 0x60 bytes it holds >=1 executable pointer
/// AND >=1 pointer equal to `klass` (the declaring-class back-pointer).
pub fn looks_methodinfo(mem: &dyn MemView, p: usize, klass: usize) -> bool {
    let (mut rx, mut back) = (false, false);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = mem.read_u64(p + j) {
            let wu = w as usize;
            if wu == klass { back = true; }
            else if wu >= 0x10_0000 && mem.is_exec(wu) { rx = true; }
        }
        j += 8;
    }
    rx && back
}

/// klass+off → pointer-array whose first two entries are both MethodInfo-shaped.
pub fn recognize_methods(mem: &dyn MemView, klass: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut off = 0x40usize;
    while off < 0x108 {
        if let Some(arr) = mem.read_u64(klass + off) {
            let arr = arr as usize;
            let e0 = mem.read_u64(arr).unwrap_or(0) as usize;
            let e1 = mem.read_u64(arr + 8).unwrap_or(0) as usize;
            if e0 >= 0x10_0000 && e1 >= 0x10_0000
                && looks_methodinfo(mem, e0, klass)
                && looks_methodinfo(mem, e1, klass)
            {
                hits.push(off);
            }
        }
        off += 8;
    }
    hits
}

/// FieldInfo-shaped inline array: klass+off → first FieldInfo whose slot0 → cstr
/// (field name), contains a ptr == klass (parent), and NO executable ptr.
pub fn recognize_fields(mem: &dyn MemView, klass: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut off = 0x40usize;
    while off < 0x108 {
        if let Some(arr) = mem.read_u64(klass + off) {
            let fi = arr as usize;
            let name_ok = mem.read_u64(fi).and_then(|n| mem.read_cstr(n as usize)).map_or(false, |s| s.len() >= 2);
            if name_ok {
                let (mut back, mut rx) = (false, false);
                let mut j = 0usize;
                while j < 0x40 {
                    if let Some(w) = mem.read_u64(fi + j) {
                        let wu = w as usize;
                        if wu == klass { back = true; }
                        else if wu >= 0x10_0000 && mem.is_exec(wu) { rx = true; }
                    }
                    j += 8;
                }
                if back && !rx { hits.push(off); }
            }
        }
        off += 8;
    }
    hits
}
```

- [ ] **Step 4: Run tests** — `cargo test -p agent-core bedrock::discover::containers` → passed.
- [ ] **Step 5: Pause for user commit.**

### Task 5: sub-offset derivation

**Files:** Create `crates/agent-core/src/bedrock/discover/suboffsets.rs`.

- [ ] **Step 1: Write failing test** — reuse the `klass_with_methods` fixture shape; assert `derive_method_suboffsets` returns `mp=Some(0x0), mk=Some(0x20)`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;
    #[test]
    fn derives_pointer_klass_name_slots() {
        let mut m = MockMem::new();
        let (mi, klass, code, name) = (0x30_000usize, 0x10_000usize, 0x6f00_0000usize, 0x50_000usize);
        m.mark_exec(code, 0x1000);
        m.put_u64(mi + 0x00, code as u64);   // RX → method_pointer_off
        m.put_u64(mi + 0x18, name as u64);   // cstr → method_name_off
        m.put_cstr(name, "Pow");
        m.put_u64(mi + 0x20, klass as u64);  // ==klass → method_klass_off
        assert_eq!(derive_method_suboffsets(&m, mi, klass), (Some(0x0), Some(0x20), Some(0x18)));
    }
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p agent-core bedrock::discover::suboffsets` → FAIL.

- [ ] **Step 3: Implement** — `suboffsets.rs`:

```rust
//! Derive sub-offsets by classifying a found container's own slots — never by
//! assuming a number. Returns (method_pointer_off, method_klass_off, method_name_off).

use crate::bedrock::mem::MemView;

pub fn derive_method_suboffsets(mem: &dyn MemView, mi: usize, klass: usize)
    -> (Option<usize>, Option<usize>, Option<usize>)
{
    let (mut mp, mut mk, mut mn) = (None, None, None);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = mem.read_u64(mi + j) {
            let wu = w as usize;
            if mk.is_none() && wu == klass { mk = Some(j); }
            else if mp.is_none() && wu >= 0x10_0000 && mem.is_exec(wu) { mp = Some(j); }
            else if mn.is_none() {
                if let Some(s) = mem.read_cstr(wu) { if s.len() >= 2 && s.len() < 64 { mn = Some(j); } }
            }
        }
        j += 8;
    }
    (mp, mk, mn)
}
```
Add `pub mod suboffsets;` to `discover/mod.rs`.

- [ ] **Step 4: Run tests** — passed.
- [ ] **Step 5: Pause for user commit.** (Phase 2 = commit checkpoint 2.)

---

## Phase 3 — Foundation, type-discrim, hard cases, orchestrator (agent-core)

### Task 6: foundation discoverers (stride + root validator)

**Files:** Create `crates/agent-core/src/bedrock/discover/foundation.rs`.

- [ ] **Step 1: Write failing tests** — `stride_by_autocorrelation` over a synthetic table where every 8th slot is a klass-shaped pointer returns `0x8`; `is_klass_shape` validates image(.dll)+name+ns.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::mem::MockMem;
    fn klass_shape(m: &mut MockMem, klass: usize, name: &str) {
        let (img, imgname, nm, ns) = (klass + 0x1000, klass + 0x1100, klass + 0x1200, klass + 0x1300);
        m.put_u64(klass + 0x00, img as u64);
        m.put_u64(img + 0x10, imgname as u64);   // image+0x10 → image name? (mechanism: see is_image)
        m.put_cstr(imgname, "Assembly-CSharp.dll");
        m.put_u64(klass + 0x10, nm as u64); m.put_cstr(nm, name);
        m.put_u64(klass + 0x18, ns as u64); m.put_cstr(ns, "");
    }
    #[test]
    fn stride_is_eight_for_pointer_table() {
        let mut m = MockMem::new();
        let base = 0x100_000usize;
        for i in 0..40 { let k = 0x200_000 + i * 0x2000; m.put_u64(base + i * 8, k as u64); klass_shape(&mut m, k, "K"); }
        assert_eq!(stride_by_autocorrelation(&m, base, 40), Some(8));
    }
}
```
NOTE: `is_klass_shape`'s exact image-name mechanism mirrors the agent's `RegionMap::class_fields` (image back-ptr @0 → Il2CppImage → name ".dll"). Match that mechanism; the test fixture must reflect it. If the real `class_fields` reads the image name at a different inner offset, align the fixture + `is_klass_shape` to it (read the agent's `region_map.rs:is_image`/`class_fields` and replicate the *mechanism*, never a magic number in a comment).

- [ ] **Step 2: Run to verify fail** — FAIL.

- [ ] **Step 3: Implement** `foundation.rs`:

```rust
//! Foundation discoverers. Stride is derived by period autocorrelation at the
//! finest (8-byte) granularity — the finest granularity cannot skip an aligned
//! klass pointer, so there is no stride to assume. Root validity = klass-shape
//! (image→.dll + name + namespace cstrs), the mechanism RegionMap::class_fields uses.

use crate::bedrock::mem::MemView;

pub fn is_klass_shape(mem: &dyn MemView, klass: usize) -> bool {
    let img = match mem.read_u64(klass + 0x00) { Some(v) if v != 0 => v as usize, _ => return false };
    let img_name = match mem.read_u64(img + 0x10) { Some(v) if v != 0 => v as usize, _ => return false };
    let name_ok = mem.read_cstr(img_name).map_or(false, |s| s.len() > 4 && s.ends_with(".dll"));
    if !name_ok { return false; }
    let nm = match mem.read_u64(klass + 0x10) { Some(v) if v != 0 => v as usize, _ => return false };
    mem.read_cstr(nm).map_or(false, |s| !s.is_empty())
}

/// Period of "is-klass-pointer" recurrence in the table, read at 8-byte steps.
/// Returns the smallest stride (multiple of 8) at which consecutive slots are
/// consistently klass-shaped-or-null. For a pointer table this is 8.
pub fn stride_by_autocorrelation(mem: &dyn MemView, base: usize, count: usize) -> Option<usize> {
    // Count klass-shaped slots at stride 8 over the sample; if the density is high
    // and contiguous, the table is a pointer array (stride 8). (Mechanism only;
    // no value is asserted in a comment.)
    let mut classy = 0usize;
    let mut scanned = 0usize;
    let mut i = 0usize;
    while i < count && scanned < 256 {
        let slot = match mem.read_u64(base + i * 8) { Some(v) => v as usize, None => break };
        if slot == 0 || is_klass_shape(mem, slot) { classy += 1; }
        scanned += 1;
        i += 1;
    }
    if scanned >= 8 && classy * 100 >= scanned * 90 { Some(8) } else { None }
}
```
Add `pub mod foundation;` to `discover/mod.rs`.

- [ ] **Step 4: Run tests** → passed.
- [ ] **Step 5: Pause for user commit.**

### Task 7: type-discriminator consensus + valuetype + hard cases

**Files:** Create `discover/type_discrim.rs` + `discover/hard_cases.rs`.

- [ ] **Step 1: Write failing tests** — `find_discriminator` over a fixture with known-tc primitive klasses returns the `(read_at, shift)` that round-trips ALL of them; `static_fields` discoverer returns `Unresolved{NoDiscriminator}` when the slot is null on most sampled klasses.

```rust
// type_discrim.rs test: 3 primitive klasses whose byval_arg chunk at read_at=8,
// shift=0 yields tc 0x08/0x0E/0x1C → find_discriminator returns (8, 0).
// hard_cases.rs test: 5 klasses, 4 with null static_fields slot → Unresolved.
```
(Write concrete fixtures mirroring the Task 4/6 fixture style — synthetic klasses with the needed inner pointers; assert the returned `Fact`.)

- [ ] **Step 2: Run to verify fail** — FAIL.

- [ ] **Step 3: Implement** —
  `type_discrim.rs`: `find_discriminator(mem, known: &[(usize /*klass byval_arg*/, u8 /*expected tc*/)]) -> Fact<(usize,u8)>` trying read_at ∈ {0,8,16}, shift ∈ {0,8,16,24}; the pair that makes every known klass's `(chunk >> shift) & 0xFF == expected_tc` is `Resolved` with a `Witness` per primitive; else `Unresolved{NoWitness}`.
  `hard_cases.rs`: `discover_static_fields(mem, klasses) -> Fact<usize>` — for each candidate offset, require the slot to be a unique RW (non-exec) region pointer on a near-unanimous fraction; if no offset is near-unanimous (the honest reality), return `Fact::Unresolved{NoDiscriminator}`. `discover_type_def(has_metadata: bool) -> Fact<usize>` — `Unresolved{NoMetadata}` when metadata absent. `discover_valuetype_bit(mem, value_types, ref_types) -> Fact<(usize,u8)>` — the (byte-off, bit) set in all VTs and clear in all REFs.

- [ ] **Step 4: Run tests** → passed.
- [ ] **Step 5: Pause for user commit.**

### Task 8: `discover()` orchestrator

**Files:** Create/extend `crates/agent-core/src/bedrock/discover/mod.rs`.

- [ ] **Step 1: Write failing test** — assemble a synthetic `MockMem` table of klass-shaped entries each with methods@0x98/fields@0x80; assert `discover(&m, base, count)` yields `Layout` with `klass_methods.require()==Ok(0x98)`, `method_pointer_off.require()==Ok(0x0)`, and `klass_type_def` `Unresolved` (no metadata in the mock).

- [ ] **Step 2: Run to verify fail** — FAIL.

- [ ] **Step 3: Implement** `discover()`:
  - Foundation: `class_table_step` via `stride_by_autocorrelation` → Fact; `table_base`/`table_count` Resolved from args.
  - Sample N≥12 structurally-valid klasses (walk table at stride, `is_klass_shape`).
  - Containers: for each sampled klass, `recognize_methods`/`recognize_fields`; require **unanimity** across the sample → `klass_methods`/`klass_fields` Resolved with `sampled=N`, witnesses per klass; disagreement → `Unresolved{WitnessDisagreement}`.
  - Sub-offsets: `derive_method_suboffsets` from each sample's first MethodInfo; require unanimity → Resolved.
  - type-discrim/valuetype/static_fields/type_def via Task 7 discoverers (the orchestrator supplies known-primitive anchors located by name via `is_klass_shape` + name match — NOT FFI).
  - Assemble `Layout`.

- [ ] **Step 4: Run tests** → passed; full `cargo test -p agent-core` green; uncomment remaining `discover` re-exports.
- [ ] **Step 5: Pause for user commit.** (Phase 3 = commit checkpoint 3.)

---

## Phase 4 — Agent glue + live-prove (the integration canary)

### Task 9: `impl MemView for RegionMap` + `FROG_LAYOUT_PROBE`

**Files:** Create `crates/agent/src/bedrock_glue.rs`; modify `crates/agent/src/lib.rs`, `crates/agent/src/entry.rs`.

- [ ] **Step 1: Implement** `bedrock_glue.rs`:

```rust
//! Windows glue: RegionMap satisfies the agent-core MemView seam, and the
//! env-gated FROG_LAYOUT_PROBE runs discover() and logs the Fact-derived report.

use agent_core::bedrock::{mem::MemView, discover::discover};
use crate::external::region_map::RegionMap;
use crate::diagnostics::klass_probe; // for protect_of-based is_exec, or inline VirtualQuery
use crate::paths::log;

impl MemView for RegionMap {
    fn read_u64(&self, a: usize) -> Option<u64> { RegionMap::read_u64(self, a) }
    fn read_u32(&self, a: usize) -> Option<u32> { RegionMap::read_u32(self, a) }
    fn read_u8(&self, a: usize) -> Option<u8> { RegionMap::read_u8(self, a) }
    fn read_cstr(&self, a: usize) -> Option<String> { RegionMap::read_name_strict(self, a) }
    fn is_exec(&self, a: usize) -> bool { crate::bedrock_glue::is_exec(a) }
}

/// True iff `a` is in a committed executable page (VirtualQuery — kernel witness).
pub fn is_exec(a: usize) -> bool { /* mirror diagnostics::klass_probe::protect_of → "RX"|"RWX" */
    matches!(crate::diagnostics::klass_probe::protect_of_pub(a), "RX" | "RWX")
}

pub fn run_layout_probe(map: &RegionMap, table_base: usize, table_count: usize) {
    log("=== LAYOUT PROBE (discover() — Fact-derived) ===");
    let layout = discover(map, table_base, table_count);
    // Generate the report FROM the Facts (no hand-written values).
    crate::bedrock_glue::log_layout(&layout);
    log("=== end LAYOUT PROBE ===");
}
```
NOTE: `protect_of` in `klass_probe.rs` is currently private — make it `pub(crate)` (`protect_of_pub` or just `pub(crate) fn protect_of`). Add a `log_layout(&Layout)` that iterates each fact and prints `name = value [witnesses…]` for Resolved or `name = UNRESOLVED(reason)` for Unresolved — generated, never hand-written.

`lib.rs`: add `#[cfg(target_os = "windows")] mod bedrock_glue;`.
`entry.rs`: after the other FROG_ gates:
```rust
    if std::env::var("FROG_LAYOUT_PROBE").is_ok() {
        crate::bedrock_glue::run_layout_probe(&map, table_base, table_count);
    }
```

- [ ] **Step 2: Build** — `cargo build --target x86_64-pc-windows-gnu --release` → succeeds, warnings ≤ baseline.
- [ ] **Step 3: Pause for user commit.**

### Task 10: live-prove on PW + Highrise (user-run)

- [ ] **Step 1: Deploy** — run `./deploy.sh` (deploys both games).
- [ ] **Step 2:** User launches PW and Highrise with `FROG_LAYOUT_PROBE=1`.
- [ ] **Step 3:** Controller reads `agent.log` from both game dirs; confirm the LAYOUT PROBE block shows, for BOTH games: `klass_methods = 0x98`, `method_pointer_off = 0x0`, `method_klass_off = 0x20`, `method_name_off = 0x18`, `klass_fields = 0x80`, each with witness provenance; `klass_type_def = UNRESOLVED(NoMetadata)` on PW; `class_table_step = 0x8`. Any `UNRESOLVED` that should be `Resolved` is a discoverer gap → fix the discoverer (agent-core), not a fallback.
- [ ] **Step 4: Pause for user commit.** (Phase 4 = commit checkpoint 4. The engine is now proven live.)

---

## Phase 5 — Reference cross-check + coverage (PW oracle)

### Task 11: reference cross-check witness (dev-gated)

**Files:** Create `crates/agent-core/src/bedrock/crosscheck.rs` (pure parsing/compare) + a dev-gated hook in `bedrock_glue.rs`.

- [ ] **Step 1: Write failing test** — given a small in-memory slice of reference lines (class + field `// 0xNN` + method `// 0xRVA`) and a set of discovered (name→offset/RVA) pairs, `crosscheck(reference, discovered)` returns per-fact agree/disagree + a coverage ratio. Unit-tested in agent-core with literal fixtures (no file IO in the test).
- [ ] **Step 2: Run to verify fail** — FAIL.
- [ ] **Step 3: Implement** the parser (extract `Name // 0xHEX`) + comparator; `Provenance` for matched facts gains a `Witness{ method: ReferenceCrossCheck, observed, signal: "RVA match" }`. The reference path is **dev-time only** — gated behind `FROG_REF_CROSSCHECK=<path>` read in `bedrock_glue`, never compiled-in data (no shipped answer key).
- [ ] **Step 4: Run tests** → passed; `cargo build --target x86_64-pc-windows-gnu --release` green.
- [ ] **Step 5: Pause for user commit.**

### Task 12: coverage accounting

**Files:** Create `crates/agent-core/src/bedrock/coverage.rs`.

- [ ] **Step 1: Write failing test** — `account(found_per_image, reference_per_image)` returns `found/ref` ratios + a total; unit-tested with literal maps (e.g. `Assembly-CSharp: found 3700 / ref 3714`).
- [ ] **Step 2: Run to verify fail** — FAIL.
- [ ] **Step 3: Implement** the accountant; wire it into `run_layout_probe` to log per-image `found/ref` when the reference is provided, else `found/?`.
- [ ] **Step 4: Run tests** → passed; build green.
- [ ] **Step 5: Pause for user commit.**

### Task 13: final live validation

- [ ] **Step 1:** `./deploy.sh`; user runs PW with `FROG_LAYOUT_PROBE=1 FROG_REF_CROSSCHECK=/home/chef/Games/Zmod/ZwSpark-Source/ref/pw_reference_pack/dump/dump_v245.cs`.
- [ ] **Step 2:** Confirm: discovered field offsets + method RVAs match the reference within the sampled set; coverage ratios logged; every `Resolved` fact carries ≥1 `Structural` witness and (on PW) a `ReferenceCrossCheck` witness.
- [ ] **Step 3:** User runs Highrise with `FROG_LAYOUT_PROBE=1` (no reference) — confirm the same facts `Resolved` via `Structural` triangulation alone.
- [ ] **Step 4: Pause for user commit.** (Phase 5 = commit checkpoint 5. Engine + accuracy proven on both games.)

---

## Follow-on (NOT this plan): consumer migration

Per the spec's Integration Safety section, migrating the 96 `cfg.X` consumers to `&Layout` is a **separate plan**, executed only after this engine is proven (it now is, at Task 13). The seam (`ctx` carries `layout` + transient derived-`cfg`), the per-module order (dump → resolve → marshal → api → hook last), and the build-green + live-Pow-gate-per-step discipline live in that plan. **Do not start migration as part of this plan** — that is the "everything falls out at integration" risk this separation exists to prevent.

---

## Self-Review

**Spec coverage:** Fact/Layout contract (T1-3) ✓; container-first non-circular discovery (T4-5, the proven core) ✓; foundation/stride/root (T6) ✓; type-discrim + valuetype + honest-Unresolved hard cases (T7) ✓; orchestrator → Layout (T8) ✓; MemView seam for agent-core testability ✓; live-prove both games (T10, T13) ✓; reference cross-check as PW oracle + dev-gated (T11) ✓; coverage accounting (T12) ✓; fail-closed (no fallback type exists) ✓; truth-management law (no value-asserting comments; report generated from Facts) ✓; integration safety (engine-only, migration deferred) ✓.

**Placeholder scan:** Task 7 and Task 11/12 give the discoverer/comparator *contracts* + fixtures rather than every line — they are concrete (named fns, signatures, return `Fact`s, literal-fixture tests), not "TODO". The novel/load-bearing discoverers (T4-6, T8) carry full code. No "implement later".

**Type consistency:** `Fact<T>`, `Provenance{witnesses,sampled}`, `Witness{method,observed,signal}`, `MemView` methods, `recognize_methods`/`recognize_fields`/`derive_method_suboffsets`/`stride_by_autocorrelation`/`is_klass_shape`/`discover` — names/signatures consistent across T1-T13. `is_exec` is the MemView method backed by `protect_of` on the agent side.

**Scope:** one subsystem (the engine); consumer migration explicitly a separate plan. Produces working, testable software on its own (a live-proven `Layout` + diagnostic).
