# Il2Cpp Metadata Parser (agent-core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure, host-testable parser that turns a decrypted `global-metadata` byte blob into a `Dump` (class/namespace/field names + hierarchy) — the foundation for observing obfuscated Il2Cpp games. No game, injection, or Proton needed; verified entirely with synthetic fixtures.

**Architecture:** All logic lives in `agent-core` as pure functions over `&[u8]`. A data-driven `MetadataLayout` describes one il2cpp version's byte layout, so supporting a new version = adding one layout value (not new code). The parser walks `images → type definitions → field definitions`, resolving names via the string table, and emits the existing `Dump` model. A separate magic-finder locates candidate metadata blobs in a byte slice. The runtime piece that *produces* those bytes (scanning live process memory) is a **follow-up plan** in the `agent` crate.

**Tech Stack:** Rust (edition 2021), `agent-core` (no new deps). Reference for real version layouts: Il2CppDumper's `Il2CppDumper/Il2Cpp/MetadataClass.cs`.

**Scope:** v1 = class/namespace/field **names** + hierarchy. Field **type names** and live **values** are explicitly out of scope (phase 2 — needs the binary's registration structs). This plan is the parser only; the live-memory scan + agent wiring is the next plan.

---

### Task 1: Metadata module scaffold + layout + byte readers

**Files:**
- Create: `crates/agent-core/src/metadata.rs`
- Modify: `crates/agent-core/src/lib.rs` (add `pub mod metadata;`)
- Test: `#[cfg(test)] mod tests` inside `metadata.rs`

- [ ] **Step 1: Declare the module**

Add to `crates/agent-core/src/lib.rs` (after the existing `pub mod` lines):
```rust
pub mod metadata;
```

- [ ] **Step 2: Write the layout type, magic, and byte readers**

Create `crates/agent-core/src/metadata.rs`:
```rust
//! Pure parser for the il2cpp `global-metadata` format.
//! Operates on a decrypted byte blob; no FFI, fully host-testable.

/// `Il2CppGlobalMetadataHeader.sanity`. In a little-endian blob the first four
/// bytes are `AF 1B B1 FA`.
pub const METADATA_MAGIC: u32 = 0xFAB1_1BAF;

/// Byte layout for one il2cpp metadata version. `h_*` are byte positions of a
/// field within the header; the `*_index`/`*_start`/`*_count` are byte offsets
/// within their definition struct; `*_size` are struct sizes in bytes.
#[derive(Debug, Clone, Copy)]
pub struct MetadataLayout {
    pub h_string_offset: usize,
    pub h_string_size: usize,
    pub h_type_defs_offset: usize,
    pub h_type_defs_size: usize,
    pub h_fields_offset: usize,
    pub h_fields_size: usize,
    pub h_images_offset: usize,
    pub h_images_size: usize,

    pub image_size: usize,
    pub image_name_index: usize, // u32
    pub image_type_start: usize, // i32
    pub image_type_count: usize, // u32

    pub type_size: usize,
    pub type_name_index: usize,      // u32
    pub type_namespace_index: usize, // u32
    pub type_field_start: usize,     // i32
    pub type_field_count: usize,     // u16

    pub field_size: usize,
    pub field_name_index: usize, // u32
}

pub(crate) fn read_u32(bytes: &[u8], pos: usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    let s = bytes.get(pos..end)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn read_i32(bytes: &[u8], pos: usize) -> Option<i32> {
    read_u32(bytes, pos).map(|v| v as i32)
}

pub(crate) fn read_u16(bytes: &[u8], pos: usize) -> Option<u16> {
    let end = pos.checked_add(2)?;
    let s = bytes.get(pos..end)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Read a NUL-terminated string starting at `pos`. Bounded by the slice; never panics.
pub(crate) fn read_cstr(bytes: &[u8], pos: usize) -> String {
    if pos >= bytes.len() {
        return String::new();
    }
    let mut end = pos;
    while end < bytes.len() && bytes[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&bytes[pos..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_ints() {
        let bytes = [0xAF, 0x1B, 0xB1, 0xFA, 0x05, 0x00];
        assert_eq!(read_u32(&bytes, 0), Some(0xFAB1_1BAF));
        assert_eq!(read_u16(&bytes, 4), Some(5));
        assert_eq!(read_u32(&bytes, 4), None); // out of bounds
    }

    #[test]
    fn reads_nul_terminated_string() {
        let bytes = b"Game\0Player\0";
        assert_eq!(read_cstr(bytes, 0), "Game");
        assert_eq!(read_cstr(bytes, 5), "Player");
        assert_eq!(read_cstr(bytes, 100), ""); // out of bounds is empty, not a panic
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p agent-core metadata`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit** — STOP and let the human commit (do not run `git commit` yourself).

---

### Task 2: Header parser

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`
- Test: in the same `#[cfg(test)]` module

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:
```rust
    // Builds a 40-byte header for the TEST_LAYOUT used across these tests.
    fn test_header() -> Vec<u8> {
        let mut h = vec![0u8; 40];
        h[0..4].copy_from_slice(&METADATA_MAGIC.to_le_bytes());
        h[4..8].copy_from_slice(&29u32.to_le_bytes()); // version
        let put = |h: &mut [u8], pos: usize, v: u32| h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut h, 8, 40);  // string_offset
        put(&mut h, 12, 24); // string_size
        put(&mut h, 16, 80); // type_defs_offset
        put(&mut h, 20, 16); // type_defs_size
        put(&mut h, 24, 96); // fields_offset
        put(&mut h, 28, 16); // fields_size
        put(&mut h, 32, 64); // images_offset
        put(&mut h, 36, 16); // images_size
        h
    }

    #[test]
    fn parses_header_and_rejects_bad_magic() {
        let h = test_header();
        let parsed = parse_header(&h, &TEST_LAYOUT).unwrap();
        assert_eq!(parsed.string_offset, 40);
        assert_eq!(parsed.images_offset, 64);
        assert_eq!(parsed.images_size, 16);

        let mut bad = h.clone();
        bad[0] = 0; // corrupt magic
        assert!(parse_header(&bad, &TEST_LAYOUT).is_none());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p agent-core parses_header`
Expected: FAIL — `cannot find ... parse_header` / `MetaHeader` / `TEST_LAYOUT`.

- [ ] **Step 3: Implement the header parser + the test layout constant**

Add to `metadata.rs` (above the `#[cfg(test)]` module):
```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct MetaHeader {
    pub string_offset: u32,
    pub type_defs_offset: u32,
    pub fields_offset: u32,
    pub images_offset: u32,
    pub images_size: u32,
}

pub(crate) fn parse_header(bytes: &[u8], layout: &MetadataLayout) -> Option<MetaHeader> {
    if read_u32(bytes, 0)? != METADATA_MAGIC {
        return None;
    }
    Some(MetaHeader {
        string_offset: read_u32(bytes, layout.h_string_offset)?,
        type_defs_offset: read_u32(bytes, layout.h_type_defs_offset)?,
        fields_offset: read_u32(bytes, layout.h_fields_offset)?,
        images_offset: read_u32(bytes, layout.h_images_offset)?,
        images_size: read_u32(bytes, layout.h_images_size)?,
    })
}
```

Add a self-consistent layout used by the tests (place it just above `mod tests`, marked `#[cfg(test)]`):
```rust
#[cfg(test)]
pub(crate) const TEST_LAYOUT: MetadataLayout = MetadataLayout {
    h_string_offset: 8,
    h_string_size: 12,
    h_type_defs_offset: 16,
    h_type_defs_size: 20,
    h_fields_offset: 24,
    h_fields_size: 28,
    h_images_offset: 32,
    h_images_size: 36,
    image_size: 16,
    image_name_index: 0,
    image_type_start: 4,
    image_type_count: 8,
    type_size: 16,
    type_name_index: 0,
    type_namespace_index: 4,
    type_field_start: 8,
    type_field_count: 12,
    field_size: 8,
    field_name_index: 0,
};
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p agent-core metadata`
Expected: PASS.

- [ ] **Step 5: Commit** — STOP for the human to commit.

---

### Task 3: Full parse — images → types → fields → `Dump`

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`
- Test: same test module

- [ ] **Step 1: Write the failing test (synthetic full blob)**

Add inside `mod tests`:
```rust
    // Full blob: header + string table + 1 image + 1 type ("Game.Player") + 2 fields.
    fn test_blob() -> Vec<u8> {
        let mut b = test_header(); // 40 bytes
        // string table @40, size 24: "Game\0Player\0health\0mana\0"
        // indices:                     0     5       12       19
        b.extend_from_slice(b"Game\0Player\0health\0mana\0");
        assert_eq!(b.len(), 64);
        // images @64, size 16: name_index=0, type_start=0, type_count=1
        let mut img = vec![0u8; 16];
        img[0..4].copy_from_slice(&0u32.to_le_bytes()); // name_index
        img[4..8].copy_from_slice(&0i32.to_le_bytes()); // type_start
        img[8..12].copy_from_slice(&1u32.to_le_bytes()); // type_count
        b.extend_from_slice(&img);
        assert_eq!(b.len(), 80);
        // types @80, size 16: name=5("Player"), namespace=0("Game"), field_start=0, field_count=2
        let mut ty = vec![0u8; 16];
        ty[0..4].copy_from_slice(&5u32.to_le_bytes());  // name_index
        ty[4..8].copy_from_slice(&0u32.to_le_bytes());  // namespace_index
        ty[8..12].copy_from_slice(&0i32.to_le_bytes()); // field_start
        ty[12..14].copy_from_slice(&2u16.to_le_bytes()); // field_count
        b.extend_from_slice(&ty);
        assert_eq!(b.len(), 96);
        // fields @96, 2 x size 8: name=12("health"), name=19("mana")
        let mut f0 = vec![0u8; 8];
        f0[0..4].copy_from_slice(&12u32.to_le_bytes());
        let mut f1 = vec![0u8; 8];
        f1[0..4].copy_from_slice(&19u32.to_le_bytes());
        b.extend_from_slice(&f0);
        b.extend_from_slice(&f1);
        b
    }

    #[test]
    fn parses_full_blob_to_dump() {
        use crate::model::{DumpedClass, DumpedField};
        let dump = parse_metadata(&test_blob(), &TEST_LAYOUT).unwrap();
        assert_eq!(
            dump.classes,
            vec![DumpedClass {
                namespace: "Game".to_string(),
                name: "Player".to_string(),
                fields: vec![
                    DumpedField { name: "health".to_string(), type_name: String::new() },
                    DumpedField { name: "mana".to_string(), type_name: String::new() },
                ],
            }]
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p agent-core parses_full_blob`
Expected: FAIL — `cannot find function 'parse_metadata'`.

- [ ] **Step 3: Implement `parse_metadata`**

Add to `metadata.rs` (above the test module):
```rust
use crate::model::{Dump, DumpedClass, DumpedField};

/// Parse a decrypted global-metadata blob into a `Dump` (class/namespace/field
/// names + hierarchy). Returns `None` if the magic or a required read fails.
/// Out-of-range table indices are skipped, never panicked on.
pub fn parse_metadata(bytes: &[u8], layout: &MetadataLayout) -> Option<Dump> {
    let h = parse_header(bytes, layout)?;
    let string_base = h.string_offset as usize;
    let read_name = |idx: u32| read_cstr(bytes, string_base.wrapping_add(idx as usize));

    let num_images = (h.images_size as usize) / layout.image_size.max(1);
    let mut classes = Vec::new();

    for i in 0..num_images {
        let img = h.images_offset as usize + i * layout.image_size;
        let type_start = match read_i32(bytes, img + layout.image_type_start) {
            Some(v) if v >= 0 => v as usize,
            _ => continue,
        };
        let type_count = read_u32(bytes, img + layout.image_type_count).unwrap_or(0) as usize;

        for t in 0..type_count {
            let tdef = h.type_defs_offset as usize + (type_start + t) * layout.type_size;
            let name_idx = match read_u32(bytes, tdef + layout.type_name_index) {
                Some(v) => v,
                None => continue,
            };
            let ns_idx = read_u32(bytes, tdef + layout.type_namespace_index).unwrap_or(0);
            let field_start = read_i32(bytes, tdef + layout.type_field_start).unwrap_or(-1);
            let field_count = read_u16(bytes, tdef + layout.type_field_count).unwrap_or(0) as usize;

            let mut fields = Vec::new();
            if field_start >= 0 {
                let fs = field_start as usize;
                for f in 0..field_count {
                    let fdef = h.fields_offset as usize + (fs + f) * layout.field_size;
                    if let Some(fname_idx) = read_u32(bytes, fdef + layout.field_name_index) {
                        fields.push(DumpedField {
                            name: read_name(fname_idx),
                            type_name: String::new(), // phase 2
                        });
                    }
                }
            }

            classes.push(DumpedClass {
                namespace: read_name(ns_idx),
                name: read_name(name_idx),
                fields,
            });
        }
    }

    Some(Dump { classes })
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p agent-core`
Expected: PASS (all metadata tests + the prior 8 agent-core tests).

- [ ] **Step 5: Commit** — STOP for the human to commit.

---

### Task 4: Magic-candidate finder

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`
- Test: same test module

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:
```rust
    #[test]
    fn finds_magic_offsets() {
        let mut data = vec![0u8; 10];
        data.extend_from_slice(&METADATA_MAGIC.to_le_bytes()); // magic at offset 10
        data.extend_from_slice(&[1, 2, 3]);
        data.extend_from_slice(&METADATA_MAGIC.to_le_bytes()); // magic at offset 17
        assert_eq!(find_magic_offsets(&data), vec![10, 17]);
        assert_eq!(find_magic_offsets(&[0u8; 4]), Vec::<usize>::new());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p agent-core finds_magic`
Expected: FAIL — `cannot find function 'find_magic_offsets'`.

- [ ] **Step 3: Implement it**

Add to `metadata.rs` (above the test module):
```rust
/// Byte positions in `bytes` where the metadata magic appears (little-endian).
/// The runtime memory-scan (next plan) feeds region bytes here, then validates
/// each candidate by attempting `parse_header`.
pub fn find_magic_offsets(bytes: &[u8]) -> Vec<usize> {
    let magic = METADATA_MAGIC.to_le_bytes();
    if bytes.len() < 4 {
        return Vec::new();
    }
    (0..=bytes.len() - 4)
        .filter(|&i| bytes[i..i + 4] == magic)
        .collect()
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p agent-core`
Expected: PASS.

- [ ] **Step 5: Commit** — STOP for the human to commit.

---

### Task 5: Provide a real version layout (from Il2CppDumper)

> This is the one task that needs an external reference rather than synthetic data. The parser logic is already proven by Tasks 1–4; this supplies the production layout values for a real game.

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`

- [ ] **Step 1: Determine the target version**

The il2cpp metadata version is the `int` at byte offset 4 of a *decrypted* `global-metadata.dat` (or read it from a clean game's on-disk file: `xxd -l 8 global-metadata.dat` → bytes 4–7, little-endian). Record the version (e.g. 24, 27, 29, 31).

- [ ] **Step 2: Transcribe the layout from Il2CppDumper**

From `Il2CppDumper/Il2Cpp/MetadataClass.cs` (https://github.com/Perfare/Il2CppDumper), for the target version, read the field order of `Il2CppGlobalMetadataHeader`, `Il2CppImageDefinition`, `Il2CppTypeDefinition`, `Il2CppFieldDefinition`. Compute byte positions (every header field is 4 bytes; struct fields per their types) and fill a `pub const` (NOT cfg-gated) layout. Example shape (values must come from the reference for the chosen version):
```rust
/// Layout for il2cpp metadata version 29 (confirm against Il2CppDumper).
pub const LAYOUT_V29: MetadataLayout = MetadataLayout {
    h_string_offset: /* byte pos of stringOffset in header */ 0,
    h_string_size: 0,
    h_type_defs_offset: 0,
    h_type_defs_size: 0,
    h_fields_offset: 0,
    h_fields_size: 0,
    h_images_offset: 0,
    h_images_size: 0,
    image_size: 0,
    image_name_index: 0,
    image_type_start: 0,
    image_type_count: 0,
    type_size: 0,
    type_name_index: 0,
    type_namespace_index: 0,
    type_field_start: 0,
    type_field_count: 0,
    field_size: 0,
    field_name_index: 0,
};
```
(The zeros above are the *only* placeholders in this plan, and intentionally so — they are version-specific data to be filled from the named reference, not logic to invent.)

Then expose layouts via a **runtime version detector**, so the *same built DLL* adapts to any game without recompiling:
```rust
/// Pick the layout for a metadata blob, reading its version (the u32 at byte 4).
pub fn layout_for_version(version: u32) -> Option<MetadataLayout> {
    match version {
        29 => Some(LAYOUT_V29),
        // add 24, 27, 31, 35, 38, 39 as each layout is transcribed
        _ => None,
    }
}
```
The runtime scanner reads `read_u32(blob, 4)` and calls `layout_for_version` — no hardcoded version, auto-adapts across games. **Caveat:** metadata versions 24.x and 27.x have sub-variants that share the same `int` but differ in struct layout; those need extra struct-size probing (as Il2CppDumper does) and should be handled when first targeted. Modern versions (29/31/35+) are cleaner.

- [ ] **Step 3: Validate against a real blob**

Once filled, validate offline against a clean game's on-disk `global-metadata.dat` (if unencrypted): write a throwaway `#[test]` that loads the file (via `include_bytes!` or `std::fs`) and asserts `parse_metadata(&bytes, &LAYOUT_V29)` returns a plausible class count and that a known class name appears. Adjust offsets until correct. (This test can be `#[ignore]`d in CI since it needs the file.)

- [ ] **Step 4: Commit** — STOP for the human to commit.

---

## Self-Review

- **Spec coverage:** parse header (§4) ✓, walk images→types→fields → Dump (§5) ✓, magic finder for the runtime scan (§5) ✓, version-keyed layout table (§hardcoding concern / open Q4) ✓, names-only/type-names-phase-2 scope (§7) ✓. The runtime memory-scan (§5 step 1) is intentionally deferred to the follow-up plan, as the spec's phasing allows.
- **Placeholders:** only the version-specific zeros in Task 5 Step 2, explicitly flagged as reference-sourced data (not logic). All logic steps have complete code.
- **Type consistency:** `MetadataLayout`, `MetaHeader`, `parse_header`, `parse_metadata`, `find_magic_offsets`, `read_u32/i32/u16/cstr` are used consistently; `parse_metadata` emits `Dump`/`DumpedClass`/`DumpedField` from `model.rs` exactly as `build_dump` does, so `format_dump` and the TCP server consume it unchanged.
- **Note:** Task 2 Step 3 includes an inline correction to the Step-1 assertions so they match `MetaHeader`'s actual fields.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-23-metadata-parser-plan.md`. Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task (on Opus), I re-verify each result, fast iteration. No commits (human commits at each STOP).
2. **Inline Execution** — I work the tasks here in batches with checkpoints.

After this plan: a **follow-up plan** for the `agent` crate adds the live memory-scan (`VirtualQuery` walk → `find_magic_offsets` → validate via `parse_header` → `parse_metadata`) and wires the result into the agent's resolution path as the obfuscated-game backend.
