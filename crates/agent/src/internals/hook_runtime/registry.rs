//! Lock-free hot-path hook registry.
//!
//! INVARIANT: id ∈ [0, MAX_HOOKS); same id is used for:
//!   - thunk_slot embedded id (writeable in H5)
//!   - HOOK_SLOTS[id]      (this file)
//!   - REENTRY[id]         (this file)
//!   - HookHandle::from_raw(id) (script-visible)
//!
//! Hot path (`dispatch_rust`):
//!   if !SLOT_VALID[id].load(Acquire) { return; }
//!   let ctx = unsafe { (*HOOK_SLOTS[id].get()).assume_init_ref() };
//!   // ... use ctx, never touches INSTALL_GUARD
//!
//! Install/remove:
//!   let _guard = INSTALL_GUARD.lock().unwrap();  // serialize allocation
//!   write to HOOK_SLOTS[id]; publish via SLOT_VALID[id].store(true, Release);

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::AtomicBool;
use std::sync::Mutex;

use agent_core::spine::MethodPtr;

use crate::inline_detour::Hook;
use crate::internals::marshal::MethodSignature;

pub const MAX_HOOKS: usize = 256;

pub struct HookCtx {
    pub method:     MethodPtr,
    pub sig:        MethodSignature,
    pub thunk_addr: usize,
    /// The `inline_detour::Hook` — owns the trampoline + stolen-bytes restore.
    /// Kept here so removal Drop-restores the original prologue.
    pub patch:      Hook,
    /// wasmi::Func — resolved at install time, called from dispatcher.
    /// Stored as raw bits to keep this struct Send/Sync; see api.rs for
    /// the safe wrapper.
    pub handler_func_ref: u64,
}

// SAFETY: ctx is only read while SLOT_VALID[id] is Acquire-true. Writers
// hold INSTALL_GUARD. The UnsafeCell allows the publish/unpublish dance.
pub struct SlotCell(pub UnsafeCell<MaybeUninit<HookCtx>>);
unsafe impl Sync for SlotCell {}

static SLOT_VALID: [AtomicBool; MAX_HOOKS] = {
    // const-init array of AtomicBool. Each is false (unset).
    const FALSE: AtomicBool = AtomicBool::new(false);
    [FALSE; MAX_HOOKS]
};

static REENTRY: [AtomicBool; MAX_HOOKS] = {
    const FALSE: AtomicBool = AtomicBool::new(false);
    [FALSE; MAX_HOOKS]
};

// SAFETY: SLOT_VALID gates reads. Writers hold INSTALL_GUARD.
#[allow(clippy::declare_interior_mutable_const)]
static HOOK_SLOTS: [SlotCell; MAX_HOOKS] = {
    const EMPTY: SlotCell = SlotCell(UnsafeCell::new(MaybeUninit::uninit()));
    [EMPTY; MAX_HOOKS]
};

pub static INSTALL_GUARD: Mutex<()> = Mutex::new(());

/// Hot-path lookup. Zero locks. Returns `None` if the slot is unpublished
/// or the id is out of range.
pub fn ctx_for(method_id: u64) -> Option<&'static HookCtx> {
    let id = method_id as usize;
    if id >= MAX_HOOKS { return None; }
    if !SLOT_VALID[id].load(core::sync::atomic::Ordering::Acquire) { return None; }
    // SAFETY: SLOT_VALID is Acquire-true and remains true until remove_hook
    // (which holds INSTALL_GUARD) clears it. The returned reference is valid
    // for the duration of the dispatcher call.
    Some(unsafe { (*HOOK_SLOTS[id].0.get()).assume_init_ref() })
}

/// Find a free slot id. Must be called under INSTALL_GUARD.
pub fn alloc_slot() -> Option<u64> {
    for id in 0..MAX_HOOKS {
        if !SLOT_VALID[id].load(core::sync::atomic::Ordering::Relaxed) {
            return Some(id as u64);
        }
    }
    None
}

/// Publish a HookCtx into the slot. Caller must hold INSTALL_GUARD.
pub unsafe fn publish_slot(id: u64, ctx: HookCtx) {
    let i = id as usize;
    (*HOOK_SLOTS[i].0.get()).write(ctx);
    SLOT_VALID[i].store(true, core::sync::atomic::Ordering::Release);
}

/// Unpublish a slot (caller must hold INSTALL_GUARD). Drops the HookCtx
/// (which Drops the inline_detour::Hook, which restores original bytes).
pub unsafe fn unpublish_slot(id: u64) {
    let i = id as usize;
    // Release the slot first so no new dispatch can read it.
    SLOT_VALID[i].store(false, core::sync::atomic::Ordering::Release);
    // Drop the HookCtx in place.
    let cell = &mut *HOOK_SLOTS[i].0.get();
    cell.assume_init_drop();
    // Clear reentry just in case.
    REENTRY[i].store(false, core::sync::atomic::Ordering::Release);
}

/// Try to mark this method as reentrant — returns `true` if we were ALREADY
/// inside the handler (the caller should run the trampoline directly and skip
/// wasm). Returns `false` (and sets the flag) if we're entering fresh.
pub fn try_enter_reentry(id: u64) -> bool {
    let i = id as usize;
    if i >= MAX_HOOKS { return true; }   // unknown id: be conservative
    REENTRY[i].swap(true, core::sync::atomic::Ordering::AcqRel)
}

pub fn clear_reentry(id: u64) {
    let i = id as usize;
    if i >= MAX_HOOKS { return; }
    REENTRY[i].store(false, core::sync::atomic::Ordering::Release);
}
