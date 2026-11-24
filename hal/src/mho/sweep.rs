// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's sweep (protocols/mho-acquisition.md section 12.1): the auto trigger lives in the TPU, 0x226c bit 0
//! AUTO, 0x2270 = t1 and 0x2274 = t2 in 4 ns ticks, with t1 = clamp(20 x tdiv, 200 ms, 1 s) and t2 = max(5 ms,
//! min(20 x tdiv, 1 s) / 2, holdoff); 0x4020 follows the sweep in bits 7 (AUTO, 0x6080) and 11 (NORMAL, 0x6800).
//! **Verified on the unit** under the stock system (an MHO98, issue #4: 1 us/div 200 ms / 5 ms, 1 ms/div 200 ms /
//! 10 ms, 20 ms/div 400 ms / 200 ms). OOS left the snapshot's AUTO on whatever the screen's sweep (an MHO98,
//! mho-dev-47: 0x226c 1 with NORMAL, and a single never waited for its edge).

pub const SWEEP: u32 = 0x226c;
pub const T1: u32 = 0x2270;
pub const T2: u32 = 0x2274;
pub const MODE_4020: u32 = 0x4020;
const AUTO_4020: u32 = 1 << 7;
const NORMAL_4020: u32 = 1 << 11;
const TICK: f64 = 4e-9;

/// The TPU's sweep words for AUTO (`auto`) or NORMAL at `tdiv` s/div with `holdoff` s.
pub fn words(auto: bool, tdiv: f64, holdoff: f64) -> [(u32, u32); 3] {
    let t1 = (20.0 * tdiv).clamp(0.2, 1.0);
    let t2 = (20.0 * tdiv).min(1.0) / 2.0;
    let t2 = t2.max(5e-3).max(holdoff);
    let ticks = |t: f64| (t / TICK).round() as u32;
    [(SWEEP, auto as u32), (T1, ticks(t1)), (T2, ticks(t2))]
}

/// 0x4020 with the sweep's bit (7 for AUTO, 11 for NORMAL) over `base`'s other bits.
pub fn word_4020(base: u32, auto: bool) -> u32 {
    (base & !(AUTO_4020 | NORMAL_4020)) | if auto { AUTO_4020 } else { NORMAL_4020 }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the timeouts' clamps or tick, NORMAL leaving AUTO on, the 0x4020 bits swapped.
    #[test]
    fn the_sweep_is_the_stock_systems() {
        let ms = |t: u32| (t as f64 * TICK * 1e3).round() as u32;
        let w = words(true, 1e-6, 0.0);
        assert_eq!((w[0].1, ms(w[1].1), ms(w[2].1)), (1, 200, 5));
        let w = words(true, 1e-3, 0.0);
        assert_eq!((ms(w[1].1), ms(w[2].1)), (200, 10));
        let w = words(true, 20e-3, 0.0);
        assert_eq!((ms(w[1].1), ms(w[2].1)), (400, 200));
        assert_eq!(words(false, 1e-3, 0.0)[0], (SWEEP, 0));
        assert_eq!((word_4020(0x6000, true), word_4020(0x6080, false)), (0x6080, 0x6800));
    }
}
