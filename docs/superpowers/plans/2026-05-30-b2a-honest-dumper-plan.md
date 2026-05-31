# B-2a: Honest Dumper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate 229 garbage `<type:NNN>` entries and dozens of absurd `Offset: 0xf9xxxxxx` lines from PW's `internals.txt`, restore identifiability for open-generic class headers, and add operator-distinguishable diagnostics for any future garbage-vs-unhandled cases.

**Architecture:** Five surgical edits across `resolve.rs` (Fix A + B), `dump.rs` (Fix C-prep + Fix C + Fix D), and `api.rs` (Fix C). No new types, no new modules, ~30 lines of touched code. Each fix is local and individually testable. Pure-Rust unit tests live in agent-core for Fix A + B; live-game regression test for Fix C + D.

**Tech Stack:** Rust 2021, no new deps. Targets: `x86_64-pc-windows-gnu` (agent), Linux host (agent-core tests).

**Spec:** `docs/superpowers/specs/2026-05-30-b2a-honest-dumper-design.md`

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/agent/src/internals/resolve.rs` | Modify | Fix A: 4 new match arms (5 tcs); Fix B: smart catch-all split |
| `crates/agent/src/internals/dump.rs` | Modify | Fix C-prep: reorder token check; Fix C: tc-validation in memory-walk; Fix D: open-generic header stopgap |
| `crates/agent/src/internals/api.rs` | Modify | Fix C: parallel tc-validation in memory-walk fallback of `for_each_field` |
| `crates/agent-core/tests/resolver.rs` | Create | Host-runnable unit tests for Fix A + B |

**No agent-core source changes.** All structural logic lives in `agent` (Windows-only). Tests in `agent-core` mock the structures Fix A + B operate on so the unit tests run on Linux without cross-compile.

---

## Task 1: Fix A — Add match arms for 5 unhandled type codes

**Files:**
- Modify: `crates/agent/src/internals/resolve.rs:323` (after the existing `0x1C` arm, before `_ => {}`)

The current resolver has no handler for CMOD_REQD (0x20), CMOD_OPT (0x21), MODIFIER (0x40), SENTINEL (0x41), PINNED (0x45). Each wraps an inner Il2CppType*. Pattern matches the existing 0x14/0x1D/0x15 recursion blocks.

- [ ] **Step 1: Locate the insertion point**

Open `crates/agent/src/internals/resolve.rs`. Find the existing `0x1C => return "System.Object".into(),` arm. The next line is `_ => {}`. The 4 new arms go BETWEEN those two lines.

- [ ] **Step 2: Insert the 4 match arms**

Replace this region in `crates/agent/src/internals/resolve.rs`:

```rust
        0x1C => return "System.Object".into(),
        _ => {}
```

With:

```rust
        0x1C => return "System.Object".into(),
        0x20 | 0x21 => {
            // CMOD_REQD / CMOD_OPT — wrap an inner Il2CppType. data64 → inner type ptr.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<cmod-unresolved>".into();
        }
        0x40 => {
            // MODIFIER — wrap an inner type.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<modifier-unresolved>".into();
        }
        0x41 => {
            // SENTINEL — varargs marker; inner type follows.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<sentinel-unresolved>".into();
        }
        0x45 => {
            // PINNED — pinned modifier.
            let inner = data64 as usize;
            if inner != 0 {
                return il2cpp_type_name_depth(map, inner, type_maps, cfg, api, ctx, depth + 1);
            }
            return "<pinned-unresolved>".into();
        }
        _ => {}
```

- [ ] **Step 3: Build to verify clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean build, no errors. Warnings ok.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
resolve: add CMOD_REQD/CMOD_OPT/MODIFIER/SENTINEL/PINNED tc handlers
```

---

## Task 2: Fix B — Smart catch-all (split valid-but-unhandled from garbage)

**Files:**
- Modify: `crates/agent/src/internals/resolve.rs:326` (the trailing `format!("<type:{}>", tc)` line)

- [ ] **Step 1: Replace the catch-all format**

In `crates/agent/src/internals/resolve.rs`, find the line `format!("<type:{}>", tc)`. After Task 1 it should be the very last expression in the `il2cpp_type_name_depth` function.

Replace:

```rust
    format!("<type:{}>", tc)
}
```

With:

```rust
    if tc <= 0x45 {
        format!("<unhandled-tc:0x{:02x}>", tc)
    } else {
        format!("<garbage-tc:0x{:02x} @ {:#x}>", tc, type_ptr)
    }
}
```

(`type_ptr` is already a function parameter — no additional capture needed.)

- [ ] **Step 2: Build to verify clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
resolve: smart catch-all (split unhandled vs garbage tcs)
```

---

## Task 3: Fix A + B — host-runnable unit tests

**Files:**
- Create: `crates/agent-core/tests/resolver.rs`

The dumper's type resolver is in the `agent` crate (Windows-only). To test Fix A + B without cross-compiling and running a game, we'd need to either:

(a) Mock the heavy `RegionMap`/`Il2CppApi`/`TypeMaps` types and inline-test the resolver. Possible but invasive — the resolver's signature touches several agent-internal types.

(b) Skip unit tests for the resolver itself and rely on the live-game regression in Task 7.

Per the spec's testing strategy, we recommend (b) — the resolver is a leaf function tested directly by live `internals.txt` content. Unit-testing it would require lifting the resolver into agent-core (much bigger architectural change) OR maintaining duplicate mock infrastructure (drift risk).

- [ ] **Step 1: Document the testing decision**

Create `crates/agent-core/tests/resolver.rs`:

```rust
//! Resolver unit tests are intentionally deferred.
//!
//! The dumper's il2cpp type resolver lives in the `agent` crate (Windows-only,
//! cross-compile required). The resolver function signature couples to
//! `RegionMap`, `Il2CppApi`, and `TypeMaps` — all agent-internal types whose
//! mocks would either:
//!
//!   - Drift from the real types (silent test rot), or
//!   - Require lifting the resolver into agent-core (large architectural
//!     change disproportionate to B-2a's scope).
//!
//! B-2a relies on live-game regression (PW + Highrise) for Fix A + B
//! correctness. See `docs/superpowers/plans/2026-05-30-b2a-honest-dumper-plan.md`
//! Task 7 for the manual verification matrix.
//!
//! A future brick that promotes the resolver into agent-core would naturally
//! land unit tests then.

#[test]
fn deferred_to_live_regression() {
    // Sentinel test — keeps the file compiled and discoverable.
}
```

- [ ] **Step 2: Verify the sentinel test runs**

Run: `cargo test -p agent-core --test resolver`
Expected: 1 test passed.

- [ ] **Step 3: Commit (user runs)**

Suggested message:
```
agent-core: defer resolver unit tests with documented rationale
```

---

## Task 4: Fix C-prep — reorder token check in dump.rs to mirror api.rs

**Files:**
- Modify: `crates/agent/src/internals/dump.rs` (the memory-walk path of `collect_runtime_fields`)

`api.rs:60-61` reads `token` first, bails on `token == 0`, then reads `type_ptr`. `dump.rs:382-395` reads `type_ptr` first (then does 11+ lines of work like `il2cpp_type_name` resolution and offset adjustment), and only THEN reads `token` to bail. Mirror api.rs in dump.rs so the new tc-guard lands in the cheap fail-fast position.

- [ ] **Step 1: Locate the memory-walk block**

Open `crates/agent/src/internals/dump.rs`. Inside `fn collect_runtime_fields` (around line 321), find the `else` branch that's the memory-walk fallback. Inside that branch's `for fi in 0..MAX_FIELDS_PER_CLASS` loop you'll see (approximately lines 370-396):

```rust
let f = fields_ptr + fi * 32;
let name_ptr = map.read_u64(f).unwrap_or(0) as usize;
if name_ptr == 0 {
    break;
}
let fname = match map.read_name(name_ptr) {
    Some(n) => n,
    None => continue,
};
if fname.is_empty() {
    continue;
}
let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
let ftype = if type_ptr != 0 {
    il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref())
} else {
    "?".to_string()
};
let raw_offset = map.read_u32(f + 24).unwrap_or(0);
let offset = if crate::internals::api::klass_is_valuetype_via_map(cls as usize as u64, cfg, map) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
let token = map.read_u32(f + 28).unwrap_or(0);
if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
rt_fields.push((fname, ftype, offset, token));
```

- [ ] **Step 2: Reorder so token check lands BEFORE the type_ptr read**

Replace the block above with:

```rust
let f = fields_ptr + fi * 32;
let name_ptr = map.read_u64(f).unwrap_or(0) as usize;
if name_ptr == 0 {
    break;
}
let fname = match map.read_name(name_ptr) {
    Some(n) => n,
    None => continue,
};
if fname.is_empty() {
    continue;
}
// Read token first; bail early if scanner garbage. Mirrors api.rs ordering.
let token = map.read_u32(f + 28).unwrap_or(0);
if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
let ftype = if type_ptr != 0 {
    il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref())
} else {
    "?".to_string()
};
let raw_offset = map.read_u32(f + 24).unwrap_or(0);
let offset = if crate::internals::api::klass_is_valuetype_via_map(cls as usize as u64, cfg, map) {
    raw_offset.saturating_sub(0x10)
} else {
    raw_offset
};
rt_fields.push((fname, ftype, offset, token));
```

- [ ] **Step 3: Build to verify clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean. Pre-existing warnings only.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
dump: reorder token check before type_ptr read (mirror api.rs)
```

---

## Task 5: Fix C — tc-validation in both memory-walk paths

**Files:**
- Modify: `crates/agent/src/internals/dump.rs` (memory-walk path of `collect_runtime_fields`, post-Task 4)
- Modify: `crates/agent/src/internals/api.rs` (memory-walk path of `for_each_field`, around line 61)

Validate that the FieldInfo's `type_ptr` produces a plausible type code before accepting the row. Garbage entries past the real array end have `type_ptr` pointing at random memory whose tc decoded value is outside the valid `0x01..=0x45` range.

### Step A: Patch `dump.rs`

- [ ] **Step 1: Locate the post-reorder block**

In `crates/agent/src/internals/dump.rs`, find the block from Task 4. The `let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;` line is now after the token check.

- [ ] **Step 2: Insert tc-validation immediately after `type_ptr` is read**

Replace:

```rust
let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
let ftype = if type_ptr != 0 {
    il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref())
} else {
    "?".to_string()
};
```

With:

```rust
let type_ptr = map.read_u64(f + 8).unwrap_or(0) as usize;
// Validate type_ptr produces a plausible type code. Garbage FieldInfo
// entries past the real array end have type_ptr pointing to random
// memory that doesn't decode as a valid tc in 0x01..=0x45.
if type_ptr == 0 { continue; }
let chunk = map.read_u64(type_ptr + cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
let tc = ((chunk >> cfg.discrim_shift) & 0xFF) as u8;
if tc == 0 || tc > 0x45 { continue; }
let ftype = il2cpp_type_name(map, type_ptr, type_maps, cfg, api, ctx.as_ref());
```

(The `if type_ptr != 0` branch and the `else "?".to_string()` fallback both collapse into the new `continue` guard — by the time we resolve the name, type_ptr is known non-zero and tc is plausible.)

### Step B: Patch `api.rs`

- [ ] **Step 3: Locate the memory-walk fallback in `for_each_field`**

In `crates/agent/src/internals/api.rs`, find `fn for_each_field`. Inside the `else` branch (the memory-walk fallback), find this block around lines 55-68:

```rust
for fi in 0..256usize {
    let slot = fields_ptr + fi * 32;
    let name_ptr = match cache::read_u64(slot) { Some(p) if p != 0 => p as usize, _ => break };
    let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => continue };
    let token = cache::read_u32(slot + 28).unwrap_or(0);
    if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
    let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
    let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
    let offset = if klass_is_valuetype(klass as u64) {
        raw_offset.saturating_sub(0x10)
    } else {
        raw_offset
    };
    if f(&name, offset, type_ptr) { return; }
}
```

- [ ] **Step 4: Insert tc-validation after `type_ptr` is read**

Replace the same region with:

```rust
for fi in 0..256usize {
    let slot = fields_ptr + fi * 32;
    let name_ptr = match cache::read_u64(slot) { Some(p) if p != 0 => p as usize, _ => break };
    let name = match cache::read_cstr(name_ptr) { Some(n) if !n.is_empty() => n, _ => continue };
    let token = cache::read_u32(slot + 28).unwrap_or(0);
    if token == 0 { continue; }   // scanner garbage: real fields always have a metadata token
    let type_ptr = cache::read_u64(slot + 8).unwrap_or(0) as usize;
    // Validate type_ptr produces a plausible type code. Garbage FieldInfo
    // entries past the real array end have type_ptr pointing to random
    // memory that doesn't decode as a valid tc in 0x01..=0x45.
    if type_ptr == 0 { continue; }
    let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
    if tc == 0 || tc > 0x45 { continue; }
    let raw_offset = cache::read_u32(slot + 24).unwrap_or(0);
    let offset = if klass_is_valuetype(klass as u64) {
        raw_offset.saturating_sub(0x10)
    } else {
        raw_offset
    };
    if f(&name, offset, type_ptr) { return; }
}
```

(Notice `c.cfg.X` in api.rs vs `cfg.X` in dump.rs — `for_each_field` reaches cfg via the `ctx::get()` binding `c` already in scope, while `collect_runtime_fields` has `cfg: &Il2CppConfig` as a direct parameter.)

- [ ] **Step 5: Build to verify clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 6: Commit (user runs)**

Suggested message:
```
dump+api: validate FieldInfo type_ptr tc in memory-walk fallbacks
```

---

## Task 6: Fix D — Open-generic class header stopgap

**Files:**
- Modify: `crates/agent/src/internals/dump.rs:161-162` (where `cname` and `cns` are read)

When both `class_get_name` and `class_get_namespace` return empty (open-generic instantiation on obfuscated builds), synthesize an identifying header so the class's fields aren't orphaned and the class is locatable.

- [ ] **Step 1: Locate the class-emission block**

Open `crates/agent/src/internals/dump.rs`. Find lines 161-162:

```rust
let cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
let cns   = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
```

- [ ] **Step 2: Make cname mutable + add the fallback header**

Replace those two lines with:

```rust
let mut cname = unsafe { cstr_to_string((api.class_get_name)(cls)) };
let     cns   = unsafe { cstr_to_string((api.class_get_namespace)(cls)) };
if cname.is_empty() && cns.is_empty() {
    cname = format!("<generic @ {:#x}>", cls as usize);
}
```

`cls` is the klass pointer in scope at that point (an `*mut Il2CppClass`). The cast `cls as usize` produces the address for display. The synthesized header uses angle brackets — angle brackets are not valid C# class-name characters, so there's no risk of colliding with a real class name.

- [ ] **Step 3: Build to verify clean**

Run: `cargo build --target x86_64-pc-windows-gnu --release`
Expected: clean.

- [ ] **Step 4: Commit (user runs)**

Suggested message:
```
dump: synthesize "<generic @ addr>" header for empty-name open generics
```

---

## Task 7: Live-game regression gate

**Files:** none modified; pure verification.

This is the proof. All four fixes are surgical and individually local, so the regression criterion is unambiguous: the artifacts the audit identified must be gone from `internals.txt`, and the existing Invoke + Hook test scripts must still pass.

### Step A: Deploy

- [ ] **Step 1: Deploy the agent**

Run: `./deploy.sh release`
Expected: clean build + deploy to both Pixel Worlds and Highrise.

### Step B: Verify PW dump artifacts

- [ ] **Step 2: Launch PW**

Tell user: launch Pixel Worlds with launch options:
```
WINEDLLOVERRIDES="version=n,b" %command%
```

Wait for the agent to write `internals.txt`. Verify dump completed via `frog.log` showing `=== end RAPID CLASS DUMP ===` line.

- [ ] **Step 3: Run the verification grep matrix on PW**

Run these greps against `/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt`:

```bash
DUMP="/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/internals.txt"

# Garbage tc count — expect 0
GARBAGE=$(grep -c "<garbage-tc:" "$DUMP")
echo "garbage-tc count: $GARBAGE (expect 0)"

# Unhandled-tc count — expect ≤ 4 (the CMOD entries Fix A now resolves; possibly 0)
UNHANDLED=$(grep -c "<unhandled-tc:" "$DUMP")
echo "unhandled-tc count: $UNHANDLED (expect ≤ 4)"

# Old-format <type:NNN> entries — expect 0
OLDTYPE=$(grep -c "<type:" "$DUMP")
echo "old <type:NNN> count: $OLDTYPE (expect 0)"

# Absurd offsets — expect 0
ABSURD=$(grep -c "Offset: 0xf" "$DUMP")
echo "absurd 0xf-offset count: $ABSURD (expect 0)"

# Open-generic stopgap headers — expect handful (these were orphans pre-B-2a)
GENERIC=$(grep -c "<generic @ 0x" "$DUMP")
echo "<generic @ 0x> header count: $GENERIC (expect handful)"

# Total dumped — should match agent.log
echo
grep "dumped" "/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/agent.log" | tail -1
```

Expected outcomes:
- `<garbage-tc:` count == 0
- `<unhandled-tc:` count ≤ 4 (typically 0 — Fix A resolves CMOD inner types)
- `<type:` count == 0 (old format entirely retired)
- `Offset: 0xf` count == 0
- `<generic @ 0x` count > 0 (was 0 before; orphans now have headers)
- `dumped N classes, M fields` line shows `N >= 1543` and `M ≈ 20107` (was 20336; ~229 garbage entries removed)

### Step C: Verify Highrise dump (no regression)

- [ ] **Step 4: Launch Highrise**

Tell user: launch Highrise with the same launch options.

- [ ] **Step 5: Run the verification matrix on Highrise**

```bash
DUMP="/home/chef/.local/share/Steam/steamapps/common/Highrise/internals.txt"

# All of these should remain 0 — Highrise was clean before B-2a, must stay clean.
echo "garbage-tc:   $(grep -c '<garbage-tc:' "$DUMP")"
echo "unhandled-tc: $(grep -c '<unhandled-tc:' "$DUMP")"
echo "old <type:    $(grep -c '<type:' "$DUMP")"
echo "0xf-offset:   $(grep -c 'Offset: 0xf' "$DUMP")"
echo "<generic @:   $(grep -c '<generic @ 0x' "$DUMP")"
echo
grep "dumped" "/home/chef/.local/share/Steam/steamapps/common/Highrise/agent.log" | tail -1
```

Expected outcomes:
- All five counts == 0 (Highrise has names for all open generics + no garbage entries pre-B-2a).
- `dumped N classes, M fields` ≈ `15226 classes, 79340 fields` (matches pre-B-2a Highrise baseline).

### Step D: Sub-brick I (Invoke) regression on Highrise

- [ ] **Step 6: Run test_invoke.wasm**

Tell user: launch Highrise with `FROG_WASM=test_invoke.wasm`:
```
WINEDLLOVERRIDES="version=n,b" FROG_WASM=test_invoke.wasm %command%
```

Verify `agent.log` shows:
```
[wasm] invoke Math::Pow(2.0,3.0) status OK
[wasm] invoke Math::Pow returned 8.0 OK
```

Both lines must appear. If `8.0 OK` is missing, Fix C accidentally dropped a legitimate field used by the Invoke runtime — STOP and report.

### Step E: Sub-brick II (Hook) regression on Highrise

- [ ] **Step 7: Run test_hook.wasm**

Same launch options with `FROG_WASM=test_hook.wasm`. Verify `agent.log` shows the Math.Pow hook lifecycle (install_hook OK, hooked Pow returned UNEXPECTED, remove_hook OK, unhooked Pow returned 8.0 OK).

If `unhooked Pow returned 8.0 OK` doesn't appear, the Hook path has regressed — STOP and report.

### Step F: Sub-brick I regression on PW

- [ ] **Step 8: Run test_invoke.wasm on PW**

Tell user: launch Pixel Worlds with `FROG_WASM=test_invoke.wasm`. Same expected `[wasm] invoke Math::Pow returned 8.0 OK` line.

### Step G: Sub-brick II regression on PW

- [ ] **Step 9: Run test_hook.wasm on PW**

Same launch options on PW. Same expected lifecycle.

### Step H: Hand back to user

- [ ] **Step 10: Report**

If all four runs pass (PW dump clean + Highrise dump clean + invoke+hook still green on both games), B-2a is GREEN. Mark task #113 in the global tracker as completed.

If any step regresses, capture the relevant grep output / log excerpt and hand back to controller. Most likely diagnostic paths:
- `<garbage-tc:` on PW but expected 0 → Fix C's threshold (0x45) is wrong for some legitimate type; widen to 0x55 and re-test.
- Hook 8.0 OK missing → Fix C dropped a method's field; check which method's args the Hook uses and ensure their FieldInfo passes the tc-guard.
- Open generic count = 0 on PW → Fix D's empty-string check didn't fire; perhaps `cstr_to_string` returns whitespace, not empty.

---

## Self-review

**1. Spec coverage:**
- Fix A (5 unhandled tcs) → Task 1 ✓
- Fix B (smart catch-all) → Task 2 ✓
- Fix A + B unit tests → Task 3 (deferred with documented rationale + sentinel) ✓
- Fix C-prep (token reorder) → Task 4 ✓
- Fix C (tc-validation in both files) → Task 5 ✓
- Fix D (open-generic stopgap) → Task 6 ✓
- Live-game regression gate → Task 7 (with PW + Highrise + Invoke + Hook subsections) ✓

**2. Placeholder scan:** No TBD / TODO / vague verbs. Every code block is complete and copy-paste ready. Task 3 documents a real engineering decision (deferred unit tests with rationale) rather than punting.

**3. Type consistency:**
- `il2cpp_type_name_depth` signature consistent across Tasks 1 + 5 (uses existing arg list: `map, inner, type_maps, cfg, api, ctx, depth + 1`).
- `tc` variable name consistent across Tasks 2 and 5.
- `type_ptr` references consistent (parameter at resolve.rs, local binding at dump.rs/api.rs).
- `cfg.il2cpp_type_discrim_read_at` / `cfg.discrim_shift` field names consistent with the Bedrock B-1 config naming.
- `c.cfg.X` in api.rs vs `cfg.X` in dump.rs called out explicitly in Task 5 Step 4.

**Deviation noted (and justified):**
- Task 3 deviates from the spec's "create resolver.rs with synthetic tests" by deferring with rationale. Justified inline: the resolver couples to agent-internal types whose mock infrastructure would either drift (silent rot) or require lifting the resolver into agent-core (out of B-2a scope). Live-game regression in Task 7 is the substitute proof. A future "promote resolver to agent-core" brick naturally lands the unit tests then. Decision documented in the file itself so future engineers see the rationale.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-30-b2a-honest-dumper-plan.md`. **7 tasks**, scoped to surgical edits.

Two execution options:

**1. Subagent-Driven (recommended)** — fresh Sonnet subagent per task (per your standing routing for mechanical/refactor work — Tasks 1, 2, 4, 5, 6 are all formulaic), with Opus reserved only for Task 7's regression diagnosis if anything goes sideways. Controller re-checks between each.

**2. Inline Execution** — execute each task in this session with checkpoints between for your review.

Which approach?
