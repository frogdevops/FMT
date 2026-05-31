# B-2bc: Concrete Bugs + Honesty Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate four silent-failure modes from the agent — Unicode/long-name loss in `read_name`, IOCP pending-map leak under socket churn, `0xffffffff` sentinel offsets shown as garbage, and the structurally-known `0x1F` TYPEDBYREF unhandled tc.

**Architecture:** Four surgical fixes across four files. Fix 1 rewrites `read_name` to handle 1024-byte windows and Unicode. Fix 2 adds a `closesocket` detour mirroring the existing 9-WinSock-hook template, plus a `CAP_HIT_COUNT` atomic for operator visibility of map saturation. Fix 3 changes a single offset display in `field_line`. Fix 4 adds one match arm to the resolver. ~80 lines total touched.

**Tech Stack:** Rust 2021, no new deps. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (compile-only verification for region_map test).

**Spec:** `docs/superpowers/specs/2026-05-30-b2bc-concrete-bugs-and-honesty-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent/src/external/region_map.rs` | Modify | Fix 1: rewrite `read_name` body (window 64→1024; filter 0x20..=0x7E → reject 0x01..=0x1F; UTF-8 lossy decode) |
| `crates/agent/src/protocol/capture.rs` | Modify | Fix 2: `CAP_HIT_COUNT` atomic + warning logging; `closesocket_detour` + `ORIG_CLOSESOCKET`; one new `hook_sym!` invocation; on-close `retain` to clear PENDING by socket_id |
| `crates/agent/src/internals/dump.rs` | Modify | Fix 3: `field_line` formatter — recognize `0xffffffff` sentinel and emit `META` |
| `crates/agent/src/internals/resolve.rs` | Modify | Fix 4: insert `0x1F => "System.TypedReference"` match arm |

**No new files.** Each fix touches one file. No agent-core changes — these are all Windows-only paths.

---

## Task 1: Fix 1 — `read_name` window + UTF-8 decode

**Files:**
- Modify: `crates/agent/src/external/region_map.rs:122-140`

The current `read_name` reads exactly 64 bytes, returns None if any byte is outside `0x20..=0x7E`, and returns None if no NUL is found in the window. Rewriting to (a) read 1024 bytes, (b) reject only control bytes (`0x01..=0x1F`), and (c) decode with `String::from_utf8_lossy` so non-ASCII appears as replacement characters rather than silent None.

- [ ] **Step 1: Locate the function**

Open `crates/agent/src/external/region_map.rs`. Find the doc comment at line 122:
```rust
/// NUL-terminated printable-ASCII string (<= 63 chars) at `addr`, or None.
/// Bounds-checked via `in_region`; safe to call on any address.
pub fn read_name(&self, addr: usize) -> Option<String> {
```

- [ ] **Step 2: Replace the function (doc + body)**

Replace lines 122-140 (the existing doc comment through closing brace of `read_name`) with:

```rust
/// NUL-terminated string at `addr`, decoded as UTF-8 (lossy on invalid sequences).
/// Reads up to 1024 bytes; rejects any control byte (0x01..=0x1F) as a garbage
/// signal. Returns `None` if no NUL found in window, the string is empty, or
/// the address is out of mapped regions. Bounds-checked via `in_region`.
pub fn read_name(&self, addr: usize) -> Option<String> {
    if !self.in_region(addr, 1024) {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 1024) };
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == 0 {
            end = Some(i);
            break;
        }
        if b < 0x20 {
            // control bytes = garbage signal (random binary in low control range)
            return None;
        }
    }
    let len = end?;
    if len == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[..len]).into_owned())
}
```

- [ ] **Step 3: Verify `is_image` still works (read-only check)**

The function at lines 142-154 calls `read_name` to check that a string ends in `.dll`. `.dll` is pure ASCII so the new code returns it correctly. No change needed.

`grep -n "read_name" crates/agent/src/external/region_map.rs` should show:
- Line ~124 (the function definition)
- Line ~151 (`is_image` caller)
- No other internal usages.

- [ ] **Step 4: Build cross-compile + agent-core tests**

```bash
cargo build --target x86_64-pc-windows-gnu --release
cargo test -p agent-core
```

Expected: both clean. Pre-existing warnings ok.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
region_map: read_name accepts UTF-8 + 1024-byte names; reject only control bytes
```

---

## Task 2: Fix 2a — `CAP_HIT_COUNT` atomic + warning log

**Files:**
- Modify: `crates/agent/src/protocol/capture.rs` (after line 180 for the static; in `wsarecv_detour` ~line 405 and `wsarecvfrom_detour` ~line 458 for the warning emission)

Add a process-lifetime atomic counter that increments every time PENDING is full and we silently drop. Log on first hit and every 1000 hits after, so the operator sees `IOCP_CAP_HIT` when capture degrades rather than guessing.

- [ ] **Step 1: Add the static counter declaration**

Open `crates/agent/src/protocol/capture.rs`. Find the existing `const MAX_PENDING: usize = 4096;` at line 180. Add a new static declaration immediately AFTER it:

```rust
const MAX_PENDING: usize = 4096;

/// Tracks PENDING-full events (silent capture-degradation signal). One-shot log
/// on first hit and every 1000 thereafter; process-lifetime cumulative.
static CAP_HIT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
```

- [ ] **Step 2: Add a helper function for the warning emission**

Immediately after the static, add:

```rust
/// Called on every PENDING-full silent drop. Bumps the counter and logs the
/// first hit + every 1000th hit.
fn note_pending_cap_hit() {
    let prev = CAP_HIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if prev == 0 {
        crate::paths::log(
            "⚠ IOCP_CAP_HIT — PENDING map full at MAX_PENDING=4096; capture degraded for new I/O"
        );
    } else if (prev + 1) % 1000 == 0 {
        crate::paths::log(&format!(
            "⚠ IOCP_CAP_HIT count={} (degraded capture)", prev + 1
        ));
    }
}
```

- [ ] **Step 3: Call `note_pending_cap_hit` at each silent-drop site**

Find the two silent-drop sites in `wsarecv_detour` (around line 405) and `wsarecvfrom_detour` (around line 458). Both have the same pattern:

```rust
if let Ok(mut map) = pending().try_lock() {
    if map.len() < MAX_PENDING {
        map.insert(lp_overlapped as usize, (s as u64, lp_buffers as usize, dw_buffer_count));
    }
}
```

REPLACE each occurrence (there should be exactly 2) with:

```rust
if let Ok(mut map) = pending().try_lock() {
    if map.len() < MAX_PENDING {
        map.insert(lp_overlapped as usize, (s as u64, lp_buffers as usize, dw_buffer_count));
    } else {
        note_pending_cap_hit();
    }
}
```

The only change inside each is the `else { note_pending_cap_hit(); }` arm.

- [ ] **Step 4: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean. Pre-existing warnings ok.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
capture: CAP_HIT_COUNT atomic + one-shot/per-1000 log on PENDING full
```

---

## Task 3: Fix 2b — `closesocket_detour` (5th WinSock hook)

**Files:**
- Modify: `crates/agent/src/protocol/capture.rs` (new detour function near other detour bodies; new `ORIG_CLOSESOCKET` static near other `ORIG_*` statics; new `hook_sym!` invocation in `install_packet_hooks`)

Add a `closesocket` detour that walks PENDING and removes all entries owned by the socket being closed. This is the structural cleanup for the leak — when a socket goes away, its pending I/O records must go too, regardless of whether a final GQCS ever returned.

- [ ] **Step 1: Add `ORIG_CLOSESOCKET` static**

Find the block of `ORIG_*` declarations around lines 188-194:

```rust
static ORIG_SEND:        OnceLock<usize> = OnceLock::new();
static ORIG_WSASEND:     OnceLock<usize> = OnceLock::new();
static ORIG_SENDTO:      OnceLock<usize> = OnceLock::new();
static ORIG_WSASENDTO:   OnceLock<usize> = OnceLock::new();
static ORIG_RECV:        OnceLock<usize> = OnceLock::new();
static ORIG_WSARECV:     OnceLock<usize> = OnceLock::new();
static ORIG_RECVFROM:    OnceLock<usize> = OnceLock::new();
static ORIG_WSARECVFROM: OnceLock<usize> = OnceLock::new();
```

Add a new line at the end of this block:

```rust
static ORIG_CLOSESOCKET: OnceLock<usize> = OnceLock::new();
```

- [ ] **Step 2: Add the `closesocket_detour` function**

Pick a location near other detour bodies (e.g. after `recvfrom_detour`, ~line 360). Insert:

```rust
type CloseSocketFn = unsafe extern "system" fn(s: usize) -> i32;

/// closesocket detour: when a socket closes, evict all PENDING entries owned
/// by it. The GQCS/GQCSEx detours remain the primary cleanup path; this is
/// the fallback when completion never returns through our hooks (timeouts,
/// abrupt closes, alternate completion mechanisms).
unsafe extern "system" fn closesocket_detour(s: usize) -> i32 {
    // Evict all PENDING entries for this socket.
    if let Ok(mut map) = pending().try_lock() {
        let sid = s as u64;
        map.retain(|_overlapped, &mut (entry_sid, _, _)| entry_sid != sid);
    }
    // Delegate to the original closesocket via the trampoline.
    let tramp = ORIG_CLOSESOCKET.get().copied().unwrap_or(0);
    if tramp != 0 {
        let f: CloseSocketFn = std::mem::transmute(tramp);
        return f(s);
    }
    -1
}
```

- [ ] **Step 3: Install the hook in `install_packet_hooks`**

Find `install_packet_hooks` at line 573. Inside, after the existing `hook_sym!(ws2, b"WSARecvFrom\0", ...);` call (around line 607), add:

```rust
hook_sym!(ws2, b"closesocket\0",  ORIG_CLOSESOCKET, closesocket_detour);
```

(Indentation should match the surrounding hook_sym! lines.)

- [ ] **Step 4: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean.

- [ ] **Step 5: Commit (user runs)**

Suggested message:
```
capture: closesocket detour evicts PENDING entries on socket close
```

---

## Task 4: Fix 3 — `META` marker for sentinel offsets

**Files:**
- Modify: `crates/agent/src/internals/dump.rs:52-56`

Single-site change in the `field_line` formatter. The 3 call sites (lines ~220, ~231, ~281) all go through `field_line` and get the format for free.

- [ ] **Step 1: Locate `field_line`**

Open `crates/agent/src/internals/dump.rs`. Find `fn field_line` at line 52:

```rust
fn field_line(name: &str, type_name: &str, offset: u32, token: u32) -> String {
    if type_name.is_empty() {
        format!("    {}: <?> // Offset: {:#x}, Token: {:#x}", name, offset, token)
    } else {
        format!("    {}: {} // Offset: {:#x}, Token: {:#x}", name, type_name, offset, token)
    }
}
```

- [ ] **Step 2: Replace with sentinel-aware version**

Replace the function body with:

```rust
fn field_line(name: &str, type_name: &str, offset: u32, token: u32) -> String {
    // il2cpp uses 0xffffffff as the "field exists in metadata but runtime
    // offset not computed" sentinel (e.g. thread_local_static_fields_index).
    // Display as META so modders see intent rather than what looks like garbage.
    let offset_str = if offset == 0xffffffff {
        "META".to_string()
    } else {
        format!("{:#x}", offset)
    };
    if type_name.is_empty() {
        format!("    {}: <?> // Offset: {}, Token: {:#x}", name, offset_str, token)
    } else {
        format!("    {}: {} // Offset: {}, Token: {:#x}", name, type_name, offset_str, token)
    }
}
```

Note: the format specifier changes from `{:#x}` to `{}` in two places — because `offset_str` is already formatted.

- [ ] **Step 3: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
dump: field_line displays 0xffffffff sentinel offset as META
```

---

## Task 5: Fix 4 — `0x1F` TYPEDBYREF match arm

**Files:**
- Modify: `crates/agent/src/internals/resolve.rs:323` (insert before existing `0x1C` arm)

Single-line addition. The arm goes just before `0x1C => return "System.Object".into(),` to mirror surrounding match-arm style.

- [ ] **Step 1: Locate the insertion point**

Open `crates/agent/src/internals/resolve.rs`. Find the existing arm:

```rust
        0x1C => return "System.Object".into(),
```

at line ~323. The new arm goes immediately BEFORE it.

- [ ] **Step 2: Insert the new arm**

Add this line just before `0x1C`:

```rust
        0x1F => return "System.TypedReference".into(),
        0x1C => return "System.Object".into(),
```

(Indentation should match `0x1C`.)

- [ ] **Step 3: Build cross-compile**

```bash
cargo build --target x86_64-pc-windows-gnu --release
```

Expected: clean.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
resolve: handle tc=0x1F as System.TypedReference
```

---

## Task 6: Deploy + Live-Game Regression Gate

**Files:** none modified; pure verification.

The four fixes are independent and individually local. The regression criteria are unambiguous on each:

### Step A: Deploy

- [ ] **Step 1: Deploy the agent**

Run: `./deploy.sh release`
Expected: clean build + deploy to both Pixel Worlds and Highrise.

### Step B: PW dump verification (Fixes 3 + 4 + supports 1)

- [ ] **Step 2: Launch PW**

Tell user: launch Pixel Worlds with:
```
WINEDLLOVERRIDES="version=n,b" %command%
```
Wait for `=== end RAPID CLASS DUMP ===` in `agent.log`.

- [ ] **Step 3: Run the PW verification matrix**

```bash
DUMP="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"
LOG="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log"

echo "=== B-2bc PW verification ==="
echo "Fix 3 — sentinel offsets:"
echo "  0xffffffff offsets: $(grep -c 'Offset: 0xffffffff' "$DUMP")   (expect 0)"
echo "  META offsets:        $(grep -c 'Offset: META' "$DUMP")         (expect ~21)"
echo
echo "Fix 4 — TypedReference resolution:"
echo "  <unhandled-tc:0x1f>:    $(grep -c '<unhandled-tc:0x1f>' "$DUMP")  (expect 0)"
echo "  System.TypedReference:   $(grep -c 'System.TypedReference' "$DUMP")  (expect >=1)"
echo
echo "Fix 1 — read_name (no regression):"
echo "  dumped line:"
grep "dumped" "$LOG" | tail -1
echo
echo "Fix 2 — IOCP CAP_HIT (should be 0 on a short session):"
echo "  IOCP_CAP_HIT in log: $(grep -c 'IOCP_CAP_HIT' "$LOG")   (expect 0)"
```

Expected outcomes:
- `Offset: 0xffffffff` count == 0
- `Offset: META` count ≈ 21
- `<unhandled-tc:0x1f>` count == 0
- `System.TypedReference` count ≥ 1
- `dumped N classes, M fields` shows `N >= 1543` (PW B-2a baseline) and `M >= 20107` (Fix 1 may surface more)
- No `IOCP_CAP_HIT` log lines

### Step C: Highrise dump verification (no regression)

- [ ] **Step 4: Launch Highrise**

Same launch options.

- [ ] **Step 5: Run the Highrise verification matrix**

```bash
DUMP="/home/chef/.local/share/Steam/steamapps/common/Highrise/internals.txt"
LOG="/home/chef/.local/share/Steam/steamapps/common/Highrise/agent.log"

echo "=== B-2bc Highrise verification ==="
echo "Sentinel offsets (Highrise had 0 pre-B-2bc):"
echo "  0xffffffff: $(grep -c 'Offset: 0xffffffff' "$DUMP")   (expect 0)"
echo "  META:        $(grep -c 'Offset: META' "$DUMP")          (expect 0 or small)"
echo
echo "TypedReference resolution:"
echo "  <unhandled-tc:0x1f>:    $(grep -c '<unhandled-tc:0x1f>' "$DUMP")  (expect 0)"
echo "  System.TypedReference:   $(grep -c 'System.TypedReference' "$DUMP")  (expect >= 0)"
echo
echo "dumped line:"
grep "dumped" "$LOG" | tail -1
echo
echo "IOCP_CAP_HIT:"
echo "  $(grep -c 'IOCP_CAP_HIT' "$LOG")   (expect 0)"
```

Expected outcomes:
- `Offset: 0xffffffff` count == 0 (was 0 pre-B-2bc)
- `Offset: META` count == 0 (Highrise has no sentinel entries)
- `<unhandled-tc:0x1f>` count == 0
- `System.TypedReference` count ≥ 0
- `dumped N classes, M fields` shows N around 15226 (baseline) or slightly higher (Fix 1 may surface more)
- No `IOCP_CAP_HIT` log lines

### Step D: Sub-brick I (Invoke) regression

- [ ] **Step 6: Run test_invoke.wasm on Highrise**

```
WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_invoke.wasm %command%
```

Verify `agent.log` shows:
```
[wasm] invoke Math::Pow(2.0,3.0) status OK
[wasm] invoke Math::Pow returned 8.0 OK
```

- [ ] **Step 7: Run test_invoke.wasm on PW**

Same options. Same expected output.

### Step E: Sub-brick II (Hook) regression

- [ ] **Step 8: Run test_hook.wasm on PW**

```
WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_hook.wasm %command%
```

Verify `agent.log` shows the 4-line hook lifecycle ending in `[wasm] unhooked Pow returned 8.0 OK`.

- [ ] **Step 9: Hand back to user**

If all four runs match expectations (PW dump + Highrise dump + Invoke + Hook), **B-2bc is GREEN**.

Most likely diagnostic paths if anything regresses:
- `Offset: 0xffffffff` count > 0 → Fix 3's sentinel-detection isn't running (check field_line replacement)
- `IOCP_CAP_HIT` appears mid-gameplay → closesocket hook didn't install (check `install_packet_hooks` invocation order)
- Invoke/Hook returns != 8.0 → Fix 1's relaxed filter is surfacing a garbage class that confuses `find_class` (check the wasm flow's `find_class("System::Math")` returns non-zero)
- Build error mentioning `note_pending_cap_hit` → ordering of static decl + function decl wrong (Task 2 Steps 1 + 2 must precede Step 3)

---

## Self-review

**1. Spec coverage:**
- Fix 1 (read_name window + filter + UTF-8) → Task 1 ✓
- Fix 2 (IOCP leak: counter + closesocket detour) → Tasks 2 + 3 ✓
- Fix 3 (sentinel offset META) → Task 4 ✓
- Fix 4 (0x1F TypedReference) → Task 5 ✓
- Live-game regression (PW + Highrise dump + Invoke + Hook) → Task 6 ✓

**2. Placeholder scan:** No TBD/TODO/vague verbs. Each code block is complete and copy-paste ready. The closesocket hook follows the existing `hook_sym!` template exactly.

**3. Type consistency:**
- `CAP_HIT_COUNT: AtomicU64` and `note_pending_cap_hit()` named identically across Task 2 Steps 1-3.
- `ORIG_CLOSESOCKET` / `closesocket_detour` / `CloseSocketFn` consistent across Task 3.
- `offset_str` local variable consistent within Fix 3.
- `0x1F` arm format mirrors the surrounding match arm style exactly.

**Risks / deferrals noted (and justified):**
- No agent-core unit tests for the four fixes; same B-2a precedent (paths require Windows or RegionMap mocks). Live-game regression is the substitute proof.
- IOCP `CAP_HIT_COUNT` is process-lifetime cumulative (not decay-windowed); per spec decision. If a future feature needs decay, that's an additive change.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-30-b2bc-concrete-bugs-and-honesty-plan.md`. **6 tasks**, scoped to surgical edits.

Two execution options:

**1. Subagent-Driven (recommended)** — Sonnet on Tasks 1, 4, 5 (mechanical formatter / single-line / single-arm), Sonnet on Tasks 2 + 3 (CAP_HIT + closesocket are template-following). Opus reserved only for Task 6 if regression diagnosis is needed. Controller re-checks between each.

**2. Inline Execution** — execute each task in this session with checkpoints between for your review.

Which approach?
