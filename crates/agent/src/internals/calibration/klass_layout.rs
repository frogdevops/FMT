//! Phase 1: probe klass-struct offsets. FATAL on any failure.

use crate::external::cache;
use crate::external::region_map::RegionMap;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

const MIN_RATIO: f32 = 0.90;
const ANCHOR_COUNT: usize = 50;

/// Sample up to `n` non-null klass pointers from the class table, each paired
/// with the FFI-derived "expected" value via `extract_truth`.
fn sample_klass_anchors<T: Clone>(
    api: &Il2CppApi,
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
    n: usize,
    extract_truth: impl Fn(usize) -> Option<T>,
) -> Vec<(usize, T)> {
    let mut out: Vec<(usize, T)> = Vec::with_capacity(n);
    let stride = if table_count > n { table_count / n } else { 1 };
    let mut i = 0usize;
    while i < table_count && out.len() < n {
        let slot = table_base.wrapping_add(i * class_table_step);
        if let Some(klass) = map.read_u64(slot) {
            if klass != 0 {
                let k = klass as usize;
                // Gate FFI on potentially-garbage class-table slots: obfuscated
                // builds hold non-klass values in non-null slots, and calling
                // FFI (extract_truth) on those crashes the process.
                if cache::is_klass_shape(k) {
                    if let Some(truth) = extract_truth(k) {
                        out.push((k, truth));
                    }
                }
            }
        }
        i += stride;
    }
    crate::paths::log(&format!("  Phase1: sample_klass_anchors gathered {} anchors", out.len()));
    out
}

// ── Probes ────────────────────────────────────────────────────────────

pub fn probe_klass_namespace(
    api: &Il2CppApi,
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
) -> ProbeOutcome {
    crate::paths::log("  Phase1: probe_klass_namespace");
    let candidates = vec![0x10usize, 0x18, 0x20, 0x28];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let ns = unsafe { cstr_to_string((api.class_get_namespace)(k as *mut Il2CppClass)) };
            if ns.is_empty() { None } else { Some(ns) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<String> {
        let ns_ptr = map.read_u64(k + off)? as usize;
        let s = map.read_name(ns_ptr).unwrap_or_default();
        if s.is_empty() { None } else { Some(s) }
    };
    match pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO) {
        Some((off, score)) => ProbeOutcome {
            field_name: "klass_namespace",
            winning_offset: Some(off),
            match_count: score.matches, anchor_count: total,
            fell_back: false, candidates_tried: candidates,
        },
        None => ProbeOutcome {
            field_name: "klass_namespace",
            winning_offset: None, match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: candidates,
        },
    }
}

pub fn probe_klass_type_def(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    crate::paths::log("  Phase1: probe_klass_type_def");
    // type_def points at the start of byval_arg (an Il2CppType). The first
    // 8 bytes are `data` — a pointer that is non-null for valid types.
    let candidates = vec![0x18usize, 0x20, 0x28, 0x30];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        // valid byval_arg → data is a non-null pointer in mapped region
        let data = map.read_u64(k + off)?;
        if data != 0 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_type_def", result, total, candidates)
}

pub fn probe_klass_fields(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    crate::paths::log("  Phase1: probe_klass_fields");
    // klass_fields points at FieldInfo array. The first FieldInfo's name@0
    // is a non-empty cstr for any class with fields.
    let candidates = vec![0x70usize, 0x78, 0x80, 0x88, 0x90];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            // Only sample classes the FFI says have ≥1 field.
            if let Some(get_fields) = api.class_get_fields {
                let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
                let fi = unsafe { get_fields(k as *mut Il2CppClass, &mut iter) };
                if fi.is_null() { return None; }
                Some(())
            } else { None }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let arr = map.read_u64(k + off)? as usize;
        if arr == 0 { return None; }
        // FieldInfo[0].name@0 should be a non-empty cstr ptr.
        let name_ptr = map.read_u64(arr)? as usize;
        let name = map.read_name(name_ptr)?;
        if name.is_empty() { None } else { Some(()) }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_fields", result, total, candidates)
}

pub fn probe_klass_methods(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    crate::paths::log("  Phase1: probe_klass_methods");
    // klass_methods points at MethodInfo* array. The first MethodInfo at
    // *arr[0] has methodPointer at +0x08 in the code region (0x6xxx range).
    let candidates = vec![0x88usize, 0x90, 0x98, 0xA0, 0xA8];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let arr = map.read_u64(k + off)? as usize;
        if arr == 0 { return None; }
        let method_info_ptr = map.read_u64(arr)? as usize;
        if method_info_ptr == 0 { return None; }
        let method_pointer = map.read_u64(method_info_ptr + 0x08)?;
        // Verify methodPointer is in a reasonable code region (high address).
        if method_pointer < 0x10_0000 { return None; }
        Some(())
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, MIN_RATIO);
    finalize("klass_methods", result, total, candidates)
}

pub fn probe_klass_static_fields(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> ProbeOutcome {
    crate::paths::log("  Phase1: probe_klass_static_fields");
    // klass_static_fields points at a data region (static storage). It's
    // non-null only for classes WITH static fields, so we use a permissive
    // threshold and look for a non-zero pointer in a sensible range.
    let candidates = vec![0xA8usize, 0xB0, 0xB8, 0xC0, 0xC8];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let p = map.read_u64(k + off)?;
        // static_fields is a non-null data pointer for classes WITH static fields.
        // Reject zero so the probe discriminates real static-storage offsets.
        if p > 0x10_0000 { Some(()) } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.80);  // lower threshold
    finalize("klass_static_fields", result, total, candidates)
}

pub fn probe_klass_valuetype(
    api: &Il2CppApi, map: &RegionMap,
) -> (ProbeOutcome, Option<u8>) {
    crate::paths::log("  Phase1: probe_klass_valuetype");
    // Promoted from diagnostics/valuetype_probe.rs: cross-validate value
    // types vs reference types. Anchor offsets are 0x00..0x200 with bit
    // probes per offset. We need both the offset AND the bit mask.
    use crate::internals::api as iapi;
    let vts = ["System::Int32", "System::Single", "System::Boolean", "System::Byte", "System::Double"];
    let rts = ["System::String", "System::Object", "System::Type", "System::Exception"];
    let vt_klasses: Vec<usize> = vts.iter()
        .filter_map(|n| { let k = iapi::find_class(n); if k != 0 { Some(k as usize) } else { None } })
        .collect();
    let rt_klasses: Vec<usize> = rts.iter()
        .filter_map(|n| { let k = iapi::find_class(n); if k != 0 { Some(k as usize) } else { None } })
        .collect();
    if vt_klasses.len() < 4 || rt_klasses.len() < 3 {
        return (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: None,
            match_count: 0, anchor_count: 0,
            fell_back: true, candidates_tried: vec![],
        }, None);
    }
    let mut best: Option<(usize, u8, u32)> = None;  // (offset, bit, score)
    for off in 0..0x200usize {
        for bit_idx in 0..8u8 {
            let mask = 1u8 << bit_idx;
            let vt_match = vt_klasses.iter().filter(|k| {
                map.read_u8(*k + off).map(|b| (b & mask) != 0).unwrap_or(false)
            }).count() as u32;
            let rt_clear = rt_klasses.iter().filter(|k| {
                map.read_u8(*k + off).map(|b| (b & mask) == 0).unwrap_or(false)
            }).count() as u32;
            let total = (vt_klasses.len() + rt_klasses.len()) as u32;
            let score = vt_match + rt_clear;
            if score == total {
                match best {
                    Some((_, _, s)) if score <= s => {}
                    _ => best = Some((off, mask, score)),
                }
            }
        }
    }
    let total = (vt_klasses.len() + rt_klasses.len()) as u32;
    match best {
        Some((off, bit, score)) => (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: Some(off),
            match_count: score, anchor_count: total,
            fell_back: false, candidates_tried: vec![],
        }, Some(bit)),
        None => (ProbeOutcome {
            field_name: "klass_valuetype_off",
            winning_offset: None, match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: vec![],
        }, None),
    }
}

pub fn finalize_pub(
    name: &'static str,
    result: Option<(usize, crate::internals::calibration::candidates_local::CandidateScore)>,
    total: u32,
    candidates: Vec<usize>,
) -> ProbeOutcome {
    finalize(name, result, total, candidates)
}

fn finalize(name: &'static str, result: Option<(usize, crate::internals::calibration::candidates_local::CandidateScore)>,
            total: u32, candidates: Vec<usize>) -> ProbeOutcome {
    match result {
        Some((off, score)) => ProbeOutcome {
            field_name: name, winning_offset: Some(off),
            match_count: score.matches, anchor_count: total,
            fell_back: false, candidates_tried: candidates,
        },
        None => ProbeOutcome {
            field_name: name, winning_offset: None,
            match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: candidates,
        },
    }
}

/// True if ANY of the critical Phase 1 probes failed → caller terminates.
pub fn any_critical_failed(outcomes: &[ProbeOutcome]) -> bool {
    outcomes.iter().any(|o| o.fell_back)
}
