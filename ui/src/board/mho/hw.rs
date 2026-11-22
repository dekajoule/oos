// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The MHO900's acquisition behind the screen (`oos-ui --hw`, started by `oos-mho-live` after `oos-fpgad mho-start`
//! set the board up from the unit's own snapshot; docs/mho.md "First acquisition"). One acquisition after the other is
//! armed and read whole with the sequences of the DHO800/DHO900 (`snapshot::acquire`), decoded to volts at the
//! snapshot's scale (docs/protocols/mho-acquisition.md, section 5) and handed to the screen as frames of the
//! snapshot's channel, so the trace sits on its own channel in volts. The board measures at the snapshot's setting
//! only: the screen's scale, time base and trigger keys do not set it yet. **Verified on the unit** that the board
//! measures there (two MHO98, issues #4 and #491); this thread not yet.

use crate::board::{lock, BasicStats, Frame, HwShared, Shared};
use oos_hal::mho::bus::{Bar1, DmaBuf};
use oos_hal::mho::{acq, snapshot};
use std::time::Duration;

/// The unit's snapshot, as `oos-mho-live` leaves it.
const SNAPSHOT: &str = "/oos/mho-snapshot.txt";
/// How long one acquisition may take to complete, the frames dropped after the start while the board settles (the
/// first two after the set-up came with spikes on an MHO98, issue #491), and the pause between frames.
const DONE_MS: u64 = 1000;
const SETTLE: u64 = 2;
const PERIOD_MS: u64 = 20;
/// Frames the queue holds at most: the screen takes the newest.
const QUEUE_MAX: usize = 4;

/// Starts the thread; its state is the board's `Shared`, as the DHO800/DHO900's thread keeps it.
pub(super) fn spawn() -> HwShared {
    let out: HwShared = std::sync::Arc::new(std::sync::Mutex::new(Shared::default()));
    lock(&out).status = "MHO900: starting".into();
    let shared = out.clone();
    std::thread::spawn(move || run(&shared));
    out
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
    let mut bar = match Bar1::open_len(acq::WINDOW_USED) {
        Ok(b) => b,
        Err(e) => return status(format!("{}: {} (oos-fpgad mho-start first)", acq::BAR_DEVICE, e)),
    };
    let mut c2h = match std::fs::File::open(acq::C2H_DEVICE) {
        Ok(f) => f,
        Err(e) => return status(format!("{}: {}", acq::C2H_DEVICE, e)),
    };
    // the whole record: the window from its first point, the trigger in its middle at zero delay
    let depth = s.fpga.get(&snapshot::DEPTH).copied().unwrap_or(4000);
    let (mut sh, win) = (s.shadows(), s.window(depth));
    let dt = 10.0 * s.timebase / win.n as f64;
    let ch = s.channel - 1;
    status(format!("MHO900: CH{} at {} V/div, {} points of {:.3e} s", s.channel, s.scale_text, win.n, dt));
    let mut buf = DmaBuf::new();
    let mut seq = 0u64;
    loop {
        if lock(out).hold {
            std::thread::sleep(Duration::from_millis(PERIOD_MS));
            continue;
        }
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
        if seq <= SETTLE {
            continue;
        }
        let mut f = frame(&words, ch, s.scale, dt, win.start, depth);
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

/// A frame of the read-out `words` of channel `ch` (0 to 3) at `scale` V/div: volts as the stock system decodes them
/// (section 5), the trigger in the record's middle counted from the window's first point.
fn frame(words: &[u16], ch: usize, scale: f64, dt: f64, start: u32, depth: u32) -> Frame {
    let v: Vec<f32> = words.iter().map(|&w| acq::volts(w, scale, 0.0) as f32).collect();
    let mut f = Frame {
        dt,
        pre: (depth / 2).saturating_sub(start) as usize,
        acq: (1.0 / dt, depth as usize),
        ..Frame::default()
    };
    f.ch[ch] = v;
    f.basic = std::array::from_fn(|c| BasicStats::of(&f.ch[c]));
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: the trace on another channel than the snapshot's, volts off by the scale, the trigger point
    // off the record's middle.
    #[test]
    fn a_frame_holds_the_channel_in_volts_with_the_trigger_in_the_middle() {
        let code = |v: f64| (32768.0 + v / 0.1 * 7500.0) as u16;
        let words: Vec<u16> = (0..10_000).map(|i| code(if i < 5000 { 0.0 } else { 0.3 })).collect();
        let f = frame(&words, 1, 0.1, 2e-7, 0, 10_000);
        assert!(f.ch[0].is_empty() && f.ch[2].is_empty() && f.ch[3].is_empty());
        assert_eq!(f.ch[1].len(), 10_000);
        assert!((f.ch[1][0] - 0.0).abs() < 1e-4 && (f.ch[1][9999] - 0.3).abs() < 1e-4);
        assert_eq!((f.pre, f.dt, f.acq.1), (5000, 2e-7, 10_000));
    }
}
