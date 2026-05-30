//! Pure protocol primitives: a raw captured frame and a bounded ring. The agent
//! crate's detours produce `RawFrame`s; this ring caps memory by BOTH frame count
//! and total bytes so capture can never become a firehose. Host-testable.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server (a send).
    C2S,
    /// Server → client (a recv).
    S2C,
}

/// One captured packet: raw bytes only, no interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub timestamp_ms: u64,
    pub direction: Direction,
    pub socket_id: u64,
    pub bytes: Vec<u8>,
}

/// A capacity-bounded FIFO of frames. Pushing past either the frame cap or the
/// byte cap evicts oldest-first until both fit.
#[derive(Debug)]
pub struct FrameRing {
    frames: VecDeque<RawFrame>,
    total_bytes: usize,
    max_frames: usize,
    max_bytes: usize,
}

impl FrameRing {
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        FrameRing { frames: VecDeque::new(), total_bytes: 0, max_frames, max_bytes }
    }

    pub fn push(&mut self, frame: RawFrame) {
        self.total_bytes += frame.bytes.len();
        self.frames.push_back(frame);
        while self.frames.len() > self.max_frames
            || (self.total_bytes > self.max_bytes && self.frames.len() > 1)
        {
            if let Some(dropped) = self.frames.pop_front() {
                self.total_bytes -= dropped.bytes.len();
            }
        }
    }

    pub fn len(&self) -> usize { self.frames.len() }
    pub fn is_empty(&self) -> bool { self.frames.is_empty() }
    pub fn total_bytes(&self) -> usize { self.total_bytes }

    /// Return a clone of the frame at `idx` (0 = oldest). Returns `None` if
    /// `idx >= self.len()`. Clone is cheap for typical small frames; avoids
    /// exposing the internal `VecDeque` directly.
    pub fn get(&self, idx: usize) -> Option<RawFrame> {
        self.frames.get(idx).cloned()
    }

    /// Remove and return all frames in FIFO order (for the TCP consumer).
    pub fn drain(&mut self) -> Vec<RawFrame> {
        self.total_bytes = 0;
        self.frames.drain(..).collect()
    }
}

// ── Iter trait impl — lazy walk over the bounded ring ───────────────────────

use crate::spine::Iter;

/// Lightweight iterator state for `Iter<RawFrame> for &'a FrameRing`.
/// Walks from oldest to newest without consuming the ring.
#[derive(Debug)]
pub struct FrameRingIter<'a> {
    ring:   &'a FrameRing,
    cursor: usize,
    limit:  usize,
}

impl<'a> Iterator for FrameRingIter<'a> {
    type Item = RawFrame;
    fn next(&mut self) -> Option<RawFrame> {
        if self.cursor >= self.limit { return None; }
        let frame = self.ring.get(self.cursor)?;
        self.cursor += 1;
        Some(frame)
    }
}

impl<'a> Iter<RawFrame> for &'a FrameRing {
    type Iter = FrameRingIter<'a>;
    fn iter(&self) -> Self::Iter {
        FrameRingIter { ring: self, cursor: 0, limit: self.len() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dir: Direction, n: usize) -> RawFrame {
        RawFrame { timestamp_ms: 0, direction: dir, socket_id: 1, bytes: vec![0u8; n] }
    }

    #[test]
    fn push_within_caps_keeps_all() {
        let mut r = FrameRing::new(4, 1024);
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::S2C, 10));
        assert_eq!(r.len(), 2);
        assert_eq!(r.total_bytes(), 20);
    }

    #[test]
    fn evicts_oldest_over_frame_cap() {
        let mut r = FrameRing::new(2, 1_000_000);
        for _ in 0..3 { r.push(frame(Direction::C2S, 10)); }
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn evicts_oldest_over_byte_cap() {
        let mut r = FrameRing::new(100, 25);
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::C2S, 10));
        r.push(frame(Direction::C2S, 10));
        assert!(r.total_bytes() <= 25);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn drain_empties_and_returns_in_order() {
        let mut r = FrameRing::new(4, 1024);
        r.push(frame(Direction::C2S, 1));
        r.push(frame(Direction::S2C, 2));
        let out = r.drain();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].direction, Direction::C2S);
        assert_eq!(r.len(), 0);
        assert_eq!(r.total_bytes(), 0);
    }
}
