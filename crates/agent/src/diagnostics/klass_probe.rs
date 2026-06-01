//! FIND-FIRST recon (opt-in `FROG_KLASS_PROBE`): dump the `Il2CppClass` struct
//! layout for a few sample classes and chase every pointer slot, so we can locate
//! the `methods`/`method_count` offsets (for `find_method`) and the
//! `static_fields` base (for `static_field`). No existing machinery resolves
//! those — derive them structurally from this dump. Read-only, bounded,
//! crash-safe (every read validated through the region cache / VirtualQuery).

use std::ffi::c_void;

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ,
    PAGE_EXECUTE_READWRITE, PAGE_READONLY, PAGE_READWRITE,
};

use crate::external::cache;
use crate::external::region_map::RegionMap;
use crate::internals::api;
use crate::internals::ctx;
use crate::paths::log;

/// Page protection at `addr`, as a short tag. Lets us spot `static_fields`
/// (points into a RW data region) vs code (RX) vs metadata (RO).
fn protect_of(addr: usize) -> &'static str {
    unsafe {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        if VirtualQuery(addr as *const c_void, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
            return "?";
        }
        if mbi.State != MEM_COMMIT {
            return "free";
        }
        let p = mbi.Protect;
        if p & PAGE_EXECUTE_READWRITE != 0 { "RWX" }
        else if p & PAGE_EXECUTE_READ != 0 { "RX" }
        else if p & PAGE_READWRITE != 0 { "RW" }
        else if p & PAGE_READONLY != 0 { "RO" }
        else { "oth" }
    }
}

fn short(s: &str) -> String {
    if s.len() > 24 { format!("{}…", &s[..24]) } else { s.to_string() }
}

/// Annotate one klass-struct slot value.
fn annotate(v: u64) -> String {
    if v == 0 {
        return String::new();
    }
    let vu = v as usize;
    if v < 0x10_0000 {
        return format!("int({})", v);
    }
    // Readable pointer?
    let t0 = match cache::read_u64(vu) {
        Some(t) => t,
        None => return "deadptr".to_string(),
    };
    let prot = protect_of(vu);
    let t1 = cache::read_u64(vu + 8).unwrap_or(0);
    let t2 = cache::read_u64(vu + 16).unwrap_or(0);
    let t3 = cache::read_u64(vu + 24).unwrap_or(0);
    // String AT v? (e.g. a `name` field)
    let str_here = cache::read_cstr(vu).filter(|s| s.len() >= 2).map(|s| format!(" =\"{}\"", short(&s)));
    // String at *v (v is an array/struct whose first ptr → a name; reveals
    // the fields array AND, hopefully, the methods array: methods→MethodInfo*→name).
    let str_t0 = cache::read_cstr(t0 as usize).filter(|s| s.len() >= 2).map(|s| format!(" t0→\"{}\"", short(&s)));
    format!(
        "[{}]{}{} →[{:#x},{:#x},{:#x},{:#x}]",
        prot,
        str_here.unwrap_or_default(),
        str_t0.unwrap_or_default(),
        t0, t1, t2, t3
    )
}

pub fn run_klass_probe() {
    log("=== KLASS PROBE (FIND-FIRST: methods + static_fields) ===");
    for cname in ["Player", "GameManager", "World", "PlayerData"] {
        let klass = api::find_class(cname).map(|k| k.as_u64()).unwrap_or(0);
        if klass == 0 {
            log(&format!("  {} : NOT FOUND", cname));
            continue;
        }
        log(&format!("--- {} @ {:#x} ---", cname, klass));
        let k = klass as usize;
        // Dump well past klass_fields (0x80) into where methods/static_fields live.
        for off in (0..0x160usize).step_by(8) {
            let v = match cache::read_u64(k + off) {
                Some(v) => v,
                None => {
                    log(&format!("  +{:#05x}: <unreadable>", off));
                    continue;
                }
            };
            log(&format!("  +{:#05x}: {:#018x}  {}", off, v, annotate(v)));
        }
    }
    log("=== end KLASS PROBE ===");
}

/// Round-2 recon (opt-in `FROG_MEMBER_PROBE`): dump `Player`'s MethodInfo structs
/// (to derive the `name`/`param_count` layout for the structural `find_method`
/// over `methods @ klass+0x98`) and its FieldInfo type-attrs (to find the static
/// flag for `static_field`, base at `klass+0xB8`). Read-only, bounded, crash-safe.
pub fn run_member_probe() {
    let c = match ctx::get() {
        Some(c) => c,
        None => { log("member probe: no internals ctx"); return; }
    };
    let klass = api::find_class("Player").map(|k| k.as_u64() as usize).unwrap_or(0);
    if klass == 0 {
        log("member probe: Player not found");
        return;
    }
    let kf = c.cfg.klass_fields; // 0x80
    log(&format!("=== MEMBER PROBE: Player @ {:#x} (klass_fields={:#x}) ===", klass, kf));

    // methods array = klass + fields + 0x18 ; each entry → MethodInfo*.
    // Dump 14 words UNCONDITIONALLY, annotating each: a readable string (name?),
    // or a pointer with its protection (RX = methodPointer/code). This reveals the
    // MethodInfo layout (name offset, methodPointer, and param_count near 0x48).
    let methods = cache::read_u64(klass + kf + 0x18).unwrap_or(0) as usize;
    log(&format!("--- methods @ {:#x} (14 words/MethodInfo; \"str\"=name, [RX]=code) ---", methods));
    for i in 0..16usize {
        let mi = match cache::read_u64(methods + i * 8) {
            Some(v) if v != 0 => v as usize,
            _ => break,
        };
        let mut parts = Vec::new();
        for j in 0..14usize {
            let w = cache::read_u64(mi + j * 8).unwrap_or(0);
            let tag = match cache::read_cstr(w as usize) {
                Some(s) if s.len() >= 2 && s.len() < 48 => format!("\"{}\"", s),
                _ if w >= 0x1000 => format!("{:#x}[{}]", w, protect_of(w as usize)),
                _ => format!("{:#x}", w),
            };
            parts.push(tag);
        }
        log(&format!("  m[{:>2}]@{:#x}: {}", i, mi, parts.join(" ")));
    }

    // fields array = klass + fields ; 32-byte FieldInfo {name@0,type@8,parent@16,offset@24,token@28}
    let fields = cache::read_u64(klass + kf).unwrap_or(0) as usize;
    log("--- fields (name off=offset tc typechunk — static flag should distinguish) ---");
    for i in 0..40usize {
        let fi = fields + i * 32;
        let name = match cache::read_u64(fi).and_then(|p| cache::read_cstr(p as usize)) {
            Some(n) if !n.is_empty() => n,
            _ => break,
        };
        let type_ptr = cache::read_u64(fi + 8).unwrap_or(0) as usize;
        let offset = cache::read_u32(fi + 24).unwrap_or(0);
        let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
        let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
        log(&format!("  f[{:>2}] \"{}\" off={:#x} tc={:#x} typechunk={:#018x}", i, name, offset, tc, chunk));
    }
    log("=== end MEMBER PROBE ===");
}

// ───────────────────────────────────────────────────────────────────────────
// RECOGNIZER PROBE (opt-in `FROG_RECOGNIZER_PROBE`)
//
// Proves the container-first, NON-CIRCULAR discovery the bedrock redesign rests
// on: find the `methods` array by intrinsic structure — a pointer-array whose
// entries are MethodInfo-shaped (each contains >=1 executable (RX) pointer AND
// >=1 pointer back to the owning klass) — then DERIVE method_pointer_off /
// method_klass_off / method_name_off by classifying that MethodInfo's own slots.
// Zero hardcoded sub-offsets, so it cannot cascade the way the current
// `probe_klass_methods` does (it assumes methodPointer @ +0x08 and false-fails
// when that assumption is wrong — e.g. PW's real method_pointer_off is 0x0).
//
// Crash-safe: every read goes through the passed RegionMap (VirtualQuery-backed,
// bounds-checked, never faults). Klasses are sampled from the table structurally
// (no find_class-by-name, which 404s across games and chases FFI — the thing
// that crashed the earlier probe on PW).
// ───────────────────────────────────────────────────────────────────────────

fn is_exec(addr: usize) -> bool {
    matches!(protect_of(addr), "RX" | "RWX")
}

/// A struct at `p` is MethodInfo-shaped if, within its first 0x60 bytes, it holds
/// >=1 executable pointer (compiled code) AND >=1 pointer equal to `klass` (the
/// declaring-class back-pointer). No sub-offset is assumed.
fn looks_methodinfo(map: &RegionMap, p: usize, klass: usize) -> bool {
    let (mut rx, mut back) = (false, false);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = map.read_u64(p + j) {
            let wu = w as usize;
            if wu == klass {
                back = true;
            } else if wu >= 0x10_0000 && is_exec(wu) {
                rx = true;
            }
        }
        j += 8;
    }
    rx && back
}

/// Find the `methods` offset(s) by structure: klass+off → array of pointers
/// whose first two entries are both MethodInfo-shaped for THIS klass. Two
/// consecutive MethodInfo-shaped back-pointers to the same klass do not occur
/// by chance, so this needs no candidate window and no sub-offset.
fn recognize_methods(map: &RegionMap, klass: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut off = 0x40usize;
    while off < 0x108 {
        if let Some(arr) = map.read_u64(klass + off) {
            let arr = arr as usize;
            let e0 = map.read_u64(arr).unwrap_or(0) as usize;
            let e1 = map.read_u64(arr + 8).unwrap_or(0) as usize;
            if e0 >= 0x10_0000
                && e1 >= 0x10_0000
                && looks_methodinfo(map, e0, klass)
                && looks_methodinfo(map, e1, klass)
            {
                hits.push(off);
            }
        }
        off += 8;
    }
    hits
}

/// Given the first MethodInfo `mi`, derive sub-offsets by classifying its slots:
/// the executable-ptr slot → method_pointer_off; the ==klass slot →
/// method_klass_off; the readable-name slot → method_name_off. First of each.
fn derive_method_suboffsets(
    map: &RegionMap,
    mi: usize,
    klass: usize,
) -> (Option<usize>, Option<usize>, Option<usize>) {
    let (mut mp, mut mk, mut mn) = (None, None, None);
    let mut j = 0usize;
    while j < 0x60 {
        if let Some(w) = map.read_u64(mi + j) {
            let wu = w as usize;
            if mk.is_none() && wu == klass {
                mk = Some(j);
            } else if mp.is_none() && wu >= 0x10_0000 && is_exec(wu) {
                mp = Some(j);
            } else if mn.is_none() {
                if let Some(s) = map.read_name_strict(wu) {
                    if s.len() >= 2 && s.len() < 64 {
                        mn = Some(j);
                    }
                }
            }
        }
        j += 8;
    }
    (mp, mk, mn)
}

/// Tally a value into a (value, count) vote list.
fn tally(v: &mut Vec<(usize, u32)>, k: usize) {
    if let Some(e) = v.iter_mut().find(|e| e.0 == k) {
        e.1 += 1;
    } else {
        v.push((k, 1));
    }
}

fn fmt_votes(v: &[(usize, u32)]) -> String {
    v.iter()
        .map(|(o, c)| format!("{:#x}×{}", o, c))
        .collect::<Vec<_>>()
        .join("  ")
}

pub fn run_recognizer_probe(map: &RegionMap, table_base: usize, table_count: usize) {
    log("=== RECOGNIZER PROBE (container-first, zero hardcoded sub-offsets) ===");
    const STEP: usize = 8;
    const WANT: usize = 12;
    let mut tested = 0usize;
    let mut methods_votes: Vec<(usize, u32)> = Vec::new();
    let mut mp_votes: Vec<(usize, u32)> = Vec::new();
    let mut mk_votes: Vec<(usize, u32)> = Vec::new();
    let mut mn_votes: Vec<(usize, u32)> = Vec::new();

    let mut i = 0usize;
    while tested < WANT && i < table_count {
        let slot = table_base + i * STEP;
        i += 1;
        let k = match map.read_u64(slot) {
            Some(v) if v != 0 => v as usize,
            _ => continue,
        };
        // Structural validity (the verified-sound root validator).
        let (name, _ns) = match map.class_fields(k) {
            Some(x) => x,
            None => continue,
        };
        let m_hits = recognize_methods(map, k);
        if m_hits.is_empty() {
            continue; // skip classes with no recognizable methods array (0-method types)
        }
        tested += 1;
        let mo = m_hits[0];
        let arr = map.read_u64(k + mo).unwrap_or(0) as usize;
        let mi0 = map.read_u64(arr).unwrap_or(0) as usize;
        let (mp, mk, mn) = derive_method_suboffsets(map, mi0, k);
        tally(&mut methods_votes, mo);
        if let Some(v) = mp {
            tally(&mut mp_votes, v);
        }
        if let Some(v) = mk {
            tally(&mut mk_votes, v);
        }
        if let Some(v) = mn {
            tally(&mut mn_votes, v);
        }
        log(&format!(
            "  {:<28} methods=[{}] mp_off={} mk_off={} mn_off={}",
            short(&name),
            m_hits
                .iter()
                .map(|o| format!("{:#x}", o))
                .collect::<Vec<_>>()
                .join(","),
            mp.map_or("?".into(), |v| format!("{:#x}", v)),
            mk.map_or("?".into(), |v| format!("{:#x}", v)),
            mn.map_or("?".into(), |v| format!("{:#x}", v)),
        ));
    }

    methods_votes.sort_by(|a, b| b.1.cmp(&a.1));
    mp_votes.sort_by(|a, b| b.1.cmp(&a.1));
    mk_votes.sort_by(|a, b| b.1.cmp(&a.1));
    mn_votes.sort_by(|a, b| b.1.cmp(&a.1));
    log(&format!("  --- consensus over {} klasses ---", tested));
    log(&format!("  methods_off                 : {}", fmt_votes(&methods_votes)));
    log(&format!("  method_pointer_off (DERIVED): {}", fmt_votes(&mp_votes)));
    log(&format!("  method_klass_off   (DERIVED): {}", fmt_votes(&mk_votes)));
    log(&format!("  method_name_off    (DERIVED): {}", fmt_votes(&mn_votes)));
    log("  baseline that CASCADES: probe_klass_methods assumes methodPointer @ +0x08");
    log("  → PROOF: if methods_off consensus is unanimous AND derived method_pointer_off != 0x08,");
    log("    the structural recognizer is independent of the cascade and the bug is confirmed.");
    log("=== end RECOGNIZER PROBE ===");
}
