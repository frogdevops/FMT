//! Platform-agnostic scan-based instance-discovery backend for
//! `Iter<Instance> for KlassPtr`.
//!
//! Distinct from `metadata_backend` because instance discovery has different
//! lifecycle semantics from structural field/method walks: it depends on
//! live process state (heap layout, allocator state) rather than calibrated
//! il2cpp offsets. Same registration pattern; different vtable.
//!
//! # Registration
//! Call `register(next_match, validate)` once from `agent`'s init path,
//! alongside `mem_backend::register` and `metadata_backend::register`.
//! Until registration, iterators yield zero items.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Yield the next candidate address that matches the byte signature of
/// `target_klass` (klass pointer at offset 0), advancing `cursor` to the
/// position just past the returned hit. Returns `None` when no more matches
/// exist within the scannable range.
///
/// Implementation note: backends typically perform an AOB scan on first
/// call, cache results, and stream them on subsequent calls — but the
/// caller (the iterator) treats this opaquely.
pub type NextMatchFn = fn(target_klass: usize, cursor: &mut usize) -> Option<usize>;

/// Structural validation: returns `true` iff `addr` passes ALL of these
/// universal checks (no per-klass branching):
///   1. address is in a writable memory region
///   2. address is aligned to pointer size (8 on x86_64)
///   3. `klass_of(addr) == target_klass`
///   4. the klass at `addr+0` passes `is_klass_shape` (name + namespace
///      pointers are valid cstrs in mapped memory)
///
/// Returning `false` causes the iterator to skip the candidate silently
/// (no log spam) and try the next match.
pub type ValidateFn = fn(addr: usize, target_klass: usize) -> bool;

static NEXT_MATCH_FN: AtomicUsize = AtomicUsize::new(0);
static VALIDATE_FN:   AtomicUsize = AtomicUsize::new(0);

/// Register the scan-discovery backend. Call once at agent start, after
/// `ctx::init` and `register_mem_backend()` (the backend's validation reads
/// the region cache + klass_of).
pub fn register(next_match: NextMatchFn, validate: ValidateFn) {
    NEXT_MATCH_FN.store(next_match as usize, Ordering::Release);
    VALIDATE_FN  .store(validate   as usize, Ordering::Release);
}

/// Invoke the registered next-match backend. Returns `None` if not
/// registered or the backend itself signalled end-of-scan.
pub fn next_match(target_klass: usize, cursor: &mut usize) -> Option<usize> {
    let p = NEXT_MATCH_FN.load(Ordering::Acquire);
    if p == 0 { return None; }
    let f: NextMatchFn = unsafe { std::mem::transmute(p) };
    f(target_klass, cursor)
}

/// Invoke the registered validation backend. Returns `false` if not
/// registered (fail-closed: no candidate is "valid" until backend exists).
pub fn validate(addr: usize, target_klass: usize) -> bool {
    let p = VALIDATE_FN.load(Ordering::Acquire);
    if p == 0 { return false; }
    let f: ValidateFn = unsafe { std::mem::transmute(p) };
    f(addr, target_klass)
}
