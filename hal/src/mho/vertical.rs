// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! A channel's vertical settings beside its scale: the offset, AC coupling and the bandwidth limit, from the unit's
//! recording of them (`tools/mho-settings.py --extended`, docs/protocols/mho-acquisition.md section 11). The MHO900's
//! front end is the DHO800/DHO900's (afe-settings.md): the offset is the DAC word 0x28, `zero - offset / lsb`, with one
//! `lsb` per input-attenuator path (IG); AC clears bit 15 of 0x07; the bandwidth limit is in bits 10:0 of 0x2e and
//! some bits of the FPGA's 0x1004. Pure: the recording in, front-end frames and FPGA bits out. **Seen on an MHO98's
//! recording** (issue #4); not yet verified on hardware.

use super::settings::{Plan, Setting, Settings};

/// What the screen asks of a channel beside its scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertical {
    /// at the BNC, in V
    pub offset: f64,
    pub ac: bool,
    /// the bandwidth limit in MHz, 0 = none
    pub bw_mhz: u32,
}

/// The DAC word's register, the coupling register and its DC bit, the bandwidth bits.
const DAC: u8 = 0x28;
const INPUT: u8 = 0x07;
const DC: u16 = 0x8000;
const BW_REG: u8 = 0x2e;
const BW_BITS: u16 = 0x07ff;
/// The FPGA word with per-channel bandwidth bits (0x00f00000 for CH1 at 20 MHz, 0x00c00000 for CH2, an MHO98).
const BW_FPGA: u32 = 0x1004;

/// The last word channel `ch`'s front end got at register `r` in `frames`.
pub fn last(frames: &[(usize, u8, u16)], ch: usize, r: u8) -> Option<u16> {
    frames.iter().rev().find(|f| f.0 == ch && f.1 == r).map(|f| f.2)
}

/// The input-attenuator path (IG 0 to 3) the words of a scale set (afe-settings.md `hz_input_gain`): IG 0 sets
/// 0x23 bits 13:11 to 5; IG 1 to 3 set them to 2 and 0x20 bits 2:0 to 0, 4 or 6.
pub fn input_gain(frames: &[(usize, u8, u16)], ch: usize) -> Option<usize> {
    let r23 = last(frames, ch, 0x23)?;
    if (r23 >> 11) & 7 == 5 {
        return Some(0);
    }
    match last(frames, ch, 0x20)? & 7 {
        0 => Some(1),
        4 => Some(2),
        6 => Some(3),
        _ => None,
    }
}

/// The recorded scale of channel `ch` nearest to `scale` V/div.
fn scale_setting(s: &Settings, ch: usize, scale: f64) -> Option<&Setting> {
    s.steps(ch, "scale")
        .into_iter()
        .min_by(|a, b| (a.0 / scale).ln().abs().total_cmp(&(b.0 / scale).ln().abs()))
        .map(|x| x.1)
}

/// V per DAC code of channel `ch` for each input path, from the recorded offsets (`CHn_offset=V_at=S`: the DAC word
/// at each offset against the one at 0 V at the same scale). IG 3 has no recorded scale in a `--extended` run; it is
/// taken as ten times IG 2's, the DHO800/DHO900's ratio (afe-settings.md 3.2). **Assumed** there.
pub fn lsb(s: &Settings, ch: usize) -> [Option<f64>; 4] {
    let mut sums = [(0.0, 0u32); 4];
    let prefix = format!("CH{}_offset=", ch);
    for st in &s.list {
        let Some((v, at)) = st.name.strip_prefix(&prefix).and_then(|r| r.split_once("_at=")) else { continue };
        let (Ok(v), Ok(at)) = (v.parse::<f64>(), at.parse::<f64>()) else { continue };
        if v == 0.0 {
            continue;
        }
        let zero = s.list.iter().find(|z| z.name == format!("CH{}_offset=0_at={}", ch, fmt(at)));
        let zero = zero.and_then(|z| last(&z.afe, ch, DAC));
        let ig = scale_setting(s, ch, at).and_then(|sc| input_gain(&sc.afe, ch));
        if let (Some(z), Some(code), Some(ig)) = (zero, last(&st.afe, ch, DAC), ig) {
            if code != z {
                sums[ig].0 += -v / (code as f64 - z as f64);
                sums[ig].1 += 1;
            }
        }
    }
    let mut out = sums.map(|(sum, n)| (n > 0).then(|| sum / n as f64));
    if out[3].is_none() {
        out[3] = out[2].map(|l| 10.0 * l);
    }
    out
}

/// The number as the recorder writes it in a name (`0.1`, `1`, `0.01`).
fn fmt(x: f64) -> String {
    let s = format!("{}", x);
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Lays channel `ch`'s vertical settings over `plan` (made for scale `scale`): the DAC word for the offset, the
/// coupling word, the bandwidth bits. Returns what could not be set, for the log.
pub fn apply(s: &Settings, plan: &mut Plan, ch: usize, scale: f64, v: Vertical) -> Vec<String> {
    let mut missing = Vec::new();
    if v.offset != 0.0 {
        let ig = scale_setting(s, ch, scale).and_then(|sc| input_gain(&sc.afe, ch));
        match (last(&plan.afe, ch, DAC), ig.and_then(|g| lsb(s, ch)[g])) {
            (Some(zero), Some(l)) => {
                let code = (zero as f64 - v.offset / l).round().clamp(0.0, 65535.0) as u16;
                plan.afe.push((ch, DAC, code));
            }
            _ => missing.push(format!("CH{}: no recorded offset for this scale (mho-settings.py --extended)", ch)),
        }
    }
    if v.ac {
        match last(&plan.afe, ch, INPUT) {
            Some(r07) => plan.afe.push((ch, INPUT, r07 & !DC)),
            None => missing.push(format!("CH{}: AC coupling: no input word in the plan", ch)),
        }
    }
    if v.bw_mhz > 0 {
        let name = if v.bw_mhz <= 20 { "20M" } else { "250M" };
        let limit = s.list.iter().find(|x| x.name == format!("CH{}_bwlimit={}", ch, name));
        let off = s.list.iter().find(|x| x.name == format!("CH{}_bwlimit=OFF", ch));
        match (limit, off, last(&plan.afe, ch, BW_REG)) {
            (Some(l), Some(o), Some(r2e)) => {
                if let Some(b) = last(&l.afe, ch, BW_REG) {
                    plan.afe.push((ch, BW_REG, (r2e & !BW_BITS) | (b & BW_BITS)));
                }
                let d = l.fpga.get(&BW_FPGA).copied().unwrap_or(0) ^ o.fpga.get(&BW_FPGA).copied().unwrap_or(0);
                if let Some(w) = plan.fpga.iter_mut().find(|w| w.0 == BW_FPGA) {
                    w.1 ^= d;
                }
            }
            _ => missing.push(format!("CH{}: no recorded bandwidth limit {} (mho-settings.py --extended)", ch, name)),
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(name: &str, fpga: &[(u32, u32)], afe: &[(usize, u8, u16)]) -> String {
        let mut s = format!("setting {}\nstate on1=1\n", name);
        for (a, v) in fpga {
            s += &format!("fpga {:#x} {:#010x}\n", a, v);
        }
        for (c, r, d) in afe {
            s += &format!("afe {} {:#04x} {:#06x}\n", c, r, d);
        }
        s + "endsetting\n"
    }

    fn file() -> Settings {
        let t = [
            "---- oos settings ----\nformat 1\nmodel MHO98\n".to_string(),
            setting("CH1_scale=0.01", &[], &[(1, 0x23, 0x2875), (1, 0x20, 0xa04c), (1, 0x28, 0x7f8b)]),
            setting("CH1_scale=0.1", &[], &[(1, 0x23, 0x1075), (1, 0x20, 0xa038), (1, 0x28, 0x7fd0)]),
            setting("CH1_scale=1", &[], &[(1, 0x23, 0x1075), (1, 0x20, 0xa04c), (1, 0x28, 0x8012)]),
            setting("CH1_offset=0_at=0.01", &[], &[(1, 0x28, 0x7f8b)]),
            setting("CH1_offset=0.04_at=0.01", &[], &[(1, 0x28, 0x7c79)]),
            setting("CH1_offset=0_at=0.1", &[], &[(1, 0x28, 0x7fd0)]),
            setting("CH1_offset=-0.4_at=0.1", &[], &[(1, 0x28, 0x7d5a)]),
            setting("CH1_offset=0_at=1", &[], &[(1, 0x28, 0x8012)]),
            setting("CH1_offset=4_at=1", &[], &[(1, 0x28, 0x9025)]),
            setting("CH1_bwlimit=20M", &[(0x1004, 0x04f0_4a84)], &[(1, 0x2e, 0x1905)]),
            setting("CH1_bwlimit=OFF", &[(0x1004, 0x0400_4a84)], &[(1, 0x2e, 0x1fcb)]),
            "end\n".to_string(),
        ]
        .concat();
        Settings::parse(&t).unwrap()
    }

    // Break it catches: a path taken from the wrong register bits, the slope's sign lost (it flips between IG 0 and the
    // attenuated paths, as on the DHO800/DHO900), IG 3 left without a slope.
    #[test]
    fn the_offset_has_one_slope_per_input_path() {
        let s = file();
        let l = lsb(&s, 1);
        assert!((l[0].unwrap() - 0.04 / (0x7f8b - 0x7c79) as f64).abs() < 1e-9, "IG 0: up in volts, down in codes");
        assert!(l[1].unwrap() < 0.0 && l[2].unwrap() < 0.0);
        assert!((l[3].unwrap() / l[2].unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(input_gain(&s.list[0].afe, 1), Some(0));
        assert_eq!(input_gain(&s.list[1].afe, 1), Some(1));
        assert_eq!(input_gain(&s.list[2].afe, 1), Some(2));
    }

    fn plan() -> Plan {
        Plan {
            fpga: vec![(0x1004, 0x0400_4a84)],
            adc: Default::default(),
            afe: vec![(1, 0x07, 0x8440), (1, 0x2e, 0x5fcb), (1, 0x28, 0x7fd0)],
            select: vec![],
            tdiv: 2e-4,
            scale: 0.1,
            srate: 5e6,
            depth: 10_000,
            pre: 5000,
            mask: 1,
            scales: [0.1, 0.0, 0.0, 0.0],
        }
    }

    // Break it catches: the offset at another scale's zero or slope, AC setting a bit other than DC's, the bandwidth
    // bits taking the recorded scale's other bits of 0x2e along, the FPGA's bits not laid over.
    #[test]
    fn offset_coupling_and_bandwidth_lay_their_words_over_the_plan() {
        let s = file();
        let mut p = plan();
        let v = Vertical { offset: -0.4, ac: true, bw_mhz: 20 };
        assert!(apply(&s, &mut p, 1, 0.1, v).is_empty());
        assert_eq!(last(&p.afe, 1, 0x28), Some(0x7d5a));
        assert_eq!(last(&p.afe, 1, 0x07), Some(0x0440));
        assert_eq!(last(&p.afe, 1, 0x2e), Some(0x5905));
        assert_eq!(p.fpga, [(0x1004, 0x04f0_4a84)]);
        let mut q = plan();
        assert!(apply(&s, &mut q, 1, 0.1, Vertical::default()).is_empty());
        assert_eq!(q, plan(), "nothing asked, nothing laid over");
        assert_eq!(apply(&s, &mut q, 2, 0.1, Vertical { offset: 0.1, ..Vertical::default() }).len(), 1);
    }
}
