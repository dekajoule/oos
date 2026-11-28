// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Roll on the MHO900, in software: from `ROLL_TDIV` with Auto Roll on, the board records one division at a time
//! without waiting for a trigger (the TPU's auto trigger after `AUTO_TICKS`), and each record is appended to a ring
//! of ten divisions whose newest point is at the right edge of the screen. Between two records the board is armed and
//! read again, a few milliseconds without points. The stock system rolls with a read-out in chunks that OOS does not
//! know yet (protocols/mho-acquisition.md section 12.3). **Not yet verified on hardware.**

use crate::board::Frame;
use crate::model::NCH;
use std::collections::VecDeque;

/// The TPU's sweep word (bit 0: AUTO) and its two timeouts in 4 ns ticks (section 12.1), and the timeout roll sets:
/// 1 ms, so a record without an edge ends at once.
pub(super) const SWEEP: u32 = 0x226c;
pub(super) const T1: u32 = 0x2270;
pub(super) const T2: u32 = 0x2274;
pub(super) const AUTO_TICKS: u32 = 250_000;
/// Points of the ring at most; deeper rings keep the minimum and maximum of each block of points.
const RING_MAX: usize = 20_000;

/// The ring: each channel's points (pairs of minimum and maximum when `q` > 1) of the last ten divisions.
pub(super) struct Ring {
    ch: [VecDeque<f32>; NCH],
    keep: usize,
    /// points of a record per block (1: every point kept)
    q: usize,
    dt: f64,
    tdiv: f64,
}

impl Ring {
    /// A ring for ten divisions of `tdiv` at a record's point spacing `dt`.
    pub(super) fn new(tdiv: f64, dt: f64) -> Ring {
        let points = (10.0 * tdiv / dt).ceil().max(1.0) as usize;
        let q = points.div_ceil(RING_MAX).max(1);
        let (keep, step) = if q > 1 { (2 * points.div_ceil(q), dt * q as f64 / 2.0) } else { (points, dt) };
        Ring { ch: std::array::from_fn(|_| VecDeque::new()), keep, q, dt: step, tdiv }
    }

    /// Appends a record's points of every channel in it.
    pub(super) fn push(&mut self, f: &Frame) {
        for (ring, v) in self.ch.iter_mut().zip(&f.ch) {
            if v.is_empty() {
                ring.clear();
                continue;
            }
            if self.q == 1 {
                ring.extend(v.iter().copied());
            } else {
                for b in v.chunks(self.q) {
                    let (lo, hi) = b.iter().fold((f32::MAX, f32::MIN), |(l, h), &x| (l.min(x), h.max(x)));
                    ring.extend([lo, hi]);
                }
            }
            while ring.len() > self.keep {
                ring.pop_front();
            }
        }
    }

    /// The ring as a frame: the newest point at the right edge (half a screen right of `pre`).
    pub(super) fn frame(&self) -> Frame {
        let len = self.ch.iter().map(VecDeque::len).max().unwrap_or(0);
        Frame {
            dt: self.dt,
            pre: (len as f64 - 1.0 - 5.0 * self.tdiv / self.dt).max(0.0) as usize,
            ch: self.ch.clone().map(|r| r.into_iter().collect()),
            roll: true,
            envelope: self.q > 1,
            partial: len < self.keep,
            ..Frame::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the newest point not at the right edge, a ring that grows past ten divisions, a deep ring
    // kept whole instead of as minimum and maximum.
    #[test]
    fn the_ring_keeps_ten_divisions_newest_at_the_right() {
        let mut r = Ring::new(0.2, 1e-4);
        let rec = Frame {
            dt: 1e-4,
            ch: std::array::from_fn(|c| if c == 0 { vec![1.0; 2000] } else { vec![] }),
            ..Frame::default()
        };
        for _ in 0..12 {
            r.push(&rec);
        }
        let f = r.frame();
        assert_eq!((f.ch[0].len(), f.pre, f.partial, f.envelope), (20_000, 9_999, false, false));
        let mut deep = Ring::new(10.0, 1e-4);
        deep.push(&Frame {
            dt: 1e-4,
            ch: std::array::from_fn(|c| if c == 0 { vec![0.5; 100_000] } else { vec![] }),
            ..Frame::default()
        });
        let f = deep.frame();
        assert!(f.envelope && f.ch[0].len() == 2 * 100_000 / 50 && f.partial);
    }
}
