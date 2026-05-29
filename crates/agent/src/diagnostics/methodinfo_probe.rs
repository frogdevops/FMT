//! One-shot probe (opt-in `FROG_METHODINFO_PROBE`): derives the 5 MethodInfo /
//! ParamInfo offsets structurally. Anchors on `System::Math::Pow(double, double)`
//! — single overload, return type R8 (tc=0x0D), both params R8 (tc=0x0D). Falls
//! back to `System::Single::IsNaN(float)` if Math.Pow not present.
//!
//! v3 over v2: unambiguous anchor + raw hex dump of the candidate parameters
//! base so the operator sees the actual ParameterInfo layout instead of
//! guessing strides.

use crate::external::cache;
use crate::internals::api;
use crate::internals::ctx;
use crate::paths::log;

const IL2CPP_TYPE_R8: u8 = 0x0D;
const IL2CPP_TYPE_R4: u8 = 0x0C;
const IL2CPP_TYPE_BOOLEAN: u8 = 0x02;

fn read_tc(type_ptr: usize) -> u8 {
    let c = match ctx::get() { Some(c) => c, None => return 0 };
    let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8
}

fn classify(label: &str, ptr: u64, expected_tc: u8, expected_name: &str) -> String {
    if ptr == 0 || ptr < 0x10000 {
        return format!("{}: ptr={:#x} (invalid)", label, ptr);
    }
    let tc = read_tc(ptr as usize);
    if tc == expected_tc {
        format!("{}: ptr={:#x} tc={:#04x} {} ✓", label, ptr, tc, expected_name)
    } else {
        format!("{}: ptr={:#x} tc={:#04x}", label, ptr, tc)
    }
}

struct Anchor {
    class: &'static str,
    method: &'static str,
    argc: u32,
    return_tc: u8,
    param_tc: u8,
    name: &'static str,
}

const ANCHORS: &[Anchor] = &[
    Anchor { class: "System::Math",   method: "Pow",   argc: 2, return_tc: IL2CPP_TYPE_R8, param_tc: IL2CPP_TYPE_R8, name: "R8 (double)" },
    Anchor { class: "System::Single", method: "IsNaN", argc: 1, return_tc: IL2CPP_TYPE_BOOLEAN, param_tc: IL2CPP_TYPE_R4, name: "R4 (float)" },
];

pub fn run_methodinfo_probe() {
    log("=== METHODINFO PROBE v3 (unambiguous anchor + raw dump) ===");

    let (anchor, _klass, method) = {
        let mut found = None;
        for a in ANCHORS {
            let k = api::find_class(a.class);
            if k == 0 { log(&format!("anchor {}: class not found", a.class)); continue; }
            let m = api::find_method(k, a.method, a.argc);
            if m == 0 { log(&format!("anchor {}::{}({}) not found", a.class, a.method, a.argc)); continue; }
            log(&format!("anchor: {}::{}({}) @ {:#x}  (return={}, params={})",
                         a.class, a.method, a.argc, m, a.name, a.name));
            found = Some((a, k, m));
            break;
        }
        match found {
            Some(t) => t,
            None => { log("methodinfo probe: no usable anchor found"); return; }
        }
    };

    // ── 1. return_type at +0x28 — sanity check ─────────────────────────
    log("--- return_type sanity (expect tc to match anchor's return type) ---");
    let rt = cache::read_u64(method as usize + 0x28).unwrap_or(0);
    log(&format!("  +0x28  {}", classify("return_type", rt, anchor.return_tc, anchor.name)));

    // ── 2. Scan +0x28..+0x60 for parameters ptr — looking for a ptr whose
    //       deref+type_off lookup yields anchor.param_tc ────────────────
    log("--- scanning +0x28..+0x60 for parameters base (deref + check param[0].type tc) ---");
    let mut param_base_candidates: Vec<(usize, u64)> = Vec::new();
    for off in (0x28..0x60usize).step_by(8) {
        let base_ptr = cache::read_u64(method as usize + off).unwrap_or(0);
        if base_ptr == 0 || base_ptr < 0x10000 { continue; }
        // For each candidate type_off inside ParameterInfo, check tc.
        let mut hit = None;
        for type_off in [0x00usize, 0x08, 0x10, 0x18, 0x20] {
            let type_ptr = cache::read_u64(base_ptr as usize + type_off).unwrap_or(0);
            if type_ptr == 0 || type_ptr < 0x10000 { continue; }
            let tc = read_tc(type_ptr as usize);
            if tc == anchor.param_tc {
                hit = Some((type_off, type_ptr, tc));
                break;
            }
        }
        match hit {
            Some((to, tp, tc)) => {
                log(&format!("  +{:#04x}  base={:#x}  type_off={:#04x}  type_ptr={:#x} tc={:#04x} MATCH",
                             off, base_ptr, to, tp, tc));
                param_base_candidates.push((off, base_ptr));
            }
            None => {
                log(&format!("  +{:#04x}  base={:#x}  no param_tc match at type_off in {{0,8,16,24,32}}",
                             off, base_ptr));
            }
        }
    }

    if param_base_candidates.is_empty() {
        log("no parameters base found; aborting");
        return;
    }

    // ── 3. Raw hex dump around the first matching base — 0x40 bytes ────
    let (best_off, best_base) = param_base_candidates[0];
    log(&format!("--- raw hex dump of parameters base @ {:#x} (from method+{:#04x}) ---",
                 best_base, best_off));
    let base_u = best_base as usize;
    for row in 0..8 {
        let start = base_u + row * 8;
        let v = cache::read_u64(start).unwrap_or(0);
        log(&format!("  +{:#04x}  {:#018x}", row * 8, v));
    }

    // ── 4. For the best base, deref each 8-byte slot as a possible
    //       Il2CppType ptr and check tc ────────────────────────────────
    log("--- type-tc check at each 8-byte slot in the first 0x40 bytes ---");
    for row in 0..8 {
        let off = row * 8;
        let p = cache::read_u64(base_u + off).unwrap_or(0);
        if p < 0x10000 { continue; }
        let tc = read_tc(p as usize);
        let marker = if tc == anchor.param_tc { " ← param_tc MATCH" } else { "" };
        log(&format!("  slot +{:#04x}  ptr={:#x} tc={:#04x}{}", off, p, tc, marker));
    }

    // ── 5. flags at +0x4C ──────────────────────────────────────────────
    let flags = cache::read_u32(method as usize + 0x4C).unwrap_or(0);
    log(&format!("--- flags @ +0x4C = {:#x}  (static_bit (0x10) = {})  ---",
                 flags, (flags & 0x10) != 0));

    log("=== end METHODINFO PROBE v3 ===");
}
