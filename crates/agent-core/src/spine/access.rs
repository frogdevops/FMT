//! Capability-disciplined access traits: `Read<T>` / `Write<T>` / `Iter<T>`.
//! Spans the three Spec-2 domains — a handle's type DECLARES its capabilities,
//! and scripts compose via trait bounds (`fn f<H: Read<u32>>(h: H)`).
//!
//! YAGNI discipline: one method per trait. Batch reads / CAS / offset variants
//! get added when a real caller demands. The existing typed `_t` free functions
//! become one-line façades calling these trait methods.
//!
//! # Trait impls for `MemAddr` and `FieldAddr`
//!
//! `Read<T> for MemAddr<C>` and `Write<T> for MemAddr<ReadWrite>` live here
//! (not in the `agent` crate) because the orphan rule requires at least one of
//! the trait or the type to be local to the implementing crate. Both
//! `Read`/`Write` and `MemAddr` are defined in this crate, so the impls belong
//! here. The actual platform I/O (region-cache validation on Windows, guarded
//! VirtualProtect write) is injected via the `mem_backend` static vtable that
//! the `agent` crate registers at startup.

use crate::spine::value::MemValue;
use crate::spine::error::MemError;
use crate::spine::addr::{MemAddr, ReadWrite};
use crate::spine::field_info::FieldInfo;
use crate::spine::handles::{FieldAddr, Instance, KlassPtr, MethodPtr};
use crate::spine::{mem_backend, metadata_backend};

/// Defensive cap: real il2cpp classes never have this many fields. Mirrors
/// the `for_each_field` cap in `agent::internals::api`.
const MAX_FIELDS_PER_CLASS: usize = 256;

/// Defensive cap: real il2cpp classes never have this many methods. Mirrors
/// the `find_method` cap in `agent::internals::api`.
const MAX_METHODS_PER_CLASS: usize = 4096;

/// Read a typed value of `T` from this handle.
pub trait Read<T: MemValue> {
    fn read(&self) -> Result<T, MemError>;
}

/// Write a typed value of `T` through this handle. Capability-disciplined:
/// only handles whose impl explicitly opts in are writable. `MemAddr<ReadOnly>`
/// has no `Write<T>` impl, so `read_only.write(...)` won't compile.
pub trait Write<T: MemValue> {
    fn write(&self, value: T) -> Result<(), MemError>;
}

/// Lazily iterate items of type `T`. The associated `Iter` type lets impls
/// define their own state struct without allocating a Vec. Items are NOT
/// bounded by `MemValue` — iterators can yield handles (e.g.
/// `Iter<FieldInfo> for KlassPtr`) or other domain types.
pub trait Iter<T> {
    type Iter: Iterator<Item = T>;
    fn iter(&self) -> Self::Iter;
}

// ── Trait impls — the load-bearing capability surface ───────────────────────

/// `Read<T>` works for any capability (ReadOnly and ReadWrite alike).
impl<T: MemValue, C> Read<T> for MemAddr<C> {
    fn read(&self) -> Result<T, MemError> {
        let width = T::VAL_TYPE.fixed_width().ok_or(MemError::BadType)?;
        let a = self.as_u64() as usize;
        let mut buf = vec![0u8; width];
        let ok = unsafe { mem_backend::raw_read(a, buf.as_mut_ptr(), width) };
        if !ok {
            return Err(MemError::Unreadable);
        }
        T::from_le_bytes_spine(&buf).ok_or(MemError::BadType)
    }
}

/// `Write<T>` requires `MemAddr<ReadWrite>` — `MemAddr<ReadOnly>` has no impl,
/// so passing a read-only handle is a compile-time error.
impl<T: MemValue> Write<T> for MemAddr<ReadWrite> {
    fn write(&self, value: T) -> Result<(), MemError> {
        let bytes = value.to_le_bytes_buf();
        let a = self.as_u64() as usize;
        let ok = unsafe { mem_backend::raw_write(a, bytes.as_ptr(), bytes.len()) };
        if ok { Ok(()) } else { Err(MemError::Unwritable) }
    }
}

/// `Read<T>` for `FieldAddr` adds a runtime type-mismatch guard.
impl<T: MemValue> Read<T> for FieldAddr {
    fn read(&self) -> Result<T, MemError> {
        if T::VAL_TYPE != self.val_type {
            return Err(MemError::BadType);
        }
        Read::<T>::read(&self.addr)
    }
}

/// `Write<T>` for `FieldAddr` adds a runtime type-mismatch guard.
impl<T: MemValue> Write<T> for FieldAddr {
    fn write(&self, value: T) -> Result<(), MemError> {
        if T::VAL_TYPE != self.val_type {
            return Err(MemError::BadType);
        }
        Write::<T>::write(&self.addr, value)
    }
}

// ── KlassPtr iteration: fields + methods ────────────────────────────────────

/// Lightweight (3-usize, `Copy`) iterator state for `Iter<FieldInfo> for
/// KlassPtr`. The actual walk — config offsets, tc decoding, value-type
/// adjustment, garbage filtering — lives in the agent crate behind the
/// `metadata_backend::FieldsFn` vtable.
#[derive(Debug, Clone, Copy)]
pub struct FieldInfoIter {
    klass:  usize,
    cursor: usize,
    limit:  usize,
}

impl Iterator for FieldInfoIter {
    type Item = FieldInfo;

    fn next(&mut self) -> Option<FieldInfo> {
        if self.cursor >= self.limit {
            return None;
        }
        let raw = metadata_backend::fields_at(self.klass, self.cursor)?;
        // Backend skips garbage internally and tells us where to resume.
        // Guard against backends that fail to advance (would infinite-loop).
        if raw.next_cursor <= self.cursor {
            return None;
        }
        self.cursor = raw.next_cursor;
        Some(FieldInfo {
            name_ptr:  raw.name_ptr,
            offset:    raw.offset,
            val_type:  raw.val_type,
            token:     raw.token,
            is_static: raw.is_static,
            type_ptr:  raw.type_ptr,
        })
    }
}

impl Iter<FieldInfo> for KlassPtr {
    type Iter = FieldInfoIter;
    fn iter(&self) -> Self::Iter {
        FieldInfoIter {
            klass:  self.as_u64() as usize,
            cursor: 0,
            limit:  MAX_FIELDS_PER_CLASS,
        }
    }
}

/// Lightweight (3-usize, `Copy`) iterator state for `Iter<MethodPtr> for
/// KlassPtr`. Stops on backend `None` (end-of-array via klass back-pointer
/// sentinel) or on hitting the defensive `MAX_METHODS_PER_CLASS` cap.
#[derive(Debug, Clone, Copy)]
pub struct MethodPtrIter {
    klass:  usize,
    cursor: usize,
    limit:  usize,
}

impl Iterator for MethodPtrIter {
    type Item = MethodPtr;

    fn next(&mut self) -> Option<MethodPtr> {
        if self.cursor >= self.limit {
            return None;
        }
        let raw = metadata_backend::methods_at(self.klass, self.cursor)?;
        self.cursor += 1;
        Some(MethodPtr::from_raw(raw))
    }
}

impl Iter<MethodPtr> for KlassPtr {
    type Iter = MethodPtrIter;
    fn iter(&self) -> Self::Iter {
        MethodPtrIter {
            klass:  self.as_u64() as usize,
            cursor: 0,
            limit:  MAX_METHODS_PER_CLASS,
        }
    }
}

// ── KlassPtr instance iteration via scan_backend ────────────────────────────

/// Lightweight (2-usize, `Copy`) iterator state for `Iter<Instance> for
/// KlassPtr`. The actual scan + structural validation lives in the agent
/// crate behind the `scan_backend::{NextMatchFn, ValidateFn}` vtable.
///
/// `cursor` is an OPAQUE, backend-owned cursor (e.g. an index into the
/// backend's cached scan results), NOT a memory address. The iterator holds
/// it by value and passes `&mut self.cursor` so the backend can advance it
/// in place across calls. This intentionally differs from the metadata
/// backend's value-in/value-out cursor convention because scan results are
/// streamed from a backend-side cache.
///
/// Per the B-6b spec: validation is UNIVERSAL/structural (no per-klass logic).
/// Backend's `next_match` yields candidates; `validate` filters out non-instance
/// coincidental matches via region check + alignment + klass_of + klass-shape.
#[derive(Debug, Clone, Copy)]
pub struct InstanceIter {
    klass:  usize,
    cursor: usize,
}

impl Iterator for InstanceIter {
    type Item = Instance;

    fn next(&mut self) -> Option<Instance> {
        loop {
            let before = self.cursor;
            let candidate = crate::spine::scan_backend::next_match(
                self.klass,
                &mut self.cursor,
            )?;
            // Liveness guard: a well-behaved backend MUST advance `cursor` on a
            // `Some(_)` return. If it didn't, terminate rather than spin forever
            // — this iterator runs on the game thread (game frozen), so a
            // non-advancing backend would be a hard freeze, not slow output.
            // Mirrors FieldInfoIter's non-advancing cursor guard.
            if self.cursor <= before {
                return None;
            }
            if crate::spine::scan_backend::validate(candidate, self.klass) {
                return Some(Instance::from_raw(candidate as u64));
            }
            // Validation failed: try the next candidate (silent skip, no log spam).
        }
    }
}

impl Iter<Instance> for KlassPtr {
    type Iter = InstanceIter;
    fn iter(&self) -> Self::Iter {
        InstanceIter {
            klass:  self.as_u64() as usize,
            cursor: 0,
        }
    }
}

#[cfg(test)]
mod instance_iter_tests {
    use super::*;
    use crate::spine::scan_backend;
    use std::sync::Once;

    static INIT: Once = Once::new();

    // Klass sentinels selecting a scenario from the single shared mock backend.
    // (All test modules in this crate share the same global scan_backend
    // statics, so we parameterize ONE backend by target_klass rather than
    // registering several.)
    const KLASS_MIXED:   usize = 0xDEAD_BEEF; // 4 candidates, 2 valid
    const KLASS_EMPTY:   usize = 0xE0;        // backend yields nothing
    const KLASS_ALL_BAD: usize = 0xBAD;       // 4 candidates, none valid

    fn test_next_match(target: usize, cursor: &mut usize) -> Option<usize> {
        let candidates: &[usize] = match target {
            KLASS_EMPTY => &[],
            _           => &[0x1000, 0x2000, 0x3000, 0x4000],
        };
        if *cursor >= candidates.len() {
            return None;
        }
        let v = candidates[*cursor];
        *cursor += 1;
        Some(v)
    }

    fn test_validate(addr: usize, target: usize) -> bool {
        match target {
            KLASS_ALL_BAD => false,
            _             => addr == 0x1000 || addr == 0x3000,
        }
    }

    fn init_test_backend() {
        INIT.call_once(|| {
            scan_backend::register(test_next_match, test_validate);
        });
    }

    #[test]
    fn instance_iter_yields_only_validated_candidates() {
        init_test_backend();
        let klass = KlassPtr::from_raw(KLASS_MIXED as u64);
        let yielded: Vec<u64> =
            <KlassPtr as Iter<Instance>>::iter(&klass).map(|i| i.as_u64()).collect();
        assert_eq!(yielded, vec![0x1000, 0x3000]);
    }

    #[test]
    fn instance_iter_terminates_when_backend_returns_none() {
        init_test_backend();
        let klass = KlassPtr::from_raw(KLASS_MIXED as u64);
        let count = <KlassPtr as Iter<Instance>>::iter(&klass).count();
        assert_eq!(count, 2);
    }

    #[test]
    fn instance_iter_empty_scan_yields_nothing() {
        init_test_backend();
        let klass = KlassPtr::from_raw(KLASS_EMPTY as u64);
        let count = <KlassPtr as Iter<Instance>>::iter(&klass).count();
        assert_eq!(count, 0);
    }

    #[test]
    fn instance_iter_all_invalid_terminates_cleanly() {
        init_test_backend();
        let klass = KlassPtr::from_raw(KLASS_ALL_BAD as u64);
        // All 4 candidates fail validation; iterator must terminate (the backend
        // advances the cursor each call), yielding zero — not spin forever.
        let count = <KlassPtr as Iter<Instance>>::iter(&klass).count();
        assert_eq!(count, 0);
    }
}
