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
//! Without it the board stays at the snapshot's setting. With a recording of `--extended` several channels at once
//! (`plan_many`) and each channel's offset, AC coupling and bandwidth limit (`vertical`); GND shows 0 V. The trigger
//! is the edge on the trigger's channel (the first one shown when it is off) at the screen's level and slope
//! (`tpu::edge`, the DHO's TPU, verified on two MHO98). **Verified on the unit** that
//! the board measures at the snapshot's setting (three units, issues #4, #248, #491) and that the keys set the scale
//! and the time base up to 500 MSa/s (two MHO98, #4 and #491); the trigger and 1 GSa/s and above not yet.

use crate::board::{lock, BasicStats, Frame, HwShared, Shared};
use oos_hal::mho::afe_bus::{AfeBus, GpioAfe, Line};
use oos_hal::mho::bus::{Bar1, Bus, DmaBuf};
use oos_hal::mho::settings::{Plan, Settings};
use oos_hal::mho::{acq, record, snapshot, tpu, vertical};
use std::collections::BTreeMap;
use std::time::Duration;

/// The unit's snapshot and its recording of the stock settings, as `oos-mho-live` leaves them.
const SNAPSHOT: &str = "/oos/mho-snapshot.txt";
const SETTINGS: &str = "/oos/mho-settings.txt";
/// With this file there, the time bases up to 20 us/div record at 4 GSa/s, thinned (`settings::plan_fast`), instead of
/// at their own rate (`settings::plan`). On an MHO98 (mho-dev-26, issue #491) their own rate held the trigger better
/// (the edge at the trigger point at every rate, 100 ns/div to 200 us/div), so it is the default.
const FAST: &str = "/oos/mho-fast";
/// With this file there, 0x1000's bit 7 is kept set as the stock system keeps it (`snapshot::S1000`, mho-dev-28),
/// and with the other no ADC sync is pulsed after a setting: to compare on a unit without another build.
const S1000_STOCK: &str = "/oos/mho-s1000-stock";
const NO_ADC_SYNC: &str = "/oos/mho-no-adc-sync";
/// How long one acquisition may take to complete at least (`Now::done_ms` adds twice the record's span: a 1 s limit
/// left 50 and 100 ms/div broken on an MHO98, issue #4), the frames dropped after a change while the board settles (the
/// first two after the set-up came with spikes on an MHO98, issue #491), and the pause between frames.
const DONE_MS: u64 = 1000;
const SETTLE: u32 = 2;
const PERIOD_MS: u64 = 20;
/// The SPU's slope bit in 0x1004.
const SLOPE_1004: u32 = 1 << 14;
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

/// What the thread measures now: the channels shown, the sample interval, the points read of each channel's record
/// and the samples before the trigger in them, the FSM's channel mask.
struct Now {
    shown: Vec<Shown>,
    dt: f64,
    n: u32,
    pre: u32,
    mask: u32,
    /// every how many points of the record make the frame (`settings::plan_fast`: recorded at 4 GSa/s)
    k: u32,
}

/// A channel shown: its number (1 to 4), the hardware's scale in V/div at the BNC, the probe's ratio, the offset at the
/// BNC, GND coupling, the read path's digital gain (the trigger level's conversion, `Plan::gain`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shown {
    ch: usize,
    scale: f64,
    probe: f64,
    offset: f64,
    gnd: bool,
    gain: f64,
}

impl Now {
    /// The board as plan `p` for the channels `chans` sets it, every `k`-th point of its record making the frame.
    fn of(chans: &[super::Chan], p: &Plan, k: u32) -> Now {
        let shown = chans
            .iter()
            .map(|c| Shown {
                ch: c.ch,
                scale: p.scales[c.ch - 1],
                probe: c.probe,
                offset: c.vertical.offset,
                gnd: c.gnd,
                gain: p.gain(c.ch),
            })
            .collect();
        Now { shown, dt: k as f64 / p.srate, n: p.depth.min(POINTS_MAX), pre: p.pre.min(p.depth), mask: p.mask, k }
    }

    /// How long an acquisition may take: `DONE_MS` and twice the record's span (the points before the trigger, the wait
    /// for it, the points after it).
    fn done_ms(&self) -> u64 {
        DONE_MS + (2000.0 * self.n as f64 * self.dt / self.k.max(1) as f64) as u64
    }

    /// The channel the trigger is on: `ch` when it is shown, else the first one shown.
    fn trigger_on(&self, ch: usize) -> Option<Shown> {
        self.shown.iter().find(|c| c.ch == ch).or(self.shown.first()).copied()
    }

    /// The read-out of the record: the points around the trigger of every channel recorded (`mask`), interleaved.
    fn window(&self, s: &snapshot::Snapshot) -> record::Window {
        let ratio = record::ratio(self.mask);
        let mut win = s.window(self.n);
        (win.n, win.start, win.ratio, win.fsm) = (self.n * ratio, self.start() * ratio, ratio, self.mask);
        win
    }

    /// The first point read of the channel's record.
    fn start(&self) -> u32 {
        self.pre.saturating_sub(self.n / 2)
    }
}

/// The status line of a plan set.
fn set_text(p: &Plan, k: u32) -> String {
    let how = if k > 1 { format!("; recorded at 4 GSa/s, every {}th point", k) } else { String::new() };
    let chans: Vec<String> =
        (0..4).filter(|&c| p.scales[c] > 0.0).map(|c| format!("CH{} at {} V/div", c + 1, p.scales[c])).collect();
    format!(
        "MHO900: {}, {} s/div, {:.3e} Sa/s (recorded settings{})",
        chans.join(", "),
        p.tdiv,
        p.srate / k as f64,
        how
    )
}

/// The plan for what the screen asks, with each channel's vertical settings laid over it; what could not be set goes
/// to the log.
fn plan_for(st: &Settings, w: &super::Want, fast: bool) -> Result<(Plan, u32), String> {
    let chans: Vec<(usize, f64)> = w.chans.iter().map(|c| (c.ch, c.scale)).collect();
    let (mut p, k) = match chans.as_slice() {
        [(ch, scale)] if fast => st.plan_fast(*ch, w.tdiv, *scale)?,
        _ => (st.plan_many(&chans, w.tdiv)?, 1),
    };
    for c in &w.chans {
        let scale = p.scales[c.ch - 1];
        for m in vertical::apply(st, &mut p, c.ch, scale, c.vertical) {
            eprintln!("oos-ui mho: {}", m);
        }
        if c.fifty {
            eprintln!("oos-ui mho: CH{}: 50 ohm is not set yet, the input stays at 1 Mohm", c.ch);
        }
    }
    Ok((p, k))
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
    let fast = std::path::Path::new(FAST).exists();
    let depth = s.fpga.get(&snapshot::DEPTH).copied().unwrap_or(4000);
    let mut now = Now {
        shown: vec![Shown { ch: s.channel, scale: s.scale, probe: 1.0, offset: 0.0, gnd: false, gain: 1.0 }],
        dt: 10.0 * s.timebase / depth as f64,
        n: depth,
        pre: depth / 2,
        mask: s.fsm_mask(),
        k: 1,
    };
    let mut sh = s.shadows();
    if std::path::Path::new(S1000_STOCK).exists() {
        sh.s1000 = snapshot::S1000;
    }
    let adc_sync = !std::path::Path::new(NO_ADC_SYNC).exists();
    let mut applied: Option<super::Want> = None;
    status(format!(
        "MHO900: CH{} at {} V/div (snapshot){}",
        s.channel,
        s.scale_text,
        if settings.is_some() { "; the keys set the board" } else { "; no settings file: the keys don't" }
    ));
    let mut buf = DmaBuf::new();
    let (mut seq, mut settle) = (0u64, SETTLE);
    // the status words of the last acquisitions, how often each, in the screen's log every 50 frames
    let mut statuses: BTreeMap<u32, u32> = BTreeMap::new();
    // the TPU's shadows (the MHO900's stock 0x201c is the DHO's) and the trigger last set, with the scale it was for
    let mut tsh = tpu::Shadows::APP;
    let mut trig_set: Option<(super::Trig, Shown)> = None;
    let mut edges = EdgeLog::default();
    // the stock system's upper bits of 0x1154 for the pass-through: the snapshot's, then each plan's
    let mut keep_1154 = s.fpga.get(&0x1154).copied().unwrap_or(0);
    // the plan's 0x1004, whose bit 14 the trigger sets with its slope
    let mut keep_1004 = None;
    // after a setting, the levels of every channel recorded go to the log once (which word holds which channel)
    let mut show = false;
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
            if applied.as_ref().map(|a| (&a.chans, a.tdiv)) != Some((&w.chans, w.tdiv)) {
                if !w.chans.is_empty() {
                    match plan_for(st, &w, fast) {
                        Ok((p, k)) => {
                            apply(&mut bar, afe.as_mut(), &p, sh.s1000, adc_sync);
                            // the plan wrote the recorded level words (the stock level, 0 V): the screen's again
                            trig_set = None;
                            keep_1154 = p.fpga.iter().find(|w| w.0 == 0x1154).map_or(keep_1154, |w| w.1);
                            keep_1004 = p.fpga.iter().find(|w| w.0 == 0x1004).map(|w| w.1);
                            now = Now::of(&w.chans, &p, k);
                            status(set_text(&p, k));
                            (settle, show) = (SETTLE, true);
                        }
                        Err(e) => status(format!("MHO900: {}", e)),
                    }
                }
                applied = Some(w.clone());
            }
            if let Some(c) = now.trigger_on(w.trig.ch) {
                if trig_set != Some((w.trig, c)) {
                    trigger(&mut bar, &mut tsh, c, w.trig, keep_1004);
                    trig_set = Some((w.trig, c));
                }
            }
        }
        sh.s4008 = (sh.s4008 & !0xf) | now.mask;
        let win = now.window(&s);
        let pass = snapshot::passthrough_keeping(keep_1154);
        let r = snapshot::acquire_with(
            &mut bar,
            &mut sh,
            &win,
            now.done_ms(),
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
        let bytes = match buf.read(&mut c2h, 2 * win.n as usize, |_, _| {}) {
            Ok(b) => b,
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
        let words = split(bytes, now.mask, now.shown[0].scale, std::mem::take(&mut show));
        let mut f = frame(&words, &now);
        if let Some((t, c)) = trig_set {
            edges.align(&mut f, c.ch - 1, t, c.scale * c.probe, seq);
        }
        publish(out, f, seq);
        std::thread::sleep(Duration::from_millis(PERIOD_MS));
    }
}

/// Sets the board to a plan: the ADCs' registers that read otherwise, the FPGA's words, the front end's words and
/// select lines (the MHO900's channel n on the bus's chip select 4 - n).
fn apply(bar: &mut Bar1, afe: Option<&mut GpioAfe>, p: &Plan, s1000: u32, adc_sync: bool) {
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
    // the pre-processing reset of the DHO800/DHO900's horizontal set-up (0x1000 bit 5, horizontal-config.md section
    // 4.1 steps 13-14) before the words and again after them: once the board ran at 4 GSa/s, the trigger and the record
    // of a slower rate no longer met without it (an MHO98, mho-dev-24, issue #491). **Assumed** to be what resyncs them.
    let reset = |bar: &mut Bar1| {
        bar.wr(0x1000, s1000 | 0x20);
        bar.wr(0x1000, s1000 & !0x20);
    };
    reset(bar);
    for &(a, v) in &p.fpga {
        bar.wr(a, v);
    }
    reset(bar);
    // the ADC sync of the DHO800/DHO900's cold boot (0x1000 bit 1: clear, set, clear; cold-boot.md, `spu_adc_sync`),
    // which the MHO900's start never pulsed: whether the slower rates met the trigger changed from one start of the
    // board to the next (an MHO98, mho-dev-24 to -28, issue #491), as a phase that settles at random would. **Assumed**
    // to fix that phase. `NO_ADC_SYNC` leaves it out.
    if adc_sync {
        for v in [s1000 & !2, s1000 | 2, s1000 & !2] {
            bar.wr(0x1000, v);
        }
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

/// The words of each channel from a read-out of the channels in `mask`, interleaved as on the DHO800/DHO900 (verified
/// on an MHO98 for CH2, CH3 and CH4 recorded with CH1, mho-dev-30, issue #4). With `show`, the levels of each channel
/// at the first shown channel's `scale` go to the screen's log once after a setting.
fn split(bytes: &[u8], mask: u32, scale: f64, show: bool) -> [Vec<u16>; 4] {
    let all = record::deinterleave(bytes, mask);
    if show && record::ratio(mask) > 1 {
        let text: Vec<String> = (0..4)
            .filter_map(|c| snapshot::Levels::of(&all[c], scale).map(|l| (c, l)))
            .map(|(c, l)| format!("CH{} {:.3} V..{:.3} V, mean {:.3} V", c + 1, l.min, l.max, l.mean))
            .collect();
        eprintln!("oos-ui mho: record of the channels {:#x} at {} V/div: {}", mask, scale, text.join("; "));
    }
    all
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
    /// frames without the set slope's edge but with the other one within 4 points of the trigger point
    other: u32,
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
            None => {
                self.missing += 1;
                let flip = match t.slope {
                    tpu::Slope::Rising => tpu::Slope::Falling,
                    _ => tpu::Slope::Rising,
                };
                let o = edge_near(&f.ch[ch], f.pre, t.level as f32, h, flip);
                self.other += o.is_some_and(|i| i.abs_diff(f.pre) <= 4) as u32;
            }
        }
        if seq % 50 == 49 {
            self.offsets.sort_unstable();
            let at = |k: usize| self.offsets.get(k).copied().unwrap_or(0);
            let n = self.offsets.len();
            eprintln!(
                "oos-ui mho: edge against the trigger point, last 50 frames: {} found (min {}, median {}, max {} \
                 samples of {:.3e} s), {} without ({} of them with the other slope's edge at the trigger point)",
                n,
                at(0),
                at(n / 2),
                at(n.saturating_sub(1)),
                f.dt,
                self.missing,
                self.other
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

/// The edge trigger on channel `c` at the screen's level (V at the probe tip) for the hardware's scale and offset (at
/// the BNC): the DHO800/DHO900's sequence (`tpu::edge`), the level converted with the channel's digital gain as the
/// stock system does it (with 1.0 the comparator sat 17 mV above a 0.15 V level at 100 mV/div, an MHO98, issue #4),
/// hysteresis by the scale as there, holdoff the shortest. The auto trigger's timeout stays the stock system's. The
/// SPU's 0x1004 bit 14 (`s1004`, the plan's word) follows the slope as the stock system sets it: 1 rising, 0 falling
/// (an MHO98, issue #4).
fn trigger(bar: &mut Bar1, sh: &mut tpu::Shadows, c: Shown, t: super::Trig, s1004: Option<u32>) {
    if let Some(v) = s1004 {
        bar.wr(0x1004, if t.slope == tpu::Slope::Falling { v & !SLOPE_1004 } else { v | SLOPE_1004 });
    }
    let nv = |v: f64| v * 1e9;
    let level = t.level / c.probe;
    let noise = tpu::noise_hysteresis(nv(c.scale), false);
    for (a, v) in tpu::edge(sh, c.ch as u32, t.slope, nv(level), nv(c.offset), nv(c.scale), c.gain, noise, 8_000_000) {
        bar.wr(a, v);
    }
    eprintln!("oos-ui mho: trigger: CH{} {:?} at {} V ({} V at the BNC)", c.ch, t.slope, t.level, level);
}

/// Every `k`-th point of a record whose trigger is at `pre`, the trigger among them: the points and the trigger's index
/// in them.
fn thin(words: &[u16], pre: u32, k: u32) -> (Vec<u16>, u32) {
    let (pre, k) = (pre as usize, k.max(1) as usize);
    let first = pre % k;
    (words.iter().skip(first).step_by(k).copied().collect(), ((pre - first) / k) as u32)
}

/// A frame of the channels shown from their read-out `words`: volts as the stock system decodes them (section 5, the
/// screen's offset at mid-code) times the probe's ratio, 0 V with GND coupling, every `k`-th point, the trigger at its
/// sample of the window.
fn frame(words: &[Vec<u16>; 4], now: &Now) -> Frame {
    let mut f = Frame { dt: now.dt, ..Frame::default() };
    for c in &now.shown {
        let (w, pre) = thin(&words[c.ch - 1], now.pre - now.start(), now.k);
        let volt = |x: u16| if c.gnd { 0.0 } else { (acq::volts(x, c.scale, c.offset) * c.probe) as f32 };
        f.ch[c.ch - 1] = w.iter().map(|&x| volt(x)).collect();
        (f.pre, f.acq) = (pre as usize, (1.0 / now.dt, w.len()));
    }
    f.basic = std::array::from_fn(|c| BasicStats::of(&f.ch[c]));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the trace on another channel than the one measured, volts off by the scale or the probe,
    // the trigger point off.
    #[test]
    fn a_frame_holds_the_channels_in_volts_at_the_probe_tip() {
        // the board at -0.2 V offset: 0 V reads 0.2 V above mid-code
        let code = |v: f64| (32768.0 + (v - 0.2) / 0.1 * 7500.0) as u16;
        let words: [Vec<u16>; 4] = [
            vec![],
            (0..10_000).map(|i| code(if i < 5000 { 0.0 } else { 0.3 })).collect(),
            vec![],
            vec![32768; 10_000],
        ];
        let shown = |ch, gnd| Shown { ch, scale: 0.1, probe: 10.0, offset: -0.2, gnd, gain: 1.1 };
        let now = Now { shown: vec![shown(2, false), shown(4, true)], dt: 2e-7, n: 10_000, pre: 5000, mask: 0xb, k: 1 };
        let f = frame(&words, &now);
        assert!(f.ch[0].is_empty() && f.ch[2].is_empty());
        assert_eq!((f.ch[1].len(), f.ch[3].len()), (10_000, 10_000));
        assert!((f.ch[1][0] - 0.0).abs() < 1e-3 && (f.ch[1][9999] - 3.0).abs() < 1e-3);
        assert!(f.ch[3].iter().all(|&v| v == 0.0), "GND reads 0 V");
        assert_eq!((f.pre, f.dt, f.acq.1), (5000, 2e-7, 10_000));
    }

    // Break it catches: CH1's words taken for CH2 when both are recorded, a single channel's words split.
    #[test]
    fn a_channel_recorded_with_ch1_is_taken_from_the_odd_words() {
        let bytes: Vec<u8> = [1u16, 2, 1, 2, 1, 2].iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(split(&bytes, 3, 1.0, false)[1], [2, 2, 2]);
        assert_eq!(split(&bytes, 9, 1.0, false)[0], [1, 1, 1]);
        assert_eq!(split(&bytes, 1, 1.0, false)[0], [1, 2, 1, 2, 1, 2]);
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
