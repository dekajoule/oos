// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's frequency counter: the DHO800/DHO900's (protocols/counter.md) at the same addresses, with other clocks
//! (protocols/mho-acquisition.md section 12.4). Its gate counts 4 ns (1 s: 0x0ee6b280, 0.25 s: 0x03b9aca0, **verified
//! on the unit** under the stock system). The tick of its time stamps is one ADC sample, 0.25 ns at 4 GSa/s, as on the
//! DHO800/DHO900 (0.8 ns at 1.25 GSa/s): **verified on the unit** under OOS (an MHO98, mho-dev-35, issue #4: the single
//! period of the 1 kHz probe compensation signal read 4000002 ticks, the counter 1.000000 kHz).

pub use crate::dho::counter::{gate_s, read, stop, Config, Mode, Raw};

/// The gate register's unit and the time stamps' tick.
pub const GATE_TICK: f64 = 4e-9;
pub const TICK: f64 = 0.25e-9;

/// The writes that start the counter on `c` from zero.
pub fn start(c: &Config) -> Vec<(u32, u32)> {
    crate::dho::counter::start_ticked(c, GATE_TICK)
}

/// The signal's period from a reading with gate `gate_s` (`dho::counter::period` with the MHO900's tick).
pub fn period(r: &Raw, gate_s: f64) -> Option<f64> {
    crate::dho::counter::period_ticked(r, gate_s, TICK)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the DHO's gate unit on the MHO900, the DHO's tick in the period, a slow signal's single period
    // dropped.
    #[test]
    fn the_gate_counts_4_ns() {
        let w = start(&Config { ch: 0, mode: Mode::Frequency, digits: 6 });
        assert!(w.contains(&(crate::dho::counter::GATE, 0x0ee6_b280)));
        let q = start(&Config { ch: 0, mode: Mode::Frequency, digits: 3 });
        assert!(q.contains(&(crate::dho::counter::GATE, 0x03b9_aca0)));
        // 1 kHz in a 1 s gate, the first edge 0.1 ms after the gate opens, the last 0.1 ms before it closes
        let edge = (0.1e-3 / TICK) as u64;
        let r = Raw { start: edge, end: edge, edges: 1000, freq_valid: true, ..Default::default() };
        assert!((period(&r, 1.0).unwrap() - 0.9998e-3 / 0.999).abs() < 1e-12);
        let slow = Raw { period: 8_000_000, period_valid: true, ..Default::default() };
        assert_eq!(period(&slow, 1.0), Some(2e-3));
    }
}
