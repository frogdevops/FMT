//! Phase 4: probe FieldInfo + ParameterInfo strides and per-element offsets.

use crate::external::region_map::RegionMap;
use crate::internals::api as iapi;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;

const MIN_RATIO: f32 = 0.90;

/// ParameterInfo stride: probe via String::PadLeft(Int32, Char).
/// param[0] type is Int32 (tc=0x08), param[1] type is Char (tc=0x03).
/// We test (stride, type_off) pairs and accept the one where both
/// param[0] and param[1] yield expected tcs.
pub fn probe_param_info(
    map: &RegionMap,
    klass_type_def_off: usize,
    discrim_read_at: usize,
    discrim_shift: usize,
    method_parameters_off: usize,
) -> (ProbeOutcome, ProbeOutcome) {
    let string = iapi::find_class("System::String");
    let padleft = if string != 0 { iapi::find_method(string, "PadLeft", 2) } else { 0 };
    if padleft == 0 {
        return (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
        );
    }
    let params_base = map.read_u64(padleft as usize + method_parameters_off).unwrap_or(0) as usize;
    if params_base == 0 {
        return (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: 0, fell_back: true,
                candidates_tried: vec![],
            },
        );
    }

    // Read tc from a candidate type ptr.
    let read_tc = |type_ptr: usize| -> Option<u8> {
        let chunk = map.read_u64(type_ptr + klass_type_def_off - klass_type_def_off + discrim_read_at)?;
        // simpler: read from type_ptr directly; klass_type_def_off doesn't apply here
        let chunk = map.read_u64(type_ptr + discrim_read_at)?;
        Some(((chunk >> discrim_shift) & 0xFF) as u8)
    };

    let stride_candidates: Vec<usize> = vec![0x08, 0x10, 0x18, 0x20, 0x28];
    let type_off_candidates: Vec<usize> = vec![0x00, 0x08, 0x10, 0x18];

    let mut best: Option<(usize, usize, u32)> = None;  // (stride, type_off, matches)
    for &stride in &stride_candidates {
        for &type_off in &type_off_candidates {
            let p0_type = map.read_u64(params_base + 0 + type_off).unwrap_or(0) as usize;
            let p1_type = map.read_u64(params_base + stride + type_off).unwrap_or(0) as usize;
            if p0_type == 0 || p1_type == 0 { continue; }
            let p0_tc = read_tc(p0_type).unwrap_or(0);
            let p1_tc = read_tc(p1_type).unwrap_or(0);
            // PadLeft: arg0=Int32 (tc=0x08), arg1=Char (tc=0x03).
            let matches = (if p0_tc == 0x08 { 1 } else { 0 })
                        + (if p1_tc == 0x03 { 1 } else { 0 });
            if matches == 2 {
                match best {
                    None => best = Some((stride, type_off, matches)),
                    Some((_, _, m)) if matches > m => best = Some((stride, type_off, matches)),
                    _ => {}
                }
            }
        }
    }

    let total = 2u32;
    match best {
        Some((stride, type_off, matches)) => (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: Some(stride),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: stride_candidates,
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: Some(type_off),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: type_off_candidates,
            },
        ),
        None => (
            ProbeOutcome {
                field_name: "param_info_size", winning_offset: None,
                match_count: 0, anchor_count: total, fell_back: true,
                candidates_tried: stride_candidates,
            },
            ProbeOutcome {
                field_name: "param_info_type_off", winning_offset: None,
                match_count: 0, anchor_count: total, fell_back: true,
                candidates_tried: type_off_candidates,
            },
        ),
    }
}
