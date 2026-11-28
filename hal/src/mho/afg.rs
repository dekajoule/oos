// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's two generators (protocols/mho-acquisition.md section 12.5): the FPGA words of each, its waveform
//! memory, and the lines for the generator board's microcontroller (its relays). **Seen on an MHO98 under the stock
//! system** (issue #4): the output bit, the frequency word, square and noise, the amplitude and offset words, the relay
//! lines. **Verified on the unit** under OOS (the same MHO98, mho-dev-35 and -37): the offset, and that the output
//! needs the waveform memory filled, the square too: a square written there gave 1.08 Vpp at 1 kHz with the amplitude
//! word of 1 Vpp; square, noise and DC loaded by oos-ui read 1.034 Vpp at 1.000000 kHz, 0.99 Vpp of noise and the
//! offset's 0.304 V for DC (hp197). The memory reads back 0. From mho-dev-38 on (a start of the FPGA by OOS, not the
//! stock system's state): the output's scale words (`Regs::scale`) start at 0 and must be set, and noise plays the
//! memory in random order. Amplitude is set in the relays' gear of 1 Vpp (all relays off), from about
//! 0.8 to 2 Vpp in the stock system; OOS uses that gear for every amplitude.

/// The frequency word's clock: f / 250 MHz x (2^51 - 1) (1 kHz: 0x2_18def416, read under the stock system).
pub const CLOCK_HZ: f64 = 250e6;
/// The amplitude word at 1 Vpp in the 1 Vpp gear (0.4806 of full scale; 0.8 Vpp: 0.3845), and the highest amplitude
/// OOS sets in it.
pub const FULL_VPP: f64 = 1.0 / 0.4806;
pub const MAX_VPP: f64 = 2.0;
/// The offset's PWM code at 0 V and per volt (0 V 0x0481, +0.5 V 0x0550, -0.5 V 0x03b3), and the offset OOS allows.
pub const OFFSET_ZERO: f64 = 0x0481 as f64;
pub const OFFSET_PER_V: f64 = (0x0550 - 0x03b3) as f64;
pub const MAX_OFFSET: f64 = 1.0;
/// The output's full scale in the first scale word (`Regs::scale`), without modulation.
pub const SCALE_FULL: u32 = 0x7fff;
/// The frequency word with noise on.
pub const NOISE_WORD: u64 = 0x20_0000_0000;

/// The words of generator `n` (1 or 2).
pub struct Regs {
    /// bit 0: the output
    pub out: u32,
    pub freq_lo: u32,
    pub freq_hi: u32,
    pub noise: u32,
    pub square: u32,
    /// 16 bits of full scale
    pub ampl: u32,
    /// bits 31:16: the offset's PWM code (bits 15:0 kept)
    pub offset: u32,
    /// the output's scale is the signed 16-bit sum of these two words (AM splits it between them): 0x7fff and 0 without
    /// modulation, as the stock system leaves them; both 0 after a start of the FPGA, and the output stays flat
    pub scale: [u32; 2],
}

pub fn regs(n: usize) -> Regs {
    let (base, ampl, offset, out) =
        if n == 2 { (0x23_0000, 0x2_0020, 0x1_0108, 0x5_0004) } else { (0x13_0000, 0x2_001c, 0x1_0100, 0x4_0004) };
    Regs {
        out,
        freq_lo: base + 0x28,
        freq_hi: base + 0x2c,
        noise: base + 0x6c,
        square: base + 0x7c,
        ampl,
        offset,
        scale: [base + 0x48, base + 0x4c],
    }
}

/// What a generator puts out: a wave from the memory (`table`), noise, or the offset alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Sine,
    Square,
    /// the rising part's share of the period, 0 to 1
    Ramp(f64),
    /// the high part's share of the period, 0 to 1
    Pulse(f64),
    /// the stored arbitrary wave (`table` takes its samples)
    Arb,
    Noise,
    /// the offset alone (amplitude 0)
    Dc,
}

/// A generator's waveform memory: 16384 signed 16-bit samples, two per word (8192 words), one period at full scale
/// (the amplitude word scales it), loaded with the output off; generator 1's from 0x100000, generator 2's from
/// 0x200000. **Verified on the unit** with a square (the first half of the words 0x7fff7fff, the second 0x80018001),
/// noise and DC, and that bits 15:0 hold the earlier sample (a step from high to low after an odd sample, mho-dev-38).
pub const TABLE_SAMPLES: usize = 16384;

pub fn table_base(n: usize) -> u32 {
    if n == 2 {
        0x20_0000
    } else {
        0x10_0000
    }
}

/// One period of `shape` as the memory's samples (full scale +-32767); `arb` the stored wave's samples (-1 to 1) for
/// `Shape::Arb`. DC is all zero (its level is the offset's). Noise plays the memory in random order, so its table
/// holds the values noise takes: each the sum of four pseudo-random draws (near a normal distribution), a fixed seed so
/// every start loads the same, scaled to full scale.
pub fn table(shape: Shape, arb: &[f32]) -> Vec<i16> {
    match shape {
        Shape::Noise => return noise(),
        Shape::Dc => return vec![0; TABLE_SAMPLES],
        _ => {}
    }
    let n = TABLE_SAMPLES;
    let full = 32767.0;
    (0..n)
        .map(|i| {
            let x = i as f64 / n as f64;
            let v = match shape {
                Shape::Sine => (2.0 * std::f64::consts::PI * x).sin(),
                Shape::Ramp(sym) => {
                    let s = sym.clamp(0.0, 1.0);
                    if x < s {
                        -1.0 + 2.0 * x / s
                    } else {
                        1.0 - 2.0 * (x - s) / (1.0 - s).max(1e-9)
                    }
                }
                Shape::Pulse(duty) => {
                    if x < duty.clamp(0.0, 1.0) {
                        1.0
                    } else {
                        -1.0
                    }
                }
                Shape::Arb if !arb.is_empty() => arb[(x * arb.len() as f64) as usize % arb.len()] as f64,
                _ => {
                    if x < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            (v.clamp(-1.0, 1.0) * full).round() as i16
        })
        .collect()
}

/// Noise: xorshift32, each value the sum of four draws, scaled to full scale.
fn noise() -> Vec<i16> {
    let mut x = 0x2545_f491u32;
    let mut draw = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as i32 as f64
    };
    let v: Vec<f64> = (0..TABLE_SAMPLES).map(|_| (0..4).map(|_| draw()).sum()).collect();
    let peak = v.iter().fold(0.0f64, |m, a| m.max(a.abs()));
    v.iter().map(|a| (a / peak * 32767.0).round() as i16).collect()
}

/// The writes that load `samples` (`TABLE_SAMPLES` of them) into generator `n`'s memory: two per word, the earlier in
/// bits 15:0.
pub fn table_words(n: usize, samples: &[i16]) -> Vec<(u32, u32)> {
    let base = table_base(n);
    samples
        .chunks(2)
        .enumerate()
        .map(|(k, p)| (base + 4 * k as u32, (p.get(1).copied().unwrap_or(0) as u16 as u32) << 16 | p[0] as u16 as u32))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Out {
    pub on: bool,
    pub hz: f64,
    pub vpp: f64,
    pub offset: f64,
    pub shape: Shape,
}

pub fn freq_word(hz: f64) -> u64 {
    (hz.max(0.0) / CLOCK_HZ * ((1u64 << 51) - 1) as f64) as u64
}

pub fn ampl_word(vpp: f64) -> u32 {
    (vpp.clamp(0.0, MAX_VPP) / FULL_VPP * 65535.0).round() as u32
}

/// The offset word: the PWM code in bits 31:16 over the kept bits 15:0 of `old`.
pub fn offset_word(v: f64, old: u32) -> u32 {
    let code = (OFFSET_ZERO + v.clamp(-MAX_OFFSET, MAX_OFFSET) * OFFSET_PER_V).round().clamp(0.0, 65535.0) as u32;
    code << 16 | (old & 0xffff)
}

/// The words for `o` on generator `n` with the output off (`out_old`: the output word as read, `offset_old` the offset
/// word as read); the output bit goes on last, after the relays (`relays`).
pub fn words(n: usize, o: &Out, out_old: u32, offset_old: u32) -> Vec<(u32, u32)> {
    let r = regs(n);
    let f = if o.shape == Shape::Noise { NOISE_WORD } else { freq_word(o.hz) };
    vec![
        (r.out, out_old & !1),
        (r.noise, (o.shape == Shape::Noise) as u32),
        (r.square, (o.shape == Shape::Square) as u32),
        (r.freq_lo, f as u32),
        (r.freq_hi, (f >> 32) as u32),
        (r.ampl, if o.shape == Shape::Dc { 0 } else { ampl_word(o.vpp) }),
        (r.offset, offset_word(o.offset, offset_old)),
        (r.scale[0], SCALE_FULL),
        (r.scale[1], 0),
    ]
}

/// The lines to the generator board's microcontroller for generator `n`: the output relay, and before it is switched on
/// the gear relays 4 to 1 off (the 1 Vpp gear).
pub fn relays(n: usize, on: bool) -> Vec<String> {
    let line = |r: u32, v: u32| format!(":AFG:RELAY:CH{}:STATE {},{}\n", n, r, v);
    if on {
        let mut v: Vec<String> = (1..=4).rev().map(|r| line(r, 0)).collect();
        v.push(line(0, 1));
        v
    } else {
        vec![line(0, 0)]
    }
}

/// The microcontroller's identity query, and what its answer holds (`RIGOL,AFG(01),...` on an MHO98).
pub const IDN: &str = "*IDN?\n";
pub fn is_generator_board(answer: &str) -> bool {
    answer.contains("AFG")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the frequency word's clock or width, the amplitude's full scale, the offset's zero or slope,
    // the offset word losing its low half, the output switched on before the relays, generator 2's block, the output's
    // scale left at 0 (a flat output after a start of the FPGA).
    #[test]
    fn the_words_are_the_stock_systems() {
        assert_eq!(freq_word(1e3), 0x2_18de_f416);
        assert_eq!(freq_word(1e6), 0x831_26e9_78d4);
        assert_eq!(ampl_word(1.0), (0.4806 * 65535.0f64).round() as u32);
        assert_eq!(offset_word(0.5, 0x1234) >> 16, 0x0550);
        assert_eq!(offset_word(-0.5, 0x1234), 0x03b3_1234);
        let o = Out { on: true, hz: 1e3, vpp: 1.0, offset: 0.0, shape: Shape::Square };
        let w = words(1, &o, 0x81, 0);
        assert_eq!(w[0], (0x4_0004, 0x80), "output off first");
        assert!(w.contains(&(0x13_007c, 1)) && w.contains(&(0x13_006c, 0)) && w.contains(&(0x1_0100, 0x0481_0000)));
        assert!(w.contains(&(0x13_0048, 0x7fff)) && w.contains(&(0x13_004c, 0)), "the output's scale");
        let n = words(2, &Out { shape: Shape::Noise, ..o }, 0, 0);
        assert!(n.contains(&(0x23_0028, 0)) && n.contains(&(0x23_002c, 0x20)) && n.contains(&(0x23_006c, 1)));
        let r = relays(1, true);
        assert_eq!((r[0].as_str(), r[4].as_str()), (":AFG:RELAY:CH1:STATE 4,0\n", ":AFG:RELAY:CH1:STATE 0,1\n"));
        assert!(is_generator_board("RIGOL,AFG(01),x,00.01.06"));
    }

    // Break it catches: the square of the test on the unit not reproduced, a table of the wrong length, a sine that
    // does not start at 0 or misses its peak, a ramp's symmetry ignored, noise as two levels (the square's table gave
    // a random square on the unit) or with an offset, a noise table that changes between starts.
    #[test]
    fn the_memory_holds_one_period() {
        let sq = table_words(1, &table(Shape::Square, &[]));
        assert_eq!(sq.len(), 8192);
        assert_eq!(
            (sq[0], sq[4095].1, sq[4096].1, sq[8191]),
            ((0x10_0000, 0x7fff_7fff), 0x7fff_7fff, 0x8001_8001, (0x10_0000 + 4 * 8191, 0x8001_8001))
        );
        let sine = table(Shape::Sine, &[]);
        assert_eq!((sine[0], sine[4096], sine[12288]), (0, 32767, -32767));
        let ramp = table(Shape::Ramp(1.0), &[]);
        assert!(ramp[0] == -32767 && ramp[8192] == 0 && ramp[16383] > 32700);
        let arb = table(Shape::Arb, &[0.0, 1.0]);
        assert_eq!((arb[0], arb[8192]), (0, 32767));
        let noise = table(Shape::Noise, &[]);
        let mean = noise.iter().map(|&v| v as f64).sum::<f64>() / noise.len() as f64;
        let rms = (noise.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / noise.len() as f64).sqrt();
        assert_eq!((noise.len(), noise.iter().map(|v| v.unsigned_abs()).max()), (TABLE_SAMPLES, Some(32767)));
        assert!(mean.abs() < 1000.0 && rms > 6000.0 && rms < 16000.0, "noise mean {} rms {}", mean, rms);
        assert_eq!(noise, table(Shape::Noise, &[]), "the same table on every start");
        assert!(table(Shape::Dc, &[]).iter().all(|&v| v == 0));
        assert_eq!(table_words(2, &noise)[0].0, 0x20_0000);
    }
}
