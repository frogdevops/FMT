# External Brick — `mem` Read/Write API — Design

- **Date:** 2026-05-27
- **Status:** Draft — pending spec review
- **Scope:** The **first brick of Spec 2**: a minimal, type-driven, full-control memory read/write API for the `external` domain, exposed to WASM scripts. Built first because it is the most dangerous domain (write can crash the game) — proving the safety+performance model here sets the pattern for the `internals` and `protocol` bricks. **Out of scope:** the other two domain bricks, named/game-typed access (that is `internals` composing onto this), the frontend transport's *implementation*, events/interactions, and SEH fault-catching (a known v2 hardening).

## Context

Spec 2 (the read+write platform) is bricked into three domain sub-specs — `external → internals → protocol` — each its own spec→plan. This is `external`. The domain restructure already isolated the code (`external/{region_map,scan,write}`), and memory is reliability-proven (read + write, on PW). This brick turns that proven substrate into a callable API.

**Locked design decisions (from brainstorming):**

| Decision | Locked choice |
|---|---|
| Surface | **5 ops**: `read`, `scan`, `regions` (read side) · `write`, `write_if` (write side) — fits on a postcard |
| Type model | closed **typed value set**, declared per op (anti-garbage by construction) |
| Power | **full control** — any address, any type; the gate withholds *crashes*, never *capability* |
| Write governance | **gated** — read is open to every module; write is granted per-module by the host |
| Read performance | sorted **region cache + binary-search validate** — near-zero, no syscall on the hot path |
| Write safety | fresh-validate → optional confirm → guarded write |
| Transport | **WASM host-functions now**; frontend TCP-command door designed into the shape, built later |
| Philosophy | Rust's: safety **and** performance, both peak. The API is the *first contact* — a bad address is rejected before execution, never crashes |

## Goal

A WASM module can call `mem.read`/`mem.scan`/`mem.regions` always, and `mem.write`/`mem.write_if` when granted, to read and write the live game's memory with declared types — and a delusional address returns an error, never a crash.

## The value model (pure, in `agent-core`)

The closed type set — the whole "anti-stupidity" gate:

```rust
// crates/agent-core/src/mem_value.rs  (new, pure, host-tested)

/// The closed set of value types the mem API understands. `u8` tag for the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValType {
    U8 = 0, U16 = 1, U32 = 2, U64 = 3,
    I8 = 4, I16 = 5, I32 = 6, I64 = 7,
    F32 = 8, F64 = 9,
    Bytes = 10, // length supplied by caller
    Cstr = 11,  // NUL-terminated, max length supplied by caller
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U8(u8), U16(u16), U32(u32), U64(u64),
    I8(i8), I16(i16), I32(i32), I64(i64),
    F32(f32), F64(f64),
    Bytes(Vec<u8>), Cstr(String),
}

impl ValType {
    /// Fixed byte width, or None for variable-length (Bytes/Cstr).
    pub fn fixed_width(self) -> Option<usize> { /* U8=>1 … F64=>8, Bytes/Cstr=>None */ }
    pub fn from_tag(tag: u8) -> Option<ValType>;
}

impl Value {
    /// Little-endian bytes of this value (or raw bytes / UTF-8 for Bytes/Cstr).
    pub fn encode(&self) -> Vec<u8>;
    /// Interpret `bytes` as `ty`. None if the length is wrong for a fixed type.
    pub fn decode(ty: ValType, bytes: &[u8]) -> Option<Value>;
    pub fn val_type(&self) -> ValType;
}
```

Negative status codes shared by the ABI (also defined here so host + future frontend agree):

```rust
pub mod status {
    pub const OK: i32 = 0;
    pub const ERR_UNREADABLE: i32 = -1;   // address not in a readable region
    pub const ERR_UNWRITABLE: i32 = -2;   // target not committed/writable
    pub const ERR_BAD_TYPE: i32 = -3;     // unknown ValType tag / bad length
    pub const ERR_BUF_TOO_SMALL: i32 = -4;// guest out-buffer too small
    pub const ERR_DENIED: i32 = -5;       // write not granted to this module
    pub const CHANGED: i32 = 1;           // write_if: current != expected, not written
}
```

## The 5 ops — core Rust API (in `external`)

Operates on raw process memory using the region cache (reads) and the proven guarded write (writes):

```rust
// crates/agent/src/external/api.rs  (new)
pub fn read(addr: usize, ty: ValType, len: usize) -> Result<Value, i32>;
pub fn scan(pattern: &[u8], max_hits: usize) -> Vec<usize>;
pub fn regions() -> Vec<RegionInfo>;            // { base, size, prot }
pub fn write(addr: usize, value: &Value) -> Result<(), i32>;
pub fn write_if(addr: usize, expected: &Value, new: &Value) -> Result<bool, i32>; // Ok(true)=written, Ok(false)=changed
```

## Read architecture — near-zero (the hot path)

```rust
// crates/agent/src/external/cache.rs  (new)
// Global cached region map; reads validate against it, never syscall on the hot path.
static REGIONS: OnceLock<RwLock<RegionMap>> = OnceLock::new();
```
- A background thread re-captures the region map every ~500 ms (off the hot path; the staleness probe proved regions drift slowly enough that this stays current).
- `read` validates the target by a **binary search** over the cached sorted regions (`RegionMap::in_region`, already O(log n) — ~10 comparisons, single-digit ns, no syscall, no allocation), then performs a typed load and returns a `Value`.
- **Cache miss** (address not in any cached region — e.g., a freshly-allocated region): fall back to one live `VirtualQuery`; if valid, read it (the next background refresh folds the region in). Correct for new memory, slow path only on miss.
- `scan` walks committed regions with a bounded, streaming substring search (first-byte `memchr`-style skip, then compare) — capped by `max_hits`; it does **not** snapshot-then-search. This replaces the old per-call 8192-region capture.

## Write architecture — gated, guarded, confirming (the cold path)

Write is rare and dangerous, so it validates *harder* and can afford the syscall:
- `write(addr, value)`: encode the value → run the proven `guarded_write` (fresh `VirtualQuery` committed/writable check → `VirtualProtect` RW → write → restore). Bad target → `Err(ERR_UNWRITABLE)`, never a fault.
- `write_if(addr, expected, new)` — **"read, confirm, write"**: `read` the current value (fast path), compare to `expected`; only if equal, `write(new)`. Returns `Ok(false)` (= `CHANGED`) if it differs — refuses the write. This is the safety primitive for "don't clobber what you didn't expect."

## Host ABI — the WASM boundary (the keystone)

WASM params are scalars and the guest can't see host memory, so results cross **into the guest's own linear memory** via a guest-provided buffer (the Spec-1 hybrid ABI, generalized). Host imports under module `mem`:

```
mem.read(addr: i64, ty: i32, len: i32, out_ptr: i32, out_cap: i32) -> i32
    // writes the encoded value into guest memory at out_ptr (<= out_cap);
    // returns bytes written (>=0) or a negative status code. `len` used for Bytes/Cstr.

mem.scan(pat_ptr: i32, pat_len: i32, out_ptr: i32, out_cap_count: i32) -> i32
    // reads pattern from guest mem; writes up to out_cap_count u64 addresses
    // (LE) to out_ptr; returns hit count.

mem.regions(out_ptr: i32, out_cap_count: i32) -> i32
    // writes region records {u64 base, u64 size, u32 prot} to guest mem; returns count.

mem.write(addr: i64, ty: i32, in_ptr: i32, in_len: i32) -> i32
    // reads in_len bytes from guest mem at in_ptr, decodes as ty, validates, writes;
    // returns OK or negative status. Only registered for write-granted modules.

mem.write_if(addr: i64, ty: i32, exp_ptr: i32, exp_len: i32, new_ptr: i32, new_len: i32) -> i32
    // returns OK (written), CHANGED (1, not written), or negative status.
```

All guest-memory access is bounds-checked against the guest's linear memory size (as Spec-1's `log` already does) before reading args or writing results. Value encode/decode uses `agent_core::mem_value`, so the wire format is identical for the future frontend transport.

## Write governance — gated per module

- **Read host-functions (`read`/`scan`/`regions`) are always registered** for every module — read is safe for anyone.
- **Write host-functions (`write`/`write_if`) are registered only if the module is write-granted.** v1 grant mechanism: a host flag (env `FROG_WASM_WRITE=1` grants write to the configured module; default off → read-only). A non-granted module simply has no write imports to call (a link error if it tries, surfaced at instantiation) — the cleanest possible gate. The orchestrator owns this decision; the split read/write surface makes it nearly free.

## Transport — one core, WASM door first

The 5 ops live once in `external::api` (the runtime owns them). v1 builds the **WASM host-function door**. The **frontend TCP-command door** is designed-in — the same op set + the same `mem_value` wire encoding — but implemented when a frontend exists to drive it (Layer 2). No frontend code in this brick.

## File structure

- `crates/agent-core/src/mem_value.rs` — NEW, pure: `ValType`, `Value`, `encode`/`decode`, `status` codes. Host-tested.
- `crates/agent/src/external/api.rs` — NEW: the 5 core ops over raw memory.
- `crates/agent/src/external/cache.rs` — NEW: the background-refreshed cached `RegionMap` + miss-fallback validate.
- `crates/agent/src/external/scan.rs` — extend with the streaming AOB search (`scan`).
- `crates/agent/src/external/{region_map,write}.rs` — reused as-is (`in_region`, `guarded_write`).
- `crates/agent/src/runtime/host.rs` — extend: register the `mem.*` host functions (read trio always; write pair when granted), bridging guest calls ↔ `external::api` via `mem_value` encoding.

## Error handling & safety

- Every op returns a status; a bad address/type/buffer is an `Err`/negative code, never a panic or fault. (Residual: a *just-freed* address still in the cache for <500 ms is the one stale-positive window; SEH fault-catch is the v2 bulletproofing, out of scope here.)
- Reads never allocate on the hot path beyond the returned `Value`; writes restore page protection.
- Fuel + linear-memory caps from Spec 1 still bound the calling script.

## Testing

- **Host unit tests (`agent-core::mem_value`, on Linux):** `encode`/`decode` round-trips for every `ValType`; `fixed_width`; `decode` rejects wrong-length fixed types (→ `ERR_BAD_TYPE`); `from_tag` rejects unknown tags; `status` code constants.
- **WASM integration gate (on PW, manual):** a test `.wasm` that (1) `mem.scan`s for a known byte pattern, (2) `mem.read`s a found address as a typed value and logs it, (3) with write granted, `mem.write_if`s it (confirm), and (4) confirms a delusional address (`0x10`) returns `ERR_UNREADABLE`/`ERR_UNWRITABLE`, game survives. A non-granted module confirms `write` is absent. Mirrors the proven `FROG_*` probe gates.

## Out of scope (later bricks / versions)

- The `internals` and `protocol` bricks; named/game-typed reads (`internals` composing onto `external`).
- The frontend TCP-command transport implementation.
- SEH fault-catching (v2 read hardening).
- Events, `on_change`, interactions, panel control (Layer 2).
- A batch/multi-read fast path (optimization for later if hot loops need it).

## Implementation sequencing (for the plan)

1. `agent_core::mem_value` — `ValType`/`Value`/`encode`/`decode`/`status`, TDD.
2. `external::cache` — cached `RegionMap` + background refresh + miss-fallback validate.
3. `external::api` — the 5 ops over raw memory (reusing `in_region`, `guarded_write`, the new `scan`).
4. `runtime::host` — register `mem.*` host functions + the write grant gate.
5. WASM integration gate on PW (manual).
