# B-2bc: Concrete Bugs + Honesty — Design

**Date:** 2026-05-30
**Branch:** `ffi-class-table` (or successor)
**Status:** approved, ready for plan-writing
**Builds on:** B-2a (Honest Dumper) — shipped + game-verified
**Audit reference:** `docs/superpowers/audit-2026-05-29-architecture-review.md`

---

## Goal

Close the four concrete production-impact bugs surfaced by the audit + B-2a verification: silent Unicode/long-name loss via `read_name`, silent IOCP capture stop via the pending-map leak, the `0xffffffff` sentinel offset disguise, and one structurally-known unhandled type code (`0x1F` TYPEDBYREF). Four surgical fixes, ~80 lines total, no architectural change.

## The bedrock principle, applied to silent failures

Across B-1 (probe-and-verify), B-2a (honest dumper), B-2bc (silent-failure elimination), the same posture holds:

> **A failure that the operator can't see is worse than one that's loud. Cap-hits, silent drops, and disguising sentinels as offsets are all flavors of the same bug — the agent is doing something wrong and not saying so. Fix the visibility, then fix the cause.**

## Non-goals (deferred to subsequent bricks)

| Item | Deferred to |
|---|---|
| `aob_scan` cache coherence | Wait for first real caller (no-op until then) |
| Anti-cheat gate periodic re-check | Policy decision; not a bug per se |
| The 10 PW-specific unhandled tcs (0x2a, 0x27, 0x2f, 0x30, 0x33, 0x36, 0x39, 0x3c, 0x44, 0xcc) | B-2d (name de-obfuscation by behavioral signature) |
| Function-pointer signature walk (FNPTR 0x1B) | Future enrichment brick |
| Native int (I 0x1A) — already covered ambiguously by existing IntPtr arm | Tier-2 if observed |
| Dead-code sweep (unused `_t` siblings, `METHOD_ATTRIBUTE_STATIC_BIT`) | Own micro-brick |
| Cross-domain transactionality docs / `i64↔u64` docs / `InvokeArg` round-trip test (former B-2c) | Folded into this brick's testing posture, not separate docs |

The honesty-docs items get folded in via the regression matrix (we test InvokeArg round-trip implicitly via test_invoke + test_hook + B-2a's regression matrix; cross-domain transactionality and i64 sign-extension are documented behaviors with no observed bugs).

---

## The four fixes

### Fix 1 — `read_name` window + filter (foundation)

**Where:** `crates/agent/src/external/region_map.rs:124-140`

**What's broken:**
- Reads exactly 64 bytes; returns `None` if no NUL found in window.
- Filter `if !(0x20..=0x7E).contains(&b) { return None; }` rejects all non-ASCII bytes including all UTF-8 multibytes.
- Cascade: `dump.rs::collect_runtime_fields` `continue`s on `None` (line ~378); `resolve.rs::typedef_name` returns `None` (line ~120); the field/type silently disappears.
- Real names >63 chars (generic instantiations like `Dictionary<KeyValuePair<List<T>, ...>>`) or any Unicode bytes silently fail.

**Fix — replace the body of `read_name`:**

```rust
pub fn read_name(&self, addr: usize) -> Option<String> {
    if !self.in_region(addr, 1024) { return None; }
    let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, 1024) };
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == 0 { end = Some(i); break; }
        if b < 0x20 { return None; }   // control bytes = garbage
    }
    let len = end?;
    if len == 0 { return None; }
    Some(String::from_utf8_lossy(&bytes[..len]).into_owned())
}
```

**Decisions locked:**
- **Window: 1024 bytes** (up from 64). Cost is one allocation per call; covers all observed Unity generic-explosion names. The bounds check still gates `in_region`.
- **Filter: reject `0x00..=0x1F` control bytes; accept everything else.** Catches obvious garbage (random binary in the high control range is rare; binary in the low control range is the strongest garbage signal). UTF-8 multibytes pass through.
- **Decode: `String::from_utf8_lossy`** — invalid UTF-8 sequences become replacement characters rather than `None`. The operator still sees something for diagnostic; garbage bytes look like `���` rather than silent omission.
- **Empty-name handling:** still returns `None` if the cstring is empty (preserves the existing "skip empty names" semantics in callers).

**Risk + mitigation:** previously-rejected fields/classes will now appear in the dump. If they were previously hidden by garbage, they'll show as `���...` strings, which is honest. B-2a's regression matrix doesn't assert exact counts; only specific entries. No test breakage expected.

**Impact:** unblocks every downstream caller that calls `map.read_name(...)` — dump.rs field names, resolve.rs type names, `is_image` shape check (unchanged behavior on `.dll` strings).

### Fix 2 — IOCP pending-map leak (user-facing capture stop)

**Where:** `crates/agent/src/protocol/capture.rs:178-183, 400-410, 450-460, 500-555` + new `closesocket` detour

**What's broken:**
- `PENDING: HashMap<usize, (u64 socket_id, usize buffers_addr, u32 count)>` keyed by `lp_overlapped` accumulates entries on WSARecv submission.
- Entries are removed only on GQCS/GQCSEx completion (lines 502, 551). If completion never returns through our hooks — timeout, socket close without final GQCS, alternate IOCP-completion mechanism — entries leak.
- At `MAX_PENDING = 4096`, new WSARecv silently fails to track (lines 406, 459 silently drop). Capture goes silent for new I/O on those sockets. **Operator sees no symptom in the log.**

**Fix — three coordinated changes in `capture.rs`:**

**a) Add `CAP_HIT_COUNT` atomic + one-shot warning:**

```rust
static CAP_HIT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
```

In every silent-drop branch (lines 406, 459) where `map.len() >= MAX_PENDING`:

```rust
let prev = CAP_HIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
if prev == 0 {
    crate::paths::log("⚠ IOCP_CAP_HIT — PENDING map full at MAX_PENDING=4096; capture degraded for new I/O");
} else if prev % 1000 == 0 {
    crate::paths::log(&format!("⚠ IOCP_CAP_HIT count={} (degraded capture)", prev + 1));
}
```

Cost: 1 atomic increment on the silent-drop branch (cheap); 1 log per 1000 hits (negligible).

**b) Add a `closesocket` detour** (5th WinSock hook). Pattern mirrors the existing `send_detour` / `recv_detour` template at the top of `capture.rs`:

```rust
type CloseSocketFn = unsafe extern "system" fn(s: usize) -> i32;
static ORIG_CLOSESOCKET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "system" fn closesocket_detour(s: usize) -> i32 {
    // Walk PENDING and remove all entries owned by this socket.
    if let Ok(mut map) = pending().try_lock() {
        let sid = s as u64;
        map.retain(|_overlapped, &mut (entry_sid, _, _)| entry_sid != sid);
    }
    let tramp = ORIG_CLOSESOCKET.get().copied().unwrap_or(0);
    if tramp != 0 {
        let f: CloseSocketFn = std::mem::transmute(tramp);
        return f(s);
    }
    -1
}
```

Hook it during `install_packet_hooks()` alongside the existing 4 detours.

**c) Existing GQCS/GQCSEx cleanup remains as the primary path** for entries whose socket DOES return through completion — the closesocket hook is the fallback. Both paths use `map.remove(...)` which is idempotent — race between closesocket-cleanup and final-GQCS-completion is safe (the GQCS branch already handles `None` gracefully — existing pattern returns without action).

**Decisions locked:**
- **Closesocket-hook + warning-counter + linear-walk.** No parallel SOCKET_INDEX (linear walk over 4096 entries is microseconds; YAGNI).
- **Logging strategy: first hit + every 1000.** Avoids log spam under sustained degraded operation; still operator-visible.
- **CAP_HIT_COUNT is process-lifetime** (no decay). If the agent is healthy except for one degraded session, the cumulative count gives an honest signal.

**Risk + mitigation:** if the closesocket detour races a final IOCP completion, `map.remove` on the GQCS side returns `None` instead of `Some(...)` and that branch (already existing) just early-returns. Tested implicitly by existing flow patterns.

**Regression signal:** long PW gameplay session shouldn't produce `IOCP_CAP_HIT` in the log (was: gradual silent degradation). Short sessions are operationally indistinguishable from pre-fix.

### Fix 3 — Sentinel offset `META` marker

**Where:** `crates/agent/src/internals/dump.rs:52-56` (the `field_line` formatter)

**What's broken:**
- 21 PW field entries show `Offset: 0xffffffff` — il2cpp's "field exists in type metadata but runtime offset not computed" sentinel (e.g., `thread_local_static_fields_index` sentinel).
- The raw u32 sentinel surfaces in the operator-facing dump as a hex address, which is meaningless to modders and looks like a bug.

**Fix — change the `field_line` formatter to recognize the sentinel:**

```rust
fn field_line(name: &str, type_name: &str, offset: u32, token: u32) -> String {
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

**Decisions locked:**
- **Single-site change in `field_line`** — all 3 call sites (FFI iterator at line ~220, memory walk at line ~231, metadata-only path at line ~281) get the format for free.
- **`META` label** — communicates intent ("this field is from metadata; runtime didn't materialize an offset"); a modder seeing `META` knows "I can't read this field at any address."
- **No row filtering** — the field is still emitted (honest dumper principle: B-2a-validated rows are preserved; only the offset display changes).
- **Field emission count invariance** — same number of lines, just one label changes. B-2a's regression assertions stay stable.

**Edge case:** `offset == 0` is a legitimate value (first field in a class or static fields). The check `== 0xffffffff` doesn't flag it. Verified.

**Risk + mitigation:** any external tool parsing `internals.txt` and asserting `Offset: 0x[0-9a-f]+` on every line breaks. Mitigation: no such tools exist today; modders eyeball; frontend plugin reads typed data from a different surface.

**Regression signal:** PW `grep -c 'Offset: 0xffffffff'` → 0 (was ~21). `grep -c 'Offset: META'` → ~21. Highrise both → 0 (Highrise had no sentinel entries).

### Fix 4 — `0x1F` TYPEDBYREF match arm (resolver completion, conservative)

**Where:** `crates/agent/src/internals/resolve.rs:323` (insert before existing `0x1C` arm)

**What's broken:**
- After B-2a's smart catch-all, 11 distinct tcs surface as `<unhandled-tc:0xNN>` on PW. Only one of them — `0x1F` (TYPEDBYREF, `System.TypedReference`) — has a documented, unambiguous CLR mapping. The other 10 are obfuscated/non-standard PW-specific values.

**Fix — single arm:**

```rust
0x1F => return "System.TypedReference".into(),
```

Insert immediately before the existing `0x1C => return "System.Object".into(),` arm.

**Decisions locked:**
- **Only add arms for tcs we KNOW.** Adding match arms for the 10 obfuscated tcs without behavioral analysis is guessing — violates `no-hardcoding-adaptive-resolution` and `challenge-assumptions-prove-it`. They stay in the smart catch-all (which is the honest operator-visible state).
- **Skip 0x1A and 0x1B** — 0x1A might already be covered by the existing `0x0F | 0x18 => "System.IntPtr"` arm (il2cpp's actual enum value needs verification); 0x1B (FNPTR) needs real signature walking, not a placeholder string. Both deferred to tier-2 if observed.

**Impact:** PW gains `System.TypedReference` resolution where the tc 0x1F appears (handful of entries). The 10 other unhandled tcs continue producing `<unhandled-tc:0xNN>`, which is the correct behavior under the honest-dumper principle.

**Regression signal:** PW `grep -c '<unhandled-tc:0x1f>'` → 0 (was N). `grep -c 'System.TypedReference'` → ~N more entries.

---

## Architecture summary

```
b2bc fix layout
────────────────────────────────────────────────────
region_map.rs::read_name
  Fix 1: window 64 → 1024
         filter 0x20..=0x7E → reject only 0x00..=0x1F
         from_utf8_lossy for non-ASCII decode

protocol/capture.rs
  Fix 2: CAP_HIT_COUNT atomic + one-shot/per-1000 warning
         closesocket_detour (5th WinSock hook)
         linear retain() on map by socket_id
         GQCS/GQCSEx primary cleanup preserved

dump.rs::field_line
  Fix 3: offset == 0xffffffff → "META" string
         single-site formatter change

resolve.rs::il2cpp_type_name_depth
  Fix 4: 0x1F => "System.TypedReference"
         single arm; obfuscated tcs stay in smart catch-all
```

**Total touched code:** ~80 lines across 4 files. Each fix individually verifiable. No new types, no new modules.

---

## Testing strategy

### Unit tests (host-runnable; agent-core)

**No new unit tests.** The four fixes touch agent-internal types that would require mock infrastructure (RegionMap for `read_name`, Win32 IOCP for the closesocket hook, Il2CppApi for `field_line`/`resolve`). Per the B-2a precedent, unit-testing those would either drift or require lifting code into agent-core (out of B-2bc scope). Live-game regression is the substitute proof.

### Live-game regression (manual; PW + Highrise)

Deploy via `./deploy.sh release`. Launch each game with the standard options. After `internals.txt` is written:

**Fix 1 — read_name:**
```bash
# PW: previously-hidden names (long generic instantiations or Unicode) now appear.
# Expect: dumped class count ≥ pre-b2bc baseline; field count ≥ baseline.
grep "dumped" "/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log" | tail -1
```

No regression criterion — purely additive. Manual eyeball spot-check: `grep "" /path/to/internals.txt | wc -l` should be ≥ pre-b2bc lines.

**Fix 2 — IOCP:**
```bash
# Short session: IOCP_CAP_HIT should NOT appear (was: silent).
# Long PW gameplay: still no IOCP_CAP_HIT (proves closesocket cleanup works).
grep "IOCP_CAP_HIT" "/path/to/agent.log"
```

Expected: no hits in routine play. If hits appear under sustained load, the closesocket hook is correctly surfacing the degradation that was previously silent.

**Fix 3 — sentinel offset:**
```bash
DUMP_PW="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"
echo "0xffffffff offsets: $(grep -c 'Offset: 0xffffffff' "$DUMP_PW")   (expect 0)"
echo "META offsets:        $(grep -c 'Offset: META' "$DUMP_PW")         (expect ~21)"
```

**Fix 4 — TypedReference:**
```bash
echo "<unhandled-tc:0x1f>: $(grep -c '<unhandled-tc:0x1f>' "$DUMP_PW")  (expect 0)"
echo "System.TypedReference: $(grep -c 'System.TypedReference' "$DUMP_PW")  (expect ≥1)"
```

### Sub-brick I / II regression (load-bearing)

`scratch/test_invoke.wasm` and `scratch/test_hook.wasm` must still PASS on PW + Highrise. They're the load-bearing proof that:
- Fix 1's relaxed filter doesn't surface garbage as real class names that confuse `find_class` calls.
- Fix 2's closesocket hook doesn't interfere with the WinSock detour cascade.
- Fix 4's TypedReference arm doesn't change anything for primitive types Math.Pow uses.

---

## What ships when B-2bc lands

- `read_name` decodes Unicode + long names; ~unknown number of previously-hidden classes/fields now appear in the dump.
- IOCP `CAP_HIT_COUNT` makes the silent capture-stop case operator-visible; `closesocket` hook makes routine session leakage impossible.
- 21 PW `0xffffffff` sentinel offsets display as `META`.
- ~N PW `<unhandled-tc:0x1f>` entries display as `System.TypedReference`.
- The 10 PW-specific obfuscated tcs continue surfacing as `<unhandled-tc:0xNN>` — honest, banked for B-2d.

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Fix 1's window 64→1024 surfaces previously-hidden garbage strings | `String::from_utf8_lossy` gives operator-visible `���` rather than silent omission; control-byte rejection catches binary garbage. |
| Fix 2's closesocket detour races with a final IOCP completion | Race is safe — both branches use `map.retain`/`map.remove` which are idempotent and gracefully handle missing keys. |
| Fix 2's closesocket detour breaks something else (e.g. WinSock close semantics) | Detour follows the existing 4-detour template exactly; calls original via trampoline; only modification is the PENDING-map cleanup before delegation. |
| Fix 3's `META` label collides with a real type called `META` | C# type names can't be unquoted bare words in this dump format — `Type: META` would have leading whitespace and the colon convention is used; no collision. |
| Fix 4 mislabels a non-TypedReference tc=0x1F entry | 0x1F is unambiguously TYPEDBYREF in `Il2CppTypeEnum`; cross-checked against il2cpp source. |
| B-2a regression suite breaks because of count changes | Fix 3 is count-invariant by design. Fixes 1, 4 may increase counts (more names resolved). B-2a's matrix doesn't assert specific counts, only that garbage entries are 0 — that stays true. |
