//! Phase 3: probe the il2cpp type discriminator extraction recipe.

use crate::external::region_map::RegionMap;
use crate::internals::api as iapi;
use crate::internals::calibration::candidates_local::pick_offset_by_consensus;
use crate::internals::calibration::ProbeOutcome;

const MIN_RATIO: f32 = 0.90;

/// Build a (klass-ptr, expected-tc) anchor list from known il2cpp types.
fn type_anchors() -> Vec<(usize, u8)> {
    [("System::Int32", 0x08u8), ("System::String", 0x0E), ("System::Object", 0x1C),
     ("System::Single", 0x0C), ("System::Double", 0x0D)]
        .iter().filter_map(|(name, tc)| {
            let k = iapi::find_class(name);
            if k != 0 { Some((k as usize, *tc)) } else { None }
        }).collect()
}

/// Probes:
///   il2cpp_type_discrim_read_at = offset from klass+klass_type_def at which
///     to read 8 bytes (the discriminator chunk).
///   discrim_shift = how many bits to right-shift before masking 0xFF.
///
/// Given known klass_type_def offset (from Phase 1), we read at klass+type_def
/// + N for N in [0x00, 0x08]; and shift by [0, 8, 16, 24]. The (offset, shift)
/// pair where >=90% of anchors yield their expected tc wins.
pub fn probe_type_discrim(
    map: &RegionMap,
    klass_type_def_off: usize,
) -> (ProbeOutcome, ProbeOutcome) {
    let anchors = type_anchors();
    let total = anchors.len() as u32;

    let read_candidates: Vec<usize> = vec![0x00, 0x08];
    let shift_candidates: Vec<usize> = vec![0, 8, 16, 24];

    // Joint probe: enumerate all (read_off, shift) pairs and find the best.
    let mut best: Option<(usize, usize, u32)> = None;  // (read_off, shift, matches)
    for &read_off in &read_candidates {
        for &shift in &shift_candidates {
            let chunk_at = klass_type_def_off + read_off;
            let matches = anchors.iter().filter(|(k, expected_tc)| {
                map.read_u64(k + chunk_at).map(|chunk| {
                    ((chunk >> shift) & 0xFF) as u8 == *expected_tc
                }).unwrap_or(false)
            }).count() as u32;
            let ratio = matches as f32 / total as f32;
            if ratio >= MIN_RATIO {
                match best {
                    None => best = Some((read_off, shift, matches)),
                    Some((_, _, m)) if matches > m => best = Some((read_off, shift, matches)),
                    _ => {}
                }
            }
        }
    }

    match best {
        Some((read_off, shift, matches)) => (
            ProbeOutcome {
                field_name: "il2cpp_type_discrim_read_at",
                winning_offset: Some(read_off),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: read_candidates,
            },
            ProbeOutcome {
                field_name: "discrim_shift",
                winning_offset: Some(shift),
                match_count: matches, anchor_count: total,
                fell_back: false, candidates_tried: shift_candidates,
            },
        ),
        None => (
            ProbeOutcome {
                field_name: "il2cpp_type_discrim_read_at",
                winning_offset: None, match_count: 0, anchor_count: total,
                fell_back: true, candidates_tried: read_candidates,
            },
            ProbeOutcome {
                field_name: "discrim_shift",
                winning_offset: None, match_count: 0, anchor_count: total,
                fell_back: true, candidates_tried: shift_candidates,
            },
        ),
    }
}
