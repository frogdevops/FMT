# Internals Brick 2a — `il2cpp` Read/Resolve API — Design

- **Date:** 2026-05-27
- **Status:** Draft — pending spec review
- **Scope:** The **foundation** of the internals brick (the second domain brick of Spec 2): a minimal, **by-name**, read/resolve `il2cpp` API exposed to WASM scripts. internals is a **peer domain**, not a lookup service for external — an internals-only actor works entirely by name and never touches a raw address. **Out of scope:** `set_field` (sub-brick 2b, gated write), method **invoke** (2c), method **hook** (2d), runtime field *enumeration* (deferred — `internals.txt` is the discovery surface), and full live-instance enumeration.

## Context

internals itself bricks into layers, each proven before the next: **2a read/resolve → 2b set_field → 2c invoke → 2d hook**. This is 2a. The resolver is already proven (`internals.txt` dumps clean on PW, *and survived the game update that broke export-hardcoded tools* — see `no-hardcoding-adaptive-resolution`). 2a **wraps the proven machinery as a callable, typed, by-name API** — it re-solves nothing:
- the class-table walk (from `build_type_maps`),
- `collect_runtime_fields(klass, …) → (name, type_name, offset, token)` per field (via `class_get_fields` or walking `klass->fields`),
- `class_get_name`/`class_get_namespace`, and the `Il2CppType` `tc` discriminator the resolver already reads.

**Peer-domain principle:** internals speaks il2cpp natively (classes/fields by name) and reuses external's *validated* read under the hood (DRY) — so it stands alone (read by name) **and** composes (emit `address + ValType`), and an actor picks their level. This honors the [[spec2-domain-audit-and-cleanup]] **composition contract**: internals emits external's *exact* currency (`u64` address, `mem_value::ValType`), never a parallel type system.

## Goal

A WASM script resolves and reads game state **by name** — `il2cpp.get_field(player, Player, "health")` — without touching an address, and can also get `(offset, val_type)` to compose with `mem.*`. The cross-brick proof: read `Player.health` by name, live, on PW.

## The class handle

**A class handle is its klass pointer (`u64`)** — already external's currency (an address). `find_class` returns it; every other op takes it. `0` means "not found".

## The 6 by-name ops — core Rust API (in `internals`)

The foundation covers **both axes** — fields *and* methods. A class handle is a klass ptr (`u64`); a **method handle is a `MethodInfo*` (`u64`)** — same currency.

```rust
// crates/agent/src/internals/api.rs  (new)
/// Search the live class table; `name` is "Class" or "Namespace::Class". 0 = not found.
pub fn find_class(name: &str) -> u64;
/// Locate a method by name + arg count (argc disambiguates overloads). Returns the
/// MethodInfo* handle, or 0. READ-ONLY — just locates; calling (2c) / hooking (2d) build on it.
pub fn find_method(klass: u64, name: &str, argc: u32) -> u64;
/// Field offset + external ValType for `name`, or None. (The composition bridge.)
pub fn field_info(klass: u64, name: &str) -> Option<(u32, ValType)>;
/// Read a field by name through external's validated read. The native read.
pub fn get_field(instance: u64, klass: u64, name: &str) -> Result<Value, i32>;
/// Address of a static field (no instance needed — the entry point). 0 = not found.
pub fn static_field(klass: u64, name: &str) -> u64;
/// The klass pointer stored at an object's head ("what is this object?"). 0 = unreadable.
pub fn klass_of(instance: u64) -> u64;
```

`find_method` returns the `MethodInfo*` because that's the handle both later layers need: 2c (invoke) passes it to `runtime_invoke`; 2d (hook) reads its compiled-code pointer (`methodPointer`) to detour. 2a only *locates* it.

## The two rules

- **`val_type` mapping** — keyed off the `Il2CppType` **`tc` discriminator** (not the type-name string, which would be fragile): the **numeric primitives `tc 0x02–0x0D`** map to their exact `ValType` (Boolean→`U8`, Char→`U16`, I1→`I8`/U1→`U8`, I2→`I16`/U2→`U16`, I4/Int32→`I32`, U4→`U32`, I8→`I64`/U8→`U64`, R4/Single→`F32`, R8/Double→`F64`); **String (0x0E), Object, CLASS, ARRAY/SZARRAY, GENERICINST** (reference types) → `U64` (the pointer — chase it); **Void (0x01)** → `None` (no value). A pure `valtype_from_tc(tc: u8) -> Option<ValType>` lives in `agent-core` and is unit-tested. The field's full il2cpp type name remains available via the resolver for display.
- **`find_class` matching** — accepts bare `"Player"` or qualified `"Namespace::Player"`; on a bare-name collision, returns the first match; `0` if none. (Qualified form disambiguates; `internals.txt` shows the exact names.)

## Reuse, no re-solving

- `find_class` → the proven class-table walk + `class_get_name`/`class_get_namespace`.
- `find_method` → `class_get_method_from_name(klass, name, argc)` (or iterate `class_get_methods` matching name + `parameters_count`) → the `MethodInfo*`. Read-only. **May require resolving method-accessor exports not yet in `Il2CppApi`** (`class_get_method_from_name`/`class_get_methods`) — added via the same signature-scan approach as the existing exports, never hardcoded. Flagged for the plan alongside `static_field` as the two pieces of 2a needing new resolution.
- `field_info`/`get_field` → `collect_runtime_fields` for the offset + the field's `tc` → `valtype_from_tc`.
- `get_field` → external's `cache::validate_read` + typed read (`external::api::read`) + `mem_value` decode — internals composes external *under the hood*.
- `static_field` → the field machinery + the klass's **static-fields base pointer** (a klass-struct offset — if not already in `il2cpp_config`, derived **structurally** like the other offsets, never hardcoded) + detecting the field is static. Resolves the static field's absolute address. **This is the one op whose offset machinery may need a small derivation — flagged for the plan as the riskiest piece of 2a.**
- `klass_of` → one external read of `u64` at `instance + 0`.

## Host ABI — `il2cpp.*` (read-only, registered alongside `mem.*`)

The runtime registers these on the same Linker as `mem.*` (extend `run_wasm_with_mem` → it registers both API namespaces). Read-only, so **no write gate**. Guest-memory access reuses the bounds-checked helpers from `mem_host`.

```
il2cpp.find_class(name_ptr: i32, name_len: i32) -> i64            // klass ptr, 0 = not found
il2cpp.find_method(klass: i64, name_ptr: i32, name_len: i32, argc: i32) -> i64  // MethodInfo*, 0 = not found
il2cpp.field_info(klass: i64, name_ptr: i32, name_len: i32) -> i64
    // not found -> -1; else ((val_type_tag as i64) << 32) | (offset as u32 as i64)
il2cpp.get_field(instance: i64, klass: i64, name_ptr: i32, name_len: i32, out_ptr: i32, out_cap: i32) -> i32
    // writes the encoded Value into the guest buffer; returns bytes (>=0) or a negative status
il2cpp.static_field(klass: i64, name_ptr: i32, name_len: i32) -> i64   // static field address, 0 = not found
il2cpp.klass_of(instance: i64) -> i64                                  // klass ptr at instance head, 0 = unreadable
```

`get_field` returns the same `mem_value::status` codes external uses (`ERR_UNREADABLE`, `ERR_BAD_TYPE`, `ERR_BUF_TOO_SMALL`) — one error vocabulary across the platform.

## Composition (the cross-brick proof)

internals stands alone *and* harmonizes with external:
- **native (internals-only):** `cls = find_class("Player"); hp = get_field(player, cls, "health")` — no address.
- **bridge (compose):** `(off, ty) = field_info(cls, "health"); hp = mem.read(player + off, ty)`.
- **full chain (internals→external→internals):** `gm = find_class("GameManager"); paddr = static_field(gm, "localPlayer"); player = mem.read(paddr, U64); hp = get_field(player, find_class("Player"), "health")`.

2a's gate proves `get_field` reads a real field value live, and the full chain closes on PW — the first time the domain graph is *visible*.

## Error handling & safety

- Read-only; every read goes through external's validated path (bad address → `Err`, never a fault). `find_class`/`static_field`/`klass_of` return `0` on miss, never panic.
- No allocation on the hot path beyond the returned `Value`. Fuel + linear-memory caps from Spec 1 still bound the script.

## Testing

- **Host unit tests (`agent-core`):** `valtype_from_tc` — every primitive `tc` → correct `ValType`; reference-type `tc`s → `U64`; unknown `tc` → `None`/`U64` fallback (pin the exact rule).
- **WASM cross-brick gate (PW, manual):** a test `.wasm` that `find_class("...")`, `find_method("Update", 0)` returns non-zero (the method axis resolves), `get_field`s a known field and logs a sane value, `field_info` + `mem.read` agree with `get_field`, `static_field` → `mem.read` → `get_field` chain yields a live value, and `find_class("Nonexistent")` → 0 / `get_field` of a bad instance → `ERR_UNREADABLE` (no crash). Mirrors `test_mem.wasm`.

## Out of scope (later internals sub-bricks)

- **2b** `set_field` by name (gated, guarded-write underneath).
- **2c** method **invoke** (`runtime_invoke` + arg marshaling).
- **2d** method **hook** (detour a managed method — the crown jewel).
- Runtime field **enumeration** (records-with-strings ABI; `internals.txt` is the discovery surface for now).
- Live **instance enumeration** (heap walk).

## Implementation sequencing (for the plan)

1. `agent_core`: `valtype_from_tc(tc) -> Option<ValType>`, TDD.
2. `internals::api` — `find_class`, `find_method`, `field_info`, `get_field`, `static_field`, `klass_of` (wrapping `collect_runtime_fields` + the class-table walk + `class_get_method_from_name` + external's read + `valtype_from_tc`).
3. `runtime` — register the `il2cpp.*` host functions alongside `mem.*` (read-only, no gate).
4. WASM cross-brick gate on PW (manual).
