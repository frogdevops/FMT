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
///
/// Uses stride-1 (sequential scan) rather than stride-N so that sparse tables
/// (e.g. PW fill ratio ~21%) still accumulate the full `n` anchors instead of
/// visiting only ~n slot positions and hitting mostly nulls.  On dense tables
/// the first `n` real klasses are found well before the end of the table.
fn sample_klass_anchors<T: Clone>(
    _api: &Il2CppApi,
    map: &RegionMap,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
    n: usize,
    extract_truth: impl Fn(usize) -> Option<T>,
) -> Vec<(usize, T)> {
    let mut out: Vec<(usize, T)> = Vec::with_capacity(n);
    // Skip past nulls/garbage; stride 1 with sparse-aware progress. We sample
    // up to n REAL klasses (not n slot positions) — the old strided form
    // dropped to ~10 anchors on sparse tables (PW: 3991/18515 slots filled).
    for i in 0..table_count {
        if out.len() >= n { break; }
        let slot = table_base.wrapping_add(i * class_table_step);
        let klass = match map.read_u64(slot) {
            Some(k) if k != 0 => k as usize,
            _ => continue,
        };
        // Gate FFI on potentially-garbage class-table slots: obfuscated
        // builds hold non-klass values in non-null slots, and calling
        // FFI (extract_truth) on those crashes the process.
        if !cache::is_klass_shape(klass) { continue; }
        if let Some(truth) = extract_truth(klass) {
            out.push((klass, truth));
        }
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
    // type_def points at the start of byval_arg (an Il2CppType*). The OLD probe
    // accepted "any non-null pointer," which wrong-picked +0x18 (the namespace
    // string pointer) over the real +0x20 (byval_arg) — silently corrupting
    // type resolution. This version is DISCRIMINATING: for each candidate
    // offset, follow the pointer as if it were byval_arg, extract the type code
    // (tc) using the FALLBACK discrim recipe (proven stable v24→v31), and accept
    // only offsets whose tc is a VALID Il2CppTypeEnum (0x01..=0x21). At +0x18 the
    // bytes are a string-ptr target → garbage tc; at +0x20 the tc is valid.
    //
    // Even stronger: cross-check known types — System::Int32's byval_arg must
    // yield tc 0x08 AND System::String's must yield 0x0E. That's fully
    // discriminating (a wrong offset cannot satisfy both on real types).
    use crate::internals::calibration::anchors::local_find_class;
    let candidates = vec![0x18usize, 0x20, 0x28, 0x30];

    let fallback = crate::internals::config::Il2CppConfig::fallback_constants();
    let read_tc = |type_ptr: usize| -> Option<u8> {
        let chunk = map.read_u64(type_ptr + fallback.il2cpp_type_discrim_read_at)?;
        Some(((chunk >> fallback.discrim_shift) & 0xFF) as u8)
    };

    // Known-type cross-check anchors (klass, expected-tc-of-byval_arg).
    let int32 = local_find_class(api, table_base, table_count, class_table_step, "System::Int32");
    let strk  = local_find_class(api, table_base, table_count, class_table_step, "System::String");
    let known: Vec<(usize, u8)> = [(int32, 0x08u8), (strk, 0x0E)]
        .into_iter().filter(|(k, _)| *k != 0).collect();

    // General anchors: sample real klasses; require tc in the valid range.
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;

    let mut winner: Option<(usize, u32)> = None;  // (offset, matches)
    for &off in &candidates {
        // Gate 1 (strongest): known types must produce their exact tc.
        if !known.is_empty() {
            let known_ok = known.iter().all(|(k, expect)| {
                let bv = map.read_u64(k + off).unwrap_or(0) as usize;
                bv != 0 && read_tc(bv) == Some(*expect)
            });
            if !known_ok { continue; }
        }
        // Gate 2: the sampled population must mostly yield a VALID il2cpp tc.
        let matches = anchors.iter().filter(|(k, _)| {
            let bv = map.read_u64(k + off).unwrap_or(0) as usize;
            if bv == 0 { return false; }
            matches!(read_tc(bv), Some(tc) if (0x01..=0x21).contains(&tc))
        }).count() as u32;
        let ratio = if total == 0 { 0.0 } else { matches as f32 / total as f32 };
        if ratio >= MIN_RATIO {
            match winner {
                None => winner = Some((off, matches)),
                Some((_, m)) if matches > m => winner = Some((off, matches)),
                _ => {}
            }
        }
    }

    match winner {
        Some((off, matches)) => ProbeOutcome {
            field_name: "klass_type_def", winning_offset: Some(off),
            match_count: matches, anchor_count: total,
            fell_back: false, candidates_tried: candidates,
        },
        None => ProbeOutcome {
            field_name: "klass_type_def", winning_offset: None,
            match_count: 0, anchor_count: total,
            fell_back: true, candidates_tried: candidates,
        },
    }
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
            // Only sample classes that have ≥1 field.
            if let Some(get_fields) = api.class_get_fields {
                // FFI path: fast check via il2cpp ABI.
                let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
                let fi = unsafe { get_fields(k as *mut Il2CppClass, &mut iter) };
                if fi.is_null() { return None; }
                Some(())
            } else {
                // Memory-walk fallback for builds where class_get_fields was not
                // resolved (e.g. obfuscated/stripped il2cpp like Pixel Worlds).
                // Peek klass + fallback.klass_fields → FieldInfo array; if the
                // first element's name pointer resolves to a non-empty cstr the
                // class has at least one field.
                let fallback = crate::internals::config::Il2CppConfig::fallback_constants();
                let arr = map.read_u64(k + fallback.klass_fields)? as usize;
                if arr == 0 { return None; }
                let name_ptr = map.read_u64(arr)? as usize;
                if name_ptr == 0 { return None; }
                let name = map.read_name(name_ptr)?;
                if name.is_empty() { None } else { Some(()) }
            }
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
    // klass_static_fields points at static storage. There is NO honest
    // discriminator: most classes have NULL static_fields, and a neighboring
    // offset that happens to hold an always-populated pointer (e.g. a vtable
    // slot) would clear a permissive threshold and WRONG-PICK. So this probe is
    // deliberately conservative — it can only override the fallback if a strict,
    // near-unanimous signal exists; otherwise it falls back to the v24 constant
    // (which is correct and the value used pre-B-1). The report line will read
    // "Falling back to constant" — honest about the lack of independent
    // verification.
    let candidates = vec![0xA8usize, 0xB0, 0xB8, 0xC0, 0xC8];
    let anchors = sample_klass_anchors(api, map, table_base, table_count, class_table_step, ANCHOR_COUNT,
        |k| {
            let name = unsafe { cstr_to_string((api.class_get_name)(k as *mut Il2CppClass)) };
            if name.is_empty() { None } else { Some(()) }
        });
    let total = anchors.len() as u32;
    let extract = |k: &usize, off: usize| -> Option<()> {
        let p = map.read_u64(k + off)?;
        if p > 0x10_0000 { Some(()) } else { None }
    };
    // 0.99 threshold: only a near-unanimous, every-class-populated offset can
    // win. Because real static_fields is null on most classes, the CORRECT
    // offset will NOT clear this — so the probe falls back rather than
    // wrong-picking a coincidentally-populated neighbor (which would also be
    // unlikely to hit ~100%). Net: it never overrides on weak evidence.
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.99);
    finalize("klass_static_fields", result, total, candidates)
}

pub fn probe_klass_valuetype(
    api: &Il2CppApi, map: &RegionMap,
    table_base: usize, table_count: usize, class_table_step: usize,
) -> (ProbeOutcome, Option<u8>) {
    crate::paths::log("  Phase1: probe_klass_valuetype");
    // Promoted from diagnostics/valuetype_probe.rs: cross-validate value
    // types vs reference types. Anchor offsets are 0x00..0x200 with bit
    // probes per offset. We need both the offset AND the bit mask.
    // CTX-FREE anchors — ctx isn't init'd during probe().
    use crate::internals::calibration::anchors::local_find_class;
    let vts = ["System::Int32", "System::Single", "System::Boolean", "System::Byte", "System::Double"];
    let rts = ["System::String", "System::Object", "System::Type", "System::Exception"];
    let vt_klasses: Vec<usize> = vts.iter()
        .filter_map(|n| { let k = local_find_class(api, table_base, table_count, class_table_step, n); if k != 0 { Some(k) } else { None } })
        .collect();
    let rt_klasses: Vec<usize> = rts.iter()
        .filter_map(|n| { let k = local_find_class(api, table_base, table_count, class_table_step, n); if k != 0 { Some(k) } else { None } })
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
