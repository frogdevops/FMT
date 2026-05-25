//! Output-file paths and the log helper.
//!
//! All artefacts (`agent.log`, `internals.txt`) are anchored to the directory
//! containing `agent.dll`, not the launcher's current working directory.
//! That way the IDE plugin can find them regardless of how Steam/Proton
//! invoked the game.

use std::path::PathBuf;

use agent_core::logfile::append_log;

use crate::host;

/// Resolve a file next to the agent DLL itself, falling back to the launcher's
/// CWD if the loader can't locate our module (very rare).
pub fn output_path(filename: &str) -> PathBuf {
    host::agent_dir()
        .map(|d| d.join(filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

pub fn log_path() -> PathBuf {
    output_path("agent.log")
}

pub fn dump_path() -> PathBuf {
    output_path("internals.txt")
}

/// Append one line to `agent.log`. Errors are silently ignored — there's no
/// useful recovery from a logging failure inside an injected DLL.
pub fn log(line: &str) {
    let _ = append_log(&log_path(), line);
}
