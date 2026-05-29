//! One-shot probe (opt-in `FROG_METHODINFO_PROBE`): derives the 5 MethodInfo /
//! ParamInfo offsets structurally.
//!
//! v4: keeps the v3 Math.Pow / Single.IsNaN anchor for the first 4 offsets,
//! adds a second anchor (`System::String::PadLeft(Int32, Char)`) specifically
//! for `param_info_size` — its two params have distinct tcs (I4=0x08 and
//! CHAR=0x03) so the right stride is unambiguous.

use crate::external::cache;
use crate::internals::api;
use crate::internals::ctx;
use crate::paths::log;

const IL2CPP_TYPE_BOOLEAN: u8 = 0x02;
const IL2CPP_TYPE_CHAR:    u8 = 0x03;
const IL2CPP_TYPE_I4:      u8 = 0x08;
const IL2CPP_TYPE_R4:      u8 = 0x0C;
const IL2CPP_TYPE_R8:      u8 = 0x0D;

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

const ANCHORS_RETURN_PARAMS: &[Anchor] = &[
    Anchor { class: "System::Math",   method: "Pow",   argc: 2, return_tc: IL2CPP_TYPE_R8,      param_tc: IL2CPP_TYPE_R8, name: "R8" },
    Anchor { class: "System::Single", method: "IsNaN", argc: 1, return_tc: IL2CPP_TYPE_BOOLEAN, param_tc: IL2CPP_TYPE_R4, name: "R4" },
];

pub fn run_methodinfo_probe() {
    log("=== METHODINFO PROBE v4 ===");

    // ── Phase A — same-type anchor to lock 4 offsets ─────────────────────
    log("--- phase A: same-type anchor (locks return/parameters/flags/type_off) ---");
    let mut found_a = None;
    for a in ANCHORS_RETURN_PARAMS {
        let k = api::find_class(a.class);
        if k == 0 { continue; }
        let m = api::find_method(k, a.method, a.argc);
        if m == 0 { continue; }
        log(&format!("phase A anchor: {}::{}({}) @ {:#x}", a.class, a.method, a.argc, m));
        found_a = Some((a, m));
        break;
    }
    let (anchor_a, method_a) = match found_a {
        Some(x) => x,
        None => { log("phase A: no anchor found, aborting"); return; }
    };

    // return_type at +0x28
    let rt = cache::read_u64(method_a as usize + 0x28).unwrap_or(0);
    log(&format!("  return_type +0x28  {}", classify("ret", rt, anchor_a.return_tc, anchor_a.name)));

    // parameters at +0x30
    let params_a = cache::read_u64(method_a as usize + 0x30).unwrap_or(0);
    log(&format!("  parameters +0x30   base={:#x}", params_a));
    // param[0] type at +0x00
    let p0_a = cache::read_u64(params_a as usize).unwrap_or(0);
    log(&format!("  param[0]@+0x00     {}", classify("type", p0_a, anchor_a.param_tc, anchor_a.name)));

    // flags at +0x4C
    let flags_a = cache::read_u32(method_a as usize + 0x4C).unwrap_or(0);
    log(&format!("  flags +0x4C        {:#x}  (static_bit={})", flags_a, (flags_a & 0x10) != 0));

    // ── Phase B — distinct-type anchor for stride ────────────────────────
    log("--- phase B: PadLeft(Int32, Char) for stride disambiguation ---");
    let string_klass = api::find_class("System::String");
    if string_klass == 0 {
        log("phase B: System::String not found, aborting stride probe");
        return;
    }
    let pad_method = api::find_method(string_klass, "PadLeft", 2);
    if pad_method == 0 {
        log("phase B: String::PadLeft(2) not found, aborting stride probe");
        return;
    }
    log(&format!("phase B anchor: String::PadLeft(2) @ {:#x}", pad_method));

    let params_b = cache::read_u64(pad_method as usize + 0x30).unwrap_or(0) as usize;
    log(&format!("  parameters base    {:#x}", params_b));

    // Raw hex dump of first 0x40 bytes
    log("  raw dump:");
    for row in 0..8 {
        let v = cache::read_u64(params_b + row * 8).unwrap_or(0);
        log(&format!("    +{:#04x}  {:#018x}", row * 8, v));
    }

    // Check param[0].type (must be I4 = 0x08)
    let p0_b = cache::read_u64(params_b).unwrap_or(0);
    log(&format!("  param[0]@+0x00     {}", classify("type", p0_b, IL2CPP_TYPE_I4, "I4 (Int32)")));

    // For each candidate stride, read param[1] type ptr and check for CHAR (0x03).
    log("  param[1] candidates by stride:");
    for stride in [0x08usize, 0x10, 0x18, 0x20, 0x28] {
        let p1 = cache::read_u64(params_b + stride).unwrap_or(0);
        log(&format!("    stride={:#04x}   {}",
                     stride, classify("type", p1, IL2CPP_TYPE_CHAR, "CHAR")));
    }

    log("=== end METHODINFO PROBE v4 ===");
}
