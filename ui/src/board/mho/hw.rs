// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's acquisition behind the screen (`oos-ui --hw`, started by `oos-mho-live` after `oos-fpgad mho-start`
//! set the board up from the unit's own snapshot; docs/mho.md "First acquisition"). One acquisition after the other is
//! armed and read whole with the sequences of the DHO800/DHO900 (`snapshot::acquire`), decoded to volts and handed to
//! the screen as frames of the channel measured.
//!
//! With the unit's recording of the stock settings (`/oos/mho-settings.txt`, `tools/mho-settings.py`) the screen's
//! keys set the board: the first channel on, its scale and the time base go to the nearest recorded setting
//! (`settings::Settings::plan`): the FPGA's words, the ADCs' registers, the front end's words and select lines.
//! Without it the board stays at the snapshot's setting. One channel at a time; the trigger is the edge on the channel
//! measured at the screen's level and slope (`tpu::edge`, assumed to be the DHO's TPU). **Verified on the unit** that
//! the board measures at the snapshot's setting (three units, issues #4, #248, #491) and that the keys set the scale
//! and the time base up to 500 MSa/s (two MHO98, #4 and #491); the trigger and 1 GSa/s and above not yet.

use crate::board::{lock, BasicStats, Frame, HwShared, Shared};
use oos_hal::mho::afe_bus::{AfeBus, GpioAfe, Line};
use oos_hal::mho::bus::{Bar1, Bus, DmaBuf};
use oos_hal::mho::settings::{Plan, Settings};
use oos_hal::mho::{acq, snapshot, tpu};
use std::collections::BTreeMap;
use std::time::Duration;

/// The unit's snapshot and its recording of the stock settings, as `oos-mho-live` leaves them.
const SNAPSHOT: &str = "/oos/mho-snapshot.txt";
const SETTINGS: &str = "/oos/mho-settings.txt";
/// How long one acquisition may take to complete, the frames dropped after a change while the board settles (the
/// first two after the set-up came with spikes on an MHO98, issue #491), and the pause between frames.
const DONE_MS: u64 = 1000;
const SETTLE: u32 = 2;
const PERIOD_MS: u64 = 20;
/// Frames the queue holds at most: the screen takes the newest.
const QUEUE_MAX: usize = 4;
/// Points read of a record at most (a deeper record is read from its middle, around the trigger): a recording whose
/// depth the stock system set to 1 M (an MHO934's, issue #248) is read whole, as the DHO800/DHO900 reads its deep
/// records.
const POINTS_MAX: u32 = 1_000_000;

/// Starts the thread; its state is the board's `Shared`, as the DHO800/DHO900's thread keeps it.
pub(super) fn spawn() -> HwShared {
    let out: HwShared = std::sync::Arc::new(std::sync::Mutex::new(Shared::default()));
    lock(&out).status = "MHO900: starting".into();
    let shared = out.clone();
    std::thread::spawn(move || run(&shared));
    out
}

/// What the thread measures now: the channel (1 to 4), the hardware's scale in V/div, the sample interval, the
/// points read and the samples before the trigger in them, the FSM's channel mask.
struct Now {
    ch: usize,
    scale: f64,
    dt: f64,
    n: u32,
    pre: u32,
    mask: u32,
    /// every how many points of the record make the frame (`settings::plan_fast`: recorded at 4 GSa/s)
    k: u32,
}

impl Now {
    /// The board as plan `p` of channel `ch` sets it, every `k`-th point of its record making the frame.
    fn of(ch: usize, p: &Plan, k: u32) -> Now {
        Now {
            ch,
            scale: p.scale,
            dt: k as f64 / p.srate,
            n: p.depth.min(POINTS_MAX),
            pre: p.pre.min(p.depth),
            mask: p.mask,
            k,
        }
    }
}

/// The status line of a plan set.
fn set_text(ch: usize, p: &Plan, k: u32) -> String {
    let how = if k > 1 { format!("; recorded at 4 GSa/s, every {}th point", k) } else { String::new() };
    format!(
        "MHO900: CH{} at {} V/div, {} s/div, {:.3e} Sa/s (recorded settings{})",
        ch,
        p.scale,
        p.tdiv,
        p.srate / k as f64,
        how
    )
}

fn run(out: &HwShared) {
    let status = |s: String| {
        eprintln!("oos-ui mho: {}", s);
        lock(out).status = s;
    };
    let s = match std::fs::read_to_string(SNAPSHOT)
        .map_err(|e| e.to_string())
        .and_then(|t| snapshot::Snapshot::parse(&t))
    {
        Ok(s) => s,
        Err(e) => return status(format!("{}: {} (oos-mho-live)", SNAPSHOT, e)),
    };
    let settings = std::fs::read_to_string(SETTINGS).ok().map(|t| Settings::parse(&t));
    let settings = match settings {
        Some(Ok(st)) => Some(st),
        Some(Err(e)) => {
            status(format!("{}: {}; the board stays at the snapshot's setting", SETTINGS, e));
            None
        }
        None => None,
    };
    let mut bar = match Bar1::open_len(acq::WINDOW_USED) {
        Ok(b) => b,
        Err(e) => return status(format!("{}: {} (oos-fpgad mho-start first)", acq::BAR_DEVICE, e)),
    };
    let mut c2h = match std::fs::File::open(acq::C2H_DEVICE) {
        Ok(f) => f,
        Err(e) => return status(format!("{}: {}", acq::C2H_DEVICE, e)),
    };
    let mut afe = settings.as_ref().and_then(|_| GpioAfe::open().ok());
    let depth = s.fpga.get(&snapshot::DEPTH).copied().unwrap_or(4000);
    let mut now = Now {
        ch: s.channel,
        scale: s.scale,
        dt: 10.0 * s.timebase / depth as f64,
        n: depth,
        pre: depth / 2,
        mask: s.fsm_mask(),
        k: 1,
    };
    let mut sh = s.shadows();
    let mut applied: Option<super::Want> = None;
    status(format!(
        "MHO900: CH{} at {} V/div (snapshot){}",
        now.ch,
        s.scale_text,
        if settings.is_some() { "; the keys set the board" } else { "; no settings file: the keys don't" }
    ));
    let mut buf = DmaBuf::new();
    let (mut seq, mut settle) = (0u64, SETTLE);
    // the status words of the last acquisitions, how often each, in the screen's log every 50 frames
    let mut statuses: BTreeMap<u32, u32> = BTreeMap::new();
    // the TPU's shadows (the MHO900's stock 0x201c is the DHO's) and the trigger last set, with the scale it was for
    let mut tsh = tpu::Shadows::APP;
    let mut trig_set: Option<(super::Trig, usize, f64, f64)> = None;
    let mut edges = EdgeLog::default();
    // the stock system's upper bits of 0x1154 for the pass-through: the snapshot's, then each plan's
    let mut keep_1154 = s.fpga.get(&0x1154).copied().unwrap_or(0);
    loop {
        let (hold, want) = {
            let g = lock(out);
            (g.hold, g.want.clone())
        };
        if hold {
            std::thread::sleep(Duration::from_millis(PERIOD_MS));
            continue;
        }
        if let (Some(st), Some(w)) = (&settings, want) {
            if applied.as_ref().map(|a| (a.ch, a.scale, a.tdiv, a.probe)) != Some((w.ch, w.scale, w.tdiv, w.probe)) {
                if let Some(ch) = w.ch {
                    match st.plan_fast(ch, w.tdiv, w.scale) {
                        Ok((p, k)) => {
                            apply(&mut bar, afe.as_mut(), &p);
                            // the plan wrote the recorded level words (the stock level, 0 V): the screen's again
                            trig_set = None;
                            keep_1154 = p.fpga.iter().find(|w| w.0 == 0x1154).map_or(keep_1154, |w| w.1);
                            now = Now::of(ch, &p, k);
                            status(set_text(ch, &p, k));
                            settle = SETTLE;
                        }
                        Err(e) => status(format!("MHO900: CH{}: {}", ch, e)),
                    }
                }
                applied = Some(w.clone());
            }
            if let Some(ch) = w.ch {
                let key = (w.trig, ch, now.scale, w.probe);
                if trig_set != Some(key) {
                    trigger(&mut bar, &mut tsh, now.ch, now.scale, w.probe, w.trig);
                    trig_set = Some(key);
                }
            }
        }
        sh.s4008 = (sh.s4008 & !0xf) | now.mask;
        let mut win = s.window(now.n);
        win.n = now.n;
        win.start = now.pre.saturating_sub(now.n / 2);
        win.fsm = now.mask;
        let pass = snapshot::passthrough_keeping(keep_1154);
        let r = snapshot::acquire_with(
            &mut bar,
            &mut sh,
            &win,
            DONE_MS,
            &mut |ms| std::thread::sleep(Duration::from_millis(ms)),
            &pass,
        );
        let a = match r {
            Ok(a) => a,
            Err(a) => {
                status(format!("MHO900: no acquisition within {} ms (status {:#x})", a.ms, a.status));
                continue;
            }
        };
        // which status words the acquisitions ended with (0x4034 bit 6, the DHO's "trigger fired", was set on every
        // one of an MHO98's, issue #491: another bit has to tell the trigger from the auto trigger's timeout)
        tally(&mut statuses, a.status, seq);
        let words: Vec<u16> = match buf.read(&mut c2h, 2 * win.n as usize, |_, _| {}) {
            Ok(b) => b.as_chunks::<2>().0.iter().map(|w| u16::from_le_bytes(*w)).collect(),
            Err(e) => {
                status(format!("MHO900: read-out: {}", e));
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        seq += 1;
        if settle > 0 {
            settle -= 1;
            continue;
        }
        let probe = applied.as_ref().map_or(1.0, |w| w.probe);
        let (words, pre) = thin(&words, now.pre - win.start, now.k);
        let mut f = frame(&words, now.ch - 1, now.scale, probe, now.dt, pre);
        if let Some((t, ..)) = trig_set {
            edges.align(&mut f, now.ch - 1, t, now.scale * probe, seq);
        }
        publish(out, f, seq);
        std::thread::sleep(Duration::from_millis(PERIOD_MS));
    }
}

/// Sets the board to a plan: the ADCs' registers that read otherwise, the FPGA's words, the front end's words and
/// select lines (the MHO900's channel n on the bus's chip select 4 - n).
fn apply(bar: &mut Bar1, afe: Option<&mut GpioAfe>, p: &Plan) {
    let wait = || std::thread::sleep(Duration::from_micros(acq::ADC_SPI_WAIT_US));
    for (n, regs) in p.adc.iter().enumerate() {
        for (&r, &v) in regs {
            let read = |bar: &mut Bar1| {
                for (off, w) in acq::adc_read(n as u32, r as u32).into_iter().flatten() {
                    bar.wr(off, w);
                    wait();
                }
                let x = bar.rd(acq::ADC_SPI_READ);
                (x & acq::ADC_SPI_VALID != 0).then_some(x as u16)
            };
            if read(bar) != Some(v) {
                for (off, w) in acq::adc_write(n as u32, r as u32, v).into_iter().flatten() {
                    bar.wr(off, w);
                    wait();
                }
            }
        }
    }
    for &(a, v) in &p.fpga {
        bar.wr(a, v);
    }
    if let Some(afe) = afe {
        for &(c, r, d) in &p.afe {
            afe.write(4 - c, r, d);
        }
    }
    for &(g, v) in &p.select {
        if let Some(name) = acq::afe::select_name(g) {
            if let Err(e) = Line::output(&name, v) {
                eprintln!("oos-ui mho: {}: {}", name, e);
            }
        }
    }
}

/// Counts the status word an acquisition ended with; every 50 frames the counts go to the screen's log.
fn tally(statuses: &mut BTreeMap<u32, u32>, status: u32, seq: u64) {
    *statuses.entry(status).or_default() += 1;
    if seq % 50 == 49 {
        let list: Vec<String> = statuses.iter().map(|(s, n)| format!("{:#010x} x{}", s, n)).collect();
        eprintln!("oos-ui mho: status of the last 50 acquisitions: {}", list.join(", "));
        statuses.clear();
    }
}

/// Hands a frame to the screen: the newest in the queue, the oldest dropped beyond `QUEUE_MAX`.
fn publish(out: &HwShared, mut f: Frame, seq: u64) {
    f.seq = seq;
    let mut g = lock(out);
    f.stamp = g.stamp;
    g.decoded += 1;
    g.queue.push_back(f.clone());
    while g.queue.len() > QUEUE_MAX {
        g.queue.pop_front();
        g.dropped += 1;
    }
    g.frame = f;
}

/// Where the trigger's edge lies in the frames against the record's trigger point, in the screen's log every 50
/// frames. From 500 MSa/s on the edge was not at the trigger point although the board's trigger fired (an MHO98,
/// mho-dev-23, issue #491): until that offset is known, the screen's trigger point is put on the edge nearest to it
/// (a software trigger on the board's frames; a frame without such an edge keeps the record's trigger point).
#[derive(Default)]
struct EdgeLog {
    offsets: Vec<i64>,
    missing: u32,
}

impl EdgeLog {
    fn align(&mut self, f: &mut Frame, ch: usize, t: super::Trig, scale_tip: f64, seq: u64) {
        let h = (0.3 * scale_tip) as f32;
        let e = edge_near(&f.ch[ch], f.pre, t.level as f32, h, t.slope);
        match e {
            Some(i) => {
                self.offsets.push(i as i64 - f.pre as i64);
                f.pre = i;
            }
            None => self.missing += 1,
        }
        if seq % 50 == 49 {
            self.offsets.sort_unstable();
            let at = |k: usize| self.offsets.get(k).copied().unwrap_or(0);
            let n = self.offsets.len();
            eprintln!(
                "oos-ui mho: edge against the trigger point, last 50 frames: {} found (min {}, median {}, max {} \
                 samples of {:.3e} s), {} without",
                n,
                at(0),
                at(n / 2),
                at(n.saturating_sub(1)),
                f.dt,
                self.missing
            );
            *self = EdgeLog::default();
        }
    }
}

/// The crossing of `level` in the slope's direction nearest to sample `pre`: a crossing counts after the signal was
/// `h` beyond the level on the other side (the hysteresis of the board's trigger).
fn edge_near(v: &[f32], pre: usize, level: f32, h: f32, slope: tpu::Slope) -> Option<usize> {
    let (rise, fall) = match slope {
        tpu::Slope::Rising => (true, false),
        tpu::Slope::Falling => (false, true),
        tpu::Slope::Either => (true, true),
    };
    let (mut below, mut above) = (false, false);
    let mut best: Option<usize> = None;
    for (i, &x) in v.iter().enumerate() {
        let hit = (rise && below && x >= level) || (fall && above && x <= level);
        if hit && best.is_none_or(|b| i.abs_diff(pre) < b.abs_diff(pre)) {
            best = Some(i);
        }
        if x >= level {
            below = false;
        }
        if x <= level {
            above = false;
        }
        below |= x < level - h;
        above |= x > level + h;
    }
    best
}

/// The edge trigger on channel `ch` at the screen's level (V at the probe tip, `probe` the ratio) for the hardware's
/// `scale` (V/div at the BNC): the DHO800/DHO900's sequence (`tpu::edge`), hysteresis by the scale as there, holdoff
/// the shortest. The auto trigger's timeout stays the stock system's.
fn trigger(bar: &mut Bar1, sh: &mut tpu::Shadows, ch: usize, scale: f64, probe: f64, t: super::Trig) {
    let nv = |v: f64| v * 1e9;
    let level = nv(t.level / probe.max(1e-9));
    let noise = tpu::noise_hysteresis(nv(scale), false);
    for (a, v) in tpu::edge(sh, ch as u32, t.slope, level, 0.0, nv(scale), 1.0, noise, 8_000_000) {
        bar.wr(a, v);
    }
    eprintln!("oos-ui mho: trigger: CH{} {:?} at {} V ({} V at the BNC)", ch, t.slope, t.level, t.level / probe);
}

/// Every `k`-th point of a record whose trigger is at `pre`, the trigger among them: the points and the trigger's index
/// in them.
fn thin(words: &[u16], pre: u32, k: u32) -> (Vec<u16>, u32) {
    let (pre, k) = (pre as usize, k.max(1) as usize);
    let first = pre % k;
    (words.iter().skip(first).step_by(k).copied().collect(), ((pre - first) / k) as u32)
}

/// A frame of the read-out `words` of channel `ch` (0 to 3) at `scale` V/div: volts as the stock system decodes them
/// (section 5) times the probe's ratio, the trigger at sample `pre` of the window.
fn frame(words: &[u16], ch: usize, scale: f64, probe: f64, dt: f64, pre: u32) -> Frame {
    let v: Vec<f32> = words.iter().map(|&w| (acq::volts(w, scale, 0.0) * probe) as f32).collect();
    let mut f = Frame { dt, pre: pre as usize, acq: (1.0 / dt, words.len()), ..Frame::default() };
    f.ch[ch] = v;
    f.basic = std::array::from_fn(|c| BasicStats::of(&f.ch[c]));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the trace on another channel than the one measured, volts off by the scale or the probe,
    // the trigger point off.
    #[test]
    fn a_frame_holds_the_channel_in_volts_at_the_probe_tip() {
        let code = |v: f64| (32768.0 + v / 0.1 * 7500.0) as u16;
        let words: Vec<u16> = (0..10_000).map(|i| code(if i < 5000 { 0.0 } else { 0.3 })).collect();
        let f = frame(&words, 1, 0.1, 10.0, 2e-7, 5000);
        assert!(f.ch[0].is_empty() && f.ch[2].is_empty() && f.ch[3].is_empty());
        assert_eq!(f.ch[1].len(), 10_000);
        assert!((f.ch[1][0] - 0.0).abs() < 1e-3 && (f.ch[1][9999] - 3.0).abs() < 1e-3);
        assert_eq!((f.pre, f.dt, f.acq.1), (5000, 2e-7, 10_000));
    }

    // Break it catches: the trigger's point dropped by the thinning, the trigger's index off by the points before it.
    #[test]
    fn thinning_keeps_the_trigger_point() {
        let w: Vec<u16> = (0..80).collect();
        assert_eq!(thin(&w, 43, 8), ((3..80).step_by(8).collect::<Vec<u16>>(), 5));
        assert_eq!(thin(&w, 43, 1), (w.clone(), 43));
    }

    // Break it catches: the edge taken in the wrong direction, a noise wiggle around the level taken for an edge, the
    // crossing farther from the trigger point taken.
    #[test]
    fn the_edge_nearest_the_trigger_point_is_found_in_its_direction() {
        // low 0..100, high 100..300, low 300..500, high 500..; noise around the level on the rising edge at 100
        let mut v: Vec<f32> = (0..600).map(|i| if (100..300).contains(&i) || i >= 500 { 0.3 } else { 0.0 }).collect();
        v[100..104].copy_from_slice(&[0.16, 0.14, 0.16, 0.14]);
        let r = |pre| edge_near(&v, pre, 0.15, 0.03, tpu::Slope::Rising);
        assert_eq!((r(103), r(450), r(0)), (Some(100), Some(500), Some(100)));
        assert_eq!(edge_near(&v, 250, 0.15, 0.03, tpu::Slope::Falling), Some(300));
        assert_eq!(edge_near(&v, 280, 0.15, 0.03, tpu::Slope::Either), Some(300));
        assert_eq!(edge_near(&[0.3; 50], 25, 0.15, 0.03, tpu::Slope::Rising), None);
    }
}
