//! One-shot probe (opt-in `FROG_METHODINFO_PROBE`): derives the 5 MethodInfo /
//! ParamInfo offsets needed by invoke + hook marshalling. Anchors on
//! `System::String::Concat` (exists in every il2cpp game; has known params).

use crate::external::cache;
use crate::internals::api;
use crate::paths::log;

pub fn run_methodinfo_probe() {
    log("=== METHODINFO PROBE ===");
    let klass = api::find_class("System::String");
    if klass == 0 { log("methodinfo probe: System::String not found"); return; }
    // Concat(String, String) is the simplest 2-arg overload.
    let method = api::find_method(klass, "Concat", 2);
    if method == 0 { log("methodinfo probe: String::Concat(2) not found"); return; }
    log(&format!("methodinfo probe: Concat @ {:#x}", method));

    // Candidate offsets for `parameters` ptr — scan every 8-byte slot in [0x28..0x60].
    // The right offset points to a ParameterInfo array; first element's `type` ptr
    // (also probed) should resolve to a valid Il2CppType.
    log("--- candidates: method_parameters_off ---");
    for off in (0x28..0x60usize).step_by(8) {
        let cand = cache::read_u64(method as usize + off).unwrap_or(0);
        if cand == 0 || cand < 0x10000 { continue; }
        log(&format!("  +{:#04x} -> {:#x}", off, cand));
    }

    // Candidate offsets for `return_type` ptr — usually 0x40 or thereabouts.
    log("--- candidates: method_return_type_off ---");
    for off in (0x30..0x50usize).step_by(8) {
        let cand = cache::read_u64(method as usize + off).unwrap_or(0);
        if cand == 0 || cand < 0x10000 { continue; }
        log(&format!("  +{:#04x} -> {:#x}", off, cand));
    }

    // Candidate offsets for `flags` u32 — should have METHOD_ATTRIBUTE_STATIC (0x10)
    // bit clear for Concat (instance method? actually Concat IS static — bit should be SET).
    log("--- candidates: method_flags_off (look for 0x10 bit set on Concat) ---");
    for off in (0x40..0x58usize).step_by(4) {
        let cand = cache::read_u32(method as usize + off).unwrap_or(0);
        log(&format!("  +{:#04x} -> {:#x} (static_bit={})", off, cand, (cand & 0x10) != 0));
    }

    // Once method_parameters_off is determined (use first candidate that points
    // somewhere readable), probe ParamInfo layout: stride is usually 32 bytes,
    // type-ptr offset within is usually 0x10.
    log("--- ParamInfo layout will be probed once method_parameters_off picked ---");
    log("    bank stride (param_info_size) + type_offset (param_info_type_off)");
    log("    by reading param[0]+candidate and verifying Il2CppType.tc matches String");
    log("=== end METHODINFO PROBE ===");
}
