//! Phase 6: structurally detect metadata version (informational only).
//! When metadata is absent (obfuscated games), returns None; the runtime
//! config is entirely probe-derived regardless.

use crate::external::scan::scan_process_for_metadata;

pub fn probe_metadata_version() -> Option<u32> {
    let result = scan_process_for_metadata()?;
    Some(result.version)
}
