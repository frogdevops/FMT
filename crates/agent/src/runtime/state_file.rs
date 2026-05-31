//! Writes `<game_dir>/scripts/.state.json` atomically (temp + rename).
//! Format is documented in the B-6a spec; version 1.
//! Called on every state transition (via the orchestrator) + once every 5s
//! as heartbeat (via the watcher).

use std::fs::{rename, File};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::output_path;
use crate::runtime::registry::list;

const STATE_FILE: &str = "scripts/.state.json";
const STATE_FILE_TMP: &str = "scripts/.state.json.tmp";
const VERSION: u32 = 1;

/// Write the current registry state to the state file. Atomic via temp+rename.
/// Logs and skips on IO failure (don't crash the agent over telemetry).
pub fn write_state() {
    let path = output_path(STATE_FILE);
    let tmp_path = output_path(STATE_FILE_TMP);
    let runtimes = list();

    let ts = current_iso8601();
    let mut json = String::new();
    json.push_str("{\n");
    json.push_str(&format!("  \"version\": {},\n", VERSION));
    json.push_str(&format!("  \"ts\": \"{}\",\n", ts));
    json.push_str("  \"runtimes\": [\n");
    for (i, r) in runtimes.iter().enumerate() {
        json.push_str("    {\n");
        json.push_str(&format!("      \"id\": {},\n", r.id.0));
        json.push_str(&format!("      \"hooks_installed\": {},\n", r.hooks_installed));
        json.push_str(&format!("      \"journal_addresses\": {}\n", r.journal_addresses));
        json.push_str(if i + 1 == runtimes.len() { "    }\n" } else { "    },\n" });
    }
    json.push_str("  ]\n");
    json.push_str("}\n");

    if let Err(e) = write_atomic(&tmp_path, &path, json.as_bytes()) {
        crate::paths::log(&format!("state_file: write failed {:?}", e));
    }
}

/// Atomic write: stage to `tmp`, flush + fsync, then `rename(tmp, dest)`.
/// The inner block drops the File handle (closing it) before `rename`, so
/// even a crash during rename leaves either the prior `dest` content or the
/// new content — never a partial file.
fn write_atomic(tmp: &Path, dest: &Path, data: &[u8]) -> std::io::Result<()> {
    {
        let mut f = File::create(tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    rename(tmp, dest)?;
    Ok(())
}

/// Bare-bones ISO 8601 UTC timestamp without external deps.
fn current_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, mon, day, h, m, s) = secs_to_ymd_hms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, mon, day, h, m, s)
}

/// Convert UNIX seconds to (year, month, day, hour, minute, second) UTC.
/// Algorithm: Howard Hinnant's days_from_civil inverse. Valid 1970-2099.
fn secs_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = (secs % 60) as u32;
    let m = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153*mp + 2)/5 + 1) as u32;
    let mon = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if mon <= 2 { 1 } else { 0 }) as u32;
    (year, mon, d, h, m, s)
}
