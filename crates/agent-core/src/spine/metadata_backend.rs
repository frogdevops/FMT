//! Platform-agnostic il2cpp-metadata walk backend for `Iter<FieldInfo>` /
//! `Iter<MethodPtr>` impls on `KlassPtr`.
//!
//! Same rationale as `mem_backend`: `agent_core` knows nothing about il2cpp
//! offsets, the type-discriminator recipe, or value-type offset adjustment —
//! all of that lives in the `agent` crate (`Il2CppConfig` + `internals::api`).
//! The `agent` crate registers two function pointers here at startup; the
//! iterator state in `spine/access.rs` calls into them via a static vtable.
//!
//! # Registration
//! Call `register(fields, methods)` once from `agent`'s init path, alongside
//! `mem_backend::register`. Until registration, iterators yield zero items.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::mem_value::ValType;

/// Yield the next real `FieldInfo` of `klass` starting at physical slot
/// `cursor`, or `None` if iteration should stop. Implementations must apply
/// the same garbage filters as the legacy `for_each_field` walk (token != 0,
/// type_ptr decodes to a valid tc, value-type offset adjustment) so the
/// iterator doesn't surface scanner artefacts.
///
/// The backend advances internally past garbage entries: the returned
/// `next_cursor` is the physical slot index just past the returned record,
/// which the iterator uses for the subsequent call. Returning `None`
/// terminates iteration (real end-of-array sentinel: name_ptr == 0).
pub type FieldsFn = fn(klass: usize, cursor: usize) -> Option<FieldInfoRaw>;

/// Yield the `cursor`-th `MethodPtr` of `klass`, or `None` if iteration should
/// stop (end of array, klass back-pointer mismatch, etc.). Returns the raw
/// `MethodInfo*` as a `u64`.
pub type MethodsFn = fn(klass: usize, cursor: usize) -> Option<u64>;

/// Raw FieldInfo payload returned by the agent-side backend. Mirrors the
/// `spine::FieldInfo` fields but is defined here to avoid a circular module
/// dependency through `spine::access`. `next_cursor` is the physical slot
/// index the iterator should pass on its next call (the backend may have
/// skipped garbage internally, so this is NOT necessarily `cursor + 1`).
#[derive(Debug, Clone, Copy)]
pub struct FieldInfoRaw {
    pub name_ptr:    usize,
    pub offset:      u32,
    pub val_type:    ValType,
    pub token:       u32,
    pub next_cursor: usize,
}

static FIELDS_FN:  AtomicUsize = AtomicUsize::new(0);
static METHODS_FN: AtomicUsize = AtomicUsize::new(0);

/// Register the il2cpp-metadata walk functions. Call once at agent start,
/// after `ctx::init` (the backends read `ctx::get()` for the config offsets).
pub fn register(fields: FieldsFn, methods: MethodsFn) {
    FIELDS_FN .store(fields  as usize, Ordering::Release);
    METHODS_FN.store(methods as usize, Ordering::Release);
}

/// Invoke the registered fields backend. Returns `None` if not registered or
/// the backend itself returned `None`.
pub fn fields_at(klass: usize, cursor: usize) -> Option<FieldInfoRaw> {
    let p = FIELDS_FN.load(Ordering::Acquire);
    if p == 0 { return None; }
    let f: FieldsFn = unsafe { std::mem::transmute(p) };
    f(klass, cursor)
}

/// Invoke the registered methods backend. Returns `None` if not registered or
/// the backend signalled end-of-array.
pub fn methods_at(klass: usize, cursor: usize) -> Option<u64> {
    let p = METHODS_FN.load(Ordering::Acquire);
    if p == 0 { return None; }
    let f: MethodsFn = unsafe { std::mem::transmute(p) };
    f(klass, cursor)
}
