// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's logic analyser: the raw read-out of the DHO800/DHO900 (protocols/la.md 4.2), **assumed** the same on
//! the MHO900, and its thresholds (protocols/mho-acquisition.md section 12.4). One word 0x12d4 holds both pods' DAC
//! codes, pod 1 (D0-D7) in bits 15:0, pod 2 (D8-D15) in bits 31:16, at zero + V x 166 (pod 1) and 168 (pod 2) codes
//! per volt. **Verified on the unit** under the stock system (an MHO98, issue #4): 0x122a123e at 0 V, 0x13151326 at
//! 1.4 V, 0x1572157c at 5 V. The zero codes are that unit's; another unit's calibration may move them by a few codes.

pub use crate::dho::la::{passthrough, read_bytes, readout, restore};
pub use crate::dho::scu::Shadows;

pub const THRESHOLD: u32 = 0x12d4;

/// The LA samples at a quarter of the analog rate (the board's, before any thinning), and the rate block 0x12a4,
/// 0x12a8, 0x12ac holds it: below the fast range 0x12a4 = 0 and 0x12ac = 250 MHz / LA rate, in it 0x12a4 = 0x8000 |
/// 1 GHz / LA rate and 0x12ac = 1. **Verified on the unit** under the stock system (an MHO98, issue #4, 10 k points):
/// 5 ms/div (200 kSa/s) 0/0/5000, 1 ms/div 0/0/1000, 200 us/div 0/0/200, 20 us/div (50 MSa/s) 0/0/20, 1 us/div
/// (1 GSa/s) 0x8004/0/1. Where the fast range begins (above 125 MSa/s of LA rate, as the DHO800/DHO900's begins at
/// half its slow clock) is **assumed**. Depth, segment and the points around the trigger do not change with the LA on.
pub const LA_PER_ANALOG: u32 = 4;

pub fn rate_block(analog_rate: f64) -> [(u32, u32); 3] {
    let la = (analog_rate / LA_PER_ANALOG as f64).max(1.0);
    let (a4, ac) =
        if la > 125e6 { (0x8000 | ((1e9 / la).round() as u32 & 0xff), 1) } else { (0, (250e6 / la).round() as u32) };
    [(0x12a4, a4), (0x12a8, 0), (0x12ac, ac)]
}
/// The zero codes and codes per volt of pod 1 and pod 2.
pub const ZERO: [f64; 2] = [0x123e as f64, 0x122a as f64];
pub const CODES_PER_V: [f64; 2] = [166.0, 168.0];
/// The thresholds' range (la.md 5.1).
pub const MAX_V: f64 = 15.0;

/// The threshold word for pod 1 and pod 2 at `v` volts.
pub fn threshold_word(v: [f64; 2]) -> u32 {
    let code = |p: usize| (ZERO[p] + v[p].clamp(-MAX_V, MAX_V) * CODES_PER_V[p]).max(0.0) as u32 & 0xffff;
    code(1) << 16 | code(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the pods' halves swapped, the DHO's 310 codes per volt, a rounding up.
    #[test]
    fn the_thresholds_are_the_stock_systems() {
        assert_eq!(threshold_word([0.0, 0.0]), 0x122a_123e);
        assert_eq!(threshold_word([1.4, 1.4]), 0x1315_1326);
        assert_eq!(threshold_word([5.0, 5.0]), 0x1572_157c);
    }

    // Break it catches: the DHO's 125 MHz in place of 250, the LA at the analog rate, the fast range's form.
    #[test]
    fn the_rate_block_is_the_stock_systems() {
        let ac = |r: f64| rate_block(r)[2].1;
        assert_eq!((ac(200e3), ac(1e6), ac(5e6), ac(50e6)), (5000, 1000, 200, 20));
        assert_eq!(rate_block(200e3)[0], (0x12a4, 0));
        assert_eq!(rate_block(1e9), [(0x12a4, 0x8004), (0x12a8, 0), (0x12ac, 1)]);
    }
}
