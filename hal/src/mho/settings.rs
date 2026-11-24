// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The stock system's settings of one unit (`tools/mho-settings.py`, docs/protocols/mho-acquisition.md section 7),
//! and the board's state for a channel, time base and scale made from them (section 11). Pure: text in, a plan out;
//! the MHO900's acquisition thread of oos-ui runs it. The values are the unit's own, read from its file (ADR 0013).
//! **Not yet verified on hardware.**
//!
//! A channel's state at a time base is one recorded setting (`CHn tdiv=T`, at 1 V/div). A scale changes some bits of
//! some words against 1 V/div (`CHn scale=V` against `CHn scale=1`, both at 200 us/div): those bits are laid over the
//! time base's words. The front end gets the channel's switch-on set and the scale's words, the ADCs the channel's
//! registers (their input selection follows the channel).

use super::snapshot::{NOT_REPLAYED, SYNC};
use std::collections::BTreeMap;

/// The line that starts the machine-readable part of `mho-settings.py`.
pub const MARK: &str = "---- oos settings ----";
pub const FORMAT: u32 = 1;

/// Words that hold live values, not settings (they read differently from one setting to the next with nothing
/// changed; an MHO934's recording, issue #248): not written.
pub const LIVE: [u32; 8] = [0x1024, 0x1028, 0x1030, 0x1044, 0x1048, 0x1438, 0x40b8, 0x40d0];

/// The SPU's digital filter (docs/protocols/adc-mode.md section 4.2: bits 1:0 = 1 bypass it, bit 0 = 0 runs it with
/// the coefficients loaded through 0x109c). The stock system runs it from 1 GSa/s on (0x1098 = 0) with coefficients
/// its start loads and OOS does not; with none loaded the read-out is a flat line at 0 V (an MHO98, issue #491).
/// The plan keeps it bypassed at every rate. **Assumed** that this is why the read-out was flat.
pub const FILTER: u32 = 0x1098;

/// The rate at which the board's trigger lies at the record's trigger point: 4 GSa/s, the ADCs' own (an MHO98,
/// mho-dev-24, issue #491: the edge within 2 samples of it in every frame at 4 GSa/s; at every rate below, 2 GSa/s to
/// 5 MSa/s, anywhere in the record). Below it a plan records at this rate, deeper by the ratio, and the frame takes
/// every n-th point (`plan_fast`), up to `OVERSAMPLE_MAX` times.
pub const FAST: f64 = 4e9;
pub const OVERSAMPLE_MAX: u32 = 100;
/// The deepest record `plan_deep` makes (the acquisition thread reads at most this many points).
pub const DEEP_MAX: u32 = 1_000_000;

/// One recorded setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setting {
    pub name: String,
    /// the scope's own report: on1..on4, scale1..scale4, tdiv, srate, mdepth
    pub state: BTreeMap<String, String>,
    pub fpga: BTreeMap<u32, u32>,
    pub adc: [BTreeMap<u8, u16>; 2],
    /// front-end frames sent while it changed, in order: (channel, register, data)
    pub afe: Vec<(usize, u8, u16)>,
    /// the select lines after it: (gpio number, level)
    pub select: Vec<(u32, bool)>,
}

impl Setting {
    pub fn num(&self, key: &str) -> Option<f64> {
        self.state.get(key).and_then(|v| v.parse().ok())
    }
}

/// The board's state for one channel, time base and scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// the FPGA's words to write, in address order (the SCU's mode word left to the arm)
    pub fpga: Vec<(u32, u32)>,
    /// the ADCs' registers as they must read (brought there where they read otherwise)
    pub adc: [BTreeMap<u8, u16>; 2],
    /// front-end frames: the channel's switch-on set, then its scale's words
    pub afe: Vec<(usize, u8, u16)>,
    pub select: Vec<(u32, bool)>,
    /// what the stock system reported for this setting: the time base and scale recorded, the sample rate, the depth
    pub tdiv: f64,
    pub scale: f64,
    pub srate: f64,
    pub depth: u32,
    /// samples before the trigger (0x4050)
    pub pre: u32,
    /// the FSM's channel mask
    pub mask: u32,
    /// the scale recorded for each channel shown, 0 for one not shown (`scale` is the first one's)
    pub scales: [f64; 4],
}

impl Plan {
    /// Channel `ch`'s digital gain (its first slot's Q10 word, afe-settings.md 4.1), 1.0 if the plan has none. The
    /// stock system converts the trigger level with it: 6720 codes/div at 100 mV/div where the gain is 1.114 (an
    /// MHO98, issue #4: 7500 / 1.114 = 6731).
    pub fn gain(&self, ch: usize) -> f64 {
        let w = slot_words(slots(ch, self.mask)[0])[0];
        self.fpga.iter().find(|x| x.0 == w).map_or(1.0, |x| x.1 as f64 / 1024.0).max(1e-3)
    }
}

/// The SPU's peak-detect bit in 0x1004: an MHO98 under the stock system sets it at 1 ms/div (0x04004a84 to
/// 0x04004a86) and not at 4 GSa/s (issue #4), as the DHO800/DHO900 do (horizontal-config.md, acquire modes).
pub const PEAK_1004: u32 = 1 << 1;

impl Plan {
    /// Sets channel `ch`'s digital gain and offset words (`gain`, `offset`) in every slot it has, main and zoom.
    pub fn set_slots(&mut self, ch: usize, gain: u32, offset: u32) {
        for k in slots(ch, self.mask) {
            let w = slot_words(k);
            for (a, v) in [(w[0], gain), (w[1], offset), (w[2], gain), (w[3], offset)] {
                self.set(a, v);
            }
        }
        self.fpga.sort_unstable();
    }

    /// The points before the trigger: `pre` of the record's depth (0x4050), the rest after it (0x4054).
    pub fn set_pre(&mut self, pre: u32) {
        self.pre = pre.min(self.depth);
        self.set(0x4050, self.pre);
        self.set(0x4054, self.depth - self.pre);
        self.fpga.sort_unstable();
    }

    /// The slowest recorded setting's sample rate for these channels (roll records at it).
    pub fn slowest_rate(st: &Settings, chans: &[(usize, f64)]) -> Option<f64> {
        let ch = chans.first()?.0;
        let t = st.steps(ch, "tdiv").into_iter().map(|x| x.0).reduce(f64::max)?;
        st.plan_many(chans, t).ok().map(|p| p.srate)
    }

    /// Peak detect: 0x1004 bit 1, below the board's highest rate for the channels recorded (4 GSa/s alone, 2 with
    /// two, 1 with four: there is nothing to detect over at that rate). False where it is not set.
    pub fn peak(&mut self) -> bool {
        if self.srate >= FAST / crate::dho::hz::ratio(self.mask) as f64 * 0.999 {
            return false;
        }
        match self.fpga.iter_mut().find(|w| w.0 == 0x1004) {
            Some(w) => {
                w.1 |= PEAK_1004;
                true
            }
            None => false,
        }
    }
}

/// The gain and offset words channel `ch` has in a setting recorded of it alone: slot 0 for CH1, slot 1 for the
/// others (recorded together with CH1).
pub fn own_slot(s: &Setting, ch: usize) -> Option<(u32, u32)> {
    let w = slot_words(if ch == 1 { 0 } else { 1 });
    Some((*s.fpga.get(&w[0])?, *s.fpga.get(&w[1])?))
}

/// The read path's digital gain and offset of slot `k` (0 to 3) and of its zoom copy (afe-settings.md 4.1): the
/// channel of each slot follows the channels recorded (`slots`).
fn slot_words(k: usize) -> [u32; 4] {
    let k = k as u32;
    [0x14c8 + 12 * k, 0x14d0 + 12 * k, 0x14f8 + 8 * k, 0x14fc + 8 * k]
}

/// The slots channel `ch` has in a record of the channels in `mask`: all four alone, the lower channel's 0 and 2 and
/// the higher's 1 and 3 with two (CH1 and another: the stock system records no pair without CH1), its own (`ch - 1`)
/// with three or four. Seen in an MHO98's recording (issue #4): CH2's gain in slots 1 and 3 with CH1+CH2, CH1 to CH4
/// in slots 0 to 3 with all four and with CH2+CH3 (recorded as CH1+CH2+CH3).
pub fn slots(ch: usize, mask: u32) -> Vec<usize> {
    match crate::dho::hz::ratio(mask) {
        1 => vec![0, 1, 2, 3],
        2 if mask.trailing_zeros() as usize == ch - 1 => vec![0, 2],
        2 => vec![1, 3],
        _ => vec![ch - 1],
    }
}

/// A setting the stock system recorded in roll mode: 0x1170 bit 31 (with ROLL on it rolls from 50 ms/div, 0x1170
/// 0x40000001 to 0xc000000a, protocols/mho-acquisition.md 12.3).
fn rolled(s: &Setting) -> bool {
    s.fpga.get(&0x1170).is_some_and(|v| v & 0x8000_0000 != 0)
}

/// The slowest of `steps` recorded as an ordinary record.
fn normal_of(steps: Vec<(f64, &Setting)>) -> Option<Setting> {
    steps.into_iter().filter(|(_, s)| !rolled(s)).map(|x| x.1).next_back().cloned()
}

/// A plan made from a setting recorded in roll mode, made an ordinary record as the stock system records 50 and 100
/// ms/div with Auto Roll off: the trigger in the middle (0x4050 = 0x4054 = depth / 2), and 0x11fc, 0x1154 and 0x1170
/// from the slowest ordinary setting `normal` of the same channels (**assumed** to hold at the slower time bases). On
/// an MHO98 OOS replayed the roll words (0x4050 999999, 0x4054 1, 0x11fc 100) and 23.4 ms after the trigger point read
/// 0 V, where the stock system holds 0x4050 = 0x4054 = 500000 and 0x11fc = 1000 at 50 ms/div (mho-dev-41, issue #4).
fn unroll(mut p: Plan, normal: Option<Setting>) -> Result<Plan, String> {
    if !p.fpga.iter().any(|w| w.0 == 0x1170 && w.1 & 0x8000_0000 != 0) {
        return Ok(p);
    }
    if let Some(n) = normal {
        for a in [0x11fc, 0x1154, 0x1170] {
            if let Some(&v) = n.fpga.get(&a) {
                p.set(a, v);
            }
        }
    }
    p.pre = p.depth / 2;
    p.set(0x4050, p.pre);
    p.set(0x4054, p.depth - p.pre);
    p.fpga.sort_unstable();
    Ok(p)
}

/// Words a plan never writes: not settings, live values, the sync, the SCU's mode word (the arm's).
fn skipped(a: u32) -> bool {
    NOT_REPLAYED.iter().any(|(n, _)| *n == a) || LIVE.contains(&a) || a == SYNC || a == super::snapshot::MODE
}

/// All settings of a unit's file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub model: String,
    pub list: Vec<Setting>,
}

/// The recorded step nearest to `x`, by ratio.
fn nearest(v: Vec<(f64, &Setting)>, x: f64) -> Option<(f64, &Setting)> {
    v.into_iter().min_by(|a, b| (a.0 / x).ln().abs().total_cmp(&(b.0 / x).ln().abs()))
}

fn num(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("not a number: {:?}", s))
}

/// The record's segment size (0x10e0) for `depth` points of one channel: a quarter of the DHO800/DHO900's
/// (`hz::mem_size`), which gives the stock system's 0x200 at 10 k points and 0x8000 at 1 M (an MHO98's recording, issue
/// #491); twice that with two channels recorded (0x400 at 10 k). **Assumed** between them.
pub fn segment(depth: u32) -> u32 {
    (crate::dho::hz::mem_size(depth as i128) / 4) as u32
}

impl Plan {
    fn set(&mut self, a: u32, v: u32) {
        match self.fpga.iter_mut().find(|w| w.0 == a) {
            Some(w) => w.1 = v,
            None => self.fpga.push((a, v)),
        }
    }
}

impl Settings {
    /// `plan`, recorded at `FAST` when the setting's rate is below it by a whole ratio of at most `OVERSAMPLE_MAX`:
    /// the channel's 4 GSa/s setting with the depth, the segment and the points before and after the trigger times
    /// the ratio, and the ratio (every that many points of the record make the frame; 1: the plan as recorded).
    pub fn plan_fast(&self, ch: usize, tdiv: f64, scale: f64) -> Result<(Plan, u32), String> {
        let p = self.plan(ch, tdiv, scale)?;
        let r = FAST / p.srate;
        let k = r.round() as u32;
        if k <= 1 || k > OVERSAMPLE_MAX || (r - k as f64).abs() > 1e-6 {
            return Ok((p, 1));
        }
        let fast = self.steps(ch, "tdiv").into_iter().filter(|(_, s)| s.num("srate") == Some(FAST)).map(|x| x.0);
        let Some(t4) = fast.reduce(f64::max) else {
            return Ok((p, 1));
        };
        let mut f = self.plan(ch, t4, scale)?;
        let (depth, pre) = (p.depth * k, p.pre * k);
        f.set(super::snapshot::DEPTH, depth);
        f.set(0x10e0, segment(depth) * crate::dho::hz::ratio(f.mask));
        f.set(0x4050, pre);
        f.set(0x4054, depth - pre);
        f.fpga.sort_unstable();
        (f.depth, f.pre, f.tdiv) = (depth, pre, p.tdiv);
        Ok((f, k))
    }

    /// The plan for the channels `chans` at `tdiv` with a record of the span of `depth` points at `rate` Sa/s (a depth
    /// the screen fixes instead of the recorded 10 k): the words of the recorded time base whose rate is nearest to
    /// `rate` (the slowest of equal ones), the depth scaled to its rate so the record spans the same time (at most
    /// `DEEP_MAX`), and the depth (0x10e4), segment (0x10e0) and points before and after the trigger (0x4050/0x4054)
    /// set as `plan_fast` sets them. The screen's rates need not be the board's: 8 MSa/s asked at 100 k points took
    /// none on an MHO98 (mho-dev-35, issue #4). Err when no setting of these channels is recorded.
    pub fn plan_deep(&self, chans: &[(usize, f64)], tdiv: f64, rate: f64, depth: u32) -> Result<Plan, String> {
        let ch = chans.first().ok_or("no channel on")?.0;
        let off = |p: &Plan| (p.srate / rate).ln().abs();
        let mut best: Option<Plan> = None;
        for (t, _) in self.steps(ch, "tdiv") {
            let Ok(p) = self.plan_many(chans, t) else { continue };
            let better = best
                .as_ref()
                .is_none_or(|b| off(&p) < off(b) - 1e-9 || (off(&p) - off(b)).abs() < 1e-9 && p.tdiv > b.tdiv);
            if better {
                best = Some(p);
            }
        }
        let mut p = best.ok_or("no setting of these channels recorded")?;
        let depth = ((depth as f64 * p.srate / rate).round() as u32).clamp(1000, DEEP_MAX);
        let pre = depth / 2;
        p.set(super::snapshot::DEPTH, depth);
        p.set(0x10e0, segment(depth) * crate::dho::hz::ratio(p.mask));
        p.set(0x4050, pre);
        p.set(0x4054, depth - pre);
        p.fpga.sort_unstable();
        (p.depth, p.pre, p.tdiv) = (depth, pre, tdiv);
        Ok(p)
    }

    /// Reads the part after `MARK`. An error names the line.
    pub fn parse(text: &str) -> Result<Settings, String> {
        let mut lines = text.lines().skip_while(|l| l.trim() != MARK);
        if lines.next().is_none() {
            return Err(format!("no line {:?}: not a file of mho-settings.py", MARK));
        }
        let mut out = Settings::default();
        let (mut format, mut end, mut cur): (_, _, Option<Setting>) = (None, false, None);
        for (i, line) in lines.enumerate() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let bad = |e: String| format!("settings line {} {:?}: {}", i + 1, line, e);
            match (f.as_slice(), cur.as_mut()) {
                ([], _) => {}
                (["end"], None) => {
                    end = true;
                    break;
                }
                (["format", v], None) => format = Some(num(v).map_err(bad)?),
                (["model", v], None) => out.model = v.to_string(),
                (["hwcode", _], None) => {}
                (["setting", name], None) => cur = Some(Setting { name: name.to_string(), ..Setting::default() }),
                (["endsetting"], Some(_)) => out.list.extend(cur.take()),
                (["scpi", ..], Some(_)) => {}
                (["state", kv @ ..], Some(s)) => {
                    for p in kv {
                        if let Some((k, v)) = p.split_once('=') {
                            s.state.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                (["fpga", a, v], Some(s)) => {
                    s.fpga.insert(num(a).map_err(bad)?, num(v).map_err(bad)?);
                }
                (["adc", n, r, d], Some(s)) => {
                    let (n, r, d) = (num(n).map_err(bad)?, num(r).map_err(bad)?, num(d).map_err(bad)?);
                    if n > 1 || r > 0x7f || d > 0xffff {
                        return Err(bad("ADC 0 or 1, register below 0x80, 16 bits".into()));
                    }
                    s.adc[n as usize].insert(r as u8, d as u16);
                }
                (["afe", ch, r, d], Some(s)) => {
                    let (ch, r, d) = (num(ch).map_err(bad)?, num(r).map_err(bad)?, num(d).map_err(bad)?);
                    if !(1..=4).contains(&ch) || r > 0xff || d > 0xffff {
                        return Err(bad("channel 1 to 4, register 8 bits, data 16".into()));
                    }
                    s.afe.push((ch as usize, r as u8, d as u16));
                }
                (["select", name, v], Some(s)) => {
                    let g = &name[name.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
                    s.select.push((num(g).map_err(bad)?, *v == "1"));
                }
                (_, None) | (_, Some(_)) => return Err(bad("unexpected line".into())),
            }
        }
        if format != Some(FORMAT) {
            return Err(format!("settings format {:?}, this build reads {}", format, FORMAT));
        }
        if !end || cur.is_some() {
            return Err("the settings end before their line `end` (cut off?)".into());
        }
        Ok(out)
    }

    fn get(&self, name: &str) -> Option<&Setting> {
        self.list.iter().find(|s| s.name == name)
    }

    /// The recorded values of `kind` (`tdiv`, `scale`) for channel `ch`, from the names `CHn_kind=V`.
    pub fn steps(&self, ch: usize, kind: &str) -> Vec<(f64, &Setting)> {
        self.named(&format!("CH{}_{}=", ch, kind))
    }

    /// The board's state for channel `ch` (1 to 4) alone at the recorded time base and scale nearest to `tdiv` and
    /// `scale` (both in a factor's distance). Err when the file holds no setting of the channel.
    pub fn plan(&self, ch: usize, tdiv: f64, scale: f64) -> Result<Plan, String> {
        let (td, tb) = nearest(self.steps(ch, "tdiv"), tdiv).ok_or(format!("no time base of CH{} recorded", ch))?;
        let (sv, sc) = nearest(self.steps(ch, "scale"), scale).ok_or(format!("no scale of CH{} recorded", ch))?;
        let reference = self.get(&format!("CH{}_scale=1", ch)).ok_or(format!("no CH{} at 1 V/div recorded", ch))?;
        let adc = self
            .steps(ch, "tdiv")
            .into_iter()
            .map(|(_, s)| s)
            .find(|s| !s.adc[0].is_empty())
            .map(|s| s.adc.clone())
            .ok_or(format!("no ADC registers of CH{} recorded", ch))?;
        // the scale's bits against 1 V/div, laid over the time base's word
        let fpga = tb
            .fpga
            .iter()
            .filter(|(a, _)| !skipped(**a))
            .map(|(&a, &v)| {
                let d = sc.fpga.get(&a).copied().unwrap_or(0) ^ reference.fpga.get(&a).copied().unwrap_or(0);
                (a, if a == FILTER { (v ^ d) & !3 | 1 } else { v ^ d })
            })
            .collect();
        let switch_on = self.get(&format!("CH{}_alone", ch)).map(|s| s.afe.clone()).unwrap_or_default();
        let own = |v: &[(usize, u8, u16)]| v.iter().filter(|f| f.0 == ch).copied().collect::<Vec<_>>();
        let srate = tb.num("srate").filter(|&r| r > 0.0).ok_or("the time base's sample rate is not recorded")?;
        // the channels the stock system records for this one: CH2, CH3 or CH4 alone together with CH1 (0x4008 bits
        // 3:0 = 3, 5, 9 in an MHO98's recording, issue #491); the channel's own bit if none is recorded
        let own_bit = 1 << (ch - 1);
        let mask = tb.fpga.get(&super::snapshot::MODE).map(|m| m & 0xf).filter(|m| m & own_bit != 0).unwrap_or(own_bit);
        let normal = normal_of(self.steps(ch, "tdiv"));
        unroll(
            Plan {
                fpga,
                adc,
                afe: [own(&switch_on), own(&sc.afe)].concat(),
                select: sc.select.clone(),
                tdiv: td,
                scale: sv,
                srate,
                depth: tb.fpga.get(&super::snapshot::DEPTH).copied().unwrap_or(10_000),
                pre: tb.fpga.get(&0x4050).copied().unwrap_or(5000),
                mask,
                scales: std::array::from_fn(|c| if c + 1 == ch { sv } else { 0.0 }),
            },
            normal,
        )
    }

    /// The board's state for the channels `chans` (channel 1 to 4, scale in V/div, in channel order) at the time base
    /// nearest to `tdiv`: one channel as `plan`; several at the recorded setting of the same channels on
    /// (`combo=a,b_tdiv=T` and `CH1-4_tdiv=T` of `mho-settings.py --extended`, all at 1 V/div), each channel's
    /// scale laid over it as in `plan` and its digital gain in its slots (`slots`). Err when the file has no setting
    /// of these channels together.
    pub fn plan_many(&self, chans: &[(usize, f64)], tdiv: f64) -> Result<Plan, String> {
        match chans {
            [] => Err("no channel on".into()),
            [(ch, scale)] => self.plan(*ch, tdiv, *scale),
            _ => self.plan_together(chans, tdiv),
        }
    }

    fn plan_together(&self, chans: &[(usize, f64)], tdiv: f64) -> Result<Plan, String> {
        let list: Vec<String> = chans.iter().map(|c| c.0.to_string()).collect();
        let prefix =
            if chans.len() == 4 { "CH1-4_tdiv=".to_string() } else { format!("combo={}_tdiv=", list.join(",")) };
        let steps = self.named(&prefix);
        let normal = normal_of(steps.clone());
        let missing = || format!("CH{} together not recorded (mho-settings.py --extended)", list.join("+CH"));
        let adc =
            steps.iter().map(|x| x.1).find(|s| !s.adc[0].is_empty()).map(|s| s.adc.clone()).ok_or_else(missing)?;
        let (td, tb) = nearest(steps, tdiv).ok_or_else(missing)?;
        let bits = chans.iter().fold(0, |m, c| m | 1 << (c.0 - 1));
        let mask = tb.fpga.get(&super::snapshot::MODE).map(|m| m & 0xf).filter(|m| m & bits == bits).unwrap_or(bits);
        let mut fpga: BTreeMap<u32, u32> =
            tb.fpga.iter().filter(|(a, _)| !skipped(**a)).map(|(&a, &v)| (a, v)).collect();
        let (mut afe, mut select, mut scales) = (Vec::new(), Vec::new(), [0.0; 4]);
        let slot_set: Vec<u32> = (0..4).flat_map(slot_words).collect();
        for &(ch, scale) in chans {
            let (sv, sc) = nearest(self.steps(ch, "scale"), scale).ok_or(format!("no scale of CH{} recorded", ch))?;
            let reference = self.get(&format!("CH{}_scale=1", ch)).ok_or(format!("no CH{} at 1 V/div", ch))?;
            for (a, v) in fpga.iter_mut().filter(|(a, _)| !slot_set.contains(a)) {
                *v ^= sc.fpga.get(a).copied().unwrap_or(0) ^ reference.fpga.get(a).copied().unwrap_or(0);
            }
            // the channel's gain where it stands alone: slot 0 for CH1, slot 1 for the others (recorded with CH1)
            let own = slot_words(if ch == 1 { 0 } else { 1 });
            for k in slots(ch, mask) {
                for (dst, src) in slot_words(k).into_iter().zip(own) {
                    if let Some(&w) = sc.fpga.get(&src) {
                        fpga.insert(dst, w);
                    }
                }
            }
            let switch_on = self.get(&format!("CH{}_alone", ch)).map(|s| s.afe.clone()).unwrap_or_default();
            afe.extend(switch_on.iter().chain(&sc.afe).filter(|f| f.0 == ch));
            let line = format!("_ch{}", ch);
            let mine = |g: &u32| super::acq::afe::select_name(*g).is_some_and(|n| n.ends_with(&line));
            select.extend(sc.select.iter().filter(|(g, _)| mine(g)));
            scales[ch - 1] = sv;
        }
        if let Some(f) = fpga.get_mut(&FILTER) {
            *f = *f & !3 | 1;
        }
        let srate = tb.num("srate").filter(|&r| r > 0.0).ok_or("the time base's sample rate is not recorded")?;
        unroll(
            Plan {
                fpga: fpga.into_iter().collect(),
                adc,
                afe,
                select,
                tdiv: td,
                scale: scales[chans[0].0 - 1],
                srate,
                depth: tb.fpga.get(&super::snapshot::DEPTH).copied().unwrap_or(10_000),
                pre: tb.fpga.get(&0x4050).copied().unwrap_or(5000),
                mask,
                scales,
            },
            normal,
        )
    }

    /// The settings whose names start with `prefix` followed by a number, by that number.
    fn named(&self, prefix: &str) -> Vec<(f64, &Setting)> {
        let mut v: Vec<(f64, &Setting)> = self
            .list
            .iter()
            .filter_map(|s| s.name.strip_prefix(prefix).and_then(|x| x.parse().ok()).map(|x| (x, s)))
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(name: &str, state: &str, fpga: &[(u32, u32)], adc: bool, afe: &[(usize, u8, u16)]) -> String {
        let mut s = format!("setting {}\nscpi :X\nstate {}\n", name, state);
        for (a, v) in fpga {
            s += &format!("fpga {:#x} {:#010x}\n", a, v);
        }
        if adc {
            s += "adc 0 0x54 0x1803\nadc 1 0x55 0x0005\n";
        }
        for (c, r, d) in afe {
            s += &format!("afe {} {:#04x} {:#06x}\n", c, r, d);
        }
        s + "select c131 1\nendsetting\n"
    }

    fn file() -> String {
        let st = |t: &str, r: &str| format!("on1=0 on2=1 tdiv={} srate={} mdepth=1.0000E+04", t, r);
        [
            "people\n---- oos settings ----\nformat 1\nmodel MHO934\nhwcode 3\n".to_string(),
            setting("CH2_alone", &st("1e-6", "2e9"), &[], false, &[(2, 0x2b, 0x4c5f), (1, 0x07, 1)]),
            setting(
                "CH2_tdiv=2e-9",
                &st("2e-9", "4e9"),
                &[(0x10d0, 0x8001), (0x1098, 0), (0x10e4, 10_000), (0x1014, 0x0480_0000), (0x1210, 49_999)],
                true,
                &[],
            ),
            setting(
                "CH2_tdiv=2e-4",
                &st("2e-4", "5e6"),
                &[
                    (0x10d0, 0),
                    (0x10d8, 50),
                    (0x1098, 1),
                    (0x10e4, 10_000),
                    (0x1014, 0x0480_0000),
                    (0x1024, 7),
                    (0x4008, 0x1888_32a3),
                ],
                false,
                &[],
            ),
            setting(
                "CH2_scale=0.1",
                &st("2e-4", "5e6"),
                &[(0x1014, 0x0400_0000), (0x14d4, 0x1234)],
                false,
                &[(2, 0x07, 0x8440)],
            ),
            setting(
                "CH2_scale=1",
                &st("2e-4", "5e6"),
                &[(0x1014, 0x0480_0000), (0x14d4, 0x1000)],
                false,
                &[(2, 0x07, 0x8441)],
            ),
            "end\n".to_string(),
        ]
        .concat()
    }

    #[test]
    fn a_file_is_read_setting_by_setting() {
        let s = Settings::parse(&file()).unwrap();
        assert_eq!((s.model.as_str(), s.list.len()), ("MHO934", 5));
        assert_eq!(s.steps(2, "tdiv").iter().map(|x| x.0).collect::<Vec<_>>(), [2e-9, 2e-4]);
        assert!(Settings::parse("no mark").is_err());
        assert!(Settings::parse(&file().replace("end\n", "")).is_err());
        assert!(Settings::parse(&file().replace("format 1", "format 2")).is_err());
    }

    // Break it catches: the scale's bits lost when the time base is set, the time base's taken from the scale's
    // setting, a live counter or a status word written, another channel's frames sent, the ADCs not taken along.
    #[test]
    fn a_time_base_and_a_scale_make_one_state() {
        let s = Settings::parse(&file()).unwrap();
        let p = s.plan(2, 2.1e-4, 0.1).unwrap();
        // CH2 recorded together with CH1, as the stock system does it
        assert_eq!((p.tdiv, p.scale, p.srate, p.depth, p.mask), (2e-4, 0.1, 5e6, 10_000, 3));
        let w: BTreeMap<u32, u32> = p.fpga.iter().copied().collect();
        // the time base's sampling, the scale's bit 23 of 0x1014 cleared against 1 V/div, the scale's lane gain
        assert_eq!(w.get(&0x10d8), Some(&50));
        assert_eq!(w.get(&0x1014), Some(&0x0400_0000));
        assert_eq!(w.get(&0x1024), None, "a live word");
        assert_eq!(p.afe, [(2, 0x2b, 0x4c5f), (2, 0x07, 0x8440)]);
        assert_eq!(p.adc[0].get(&0x54), Some(&0x1803));
        assert_eq!(p.select, [(131, true)]);
        let fast = s.plan(2, 1e-9, 1.0).unwrap();
        assert_eq!((fast.tdiv, fast.srate, fast.scale), (2e-9, 4e9, 1.0));
        assert_eq!(fast.fpga.iter().find(|w| w.0 == 0x10d0), Some(&(0x10d0, 0x8001)));
        assert_eq!(fast.mask, 2, "no mode word recorded: the channel's own bit");
        assert_eq!(fast.fpga.iter().find(|w| w.0 == crate::mho::snapshot::MODE), None, "the mode word is the arm's");
        // the filter the stock system runs from 1 GSa/s on stays bypassed: OOS loads no coefficients
        assert_eq!(fast.fpga.iter().find(|w| w.0 == FILTER), Some(&(FILTER, 1)));
        assert_eq!(w.get(&FILTER), Some(&1));
        assert!(s.plan(3, 1e-3, 1.0).is_err());
    }

    // Break it catches: CH1's gain in its partner's slots, a channel of four in another's slot, one channel's gain not
    // in every slot, several channels planned without a recording of them together.
    #[test]
    fn several_channels_take_their_slots() {
        assert_eq!(
            (slots(1, 1), slots(1, 3), slots(2, 3), slots(4, 9)),
            (vec![0, 1, 2, 3], vec![0, 2], vec![1, 3], vec![1, 3])
        );
        assert_eq!((slots(2, 7), slots(3, 0xf), slots(4, 0xb)), (vec![1], vec![2], vec![3]));
        assert_eq!(slot_words(1), [0x14d4, 0x14dc, 0x1500, 0x1504]);
        let s = Settings::parse(&file()).unwrap();
        assert_eq!(s.plan_many(&[(2, 0.1)], 2e-4), s.plan(2, 2e-4, 0.1));
        assert!(s.plan_many(&[(1, 0.1), (2, 0.1)], 2e-4).unwrap_err().contains("--extended"));
        assert!(s.plan_many(&[], 2e-4).is_err());
        let p = s.plan(2, 2e-4, 0.1).unwrap();
        assert_eq!(p.gain(2), 1.0, "no gain word recorded");
        let p = Plan { fpga: vec![(0x14c8, 0x400), (0x14d4, 0x475)], ..p };
        assert!((p.gain(2) - 1141.0 / 1024.0).abs() < 1e-12 && p.gain(1) == 1.0);
    }

    // Break it catches: a slow setting recorded at its own rate (its trigger lies anywhere), the depth or the points
    // before the trigger not scaled, the segment of the shallow record kept, a rate the ADCs cannot divide to taken.
    #[test]
    fn a_slower_rate_is_recorded_at_4_gsa_s_and_thinned() {
        let s = Settings::parse(&file()).unwrap();
        // 5 MSa/s is 800 times below 4 GSa/s: more than OVERSAMPLE_MAX, recorded as it is
        let (p, k) = s.plan_fast(2, 2e-4, 0.1).unwrap();
        assert_eq!((k, p.srate), (1, 5e6));
        let t = file().replace("tdiv=2e-4 srate=5e6", "tdiv=2e-4 srate=5e8");
        let s = Settings::parse(&t).unwrap();
        let (p, k) = s.plan_fast(2, 2e-4, 0.1).unwrap();
        let w: BTreeMap<u32, u32> = p.fpga.iter().copied().collect();
        assert_eq!((k, p.srate, p.tdiv, p.depth, p.pre), (8, 4e9, 2e-4, 80_000, 40_000));
        assert_eq!(
            (w[&0x10d0], w[&0x10e4], w[&0x10e0], w[&0x4050], w[&0x4054]),
            (0x8001, 80_000, 0x800, 40_000, 40_000)
        );
        assert_eq!(w.get(&0x1014), Some(&0x0400_0000), "the scale's bits stay");
        assert_eq!((segment(10_000), segment(1_000_000)), (0x200, 0x8000));
    }

    // Break it catches: a setting the stock system recorded rolling replayed as it is (everything before the trigger,
    // the roll words), the ordinary setting's words not taken over.
    #[test]
    fn a_rolled_setting_becomes_an_ordinary_record() {
        let t = file().replace(
            "setting CH2_scale=0.1",
            "setting CH2_tdiv=5e-2\nscpi :X\nstate on1=0 on2=1 tdiv=5e-2 srate=2e6 mdepth=1.0000E+06\nfpga 0x10e4 1000000\n\
             fpga 0x4050 999999\nfpga 0x4054 1\nfpga 0x11fc 50\nfpga 0x1154 0xc3\nfpga 0x1170 0xc0000014\nselect c131 1\n\
             endsetting\nsetting CH2_scale=0.1",
        )
        .replace("fpga 0x4008 0x188832a3\n", "fpga 0x4008 0x188832a3\nfpga 0x11fc 0x3e8\nfpga 0x1154 0x4c3\nfpga 0x1170 0x40000002\n");
        let s = Settings::parse(&t).unwrap();
        let p = s.plan(2, 5e-2, 0.1).unwrap();
        let w: BTreeMap<u32, u32> = p.fpga.iter().copied().collect();
        assert_eq!((p.depth, p.pre), (1_000_000, 500_000));
        assert_eq!(
            (w[&0x4050], w[&0x4054], w[&0x11fc], w[&0x1154], w[&0x1170]),
            (500_000, 500_000, 1000, 0x4c3, 0x4000_0002)
        );
        let q = s.plan(2, 2e-4, 0.1).unwrap();
        assert_eq!(q.pre, 5000, "an ordinary setting stays as recorded");
    }

    // Break it catches: a fixed depth taken at the screen's time base's own rate, the depth words of the shallow record
    // kept, a rate no setting ran at made up.
    #[test]
    fn a_fixed_depth_takes_the_setting_that_ran_at_its_rate() {
        let s = Settings::parse(&file()).unwrap();
        let p = s.plan_deep(&[(2, 0.1)], 2e-3, 5e6, 100_000).unwrap();
        let w: BTreeMap<u32, u32> = p.fpga.iter().copied().collect();
        assert_eq!((p.srate, p.tdiv, p.depth, p.pre), (5e6, 2e-3, 100_000, 50_000));
        assert_eq!((w[&0x10d8], w[&0x10e4], w[&0x4050], w[&0x4054]), (50, 100_000, 50_000, 50_000));
        assert_eq!(w[&0x10e0], segment(100_000) * 2, "CH2 recorded with CH1");
        // a rate no setting ran at: the nearest one, the depth scaled to the same span
        let q = s.plan_deep(&[(2, 0.1)], 2e-3, 8e6, 100_000).unwrap();
        assert_eq!((q.srate, q.depth), (5e6, 62_500));
        assert!(s.plan_deep(&[(3, 0.1)], 2e-3, 5e6, 100_000).is_err(), "CH3 is not recorded");
    }
}
