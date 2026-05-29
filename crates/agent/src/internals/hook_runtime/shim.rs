//! Universal capture shim. Entered via JMP from a per-method thunk; the thunk
//! has already loaded R10 with method_id. RCX/RDX/R8/R9/XMM0..3 still hold the
//! original method's args; the original caller's return address is at [rsp].
//!
//! On entry:
//!   - rsp is 16-aligned + 8 (caller's `call` pushed return addr)
//!   - r10 = method_id (from thunk)
//!   - r11 = (was thunk's shim_addr, now scratch)
//!   - rcx, rdx, r8, r9 = int args (positional, by Win64 ABI)
//!   - xmm0..xmm3 = float args (same positional slots — float-or-int per arg)
//!   - stack args at [rsp + 8 + (4 * 8)] = [rsp + 0x28] and beyond
//!     (after our `push rbp`, that becomes [rbp + 0x10])
//!
//! After our prologue (push rbp; sub rsp, 112):
//!     [rbp+8]   = return addr (back to original caller)
//!     [rbp]     = saved rbp
//!     [rsp..rsp+96] = the RegArgs we're populating
//!     [rsp+96..rsp+112] = padding for 16-byte alignment + shadow space margin

use core::arch::global_asm;

global_asm!(
    ".global universal_shim",
    "universal_shim:",
    // ── Prologue ────────────────────────────────────────────────────
    "  push rbp",
    "  mov  rbp, rsp",
    "  sub  rsp, 112",                  // 96 RegArgs + 16 alignment/padding
    // ── Capture method_id ───────────────────────────────────────────
    "  mov  qword ptr [rsp + 0],  r10", // method_id
    // ── Capture int args (RCX, RDX, R8, R9) ─────────────────────────
    "  mov  qword ptr [rsp + 8],  rcx", // int_args[0]
    "  mov  qword ptr [rsp + 16], rdx", // int_args[1]
    "  mov  qword ptr [rsp + 24], r8",  // int_args[2]
    "  mov  qword ptr [rsp + 32], r9",  // int_args[3]
    // ── Capture float args (XMM0..XMM3, low 64 bits each) ──────────
    "  movsd qword ptr [rsp + 40], xmm0",
    "  movsd qword ptr [rsp + 48], xmm1",
    "  movsd qword ptr [rsp + 56], xmm2",
    "  movsd qword ptr [rsp + 64], xmm3",
    // ── Pointer to caller's stack args (skip our saved rbp + ret addr) ─
    "  lea  r11, [rbp + 16]",
    "  mov  qword ptr [rsp + 72], r11",
    // ── Call dispatch_rust(method_id, &RegArgs) ────────────────────
    "  mov  rcx, qword ptr [rsp + 0]",  // arg 1: method_id
    "  mov  rdx, rsp",                  // arg 2: &RegArgs
    "  sub  rsp, 32",                   // shadow space
    "  call dispatch_rust",
    "  add  rsp, 32",
    // ── Load return values into rax + xmm0 (caller picks one) ──────
    "  mov   rax,  qword ptr [rsp + 80]",  // ret_int  → RAX
    "  movsd xmm0, qword ptr [rsp + 88]",  // ret_float → XMM0
    // ── Epilogue ───────────────────────────────────────────────────
    "  mov rsp, rbp",
    "  pop rbp",
    "  ret",
);

extern "C" {
    /// The shim entry point. Never called directly — its address is patched
    /// into per-method thunks. Declared `extern "C"` so we can take its
    /// address; calling convention details are handled inside the asm.
    pub fn universal_shim();
}

/// Dispatched-into Rust function. The shim calls this with
/// (method_id: u64, regs: *mut RegArgs). Body lives in dispatcher.rs.
#[allow(dead_code)]
extern "system" {
    pub fn dispatch_rust(method_id: u64, regs: *mut crate::internals::hook_runtime::regargs::RegArgs);
}
