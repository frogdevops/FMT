use agent_core::calibration::{pick_offset_by_consensus, CandidateScore};

#[test]
fn single_clear_winner() {
    // Anchors: 10 pairs where offset 0x18 always extracts the expected value.
    let anchors: Vec<(usize, &str)> = (0..10).map(|i| (i, "EXPECTED")).collect();
    let candidates = [0x10, 0x18, 0x20];
    let extract = |subject: &usize, off: usize| -> Option<&'static str> {
        if off == 0x18 { Some("EXPECTED") } else { None }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    let (off, score) = result.expect("should find winner");
    assert_eq!(off, 0x18);
    assert_eq!(score.matches, 10);
    assert_eq!(score.total, 10);
}

#[test]
fn no_candidate_clears_threshold() {
    let anchors: Vec<(usize, &str)> = (0..10).map(|i| (i, "X")).collect();
    let candidates = [0x10, 0x18, 0x20];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { None };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn multiple_above_threshold_picks_highest() {
    let anchors: Vec<(usize, u32)> = (0..10).map(|i| (i, 42u32)).collect();
    let candidates = [0x10, 0x18];
    let extract = |subject: &usize, off: usize| -> Option<u32> {
        match (subject, off) {
            (_, 0x10) if *subject < 9 => Some(42),  // 9 of 10 match
            (_, 0x18) => Some(42),                   // 10 of 10 match
            _ => None,
        }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    let (off, score) = result.expect("should find winner");
    assert_eq!(off, 0x18, "should prefer the higher-scoring candidate");
    assert_eq!(score.matches, 10);
}

#[test]
fn empty_anchors_returns_none() {
    let anchors: Vec<(usize, &str)> = vec![];
    let candidates = [0x10];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { Some("X") };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn empty_candidates_returns_none() {
    let anchors: Vec<(usize, &str)> = vec![(0, "X")];
    let candidates: [usize; 0] = [];
    let extract = |_: &usize, _: usize| -> Option<&'static str> { Some("X") };
    assert!(pick_offset_by_consensus(&candidates, &anchors, extract, 0.90).is_none());
}

#[test]
fn threshold_exactly_at_boundary() {
    // 9 of 10 matches = 0.90 exactly → should win at min_ratio=0.90.
    let anchors: Vec<(usize, u32)> = (0..10).map(|i| (i, 1u32)).collect();
    let candidates = [0x10];
    let extract = |subject: &usize, _: usize| -> Option<u32> {
        if *subject < 9 { Some(1) } else { Some(2) }
    };
    let result = pick_offset_by_consensus(&candidates, &anchors, extract, 0.90);
    assert!(result.is_some(), "9/10 should clear 0.90");
}
