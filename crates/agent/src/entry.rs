use std::os::raw::c_void;
use std::ptr;
use std::time::Duration;

use agent_core::logfile::write_text;
use agent_core::respect::{should_decline, DeclineReason};
use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::internals::dump::build_internals_lines;
use crate::host;
use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;
use crate::external::scan::{find_class_table, find_types_array, scan_process_for_metadata};
use crate::paths::{dump_path, log, log_path};
use crate::external::region_map::RegionMap;
use crate::internals::resolve::build_type_maps;

const DLL_PROCESS_ATTACH: u32 = 1;

type LpthreadStartRoutine = unsafe extern "system" fn(*mut c_void) -> u32;

extern "system" {
    fn CreateThread(
        lp_thread_attributes: *const c_void,
        dw_stack_size: usize,
        lp_start_address: Option<LpthreadStartRoutine>,
        lp_parameter: *const c_void,
        dw_creation_flags: u32,
        lp_thread_id: *mut u32,
    ) -> HANDLE;
}

extern "system" fn worker(_param: *mut c_void) -> u32 {
    let _ = write_text(&log_path(), "");
    log("agent loaded");

    // Anti-cheat respect gate — enumerate loaded modules and bail out cleanly
    // if any known anti-tamper system is present. We never try to bypass these;
    // running our scanners under EAC/BattlEye/Vanguard would (a) get the user
    // banned and (b) is explicitly out of scope for this project.
    let modules = host::enumerate_loaded_modules();
    if let Some(DeclineReason::AntiCheat(name)) = should_decline(&modules) {
        log(&format!("declining: anti-cheat present ({}); not engaging", name));
        log("agent terminated: respect gate");
        return 0;
    }

    log("=== RAPID CLASS DUMP ===");

    // Phase 1: find decrypted global-metadata in memory.
    log("  scanning memory for global-metadata...");
    let metadata_result = scan_process_for_metadata();
    if let Some(ref mr) = metadata_result {
        log(&format!(
            "  metadata: {} classes, blob @ {:#x} v{}",
            mr.dump.classes.len(), mr.blob_addr, mr.version
        ));
    }

    // Phase 2: locate the live class table. Game may still be loading types,
    // so retry briefly until we get a non-empty result.
    let table = (0..30).find_map(|_| {
        let t = find_class_table();
        if t.is_none() {
            std::thread::sleep(Duration::from_millis(500));
        }
        t
    });
    let (table_base, table_count) = match table {
        Some(t) => {
            log(&format!("  table @ {:#x}, {} slots", t.0, t.1));
            t
        }
        None => {
            log("  FAILED to locate class table");
            log("agent terminated: no class table");
            return 0;
        }
    };

    // Phase 3: resolve the il2cpp API. Tries standard exports first, then
    // bytecode-pattern signature scanning for obfuscated builds.
    let api = match unsafe { Il2CppApi::resolve() } {
        Some(a) => a,
        None => {
            log("  FAILED to resolve il2cpp API (neither standard exports nor signature scan succeeded)");
            log("agent terminated: no il2cpp api");
            return 0;
        }
    };

    // Phase 4: pick the per-version struct-offset config.
    let cfg = metadata_result
        .as_ref()
        .and_then(|mr| Il2CppConfig::for_metadata_version(mr.version))
        .unwrap_or_else(Il2CppConfig::default);
    let ver_str = metadata_result
        .as_ref()
        .map_or("unknown".into(), |mr| mr.version.to_string());
    log(&format!(
        "  config: metadata v{}, klass_namespace={:#x}, klass_type_def={:#x}",
        ver_str, cfg.klass_namespace, cfg.klass_type_def
    ));

    // Phase 5: wait 8s for classes to finish loading, then snapshot memory.
    log("  waiting 8s for classes to load...");
    std::thread::sleep(Duration::from_secs(8));
    let map = RegionMap::capture(8192);
    let type_maps = build_type_maps(table_base, table_count, &api, &map, &cfg);

    // Phase 6: find Il2CppMetadataRegistration.types array for typeIndex
    // resolution (requires `map`).
    let types_array = metadata_result.as_ref().and_then(|mr| {
        log(&format!("  metadata: {} type definitions", mr.type_count));
        let arr = find_types_array(mr.type_count, &map);
        match arr {
            Some(a) => log(&format!("  types array @ {:#x}", a)),
            None => log("  types array: not found"),
        }
        arr
    });

    // Phase 7: build the dump and write it.
    let (all_lines, runtime_field_count) = build_internals_lines(
        table_base,
        table_count,
        &api,
        &cfg,
        &map,
        &type_maps,
        metadata_result.as_ref(),
        types_array,
    );
    let summary = format!(
        "dumped {} classes, {} fields (runtime)\n",
        all_lines.iter().filter(|l| l.contains(" fields)")).count(),
        runtime_field_count
    );
    let _ = write_text(&dump_path(), &format!("{}{}", summary, all_lines.join("\n")));
    log(summary.trim());
    log("  wrote internals.txt");
    log("=== end RAPID CLASS DUMP ===");

    // Opt-in memory staleness probe (FROG_MEM_PROBE): re-snapshots regions on a
    // bounded timer and re-validates sampled klass pointers, to prove whether a
    // one-shot RegionMap holds up for a live session. No-op unless the env is set.
    if std::env::var("FROG_MEM_PROBE").is_ok() {
        crate::diagnostics::mem_probe::run_staleness_probe(table_base, table_count, &cfg);
    }

    // Opt-in memory WRITE probe (FROG_WRITE_PROBE): proves the guarded write
    // primitive works, its guard rejects bad targets, and a genuine game address
    // is writable. No-op unless the env is set.
    if std::env::var("FROG_WRITE_PROBE").is_ok() {
        crate::diagnostics::mem_probe::run_write_probe(table_base, table_count, &cfg);
    }

    crate::external::cache::start_refresher();

    crate::internals::ctx::init(crate::internals::ctx::InternalsCtx {
        table_base,
        table_count,
        api: api.clone(),
        cfg: cfg.clone(),
    });

    // Klass probe must run AFTER ctx::init() so find_class() has a valid context.
    if std::env::var("FROG_KLASS_PROBE").is_ok() {
        crate::diagnostics::klass_probe::run_klass_probe();
    }
    // Round-2 recon: MethodInfo layout + FieldInfo static flag.
    if std::env::var("FROG_MEMBER_PROBE").is_ok() {
        crate::diagnostics::klass_probe::run_member_probe();
    }

    crate::runtime::host::maybe_run_configured();

    // Start TCP server
    crate::protocol::start_tcp_server();

    // Install packet hooks
    unsafe {
        crate::protocol::install_packet_hooks();
    }

    log("agent running: packet hooks and TCP server active");

    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

const DLL_PROCESS_DETACH: u32 = 0;

#[no_mangle]
pub extern "system" fn DllMain(_module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            unsafe {
                CreateThread(ptr::null(), 0, Some(worker), ptr::null(), 0, ptr::null_mut());
            }
        }
        DLL_PROCESS_DETACH => {
            unsafe {
                crate::protocol::remove_packet_hooks();
            }
        }
        _ => {}
    }
    TRUE
}
