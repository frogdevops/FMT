//! Glue between the agent's RegionMap and agent-core's bedrock discovery engine.
//!
//! Implements `MemView` for `RegionMap` (forwarding to its inherent readers) so
//! the pure bedrock logic can run on live process memory, then provides the
//! `run_layout_probe` entry point gated on `FROG_LAYOUT_PROBE`.

use agent_core::bedrock::{Fact, Layout, MemView};
use agent_core::bedrock::discover::discover;

use crate::external::region_map::RegionMap;
use crate::paths::log;

// ── MemView impl ─────────────────────────────────────────────────────────────

impl MemView for RegionMap {
    fn read_u64(&self, addr: usize) -> Option<u64> {
        // Call the INHERENT method explicitly to avoid calling back into this
        // trait method (RegionMap has same-named inherent methods; the explicit
        // form `RegionMap::read_u64(self, a)` resolves to the inherent, never
        // the trait, so there is no unconditional_recursion risk).
        RegionMap::read_u64(self, addr)
    }

    fn read_u32(&self, addr: usize) -> Option<u32> {
        RegionMap::read_u32(self, addr)
    }

    fn read_u8(&self, addr: usize) -> Option<u8> {
        RegionMap::read_u8(self, addr)
    }

    fn read_cstr(&self, addr: usize) -> Option<String> {
        // Forwards to the differently-named inherent method; strict printable-ASCII
        // filter is intentional — discovery code validates unknown memory.
        RegionMap::read_name_strict(self, addr)
    }

    fn is_exec(&self, addr: usize) -> bool {
        // Uses the VirtualQuery-backed protection tag from klass_probe so the
        // classification matches the rest of the probe suite.
        matches!(
            crate::diagnostics::klass_probe::protect_of(addr),
            "RX" | "RWX"
        )
    }
}

// ── Layout probe entry point ──────────────────────────────────────────────────

/// Run the full bedrock layout discovery and log a structured report.
/// Gated externally on `FROG_LAYOUT_PROBE`; called from `entry.rs`.
pub fn run_layout_probe(map: &RegionMap, table_base: usize, table_count: usize) {
    log("=== LAYOUT PROBE (bedrock discovery engine) ===");
    let layout = discover(map, table_base, table_count);
    log_layout(&layout);
    log("=== end LAYOUT PROBE ===");
}

// ── Report generation ─────────────────────────────────────────────────────────

/// Format a `Fact<usize>` for the layout report. Every number comes from the
/// Fact itself — no hand-written values appear here.
fn fmt_usize(label: &str, fact: &Fact<usize>) -> String {
    match fact {
        Fact::Resolved { value, provenance } => {
            let witnesses: String = provenance
                .witnesses
                .iter()
                .map(|w| format!("{:?} observed={:#x} signal={}", w.method, w.observed, w.signal))
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "  {} = {:#x}  [{} ; sampled={}]",
                label, value, witnesses, provenance.sampled
            )
        }
        Fact::Unresolved { reason } => format!("  {} = UNRESOLVED({:?})", label, reason),
    }
}

/// Format a `Fact<u8>` for the layout report. Same contract: values come from
/// the Fact, not from any hand-written constant.
fn fmt_u8(label: &str, fact: &Fact<u8>) -> String {
    match fact {
        Fact::Resolved { value, provenance } => {
            let witnesses: String = provenance
                .witnesses
                .iter()
                .map(|w| format!("{:?} observed={:#x} signal={}", w.method, w.observed, w.signal))
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "  {} = {:#x}  [{} ; sampled={}]",
                label, value, witnesses, provenance.sampled
            )
        }
        Fact::Unresolved { reason } => format!("  {} = UNRESOLVED({:?})", label, reason),
    }
}

/// Iterate every field in `Layout` and emit one line per Fact. All 22 facts are
/// printed; every numeric value is read from the Fact — this function never
/// asserts a hand-written offset.
fn log_layout(layout: &Layout) {
    log(&fmt_usize("table_base",             &layout.table_base));
    log(&fmt_usize("table_count",            &layout.table_count));
    log(&fmt_usize("class_table_step",       &layout.class_table_step));
    log(&fmt_usize("klass_namespace",        &layout.klass_namespace));
    log(&fmt_usize("klass_fields",           &layout.klass_fields));
    log(&fmt_usize("klass_methods",          &layout.klass_methods));
    log(&fmt_usize("klass_static_fields",    &layout.klass_static_fields));
    log(&fmt_usize("klass_type_def",         &layout.klass_type_def));
    log(&fmt_usize("klass_generic_class",    &layout.klass_generic_class));
    log(&fmt_usize("klass_valuetype_off",    &layout.klass_valuetype_off));
    log(&fmt_u8(   "klass_valuetype_bit",    &layout.klass_valuetype_bit));
    log(&fmt_usize("type_discrim_read_at",   &layout.type_discrim_read_at));
    log(&fmt_u8(   "discrim_shift",          &layout.discrim_shift));
    log(&fmt_usize("method_pointer_off",     &layout.method_pointer_off));
    log(&fmt_usize("method_klass_off",       &layout.method_klass_off));
    log(&fmt_usize("method_name_off",        &layout.method_name_off));
    log(&fmt_usize("method_param_count_off", &layout.method_param_count_off));
    log(&fmt_usize("method_return_type_off", &layout.method_return_type_off));
    log(&fmt_usize("method_parameters_off",  &layout.method_parameters_off));
    log(&fmt_usize("method_flags_off",       &layout.method_flags_off));
    log(&fmt_usize("param_info_size",        &layout.param_info_size));
    log(&fmt_usize("param_info_type_off",    &layout.param_info_type_off));
}
