//! Per-field metadata yielded by `Iter<FieldInfo> for KlassPtr`. Lightweight,
//! `Copy`, decoupled from FFI — the iterator reads the structural offsets via
//! agent-side primitives, then yields these descriptors.

use crate::mem_value::ValType;

/// One il2cpp instance-field's metadata (offset within the parent instance +
/// declared value type + metadata token + name-pointer for lazy resolution).
///
/// `name_ptr` is the raw address of the field's NUL-terminated name in the
/// string heap. Callers that need the name decode it via `RegionMap::read_name`
/// (agent-side); keeping it as a raw pointer means iteration doesn't allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldInfo {
    pub name_ptr: usize,
    pub offset:   u32,
    pub val_type: ValType,
    pub token:    u32,
}
