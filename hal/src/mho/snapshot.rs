// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Starting the acquisition board from a snapshot of the stock system (docs/protocols/mho-acquisition.md, sections 7
//! and 10): the machine-readable part of `tools/mho-check.py --snapshot`, read on the unit from the unit's own file,
//! and the steps that set the ADCs, the JESD204 receiver, the FPGA's configuration and one front end as the stock
//! system had them. Pure: text in, steps out; `oos-fpgad mho-start` runs them.
//!
//! **Not yet verified on hardware.** The order of the steps and the registers left out are assumed (section 10).
//! OOS ships none of the values: they are the unit's own (ADR 0013).

use super::acq;
use crate::dho::bus::Bus;
use crate::dho::scu::{self, Shadows, Window};
use std::collections::BTreeMap;

/// The line that starts the machine-readable part, and the format this reader takes.
pub const MARK: &str = "---- oos snapshot ----";
pub const FORMAT: u32 = 1;

/// The FPGA's link reset (pulsed 1, 5 ms, 0 at a cold start), the JESD204 receiver's words, and the sync enable the
/// stock system sets for 50 ms after them (assumed: the PWM block's 0x10120).
pub const LINK_RESET: u32 = 0x2003c;
pub const JESD: (u32, u32) = (0x20000, 0x20fff);
pub const SYNC: u32 = 0x10120;

/// Configuration words that are not written back, and why: status, counters and read-back words, and the words the
/// start writes itself (the run control and the frame window of the arm, the SPU's reset pulses, the link reset and
/// the sync enable).
pub const NOT_REPLAYED: [(u32, &str); 21] = [
    (0x1000, "SPU control: reset pulses, written by the arm's stop sequence"),
    (0x11e8, "TX state (read)"),
    (0x1208, "recorded range (read)"),
    (0x120c, "recorded range in roll (read)"),
    (0x1210, "ADC clock count (read)"),
    (0x1438, "temperature (read)"),
    (0x4004, "run control: written by the arm"),
    (0x400c, "record and play mode: written by the arm; reads back with state bits (two units differ)"),
    (0x4010, "frame window: written by the arm"),
    (0x4014, "frame window: written by the arm"),
    (0x4018, "frame window: written by the arm"),
    (0x401c, "hardware version (read)"),
    (0x4030, "status (read)"),
    (0x4034, "status (read, written to clear)"),
    (0x4080, "play information: pulses of the arm"),
    (0x40b0, "time stamp (read)"),
    (0x40b4, "time stamp (read)"),
    (0x40c0, "time stamp (read)"),
    (0x40c4, "time stamp (read)"),
    (0x4100, "stop reason (read, written to clear)"),
    (LINK_RESET, "link reset: pulsed by the start"),
];

/// The stock system's SPU control word 0x1000 between its pulses: bit 7 set. It reads set after the FPGA is loaded
/// (0x88, mho-start's bring-up) and in every one of an MHO98's 202 recorded settings (issue #491); the other upper bits
/// it reads with change from one setting to the next and are gone in roll mode (state). What bit 7 does is not known.
/// The shadow stays the DHO800/DHO900's (0): with bit 7 kept (mho-dev-28) the slower rates lost the trigger point in
/// the first run of a start, as they did in other first runs without it; the acquisition thread sets it on request.
pub const S1000: u32 = 0x80;

/// The record depth in points (`--map`: 0x3e8 at 1 k).
pub const DEPTH: u32 = 0x10e4;
/// The SCU's mode word: bits 3:0 the FSM's channel mask.
pub const MODE: u32 = 0x4008;
/// The SCU's mode word for the channels `mask`: bits 3:0 the mask, bits 11:8 the record's interleave factor (1, 2 or 4),
/// as in every 0x4008 of two units' recordings (940 settings of an MHO98 and an MHO934, issue #4 and #248), the rest of
/// `base`. OOS left bits 11:8 at 0 with the DHO800/DHO900's word, and about half the acquisitions fired on the other
/// slope (mho-dev-45, issue #4); **assumed** to be why.
pub fn mode_word(base: u32, mask: u32) -> u32 {
    (base & !0xf0f) | (mask & 0xf) | crate::dho::hz::ratio(mask) << 8
}

/// The bits of 0x4080 the stock system holds set in every recorded setting (bits 28:26).
pub const S4080_STOCK: u32 = 0x1c00_0000;

/// One setting of the stock system, as `mho-check.py --snapshot` took it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub model: String,
    pub hwcode: Option<u32>,
    /// the channel the signal is on, 1 to 4
    pub channel: usize,
    /// its scale at the input (probe ratio 1), as the tool wrote it, and in V/div
    pub scale_text: String,
    pub scale: f64,
    pub timebase: f64,
    pub trigger_level: f64,
    /// front-end frames of the channel in the order the stock system sent them: (step, register, data)
    pub afe: Vec<(String, u8, u16)>,
    /// select lines as read at the end: (gpio number, level)
    pub select: Vec<(u32, bool)>,
    pub fpga: BTreeMap<u32, u32>,
    pub adc: [BTreeMap<u8, u16>; 2],
}

/// One step of the start.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// a register write
    Wr(u32, u32),
    /// a pause, milliseconds
    Wait(u32),
    /// an ADC register write through the bridge (`acq::adc_write`): ADC, register, data
    Adc(u32, u8, u16),
    /// an ADC register read through the bridge, reported (a recipe's check, or the stock program's read in a capture)
    AdcRead(u32, u8),
    /// a frame on a channel's front end: channel (1 to 4), register, data
    Afe(usize, u8, u16),
    /// a select line (its device-tree name) to a level
    Select(String, bool),
}

fn num(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("not a number: {:?}", s))
}

impl Snapshot {
    /// Reads the part after `MARK` of a snapshot report (everything before it is for people). An error names the line.
    pub fn parse(text: &str) -> Result<Snapshot, String> {
        let mut lines = text.lines().skip_while(|l| l.trim() != MARK);
        if lines.next().is_none() {
            return Err(format!("no line {:?}: not a snapshot of mho-check.py --snapshot", MARK));
        }
        let mut s = Snapshot::default();
        let (mut format, mut end) = (None, false);
        for (i, line) in lines.enumerate() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let bad = |e: String| format!("snapshot line {} {:?}: {}", i + 1, line, e);
            let float = |v: &str| v.parse::<f64>().map_err(|_| bad(format!("not a number: {:?}", v)));
            match f.as_slice() {
                [] => {}
                ["end"] => {
                    end = true;
                    break;
                }
                ["format", v] => format = Some(num(v).map_err(bad)?),
                ["model", v] => s.model = v.to_string(),
                ["hwcode", v] => s.hwcode = num(v).ok(),
                ["channel", v] => s.channel = num(v).map_err(bad)? as usize,
                ["scale", v] => {
                    s.scale_text = v.to_string();
                    s.scale = float(v)?;
                }
                ["timebase", v] => s.timebase = float(v)?,
                ["trigger-level", v] => s.trigger_level = float(v)?,
                ["afe", step, r, d] => {
                    let (r, d) = (num(r).map_err(bad)?, num(d).map_err(bad)?);
                    if r > 0xff || d > 0xffff {
                        return Err(bad("a front-end register is 8 bits, its data 16".into()));
                    }
                    s.afe.push((step.to_string(), r as u8, d as u16));
                }
                ["select", name, v] => {
                    // the line's name, then its gpio number (`a149`, `sysref_reset54`): the digits at the end
                    let g = &name[name.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
                    s.select.push((num(g).map_err(bad)?, *v == "1"));
                }
                ["fpga", a, v] => {
                    let a = num(a).map_err(bad)?;
                    if !a.is_multiple_of(4) || a as usize >= acq::BAR_LEN {
                        return Err(bad("not a register of the window".into()));
                    }
                    s.fpga.insert(a, num(v).map_err(bad)?);
                }
                ["adc", n, r, d] => {
                    let (n, r, d) = (num(n).map_err(bad)?, num(r).map_err(bad)?, num(d).map_err(bad)?);
                    if n > 1 || r > 0x7f || d > 0xffff {
                        return Err(bad("ADC 0 or 1, register below 0x80, 16 bits of data".into()));
                    }
                    s.adc[n as usize].insert(r as u8, d as u16);
                }
                _ => {} // a key of a later format: left alone
            }
        }
        if format != Some(FORMAT) {
            return Err(format!("snapshot format {:?}, this build reads {}", format, FORMAT));
        }
        if !end {
            return Err("the snapshot ends before its line `end` (cut off?)".into());
        }
        if !(1..=4).contains(&s.channel) || s.scale <= 0.0 {
            return Err(format!("channel {} at {} V/div: no setting to start from", s.channel, s.scale));
        }
        Ok(s)
    }

    /// The front-end frames of the snapshot's setting: the channel's set of its switch-on (25 frames), then the words
    /// of its scale.
    pub fn afe_frames(&self) -> Result<Vec<(u8, u16)>, String> {
        let of = |step: &str| -> Vec<(u8, u16)> {
            self.afe.iter().filter(|(s, _, _)| s == step).map(|&(_, r, d)| (r, d)).collect()
        };
        let (on, scale) = (of("on"), of(&format!("scale={}", self.scale_text)));
        if on.is_empty() || scale.is_empty() {
            return Err(format!(
                "the snapshot lacks the front end's set of its switch-on or its words at {} V/div",
                self.scale_text
            ));
        }
        Ok([on, scale].concat())
    }

    /// The start in its phases, in order: the ADCs' registers, the link (reset pulse, the receiver's words, the sync
    /// enable for 50 ms), the FPGA's configuration (the SCU's mode word last), the front ends' set-up of the stock
    /// program's start (`init`, from `parse_afe_init`; left out when empty), the snapshot channel's front end and the
    /// select lines.
    pub fn plan(&self, init: &[Step]) -> Result<Vec<(&'static str, Vec<Step>)>, String> {
        let adc: Vec<Step> =
            (0..2u32).flat_map(|n| self.adc[n as usize].iter().map(move |(&r, &d)| Step::Adc(n, r, d))).collect();
        let jesd = |a: &u32| (JESD.0..=JESD.1).contains(a) && *a != LINK_RESET;
        let mut link = vec![Step::Wr(LINK_RESET, 1), Step::Wait(5), Step::Wr(LINK_RESET, 0)];
        link.extend(self.fpga.iter().filter(|(a, _)| jesd(a)).map(|(&a, &v)| Step::Wr(a, v)));
        let sync = self.fpga.get(&SYNC).copied().unwrap_or(0);
        link.extend([Step::Wr(SYNC, sync | 1), Step::Wait(50), Step::Wr(SYNC, sync & !1)]);
        let skip = |a: &u32| jesd(a) || *a == SYNC || *a == MODE || NOT_REPLAYED.iter().any(|(n, _)| n == a);
        let mut cfg: Vec<Step> = self.fpga.iter().filter(|(a, _)| !skip(a)).map(|(&a, &v)| Step::Wr(a, v)).collect();
        if self.fpga.contains_key(&MODE) {
            cfg.push(Step::Wr(MODE, self.shadows().s4008));
        }
        let ch = self.channel;
        let mut afe: Vec<Step> = self.afe_frames()?.into_iter().map(|(r, d)| Step::Afe(ch, r, d)).collect();
        for &(g, v) in &self.select {
            // only the front ends' select lines: the snapshot also reads others (the PMIC's VSEL lines, the panel's
            // reset, `sysref_reset`), which the start never drives
            if let Some(name) = acq::afe::select_name(g) {
                afe.push(Step::Select(name, v));
            }
        }
        if adc.is_empty() {
            return Err("the snapshot holds no ADC register".into());
        }
        let mut plan = vec![("adc", adc), ("link", link), ("fpga", cfg)];
        if !init.is_empty() {
            plan.push(("afe-init", init.to_vec()));
        }
        plan.push(("afe", afe));
        Ok(plan)
    }

    /// The SCU's shadows for the arm and the read-out of the DHO800/DHO900 (the same registers and bits,
    /// mho-acquisition.md section 3): the snapshot's values, the run bits of 0x4004 cleared.
    ///
    /// The control words 0x4004, 0x400c, 0x1000 and 0x4080 read back with state bits (an MHO934 and an MHO98 read
    /// different values in the same setting, 0x4080 with its done bit set), so they are not taken from the snapshot:
    /// they start from the DHO800/DHO900's stock values, the same bits (assumed). Of the mode word 0x4008 the snapshot
    /// gives the FSM's channel mask and bit 27 (set on every MHO900 read so far); the rest is the DHO's.
    pub fn shadows(&self) -> Shadows {
        let h = Shadows::HANDOVER;
        let g = |a: u32, d: u32| self.fpga.get(&a).copied().unwrap_or(d);
        Shadows {
            s4004: h.s4004 & !(1 | 4 | 1 << 31),
            s4008: mode_word(h.s4008 | (g(MODE, 0) & 1 << 27), self.fsm_mask()),
            s400c: h.s400c,
            s1000: h.s1000,
            s1010: g(0x1010, h.s1010),
            // bits 28:26 as the stock system holds them (0x1c00xxxx in every recorded setting of an MHO98; OOS left
            // bit 28 clear with the DHO800/DHO900's value, and a third of the frames at 100 us/div triggered on the
            // other slope, mho-dev-44, issue #4). **Assumed** to be what makes the difference.
            s4080: (h.s4080 & !(1 << 31)) | (g(0x4080, 0) & S4080_STOCK),
            s1004: g(0x1004, h.s1004),
            s4020: g(0x4020, h.s4020),
            s1080: g(0x1080, h.s1080),
            s1084: g(0x1084, h.s1084),
            s40b8: g(0x40b8, h.s40b8),
        }
    }

    /// The FSM's channel mask: bits 3:0 of the snapshot's 0x4008 (the channel's bit if none).
    pub fn fsm_mask(&self) -> u32 {
        match self.fpga.get(&MODE).map(|m| m & 0xf) {
            Some(m) if m != 0 => m,
            _ => 1 << (self.channel - 1),
        }
    }

    /// The read-out window: `n` points (at most the record, a multiple of 4) around the record's middle, where the
    /// trigger is at zero delay; the FSM's channel mask as the snapshot had it (the channel's bit if none).
    pub fn window(&self, n: u32) -> Window {
        let depth = self.fpga.get(&DEPTH).copied().filter(|&d| d >= 4).unwrap_or(n);
        let n = (n.min(depth) / 4 * 4).max(4);
        let fsm = self.fsm_mask();
        Window { n, start: (depth / 2).saturating_sub(n / 2), ratio: 1, fsm, la: false, avg: false }
    }
}

/// The line that starts the machine-readable part of `mho-check.py --afe-init`.
pub const AFE_INIT_MARK: &str = "---- oos afe init ----";

/// The front ends' set-up at the start of the stock program (`mho-check.py --afe-init`, the unit's own file): its
/// frames on every channel and the changes of the front ends' select lines, in the order they happened. Other lines
/// the trace saw (the PMIC's, the panel's, `sysref_reset`) are left out: the start never drives them.
pub fn parse_afe_init(text: &str) -> Result<Vec<Step>, String> {
    let mut lines = text.lines().skip_while(|l| l.trim() != AFE_INIT_MARK);
    if lines.next().is_none() {
        return Err(format!("no line {:?}: not a file of mho-check.py --afe-init", AFE_INIT_MARK));
    }
    let (mut out, mut format, mut end) = (Vec::new(), None, false);
    for (i, line) in lines.enumerate() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let bad = |e: String| format!("afe-init line {} {:?}: {}", i + 1, line, e);
        match f.as_slice() {
            [] => {}
            ["end"] => {
                end = true;
                break;
            }
            ["format", v] => format = Some(num(v).map_err(bad)?),
            ["afe", ch, r, d] => {
                let (ch, r, d) = (num(ch).map_err(bad)?, num(r).map_err(bad)?, num(d).map_err(bad)?);
                if !(1..=4).contains(&ch) || r > 0xff || d > 0xffff {
                    return Err(bad("channel 1 to 4, register 8 bits, data 16".into()));
                }
                out.push(Step::Afe(ch as usize, r as u8, d as u16));
            }
            ["select", name, v] => {
                let g = &name[name.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
                if let Some(n) = acq::afe::select_name(num(g).map_err(bad)?) {
                    out.push(Step::Select(n, *v == "1"));
                }
            }
            _ => {}
        }
    }
    if format != Some(FORMAT) {
        return Err(format!("afe-init format {:?}, this build reads {}", format, FORMAT));
    }
    if !end {
        return Err("the afe-init file ends before its line `end` (cut off?)".into());
    }
    Ok(out)
}

/// What one arm showed: the time to completion (or to giving up), the play information word, the status and the stop
/// reason.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Armed {
    pub ms: u64,
    pub playinfo: u32,
    pub status: u32,
    pub stop_reason: u32,
}

/// One acquisition with the sequences of the DHO800/DHO900 (the same SCU registers, mho-acquisition.md section 3):
/// the status cleared, the arm, a wait of up to `done_ms` for its completion (0x4080 bit 30, polled every 2 ms with
/// `sleep`), then the pass-through of the play side and the play-last read-out. `Ok`: the caller reads `2 * win.n`
/// bytes from the read-out stream. `Err`: the acquisition did not complete; nothing was requested.
pub fn acquire<B: Bus>(
    bus: &mut B,
    sh: &mut Shadows,
    win: &Window,
    done_ms: u64,
    sleep: &mut dyn FnMut(u64),
) -> Result<Armed, Armed> {
    acquire_with(bus, sh, win, done_ms, sleep, &scu::passthrough())
}

/// The play side's pass-through for the MHO900: the DHO800/DHO900's (`scu::passthrough`) with the upper bits of
/// 0x1154 kept as the stock system set them in `word` (0x4c3 in every recorded setting from 20 ns/div on, 0x5c6 with
/// the sinc interpolation below; the DHO's sets them 0). At 1 GSa/s and above the read-out held a flat line with the
/// DHO's (hp197's MHO98, mho-dev-21, issue #4), where the stock system's settings of 1 us/div and 2 us/div differ in
/// the rate code only (assumed: those bits matter where the two ADCs interleave).
pub fn passthrough_keeping(word: u32) -> Vec<crate::dho::Write> {
    scu::passthrough().into_iter().map(|(a, v)| if a == 0x1154 { (a, v | (word & !0x7)) } else { (a, v) }).collect()
}

/// `acquire` with the play side's pass-through `pass`.
pub fn acquire_with<B: Bus>(
    bus: &mut B,
    sh: &mut Shadows,
    win: &Window,
    done_ms: u64,
    sleep: &mut dyn FnMut(u64),
    pass: &[crate::dho::Write],
) -> Result<Armed, Armed> {
    let last = bus.rd(0x4034);
    bus.apply(&scu::clear_status(last));
    bus.apply(&scu::arm(win.fsm, sh, 0, 1));
    let mut ms = 0;
    let mut v = bus.rd(0x4080);
    while v & scu::PLAYINFO_DONE == 0 && ms < done_ms {
        sleep(2);
        ms += 2;
        v = bus.rd(0x4080);
    }
    let a = Armed { ms, playinfo: v, status: bus.rd(0x4034), stop_reason: bus.rd(0x4100) };
    if v & scu::PLAYINFO_DONE == 0 {
        return Err(a);
    }
    sh.s4080 = v;
    bus.apply(pass);
    bus.apply(&scu::readout(win, sh));
    Ok(a)
}

/// What a frame of the probe compensation signal looks like: its extremes and mean, and the two levels either side
/// of the mean with the share of points at the upper one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub low: f64,
    pub high: f64,
    pub high_share: f64,
}

impl Levels {
    /// Volts of the words at `scale` V/div and zero offset (`acq::volts`).
    pub fn of(words: &[u16], scale: f64) -> Option<Levels> {
        let v: Vec<f64> = words.iter().map(|&w| acq::volts(w, scale, 0.0)).collect();
        if v.is_empty() {
            return None;
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let avg = |s: &[f64]| if s.is_empty() { mean } else { s.iter().sum::<f64>() / s.len() as f64 };
        let (hi, lo): (Vec<f64>, Vec<f64>) = v.iter().partition(|&&x| x > mean);
        Some(Levels {
            min: v.iter().copied().fold(f64::INFINITY, f64::min),
            max: v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            mean,
            low: avg(&lo),
            high: avg(&hi),
            high_share: hi.len() as f64 / v.len() as f64,
        })
    }

    /// A square wave: two levels at least half a division apart, each held by at least a tenth of the points, and
    /// the points close to them (the levels hold most of the swing).
    pub fn is_square(&self, scale: f64) -> bool {
        let swing = self.high - self.low;
        swing >= 0.5 * scale
            && (0.1..=0.9).contains(&self.high_share)
            && (self.max - self.min) < 1.5 * swing
            && self.max < 4.0 * scale + 1e-9
            && self.min > -4.0 * scale - 1e-9
    }

    /// The square wave with one of its levels at the end of the range (a signal larger than the scale's range, an
    /// MHO98 at 0.1 V/div, issue #491): it measures, the scale is too fine. Both ends reached is not the square but
    /// spikes, as in the first acquisitions after the set-up.
    pub fn is_clipped_square(&self, scale: f64) -> bool {
        let rail = 4.3 * scale;
        let swing = self.high - self.low;
        swing >= 0.5 * scale
            && (0.1..=0.9).contains(&self.high_share)
            && (self.max - self.min) < 1.5 * swing
            && ((self.max >= rail) != (self.min <= -rail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "for people\n---- oos snapshot ----\nformat 1\nmodel MHO98\nhwcode 3\nchannel 2\nscale 1\n\
        timebase 2e-4\ntrigger-level 1.5\nafe on 0x07 0xc440\nafe on 0x2b 0x4c5f\nafe base 0x28 0x8000\n\
        afe scale=0.5 0x07 0x8440\nafe scale=1 0x07 0x8440\nafe scale=1 0x29 0x0280\nselect vsel46 0\n\
        select reset52 1\nselect sysref_reset54 0\nselect a129 0\nselect c131 1\nfpga 0x1004 0x04004a50\nfpga 0x10e4 0x00002710\nfpga 0x1210 0x0000c34f\nfpga 0x4004 0x80000005\n\
        fpga 0x4008 0x08805182\nfpga 0x10120 0x00000000\nfpga 0x20004 0x00000011\nfpga 0x2003c 0x00000000\n\
        adc 0 0x0a 0x8080\nadc 1 0x09 0x0700\nend\n";

    #[test]
    fn a_snapshot_is_read_from_its_mark_on() {
        let s = Snapshot::parse(TEXT).unwrap();
        assert_eq!((s.model.as_str(), s.hwcode, s.channel, s.scale), ("MHO98", Some(3), 2, 1.0));
        assert_eq!(s.afe.len(), 6);
        assert_eq!(s.select, [(46, false), (52, true), (54, false), (129, false), (131, true)]);
        assert_eq!(s.fpga.get(&0x4008), Some(&0x0880_5182));
        assert_eq!(s.adc[1].get(&0x09), Some(&0x0700));
    }

    // Break it catches: a file of another tool, a cut-off upload, a newer format read as if it were this one, a
    // register outside the window or a word too wide for its bus.
    #[test]
    fn what_is_not_a_whole_snapshot_is_refused() {
        assert!(Snapshot::parse("mho-check 8\nno part for OOS\n").is_err());
        assert!(Snapshot::parse(&TEXT.replace("end\n", "")).is_err());
        assert!(Snapshot::parse(&TEXT.replace("format 1", "format 2")).is_err());
        assert!(Snapshot::parse(&TEXT.replace("fpga 0x1004", "fpga 0x1002")).is_err());
        assert!(Snapshot::parse(&TEXT.replace("adc 1 0x09", "adc 2 0x09")).is_err());
        assert!(Snapshot::parse(&TEXT.replace("afe on 0x07 0xc440", "afe on 0x07 0x1c440")).is_err());
        assert!(Snapshot::parse(&TEXT.replace("channel 2", "channel 5")).is_err());
    }

    #[test]
    fn the_front_end_gets_its_whole_set_then_its_scale() {
        let s = Snapshot::parse(TEXT).unwrap();
        assert_eq!(s.afe_frames().unwrap(), [(0x07, 0xc440), (0x2b, 0x4c5f), (0x07, 0x8440), (0x29, 0x0280)]);
        let none = Snapshot { scale_text: "2".into(), ..s };
        assert!(none.afe_frames().is_err());
    }

    // Break it catches: a status, counter or run-control word written back, the JESD words before the link reset,
    // the mode word before the rest of the configuration, the sync enable left on.
    #[test]
    fn the_plan_writes_the_configuration_but_no_status_and_the_mode_last() {
        let p = Snapshot::parse(TEXT).unwrap().plan(&[]).unwrap();
        let names: Vec<&str> = p.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["adc", "link", "fpga", "afe"]);
        assert_eq!(p[0].1, [Step::Adc(0, 0x0a, 0x8080), Step::Adc(1, 0x09, 0x0700)]);
        assert_eq!(
            p[1].1,
            [
                Step::Wr(0x2003c, 1),
                Step::Wait(5),
                Step::Wr(0x2003c, 0),
                Step::Wr(0x20004, 0x11),
                Step::Wr(0x10120, 1),
                Step::Wait(50),
                Step::Wr(0x10120, 0)
            ]
        );
        // the mode word: the DHO's with the snapshot's channel mask and bit 27, not its state bits
        assert_eq!(p[2].1, [Step::Wr(0x1004, 0x0400_4a50), Step::Wr(0x10e4, 0x2710), Step::Wr(0x4008, 0x0880_0102)]);
        // the PMIC's VSEL lines, the panel's reset and sysref_reset are in the snapshot, never in the plan
        assert_eq!(
            p[3].1[4..],
            [Step::Select("afe_sel_a_ch2".into(), false), Step::Select("afe_sel_c_ch2".into(), true)]
        );
    }

    // Break it catches: the interleave field of the mode word left 0 (the DHO's word), another field than bits 11:8.
    #[test]
    fn the_mode_word_carries_the_interleave_factor() {
        assert_eq!(mode_word(0x0880_0001, 1), 0x0880_0101);
        assert_eq!(mode_word(0x0880_0001, 0x3), 0x0880_0203);
        assert_eq!(mode_word(0x0880_0001, 0xd), 0x0880_040d);
        assert_eq!(mode_word(0x0880_0101, 0x8), 0x0880_0108, "CH4 alone: one channel");
    }

    #[test]
    fn the_read_out_window_is_the_records_middle_with_the_snapshots_channel_mask() {
        let s = Snapshot::parse(TEXT).unwrap();
        let w = s.window(4000);
        assert_eq!((w.n, w.start, w.ratio, w.fsm), (4000, 3000, 1, 2));
        assert_eq!(s.window(100_000).n, 10_000);
        // Break it catches: a control word taken from its read-back, which carries state bits (0x4080 read with its
        // done bit set on an MHO934): the arm would write them back into the run control
        let mut busy = s.clone();
        busy.fpga.extend([(0x4004, 0x0f00_a829), (0x400c, 0xa280_8dc2), (0x1000, 0x0585_0a80), (0x4080, 0x5c00_0023)]);
        let (h, b) = (Shadows::HANDOVER, busy.shadows());
        // only bits 28:26 of 0x4080 come from the read-back (the stock system holds them set), not its done bit 30
        assert_eq!((b.s4004, b.s400c, b.s1000, b.s4080), (0, h.s400c, h.s1000, h.s4080 | S4080_STOCK));
        assert!(!busy.plan(&[]).unwrap()[2]
            .1
            .iter()
            .any(|w| matches!(w, Step::Wr(0x400c | 0x4004 | 0x4080 | 0x1000, _))));
        let mut no_mode = s.clone();
        no_mode.fpga.remove(&MODE);
        assert_eq!(no_mode.window(4000).fsm, 2);
    }

    /// A register window that records the writes and completes the arm after `done_after` reads of 0x4080.
    struct Fake {
        writes: Vec<(u32, u32)>,
        reads_4080: u32,
        done_after: u32,
    }

    impl Bus for Fake {
        fn wr(&mut self, off: u32, v: u32) {
            self.writes.push((off, v));
        }
        fn rd(&mut self, off: u32) -> u32 {
            if off == 0x4080 {
                self.reads_4080 += 1;
                if self.reads_4080 > self.done_after {
                    return scu::PLAYINFO_DONE | 0x10;
                }
            }
            0
        }
    }

    // Break it catches: a read-out requested for an arm that never completed (the read would wait on an empty
    // stream), a wait that never ends, the read-out without the pass-through before it.
    #[test]
    fn the_read_out_is_requested_only_after_the_arm_completed() {
        let s = Snapshot::parse(TEXT).unwrap();
        let win = s.window(4000);
        let mut slept = 0;
        let mut never = Fake { writes: Vec::new(), reads_4080: 0, done_after: u32::MAX };
        let r = acquire(&mut never, &mut s.shadows(), &win, 100, &mut |ms| slept += ms);
        assert_eq!(r.map_err(|a| a.ms), Err(100));
        assert_eq!(slept, 100);
        assert!(!never.writes.iter().any(|w| w.0 == 0x1100), "no read-out window without a completed arm");
        let mut soon = Fake { writes: Vec::new(), reads_4080: 0, done_after: 3 };
        let a = acquire(&mut soon, &mut s.shadows(), &win, 100, &mut |_| {}).unwrap();
        assert_eq!((a.ms, a.playinfo), (6, scu::PLAYINFO_DONE | 0x10));
        let pos = |o: u32| soon.writes.iter().position(|w| w.0 == o).unwrap();
        assert!(pos(0x1170) < pos(0x1100), "the pass-through comes before the read-out window");
        assert_eq!(soon.writes.last(), Some(&(0x4004, 5)), "the read-out ends with the play-last bit and the FSM run");
    }

    // Break it catches: the set-up of another channel replayed on the snapshot's, a select line of the PMIC or the
    // panel driven, the set-up after the snapshot's own words (it would undo them)
    #[test]
    fn the_stock_programs_set_up_comes_before_the_snapshots_front_end() {
        let text = "x\n---- oos afe init ----\nformat 1\nafe 1 0x22 0x0000\nselect vsel46 1\nselect c153 1\n\
                    afe 4 0x36 0x1234\nend\n";
        let init = parse_afe_init(text).unwrap();
        assert_eq!(
            init,
            [Step::Afe(1, 0x22, 0), Step::Select("afe_sel_c_ch1".into(), true), Step::Afe(4, 0x36, 0x1234)]
        );
        let p = Snapshot::parse(TEXT).unwrap().plan(&init).unwrap();
        let names: Vec<&str> = p.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["adc", "link", "fpga", "afe-init", "afe"]);
        assert_eq!(p[4].1[0], Step::Afe(2, 0x07, 0xc440));
        assert!(parse_afe_init("no mark\n").is_err());
        assert!(parse_afe_init(&text.replace("end\n", "")).is_err());
        assert!(parse_afe_init(&text.replace("afe 4", "afe 5")).is_err());
    }

    // Break it catches: the stock system's upper bits of 0x1154 cleared by the pass-through (the read-out at 1 GSa/s
    // and above was flat with them cleared), or the pass-through's other words changed.
    #[test]
    fn the_mho_pass_through_keeps_the_stock_bits_of_0x1154() {
        let p = passthrough_keeping(0x4c3);
        let w1154: Vec<u32> = p.iter().filter(|w| w.0 == 0x1154).map(|w| w.1).collect();
        assert_eq!(w1154, [0x4c1, 0x4c3]);
        let others = |v: &[(u32, u32)]| v.iter().filter(|w| w.0 != 0x1154).copied().collect::<Vec<_>>();
        assert_eq!(others(&p), others(&scu::passthrough()));
    }

    // Break it catches: a flat line, noise, a clipped trace or a single spike taken for the probe compensation signal.
    #[test]
    fn a_square_wave_is_told_from_a_line_and_from_noise() {
        let code = |v: f64| (32768.0 + v * 7500.0) as u16; // 1 V/div
        let square: Vec<u16> = (0..1000).map(|i| code(if (i / 100) % 2 == 0 { 0.0 } else { 3.0 })).collect();
        let l = Levels::of(&square, 1.0).unwrap();
        assert!(l.is_square(1.0), "{:?}", l);
        assert!((l.low - 0.0).abs() < 1e-3 && (l.high - 3.0).abs() < 1e-3 && (l.high_share - 0.5).abs() < 1e-9);
        let flat = vec![code(0.01); 1000];
        assert!(!Levels::of(&flat, 1.0).unwrap().is_square(1.0));
        let noise: Vec<u16> = (0..1000).map(|i| code(((i * 7919) % 100) as f64 / 100.0 * 0.2)).collect();
        assert!(!Levels::of(&noise, 1.0).unwrap().is_square(1.0));
        let spike: Vec<u16> = (0..1000).map(|i| code(if i == 500 { 3.0 } else { 0.0 })).collect();
        assert!(!Levels::of(&spike, 1.0).unwrap().is_square(1.0));
        let clipped = vec![0xffffu16; 1000];
        assert!(!Levels::of(&clipped, 1.0).unwrap().is_square(1.0));
        assert!(!Levels::of(&clipped, 1.0).unwrap().is_clipped_square(1.0));
        // 0 to 3 V at 0.1 V/div: its upper level at the end of the range; spikes to both ends are not the square
        let over: Vec<u16> = (0..1000).map(|i| if i % 100 < 50 { 0x8000 } else { 0xffff }).collect();
        let l = Levels::of(&over, 0.1).unwrap();
        assert!(!l.is_square(0.1) && l.is_clipped_square(0.1), "{:?}", l);
        let both: Vec<u16> = (0..1000).map(|i| [0x8000, 0xffff, 0x0000][i % 3]).collect();
        assert!(!Levels::of(&both, 0.1).unwrap().is_clipped_square(0.1));
        assert_eq!(Levels::of(&[], 1.0), None);
    }
}
