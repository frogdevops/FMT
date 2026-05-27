//! External domain: raw process memory — region snapshot + bounds-checked reads,
//! AOB/metadata scanning, and guarded writes. Reliability-proven (read + write).

pub mod region_map;
pub mod scan;
pub mod write;
