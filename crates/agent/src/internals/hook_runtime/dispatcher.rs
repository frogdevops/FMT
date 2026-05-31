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

    // SAFETY: shim guarantees regs is non-null and points at a valid RegArgs.
    let regs = unsafe { &mut *regs };

    // Scope-block: ctx (a &'static HookCtx tied to the slot's SLOT_VALID=true
    // contract) lives only inside this block. After the closing brace, no
    // live reference to HookCtx remains, so the piggyback below is free to
    // unpublish the slot via registry_reload without dangling-ref UB.
    {
        let ctx = match ctx_for(method_id) {
            Some(c) => c,
            None => {
                crate::paths::log(&format!("dispatch_rust: [1/5] ctx_for({}) is None — zero ret + return", method_id));
                regs.ret_int = 0;
                regs.ret_float = 0.0;
                return;
            }
        };
        crate::paths::log("dispatch_rust: [1/5] ctx_for OK");

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

        let regs_ptr: *mut RegArgs = regs as *mut RegArgs;
        crate::paths::log("dispatch_rust: [4/5] calling with_current_context");
        super::api::with_current_context(ctx, regs_ptr, &args, |handler_result| {
            crate::paths::log(&format!("dispatch_rust: handler returned return_value.is_some()={} called_original={}",
                handler_result.return_value.is_some(), handler_result.called_original));
            if let Some(rv) = handler_result.return_value {
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
                crate::paths::log(&format!("dispatch_rust: transparent observer — calling trampoline at {:#x}", ctx.patch.trampoline));
                unsafe {
                    call_trampoline_with_regargs(ctx.patch.trampoline as u64, regs_ptr);
                }
                crate::paths::log(&format!("dispatch_rust: trampoline returned ret_int={:#x} ret_float={}",
                    unsafe { (*regs_ptr).ret_int }, unsafe { (*regs_ptr).ret_float }));
            }
        });
        crate::paths::log("dispatch_rust: [4/5] with_current_context returned");
    }  // ctx dropped here; SLOT_VALID/ctx ref no longer in flight

    clear_reentry(method_id);
    crate::paths::log("dispatch_rust: [5/5] DONE");

    // PIGGYBACK: per [[hooks-are-the-sync-primitive]], we're on the game
    // thread with the game frozen by the inline-detour call stack. Safe to
    // run registry_reload here because:
    //   - ctx is out of scope (no dangling reference)
    //   - reentry is cleared (a re-firing of the same method won't read
    //     a soon-to-be-stale slot)
    //   - SLOT_VALID release-stores by unpublish_slot prevent fresh
    //     ctx_for hits from other threads
    //
    // Single-game-thread assumption documented in the B-6a spec; if a
    // hooked method ever fires from multiple OS threads, an epoch-counter
    // fix lands as a separate brick.
    if let Some(bytes) = crate::runtime::orchestrator::take_reload_pending() {
        crate::paths::log("dispatch_rust: piggyback drained reload-pending; running registry_reload");
        crate::runtime::orchestrator::registry_reload(&bytes);
    }
}
