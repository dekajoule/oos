// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's two generators (protocols/mho-acquisition.md section 12.5): the FPGA words of each, its waveform
//! memory and the lines for the generator board's microcontroller (its relays). **Seen on an MHO98 under the stock
//! system** (issue #4): the output bit, the frequency word, square and noise, the amplitude and offset words, the relay
//! lines. **Verified on the unit** (MHO98, mho-dev-37): the output follows the waveform memory; with the memory empty
//! the output stays at the offset whatever the words say. OOS fills it with one period of square, noise or DC; the
//! other shapes go out square. Amplitude is set in the relays' gear of 1 Vpp (all relays off), from about 0.8 to 2 Vpp
//! in the stock system; OOS uses that gear for every amplitude.

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
/// The frequency word with noise on.
pub const NOISE_WORD: u64 = 0x20_0000_0000;

/// Samples in a generator's waveform memory: one period, signed 16 bits, full scale +-32767 (the amplitude word scales
/// it). Two samples per word, the earlier in bits 15:0; generator 1's memory at 0x100000, generator 2's at 0x200000.
/// The memory reads back 0, so what was written cannot be checked by reading.
pub const TABLE_LEN: usize = 16384;
pub const TABLE_FULL: i16 = 32767;

pub fn table_base(n: usize) -> u32 {
    if n == 2 {
        0x20_0000
    } else {
        0x10_0000
    }
}

/// One period of `shape`: square high for the first half and low for the second; noise a band-limited pseudo-random
/// sequence (a fixed seed, so every start loads the same); DC all zero (its level is the offset's).
pub fn table(shape: Shape) -> Vec<i16> {
    match shape {
        Shape::Square => (0..TABLE_LEN).map(|i| if i < TABLE_LEN / 2 { TABLE_FULL } else { -TABLE_FULL }).collect(),
        Shape::Dc => vec![0; TABLE_LEN],
        Shape::Noise => {
            // xorshift32, each value the mean of four draws (a softer spectrum than single draws), normalised to full
            // scale and held for four samples: the memory is played at one sample per clock for noise (`NOISE_WORD`)
            let mut x = 0x2545_f491u32;
            let mut draw = || {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as i32 as f64
            };
            let v: Vec<f64> = (0..TABLE_LEN / 4).map(|_| (0..4).map(|_| draw()).sum::<f64>()).collect();
            let peak = v.iter().fold(0.0f64, |m, a| m.max(a.abs()));
            v.iter().flat_map(|a| [(a / peak * TABLE_FULL as f64).round() as i16; 4]).collect()
        }
    }
}

/// The writes that load `samples` into generator `n`'s waveform memory; sent with the output off (`words` first).
pub fn table_words(n: usize, samples: &[i16]) -> Vec<(u32, u32)> {
    let base = table_base(n);
    samples
        .chunks(2)
        .enumerate()
        .map(|(k, p)| (base + 4 * k as u32, p[0] as u16 as u32 | (*p.get(1).unwrap_or(&0) as u16 as u32) << 16))
        .collect()
}

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
}

pub fn regs(n: usize) -> Regs {
    let (base, ampl, offset, out) =
        if n == 2 { (0x23_0000, 0x2_0020, 0x1_0108, 0x5_0004) } else { (0x13_0000, 0x2_001c, 0x1_0100, 0x4_0004) };
    Regs { out, freq_lo: base + 0x28, freq_hi: base + 0x2c, noise: base + 0x6c, square: base + 0x7c, ampl, offset }
}

/// What a generator puts out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Square,
    Noise,
    /// the offset alone (amplitude 0)
    Dc,
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
    // the offset word losing its low half, the output switched on before the relays, generator 2's block.
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
        let n = words(2, &Out { shape: Shape::Noise, ..o }, 0, 0);
        assert!(n.contains(&(0x23_0028, 0)) && n.contains(&(0x23_002c, 0x20)) && n.contains(&(0x23_006c, 1)));
        let r = relays(1, true);
        assert_eq!((r[0].as_str(), r[4].as_str()), (":AFG:RELAY:CH1:STATE 4,0\n", ":AFG:RELAY:CH1:STATE 0,1\n"));
        assert!(is_generator_board("RIGOL,AFG(01),x,00.01.06"));
    }

    // Break it catches: a table that is not one full period of 16384 samples, a square without its two halves at
    // full scale, the two samples of a word swapped, generator 2's memory, a noise table that is not full scale or
    // has an offset, a DC table that is not zero.
    #[test]
    fn the_tables_fill_the_waveform_memory() {
        let sq = table(Shape::Square);
        assert_eq!(sq.len(), TABLE_LEN);
        assert_eq!(
            (sq[0], sq[TABLE_LEN / 2 - 1], sq[TABLE_LEN / 2], sq[TABLE_LEN - 1]),
            (32767, 32767, -32767, -32767)
        );
        let w = table_words(1, &sq);
        assert_eq!(w.len(), TABLE_LEN / 2);
        assert_eq!((w[0], w[TABLE_LEN / 2 - 1]), ((0x10_0000, 0x7fff_7fff), (0x10_7ffc, 0x8001_8001)));
        assert_eq!(table_words(2, &[1, -2])[0], (0x20_0000, 0xfffe_0001));
        let n = table(Shape::Noise);
        assert_eq!(n.len(), TABLE_LEN);
        assert_eq!(n.iter().map(|v| v.unsigned_abs()).max(), Some(32767));
        let mean = n.iter().map(|&v| v as f64).sum::<f64>() / TABLE_LEN as f64;
        assert!(mean.abs() < 1000.0, "noise mean {}", mean);
        assert_eq!(n, table(Shape::Noise), "the same table on every start");
        assert!(table(Shape::Dc).iter().all(|&v| v == 0));
    }
}
