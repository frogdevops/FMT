# Runtime Metadata Memory-Scan (agent) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the metadata parser *live* — at runtime, scan our own process memory for the decrypted `global-metadata` blob, auto-detect its version, parse it, and emit a `Dump`. No il2cpp functions are called → no crashes. This is the piece that turns "we have a parser" into "we observe a running (obfuscated) game."

**Architecture:** The candidate-selection logic (`find_and_parse`) and version lookup (`layout_for_version`) are pure functions in `agent-core` (host-tested). The Windows-only `agent` crate adds a `VirtualQuery` region walker that hands each committed+readable region's bytes to `find_and_parse`. The result (`Dump`) flows into the existing `format_dump` → `internals.txt` and the existing TCP server — unchanged.

**Tech Stack:** Rust, `agent-core` (no new deps), `windows-sys` (`Win32_System_Memory`, already enabled).

**Dependency:** to find anything on a *real* game, `layout_for_version` must return a real layout — that's Task 5 of the parser plan (transcribe `LAYOUT_V29` from Il2CppDumper for a target version). This plan builds the machinery (testable now) and goes live once that data exists.

**Builds on:** `2026-05-23-metadata-parser-plan.md` (parser core, `find_magic_offsets`, `parse_metadata`, candidate validation — all done).

---

### Task 1: `find_and_parse` (agent-core, pure, TDD)

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`
- Test: existing `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:
```rust
    #[test]
    fn find_and_parse_locates_embedded_blob() {
        // A region: 16 bytes of padding, then a valid metadata blob.
        let mut region = vec![0u8; 16];
        region.extend_from_slice(&test_blob());
        // test_blob's header version (byte 4) is 29 → map it to TEST_LAYOUT.
        let dump = find_and_parse(&region, |v| if v == 29 { Some(TEST_LAYOUT) } else { None }).unwrap();
        assert_eq!(dump.classes.len(), 1);
        assert_eq!(dump.classes[0].name, "Player");
    }

    #[test]
    fn find_and_parse_returns_none_without_magic() {
        let region = vec![0u8; 256];
        assert!(find_and_parse(&region, |_| Some(TEST_LAYOUT)).is_none());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p agent-core find_and_parse`
Expected: FAIL — `cannot find function 'find_and_parse'`.

- [ ] **Step 3: Implement it**

Add to `metadata.rs` (above the test module):
```rust
/// Scan `bytes` for metadata-magic candidates and return the first that parses
/// into a non-empty `Dump`. `layout_for` maps a metadata version (the u32 at the
/// candidate's byte offset +4) to its layout; candidates whose version is
/// unsupported or whose blob fails validation are skipped.
pub fn find_and_parse(
    bytes: &[u8],
    layout_for: impl Fn(u32) -> Option<MetadataLayout>,
) -> Option<Dump> {
    for off in find_magic_offsets(bytes) {
        let version = match read_u32(bytes, off + 4) {
            Some(v) => v,
            None => continue,
        };
        let layout = match layout_for(version) {
            Some(l) => l,
            None => continue,
        };
        if let Some(dump) = parse_metadata(&bytes[off..], &layout) {
            if !dump.classes.is_empty() {
                return Some(dump);
            }
        }
    }
    None
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p agent-core`
Expected: PASS (16 total).

- [ ] **Step 5: Commit** — STOP for the human to commit.

---

### Task 2: `layout_for_version` (agent-core)

**Files:**
- Modify: `crates/agent-core/src/metadata.rs`
- Test: existing test module

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:
```rust
    #[test]
    fn layout_for_unknown_version_is_none() {
        assert!(layout_for_version(9999).is_none());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p agent-core layout_for_unknown`
Expected: FAIL — `cannot find function 'layout_for_version'`.

- [ ] **Step 3: Implement the lookup shell**

Add to `metadata.rs` (above the test module):
```rust
/// Map a metadata version number to its byte layout. Real layouts are filled
/// in by the parser plan's Task 5 (transcribed from Il2CppDumper); until then
/// this returns `None`, so the scanner simply finds nothing rather than
/// misparsing. Add `29 => Some(LAYOUT_V29),` etc. as each layout lands.
pub fn layout_for_version(_version: u32) -> Option<MetadataLayout> {
    match _version {
        // 29 => Some(LAYOUT_V29),
        _ => None,
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p agent-core`
Expected: PASS (17 total).

- [ ] **Step 5: Commit** — STOP for the human to commit.

---

### Task 3: Windows memory-region walker (agent, glue)

> `#[cfg(target_os = "windows")]`-only; verified by cross-compile. The runtime correctness is verified at the live run (Task 5).

**Files:**
- Create: `crates/agent/src/mem_scan.rs`
- Modify: `crates/agent/src/lib.rs` (add `#[cfg(target_os = "windows")] mod mem_scan;`)

- [ ] **Step 1: Implement the walker**

Add to `crates/agent/src/lib.rs` (with the other cfg-gated mods):
```rust
#[cfg(target_os = "windows")]
mod mem_scan;
```

Create `crates/agent/src/mem_scan.rs`:
```rust
use std::ffi::c_void;

use agent_core::metadata::{find_and_parse, layout_for_version};
use agent_core::model::Dump;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

fn is_readable(protect: u32) -> bool {
    const MASK: u32 = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    (protect & MASK) != 0 && (protect & PAGE_GUARD) == 0
}

/// Walk this process's committed, readable memory regions looking for the
/// decrypted global-metadata blob. Returns the first region that parses into a
/// non-empty `Dump`. Read-only; never calls into the game.
pub fn scan_process_for_metadata() -> Option<Dump> {
    unsafe {
        let mut addr: usize = 0;
        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let n = VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if n == 0 {
                break; // end of address space
            }
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            let next = base.saturating_add(size);

            if mbi.State == MEM_COMMIT && is_readable(mbi.Protect) && size >= 8 {
                let slice = std::slice::from_raw_parts(base as *const u8, size);
                if let Some(dump) = find_and_parse(slice, layout_for_version) {
                    return Some(dump);
                }
            }

            if next <= addr {
                break; // no forward progress / overflow guard
            }
            addr = next;
        }
    }
    None
}
```

- [ ] **Step 2: Verify host build + cross-compile**

Run:
```bash
cargo build
cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -5
```
Expected: both succeed (host cfg-gates `mem_scan` out; cross-compile builds it). Fix any `windows-sys` 0.59 type mismatch minimally if it arises (the memory constants are the same ones already used in `il2cpp_ffi.rs`).

- [ ] **Step 3: Commit** — STOP for the human to commit.

---

### Task 4: Wire the scan into the worker (agent, glue)

> Adds a metadata-scan path to the agent's worker, feeding the existing `internals.txt` writer and TCP server. `#[cfg(target_os = "windows")]`. Verified by cross-compile.

**Files:**
- Modify: `crates/agent/src/entry.rs`

- [ ] **Step 1: Use the scan after the respect gate**

In `crates/agent/src/entry.rs`, add near the other `use crate::...` lines:
```rust
use crate::mem_scan::scan_process_for_metadata;
```

In the `worker` function, AFTER `log("respect gate passed");` and BEFORE the existing il2cpp-resolution block, insert a metadata-scan attempt. (Keep the existing il2cpp path as a fallback for clean games; the scan is the obfuscation-safe primary.) Insert:
```rust
    // Primary path: read the decrypted metadata directly from memory.
    // Read-only, crash-safe, works on obfuscated games. Returns None until a
    // real layout is registered (parser plan Task 5) or if no blob is present.
    log("scanning memory for global-metadata...");
    if let Some(dump) = scan_process_for_metadata() {
        log(&format!(
            "metadata scan: {} classes, {} fields",
            dump.class_count(),
            dump.total_fields()
        ));
        let text = format_dump(&dump);
        match write_text(&dump_path(), &text) {
            Ok(()) => log("wrote internals.txt (from metadata scan)"),
            Err(e) => log(&format!("failed to write internals.txt: {}", e)),
        }
        // (Optionally also serve `dump` over the TCP server here in a later step.)
        return 0;
    }
    log("metadata scan found nothing; falling back to il2cpp API path");
```

`dump.class_count()` / `dump.total_fields()` already exist on `Dump`; `format_dump`, `write_text`, `dump_path` are already imported/defined in `entry.rs`.

- [ ] **Step 2: Verify host build + cross-compile**

Run:
```bash
cargo build
cargo build -p agent --target x86_64-pc-windows-gnu 2>&1 | tail -5
```
Expected: both clean.

- [ ] **Step 3: Commit** — STOP for the human to commit.

---

### Task 5: Live validation (needs a real layout)

> This is the go-live step and depends on parser-plan Task 5 supplying a real `LAYOUT_V29` (or other version) and `layout_for_version` routing to it.

- [ ] **Step 1: Register a real layout**

Implement parser-plan Task 5 (transcribe `LAYOUT_V29` from Il2CppDumper), then enable the route in `layout_for_version`: `29 => Some(LAYOUT_V29),`.

- [ ] **Step 2: Cross-compile release**

Run: `cargo build -p agent --target x86_64-pc-windows-gnu --release` → `target/.../release/agent.dll`.

- [ ] **Step 3: Run against a target game (manual)**

Inject into a **single-player** Il2Cpp game of the targeted version (read its metadata version from byte 4 first). Check `agent.log` for `metadata scan: N classes, M fields` and `internals.txt` for real class names. No crash regardless of outcome (read-only).

- [ ] **Step 4: Commit notes** — STOP for the human.

---

## Self-Review

- **Spec coverage:** memory scan for the blob (design §5) ✓, runtime version detection (`find_and_parse` + `layout_for_version`) ✓, candidate validation reused from the parser (via `parse_metadata`) ✓, feeds existing `Dump`/`format_dump`/`internals.txt`/TCP ✓, read-only/crash-safe (no game calls) ✓, respect gate preserved (scan returns None → falls back/declines, never crashes) ✓.
- **Placeholders:** none — `layout_for_version` intentionally returns `None` until real layouts land (documented), which is correct behavior, not a stub gap.
- **Type consistency:** `find_and_parse(bytes, layout_for)`, `layout_for_version`, `scan_process_for_metadata() -> Option<Dump>`, and the `entry.rs` use of `Dump::class_count/total_fields` + `format_dump`/`write_text` all match existing signatures.
- **Limitation noted:** `find_and_parse` parses from a candidate to the *region* end, so a blob spanning multiple memory regions could be missed (rare — metadata is usually one allocation). Extending across adjacent committed regions is a future refinement.

## Execution Handoff

Two options (same as before):
1. **Subagent-Driven (recommended)** — fresh subagent per task on Opus, I re-verify, no commits (stop at each commit point).
2. **Inline** — batches with checkpoints.

Tasks 1–2 are pure/host-testable and can be done now. Tasks 3–4 cross-compile-verify now but only *find* something once a real layout is registered (parser-plan Task 5). Task 5 here is the live run.
