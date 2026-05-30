//! Hook API: install_hook / remove_hook / call_original + per-thread
//! "current hook" plumbing the WASM host fns key into.

use std::cell::RefCell;

use agent_core::spine::{HookError, HookHandle, InvokeArg, MethodPtr};

use crate::inline_detour;
use crate::internals::hook_runtime::registry::{
    alloc_slot, ctx_for, publish_slot, unpublish_slot, HookCtx, INSTALL_GUARD,
};
use crate::internals::hook_runtime::regargs::RegArgs;
use crate::internals::hook_runtime::thunks::{emit_thunk, free_thunk};
use crate::internals::marshal::read_signature;

pub struct HandlerResult {
    /// Value from `hook_set_return` if the handler explicitly set one.
    pub return_value: Option<InvokeArg>,
    /// True if the handler invoked `call_original` (regs.ret_* already holds the original return).
    pub called_original: bool,
}

/// Per-thread "current hook" state — the WASM host fns read this when the
/// script calls hook_arg / hook_set_arg / call_original etc. inside a handler.
pub(crate) struct CurrentContext {
    pub method:          MethodPtr,
    /// Raw pointer to the shim-allocated RegArgs. Valid for the duration of
    /// dispatch_rust; never escapes that frame.
    pub regs:            *mut RegArgs,
    pub args:            Vec<InvokeArg>,
    pub explicit_return: Option<InvokeArg>,
    pub called_original: bool,
}

thread_local! {
    // Per-thread STACK so nested hook dispatches (handler A's call_original
    // triggering hooked method B on the same thread) don't corrupt each
    // other's context. Each dispatch pushes; the same dispatch pops at the
    // end of with_current_context. Host fns operate on `.last()` / `.last_mut()`
    // — the top of stack = the currently-active hook context.
    static CURRENT: RefCell<Vec<CurrentContext>> = RefCell::new(Vec::new());
}

/// Called by the dispatcher with the per-method context. Pushes context onto
/// the per-thread CURRENT slot, invokes the wasm handler via
/// `crate::runtime::host::call_hook_handler`, then takes back the result.
///
/// `regs` is a raw pointer (not `&mut`) so the dispatcher doesn't hold a live
/// `&mut RegArgs` borrow across the closure boundary — the closure's
/// `pack_return_into_regargs` also needs `&mut RegArgs`.
///
/// SAFETY: caller guarantees `regs` is non-null and valid for the duration of
/// this call (the shim guarantees this on the hot path).
pub fn with_current_context(
    ctx: &crate::internals::hook_runtime::registry::HookCtx,
    regs: *mut RegArgs,
    args: &[InvokeArg],
    cont: impl FnOnce(HandlerResult),
) {
    crate::paths::log(&format!("with_current_context: ENTRY method={:#x}", ctx.method.as_u64()));

    // PUSH our context onto the per-thread stack. Pairs with the pop below.
    CURRENT.with(|c| {
        c.borrow_mut().push(CurrentContext {
            method:          ctx.method,
            regs,
            args:            args.to_vec(),
            explicit_return: None,
            called_original: false,
        });
    });

    // Invoke the wasm handler. The handler is a table entry (() -> ()) — args
    // and return travel via the hook_arg/hook_set_arg/... host fns.
    // call_hook_handler is a v1 stub (see crate::runtime::host for the honesty
    // disclaimer); real wasmi Store wiring is deferred to H11.
    if let Err(e) = crate::runtime::host::call_hook_handler(ctx.handler_func_ref) {
        crate::paths::log(&format!("hook handler call failed: {:?}", e));
    }

    // POP the top of stack — the context we pushed above. Underflow would
    // mean a push/pop pairing bug elsewhere; expect-panic is the right
    // visibility on that invariant.
    let result = CURRENT.with(|c| {
        let cc = c.borrow_mut().pop().expect("context underflow");
        HandlerResult {
            return_value: cc.explicit_return,
            called_original: cc.called_original,
        }
    });

    crate::paths::log("with_current_context: invoking cont closure");
    cont(result);
}

/// Install a hook on `method`. `handler_func_ref` is a wasm function-table
/// index resolved at install time; the dispatcher uses it in H10 to call the
/// handler.
///
/// Steps (serialised by INSTALL_GUARD; dispatch never blocks):
///   1. Acquire INSTALL_GUARD.
///   2. Read signature (fails → MethodNotHookable).
///   3. Alloc slot id (fails → SlotPoolExhausted).
///   4. Emit per-method thunk (fails → PatchFailed).
///   5. Patch target via inline_detour::install (fails → PatchFailed + free_thunk).
///   6. Build HookCtx + publish_slot.
///   7. Return HookHandle.
pub fn install_hook(
    method: MethodPtr,
    handler_func_ref: u64,
) -> Result<HookHandle, HookError> {
    crate::paths::log(&format!("install_hook: ENTRY method={:#x} handler={}", method.as_u64(), handler_func_ref));

    let _guard = INSTALL_GUARD.lock().map_err(|_| {
        crate::paths::log("install_hook: INSTALL_GUARD poisoned");
        HookError::PatchFailed
    })?;
    crate::paths::log("install_hook: [1/6] guard acquired");

    // Step 2: cache the signature so the dispatcher doesn't re-read per-call.
    let sig = read_signature(method).map_err(|e| {
        crate::paths::log(&format!("install_hook: [2/6] read_signature FAIL {:?}", e));
        HookError::MethodNotHookable
    })?;
    crate::paths::log(&format!("install_hook: [2/6] sig param_count={} is_static={}", sig.param_types.len(), sig.is_static));

    // Step 3: pick a free slot id.
    let id = alloc_slot().ok_or_else(|| {
        crate::paths::log("install_hook: [3/6] alloc_slot exhausted");
        HookError::SlotPoolExhausted
    })?;
    crate::paths::log(&format!("install_hook: [3/6] slot id={}", id));

    // Step 4: emit the per-method thunk with this id embedded.
    let thunk_addr = unsafe { emit_thunk(id) }.ok_or_else(|| {
        crate::paths::log("install_hook: [4/6] emit_thunk FAIL");
        HookError::PatchFailed
    })?;
    crate::paths::log(&format!("install_hook: [4/6] thunk_addr={:#x}", thunk_addr));

    // Step 5: patch the target method's prologue to jmp to our thunk.
    // Read methodPointer from the MethodInfo struct — that's the actual code
    // address to patch. method.as_u64() is the struct ptr; methodPointer lives
    // at method + method_pointer_off (Frog's shifted layout puts it at +0x08).
    let cfg = match crate::internals::ctx::get() {
        Some(c) => &c.cfg,
        None => {
            crate::paths::log("install_hook: [5/6] no internals ctx");
            unsafe { free_thunk(thunk_addr); }
            return Err(HookError::PatchFailed);
        }
    };
    let method_ptr_addr = match crate::external::cache::read_u64(
        method.as_u64() as usize + cfg.method_pointer_off,
    ) {
        Some(p) if p != 0 => p as usize,
        _ => {
            crate::paths::log(&format!(
                "install_hook: [5/6] methodPointer at method+{:#x} unreadable or null",
                cfg.method_pointer_off
            ));
            unsafe { free_thunk(thunk_addr); }
            return Err(HookError::PatchFailed);
        }
    };
    crate::paths::log(&format!(
        "install_hook: [5/6] methodPointer={:#x} (will patch HERE)",
        method_ptr_addr
    ));

    // If the patch fails, free_thunk first to prevent a leak.
    let patch = unsafe {
        inline_detour::install(method_ptr_addr, thunk_addr)
            .ok_or_else(|| {
                crate::paths::log("install_hook: [5/6] inline_detour::install FAIL — freeing thunk");
                free_thunk(thunk_addr);
                HookError::PatchFailed
            })?
    };
    crate::paths::log(&format!("install_hook: [5/6] patched methodPointer trampoline={:#x}", patch.trampoline));

    // Step 6: build HookCtx and publish (Release store — dispatchers see it).
    let ctx = HookCtx { method, sig, thunk_addr, patch, handler_func_ref };
    unsafe { publish_slot(id, ctx); }
    crate::paths::log(&format!("install_hook: [6/6] published slot id={} — DONE", id));

    // Step 7: return the opaque handle.
    Ok(HookHandle::from_raw(id))
}

/// Remove an installed hook. Restores original bytes, frees the thunk slot.
///
/// Steps (serialised by INSTALL_GUARD):
///   1. Acquire INSTALL_GUARD.
///   2. ctx_for(handle) → UnknownHandle on miss.
///   3. Save thunk_addr BEFORE unpublish (ctx dropped by unpublish_slot).
///   4. unpublish_slot → Drops HookCtx → Drops inline_detour::Hook →
///      restores original bytes + frees trampoline.
///   5. free_thunk(saved_addr) → overwrite with int3 + return to slab.
pub fn remove_hook(handle: HookHandle) -> Result<(), HookError> {
    let _guard = INSTALL_GUARD.lock().map_err(|_| HookError::PatchFailed)?;
    let id = handle.as_u64();

    // Step 2: verify the slot is live.
    let ctx = ctx_for(id).ok_or(HookError::UnknownHandle)?;

    // Step 3: save thunk_addr BEFORE unpublish — unpublish drops the HookCtx.
    let thunk_addr = ctx.thunk_addr;

    // Step 4: unpublish → Drop restores original bytes + frees trampoline.
    unsafe { unpublish_slot(id); }

    // Step 5: overwrite thunk slot with int3 + return to slab freelist.
    unsafe { free_thunk(thunk_addr); }

    Ok(())
}

/// Read the i-th declared arg as a packed InvokeArg byte buffer. Returns the
/// number of bytes written or a negative error code.
pub fn hook_arg_read(arg_idx: usize) -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = borrow.last()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_arg outside handler"))?;
        let arg = cc.args.get(arg_idx)
            .ok_or(agent_core::spine::InvokeError::ArgCountMismatch {
                expected: cc.args.len() as u8,
                got: arg_idx as u8,
            })?;
        Ok(arg.encode())
    })
}

/// Write a new InvokeArg for the i-th declared arg. The dispatcher will
/// flush args_to_regargs at handler exit if needed; for simplicity we apply
/// immediately to current CC + the underlying RegArgs.
pub fn hook_arg_write(arg_idx: usize, bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    use crate::internals::marshal::args_to_regargs;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_arg outside handler"))?;
        let (decoded, _) = agent_core::spine::InvokeArg::decode(bytes)
            .ok_or(agent_core::spine::InvokeError::MarshalFailed { idx: arg_idx as u8, reason: "decode failed" })?;
        if arg_idx >= cc.args.len() {
            return Err(agent_core::spine::InvokeError::ArgCountMismatch {
                expected: cc.args.len() as u8,
                got: arg_idx as u8,
            });
        }
        cc.args[arg_idx] = decoded;
        let regs = unsafe { &mut *cc.regs };
        args_to_regargs(cc.method, &cc.args, regs)?;
        Ok(())
    })
}

/// For instance methods, returns the Instance pointer (the `this`). For
/// static methods, returns 0.
pub fn hook_this_get() -> u64 {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let cc = match borrow.last() { Some(x) => x, None => return 0 };
        let regs = unsafe { &*cc.regs };
        // `this` is always physical slot 0 for instance methods. We re-read
        // signature to know is_static.
        let sig = match crate::internals::marshal::read_signature(cc.method) {
            Ok(s) => s, Err(_) => return 0,
        };
        if sig.is_static { 0 } else { regs.int_args[0] }
    })
}

/// Set the explicit return value for this handler invocation. Overrides
/// any prior call_original return.
pub fn hook_set_return(bytes: &[u8]) -> Result<(), agent_core::spine::InvokeError> {
    let (decoded, _) = agent_core::spine::InvokeArg::decode(bytes)
        .ok_or(agent_core::spine::InvokeError::MarshalFailed { idx: 0, reason: "decode failed" })?;
    CURRENT.with(|c| {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()
            .ok_or(agent_core::spine::InvokeError::InternalFailure("hook_set_return outside handler"))?;
        cc.explicit_return = Some(decoded);
        Ok(())
    })
}

/// Run the trampoline with the current arg state. Returns a packed InvokeArg
/// of the return value.
pub fn call_original_now() -> Result<Vec<u8>, agent_core::spine::InvokeError> {
    use crate::internals::hook_runtime::replay::call_trampoline_with_regargs;
    use agent_core::mem_value::{ValType, Value};

    let (regs_ptr, sig_return_type, sig_return_tc, trampoline) = CURRENT.with(|c| -> Option<_> {
        let mut borrow = c.borrow_mut();
        let cc = borrow.last_mut()?;
        // Find the trampoline + sig via the registry — we look up by walking
        // until we find the slot whose method matches. Slow but bounded (256).
        for id in 0..crate::internals::hook_runtime::registry::MAX_HOOKS as u64 {
            if let Some(ctx) = ctx_for(id) {
                if ctx.method == cc.method {
                    cc.called_original = true;
                    return Some((cc.regs, ctx.sig.return_type, ctx.sig.return_tc, ctx.patch.trampoline as u64));
                }
            }
        }
        None
    }).ok_or(agent_core::spine::InvokeError::InternalFailure("no current hook"))?;

    let regs = unsafe { &mut *regs_ptr };
    unsafe { call_trampoline_with_regargs(trampoline, regs as *mut RegArgs); }

    // Pull the return out of regs and pack it. Read directly from
    // ret_int/ret_float (no +0x10 box — trampoline writes value straight to
    // those slots since this isn't going through runtime_invoke which boxes).
    let v = match sig_return_type {
        ValType::U8  => Value::U8(regs.ret_int as u8),
        ValType::U16 => Value::U16(regs.ret_int as u16),
        ValType::U32 => Value::U32(regs.ret_int as u32),
        ValType::U64 => Value::U64(regs.ret_int),
        ValType::I8  => Value::I8(regs.ret_int as i8),
        ValType::I16 => Value::I16(regs.ret_int as i16),
        ValType::I32 => Value::I32(regs.ret_int as i32),
        ValType::I64 => Value::I64(regs.ret_int as i64),
        ValType::F32 => Value::F32(regs.ret_float as f32),
        ValType::F64 => Value::F64(regs.ret_float),
        ValType::Bytes | ValType::Cstr => return Err(agent_core::spine::InvokeError::MarshalFailed {
            idx: 0,
            reason: "var-len returns not supported in v1",
        }),
    };
    let _ = sig_return_tc;
    Ok(InvokeArg::Prim(v).encode())
}
