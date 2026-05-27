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
use crate::internals::api;
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
        let klass = api::find_class(cname);
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
