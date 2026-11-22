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
//! Without it the board stays at the snapshot's setting. One channel at a time. **Verified on the unit** that the
//! board measures at the snapshot's setting (three units, issues #4, #248, #491); the keys not yet.

use crate::board::{lock, BasicStats, Frame, HwShared, Shared};
use oos_hal::mho::afe_bus::{AfeBus, GpioAfe, Line};
use oos_hal::mho::bus::{Bar1, Bus, DmaBuf};
use oos_hal::mho::settings::{Plan, Settings};
use oos_hal::mho::{acq, snapshot};
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
            if applied.as_ref() != Some(&w) {
                if let Some(ch) = w.ch {
                    match st.plan(ch, w.tdiv, w.scale) {
                        Ok(p) => {
                            apply(&mut bar, afe.as_mut(), &p);
                            now = Now {
                                ch,
                                scale: p.scale,
                                dt: 1.0 / p.srate,
                                n: p.depth.min(POINTS_MAX),
                                pre: p.pre.min(p.depth),
                                mask: p.mask,
                            };
                            status(format!(
                                "MHO900: CH{} at {} V/div, {} s/div, {:.3e} Sa/s (recorded settings)",
                                ch, p.scale, p.tdiv, p.srate
                            ));
                            settle = SETTLE;
                        }
                        Err(e) => status(format!("MHO900: CH{}: {}", ch, e)),
                    }
                }
                applied = Some(w);
            }
        }
        sh.s4008 = (sh.s4008 & !0xf) | now.mask;
        let mut win = s.window(now.n);
        win.n = now.n;
        win.start = now.pre.saturating_sub(now.n / 2);
        win.fsm = now.mask;
        let r = snapshot::acquire(&mut bar, &mut sh, &win, DONE_MS, &mut |ms| {
            std::thread::sleep(Duration::from_millis(ms))
        });
        if let Err(a) = r {
            status(format!("MHO900: no acquisition within {} ms (status {:#x})", a.ms, a.status));
            continue;
        }
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
        let mut f = frame(&words, now.ch - 1, now.scale, probe, now.dt, now.pre - win.start);
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
        drop(g);
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
}
