// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! mho-poll: watches FPGA registers of an MHO900 while another program drives the board, for
//! `tools/mho-check.py --capture-all` (docs/protocols/mho-acquisition.md, section 7). It records every change of the
//! ADC bridge word 0x3000 (each ADC register transfer leaves its word there for at least 100 us, section 4), and of
//! the link reset 0x2003c and the sync enable 0x10120, each with its time. Read only: the device is opened read-only
//! and the window mapped without write permission. Static for aarch64: it runs under the stock system too.
//!
//!   mho-poll [--seconds N] [--wait S] OUT
//!       waits up to S s (default 30) for /dev/xdma0_bypass, then watches for N s (default 40) and writes OUT: one
//!       line `MICROSECONDS OFFSET VALUE` per change (hex), then `polls`, `loop_us`, `marker` and `end`. At its time
//!       zero it writes `mho-poll t0` into the kernel's trace buffer (trace_marker), when tracing is on.

#[cfg(target_os = "linux")]
fn main() {
    use oos_hal::mho::acq::WINDOW_USED;
    use oos_hal::mho::bus::{Bar1, Bus};
    use std::time::{Duration, Instant};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |k: &str, d: u64| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let (seconds, wait) = (opt("--seconds", 40), opt("--wait", 30));
    let Some(out) = args.last().filter(|a| !a.starts_with("--") && args.len() % 2 == 1) else {
        eprintln!("usage: mho-poll [--seconds N] [--wait S] OUT");
        std::process::exit(2);
    };
    // the device comes back after the PCIe host and xdma were loaded again: open it only then, so that this watcher
    // never holds it while they are unloaded
    let t = Instant::now();
    let mut bar = loop {
        match Bar1::open_read_only(WINDOW_USED) {
            Ok(b) => break b,
            Err(e) if t.elapsed() > Duration::from_secs(wait) => {
                eprintln!("mho-poll: /dev/xdma0_bypass: {}", e);
                std::process::exit(1);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    const BRIDGE: u32 = 0x3000;
    const OTHERS: [u32; 2] = [0x2003c, 0x10120];
    let start = Instant::now();
    // time zero in the kernel's trace buffer, where mho-check.py records the GPIO lines: the two logs are merged on it
    let marker = std::fs::write("/sys/kernel/debug/tracing/trace_marker", "mho-poll t0\n").is_ok();
    let mut log: Vec<(u64, u32, u32)> = Vec::with_capacity(1 << 20);
    let mut last_bridge = bar.rd(BRIDGE);
    let mut last: Vec<u32> = OTHERS.iter().map(|&o| bar.rd(o)).collect();
    log.push((0, BRIDGE, last_bridge));
    log.extend(OTHERS.iter().zip(&last).map(|(&o, &v)| (0, o, v)));
    let mut polls = 0u64;
    let end = Duration::from_secs(seconds);
    while start.elapsed() < end {
        polls += 1;
        let v = bar.rd(BRIDGE);
        if v != last_bridge {
            log.push((start.elapsed().as_micros() as u64, BRIDGE, v));
            last_bridge = v;
        }
        if polls.is_multiple_of(32) {
            for (i, &o) in OTHERS.iter().enumerate() {
                let v = bar.rd(o);
                if v != last[i] {
                    log.push((start.elapsed().as_micros() as u64, o, v));
                    last[i] = v;
                }
            }
        }
    }
    let mut text: String = log.iter().map(|(t, o, v)| format!("{} {:#x} {:#010x}\n", t, o, v)).collect();
    text += &format!(
        "polls {}\nloop_us {:.3}\nmarker {}\nend\n",
        polls,
        start.elapsed().as_micros() as f64 / polls.max(1) as f64,
        marker as u8
    );
    if let Err(e) = std::fs::write(out, text) {
        eprintln!("mho-poll: {}: {}", out, e);
        std::process::exit(1);
    }
    eprintln!("mho-poll: {} changes in {} polls written to {}", log.len(), polls, out);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mho-poll runs on an MHO900 only");
    std::process::exit(3);
}
