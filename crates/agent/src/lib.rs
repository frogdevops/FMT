// Windows-only agent glue lives behind cfg gates so the workspace
// still builds and tests natively on non-Windows hosts.
#[cfg(target_os = "windows")]
mod il2cpp_ffi;
#[cfg(target_os = "windows")]
mod real_runtime;
#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
mod entry;
#[cfg(target_os = "windows")]
mod mem_scan;
