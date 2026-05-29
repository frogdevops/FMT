//! Replay path: takes a `RegArgs` buffer (after the wasm handler may have
//! modified arg slots) and a trampoline pointer; loads the args back into
//! the appropriate registers and calls the trampoline. The trampoline runs
//! the original method's stolen-bytes prologue, then jumps back to
//! target+stolen_len. We capture RAX / XMM0 into ret_int / ret_float.
//!
//! Calling convention from Rust:
//!   call_trampoline_with_regargs(trampoline_ptr: u64, regs: *mut RegArgs)
//!   - rcx = trampoline_ptr
//!   - rdx = &mut RegArgs
//!
//! NOTE on stack args (arg 5+): NOT replayed in v1. Hooks targeting methods
//! with more than 4 args will read/modify args 0..3 correctly but additional
//! stack-borne args are passed through unchanged via the original stack frame
//! (which the trampoline still sees). Marshalling layer documents this.

use core::arch::global_asm;

global_asm!(
    ".global call_trampoline_with_regargs",
    "call_trampoline_with_regargs:",
    // ── Prologue: save callee-saved regs + align stack ────────────
    //  Entry rsp ≡ 8 mod 16 (standard Win64 callee entry).
    //  push rbx → 0 mod 16.  sub rsp, 32 (multiple of 16) → 0 mod 16.
    //  Before `call rax` rsp must be 0 mod 16 so callee sees 8 mod 16
    //  (required for SSE 16-byte aligned stack ops in Pow's body).
    //
    //  We do NOT push rbp / set up a frame pointer — it would shift
    //  parity by 8 and misalign the trampoline call.
    "  push rbx",                       // stash &RegArgs survives across call
    "  sub  rsp, 32",                   // shadow space + alignment
    // ── Stash inputs ──────────────────────────────────────────────
    "  mov  rax, rcx",                  // rax = trampoline ptr (will call via rax)
    "  mov  rbx, rdx",                  // rbx = &RegArgs (survives the call)
    // ── Rehydrate args from RegArgs ──────────────────────────────
    //  CAREFUL: load rcx LAST among int regs because we need rdx alive
    //  to address RegArgs throughout. Actually rdx becomes int_args[1]
    //  so load it second-to-last; rcx (int_args[0]) is last from-rdx access.
    "  mov  r8,    qword ptr [rdx + 24]",   // int_args[2]
    "  mov  r9,    qword ptr [rdx + 32]",   // int_args[3]
    "  movsd xmm0, qword ptr [rdx + 40]",   // float_args[0]
    "  movsd xmm1, qword ptr [rdx + 48]",   // float_args[1]
    "  movsd xmm2, qword ptr [rdx + 56]",   // float_args[2]
    "  movsd xmm3, qword ptr [rdx + 64]",   // float_args[3]
    "  mov  rcx,   qword ptr [rdx + 8]",    // int_args[0]
    "  mov  rdx,   qword ptr [rdx + 16]",   // int_args[1]  ← last rdx use
    // ── Call the trampoline ──────────────────────────────────────
    "  call rax",
    // ── Capture return into RegArgs (rbx still has &RegArgs) ─────
    "  mov   qword ptr [rbx + 80], rax",
    "  movsd qword ptr [rbx + 88], xmm0",
    // ── Epilogue ─────────────────────────────────────────────────
    "  add rsp, 32",
    "  pop rbx",
    "  ret",
);

extern "C" {
    pub fn call_trampoline_with_regargs(
        trampoline_ptr: u64,
        regs: *mut crate::internals::hook_runtime::regargs::RegArgs,
    );
}
