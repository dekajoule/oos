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
}
