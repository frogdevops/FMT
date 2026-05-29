//! Phase 2: probe MethodInfo offsets. Non-fatal; falls back per-field.

use crate::external::region_map::RegionMap;
use crate::internals::calibration::anchors::{local_find_class, local_find_method};
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;

const MIN_RATIO: f32 = 0.90;

/// Returns a (Math.Pow method, String.PadLeft method) anchor pair, or None if
/// either isn't found. CTX-FREE — walks the live table via `api` so it works
/// before `ctx::init` runs (which is after probe()).
fn anchor_methods(
    api: &Il2CppApi,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
) -> Option<(u64, u64)> {
    let cfg = Il2CppConfig::fallback_constants();
    let math = local_find_class(api, table_base, table_count, class_table_step, "System::Math");
    let pow = if math != 0 { local_find_method(&cfg, math, "Pow", 2) } else { 0 };
    let string = local_find_class(api, table_base, table_count, class_table_step, "System::String");
    let padleft = if string != 0 { local_find_method(&cfg, string, "PadLeft", 2) } else { 0 };
    if pow == 0 || padleft == 0 { None } else { Some((pow as u64, padleft as u64)) }
}

pub fn probe_method_pointer_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_pointer_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x00usize, 0x08, 0x10];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        // methodPointer is a code address in 0x6xxxxxxxxxxx range typically;
        // verify it's not 0 and not in the Unity runtime data region.
        if p > 0x10_0000_0000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_pointer_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_name_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_name_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, &str)> = vec![(pow, "Pow"), (padleft, "PadLeft")];
    let candidates = vec![0x10usize, 0x18, 0x20];
    let extract = |m: &u64, off: usize| -> Option<&'static str> {
        let name_ptr = map.read_u64(*m as usize + off)? as usize;
        let s = map.read_name(name_ptr).unwrap_or_default();
        match s.as_str() {
            "Pow" => Some("Pow"),
            "PadLeft" => Some("PadLeft"),
            _ => None,
        }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_name_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_klass_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_klass_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let math = local_find_class(api, table_base, table_count, class_table_step, "System::Math") as u64;
    let string = local_find_class(api, table_base, table_count, class_table_step, "System::String") as u64;
    let anchors: Vec<(u64, u64)> = vec![(pow, math), (padleft, string)];
    let candidates = vec![0x18usize, 0x20, 0x28];
    let extract = |m: &u64, off: usize| -> Option<u64> {
        map.read_u64(*m as usize + off)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_klass_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_flags_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // Math.Pow and String.PadLeft are both effectively-callable methods.
    // Math.Pow is static (METHOD_ATTRIBUTE_STATIC=0x10 set).
    // String.PadLeft is instance (bit clear).
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_flags_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, bool)> = vec![(pow, true), (padleft, false)];
    let candidates = vec![0x40usize, 0x44, 0x48, 0x4C, 0x50];
    let extract = |m: &u64, off: usize| -> Option<bool> {
        let v = map.read_u32(*m as usize + off)?;
        Some(v & 0x10 != 0)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_flags_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_parameters_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_parameters_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x28usize, 0x30, 0x38];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        if p > 0x10000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_parameters_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_return_type_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_return_type_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, ())> = vec![(pow, ()), (padleft, ())];
    let candidates = vec![0x20usize, 0x28, 0x30];
    let extract = |m: &u64, off: usize| -> Option<()> {
        let p = map.read_u64(*m as usize + off)?;
        if p > 0x10000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_return_type_off", result, anchors.len() as u32, candidates)
}

pub fn probe_method_param_count_off(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    // Both Math.Pow and String.PadLeft have argc=2.
    let (pow, padleft) = match anchor_methods(api, table_base, table_count, class_table_step) {
        Some(x) => x,
        None => return ProbeOutcome {
            field_name: "method_param_count_off",
            winning_offset: None, match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        },
    };
    let anchors: Vec<(u64, u8)> = vec![(pow, 2u8), (padleft, 2u8)];
    let candidates = vec![0x50usize, 0x52, 0x54];
    let extract = |m: &u64, off: usize| -> Option<u8> {
        map.read_u8(*m as usize + off)
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    super::klass_layout::finalize_pub("method_param_count_off", result, anchors.len() as u32, candidates)
}
