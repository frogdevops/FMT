//! Per-field metadata yielded by `Iter<FieldInfo> for KlassPtr`. Lightweight,
//! `Copy`, decoupled from FFI — the iterator reads the structural offsets via
//! agent-side primitives, then yields these descriptors.

use crate::mem_value::ValType;

/// One il2cpp field's metadata (offset within the parent instance or static
/// block + declared value type + metadata token + name-pointer for lazy
/// resolution).
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
    /// `true` if the field is declared `static`. Static fields' `offset` is
    /// relative to the class's `static_fields` base (not an instance), so the
    /// dump and any composing caller MUST handle them distinctly from instance
    /// fields. Populated by `metadata_backend::fields_at` reading the
    /// FIELD_ATTRIBUTE_STATIC (0x10) bit on the field's type-attrs chunk.
    pub is_static: bool,
    /// Raw `Il2CppType*` for this field (the pointer at field-slot+8). Carried
    /// so the dumper can resolve full human-readable type names (generics,
    /// arrays) via the existing type-name resolver. 0 if unavailable.
    pub type_ptr: usize,
}
