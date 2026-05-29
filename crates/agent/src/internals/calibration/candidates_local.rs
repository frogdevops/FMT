//! Re-export of agent_core's pick_offset_by_consensus for ergonomic
//! use inside the agent crate. Pure pass-through.

pub use agent_core::calibration::{pick_offset_by_consensus, CandidateScore};
