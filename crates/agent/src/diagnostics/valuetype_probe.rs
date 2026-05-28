//! One-shot probe (opt-in `FROG_VALUETYPE_PROBE`): derives the offset and bit
//! of `Il2CppClass::valuetype` by cross-validating multiple known value types
//! and multiple known reference types. A real flag bit is set in EVERY value
//! type and clear in EVERY reference type.

use crate::external::cache;
use crate::internals::api;
use crate::paths::log;

const VALUE_TYPES: &[&str] = &[
    "System::Int32",
    "System::Single",
    "System::Boolean",
    "System::Byte",
    "System::Double",
];

const REF_TYPES: &[&str] = &[
    "System::String",
    "System::Object",
    "System::Type",
    "System::Exception",
];

pub fn run_valuetype_probe() {
    log("=== VALUETYPE PROBE ===");

    let resolve = |names: &[&str]| -> Vec<(String, usize)> {
        names.iter().filter_map(|n| {
            let k = api::find_class(n);
            if k == 0 { None } else { Some((n.to_string(), k as usize)) }
        }).collect()
    };

    let vt_klasses  = resolve(VALUE_TYPES);
    let ref_klasses = resolve(REF_TYPES);

    log(&format!(
        "valuetype probe: {}/{} value-type anchors resolved, {}/{} reference-type anchors resolved",
        vt_klasses.len(), VALUE_TYPES.len(),
        ref_klasses.len(), REF_TYPES.len()
    ));
    for (name, addr) in &vt_klasses { log(&format!("  VT  {} @ {:#x}", name, addr)); }
    for (name, addr) in &ref_klasses { log(&format!("  REF {} @ {:#x}", name, addr)); }

    if vt_klasses.len() < 2 || ref_klasses.len() < 2 {
        log("valuetype probe: not enough anchors resolved; aborting");
        return;
    }

    // For each (offset, bit) candidate: must be SET in every value-type anchor
    // AND CLEAR in every reference-type anchor.
    let mut survivors: Vec<(usize, u8)> = Vec::new();
    for off in 0..0x200usize {
        for bit_idx in 0..8u8 {
            let mask = 1u8 << bit_idx;
            let mut all_vt_set = true;
            for (_, k) in &vt_klasses {
                let b = cache::read_u8(k + off).unwrap_or(0);
                if (b & mask) == 0 { all_vt_set = false; break; }
            }
            if !all_vt_set { continue; }
            let mut all_ref_clear = true;
            for (_, k) in &ref_klasses {
                let b = cache::read_u8(k + off).unwrap_or(0xFF);
                if (b & mask) != 0 { all_ref_clear = false; break; }
            }
            if !all_ref_clear { continue; }
            survivors.push((off, mask));
        }
    }

    log(&format!("valuetype probe: {} surviving candidate(s) after cross-validation", survivors.len()));
    for (off, bit) in &survivors {
        log(&format!("  CANDIDATE  +{:#05x}  bit={:#04x}", off, bit));
    }
    log("=== end VALUETYPE PROBE ===");
}
