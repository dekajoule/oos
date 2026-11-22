// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The first acquisition on the screen (docs/mho.md, "First acquisition"): `oos-ui --fpga-live --snapshot FILE`,
//! after `oos-fpgad mho-start` brought the board up and set it from the unit's own snapshot of the stock system. One
//! acquisition after the other is armed and read with the sequences of the DHO800/DHO900 (`snapshot::acquire`), and
//! each frame's words go to the screen's live source. Started by hand only (`oos-mho-live`). Not yet verified on
//! hardware.

use crate::board::{lock, LiveShared};
use oos_hal::mho::bus::{Bar1, DmaBuf};
use oos_hal::mho::{acq, snapshot};
use std::time::Duration;

/// How long one acquisition may take to complete before the loop arms again.
const DONE_MS: u64 = 1000;

/// Starts the loop on the snapshot in `file`, `n` points a frame, `period_ms` between frames.
pub(super) fn spawn(file: String, n: u32, period_ms: u64) -> LiveShared {
    let shared: LiveShared = std::sync::Arc::new(std::sync::Mutex::new((0, Vec::new(), "starting".into())));
    let out = shared.clone();
    std::thread::spawn(move || {
        let status = |s: String| {
            eprintln!("oos-ui mho-live: {}", s);
            lock(&out).2 = s;
        };
        let s =
            match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| snapshot::Snapshot::parse(&t))
            {
                Ok(s) => s,
                Err(e) => return status(format!("{}: {}", file, e)),
            };
        let mut bar = match Bar1::open_len(acq::WINDOW_USED) {
            Ok(b) => b,
            Err(e) => return status(format!("{}: {} (oos-fpgad mho-start first)", acq::BAR_DEVICE, e)),
        };
        let mut c2h = match std::fs::File::open(acq::C2H_DEVICE) {
            Ok(f) => f,
            Err(e) => return status(format!("{}: {}", acq::C2H_DEVICE, e)),
        };
        let (mut sh, win) = (s.shadows(), s.window(n));
        status(format!("CH{} at {} V/div, {} points a frame", s.channel, s.scale_text, win.n));
        let mut buf = DmaBuf::new();
        let mut seq = 0u64;
        loop {
            match snapshot::acquire(&mut bar, &mut sh, &win, DONE_MS, &mut |ms| {
                std::thread::sleep(Duration::from_millis(ms))
            }) {
                Err(a) => status(format!("no acquisition within {} ms (status {:#x})", a.ms, a.status)),
                Ok(a) => match buf.read(&mut c2h, 2 * win.n as usize, |_, _| {}) {
                    Ok(bytes) => {
                        seq += 1;
                        let words: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|w| u16::from_le_bytes(*w)).collect();
                        let note = format!("frame {} after {} ms, status {:#x}", seq, a.ms, a.status);
                        *lock(&out) = (seq, words, note);
                    }
                    Err(e) => status(format!("read-out: {}", e)),
                },
            }
            std::thread::sleep(Duration::from_millis(period_ms));
        }
    });
    shared
}
