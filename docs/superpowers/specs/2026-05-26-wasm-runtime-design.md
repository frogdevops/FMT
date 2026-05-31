# WASM Runtime — Design (Spec 1 of the observation platform)

- **Date:** 2026-05-26
- **Status:** Approved design, pending spec review
- **Scope:** The **bare WASM runtime only** — embed an engine, run one sandboxed module, prove it via a hello-world. The source APIs (`mem`/`il2cpp`/`proto`/`panel`), the event model, orchestration, and frontend wiring are **Spec 2+** and explicitly out of scope here.

## Context — the platform this is the first brick of

`Frog` is evolving from a fixed Unity-internals inspector into an **extensible observation *and interaction* platform** — a "set the stage" tool in the BepInEx / Frida / MelonLoader lineage. The agent already observes three domains: **internals** (full il2cpp class/field/generic resolution), **protocol** (WinSock packet capture), and **memory**. The vision: expose these three as **APIs — read *and* write — to a WASM scripting runtime embedded in the agent**, so developers — in *any* language that compiles to WASM — write scripts that compose the three domains (e.g. parse a packet *using* live type layout; watch and edit a typed field) and emit structured views to frontend panels. The host exposes the primitives; what an author scripts with them is the author's own work and responsibility (see Guiding stance).

**Guiding stance — an interaction platform ("wild-west stage" model):** the runtime exposes **read *and* write** host APIs across the three domains (`mem`/`il2cpp`/`proto`) plus an output/`emit` API — the same dual-use category as BepInEx, MelonLoader, and Frida. What a developer authors in a script is *their* work and *their* responsibility, conveyed by an open-source license with the usual no-warranty / no-liability disclaimer (the stage is neutral; the actor owns the act). The project is, and is positioned as, a **general reverse-engineering / observation / modding / SRE platform** — **not** a cheat for any specific game; it ships **no cheat-specific features** (no aimbot, no anti-cheat-circumvention logic, no advantage-injection). The WASM sandbox still does real work: scripts run *only* through the host APIs we expose — auditable, no arbitrary native code — making this a *more* constrained stage than BepInEx's arbitrary execution, not a less safe one.

**Use note (risk is the actor's):** writing to a *live online* game can violate its ToS and risk a ban, and where it circumvents anti-cheat for advantage it can carry legal exposure. That risk falls on whoever runs such a script — which is the whole point of the stage/actor split. The platform is general; how it is pointed is the user's choice and the user's responsibility.

This document specifies only the foundation: a working sandbox that can run a module and let it call back into the agent. We build the playpen before wiring the phone lines (read/write domain APIs are Spec 2).

## Locked decisions (carried into all later specs)

- **Runtime lives inside the agent** (the game process), so future `mem`/`il2cpp` reads are direct and cheap. (A frontend-hosted runtime would make every read a TCP round-trip.)
- **Engine: `wasmi`** — a small, pure-Rust WASM *interpreter*, embedded like a Lua scripting runtime. Chosen over a JIT (`wasmtime`) because it creates **no new executable pages** in the live game process (a quieter anti-cheat footprint) and is trivial to embed; our scripts are small reactive handlers that don't need JIT speed.
- **Execution model (for Spec 2+): reactive/event-driven** — the host fires events (`on_packet`, `on_tick`, …) into the module; a developer who wants a loop builds one on top of `on_tick`. Not exercised in Spec 1 beyond a single entry call.
- **Timing: game-frame synced by default (BepInEx-style).** All script handlers run on the **game's update tick, single-threaded** — matching what Unity mod/RE authors expect and giving frame-coherent reads + a safe point to *write*. The hooks stay dumb-fast (a detour copies bytes, enqueues, returns); once per frame the agent's update-hook drains the queue → fires `on_packet` handlers → fires `on_tick`, all on the game thread. An off-thread `on_timer` stays available for heavy work that shouldn't touch the frame. **Spec-2 sub-problem:** finding a stable per-frame hook point — likely in `UnityPlayer.dll` (the engine, usually *not* obfuscated) rather than the obfuscated `GameAssembly` — is its own prove-it hunt. **Fuel is per-*invocation*, not per-*lifetime*.** Scripts register handlers and return; the *host* runs the forever-loop and calls them (a packet-parsing script "runs for days" by being invoked millions of times, each call a blink). Fuel bounds a single handler call so one can't spin and hang/stutter — exactly the runaway-`while true` case a naive Lua executor *can't* interrupt. **Context-dependent:** tight fuel on the game-frame tick (protect FPS); generous/effectively-unlimited off-thread (`on_timer`/one-shot jobs, which can't stall the game); scripts chunk long work across ticks. So there is no ceiling on what a script can do over time — only "don't hog one frame."
- **Module memory: sandboxed arena, host-capped, language-managed.** Core WASM (what `wasmi` runs) has no built-in GC; the language compiled *into* the module manages its own **linear memory** (Rust ownership, C/C++ `malloc`/`free`, Go/C#/AS ship their GC inside the module). That linear memory is a *sealed arena separate from the game*. The host sets a **max linear-memory size per module** (a sibling to fuel). Consequences are contained: a leak/hog fills only the module's own capped box (its own allocs then fail; game/agent unaffected); a corruption/bad-free can only mangle the module's own bytes — worst case it traps and the host halts that one script. WASM bounds-checks every linear-memory access, so a module cannot reach into the game or agent through its own memory. **Therefore the only memory surface with game-level blast radius is the *game-write host function* (Spec 2)** — where the *host* reaches into the game for the script; a bad write there can crash the game (the actor's responsibility), unlike anything in the module's own sandbox. Caveat: the runtime halts safely on *script* errors (traps/fuel), but a *write* to bad memory can still crash the **game** from corrupted state — that fault is the actor's, not the runtime's to catch.
- **ABI (internal detail): hybrid** — big data (packets, memory regions) passed by reference/handle so a script reading 4 bytes doesn't drag a whole buffer; small data (strings, results) copied directly. Invisible to script authors. Spec 1 exercises only the simplest "copy a small string out" path via `log`.

## Goal of Spec 1

Embed `wasmi`; load a `.wasm` module; run it in a fuel-limited sandbox; expose **exactly one** host function — `log(text)` — so we can observe that the module ran and phoned home. Success = a hello-world module's message appears in the agent log. Nothing more.

## Architecture

Put the engine logic in **`agent-core`** (pure, cross-platform) so it is **host-testable without the game**; the Windows `agent` crate is the thin wirer.

- **`crates/agent-core/src/wasm.rs`** (new): `run_wasm(module_bytes: &[u8], log: &mut dyn FnMut(&str)) -> Result<(), WasmError>`.
  - Creates a `wasmi` engine/store, sets a **fuel limit**, defines the one host import `log`, instantiates the module, and calls its entry export (`frog_main`, taking/returning nothing).
  - The `log` import signature (WASM ABI): `log(ptr: i32, len: i32)`. The host reads `len` bytes starting at `ptr` from the **module's own linear memory** (bounds-checked against that memory's size), lossily decodes as UTF-8, and passes the string to the `log` callback. This is the simplest instance of the hybrid ABI's "pass small data out" path.
  - `WasmError` enum covers: module didn't parse, no exported memory, missing `frog_main`, trap/fuel-exhausted at runtime. All returned as `Err`, never a panic.
- **`crates/agent/src/wasm_host.rs`** (new): `maybe_run_configured()` — reads env var `FROG_WASM`; if set, reads that file's bytes and calls `agent_core::wasm::run_wasm(&bytes, &mut |s| crate::paths::log(s))`. Logs load/parse failures and the final result. If `FROG_WASM` is unset, does nothing.
- **`crates/agent/src/entry.rs`** (modify): call `wasm_host::maybe_run_configured()` from the worker, after the dump, so it never affects the normal dump path and only runs when explicitly pointed at a module.
- **`crates/agent/src/lib.rs`** (modify): `mod wasm_host;`.
- **`crates/agent-core/Cargo.toml`** (modify): add the `wasmi` dependency (must cross-compile to `x86_64-pc-windows-gnu` — it is pure Rust, so it does).

## Data flow

```
FROG_WASM=path  →  agent reads file bytes
                →  agent_core::wasm::run_wasm(bytes, log_cb)
                     ├─ wasmi: parse + instantiate (sandboxed, fuel-limited)
                     ├─ define host import: log(ptr,len) → read guest memory → log_cb(str)
                     └─ call export frog_main()
                          └─ guest calls log("hello from wasm") → appears in agent.log
```

## Error handling & safety

- **Fuel limit:** a module that loops forever exhausts fuel and traps; `run_wasm` returns `Err`, the agent logs it, and continues. A misbehaving module cannot hang the agent or the game.
- **Bounds-checked memory read:** the `log` import validates `[ptr, ptr+len)` against the guest memory size before reading; an out-of-range request yields an empty/elided string, never an out-of-bounds read.
- **Minimal surface in Spec 1:** the only host import is `log`. No domain APIs (read or write) exist yet — those arrive in Spec 2. Spec 1 is deliberately the smallest thing that proves a module runs and can call back.
- **Opt-in:** absent `FROG_WASM`, the runtime never runs; zero impact on the existing dump.

## Testing

- **Host unit tests (`agent-core`, no game needed):** `wasmi` is cross-platform, so `run_wasm` is fully testable on the dev machine with an inline WAT fixture:
  - A module that imports `log` and calls it with a constant string in its data segment → assert the callback received exactly `"hello from wasm"`.
  - A module with no `frog_main` export → assert `Err` (no panic).
  - A module whose `frog_main` loops forever → assert it traps on fuel exhaustion and returns `Err` within bounds (no hang).
  - A `log` call with an out-of-range `ptr/len` → assert no panic, empty string.
- **Integration gate (on Windows/PW, manual):** build the agent, point `FROG_WASM` at the hello-world `.wasm`, run the game, confirm `hello from wasm` in `agent.log` and the game survives. Proves a sandboxed module runs *inside the game process* and phones home.

## Host guardrails (we babysit so the game never pays for a script's mistakes)

The platform assumes actors *will* write careless or hostile scripts and contains them by construction, not by trust: the WASM **sandbox** (no reach outside the module except through host functions we expose), the **per-invocation fuel** cap (a spinning handler is killed, can't hang/stutter), and the **per-module linear-memory cap** (a leak/hog fills only the script's own box). A script's bad memory management stays the script's problem. The only surface that deliberately escapes containment is the game-write host function (Spec 2), and that one is documented as the actor's responsibility.

## Out of scope (Spec 2+)

- The `mem` / `il2cpp` / `proto` **read *and write*** APIs and the `panel`/`emit` output API.
- The event model (`on_packet`, `on_tick`, `on_change`, `on_timer`) and module subscription.
- The game-frame hook (locating a per-frame function in `UnityPlayer.dll`) and the single-threaded tick scheduler.
- Orchestration (routing events→scripts→panels), module hot-loading from the frontend.
- Any frontend/TCP changes.

## Architectural-shift impact (forward-looking — Spec 2+, not this spec)

This is the first brick of a **major architectural shift**, and the later specs *will* require substantial rework of existing code — flagged here so it's expected, not a surprise:

- **Sources become host-callable APIs.** `type_resolve` / `dump_writer` (internals), `packet.rs` (protocol), and `mem_scan` / `region_map` (memory) currently produce a one-shot `internals.txt` dump and a packet log. They must be refactored into **callable, composable functions** behind the WASM host interface — a different shape than today's batch dump.
- **Threading moves to game-frame sync.** Today the agent runs its work on its own worker thread once. The platform moves script execution onto a **single-threaded game-frame tick** (hooks enqueue; the frame drains), which reshapes how the existing capture/dump code is invoked.
- **Optimizations and structure will be stripped/reworked.** Some current code is shaped for the one-shot dump (e.g. building `td_map`/`klass_map` eagerly, the dump-to-file path). Migrating to the API model means **removing or reworking** those, and a general **code-quality pass** (clear module boundaries, the dead `allow(dead_code)` maps, the diagnostic scaffolding) as part of the shift — not bolting the platform on top of the current shape.

Spec 1 stays additive and minimal precisely so the playpen is *proven* before that larger rework begins. The migration is its own spec(s) with its own plan; this document does not undertake it.
