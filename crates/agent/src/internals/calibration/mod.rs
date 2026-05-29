//! Probe-and-verify calibration — replaces version-table dispatch.
//! See docs/superpowers/specs/2026-05-30-bedrock-b1-probe-verify-design.md.
//!
//! Each phase is its own file; the orchestrator (`Il2CppConfig::probe()`)
//! lives in config.rs and calls the phase functions in dependency order.

pub mod candidates_local;  // thin wrapper around agent_core::calibration for ergonomics
pub mod anchors;           // CTX-FREE local_find_class / local_find_method shared by all phases
pub mod stability;
pub mod klass_layout;
pub mod method_layout;
pub mod type_discrim;
pub mod field_param_layout;
pub mod ffi_verify;
pub mod metadata_version;

use crate::internals::calibration::ffi_verify::VerificationReport;
use crate::internals::calibration::stability::StabilityResult;

/// Per-field outcome of a single probe.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub field_name:       &'static str,
    pub winning_offset:   Option<usize>,
    pub match_count:      u32,
    pub anchor_count:     u32,
    pub fell_back:        bool,
    pub candidates_tried: Vec<usize>,
}

impl ProbeOutcome {
    /// Format a single calibration-report line for an offset probe.
    pub fn log_line(&self) -> String {
        match (self.winning_offset, self.fell_back) {
            (Some(off), false) => format!(
                "  {:<24} +{:#06x}  match={}/{}  candidates_tried={:?}",
                self.field_name, off, self.match_count, self.anchor_count,
                self.candidates_tried
            ),
            (None, true) => format!(
                "❌ {} — no candidate >=90% (best in {:?}, scored {}/{}). Falling back to constant.",
                self.field_name, self.candidates_tried, self.match_count, self.anchor_count
            ),
            (Some(off), true) => format!(
                "⚠ {:<24} +{:#06x}  match={}/{}  USED FALLBACK (probe found candidate but discarded)",
                self.field_name, off, self.match_count, self.anchor_count
            ),
            (None, false) => format!(
                "❌ {} — probe error (no result, no fallback)", self.field_name
            ),
        }
    }
}

/// Structured calibration result returned alongside Il2CppConfig.
#[derive(Debug)]
pub struct ConfidenceReport {
    pub phase0_stability:        StabilityResult,
    pub phase1_klass:            Vec<ProbeOutcome>,
    pub phase2_method:           Vec<ProbeOutcome>,
    pub phase3_type_discrim:     Vec<ProbeOutcome>,
    pub phase4_field_param:      Vec<ProbeOutcome>,
    pub phase5_ffi:              VerificationReport,
    pub phase6_metadata_version: Option<u32>,
}

impl ConfidenceReport {
    /// Log the full calibration report block to agent.log.
    pub fn log(&self) {
        use crate::paths::log;
        log("=== CALIBRATION REPORT ===");
        log(&format!("Phase 0 (stability): {}", self.phase0_stability.summary()));
        log("Phase 1 (klass layout):");
        for o in &self.phase1_klass { log(&o.log_line()); }
        log("Phase 2 (method layout):");
        for o in &self.phase2_method { log(&o.log_line()); }
        log("Phase 3 (type discriminator):");
        for o in &self.phase3_type_discrim { log(&o.log_line()); }
        log("Phase 4 (field+param layout):");
        for o in &self.phase4_field_param { log(&o.log_line()); }
        log("Phase 5 (FFI verify):");
        for line in self.phase5_ffi.lines() { log(&line); }
        log(&format!(
            "Phase 6 (metadata version): {}",
            self.phase6_metadata_version
                .map(|v| format!("{} (probed)", v))
                .unwrap_or_else(|| "NOT FOUND (obfuscated)".to_string())
        ));
        log("=== END CALIBRATION ===");
    }
}
