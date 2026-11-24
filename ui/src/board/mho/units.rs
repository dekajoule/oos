// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's units beside the record, once per pass of the acquisition loop: the frequency counter
//! (`oos_hal::mho::counter`) and generator 1 (`oos_hal::mho::afg`: the FPGA's words, its waveform memory and the relays
//! of the generator board's microcontroller on `/dev/ttyS4`). **Not yet verified on hardware** under OOS; the words are the ones an
//! MHO98 showed under the stock system (protocols/mho-acquisition.md section 12).

use crate::board::{lock, CounterRead, CounterWant, HwShared};
use oos_hal::mho::afg::{self, Out, Shape};
use oos_hal::mho::bus::{Bar1, Bus};
use oos_hal::mho::counter;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The generator board's microcontroller, and the speeds tried in turn until it answers its identity query.
const MCU: &str = "/dev/ttyS4";
const SPEEDS: [u32; 8] = [115_200, 9_600, 19_200, 38_400, 57_600, 230_400, 460_800, 921_600];
/// The generator's blocks lie beyond the acquisition's window (up to 0x23ffff).
const AFG_WINDOW: usize = 0x24_0000;
/// The DAC value the stock system sends to the microcontroller with every frequency (purpose not known).
const DAC_LINE: &str = ":AFG:DAC:VAL 0x02,0x2082\n";

#[derive(Default)]
pub(super) struct Units {
    counting: Option<(CounterWant, Instant)>,
    counter_n: u64,
    gen: Option<super::Gen>,
    big: Option<Bar1>,
    mcu: Option<std::fs::File>,
    mcu_tried: bool,
    /// the wave in generator 1's memory: shape, duty, symmetry, arbitrary wave's id
    table: Option<(usize, f64, f64, u32)>,
}

impl Units {
    pub(super) fn tick(&mut self, bar: &mut Bar1, out: &HwShared) {
        self.counter(bar, out);
        let g = lock(out).afg[0];
        if g.is_some() && g != self.gen {
            if let Some(g) = &g {
                self.generator(g);
            }
            self.gen = g;
        }
    }

    /// Started when the UI wants another set-up, read once a gate (and 0.1 s) has passed, as on the DHO800/DHO900.
    fn counter(&mut self, bar: &mut Bar1, out: &HwShared) {
        let want = lock(out).counter_want;
        if want != self.counting.map(|c| c.0) {
            match want {
                Some(w) => {
                    let mode =
                        [counter::Mode::Frequency, counter::Mode::Period, counter::Mode::Totalize][w.mode.min(2)];
                    bar.apply(&counter::start(&counter::Config { ch: w.ch, mode, digits: w.digits }));
                    eprintln!("oos-ui mho: counter on CH{}, gate {} s", w.ch + 1, counter::gate_s(w.digits));
                    self.counting = Some((w, Instant::now()));
                }
                None => {
                    bar.apply(&counter::stop());
                    self.counting = None;
                }
            }
            lock(out).counter = None;
            return;
        }
        if let Some((w, t)) = self.counting {
            let gate = counter::gate_s(w.digits);
            if t.elapsed().as_secs_f64() >= gate + 0.1 {
                let r = counter::read(bar);
                self.counter_n += 1;
                let period = counter::period(&r, gate);
                if self.counter_n % 10 == 1 {
                    eprintln!(
                        "oos-ui mho: counter reading {}: {} edges, stamps {} / {}, single period {}, valid {}/{}: {}",
                        self.counter_n,
                        r.edges,
                        r.start,
                        r.end,
                        r.period,
                        r.freq_valid,
                        r.period_valid,
                        period.map_or("no result".into(), |p| format!("{:.6e} Hz", 1.0 / p))
                    );
                }
                lock(out).counter = Some(CounterRead { period, total: r.total, n: self.counter_n });
                self.counting = Some((w, Instant::now()));
            }
        }
    }

    /// Sets generator 1: the FPGA's words with the output off, its waveform memory when the wave changed, then the
    /// relays, then the output bit.
    fn generator(&mut self, g: &super::Gen) {
        let &(on, hz, vpp, offset, wave, duty, symmetry, arb) = g;
        let shape = match wave {
            0 => Shape::Sine,
            1 => Shape::Square,
            2 => Shape::Ramp(symmetry / 100.0),
            3 => Shape::Pulse(duty / 100.0),
            4 => Shape::Noise,
            5 => Shape::Dc,
            _ => Shape::Arb,
        };
        let wave_key = (wave, duty, symmetry, arb);
        let o = Out { on, hz, vpp, offset, shape };
        if self.big.is_none() {
            match Bar1::open_len(AFG_WINDOW) {
                Ok(b) => self.big = Some(b),
                Err(e) => return eprintln!("oos-ui mho: generator: {}", e),
            }
        }
        if on && !self.mcu_tried {
            self.mcu_tried = true;
            self.mcu = find_mcu();
        }
        let Some(big) = self.big.as_mut() else { return };
        let r = afg::regs(1);
        let (out_old, offset_old) = (big.rd(r.out), big.rd(r.offset));
        big.apply(&afg::words(1, &o, out_old, offset_old));
        if self.table != Some(wave_key) {
            let samples = afg::table(shape, &crate::board::lock(&super::ARB));
            big.apply(&afg::table_words(1, &samples));
            self.table = Some(wave_key);
        }
        let lines = afg::relays(1, on);
        match self.mcu.as_mut() {
            Some(m) => {
                for l in lines.iter().map(String::as_str).chain(on.then_some(DAC_LINE)) {
                    let _ = m.write_all(l.as_bytes());
                    std::thread::sleep(Duration::from_millis(3));
                }
            }
            None if on => eprintln!("oos-ui mho: generator: no board answered on {}; its output relay stays open", MCU),
            None => {}
        }
        if on {
            big.wr(r.out, out_old | 1);
        }
        // every word read back, so a report shows which of them the board took
        let back: Vec<String> = [r.out, r.offset, r.ampl, r.freq_lo, r.freq_hi, r.square, r.noise]
            .iter()
            .map(|&a| format!("{:#x}={:#x}", a, big.rd(a)))
            .collect();
        eprintln!(
            "oos-ui mho: generator 1 {} {:?} {} Hz {} Vpp offset {} V (read back: {})",
            if on { "on" } else { "off" },
            shape,
            hz,
            vpp.min(afg::MAX_VPP),
            offset,
            back.join(" ")
        );
    }
}

/// Opens the microcontroller's line at the speed it answers its identity query at (`stty`, then a read that ends 0.3 s
/// after the last byte). Logs the answer's maker, kind and version only.
fn find_mcu() -> Option<std::fs::File> {
    for s in SPEEDS {
        let ok = std::process::Command::new("stty")
            .args(["-F", MCU, &s.to_string(), "raw", "-echo", "min", "0", "time", "3"])
            .status()
            .is_ok_and(|x| x.success());
        if !ok {
            eprintln!("oos-ui mho: generator: {} cannot be set to {} Bd", MCU, s);
            return None;
        }
        let Ok(mut f) = std::fs::OpenOptions::new().read(true).write(true).open(MCU) else {
            eprintln!("oos-ui mho: generator: {} cannot be opened", MCU);
            return None;
        };
        let mut junk = [0u8; 256];
        while f.read(&mut junk).is_ok_and(|n| n > 0) {}
        if f.write_all(afg::IDN.as_bytes()).is_err() {
            continue;
        }
        let mut answer = Vec::new();
        let mut buf = [0u8; 128];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 || answer.len() > 512 {
                break;
            }
            answer.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8_lossy(&answer);
        if afg::is_generator_board(&text) {
            let fields: Vec<&str> = text.trim().split(',').collect();
            let shown: Vec<&str> = fields.iter().take(2).chain(fields.last()).copied().collect();
            eprintln!("oos-ui mho: generator board at {} Bd: {}", s, shown.join(","));
            return Some(f);
        }
    }
    eprintln!("oos-ui mho: generator: no answer on {} at {:?} Bd", MCU, SPEEDS);
    None
}
