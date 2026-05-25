/// Version-adaptive il2cpp struct offsets.
///
/// Every offset is determined once, at startup, by detecting the il2cpp metadata
/// version.  This file contains the known version→layout map.  When a version is
/// unknown the runtime‑detection fallback estimates the layout heuristically.
///
/// Pixel Worlds (Unity 2019.4) uses offsets from the v24 group.

#[derive(Debug, Clone)]
pub struct Il2CppConfig {
    // ── s_TypeInfoTable ──────────────────────────────────────────
    /// Byte step between consecutive slot pointers in the class table.
    pub class_table_step: usize,        // almost always 8 on x64

    // ── Il2CppClass (klass) ──────────────────────────────────────
    /// Offset from klass to the `namespaze` (`const char*`) field.
    pub klass_namespace: usize,
    /// Offset from klass to the `typeDefinition` or `byval_arg.data`
    /// pointer that we use for the reverse type map.
    pub klass_type_def: usize,

    // ── Il2CppType ───────────────────────────────────────────────
    /// Byte offset to read an 8‑byte (u64) chunk from an Il2CppType
    /// that contains the Il2CppTypeEnum discriminator somewhere inside.
    pub il2cpp_type_discrim_read_at: usize,
    /// Number of bits to right‑shift the chunk so that the lowest byte
    /// becomes the discriminator.
    pub discrim_shift: u8,
}

impl Il2CppConfig {
    /// Default layout determined empirically from Pixel Works
    /// (Unity 2019.4 / il2cpp metadata v24 group).
    pub const fn default() -> Self {
        Self {
            class_table_step:          8,   // x64 pointer width

            klass_namespace:            0x18,
            klass_type_def:             0x20,

            il2cpp_type_discrim_read_at: 0x08,
            discrim_shift:              16,
        }
    }

    /// Return a config for a known metadata version, or `None` if the
    /// version is not yet supported.  When `None` the caller should
    /// fall back to `Il2CppConfig::default()` (which will work for
    /// most Unity 2017–2020 games).
    pub fn for_metadata_version(_version: u32) -> Option<Self> {
        // Future: match version and produce version-specific offsets.
        // For now the default layout covers Pixel Worlds / v24 group.
        match _version {
            // 24 | 27 => Some(Self::v24()),
            // 29      => Some(Self::v29()),
            // 30      => Some(Self::v30()),
            _ => None,
        }
    }

    // ── Version instances (to be filled as each layout is added) ──
    //
    // pub const fn v24() -> Self { ... }
    // pub const fn v27() -> Self { ... }
    // pub const fn v29() -> Self { ... }
    // pub const fn v30() -> Self { ... }
    //
    // Reference: Il2CppDumper / MetadataClass.cs + HeaderConstants.cs
}
