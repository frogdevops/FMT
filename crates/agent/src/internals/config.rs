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

    // ── Il2CppClass method / static-field accessors ───────────────
    /// klass → MethodInfo** array (the class's own methods). = klass_fields + 0x18.
    pub klass_methods: usize,
    /// klass → static-fields storage base pointer. = klass_fields + 0x38.
    pub klass_static_fields: usize,
    /// MethodInfo → name (const char*).
    pub method_name_off: usize,
    /// MethodInfo → declaring klass (Il2CppClass*); doubles as array-end sentinel.
    pub method_klass_off: usize,
    /// MethodInfo → parameters_count (u8).
    pub method_param_count_off: usize,

    /// Offset of the byte containing the valuetype bit in Il2CppClass.byval_arg.
    /// Derived structurally via diagnostics::valuetype_probe (cross-validated on
    /// 5 value types + 4 reference types; 0x2B/0x80 = bit 31 of the Il2CppType
    /// bitfield, the valuetype bit per standard il2cpp ABI).
    pub klass_valuetype_off: usize,
    pub klass_valuetype_bit: u8,

    // ── MethodInfo / ParameterInfo ───────────────────────────────
    /// Offset of `methodPointer` (the actual native function code pointer)
    /// within MethodInfo.  Standard il2cpp puts this at +0x00 but Frog's
    /// probe confirmed a +0x08 shift for both v24 (Pixel Worlds) and v30
    /// (Highrise) — so we always patch +0x08, never the struct base.
    pub method_pointer_off: usize,
    /// Offset of the `return_type` ptr (→ Il2CppType*) within MethodInfo.
    /// Derived structurally via diagnostics::methodinfo_probe.
    pub method_return_type_off: usize,
    /// Offset of the `parameters` ptr (→ ParameterInfo[]) within MethodInfo.
    pub method_parameters_off: usize,
    /// Offset of the `flags` u32 within MethodInfo. METHOD_ATTRIBUTE_STATIC = 0x10.
    pub method_flags_off: usize,
    /// Stride between ParameterInfo entries. Probed via PadLeft(Int32, Char).
    pub param_info_size: usize,
    /// Offset of the type ptr within ParameterInfo. Probed: type is the FIRST field.
    pub param_info_type_off: usize,
}

/// Minimum number of anchors required for a probe to OVERRIDE the fallback constant.
/// Below this threshold the probe outcome is logged but the field is kept at the
/// verified-correct prior (v24 baseline).  Two anchors (e.g. Math.Pow +
/// String.PadLeft) can produce a spurious perfect-score on a sparse/mis-loaded
/// table; three forces at least minimal triangulation.
const MIN_OVERRIDE_ANCHORS: u32 = 3;

/// Apply a probe outcome to a config field. The fallback constant is a
/// VERIFIED-CORRECT prior; a probe may only OVERRIDE it when it produced a
/// winning offset AND gathered enough independent anchors (≥ MIN_OVERRIDE_ANCHORS).
/// Every override that DIFFERS from the prior is logged loudly BEFORE it is
/// applied, so the calibration trail makes clear when (and why) the live runtime
/// diverged from the baseline. When the probe fell back, or when the anchor count
/// is below the threshold, the field is left at its prior value (the floor).
fn apply_offset(field: &mut usize, outcome: &crate::internals::calibration::ProbeOutcome) {
    if let Some(off) = outcome.winning_offset {
        if outcome.anchor_count < MIN_OVERRIDE_ANCHORS {
            // Too few anchors — log as weak and keep the verified-correct fallback.
            crate::paths::log(&format!(
                "⚠ PROBE WEAK: {} probed={:#x} (only {}/{}) — keeping fallback {:#x}",
                outcome.field_name, off,
                outcome.match_count, outcome.anchor_count, *field));
            return;
        }
        if off != *field {
            crate::paths::log(&format!(
                "⚠ PROBE OVERRIDE: {} fallback={:#x} → probed={:#x} (match {}/{})",
                outcome.field_name, *field, off, outcome.match_count, outcome.anchor_count));
        }
        *field = off;
    }
}

impl Il2CppConfig {
    // ── Baseline constants ───────────────────────────────────────

    /// Last-resort baseline. Values empirically validated on Unity 2019/2021 era
    /// IL2CPP runtimes. Used as the initial seed for `probe()` and as the per-
    /// field fallback when an individual probe fails.
    pub fn fallback_constants() -> Self {
        Self {
            class_table_step:            8,
            klass_namespace:             0x18,
            klass_type_def:              0x20,   // byval_arg.data (inline)
            klass_generic_class:         0x48,
            klass_fields:                0x80,
            il2cpp_type_discrim_read_at: 0x08,
            discrim_shift:               16,
            klass_methods:               0x98,
            klass_static_fields:         0xB8,
            method_name_off:             0x18,
            method_klass_off:            0x20,
            method_param_count_off:      0x52,
            klass_valuetype_off:         0x2B,
            klass_valuetype_bit:         0x80,
            method_pointer_off:          0x08,
            method_return_type_off:      0x28,
            method_parameters_off:       0x30,
            method_flags_off:            0x4C,
            param_info_size:             0x18,
            param_info_type_off:         0x00,
        }
    }

    /// Probe-and-Verify Discipline: derive every offset from live FFI ground
    /// truth. Returns (config, ConfidenceReport). Phase 1 failure terminates;
    /// other phases fall back to `fallback_constants()` per-field.
    pub fn probe(
        map: &crate::external::region_map::RegionMap,
        api: &crate::internals::ffi::Il2CppApi,
        table_base: usize,
        table_count: usize,
        phase0: crate::internals::calibration::stability::StabilityResult,
    ) -> (Self, crate::internals::calibration::ConfidenceReport) {
        use crate::internals::calibration::{
            klass_layout, method_layout, type_discrim,
            field_param_layout, ffi_verify, metadata_version,
        };

        let mut cfg = Self::fallback_constants();

        // Phase 0 stability is computed by the caller (entry.rs) BEFORE the map
        // snapshot, so the map reflects classes that have finished loading.

        // Phase 1 (FATAL)
        crate::paths::log("probe: Phase 1 (klass) ENTER");
        let n = klass_layout::probe_klass_namespace(api, map, table_base, table_count, cfg.class_table_step);
        let td = klass_layout::probe_klass_type_def(api, map, table_base, table_count, cfg.class_table_step);
        let fl = klass_layout::probe_klass_fields(api, map, table_base, table_count, cfg.class_table_step);
        let me = klass_layout::probe_klass_methods(api, map, table_base, table_count, cfg.class_table_step);
        let sf = klass_layout::probe_klass_static_fields(api, map, table_base, table_count, cfg.class_table_step);
        let (vt, vt_bit) = klass_layout::probe_klass_valuetype(api, map, table_base, table_count, cfg.class_table_step);
        crate::paths::log("probe: Phase 1 (klass) EXIT");
        apply_offset(&mut cfg.klass_namespace, &n);
        apply_offset(&mut cfg.klass_type_def, &td);
        apply_offset(&mut cfg.klass_fields, &fl);
        apply_offset(&mut cfg.klass_methods, &me);
        apply_offset(&mut cfg.klass_static_fields, &sf);
        apply_offset(&mut cfg.klass_valuetype_off, &vt);
        if let Some(b) = vt_bit { cfg.klass_valuetype_bit = b; }
        let phase1 = vec![n, td, fl, me, sf, vt];

        // Phase 2
        crate::paths::log("probe: Phase 2 (method) ENTER");
        let cts = cfg.class_table_step;
        let mp = method_layout::probe_method_pointer_off(api, map, table_base, table_count, cts);
        let mn = method_layout::probe_method_name_off(api, map, table_base, table_count, cts);
        let mk = method_layout::probe_method_klass_off(api, map, table_base, table_count, cts);
        let mf = method_layout::probe_method_flags_off(api, map, table_base, table_count, cts);
        let mpars = method_layout::probe_method_parameters_off(api, map, table_base, table_count, cts);
        let mret = method_layout::probe_method_return_type_off(api, map, table_base, table_count, cts);
        let mpc = method_layout::probe_method_param_count_off(api, map, table_base, table_count, cts);
        apply_offset(&mut cfg.method_pointer_off, &mp);
        apply_offset(&mut cfg.method_name_off, &mn);
        apply_offset(&mut cfg.method_klass_off, &mk);
        apply_offset(&mut cfg.method_flags_off, &mf);
        apply_offset(&mut cfg.method_parameters_off, &mpars);
        apply_offset(&mut cfg.method_return_type_off, &mret);
        apply_offset(&mut cfg.method_param_count_off, &mpc);
        let phase2 = vec![mp, mn, mk, mf, mpars, mret, mpc];
        crate::paths::log("probe: Phase 2 (method) EXIT");

        // Phase 3
        crate::paths::log("probe: Phase 3 (type_discrim) ENTER");
        let (td_read, td_shift) = type_discrim::probe_type_discrim(
            api, map, table_base, table_count, cfg.class_table_step, cfg.klass_type_def);
        apply_offset(&mut cfg.il2cpp_type_discrim_read_at, &td_read);
        {
            let prev = cfg.discrim_shift;
            if let Some(off) = td_shift.winning_offset {
                if td_shift.anchor_count < MIN_OVERRIDE_ANCHORS {
                    crate::paths::log(&format!(
                        "⚠ PROBE WEAK: {} probed={:#x} (only {}/{}) — keeping fallback {:#x}",
                        td_shift.field_name, off,
                        td_shift.match_count, td_shift.anchor_count, prev));
                } else {
                    if off as u8 != prev {
                        crate::paths::log(&format!(
                            "⚠ PROBE OVERRIDE: {} fallback={:#x} → probed={:#x} (match {}/{})",
                            td_shift.field_name, prev, off, td_shift.match_count, td_shift.anchor_count));
                    }
                    cfg.discrim_shift = off as u8;
                }
            }
        }
        let phase3 = vec![td_read, td_shift];
        crate::paths::log("probe: Phase 3 (type_discrim) EXIT");

        // Phase 4
        crate::paths::log("probe: Phase 4 (field_param) ENTER");
        let (pi_size, pi_type) = field_param_layout::probe_param_info(
            api, map, table_base, table_count, cfg.class_table_step,
            cfg.klass_type_def, cfg.il2cpp_type_discrim_read_at,
            cfg.discrim_shift as usize, cfg.method_parameters_off,
        );
        apply_offset(&mut cfg.param_info_size, &pi_size);
        apply_offset(&mut cfg.param_info_type_off, &pi_type);
        let phase4 = vec![pi_size, pi_type];
        crate::paths::log("probe: Phase 4 (field_param) EXIT");

        // Phase 5
        crate::paths::log("probe: Phase 5 (ffi_verify) ENTER");
        let phase5 = ffi_verify::run_verification(api, table_base, table_count, cfg.class_table_step);
        crate::paths::log("probe: Phase 5 (ffi_verify) EXIT");

        // Phase 6
        crate::paths::log("probe: Phase 6 (metadata) ENTER");
        let phase6 = metadata_version::probe_metadata_version();
        crate::paths::log("probe: Phase 6 (metadata) EXIT");

        let report = crate::internals::calibration::ConfidenceReport {
            phase0_stability: phase0,
            phase1_klass: phase1,
            phase2_method: phase2,
            phase3_type_discrim: phase3,
            phase4_field_param: phase4,
            phase5_ffi: phase5,
            phase6_metadata_version: phase6,
        };
        (cfg, report)
    }

}

impl Default for Il2CppConfig {
    fn default() -> Self { Self::fallback_constants() }
}
