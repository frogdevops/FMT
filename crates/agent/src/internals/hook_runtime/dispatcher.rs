//! `dispatch_rust` — the hot path called from the universal shim. Looks up
//! the HookCtx by method_id, applies per-method reentry guard, marshals args
//! to the wasm handler, packs the return back into RegArgs.

use crate::internals::hook_runtime::regargs::RegArgs;
use crate::internals::hook_runtime::registry::{
    clear_reentry, ctx_for, try_enter_reentry,
};
use crate::internals::hook_runtime::replay::call_trampoline_with_regargs;
use crate::internals::marshal::{
    pack_return_into_regargs, regargs_to_args,
};

/// Entry called from `universal_shim`. The function NEVER unwinds — any
/// failure to dispatch falls through to the trampoline (run-original-only
/// fallback) so the game keeps making progress.
#[no_mangle]
pub extern "system" fn dispatch_rust(method_id: u64, regs: *mut RegArgs) {
    crate::paths::log(&format!("dispatch_rust: ENTRY method_id={}", method_id));

    // SAFETY: shim guarantees regs is non-null and points at a valid RegArgs
    // for the duration of this call.
    let regs = unsafe { &mut *regs };

    let ctx = match ctx_for(method_id) {
        Some(c) => c,
        None => {
            // No hook installed (or already torn down) — run trampoline only.
            // ctx_for returns None when the slot was unpublished; the trampoline
            // address is gone with it. Best we can do is write zero return and
            // return; the original caller will see zeros (degraded but won't
            // crash). In practice this path is rare — install/remove serialized
            // by INSTALL_GUARD, and a method only reaches the shim if its hook
            // slot was valid at the moment the JMP was taken.
            crate::paths::log(&format!("dispatch_rust: [1/5] ctx_for({}) is None — zero ret + return", method_id));
            regs.ret_int = 0;
            regs.ret_float = 0.0;
            return;
        }
    };
    crate::paths::log("dispatch_rust: [1/5] ctx_for OK");

    // Per-method reentry guard: if we're already inside this hook (e.g. the
    // wasm handler is calling the same method via call_original or other
    // invocation), skip the wasm path and run trampoline directly.
    if try_enter_reentry(method_id) {
        crate::paths::log("dispatch_rust: [2/5] reentry detected — direct trampoline replay");
        unsafe {
            call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs);
        }
        crate::paths::log("dispatch_rust: [2/5] reentry trampoline returned");
        // Don't clear the guard — we re-entered while the outer guard is set.
        // The outermost dispatch_rust frame clears it.
        return;
    }
    crate::paths::log("dispatch_rust: [2/5] reentry OK (entered fresh)");

    // Snapshot args (read-only view for the handler).
    let args = match regargs_to_args(ctx.method, regs) {
        Ok(a) => a,
        Err(e) => {
            crate::paths::log(&format!("dispatch_rust: [3/5] regargs_to_args FAIL {:?} — fallback trampoline", e));
            unsafe { call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs as *mut RegArgs); }
            clear_reentry(method_id);
            return;
        }
    };
    crate::paths::log(&format!("dispatch_rust: [3/5] regargs_to_args OK arg_count={}", args.len()));

    // Invoke wasm handler. The handler reads/writes args via host fns
    // (hook_arg, hook_set_arg) and may call_original / hook_set_return.
    // Per-thread "current hook context" pointer for the host fns to find.
    //
    // `regs` is passed as a raw pointer so no live `&mut` borrow is held
    // across the closure boundary — the closure independently needs `&mut
    // RegArgs` for pack_return_into_regargs.
    let regs_ptr: *mut RegArgs = regs as *mut RegArgs;
    crate::paths::log("dispatch_rust: [4/5] calling with_current_context");
    super::api::with_current_context(ctx, regs_ptr, &args, |handler_result| {
        crate::paths::log(&format!("dispatch_rust: handler returned return_value.is_some()={} called_original={}",
            handler_result.return_value.is_some(), handler_result.called_original));
        if let Some(rv) = handler_result.return_value {
            // Handler explicitly set a return value via hook_set_return.
            // SAFETY: regs_ptr came from `&mut *regs` at the top of this
            // function; it remains valid for the lifetime of dispatch_rust.
            let regs_ref = unsafe { &mut *regs_ptr };
            if let Err(e) = pack_return_into_regargs(
                ctx.sig.return_type,
                ctx.sig.return_tc,
                &rv,
                regs_ref,
            ) {
                crate::paths::log(&format!("hook: pack_return failed for method_id={}: {:?}", method_id, e));
            }
        } else if !handler_result.called_original {
            // Transparent observer: handler took no action (neither
            // hook_set_return nor call_original was invoked). Run the original
            // so the game keeps progressing as if the hook weren't installed.
            crate::paths::log(&format!("dispatch_rust: transparent observer — calling trampoline at {:#x}", ctx.patch.trampoline));
            unsafe {
                call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs_ptr);
            }
            crate::paths::log(&format!("dispatch_rust: trampoline returned ret_int={:#x} ret_float={}",
                unsafe { (*regs_ptr).ret_int }, unsafe { (*regs_ptr).ret_float }));
        }
        // else: handler called call_original → regs.ret_int/ret_float already
        // hold the trampoline's return value; nothing more to write.
        // If the handler wrote modified args back, args_to_regargs already
        // pushed them via hook_set_arg.
    });
    crate::paths::log("dispatch_rust: [4/5] with_current_context returned");

    clear_reentry(method_id);
    crate::paths::log("dispatch_rust: [5/5] DONE");
}
