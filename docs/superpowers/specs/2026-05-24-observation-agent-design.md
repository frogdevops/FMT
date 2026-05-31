# Observation Agent — Design (Sub-project A)

- **Date:** 2026-05-24
- **Status:** Approved design, pending spec review
- **Scope:** The Rust observation agent only. The agent↔plugin protocol (B) and the RustRover plugin (C) are separate, later specs.

## Context

`Frog` is a personal, educational live Unity-internals inspector: a native Rust DLL injected into a running il2cpp game that reads the game's internals by **pure observation** and (eventually) streams them to a JetBrains/RustRover plugin.

We have already proven, on a fully-hardened target (Pixel Worlds — encrypted on-disk metadata, obfuscated exports, stripped magic/path markers), that we can read live `Il2CppClass` structs out of memory with **zero decryption and zero calls into obfuscated code**: class name (`+0x10`), namespace (`+0x18`), image back-pointer (`+0x00`), and the images/assemblies graph. Offsets were derived from the game's own getter bytecode, not hardcoded.

The current code is diagnostic scaffolding accreted over many iterations. This design defines the clean target the agent should be rebuilt toward.

## The problem this design solves

The proven approach reads `g_typeTable` (a.k.a. `s_TypeInfoDefinitionTable`) — the runtime array of `Il2CppClass*` indexed by type-definition index. But `g_typeTable` is **lazily populated**: only types the game has already instantiated are non-null (~3147 of 6288 on PW at inject). Reading it gives a partial, growing subset — "picking leaves from the canopy."

The goal is **everything**: all defined types (and their fields and methods), loaded or not. That requires reaching the **root**.

## Guiding principle: root-first

A tree's branches and twigs all connect down to one root. For il2cpp:

```
ROOT     = the global metadata (decrypted, resident in RAM)
TRUNK    = the metadata header (an offset/size table linking root -> every branch)
BRANCHES = typeDefinition[] / fieldDefinition[] / methodDefinition[] tables
TWIGS    = individual types / fields / methods (ALL of them, loaded or not)
LEAVES   = the identifier name strings (the string heap)
```

Reaching the root yields the complete tree in one consistent traversal. Crucially, this is **observation, not decryption**: the metadata is already decrypted and resident (the runtime needs it to lazy-create classes), and we are *already reading from its string heap* (every loaded class name points into it). PW only stripped marker bytes and obfuscated exports — the content is plaintext-resident.

## Goals

- Produce a **complete** dump of the game's type tree — every type (all ~6288 on PW), each with namespace, fields (name + type), and methods (name) — written to `internals.txt` as the confirmation artifact.
- Preserve the **live-events** capability: surface types as they become instantiated while the user interacts with the game (a genuinely useful observability feature discovered along the way).
- Remain strictly **observation-only and crash-safe**: read-only, every dereference validated against committed memory regions, bounded scans (full-process scans fault under Wine), never call into the game, never write game memory, never decrypt.
- Stay **adaptive**: derive structure offsets from the binary's own code or from structural invariants and cross-checks — never hardcode obfuscated names or per-build constants.

## Non-goals (this spec)

- The wire protocol / agent API (sub-project B) — later.
- The RustRover Kotlin plugin (sub-project C) — later.
- Method *signatures* / parameter types / generic instantiation detail — v1 reads method *names*; richer signatures are a later enhancement.
- Games that protect themselves (active anti-cheat blocking injection, per-access-encrypted metadata). The respect gate declines these; out of scope.

## Architecture

A pipeline rooted at the metadata. Five components:

1. **Locator** — finds the root (metadata base + header). The single high-risk component; everything downstream is mechanical once it succeeds.
2. **Metadata reader** — given the root, walks `typeDefinition[]` (→ name, namespace, fieldStart/count, methodStart/count), `fieldDefinition[]` (→ name, type), `methodDefinition[]` (→ name). Produces the complete model — all types, loaded or not.
3. **Model** — `assemblies → images → types → { fields, methods }`, each type carrying a dynamic `loaded` flag. (Reshape the existing `agent-core` dump model toward this.)
4. **Confirmation output** — serialize the full tree to `internals.txt`.
5. **Live-watch** — poll `g_typeTable` for slots flipping null→class as the user plays; update each type's `loaded` flag and emit "live events." Rides on top of the static-complete tree: the tree is the map, live-watch shows what's lighting up.

Crash-safety is a property of every component: read-only, region-validated, bounded.

## The Locator — string-heap-anchored header find

Chosen over alternatives because it anchors on data already proven readable and avoids both failure modes that have hurt us (identifying obfuscated functions; unbounded/ blind scans).

**Algorithm:**

1. **Locate the string heap for free.** Read a sample of loaded classes (already possible). Every name pointer (`class+0x10`) points into the identifier **string heap**. Cluster these pointers to bound the heap's address range. (No new RE — we already read names from here.)
2. **Find the header, bounded to the region around the heap.** The metadata blob is one contiguous allocation (`[header | …tables… | string heap | …]`), so the header sits a bounded distance from the located heap. The header is a run of `(offset:u32, size:u32)` pairs.
3. **Cross-check (the false-positive kill-shot).** The header contains `stringOffset`/`stringSize`. The correct base is the candidate where **`base + stringOffset` lands inside the independently-located string-heap range** and `stringSize` matches its extent. Solving for the base that points back at the heap we already see drives false positives to ~zero.
4. **Confirm before trusting.** Re-derive several loaded classes' names *through the header* (`typeDef → nameIndex → string heap`) and verify they match the names already read off `g_typeTable`. Agreement = root confirmed.

**Rejected alternatives:**
- *Read an obfuscated accessor's bytecode for the metadata global* — requires identifying the right function among 502 obfuscated exports (the failure that crashed us before).
- *Blind header signature hunt* — false positives + unbounded scan faults under Wine.
- *Class→typedef linkage* — depends on an uncertain version-specific `Il2CppClass→typeDef` offset. Kept as a backup route for unloaded names if the header is unreachable.

## Data flow

```
[Locator] loaded-class name ptrs -> string-heap range -> header (cross-checked) -> root {base, header}
              |
              v
[Metadata reader] root -> typeDefinition[] + fieldDefinition[] + methodDefinition[] + string heap
              |
              v
[Model] assemblies -> images -> types(+loaded flag) -> { fields, methods }
              |
              +-- [Output] -> internals.txt   (complete-tree confirmation)
              |
[Live-watch] poll g_typeTable -> flip `loaded` flags / emit events (dynamic layer over the static tree)
```

## Crash-safety & legality

- Read-only throughout. Every dereference of a scanned/derived address is gated by an `in_region` check against a captured map of committed, readable regions.
- Scans are **bounded** (region-capped / anchored windows). Full-process reads fault under Wine and are prohibited.
- The agent never calls game functions, never writes game memory, never decrypts. It reads data the runtime has already decrypted and keeps resident.
- The respect gate stays in force: if a game's protection blocks injection or the metadata is not plaintext-resident, decline — never circumvent.

## Testing & validation

- **Pure-logic units** (host-testable, no game): header-candidate validation, the `stringOffset` cross-check, typeDef/field/method record parsing against synthetic fixtures. Mirrors the existing `agent-core` metadata test pattern.
- **The Phase-1 spike is the gate:** on PW, *cluster loaded-class name pointers → locate string heap → find the header whose `stringOffset` points back into it → re-derive known names and match*. Pass = root reached and trustworthy. This must pass before any downstream component is built.
- **Cross-check completeness:** the loaded subset read via `g_typeTable` must be a subset of, and name-consistent with, the types read via the root. Divergence flags a Locator bug.
- **Crash-safety:** the game must survive every run (verified empirically on PW, as in prior iterations).

## Phases

1. **Locator spike + complete tree** (the make-or-break). Sub-phases iterate only on failure: (1a) locate string heap, (1b) find header, (1c) cross-check + confirm, (1d) walk typeDefs → type list to `internals.txt`, (1e) extend the same reader to fields + methods. Success = complete tree in `internals.txt`.
2. **Cleanup** — remove dead code: the retired sig-scanner, blind/full-process scans, unused `#[allow(dead_code)]`, the dead RealRuntime/TCP path, unused imports.
3. **Quality** — dedup, correct module boundaries, naming, offset verification, the `loaded` flag wired into the model.
4. **Optimize** — cache/refresh the region map instead of rebuilding per tick; targeted `g_typeTable` watch for live-events; bound per-tick cost; tune poll interval.
5. **Freeze the agent↔plugin API** (snapshot + delta protocol) — the seam into sub-project B.

## Risks & fallbacks

- **Primary risk:** PW transformed the header *structure* in place (not just stripped the magic). Only real failure mode. No current evidence for it; strong evidence the metadata is plaintext-resident (we read names from the string heap).
- **Graceful floor:** if the formal header is unreachable, we still have `g_typeTable` (loaded subset, named) + the located string heap, plus the class→typedef backup route for unloaded names. We never regress below the already-proven loaded-subset dump.
- **Multi-region table:** the metadata blob may span adjacent sub-regions; the Locator coalesces contiguous regions within the crash-safe envelope.
