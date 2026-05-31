# Domain Restructure + Protocol Rebuild — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **COMMITS ARE THE USER'S.** This project's owner commits all work themselves. Do NOT run `git commit`/`git push`. Where a task ends in a checkpoint, **stop and hand the diff back to the user to commit**, then continue.

**Goal:** Reorganize the agent into domain-isolated modules (`external`/`internals`/`protocol`/`runtime`/`diagnostics`) and rebuild the protocol capture from scratch as a universal raw-frame pipeline, on a behavior-preserving base, so Spec 2's APIs map 1:1 onto domains.

**Architecture:** Part A is a behavior-preserving move of the *proven* domains (memory, il2cpp) + runtime + diagnostics into domain folders — verified by the existing test suite staying green and an identical PW smoke run. Part B rebuilds the protocol: a pure `agent_core::protocol` ring (host-tested) fed by dumb-fast validated WinSock/IOCP detours, draining raw frames to the existing TCP stream, with the 150 MB file firehose deleted.

**Tech Stack:** Rust, `wasmi`-adjacent agent crate (cdylib), `windows-sys` (WinSock/VirtualQuery), `iced-x86` (trampoline), cross-compiled to `x86_64-pc-windows-gnu`. Pure logic lives in `agent-core` (host-testable on Linux); FFI lives in `agent` (verified on Pixel Worlds).

**Verification commands used throughout:**
- Host tests: `cargo test -p agent-core`
- Windows compile: `cargo check -p agent --target x86_64-pc-windows-gnu`
- Deploy to game: `./deploy.sh` (builds release, copies DLLs to Pixel Worlds)

---

## PART A — Domain reorg (behavior-preserving)

> Protocol files (`packet.rs`, `hook.rs`, `bson.rs`) stay flat and untouched in Part A; they are handled in Part B. After each task the crate must cross-compile clean and `agent-core` tests stay green (currently **40**).

### Task A1: `external/` domain (memory)

**Files:**
- Move: `crates/agent/src/region_map.rs` → `crates/agent/src/external/region_map.rs`
- Move: `crates/agent/src/mem_scan.rs` → `crates/agent/src/external/scan.rs`
- Move: `crates/agent/src/mem_write.rs` → `crates/agent/src/external/write.rs`
- Create: `crates/agent/src/external/mod.rs`
- Modify: `crates/agent/src/lib.rs`, and call sites in `type_resolve.rs`, `dump_writer.rs`, `mem_probe.rs`, `entry.rs`

- [ ] **Step 1: Move the three files with git**

```bash
cd crates/agent/src
mkdir -p external
git mv region_map.rs external/region_map.rs
git mv mem_scan.rs   external/scan.rs
git mv mem_write.rs  external/write.rs
```

- [ ] **Step 2: Create `external/mod.rs` that re-exports the domain's public surface**

```rust
//! External domain: raw process memory — region snapshot + bounds-checked reads,
//! AOB/metadata scanning, and guarded writes. Reliability-proven (read + write).

pub mod region_map;
pub mod scan;
pub mod write;

pub use region_map::{is_readable, RegionMap, Tunables};
pub use scan::{find_class_table, find_types_array, scan_process_for_metadata, MetadataResult};
pub use write::{guarded_write, WriteError};
```

- [ ] **Step 3: Fix the internal cross-ref inside `external/scan.rs`, and document the easy-game fallback**

`scan.rs` re-exports from the old path. Change its line 21:
```rust
// OLD: pub use crate::region_map::{is_readable, RegionMap, Tunables};
pub use crate::external::region_map::{is_readable, RegionMap, Tunables};
```
Then add a doc comment directly above `pub fn scan_process_for_metadata` so it isn't mistaken for dead code (spec Section 3):
```rust
/// Locate + parse the global-metadata blob in memory. Only succeeds on
/// **non-obfuscated** games where the metadata magic is present; on Pixel Worlds
/// the magic is stripped, so this returns `None` and the FFI/class-table path
/// (the rest of the worker) carries the dump. Kept as the easy-game fallback.
```

- [ ] **Step 4: Update `lib.rs` — replace the three flat decls with the domain module**

Remove these lines:
```rust
#[cfg(target_os = "windows")]
mod mem_scan;
#[cfg(target_os = "windows")]
mod region_map;
#[cfg(target_os = "windows")]
mod mem_write;
```
Add (with the other domain decls):
```rust
#[cfg(target_os = "windows")]
mod external;
```

- [ ] **Step 5: Update all call sites to the new paths**

```
type_resolve.rs:19  use crate::region_map::{RegionMap, Tunables};      → use crate::external::region_map::{RegionMap, Tunables};
dump_writer.rs:29   use crate::region_map::{RegionMap, Tunables};      → use crate::external::region_map::{RegionMap, Tunables};
dump_writer.rs:27   use crate::mem_scan::MetadataResult;               → use crate::external::scan::MetadataResult;
mem_probe.rs:21     use crate::region_map::RegionMap;                  → use crate::external::region_map::RegionMap;
mem_probe.rs:19     use crate::mem_write::guarded_write;               → use crate::external::write::guarded_write;
mem_probe.rs:101,215  crate::region_map::Tunables::load()              → crate::external::region_map::Tunables::load()
entry.rs:13         use crate::mem_scan::{find_class_table, find_types_array, scan_process_for_metadata};
                                                                       → use crate::external::scan::{...};
entry.rs:15         use crate::region_map::RegionMap;                  → use crate::external::region_map::RegionMap;
```

- [ ] **Step 6: Verify it compiles for Windows and tests pass**

Run: `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep -E "error" || echo OK` → Expected: `OK`
Run: `cargo test -p agent-core 2>&1 | grep "test result" | head -1` → Expected: `40 passed`

- [ ] **Step 7: Checkpoint** — hand diff to user to commit (suggested message: `refactor: move memory into external/ domain`).

### Task A2: `internals/` domain (il2cpp)

**Files:**
- Move: `il2cpp_ffi.rs` → `internals/ffi.rs`; `il2cpp_config.rs` → `internals/config.rs`; `type_resolve.rs` → `internals/resolve.rs`; `dump_writer.rs` → `internals/dump.rs`
- Create: `crates/agent/src/internals/mod.rs`
- Modify: `lib.rs`, call sites in `entry.rs`, and the internal cross-refs among the four moved files

- [ ] **Step 1: Move the four files**

```bash
cd crates/agent/src
mkdir -p internals
git mv il2cpp_ffi.rs    internals/ffi.rs
git mv il2cpp_config.rs internals/config.rs
git mv type_resolve.rs  internals/resolve.rs
git mv dump_writer.rs   internals/dump.rs
```

- [ ] **Step 2: Create `internals/mod.rs`**

```rust
//! Internals domain: il2cpp metadata — API resolution, per-version offsets,
//! type-name resolution (string-heap derived), and the batch dump. Reliability-proven.

pub mod ffi;
pub mod config;
pub mod resolve;
pub mod dump;

pub use config::Il2CppConfig;
pub use ffi::{cstr_to_string, Il2CppApi};
pub use resolve::{build_type_maps, il2cpp_type_name, GenericCtx, TypeMaps};
pub use dump::build_internals_lines;
```

- [ ] **Step 3: Fix cross-refs inside the four moved files**

```
internals/resolve.rs:16  use crate::il2cpp_config::Il2CppConfig;            → use crate::internals::config::Il2CppConfig;
internals/resolve.rs:17  use crate::il2cpp_ffi::{cstr_to_string, Il2CppApi}; → use crate::internals::ffi::{cstr_to_string, Il2CppApi};
internals/resolve.rs:19  use crate::region_map::{RegionMap, Tunables};      → use crate::external::region_map::{RegionMap, Tunables};
internals/dump.rs:25     use crate::il2cpp_config::Il2CppConfig;            → use crate::internals::config::Il2CppConfig;
internals/dump.rs:26     use crate::il2cpp_ffi::{cstr_to_string, Il2CppApi}; → use crate::internals::ffi::{cstr_to_string, Il2CppApi};
internals/dump.rs:27     use crate::mem_scan::MetadataResult;               → use crate::external::scan::MetadataResult;
internals/dump.rs:29     use crate::region_map::{RegionMap, Tunables};      → use crate::external::region_map::{RegionMap, Tunables};
internals/dump.rs:30     use crate::type_resolve::{il2cpp_type_name, GenericCtx, TypeMaps}; → use crate::internals::resolve::{...};
```
(If `internals/ffi.rs` or `config.rs` import other moved modules, repoint them the same way — grep `crate::(il2cpp_ffi|il2cpp_config|type_resolve|dump_writer|region_map|mem_scan)` inside `internals/` and fix each hit.)

- [ ] **Step 4: Update `lib.rs`** — remove the four flat decls (`il2cpp_ffi`, `il2cpp_config`, `type_resolve`, `dump_writer`), add `#[cfg(target_os = "windows")] mod internals;`

- [ ] **Step 5: Update `entry.rs` call sites**

```
entry.rs:9   use crate::dump_writer::build_internals_lines;  → use crate::internals::dump::build_internals_lines;
entry.rs:11  use crate::il2cpp_config::Il2CppConfig;          → use crate::internals::config::Il2CppConfig;
entry.rs:12  use crate::il2cpp_ffi::Il2CppApi;                → use crate::internals::ffi::Il2CppApi;
entry.rs:16  use crate::type_resolve::build_type_maps;        → use crate::internals::resolve::build_type_maps;
```

- [ ] **Step 6: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu` clean; `cargo test -p agent-core` → 40 passed.

- [ ] **Step 7: Checkpoint** — hand diff to user (`refactor: move il2cpp into internals/ domain`).

### Task A3: `runtime/` + `diagnostics/`

**Files:**
- Move: `wasm_host.rs` → `runtime/host.rs`; `mem_probe.rs` → `diagnostics/mem_probe.rs`
- Create: `runtime/mod.rs`, `diagnostics/mod.rs`
- Modify: `lib.rs`, call sites in `entry.rs`, cross-refs in `diagnostics/mem_probe.rs`

- [ ] **Step 1: Move**

```bash
cd crates/agent/src
mkdir -p runtime diagnostics
git mv wasm_host.rs runtime/host.rs
git mv mem_probe.rs diagnostics/mem_probe.rs
```

- [ ] **Step 2: Create the mod files**

`runtime/mod.rs`:
```rust
//! Runtime domain: the embedded WASM host bridge.
pub mod host;
```
`diagnostics/mod.rs`:
```rust
//! Diagnostics: opt-in FROG_* probes that prove domain reliability (not part of
//! any domain's runtime path).
pub mod mem_probe;
```

- [ ] **Step 3: Fix cross-refs in `diagnostics/mem_probe.rs`**

```
use crate::il2cpp_config::Il2CppConfig;     → use crate::internals::config::Il2CppConfig;
use crate::mem_write::guarded_write;        → use crate::external::write::guarded_write;
use crate::region_map::RegionMap;           → use crate::external::region_map::RegionMap;
crate::region_map::Tunables::load()         → crate::external::region_map::Tunables::load()  (both call sites)
```

- [ ] **Step 4: Update `lib.rs`** — remove `mod wasm_host;` and `mod mem_probe;`, add `#[cfg(target_os="windows")] mod runtime;` and `#[cfg(target_os="windows")] mod diagnostics;`

- [ ] **Step 5: Update `entry.rs` call sites**

```
entry.rs:158  crate::wasm_host::maybe_run_configured();              → crate::runtime::host::maybe_run_configured();
entry.rs:148  crate::mem_probe::run_staleness_probe(...)             → crate::diagnostics::mem_probe::run_staleness_probe(...)
entry.rs:155  crate::mem_probe::run_write_probe(...)                 → crate::diagnostics::mem_probe::run_write_probe(...)
```

- [ ] **Step 6: Verify** — `cargo check -p agent --target x86_64-pc-windows-gnu` clean; `cargo test -p agent-core` → 40 passed.

- [ ] **Step 7: Checkpoint** — hand diff to user (`refactor: move wasm into runtime/, probes into diagnostics/`).

### Task A4: PART A verification gate (manual, on Pixel Worlds)

**No code.** Prove the reorg changed nothing.

- [ ] **Step 1:** `./deploy.sh` (builds release, deploys to Pixel Worlds).
- [ ] **Step 2:** Launch with `WINEDLLOVERRIDES="version=n,b" FROG_MEM_PROBE=1 FROG_WRITE_PROBE=1 %command%`.
- [ ] **Step 3:** Confirm in the game folder: `internals.txt` still **zero unresolved**, same ~18.5k slots / ~7.3k classes / `td_fail=0` / unanimous `string_heap_base` in `agent.log`; `MEMORY STALENESS PROBE` RELIABLE; `MEMORY WRITE PROBE` 3/3 PASS. Identical behavior ⇒ the move was safe.
- [ ] **Step 4: Checkpoint** — report results to user. Part A done.

---

## PART B — Protocol rebuild (universal raw capture)

### Task B1: `agent_core::protocol` — `RawFrame` + `FrameRing` (pure, TDD)

**Files:**
- Create: `crates/agent-core/src/protocol.rs`
- Modify: `crates/agent-core/src/lib.rs` (add `pub mod protocol;`)

- [ ] **Step 1: Write the failing tests**

In `crates/agent-core/src/protocol.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dir: Direction, n: usize) -> RawFrame {
        RawFrame { timestamp_ms: 0, direction: dir, socket_id: 1, bytes: vec![0u8; n] }
    }

    #[test]
    fn push_within_caps_keeps_all() {
        let mut r = FrameRing::new(4, 1024);
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::S2C, 10));
        assert_eq!(r.len(), 2);
        assert_eq!(r.total_bytes(), 20);
    }

    #[test]
    fn evicts_oldest_over_frame_cap() {
        let mut r = FrameRing::new(2, 1_000_000);
        for _ in 0..3 { r.push(frame(Direction::C2S, 10)); }
        assert_eq!(r.len(), 2); // oldest dropped
    }

    #[test]
    fn evicts_oldest_over_byte_cap() {
        let mut r = FrameRing::new(100, 25);
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::C2S, 10)); // 30 > 25 -> drop until <= cap
        assert!(r.total_bytes() <= 25);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn drain_empties_and_returns_in_order() {
        let mut r = FrameRing::new(4, 1024);
        r.push(frame(Direction::C2S, 1));
        r.push(frame(Direction::S2C, 2));
        let out = r.drain();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].direction, Direction::C2S);
        assert_eq!(r.len(), 0);
        assert_eq!(r.total_bytes(), 0);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p agent-core protocol` → Expected: FAIL (module/types not defined).

- [ ] **Step 3: Implement `protocol.rs` (above the tests)**

```rust
//! Pure protocol primitives: a raw captured frame and a bounded ring. The agent
//! crate's detours produce `RawFrame`s; this ring caps memory by BOTH frame count
//! and total bytes so capture can never become a firehose. Host-testable.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server (a send).
    C2S,
    /// Server → client (a recv).
    S2C,
}

/// One captured packet: raw bytes only, no interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub timestamp_ms: u64,
    pub direction: Direction,
    pub socket_id: u64,
    pub bytes: Vec<u8>,
}

/// A capacity-bounded FIFO of frames. Pushing past either the frame cap or the
/// byte cap evicts oldest-first until both fit.
pub struct FrameRing {
    frames: VecDeque<RawFrame>,
    total_bytes: usize,
    max_frames: usize,
    max_bytes: usize,
}

impl FrameRing {
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        FrameRing { frames: VecDeque::new(), total_bytes: 0, max_frames, max_bytes }
    }

    pub fn push(&mut self, frame: RawFrame) {
        self.total_bytes += frame.bytes.len();
        self.frames.push_back(frame);
        while self.frames.len() > self.max_frames
            || (self.total_bytes > self.max_bytes && self.frames.len() > 1)
        {
            if let Some(dropped) = self.frames.pop_front() {
                self.total_bytes -= dropped.bytes.len();
            }
        }
    }

    pub fn len(&self) -> usize { self.frames.len() }
    pub fn is_empty(&self) -> bool { self.frames.is_empty() }
    pub fn total_bytes(&self) -> usize { self.total_bytes }

    /// Remove and return all frames in FIFO order (for the TCP consumer).
    pub fn drain(&mut self) -> Vec<RawFrame> {
        self.total_bytes = 0;
        self.frames.drain(..).collect()
    }
}
```

- [ ] **Step 4: Add the module** — in `crates/agent-core/src/lib.rs` add `pub mod protocol;` (alphabetically near `model`).

- [ ] **Step 5: Run to verify pass** — `cargo test -p agent-core protocol` → Expected: 4 passed. Then `cargo test -p agent-core` → Expected: 44 passed.

- [ ] **Step 6: Checkpoint** — hand diff to user (`feat: agent-core protocol RawFrame + bounded FrameRing`).

### Task B2: `protocol/` module — capture skeleton + TCP drain (no detours yet)

**Files:**
- Create: `crates/agent/src/protocol/mod.rs`, `crates/agent/src/protocol/capture.rs`
- Move: `crates/agent/src/hook.rs` → `crates/agent/src/protocol/hook.rs`
- Modify: `crates/agent/src/lib.rs`

This task stands up the ring, the global state, and the TCP server so the next task only adds detours.

- [ ] **Step 1: Move `hook.rs` into the domain**

```bash
cd crates/agent/src
mkdir -p protocol
git mv hook.rs protocol/hook.rs
```

- [ ] **Step 2: Create `protocol/mod.rs`**

```rust
//! Protocol domain: universal raw network capture. WinSock/IOCP detours copy raw
//! bytes into a bounded ring (no firehose); a TCP server streams frames out.
//! Decoding (BSON, etc.) is the consumer's job, not the backend's.

pub mod capture;
pub mod hook;

pub use capture::{install_packet_hooks, remove_packet_hooks, start_tcp_server};
```

- [ ] **Step 3: Create `protocol/capture.rs` with global state + TCP drain**

```rust
//! Raw capture: hooks copy bytes into RING; a background thread drains RING to any
//! connected TCP client on 127.0.0.1:50051. All detours are dumb-fast + validated;
//! none may panic the game thread.

use std::io::Write;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_core::protocol::{Direction, FrameRing, RawFrame};

/// Capacity caps — the firehose killer (≈64k frames or 64 MB, oldest evicted).
const MAX_FRAMES: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;

static RING: OnceLock<Mutex<FrameRing>> = OnceLock::new();

fn ring() -> &'static Mutex<FrameRing> {
    RING.get_or_init(|| Mutex::new(FrameRing::new(MAX_FRAMES, MAX_BYTES)))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Capture a raw frame. Called from detours: validate length, copy, push. Uses
/// try_lock so a contended/poisoned ring never blocks or panics a hooked thread.
pub(crate) fn capture(direction: Direction, socket_id: u64, ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 || len > MAX_BYTES {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    if let Ok(mut r) = ring().try_lock() {
        r.push(RawFrame { timestamp_ms: now_ms(), direction, socket_id, bytes });
    }
}

/// Background thread: every 50 ms, drain the ring and write each frame to the
/// connected client as a length-prefixed record: [u8 dir][u64 socket][u32 len][bytes].
pub fn start_tcp_server() {
    std::thread::spawn(|| {
        let listener = match TcpListener::bind("127.0.0.1:50051") {
            Ok(l) => { crate::paths::log("protocol: TCP raw stream on 127.0.0.1:50051"); l }
            Err(e) => { crate::paths::log(&format!("protocol: TCP bind failed: {}", e)); return; }
        };
        for stream in listener.incoming() {
            let mut stream = match stream { Ok(s) => s, Err(_) => continue };
            crate::paths::log("protocol: client connected");
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let frames = match ring().try_lock() { Ok(mut r) => r.drain(), Err(_) => continue };
                let mut buf = Vec::new();
                for f in &frames {
                    buf.push(if f.direction == Direction::C2S { 0u8 } else { 1u8 });
                    buf.extend_from_slice(&f.socket_id.to_le_bytes());
                    buf.extend_from_slice(&(f.bytes.len() as u32).to_le_bytes());
                    buf.extend_from_slice(&f.bytes);
                }
                if !buf.is_empty() && stream.write_all(&buf).is_err() {
                    crate::paths::log("protocol: client disconnected");
                    break;
                }
            }
        }
    });
}

// Detours + install/remove are added in Task B3. `capture` is defined now but
// unused until B3 wires the detours — an unused-fn warning here is expected and
// disappears in B3.
pub unsafe fn install_packet_hooks() { /* B3 */ }
pub unsafe fn remove_packet_hooks() { /* B3 */ }
```

- [ ] **Step 4: Update `lib.rs`** — remove `mod hook;`, `mod packet;`, `mod bson;`; add `#[cfg(target_os="windows")] mod protocol;`. Delete the now-orphaned files:

```bash
git rm crates/agent/src/packet.rs crates/agent/src/bson.rs
```

- [ ] **Step 5: Point `entry.rs` at the new module**

```
entry.rs:161  crate::packet::start_tcp_server();        → crate::protocol::start_tcp_server();
entry.rs:165  crate::packet::install_packet_hooks();    → crate::protocol::install_packet_hooks();
entry.rs:187  crate::packet::remove_packet_hooks();      → crate::protocol::remove_packet_hooks();
```

- [ ] **Step 6: Verify compile** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep error || echo OK` → Expected `OK` (capture/install/remove are stubs but the crate builds and the firehose code is gone).

- [ ] **Step 7: Checkpoint** — hand diff to user (`refactor: protocol/ skeleton + ring drain, delete packet.rs+bson.rs slop`).

### Task B3: WinSock + IOCP detours (broad capture, validated incoming)

**Files:**
- Modify: `crates/agent/src/protocol/capture.rs`

Implement `install_packet_hooks`/`remove_packet_hooks` and the detours. Outgoing is simple; incoming carries the fix.

- [ ] **Step 1: Add the hook handles + resolver**

At top of `capture.rs` add a global list of installed hooks and a ws2_32/kernel32 symbol resolver (mirror the old packet.rs resolution: `GetModuleHandleA("ws2_32.dll")`/`GetProcAddress` for `send`,`WSASend`,`sendto`,`WSASendTo`,`recv`,`WSARecv`,`recvfrom`,`WSARecvFrom`; `kernel32.dll` for `GetQueuedCompletionStatus`,`GetQueuedCompletionStatusEx`). Store each `Hook` in `static HOOKS: Mutex<Vec<Hook>>`.

```rust
static HOOKS: Mutex<Vec<Hook>> = Mutex::new(Vec::new());

thread_local! { static IN_HOOK: std::cell::Cell<bool> = std::cell::Cell::new(false); }

/// Run `body` only if not already inside a detour on this thread (reentrancy guard).
fn guard<R>(default: R, body: impl FnOnce() -> R) -> R {
    IN_HOOK.with(|f| {
        if f.get() { return default; }
        f.set(true);
        let r = body();
        f.set(false);
        r
    })
}
```

- [ ] **Step 2: Outgoing detour template (send-family)**

For each send-family function, the detour calls the original via its trampoline, captures the outbound buffer as `Direction::C2S`, and returns the original result. `send`/`recv` signature shown; `sendto`/`WSASendTo` mirror it (extra addr args passed through untouched), and `WSASend` reads its `WSABUF` array.

```rust
// Trampolines: each hook exposes its original fn pointer; store typed casts.
type SendFn = unsafe extern "system" fn(usize, *const u8, i32, i32) -> i32;
static ORIG_SEND: OnceLock<usize> = OnceLock::new();

unsafe extern "system" fn send_detour(s: usize, buf: *const u8, len: i32, flags: i32) -> i32 {
    let orig: SendFn = std::mem::transmute(*ORIG_SEND.get().unwrap());
    let ret = orig(s, buf, len, flags);
    guard((), || {
        if ret > 0 { capture(Direction::C2S, s as u64, buf, ret as usize); }
    });
    ret
}
```
Repeat the same shape for `sendto_detour` (signature adds `*const sockaddr, i32`), `wsasend_detour`/`wsasendto_detour` (iterate the `WSABUF` array: each `{ len: u32, buf: *const u8 }`, capture each). Keep each detour's body inside `guard`.

- [ ] **Step 3: Synchronous recv-family detour (incoming, simple path)**

```rust
type RecvFn = unsafe extern "system" fn(usize, *mut u8, i32, i32) -> i32;
static ORIG_RECV: OnceLock<usize> = OnceLock::new();

unsafe extern "system" fn recv_detour(s: usize, buf: *mut u8, len: i32, flags: i32) -> i32 {
    let orig: RecvFn = std::mem::transmute(*ORIG_RECV.get().unwrap());
    let ret = orig(s, buf, len, flags);
    guard((), || {
        if ret > 0 { capture(Direction::S2C, s as u64, buf as *const u8, ret as usize); }
    });
    ret
}
```
`recvfrom_detour` mirrors this. Data is present on return; `ret` bounds the copy — no stale-buffer risk.

- [ ] **Step 4: Async WSARecv + IOCP completion (incoming, the FIX)**

`WSARecv` may return immediately (capture now) or `WSA_IO_PENDING` (data lands later, signaled via IOCP). For the pending case, record `(overlapped_ptr → (socket, wsabuf_ptr, wsabuf_count))` in a **capped** map; drain it at `GetQueuedCompletionStatus(Ex)` when the OS reports `bytes_transferred`, validating before copying, and **removing the entry on completion** (no leak, no aliasing). Completion-routine (APC) recvs never reach GQCS — **count** them so the gap is visible.

```rust
static PENDING: OnceLock<Mutex<std::collections::HashMap<usize, (u64, usize, u32)>>> = OnceLock::new();
static APC_GAP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const MAX_PENDING: usize = 4096;

fn pending() -> &'static Mutex<std::collections::HashMap<usize, (u64, usize, u32)>> {
    PENDING.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

// In wsarecv_detour, after calling orig:
//   if ret == 0 (immediate): capture each WSABUF up to bytes_received.
//   if WSA_IO_PENDING && overlapped != null:
//       if lpCompletionRoutine != null { APC_GAP.fetch_add(1, Relaxed); }   // we will miss this one
//       else if let Ok(mut p) = pending().try_lock() {
//           if p.len() < MAX_PENDING { p.insert(overlapped as usize, (s as u64, lpBuffers as usize, dwBufferCount)); }
//       }

// In gqcs_detour / gqcsex_detour, after orig reports a completion for `overlapped` with `bytes`:
//   if let Ok(mut p) = pending().try_lock() {
//       if let Some((sock, bufs_ptr, count)) = p.remove(&(overlapped as usize)) {
//           // validate + copy up to `bytes` across the WSABUF array, S2C
//           capture_wsabufs(Direction::S2C, sock, bufs_ptr as *const WsaBuf, count, bytes);
//       }
//   }
```
`capture_wsabufs` walks the array, and for each `{len, buf}` copies `min(remaining, len)` bytes via `capture(...)` (which already null/len-guards), decrementing `remaining`. Validation = the null/len guard in `capture` + the `bytes` cap; a freed buffer can't be hit because the entry is removed exactly once on its matching completion.

- [ ] **Step 5: `install_packet_hooks` / `remove_packet_hooks`**

```rust
pub unsafe fn install_packet_hooks() {
    let ws2 = /* GetModuleHandleA("ws2_32.dll") */;
    // For each (symbol, detour, ORIG slot): resolve addr, ORIG.set(addr),
    //   let h = crate::protocol::hook::install(addr, detour as *const () as usize);
    //   HOOKS.lock().unwrap().push(h); log success/failure per symbol.
    // Same for kernel32 GQCS/GQCSEx.
    // Initialize APC_GAP logging note.
    crate::paths::log("protocol: hooks installed (send/recv family + IOCP)");
}

pub unsafe fn remove_packet_hooks() {
    if let Ok(mut hooks) = HOOKS.lock() {
        for h in hooks.drain(..) { h.remove(); }
    }
    let gap = APC_GAP.load(std::sync::atomic::Ordering::Relaxed);
    crate::paths::log(&format!("protocol: hooks removed (APC-completion recvs skipped: {})", gap));
}
```
(Use the exact `Hook`/`install` API from `protocol/hook.rs` — `install(target_addr, detour_addr) -> Hook`, `Hook::remove(self)` — as `packet.rs` did at the lines listed in the spec's reference grep.)

- [ ] **Step 6: Verify compile** — `cargo check -p agent --target x86_64-pc-windows-gnu 2>&1 | grep error || echo OK` → Expected `OK`. Fix any signature mismatches against `windows-sys` WinSock types (`WSABUF`, `WSAOVERLAPPED`, `SOCKET`).

- [ ] **Step 7: Checkpoint** — hand diff to user (`feat: universal raw capture detours + validated IOCP incoming`).

### Task B4: PART B verification gate (manual, on Pixel Worlds)

**No code.** Prove the rebuild works and the firehose is gone.

- [ ] **Step 1:** `./deploy.sh`; launch PW with the usual options (no `FROG_*` probe flags needed).
- [ ] **Step 2:** Connect `scratch/listen_packets.py` to `127.0.0.1:50051`; confirm raw frames arrive and decode (length-prefixed records).
- [ ] **Step 3 (the incoming fix):** confirm **both** directions appear — `S2C` frames present, not just `C2S`. Play for a few minutes.
- [ ] **Step 4 (firehose dead):** confirm in the game folder there is **no `activity.log`**, **no multi-hundred-MB `packets.log`**, and `agent.log` is quiet (no per-packet spam). On exit, `agent.log` shows the `APC-completion recvs skipped: N` counter.
- [ ] **Step 5 (bounded):** play a longer session; confirm the agent's memory stays bounded (the ring caps at ≈64 MB) and the game survives.
- [ ] **Step 6: Checkpoint** — report results to user. Restructure complete; foundation is clean for Spec 2.

---

## Notes for the executor

- **Never commit.** Stop at each checkpoint and let the user commit.
- After any task touching agent code, run `./deploy.sh` only when a PART verification gate calls for it (A4, B4) — not after every task.
- If a `windows-sys` WinSock type name differs from what a step assumes, trust the crate and adjust the signature; the *behavior* (call orig → capture `ret`-bounded bytes → return orig) is what matters.
- Part A is pure motion: if a PW smoke (A4) shows any behavior change, a path/re-export was missed — diff against the spec's reference grep, don't "fix" the proven domains.
