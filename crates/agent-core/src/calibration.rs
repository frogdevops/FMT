//! Multi-candidate consensus matching — the shared primitive every
//! calibration phase uses. Pure generic; no FFI. Lives in agent-core so
//! it can be unit-tested on Linux without cross-compile.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateScore {
    pub offset:  usize,
    pub matches: u32,
    pub total:   u32,
}

/// Pick the candidate offset whose extracted value most often matches the
/// ground-truth expected value. Returns None if no candidate clears
/// `min_ratio` (typically 0.90). Among candidates that clear the threshold,
/// the one with the highest absolute match count wins.
///
/// `anchors`  = list of (subject, expected_value) pairs from FFI ground truth.
/// `extract`  = read function: given a subject + candidate offset → extracted value.
/// `min_ratio` = minimum match fraction; e.g. 0.90 = "at least 90% of anchors match".
pub fn pick_offset_by_consensus<S, V, F>(
    candidates: &[usize],
    anchors:    &[(S, V)],
    extract:    F,
    min_ratio:  f32,
) -> Option<(usize, CandidateScore)>
where
    F: Fn(&S, usize) -> Option<V>,
    V: PartialEq,
{
    if anchors.is_empty() || candidates.is_empty() {
        return None;
    }
    let total = anchors.len() as u32;
    let mut best: Option<(usize, CandidateScore)> = None;
    for &off in candidates {
        let mut matches = 0u32;
        for (subj, expected) in anchors {
            if extract(subj, off).as_ref() == Some(expected) {
                matches += 1;
            }
        }
        let ratio = matches as f32 / total as f32;
        if ratio >= min_ratio {
            let score = CandidateScore { offset: off, matches, total };
            match &best {
                None => best = Some((off, score)),
                Some((_, prev)) if matches > prev.matches => best = Some((off, score)),
                _ => {}
            }
        }
    }
    best
}
