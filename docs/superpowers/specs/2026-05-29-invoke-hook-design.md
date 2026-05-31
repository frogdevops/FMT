# Method Invoke + Method Hook — Design

**Date:** 2026-05-29
**Branch:** `ffi-class-table`
**Status:** approved, ready for plan-writing (two sub-bricks)
**Builds on:** `2026-05-28-trait-spine-design.md`

---

## Goal

Land the **two highest-priority Spec-2 capabilities** so WASM scripts can call managed methods (`Player.TakeDamage(50)`) and detour them (observe + mutate args, optionally call original, mutate return). Both build on the same marshalling layer, share the same `MethodPtr` spine handle, and share an FFI substrate (`runtime_invoke` + 3 small forwarders).

This is **two sub-bricks** ships with a PW gate between them: Invoke ships first (smaller, no asm) and gives an early `il2cpp.invoke(...)` capability; Hook ships second (asm-heavy) and reuses Invoke's marshalling.

## Non-goals (v1)

- **Multi-hook chains** — one Frog hook per method. `install_hook` on an already-hooked method returns `HookError::AlreadyHooked`. Chains need an ordering policy; deferred.
- **Generic methods** (`List<T>::Add(T)`) — requires per-call-site generic instantiation; deferred to tier-2 add.
- **Async/awaitable method invocation** — synchronous from the calling thread is sufficient.
- **No change to existing script-facing surface** — `mem.*` and existing `il2cpp.*` (find_class/find_method/field_addr/etc.) keep working untouched.
- **No FFI changes to the dumper path** — `Il2CppApi` gains 4 new slots; existing 12 slots untouched.

## Prerequisites (separate small brick, lands first)

| # | Item | Rationale |
|---|---|---|
| P-1 | Value-type `FieldInfo` offset fix (task #72) | Full marshalling needs `Struct(Vec<u8>)` to read correct byte windows. The fix lands in one place (`dump.rs` + `internals/api.rs` FieldInfo readers — detect valuetype via `klass.flags & VALUETYPE_BIT`, subtract `sizeof(Il2CppObject) = 0x10` from `FieldInfo::offset`). |
| P-2 | Move `protocol/hook.rs` → `crates/agent/src/inline_detour.rs` | Domain-agnostic x86_64 patcher used by two domains (protocol/capture + the new internals/hook_runtime). Move + update 1–2 import paths. Zero behavior change. |

Both prerequisites are small (~100–150 LOC total). They MUST land before Sub-brick I starts.

---

## Status code ranges (partitioned across the spine ABI)

| Range | Owner | Source |
|---|---|---|
| `0` | OK | every host fn |
| `1` | CHANGED (mem write_if differed) | mem |
| `-1..-5` | `MemError` | mem |
| `-100..-106` | `InvokeError` | invoke |
| `-200..-205` | `HookError` | hook |
| `-1000..` | reserved future | — |

Script-side classification: `if (status < -200) hook-error; else if (status < -100) invoke-error; else if (status < 0) mem-error;`.

---

# Section 1 — Thunk emitter + universal shim (asm substrate)

Everything in the Hook sub-brick sits on these three asm pieces. Pure x86_64 Windows; host-testable only as compilation. Lives at `crates/agent/src/internals/hook_runtime/{shim, replay, thunks, regargs}.rs`.

### `RegArgs` POD

```rust
#[repr(C)]
pub struct RegArgs {
    pub method_id:    u64,           // [rbp - 8]   — from r10 (thunk-supplied)
    pub int_args:     [u64; 4],      // [rbp - 16..-40]  — RCX, RDX, R8, R9
    pub float_args:   [f64; 4],      // [rbp - 48..-72]  — XMM0..XMM3 (f32 zero-extended)
    pub stack_args:   *const u64,    // [rbp - 80]  — ptr to caller's stack-args region
    pub ret_int:      u64,           // captured RAX on return path
    pub ret_float:    f64,           // captured XMM0 on return path
}
```

### Universal shim (`universal_shim`, inline asm)

Entry point of every hooked method. On entry: `R10 = method_id` (from thunk), `RCX/RDX/R8/R9 + XMM0..3 = method args`, return addr at `[rsp]`.

```text
push rbp; mov rbp, rsp
sub rsp, sizeof(RegArgs) + 16              ; alloca + alignment
mov [rbp - 8], r10                          ; method_id
mov [rbp - 16], rcx                         ; int args 1..4
mov [rbp - 24], rdx
mov [rbp - 32], r8
mov [rbp - 40], r9
movsd [rbp - 48], xmm0                      ; float args 1..4
movsd [rbp - 56], xmm1
movsd [rbp - 64], xmm2
movsd [rbp - 72], xmm3
lea r11, [rbp + 16]                         ; caller's stack-args region
mov [rbp - 80], r11
mov rcx, [rbp - 8]                          ; rcx = method_id
lea rdx, [rbp - 80]                         ; rdx = &RegArgs
sub rsp, 32                                 ; shadow space
call dispatch_rust                          ; returns; result in RAX (int) / XMM0 (float)
add rsp, 32
mov rsp, rbp; pop rbp
ret
```

Touches RAX/RCX/RDX/R8-R11/XMM0-XMM3 only — all caller-saved per Win64 ABI. RBX/RBP/R12-R15/XMM6-XMM15 preserved by frame discipline.

### `call_trampoline_with_regargs` (symmetric replay, inline asm)

Used by `il2cpp::call_original` to invoke the trampoline (`inline_detour::Hook::trampoline`) with a `RegArgs` buffer.

```text
; rcx = trampoline ptr, rdx = &mut RegArgs
mov rax, rcx                                ; save trampoline ptr
mov rcx, [rdx + 8]                          ; rehydrate int args
mov r8,  [rdx + 16]
; etc., load XMM0..3 from rdx + 32..56
sub rsp, <alignment + stack_arg_bytes>
rep movsq                                   ; copy stack args from RegArgs to live stack
call rax                                    ; runs stolen-bytes prologue + jmp back to target+stolen_len
mov [rdx + ret_int],   rax                  ; capture return
movsd [rdx + ret_flt], xmm0
ret
```

### Thunk emitter (`thunks.rs`)

At hook-install time: `slab.alloc()` returns a 24-byte slot in a `PAGE_EXECUTE_READWRITE` page (170 thunks per 4 KiB page). Emit:

```text
49 BA <method_id u64>   ; mov r10, <method_id>             (10 bytes)
49 BB <shim_addr u64>   ; mov r11, <universal_shim addr>   (10 bytes)
41 FF E3                ; jmp r11                          (3 bytes; 1 byte padding)
```

Then `inline_detour::install(method.as_u64() as usize, thunk_addr)` does the inline patch.

Slab is a freelist; `remove_hook` returns the slot.

**Constraints documented at the top of `shim.rs`:**
- No nonvolatile clobbering (frame discipline preserves callee-saved regs).
- Stack args (arg5+) captured as a pointer, NOT copied — marshal reads them lazily by offset.
- Variadic managed methods are not a thing in il2cpp; param count is known per `MethodInfo`.

---

# Section 2 — Marshalling layer + `InvokeArg` + `InvokeError`

The bridge between the universal shim's `RegArgs` (and `il2cpp_runtime_invoke`'s `void**` args) and the script-visible `InvokeArg` enum.

### 2a — Method-signature reading (probed, not hardcoded)

We have `method_name_off = 0x18`, `method_klass_off = 0x20`, `method_param_count_off = 0x52` already. **Five new offsets are derived structurally** by a Phase 0 probe pass:

```rust
// agent/src/internals/config.rs — new fields
pub method_parameters_off:  usize,   // probed: ptr to ParameterInfo[]
pub method_return_type_off: usize,   // probed: ptr to Il2CppType
pub method_flags_off:       usize,   // probed: u32 — METHOD_ATTRIBUTE_STATIC = 0x0010
pub param_info_size:        usize,   // probed: stride between ParameterInfo entries
pub param_info_type_off:    usize,   // probed: offset of Il2CppType ptr within ParameterInfo
```

**Probe strategy (rigid, no fallback):**

```text
1. Allocate a 16-element ParamInfo scratch buffer, all bytes zeroed.
2. Locate a built-in method with ≥ 2 known param types (e.g. System.String::Concat).
3. For each candidate offset { 0x08, 0x10, 0x18, 0x20 }:
     read param[0] type-ptr at that offset
     resolve as Il2CppType, read the `tc` discriminator (existing valtype_from_tc)
     if it matches the known type of param[0] → candidate offset wins
4. Validate against param[1] for the second arg (sanity).
5. If no offset validates: REJECT and log. Do NOT fall back to a guess.
   Probe failure disables hook_runtime entirely (mem + dump + protocol keep working).
```

Per the `no-hardcoding-adaptive-resolution` charter — wrong offsets here mean corrupt arg reads in production.

### 2b — `InvokeArg` enum (agent-core, host-testable)

```rust
// agent-core/src/spine/invoke_arg.rs
pub enum InvokeArg {
    Prim(Value),          // wraps existing mem_value::Value for u8..u64 / i8..i64 / f32 / f64
    Instance(Instance),   // spine handle for il2cpp object refs
    String(String),       // marshalled via il2cpp_string_new (input) / Il2CppString::chars (output)
    Struct(Vec<u8>),      // packed byte buffer (Vector3 = 12 bytes, etc.)
    Array(Vec<InvokeArg>),// homogeneous; element class read from Il2CppArray::klass
    Null,                 // nullable refs (System.String, object, arrays)
}
```

`Value` (mem domain's vocabulary) is preserved unchanged; `InvokeArg::Prim` wraps it. No churn to the mem domain.

### 2c — `InvokeError` (spine error type, sibling to MemError)

```rust
// agent-core/src/spine/error.rs — added enum
pub enum InvokeError {
    NotFound,                                          // method/class missing
    ArgCountMismatch { expected: u8, got: u8 },
    ArgTypeMismatch { idx: u8, expected: ValType, got: ValType },
    NullInstance,                                      // instance method on null `this`
    MarshalFailed { idx: u8, reason: &'static str },
    ManagedException(String),                          // message read via il2cpp_exception_get_message; raw ptr logged on host side
    InternalFailure(&'static str),                     // FFI returned unexpected
}
impl From<InvokeError> for i32 { /* -100..-106 */ }
```

### 2d — FFI additions to `Il2CppApi`

```rust
// new opaque handle types in internals/ffi.rs (add to the existing block of `pub type X = c_void;` aliases)
pub type MethodInfo      = c_void;
pub type Il2CppString    = c_void;
pub type Il2CppArray     = c_void;
pub type Il2CppException = c_void;

// new function-pointer types
type RuntimeInvoke         = unsafe extern "C" fn(*mut MethodInfo, *mut c_void,
                                                  *mut *mut c_void, *mut *mut Il2CppException) -> *mut c_void;
type StringNew             = unsafe extern "C" fn(*const c_char) -> *mut Il2CppString;
type ArrayNew              = unsafe extern "C" fn(*mut Il2CppClass, usize) -> *mut Il2CppArray;
type ExceptionGetMessage   = unsafe extern "C" fn(*mut Il2CppException) -> *mut Il2CppString;

// added to Il2CppApi struct (4 fields):
pub runtime_invoke:         RuntimeInvoke,
pub string_new:             StringNew,
pub array_new:              ArrayNew,
pub exception_get_message:  ExceptionGetMessage,
```

Resolver paths:
- **Standard exports** — 4 `get_std!` lines added to `resolve_from_game_assembly` (works on Highrise + any non-obfuscated build).
- **Sig-scan** (`resolve_obfuscated_api`):
  - `string_new` / `array_new` / `exception_get_message` are small forwarding stubs; short stable byte patterns per Unity version.
  - `runtime_invoke` is a large body — Phase 2 starts with a PW-only prologue pattern (20–30 stable bytes) derived from a dump of the obfuscated export. Highrise validation is follow-up. Both fallback paths returning `None` cleanly disables invoke without breaking the dumper.

### 2e — Marshalling functions (`agent/src/internals/marshal.rs`)

```rust
// INVOKE — single combined entry (replaces separate pack_args + unpack_return).
// Internally builds InvokeContext, calls runtime_invoke, checks exc, unpacks return.
pub fn invoke_method(
    method: MethodPtr,
    instance: Option<Instance>,        // None for static
    args: &[InvokeArg],
) -> Result<InvokeArg, InvokeError>;

// HOOK — bidirectional (RegArgs ↔ InvokeArg)
pub fn regargs_to_args(method: MethodPtr, regs: &RegArgs)
    -> Result<Vec<InvokeArg>, InvokeError>;
pub fn args_to_regargs(method: MethodPtr, args: &[InvokeArg], regs: &mut RegArgs)
    -> Result<(), InvokeError>;
pub fn pack_return_into_regargs(return_type: ValType, v: &InvokeArg, regs: &mut RegArgs)
    -> Result<(), InvokeError>;
```

### 2f — `InvokeContext` (stable arg-lifetime storage for invoke)

```rust
struct InvokeContext {
    args_ptrs:      Vec<*mut c_void>,   // points into arg_slabs
    arg_slabs:      Vec<Vec<u8>>,       // owned storage for struct/string payloads
    string_handles: Vec<*mut Il2CppString>,  // managed-side owned; we just track
    exc:            *mut Il2CppException,    // out-param target; stack-local
}
```

Built on the stack inside `invoke_method`; `&context.args_ptrs[0]` is the `**args` for `runtime_invoke`; dropped on the way out — slabs free naturally; string handles stay live in the il2cpp GC.

### 2g — Per-type marshalling table

| Kind | Pack (Rust → boxed `void*`) | Unpack (`void*` → Rust, eager copy) | Win64 reg slot for hook |
|---|---|---|---|
| `Prim(U32)` | stack-alloca, `&val as *mut c_void` | `*(p as *const u32)` | `[RCX,RDX,R8,R9][positional]` (int) |
| `Prim(U64 / I64)` | same | `*(p as *const u64)` | int slot |
| `Prim(F32)` | stack-alloca | `*(p as *const f32)` | `XMM[positional]` (float) |
| `Prim(F64)` | stack-alloca | `*(p as *const f64)` | `XMM[positional]` (float) |
| `Instance(h)` | `h.as_u64() as *mut c_void` | `Instance::from_raw(*(p as *const u64))` | int slot |
| `String(s)` | `string_new(utf8_to_utf16_cstr(&s))` | `read_il2cpp_string(p)` → eager copy to Rust `String` | int slot |
| `Struct(bytes)` | stack-alloca + memcpy | eager memcpy from p; length = `class.instance_size - 0x10` | int slot (by-ptr) |
| `Array(elems)` | `array_new(elem_class, len)` + per-elem pack | walk Il2CppArray header (length@0x18, data@0x20) + per-elem unpack (eager) | int slot |
| `Null` | `std::ptr::null_mut()` | tag as `InvokeArg::Null` | zeroed int slot |

**All `unpack_*` paths COPY managed-heap data eagerly** into Rust-owned storage. The returned `InvokeArg` never holds a pointer into the managed heap (GC may move/reclaim). The single exception: `InvokeArg::Instance(h)` returns just the pointer value as a handle; reading the object's fields later is a separate `mem::read_t(field_addr)` op with its own freshness story.

### 2h — Instance-method `this` register shift

```rust
const METHOD_ATTRIBUTE_STATIC: u32 = 0x0010;

fn is_static(method: MethodPtr) -> bool {
    cache::read_u32(method.as_u64() as usize + cfg.method_flags_off)
        .map(|f| f & METHOD_ATTRIBUTE_STATIC != 0)
        .unwrap_or(false)
}

// regargs_to_args / args_to_regargs:
//   let reg_slot_offset: usize = if is_static(method) { 0 } else { 1 };  // shift past `this`
//   for declared_param_idx in 0..param_count:
//       physical_slot = declared_param_idx + reg_slot_offset
//       read the right register based on (param_type, physical_slot)
//
// The hook handler receives ONLY declared params. `this` is exposed via il2cpp.hook_this().
```

---

# Section 3 — Hook registry + dispatcher + reentry (lock-free hot path)

### 3a — New spine pieces

```rust
// agent-core/src/spine/handles.rs — one more handle (macro-generated)
handle_newtype!(HookHandle, "An installed managed-method hook (returned by install_hook).");

// agent-core/src/spine/error.rs — sibling to MemError + InvokeError
pub enum HookError {
    SlotPoolExhausted, MethodNotHookable, PatchFailed,
    HandlerNotFound, AlreadyHooked, UnknownHandle,
}
impl From<HookError> for i32 { /* -200..-205 */ }
```

### 3b — Registry (lock-free hot path)

```rust
// agent/src/internals/hook_runtime/registry.rs
const MAX_HOOKS: usize = 256;

static HOOK_SLOTS: [UnsafeCell<MaybeUninit<HookCtx>>; MAX_HOOKS] = ...;
static SLOT_VALID: [AtomicBool; MAX_HOOKS] = ...;
static REENTRY:    [AtomicBool; MAX_HOOKS] = ...;
static INSTALL_GUARD: Mutex<()> = Mutex::new(());  // only for install/remove allocation; never in hot path

struct HookCtx {
    method:      MethodPtr,
    is_static:   bool,
    param_count: u8,
    sig:         MethodSignature,    // cached param ValTypes + return ValType
    thunk_addr:  usize,
    trampoline:  usize,
    handler:     WasmHandlerRef,     // agent-internal wrap around wasmi Func
}

// Hot path lookup (no mutex):
fn ctx_for(method_id: u64) -> Option<&'static HookCtx> {
    let id = method_id as usize;
    if id >= MAX_HOOKS || !SLOT_VALID[id].load(Ordering::Acquire) { return None; }
    Some(unsafe { (*HOOK_SLOTS[id].get()).assume_init_ref() })
}
```

**INVARIANT (debug-asserted at every install/remove/dispatch boundary):**
```
thunk_slot_N.embedded_id == N
HOOK_SLOTS[N] holds the HookCtx for that method
REENTRY[N] guards that method
HookHandle::from_raw(N) is the script-visible ticket
— ONE NUMBER FROM SCRIPT TO ASM.
```

### 3c — Install / remove / call_original (agent-internal typed API)

```rust
pub fn install_hook(method: MethodPtr, handler: WasmHandlerRef)
    -> Result<HookHandle, HookError>;
pub fn remove_hook(handle: HookHandle) -> Result<(), HookError>;
pub fn call_original(handle: HookHandle, args: &[InvokeArg])
    -> Result<InvokeArg, InvokeError>;
```

### 3d — Dispatcher flow (end-to-end)

```text
Game thread calls Player::TakeDamage(50)
  └─ patched 12 bytes: mov r10, method_id; mov r11, shim; jmp r11
       └─ universal_shim: captures RegArgs → calls dispatch_rust(id, &RegArgs)
            └─ dispatch_rust:
                 if REENTRY[id].swap(true, AcqRel):                         // already inside ourselves
                     return call_trampoline_with_regargs(ctx.trampoline)    // run trampoline only, no wasm
                 let ctx = ctx_for(id)?
                 let args = marshal::regargs_to_args(ctx.method, regs)?
                 let result = ctx.handler.call_typed(args)?                  // wasmi typed call
                 REENTRY[id].store(false, Release)
                 marshal::pack_return_into_regargs(ctx.sig.return_type, &result, regs)?
                 return                                                     // shim restores stack + ret
```

Reentry guard is **per-method**, not global — a handler for A may call B (different hooked method); B's hook fires independently. Composable.

### 3e — `WasmHandlerRef`

Agent-internal wrap around `wasmi::Func` resolved from the script's function table at `install_hook` time. Handler signature is always `() -> ()` — args/return travel via the host-fn pull/push API (Section 4). Never exposed to scripts.

---

# Section 4 — WASM host-fn surface (script-visible ABI)

### 4a — `InvokeArg` wire tags (extends `ValType` byte)

```text
Tag byte (1 byte) at the start of every packed value:
 0..9   ValType primitives (u8..u64, i8..i64, f32, f64) — payload: N bytes per ValType::fixed_width()
 10     Bytes                                            — payload: u32 len, then bytes
 11     Cstr                                             — payload: u32 len, then UTF-8 bytes
 12     Instance                                         — payload: u64 (the handle)
 13     String                                           — payload: u32 len, then UTF-8 bytes
 14     Struct                                           — payload: u32 len, then raw bytes
 15     Array                                            — payload: u32 elem_count, then elem_count packed InvokeArgs
 16     Null                                             — payload: empty
```

Tags 0..11 reuse the existing `mem` ABI exactly. Tags 12..16 are invoke/hook-only.

### 4b — Invoke surface (Sub-brick I)

```wat
(import "il2cpp" "invoke"
        (func $invoke (param i64 i64 i32 i32 i32 i32) (result i32)))
;; (method_ptr, instance_ptr_or_0_for_static, args_buf, args_len, out_buf, out_cap) -> status
;; args_buf wire format: u32 arg_count, then per-arg [u8 tag, payload]
;; out_buf on success: [u8 tag, payload]   (the return value as a packed InvokeArg)
```

### 4c — Hook install/remove (Sub-brick II)

```wat
(import "il2cpp" "install_hook" (func $install_hook (param i64 i32) (result i64)))
;; (method_ptr, handler_funcref_table_idx) -> hook_handle (positive i64) or negative status

(import "il2cpp" "remove_hook" (func $remove_hook (param i64) (result i32)))
;; (hook_handle) -> status
```

The handler is referenced via the wasm module's function table (`(elem ...)`-installed at module init). Agent resolves `wasmi::Table::get(idx).typed::<(), ()>()` once at install_hook, stashes in HookCtx.

### 4d — Hook handler host fns (only callable from inside a handler)

```wat
;; PULL: fetch arg by index, packed into out_buf as [u8 tag, payload].
(import "il2cpp" "hook_arg" (func $hook_arg (param i32 i32 i32) (result i32)))
;; (arg_idx, out_buf, out_cap) -> bytes_written or negative status

;; PUSH: replace arg N with the value packed in val_buf.
(import "il2cpp" "hook_set_arg" (func $hook_set_arg (param i32 i32 i32) (result i32)))
;; (arg_idx, val_buf, val_len) -> status

;; THIS: instance handle for instance methods; 0 for static.
(import "il2cpp" "hook_this" (func $hook_this (result i64)))

;; CALL ORIGINAL: runs trampoline with current arg state; packs return into out_buf.
;; Implicit "current hook" context (one active per thread; tracked in TLS by dispatcher).
(import "il2cpp" "call_original" (func $call_original (param i32 i32) (result i32)))
;; (out_buf, out_cap) -> status. out_buf gets [u8 tag, payload].

;; SET RETURN: overrides the value returned to the original caller.
(import "il2cpp" "hook_set_return" (func $hook_set_return (param i32 i32) (result i32)))
;; (val_buf, val_len) -> status
```

### 4e — Precedence policy (explicit)

- `hook_set_return` called → use that value, skip the agent's default original-call.
- `call_original` called (without later `hook_set_return`) → use its return.
- Neither called → **agent calls original itself**, uses its return (transparent observer pattern).
- Latest of the two explicit setters wins.

### 4f — Worked example — block all damage to the player

```wat
(module
  (import "il2cpp" "find_class"    (func $find_class    (param i32 i32) (result i64)))
  (import "il2cpp" "find_method"   (func $find_method   (param i64 i32 i32 i32) (result i64)))
  (import "il2cpp" "install_hook"  (func $install_hook  (param i64 i32) (result i64)))
  (import "il2cpp" "hook_set_arg"  (func $hook_set_arg  (param i32 i32 i32) (result i32)))
  (import "il2cpp" "call_original" (func $call_original (param i32 i32) (result i32)))
  (import "env"    "log"           (func $log           (param i32 i32)))
  (memory (export "memory") 1)
  (table 1 funcref)
  (elem (i32.const 0) $handler)

  (data (i32.const 0)   "Player")
  (data (i32.const 16)  "TakeDamage")
  (data (i32.const 128) "blocked damage")

  ;; Handler: replace arg 0 with 0, call original, log.
  (func $handler
    (local $scratch i32)
    (local.set $scratch (i32.const 512))
    (i32.store8         (local.get $scratch) (i32.const 6))   ;; tag = i32 (C# int)
    (i32.store offset=1 (local.get $scratch) (i32.const 0))   ;; value = 0
    (drop (call $hook_set_arg  (i32.const 0) (local.get $scratch) (i32.const 5)))
    (drop (call $call_original (i32.const 1024) (i32.const 32)))
    (call $log (i32.const 128) (i32.const 14)))

  (func (export "frog_main")
    (local $klass i64) (local $method i64)
    (local.set $klass  (call $find_class  (i32.const 0)  (i32.const 6)))
    (local.set $method (call $find_method (local.get $klass) (i32.const 16) (i32.const 10) (i32.const 1)))
    (drop (call $install_hook (local.get $method) (i32.const 0)))))
```

---

# Section 5 — Module layout + sequencing + testing

### 5a — File tree

```
crates/agent-core/src/spine/
├── handles.rs              ← + HookHandle (1 line via macro)
├── error.rs                ← + InvokeError + HookError
├── invoke_arg.rs           ← NEW: InvokeArg enum + tag↔variant encode/decode
└── (existing: mod, addr, value)

crates/agent/src/
├── inline_detour.rs        ← MOVED from protocol/hook.rs (prerequisite P-2)
├── internals/
│   ├── api.rs              ← + invoke_method_t (typed sibling, spine-shaped)
│   ├── config.rs           ← + 5 probed offsets
│   ├── ffi.rs              ← + 4 FFI types + Il2CppApi slots + resolver paths
│   ├── marshal.rs          ← NEW: invoke_method + the 4 marshalling functions
│   └── hook_runtime/
│       ├── mod.rs
│       ├── regargs.rs      ← RegArgs POD
│       ├── shim.rs         ← universal_shim (inline asm)
│       ├── replay.rs       ← call_trampoline_with_regargs (inline asm)
│       ├── thunks.rs       ← slab + thunk emitter
│       ├── registry.rs     ← HOOK_SLOTS + SLOT_VALID + REENTRY + INVARIANT
│       ├── api.rs          ← install_hook / remove_hook / call_original
│       └── dispatcher.rs   ← dispatch_rust (hot path)
└── runtime/
    └── mem_host.rs         ← + 8 new host fns
```

`agent-core` stays pure types / host-testable. All FFI + asm + IO in `agent`.

### 5b — Sequencing (two sub-bricks with a PW gate between)

```
PREREQUISITE (separate small brick, lands first)
├── P-1: value-type FieldInfo offset fix (task #72)
└── P-2: inline_detour.rs MOVE refactor

SUB-BRICK I — Invoke
├── Phase 0  Probe pass: derive 5 MethodInfo/ParamInfo offsets; bank into config.rs
├── Phase 1  FFI additions: 4 new Il2CppApi exports + standard-export resolvers
├── Phase 2  FFI sig-scan: runtime_invoke + 3 small forwarders for obfuscated builds
├── Phase 3  Spine: InvokeArg + InvokeError + tag↔variant encoder/decoder (agent-core)
├── Phase 4  Marshalling: invoke_method + InvokeContext + per-type pack/unpack
├── Phase 5  WASM host fn: il2cpp.invoke (single combined entry)
└── Phase 6  PW gate: call Player::GetIsLocalPlayer() (0 args, bool);
                      Player::TakeDamage(50) (1 int, void) — verify HP drop via mem read

SUB-BRICK II — Hook
├── Phase 7   RegArgs POD + universal_shim inline asm + cross-compile smoke
├── Phase 8   call_trampoline_with_regargs inline asm + thunk emitter + slab
├── Phase 9   Spine: HookHandle + HookError
├── Phase 10  Registry: HOOK_SLOTS/SLOT_VALID/REENTRY (lock-free) + INVARIANT doc
├── Phase 11  dispatch_rust: marshalling → wasmi typed call → return packing
├── Phase 12  Hook half of marshal: regargs_to_args + args_to_regargs + pack_return_into_regargs
├── Phase 13  WASM host fns: install_hook, remove_hook, hook_arg, hook_set_arg,
│                            hook_this, call_original, hook_set_return
└── Phase 14  PW gate: observer hook on Player::Update (log per N frames);
                       full-mutate hook on TakeDamage (set arg 0 to 0 + call_original
                       → verify HP unchanged after combat tick);
                       reentry safety (A→B both fire; A→call_original no loop)
```

Each sub-brick gets its own writing-plans pass + its own PW gate. Sub-brick I is ~6 tasks; Sub-brick II is ~8 tasks.

### 5c — Testing strategy

| Layer | Where | Verifies |
|---|---|---|
| Spine types (`InvokeArg`, `InvokeError`, `HookHandle`, `HookError`) | `agent-core/tests/spine.rs` (Linux) | enum size, tag round-trip, error→i32 mapping, status code range partitioning |
| `InvokeArg` encode/decode (no FFI) | `agent-core/tests/invoke_arg.rs` (Linux) | every tag packs → unpacks identical; short-buffer rejection; nested Array round-trip |
| FFI / agent compile | cross-compile `cargo build --target x86_64-pc-windows-gnu --release` | new Il2CppApi compiles; asm modules link; no unresolved symbols |
| Probe pass | PW gate (manual, `FROG_METHOD_PROBE=1`) | logs derived offsets; values bank into `config.rs` |
| Invoke MVP | PW gate | `Player::GetIsLocalPlayer()` → true; `Player::TakeDamage(50)` → HP drops by ~50 |
| Hook observer | PW gate | `Player::Update` → log line per N frames |
| Hook full-mutate | PW gate | `TakeDamage` arg→0 + `call_original` → HP unchanged |
| Reentry safety | PW gate | handler for A calls hooked B → both fire; A's handler calls `call_original` → no loop |

### 5d — What ships

- Scripts call **any non-generic managed method** with primitive, object, string, struct, or array args. Exceptions surface as `InvokeError::ManagedException(msg)`.
- Scripts **hook any non-generic managed method**, observe args + return, modify args before original, call original mid-handler, modify return after.
- Lock-free hot path; per-method reentry-safe; one Frog hook per method.
- WASM ABI extends additively — existing `mem.*` / `il2cpp.*` untouched.
- agent-core stays pure types; all asm + FFI in `agent`.

### 5e — Deferred (NOT in this brick)

| Capability | Deferred to | Why |
|---|---|---|
| Multi-hook chains | Future sub-brick | Needs ordering policy |
| In-flight packet modify | Priority #3 — separate brick | Protocol-domain; reuses `inline_detour` |
| PCAP export | Priority #4 — separate brick | Drains FrameRing; no overlap |
| Disassembly | Priority #5 — separate brick | Read-only iced-x86 over MethodInfo::methodPointer |
| Generic methods (`List<T>::Add(T)`) | Tier-2 add after Sub-brick II | Per-call-site generic instantiation |
| Async method invocation | Not planned | Synchronous suffices |

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Phase 0 probe fails on a new Unity version | Probe is its own Phase 0 task and a hard gate; uses a known-stable built-in method as anchor; rejects on failure (no fallback). |
| Inline asm bugs in Phases 7–8 | Two phases = two checkpoints. Phase 7 ships shim that stub-dispatches with no integration; Phase 8 wires it through thunks. Bug localization is easier. |
| `runtime_invoke` sig-scan version-fragile | Ship a PW-first pattern in Phase 2; Highrise validation is follow-up. `resolve()` returning `None` disables invoke cleanly without breaking the dumper. |
| `hook_runtime` init fails (probe or sig-scan) | `install_hook` returns `HookError::PatchFailed`. mem + il2cpp lookup + protocol capture keep working. The script just can't use invoke/hook on this build. |
| Two game threads hit the same hook concurrently | Lock-free hot path via `[AtomicBool; 256]` and pre-allocated `HOOK_SLOTS`. Zero mutex contention. |
| Handler recursion (A's handler calls hooked A) | `REENTRY[id]` atomic per method — second entry skips wasm and runs trampoline directly. No infinite loop. |
