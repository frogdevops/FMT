//! Internals domain: il2cpp metadata — API resolution, per-version offsets,
//! type-name resolution (string-heap derived), and the batch dump. Reliability-proven.

pub mod ffi;
pub mod config;
pub mod resolve;
pub mod dump;
