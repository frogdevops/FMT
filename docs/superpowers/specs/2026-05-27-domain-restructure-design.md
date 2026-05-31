# Domain Restructure + Protocol Rebuild — Design

- **Date:** 2026-05-27
- **Status:** Draft — pending spec review
- **Scope:** A **clean-foundation pass** before Spec 2. Reorganize the agent into domain-isolated modules (`external` / `internals` / `protocol` / `runtime`), **rebuild** the protocol capture from scratch as a universal raw-frame pipeline, and fold in the audit's confirmed cleanup. This is the "massive architectural shift" the Spec-1 design doc warned was coming. **Spec 2 (the `mem`/`il2cpp`/`proto` read+write APIs + event model) is explicitly OUT of scope** — this pass exists to make that floor level first.

## Why now

The three domains were audited and given a reliability verdict (see `spec2-domain-audit-and-cleanup` memory):
- **Memory — fully reliability-proven, read + write.** Staleness (two PW runs, snapshot holds for our access pattern), read-correctness (internals 100% + `td_fail=0`), scan/locate (unanimous `string_heap_base`), and write-safety (`FROG_WRITE_PROBE` 3/3 PASS) are all settled.
- **Internals — reliability-proven**, needs only cleanup/YAGNI polish (zero unresolved; `System.Generic` is intentional).
- **Protocol — red flag.** Distrusted Gemini-authored code with a real incoming-path defect and a logging firehose (one short session produced `activity.log` 152 MB + `packets.log` 153 MB). Needs a *rebuild*, not a refactor.

But none of the three is shaped as a clean, isolated unit yet — the audit's "three shapes, not one API / battle zone." Laying Spec 2's API floor on this tilted structure is building on sand. So: restructure into domains and rebuild the protocol first; *then* the Spec-2 APIs map 1:1 onto the domain folders and the surface writes itself.

## Guiding principles

- **Domain isolation.** `external` is memory, `internals` is il2cpp, `protocol` is network — each a sealed unit you can hold in your head and test alone, communicating through a clear interface.
- **Behavior-preserving where proven.** Memory and internals *work*; moving them must change layout only, not behavior. The existing 40 tests + a PW smoke run are the safety net.
- **Rebuild only what's broken.** Don't relocate slop you're about to delete — `protocol` is rewritten fresh into its new home.
- **Pure logic in `agent-core`, FFI/OS in `agent`.** Continue the established split so domain logic stays host-testable on Linux.
- **Crash-safety is non-negotiable.** Every memory/buffer read is bounds-checked; no detour may panic the game thread.

## Section 1 — Target module structure

```
crates/agent/src/
  external/              raw process memory (PROVEN — behavior-preserving move)
    mod.rs
    region_map.rs        (← region_map.rs, unchanged)
    scan.rs              (← mem_scan.rs)
    write.rs             (← mem_write.rs)
  internals/             il2cpp (PROVEN — behavior-preserving move)
    mod.rs
    ffi.rs               (← il2cpp_ffi.rs)
    config.rs            (← il2cpp_config.rs)
    resolve.rs           (← type_resolve.rs)
    dump.rs              (← dump_writer.rs)
  protocol/              network (REBUILT, not moved — Section 2)
    mod.rs
    capture.rs           (replaces packet.rs)
    hook.rs              (← hook.rs, KEPT — generic trampoline; only packet.rs's use of it was slop)
  runtime/
    mod.rs
    host.rs              (← wasm_host.rs)
  diagnostics/
    mod.rs
    mem_probe.rs         (← mem_probe.rs — the FROG_* probes, out of the domains)
  entry.rs  paths.rs  host.rs  lib.rs   thin infra (orchestrator, paths, module-enum, wiring)
```

`agent-core` keeps the pure halves and gains one new domain module (Section 2):
```
crates/agent-core/src/
  model.rs  metadata.rs  region_churn.rs  mem_write.rs  wasm.rs  respect.rs  logfile.rs
  protocol.rs            NEW — RawFrame + bounded ring (pure, host-tested)
```
Light touch on `agent-core` otherwise — no churn just to mirror folders.

**Module-path note:** moving files changes `crate::region_map` → `crate::external::region_map`, etc. Each domain `mod.rs` re-exports its public items so call sites read `crate::external::RegionMap`. `lib.rs` declares the domain modules instead of the flat list.

## Section 2 — Protocol rebuild: universal raw capture

Replace `packet.rs` with `protocol/capture.rs` + `agent_core::protocol`.

**Data model (pure, in `agent-core`):**
- `RawFrame { timestamp_ms: u64, direction: Direction, socket_id: u64, bytes: Vec<u8> }` — `Direction` is `C2S`/`S2C`. Raw bytes only; no parsing, no formatting, ever.
- `FrameRing` — a single capped ring bounded by **both** frame count and total bytes; pushing past either cap evicts oldest. This is the firehose killer, and its eviction logic is unit-tested.

**Capture (FFI, in `agent/protocol/capture.rs`):**
- Hook the WinSock send/recv family + IOCP completion via the kept `hook.rs` trampolines.
- Each detour is dumb-fast: validate, copy raw bytes, push a `RawFrame` to the ring, return. No locks held across work that can re-enter; no formatting.
- **Hook breadth: broad ("universal").** Full send/recv family + `GQCS`/`GQCSEx`, for cross-game coverage. Accepted trade-off: more hook surface = more anti-cheat footprint; mitigated by keeping each detour minimal. (Lean alternative — only WSARecv/send/GQCS — rejected for now as PW-specific.)

**Incoming-path fix (the actual defect):**
- **Synchronous** recv/WSARecv (data present on return) → capture in the detour's return path. Simple and reliable.
- **Async/IOCP** (`WSA_IO_PENDING`) → capture at `GQCS`/`GQCSEx`, keyed by the OVERLAPPED pointer (the key the OS itself uses), fixing the slop's three bugs:
  - **(a) validate the buffer is readable before copying** — bounds-check the WSABUF pointer/len the way `RegionMap` does; never blind `from_raw_parts`.
  - **(b) remove the pending entry on completion** — no leak, no stale-pointer aliasing across reused OVERLAPPEDs.
  - **(c) cap the pending map and count APC-completion recvs we can't drain** — a known gap is *visible* in a counter, never silently dropped.

**Output:** the ring drains to the existing **TCP stream** as raw frames (consumed by `listen_packets.py` / the frontend). **Removed:** per-packet `agent.log` spam, `activity.log` pretty-print, the unbounded `packets.log`. At most one *bounded* file behind an opt-in `FROG_*` flag, off by default.

**Crash-safety on hooked threads:** the audit's `.lock().unwrap()` panic risk is fixed — `try_lock`/poison-tolerant access, a reentrancy guard on every detour, all buffers validated, all containers bounded. A detour must never panic or block the game thread.

## Section 3 — Cleanup / what leaves

- **`bson.rs` deleted from the backend.** Already dead/unwired; "universal raw" means decoding is a script/frontend concern. Returns in Spec 2 as a `proto.bson_parse` host helper, not capture code.
- **The logging firehose is removed** (see Section 2 output).
- **`hook.rs` kept**, relocated to `protocol/hook.rs`; reviewed during the rebuild but not rewritten.
- **The metadata-blob path** (`scan_process_for_metadata` / `find_and_parse_with_offset` / `find_types_array`) is **kept and documented** as the non-obfuscated easy-game fallback (returns `None` on PW; the FFI/class-table path carries the dump). A comment prevents future readers mistaking it for dead code.
- Already-done cleanup (this branch): the string-anchor metadata experiment and `td_map`/`klass_map` are gone; not re-listed.

## Section 4 — Testing & verification

**The reorg is behavior-preserving — prove it changed nothing:**
- `cargo test` (agent-core, currently 40) stays green; cross-compile to `x86_64-pc-windows-gnu` stays clean.
- **PW smoke run** after the move: the dump still produces `internals.txt` with **zero unresolved**, the same class/slot counts, the unanimous `string_heap_base`; `FROG_MEM_PROBE` and `FROG_WRITE_PROBE` still PASS. Identical behavior = the move was safe.

**The protocol rebuild is new behavior — prove it works:**
- **Pure tests (`agent-core::protocol`):** `FrameRing` eviction (count cap, byte cap, oldest-first), `RawFrame` construction, the buffer-validation decision (readable vs reject). Host-tested on Linux.
- **PW verification run:** raw frames stream over TCP and decode in `listen_packets.py`; **both directions captured** (the incoming-path fix — S2C frames actually appear); the APC-gap counter is visible in `agent.log`; **no `activity.log`, no multi-hundred-MB `packets.log`**, agent.log quiet; capture memory stays bounded over a long session; game survives.

## Implementation sequencing (for the plan)

1. Scaffold domain folders; move `external`, `internals`, `runtime`, `diagnostics` (behavior-preserving); update `lib.rs` + call sites. **Verify:** tests green, PW smoke identical.
2. `agent_core::protocol` — `RawFrame` + `FrameRing`, TDD.
3. `protocol/capture.rs` — detours → ring → TCP, incoming-path fix, crash-safety. Delete `packet.rs` + `bson.rs`, kill the firehose.
4. Wire `entry.rs` to the new protocol module; document the metadata-blob fallback.
5. **Verify:** PW run — raw both-direction frames, bounded memory, no firehose.

## Out of scope (Spec 2+)

- The `mem` / `il2cpp` / `proto` **read+write APIs** and the WASM host-call ABI.
- The event model (`on_packet`/`on_tick`), the game-frame hook, the tick scheduler.
- Exposing `guarded_write` or `proto.bson_parse` to scripts.
- Any frontend/panel work; module hot-loading.

The restructure is deliberately behavior-preserving (except the protocol rebuild, which has its own verification) so the proven domains stay proven and Spec 2 begins on level ground.
