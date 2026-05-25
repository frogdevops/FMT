# Architecture Redesign — Generic, Dynamic, Robust

## Goal
Make the tool work for ANY il2cpp game (Unity 5.3–2023+), protected or not,
by making `global-metadata.dat` the **primary source** and runtime enrichment a
non-critical **fallback**.

---

## Why This Works

Every il2cpp game ships a `global-metadata.dat` file with the full type tree.
Its format is well-documented per version number and cannot be encrypted without
breaking the il2cpp toolchain — so even heavily protected games have a decrypted
copy in memory (or an obfuscation layer we can peel).

The metadata gives us **everything** without touching runtime memory:
- All class/struct/enum definitions (not just loaded ones)
- Field names + type indices
- Method names + signatures
- Parent class, interfaces, attributes

---

## New Architecture

```
┌─────────────────────────────────────────────────────┐
│                   agent-core                         │
│        (pure Rust, no platform deps)                 │
│                                                      │
│  metadata.rs ──► layout_for_version(N) ──► Dump      │
│     ↑                        ↑                       │
│  global-metadata.dat    Version layouts:             │
│                          v24, v27, v29, v30...       │
│                                                      │
│  runtime.rs ── Il2CppRuntime trait (enrichment API)  │
│  format.rs  ── output formatting                     │
│  model.rs   ── Dump, DumpedClass, DumpedField        │
└──────────────────────┬──────────────────────────────┘
                       │
           ┌───────────┴───────────┐
           │                       │
           ▼                       ▼
┌──────────────────┐   ┌──────────────────────┐
│  agent (Windows) │   │  tests / CLI tool    │
│                  │   │                      │
│ entry.rs         │   │ Parse metadata       │
│ mem_scan.rs      │   │ and dump to stdout   │
│ il2cpp_ffi.rs    │   │ — no game needed     │
│                  │   │                      │
│ Runtime          │   │ Hosted on macOS/     │
│ enrichment:      │   │ Linux for dev        │
│ - klass pointers │   │                      │
│ - instance scan  │   └──────────────────────┘
│ - method hooks   │
└──────────────────┘
```

## New agent-core Responsibilities

### 1. Version-Adaptive Layouts (`metadata.rs`)
Transcribe real il2cpp metadata struct layouts from the Il2CppDumper project /
Unity source. Each version gets a `MetadataLayout`:

| Version | Unity Versions | Status |
|---------|----------------|--------|
| 24 | 2018.x | ❌ need |
| 27 | 2019.x–2020.x | ❌ need |
| 29 | 2021.x | ❌ need |
| 30 | 2022.x | ❌ need |
| 31+ | 2023.x / 6.x | ❌ need |

Detection: read u32 at offset 4 in the metadata file. Auto-select layout.

### 2. Field Type Resolution (`metadata.rs`)
The metadata has a **type index** per field — we currently skip it:
```rust
fields.push(DumpedField {
    name: read_name(fname_idx),
    type_name: String::new(), // ← needs resolution
});
```

The metadata has:
- Type indices → point into a type table
- Type table entries → CLASS | VALUETYPE | ARRAY | GENERICINST | etc.
- String indices for names → fully resolve to `System.Int32`, etc.

This means we can resolve field type names **without any runtime access**.

### 3. Method Enumeration (`metadata.rs`)
Same pattern — metadata has method definitions per type with name indices.

### 4. Enriched Model (`model.rs`)
Extend `DumpedClass` and `DumpedField` to carry both metadata and runtime info:
```rust
pub struct DumpedField {
    pub name: String,
    pub type_name: String,       // from metadata
    pub runtime_type_name: Option<String>,  // from runtime reverse map
    pub offset: Option<u32>,     // from runtime class_get_field_offset
}
```

---

## New agent Responsibilities (Simplified)

### Entry Point (`entry.rs`)
1. Read `global-metadata.dat` from disk (next to the game exe)
2. Pass raw bytes to `agent-core::metadata::find_and_parse()` → get `Dump`
3. Enrich with runtime data if available (klass pointers → field offsets)
4. Format and write output

### Runtime Enrichment (Optional, Non-Critical)
- Class table scanning (existing) → enriches field type names with resolved strings
- Field offset queries (new) → add offset info for executor use
- Instance scanning (future) → find live objects

---

## Execution Plan

### Phase 1: Metadata parser — version layouts + field types (3-5 days)
1. Transcribe layout v24 from Il2CppDumper
2. Implement field type resolution from type indices
3. Add method enumeration
4. Write tests with real metadata blobs
5. Repeat for v27, v29, v30, v31

### Phase 2: agent restructure (1 day)
1. Remove dead files (real_runtime.rs, win.rs)
2. Make agent use agent-core metadata parser as primary source
3. Runtime enrichment becomes optional extension

### Phase 3: Verification (2-3 days)
1. Test on Pixel Worlds (v24? v27?) — ensure same output quality
2. Test on 1-2 other il2cpp games
3. Test on stripped/obfuscated metadata (if available)

---

## What We Keep From Current Work

The reverse type map (klass+0x20 → name) was a hard-won discovery about
how il2cpp represents types at runtime. This knowledge informs the metadata
type resolution logic — the type index in metadata maps to the same info
we reverse-engineered from memory.

**Tag `ffi-class-table-working`** at commit `3203226` preserves our working
Pixel Worlds dumper unchanged. No progress lost.

---

## Risks

- **Metadata version unknown**: If a game uses a version we don't have a layout
  for, we fall back to the runtime-only approach (still works for unprotected games)
- **Obfuscated metadata**: Some protectors encrypt/hide `global-metadata.dat`.
  We'd need a decryptor per protector (later phase)
- **Time**: Transcribing layouts is mechanical but tedious. ~1 day per version.
