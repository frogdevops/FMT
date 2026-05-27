//! Resolved il2cpp context, populated by the worker once after resolution and
//! read by the `il2cpp.*` host functions. Holds only Send+Sync data (fn pointers
//! + offsets + table bounds).

use std::sync::OnceLock;

use crate::internals::config::Il2CppConfig;
use crate::internals::ffi::Il2CppApi;

pub struct InternalsCtx {
    pub table_base: usize,
    pub table_count: usize,
    pub api: Il2CppApi,
    pub cfg: Il2CppConfig,
}

// Fn pointers + plain offsets are safe to share across threads; the worker sets
// this once before any host fn can read it.
unsafe impl Send for InternalsCtx {}
unsafe impl Sync for InternalsCtx {}

static CTX: OnceLock<InternalsCtx> = OnceLock::new();

/// Called once by the worker after il2cpp resolution. Later calls are ignored.
pub fn init(ctx: InternalsCtx) {
    let _ = CTX.set(ctx);
}

pub fn get() -> Option<&'static InternalsCtx> {
    CTX.get()
}
