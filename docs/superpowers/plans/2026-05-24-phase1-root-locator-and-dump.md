# Phase 1: Root Locator + Complete-Tree Dump — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reach the il2cpp global-metadata root by anchoring on the string heap (no magic, no obfuscated calls), then walk the full typeDefinition table to dump every type — loaded or not — with namespace, field names, and method names to `internals.txt`.

**Architecture:** Pure parsing/validation logic lives in `agent-core` (host-tested with synthetic fixtures). The `agent` crate does the Windows-only memory access: it locates the string heap from loaded-class name pointers, hands a captured byte region to `agent-core` to find the header (cross-checked against that heap) and parse the tree, then writes the dump. The make-or-break is the Locator; everything downstream is mechanical once the root is confirmed.

**Tech Stack:** Rust, cross-compiled `x86_64-pc-windows-gnu`. `agent-core` is `#![no_std`-free pure Rust (host-testable). `agent` uses `windows-sys` for `VirtualQuery`. Existing infra reused: `agent-core/src/metadata.rs` (blob parser), `agent/src/mem_scan.rs` (`RegionMap`).

**Scope note (read before starting):** This plan covers Phase 1 only — reach the root and dump the complete tree (types + namespaces + field names + method names). Deliberately deferred: field *type* names and method *signatures* (Phase 3 quality), live-events watch (Phase 4), dead-code cleanup (Phase 2), the plugin API (Phase 5). Each gets its own plan.

---

## File Structure

- `crates/agent-core/src/model.rs` — **modify**: add `methods: Vec<String>` to `DumpedClass`; add `Dump::total_methods()`.
- `crates/agent-core/src/format.rs` — **modify**: render method names.
- `crates/agent-core/src/metadata.rs` — **modify**: extend `MetadataLayout`/`MetaHeader` for `string_size` + methods; refactor the tree walk out of `parse_metadata` into a magic-independent `parse_tree`; add `locate_header_by_string_anchor` + `parse_metadata_anchored`; add the v24 layout + wire `layout_for_version(24)`.
- `crates/agent/src/locator.rs` — **create**: Windows glue — derive the string-heap address range from loaded-class name pointers, capture a byte region, call `agent-core` to locate+parse.
- `crates/agent/src/lib.rs` — **modify**: `mod locator;`.
- `crates/agent/src/entry.rs` — **modify**: worker calls the locator path and writes `internals.txt`.

---

## Stage A — agent-core pure logic (host-tested, TDD)

### Task A1: Add method names to the model

**Files:**
- Modify: `crates/agent-core/src/model.rs`
- Modify (fix existing test): `crates/agent-core/src/format.rs:30-56`

- [ ] **Step 1: Update the failing test in model**

Add to the `#[cfg(test)]` section of `crates/agent-core/src/model.rs` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_methods() {
        let dump = Dump {
            classes: vec![DumpedClass {
                namespace: "Game".into(),
                name: "Player".into(),
                fields: vec![],
                methods: vec!["Update".into(), "Start".into()],
            }],
        };
        assert_eq!(dump.total_methods(), 2);
    }
}
```

- [ ] **Step 2: Run it, expect failure**

Run: `cargo test -p agent-core model::tests::counts_methods`
Expected: FAIL — `DumpedClass` has no field `methods`, no method `total_methods`.

- [ ] **Step 3: Implement**

In `crates/agent-core/src/model.rs`, add `methods` to `DumpedClass` and `total_methods` to `Dump`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<DumpedField>,
    pub methods: Vec<String>,
}
```

```rust
    pub fn total_methods(&self) -> usize {
        self.classes.iter().map(|c| c.methods.len()).sum()
    }
```

- [ ] **Step 4: Fix the two existing call sites that build `DumpedClass`**

`crates/agent-core/src/format.rs:30-56` test and `crates/agent-core/src/metadata.rs` (`parse_metadata` ~line 142 and its tests ~line 304): add `methods: vec![]` (or real data in A4) to each `DumpedClass { … }` literal. For now in `metadata.rs:142` add `methods: Vec::new(),`. In the `format.rs` test and `metadata.rs` `parses_full_blob_to_dump` test, add `methods: vec![]` to the expected literals.

- [ ] **Step 5: Run all agent-core tests**

Run: `cargo test -p agent-core`
Expected: PASS (all existing + new).

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/model.rs crates/agent-core/src/format.rs crates/agent-core/src/metadata.rs
git commit -m "feat(agent-core): add method names to dump model"
```

### Task A2: Render method names in the formatter

**Files:**
- Modify: `crates/agent-core/src/format.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/agent-core/src/format.rs` tests:

```rust
    #[test]
    fn renders_methods() {
        let dump = Dump {
            classes: vec![DumpedClass {
                namespace: String::new(),
                name: "Player".into(),
                fields: vec![DumpedField { name: "hp".into(), type_name: String::new() }],
                methods: vec!["Update".into()],
            }],
        };
        let text = format_dump(&dump);
        assert!(text.contains("hp;"));
        assert!(text.contains("Update();"));
    }
```

- [ ] **Step 2: Run it, expect failure**

Run: `cargo test -p agent-core format::tests::renders_methods`
Expected: FAIL — methods are not rendered.

- [ ] **Step 3: Implement**

In `crates/agent-core/src/format.rs`, inside the per-class loop, after the fields loop and before the closing `}`:

```rust
        for method in &class.methods {
            out.push_str(&format!("    {}();\n", method));
        }
```

Also update the header counts line to include methods:

```rust
    out.push_str(&format!(
        "# Unity internals dump\n# classes: {}, fields: {}, methods: {}\n\n",
        dump.class_count(),
        dump.total_fields(),
        dump.total_methods()
    ));
```

Update the existing `formats_classes_and_fields` expected string's count line to `# classes: 1, fields: 2, methods: 0`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p agent-core format`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/format.rs
git commit -m "feat(agent-core): render method names in dump"
```

### Task A3: Magic-independent header locate via string anchor

This is the centerpiece. We find the header without the magic by requiring that its `string_offset` points back into the string-heap range we observed from loaded-class name pointers.

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`

- [ ] **Step 1: Extend `MetadataLayout` and `MetaHeader`**

In `crates/agent-core/src/metadata.rs`, add to `MetadataLayout` (after `h_string_size`):

```rust
    pub h_methods_offset: usize,
    pub h_methods_size: usize,
```

and after `type_field_count`:

```rust
    pub type_method_start: usize,
    pub type_method_count: usize,
    pub method_size: usize,
    pub method_name_index: usize,
```

Add to `MetaHeader`:

```rust
    pub string_size: u32,
    pub methods_offset: u32,
```

In `parse_header`, populate them (keep the magic check for the existing path):

```rust
        string_size: read_u32(bytes, layout.h_string_size)?,
        methods_offset: read_u32(bytes, layout.h_methods_offset)?,
```

Update `TEST_LAYOUT` (and `test_header`/`test_blob` in tests) so the new fields have valid values; for `TEST_LAYOUT` add `h_methods_offset: 24, h_methods_size: 28` is taken — instead reuse spare header room. Extend the test header to 48 bytes: set `h_methods_offset: 40, h_methods_size: 44`, `type_method_start: 14, type_method_count: 18` (widen `type_size` to 20), `method_size: 8, method_name_index: 0`. (Adjust the synthetic `test_blob` offsets accordingly so existing tests still pass — recompute the byte layout in the test the same way it already does.)

- [ ] **Step 2: Write the failing test for the anchor locate**

Add to `crates/agent-core/src/metadata.rs` tests:

```rust
    #[test]
    fn locates_header_by_string_anchor_without_magic() {
        // Build a region: [pad][header+tables+strings]. Strip the magic.
        let mut region = vec![0u8; 32];
        let header_off_in_region = region.len();
        let mut blob = test_blob();
        blob[0..4].copy_from_slice(&0u32.to_le_bytes()); // strip magic
        region.extend_from_slice(&blob);

        // The string heap in test_blob lives at blob offset 40 (h_string_offset value).
        let region_base = 0x10_000usize;
        let str_abs = region_base + header_off_in_region + 40; // abs start of strings
        // A name pointer we "observed" — somewhere inside the string heap.
        let observed_lo = str_abs + 2;
        let observed_hi = str_abs + 6;

        let off = locate_header_by_string_anchor(
            &region, region_base, observed_lo, observed_hi, &TEST_LAYOUT,
        );
        assert_eq!(off, Some(header_off_in_region));
    }
```

- [ ] **Step 3: Run it, expect failure**

Run: `cargo test -p agent-core metadata::tests::locates_header_by_string_anchor_without_magic`
Expected: FAIL — `locate_header_by_string_anchor` not defined.

- [ ] **Step 4: Implement the anchor locate**

Add to `crates/agent-core/src/metadata.rs`:

```rust
/// Read a header at `off` WITHOUT requiring the magic, validating that its table
/// offsets are in-bounds and that its string heap [base+string_offset,
/// +string_size) brackets the observed name-pointer range [str_lo, str_hi].
/// `region_base` is the absolute address of `bytes[0]`. Returns the header if the
/// candidate is self-consistent and string-anchored.
fn header_anchor_ok(
    bytes: &[u8],
    off: usize,
    layout: &MetadataLayout,
    region_base: usize,
    str_lo: usize,
    str_hi: usize,
) -> Option<MetaHeader> {
    let string_offset = read_u32(bytes, off + layout.h_string_offset)?;
    let string_size = read_u32(bytes, off + layout.h_string_size)?;
    let type_defs_offset = read_u32(bytes, off + layout.h_type_defs_offset)?;
    let fields_offset = read_u32(bytes, off + layout.h_fields_offset)?;
    let methods_offset = read_u32(bytes, off + layout.h_methods_offset)?;
    let images_offset = read_u32(bytes, off + layout.h_images_offset)?;
    let images_size = read_u32(bytes, off + layout.h_images_size)?;

    // The string heap's absolute span must bracket the observed pointers.
    let str_start = region_base.checked_add(off)?.checked_add(string_offset as usize)?;
    let str_end = str_start.checked_add(string_size as usize)?;
    if string_size == 0 || str_start > str_lo || str_end < str_hi {
        return None;
    }
    // All table offsets must be plausible (non-zero, ascending-ish, modest).
    let max_off = string_offset
        .max(type_defs_offset)
        .max(fields_offset)
        .max(methods_offset)
        .max(images_offset);
    if type_defs_offset == 0 || fields_offset == 0 || methods_offset == 0
        || images_offset == 0 || images_size == 0
        || (max_off as usize) > bytes.len()
    {
        return None;
    }
    Some(MetaHeader {
        string_offset,
        string_size,
        type_defs_offset,
        fields_offset,
        methods_offset,
        images_offset,
        images_size,
    })
}

/// Scan `bytes` (a captured memory region whose first byte is at absolute
/// `region_base`) for a metadata header whose string heap brackets the observed
/// name-pointer range [str_lo, str_hi]. Returns the byte offset of the header.
/// Magic is NOT required — PW strips it.
pub fn locate_header_by_string_anchor(
    bytes: &[u8],
    region_base: usize,
    str_lo: usize,
    str_hi: usize,
    layout: &MetadataLayout,
) -> Option<usize> {
    // Headers are 4-byte aligned. Walk candidate offsets.
    let mut off = 0usize;
    while off + layout.h_images_size + 4 <= bytes.len() {
        if header_anchor_ok(bytes, off, layout, region_base, str_lo, str_hi).is_some() {
            return Some(off);
        }
        off += 4;
    }
    None
}
```

- [ ] **Step 5: Run it, expect pass**

Run: `cargo test -p agent-core metadata::tests::locates_header_by_string_anchor_without_magic`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/metadata.rs
git commit -m "feat(agent-core): locate metadata header via string anchor (no magic)"
```

### Task A4: Magic-independent tree walk (incl. methods) + anchored parse

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/agent-core/src/metadata.rs` tests (uses the same `test_blob` extended in A3, which now includes a method table):

```rust
    #[test]
    fn anchored_parse_reads_tree_without_magic() {
        let mut region = vec![0u8; 32];
        let mut blob = test_blob();
        blob[0..4].copy_from_slice(&0u32.to_le_bytes()); // strip magic
        let header_off = region.len();
        region.extend_from_slice(&blob);

        let region_base = 0x20_000usize;
        let str_abs = region_base + header_off + 40;
        let dump = parse_metadata_anchored(
            &region, region_base, str_abs + 1, str_abs + 6, &TEST_LAYOUT,
        )
        .unwrap();
        assert_eq!(dump.classes.len(), 1);
        assert_eq!(dump.classes[0].name, "Player");
    }
```

- [ ] **Step 2: Run it, expect failure**

Run: `cargo test -p agent-core metadata::tests::anchored_parse_reads_tree_without_magic`
Expected: FAIL — `parse_metadata_anchored` not defined.

- [ ] **Step 3: Refactor the tree walk + add anchored parse**

In `crates/agent-core/src/metadata.rs`, extract the body of `parse_metadata` (everything after `let h = parse_header(...)?;` and the bounds check) into a new `fn parse_tree(bytes: &[u8], layout: &MetadataLayout, h: &MetaHeader) -> Dump`. `parse_metadata` becomes: parse header (with magic), run the in-bounds check, call `parse_tree`. Inside `parse_tree`, after building `fields`, also read methods:

```rust
            let method_start = read_i32(bytes, tdef + layout.type_method_start).unwrap_or(-1);
            let method_count = read_u16(bytes, tdef + layout.type_method_count).unwrap_or(0) as usize;
            let mut methods = Vec::new();
            if method_start >= 0 {
                let ms = method_start as usize;
                for m in 0..method_count {
                    let mdef = h.methods_offset as usize + (ms + m) * layout.method_size;
                    if let Some(mname_idx) = read_u32(bytes, mdef + layout.method_name_index) {
                        methods.push(read_name(mname_idx));
                    }
                }
            }
```

and include `methods` in the pushed `DumpedClass`.

Then add the anchored entry point:

```rust
/// Find the header by string anchor (no magic), then walk the full tree.
pub fn parse_metadata_anchored(
    bytes: &[u8],
    region_base: usize,
    str_lo: usize,
    str_hi: usize,
    layout: &MetadataLayout,
) -> Option<Dump> {
    let off = locate_header_by_string_anchor(bytes, region_base, str_lo, str_hi, layout)?;
    let h = header_anchor_ok(bytes, off, layout, region_base, str_lo, str_hi)?;
    Some(parse_tree(&bytes[off..], layout, &h))
}
```

- [ ] **Step 4: Run all agent-core tests**

Run: `cargo test -p agent-core`
Expected: PASS (existing magic-path tests + new anchored tests).

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/metadata.rs
git commit -m "feat(agent-core): magic-independent tree walk with methods + anchored parse"
```

### Task A5: Transcribe the v24 metadata layout

The synthetic `TEST_LAYOUT` proves the logic. The real PW build is metadata **v24.x** (ref pack pins v245). Transcribe the real byte layout so the agent can parse the live blob.

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`

- [ ] **Step 1: Transcribe from the authoritative source**

Open `Il2CppDumper`'s `Il2Cpp/MetadataClasses.cs` (the `Il2CppGlobalMetadataHeader`, `Il2CppImageDefinition`, `Il2CppTypeDefinition`, `Il2CppFieldDefinition`, `Il2CppMethodDefinition` structs, `[Version(Min = 24, ...)]` fields). Cross-reference `ref/pw_reference_pack/` if it carries struct sizes. Fill a `const LAYOUT_V24: MetadataLayout` with the exact byte positions. As a starting point (VERIFY each against the source before trusting — these are the documented v24 positions):

```rust
/// Metadata layout for il2cpp v24.x (header field byte positions; struct offsets
/// and sizes). Transcribed from Il2CppDumper MetadataClasses.cs (Version>=24).
/// Validated at runtime by the string-anchor cross-check + name re-derivation.
const LAYOUT_V24: MetadataLayout = MetadataLayout {
    h_string_offset: 0x18,
    h_string_size: 0x1C,
    h_methods_offset: 0x30,
    h_methods_size: 0x34,
    h_fields_offset: 0x60,
    h_fields_size: 0x64,
    h_type_defs_offset: 0xA0,
    h_type_defs_size: 0xA4,
    h_images_offset: 0xA8,
    h_images_size: 0xAC,
    image_size: 0x28,
    image_name_index: 0x00,
    image_type_start: 0x08,
    image_type_count: 0x0C,
    type_size: 0x5C,
    type_name_index: 0x00,
    type_namespace_index: 0x04,
    type_field_start: 0x24,
    type_field_count: 0x48,
    type_method_start: 0x28,
    type_method_count: 0x44,
    method_size: 0x2C,
    method_name_index: 0x00,
    field_size: 0x0C,
    field_name_index: 0x00,
};
```

NOTE: the header field positions (`h_images_offset` especially) and `type_size`/count offsets MUST be confirmed against the v24 source — they shift between minor versions. The runtime gate (Task C1) is the real validator: if any are wrong, the dump will be empty or garbled and you iterate here.

- [ ] **Step 2: Wire `layout_for_version`**

```rust
pub fn layout_for_version(version: u32) -> Option<MetadataLayout> {
    match version {
        24 => Some(LAYOUT_V24),
        _ => None,
    }
}
```

- [ ] **Step 3: Build check**

Run: `cargo build -p agent-core && cargo test -p agent-core`
Expected: compiles; tests still PASS (this const isn't exercised by host tests).

- [ ] **Step 4: Commit**

```bash
git add crates/agent-core/src/metadata.rs
git commit -m "feat(agent-core): add v24 metadata layout"
```

---

## Stage B — agent Windows glue (manual-tested)

### Task B1: String-heap range from loaded classes

Reuse `RegionMap` and the class fingerprint already in `crates/agent/src/mem_scan.rs`. Add a function that finds loaded `Il2CppClass` structs and returns the min/max of their name pointers (the string-heap bracket) plus a region to capture.

**Files:**
- Create: `crates/agent/src/locator.rs`
- Modify: `crates/agent/src/lib.rs` (add `mod locator;`)

- [ ] **Step 1: Implement the name-pointer bracket**

Create `crates/agent/src/locator.rs`:

```rust
//! Phase 1 locator: trace from loaded-class name pointers down to the metadata
//! root, then hand the captured bytes to agent-core to parse the full tree.

use agent_core::metadata::{layout_for_version, parse_metadata_anchored};
use agent_core::model::Dump;

use crate::mem_scan::RegionMap;

/// Scan the crash-safe region envelope for loaded Il2CppClass structs and return
/// the [min, max] absolute address of their name pointers (the string-heap
/// bracket), plus the lowest name pointer's region for capture. Read-only.
pub fn string_heap_bracket(map: &RegionMap) -> Option<(usize, usize)> {
    map.class_name_ptr_bracket()
}
```

Add the supporting method to `RegionMap` in `crates/agent/src/mem_scan.rs` (it already has `class_fields`, `read_u64`, region iteration). It must walk the same first-64-region envelope, collect the name pointer (`read_u64(p + 0x10)`) for every class-shaped slot, and return `(min, max)` over at least, say, 32 classes:

```rust
impl RegionMap {
    /// Min/max of name pointers across loaded classes found in the envelope.
    pub fn class_name_ptr_bracket(&self) -> Option<(usize, usize)> {
        const MAX_SCAN_REGIONS: usize = 64;
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        let mut found = 0usize;
        for (ri, &(start, end)) in self.regions.iter().enumerate() {
            if ri >= MAX_SCAN_REGIONS { break; }
            let mut a = start;
            while a + 8 <= end {
                let slot = unsafe { *(a as *const u64) } as usize;
                if slot != 0 {
                    if let Some(np) = self.read_u64(slot.wrapping_add(0x10)) {
                        let np = np as usize;
                        if self.class_fields(slot).is_some() {
                            if np < lo { lo = np; }
                            if np > hi { hi = np; }
                            found += 1;
                        }
                    }
                }
                a += 8;
            }
        }
        if found >= 32 && lo <= hi { Some((lo, hi)) } else { None }
    }
}
```

(`regions`, `read_u64`, `class_fields` already exist in `mem_scan.rs`. Make `regions` accessible to this method — it's the same impl block.)

Add `pub mod locator;` to `crates/agent/src/lib.rs`.

- [ ] **Step 2: Build check**

Run: `cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -3`
Expected: compiles (warnings OK).

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/locator.rs crates/agent/src/lib.rs crates/agent/src/mem_scan.rs
git commit -m "feat(agent): string-heap bracket from loaded class name pointers"
```

### Task B2: Capture region + locate + parse

**Files:**
- Modify: `crates/agent/src/locator.rs`
- Modify: `crates/agent/src/mem_scan.rs` (add a validated region-bytes copy)

- [ ] **Step 1: Add a validated region-bytes reader to `mem_scan.rs`**

```rust
impl RegionMap {
    /// Copy `len` bytes starting at `base` into a Vec, but only the prefix that
    /// stays inside one committed region (stops at the region end). Read-only,
    /// never faults.
    pub fn copy_region_bytes(&self, base: usize, len: usize) -> Vec<u8> {
        // Find the region containing base.
        let idx = match self.regions.binary_search_by(|r| r.0.cmp(&base)) {
            Ok(i) => i,
            Err(0) => return Vec::new(),
            Err(i) => i - 1,
        };
        let (start, end) = self.regions[idx];
        if base < start || base >= end { return Vec::new(); }
        let avail = (end - base).min(len);
        unsafe { std::slice::from_raw_parts(base as *const u8, avail).to_vec() }
    }
}
```

- [ ] **Step 2: Implement the full locate-and-parse in `locator.rs`**

```rust
/// Reach the metadata root and parse the complete tree. Strategy: bracket the
/// string heap from loaded classes, capture the region that contains it, then ask
/// agent-core to find the header (anchored on that bracket) and walk the tree.
/// Returns the Dump (every type, loaded or not) on success.
pub fn dump_all_internals() -> Option<Dump> {
    let map = RegionMap::capture(8192);
    let (str_lo, str_hi) = string_heap_bracket(&map)?;

    // The header precedes the string heap in one allocation. Capture a window
    // that starts a bounded distance before the observed strings and runs through
    // the metadata tables. 16 MiB comfortably spans header+tables+strings.
    const BACK: usize = 8 * 1024 * 1024;
    const SPAN: usize = 32 * 1024 * 1024;
    let region_base = str_lo.saturating_sub(BACK);
    let bytes = map.copy_region_bytes(region_base, SPAN);
    if bytes.is_empty() { return None; }

    // PW is metadata v24.x.
    let layout = layout_for_version(24)?;
    parse_metadata_anchored(&bytes, region_base, str_lo, str_hi, &layout)
}
```

- [ ] **Step 3: Build check**

Run: `cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -3`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/agent/src/locator.rs crates/agent/src/mem_scan.rs
git commit -m "feat(agent): capture region, locate root, parse complete tree"
```

### Task B3: Wire the worker to dump internals.txt

**Files:**
- Modify: `crates/agent/src/entry.rs`

- [ ] **Step 1: Replace the PULL-TEST scan block with the locator path**

In `crates/agent/src/entry.rs`, replace the current `=== PULL TEST … ===` block (the locate-once/watch loop and its `return 0;`) with:

```rust
    log("=== ROOT DUMP: locate metadata root, dump complete tree ===");
    {
        use agent_core::format::format_dump;
        use agent_core::logfile::write_text;
        // Retry while the runtime finishes first-pass class init.
        let mut dump = None;
        for _ in 0..30 {
            if let Some(d) = crate::locator::dump_all_internals() {
                if !d.classes.is_empty() { dump = Some(d); break; }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        match dump {
            None => log("  FAILED to reach metadata root (iterate Locator / verify v24 layout)"),
            Some(d) => {
                log(&format!(
                    "  dumped {} types, {} fields, {} methods",
                    d.class_count(), d.total_fields(), d.total_methods()
                ));
                let text = format_dump(&d);
                match write_text(&dump_path(), &text) {
                    Ok(()) => log("  wrote internals.txt"),
                    Err(e) => log(&format!("  failed to write internals.txt: {}", e)),
                }
            }
        }
    }
    log("=== end ROOT DUMP ===");
    return 0;
```

(`dump_path()`, `log()`, `write_text`, `Duration` are already imported/defined in `entry.rs`.)

- [ ] **Step 2: Build + stage**

```bash
cargo build -p agent --target x86_64-pc-windows-gnu --release 2>&1 | tail -3
GD="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds"
cp target/x86_64-pc-windows-gnu/release/agent.dll "$GD/agent.dll"
rm -f "$GD/agent.log" "$GD/internals.txt"
```
Expected: compiles; dll staged.

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/entry.rs
git commit -m "feat(agent): wire worker to dump complete tree to internals.txt"
```

---

## Stage C — Integration gate (manual, on PW)

### Task C1: Prove the root on Pixel Worlds

This is the make-or-break. No host test can validate it; it runs against the live game.

- [ ] **Step 1: Run PW and capture the log**

Launch Pixel Worlds via Steam/Proton with the staged `agent.dll`. Reach the menu, wait ~20s, quit. Read `"$GD/agent.log"`.

- [ ] **Step 2: Check the gate**

Expected in `agent.log`:
- `dumped N types, …` with **N in the thousands** (≈6288 — the full typedef count, NOT ~3147), confirming we read the static table, not just loaded classes.
- `wrote internals.txt`.

Then inspect `"$GD/internals.txt"`: it should contain real, correctly-namespaced types **including ones that were null/unloaded at menu** (e.g. block/item types you hadn't triggered).

- [ ] **Step 3: Cross-check correctness**

Spot-check a handful of names against `ref/pw_reference_pack/dump/dump_v245.cs`. Names + namespaces must match. If types are present but names are garbage → a `LAYOUT_V24` offset is wrong (Task A5); fix and rebuild. If `FAILED to reach metadata root` → iterate the Locator sub-phases:
- **1a** confirm `string_heap_bracket` returns a sane range (log `str_lo/str_hi`).
- **1b** widen `BACK`/`SPAN` if the header is outside the captured window.
- **1c** loosen/inspect `header_anchor_ok` (log candidate offsets that pass the bracket but fail sanity).

- [ ] **Step 4: Confirm crash-safety**

The game must survive the run (launches, playable, clean quit). If it crashes, the capture window likely crossed a faulting region — reduce `SPAN` / ensure `copy_region_bytes` stops at the region boundary (it does) and that the window starts inside a committed region.

- [ ] **Step 5: Record the result**

When `internals.txt` holds the full tree with correct names, Phase 1 is proven. (You commit your own work — this step is a checkpoint, not an auto-commit.)

---

## Self-Review

**Spec coverage:**
- "Complete dump … every type … namespace, fields, methods → internals.txt" → Tasks A1/A2 (model+format), A3/A4 (locate+walk+methods), B2/B3 (dump), C1 (verify). Field *type* names + method *signatures* explicitly deferred per spec non-goals/Phase 3.
- "Root-first / string-heap anchor" → A3 (`locate_header_by_string_anchor`), B1/B2 (bracket + capture).
- "Observation-only, crash-safe, bounded" → `copy_region_bytes` stops at region boundary; envelope capped at 64 regions; read-only throughout; C1 Step 4 verifies survival.
- "Adaptive, no hardcoding" → header found by data anchor + cross-check; layout selected by version (24), documented format, not per-build constants.
- "Self-aware completeness (X of N)" → dump count logged in B3; full-table read in A4.
- Live-watch (component #5), cleanup, optimize, API → out of scope this plan (Phases 2–5), stated up front.

**Placeholder scan:** No TBD/TODO. The one flagged uncertainty (`LAYOUT_V24` exact offsets) is an explicit transcription+verification task (A5) with the runtime gate (C1) as validator — not a placeholder, a known spike with a verification path.

**Type consistency:** `DumpedClass { namespace, name, fields, methods }` used consistently (A1 defines, A2/A4 populate). `MetaHeader` gains `string_size`/`methods_offset` (A3) used by `header_anchor_ok`/`parse_tree` (A3/A4). `RegionMap::{class_name_ptr_bracket, copy_region_bytes, read_u64, class_fields, regions}` consistent across B1/B2. `parse_metadata_anchored(bytes, region_base, str_lo, str_hi, layout)` signature identical in A4 (def) and B2 (call).
