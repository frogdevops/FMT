//! Hook API — install_hook / remove_hook / call_original + per-thread context
//! plumbing for the WASM host fns. Full impl lands in H9/H10; this file is
//! seeded here so dispatcher.rs links.

use crate::internals::hook_runtime::regargs::RegArgs;
use crate::internals::hook_runtime::registry::HookCtx;
use agent_core::spine::InvokeArg;

pub struct HandlerResult {
    pub return_value: Option<InvokeArg>,
}

/// Stub: H9/H10 wire this to the wasm runtime. For now, short-circuit so
/// the dispatcher flow is testable end-to-end before wasm dispatch is plumbed.
///
/// `regs` is passed as a raw pointer so the caller is not holding a live
/// `&mut RegArgs` borrow when it calls into the closure (the closure also
/// needs `&mut RegArgs` for `pack_return_into_regargs`).
///
/// SAFETY: caller guarantees `regs` is non-null and valid for the lifetime
/// of this call (the shim guarantees this on the hot path).
pub fn with_current_context(
    _ctx: &HookCtx,
    _regs: *mut RegArgs,
    _args: &[InvokeArg],
    cont: impl FnOnce(HandlerResult),
) {
    cont(HandlerResult { return_value: None });
}
