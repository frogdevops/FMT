#[cfg(target_os = "windows")]
mod il2cpp_ffi;
#[cfg(target_os = "windows")]
mod il2cpp_config;
#[cfg(target_os = "windows")]
mod entry;
#[cfg(target_os = "windows")]
mod mem_scan;
#[cfg(target_os = "windows")]
mod host;
#[cfg(target_os = "windows")]
mod paths;
#[cfg(target_os = "windows")]
mod region_map;
#[cfg(target_os = "windows")]
mod type_resolve;
#[cfg(target_os = "windows")]
mod dump_writer;
#[cfg(target_os = "windows")]
mod hook;
#[cfg(target_os = "windows")]
mod bson;
#[cfg(target_os = "windows")]
mod packet;
#[cfg(target_os = "windows")]
mod wasm_host;
#[cfg(target_os = "windows")]
mod mem_write;
#[cfg(target_os = "windows")]
mod mem_probe;
