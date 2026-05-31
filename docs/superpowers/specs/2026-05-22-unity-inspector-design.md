# Live Unity Internals Inspector — Design

**Date:** 2026-05-22
**Status:** Approved design, pre-implementation
**Author:** Rust-Frog (with Claude)

## 1. Purpose

A **personal, live observability tool for Unity games** — to *see how good games are built* by watching their internals while they run. Think "debugger / profiler / devtools, pointed at a running game," surfaced inside a JetBrains IDE.

It is **not** a decompiler, a cracker, or an extraction tool. Static code reading is already solved by free tooling (JetBrains' own dotPeek, dnSpy, ILSpy). This tool fills the gap those can't: **live runtime state** — values changing, the scene tree mutating, logic firing *as the game runs*.

This is a personal project, not a product. It is not distributed and not commercial.

## 2. Scope & Ethics (first-class)

The ethical/legal posture is a designed behavior, not just intent.

**The rule:** *We try to observe. If the game lets the agent in, we inspect it. If anti-cheat or active protection blocks the injection, we stop, respect it, and never force or circumvent. The boundary is decided by the game, not by us.*

- **Observe-only**, on games the user **owns**, run **locally**, for **personal study**. No redistribution of anything observed.
- **Respect gate** (a real component): on attach, the agent probes for protection signals (anti-cheat modules, active anti-tamper, a hostile/hooked runtime). If found, it **declines gracefully** and does nothing further.
- **We never decrypt anything.** Live observation sidesteps decryption entirely: the running game has already decrypted its own metadata to function; we read the resolved state through the runtime's public API. On-disk encryption is a *static tool's* problem, never ours.
- **No circumvention, ever** — by construction. The tool contains **no** anti-cheat evasion, **no** TPM/DRM bypass, **no** de-obfuscation, **no** asset/code extraction. The capability to do the illegal thing is simply never built.
- **Out of scope by choice:** online/competitive games, anti-cheat-protected games, obfuscated/stripped names (we faithfully show whatever names exist — never reconstruct deleted ones).

**v1 target:** MiSide — single-player indie horror, owned, local, no anti-cheat. (Runtime/encryption status to be confirmed by inspecting its install folder; very likely Il2Cpp.)

## 3. Why these technical choices

- **In-process agent, not external memory reader.** Being inside the process lets us call the runtime's **stable exported C API** (`il2cpp_*` / `mono_*`) instead of parsing unstable internal struct layouts. The API gives correct names/types/values and survives Unity version changes; raw memory parsing is perpetual offset-chasing and races the GC. (Rule of thumb: *inside + call the API = resilient; outside + parse bytes = fragile.*)
- **Il2Cpp first, Mono later.** Every Unity game is one or the other. Il2Cpp is the harder case and what modern Steam games (incl. MiSide) ship; solving it first means the Mono backend later is cheap (same internal interface, `mono_*` calls). The plugin and protocol never change — only the agent grows a second backend.
- **One Windows DLL artifact for both OSes.** A Rust `cdylib` built for `x86_64-pc-windows-*` runs natively on Windows *and* under Wine/Proton on Linux (Wine runs Windows DLLs). One agent, both platforms.
- **Socket as the universal boundary.** localhost TCP works transparently across the Wine boundary and across OSes. All platform-specific concerns are pushed into (a) the agent's build target and (b) the user's own injection step. The plugin only ever talks to a socket, so it is identical on Windows and Linux and never touches game memory, process namespaces, or ptrace.

## 4. Architecture

```
Phase 0 — confirmation (no socket, no UI):
┌────────────────────────┐
│  Game process           │
│  ┌──────────────────┐   │   writes
│  │ Agent (Rust DLL)  │───┼────────────►  internals.txt   ← "the DLL works" proof
│  │  - il2cpp reads   │   │
│  │  - respect gate   │   │
│  └──────────────────┘   │
└────────────────────────┘

Phase 1+ — live plugin (socket added to the agent):
┌────────────────────────┐    localhost TCP     ┌──────────────────────────┐
│  Game process           │   (fixed port,       │  RustRover / JetBrains    │
│  ┌──────────────────┐   │    serde protocol)   │  plugin                   │
│  │ Agent (Rust DLL)  │◄──┼──────────────────────┼─► tree → click → live     │
│  │  + socket I/O     │   │                      │   detail + search         │
│  │  + main-thread    │   │                      │                           │
│  │    reader → queue │   │                      │                           │
│  └──────────────────┘   │                      └──────────────────────────┘
└────────────────────────┘
```

### Components

1. **`protocol` (Rust crate)** — the shared wire contract: `serde` types for requests/responses. Pure and unit-testable on its own (round-trip tests, no game needed). Kept WASM-friendly for the future. Source of truth that the Kotlin side mirrors.

2. **`agent` (Rust `cdylib`, Windows target)** — the heart, built in two stages:
   - **Stage 0 (the confirmation build):** on load, resolve the il2cpp exports (`GetProcAddress`), run the **respect-gate probe** (decline if protection detected), then read all internals and **dump them to `internals.txt`**. No socket, no UI. The text file is the sole proof the DLL works.
   - **Stage 1 (the plugin feed):** the same agent gains a **socket** and serves the protocol. **Threading:** reads that touch Unity objects run on the **game's main thread** (via an installed update hook) under a **per-frame budget** so the game doesn't stutter; results go to a queue consumed by a **background thread** doing serialization + socket I/O. Networking never blocks the game.

3. **Minimal injector (deferred)** — likely just a simple shell script: for **Proton**, a small launch wrapper using the Wine doorstop / `WINEDLLOVERRIDES` trick; for a **native Linux** game, `LD_PRELOAD`. Built *after* the DLL exists; until then, injection is done by whatever means is handy (an existing loader/tool).

4. **JetBrains plugin (Kotlin/Gradle, IntelliJ Platform SDK)** — the real frontend. A **Tool Window** with a **Tree** of internals (like the Project panel). Clicking an item opens a **detail view** (like opening a file) showing its type/fields/components/values, **updating live**. Includes **inspect** and **search**.

> No terminal watcher / CLI client is built. The `.txt` dump is the Stage-0 confirmation; the plugin is the only consumer.

### Repository structure

The existing `Frog` Rust scaffold becomes a Cargo **workspace**:

```
Frog/
├── crates/
│   ├── protocol/     # shared serde wire types (needed once the socket exists)
│   ├── agent/        # cdylib injected into the game (il2cpp backend)
│   └── injector/     # minimal loader — deferred, built after the DLL
├── plugin/           # Kotlin + Gradle JetBrains plugin (separate build)
└── docs/superpowers/specs/
```

## 5. Data flow & protocol

- Transport: **localhost TCP, fixed port**, length-prefixed messages.
- Format: **JSON for v1** (readable, easy to debug with `nc`); a compact binary format (e.g. postcard/bincode) can replace the high-frequency `watch` stream later.
- Message kinds (illustrative): `Hello`/handshake, `ListClasses`, `Tree`, `GetObject(id)`, `Watch(id)` / `Unwatch(id)`, `WatchUpdate(id, …)`, `Declined(reason)` (respect gate), `Error`.
- **Watch-on-select:** the consumer only streams what's open. Selecting an item sends `Watch`; switching away sends `Unwatch`. The game is taxed only for the few objects actually being viewed — this is also the answer to latency/volume: localhost round-trip is sub-ms; the real costs are main-thread budget and data volume, both controlled by fetching on demand and watching only the open item.

## 6. Build order

Minimal first: prove the hard part (reading internals) with a text file, then build the frontend.

- **M0 — the confirmation (`.txt` dump).** The DLL loads, runs the respect-gate probe, calls `il2cpp_*`, reads **all** the internals (classes, hierarchy, sample object fields/values), and writes them to `internals.txt`. Opening that file and seeing real, readable names/values **is the proof the DLL works** — no socket, no terminal, no UI. Loaded by whatever injection is handy for now. This is the green-light gate.
- **M1 — the plugin (frontend).** Green light given, build the RustRover plugin. This phase adds the **socket + `protocol`** to the agent (live updates can't come from a static file), then the Kotlin tool window: tree → click → live detail view, plus search. Sub-steps: socket up (smoke-checkable with a one-off `nc` if desired) → inspect an object → live watch → tree/search UI.
- **Deferred / later:** minimal injector; phase-2 live logic tracing; Mono backend.

## 7. v1 scope vs later

**v1 (this design):**
- Il2Cpp backend only.
- Live **structure + values**, watch-on-select.
- Respect gate (decline on protection; never circumvent/decrypt/de-obfuscate/redistribute).
- Terminal watcher + JetBrains plugin (tree, inspect, search).
- Target: MiSide and other non-protected Unity Il2Cpp games the user owns.

**Phase 2 — live logic tracing.** Watch *methods fire* (call + args, events, state transitions) via method hooking (Harmony/il2cpp-hook tech, as BepInEx uses). Still pure observation; the profiler view.

**Later:** Mono backend; WASM reuse of the `protocol`/core crates; richer detail rendering.

## 8. Non-goals

No Mono backend in v1. No method/logic tracing in v1. No circumvention of any protection. No de-obfuscation or static decompilation (use dotPeek/dumpers for the static recipe). No asset/code extraction or redistribution. No online/competitive/anti-cheat targets. No WASM build (kept friendly only). No plugin-driven auto-injection or process auto-discovery (user injects manually; a fixed-port socket makes discovery unnecessary).

## 9. Open questions for the implementation plan

1. **Connection direction:** agent dials out to a listening plugin, vs. agent listens on a fixed port and the plugin connects with retry. (Both avoid process discovery; leaning toward whichever is simplest to make robust against game-restart timing.)
2. **Finding live object instances:** GC-heap walk vs. hooking an update loop to capture references — implementation detail to settle.
3. **Windows build target:** `x86_64-pc-windows-gnu` vs `-msvc`.
4. **Respect-gate signals:** concrete list of anti-cheat module names / protection indicators to probe for.
5. **Confirm MiSide specifics:** runtime backend (Il2Cpp vs Mono) and on-disk encryption, by inspecting its install folder.
6. **Injection for M0:** how to get the DLL in for the first `.txt` test — leaning toward a simple `.sh` launch wrapper (Proton doorstop / `WINEDLLOVERRIDES`; or `LD_PRELOAD` for a native game), or an existing loader for the very first run.
7. **`.txt` dump format:** flat list vs indented tree; how much per object (all fields, or a capped sample) for the M0 confirmation.
