# Metadata-Parser Backend — Observing Obfuscated Il2Cpp at Runtime

**Date:** 2026-05-23
**Status:** Approved approach, pre-implementation
**Author:** Rust-Frog (with Claude)
**Builds on:** `2026-05-22-unity-inspector-design.md`

## 1. Problem

The current agent reads internals by resolving the il2cpp C API (`il2cpp_*`) and **calling** those functions. This works on normal games (`resolve_std` path), but **obfuscated games rename their exports** (e.g. Pixel Worlds: 502 garbage-named exports, only `il2cpp_baselib` recognizable). Our signature-scanner correctly resolved the *unique* functions (domain/assemblies/image) but the *accessor* functions (`image_get_class`, `class_get_name`) have non-unique code (`mov rax,[rcx+off]; ret`), so it grabbed the wrong ones — and **calling a mis-resolved function crashes the game (access violation), which pointer-validation cannot prevent** (the crash is inside the call, before any return value).

We are **runtime observers**, not static deobfuscators. So we need a general, read-only, crash-proof way to read internals that does not depend on resolving or calling obfuscated functions.

## 2. The expert method (what this design adopts)

From RE practice (Il2CppDumper, Il2CppInspector, frida-il2cpp-bridge, katyscode tutorials): when exports/metadata are obfuscated, experts **do not resolve the scrambled functions**. They **find the decrypted `global-metadata` blob in memory and parse it directly.** The game must decrypt `global-metadata.dat` into memory at startup to run; once decrypted it is a documented binary format containing every type/field/method name and the type hierarchy.

The canonical "almost universal (when no anti-tamper)" runtime move: **scan process memory for the metadata magic and parse the format.**

**We are perfectly positioned for this** — we run as an injected DLL *inside* the process, so the decrypted blob is already in our own address space.

## 3. Approach

**Scan our process memory for the `global-metadata` header magic, validate it, then parse the format to build the same `Dump` model the rest of the tool already consumes.** No il2cpp function is ever called. Every read is validated with the existing `readable_len`/`mem_readable` guards → read-only and crash-proof by construction.

This becomes a **third resolution path**, and the *primary* one for obfuscated games:
1. `resolve_std()` — normal games (exports present). Already works.
2. **Metadata parser (this design)** — general; works whether or not exports are obfuscated.
3. ~~Scrambled-export signature scanner~~ — retired (crash-prone, build-specific, doesn't generalize).

The existing `Dump`/`format_dump`/`internals.txt` and the TCP server stay unchanged — only the *source* of the data changes (parse the metadata blob instead of calling il2cpp functions).

## 4. The `global-metadata.dat` format (from Il2CppDumper)

`Il2CppGlobalMetadataHeader` (little-endian):
- `uint sanity` = `0xFAB11BAF` → in memory the first 4 bytes are **`AF 1B B1 FA`** (our scan target)
- `int version` (e.g. 24, 27, 29, 31) — **drives struct sizes/offsets; the parser is version-aware**
- Table offset+size pairs we need:
  - `stringOffset` / `stringSize` — metadata strings: a blob of NUL-terminated strings; **name indices are byte offsets into this blob**
  - `typeDefinitionsOffset` / `typeDefinitionsSize` — array of `Il2CppTypeDefinition`
  - `fieldsOffset` / `fieldsSize` — array of `Il2CppFieldDefinition`
  - `imagesOffset` / `imagesSize` — array of `Il2CppImageDefinition`
  - `assembliesOffset` / `assembliesSize`

`Il2CppImageDefinition`: `uint nameIndex`, `int assemblyIndex`, `int typeStart`, `uint typeCount`, `int entryPointIndex`

`Il2CppTypeDefinition`: `uint nameIndex`, `uint namespaceIndex`, `int fieldStart`, `ushort field_count`, `int methodStart`, … (plus flags/token; struct **size varies by version**)

`Il2CppFieldDefinition`: `uint nameIndex`, `int typeIndex`, `uint token` (v19+)

> Field *type names* require resolving `typeIndex` → `Il2CppType` → name, and the `Il2CppType` table lives in the **binary's** `Il2CppMetadataRegistration`, not in `global-metadata`. So full type names are **phase 2** (needs registration discovery). v1 gets class/namespace/field **names** + hierarchy from the metadata blob alone.

## 5. Algorithm

1. **Locate the blob:** walk readable committed memory regions (via `VirtualQuery`); at each candidate, check for sanity `0xFAB11BAF`. Validate the match: plausible `version`, table offsets within a sane size, string table starts cleanly. (Handle multiple matches — pick the one whose tables validate.)
2. **Read the header** (version-aware field layout).
3. **Walk images:** for each `Il2CppImageDefinition` → `name = string[nameIndex]`, plus `typeStart`, `typeCount`.
4. **Walk that image's types:** for `ti in typeStart .. typeStart+typeCount` → `Il2CppTypeDefinition`; `name = string[nameIndex]`, `namespace = string[namespaceIndex]`, plus `fieldStart`, `field_count`.
5. **Walk fields:** for `fi in fieldStart .. fieldStart+field_count` → `Il2CppFieldDefinition`; `field name = string[nameIndex]`. (type name = phase 2.)
6. **Build `DumpedClass { namespace, name, fields: [DumpedField { name, type_name }] }`** → `Dump` → `format_dump` → `internals.txt`, and serve via the existing TCP protocol.

All reads bounds-checked against the validated table sizes and `mem_readable`.

## 6. Crash-safety & ethics

- **No game functions are called** → the access-violation-on-call failure mode is gone entirely.
- **Read-only**, every access validated → cannot crash the host.
- **Respect gate extends naturally:** if the magic can't be found (metadata kept encrypted/hidden in memory by aggressive anti-tamper), we **decline gracefully** — we never attempt to defeat in-memory protection. We observe what the running game has already exposed to itself; nothing more.

## 7. Scope

**v1 (this design):**
- Memory scan for `0xFAB11BAF`; version-aware header parse.
- Walk images → types → fields; emit class/namespace/field **names** + hierarchy into the existing `Dump`.
- Version coverage: target the version(s) of our actual test games first; structure the parser so adding a version = adding an offset/size table (reference Il2CppDumper's per-version definitions).

**Later:**
- Field/return **type names** (resolve `Il2CppType` via `Il2CppMetadataRegistration` discovery).
- Live **values** (registration structs + live object reads).
- Method/event tables; method tracing.

**Non-goals:** defeating in-memory encryption/anti-tamper; field-reorder-obfuscation of the header itself (Il2CppInspector-level); online games.

## 8. References to study
- **Il2CppDumper** (Perfare) — authoritative `global-metadata.dat` format + per-version struct layouts. The parser is modeled on this.
- **frida-il2cpp-bridge** (`/lib` source) — runtime reference for the registration-struct/value side (phase 2).
- **katyscode** "Finding loaders for obfuscated global-metadata" — the entry-point/`LoadMetadataFile` techniques (fallback if a future game keeps metadata encrypted until use).

## 9. Open questions for the plan
1. Confirm the il2cpp **version** of our test target(s) to pick initial struct layouts.
2. Where to scan: whole address space vs. just `GameAssembly.dll`/heap regions (perf vs. coverage).
3. Multiple magic matches → validation heuristic to pick the real metadata.
4. How to share the version-keyed offset tables cleanly (a small data-driven table in `agent-core`, host-testable).
5. Keep `resolve_std` as the fast path for clean games, or always prefer the metadata parser for consistency?
