/// Version-adaptive il2cpp struct offsets.
///
/// Every offset is determined once, at startup, by detecting the il2cpp metadata
/// version.  This file contains the known version→layout map.  When a version is
/// unknown the runtime‑detection fallback estimates the layout heuristically.
///
/// v24 group (Unity 2017–2019) is the reference used for the default layout.
///
/// ## Version → Unity mapping
/// | Metadata version | Unity versions              |
/// |------------------|-----------------------------|
/// | v16–v23          | 5.x–2017.x                  |
/// | v24              | 2018.x–2019.4               |
/// | v27              | 2020.2–2020.3               |
/// | v29              | 2021.3                      |
/// | v30              | 2022.3                      |
/// | v31              | 6000.x                      |
///
/// ## Struct stability notes
/// The Il2CppClass fields we care about (image, name, namespace, parent, fields)
/// are at **identical offsets** across all versions ≥ v24.  The only layout change
/// that affects our config is the `byval_arg` / `this_arg` fields in Il2CppClass:
///   - v24–v29: `byval_arg` / `this_arg` are inline `Il2CppType` (16 bytes each) at
///              +0x20 / +0x30
///   - v30+:    `byval_arg` / `this_arg` are `Il2CppType*` (pointer, 8 bytes each)
///              at +0x20 / +0x28
/// When they become pointers the type-definition handle moves to the
/// `typeDefinition` / `typeMetadataHandle` field at +0x68, and every field at
/// +0x30 or later shifts forward by 16 bytes.

#[derive(Debug, Clone)]
pub struct Il2CppConfig {
    // ── s_TypeInfoTable ──────────────────────────────────────────
    /// Byte step between consecutive slot pointers in the class table.
    pub class_table_step: usize,        // almost always 8 on x64

    // ── Il2CppClass (klass) ──────────────────────────────────────
    /// Offset from klass to the `namespaze` (`const char*`) field.
    pub klass_namespace: usize,
    /// Offset from klass to a 64‑bit value that uniquely identifies the
    /// type definition for this klass.
    ///
    /// For v24–v29 this is the start of the inline `byval_arg.data` (a
    /// packed encoding of the type‑definition index).  For v30+ the
    /// value lives in the `typeDefinition` / `typeMetadataHandle` field
    /// at +0x68 because `byval_arg` has become a pointer.
    pub klass_type_def: usize,
    /// Offset from klass to the `Il2CppGenericClass*` pointer.  Used to
    /// read the generic context (concrete type arguments) when resolving
    /// VAR/MVAR generic parameters to their instantiated types.
    ///
    ///   v24–v29: +0x48 (after element_class at +0x40)
    ///   v30+:    +0x38 (shifted by -16 because byval/this are pointers)
    pub klass_generic_class: usize,
    /// Offset from klass to the `FieldInfo*` pointer (the `fields` array
    /// used by the memory-walk fallback).  +0x80 is stable for v24–v30;
    /// v30+ shifts this to +0x70.
    pub klass_fields: usize,

    // ── Il2CppType ───────────────────────────────────────────────
    /// Byte offset to read an 8‑byte (u64) chunk from an Il2CppType
    /// that contains the Il2CppTypeEnum discriminator somewhere inside.
    pub il2cpp_type_discrim_read_at: usize,
    /// Number of bits to right‑shift the chunk so that the lowest byte
    /// becomes the discriminator.
    pub discrim_shift: u8,
}

impl Il2CppConfig {
    /// Default layout for the v24 metadata group (Unity 2017–2019).
    /// These offsets work for the majority of il2cpp games from that era.
    pub const fn default() -> Self {
        Self::v24()
    }

    /// Return a config for a known metadata version, or `None` if the
    /// version is not yet supported.  When `None` the caller should
    /// fall back to `Il2CppConfig::default()` (which will work for
    /// most Unity 2017–2020 games).
    pub fn for_metadata_version(version: u32) -> Option<Self> {
        match version {
            // v24 is the baseline — covers Unity 2017–2019
            24 | 25 | 26 => Some(Self::v24()),
            // v27 (Unity 2020.x) — same Il2CppClass layout as v24
            27 | 28 => Some(Self::v27()),
            // v29 (Unity 2021.3) — still uses inline byval_arg at +0x20
            29 => Some(Self::v29()),
            // v30+ (Unity 2022+) — byval_arg becomes a pointer;
            // klass_type_def moves to typeDefinition at +0x68.
            30 | 31 => Some(Self::v30()),
            _ => None,
        }
    }

    // ── Version instances ────────────────────────────────────────

    /// v24 baseline — covers Unity 2017–2019 (metadata v24–v26).
    ///
    /// Il2CppClass inline layout:
    ///   +0x00  image           (void*)
    ///   +0x08  gc_desc         (void*)
    ///   +0x10  name            (const char*)
    ///   +0x18  namespaze       (const char*)
    ///   +0x20  byval_arg       (Il2CppType, 16 bytes inline)
    ///   +0x30  this_arg        (Il2CppType, 16 bytes inline)
    ///   …
    ///   +0x58  parent          (Il2CppClass*)
    ///   +0x68  typeDefinition  (void*)
    ///
    /// Il2CppType:
    ///   +0x00  data            (8 bytes — packed typeDefIndex + flags)
    ///   +0x08  attrs / type    (u32 — discriminator at byte 2)
    const fn v24() -> Self {
        Self {
            class_table_step:            8,
            klass_namespace:             0x18,
            klass_type_def:              0x20,   // byval_arg.data (inline)
            klass_generic_class:         0x48,
            klass_fields:                0x80,
            il2cpp_type_discrim_read_at: 0x08,
            discrim_shift:               16,
        }
    }

    /// v27 — Unity 2020.x (metadata v27–v28).
    ///
    /// Identical Il2CppClass layout to v24.  No runtime struct changes.
    const fn v27() -> Self {
        Self::v24()
    }

    /// v29 — Unity 2021.3 (metadata v29).
    ///
    /// Identical Il2CppClass layout to v24.  The `typeDefinition` field
    /// was renamed `typeMetadataHandle` but remains at +0x68 and
    /// `byval_arg` is still inline at +0x20.
    const fn v29() -> Self {
        Self::v24()
    }

    /// v30 — Unity 2022.x (metadata v30–v31).
    ///
    /// **Known change**: `byval_arg` / `this_arg` became pointers
    /// (`Il2CppType*`) instead of inline structs (16 bytes → 8 bytes
    /// each).  This shifts every field at +0x30 + by −16 bytes.
    ///   - `klass_type_def` → +0x68 (typeDefinition / typeMetadataHandle)
    ///   - `klass_generic_class` → +0x38 (was +0x48)
    ///   - `klass_fields` → +0x70 (was +0x80)
    ///
    /// ⚠  All v30+ offsets are deduced from the size change, not
    /// empirically verified.  If you hit a metadata v30+ game and
    /// output is wrong, these are the offsets to check first.
    const fn v30() -> Self {
        Self {
            class_table_step:            8,
            klass_namespace:             0x18,
            klass_type_def:              0x68,   // typeDefinition / typeMetadataHandle
            klass_generic_class:         0x38,   // guessed (shifted by −16)
            klass_fields:                0x70,   // guessed (shifted by −16)
            il2cpp_type_discrim_read_at: 0x08,
            discrim_shift:               16,
        }
    }
}
